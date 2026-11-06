//! 脚本包管理器（`cm_config_package`, 36012）—— 工具 > 设置 > 脚本包。
//!
//! 一张带勾选的表：**勾选 = 启用**，取消勾选只写 `setting.ini` 的
//! `[Scripts] Disabled=`，**不碰包目录** —— 包还在原地，改清单照样会被扫描到，
//! 重新勾上就立刻回来。
//!
//! 形状照 [`syntaxdlg`](super::syntaxdlg)：`#32770` + 空 `DLGTEMPLATE`（子控件
//! 建在 `CreateDialogIndirectParamW` 之后，不用手写变长模板），状态对象留在
//! 调用方栈上、经 `GWLP_USERDATA` 交给 `dlg_proc`，模态由
//! [`prompt::run_modal_loop`] 驱动，配色走 [`chrome`]。
//!
//! 和语法着色那个框的区别只有两处：这里**没有 DLL**（禁用一个包不会卸载任何
//! 东西，它只是下次启动不进装载循环），以及每个包多一个「为什么没在跑」——
//! 停用、被同名覆盖、清单读不出来、装载失败，四件事在用户眼里都是「它没动静」，
//! 但在这一列里必须分得开。
//!
//! 列表控件那一整套（建壳、网格、列、勾选、表头自绘、消息泵）在
//! [`checklist`](super::checklist)，本文件只留包的数据和语义。

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::Cell;

use dg_render::theme::Theme;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, IDCANCEL, IDOK, SW_SHOW,
};
use windows::core::PCWSTR;

use super::checklist::{
    self, CLIENT_H, CLIENT_W, Ids, Labels, NONE_TEXT, Panel, Skin,
};
use super::prompt::wide;
use super::{chrome, uifont};

/// 控件 id。1210 段是本框自己定的 —— 1213/1214 已经是主题对话框的形状页，
/// 两边不共享窗口，但共用一个进程的 id 空间，错开比撞上便宜。
const ID_SUMMARY: i32 = 1215;
const ID_LIST: i32 = 1216;
const ID_DETAIL: i32 = 1217;
const ID_ALL_ON: i32 = 1218;
const ID_ALL_OFF: i32 = 1219;
const ID_OPEN_DIR: i32 = 1220;
const ID_OK: i32 = IDOK.0;
const ID_CANCEL: i32 = IDCANCEL.0;

/// 交给共用零件的一套 id；`checklist` 不猜 id 段。
const IDS: Ids = Ids {
    summary: ID_SUMMARY,
    list: ID_LIST,
    info: ID_DETAIL,
    all_on: ID_ALL_ON,
    all_off: ID_ALL_OFF,
    action: ID_OPEN_DIR,
    ok: ID_OK,
    cancel: ID_CANCEL,
};

/// 五列。合计比列表窄 4 px —— 列表控件的边框会吃掉客户端宽度的两三个像素，
/// 列宽刚好等于设计宽度时它就会长出一条没用的横向滚动条。
static COLUMNS: [(&str, i32); 5] = [
    ("包", 150),
    ("版本", 72),
    ("来源", 56),
    ("状态", 130),
    ("脚本", 204),
];

/// 表里的一行。调用方按 `package::scan_all` 的结果造，勾选状态由对话框维护。
pub(super) struct Row {
    pub id: String,
    pub version: String,
    /// 随包 / 用户 / 开发 / 散装。
    pub origin: String,
    /// 状态列的一句话：已启用 / 已停用 / 被同名覆盖 / 装载失败: …
    pub note: String,
    /// 这个包会跑哪几个 `.lua`（只有文件名，全路径在详情行）。
    pub scripts: Vec<String>,
    /// 包目录，详情行和「打开目录」用。
    pub dir: String,
    /// 勾选状态 = 不在 `[Scripts] Disabled` 里。
    pub enabled: bool,
}

