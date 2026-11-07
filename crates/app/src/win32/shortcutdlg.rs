//! 快捷键管理 — the box behind 工具 > 设置 > 快捷键 (`cm_config_shortcut`).
//!
//! * **Geometry** is the dialog template (`DLGTEMPLATEEX` 438x326 dialog units, 11
//!   items), converted at 1.4 logical px per unit horizontally and 1.6 vertically —
//!   the two axes differ because this dialog's font is narrower than the external
//!   tool manager's, whose box converts evenly at 1.6/1.6 (see `tooldlg`). The
//!   conversion is worth stating because it cross-checks: the search field comes out
//!   594x22 at 10,11, the list 594x328 at 10,37 and 删除 70x22 at 8,488, which is what
//!   the screen shows. The template carries `WS_THICKFRAME` and the 21x21
//!   `ScrollBar`-class size grip — so [`layout`] re-places everything from the client
//!   size: the search field and the list grow with the width, the list grows with the
//!   height, and the whole bottom block (hint, labels, current-key list, fields,
//!   buttons) rides the height delta.
//! * **The four columns** are 按键 / ID / 命令 / 说明, 59/144/120/160 px wide. The
//!   header is painted by this module rather than by the list view: a themed
//!   `SysHeader32` would keep the system's light palette, while this header is the
//!   dialog's own `#2D2D2D` with `#E0E0E0` text. The list therefore carries
//!   `LVS_NOCOLUMNHEADER` and starts under the band this module draws.
//! * **The model** is `key.ini`'s `[Shortcut]` section, held as [`KeyMap::rows`] — the
//!   rows as the file spells them. 分配 rewrites one row, 删除 drops one, and every
//!   store writes the section back and posts [`WM_SHORTCUTS`] so the frame reloads the
//!   map it dispatches from. Nothing is cached across a store: the list and the
//!   当前快捷键 box are re-derived from the map each time.
//! * **新建快捷键 is two fields**. Typing a combo in the first field hands the focus to
//!   the second, and 分配 joins them with the comma the file uses. A row that cannot run
//!   is a fact about the implementation worth showing: selecting one disables 分配, so
//!   no dead binding can be created from here. With no row selected the buttons stay
//!   enabled — the reference opens with all three bright, and the actions themselves
//!   guard an empty selection.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlggeom;
use crate::win32::dpi::scale;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    ScreenToClient,
    DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, BeginPaint, EndPaint, FillRect, FrameRect, HBRUSH,
    HDC, HFONT, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SetBkMode, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCOLUMNW, LVIF_PARAM, LVIF_TEXT,
    LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
    LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_FULLROWSELECT, LVS_NOCOLUMNHEADER, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVN_ITEMCHANGED, NMHDR, NMLVCUSTOMDRAW, LVCF_WIDTH,
    LVIF_STATE,
    LVS_EX_LABELTIP, LVM_ENSUREVISIBLE, LVM_SETITEMTEXTW,
    LIST_VIEW_ITEM_STATE_FLAGS, NM_CUSTOMDRAW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, RemoveWindowSubclass,
    SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_PUSHBUTTON, CreateDialogIndirectParamW, CreateWindowExW, DLGTEMPLATE, DestroyWindow,
    GA_ROOT, GetAncestor, GetClientRect, GetDlgCtrlID, GetDlgItem, GetWindowLongPtrW, GetWindowTextW,
    GetWindowTextLengthW, GWLP_USERDATA, HMENU, IDCANCEL, IDYES, IsDialogMessageW, MB_YESNO, MSG,
    MessageBoxW, WM_SETFONT,
    PostMessageW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, SW_SHOW, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    GetWindowRect, SBS_SIZEBOX, SBS_SIZEBOXBOTTOMRIGHTALIGN, WM_DROPFILES, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_INITDIALOG, WM_KEYDOWN, WM_SETICON, ICON_BIG, ICON_SMALL,
    WM_NCDESTROY, WM_NOTIFY, WM_PAINT,
    WM_SYSCHAR, WM_SYSKEYDOWN, WM_SIZE, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_THICKFRAME, WS_VISIBLE, WINDOW_EX_STYLE, WINDOW_STYLE, LB_ADDSTRING,
    LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, MINMAXINFO,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::cmdids;
use crate::keys::{KeyMap, combo_text};
use crate::win32::cmdtext;
use crate::win32::customdraw;
use crate::win32::ownerdraw;
use crate::win32::view::instance;
use crate::win32::window::{modifier_state, to_colorref};
use dg_render::theme::Theme;

/// Control ids
const ID_FILTER: i32 = 1_200;
const ID_LIST: i32 = 1_201;
const ID_BIND_SCRIPT: i32 = 1_202;
const ID_ASSIGN: i32 = 1_203;
const ID_CURRENT: i32 = 1_204;
const ID_KEY1: i32 = 1_205;
const ID_KEY2: i32 = 1_206;
const ID_DELETE: i32 = 1_207;
const ID_L_CURRENT: i32 = 3_000;
const ID_L_NEW: i32 = 3_001;
const ID_HINT: i32 = 3_002;
/// The template's 21x21 size grip, the `ScrollBar` at the corner.
const ID_GRIP: i32 = 3_003;

/// The dialog's client at 96 DPI: 438x326 dialog units at 1.4 x / 1.6 y.
const CLIENT_W: i32 = 613;
const CLIENT_H: i32 = 522;
/// The template's 7-unit margin, at the horizontal unit.
const MARGIN: i32 = 10;
const HEADER_TOP: i32 = 37;
const HEADER_H: i32 = 24;
/// The list's rectangle is 328 tall including the header band.
const LIST_H: i32 = 328 - HEADER_H;

/// 按键 / ID / 命令 / 说明
const COLUMNS: [i32; 4] = [59, 144, 120, 160];
const HEADER_CAPTIONS: [&str; 4] = ["按键", "ID", "命令", "说明"];

/// The hint line's red. The.lng string carries no colour and the dialog has no
/// `Theme` field for a warning.
const HINT_RED: (u8, u8, u8) = (0xFF, 0x6E, 0x6E);

/// The message the dialog posts after it stored `key.ini`.
pub(super) const WM_SHORTCUTS: u32 = WM_APP + 0x4A;

/// The dialog's one instance, as the 1460 handler keeps it.
static DLG: AtomicIsize = AtomicIsize::new(0);

