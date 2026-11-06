//! 文件已被更改 — the question a document with unsaved changes asks on its way
//! out.
//!
//! One close asks with the system's own three-button question box (是 / 否 /
//! 取消). A close that carries several documents — 关闭全部/关闭其它/关闭左侧/
//! 关闭右侧, and quitting — asks with this module's five-button box instead,
//! whose two extra answers stop the asking: 全是 saves every remaining document,
//! 全否 discards them. That is the one difference the wider answer set exists
//! for, so the two boxes are never interchangeable.
//!
//! The five-button box is the framework dialog this implementation rebuilds
//! from code, like the 编码 and 跳转 boxes: a 284x74 dialog-unit template with a
//! 32 px question icon, a borderless read-only message field and the five
//! buttons in one row. The template's `DS_SETFONT` font stays 8 pt `Ms Shell
//! Dlg` because it is what the dialog manager converts into dialog units; the
//! children are handed the UI font after creation, and the field then grows the
//! box when the message wraps past one line's worth of height.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlgtpl::{self, Class, Item};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, GetDC, GetTextMetricsW, ReleaseDC, ScreenToClient,
    SelectObject, HBRUSH, HDC, HFONT, HGDIOBJ, TEXTMETRICW,
};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::UI::Controls::EM_GETLINECOUNT;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DefWindowProcW, DestroyWindow, GetClientRect, GetDlgItem,
    GetWindow, GetWindowLongPtrW, GetWindowRect, LoadImageW, MoveWindow,
    PostQuitMessage, SendMessageW, SetDlgItemTextW, SetWindowLongPtrW,
    DLGTEMPLATE, GWL_STYLE, GWLP_USERDATA, GW_CHILD, GW_HWNDNEXT, IDNO, IDYES, IMAGE_ICON,
    LR_SHARED, MB_ICONQUESTION, MB_YESNOCANCEL, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_INITDIALOG, WM_SETFONT,
    WS_CLIPCHILDREN,
};

use dg_render::theme::Theme;

use super::chrome;
use super::customdraw;
use super::prompt::{run_modal_loop, show_modal};
use super::uifont;
use super::view::instance;
use crate::win32::window::to_colorref;

/// Which answer the user gave. The close loop turns the two all-answers into a
/// mode that skips the box for the rest of its documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Answer {
    Yes,
    YesAll,
    No,
    NoAll,
    Cancel,
}

/// The single-document question: the system's own box, in the app's dark
/// chrome (face, ink and buttons from the `Theme`, layout from the system).
/// Caption and flags are the same box's: question icon, three buttons.
pub(super) fn ask_one(owner: HWND, theme: &Theme, text: &str) -> Answer {
    let answer = chrome::message_box(
        Some(owner),
        text,
        "DogEdit",
        MB_YESNOCANCEL | MB_ICONQUESTION,
        theme,
    );
    match answer {
        x if x == IDYES.0 => Answer::Yes,
        x if x == IDNO.0 => Answer::No,
        _ => Answer::Cancel,
    }
}

// ---------------------------------------------------------------------------
// The multi-document box, as a template
// ---------------------------------------------------------------------------

const DLG_STYLE: u32 = 0x80c8_00c0;
const DLG_EXSTYLE: u32 = 0x0000_0000;
const DLG_W: u16 = 284;
const DLG_H: u16 = 74;
/// The caption carries the application's own name, the way the other
/// reimplemented dialogs do.
const DLG_TITLE: &str = "DogEdit";

const ID_CANCEL: i32 = 2;
const ID_YES: i32 = 6;
const ID_NO: i32 = 7;
const ID_YES_ALL: i32 = 1200;
const ID_NO_ALL: i32 = 1201;
/// The question icon (a static) and the message field (a borderless read-only
/// multi-line edit, so the text can still be selected and copied).
const ID_ICON: i32 = 2001;
const ID_MESSAGE: i32 = 2000;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;

/// `STM_SETICON` — the static is asked to show an icon handle instead of text.
const STM_SETICON: u32 = 0x0170;