/// 共用填表逻辑只问这四件事；列的语义仍留在本文件。
impl checklist::Row for Row {
    fn name(&self) -> &str {
        &self.id
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn cell(&self, col: usize) -> String {
        match col {
            1 => self.version.clone(),
            2 => self.origin.clone(),
            3 => self.note.clone(),
            4 => scripts_text(&self.scripts),
            _ => String::new(),
        }
    }
}

/// 调用方给对话框的输入。
pub(super) struct Request<'a> {
    pub frame: HWND,
    pub rows: &'a [Row],
    /// 清单读不出来的包有几个。它们不进表（勾选框对它们没有意义），但用户
    /// 得知道「我那个包为什么不在列表里」。
    pub broken: usize,
    pub theme: &'a Theme,
}

struct Dlg {
    frame: HWND,
    /// GDI 资源与 DPI，见 `checklist::Skin`。
    skin: Skin,
    /// 行顺序 = `rows` 的顺序。勾选状态只存在列表控件里，这里只是名字表 ——
    /// 点确定时按行号回查。
    ids: Vec<String>,
    /// 每行的目录和状态列文字，详情行用（选中变化时按行号取，不重扫磁盘）。
    dirs: Vec<String>,
    notes: Vec<String>,
    /// 当前勾选数，由 `LVN_ITEMCHANGED` 的 old/new 状态增量维护。
    checked: Cell<i32>,
    /// 清单读不出来的个数，只出现在摘要行里。
    broken: usize,
    /// 当前选中行；`-1` = 一行都没选（详情行显示一句提示）。
    selected: Cell<i32>,
    /// 点确定时填的出口；取消、Esc、标题栏 X 都保持 `None`。
    result: Option<Vec<String>>,
    destroyed: bool,
}

impl Panel for Dlg {
    fn columns() -> &'static [(&'static str, i32)] {
        &COLUMNS
    }
    fn ids() -> Ids {
        IDS
    }
    unsafe fn skin(dlg: HWND) -> Skin {
        state_mut(dlg).skin
    }
    unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
        on_command(dlg, wparam)
    }
    unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
        on_notify(dlg, lparam)
    }
    unsafe fn mark_destroyed(dlg: HWND) {
        state_mut(dlg).destroyed = true;
    }
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg)
}

/// 模态跑一遍：`Some(禁用名单)` = 点了确定，`None` = 取消。
///
/// 名单是**包 id** 的列表（可能为空 = 全部启用），顺序即列表顺序。
pub(super) fn show(req: Request<'_>) -> Option<Vec<String>> {
    let dpi = unsafe { uifont::window_dpi(req.frame) };
    let mut st = Dlg {
        frame: req.frame,
        skin: unsafe { Skin::new(req.theme, dpi) },
        ids: req.rows.iter().map(|r| r.id.clone()).collect(),
        dirs: req.rows.iter().map(|r| r.dir.clone()).collect(),
        notes: req.rows.iter().map(|r| r.note.clone()).collect(),
        checked: Cell::new(req.rows.iter().filter(|r| r.enabled).count() as i32),
        broken: req.broken,
        selected: Cell::new(-1),
        result: None,
        destroyed: false,
    };
    unsafe { run(&mut st, &req) };
    unsafe { st.skin.dispose() };
    st.result
}

