//! 菜单行的说明提示：高亮停在一行上，过一会儿在行旁边弹一条该命令的「说明」。
//!
//! 工具栏按钮的提示由控件自己按 `TTN_GETDISPINFOW` 问命令表要；菜单是模态的
//! `#32768` 窗口，没有控件可以挂工具，所以这里用 [`TipHost`] 的跟踪提示：
//! 选中项换来一条 [`WM_MENUSELECT`]，延时一拍再弹，换项/关菜单立刻收，
//! 弹够 5 秒自己收。文字走语言表的 `[STRING]`（`说明\n命令` 的前半段），
//! 表里没有的命令退回 `cmdtext` 的 说明，两者都没有就不弹 —— 动态行
//! （最近文件、语法模式、插件）不该把状态栏消息当作说明显示出来。
//!
//! 消息不是发给某一扇固定窗口的：`TrackPopupMenu` 的 owner 可以是主框架，
//! 也可以是工具条，所以这里装一条线程内的 `WH_CALLWNDPROC` 钩子看所有
//! 发出去的 `WM_MENUSELECT`。状态只放在线程局部里，不碰编辑器状态，
//! 菜单模态循环重进任何窗口过程都不会撞出第二个可变借用。
//!
//! 延时和自动收起借主框架上空闲的定时器号；提示宿主也挂在同一个框架上。

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST, MonitorFromPoint,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, GA_ROOTOWNER, GetAncestor, GetCursorPos, GetMenuItemCount,
    GetMenuItemID, GetMenuItemRect, HHOOK, HMENU, MF_POPUP, MF_SEPARATOR, MF_SYSMENU,
    KillTimer, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, WH_CALLWNDPROC,
    WM_EXITMENULOOP, WM_MENUSELECT,
};

use crate::win32::tiphost::TipHost;
use crate::win32::window::MENU_TIP_TIMER_ID;

/// 停顿多久才弹。工具条的提示由 comctl32 按同一个量级延时，这里保持一致。
const DELAY_MS: u32 = 500;
/// 一条提示最多挂多久。跟踪提示不会自己收，得由定时器收。
const AUTOPOP_MS: u32 = 5000;
/// 估算一行提示占的屏幕空间，只用来决定贴哪一侧、以及贴住屏幕边时不越界。
const TIP_H: i32 = 26;
const TIP_W: i32 = 300;

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::new()) };
    static HOOK: Cell<isize> = const { Cell::new(0) };
}

struct Pending {
    text: String,
    at: POINT,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Off,
    Delay,
    Visible,
}

struct State {
    /// 定时器挂在哪扇窗口上（通常是主框架）。
    frame: Option<HWND>,
    host: Option<TipHost>,
    pending: Option<Pending>,
    showing: bool,
    timer: bool,
    phase: Phase,
}

impl State {
    const fn new() -> Self {
        Self {
            frame: None,
            host: None,
            pending: None,
            showing: false,
            timer: false,
            phase: Phase::Off,
        }
    }

    fn arm(&mut self, frame: HWND, ms: u32) {
        unsafe {
            if self.timer {
                if let Some(old) = self.frame {
                    if old != frame {
                        let _ = KillTimer(Some(old), MENU_TIP_TIMER_ID);
                    }
                }
            }
            self.frame = Some(frame);
            let _ = SetTimer(Some(frame), MENU_TIP_TIMER_ID, ms, None);
            self.timer = true;
        }
    }

    fn kill(&mut self) {
        if !self.timer {
            return;
        }
        if let Some(frame) = self.frame {
            unsafe {
                let _ = KillTimer(Some(frame), MENU_TIP_TIMER_ID);
            }
        }
        self.timer = false;
    }

    fn hide(&mut self) {
        if !self.showing {
            return;
        }
        if let Some(host) = self.host.as_mut() {
            host.hide_tracking();
        }
        self.showing = false;
    }

    fn clear(&mut self) {
        self.hide();
        self.kill();
        self.pending = None;
        self.phase = Phase::Off;
    }

    /// 定时器只装一拍：延时到了就弹，弹完再装一拍等着自己收。
    fn tick(&mut self, frame: HWND) {
        self.kill();
        match self.phase {
            Phase::Delay => {
                let Some(p) = self.pending.take() else {
                    self.phase = Phase::Off;
                    return;
                };
                if self.host.is_none() {
                    self.host = TipHost::create(frame);
                }
                if let Some(host) = self.host.as_mut() {
                    host.show_tracking(&p.text, p.at.x, p.at.y);
                    self.showing = true;
                }
                self.phase = Phase::Visible;
                self.arm(frame, AUTOPOP_MS);
            }
            Phase::Visible => {
                self.hide();
                self.pending = None;
                self.phase = Phase::Off;
            }
            Phase::Off => {}
        }
    }
}

