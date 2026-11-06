//! 解析器下载 — 在线解析器的管理框（`cm_parser_market`, 36011）与
//! 「打开没有解析器的文件」时的首开提示框。
//!
//! 两个框共用一套下载通道：[`download`](super::download) 的任务跑在后台
//! 线程，事件进 crossbeam channel，`WM_APP_NET` 投到**框自己的**窗口上
//! （不是主窗 —— 模态循环里主窗也在跑，但框内自收自发更直接）。下载中
//! 禁用操作按钮，进度写状态行；关窗即置取消位，线程在下一个数据块退出。
//!
//! * **管理框**：远端注册表整表 + 本机已装清单对成一张表 —— 未安装 /
//!   已装 v* / 有更新 → v*。下载、卸载立即生效（磁盘动作当场完成），
//!   变化名单在关窗时交回，由调用方「从注册表摘除 → 重扫 → 重着色」
//!   （重扫只增不改，见 `scan_syntax_plugins`，所以先摘再扫）。
//! * **提示框**：本机缓存里有这个扩展名的候选才弹（候选过滤在调用方），
//!   三个明确出口 —— 装、开管理框、不再询问（调用方持久化）。
//!
//! 形状照 [`syntaxdlg`](super::syntaxdlg)：`#32770` + 空 `DLGTEMPLATE`，
//! 状态对象留在调用方栈上、经 `GWLP_USERDATA` 交给 `dlg_proc`，模态由
//! [`prompt::run_modal_loop`] 驱动。列表头是列表控件的子窗口，深色主题
//! 下的自绘与 syntaxdlg 同一套（各自持有一份 —— 几行结构体不值得跨模块
//! 依赖，见那边 `ItemChanged` 的注释）。

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlggeom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossbeam_channel::Receiver;
use dg_render::theme::Theme;
use dg_syntax::{PluginInfo, RegistryDoc, RemotePackage};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, HGDIOBJ,
};
use windows::Win32::System::SystemServices::SS_ENDELLIPSIS;
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT,
    ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETHEADER, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR,
    LVM_SETTEXTCOLOR, LVN_ITEMCHANGED, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, NM_CUSTOMDRAW, NM_DBLCLK, NMLVCUSTOMDRAW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CreateDialogIndirectParamW, DestroyWindow, DLGTEMPLATE,
    EN_CHANGE, ES_AUTOHSCROLL, GetDlgItem, GetDlgItemTextW, GetClientRect, GetWindowLongPtrW,
    GWLP_USERDATA, IDCANCEL, IDOK, PostMessageW, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_NOTIFY, WM_SETFONT,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::download::{self, FetchResult, NetEvent, NetJob, RegistrySource};
use super::prompt::{CLASS_BUTTON, CLASS_STATIC, add_child, run_modal_loop, show_modal, wide};
use super::customdraw;
use super::view::instance;
use super::window::to_colorref;
use super::{chrome, ownerdraw, uifont};

/// 下载通道的唤醒消息：投到**发起任务的窗口**（框或主窗），收信方各自
/// drain 自己的 channel。0x4E 是本实现 WM_APP 段的下一个空位（0x4D 已是
/// 主题对话框的 `WM_THEME_DLG`）。
pub(super) const WM_APP_NET: u32 = WM_APP + 0x4E;

/// lang 表的对话框号（`[DLG730]` / `[DLG731]`）。
const DLG_MANAGER: u32 = 730;
const DLG_PROMPT: u32 = 731;

const CLASS_LIST: PCWSTR = w!("SysListView32");
const CLASS_EDIT: PCWSTR = w!("edit");

/// 空单元格占位符（与 syntaxdlg 同款）。
const NONE_TEXT: &str = "—";

/// `#32770` 外框样式（`WS_CLIPCHILDREN` 的原因见 syntaxdlg）。
const DLG_STYLE: u32 = WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;

/// 空 `DLGTEMPLATE`：三个尾随字是「无菜单、预定义 `#32770` 类、无标题」。
#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

/// 一个下载任务的通道 + 取消位。框内同时只跑一个任务（busy 期间禁按钮）。
struct Net {
    rx: Receiver<NetEvent>,
    cancel: Arc<AtomicBool>,
    busy: bool,
}

/// 发起一个后台任务，事件投到 `dlg`。缓存路径缺目录时退到临时目录 ——
/// 只是缓存，失败的写入下一次会重试。
fn start_net(dlg: HWND, job: NetJob, source: RegistrySource, install_root: PathBuf) -> Net {
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let cache = download::cache_path()
        .unwrap_or_else(|| std::env::temp_dir().join("DogEdit").join("parsercache.json"));
    // `HWND` 不是 `Send`：跨线程只带裸句柄值，闭包里再包回去。
    let target = dlg.0 as isize;
    download::spawn(
        job,
        source,
        cache,
        install_root,
        tx,
        move || {
            unsafe {
                let hwnd = HWND(target as *mut core::ffi::c_void);
                let _ = PostMessageW(Some(hwnd), WM_APP_NET, WPARAM(0), LPARAM(0));
            }
        },
        cancel.clone(),
    );
    Net {
        rx,
        cancel,
        busy: true,
    }
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        // KB 档向上取整：512 B 报 "1 KB" 比 "0 KB" 诚实。
        format!("{:.0} KB", (bytes as f64 / 1024.0).ceil())
    }
}

/// `[DLG?]` 词条，找不到用中文兜底（词条进 lang/*.lng 之前代码就能跑）。
fn dlg_txt(dlg: u32, ctl: u32, fallback: &str) -> String {
    crate::win32::lang::dlg(dlg, ctl).unwrap_or(fallback).to_string()
}

/// `[STRING]` 词条。
fn lang_str(id: u32, fallback: &'static str) -> String {
    crate::win32::lang::string(id).unwrap_or(fallback).to_string()
}

/// `[STRING]` 词条带参数。
fn lang_fmt(id: u32, args: &[&str], fallback: &str) -> String {
    crate::win32::lang::format(id, args).unwrap_or_else(|| fallback.to_string())
}

// ---------------------------------------------------------------------------
// 共用小件（与 syntaxdlg 同款；那份是私有的，不值得为几行抽公共模块）
// ---------------------------------------------------------------------------

unsafe fn item(dlg: HWND, id: i32) -> Option<HWND> {
    GetDlgItem(Some(dlg), id).ok()
}

unsafe fn set_text(ctl: HWND, text: &str) {
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(ctl, PCWSTR::from_raw(buf.as_ptr()));
}

unsafe fn enable_id(dlg: HWND, id: i32, on: bool) {
    if let Some(ctl) = item(dlg, id) {
        let _ = EnableWindow(ctl, on);
    }
}