unsafe fn run(st: &mut Dlg, req: &Request<'_>) {
    let Some(dlg) = checklist::create_dialog::<Dlg>(st as *mut Dlg, req.frame, "脚本包", req.theme)
    else {
        return;
    };

    let labels = Labels {
        summary: summary_line(st.ids.len(), st.checked.get(), st.broken),
        info: String::new(),
        all_on: crate::win32::lang::trs("全选"),
        all_off: crate::win32::lang::trs("全不选"),
        action: crate::win32::lang::trs("打开目录"),
        ok: crate::win32::lang::trs("确定"),
        cancel: crate::win32::lang::trs("取消"),
    };
    checklist::build_children(dlg, &IDS, &labels, &st.skin);
    // 子控件建完才能逐个套暗色 —— `dark_theme_children` 遍历的是现有子窗口。
    chrome::dark_theme_children(dlg, chrome::theme_is_dark(req.theme));
    checklist::size_and_place(dlg, st.frame, st.skin.dpi, (CLIENT_W, CLIENT_H));
    checklist::init_list::<Dlg>(dlg, &st.skin);
    checklist::fill_list(dlg, ID_LIST, req.rows);
    // 填表期间的通知不足以算准计数 —— 默认状态就是未选，未选的行根本不发。
    st.checked
        .set(req.rows.iter().filter(|r| r.enabled).count() as i32);
    refresh_summary(dlg, st);
    refresh_detail(dlg, st);

    checklist::finish(dlg, req.frame, ID_LIST, std::ptr::addr_of!(st.destroyed));
}

/// 摘要行：勾选数是活的 —— 用户点「全不选」应该立刻看到 0。
fn summary_line(total: usize, on: i32, broken: usize) -> String {
    let tail = if broken == 0 {
        String::new()
    } else {
        format!("另有 {broken} 个清单读不出来，见 --ui-doctor。")
    };
    format!("共 {total} 个包，已启用 {on} 个。取消勾选的包下次启动不再装载（不删文件）。{tail}")
}

/// 详情行：选中那个包的目录，加上它「为什么没在跑」。
///
/// 一行都没选时给一句操作提示 —— 空着的静态文本看起来像画错了。
fn detail_line(sel: i32, dirs: &[String], notes: &[String]) -> String {
    let Some(i) = usize::try_from(sel).ok() else {
        return DETAIL_HINT.to_string();
    };
    let Some(dir) = dirs.get(i) else {
        return DETAIL_HINT.to_string();
    };
    match notes.get(i) {
        Some(note) if !note.is_empty() && note != ENABLED_NOTE => format!("{dir}  —  {note}"),
        _ => dir.clone(),
    }
}

/// 没选中时的提示。
const DETAIL_HINT: &str = "选中一行看它的目录；勾选框决定下次启动装不装它。";

/// 一切正常时状态列写这四个字。详情行靠它决定要不要再重复一遍。
const ENABLED_NOTE: &str = "已启用";

fn scripts_text(scripts: &[String]) -> String {
    if scripts.is_empty() {
        NONE_TEXT.to_string()
    } else {
        scripts.join(" ")
    }
}

unsafe fn refresh_summary(dlg: HWND, st: &Dlg) {
    if let Some(ctl) = checklist::item(dlg, ID_SUMMARY) {
        checklist::set_text(ctl, &summary_line(st.ids.len(), st.checked.get(), st.broken));
    }
}

unsafe fn refresh_detail(dlg: HWND, st: &Dlg) {
    if let Some(ctl) = checklist::item(dlg, ID_DETAIL) {
        checklist::set_text(ctl, &detail_line(st.selected.get(), &st.dirs, &st.notes));
    }
}

/// 打开选中那一行的目录。没选中就什么都不做 —— 猜一行去打开是最糟的「帮忙」。
unsafe fn open_dir(dlg: HWND) {
    let st = state_mut(dlg);
    let sel = st.selected.get();
    let Some(dir) = usize::try_from(sel).ok().and_then(|i| st.dirs.get(i).cloned()) else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let path = wide(&dir);
    let _ = ShellExecuteW(
        Some(dlg),
        None,
        PCWSTR::from_raw(path.as_ptr()),
        None,
        None,
        SW_SHOW,
    );
}

/// 确定：把没勾的行收成禁用名单。
unsafe fn accept(dlg: HWND) {
    let disabled = checklist::disabled_names(dlg, ID_LIST, &state_mut(dlg).ids);
    let Some(disabled) = disabled else {
        return;
    };
    state_mut(dlg).result = Some(disabled);
    let _ = DestroyWindow(dlg);
}

