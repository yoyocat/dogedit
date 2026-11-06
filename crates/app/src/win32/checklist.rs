//! 「带勾选的报表列表」对话框的共用零件（`pkgdlg` / `syntaxdlg`）。
//!
//! 两个框的差别只有数据源、列定义、摘要/详情文字、一个动作按钮和出口类型；
//! `#32770` 的建立、八个控件的网格、列表的列与勾选框、state image 编码、
//! 表头自绘、`LVN_ITEMCHANGED` 解析和整个 `DLGPROC` 消息泵都是同一套。
//! 这里把它们收在一处，两个调用方只保留各自的数据与语义。
//!
//! 抽象边界：**这里不放任何对话框的业务状态**。`GWLP_USERDATA` 仍指向调用方
//! 栈上的 `Dlg`，配色/字体以 `Copy` 的 [`Skin`] 快照取出；模态循环用的
//! `destroyed` 仍由调用方持有裸指针（见 [`prompt::run_modal_loop`] 的说明，
//! 那个 bool 必须走 `read_volatile`）。
//!
//! 两个框以外的对话框不要硬套本模块：`markdlg` 是单选列表、没有勾选框，
//! `parserdl` 的列与按钮布局都不同，它们该直接调 `dlggeom`。
#![allow(unsafe_op_in_unsafe_fn)]

use dg_render::theme::Theme;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, HGDIOBJ,
};
use windows::Win32::System::SystemServices::SS_ENDELLIPSIS;
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT,
    ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LVCF_TEXT, LVCF_WIDTH,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCOLUMNW, LVIF_STATE, LVIF_TEXT, LVIS_SELECTED,
    LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETEALLITEMS, LVM_GETHEADER, LVM_GETITEMSTATE,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_CHECKBOXES,
    LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL,
    LVN_ITEMCHANGED, NM_CUSTOMDRAW, NMHDR, NMLVCUSTOMDRAW, InitCommonControlsEx,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CreateDialogIndirectParamW, DLGTEMPLATE, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetDlgItem, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_NOTIFY, WM_SETFONT,
    WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR, w};

use super::prompt::{CLASS_BUTTON, CLASS_STATIC, add_child, run_modal_loop, show_modal, wide};
use super::{chrome, customdraw, dlggeom, ownerdraw, uifont, view};
use super::window::to_colorref;

pub(super) const CLASS_LIST: PCWSTR = w!("SysListView32");
/// 列表的子类化 id。表头是**列表**的子窗口，它发的 `NM_CUSTOMDRAW` 到不了对话框。
pub(super) const LIST_SUBCLASS: usize = 1;
/// 空单元格的占位符：比空白更能说明「这一项确实没有」。
pub(super) const NONE_TEXT: &str = "—";

/// 两张表共用的网格（100% 缩放，px）。尺寸一致是刻意的产品决定，不是巧合 ——
/// 见两个调用方开头的说明。
pub(super) const CLIENT_W: i32 = 640;
pub(super) const CLIENT_H: i32 = 432;
pub(super) const RECT_SUMMARY: (i32, i32, i32, i32) = (12, 10, 616, 18);
pub(super) const RECT_LIST: (i32, i32, i32, i32) = (12, 32, 616, 332);
/// 包管理器放目录/状态，语法着色框放扫描诊断 —— 同一个格子。
pub(super) const RECT_INFO: (i32, i32, i32, i32) = (12, 370, 616, 18);
pub(super) const RECT_ALL_ON: (i32, i32, i32, i32) = (12, 398, 84, 24);
pub(super) const RECT_ALL_OFF: (i32, i32, i32, i32) = (102, 398, 84, 24);
/// 「打开目录」/「在线获取...」。
pub(super) const RECT_ACTION: (i32, i32, i32, i32) = (194, 398, 96, 24);
pub(super) const RECT_OK: (i32, i32, i32, i32) = (468, 398, 72, 24);
pub(super) const RECT_CANCEL: (i32, i32, i32, i32) = (548, 398, 80, 24);

