//! 自定义标记 manager: a native modeless `#32770`, opened by the 自定义标记 command.
//! Three things this dialog has to keep straight:
//!
//! * **Lifecycle** is one modeless instance. `cm_misc_usermarker` builds
//!   the dialog with `CreateDialogParamW` the first time and `ShowWindow(h, 5)`
//!   on every later one — one instance, modeless, owned by the frame. The
//!   开启该功能 flag lives on the state field; this window holds no copy of it.
//! * **Geometry** is the dialog grid below, and that grid is its only source.
//! * **Colour** follows the rule that every visible colour comes from `Theme`.
//!   The dialog is a `#202020` face with `#2D2D2D` controls, `#4C4A48`
//!   edges and `#828790` text — `background`, `panel_bg`, `indent_line`,
//!   `panel_fg`.
//!
//! The four checks are native `BS_AUTOCHECKBOX`. Reproducing system-metric box
//! pixels is not a goal: an unchecked box is a `#282828` fill inside a
//! `#4a4a4a` edge, inverting to
//! `#4a4a4a` with a `#989898` tick when checked — but matching that is not a
//! goal: a native control gets the tick, the hit-test, the focus ring and the
//! space bar for free, and its checked box is the one the rest of the app
//! already shows (查找 has eight of them). Two things follow from the choice and
//! are easy to lose:
//!
//! * they are the only children that need [`chrome::dark_theme`], because a
//!   themed button paints itself and ignores `WM_CTLCOLORBTN` — once the button
//!   is themed, that message never arrives
//! * the dialog answers `WM_PRINTCLIENT`, because that is how a themed check
//!   asks its parent for the background behind the caption. Without it the
//!   `#32770` class brush answers `COLOR_3DFACE` `#F0F0F0` and each check wears
//!   a light block on the dark face.
//!
//! The remaining buttons are `BS_OWNERDRAW` and the list answers
//! `NM_CUSTOMDRAW`, because their faces (`#2D2D2D` on a `#202020` dialog) are
//! not something a themed `BUTTON` produces. The list's row checkboxes are
//! `LVS_EX_CHECKBOXES` state images: the control's own drawing and hit-testing,
//! not ours.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlggeom;
use crate::win32::dpi::scale;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, FillRect, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::SystemServices::SS_OWNERDRAW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, LIST_VIEW_ITEM_STATE_FLAGS, LVCF_WIDTH, LVCOLUMNW, LVIF_STATE, LVIF_TEXT,
    LVIS_FOCUSED, LVIS_SELECTED, LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR,
    LVS_EX_CHECKBOXES, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_NOCOLUMNHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVN_ITEMCHANGED,
    NM_CUSTOMDRAW, NMHDR, NMLVCUSTOMDRAW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, CreateDialogIndirectParamW,
    CreateWindowExW, DLGTEMPLATE, DestroyWindow, ES_AUTOHSCROLL, GA_ROOT, GetAncestor,
    GetClientRect, GetDlgItem, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    GWLP_USERDATA, HMENU, IDCANCEL, IsDialogMessageW, MSG, PostMessageW, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, SW_SHOW, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_DRAWITEM, WM_ERASEBKGND, WM_INITDIALOG, WM_NCDESTROY, WM_NOTIFY, WM_PRINTCLIENT,
    WM_SETFONT, WM_SETTEXT, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{PCWSTR, w};

use crate::markers::MarkSet;
use crate::win32::chrome;
use crate::win32::customdraw;
use crate::win32::ownerdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_core::FindOptions;
use dg_render::theme::{Theme, luma_blend};

const BUTTON_CLASS: &str = "Button";
const EDIT_CLASS: &str = "Edit";
const LIST_CLASS: &str = "SysListView32";
const STATIC_CLASS: &str = "Static";

/// Control ids
const ID_REGEX: i32 = 1200;
const ID_CASE: i32 = 1201;
const ID_WORD: i32 = 1202;
const ID_ADD: i32 = 1203;
const ID_DEL: i32 = 1204;
const ID_UP: i32 = 1205;
const ID_DOWN: i32 = 1206;
const ID_ALL_ON: i32 = 1207;
const ID_ALL_OFF: i32 = 1208;
const ID_RENAME: i32 = 1209;
const ID_ENABLE: i32 = 1210;
const ID_TEXT: i32 = 1211;
const ID_LIST: i32 = 1212;
/// The separator's id: the static carries none of its own.
const ID_LINE: i32 = 1214;
const ID_APPLY: i32 = 1213;
/// 关闭. The dialog has no id-2 control and its default button is this
/// `IDOK` row (`0x50010001` = `BS_TEXT | BS_DEFPUSHBUTTON`), so Enter closes it.
const ID_CLOSE: i32 = 1;

