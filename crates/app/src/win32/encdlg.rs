//! 转换编码 — the encoding dialog (`cm_doc_convertencoding`).
//!
//! The frame asks the active editor for its code page, runs the dialog
//! modally, and sends the picked page back when the dialog returns 1. The
//! four UTF-16/32 pages need one extra refresh because their line caches are
//! byte-indexed; the document model drops those caches itself.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HFONT, HDC,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DefWindowProcW, DLGTEMPLATE, DestroyWindow, GetClientRect,
    GetDlgItem, GetWindow, GetWindowLongPtrW, GWL_STYLE, GWLP_USERDATA, GW_CHILD, GW_HWNDNEXT,
    LB_ADDSTRING, LB_DELETESTRING, LB_GETCOUNT, LB_GETCURSEL, LB_GETITEMDATA, LB_GETTEXT,
    LB_GETTEXTLEN, LB_INSERTSTRING, LB_SETCURSEL, LB_SETITEMDATA, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_SETFONT, WS_CLIPCHILDREN,
};

use dg_render::theme::Theme;

use super::customdraw;
use super::chrome;
use super::grepdlg::{CODE_PAGES, marlett_font};
use super::prompt::{run_modal_loop, show_modal};
use super::view::instance;
use crate::win32::window::to_colorref;

// ---------------------------------------------------------------------------
// The dialog template, byte for byte
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x80c8_00c0;
const DLG_EXSTYLE: u32 = 0x0000_0000;
const DLG_W: u16 = 381;
const DLG_H: u16 = 199;
/// The key `0` — the dialog's own caption — of `zh-CN.lng`'s 编码 box;
/// untranslated it reads `Encodings`.
const DLG_TITLE: &str = "编码";
/// The two buttons, keys `1` and `2` of the same box.
const OK_TEXT: &str = "确定";
const CANCEL_TEXT: &str = "取消(&C)";
/// The two list labels, keys `3000` and `3001` of the same box; untranslated
/// they read `Select Code Page:` and `System Code Pages:`.
const LEFT_LABEL: &str = "请选择编码：";
const RIGHT_LABEL: &str = "所有编码：";

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_LEFT: i32 = 1200;
const ID_RIGHT: i32 = 1201;
/// The four arrows: up / down above the left list, add / remove between the
/// lists. The template leaves all four captions empty; these Marlett glyphs
/// stand in for the framework's painting (`3`=◄, `4`=►, `5`=▲, `6`=▼,
/// measured on this machine's Marlett).
const ID_UP: i32 = 1204;
const ID_DOWN: i32 = 1205;
const ID_ADD: i32 = 1202;
const ID_REMOVE: i32 = 1203;
const ID_LEFT_LABEL: i32 = 3000;
const ID_RIGHT_LABEL: i32 = 3001;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_LISTBOX: u16 = 0x0083;

/// The ten children, in template order. Rects and styles are the template's
/// own; the captions are the zh-CN.lng texts.
const ITEMS: &[Item] = &[
    Item { id: IDOK, class: Class::Atom(ATOM_BUTTON), style: 0x50010001, x: 7, y: 178, w: 50, h: 14, label: OK_TEXT },
    Item { id: IDCANCEL, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 61, y: 178, w: 50, h: 14, label: CANCEL_TEXT },
    Item { id: ID_LEFT_LABEL, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 7, y: 7, w: 164, h: 11, label: LEFT_LABEL },
    Item { id: ID_RIGHT_LABEL, class: Class::Atom(ATOM_STATIC), style: 0x50020000, x: 201, y: 7, w: 164, h: 11, label: RIGHT_LABEL },
    Item { id: ID_LEFT, class: Class::Atom(ATOM_LISTBOX), style: 0x50a10101, x: 7, y: 19, w: 166, h: 155, label: "" },
    Item { id: ID_RIGHT, class: Class::Atom(ATOM_LISTBOX), style: 0x50b10101, x: 202, y: 19, w: 170, h: 155, label: "" },
    Item { id: ID_UP, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 132, y: 3, w: 20, h: 14, label: "5" },
    Item { id: ID_DOWN, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 154, y: 3, w: 20, h: 14, label: "6" },
    Item { id: ID_ADD, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 177, y: 78, w: 20, h: 14, label: "3" },
    Item { id: ID_REMOVE, class: Class::Atom(ATOM_BUTTON), style: 0x50010000, x: 177, y: 93, w: 20, h: 14, label: "4" },
];