unsafe fn edit_text(dlg: HWND, id: i32) -> String {
    let mut buf = [0u16; 256];
    let n = GetDlgItemTextW(dlg, id, &mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 一个子控件，带上对话框的字体（`add_child` 已按 dpi 缩放过矩形）。
#[allow(clippy::too_many_arguments)]
unsafe fn child(
    dlg: HWND,
    font: HFONT,
    class: PCWSTR,
    text: &str,
    id: i32,
    style: WINDOW_STYLE,
    rect: (i32, i32, i32, i32),
    dpi: f32,
) -> Option<HWND> {
    let w = wide(text);
    let ctl = add_child(dlg, class, PCWSTR::from_raw(w.as_ptr()), id, style, rect, dpi)?;
    let _ = SendMessageW(ctl, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    Some(ctl)
}

fn default_button() -> WINDOW_STYLE {
    WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32)
}

/// 对话框共用的外观件：字体、底色、文字色、表头分隔线色、列定义。
struct Look {
    dpi: i32,
    font: HFONT,
    bg: HBRUSH,
    bg_ref: COLORREF,
    ink: COLORREF,
    line: HBRUSH,
    /// 本框列表的列（表头自绘按 `dwItemSpec` 回查）。
    columns: Vec<(&'static str, i32)>,
}

impl Look {
    unsafe fn new(frame: HWND, theme: &Theme, columns: &[(&'static str, i32)]) -> Self {
        let dpi = uifont::window_dpi(frame);
        Self {
            dpi,
            font: uifont::create(dpi),
            bg: CreateSolidBrush(to_colorref(theme.background)),
            bg_ref: to_colorref(theme.background),
            ink: to_colorref(theme.foreground),
            line: CreateSolidBrush(to_colorref(theme.indent_line)),
            columns: columns.to_vec(),
        }
    }

    fn release(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            let _ = DeleteObject(HGDIOBJ(self.bg.0));
            let _ = DeleteObject(HGDIOBJ(self.line.0));
        }
    }
}

/// 框过程里 `WM_ERASEBKGND` 的整块擦底（先 `GetClientRect` 再进来）。
unsafe fn fill_background(look: &Look, hdc: HDC, rc: &RECT) {
    let _ = FillRect(hdc, rc, look.bg);
}

/// 报表列表的列 + 暗色三件套（底色/字色/文本底色）。列标题走 lang 反查。
unsafe fn init_list(list: HWND, look: &Look) {
    let ex = (LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP) as usize;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(ex)),
        Some(LPARAM(ex as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETBKCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(look.bg_ref.0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTBKCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(look.bg_ref.0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(look.ink.0 as isize)),
    );
    for (i, (title, width)) in look.columns.iter().enumerate() {
        let title = crate::win32::lang::trs(title);
        let mut text: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let mut col = LVCOLUMNW {
            mask: LVCF_WIDTH | LVCF_TEXT,
            cx: uifont::scaled(*width, look.dpi),
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            Some(WPARAM(i)),
            Some(LPARAM(&raw mut col as isize)),
        );
    }
}

unsafe fn insert_row(list: HWND, row: i32, cells: &[String]) {
    let mut name: Vec<u16> = cells[0].encode_utf16().chain([0]).collect();
    let item_lv = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        iSubItem: 0,
        pszText: PWSTR(name.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTITEMW,
        Some(WPARAM(0)),
        Some(LPARAM(&raw const item_lv as isize)),
    );
    for (col, text) in cells.iter().enumerate().skip(1) {
        let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let sub = LVITEMW {
            iSubItem: col as i32,
            pszText: PWSTR(buf.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_SETITEMTEXTW,
            Some(WPARAM(row as usize)),
            Some(LPARAM(&raw const sub as isize)),
        );
    }
}

/// 当前选中行（单选列表），没有选中返回 -1。
unsafe fn selected_row(list: HWND) -> i32 {
    let hit = SendMessageW(
        list,
        LVM_GETNEXTITEM,
        Some(WPARAM((-1isize) as usize)),
        Some(LPARAM(LVIS_SELECTED.0 as isize)),
    );
    hit.0 as i32
}

/// 表头自绘（syntaxdlg 同款）：`SysHeader32` 是**列表**的子窗口，它的
/// `NM_CUSTOMDRAW` 到不了对话框，得在列表的子类化里接住。
unsafe fn on_header_draw(look: &Look, lparam: LPARAM) -> isize {
    let cd = &mut *(lparam.0 as *const NMLVCUSTOMDRAW as *mut NMLVCUSTOMDRAW);
    let hdc = cd.nmcd.hdc;
    let stage = cd.nmcd.dwDrawStage.0;
    if stage == CDDS_PREPAINT.0 {
        let mut rc = RECT::default();
        if GetClientRect(cd.nmcd.hdr.hwndFrom, &mut rc).is_ok() {
            let _ = FillRect(hdc, &rc, look.bg);
        }
        return CDRF_NOTIFYITEMDRAW as isize;
    }
    if stage != CDDS_ITEMPREPAINT.0 {
        return CDRF_DODEFAULT as isize;
    }
    let Some((title, _)) = look.columns.get(cd.nmcd.dwItemSpec as usize) else {
        return CDRF_DODEFAULT as isize;
    };
    let title = crate::win32::lang::trs(title);
    let mut rc = cd.nmcd.rc;
    let _ = FillRect(hdc, &rc, look.bg);
    let divider = RECT {
        left: rc.right - 1,
        top: rc.top + uifont::scaled(3, look.dpi),
        right: rc.right,
        bottom: rc.bottom - uifont::scaled(3, look.dpi),
    };
    let _ = FillRect(hdc, &divider, look.line);
    rc.left += uifont::scaled(6, look.dpi);
    rc.right -= uifont::scaled(4, look.dpi);
    ownerdraw::draw_text(hdc, rc, look.font, look.ink, &title, ownerdraw::Align::Left(0));
    CDRF_SKIPDEFAULT as isize
}

/// 这个 `NM_CUSTOMDRAW` 是不是列表控件顺手建的那个表头。
unsafe fn is_header(list: HWND, hwnd: HWND) -> bool {
    let header = SendMessageW(list, LVM_GETHEADER, None, None);
    header.0 as isize == hwnd.0 as isize
}

/// 外框尺寸按客户端算，再居中压在编辑区上（偏上 1/3，同 markdlg）。
unsafe fn size_and_place(dlg: HWND, frame: HWND, dpi: i32, client: (i32, i32)) {
    dlggeom::size_and_place(
        dlg,
        frame,
        uifont::scaled(client.0, dpi),
        uifont::scaled(client.1, dpi),
        dpi as u32,
        DLG_STYLE,
    );
}

/// 列表的子类化 id（表头自绘挂在它上面）。
const LIST_SUBCLASS: usize = 1;

// ---------------------------------------------------------------------------
// 管理框
// ---------------------------------------------------------------------------

/// 控件 id。1200 段是本框自己的（同 syntaxdlg 的约定）。
const ID_SEARCH: i32 = 1200;
const ID_SOURCE: i32 = 1201;
const ID_REFRESH: i32 = 1202;
const ID_LIST: i32 = 1203;
const ID_STATUS: i32 = 1204;
const ID_INSTALL: i32 = 1205;
const ID_UNINSTALL: i32 = 1206;
const ID_IMPORT: i32 = 1207;
/// 关闭 = `IDOK`（回车走默认按钮）；Esc 在对话框管理器里合成 `IDCANCEL`。
const ID_CLOSE: i32 = IDOK.0;
const ID_ESC: i32 = IDCANCEL.0;

/// 客户端尺寸（100% px），列宽合计 = 列表宽（636）。
const M_CLIENT: (i32, i32) = (660, 410);
const RECT_SEARCH_LABEL: (i32, i32, i32, i32) = (12, 15, 40, 18);
const RECT_SEARCH: (i32, i32, i32, i32) = (56, 10, 200, 24);
const RECT_SOURCE: (i32, i32, i32, i32) = (264, 15, 296, 18);
const RECT_REFRESH: (i32, i32, i32, i32) = (568, 10, 80, 24);
const RECT_M_LIST: (i32, i32, i32, i32) = (12, 42, 636, 300);
const RECT_M_STATUS: (i32, i32, i32, i32) = (12, 348, 636, 18);
const RECT_M_INSTALL: (i32, i32, i32, i32) = (12, 376, 96, 24);
const RECT_M_UNINSTALL: (i32, i32, i32, i32) = (116, 376, 72, 24);
const RECT_M_IMPORT: (i32, i32, i32, i32) = (196, 376, 116, 24);
const RECT_M_CLOSE: (i32, i32, i32, i32) = (572, 376, 76, 24);

const MANAGER_COLUMNS: [(&str, i32); 6] = [
    ("名称", 96),
    ("显示名", 104),
    ("扩展名", 116),
    ("版本", 64),
    ("大小", 76),
    ("状态", 180),
];

/// 一行的可执行动作。
#[derive(Clone)]
enum Action {
    None,
    /// 下载并安装/更新这个远端包。
    Install(RemotePackage),
    /// 卸载这个本地落点的包。
    Uninstall(String),
}

/// 一行显示数据 + 动作。
struct Row {
    cells: [String; 6],
    action: Action,
}

/// 管理框的输入。
pub(super) struct ManagerRequest<'a> {
    pub frame: HWND,
    pub theme: &'a Theme,
    /// 注册表来源（`[Download] Registry` 的现值），显示与请求共用。
    pub source: RegistrySource,
    /// 本机缓存里的整表（可能没有 —— 框会自己拉一次）。
    pub registry: Option<RegistryDoc>,
    /// 已装清单（`plugin_list()` 快照）。
    pub installed: Vec<PluginInfo>,
    /// 用户可写可卸载的解析器落点。
    pub user_root: PathBuf,
}

/// 关窗时交回的变化名单：装/更/卸都算。框是**立即生效**制（磁盘动作
/// 当场完成），取消也照交；调用方负责摘除 → 重扫 → 重着色。
pub(super) struct ManagerResult {
    pub changed: Vec<String>,
}

struct ManagerDlg<'a> {
    frame: HWND,
    theme: &'a Theme,
    look: Look,
    source: RegistrySource,
    registry: Option<RegistryDoc>,
    installed: Vec<PluginInfo>,
    user_root: PathBuf,
    /// 筛选后的行序表：列表行号 → 行（动作按它回查）。
    shown: Vec<Row>,
    /// 正在安装的包（`InstallDone` 时用来更新快照）。
    pending: Option<RemotePackage>,
    net: Option<Net>,
    changed: Vec<String>,
    destroyed: bool,
}

unsafe fn manager_state(hwnd: HWND) -> &'static mut ManagerDlg<'static> {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ManagerDlg<'static>)
}

/// 行是否通过筛选（名称/显示名/扩展名的包含匹配，大小写不敏感）。
fn row_matches(r: &Row, filter: &str) -> bool {
    let f = filter.to_lowercase();
    f.is_empty()
        || r.cells[0].to_lowercase().contains(&f)
        || r.cells[1].to_lowercase().contains(&f)
        || r.cells[2].to_lowercase().contains(&f)
}

fn exts_or_none(exts: &[String]) -> String {
    if exts.is_empty() {
        NONE_TEXT.to_string()
    } else {
        exts.join(" ")
    }
}

/// 行动作是否可卸载：包目录必须落在 `user_root` 之下 —— exe 旁与工作
/// 目录的包不归这个框管（可能只读）。
fn uninstallable(installed: &PluginInfo, user_root: &std::path::Path) -> bool {
    let Some(dir) = installed.dll.parent() else {
        return false;
    };
    let (Ok(dir), Ok(root)) = (dir.canonicalize(), user_root.canonicalize()) else {
        return false;
    };
    dir.starts_with(&root)
}

fn build_rows(
    registry: Option<&RegistryDoc>,
    installed: &[PluginInfo],
    user_root: &std::path::Path,
) -> Vec<Row> {
    let mut rows = Vec::new();
    match registry {
        Some(doc) => {
            for p in &doc.packages {
                let known = installed.iter().find(|i| i.name == p.name);
                let (status, action) = match known {
                    None => (lang_str(40133, "未安装"), Action::Install(p.clone())),
                    Some(i) => {
                        let newer = matches!(
                            dg_syntax::remote::version_cmp(&p.version, &i.version),
                            Some(std::cmp::Ordering::Greater)
                        );
                        if newer {
                            (
                                lang_fmt(40131, &[&p.version], &format!("有更新 → v{}", p.version)),
                                Action::Install(p.clone()),
                            )
                        } else {
                            (
                                lang_fmt(40132, &[&i.version], &format!("已装 v{}", i.version)),
                                Action::None,
                            )
                        }
                    }
                };
                rows.push(Row {
                    cells: [
                        p.name.clone(),
                        p.display.clone(),
                        exts_or_none(&p.extensions),
                        p.version.clone(),
                        size_text(p.size),
                        status,
                    ],
                    action,
                });
            }
            // 只在本地、注册表没有的包。
            for i in installed {
                if doc.find(&i.name).is_none() {
                    rows.push(local_row(i, user_root));
                }
            }
        }
        None => {
            for i in installed {
                rows.push(local_row(i, user_root));
            }
        }
    }
    rows
}

fn local_row(i: &PluginInfo, user_root: &std::path::Path) -> Row {
    Row {
        cells: [
            i.name.clone(),
            i.display.clone(),
            exts_or_none(&i.extensions),
            i.version.clone(),
            NONE_TEXT.to_string(),
            lang_str(40134, "已装（本地）"),
        ],
        action: if uninstallable(i, user_root) {
            Action::Uninstall(i.name.clone())
        } else {
            Action::None
        },
    }
}

/// 模态跑一遍，关窗交回变化名单。
pub(super) unsafe fn show_manager(req: ManagerRequest<'_>) -> ManagerResult {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES,
        ..Default::default()
    });
    let mut st = ManagerDlg {
        frame: req.frame,
        theme: req.theme,
        look: Look::new(req.frame, req.theme, &MANAGER_COLUMNS),
        source: req.source,
        registry: req.registry,
        installed: req.installed,
        user_root: req.user_root,
        shown: Vec::new(),
        pending: None,
        net: None,
        changed: Vec::new(),
        destroyed: false,
    };
    unsafe { run_manager(&mut st) };
    st.look.release();
    ManagerResult {
        changed: std::mem::take(&mut st.changed),
    }
}

unsafe fn run_manager(st: &mut ManagerDlg<'_>) {
    let tmpl = Template {
        head: DLGTEMPLATE {
            style: DLG_STYLE,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 64,
            cy: 64,
        },
        tail: [0, 0, 0],
    };
    let param = LPARAM(st as *mut ManagerDlg<'_> as isize);
    let Ok(dlg) = CreateDialogIndirectParamW(
        Some(instance()),
        &tmpl.head,
        Some(st.frame),
        Some(manager_proc),
        param,
    ) else {
        return;
    };
    let title = wide(&dlg_txt(DLG_MANAGER, 0, "解析器下载"));
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));
    let dark = chrome::theme_is_dark(st.theme);
    chrome::set_chrome_theme(st.theme);
    chrome::title_bar(dlg, dark);
    build_manager_children(dlg, st);
    chrome::dark_theme_children(dlg, dark);
    size_and_place(dlg, st.frame, st.look.dpi, M_CLIENT);
    refresh_source_line(dlg, st);
    refill_manager(dlg, st);
    update_buttons(dlg, st);
    // 还没有整表就先拉一次（首次使用 / 换源后都是这条路）。
    if st.registry.is_none() {
        start_fetch(dlg, st);
    }

    show_modal(dlg, st.frame);
    if let Some(list) = item(dlg, ID_LIST) {
        let _ = SetFocus(Some(list));
    }
    run_modal_loop(dlg, st.frame, std::ptr::addr_of!(st.destroyed));
}

unsafe fn build_manager_children(dlg: HWND, st: &mut ManagerDlg<'_>) {
    let dpi = st.look.dpi as f32 / 96.0;
    let font = st.look.font;
    let ellipsis = WINDOW_STYLE(SS_ENDELLIPSIS.0);
    let _ = child(
        dlg,
        font,
        CLASS_STATIC,
        &crate::win32::lang::trs("筛选:"),
        0,
        WINDOW_STYLE(0),
        RECT_SEARCH_LABEL,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_EDIT,
        "",
        ID_SEARCH,
        WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32),
        RECT_SEARCH,
        dpi,
    );
    let _ = child(dlg, font, CLASS_STATIC, "", ID_SOURCE, ellipsis, RECT_SOURCE, dpi);
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_MANAGER, ID_REFRESH as u32, "刷新(&R)"),
        ID_REFRESH,
        WS_TABSTOP,
        RECT_REFRESH,
        dpi,
    );
    let list = child(
        dlg,
        font,
        CLASS_LIST,
        "",
        ID_LIST,
        WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS),
        ),
        RECT_M_LIST,
        dpi,
    );
    if let Some(list) = list {
        init_list(list, &st.look);
        // 子类过程的 dwRefData 必须是状态指针：表头自绘要从它读 `look`。
        let _ = SetWindowSubclass(
            list,
            Some(manager_list_proc),
            LIST_SUBCLASS,
            st as *mut ManagerDlg<'_> as usize,
        );
    }
    let _ = child(dlg, font, CLASS_STATIC, "", ID_STATUS, ellipsis, RECT_M_STATUS, dpi);
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_MANAGER, ID_INSTALL as u32, "下载/更新(&D)"),
        ID_INSTALL,
        WS_TABSTOP,
        RECT_M_INSTALL,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_MANAGER, ID_UNINSTALL as u32, "卸载(&U)"),
        ID_UNINSTALL,
        WS_TABSTOP,
        RECT_M_UNINSTALL,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_MANAGER, ID_IMPORT as u32, "从文件安装(&I)..."),
        ID_IMPORT,
        WS_TABSTOP,
        RECT_M_IMPORT,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_MANAGER, 1, "关闭"),
        ID_CLOSE,
        default_button(),
        RECT_M_CLOSE,
        dpi,
    );
}

