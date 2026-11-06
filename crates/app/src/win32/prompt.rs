//! The native modal prompt box. Two 70x22 buttons stacked at the right (确定 above
//! 取消). Those are the control rectangles this module reproduces, so the box is
//! built from plain `CreateWindowExW` children at those offsets rather than from a
//! `DLGTEMPLATE`, whose item padding rules are not observable here. Modality is
//! the three things the dialog manager itself does: the owner window is disabled,
//! `IsDialogMessageW` drives Tab / Enter / Escape on the box, and the loop ends
//! when the box is destroyed. Disabling the owner is also what keeps the frame
//! painting — its messages still flow, only its input is parked. The buttons and
//! the edit are the real `BUTTON`/`EDIT` classes, so they take the system theme,
//! which is what makes this box the same kind of object as the rest of the
//! chrome. The face, the field and the ink come from the active `Theme`, and the
//! controls draw with the same per-DPI UI font the menus and the strip use.

#![allow(unsafe_op_in_unsafe_fn)]

pub(super) use crate::win32::wstr::wide;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateSolidBrush, DeleteObject, FillRect, GetSysColorBrush, HBRUSH, HDC, HFONT,
};
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_READONLY, GA_ROOTOWNER, GWLP_USERDATA,
    GWL_STYLE, GetAncestor, GetClientRect, GetDlgItem, GetMessageW, GetWindow, GetWindowLongPtrW,
    GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDCANCEL, IDOK, IsDialogMessageW, IsWindow, MSG,
    PostQuitMessage, RegisterClassW, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SendMessageW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_ERASEBKGND, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_CLIPCHILDREN, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    GW_CHILD, GW_HWNDNEXT,
};
use windows::core::{PCWSTR, w};

use crate::win32::view::instance;
use crate::win32::window::to_colorref;

use super::chrome;
use super::customdraw;

/// The box's window class. Registered once per process; the name is
/// process-wide, so a second box reuses the registration.
const BOX_CLASS: PCWSTR = w!("DogEditPrompt");
pub(super) const CLASS_STATIC: PCWSTR = w!("Static");
pub(super) const CLASS_EDIT: PCWSTR = w!("Edit");
pub(super) const CLASS_BUTTON: PCWSTR = w!("Button");
pub(super) const CLASS_COMBO: PCWSTR = w!("ComboBox");

/// The edit's and the question's control ids
/// them (`1200`/`3000`; the buttons are the dialog manager's `IDOK`/`IDCANCEL`).
const ID_EDIT: i32 = 1_200;
const ID_QUESTION: i32 = 3_000;

/// The client rectangles at 100% scaling, in px:
/// (x, y, width, height) for static, edit, OK
/// system-DPI-aware app, so at 120 px/inch its dialog measured 464x163 physical
/// with an 88x28 button and a 331x24 edit — those are exactly these numbers
/// times 1.25, which is why every rectangle here is unscaled and `run_box`
/// scales it.
const RECT_QUESTION: (i32, i32, i32, i32) = (9, 11, 261, 30);
const RECT_EDIT: (i32, i32, i32, i32) = (8, 48, 265, 19);
const RECT_OK: (i32, i32, i32, i32) = (277, 9, 70, 22);
const RECT_CANCEL: (i32, i32, i32, i32) = (277, 44, 70, 22);
const BOX_W: i32 = 371;
const BOX_H: i32 = 130;

/// What the box shows and what it gives back. The pointer travels through
/// `GWLP_USERDATA`, so the state has to outlive the window it is attached to.
struct BoxState<'a> {
    question: &'a str,
    initial: &'a str,
    /// The edit is read-only for a result box (选区求和 shows a total; there is
    /// nothing to type), editable for a prompt (重命名).
    editable: bool,
    /// Filled with the edit's text when 确定 was used, left `None` for 取消 and
    /// for the caption's X.
    result: Option<String>,
    /// Set from `box_proc` when the box really went away. The modal loop uses it
    /// to tell its own `WM_QUIT` from one that was meant for the frame.
    destroyed: bool,
    /// The active theme's face, edit field and ink. Null/zero when the owner
    /// carries no editor state, in which case the box keeps the system
    /// colours.
    bg: HBRUSH,
    field: HBRUSH,
    face: COLORREF,
    field_face: COLORREF,
    ink: COLORREF,
    /// The per-DPI UI font every child draws with — the same face the menus
    /// and the status strip use, not the fontless system default a plain
    /// `CreateWindowExW` edit would pick.
    font: HFONT,
}