/// The dialog's client: the 436x465 client at dpi 120, divided by 1.25.
const CLIENT_W: i32 = 349;
const CLIENT_H: i32 = 372;

/// The message the dialog posts to the frame after it changed the shared set.
pub(super) const WM_MARKS: u32 = WM_APP + 0x47;

/// One control of the dialog's grid. The rectangles are the 125% figures divided
/// by 1.25, so a control that moves here is a control that no longer lines up
/// with the grid.
struct Row {
    id: i32,
    class: &'static str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// 1 = `BS_OWNERDRAW` button, 2 = a tab stop, 3 = the edit, 4 = the
    /// owner-drawn separator, 5 = the list view, 6 = a native checkbox.
    kind: u32,
    label: &'static str,
}

/// The 17 children, in the order the measurement lists them.
const ROWS: &[Row] = &[
    Row { id: ID_LIST, class: LIST_CLASS, x: 17, y: 42, w: 161, h: 248, kind: 5, label: "" },
    Row { id: ID_TEXT, class: EDIT_CLASS, x: 185, y: 42, w: 112, h: 22, kind: 3, label: "" },
    Row { id: ID_RENAME, class: BUTTON_CLASS, x: 302, y: 42, w: 31, h: 22, kind: 1, label: "✎" },
    Row { id: ID_CASE, class: BUTTON_CLASS, x: 185, y: 74, w: 147, h: 16, kind: 6, label: "匹配大小写(&S)" },
    Row { id: ID_WORD, class: BUTTON_CLASS, x: 185, y: 98, w: 147, h: 16, kind: 6, label: "全字匹配(&W)" },
    Row { id: ID_REGEX, class: BUTTON_CLASS, x: 185, y: 120, w: 147, h: 16, kind: 6, label: "正则表达式(&R)" },
    Row { id: ID_ADD, class: BUTTON_CLASS, x: 185, y: 154, w: 84, h: 22, kind: 1, label: "添加(&A)" },
    Row { id: ID_DEL, class: BUTTON_CLASS, x: 185, y: 186, w: 84, h: 22, kind: 1, label: "删除(&D)" },
    Row { id: ID_ALL_ON, class: BUTTON_CLASS, x: 14, y: 294, w: 26, h: 22, kind: 2, label: "☑" },
    Row { id: ID_ALL_OFF, class: BUTTON_CLASS, x: 42, y: 294, w: 26, h: 22, kind: 2, label: "☐" },
    Row { id: ID_UP, class: BUTTON_CLASS, x: 70, y: 294, w: 26, h: 22, kind: 2, label: "↑" },
    Row { id: ID_DOWN, class: BUTTON_CLASS, x: 98, y: 294, w: 26, h: 22, kind: 2, label: "↓" },
    Row { id: ID_LINE, class: STATIC_CLASS, x: 17, y: 325, w: 315, h: 2, kind: 4, label: "" },
    Row { id: ID_ENABLE, class: BUTTON_CLASS, x: 17, y: 330, w: 140, h: 24, kind: 6, label: "开启该功能(&E)" },
    Row { id: ID_APPLY, class: BUTTON_CLASS, x: 188, y: 330, w: 70, h: 22, kind: 1, label: "应用(&P)" },
    Row { id: ID_CLOSE, class: BUTTON_CLASS, x: 262, y: 330, w: 70, h: 22, kind: 1, label: "关闭" },
];

/// The three option checks and 开启该功能, in the order their bits are stored.
const OPT_IDS: [i32; 4] = [ID_REGEX, ID_CASE, ID_WORD, ID_ENABLE];

/// The commands that need a selected row. The measurement had 1205/1206
/// `WS_DISABLED`, which is exactly this state with nothing selected.
const ROW_CMDS: [i32; 4] = [ID_DEL, ID_UP, ID_DOWN, ID_RENAME];

/// The dialog's one instance, as the 1318 handler keeps it.
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

/// What the dialog needs when 1318 asks for it.
pub(super) struct Request<'a> {
    pub frame: HWND,
    pub markers: Rc<RefCell<MarkSet>>,
    pub theme: &'a Theme,
}