/// The shell's question mark: `SHELL32!24`, loaded at its own 32 px rather than
/// scaled with the display: the icon stays 32 px even when the dialog units
/// grow with the DPI.
const SHELL_QUESTION_ICON: usize = 24;

/// The seven children, in template order — which is also the tab order, so the
/// first control to take focus is 取消, bottom-left answers last. Labels are the
/// template's English; the language table overwrites them at init.
const ITEMS: &[Item] = &[
    Item { id: ID_ICON, class: Class::Atom(ATOM_STATIC), style: 0x5000_0003, x: 7, y: 7, w: 21, h: 20, label: "" },
    Item { id: ID_CANCEL, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 227, y: 53, w: 50, h: 14, label: "Cancel" },
    Item { id: ID_YES, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 7, y: 53, w: 50, h: 14, label: "Yes" },
    Item { id: ID_YES_ALL, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 62, y: 53, w: 50, h: 14, label: "Yes to All" },
    Item { id: ID_NO, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 117, y: 53, w: 50, h: 14, label: "No" },
    Item { id: ID_NO_ALL, class: Class::Atom(ATOM_BUTTON), style: 0x5001_0000, x: 172, y: 53, w: 50, h: 14, label: "No to All" },
    Item { id: ID_MESSAGE, class: Class::Atom(ATOM_EDIT), style: 0x5001_0804, x: 37, y: 7, w: 240, h: 45, label: "" },
];

fn build_template() -> Vec<u8> {
    dlgtpl::build_legacy(DLG_TITLE, DLG_STYLE, DLG_EXSTYLE, DLG_W, DLG_H, 0, ITEMS)
}

/// The shared question mark. `LoadLibraryW` keeps one reference on `SHELL32`
/// for the life of the process: `LR_SHARED` icons are torn down with their
/// module, and the box can ask again at any time.
fn question_icon() -> isize {
    static ICON: OnceLock<isize> = OnceLock::new();
    *ICON.get_or_init(|| unsafe {
        let name: Vec<u16> = "SHELL32.dll".encode_utf16().chain([0]).collect();
        let Ok(lib) = LoadLibraryW(PCWSTR(name.as_ptr())) else {
            return 0;
        };
        let icon = LoadImageW(
            Some(HINSTANCE(lib.0)),
            PCWSTR(SHELL_QUESTION_ICON as *const u16),
            IMAGE_ICON,
            32,
            32,
            LR_SHARED,
        );
        icon.map(|h| h.0 as isize).unwrap_or(0)
    })
}

// ---------------------------------------------------------------------------
// The box itself
// ---------------------------------------------------------------------------

/// What the dialog procedure needs while the box is up. Lives on the caller's
/// stack and reaches the procedure through `GWLP_USERDATA`.
struct Dlg {
    text: String,
    answer: Answer,
    destroyed: bool,
    bg: HBRUSH,
    face: COLORREF,
    ink: COLORREF,
    font: HFONT,
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Dlg {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg)
}

/// Runs the five-button box modally over `frame` and returns the chosen
/// answer. The message's line breaks are normalized to the edit's CRLF form.
pub(super) fn ask_all(frame: HWND, theme: &Theme, text: &str) -> Answer {
    let dpi = unsafe { GetDpiForWindow(frame).max(96) as i32 };
    let mut st = Dlg {
        text: text.replace('\r', "").replace('\n', "\r\n"),
        answer: Answer::Cancel,
        destroyed: false,
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        face: to_colorref(theme.toolbar_bg),
        ink: to_colorref(theme.toolbar_fg),
        font: unsafe { uifont::create(dpi) },
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
            if !st.bg.0.is_null() {
                let _ = DeleteObject(st.bg.into());
            }
            if !st.font.0.is_null() {
                let _ = DeleteObject(st.font.into());
            }
        }
        return Answer::Cancel;
    };
    unsafe {
        // The dark palette: children and caption get the dark attribute, the
        // `#32770` itself is never named to `SetWindowTheme` — that recalculates
        // its frame and pushes the bottom control row off the client.
        let dark = chrome::theme_is_dark(theme);
        chrome::set_chrome_theme(theme);
        chrome::dark_theme_children(dlg, dark);
        chrome::title_bar(dlg, dark);
        let style = GetWindowLongPtrW(dlg, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(dlg, GWL_STYLE, style);
        show_modal(dlg, frame);
        run_modal_loop(dlg, frame, std::ptr::addr_of!(st.destroyed));
        if !st.bg.0.is_null() {
            let _ = DeleteObject(st.bg.into());
        }
        if !st.font.0.is_null() {
            let _ = DeleteObject(st.font.into());
        }
    }
    st.answer
}