/// Build the DLGTEMPLATEEX buffer, the same shape the other reimplemented dialogs
/// write: the EX header, the `DS_SETFONT` font block, then each item as help
/// id + ex style + style + rect + DWORD id + `0xFFFF` class atom +
/// NUL-terminated text + extra count.
fn build_template() -> Vec<u8> {
    dlgtpl::build_legacy(DLG_TITLE, DLG_STYLE, DLG_EXSTYLE, DLG_W, DLG_H, 0, ITEMS)
}

// ---------------------------------------------------------------------------
// Codepage ↔ encoding
// ---------------------------------------------------------------------------

/// The display name of one codepage — the lookup OnInitDialog does before it
/// adds a favourite row (a codepage the table does not know is skipped).
pub(super) fn cp_name(cp: u16) -> Option<&'static str> {
    CODE_PAGES.iter().find(|(c, _)| *c == cp).map(|(_, n)| *n)
}

/// The document's codepage, as the frame's own encoding query reports it.
pub(super) fn encoding_to_cp(enc: &'static encoding_rs::Encoding) -> Option<u16> {
    Some(match enc.name() {
        "UTF-8" => 65001,
        "UTF-16LE" => 1200,
        "UTF-16BE" => 1201,
        "GBK" => 936,
        "gb18030" => 54936,
        "Big5" => 950,
        "EUC-JP" => 20932,
        "ISO-2022-JP" => 50220,
        "Shift_JIS" => 932,
        "EUC-KR" => 949,
        "IBM866" => 866,
        "ISO-8859-1" => 28591,
        "ISO-8859-2" => 28592,
        "ISO-8859-3" => 28603,
        "ISO-8859-4" => 28594,
        "ISO-8859-5" => 28595,
        "ISO-8859-6" => 28596,
        "ISO-8859-7" => 28597,
        "ISO-8859-8" => 28598,
        "ISO-8859-8-I" => 38598,
        "ISO-8859-9" => 28599,
        "ISO-8859-15" => 28605,
        "KOI8-R" => 20866,
        "KOI8-U" => 21866,
        "macintosh" => 10000,
        "x-mac-cyrillic" => 10007,
        "windows-874" => 874,
        // windows-1250..1258 map onto themselves.
        name if name.starts_with("windows-125") => name["windows-".len()..].parse().ok()?,
        _ => return None,
    })
}

/// The encoding the system ANSI page names, for the `Default (ANSI)` row.
/// This implementation resolves it here.
fn acp_encoding() -> &'static encoding_rs::Encoding {
    let acp = unsafe { windows::Win32::Globalization::GetACP() };
    match acp {
        936 => encoding_rs::GBK,
        950 => encoding_rs::BIG5,
        932 => encoding_rs::SHIFT_JIS,
        949 => encoding_rs::EUC_KR,
        866 => encoding_rs::IBM866,
        874 => encoding_rs::WINDOWS_874,
        20866 => encoding_rs::KOI8_R,
        21866 => encoding_rs::KOI8_U,
        65001 => encoding_rs::UTF_8,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        // Every other ACP falls back to the Western page.
        _ => encoding_rs::WINDOWS_1252,
    }
}