struct Dlg {
    frame: HWND,
    markers: Rc<RefCell<MarkSet>>,
    theme: Theme,
    font: HFONT,
    /// The dialog face, the button face, the button face under the cursor, the
    /// field face, and the one-pixel edge.
    bg: HBRUSH,
    face: HBRUSH,
    face_hot: HBRUSH,
    edge: HBRUSH,
    dpi: f32,
    /// The option checks and 开启该功能, mirrored from the model.
    flags: AtomicU32,
    /// The selected list row, or -1.
    sel: AtomicI32,
    /// The options the checks show while no row is selected, i.e. the ones 添加
    /// will hand a new entry.
    pending: Cell<FindOptions>,
}

unsafe fn state(hwnd: HWND) -> &'static Dlg {
    &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Dlg)
}

impl Dlg {
    /// What [`ownerdraw`] needs to paint one of this dialog's buttons. The
    /// pressed state brightens the **label** rather than the face — that is
    /// what the measurement shows, so `press` is the resting face.
    fn button_look(&self) -> ownerdraw::Look {
        ownerdraw::Look {
            face: self.face,
            hot: self.face_hot,
            press: self.face,
            edge: ownerdraw::Edge::Square(self.edge),
            ink: ownerdraw::Ink {
                normal: to_colorref(self.theme.panel_fg),
                pressed: to_colorref(self.theme.foreground),
                disabled: to_colorref(self.theme.menu_disabled),
            },
            font: self.font,
            under: None,
            inset: 0,
            press_offset: 0,
            live_rect: false,
        }
    }
}

/// Show the manager, creating it the first time
/// `CreateDialogParamW` / `ShowWindow(h, 5)` pair.
pub(super) fn show(req: Request<'_>) {
    unsafe {
        if let Some(existing) = dlg_hwnd() {
            let _ = ShowWindow(existing, SW_SHOW);
            rebuild_list(existing);
            apply_flags(existing);
            let _ = SetFocus(Some(existing));
            return;
        }
        let dpi = GetDpiForWindow(req.frame).max(96) as f32 / 96.0;
        create(req, dpi);
    }
}

/// Re-read the model into an open dialog.
pub(super) fn refresh() {
    let Some(dlg) = dlg_hwnd() else {
        return;
    };
    unsafe {
        rebuild_list(dlg);
        apply_flags(dlg);
    }
}

/// Item-less `DLGTEMPLATE`; the three trailing words are "no menu, the
/// predefined `#32770` class, no caption". The caption comes after creation,
/// because putting it here would mean DWORD-aligning a variable-length string
/// by hand before the zero-item array.
#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

const DLG_STYLE: u32 = WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;

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
        // All three optional words zero, as `RC.exe` emits them. `0xFFFF` is not
        // "none" to the dialog manager — it reads the next word as a menu
        // ordinal and then fails looking for a menu in an image that has none.
        tail: [0, 0, 0],
    };
    let boxed = Box::new(Dlg {
        frame: req.frame,
        markers: req.markers.clone(),
        theme: req.theme.clone(),
        font: ui_font(dpi),
        bg: CreateSolidBrush(to_colorref(req.theme.background)),
        face: CreateSolidBrush(to_colorref(req.theme.panel_bg)),
        // One ink step over the resting face — the same 6 % the 跳转对话框's
        // buttons use, so the two dialogs light up alike.
        face_hot: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.foreground,
            req.theme.panel_bg,
            0.08,
        ))),
        edge: CreateSolidBrush(to_colorref(req.theme.indent_line)),
        dpi,
        flags: AtomicU32::new(0),
        sel: AtomicI32::new(-1),
        pending: Cell::new(FindOptions {
            case_sensitive: true,
            whole_word: false,
            regex: false,
            backward: false,
        }),
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

    let title: Vec<u16> = crate::win32::lang::dlg(251, 0)
        .unwrap_or("自定义标记")
        .encode_utf16()
        .chain([0])
        .collect();
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(title.as_ptr()));

    for row in ROWS {
        build_child(dlg, row);
    }
    // The native checks draw their own captions from the window text; the
    // owner-drawn buttons are localized in `on_draw_item`, which paints from
    // `ROWS` rather than from `WM_GETTEXT`.
    crate::win32::lang::localize_dialog(dlg, 251);
    // The four checks are native, so they need the dark sub-app or comctl32
    // paints them in the light palette. Only they get it: every other child is
    // owner-drawn, and the list has its own colours set in `init_list`.
    let dark = chrome::theme_is_dark(req.theme);
    for id in OPT_IDS {
        if let Some(ctl) = item(dlg, id) {
            chrome::dark_theme(ctl, dark);
        }
    }
    // Escape needs a cancel command to post, and this dialog has no id-2
    // control, so this one is never shown.
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        BUTTON_CLASS_W,
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

    size_and_place(dlg);
    init_list(dlg);
    rebuild_list(dlg);
    select_row(dlg, first_row(dlg));
    let _ = ShowWindow(dlg, SW_SHOW);
    if let Some(edit) = item(dlg, ID_TEXT) {
        let _ = SetFocus(Some(edit));
    }
}