/// 装钩子。`run()` 里主窗口建好之后调一次即可。
pub(super) unsafe fn install_hook() {
    HOOK.with(|slot| {
        if slot.get() != 0 {
            return;
        }
        let hook = SetWindowsHookExW(
            WH_CALLWNDPROC,
            Some(hook_proc),
            Some(crate::win32::view::instance()),
            GetCurrentThreadId(),
        );
        if let Ok(hook) = hook {
            slot.set(hook.0 as isize);
        }
    });
}

/// 撤钩子、收掉屏幕上的提示、拆掉定时器。`WM_DESTROY` 里调。
pub(super) unsafe fn shutdown() {
    remove_hook();
    STATE.with(|cell| {
        if let Ok(mut st) = cell.try_borrow_mut() {
            st.clear();
            // 主框架正在销毁，挂在它下面的提示窗口不能留到之后。
            st.host = None;
        }
    });
}

unsafe fn remove_hook() {
    HOOK.with(|slot| {
        let raw = slot.get();
        if raw != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut core::ffi::c_void));
            slot.set(0);
        }
    });
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 && lparam.0 != 0 {
        let cwp = &*(lparam.0 as *const CWPSTRUCT);
        match cwp.message {
            WM_MENUSELECT => on_menu_select(cwp.hwnd, cwp.wParam, cwp.lParam),
            WM_EXITMENULOOP => STATE.with(|cell| {
                if let Ok(mut st) = cell.try_borrow_mut() {
                    st.clear();
                }
            }),
            _ => {}
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// 主框架的 `WM_TIMER`：这一拍要么把待弹的提示放上屏幕，要么把已弹的收掉。
pub(super) fn on_timer(hwnd: HWND, id: usize) {
    if id != MENU_TIP_TIMER_ID {
        return;
    }
    STATE.with(|cell| {
        if let Ok(mut st) = cell.try_borrow_mut() {
            st.tick(hwnd);
        }
    });
}

#[derive(Debug, PartialEq)]
enum Selection {
    Closed,
    Skip,
    Item(u32),
}

/// `WM_MENUSELECT` 的 wparam 低字是命令 id（弹出行则是序号），高字是 MF_* 位；
/// lparam 是那一项所在的菜单。关菜单时发 `0xFFFF0000` + 空 lparam。
fn decode(wparam: WPARAM, lparam: LPARAM) -> Selection {
    let item = (wparam.0 & 0xFFFF) as u32;
    let flags = ((wparam.0 >> 16) & 0xFFFF) as u32;
    if lparam.0 == 0 && (flags == 0xFFFF || item == 0xFFFF) {
        return Selection::Closed;
    }
    if item == 0 || flags & (MF_POPUP.0 | MF_SEPARATOR.0 | MF_SYSMENU.0) != 0 {
        return Selection::Skip;
    }
    Selection::Item(item)
}

/// 只有「`说明\n命令`」这种两行条目才是命令说明；状态栏消息也住在
/// `[STRING]` 里，不能拿它当提示。
fn command_desc(raw: &str) -> Option<&str> {
    let (desc, _) = raw.split_once("\\n")?;
    let desc = desc.trim();
    (!desc.is_empty()).then_some(desc)
}

fn description(id: u32) -> String {
    if let Some(desc) = crate::win32::lang::string(id).and_then(command_desc) {
        return desc.to_string();
    }
    crate::win32::cmdtext::text_for(id).1.to_string()
}

unsafe fn item_position(menu: HMENU, id: u32) -> Option<u32> {
    let count = GetMenuItemCount(Some(menu)).max(0);
    for pos in 0..count {
        if GetMenuItemID(menu, pos) == id {
            return Some(pos as u32);
        }
    }
    None
}

/// 菜单是弹出来的，`hWnd` 传空让系统自己找那扇 `#32768`；返回的矩形是屏幕坐标。
unsafe fn work_area(at: POINT) -> RECT {
    let mon = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !mon.0.is_null() && GetMonitorInfoW(mon, &mut mi).as_bool() {
        return mi.rcWork;
    }
    RECT {
        left: 0,
        top: 0,
        right: 10_000,
        bottom: 10_000,
    }
}

/// 贴在高亮行右侧、和行顶对齐：弹在菜单旁边不会盖住紧跟着的下一行。
/// 右侧放不下就翻到菜单左边，两边都放不下（窄屏）才退回行的下方/上方。
fn anchor(rc: RECT, work: RECT) -> POINT {
    // 侧贴时提示和行顶齐平，但最底下一行还得把提示整条拉回工作区里。
    let y_top = rc.top.clamp(work.top, (work.bottom - TIP_H).max(work.top));
    let right = rc.right + 8;
    if right + TIP_W <= work.right {
        return POINT { x: right, y: y_top };
    }
    let left = rc.left - TIP_W - 8;
    if left >= work.left {
        return POINT { x: left, y: y_top };
    }
    let below = rc.bottom + 2;
    let y = if below + TIP_H <= work.bottom {
        below
    } else {
        (rc.top - TIP_H - 2).max(work.top)
    };
    let edge = (work.right - TIP_W).max(work.left);
    POINT {
        x: rc.left.clamp(work.left, edge),
        y,
    }
}

fn on_menu_select(owner: HWND, wparam: WPARAM, lparam: LPARAM) {
    let id = match decode(wparam, lparam) {
        Selection::Item(id) => id,
        _ => {
            STATE.with(|cell| {
                if let Ok(mut st) = cell.try_borrow_mut() {
                    st.clear();
                }
            });
            return;
        }
    };
    let text = description(id);
    if text.is_empty() {
        STATE.with(|cell| {
            if let Ok(mut st) = cell.try_borrow_mut() {
                st.clear();
            }
        });
        return;
    }
    let menu = HMENU(lparam.0 as *mut core::ffi::c_void);
    let Some(pos) = (unsafe { item_position(menu, id) }) else {
        return;
    };
    let mut rc = RECT::default();
    let at = unsafe {
        if GetMenuItemRect(None, menu, pos, &mut rc).is_ok() {
            anchor(rc, work_area(POINT { x: rc.left, y: rc.bottom }))
        } else {
            // 键盘导航拿到矩形之前的一两帧：跟着指针走，总比不弹强。
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                return;
            }
            POINT { x: pt.x, y: pt.y + 18 }
        }
    };
    // 定时器与提示宿主都挂在 owning 链的顶端；工具条弹出的右键菜单
    // owner 是工具条本身，它的 root owner 才是主框架。
    let frame = unsafe { GetAncestor(owner, GA_ROOTOWNER) };
    let frame = if frame.0.is_null() { owner } else { frame };
    STATE.with(|cell| {
        if let Ok(mut st) = cell.try_borrow_mut() {
            st.hide();
            st.pending = Some(Pending { text, at });
            st.phase = Phase::Delay;
            st.arm(frame, DELAY_MS);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(item: u32, flags: u32) -> WPARAM {
        WPARAM(((flags as usize) << 16) | item as usize)
    }

    /// 关菜单的那一条不带 HMENU，不能当成「选中了第 0 行」。
    #[test]
    fn a_closed_menu_has_no_row() {
        assert_eq!(decode(wp(0, 0xFFFF), LPARAM(0)), Selection::Closed);
        assert_eq!(decode(wp(0xFFFF, 0xFFFF), LPARAM(0)), Selection::Closed);
    }

    /// 弹出项、分隔线、系统菜单都没有命令说明。
    #[test]
    fn submenu_separator_and_system_rows_are_skipped() {
        assert_eq!(decode(wp(3, MF_POPUP.0 | 0x80), LPARAM(1)), Selection::Skip);
        assert_eq!(decode(wp(7, MF_SEPARATOR.0), LPARAM(1)), Selection::Skip);
        assert_eq!(decode(wp(9, MF_SYSMENU.0), LPARAM(1)), Selection::Skip);
        assert_eq!(decode(wp(0, 0x80), LPARAM(1)), Selection::Skip);
    }

    /// 普通叶子给的是命令 id，MF_HILITE / MF_OWNERDRAW 不影响取值。
    #[test]
    fn a_leaf_row_names_its_command() {
        assert_eq!(decode(wp(57600, 0x80 | 0x100), LPARAM(1)), Selection::Item(57600));
    }

    /// `[STRING]` 里两行的是命令，单行的是状态栏消息。
    #[test]
    fn only_the_command_pair_carries_a_description() {
        assert_eq!(
            command_desc("创建一个新的文档\\n新建"),
            Some("创建一个新的文档")
        );
        assert_eq!(command_desc("Create a new text document\\nNormal text"), Some("Create a new text document"));
        assert_eq!(command_desc("文件已被外部修改，行索引失效，请重新打开"), None);
        assert_eq!(command_desc("\\n空说明"), None);
    }

    /// 命令表里的行一定有提示文字；空 id 没有。
    #[test]
    fn a_documented_command_has_a_description() {
        assert!(!description(57600).is_empty());
        assert!(description(0).is_empty());
    }

    /// 默认贴在菜单右边、与行顶齐平，不压住下一行；右边没地方翻到左边；
    /// 两边都没有才退回行下方/上方。
    #[test]
    fn the_tip_beside_the_row_and_stays_on_screen() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let row = RECT { left: 100, top: 200, right: 340, bottom: 224 };
        assert_eq!(anchor(row, work), POINT { x: 348, y: 200 });
        let last = RECT { left: 1700, top: 1050, right: 1900, bottom: 1074 };
        assert_eq!(anchor(last, work), POINT { x: 1392, y: 1050 });
        // 贴着屏幕底边的一行：行顶对齐会把提示下半截顶出工作区，得往上收。
        let edge = RECT { left: 100, top: 1070, right: 340, bottom: 1094 };
        assert_eq!(anchor(edge, work), POINT { x: 348, y: 1054 });
        let narrow = RECT { left: 0, top: 0, right: 400, bottom: 300 };
        let bottom = RECT { left: 100, top: 280, right: 340, bottom: 299 };
        assert_eq!(anchor(bottom, narrow), POINT { x: 100, y: 252 });
    }
}