/// The encoding one picked codepage converts to, plus whether the document
/// carries a BOM afterwards: the two UTF-16 pages do (the status strip's
/// encoding menu applies them BOM'd the same way), everything else does not.
/// `None` marks the codepages `encoding_rs` cannot reproduce — the documented
/// seam this dialog stops at.
pub(super) fn cp_to_encoding(cp: u16) -> Option<(&'static encoding_rs::Encoding, bool)> {
    Some(match cp {
        0 => (acp_encoding(), false),
        65001 => (encoding_rs::UTF_8, false),
        1200 => (encoding_rs::UTF_16LE, true),
        1201 => (encoding_rs::UTF_16BE, true),
        936 => (encoding_rs::GBK, false),
        54936 => (encoding_rs::GB18030, false),
        950 => (encoding_rs::BIG5, false),
        932 => (encoding_rs::SHIFT_JIS, false),
        949 | 51949 => (encoding_rs::EUC_KR, false),
        20932 => (encoding_rs::EUC_JP, false),
        50220 => (encoding_rs::ISO_2022_JP, false),
        866 => (encoding_rs::IBM866, false),
        874 => (encoding_rs::WINDOWS_874, false),
        // The WHATWG table folds three of the list's pages into their Windows
        // superset: iso-8859-1 and us-ascii decode as windows-1252, iso-8859-9
        // as windows-1254 — encoding_rs ships no separate statics for them.
        28591 | 20127 => (encoding_rs::WINDOWS_1252, false),
        28592 => (encoding_rs::ISO_8859_2, false),
        28603 => (encoding_rs::ISO_8859_3, false),
        28594 => (encoding_rs::ISO_8859_4, false),
        28595 => (encoding_rs::ISO_8859_5, false),
        28596 => (encoding_rs::ISO_8859_6, false),
        28597 => (encoding_rs::ISO_8859_7, false),
        28598 => (encoding_rs::ISO_8859_8, false),
        38598 => (encoding_rs::ISO_8859_8_I, false),
        28599 | 1254 => (encoding_rs::WINDOWS_1254, false),
        28605 => (encoding_rs::ISO_8859_15, false),
        20866 => (encoding_rs::KOI8_R, false),
        21866 => (encoding_rs::KOI8_U, false),
        10000 => (encoding_rs::MACINTOSH, false),
        10007 => (encoding_rs::X_MAC_CYRILLIC, false),
        1250 => (encoding_rs::WINDOWS_1250, false),
        1251 => (encoding_rs::WINDOWS_1251, false),
        1252 => (encoding_rs::WINDOWS_1252, false),
        1253 => (encoding_rs::WINDOWS_1253, false),
        1255 => (encoding_rs::WINDOWS_1255, false),
        1256 => (encoding_rs::WINDOWS_1256, false),
        1257 => (encoding_rs::WINDOWS_1257, false),
        1258 => (encoding_rs::WINDOWS_1258, false),
        // UTF-32 (12000/12001), UTF-7 (65000), the EBCDIC, ISCII, DOS, HZ and
        // ISO-2022-CN/KR pages: encoding_rs has none of them.
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The dialog object
// ---------------------------------------------------------------------------

/// What OK hands back to the frame: the picked codepage, the favourites as
/// they now stand (the frame persists them when [`EncChoice::modified`]), and
/// whether anything moved.
pub(super) struct EncChoice {
    pub cp: u16,
    pub favorites: Vec<u16>,
    pub modified: bool,
}

/// The dialog's model plus the brushes the chrome colours need, held on the
/// caller's stack and reached from `dlg_proc` through `GWLP_USERDATA` — the
/// modal loop in [`dialog`] keeps that stack alive for as long as the window
/// exists.
struct Dlg {
    /// The document's codepage the frame queried.
    current: Option<u16>,
    /// Whether the favourites came from the defaults rather than
    /// `[Encoding]`: the defaults get their ` [cp]` suffix unconditionally,
    /// settings-loaded rows only when `cp > 10`.
    from_defaults: bool,
    favourites: Vec<u16>,
    result: Option<EncChoice>,
    modified: bool,
    destroyed: bool,
    /// The chrome palette, the same block the other reimplemented dialogs paint
    /// from: the face brush behind everything, the edit brush behind the two
    /// lists, and the two faces as `COLORREF`s for `SetBkColor`.
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    marlett: HFONT,
    /// The app's UI font — the one the menus, the status strip and the other
    /// dialogs draw with — handed to every child in [`init`] in place of the
    /// template's 8 pt `Ms Shell Dlg`.
    font: HFONT,
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg)
}

/// Runs 转换编码 modally over `frame`. `current` is the document's codepage,
/// `favorites` the loaded left list (an empty list stays empty — the defaults
/// arrive pre-expanded), `from_defaults` whether that list is the hard-coded
/// nine. `None` for cancel, no selection, or a codepage this implementation cannot
/// re-encode to.
pub(super) fn dialog(
    frame: HWND,
    theme: &Theme,
    current: Option<u16>,
    favorites: Vec<u16>,
    from_defaults: bool,
) -> Option<EncChoice> {
    let dpi = unsafe { GetDpiForWindow(frame).max(96) as i32 };
    let mut st = Dlg {
        current,
        from_defaults,
        favourites: favorites,
        result: None,
        modified: false,
        destroyed: false,
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        marlett: marlett_font(dpi),
        font: unsafe { super::uifont::create(dpi) },
    };
    let param = LPARAM(&mut st as *mut Dlg as isize);
    let tmpl = build_template();
    let dlg = unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dlg_proc),
            param,
        )
    };
    let Ok(dlg) = dlg else {
        unsafe {
            let _ = DeleteObject(st.bg.into());
            let _ = DeleteObject(st.field.into());
            let _ = DeleteObject(st.marlett.into());
            let _ = DeleteObject(st.font.into());
        }
        return None;
    };
    unsafe {
        // The dark palette, searchdlg's measured form: the controls and the
        // caption get the dark attribute, the dialog window itself is never
        // named to `SetWindowTheme` — that recalculates a `#32770`'s frame
        // with doubled metrics and pushes the bottom button row (确定/取消 at
        // y 178..192 of a 199-high dialog) off the client.
        let dark = chrome::theme_is_dark(theme);
        chrome::set_chrome_theme(theme);
        chrome::dark_theme_children(dlg, dark);
        chrome::title_bar(dlg, dark);
        // The template's style has no WS_CLIPCHILDREN; with the chrome block
        // as the face the parent's whole-client erase covers the children and
        // only the controls that change would repaint.
        let style = GetWindowLongPtrW(dlg, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(dlg, GWL_STYLE, style);
        show_modal(dlg, frame);
        run_modal_loop(dlg, frame, std::ptr::addr_of!(st.destroyed));
        let _ = DeleteObject(st.bg.into());
        let _ = DeleteObject(st.field.into());
        let _ = DeleteObject(st.marlett.into());
        let _ = DeleteObject(st.font.into());
    }
    st.result
}