/// 勾选变化维护计数器；选中变化换详情行。两件事都在这一条通知里。
unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
    let Some(ch) = checklist::item_change(lparam, ID_LIST) else {
        return 0;
    };
    let st = state_mut(dlg);
    if ch.checked_delta != 0 {
        st.checked.set(st.checked.get() + ch.checked_delta);
        refresh_summary(dlg, st);
    }
    // 选中位单独判：勾选一行也会把它选中，两种变化各自独立。
    if let Some(sel) = ch.selection {
        st.selected.set(sel);
        refresh_detail(dlg, st);
    }
    1
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    match id {
        ID_OK => {
            accept(dlg);
            1
        }
        ID_CANCEL => {
            let _ = DestroyWindow(dlg);
            1
        }
        ID_ALL_ON | ID_ALL_OFF => {
            checklist::set_all(dlg, ID_LIST, state_mut(dlg).ids.len() as i32, id == ID_ALL_ON);
            1
        }
        ID_OPEN_DIR => {
            open_dir(dlg);
            1
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列宽之和只许比列表窄一点：宽了会长出横向滚动条，窄了右边留一条底色。
    #[test]
    fn the_columns_fit_the_list() {
        let sum: i32 = COLUMNS.iter().map(|(_, w)| *w).sum();
        assert!(sum <= checklist::RECT_LIST.2, "列宽之和 {sum} 超过列表宽");
        assert!(
            checklist::RECT_LIST.2 - sum <= 8,
            "右边留白太多：{}",
            checklist::RECT_LIST.2 - sum
        );
        assert_eq!(CLIENT_W, 640);
        assert_eq!(CLIENT_H, 432);
    }

    /// 详情行：没选中给提示，选中给目录，没在跑的再补一句为什么。
    #[test]
    fn the_detail_line_says_where_and_why() {
        let dirs = vec!["E:\\packages\\a.one".to_string(), "E:\\packages\\a.two".to_string()];
        let notes = vec![ENABLED_NOTE.to_string(), "已停用".to_string()];
        assert_eq!(detail_line(-1, &dirs, &notes), DETAIL_HINT);
        assert_eq!(detail_line(0, &dirs, &notes), "E:\\packages\\a.one");
        assert_eq!(detail_line(1, &dirs, &notes), "E:\\packages\\a.two  —  已停用");
        // 行号越界不该 panic：列表刚清空的那一帧就会这样。
        assert_eq!(detail_line(9, &dirs, &notes), DETAIL_HINT);
    }

    /// 空脚本表用占位符，不留空白单元格。
    #[test]
    fn empty_cells_get_a_placeholder() {
        assert_eq!(scripts_text(&[]), NONE_TEXT);
        assert_eq!(
            scripts_text(&["a.lua".to_string(), "b.lua".to_string()]),
            "a.lua b.lua"
        );
    }

    #[test]
    fn the_summary_counts_what_is_ticked() {
        assert_eq!(
            summary_line(3, 2, 0),
            "共 3 个包，已启用 2 个。取消勾选的包下次启动不再装载（不删文件）。"
        );
        // 有清单读不出来的包时补一句 —— 否则用户只会觉得「我那个包不见了」。
        let broken = summary_line(3, 2, 2);
        assert!(broken.contains("另有 2 个清单读不出来"), "{broken}");
    }

    /// id 段不许和 `IDOK`/`IDCANCEL` 撞，也不许和主题对话框的 1213/1214 撞。
    #[test]
    fn control_ids_do_not_collide() {
        let ids = [ID_SUMMARY, ID_LIST, ID_DETAIL, ID_ALL_ON, ID_ALL_OFF, ID_OPEN_DIR];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "控件 id 必须唯一");
        assert!(!ids.contains(&ID_OK) && !ids.contains(&ID_CANCEL));
        assert!(!ids.contains(&1213) && !ids.contains(&1214), "主题对话框的 id");
    }
}