unsafe fn refresh_source_line(dlg: HWND, st: &ManagerDlg<'_>) {
    if let Some(ctl) = item(dlg, ID_SOURCE) {
        let text = if st.registry.is_some() {
            lang_fmt(40120, &[&st.source.describe()], &format!("来源：{}", st.source.describe()))
        } else {
            lang_str(40122, "正在获取列表…")
        };
        set_text(ctl, &text);
    }
}

/// 按筛选词重建列表；行序表同步存进 `st.shown`。
unsafe fn refill_manager(dlg: HWND, st: &mut ManagerDlg<'_>) {
    let Some(list) = item(dlg, ID_LIST) else {
        return;
    };
    let filter = edit_text(dlg, ID_SEARCH);
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, None, None);
    let rows = build_rows(st.registry.as_ref(), &st.installed, &st.user_root);
    st.shown.clear();
    let mut shown = 0usize;
    for r in rows {
        if !row_matches(&r, &filter) {
            continue;
        }
        insert_row(list, shown as i32, &r.cells);
        st.shown.push(r);
        shown += 1;
    }
    if let Some(ctl) = item(dlg, ID_STATUS) {
        let total = st.shown.len();
        set_text(
            ctl,
            &lang_fmt(40121, &[&total.to_string()], &format!("{total} 个解析器")),
        );
    }
}