/// WM_INITDIALOG — OnInitDialog's fill, in its own order: the right list
/// (98 codepages), the left list (the favourites), then the selection.
unsafe fn init(dlg: HWND) -> isize {
    // The dialog manager already gave every child the template's font — 8 pt
    // `Ms Shell Dlg`, which is 13 px at 120 DPI and reads small next to the
    // rest of the chrome (the menus and the status strip draw with the UI font,
    // 15 px at the same DPI). Replace it wholesale before anything else, the
    // way the UI-font helper hands the UI font to a dialog's children.  The
    // template's `DS_SETFONT` block is deliberately left alone: that font is
    // what the dialog manager converts into dialog units, so changing it would
    // rescale the whole 381×199 layout away from the template. Only the text
    // inside the controls changes here.
    let font = state_mut(dlg).font;
    let mut child = GetWindow(dlg, GW_CHILD).unwrap_or_default();
    while !child.0.is_null() {
        let _ = SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
    }
    // With the dialog font they would read as digits. This has to come after the
    // loop above — it is the one family the UI font must not win.
    let marlett = state_mut(dlg).marlett;
    for id in [ID_UP, ID_DOWN, ID_ADD, ID_REMOVE] {
        if let Ok(c) = GetDlgItem(Some(dlg), id) {
            let _ = SendMessageW(c, WM_SETFONT, Some(WPARAM(marlett.0 as usize)), Some(LPARAM(1)));
        }
    }
    let right = GetDlgItem(Some(dlg), ID_RIGHT).unwrap_or_default();
    for (cp, name) in CODE_PAGES {
        // The bracketed code page only appears past 10; the first ten read as a
        // bare name.
        let label = if *cp > 10 {
            format!("{name} [{cp}]")
        } else {
            name.to_string()
        };
        add_codepage(right, &label, *cp);
    }
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let st = state_mut(dlg);
    for cp in &st.favourites {
        // A codepage without a name is skipped.
        let Some(name) = cp_name(*cp) else { continue };
        let label = if st.from_defaults || *cp > 10 {
            format!("{name} [{cp}]")
        } else {
            name.to_string()
        };
        add_codepage(left, &label, *cp);
    }
    // The selection: the row whose item data is the document's codepage, else
    // nothing; no queried codepage at all selects row 0.
    match st.current {
        None => {
            let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
        }
        Some(cur) => {
            let count = SendMessageW(left, LB_GETCOUNT, None, None).0;
            for i in 0..count {
                let data = SendMessageW(left, LB_GETITEMDATA, Some(WPARAM(i as usize)), None).0;
                if data as u16 == cur {
                    let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM(i as usize)), None);
                    break;
                }
            }
        }
    }
    1
}

