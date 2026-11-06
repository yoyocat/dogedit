//! 指针提示：指针停在哪一格，皮肤要什么形状、提示什么。
//!
//! 声明在 `s:hint(region, {cursor=…, tip=…})` 里（见 [`dg_skin::HintSpec`]），这里
//! 负责把它变成屏幕上的东西：`cursor` 在 `WM_SETCURSOR` 里 `SetCursor`，`tip` 走
//! 共享的 [`TipHost`] 跟踪提示。
//!
//! **为什么不从 `WM_MOUSEMOVE` 里 `SetCursor`**：同一次移动 Windows 随后还会发一条
//! `WM_SETCURSOR`，那条会把刚设的形状按回去（`cursor.rs` 开头记的是同一件事）。
//! 所以移动只负责**记住**，落笔只在 `WM_SETCURSOR`。
//!
//! **`tip` 只有没有内置提示的区域收**：`toolbar.button` 的提示是控件按
//! `TTN_GETDISPINFOW` 问 `cmdtext` 要的，`status.pane` 的是 comctl32 自己在文字放
//! 不下时弹的。在那两处再挂一条跟踪提示就是两个框叠在一起。收不收由
//! [`dg_skin::Region::takes_skin_tip`] 说了算，声明期就拒掉。

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    HCURSOR, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_NO, IDC_SIZEALL,
    IDC_SIZENESW, IDC_SIZENWSE, IDC_SIZENS, IDC_SIZEWE, IDC_WAIT, LoadCursorW, SetCursor,
};

use dg_skin::Cursor;

use crate::win32::tiphost::TipHost;

/// 一个区域当前的指针提示。
#[derive(Default)]
pub struct SkinHint {
    /// 已经加载好的指针。`None` = 这一区没声明 `cursor`，指针保持内置形状。
    cursor: Option<HCURSOR>,
    /// 当前这一格的提示文本（模板已展开）。空 = 不弹。
    tip: String,
    /// 这条提示现在挂没挂在屏幕上。
    showing: bool,
}

impl SkinHint {
    pub const fn new() -> Self {
        Self {
            cursor: None,
            tip: String::new(),
            showing: false,
        }
    }

    /// 指针换到某一格。
    ///
    /// `hwnd` 是**那一格所在的窗口**（`anchor` 是客户区坐标，这里换成屏幕），
    /// `frame` 是提示宿主的父窗口。`input` 是这一格和绘制**同一份**的上下文 ——
    /// 所以 `@text` 展开的就是画在格子里的那串字。
    ///
    /// 只记不落笔：`SetCursor` 留给 [`apply`](Self::apply)。
    pub fn retarget(
        &mut self,
        hwnd: HWND,
        frame: HWND,
        host: Option<&dg_script::ScriptHost>,
        region: &str,
        input: &dg_script::PaintInput,
        anchor: POINT,
        tip_host: &mut Option<TipHost>,
    ) {
        let spec = host.and_then(|h| h.hint(region));
        let cursor = spec
            .as_ref()
            .and_then(|s| s.cursor)
            .and_then(system_cursor);
        let tip = spec
            .as_ref()
            .and_then(|s| s.resolve_tip(&input.text, &input.label, &input.value))
            .unwrap_or_default();
        // 同一格反复进来（`WM_MOUSEMOVE` 一秒几十条）时什么都别做：换 `TTM_*`
        // 会闪，`SetCursor` 会被随后的 `WM_SETCURSOR` 覆盖。
        if self.cursor == cursor && self.tip == tip {
            return;
        }
        self.cursor = cursor;
        // **当场落笔**，不等 `WM_SETCURSOR`：那条消息在 `WM_MOUSEMOVE` **之前**
        // 发，而形状往往正是这次移动才发现的（页签条要两条移动才把 `hot` 立起来），
        // 光记下来就得等下一次移动才看得见。`apply` 随后会按同一个值再设一遍，
        // 所以两条路永远一致。
        if cursor.is_some() {
            self.apply();
        }
        if tip.is_empty() {
            self.hide_tip(tip_host);
        } else {
            let mut at = anchor;
            unsafe {
                let _ = ClientToScreen(hwnd, &mut at);
            }
            if tip_host.is_none() {
                *tip_host = TipHost::create(frame);
            }
            if let Some(host) = tip_host.as_mut() {
                host.show_tracking(&tip, at.x, at.y);
                self.showing = true;
            }
        }
        self.tip = tip;
    }