/// `#32770` 的外框样式。`WS_CLIPCHILDREN` 不能省：没有它，父窗口整块擦背景
/// 会把子控件盖掉，只有变化过的控件重画。
pub(super) const DLG_STYLE: u32 =
    WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;

/// 八个控件的 id。两个框各占一段 id 空间，别处不共享；确定/取消固定用
/// `IDOK`/`IDCANCEL`，`DefDlgProc` 才认回车和 Esc。
#[derive(Clone, Copy)]
pub(super) struct Ids {
    pub summary: i32,
    pub list: i32,
    pub info: i32,
    pub all_on: i32,
    pub all_off: i32,
    pub action: i32,
    pub ok: i32,
    pub cancel: i32,
}

/// 八个控件的初始文字。调用方已经过语言表，本模块不再翻译。
pub(super) struct Labels {
    pub summary: String,
    pub info: String,
    pub all_on: String,
    pub all_off: String,
    pub action: String,
    pub ok: String,
    pub cancel: String,
}

/// 自绘和控件着色要用的配色与字体。全是 `Copy` 的 GDI 句柄，取一份快照就能
/// 离开 `Dlg`，不必把整个对话框状态拖进共用代码。
#[derive(Clone, Copy)]
pub(super) struct Skin {
    pub dpi: i32,
    pub font: HFONT,
    pub bg: HBRUSH,
    pub bg_ref: COLORREF,
    pub ink: COLORREF,
    pub line: HBRUSH,
}

impl Skin {
    pub(super) unsafe fn new(theme: &Theme, dpi: i32) -> Self {
        let bg_ref = to_colorref(theme.background);
        Self {
            dpi,
            font: uifont::create(dpi),
            bg: CreateSolidBrush(bg_ref),
            bg_ref,
            ink: to_colorref(theme.foreground),
            line: CreateSolidBrush(to_colorref(theme.indent_line)),
        }
    }

    pub(super) unsafe fn dispose(self) {
        let _ = DeleteObject(HGDIOBJ(self.font.0));
        let _ = DeleteObject(HGDIOBJ(self.bg.0));
        let _ = DeleteObject(HGDIOBJ(self.line.0));
    }
}

/// 一个「带勾选的报表列表」对话框向共用零件提供的差异点。
///
/// 用 trait 而不是把 `Dlg` 整个泛型化：`GWLP_USERDATA` 里仍是调用方自己的
/// 具体状态，`state_mut` 也就不用到处写 turbofish。
pub(super) trait Panel: Sized {
    fn columns() -> &'static [(&'static str, i32)];
    fn ids() -> Ids;
    /// 从对话框的 `GWLP_USERDATA` 里取配色快照（只读）。
    unsafe fn skin(dlg: HWND) -> Skin;
    unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize;
    unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize;
    /// `WM_DESTROY`：置调用方栈上那个 `destroyed`，别碰别的。
    unsafe fn mark_destroyed(dlg: HWND);
}

/// 表里的一行。两个调用方各自为自己的数据实现（外部类型也可以 —— trait 是
/// 本 crate 的），共用代码只问名字、勾选和四个副列的文字。
pub(super) trait Row {
    fn name(&self) -> &str;
    fn enabled(&self) -> bool;
    /// `col` 取 1..=4；越界返回空串。
    fn cell(&self, col: usize) -> String;
}

/// 空 `DLGTEMPLATE`：三个尾随字是「无菜单、预定义 `#32770` 类、无标题」，
/// 标题在创建之后补。必须 DWORD 对齐，对话框管理器按 32 位读它。
#[repr(C, align(4))]
pub(super) struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

/// 建壳 + 上主题。控件在 `build_children` 里补，所以模板的 `cdit` 是 0。
pub(super) unsafe fn create_dialog<P: Panel>(
    state: *mut P,
    frame: HWND,
    title: &str,
    theme: &Theme,
) -> Option<HWND> {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES,
        ..Default::default()
    });
    // `tmpl` 得活到 CreateDialogIndirectParamW 返回 —— 对话框管理器在这通调用
    // 里就把它整块读走，但栈上放着比传临时对象稳妥。
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
    let dlg = CreateDialogIndirectParamW(
        Some(view::instance()),
        &tmpl.head,
        Some(frame),
        Some(dlg_proc::<P>),
        LPARAM(state as isize),
    )
    .ok()?;
    let title = wide(title);
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));
    let dark = chrome::theme_is_dark(theme);
    chrome::set_chrome_theme(theme);
    chrome::title_bar(dlg, dark);
    Some(dlg)
}