/// One listbox row: the caption, then the codepage in the item data —
/// `LB_ADDSTRING` + `LB_SETITEMDATA`, what both fill loops do.
unsafe fn add_codepage(list: HWND, label: &str, cp: u16) {
    let mut w: Vec<u16> = label.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        list,
        LB_ADDSTRING,
        Some(WPARAM(0)),
        Some(LPARAM(w.as_mut_ptr() as isize)),
    )
    .0;
    if idx >= 0 {
        let _ = SendMessageW(
            list,
            LB_SETITEMDATA,
            Some(WPARAM(idx as usize)),
            Some(LPARAM(cp as isize)),
        );
    }
}

/// `LB_GETTEXT` of one row, the step the move and add handlers take through
/// the list
unsafe fn list_text(list: HWND, index: i32) -> String {
    let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(index as usize)), None).0;
    if len < 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    SendMessageW(
        list,
        LB_GETTEXT,
        Some(WPARAM(index as usize)),
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    );
    buf.truncate(len as usize);
    String::from_utf16_lossy(&buf)
}

/// The left list's rows and their item data, in order — what OnOkCancel
/// persists when the list changed.
unsafe fn left_favorites(dlg: HWND) -> Vec<u16> {
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let count = SendMessageW(left, LB_GETCOUNT, None, None).0;
    (0..count)
        .map(|i| SendMessageW(left, LB_GETITEMDATA, Some(WPARAM(i as usize)), None).0 as u16)
        .collect()
}

/// OnOkCancel's OK arm: no selection ends 2 (cancel), otherwise the left
/// selection's item data is the answer.
unsafe fn on_ok(dlg: HWND) {
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let sel = SendMessageW(left, LB_GETCURSEL, None, None).0;
    if sel < 0 {
        end(dlg);
        return;
    }
    let cp = SendMessageW(left, LB_GETITEMDATA, Some(WPARAM(sel as usize)), None).0 as u16;
    let st = state_mut(dlg);
    st.result = Some(EncChoice {
        cp,
        favorites: left_favorites(dlg),
        modified: st.modified,
    });
    end(dlg);
}

/// Both cancel paths: 取消, and OK with nothing selected — `EndDialog(2)`
/// either way, which the frame reads as cancel.
unsafe fn end(dlg: HWND) {
    let _ = DestroyWindow(dlg);
}