fn dlg_hwnd() -> Option<HWND> {
    let v = DLG.load(Ordering::Acquire);
    (v != 0).then_some(HWND(v as *mut core::ffi::c_void))
}

/// Whether the frame's loop should feed `msg` to this dialog's manager.
pub(super) fn translate(msg: &MSG) -> bool {
    let Some(dlg) = dlg_hwnd() else {
        return false;
    };
    let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if root != dlg {
        return false;
    }
    unsafe { IsDialogMessageW(dlg, msg).as_bool() }
}

struct Row {
    /// The `[Shortcut]` value this row binds: a `cm_*` name, or a script path.
    value: String,
    /// The ID column — the command name, or the script's path.
    id: String,
    /// The 命令 column.
    label: String,
    /// The 说明 column.
    desc: String,
    /// Whether this implementation can run it. A command it has not implemented is listed
    ///  but greyed and unbindable.
    live: bool,
}

struct Dlg {
    frame: HWND,
    map: KeyMap,
    theme: Theme,
    font: HFONT,
    bg: HBRUSH,
    face: HBRUSH,
    edge: HBRUSH,
    dpi: f32,
    /// Every row, commands first in name order, then any dropped scripts.
    rows: Vec<Row>,
    /// The rows the list currently shows — indexes into `rows`, after the
    /// filter.
    shown: Vec<usize>,
    /// The selected row, an index into `rows`, or -1.
    sel: i32,
    /// The combos in the 当前快捷键 box, so its selection can be mapped back.
    current: Vec<String>,
    /// Set while the list is being filled: the control reports a selection
    /// change for every inserted item, and those are not the user's.
    loading: bool,
}

unsafe fn state(dlg: HWND) -> &'static Dlg {
    &*(GetWindowLongPtrW(dlg, GWLP_USERDATA) as *const Dlg)
}

unsafe fn state_mut(dlg: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut Dlg)
}

/// What the dialog needs when 1460 asks for it: the frame, the map to edit (a
/// clone — the dialog's edits reach the frame through the file and
/// [`WM_SHORTCUTS`]), and the theme.
pub(super) struct Request<'a> {
    pub frame: HWND,
    pub map: &'a KeyMap,
    pub theme: &'a Theme,
}

/// Show the manager, creating it the first time
/// `CreateDialogIndirectParamW` / `ShowWindow(h, 5)` pair.
pub(super) fn show(req: Request<'_>) {
    unsafe {
        if let Some(existing) = dlg_hwnd() {
            let _ = ShowWindow(existing, SW_SHOW);
            let _ = SetFocus(Some(existing));
            return;
        }
        let dpi = GetDpiForWindow(req.frame).max(96) as f32 / 96.0;
        create(req, dpi);
    }
}

/// Item-less `DLGTEMPLATE`; the three trailing words are "no menu, the
/// predefined `#32770` class, no caption".
#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

const DLG_STYLE: u32 = WS_POPUP.0
    | WS_CAPTION.0
    | WS_SYSMENU.0
    | WS_THICKFRAME.0
    | WS_CLIPCHILDREN.0;

unsafe fn create(req: Request<'_>, dpi: f32) {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_BAR_CLASSES,
        ..Default::default()
    });
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
    let boxed = Box::new(Dlg {
        frame: req.frame,
        map: req.map.clone(),
        theme: req.theme.clone(),
        font: ui_font(dpi),
        bg: CreateSolidBrush(to_colorref(req.theme.background)),
        face: CreateSolidBrush(to_colorref(req.theme.panel_bg)),
        edge: CreateSolidBrush(to_colorref(req.theme.indent_line)),
        dpi,
        rows: command_rows(),
        shown: Vec::new(),
        sel: -1,
        current: Vec::new(),
        loading: false,
    });
    let param = LPARAM(Box::into_raw(boxed) as isize);
    let dlg = match CreateDialogIndirectParamW(
        Some(instance()),
        &tmpl.head,
        Some(req.frame),
        Some(dialog_proc),
        param,
    ) {
        Ok(h) => h,
        Err(_) => {
            drop(Box::from_raw(param.0 as *mut Dlg));
            return;
        }
    };
    DLG.store(dlg.0 as isize, Ordering::Release);

    let title: Vec<u16> = crate::win32::lang::dlg(210, 0)
        .unwrap_or("快捷键管理")
        .encode_utf16()
        .chain([0])
        .collect();
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));
    // The template's caption carries no icon; without this the dialog inherits
    // the executable's
    let _ = SendMessageW(dlg, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(0)));
    let _ = SendMessageW(dlg, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(0)));

    build_children(dlg);
    crate::win32::lang::localize_dialog(dlg, 210);
    // The two key fields would open as white boxes.
    crate::win32::chrome::dark_theme_children(dlg, crate::win32::chrome::theme_is_dark(req.theme));
    // Escape needs a cancel command to post, and the template has no id-2
    // control, so this one is never shown.
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("Button"),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0),
        0,
        0,
        0,
        0,
        Some(dlg),
        Some(HMENU(IDCANCEL.0 as usize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    );
    DragAcceptFiles(dlg, true.into());

    size_and_place(dlg);
    init_list(dlg);
    fill_list(dlg);
    let _ = ShowWindow(dlg, SW_SHOW);
    if let Some(edit) = item(dlg, ID_KEY1) {
        let _ = SetFocus(Some(edit));
    }
}