/// 选中行的动作（没有选中 → `None`）。
unsafe fn current_action<'a>(dlg: HWND, st: &'a ManagerDlg<'_>) -> Option<&'a Action> {
    let list = item(dlg, ID_LIST)?;
    let row = selected_row(list);
    if row < 0 {
        return None;
    }
    st.shown.get(row as usize).map(|r| &r.action)
}

/// busy 期间四个操作按钮全禁；空闲时按选中行动作给「下载/更新」「卸载」。
unsafe fn update_buttons(dlg: HWND, st: &ManagerDlg<'_>) {
    let busy = st.net.as_ref().is_some_and(|n| n.busy);
    let (can_install, can_uninstall) = if busy {
        (false, false)
    } else {
        match current_action(dlg, st) {
            Some(Action::Install(_)) => (true, false),
            Some(Action::Uninstall(_)) => (false, true),
            _ => (false, false),
        }
    };
    enable_id(dlg, ID_REFRESH, !busy);
    enable_id(dlg, ID_IMPORT, !busy);
    enable_id(dlg, ID_INSTALL, can_install);
    enable_id(dlg, ID_UNINSTALL, can_uninstall);
}

unsafe extern "system" fn manager_list_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    dlg: usize,
) -> LRESULT {
    if msg == WM_NOTIFY {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.code == NM_CUSTOMDRAW && is_header(hwnd, hdr.hwndFrom) {
            let st = &*(dlg as *const ManagerDlg<'_>);
            return LRESULT(on_header_draw(&st.look, lparam));
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn manager_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            // 1 = 管理器自己挑初始焦点。
            1
        }
        WM_APP_NET => {
            manager_net(hwnd);
            1
        }
        WM_COMMAND => manager_command(hwnd, wparam),
        WM_NOTIFY => manager_notify(hwnd, lparam),
        WM_ERASEBKGND => {
            let st = manager_state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                fill_background(&st.look, HDC(wparam.0 as *mut _), &rc);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = manager_state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.look.ink, st.look.bg_ref, st.look.bg)
        }
        WM_CLOSE => {
            close_with_cancel(hwnd);
            1
        }
        WM_DESTROY => {
            manager_state(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => 0,
    }
}

/// 关窗：下载中还挂着就先置取消位，线程在下一个数据块退出。
unsafe fn close_with_cancel(dlg: HWND) {
    if let Some(net) = manager_state(dlg).net.as_ref() {
        if net.busy {
            net.cancel.store(true, Ordering::Relaxed);
        }
    }
    let _ = DestroyWindow(dlg);
}

unsafe fn manager_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let notify = ((wparam.0 >> 16) & 0xFFFF) as u16;
    match id {
        ID_SEARCH if notify == EN_CHANGE as u16 => {
            let st = manager_state(dlg);
            refill_manager(dlg, st);
            update_buttons(dlg, st);
            1
        }
        ID_REFRESH => {
            start_fetch(dlg, manager_state(dlg));
            1
        }
        ID_INSTALL => {
            let st = manager_state(dlg);
            if let Some(Action::Install(pkg)) = current_action(dlg, st).cloned() {
                start_install(dlg, st, pkg);
            }
            1
        }
        ID_UNINSTALL => {
            let st = manager_state(dlg);
            if let Some(Action::Uninstall(name)) = current_action(dlg, st).cloned() {
                uninstall_selected(dlg, st, &name);
            }
            1
        }
        ID_IMPORT => {
            pick_and_install_zip(dlg, manager_state(dlg));
            1
        }
        ID_CLOSE | ID_ESC => {
            close_with_cancel(dlg);
            1
        }
        _ => 0,
    }
}

/// 列表通知：双击 = 下载/更新；选择变化 = 刷按钮可用性。
unsafe fn manager_notify(dlg: HWND, lparam: LPARAM) -> isize {
    let hdr = &*(lparam.0 as *const NMHDR);
    if hdr.idFrom as i32 != ID_LIST {
        return 0;
    }
    if hdr.code == NM_DBLCLK {
        let st = manager_state(dlg);
        if let Some(Action::Install(pkg)) = current_action(dlg, st).cloned() {
            start_install(dlg, st, pkg);
        }
        return 1;
    }
    if hdr.code == LVN_ITEMCHANGED {
        update_buttons(dlg, manager_state(dlg));
        return 0;
    }
    0
}

unsafe fn start_fetch(dlg: HWND, st: &mut ManagerDlg<'_>) {
    if st.net.as_ref().is_some_and(|n| n.busy) {
        return;
    }
    st.net = Some(start_net(
        dlg,
        NetJob::FetchRegistry,
        st.source.clone(),
        st.user_root.clone(),
    ));
    update_buttons(dlg, st);
    if let Some(ctl) = item(dlg, ID_STATUS) {
        set_text(ctl, &lang_str(40122, "正在获取列表…"));
    }
}

unsafe fn start_install(dlg: HWND, st: &mut ManagerDlg<'_>, pkg: RemotePackage) {
    if st.net.as_ref().is_some_and(|n| n.busy) {
        return;
    }
    st.pending = Some(pkg.clone());
    st.net = Some(start_net(
        dlg,
        NetJob::Install {
            pkg: Box::new(pkg.clone()),
        },
        st.source.clone(),
        st.user_root.clone(),
    ));
    update_buttons(dlg, st);
    if let Some(ctl) = item(dlg, ID_STATUS) {
        set_text(
            ctl,
            &lang_fmt(40125, &[&pkg.display], &format!("正在下载 {}…", pkg.display)),
        );
    }
}

unsafe fn uninstall_selected(dlg: HWND, st: &mut ManagerDlg<'_>, name: &str) {
    match dg_syntax::install::uninstall_package(&st.user_root, name) {
        Ok(()) => {
            st.changed.push(name.to_string());
            st.installed.retain(|i| i.name != name);
            refill_manager(dlg, st);
            update_buttons(dlg, st);
            if let Some(ctl) = item(dlg, ID_STATUS) {
                set_text(ctl, &lang_fmt(40130, &[name], &format!("已卸载 {name}。")));
            }
        }
        Err(e) => {
            if let Some(ctl) = item(dlg, ID_STATUS) {
                set_text(ctl, &e.to_string());
            }
        }
    }
}

/// 「从文件安装」：拾取 zip → 后台任务装到用户落点。
unsafe fn pick_and_install_zip(dlg: HWND, st: &mut ManagerDlg<'_>) {
    let Some(zip) = pick_zip_file(dlg) else {
        return;
    };
    if st.net.as_ref().is_some_and(|n| n.busy) {
        return;
    }
    st.net = Some(start_net(
        dlg,
        NetJob::InstallLocal { zip: PathBuf::from(&zip) },
        st.source.clone(),
        st.user_root.clone(),
    ));
    update_buttons(dlg, st);
    if let Some(ctl) = item(dlg, ID_STATUS) {
        set_text(ctl, &lang_str(40136, "正在从文件安装…"));
    }
}

unsafe fn pick_zip_file(dlg: HWND) -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OPENFILENAMEW,
    };
    let mut file = [0u16; 520];
    let filter: Vec<u16> = "解析器包\0*.zip\0所有文件\0*.*\0\0".encode_utf16().collect();
    let title: Vec<u16> = crate::win32::lang::trs("从文件安装")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: dlg,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut ofn).as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&file[..len]))
}

