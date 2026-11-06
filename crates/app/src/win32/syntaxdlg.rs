//! 语法着色 — 解析器插件包管理器（`cm_config_syntax`, 1459）。
//!
//! 这一行在命令表里一直有（1459 / 工具 > 设置 > 语法着色），但在本模块之前
//! 没有 `Command` 实现 —— 装好的包对用户是黑盒，唯一能看见的是输出窗格里
//! 「重新扫描解析器」(36007) 那份报告。这个对话框把
//! [`dg_syntax::LanguageRegistry::plugin_list`] 摊成一张带勾选的表：
//!
//! * **勾选 = 启用**。取消勾选不卸载 DLL（plan §3.2 永不 `FreeLibrary`），
//! 只是不再被解析到 —— 新打开的文件不再用它，已打开的文档保留自己的
//! 高亮器。**确定** 才生效：返回禁用名单，由调用方写回 `setting.ini` 的
//! `[Syntax] Disabled` 并按新映射重着色；取消/关窗什么都不改。
//! * **状态列** 是惰性加载的真实状态（未加载 / 已加载 / 加载失败）。DLL 只在
//! 第一次真正用到时才 `LoadLibrary`，所以「未加载」不是错误 —— 加载失败的
//! 原因在扫描诊断里，这里在底部给一行摘要。
//!
//! 形状照 [`encdlg`](super::encdlg)：`#32770` + 空 `DLGTEMPLATE`（子控件建在
//! `CreateDialogIndirectParamW` 之后，不用手写变长模板），状态对象留在调用方
//! 栈上、经 `GWLP_USERDATA` 交给 `dlg_proc`，模态由
//! [`prompt::run_modal_loop`] 驱动。配色走 [`chrome`] 的暗色主题，静态文本和
//! 对话框底色从 `Theme` 取 —— 和别的本实现对话框同一套。
//!
//! 列表控件那一整套在 [`checklist`](super::checklist)，本文件只留解析器的
//! 数据、诊断文字和「在线获取」这个额外出口。

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::Cell;

use dg_render::theme::Theme;
use dg_syntax::{PluginDiag, PluginInfo, PluginState};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, IDCANCEL, IDOK,
};

use super::checklist::{
    self, CLIENT_H, CLIENT_W, Ids, Labels, NONE_TEXT, Panel, Skin,
};
use super::{chrome, uifont};

/// 控件 id。1200 段是「对话框自己的控件」 —— 1459 在参考里没有可用的资源，
/// 这套 id 是本实现自己定的。
const ID_SUMMARY: i32 = 1200;
const ID_LIST: i32 = 1201;
const ID_DIAGS: i32 = 1202;
const ID_ALL_ON: i32 = 1203;
const ID_ALL_OFF: i32 = 1204;
const ID_MARKET: i32 = 1205;
/// 确定/取消用对话框管理器自己的 id：回车走默认按钮、Esc 走 `IDCANCEL`，
/// 都是 `DefDlgProc` 认的。
const ID_OK: i32 = IDOK.0;
const ID_CANCEL: i32 = IDCANCEL.0;

const IDS: Ids = Ids {
    summary: ID_SUMMARY,
    list: ID_LIST,
    info: ID_DIAGS,
    all_on: ID_ALL_ON,
    all_off: ID_ALL_OFF,
    action: ID_MARKET,
    ok: ID_OK,
    cancel: ID_CANCEL,
};

/// 五列，宽度合计等于列表宽（[`RECT_LIST`].2）。
static COLUMNS: [(&str, i32); 5] = [
    ("语言", 100),
    ("显示名", 110),
    ("扩展名", 180),
    ("状态", 76),
    ("query", 150),
];

/// 调用方给对话框的输入。
pub(super) struct Request<'a> {
    pub frame: HWND,
    /// [`dg_syntax::LanguageRegistry::plugin_list`] 的快照，行顺序即此顺序。
    pub plugins: &'a [PluginInfo],
    /// 最近一次扫描的诊断，只在底部给一行摘要（全文在 36007 的输出窗格里）。
    pub diags: &'a [PluginDiag],
    pub theme: &'a Theme,
}

/// 框的两个出口：禁用名单（确定），或转「解析器下载」框
/// （`cm_parser_market` 的管理框 —— 入口按钮在这里，实现在 parserdl）。
pub(super) enum Outcome {
    Disabled(Vec<String>),
    Market,
}