/// OnAddRemove's 1202 arm: the right selection joins the favourites.
unsafe fn on_add(dlg: HWND) {
    let right = GetDlgItem(Some(dlg), ID_RIGHT).unwrap_or_default();
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let rsel = SendMessageW(right, LB_GETCURSEL, None, None).0;
    if rsel < 0 {
        return;
    }
    let cp = SendMessageW(right, LB_GETITEMDATA, Some(WPARAM(rsel as usize)), None).0 as u16;
    // The duplicate check selects the existing row and stops there — the
    // modified flag is not touched for a re-selection.
    let count = SendMessageW(left, LB_GETCOUNT, None, None).0;
    for i in 0..count {
        let data = SendMessageW(left, LB_GETITEMDATA, Some(WPARAM(i as usize)), None).0;
        if data as u16 == cp {
            let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM(i as usize)), None);
            return;
        }
    }
    let text = list_text(right, rsel as i32);
    // The insert goes one under the left selection — a cursel of −1 lands on
    // row 0, because the insert index is the selection advanced by one.
    let lsel = SendMessageW(left, LB_GETCURSEL, None, None).0;
    let at = (lsel + 1) as usize;
    let mut w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        left,
        LB_INSERTSTRING,
        Some(WPARAM(at)),
        Some(LPARAM(w.as_mut_ptr() as isize)),
    )
    .0;
    if idx < 0 {
        return;
    }
    let _ = SendMessageW(
        left,
        LB_SETITEMDATA,
        Some(WPARAM(idx as usize)),
        Some(LPARAM(cp as isize)),
    );
    let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM(idx as usize)), None);
    state_mut(dlg).modified = true;
}

/// The 添加/移除 button's handler: the left selection leaves the favourites and
/// cur−1 is selected — a −1 after removing row 0 deselects, which is what the
/// selection call receives.
unsafe fn on_remove(dlg: HWND) {
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let sel = SendMessageW(left, LB_GETCURSEL, None, None).0;
    if sel < 0 {
        return;
    }
    let _ = SendMessageW(left, LB_DELETESTRING, Some(WPARAM(sel as usize)), None);
    let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM((sel - 1) as usize)), None);
    state_mut(dlg).modified = true;
}