/// drain 本框的下载事件。
unsafe fn manager_net(dlg: HWND) {
    let st = manager_state(dlg);
    // 取走所有权再 drain：处理事件要自由借用 st，收完按 busy 决定还回不还。
    let Some(mut net) = st.net.take() else {
        return;
    };
    while let Ok(ev) = net.rx.try_recv() {
        match ev {
            NetEvent::Progress { done, total } => {
                if let Some(ctl) = item(dlg, ID_STATUS) {
                    set_text(ctl, &progress_text(done, total));
                }
            }
            NetEvent::Registry(result) => {
                net.busy = false;
                match result {
                    Ok(FetchResult::Fresh(cache)) => {
                        st.registry = Some(cache.registry);
                        refresh_source_line(dlg, st);
                        refill_manager(dlg, st);
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(ctl, &lang_str(40123, "列表已更新。"));
                        }
                    }
                    Ok(FetchResult::Unchanged) => {
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(ctl, &lang_str(40127, "列表已是最新。"));
                        }
                    }
                    Err(e) => {
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(ctl, &lang_fmt(40124, &[&e], &format!("获取列表失败：{e}")));
                        }
                    }
                }
            }
            NetEvent::InstallDone { name, result } => {
                net.busy = false;
                let pkg = st.pending.take();
                match result {
                    Ok(target) => {
                        st.changed.push(name.clone());
                        // 本框视角的「已装」：把新版本顶进快照（真注册表由
                        // 调用方在关窗后摘除 + 重扫重建）。
                        if let Some(p) = pkg.as_ref() {
                            upsert_installed(&mut st.installed, p, &target);
                        }
                        refill_manager(dlg, st);
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            let ver = pkg.as_ref().map(|p| p.version.clone()).unwrap_or_default();
                            set_text(
                                ctl,
                                &lang_fmt(
                                    40129,
                                    &[&name, &ver],
                                    &format!("已安装 {name} {ver}，新打开的文件立即生效。"),
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(
                                ctl,
                                &lang_fmt(40128, &[&name, &e], &format!("安装 {name} 失败：{e}")),
                            );
                        }
                    }
                }
            }
            NetEvent::InstallLocalDone { result, .. } => {
                net.busy = false;
                match result {
                    Ok((name, version)) => {
                        st.changed.push(name.clone());
                        // 版本与显示名从 manifest 认定：快照里以最低限度登记，
                        // 关窗后的重扫会给全字段。
                        upsert_installed_simple(&mut st.installed, &name, &version);
                        refill_manager(dlg, st);
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(
                                ctl,
                                &lang_fmt(
                                    40129,
                                    &[&name, &version],
                                    &format!("已安装 {name} {version}。"),
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        update_buttons(dlg, st);
                        if let Some(ctl) = item(dlg, ID_STATUS) {
                            set_text(
                                ctl,
                                &lang_fmt(40137, &[&e], &format!("从文件安装失败：{e}")),
                            );
                        }
                    }
                }
            }
            NetEvent::Query { .. } => {}
        }
    }
    if net.busy {
        st.net = Some(net);
    }
}