/// Runs one modal box. `None` means the user cancelled; `Some(text)` is the
/// edit's content.
fn run_box(owner: HWND, title: &str, state: &mut BoxState<'_>) -> Option<String> {
    unsafe {
        register_class();
        let dpi_px = GetDpiForWindow(owner).max(96) as i32;
        let dpi = dpi_px as f32 / 96.0;
        let scale = |v: i32| (v as f32 * dpi).round() as i32;
        // The box follows the document theme: the face and the edit's field
        // come from the same palette the other dialogs use, and the caption
        // takes the dark attribute.
        //
        // 颜色要问**主窗口**，不是 `owner` 本身：所有者也可以是别的对话框
        // （主题对话框双击一项几何就开这个提示框）。`state_mut` 只是把
        // `GWLP_USERDATA` 当成 `*mut EditorState`，对着一个 `#32770` 读会拿到
        // 垃圾指针 —— `docs.act()` 当场下标越界，窗口过程里 panic 不 unwind，
        // 整个进程 abort。`GA_ROOTOWNER` 顺着所有者链走到最外层，正好是主窗口；
        // 所有者本来就是主窗口时它返回自己。
        let root = GetAncestor(owner, GA_ROOTOWNER);
        let theme = crate::win32::window::state_mut(root).map(|st| st.docs.act().theme.clone());
        if let Some(theme) = &theme {
            state.bg = CreateSolidBrush(to_colorref(theme.toolbar_bg));
            state.field = CreateSolidBrush(to_colorref(theme.toolbar_field));
            state.face = to_colorref(theme.toolbar_bg);
            state.field_face = to_colorref(theme.toolbar_field);
            state.ink = to_colorref(theme.toolbar_fg);
            state.font = crate::win32::uifont::create(dpi_px);
        }

        let title16 = wide(title);
        let state_ptr = state as *mut BoxState<'_> as *const core::ffi::c_void;
        // Created without `WS_VISIBLE`: the children are added below and the box
        // appears at once instead of an empty frame flashing at the default
        // position.
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            BOX_CLASS,
            PCWSTR::from_raw(title16.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            scale(BOX_W),
            scale(BOX_H),
            Some(owner),
            None,
            Some(instance()),
            Some(state_ptr),
        ) else {
            free_look(state);
            return None;
        };

        let question16 = wide(state.question);
        let initial16 = wide(state.initial);
        let ok16 = wide(crate::win32::lang::dlg(209, 1).unwrap_or("确定"));
        let cancel16 = wide(crate::win32::lang::dlg(209, 2).unwrap_or("取消"));
        add_child(
            hwnd,
            CLASS_STATIC,
            PCWSTR::from_raw(question16.as_ptr()),
            ID_QUESTION,
            WINDOW_STYLE::default(),
            RECT_QUESTION,
            dpi,
        );
        let edit_style = WINDOW_STYLE(
            WS_BORDER.0
                | ES_AUTOHSCROLL as u32
                | if state.editable {
                    0
                } else {
                    ES_READONLY as u32
                },
        );
        let edit = add_child(
            hwnd,
            CLASS_EDIT,
            PCWSTR::from_raw(initial16.as_ptr()),
            ID_EDIT,
            edit_style,
            RECT_EDIT,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(ok16.as_ptr()),
            IDOK.0 as i32,
            WS_TABSTOP,
            RECT_OK,
            dpi,
        );
        add_child(
            hwnd,
            CLASS_BUTTON,
            PCWSTR::from_raw(cancel16.as_ptr()),
            IDCANCEL.0 as i32,
            WS_TABSTOP,
            RECT_CANCEL,
            dpi,
        );

        if !state.font.0.is_null() {
            let _ = SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(state.font.0 as usize)),
                Some(LPARAM(1)),
            );
            let mut child = GetWindow(hwnd, GW_CHILD).unwrap_or_default();
            while !child.0.is_null() {
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(state.font.0 as usize)),
                    Some(LPARAM(1)),
                );
                child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
            }
        }
        if let Some(theme) = &theme {
            let dark = chrome::theme_is_dark(theme);
            chrome::set_chrome_theme(theme);
            chrome::dark_theme_children(hwnd, dark);
            chrome::title_bar(hwnd, dark);
            // The class erases with the theme brush; without this the parent's
            // erase would also pass over every child and only the controls
            // that change would repaint.
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        }

        show_modal(hwnd, owner);
        if let Some(e) = edit {
            // A pre-filled box has its whole line selected, so typing replaces
            // it. The 重命名 box starts empty, so that path is the rare
            // one here.
            if state.editable && !state.initial.is_empty() {
                let _ = SendMessageW(e, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            let _ = SetFocus(Some(e));
        }

        run_modal_loop(hwnd, owner, std::ptr::addr_of!(state.destroyed));
        free_look(state);
        state.result.take()
    }
}