const BUTTON_CLASS_W: PCWSTR = w!("Button");

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

unsafe fn build_child(dlg: HWND, row: &Row) {
    let dpi = state(dlg).dpi;
    let style = match row.kind {
        1 | 2 => WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE).0
                | BS_OWNERDRAW as u32
                | if row.kind == 2 { WS_TABSTOP.0 } else { 0 },
        ),
        3 => WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32),
        4 => WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0 | SS_OWNERDRAW.0),
        // A real checkbox: the control owns the tick, the hit-test and the
        // space bar, so nothing here has to draw or track it.
        6 => WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32),
        // The list view: `0x50014C0D` — report view, single
        // selection, selection kept while the dialog loses focus, and no column
        // header, which is why the `SysHeader32` comes out 197x0.
        _ => WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOCOLUMNHEADER),
        ),
    };
    let class: Vec<u16> = row.class.encode_utf16().chain([0]).collect();
    let label: Vec<u16> = row.label.encode_utf16().chain([0]).collect();
    let Ok(ctl) = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        PCWSTR::from_raw(class.as_ptr()),
        PCWSTR::from_raw(label.as_ptr()),
        style,
        scale(row.x, dpi),
        scale(row.y, dpi),
        scale(row.w, dpi),
        scale(row.h, dpi),
        Some(dlg),
        Some(HMENU(row.id as usize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    ) else {
        return;
    };
    if row.kind != 4 {
        let _ = SendMessageW(
            ctl,
            WM_SETFONT,
            Some(WPARAM(state(dlg).font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    // The row commands start disabled until a selection says otherwise.
    if ROW_CMDS.contains(&row.id) {
        let _ = EnableWindow(ctl, false);
    }
}

/// The single selection the row commands assume. The three colour messages are
/// what makes the control dark: a `SysListView32` starts out
/// `COLOR_WINDOW`/`COLOR_WINDOWTEXT`, and it paints the area below the last row
/// itself — no amount of custom draw reaches it. `clrTextBk` from
/// `NM_CUSTOMDRAW` only covers the rows it is handed.
unsafe fn init_list(dlg: HWND) {
    let Some(list) = item(dlg, ID_LIST) else {
        return;
    };
    let ex = (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_LABELTIP) as usize;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        Some(WPARAM(ex)),
        Some(LPARAM(ex as isize)),
    );
    let st = state(dlg);
    let bg = to_colorref(st.theme.background).0 as isize;
    let fg = to_colorref(st.theme.foreground).0 as isize;
    let _ = SendMessageW(list, LVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
    let _ = SendMessageW(list, LVM_SETTEXTBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg)));
    let _ = SendMessageW(list, LVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(fg)));
    let mut client = RECT::default();
    let _ = GetClientRect(list, &mut client);
    let mut col = LVCOLUMNW {
        mask: LVCF_WIDTH,
        cx: (client.right - client.left).max(1),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTCOLUMNW,
        Some(WPARAM(0)),
        Some(LPARAM(&raw mut col as isize)),
    );
}

/// Rebuild the list from the model: the row order and text are the set's, and
/// each row's check box is that entry's own `checked` flag. The entry's slot
/// rides along in `iImage`, which is what the custom draw reads back.
///
/// The check state is **not** part of the insert: `LVM_INSERTITEM` documents
/// `state`/`stateMask` as "not used by this message", so a row inserted with a
/// state image comes out unchecked. It is written afterwards, per row.
unsafe fn rebuild_list(dlg: HWND) {
    let Some(list) = item(dlg, ID_LIST) else {
        return;
    };
    let st = state(dlg);
    let rows: Vec<(String, bool, usize)> = st
        .markers
        .borrow()
        .entries
        .iter()
        .map(|m| (m.text.clone(), m.checked, m.slot))
        .collect();
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, None, None);
    for (row, (text, checked, slot)) in rows.into_iter().enumerate() {
        let mut buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: row as i32,
            iSubItem: 0,
            pszText: windows::core::PWSTR(buf.as_mut_ptr()),
            iImage: slot as i32,
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(&raw const item as isize)),
        );
        set_row_state(list, row as i32, checked);
    }
    let _ = InvalidateRect(Some(list), None, true);
}