fn upsert_installed(installed: &mut Vec<PluginInfo>, pkg: &RemotePackage, target: &std::path::Path) {
    installed.retain(|i| i.name != pkg.name);
    installed.push(PluginInfo {
        name: pkg.name.clone(),
        display: pkg.display.clone(),
        dll: target.join(format!("{}.dll", pkg.name)),
        version: pkg.version.clone(),
        extensions: pkg.extensions.clone(),
        queries: Vec::new(),
        enabled: true,
        state: dg_syntax::PluginState::Unloaded,
    });
}

fn upsert_installed_simple(installed: &mut Vec<PluginInfo>, name: &str, version: &str) {
    if let Some(i) = installed.iter_mut().find(|i| i.name == name) {
        i.version = version.to_string();
        return;
    }
    installed.push(PluginInfo {
        name: name.to_string(),
        display: name.to_string(),
        dll: PathBuf::from(name),
        version: version.to_string(),
        extensions: Vec::new(),
        queries: Vec::new(),
        enabled: true,
        state: dg_syntax::PluginState::Unloaded,
    });
}

fn progress_text(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => {
            let pct = format!("{}%", (done * 100 / total).min(100));
            // `%` 在 lang::format 里不是转义符，百分号由参数带进来。
            let plain = format!("{} / {}（{}）", size_text(done), size_text(total), pct);
            lang_fmt(40126, &[&size_text(done), &size_text(total), &pct], &plain)
        }
        _ => lang_fmt(40135, &[&size_text(done)], &format!("已下载 {}", size_text(done))),
    }
}

// ---------------------------------------------------------------------------
// 首开提示框
// ---------------------------------------------------------------------------

/// 控件 id。1300 段，避免与管理框的 1200 段混看。
const P_MSG: i32 = 1300;
const P_LIST: i32 = 1301;
const P_STATUS: i32 = 1302;
const P_INSTALL: i32 = 1303;
const P_MANAGER: i32 = 1304;
const P_NEVER: i32 = 1305;
const P_CANCEL: i32 = IDCANCEL.0;

/// 客户端尺寸（100% px），列宽合计 = 列表宽（516）。
const P_CLIENT: (i32, i32) = (540, 232);
const RECT_P_MSG: (i32, i32, i32, i32) = (12, 10, 516, 18);
const RECT_P_LIST: (i32, i32, i32, i32) = (12, 34, 516, 130);
const RECT_P_STATUS: (i32, i32, i32, i32) = (12, 170, 516, 18);
const RECT_P_INSTALL: (i32, i32, i32, i32) = (12, 198, 110, 24);
const RECT_P_MANAGER: (i32, i32, i32, i32) = (132, 198, 96, 24);
const RECT_P_NEVER: (i32, i32, i32, i32) = (240, 198, 96, 24);
const RECT_P_CANCEL: (i32, i32, i32, i32) = (456, 198, 72, 24);

const PROMPT_COLUMNS: [(&str, i32); 4] = [
    ("显示名", 120),
    ("版本", 70),
    ("大小", 90),
    ("简介", 236),
];

/// 提示框的输入。
pub(super) struct PromptRequest<'a> {
    pub frame: HWND,
    pub theme: &'a Theme,
    /// 触发提示的扩展名（小写、无点）。
    pub ext: String,
    /// 候选（调用方已经按 ABI 与本机缓存过滤过）。
    pub candidates: Vec<RemotePackage>,
    pub source: RegistrySource,
    pub install_root: PathBuf,
}

/// 提示框的出口。`None` = 取消/关窗（调用方记「本会话问过」即可）。
pub(super) enum PromptOutcome {
    /// 已装好的包名（调用方摘除旧表 → 重扫 → 重着色）。
    Installed(String),
    /// 用户要开管理框。
    OpenManager,
    /// 不再询问这个扩展名（调用方写 `[Download] Dismissed`）。
    NeverAsk,
}

struct PromptDlg<'a> {
    frame: HWND,
    theme: &'a Theme,
    look: Look,
    ext: String,
    candidates: Vec<RemotePackage>,
    source: RegistrySource,
    install_root: PathBuf,
    net: Option<Net>,
    result: Option<PromptOutcome>,
    destroyed: bool,
}

unsafe fn prompt_state(hwnd: HWND) -> &'static mut PromptDlg<'static> {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptDlg<'static>)
}

/// 模态跑一遍；`None` = 取消/关窗。
pub(super) unsafe fn show_prompt(req: PromptRequest<'_>) -> Option<PromptOutcome> {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES,
        ..Default::default()
    });
    let mut st = PromptDlg {
        frame: req.frame,
        theme: req.theme,
        look: Look::new(req.frame, req.theme, &PROMPT_COLUMNS),
        ext: req.ext,
        candidates: req.candidates,
        source: req.source,
        install_root: req.install_root,
        net: None,
        result: None,
        destroyed: false,
    };
    unsafe { run_prompt(&mut st) };
    st.look.release();
    st.result
}