/// Releases the brushes and the font a themed box built for itself.
unsafe fn free_look(state: &mut BoxState<'_>) {
    if !state.bg.0.is_null() {
        let _ = DeleteObject(state.bg.into());
        state.bg = HBRUSH::default();
    }
    if !state.field.0.is_null() {
        let _ = DeleteObject(state.field.into());
        state.field = HBRUSH::default();
    }
    if !state.font.0.is_null() {
        let _ = DeleteObject(state.font.into());
        state.font = HFONT::default();
    }
}

/// Pumps messages until the box is gone. Every native box uses this: the
/// dialog manager owns Tab, Enter and Escape, and the rest of the tree has to
/// keep being dispatched while the box is up.
///
/// `destroyed` points at the box proc's own flag. It tells a `WM_QUIT` posted
/// by this box's `WM_DESTROY` apart from one that was meant for the frame; only
/// the latter is re-posted on the way out.
///
/// The flag is written by the window procedure through the `GWLP_USERDATA`
/// raw pointer, i.e. through a second, independent handle to the state. It must
/// not arrive here as `&mut bool`: LLVM marks a `&mut` parameter `noalias`, so
/// an optimized (LTO) build is free to cache the initial `false`. It then treats
/// the box's own `WM_QUIT` as the frame's, re-posts it, and the whole process
/// ends the first time any box is dismissed. A raw pointer with a volatile
/// read keeps both sides looking at the same byte.
///
/// The frame can also go away while a box is up (taskbar close, shutdown).
/// Windows then destroys its owned boxes too, so `destroyed` is true here as
/// well — but that `WM_QUIT` came from the frame and must still be re-posted,
/// or the process is left running with no window at all. A live owner is the
/// second half of the test.
pub(super) unsafe fn run_modal_loop(hwnd: HWND, owner: HWND, destroyed: *const bool) {
    let mut msg = MSG::default();
    loop {
        let got = GetMessageW(&mut msg, None, 0, 0);
        if got.0 <= 0 {
            if got.0 == 0
                && (!std::ptr::read_volatile(destroyed) || !IsWindow(Some(owner)).as_bool())
            {
                PostQuitMessage(msg.wParam.0 as i32);
            }
            break;
        }
        if IsDialogMessageW(hwnd, &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        let _ = DispatchMessageW(&msg);
    }
    let _ = EnableWindow(owner, true);
    if !std::ptr::read_volatile(destroyed) {
        DestroyWindow(hwnd).ok();
    }
    let _ = SetFocus(Some(owner));
}

/// One child of the box, placed at a measured rectangle scaled for the owner's
/// DPI.
pub(super) unsafe fn add_child(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
    id: i32,
    style: WINDOW_STYLE,
    rect: (i32, i32, i32, i32),
    dpi: f32,
) -> Option<HWND> {
    let s = |v: i32| (v as f32 * dpi).round() as i32;
    CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        text,
        style | WS_CHILD | WS_VISIBLE,
        s(rect.0),
        s(rect.1),
        s(rect.2),
        s(rect.3),
        Some(parent),
        if id == 0 {
            None
        } else {
            Some(HMENU(id as usize as *mut core::ffi::c_void))
        },
        Some(instance()),
        None,
    )
    .ok()
}

/// Places the box over its owner and makes it the modal front: centre, park the
/// owner, show, activate. The owner is parked, not destroyed — its `WM_PAINT`
/// still runs, which is why the editor keeps updating behind the box.
pub(super) unsafe fn show_modal(hwnd: HWND, owner: HWND) {
    center_on_owner(hwnd, owner);
    let _ = EnableWindow(owner, false);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetActiveWindow(hwnd);
}