/// Every command this editor names, in name order, with the 命令 and 说明 the
/// language table gives it.
fn command_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = cmdtext::CMD_TEXT
        .iter()
        .filter_map(|&(id, label, desc)| {
            let name = cmdids::name_for_id(id)?;
            let (label, desc) = crate::win32::lang::command_text(id)
                .unwrap_or_else(|| (label.to_string(), desc.to_string()));
            Some(Row {
                value: name.to_string(),
                id: name.to_string(),
                label,
                desc,
                live: cmdids::command_for_id(id).is_some(),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// The outer rect this client needs at this DPI, centred over the frame.
unsafe fn size_and_place(dlg: HWND) {
    let st = state(dlg);
    let dpi = st.dpi;
    dlggeom::size_and_place(
        dlg,
        st.frame,
        scale(CLIENT_W, dpi),
        scale(CLIENT_H, dpi),
        (dpi * 96.0) as u32,
        DLG_STYLE,
    );
}

/// The eleven children of the template, at their measured rectangles. The list
/// starts under the band this module paints, so it is placed `HEADER_H` lower
/// than the template's list rectangle.
unsafe fn build_children(dlg: HWND) {
    let add = |id: i32, class: PCWSTR, text: &str, style: u32| {
        let label: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let ctl = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR::from_raw(label.as_ptr()),
            WINDOW_STYLE(style | WS_CHILD.0 | WS_VISIBLE.0),
            0,
            0,
            0,
            0,
            Some(dlg),
            Some(HMENU(id as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        );
        if let Ok(ctl) = ctl {
            let _ = SendMessageW(
                ctl,
                WM_SETFONT,
                Some(WPARAM(state(dlg).font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        ctl.ok()
    };
    // 1200 — the filter field.
    add(
        ID_FILTER,
        w!("Edit"),
        "",
        (WS_TABSTOP | WINDOW_STYLE(0x0080_0000)).0, // ES_AUTOHSCROLL
    );
    // 1201 — the command list.
    add(
        ID_LIST,
        w!("SysListView32"),
        "",
        WS_TABSTOP.0 | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER),
    );
    // 3002 — the red hint, 15250-15252 are the column captions the.lng carries
    // and 3000/3001 the two field labels.
    add(
        ID_HINT,
        w!("Static"),
        "*如需绑定快捷键到某个脚本,请把该脚本拖放到上面的列表中即可!",
        0,
    );
    add(ID_L_CURRENT, w!("Static"), "当前快捷键:", 0);
    add(ID_L_NEW, w!("Static"), "新建快捷键:", 0);
    // 1204 — the current-key list.
    add(
        ID_CURRENT,
        w!("ListBox"),
        "",
        WS_TABSTOP.0 | 0x0080_0000 | 0x0020_0000 | 0x0100 | 0x0001, // BORDER|VSCROLL|NOINTEGRALHEIGHT|NOTIFY
    );
    // 1205 / 1206 — the two stages of the new shortcut.
    add(
        ID_KEY1,
        w!("Edit"),
        "",
        (WS_TABSTOP | WINDOW_STYLE(0x0080_0000)).0,
    );
    add(
        ID_KEY2,
        w!("Edit"),
        "",
        (WS_TABSTOP | WINDOW_STYLE(0x0080_0000)).0,
    );
    // 1203 / 1202 / 1207 — 分配, 绑定脚本, 删除. Native push buttons, exactly
    // as DLG 210 declares them. `create` names them `DarkMode_Explorer`, so the
    // visual style draws the dark rounded face, the centred caption and the
    // hover/pressed states; painting them here would lose all three. `add`
    // also gives them the dialog's UI font, like every other child.
    add(
        ID_ASSIGN,
        w!("Button"),
        "分配",
        WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
    );
    add(
        ID_BIND_SCRIPT,
        w!("Button"),
        "绑定脚本",
        WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
    );
    add(
        ID_DELETE,
        w!("Button"),
        "删除",
        WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
    );
    // The template's size grip: a `ScrollBar` that draws the corner hatch and
    // turns a drag on it into a `WM_SIZING` for the dialog.
    add(
        ID_GRIP,
        w!("ScrollBar"),
        "",
        (SBS_SIZEBOX | SBS_SIZEBOXBOTTOMRIGHTALIGN) as u32,
    );

    // The two key fields capture key presses instead of typing text.
    for id in [ID_KEY1, ID_KEY2] {
        if let Some(edit) = item(dlg, id) {
            let _ = SetWindowSubclass(
                edit,
                Some(key_edit_proc),
                1,
                dlg.0 as usize,
            );
        }
    }
}

/// Re-place every child from the client size. The list takes the width and
/// height deltas; the bottom block rides the height delta.
unsafe fn layout(dlg: HWND, w: i32, h: i32) {
    let st = state(dlg);
    let dpi = st.dpi;
    let dw = w - scale(CLIENT_W, dpi);
    let dh = h - scale(CLIENT_H, dpi);
    let list_w = scale(CLIENT_W - 2 * MARGIN, dpi) + dw;
    let px = |v: i32| scale(v, dpi);
    let place = |id: i32, x: i32, y: i32, cw: i32, ch: i32| {
        if let Some(ctl) = item(dlg, id) {
            let _ = SetWindowPos(
                ctl,
                None,
                x,
                y,
                cw.max(1),
                ch.max(1),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    };
    place(ID_FILTER, px(MARGIN), px(11), list_w, px(22));
    place(
        ID_LIST,
        px(MARGIN),
        px(HEADER_TOP + HEADER_H),
        list_w,
        px(LIST_H) + dh,
    );
    place(ID_HINT, px(MARGIN), px(366) + dh, list_w, px(18));
    place(ID_L_CURRENT, px(MARGIN), px(389) + dh, px(119), px(16));
    place(ID_L_NEW, px(164), px(389) + dh, px(126), px(16));
    place(ID_CURRENT, px(MARGIN), px(408) + dh, px(147), px(77));
    place(ID_KEY1, px(164), px(408) + dh, px(154), px(21));
    place(ID_KEY2, px(164), px(434) + dh, px(154), px(21));
    place(ID_ASSIGN, px(162), px(462) + dh, px(70), px(22));
    place(ID_BIND_SCRIPT, px(235), px(462) + dh, px(70), px(22));
    place(ID_DELETE, px(8), px(488) + dh, px(70), px(22));
    place(ID_GRIP, w - px(21), h - px(21), px(21), px(21));
    let _ = InvalidateRect(Some(dlg), None, true);
}

/// The list's own setup: four fixed columns, the theme's colours.
unsafe fn init_list(dlg: HWND) {
    let Some(list) = item(dlg, ID_LIST) else {
        return;
    };
    let st = state(dlg);
    let ex = (LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP) as usize;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(ex)),
        Some(LPARAM(ex as isize)),
    );
    for (i, w) in COLUMNS.iter().enumerate() {
        let mut col = LVCOLUMNW {
            mask: LVCF_WIDTH,
            cx: scale(*w, st.dpi),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            Some(WPARAM(i)),
            Some(LPARAM(&raw mut col as isize)),
        );
    }
    let _ = SendMessageW(
        list,
        LVM_SETBKCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(to_colorref(st.theme.background).0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTBKCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(to_colorref(st.theme.background).0 as isize)),
    );
    let _ = SendMessageW(
        list,
        LVM_SETTEXTCOLOR,
        Some(WPARAM(0)),
        Some(LPARAM(to_colorref(st.theme.panel_fg).0 as isize)),
    );
}

/// Rebuild the list from the filter. The selection is kept by row index, so a
/// store does not move it.
unsafe fn fill_list(dlg: HWND) {
    let Some(list) = item(dlg, ID_LIST) else {
        return;
    };
    let st = state_mut(dlg);
    let needle = text_of(dlg, ID_FILTER).to_lowercase();
    st.loading = true;
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, None, None);
    st.shown.clear();
    let mut keep = -1i32;
    for (i, row) in st.rows.iter().enumerate() {
        if !needle.is_empty() {
            let hay = format!("{} {} {}", row.id, row.label, row.desc).to_lowercase();
            if !hay.contains(&needle) {
                continue;
            }
        }
        st.shown.push(i);
        if i as i32 == st.sel {
            keep = st.shown.len() as i32 - 1;
        }
        let keys = st.map.combos_for(&row.value).join(", ");
        let mut keys16: Vec<u16> = keys.encode_utf16().chain([0]).collect();
        let mut id16: Vec<u16> = row.id.encode_utf16().chain([0]).collect();
        let mut label16: Vec<u16> = row.label.encode_utf16().chain([0]).collect();
        let mut desc16: Vec<u16> = row.desc.encode_utf16().chain([0]).collect();
        // `iItem` is the row this insert lands on: comctl32 ignores wParam
        // here, and `-1` in it does not mean "append" — it means "the top".
        // Every row went to index 0 while the writer below addressed the last
        // row, so the subitems landed on the previous command and the list
        // came up blank.
        let at_row = st.shown.len() as i32 - 1;
        let item = LVITEMW {
            mask: LVIF_TEXT | LVIF_PARAM,
            iItem: at_row,
            pszText: PWSTR(keys16.as_mut_ptr()),
            lParam: LPARAM(i as isize),
            ..Default::default()
        };
        let at = SendMessageW(
            list,
            LVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw const item as isize)),
        )
        .0 as i32;
        if at < 0 {
            continue;
        }
        let row_at = at;
        for (sub, text) in [(1, &mut id16), (2, &mut label16), (3, &mut desc16)] {
            let mut cell = LVITEMW {
                mask: LVIF_TEXT,
                iSubItem: sub,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            let _ = SendMessageW(
                list,
                LVM_SETITEMTEXTW,
                Some(WPARAM(row_at as usize)),
                Some(LPARAM(&raw mut cell as isize)),
            );
        }
    }
    st.loading = false;
    select_row(dlg, keep);
}

/// Select the row at `shown` (an index into the visible rows), then re-derive
/// everything the selection drives.
unsafe fn select_row(dlg: HWND, shown: i32) {
    let st = state_mut(dlg);
    st.sel = st
        .shown
        .get(shown.max(0) as usize)
        .filter(|_| shown >= 0)
        .map(|&row| row as i32)
        .unwrap_or(-1);
    if let Some(list) = item(dlg, ID_LIST) {
        // `-1` clears: the flags go in `stateMask` either way, but `state`
        // has to be zero or the "no selection" call would select row 0 — the
        // white band the list opened with.
        let want = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
        let state = if shown >= 0 {
            LIST_VIEW_ITEM_STATE_FLAGS(want.0)
        } else {
            LIST_VIEW_ITEM_STATE_FLAGS(0)
        };
        let item = LVITEMW {
            mask: LVIF_STATE,
            iItem: shown.max(0),
            state,
            stateMask: want,
            ..Default::default()
        };
        st.loading = true;
        let _ = SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(shown.max(0) as usize)),
            Some(LPARAM(&raw const item as isize)),
        );
        st.loading = false;
        if shown >= 0 {
            let _ = SendMessageW(
                list,
                LVM_ENSUREVISIBLE,
                Some(WPARAM(shown as usize)),
                Some(LPARAM(0)),
            );
        }
    }
    refresh_current(dlg);
}

/// Fill the 当前快捷键 box from the selected row and set the buttons' state.
unsafe fn refresh_current(dlg: HWND) {
    let st = state_mut(dlg);
    let Some(row) = st.rows.get(st.sel.max(0) as usize).filter(|_| st.sel >= 0) else {
        st.current.clear();
        if let Some(lb) = item(dlg, ID_CURRENT) {
            let _ = SendMessageW(lb, LB_RESETCONTENT, None, None);
        }
        set_enabled(dlg, ID_ASSIGN, true);
        set_enabled(dlg, ID_BIND_SCRIPT, true);
        set_enabled(dlg, ID_DELETE, true);
        return;
    };
    let combos = st.map.combos_for(&row.value);
    st.current = combos.clone();
    let live = row.live;
    if let Some(lb) = item(dlg, ID_CURRENT) {
        let _ = SendMessageW(lb, LB_RESETCONTENT, None, None);
        for combo in &combos {
            let text: Vec<u16> = combo.encode_utf16().chain([0]).collect();
            let _ = SendMessageW(
                lb,
                LB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
        let _ = SendMessageW(lb, LB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    set_enabled(dlg, ID_ASSIGN, live);
    set_enabled(dlg, ID_BIND_SCRIPT, live);
    set_enabled(dlg, ID_DELETE, live && !combos.is_empty());
}

unsafe fn set_enabled(dlg: HWND, id: i32, on: bool) {
    if let Some(ctl) = item(dlg, id) {
        let _ = EnableWindow(ctl, on);
        let _ = InvalidateRect(Some(ctl), None, true);
    }
}

unsafe fn item(dlg: HWND, id: i32) -> Option<HWND> {
    GetDlgItem(Some(dlg), id).ok()
}

unsafe fn text_of(dlg: HWND, id: i32) -> String {
    let Some(ctl) = item(dlg, id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(ctl);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(ctl, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

unsafe fn set_text(ctl: HWND, text: &str) {
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(ctl, PCWSTR::from_raw(buf.as_ptr()));
}

/// The combo the two key fields describe: the first stage, and the second one
/// when the user typed it.
unsafe fn new_combo(dlg: HWND) -> Option<String> {
    let first = text_of(dlg, ID_KEY1);
    if first.trim().is_empty() {
        return None;
    }
    let second = text_of(dlg, ID_KEY2);
    Some(if second.trim().is_empty() {
        first.trim().to_string()
    } else {
        format!("{},{}", first.trim(), second.trim())
    })
}

/// 分配 (1203): bind the new combo to the selected row.
unsafe fn on_assign(dlg: HWND) {
    let Some(combo) = new_combo(dlg) else {
        return;
    };
    let (value, live) = {
        let st = state(dlg);
        let Some(row) = st.rows.get(st.sel.max(0) as usize).filter(|_| st.sel >= 0) else {
            return;
        };
        (row.value.clone(), row.live)
    };
    if !live {
        return;
    }
    // A combo can only run one thing.
    if let Some(owner) = state(dlg).map.value_of(&combo) {
        if owner != value {
            let question = format!(
                "这个快捷键已经被分配给:\n[ {} ]\n需要把它删除吗?",
                command_label(&state(dlg).rows, owner)
            );
            let text: Vec<u16> = question.encode_utf16().chain([0]).collect();
            let title: Vec<u16> = crate::win32::lang::dlg(210, 0)
                .unwrap_or("快捷键管理")
                .encode_utf16()
                .chain([0])
                .collect();
            let answer = MessageBoxW(
                Some(dlg),
                PCWSTR::from_raw(text.as_ptr()),
                PCWSTR::from_raw(title.as_ptr()),
                MB_YESNO,
            );
            if answer != IDYES {
                return;
            }
        }
    }
    let st = state_mut(dlg);
    st.map.set_binding(&combo, &value);
    store(dlg);
    clear_new_combo(dlg);
}

/// 删除 (1207): drop the selected binding, or the row's only one when the
/// 当前快捷键 box has no selection.
unsafe fn on_delete(dlg: HWND) {
    let combo = {
        let st = state(dlg);
        let picked = item(dlg, ID_CURRENT)
            .map(|lb| SendMessageW(lb, LB_GETCURSEL, None, None).0 as i32)
            .unwrap_or(-1);
        if picked >= 0 {
            st.current.get(picked as usize).cloned()
        } else if st.current.len() == 1 {
            st.current.first().cloned()
        } else {
            None
        }
    };
    let Some(combo) = combo else {
        return;
    };
    state_mut(dlg).map.clear_combo(&combo);
    store(dlg);
}

/// 绑定脚本 (1202): pick a script and bind the new combo to its path.
unsafe fn on_bind_script(dlg: HWND) {
    let Some(combo) = new_combo(dlg) else {
        return;
    };
    let start = script_dir();
    let Some(path) = pick_script(dlg, start.as_deref()) else {
        return;
    };
    // A path inside the install's own `macro` folder is written relative to the
    // executable, so the section stays portable.
    let value = relative_to_exe(&path).unwrap_or(path);
    state_mut(dlg).map.set_binding(&combo, &value);
    store(dlg);
    clear_new_combo(dlg);
}

unsafe fn clear_new_combo(dlg: HWND) {
    for id in [ID_KEY1, ID_KEY2] {
        if let Some(edit) = item(dlg, id) {
            set_text(edit, "");
        }
    }
    if let Some(edit) = item(dlg, ID_KEY1) {
        let _ = SetFocus(Some(edit));
    }
}

/// Write the section back and hand the frame the reload. The list and the
/// 当前快捷键 box are re-derived from the map, so a failed write still leaves
/// the dialog honest about what is on disk.
unsafe fn store(dlg: HWND) {
    let (frame, saved, where_) = {
        let st = state(dlg);
        let where_ = st
            .map
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "key.ini".to_string());
        (st.frame, st.map.save(), where_)
    };
    match saved {
        Ok(()) => {
            let _ = PostMessageW(Some(frame), WM_SHORTCUTS, WPARAM(0), LPARAM(0));
        }
        Err(err) => {
            let message = format!("无法写入 {}:\n{}", where_, err);
            let text: Vec<u16> = message.encode_utf16().chain([0]).collect();
            let title: Vec<u16> = crate::win32::lang::dlg(210, 0)
                .unwrap_or("快捷键管理")
                .encode_utf16()
                .chain([0])
                .collect();
            MessageBoxW(
                Some(dlg),
                PCWSTR::from_raw(text.as_ptr()),
                PCWSTR::from_raw(title.as_ptr()),
                Default::default(),
            );
        }
    }
    let keep = state(dlg).sel;
    fill_list(dlg);
    select_row(dlg, visible_index(dlg, keep));
}

/// The visible-row index of a `rows` index, or -1 when the filter hides it.
unsafe fn visible_index(dlg: HWND, row: i32) -> i32 {
    let st = state(dlg);
    st.shown
        .iter()
        .position(|&i| i as i32 == row)
        .map(|i| i as i32)
        .unwrap_or(-1)
}

/// The 命令 the prompt names for a value: the row's label, or the path itself
/// for a script.
fn command_label(rows: &[Row], value: &str) -> String {
    rows.iter()
        .find(|r| r.value == value)
        .map(|r| r.label.clone())
        .unwrap_or_else(|| value.to_string())
}

/// `<exe_dir>\macro`, the folder the 扩展 menu's script rows come from.
fn script_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("macro"))
}

/// A path under `<exe_dir>` written the way `key.ini` stores it: `macro\x.mac`.
fn relative_to_exe(path: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let full = std::path::Path::new(path).canonicalize().ok()?;
    let base = dir.canonicalize().ok()?;
    let rel = full.strip_prefix(&base).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// The script picker. Both are offered before "everything".
unsafe fn pick_script(dlg: HWND, start: Option<&std::path::Path>) -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OPENFILENAMEW,
    };
    let mut file = [0u16; 520];
    let filter: Vec<u16> = "脚本\0*.mac;*.macro\0所有文件\0*.*\0\0".encode_utf16().collect();
    let initial: Vec<u16> = match start {
        Some(dir) => dir
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain([0])
            .collect(),
        None => Vec::new(),
    };
    let title: Vec<u16> = "绑定脚本".encode_utf16().chain([0]).collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: dlg,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: if initial.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(initial.as_ptr())
        },
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

/// `LVN_ITEMCHANGED` as this module reads it: the fields around the header, in
/// the order `commctrl.h` declares them.
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

unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
    let hdr = &*(lparam.0 as *const NMHDR);
    if hdr.idFrom as i32 != ID_LIST {
        return 0;
    }
    let st = state(dlg);
    if hdr.code == LVN_ITEMCHANGED {
        if st.loading {
            return 1;
        }
        let n = &*(lparam.0 as *const ItemChanged);
        // The new state decides, not `u_changed`: a click that moves the
        // selection here arrives with only `LVIS_FOCUSED` in the changed mask
        // and both bits in the new state, so keying off the mask left the
        // dialog's own fields stale.
        if n.u_new_state & LVIS_SELECTED.0 != 0 {
            select_row(dlg, n.i_item);
            return 1;
        }
        // The row that *was* selected has just been cleared.
        let row = st.shown.get(n.i_item.max(0) as usize).copied();
        if st.sel >= 0 && row == Some(st.sel as usize) {
            select_row(dlg, -1);
            return 1;
        }
        return 0;
    }
    if hdr.code == NM_CUSTOMDRAW {
        return on_custom_draw(dlg, lparam);
    }
    0
}

/// Per-row colour: the greyed commands this implementation cannot run, and the theme's
/// selection pair on the selected row.
unsafe fn on_custom_draw(dlg: HWND, lparam: LPARAM) -> isize {
    let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
    let st = state(dlg);
    customdraw::row(cd, false, |row, selected, _| {
        let live = st
            .shown
            .get(row)
            .and_then(|&i| st.rows.get(i))
            .map(|r| r.live)
            .unwrap_or(true);
        if selected {
            (to_colorref(st.theme.foreground), to_colorref(st.theme.selection_bg))
        } else if live {
            (to_colorref(st.theme.panel_fg), to_colorref(st.theme.background))
        } else {
            (to_colorref(st.theme.menu_disabled), to_colorref(st.theme.background))
        }
    })
}

/// The header band: four captions on the dialog's face
/// separators. Painted here because the list's own header cannot be re-coloured.
unsafe fn paint_header(dlg: HWND, hdc: HDC) {
    let st = state(dlg);
    let mut client = RECT::default();
    let _ = GetClientRect(dlg, &mut client);
    let top = scale(HEADER_TOP, st.dpi);
    let bottom = top + scale(HEADER_H, st.dpi);
    let band = RECT {
        left: scale(MARGIN, st.dpi),
        top,
        right: client.right - scale(MARGIN, st.dpi),
        bottom,
    };
    FillRect(hdc, &band, st.face);
    let mut x = band.left;
    for (i, caption) in HEADER_CAPTIONS.iter().enumerate() {
        let caption = crate::win32::lang::trs(caption);
        let w = scale(COLUMNS[i], st.dpi);
        let cell = RECT {
            left: x + scale(6, st.dpi),
            top,
            right: x + w,
            bottom,
        };
        ownerdraw::draw_text(
            hdc,
            cell,
            st.font,
            to_colorref(st.theme.panel_fg),
            &caption,
            ownerdraw::Align::Left(0),
        );
        x += w;
        if i + 1 < HEADER_CAPTIONS.len() {
            let line = RECT {
                left: x,
                top,
                right: x + 1,
                bottom,
            };
            FillRect(hdc, &line, st.edge);
        }
    }
    // The rule under the band, and the frame down both sides of the list.
    let rule = RECT {
        left: band.left,
        top: bottom,
        right: band.right,
        bottom: bottom + 1,
    };
    FillRect(hdc, &rule, st.edge);
    // The 1px frame around the list, the sides and bottom the header band's
    // rule does not cover. Drawn just outside the control's rect so the list
    // does not paint over it.
    if let Some(list) = item(dlg, ID_LIST) {
        let mut r = RECT::default();
        if GetWindowRect(list, &mut r).is_ok() {
            let mut tl = POINT {
                x: r.left,
                y: r.top,
            };
            let mut br = POINT {
                x: r.right,
                y: r.bottom,
            };
            let _ = ScreenToClient(dlg, &mut tl);
            let _ = ScreenToClient(dlg, &mut br);
            let frame = RECT {
                left: tl.x - 1,
                top: tl.y - 1,
                right: br.x + 1,
                bottom: br.y + 1,
            };
            FrameRect(hdc, &frame, st.edge);
        }
    }
}

/// The key field's subclass: a key press becomes the combo it names, never a
/// character. The first field hands the focus to the second one, which is how
/// the dialog asks for a chord's second stage.
unsafe extern "system" fn key_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    dlg: usize,
) -> LRESULT {
    let dlg = HWND(dlg as *mut core::ffi::c_void);
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u32;
            // A modifier on its own is half of the next combo, not a combo.
            if matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5) {
                return LRESULT(0);
            }
            if let Some(text) = combo_text(modifier_state(), vk) {
                set_text(hwnd, &text);
                if GetDlgCtrlID(hwnd) == ID_KEY1 {
                    if let Some(second) = item(dlg, ID_KEY2) {
                        let _ = SetFocus(Some(second));
                    }
                }
            }
            LRESULT(0)
        }
        // The character the key would have typed.
        WM_CHAR | WM_SYSCHAR => LRESULT(0),
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let notify = (wparam.0 >> 16) as u16;
    match id {
        x if x == IDCANCEL.0 => {
            let _ = DestroyWindow(dlg);
            1
        }
        ID_ASSIGN if notify == 0 => {
            on_assign(dlg);
            1
        }
        ID_DELETE if notify == 0 => {
            on_delete(dlg);
            1
        }
        ID_BIND_SCRIPT if notify == 0 => {
            on_bind_script(dlg);
            1
        }
        ID_FILTER if notify == 0x0300 => {
            // EN_CHANGE: the filter runs as the user types.
            fill_list(dlg);
            1
        }
        ID_CURRENT if notify == 1 => {
            // LBN_SELCHANGE: 删除 acts on the picked binding.
            let has = item(dlg, ID_CURRENT)
                .map(|lb| SendMessageW(lb, LB_GETCURSEL, None, None).0 as i32 >= 0)
                .unwrap_or(false);
            set_enabled(dlg, ID_DELETE, has);
            1
        }
        _ => 0,
    }
}

unsafe fn on_drop(dlg: HWND, hdrop: HDROP) {
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, None) as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        if DragQueryFileW(hdrop, i, Some(&mut buf)) == 0 {
            continue;
        }
        buf.truncate(len);
        let path = String::from_utf16_lossy(&buf);
        add_script_row(dlg, &path);
    }
    DragFinish(hdrop);
    fill_list(dlg);
}

unsafe fn add_script_row(dlg: HWND, path: &str) {
    let st = state_mut(dlg);
    let value = relative_to_exe(path).unwrap_or_else(|| path.to_string());
    if st.rows.iter().any(|r| r.value == value) {
        return;
    }
    st.rows.push(Row {
        value: value.clone(),
        id: value.clone(),
        label: "脚本".to_string(),
        desc: path.to_string(),
        live: true,
    });
    // Script rows sort after the commands, in the order they were dropped.
    st.sel = st.rows.len() as i32 - 1;
}

/// `DLGPROC` is a plain `-> isize` fn pointer in this metadata.
unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            crate::win32::lang::localize_dialog(hwnd, 210);
            1
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_NOTIFY => on_notify(hwnd, lparam),
        WM_SIZE => {
            let w = (lparam.0 & 0xFFFF) as i32;
            let h = ((lparam.0 >> 16) & 0xFFFF) as i32;
            layout(hwnd, w, h);
            1
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let dpi = state(hwnd).dpi;
            mmi.ptMinTrackSize.x = scale(CLIENT_W, dpi);
            mmi.ptMinTrackSize.y = scale(CLIENT_H, dpi);
            1
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_header(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            1
        }
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state(hwnd).bg);
            }
            1
        }
        WM_CTLCOLOREDIT => {
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(st.theme.foreground),
                to_colorref(st.theme.background),
                st.bg,
            )
        }
        WM_CTLCOLORLISTBOX => {
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(st.theme.panel_fg),
                to_colorref(st.theme.background),
                st.bg,
            )
        }
        // The statics need this arm. A themed push button ignores it — the
        // visual style owns its face and caption — and an unthemed one only
        // takes the dialog's background brush from it.
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state(hwnd);
            let ctl = HWND(lparam.0 as *mut core::ffi::c_void);
            let hdc = HDC(wparam.0 as *mut _);
            let red = GetDlgCtrlID(ctl) == ID_HINT;
            let colour = if red {
                dg_render::theme::Color::rgb(HINT_RED.0, HINT_RED.1, HINT_RED.2)
            } else {
                st.theme.panel_fg
            };
            // `SetBkMode` is this arm's own: the red hint label is drawn
            // transparent so the dialog face shows through.
            SetBkMode(hdc, TRANSPARENT);
            customdraw::ctlcolor(hdc, to_colorref(colour), to_colorref(st.theme.background), st.bg)
        }
        WM_DROPFILES => {
            on_drop(hwnd, HDROP(lparam.0 as *mut core::ffi::c_void));
            1
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        // The manager is gone, but its state stays: the controls are destroyed
        // before this window is, and the last few notifications they send
        // (`WM_COMMAND`, `WM_NOTIFY`) still read it. Dropping the `Box` here
        // left those to dereference a null `GWLP_USERDATA`, which aborts the
        // process — the state is freed at `WM_NCDESTROY` instead.
        WM_DESTROY => {
            DLG.store(0, Ordering::Release);
            for id in [ID_KEY1, ID_KEY2] {
                if let Some(edit) = item(hwnd, id) {
                    let _ = RemoveWindowSubclass(edit, Some(key_edit_proc), 1);
                }
            }
            0
        }
        WM_NCDESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !ptr.is_null() {
                let b = Box::from_raw(ptr);
                let _ = DeleteObject(HGDIOBJ(b.font.0));
                let _ = DeleteObject(HGDIOBJ(b.bg.0));
                let _ = DeleteObject(HGDIOBJ(b.face.0));
                let _ = DeleteObject(HGDIOBJ(b.edge.0));
            }
            0
        }
        _ => 0,
    }
}