/// Write one row's check box — only the state-image bits of the item state, so
/// the selection and focus bits the control owns are left alone.
unsafe fn set_row_state(list: HWND, row: i32, on: bool) {
    let item = LVITEMW {
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
        Some(LPARAM(&raw const item as isize)),
    );
}

/// `LVS_EX_CHECKBOXES` keeps the check state in the item's state image: index 1
/// unchecked, index 2 checked, shifted into the state-image field.
fn state_image(on: bool) -> LIST_VIEW_ITEM_STATE_FLAGS {
    LIST_VIEW_ITEM_STATE_FLAGS((if on { 2 } else { 1 }) << 12)
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
    let _ = SendMessageW(
        ctl,
        WM_SETTEXT,
        Some(WPARAM(0)),
        Some(LPARAM(buf.as_ptr() as isize)),
    );
}

unsafe fn first_row(dlg: HWND) -> i32 {
    let Some(list) = item(dlg, ID_LIST) else {
        return -1;
    };
    SendMessageW(list, LVM_GETNEXTITEM, Some(WPARAM(usize::MAX)), Some(LPARAM(-1))).0 as i32
}

/// Make `row` the selection, then re-derive everything the selection drives: the
/// text field, the option checks, and which row commands are enabled.
unsafe fn select_row(dlg: HWND, row: i32) {
    let st = state(dlg);
    st.sel.store(row, Ordering::Relaxed);
    let text = if row >= 0 {
        st.markers
            .borrow()
            .entries
            .get(row as usize)
            .map(|m| m.text.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    if let Some(list) = item(dlg, ID_LIST) {
        let want = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
        let item = LVITEMW {
            mask: LVIF_STATE,
            iItem: row,
            state: want,
            stateMask: want,
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_SETITEMSTATE,
            Some(WPARAM(row.max(0) as usize)),
            Some(LPARAM(&raw const item as isize)),
        );
    }
    if let Some(edit) = item(dlg, ID_TEXT) {
        set_text(edit, &text);
    }
    for id in ROW_CMDS {
        if let Some(ctl) = item(dlg, id) {
            let _ = EnableWindow(ctl, row >= 0);
        }
    }
    apply_flags(dlg);
}

/// Push the model's option bits and the 开启该功能 flag into the four checks.
/// Like the find bar's toggles, these are a view of the model's state and never
/// decide it.
unsafe fn apply_flags(dlg: HWND) {
    let st = state(dlg);
    let (opts, enabled) = {
        let set = st.markers.borrow();
        let sel = st.sel.load(Ordering::Relaxed);
        let opts = if sel >= 0 {
            set.entries.get(sel as usize).map(|m| m.opts)
        } else {
            None
        };
        (opts.unwrap_or(st.pending.get()), set.enabled)
    };
    let mask = (opts.regex as u32)
        | ((opts.case_sensitive as u32) << 1)
        | ((opts.whole_word as u32) << 2)
        | ((enabled as u32) << 3);
    st.flags.store(mask, Ordering::Relaxed);
    // The controls own their ticks, so the model is pushed into them here
    // rather than read back out. `on_command` reads the control after a click.
    for (bit, id) in OPT_IDS.iter().enumerate() {
        if let Some(ctl) = item(dlg, *id) {
            let check = if (mask >> bit) & 1 == 1 {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            };
            let _ = SendMessageW(ctl, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
        }
    }
}

/// The options the three checks show, as the bits [`OPT_IDS`] stores them.
fn flags_to_opts(mask: u32) -> FindOptions {
    FindOptions {
        regex: mask & 1 != 0,
        case_sensitive: mask & 2 != 0,
        whole_word: mask & 4 != 0,
        backward: false,
    }
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let st = state(dlg);
    if id == IDCANCEL.0 || id == ID_CLOSE {
        let _ = DestroyWindow(dlg);
        return 1;
    }
    let sel = st.sel.load(Ordering::Relaxed);
    let changed = match id {
        ID_ADD => {
            let text = text_of(dlg, ID_TEXT);
            let opts = flags_to_opts(st.flags.load(Ordering::Relaxed));
            st.markers.borrow_mut().add(&text, opts);
            rebuild_list(dlg);
            true
        }
        ID_DEL if sel >= 0 => {
            st.markers.borrow_mut().remove(sel as usize);
            rebuild_list(dlg);
            select_row(dlg, first_row(dlg));
            true
        }
        ID_UP | ID_DOWN if sel >= 0 => {
            st.markers.borrow_mut().move_row(sel as usize, id == ID_UP);
            rebuild_list(dlg);
            select_row(dlg, sel);
            true
        }
        ID_ALL_ON | ID_ALL_OFF => {
            st.markers.borrow_mut().set_all_checked(id == ID_ALL_ON);
            rebuild_list(dlg);
            apply_flags(dlg);
            true
        }
        ID_RENAME if sel >= 0 => {
            let text = text_of(dlg, ID_TEXT);
            st.markers.borrow_mut().rename(sel as usize, &text);
            rebuild_list(dlg);
            select_row(dlg, sel);
            true
        }
        ID_APPLY if sel >= 0 => {
            let text = text_of(dlg, ID_TEXT);
            let opts = flags_to_opts(st.flags.load(Ordering::Relaxed));
            {
                let mut set = st.markers.borrow_mut();
                set.rename(sel as usize, &text);
                if let Some(m) = set.entries.get_mut(sel as usize) {
                    m.opts = opts;
                }
            }
            rebuild_list(dlg);
            select_row(dlg, sel);
            true
        }
        ID_ENABLE => {
            st.markers.borrow_mut().enabled = is_checked(dlg, ID_ENABLE);
            apply_flags(dlg);
            true
        }
        ID_REGEX | ID_CASE | ID_WORD => {
            let bit = match id {
                ID_REGEX => 1,
                ID_CASE => 2,
                _ => 4,
            };
            // The control has already flipped itself by the time BN_CLICKED
            // arrives, so the model follows it instead of toggling its own bit.
            let mask = if is_checked(dlg, id) {
                st.flags.load(Ordering::Relaxed) | bit
            } else {
                st.flags.load(Ordering::Relaxed) & !bit
            };
            st.pending.set(flags_to_opts(mask));
            if sel >= 0 {
                let mut set = st.markers.borrow_mut();
                if let Some(m) = set.entries.get_mut(sel as usize) {
                    m.opts = st.pending.get();
                }
            }
            st.flags.store(mask, Ordering::Relaxed);
            true
        }
        _ => false,
    };
    if changed {
        let _ = PostMessageW(Some(st.frame), WM_MARKS, WPARAM(0), LPARAM(0));
    }
    changed as isize
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

/// What an `LVN_ITEMCHANGED` says about the row's check box: `Some(on)` when the
/// state image really moved, `None` for every other change (text, icon, the
/// notification the control sends while a row is being inserted).
///
/// `uChanged` carries **`LVIF_*`** flags — `LVIF_STATE` is `0x8` — not `LVIS_*`
/// bits. Masking it with `LVIS_STATEIMAGEMASK` (`0xF000`) is always zero, which
/// is how the check boxes used to be decorative: the handler never ran. Whether
/// the box is ticked lives in the old/new state image (1 = off, 2 = on).
fn check_from_notify(changed: u32, old_state: u32, new_state: u32) -> Option<bool> {
    if changed & LVIF_STATE.0 == 0 {
        return None;
    }
    let on = |s: u32| (s & LVIS_STATEIMAGEMASK.0) >> 12 == 2;
    let (was, now) = (on(old_state), on(new_state));
    (was != now).then_some(now)
}

/// The list's notifications. A check-box click and a selection change are the
/// two things the model has to follow, and the per-row colour is the third.
unsafe fn on_notify(dlg: HWND, lparam: LPARAM) -> isize {
    let hdr = &*(lparam.0 as *const NMHDR);
    if hdr.idFrom as i32 != ID_LIST {
        return 0;
    }
    let st = state(dlg);
    if hdr.code == LVN_ITEMCHANGED {
        let n = &*(lparam.0 as *const ItemChanged);
        if let Some(on) = check_from_notify(n.u_changed, n.u_old_state, n.u_new_state) {
            st.markers.borrow_mut().set_checked(n.i_item as usize, on);
            let _ = PostMessageW(Some(st.frame), WM_MARKS, WPARAM(0), LPARAM(0));
            return 1;
        }
        if n.u_changed & LVIS_SELECTED.0 != 0 && n.u_new_state & LVIS_SELECTED.0 != 0 {
            select_row(dlg, n.i_item);
            return 1;
        }
        return 0;
    }
    if hdr.code == NM_CUSTOMDRAW {
        return on_custom_draw(dlg, lparam);
    }
    0
}

/// Per-row colour, so the eight slots are told apart the way the editor tells
/// them apart: the slot's marker colour for the text, on the list's own
/// background. A selected row keeps the theme's selection pair instead.
unsafe fn on_custom_draw(dlg: HWND, lparam: LPARAM) -> isize {
    let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
    let st = state(dlg);
    // Rows are painted per column, so this asks for the subitem stage.
    customdraw::row(cd, true, |row, selected, _| {
        if selected {
            return (to_colorref(st.theme.foreground), to_colorref(st.theme.selection_bg));
        }
        let slot = st
            .markers
            .borrow()
            .entries
            .get(row)
            .map(|m| m.slot % Theme::MARKER_SLOTS)
            .unwrap_or(0);
        (to_colorref(st.theme.marker_fg[slot]), to_colorref(st.theme.background))
    })
}

/// A button, every pixel of it, from `Theme`: the `#2D2D2D` face,
/// `#4C4A48` edge and `#828790` text, dimmed while the control is disabled and
/// one ink step lighter under the cursor. The control still owns the hit-test,
/// the focus and the click; the pixels come from [`ownerdraw`].
unsafe fn on_draw_item(dlg: HWND, lparam: LPARAM) -> isize {
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    let id = di.CtlID as i32;
    let st = state(dlg);
    if id == ID_LINE {
        // The separator the measurement puts at (21,406) 394x2.
        let rc = di.rcItem;
        FillRect(di.hDC, &rc, st.edge);
        return 1;
    }
    let Some(label) = row_label(id) else {
        return 0;
    };
    let label = crate::win32::lang::dlg(251, id as u32).unwrap_or(label);
    let look = st.button_look();
    let rc = ownerdraw::frame(di, &look);
    // `DT_NOPREFIX` draws `&` literally, and these labels are mnemonics.
    let mut shown = label.to_string();
    if let Some(at) = shown.find('&') {
        shown.remove(at);
    }
    ownerdraw::label(di, rc, &look, &shown, ownerdraw::Align::Center);
    1
}

fn row_label(id: i32) -> Option<&'static str> {
    ROWS.iter()
        .find(|r| r.id == id)
        .map(|r| r.label)
        .filter(|l| !l.is_empty())
}

/// Whether a native checkbox is ticked. `BM_GETCHECK` is the only copy of that
/// state: the model mirrors it, it does not own it.
unsafe fn is_checked(dlg: HWND, id: i32) -> bool {
    let Some(ctl) = item(dlg, id) else {
        return false;
    };
    SendMessageW(ctl, BM_GETCHECK, None, None).0 as u32 & BST_CHECKED.0 == BST_CHECKED.0
}

/// `DLGPROC` is a plain `-> isize` fn pointer in this metadata, so the returns
/// are 0/1 rather than `LRESULT`.
unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            crate::win32::lang::localize_dialog(hwnd, 251);
            // Every `BS_OWNERDRAW` button gets the hover subclass; the list's
            // separator (kind 4), the native checks (kind 6) and the list
            // itself are not owner-drawn.
            let ids: Vec<i32> = ROWS
                .iter()
                .filter(|r| r.class == BUTTON_CLASS && r.kind != 6)
                .map(|r| r.id)
                .collect();
            ownerdraw::install(hwnd, &ids);
            // TRUE: the manager sets the initial focus itself.
            1
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_NOTIFY => on_notify(hwnd, lparam),
        WM_DRAWITEM => on_draw_item(hwnd, lparam),
        // A themed checkbox paints the background behind its caption by asking
        // the parent to draw it, and it asks with `WM_PRINTCLIENT` — not
        // `WM_ERASEBKGND`, and not `WM_CTLCOLORBTN` -- a themed button never
        // receives that message. Without this arm the `#32770`
        // class answers with `COLOR_3DFACE` `#F0F0F0` and every check wears a
        // light block on the dark face.
        WM_PRINTCLIENT => {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state(hwnd).bg);
            }
            1
        }
        WM_ERASEBKGND => {
            // The `#32770` class ships a `COLOR_3DFACE` brush; the space between
            // and behind the controls has to come from Theme.
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state(hwnd).bg);
            }
            1
        }
        // The native checks ask for their caption colour and the brush behind
        // it; without this they keep the system face under the text.
        // Only reached when visual styles are off — a themed button paints
        // itself and never asks. Kept so the checks are not `COLOR_WINDOW`
        // white in the classic theme.
        WM_CTLCOLORBTN => {
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(st.theme.panel_fg),
                to_colorref(st.theme.background),
                st.bg,
            )
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
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            // The manager is gone, but its state stays: the list view and the
            // buttons are destroyed first and the last few notifications they
            // send (`WM_NOTIFY`, `WM_COMMAND`) still read it. Dropping the
            // `Box` here left those to dereference a null `GWLP_USERDATA`,
            // which aborts the process — the state is freed at
            // `WM_NCDESTROY` instead.
            DLG.store(0, Ordering::Release);
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
                let _ = DeleteObject(HGDIOBJ(b.face_hot.0));
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

    /// The grid is a measurement. A duplicated id would make a click ambiguous,
    /// and a rect outside the client would clip a control.
    #[test]
    fn the_measured_grid_is_kept() {
        assert_eq!((CLIENT_W, CLIENT_H), (349, 372));
        let mut ids: Vec<i32> = ROWS.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "control ids must be unique");
        for r in ROWS {
            assert!(r.x + r.w <= CLIENT_W, "{} overflows the width", r.id);
            assert!(r.y + r.h <= CLIENT_H, "{} overflows the height", r.id);
        }
    }

    #[test]
    fn option_bits_match_their_ids() {
        let both = flags_to_opts(0b0011);
        assert!(both.regex && both.case_sensitive && !both.whole_word);
        assert!(flags_to_opts(0b0100).whole_word);
        assert!(!flags_to_opts(0).case_sensitive);
    }

    /// The four checks are native; everything else that is a button stays
    /// owner-drawn. The two paths are wired differently — flipping a kind
    /// without the `WM_PRINTCLIENT` arm puts a `#F0F0F0` block on the dark
    /// face, and without `dark_theme` the box itself goes light — so the split
    /// is asserted rather than left to a comment.
    #[test]
    fn the_option_checks_are_native() {
        for id in OPT_IDS {
            let row = ROWS.iter().find(|r| r.id == id).expect("every id has a row");
            assert_eq!(row.kind, 6, "{id} must be a native checkbox");
        }
        for row in ROWS.iter().filter(|r| r.class == BUTTON_CLASS) {
            if !OPT_IDS.contains(&row.id) {
                assert_ne!(row.kind, 6, "{} is not one of the four checks", row.id);
            }
        }
    }

    /// 勾选读的是 state image，`uChanged` 必须按 `LVIF_*` 判断 —— 这条锁的
    /// 正是「勾选框纯装饰」那个 bug：拿 `LVIS_STATEIMAGEMASK`（0xF000）去与
    /// `uChanged`（`LVIF_STATE` = 0x8）永远是 0，处理分支一次都不跑。
    #[test]
    fn a_check_box_click_is_read_from_the_state_image() {
        assert_eq!(
            LVIS_STATEIMAGEMASK.0 & LVIF_STATE.0,
            0,
            "LVIS_ 与 LVIF_ 是两个命名空间，不能互相掩"
        );
        // 用户点一下：选中 → 未选、未选 → 选中。
        assert_eq!(
            check_from_notify(LVIF_STATE.0, state_image(true).0, state_image(false).0),
            Some(false)
        );
        assert_eq!(
            check_from_notify(LVIF_STATE.0, state_image(false).0, state_image(true).0),
            Some(true)
        );
        // 插行时控件自己发的通知：old 没有 state image、new 是「未选」 —
        // 这不是用户改的，别把它当成一次取消勾选。
        assert_eq!(check_from_notify(LVIF_STATE.0, 0, state_image(false).0), None);
        // 文本/图标变化、以及勾选没动的情况都不进模型。
        assert_eq!(check_from_notify(LVIF_TEXT.0, 0, state_image(true).0), None);
        assert_eq!(
            check_from_notify(LVIF_STATE.0, state_image(true).0, state_image(true).0),
            None
        );
    }
}