/// WM_INITDIALOG: localized button labels, the UI font on every child, the
/// message, the icon, then the height the wrapped text asks for.
unsafe fn init(dlg: HWND) -> isize {
    crate::win32::lang::localize_dialog(dlg, 291);
    let font = state_mut(dlg).font;
    let mut child = GetWindow(dlg, GW_CHILD).unwrap_or_default();
    while !child.0.is_null() {
        let _ = SendMessageW(
            child,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
    }
    let text16: Vec<u16> = state_mut(dlg).text.encode_utf16().chain([0]).collect();
    let _ = SetDlgItemTextW(dlg, ID_MESSAGE, PCWSTR(text16.as_ptr()));
    let icon = question_icon();
    if icon != 0 {
        if let Ok(field) = GetDlgItem(Some(dlg), ID_ICON) {
            let _ = SendMessageW(
                field,
                STM_SETICON,
                Some(WPARAM(icon as usize)),
                Some(LPARAM(0)),
            );
        }
    }
    grow_for_text(dlg);
    1
}

/// The 45-unit message field is roomy at the template's 8 pt font but short
/// once the taller UI font is in it, so a wrapped message can outgrow the
/// field. The box then grows by the overflow, the field grows with it, and the
/// button row slides down so it keeps the template's bottom margin.
unsafe fn grow_for_text(dlg: HWND) {
    let Ok(edit) = GetDlgItem(Some(dlg), ID_MESSAGE) else {
        return;
    };
    let lines = SendMessageW(edit, EM_GETLINECOUNT, None, None).0 as i32;
    let mut tm = TEXTMETRICW::default();
    let dc = GetDC(Some(edit));
    let old = SelectObject(dc, HGDIOBJ(state_mut(dlg).font.0));
    let _ = GetTextMetricsW(dc, &mut tm);
    if !old.0.is_null() {
        SelectObject(dc, old);
    }
    ReleaseDC(Some(edit), dc);
    let need = lines * (tm.tmHeight + tm.tmExternalLeading);
    let mut er = RECT::default();
    if GetClientRect(edit, &mut er).is_err() {
        return;
    }
    let have = er.bottom - er.top;
    if need <= have {
        return;
    }
    let extra = need - have;
    let mut wr = RECT::default();
    if GetWindowRect(dlg, &mut wr).is_err() {
        return;
    }
    let _ = MoveWindow(
        dlg,
        wr.left,
        wr.top,
        wr.right - wr.left,
        wr.bottom - wr.top + extra + 6,
        true,
    );
    let mut rc = RECT::default();
    if GetWindowRect(edit, &mut rc).is_ok() {
        let mut at = POINT {
            x: rc.left,
            y: rc.top,
        };
        let _ = ScreenToClient(dlg, &mut at);
        let _ = MoveWindow(
            edit,
            at.x,
            at.y,
            rc.right - rc.left,
            rc.bottom - rc.top + extra + 2,
            true,
        );
    }
    for id in [ID_CANCEL, ID_YES, ID_YES_ALL, ID_NO, ID_NO_ALL] {
        let Ok(button) = GetDlgItem(Some(dlg), id) else {
            continue;
        };
        let mut rc = RECT::default();
        if GetWindowRect(button, &mut rc).is_err() {
            continue;
        }
        let mut at = POINT {
            x: rc.left,
            y: rc.top,
        };
        let _ = ScreenToClient(dlg, &mut at);
        let _ = MoveWindow(
            button,
            at.x,
            at.y + extra + 4,
            rc.right - rc.left,
            rc.bottom - rc.top,
            true,
        );
    }
}

unsafe fn end(dlg: HWND, answer: Answer) {
    state_mut(dlg).answer = answer;
    let _ = DestroyWindow(dlg);
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            init(hwnd)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let answer = match id {
                ID_YES => Answer::Yes,
                ID_YES_ALL => Answer::YesAll,
                ID_NO => Answer::No,
                ID_NO_ALL => Answer::NoAll,
                ID_CANCEL => Answer::Cancel,
                _ => return 0,
            };
            end(hwnd, answer);
            1
        }
        // The caption's X is a cancel.
        WM_CLOSE => {
            end(hwnd, Answer::Cancel);
            1
        }
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state_mut(hwnd).bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        // The message field has no border and sits on the dialog face: the box
        // is one surface: the text area has no field of its own.
        WM_CTLCOLOREDIT => {
            let st = state_mut(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
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

    /// The seven children keep the template's ids, styles and rows: the icon
    /// first (the static that takes the shell's question mark), 取消 the first
    /// tab stop, the other four buttons on one row, and the borderless
    /// read-only message field filling the top right.
    #[test]
    fn template_matches_the_box() {
        assert_eq!(ITEMS.len(), 7);
        assert_eq!(ITEMS[0].id, ID_ICON);
        assert_eq!(ITEMS[0].style, 0x5000_0003);
        assert_eq!((ITEMS[0].x, ITEMS[0].y, ITEMS[0].w, ITEMS[0].h), (7, 7, 21, 20));
        let buttons: Vec<&Item> = ITEMS[1..6].iter().collect();
        assert_eq!(
            buttons.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![ID_CANCEL, ID_YES, ID_YES_ALL, ID_NO, ID_NO_ALL]
        );
        for b in &buttons {
            assert_eq!(b.class, Class::Atom(ATOM_BUTTON));
            assert_eq!(b.style, 0x5001_0000);
            assert_eq!((b.y, b.w, b.h), (53, 50, 14));
        }
        assert_eq!(
            buttons.iter().map(|b| b.x).collect::<Vec<_>>(),
            vec![227, 7, 62, 117, 172]
        );
        assert_eq!(ITEMS[6].id, ID_MESSAGE);
        assert_eq!(ITEMS[6].class, Class::Atom(ATOM_EDIT));
        assert_eq!(ITEMS[6].style, 0x5001_0804);
        assert_eq!((ITEMS[6].x, ITEMS[6].y, ITEMS[6].w, ITEMS[6].h), (37, 7, 240, 45));
    }

    #[test]
    fn template_header_and_font_block() {
        let b = build_template();
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(&b[12..16], &DLG_STYLE.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 7);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), DLG_W);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), DLG_H);
        // Header 26 + menu/class 4 + title 16 + font block 6 + face 26, padded
        // to a DWORD: the first child starts at 80, its style at 88, its id
        // at 100.
        assert_eq!(u16::from_le_bytes([b[46], b[47]]), 8, "point size");
        assert_eq!(u16::from_le_bytes([b[48], b[49]]), 0, "weight");
        assert_eq!(b[50], 0, "no italic");
        assert_eq!(b[51], 1, "DEFAULT_CHARSET");
        assert_eq!(&b[88..92], &0x5000_0003u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(b[100..104].try_into().unwrap()), 2001);
    }

    /// The line-break normalization the field needs: LF-only text would show
    /// as one wrapped run in an edit control.
    #[test]
    fn message_breaks_become_crlf() {
        let normalize = |t: &str| t.replace('\r', "").replace('\n', "\r\n");
        assert_eq!(normalize("a\nb"), "a\r\nb");
        assert_eq!(normalize("a\r\nb"), "a\r\nb");
        assert_eq!(normalize("no break"), "no break");
    }
}