/// The box is centred over the frame, the way an owned dialog is: the box
/// sits inside its owner's rect.
pub(super) unsafe fn center_on_owner(hwnd: HWND, owner: HWND) {
    let mut o = RECT::default();
    let mut b = RECT::default();
    if GetWindowRect(owner, &mut o).is_err() || GetWindowRect(hwnd, &mut b).is_err() {
        return;
    }
    let bw = b.right - b.left;
    let bh = b.bottom - b.top;
    let _ = SetWindowPos(
        hwnd,
        None,
        o.left + (o.right - o.left - bw) / 2,
        o.top + (o.bottom - o.top - bh) / 2,
        0,
        0,
        SWP_NOSIZE | SWP_NOACTIVATE,
    );
}

unsafe fn register_class() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(box_proc),
            hInstance: instance(),
            lpszClassName: BOX_CLASS,
            // The dialog face, which is what an unthemed `#32770` erases to; the
            // themed controls paint over it.
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            eprintln!("prompt class registration failed");
        }
    });
}

unsafe extern "system" fn box_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return LRESULT(0);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut BoxState<'_>;
    if msg == WM_COMMAND {
        let id = (wparam.0 & 0xFFFF) as i32;
        if id == IDOK.0 || id == IDCANCEL.0 {
            if id == IDOK.0 && !state.is_null() {
                (*state).result = Some(edit_text(hwnd));
            }
            DestroyWindow(hwnd).ok();
            return LRESULT(0);
        }
    } else if msg == WM_CLOSE {
        DestroyWindow(hwnd).ok();
        return LRESULT(0);
    } else if msg == WM_DESTROY {
        // Ends the modal loop even when the box went away some other way.
        if !state.is_null() {
            (*state).destroyed = true;
        }
        PostQuitMessage(0);
        return LRESULT(0);
    } else if msg == WM_ERASEBKGND {
        // The class brush is the light system face; a themed box fills with
        // its own instead.
        if !state.is_null() && !(*state).bg.0.is_null() {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, (*state).bg);
            }
            return LRESULT(1);
        }
    } else if msg == WM_CTLCOLORSTATIC || msg == WM_CTLCOLORBTN {
        // The question and the two buttons sit on the dialog face.
        if !state.is_null() && !(*state).bg.0.is_null() {
            return LRESULT(customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                (*state).ink,
                (*state).face,
                (*state).bg,
            ));
        }
    } else if msg == WM_CTLCOLOREDIT {
        // The answer's line is a field, one step off the face.
        if !state.is_null() && !(*state).field.0.is_null() {
            return LRESULT(customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                (*state).ink,
                (*state).field_face,
                (*state).field,
            ));
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// The edit's current text, read through the dialog item the box owns.
unsafe fn edit_text(hwnd: HWND) -> String {
    let Ok(edit) = GetDlgItem(Some(hwnd), ID_EDIT) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(edit);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(edit, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

/// 重命名-style prompt: a caption, a question, an editable value.
pub(super) fn prompt(owner: HWND, title: &str, question: &str, initial: &str) -> Option<String> {
    let mut state = BoxState {
        question,
        initial,
        editable: true,
        result: None,
        destroyed: false,
        bg: HBRUSH::default(),
        field: HBRUSH::default(),
        face: COLORREF(0),
        field_face: COLORREF(0),
        ink: COLORREF(0),
        font: HFONT::default(),
    };
    run_box(owner, title, &mut state)
}

/// 选区求和-style result box: the same measured dialog with the value in a
/// read-only edit, so 确定 dismisses it and the number stays copyable.
pub(super) fn show_value(owner: HWND, title: &str, question: &str, value: &str) -> Option<String> {
    let mut state = BoxState {
        question,
        initial: value,
        editable: false,
        result: None,
        destroyed: false,
        bg: HBRUSH::default(),
        field: HBRUSH::default(),
        face: COLORREF(0),
        field_face: COLORREF(0),
        ink: COLORREF(0),
        font: HFONT::default(),
    };
    run_box(owner, title, &mut state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialog rectangles are this box's layout constants; a typo here moves a control
    /// off the pixel grid they are drawn on.
    #[test]
    fn measured_box_geometry_is_kept() {
        assert_eq!((BOX_W, BOX_H), (371, 130));
        assert_eq!(RECT_QUESTION.2, 261);
        assert_eq!(RECT_EDIT.2, 265);
        assert_eq!(RECT_OK.0, RECT_CANCEL.0);
        assert_eq!(RECT_OK.3, 22);
        assert_eq!(RECT_OK.1 + RECT_CANCEL.1, 9 + 44);
    }

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    }
}