/// OnUpDown's two arms share one helper: take the text and the item
/// data, delete the row, re-insert it one slot over, restore the data and the
/// selection. Up needs a row above, down a row below; a refused move leaves
/// the modified flag alone.
unsafe fn on_move(dlg: HWND, up: bool) {
    let left = GetDlgItem(Some(dlg), ID_LEFT).unwrap_or_default();
    let cur = SendMessageW(left, LB_GETCURSEL, None, None).0;
    if cur < 0 {
        return;
    }
    let count = SendMessageW(left, LB_GETCOUNT, None, None).0;
    if count <= 1 {
        return;
    }
    let to: isize = if up {
        if cur == 0 {
            return;
        }
        cur - 1
    } else {
        if cur + 1 >= count {
            return;
        }
        cur + 1
    };
    let text = list_text(left, cur as i32);
    let data = SendMessageW(left, LB_GETITEMDATA, Some(WPARAM(cur as usize)), None).0;
    let _ = SendMessageW(left, LB_DELETESTRING, Some(WPARAM(cur as usize)), None);
    let mut w: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let idx = SendMessageW(
        left,
        LB_INSERTSTRING,
        Some(WPARAM(to as usize)),
        Some(LPARAM(w.as_mut_ptr() as isize)),
    )
    .0;
    if idx < 0 {
        return;
    }
    let _ = SendMessageW(left, LB_SETITEMDATA, Some(WPARAM(idx as usize)), Some(LPARAM(data)));
    let _ = SendMessageW(left, LB_SETCURSEL, Some(WPARAM(idx as usize)), None);
    state_mut(dlg).modified = true;
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            crate::win32::lang::localize_dialog(hwnd, 202);
            init(hwnd)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let notif = (wparam.0 >> 16) as i32;
            match id {
                // 确定, and the left list's double-click — the left list's
                // double-click posts the dialog its own `WM_COMMAND` for
                // 确定.
                IDOK => {
                    on_ok(hwnd);
                    1
                }
                IDCANCEL => {
                    end(hwnd);
                    1
                }
                ID_ADD => {
                    on_add(hwnd);
                    1
                }
                ID_REMOVE => {
                    on_remove(hwnd);
                    1
                }
                ID_UP => {
                    on_move(hwnd, true);
                    1
                }
                ID_DOWN => {
                    on_move(hwnd, false);
                    1
                }
                // LBN_DBLCLK: the double-click is OK.
                ID_LEFT if notif == 2 => {
                    on_ok(hwnd);
                    1
                }
                _ => 0,
            }
        }
        WM_ERASEBKGND => {
            let st = state_mut(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // The labels and the buttons sit on the dialog face.
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            // The two lists take the chrome field face
            // every edit and combo the same two values.
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_CLOSE => {
            // The caption's X is a cancel: the result stays `None`.
            end(hwnd);
            1
        }
        WM_DESTROY => {
            state_mut(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam).0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template has to be the recorded one: ten children in template
    /// order, the two lists with their own styles, the four arrows plain
    /// buttons, and the 381×199 `DS_MODALFRAME | DS_SETFONT` header.
    #[test]
    fn template_matches_the_resource() {
        assert_eq!(ITEMS.len(), 10);
        assert_eq!(ITEMS[0].id, IDOK);
        assert_eq!(ITEMS[0].style, 0x50010001);
        assert_eq!(ITEMS[1].id, IDCANCEL);
        assert_eq!(ITEMS[2].id, ID_LEFT_LABEL);
        assert_eq!(ITEMS[3].id, ID_RIGHT_LABEL);
        assert_eq!((ITEMS[2].x, ITEMS[2].y), (7, 7));
        assert_eq!((ITEMS[3].x, ITEMS[3].y), (201, 7));
        assert_eq!(ITEMS[4].id, ID_LEFT);
        assert_eq!(ITEMS[4].style, 0x50a10101);
        assert_eq!((ITEMS[4].x, ITEMS[4].y, ITEMS[4].w, ITEMS[4].h), (7, 19, 166, 155));
        assert_eq!(ITEMS[5].id, ID_RIGHT);
        assert_eq!(ITEMS[5].style, 0x50b10101);
        assert_eq!((ITEMS[5].x, ITEMS[5].y, ITEMS[5].w, ITEMS[5].h), (202, 19, 170, 155));
        assert_eq!(ITEMS[6].id, ID_UP);
        assert_eq!((ITEMS[6].x, ITEMS[6].y), (132, 3));
        assert_eq!(ITEMS[7].id, ID_DOWN);
        assert_eq!((ITEMS[7].x, ITEMS[7].y), (154, 3));
        assert_eq!(ITEMS[8].id, ID_ADD);
        assert_eq!((ITEMS[8].x, ITEMS[8].y), (177, 78));
        assert_eq!(ITEMS[9].id, ID_REMOVE);
        assert_eq!((ITEMS[9].x, ITEMS[9].y), (177, 93));
        for it in &ITEMS[6..] {
            assert_eq!(it.style, 0x50010000);
            assert_eq!((it.w, it.h), (20, 14));
        }
        let b = build_template();
        // dlgVer + signature, then the header's styles.
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(&b[12..16], &DLG_STYLE.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 10);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), DLG_W);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), DLG_H);
        // The item count and the first item survive a round trip through the
        // buffer the dialog manager reads: header (26) + menu/class (4) +
        // the title (6) + font block (6) + face (26) = the first item at 68,
        // its style at 76 and its id (OK) at 88.
        assert_eq!(&b[76..80], &0x50010001u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(b[88..92].try_into().unwrap()), 1);
        // The `DS_SETFONT` block stays the template's 8 pt / charset 1. That
        // font is what the dialog manager turns into dialog units, so raising
        // it here would rescale the 381×199 layout instead of enlarging the
        // text — the children get the UI font in `init` for that.
        assert_eq!(u16::from_le_bytes([b[36], b[37]]), 8, "point size");
        assert_eq!(u16::from_le_bytes([b[38], b[39]]), 0, "weight 0 = the face's own");
        assert_eq!(b[40], 0, "no italic");
        assert_eq!(b[41], 1, "DEFAULT_CHARSET");
    }

    /// The defaults, as this implementation's own init writes them.
    #[test]
    fn the_nine_default_favourites() {
        // The stack table the default path builds; the dialog receives it
        // pre-expanded by `encoding_favorites`.
        assert_eq!(cp_name(0), Some("Default (ANSI)"));
        assert_eq!(cp_name(65001), Some("UTF-8"));
        assert_eq!(cp_name(1200), Some("Unicode (UTF-16)"));
        assert_eq!(cp_name(1201), Some("Unicode Big Endian (UTF-16)"));
        assert_eq!(cp_name(936), Some("Chinese Simplified (GB2312)"));
        assert_eq!(cp_name(950), Some("Chinese Traditional (Big5)"));
        assert_eq!(cp_name(932), Some("Japanese (Shift-JIS)"));
        assert_eq!(cp_name(949), Some("Korean"));
        assert_eq!(cp_name(1252), Some("Western European (Windows)"));
        // A favourite the table does not know is skipped by the fill.
        assert_eq!(cp_name(60000), None);
    }

    /// The codepage ↔ encoding seams: the four special pages, the WHATWG
    /// folds, and the pages encoding_rs cannot reproduce.
    #[test]
    fn codepage_encoding_map() {
        assert_eq!(encoding_to_cp(encoding_rs::UTF_8), Some(65001));
        assert_eq!(encoding_to_cp(encoding_rs::UTF_16LE), Some(1200));
        assert_eq!(encoding_to_cp(encoding_rs::GBK), Some(936));
        assert_eq!(encoding_to_cp(encoding_rs::WINDOWS_1251), Some(1251));
        // BOM handling: the two UTF-16 pages carry one, nothing else does.
        assert_eq!(cp_to_encoding(1200), Some((encoding_rs::UTF_16LE, true)));
        assert_eq!(cp_to_encoding(1201), Some((encoding_rs::UTF_16BE, true)));
        assert_eq!(cp_to_encoding(65001), Some((encoding_rs::UTF_8, false)));
        assert_eq!(cp_to_encoding(936), Some((encoding_rs::GBK, false)));
        assert_eq!(cp_to_encoding(54936), Some((encoding_rs::GB18030, false)));
        assert_eq!(cp_to_encoding(51949), Some((encoding_rs::EUC_KR, false)));
        // The WHATWG folds: 28591/20127 → windows-1252, 28599/1254 → 1254's
        // static.
        assert_eq!(cp_to_encoding(28591), Some((encoding_rs::WINDOWS_1252, false)));
        assert_eq!(cp_to_encoding(20127), Some((encoding_rs::WINDOWS_1252, false)));
        assert_eq!(cp_to_encoding(28599), Some((encoding_rs::WINDOWS_1254, false)));
        assert_eq!(cp_to_encoding(1254), Some((encoding_rs::WINDOWS_1254, false)));
        // The seam: UTF-32, UTF-7, EBCDIC, ISCII, HZ, the DOS pages.
        assert_eq!(cp_to_encoding(12000), None);
        assert_eq!(cp_to_encoding(12001), None);
        assert_eq!(cp_to_encoding(65000), None);
        assert_eq!(cp_to_encoding(500), None);
        assert_eq!(cp_to_encoding(57002), None);
        assert_eq!(cp_to_encoding(52936), None);
        assert_eq!(cp_to_encoding(437), None);
    }
}