    /// 指针离开这个区域（换窗口、换格到空白处）。
    pub fn leave(&mut self, tip_host: &mut Option<TipHost>) {
        self.cursor = None;
        self.hide_tip(tip_host);
        self.tip.clear();
    }

    /// `WM_SETCURSOR` 里调。`true` = 指针归皮肤管 —— 调用方返回 `TRUE`，别让
    /// `DefWindowProc` 把类的箭头塞回来。
    pub fn apply(&self) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        unsafe {
            let _ = SetCursor(Some(cursor));
        }
        true
    }

    fn hide_tip(&mut self, tip_host: &mut Option<TipHost>) {
        if !self.showing {
            return;
        }
        if let Some(host) = tip_host.as_mut() {
            host.hide_tracking();
        }
        self.showing = false;
    }
}

/// 提示挂在格子的**左下角往下两像素**：横着和格子左边界对齐，竖着不压住格子
/// 自己那行字。客户区坐标，调用方换成屏幕。
///
/// 收的是两条边而不是一个 `RECT`：页签条只存了 `left`/`right`（上下是整条的高度），
/// 硬凑一个 `RECT` 只会让调用方多写两行假数据。
///
/// 两像素的缝在 125% 下不放大也没关系 —— 它只是「别贴死」，不是度量。
pub fn under(left: i32, bottom: i32) -> POINT {
    POINT {
        x: left,
        y: bottom + 2,
    }
}

/// 词表里的名字 → 系统的标准指针。全是共享句柄，`LoadCursorW` 不分配。
pub fn system_cursor(shape: Cursor) -> Option<HCURSOR> {
    let id = match shape {
        Cursor::Arrow => IDC_ARROW,
        Cursor::Ibeam => IDC_IBEAM,
        Cursor::Hand => IDC_HAND,
        Cursor::Cross => IDC_CROSS,
        Cursor::SizeNs => IDC_SIZENS,
        Cursor::SizeWe => IDC_SIZEWE,
        Cursor::SizeNwse => IDC_SIZENWSE,
        Cursor::SizeNesw => IDC_SIZENESW,
        Cursor::SizeAll => IDC_SIZEALL,
        Cursor::Wait => IDC_WAIT,
        Cursor::AppStarting => IDC_APPSTARTING,
        Cursor::No => IDC_NO,
    };
    unsafe { LoadCursorW(None, id).ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(rect: (f32, f32, f32, f32), dpi: i32) -> dg_script::PaintInput {
        dg_script::PaintInput {
            rect,
            thumb: rect,
            bar: (0.0, 0.0, 0.0, 0.0),
            scroll: 0.0,
            state: 0,
            part: "Tab1".into(),
            text: "main.rs".into(),
            label: String::new(),
            value: "7".into(),
            dpi,
            index: 0,
        }
    }

    /// 提示挂在格子左下角往下两像素，横向对齐左边界。
    #[test]
    fn the_tip_hangs_under_the_cells_left_corner() {
        let at = under(10, 50);
        assert_eq!((at.x, at.y), (10, 52));
    }

    /// 上下文里那三样就是提示能用的全部 —— `@text` 展开的必须是画在格子里的
    /// 那串字，所以两边读的是同一个字段。
    #[test]
    fn the_hint_reads_the_cells_own_text() {
        let inp = input((10.0, 20.0, 100.0, 30.0), 96);
        let spec = dg_skin::HintSpec {
            cursor: Some(Cursor::Hand),
            tip: Some("@text".into()),
        };
        assert_eq!(
            spec.resolve_tip(&inp.text, &inp.label, &inp.value).as_deref(),
            Some("main.rs")
        );
    }

    /// 词表里每个名字都能换成一个真句柄 —— 少一个就是漏了一个 `IDC_*`。
    #[test]
    fn every_cursor_name_loads_a_system_cursor() {
        for shape in Cursor::ALL {
            assert!(system_cursor(shape).is_some(), "{shape} did not load");
        }
    }

    /// 两条页签条加脚本窗格收 `tip`，其余区域的提示归控件。
    #[test]
    fn only_the_tip_less_regions_take_a_tip() {
        use dg_skin::Region;
        let takes: Vec<&str> = Region::ALL
            .into_iter()
            .filter(|r| r.takes_skin_tip())
            .map(|r| r.as_str())
            .collect();
        // `custom` 是脚本窗格（`pane.<id>`）落的那一格：那上面没有任何控件，
        // 提示只能由皮肤给。
        assert_eq!(takes, vec!["tab", "dock.tab", "custom"]);
        assert!(!Region::ToolbarButton.takes_skin_tip());
        assert!(!Region::StatusPane.takes_skin_tip());
    }
}