/// 八个控件，顺序与两个旧实现一致：摘要 → 列表 → 详情/诊断 → 三个动作 →
/// 确定 → 取消。静态文本用 `SS_ENDELLIPSIS`，路径和状态都可能比格子长。
pub(super) unsafe fn build_children(dlg: HWND, ids: &Ids, labels: &Labels, skin: &Skin) {
    let dpi = skin.dpi as f32 / 96.0;
    let ellipsis = WINDOW_STYLE(SS_ENDELLIPSIS.0);
    let _ = child(
        dlg,
        skin.font,
        CLASS_STATIC,
        &labels.summary,
        ids.summary,
        ellipsis,
        RECT_SUMMARY,
        dpi,
    );
    let _ = child(
        dlg,
        skin.font,
        CLASS_LIST,
        "",
        ids.list,
        WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS),
        ),
        RECT_LIST,
        dpi,
    );
    let _ = child(
        dlg,
        skin.font,
        CLASS_STATIC,
        &labels.info,
        ids.info,
        ellipsis,
        RECT_INFO,
        dpi,
    );
    for (id, label, rect) in [
        (ids.all_on, &labels.all_on, RECT_ALL_ON),
        (ids.all_off, &labels.all_off, RECT_ALL_OFF),
        (ids.action, &labels.action, RECT_ACTION),
    ] {
        let _ = child(dlg, skin.font, CLASS_BUTTON, label, id, WS_TABSTOP, rect, dpi);
    }
    let _ = child(
        dlg,
        skin.font,
        CLASS_BUTTON,
        &labels.ok,
        ids.ok,
        default_button(),
        RECT_OK,
        dpi,
    );
    let _ = child(
        dlg,
        skin.font,
        CLASS_BUTTON,
        &labels.cancel,
        ids.cancel,
        WS_TABSTOP,
        RECT_CANCEL,
        dpi,
    );
}