unsafe fn run_prompt(st: &mut PromptDlg<'_>) {
    let tmpl = Template {
        head: DLGTEMPLATE {
            style: DLG_STYLE,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 64,
            cy: 64,
        },
        tail: [0, 0, 0],
    };
    let param = LPARAM(st as *mut PromptDlg<'_> as isize);
    let Ok(dlg) = CreateDialogIndirectParamW(
        Some(instance()),
        &tmpl.head,
        Some(st.frame),
        Some(prompt_proc),
        param,
    ) else {
        return;
    };
    let title = wide(&dlg_txt(DLG_PROMPT, 0, "下载解析器"));
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));
    let dark = chrome::theme_is_dark(st.theme);
    chrome::set_chrome_theme(st.theme);
    chrome::title_bar(dlg, dark);
    build_prompt_children(dlg, st);
    chrome::dark_theme_children(dlg, dark);
    size_and_place(dlg, st.frame, st.look.dpi, P_CLIENT);
    if let Some(list) = item(dlg, P_LIST) {
        init_list(list, &st.look);
        let _ = SetWindowSubclass(
            list,
            Some(prompt_list_proc),
            LIST_SUBCLASS,
            st as *mut PromptDlg<'_> as usize,
        );
        for (row, p) in st.candidates.iter().enumerate() {
            insert_row(
                list,
                row as i32,
                &[
                    p.display.clone(),
                    p.version.clone(),
                    size_text(p.size),
                    if p.description.is_empty() {
                        NONE_TEXT.to_string()
                    } else {
                        p.description.clone()
                    },
                ],
            );
        }
    }
    if let Some(ctl) = item(dlg, P_MSG) {
        set_text(
            ctl,
            &lang_fmt(
                40138,
                &[&st.ext],
                &format!("未找到 *.{0} 的解析器。以下解析器支持该类型：", st.ext),
            ),
        );
    }

    show_modal(dlg, st.frame);
    if let Some(list) = item(dlg, P_LIST) {
        let _ = SetFocus(Some(list));
    }
    run_modal_loop(dlg, st.frame, std::ptr::addr_of!(st.destroyed));
}

unsafe fn build_prompt_children(dlg: HWND, st: &mut PromptDlg<'_>) {
    let dpi = st.look.dpi as f32 / 96.0;
    let font = st.look.font;
    let ellipsis = WINDOW_STYLE(SS_ENDELLIPSIS.0);
    let _ = child(dlg, font, CLASS_STATIC, "", P_MSG, ellipsis, RECT_P_MSG, dpi);
    let list = child(
        dlg,
        font,
        CLASS_LIST,
        "",
        P_LIST,
        WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS),
        ),
        RECT_P_LIST,
        dpi,
    );
    if let Some(list) = list {
        init_list(list, &st.look);
        let _ = SetWindowSubclass(
            list,
            Some(prompt_list_proc),
            LIST_SUBCLASS,
            st as *mut PromptDlg<'_> as usize,
        );
    }
    let _ = child(dlg, font, CLASS_STATIC, "", P_STATUS, ellipsis, RECT_P_STATUS, dpi);
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_PROMPT, P_INSTALL as u32, "下载并安装(&D)"),
        P_INSTALL,
        default_button(),
        RECT_P_INSTALL,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_PROMPT, P_MANAGER as u32, "管理(&M)..."),
        P_MANAGER,
        WS_TABSTOP,
        RECT_P_MANAGER,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_PROMPT, P_NEVER as u32, "不再询问(&N)"),
        P_NEVER,
        WS_TABSTOP,
        RECT_P_NEVER,
        dpi,
    );
    let _ = child(
        dlg,
        font,
        CLASS_BUTTON,
        &dlg_txt(DLG_PROMPT, 2, "取消"),
        P_CANCEL,
        WS_TABSTOP,
        RECT_P_CANCEL,
        dpi,
    );
}

unsafe extern "system" fn prompt_list_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    dlg: usize,
) -> LRESULT {
    if msg == WM_NOTIFY {
        let hdr = &*(lparam.0 as *const NMHDR);
        if hdr.code == NM_CUSTOMDRAW && is_header(hwnd, hdr.hwndFrom) {
            let st = &*(dlg as *const PromptDlg<'_>);
            return LRESULT(on_header_draw(&st.look, lparam));
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            1
        }
        WM_APP_NET => {
            prompt_net(hwnd);
            1
        }
        WM_COMMAND => prompt_command(hwnd, wparam),
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if hdr.idFrom as i32 == P_LIST && hdr.code == NM_DBLCLK {
                prompt_install(hwnd);
                1
            } else {
                0
            }
        }
        WM_ERASEBKGND => {
            let st = prompt_state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                fill_background(&st.look, HDC(wparam.0 as *mut _), &rc);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = prompt_state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.look.ink, st.look.bg_ref, st.look.bg)
        }
        WM_CLOSE => {
            close_prompt(hwnd);
            1
        }
        WM_DESTROY => {
            prompt_state(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => 0,
    }
}

unsafe fn close_prompt(dlg: HWND) {
    if let Some(net) = prompt_state(dlg).net.as_ref() {
        if net.busy {
            net.cancel.store(true, Ordering::Relaxed);
        }
    }
    let _ = DestroyWindow(dlg);
}

unsafe fn prompt_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    match id {
        P_INSTALL => {
            prompt_install(dlg);
            1
        }
        P_MANAGER => {
            prompt_state(dlg).result = Some(PromptOutcome::OpenManager);
            let _ = DestroyWindow(dlg);
            1
        }
        P_NEVER => {
            prompt_state(dlg).result = Some(PromptOutcome::NeverAsk);
            let _ = DestroyWindow(dlg);
            1
        }
        P_CANCEL => {
            close_prompt(dlg);
            1
        }
        _ => 0,
    }
}

/// 下载并安装选中的候选（没选中就第一个）。
unsafe fn prompt_install(dlg: HWND) {
    let st = prompt_state(dlg);
    if st.net.as_ref().is_some_and(|n| n.busy) {
        return;
    }
    let row = match item(dlg, P_LIST) {
        Some(list) => selected_row(list),
        None => return,
    };
    let pkg = match st
        .candidates
        .get(if row >= 0 { row as usize } else { 0 })
    {
        Some(p) => p.clone(),
        None => return,
    };
    st.net = Some(start_net(
        dlg,
        NetJob::Install {
            pkg: Box::new(pkg.clone()),
        },
        st.source.clone(),
        st.install_root.clone(),
    ));
    for id in [P_INSTALL, P_MANAGER, P_NEVER] {
        enable_id(dlg, id, false);
    }
    if let Some(ctl) = item(dlg, P_STATUS) {
        set_text(
            ctl,
            &lang_fmt(40125, &[&pkg.display], &format!("正在下载 {}…", pkg.display)),
        );
    }
}