/// The dialog's font: the shell's dialog face at the dialog's DPI, not the
/// editor's mono face.
fn ui_font(dpi: f32) -> HFONT {
    unsafe {
        let face: Vec<u16> = "Segoe UI".encode_utf16().chain([0]).collect();
        CreateFontW(
            -scale(12, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR::from_raw(face.as_ptr()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the list round-trip reads a control back; the dialog itself never
    // asks the list what it holds.
    use windows::Win32::UI::Controls::LVM_GETITEMTEXTW;

    /// The grid is a measurement: no control may sit outside the client at the
    /// default size, and the ids have to stay unique for `GetDlgItem`.
    /// The notification the selection arrives as has to line up with
    /// `NMLISTVIEW`: `u_changed` at 40 on x64 (NMHDR is 24, iItem and iSubItem
    /// are 4 each, then the two state words).
    #[test]
    fn item_changed_matches_nmlistview() {
        assert_eq!(core::mem::size_of::<NMHDR>(), 24);
        assert_eq!(core::mem::offset_of!(ItemChanged, i_item), 24);
        assert_eq!(core::mem::offset_of!(ItemChanged, u_new_state), 32);
        assert_eq!(core::mem::offset_of!(ItemChanged, u_changed), 40);
    }

    #[test]
    fn the_measured_grid_fits() {
        assert_eq!((CLIENT_W, CLIENT_H), (613, 522));
        // Every template rectangle, at 1.4 x / 1.6 y, inside the client.
        let rects = [
            (ID_FILTER, 10, 11, 594, 22),
            (ID_LIST, 10, HEADER_TOP + HEADER_H, 594, LIST_H),
            (ID_HINT, 10, 366, 574, 18),
            (ID_L_CURRENT, 10, 389, 119, 16),
            (ID_L_NEW, 164, 389, 126, 16),
            (ID_CURRENT, 10, 408, 147, 77),
            (ID_KEY1, 164, 408, 154, 21),
            (ID_KEY2, 164, 434, 154, 21),
            (ID_ASSIGN, 162, 462, 70, 22),
            (ID_BIND_SCRIPT, 235, 462, 70, 22),
            (ID_DELETE, 8, 488, 70, 22),
        ];
        for (id, x, y, w, h) in rects {
            assert!(x + w <= CLIENT_W, "{id} overflows the width");
            assert!(y + h <= CLIENT_H, "{id} overflows the height");
        }
    }

    /// the last one may leave the list's right edge unpainted.
    #[test]
    fn columns_are_the_measured_widths() {
        assert_eq!(COLUMNS, [59, 144, 120, 160]);
        assert!(COLUMNS.iter().sum::<i32>() < CLIENT_W - 2 * MARGIN);
    }

    /// The three buttons must stay native `BS_PUSHBUTTON` controls. DLG 210
    /// declares them that way, and the visual style is what draws the rounded
    /// dark face, the centred caption and the hover/pressed states. A source
    /// scan, because `build_children` needs a live dialog with a valid
    /// `GWLP_USERDATA` state; the strings below are exactly the hooks a return
    /// to parent-painted buttons would need.
    #[test]
    fn the_three_buttons_stay_native_push_buttons() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/win32/shortcutdlg.rs");
        let src = std::fs::read_to_string(&path).expect("shortcutdlg.rs");
        let body = src.split("mod tests {").next().unwrap();
        for gone in [
            "BS_OWNERDRAW",
            "WM_DRAWITEM",
            "ownerdraw::install",
            "face_hot",
            "button_look",
        ] {
            assert!(
                !body.contains(gone),
                "{gone} would put the buttons back under this dialog's own paint"
            );
        }
        assert_eq!(
            body.matches("BS_PUSHBUTTON as u32").count(),
            3,
            "one push-button style per button"
        );
        assert!(
            body.contains("dark_theme_children"),
            "the visual style needs its dark sub-app name"
        );
    }

    /// Every listed command has a name.
    #[test]
    fn command_rows_are_named_and_sorted() {
        let rows = command_rows();
        assert!(rows.len() > 300, "only {} rows", rows.len());
        for pair in rows.windows(2) {
            assert!(pair[0].id <= pair[1].id, "{} after {}", pair[0].id, pair[1].id);
        }
        assert!(rows.iter().all(|r| !r.id.is_empty()));
        // This implementation has implementations for a good part of the table; if this
        // ever hits zero the `command_for_id` wiring broke.
        assert!(rows.iter().any(|r| r.live));
    }

    /// The four columns are written through comctl32, so the `LVITEMW` shapes
    /// [`fill_list`] builds are the ones that matter: this inserts a row the
    /// same way and reads every cell back. A blank list is what a wrong
    /// `pszText` or `iSubItem` looks like at runtime, and it looks like a
    /// layout bug from the outside.
    #[test]
    fn list_stores_the_four_columns() {
        unsafe {
            let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwICC: ICC_BAR_CLASSES,
                ..Default::default()
            });
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                200,
                200,
                None,
                None,
                Some(instance()),
                None,
            )
            .unwrap();
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("SysListView32"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | LVS_REPORT),
                0,
                0,
                200,
                200,
                Some(owner),
                Some(HMENU(1 as *mut core::ffi::c_void)),
                Some(instance()),
                None,
            )
            .unwrap();
            for (i, w) in COLUMNS.iter().enumerate() {
                let mut col = LVCOLUMNW {
                    mask: LVCF_WIDTH,
                    cx: *w,
                    ..Default::default()
                };
                let _ = SendMessageW(
                    list,
                    LVM_INSERTCOLUMNW,
                    Some(WPARAM(i)),
                    Some(LPARAM(&raw mut col as isize)),
                );
            }
            // Row 0 is a plain 按键 cell; row 1 carries all four columns. The
            // row a `LVM_INSERTITEMW` lands on is `LVITEMW.iItem` — comctl32
            // here ignores wParam (and `-1` for it does not mean "append", it
            // means "the top"), which is how the subitems first ended up on
            // the wrong command: the insert put every row at index 0 while the
            // writer addressed `count - 1`.
            let mut first: Vec<u16> = "C+K,D".encode_utf16().chain([0]).collect();
            let item0 = LVITEMW {
                mask: LVIF_TEXT,
                pszText: PWSTR(first.as_mut_ptr()),
                ..Default::default()
            };
            let at0 = SendMessageW(
                list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item0 as isize)),
            );
            assert_eq!(at0.0, 0, "LVM_INSERTITEMW failed");

            let mut cells: Vec<Vec<u16>> = ["C+K,R", "cm_doc_browsermode", "阅览模式", "切换"]
                .iter()
                .map(|s| s.encode_utf16().chain([0]).collect())
                .collect();
            let item1 = LVITEMW {
                mask: LVIF_TEXT | LVIF_PARAM,
                iItem: 1,
                pszText: PWSTR(cells[0].as_mut_ptr()),
                lParam: LPARAM(7),
                ..Default::default()
            };
            let at1 = SendMessageW(
                list,
                LVM_INSERTITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const item1 as isize)),
            );
            let row = at1.0 as usize;
            assert_eq!(row, 1, "the second insert did not land on row 1");

            for sub in 1..4 {
                let mut cell = LVITEMW {
                    mask: LVIF_TEXT,
                    iSubItem: sub as i32,
                    pszText: PWSTR(cells[sub].as_mut_ptr()),
                    ..Default::default()
                };
                let _ = SendMessageW(
                    list,
                    LVM_SETITEMTEXTW,
                    Some(WPARAM(row)),
                    Some(LPARAM(&raw mut cell as isize)),
                );
            }
            let read = |row: usize, sub: usize| {
                let mut buf = [0u16; 64];
                let mut item = LVITEMW {
                    mask: LVIF_TEXT,
                    iSubItem: sub as i32,
                    pszText: PWSTR(buf.as_mut_ptr()),
                    cchTextMax: buf.len() as i32,
                    ..Default::default()
                };
                let n = SendMessageW(
                    list,
                    LVM_GETITEMTEXTW,
                    Some(WPARAM(row)),
                    Some(LPARAM(&raw mut item as isize)),
                );
                String::from_utf16_lossy(&buf[..(n.0.max(0) as usize).min(buf.len())])
            };
            for sub in 0..4 {
                assert_eq!(
                    read(row, sub),
                    String::from_utf16_lossy(&cells[sub][..cells[sub].len() - 1]),
                    "row {row} column {sub}"
                );
            }
            // The row above kept its own text: an insert that landed on top
            // would have pushed it down and left row 0 with this row's 按键.
            assert_eq!(read(0, 0), "C+K,D");
            // The row above kept its own text: an insert at the top would have
            // moved it down and left row 0 holding this row's first column.
            assert_eq!(read(0, 0), "C+K,D");
            let _ = DestroyWindow(list);
            let _ = DestroyWindow(owner);
        }
    }
}