pub(super) fn default_button() -> WINDOW_STYLE {
    WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32)
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn child(
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
    let _ = SendMessageW(
        ctl,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    Some(ctl)
}

/// 列、勾选框和三个颜色消息。颜色消息不能省：`SysListView32` 默认是
/// `COLOR_WINDOW` 的底，最后一行以下的空白由控件自己刷，那是自绘够不到的地方。
pub(super) unsafe fn init_list<P: Panel>(dlg: HWND, skin: &Skin) {
    let Some(list) = item(dlg, P::ids().list) else {
        return;
    };
    let ex = (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP) as usize;
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
        Some(LPARAM(skin.bg_ref.0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTBKCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(skin.bg_ref.0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(skin.ink.0 as isize)),
    );
    let _ = SetWindowSubclass(list, Some(list_proc::<P>), LIST_SUBCLASS, dlg.0 as usize);
    for (i, (title, width)) in P::columns().iter().enumerate() {
        let title = crate::win32::lang::trs(title);
        let mut text: Vec<u16> = title.encode_utf16().chain([0]).collect();
        let mut col = LVCOLUMNW {
            mask: LVCF_WIDTH | LVCF_TEXT,
            cx: uifont::scaled(*width, skin.dpi),
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

/// 一行一个条目：第 0 列是名字 + 勾选框，其余四列用 `LVM_SETITEMTEXTW` 补。
pub(super) unsafe fn fill_list<R: Row>(dlg: HWND, id_list: i32, rows: &[R]) {
    let Some(list) = item(dlg, id_list) else {
        return;
    };
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, None, None);
    for (row, r) in rows.iter().enumerate() {
        let mut name: Vec<u16> = r.name().encode_utf16().chain([0]).collect();
        let lvitem = LVITEMW {
            mask: LVIF_TEXT,
            iItem: row as i32,
            iSubItem: 0,
            pszText: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw const lvitem as isize)),
        );
        // `LVM_INSERTITEM` 不读 `state`/`stateMask`（Win32 文档原话），勾选状态
        // 只能随后单独写；少了这一步插进去的行一律未选。
        set_row_state(list, row as i32, r.enabled());
        for col in 1..=4usize {
            let mut buf: Vec<u16> = r.cell(col).encode_utf16().chain([0]).collect();
            let sub = LVITEMW {
                iSubItem: col as i32,
                pszText: PWSTR(buf.as_mut_ptr()),
                ..Default::default()
            };
            let _ = SendMessageW(
                list,
                LVM_SETITEMTEXTW,
                Some(WPARAM(row)),
                Some(LPARAM(&raw const sub as isize)),
            );
        }
    }
}

/// `LVS_EX_CHECKBOXES` 把勾选状态放在 item 的 state image 里：1 = 未选、
/// 2 = 选中，再左移进 state-image 字段。
pub(super) fn state_image(on: bool) -> LIST_VIEW_ITEM_STATE_FLAGS {
    LIST_VIEW_ITEM_STATE_FLAGS((if on { 2 } else { 1 }) << 12)
}

/// 某一行的勾选状态。`LVM_GETITEMSTATE` 返回的就是 state image 字段。
pub(super) unsafe fn checked(list: HWND, row: i32) -> bool {
    let st = SendMessageW(
        list,
        LVM_GETITEMSTATE,
        Some(WPARAM(row as usize)),
        Some(LPARAM(LVIS_STATEIMAGEMASK.0 as isize)),
    );
    (st.0 as u32 & LVIS_STATEIMAGEMASK.0) >> 12 == 2
}

/// 写一行的勾选状态。`stateMask` 只点 state image 那 4 位，别的位不动。
pub(super) unsafe fn set_row_state(list: HWND, row: i32, on: bool) {
    let lvitem = LVITEMW {
        mask: LVIF_STATE,
        iItem: row,
        state: state_image(on),
        stateMask: LVIS_STATEIMAGEMASK,
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_SETITEMSTATE,
        Some(WPARAM(row as usize)),
        Some(LPARAM(&raw const lvitem as isize)),
    );
}

/// 全选/全不选：逐行写 state image。每行都会发 `LVN_ITEMCHANGED`，计数器跟着
/// 走，所以这里不用额外刷摘要。
pub(super) unsafe fn set_all(dlg: HWND, id_list: i32, rows: i32, on: bool) {
    let Some(list) = item(dlg, id_list) else {
        return;
    };
    for row in 0..rows {
        set_row_state(list, row, on);
    }
}

/// 收成禁用名单：没勾的行按列表顺序留下。`None` = 列表控件没建出来，调用方
/// 此时必须什么也不做（不能当成「全部启用」去落地并关窗）。
pub(super) unsafe fn disabled_names(
    dlg: HWND,
    id_list: i32,
    names: &[String],
) -> Option<Vec<String>> {
    let list = item(dlg, id_list)?;
    Some(
        names
            .iter()
            .enumerate()
            .filter(|(row, _)| !checked(list, *row as i32))
            .map(|(_, name)| name.clone())
            .collect(),
    )
}

pub(super) unsafe fn item(dlg: HWND, id: i32) -> Option<HWND> {
    GetDlgItem(Some(dlg), id).ok()
}

pub(super) unsafe fn set_text(ctl: HWND, text: &str) {
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(ctl, PCWSTR::from_raw(buf.as_ptr()));
}

/// 外框尺寸按客户端算，再居中压在编辑区上（见 `dlggeom`）。
pub(super) unsafe fn size_and_place(
    dlg: HWND,
    frame: HWND,
    dpi: i32,
    client: (i32, i32),
) {
    dlggeom::size_and_place(
        dlg,
        frame,
        uifont::scaled(client.0, dpi),
        uifont::scaled(client.1, dpi),
        dpi as u32,
        DLG_STYLE,
    );
}

/// `LVN_ITEMCHANGED` 读到的字段，顺序照 `commctrl.h` 的声明。
#[repr(C)]
struct ItemChanged {
    hdr: NMHDR,
    i_item: i32,
    i_sub_item: i32,
    u_new_state: u32,
    u_old_state: u32,
    u_changed: u32,
    pt_location: POINT,
    l_param: LPARAM,
}

/// 一条状态变化通知里，两个调用方各自关心的部分。
pub(super) struct Change {
    /// 勾选数的增量（-1 / 0 / +1）。
    pub checked_delta: i32,
    /// 选中位变了时的新选中行；取消选中是 -1。`None` = 没变。
    pub selection: Option<i32>,
}

/// 解析 `LVN_ITEMCHANGED`；不是本列表的状态变化就返回 `None`。
///
/// `uChanged` 装的是 **LVIF_** 标志（`LVIF_STATE` = 0x8），不是 `LVIS_` 位 ——
/// 拿 `LVIS_STATEIMAGEMASK` 去与它永远是 0，勾选就悄悄丢了。
pub(super) unsafe fn item_change(lparam: LPARAM, id_list: i32) -> Option<Change> {
    let hdr = &*(lparam.0 as *const NMHDR);
    if hdr.idFrom as i32 != id_list || hdr.code != LVN_ITEMCHANGED {
        return None;
    }
    let n = &*(lparam.0 as *const ItemChanged);
    if n.u_changed & LVIF_STATE.0 == 0 {
        return None;
    }
    let mask = LVIS_STATEIMAGEMASK.0;
    let on_new = (n.u_new_state & mask) >> 12 == 2;
    let on_old = (n.u_old_state & mask) >> 12 == 2;
    let checked_delta = on_new as i32 - on_old as i32;
    let sel_new = n.u_new_state & LVIS_SELECTED.0 != 0;
    let sel_old = n.u_old_state & LVIS_SELECTED.0 != 0;
    let selection = (sel_new != sel_old).then(|| if sel_new { n.i_item } else { -1 });
    Some(Change {
        checked_delta,
        selection,
    })
}

/// 表头 `NM_CUSTOMDRAW` 的转发。`SysHeader32` 的控件 id 是 0，`idFrom` 认不
/// 出来，只能按窗口句柄比；它发的通知只到列表这里，到不了对话框。
unsafe extern "system" fn list_proc<P: Panel>(
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
            let dlg = HWND(dlg as *mut core::ffi::c_void);
            return LRESULT(on_header_draw(P::skin(dlg), P::columns(), lparam));
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

unsafe fn is_header(list: HWND, hwnd: HWND) -> bool {
    let header = SendMessageW(list, LVM_GETHEADER, None, None);
    header.0 as isize == hwnd.0 as isize
}

/// 表头自绘：列表控件会顺手建一个 `SysHeader32`，它按系统主题画 —— 深色
/// 对话框上就是一条白带。两个阶段都自己画：先刷整条底，再逐列刷格子、
/// 画标题，最后一条竖线当列分隔。
unsafe fn on_header_draw(
    skin: Skin,
    columns: &[(&str, i32)],
    lparam: LPARAM,
) -> isize {
    let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
    let hdc = cd.nmcd.hdc;
    let stage = cd.nmcd.dwDrawStage.0;
    if stage == CDDS_PREPAINT.0 {
        let mut rc = RECT::default();
        if GetClientRect(cd.nmcd.hdr.hwndFrom, &mut rc).is_ok() {
            let _ = FillRect(hdc, &rc, skin.bg);
        }
        return CDRF_NOTIFYITEMDRAW as isize;
    }
    if stage != CDDS_ITEMPREPAINT.0 {
        return CDRF_DODEFAULT as isize;
    }
    let Some((title, _)) = columns.get(cd.nmcd.dwItemSpec as usize) else {
        return CDRF_DODEFAULT as isize;
    };
    let title = crate::win32::lang::trs(title);
    let mut rc = cd.nmcd.rc;
    let _ = FillRect(hdc, &rc, skin.bg);
    let divider = RECT {
        left: rc.right - 1,
        top: rc.top + uifont::scaled(3, skin.dpi),
        right: rc.right,
        bottom: rc.bottom - uifont::scaled(3, skin.dpi),
    };
    let _ = FillRect(hdc, &divider, skin.line);
    rc.left += uifont::scaled(6, skin.dpi);
    rc.right -= uifont::scaled(4, skin.dpi);
    ownerdraw::draw_text(
        hdc,
        rc,
        skin.font,
        skin.ink,
        &title,
        ownerdraw::Align::Left(0),
    );
    CDRF_SKIPDEFAULT as isize
}

/// 两个框的 `DLGPROC` 以前逐字相同，只有注释不一样；命令/通知的语义交给
/// [`Panel`]，这里只做壳和配色。
///
/// `DLGPROC` 在这个 metadata 里是 `-> isize`，所以返回 0/1 而不是 `LRESULT`。
pub(super) unsafe extern "system" fn dlg_proc<P: Panel>(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            // 1 = 管理器自己挑初始焦点。子控件这时还没建，所以 `finish` 在
            // `show_modal` 之后再把焦点给列表。
            1
        }
        WM_COMMAND => P::on_command(hwnd, wparam),
        WM_NOTIFY => P::on_notify(hwnd, lparam),
        WM_ERASEBKGND => {
            // `#32770` 自带 `COLOR_3DFACE` 刷子，控件之间和背后的底色得自己
            // 从 Theme 出。
            let skin = P::skin(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, skin.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let skin = P::skin(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), skin.ink, skin.bg_ref, skin.bg)
        }
        WM_CLOSE => {
            // 标题栏的 X 等于取消：`result` 保持 `None`。
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            P::mark_destroyed(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => 0,
    }
}

/// 显示、把焦点交给列表、跑模态循环。`destroyed` 必须是调用方栈上那个裸
/// bool 的地址 —— `run_modal_loop` 用 `read_volatile` 读它，不能改成 `&mut`。
pub(super) unsafe fn finish(
    dlg: HWND,
    frame: HWND,
    id_list: i32,
    destroyed: *const bool,
) {
    show_modal(dlg, frame);
    if let Some(list) = item(dlg, id_list) {
        let _ = SetFocus(Some(list));
    }
    run_modal_loop(dlg, frame, destroyed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模板头必须 DWORD 对齐：对话框管理器按 32 位读它。
    #[test]
    fn the_template_is_dword_aligned() {
        assert_eq!(std::mem::align_of::<Template>(), 4);
        assert_eq!(
            std::mem::size_of::<Template>(),
            std::mem::size_of::<DLGTEMPLATE>() + 6
        );
    }

    /// 勾选框的 state image 编码：1 未选、2 选中，左移 12 位。
    #[test]
    fn check_state_images_match_the_control() {
        assert_eq!(state_image(false).0, 1 << 12);
        assert_eq!(state_image(true).0, 2 << 12);
        assert_eq!((2u32 << 12 & LVIS_STATEIMAGEMASK.0) >> 12, 2);
    }

    /// 共用的网格：控件不能出界、按钮之间不能叠。列宽之和只许比列表窄一点，
    /// 宽了列表会长出一条没用的横向滚动条。
    #[test]
    fn the_grid_fits_the_client() {
        let rects = [
            RECT_SUMMARY,
            RECT_LIST,
            RECT_INFO,
            RECT_ALL_ON,
            RECT_ALL_OFF,
            RECT_ACTION,
            RECT_OK,
            RECT_CANCEL,
        ];
        for (x, y, w, h) in rects {
            assert!(x + w <= CLIENT_W, "({x},{y},{w},{h}) 超出宽度");
            assert!(y + h <= CLIENT_H, "({x},{y},{w},{h}) 超出高度");
        }
        let mut buttons = [RECT_ALL_ON, RECT_ALL_OFF, RECT_ACTION, RECT_OK, RECT_CANCEL];
        buttons.sort_by_key(|r| r.0);
        for pair in buttons.windows(2) {
            assert!(pair[0].0 + pair[0].2 <= pair[1].0, "{pair:?} 重叠");
        }
    }
}