/// 共用填表逻辑只问这四件事；列的语义仍留在本文件。
impl checklist::Row for PluginInfo {
    fn name(&self) -> &str {
        &self.name
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn cell(&self, col: usize) -> String {
        match col {
            1 => self.display.clone(),
            2 => exts_text(&self.extensions),
            3 => state_text(self.state).to_string(),
            4 => queries_text(&self.queries),
            _ => String::new(),
        }
    }
}

/// 对话框对象：留在 [`show`] 的栈上，`dlg_proc` 经 `GWLP_USERDATA` 拿裸指针。
struct Dlg {
    frame: HWND,
    /// GDI 资源与 DPI，见 `checklist::Skin`。
    skin: Skin,
    /// 行顺序 = `plugin_list()` 的顺序（包名字典序）。勾选状态只存在列表控件
    /// 里，这里只是名字表 —— 点确定时按行号回查。
    names: Vec<String>,
    /// 当前勾选数，由 `LVN_ITEMCHANGED` 的 old/new 状态增量维护。
    checked: Cell<i32>,
    /// 点确定/「在线获取」时填的出口；取消、Esc、标题栏 X 都保持 `None`。
    result: Option<Outcome>,
    /// `WM_DESTROY` 置真，模态循环靠它区分「本框自己销毁」和「主窗收到
    /// WM_QUIT」（后者要重新投递，否则关一次对话框就把进程结束了）。
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

/// 模态跑一遍：`Some(出口)` = 点了确定（禁用名单）或「在线获取」，
/// `None` = 取消。
///
/// 名单是**包名**的列表（可能为空 = 全部启用），顺序即列表顺序。
pub(super) fn show(req: Request<'_>) -> Option<Outcome> {
    let dpi = unsafe { uifont::window_dpi(req.frame) };
    let mut st = Dlg {
        frame: req.frame,
        skin: unsafe { Skin::new(req.theme, dpi) },
        names: req.plugins.iter().map(|p| p.name.clone()).collect(),
        checked: Cell::new(req.plugins.iter().filter(|p| p.enabled).count() as i32),
        result: None,
        destroyed: false,
    };
    unsafe { run(&mut st, &req) };
    unsafe { st.skin.dispose() };
    st.result
}

unsafe fn run(st: &mut Dlg, req: &Request<'_>) {
    let title = crate::win32::lang::dlg(223, 0).unwrap_or("语法着色");
    let Some(dlg) = checklist::create_dialog::<Dlg>(st as *mut Dlg, req.frame, title, req.theme)
    else {
        return;
    };

    let labels = Labels {
        summary: summary_line(st.names.len(), st.checked.get()),
        info: diag_line(req.diags),
        all_on: crate::win32::lang::trs("全选"),
        all_off: crate::win32::lang::trs("全不选"),
        action: crate::win32::lang::trs("在线获取..."),
        ok: crate::win32::lang::dlg(223, 1).unwrap_or("确定").to_string(),
        cancel: crate::win32::lang::dlg(223, 2).unwrap_or("取消").to_string(),
    };
    checklist::build_children(dlg, &IDS, &labels, &st.skin);
    // 子控件建完才能逐个套暗色 —— `dark_theme_children` 遍历的是现有子窗口。
    chrome::dark_theme_children(dlg, chrome::theme_is_dark(req.theme));
    checklist::size_and_place(dlg, st.frame, st.skin.dpi, (CLIENT_W, CLIENT_H));
    checklist::init_list::<Dlg>(dlg, &st.skin);
    checklist::fill_list(dlg, ID_LIST, req.plugins);
    // 填表期间的通知（`LVM_SETITEMSTATE` 会发）不足以算准计数 —— 默认状态
    // 就是未选，未选的行根本不发通知。填完按真实状态校准一次。
    st.checked
        .set(req.plugins.iter().filter(|p| p.enabled).count() as i32);
    refresh_summary(dlg, st);

    checklist::finish(dlg, req.frame, ID_LIST, std::ptr::addr_of!(st.destroyed));
}

/// 摘要行：勾选数是活的（每改一次勾选重写一遍），这是这张表唯一需要实时
/// 反馈的地方 —— 用户点「全不选」应该立刻看到 0。
fn summary_line(total: usize, on: i32) -> String {
    crate::win32::lang::format(40050, &[&total.to_string(), &on.to_string()])
        .unwrap_or_else(|| {
            format!("共 {total} 个解析器包，已启用 {on} 个。取消勾选的包不再被新打开的文件使用。")
        })
}

/// 底部诊断行。诊断全文在 36007 的输出窗格里，这里只给第一条和总数 —
/// 有包坏掉时用户至少知道有东西坏了，而不是看见空列表以为没装包。
fn diag_line(diags: &[PluginDiag]) -> String {
    match diags.split_first() {
        None => crate::win32::lang::string(40051)
            .unwrap_or("诊断：无")
            .to_string(),
        Some((first, rest)) => {
            let more = if rest.is_empty() {
                String::new()
            } else {
                crate::win32::lang::format(40053, &[&diags.len().to_string()])
                    .unwrap_or_else(|| {
                        format!("（等 {} 条，详见 重新扫描解析器 的输出）", diags.len())
                    })
            };
            let head = crate::win32::lang::format(40052, &[&first.package, &first.message])
                .unwrap_or_else(|| format!("诊断：{} — {}", first.package, first.message));
            format!("{head}{more}")
        }
    }
}

/// 状态列的文字。`Unloaded` 是惰性加载的正常状态，不是错误。
fn state_text(s: PluginState) -> &'static str {
    match s {
        PluginState::Unloaded => crate::win32::lang::string(40014).unwrap_or("未加载"),
        PluginState::Loaded => crate::win32::lang::string(40015).unwrap_or("已加载"),
        PluginState::Failed => crate::win32::lang::string(40016).unwrap_or("加载失败"),
    }
}

fn exts_text(exts: &[String]) -> String {
    if exts.is_empty() {
        NONE_TEXT.to_string()
    } else {
        exts.join(" ")
    }
}

fn queries_text(queries: &[String]) -> String {
    if queries.is_empty() {
        NONE_TEXT.to_string()
    } else {
        queries.join(" ")
    }
}

unsafe fn refresh_summary(dlg: HWND, st: &Dlg) {
    if let Some(ctl) = checklist::item(dlg, ID_SUMMARY) {
        checklist::set_text(ctl, &summary_line(st.names.len(), st.checked.get()));
    }
}

/// 确定：把没勾的行收成禁用名单。
unsafe fn accept(dlg: HWND) {
    let disabled = checklist::disabled_names(dlg, ID_LIST, &state_mut(dlg).names);
    let Some(disabled) = disabled else {
        return;
    };
    state_mut(dlg).result = Some(Outcome::Disabled(disabled));
    let _ = DestroyWindow(dlg);
}

/// 勾选变化的通知：只用 old/new 两个 state 的差维护计数器，再刷摘要。
/// 摘要无条件重写 —— 选中位变化也会走到这里，文字本身不变但代价可以忽略。
unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
    let Some(ch) = checklist::item_change(lparam, ID_LIST) else {
        return 0;
    };
    let st = state_mut(dlg);
    if ch.checked_delta != 0 {
        st.checked.set(st.checked.get() + ch.checked_delta);
    }
    refresh_summary(dlg, st);
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
            checklist::set_all(dlg, ID_LIST, state_mut(dlg).names.len() as i32, id == ID_ALL_ON);
            1
        }
        // 在线获取：转「解析器下载」框。勾选的改动不落地（同取消）。
        ID_MARKET => {
            state_mut(dlg).result = Some(Outcome::Market);
            let _ = DestroyWindow(dlg);
            1
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列宽之和必须刚好等于列表宽，否则右边会留一条空白列。
    #[test]
    fn the_columns_fill_the_list() {
        assert_eq!(CLIENT_W, 640);
        assert_eq!(CLIENT_H, 432);
        let sum: i32 = COLUMNS.iter().map(|(_, w)| *w).sum();
        assert_eq!(sum, checklist::RECT_LIST.2, "列宽之和必须等于列表宽");
    }

    /// 状态列的文字是给用户看的，`Unloaded` 不能被写成「失败」。
    #[test]
    fn state_column_words() {
        assert_eq!(state_text(PluginState::Unloaded), "未加载");
        assert_eq!(state_text(PluginState::Loaded), "已加载");
        assert_eq!(state_text(PluginState::Failed), "加载失败");
    }

    /// 空列表用占位符，不留空白单元格。
    #[test]
    fn empty_cells_get_a_placeholder() {
        assert_eq!(exts_text(&[]), NONE_TEXT);
        assert_eq!(exts_text(&["rs".into(), "toml".into()]), "rs toml");
        assert_eq!(queries_text(&[]), NONE_TEXT);
        assert_eq!(
            queries_text(&["folds".into(), "highlights".into()]),
            "folds highlights"
        );
    }

    #[test]
    fn summary_and_diagnostics_read_as_one_line() {
        assert_eq!(
            summary_line(62, 59),
            "共 62 个解析器包，已启用 59 个。取消勾选的包不再被新打开的文件使用。"
        );
        assert_eq!(diag_line(&[]), "诊断：无");
        let one = [PluginDiag {
            package: "broken".into(),
            message: "缺少 manifest.toml".into(),
        }];
        assert_eq!(diag_line(&one), "诊断：broken — 缺少 manifest.toml");
        let two = [
            one[0].clone(),
            PluginDiag {
                package: "illegal".into(),
                message: "manifest 缺少 name".into(),
            },
        ];
        let line = diag_line(&two);
        assert!(
            line.starts_with("诊断：broken — 缺少 manifest.toml"),
            "{line}"
        );
        assert!(line.contains("等 2 条"), "{line}");
    }

    /// id 段不许和 `IDOK`/`IDCANCEL` 撞，否则回车/Esc 会打到一个普通按钮上。
    #[test]
    fn control_ids_do_not_collide_with_the_dialog_managers() {
        let ids = [ID_SUMMARY, ID_LIST, ID_DIAGS, ID_ALL_ON, ID_ALL_OFF];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "控件 id 必须唯一");
        assert!(!ids.contains(&ID_OK) && !ids.contains(&ID_CANCEL));
    }
}