/// drain 本框的下载事件：进度写状态行；装完把结果交回调用方关窗。
unsafe fn prompt_net(dlg: HWND) {
    let st = prompt_state(dlg);
    // 取走所有权再 drain（同 manager_net 的借用理由）。
    let Some(mut net) = st.net.take() else {
        return;
    };
    while let Ok(ev) = net.rx.try_recv() {
        match ev {
            NetEvent::Progress { done, total } => {
                if let Some(ctl) = item(dlg, P_STATUS) {
                    set_text(ctl, &progress_text(done, total));
                }
            }
            NetEvent::InstallDone { name, result } => {
                net.busy = false;
                match result {
                    Ok(_) => {
                        st.result = Some(PromptOutcome::Installed(name));
                        let _ = DestroyWindow(dlg);
                        return;
                    }
                    Err(e) => {
                        if let Some(ctl) = item(dlg, P_STATUS) {
                            set_text(
                                ctl,
                                &lang_fmt(40128, &[&name, &e], &format!("安装 {name} 失败：{e}")),
                            );
                        }
                        for id in [P_INSTALL, P_MANAGER, P_NEVER] {
                            enable_id(dlg, id, true);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if net.busy {
        st.net = Some(net);
    }
}

// ---------------------------------------------------------------------------
// 测试：网格与纯函数（Win32 不可用照测 —— 全是常量与纯函数）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 网格是手算的：控件不能出界，同一行按钮互不叠，列宽填满列表。
    #[test]
    fn manager_grid_fits_the_client() {
        assert_eq!(M_CLIENT, (660, 410));
        let rects = [
            RECT_SEARCH_LABEL,
            RECT_SEARCH,
            RECT_SOURCE,
            RECT_REFRESH,
            RECT_M_LIST,
            RECT_M_STATUS,
            RECT_M_INSTALL,
            RECT_M_UNINSTALL,
            RECT_M_IMPORT,
            RECT_M_CLOSE,
        ];
        for (x, y, w, h) in rects {
            assert!(x + w <= M_CLIENT.0, "({x},{y},{w},{h}) 超出宽度");
            assert!(y + h <= M_CLIENT.1, "({x},{y},{w},{h}) 超出高度");
        }
        let mut buttons = [RECT_M_INSTALL, RECT_M_UNINSTALL, RECT_M_IMPORT, RECT_M_CLOSE];
        buttons.sort_by_key(|r| r.0);
        for pair in buttons.windows(2) {
            assert!(pair[0].0 + pair[0].2 <= pair[1].0, "{pair:?} 重叠");
        }
        let sum: i32 = MANAGER_COLUMNS.iter().map(|(_, w)| w).sum();
        assert_eq!(sum, RECT_M_LIST.2, "列宽之和必须等于列表宽");
    }

    #[test]
    fn prompt_grid_fits_the_client() {
        assert_eq!(P_CLIENT, (540, 232));
        let rects = [
            RECT_P_MSG,
            RECT_P_LIST,
            RECT_P_STATUS,
            RECT_P_INSTALL,
            RECT_P_MANAGER,
            RECT_P_NEVER,
            RECT_P_CANCEL,
        ];
        for (x, y, w, h) in rects {
            assert!(x + w <= P_CLIENT.0, "({x},{y},{w},{h}) 超出宽度");
            assert!(y + h <= P_CLIENT.1, "({x},{y},{w},{h}) 超出高度");
        }
        let mut buttons = [RECT_P_INSTALL, RECT_P_MANAGER, RECT_P_NEVER, RECT_P_CANCEL];
        buttons.sort_by_key(|r| r.0);
        for pair in buttons.windows(2) {
            assert!(pair[0].0 + pair[0].2 <= pair[1].0, "{pair:?} 重叠");
        }
        let sum: i32 = PROMPT_COLUMNS.iter().map(|(_, w)| w).sum();
        assert_eq!(sum, RECT_P_LIST.2, "列宽之和必须等于列表宽");
    }

    /// id 段不许和 `IDOK`/`IDCANCEL` 撞，否则回车/Esc 会打到普通按钮上。
    #[test]
    fn control_ids_do_not_collide_with_the_dialog_managers() {
        let m = [
            ID_SEARCH, ID_SOURCE, ID_REFRESH, ID_LIST, ID_STATUS, ID_INSTALL, ID_UNINSTALL,
            ID_IMPORT,
        ];
        let p = [P_MSG, P_LIST, P_STATUS, P_INSTALL, P_MANAGER, P_NEVER];
        for ids in [&m[..], &p[..]] {
            let mut sorted = ids.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "控件 id 必须唯一");
            assert!(!ids.contains(&ID_CLOSE) && !ids.contains(&ID_ESC));
        }
    }

    #[test]
    fn rows_read_like_the_table() {
        let root = std::env::temp_dir();
        let installed = vec![PluginInfo {
            name: "rust".into(),
            display: "Rust".into(),
            dll: root.join("x").join("rust").join("rust.dll"),
            version: "1.0.0".into(),
            extensions: vec!["rs".into()],
            queries: vec![],
            enabled: true,
            state: dg_syntax::PluginState::Unloaded,
        }];
        let doc = RegistryDoc::parse(
            r#"{"schema":1,"updated":"","packages":[
                {"name":"rust","display":"Rust","version":"1.2.0","extensions":["rs"],"size":10,
                 "sha256":"0000000000000000000000000000000000000000000000000000000000000000","path":"pkg/rust/1.2.0.zip"},
                {"name":"go","display":"Go","version":"0.9.0","extensions":["go"],"size":10,
                 "sha256":"0000000000000000000000000000000000000000000000000000000000000000","path":"pkg/go/0.9.0.zip"}]}"#,
        )
        .unwrap();
        let rows = build_rows(Some(&doc), &installed, &root);
        assert_eq!(rows.len(), 2);
        // 行序按包名字典序：go 在前（本地没有 → 未安装），rust 在后
        // （远端 1.2.0 > 本地 1.0.0 → 有更新、可装）。
        assert_eq!(rows[0].cells[5], "未安装");
        assert!(matches!(rows[0].action, Action::Install(_)));
        assert_eq!(rows[1].cells[5], "有更新 → v1.2.0");
        assert!(matches!(rows[1].action, Action::Install(_)));
    }

    #[test]
    fn filter_matches_name_display_and_ext() {
        let r = Row {
            cells: [
                "rust".into(),
                "Rust".into(),
                "rs".into(),
                "1.0.0".into(),
                "1 KB".into(),
                "未安装".into(),
            ],
            action: Action::None,
        };
        assert!(row_matches(&r, ""));
        assert!(row_matches(&r, "RU"));
        assert!(row_matches(&r, "rust"));
        assert!(!row_matches(&r, "python"));
    }

    #[test]
    fn sizes_and_progress_read_like_humans() {
        assert_eq!(size_text(512), "1 KB");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(
            progress_text(1024 * 1024, Some(2 * 1024 * 1024)),
            "1.0 MB / 2.0 MB（50%）"
        );
        assert_eq!(progress_text(1024, None), "已下载 1 KB");
    }

    #[test]
    fn only_user_root_packages_are_uninstallable() {
        let root = std::env::temp_dir().join(format!("dg-parserdl-{}", std::process::id()));
        let inside = root.join("parsers").join("demo").join("demo.dll");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        let mk = |dll: std::path::PathBuf| PluginInfo {
            name: "demo".into(),
            display: "Demo".into(),
            dll,
            version: "1.0.0".into(),
            extensions: vec![],
            queries: vec![],
            enabled: true,
            state: dg_syntax::PluginState::Unloaded,
        };
        assert!(uninstallable(&mk(inside), &root.join("parsers")));
        assert!(!uninstallable(
            &mk(std::path::PathBuf::from("C:\\elsewhere\\demo.dll")),
            &root.join("parsers")
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
