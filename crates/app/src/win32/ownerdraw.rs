//! Owner-drawn push buttons: how one looks, how it answers the mouse, and
//! where its label goes.
//!
//! Four dialogs paint their own buttons — 跳转对话框 (`gotoany`), 自定义标记
//! (`markdlg`), 快捷键管理 (`shortcutdlg`) and the find bar (`minifind`) —
//! because a themed `BUTTON` keeps the system palette and would be the only
//! light control in a dark dialog. Each had grown its own copy of "fill the
//! face, draw the frame, draw the label, pick the colour by state", and the
//! copies had drifted: only one had a pressed face, only one inset its face by
//! a pixel, and **none** had a hover state, so every one of those buttons sat
//! there looking dead under the cursor.
//!
//! What is shared lives here. What stays with the dialog is its palette (the
//! [`Look`] it builds) and any decoration of its own — 自定义标记's checkbox,
//! the find bar's "this toggle is on" face.
//!
//! The hover state is the reason this is more than a paint helper: a themed
//! button lightens itself when the mouse arrives, an owner-drawn one is painted
//! by the parent and the parent is never told. [`install`] is that missing
//! notification. It keeps the state in a thread-local set of window handles
//! instead of each dialog's state struct, so a dialog only calls [`install`]
//! once and every paint path gets the hover for free.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DrawTextW, FillRect, FrameRect,
    HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, InvalidateRect, RoundRect, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_SELECTED, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GWLP_USERDATA, GWLP_WNDPROC, GetClientRect, GetDlgItem, GetWindowLongPtrW,
    SetWindowLongPtrW, WM_MOUSEMOVE, WM_NCDESTROY, WNDPROC,
};

/// Where the label sits. `Left` carries the inset from the face's left edge —
/// 快捷键管理 draws its three buttons that way, 自定义标记's checkboxes too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Align {
    Center,
    Left(i32),
}

/// The outline, which is the one part the four dialogs genuinely do
/// differently: a rounded 1 px ring (跳转), a square 1 px frame (标记/快捷键),
/// or nothing at all (the find bar's flat toolbar buttons). A rounded button
/// may name a brighter pen for the hover state — on a dark dialog the face step
/// alone is easy to miss and the border is what the eye catches.
pub(super) enum Edge {
    None,
    Square(HBRUSH),
    Round {
        pen: HPEN,
        hot_pen: Option<HPEN>,
        diameter: i32,
    },
}

/// The label colour per state. `pressed` is separate because two of the four
/// dialogs brighten the text instead of the face when a button goes down.
pub(super) struct Ink {
    pub normal: COLORREF,
    pub pressed: COLORREF,
    pub disabled: COLORREF,
}

/// Everything about how one dialog's buttons look. Built per paint from the
/// dialog's own brushes; nothing here is created or freed by this module.
pub(super) struct Look {
    pub face: HBRUSH,
    /// Face under the cursor. A dialog that wants no hover passes `face`.
    pub hot: HBRUSH,
    pub press: HBRUSH,
    pub edge: Edge,
    pub ink: Ink,
    /// The font the label is drawn with — the button's own (`WM_GETFONT`) when
    /// the caller has it, else the dialog's.
    pub font: HFONT,
    /// Painted over the whole control rect before the face. A rounded button
    /// needs it: the pixels outside the corner arc would otherwise keep the
    /// system face colour and show as light notches.
    pub under: Option<HBRUSH>,
    /// How far the face is inset from the control rect. 1 for a rounded button
    /// (the dialog surface shows as a ring around the face), 0 for a square
    /// one, which covers its whole rect.
    pub inset: i32,
    /// How far the label travels while the button is held down. 1 is what the
    /// system's own buttons do.
    pub press_offset: i32,
    /// Read the control's live client rect instead of `di.rcItem`. The two are
    /// normally the same rectangle, but a control that was moved or resized
    /// just before this message can hand over a stale `rcItem` — 跳转对话框
    /// resizes its controls with `WM_SIZE`, so it asks for the live one. The
    /// dialogs that never move a button leave this false and keep the rect the
    /// button itself measured (measured: the two differ by a pixel there, and
    /// the label centring follows it).
    pub live_rect: bool,
}

/// The four states a button can be painted in, in precedence order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum State {
    Normal,
    Hot,
    Pressed,
    Disabled,
}

/// Which state `itemState` plus the hover set says. Disabled wins outright — a
/// disabled button is not hot and cannot be pressed; pressed wins over hot
/// because the held-down face is the stronger signal.
pub(super) fn state_of(item_state: u32, hot: bool) -> State {
    if item_state & ODS_DISABLED.0 != 0 {
        State::Disabled
    } else if item_state & ODS_SELECTED.0 != 0 {
        State::Pressed
    } else if hot {
        State::Hot
    } else {
        State::Normal
    }
}

fn face_of(look: &Look, state: State) -> HBRUSH {
    match state {
        State::Pressed => look.press,
        State::Hot => look.hot,
        State::Normal | State::Disabled => look.face,
    }
}

fn ink_of(look: &Look, state: State) -> COLORREF {
    match state {
        State::Pressed => look.ink.pressed,
        State::Disabled => look.ink.disabled,
        State::Normal | State::Hot => look.ink.normal,
    }
}

/// The label's rectangle: the face rect, inset for `Align::Left`, then moved by
/// the pressed offset.
fn label_rect(rc: RECT, align: Align, press_offset: i32) -> RECT {
    let mut out = match align {
        Align::Center => rc,
        Align::Left(pad) => RECT {
            left: rc.left + pad,
            ..rc
        },
    };
    out.left += press_offset;
    out.top += press_offset;
    out
}

/// The control's *current* client rect, not `di.rcItem`. For a button the two
/// are the same rectangle — but `rcItem` is filled in by the button just before
/// it sends this message, and a control moved in the meantime can hand over a
/// rect that no longer matches its window (measured in 跳转对话框, where a
/// second border appeared inside the first).
unsafe fn control_rect(di: &DRAWITEMSTRUCT, live: bool) -> RECT {
    if !live {
        return di.rcItem;
    }
    let mut r = RECT::default();
    if GetClientRect(di.hwndItem, &mut r).is_ok() && r.right > 0 && r.bottom > 0 {
        return r;
    }
    di.rcItem
}

/// Paints the button's face and frame, and hands back the rect the label
/// should be drawn in.
pub(super) unsafe fn frame(di: &DRAWITEMSTRUCT, look: &Look) -> RECT {
    let rc = control_rect(di, look.live_rect);
    if let Some(under) = look.under {
        FillRect(di.hDC, &rc, under);
    }
    let inner = RECT {
        left: rc.left + look.inset,
        top: rc.top + look.inset,
        right: rc.right - look.inset,
        bottom: rc.bottom - look.inset,
    };
    let state = state_of(di.itemState.0, is_hot(di.hwndItem));
    let face = face_of(look, state);
    let old_brush = SelectObject(di.hDC, HGDIOBJ(face.0));
    match look.edge {
        Edge::None => {
            FillRect(di.hDC, &inner, face);
        }
        Edge::Square(edge) => {
            FillRect(di.hDC, &inner, face);
            FrameRect(di.hDC, &inner, edge);
        }
        Edge::Round {
            pen,
            hot_pen,
            diameter,
        } => {
            let pen = match (hot_pen, state) {
                (Some(hot), State::Hot) => hot,
                _ => pen,
            };
            let old_pen = SelectObject(di.hDC, HGDIOBJ(pen.0));
            let _ = RoundRect(
                di.hDC,
                inner.left,
                inner.top,
                inner.right,
                inner.bottom,
                diameter,
                diameter,
            );
            SelectObject(di.hDC, old_pen);
        }
    }
    SelectObject(di.hDC, old_brush);
    rc
}

/// Draws the label over a rect [`frame`] handed back (or a narrower one, for a
/// caller that draws something of its own first — 自定义标记's checkbox).
pub(super) unsafe fn label(di: &DRAWITEMSTRUCT, rc: RECT, look: &Look, text: &str, align: Align) {
    let state = state_of(di.itemState.0, is_hot(di.hwndItem));
    let trc = label_rect(rc, align, look.press_offset);
    draw_text(di.hDC, trc, look.font, ink_of(look, state), text, align);
}

/// One line of text into any DC, with the font and ink already decided.
///
/// [`label`] is this plus the state lookup. It is public to the module because
/// the two header painters and 自定义标记's tick glyph draw text that is not a
/// button label, and they had each copied this body — including the trap below.
///
/// **No trailing NUL.** `DrawTextW` in `windows` 0.62 takes `&mut [u16]` and
/// passes `slice.len()` as `cchText`, so a NUL-terminated buffer counts the NUL
/// as one more character: measured 「添加(A)」 52 px bare vs 59 px with the NUL,
/// which centres the text 3 px to the left.
pub(super) unsafe fn draw_text(
    hdc: HDC,
    mut rc: RECT,
    font: HFONT,
    ink: COLORREF,
    text: &str,
    align: Align,
) {
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, ink);
    let mut buf = text_buf(text);
    let flags = match align {
        Align::Center => DT_CENTER,
        Align::Left(_) => DT_LEFT,
    };
    DrawTextW(
        hdc,
        &mut buf,
        &mut rc,
        flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
}

/// The `DrawTextW` buffer for `s`: exactly its UTF-16 code units, no terminator.
///
/// Split out of [`draw_text`] so a test can hold the length to the invariant —
/// the bug it guards is silent at compile time and shows up only as text
/// sitting a few pixels off centre, which is easy to mistake for a font
/// difference.
fn text_buf(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

thread_local! {
    /// The buttons whose cursor is inside them right now, by window handle.
    /// Thread-local because a dialog lives on the thread that created it and
    /// the subclass runs on that same thread.
    static HOT: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

/// Whether the cursor is over `btn`. A paint path calls this through [`frame`]
/// and [`label`]; nothing else needs to.
pub(super) fn is_hot(btn: HWND) -> bool {
    HOT.with(|h| h.borrow().contains(&(btn.0 as isize)))
}

/// Records `on` for `btn`, and answers whether that changed anything — the
/// repaint is only worth asking for when it did.
fn set_hot(btn: HWND, on: bool) -> bool {
    HOT.with(|h| {
        let mut v = h.borrow_mut();
        let key = btn.0 as isize;
        let had = v.contains(&key);
        match (on, had) {
            (true, false) => {
                v.push(key);
                true
            }
            (false, true) => {
                v.retain(|k| *k != key);
                true
            }
            _ => false,
        }
    })
}

/// `lParam`'s client coordinates are inside `hwnd`'s client rect? A captured
/// button keeps receiving `WM_MOUSEMOVE` after the cursor has left it, so the
/// point decides, not the message.
fn point_in_client(hwnd: HWND, lparam: LPARAM) -> bool {
    let mut rc = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
        return false;
    }
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}

/// The subclass every owner-drawn button gets. It only maintains the hover set
/// and asks for the repaint; every message still reaches the button, which owns
/// the press/release state machine, the `WM_DRAWITEM` it sends its parent, and
/// its own tracking.
unsafe extern "system" fn btn_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_MOUSEMOVE => {
            let inside = point_in_client(hwnd, lparam);
            if set_hot(hwnd, inside) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            if inside {
                // `TME_LEAVE` is what turns this into a `WM_MOUSELEAVE` later.
                // The request is one-shot, so a themed control renews it on
                // every move and so does this.
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
        }
        WM_MOUSELEAVE => {
            if set_hot(hwnd, false) {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        // A destroyed button must not leave its handle in the set: Windows
        // reuses handle values, and a new button that landed on a stale one
        // would come up already hot.
        WM_NCDESTROY => {
            set_hot(hwnd, false);
        }
        _ => {}
    }
    let orig = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if orig != 0 {
        let f: WNDPROC = core::mem::transmute(orig);
        return CallWindowProcW(f, hwnd, msg, wparam, lparam).0 as isize;
    }
    0
}

/// Gives one button the hover behaviour. Safe to call more than once on the
/// same handle only if the window procedure the first install chained to is
/// still in place — callers install once, when the button is created.
pub(super) unsafe fn install_handle(btn: HWND) {
    let prev = SetWindowLongPtrW(btn, GWLP_WNDPROC, btn_proc as *const () as usize as isize);
    // The procedure it replaced rides on the button's own USERDATA: a `BUTTON`
    // has no other use for that slot, and the dialogs' USERDATA is taken by their
    // own state structs.
    SetWindowLongPtrW(btn, GWLP_USERDATA, prev);
}

/// Installs the hover behaviour on every one of `ids` the dialog has.
pub(super) unsafe fn install(dlg: HWND, ids: &[i32]) {
    for id in ids {
        if let Ok(btn) = GetDlgItem(Some(dlg), *id) {
            install_handle(btn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disabled wins, then pressed, then hot. The order matters: a disabled
    /// button under the cursor must not light up, and a held-down one must not
    /// fall back to the hover face when the cursor wanders off it.
    #[test]
    fn the_state_precedence_is_disabled_then_pressed_then_hot() {
        assert_eq!(state_of(0, false), State::Normal);
        assert_eq!(state_of(0, true), State::Hot);
        assert_eq!(state_of(ODS_SELECTED.0, false), State::Pressed);
        assert_eq!(state_of(ODS_SELECTED.0, true), State::Pressed);
        assert_eq!(state_of(ODS_DISABLED.0, true), State::Disabled);
        assert_eq!(
            state_of(ODS_DISABLED.0 | ODS_SELECTED.0, true),
            State::Disabled,
            "a disabled button cannot be pressed"
        );
        // ODS_FOCUS alone is not one of the four.
        assert_eq!(state_of(0x10, false), State::Normal);
    }

    /// `Align::Left(pad)` insets from the face's left edge and leaves the top
    /// alone; the pressed offset moves both axes by the same amount.
    #[test]
    fn the_label_rect_follows_the_alignment_then_the_press() {
        let rc = RECT {
            left: 10,
            top: 20,
            right: 110,
            bottom: 50,
        };
        assert_eq!(label_rect(rc, Align::Center, 0), rc);
        let left = label_rect(rc, Align::Left(6), 0);
        assert_eq!((left.left, left.top, left.right), (16, 20, 110));
        let pressed = label_rect(rc, Align::Center, 1);
        assert_eq!((pressed.left, pressed.top), (11, 21));
        let both = label_rect(rc, Align::Left(6), 1);
        assert_eq!((both.left, both.top), (17, 21));
    }

    /// The hover set is a set: a second `on` for the same handle is not a
    /// change (so no repaint), and neither is an `off` for one that is not in
    /// it.
    #[test]
    fn the_hover_set_only_reports_real_changes() {
        let btn = HWND(0x1234 as *mut core::ffi::c_void);
        set_hot(btn, false);
        assert!(!is_hot(btn));
        assert!(set_hot(btn, true), "first entry is a change");
        assert!(is_hot(btn));
        assert!(!set_hot(btn, true), "already hot");
        assert!(set_hot(btn, false), "leaving is a change");
        assert!(!is_hot(btn));
        assert!(!set_hot(btn, false), "already cold");
    }

    /// The subclass, driven through a real button. `SendMessageW` runs
    /// `btn_proc` on *this* thread — the same one that created the window — so
    /// the thread-local the paint path reads is the one being written here.
    ///
    /// This is the half a screenshot cannot prove on its own: a paint diff only
    /// shows the hot face if the button was really marked hot, and a session
    /// with no active input queue (no foreground window) never delivers a
    /// `WM_MOUSEMOVE` at all, so every hover measurement reads a flat zero.
    #[test]
    fn the_subclass_marks_the_button_hot_while_the_point_is_inside() {
        use windows::Win32::UI::WindowsAndMessaging::{
            BS_OWNERDRAW, CreateWindowExW, DestroyWindow, HMENU, SendMessageW, WINDOW_EX_STYLE,
            WINDOW_STYLE, WS_CHILD, WS_POPUP,
        };
        use windows::core::{PCWSTR, w};

        // Pack a client point the way the OS does for WM_MOUSEMOVE.
        let at = |x: i32, y: i32| LPARAM(((y << 16) | (x & 0xFFFF)) as isize);
        let make = |class, parent, style, id| unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR::null(),
                style,
                0,
                0,
                40,
                20,
                parent,
                Some(HMENU(id as *mut core::ffi::c_void)),
                Some(super::super::view::instance()),
                None,
            )
        };

        unsafe {
            let owner = make(w!("STATIC"), None, WS_POPUP, 0).expect("owner window");
            let btn = make(
                w!("BUTTON"),
                Some(owner),
                WINDOW_STYLE(WS_CHILD.0 | BS_OWNERDRAW as u32),
                1,
            )
            .expect("button");

            assert!(!is_hot(btn), "a fresh button is cold");
            install_handle(btn);

            // Inside the 40x20 client.
            SendMessageW(btn, WM_MOUSEMOVE, Some(WPARAM(0)), Some(at(5, 5)));
            assert!(is_hot(btn), "a move inside the client marks it hot");

            // A captured button keeps receiving moves after the cursor has left
            // it, so the point — not the message — decides.
            SendMessageW(btn, WM_MOUSEMOVE, Some(WPARAM(0)), Some(at(60, 5)));
            assert!(!is_hot(btn), "a move outside the client clears it");

            SendMessageW(btn, WM_MOUSEMOVE, Some(WPARAM(0)), Some(at(5, 5)));
            assert!(is_hot(btn));
            SendMessageW(btn, WM_MOUSELEAVE, None, None);
            assert!(!is_hot(btn), "leaving clears it");

            // A destroyed button must not linger: handles get reused, and a new
            // button that landed on a stale one would come up already hot.
            SendMessageW(btn, WM_MOUSEMOVE, Some(WPARAM(0)), Some(at(5, 5)));
            assert!(is_hot(btn));
            let _ = DestroyWindow(btn);
            assert!(!is_hot(btn), "WM_NCDESTROY drops the handle");
            let _ = DestroyWindow(owner);
        }
    }

    /// The other half: that a hot button is really painted in the hot brush.
    /// [`frame`] is driven against a memory DC and the pixel is read back, so
    /// this closes the loop `is_hot` → `state_of` → `face_of` → `FillRect`
    /// without needing anything to appear on screen.
    #[test]
    fn the_hot_face_is_the_one_that_gets_painted() {
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
            GetDC, GetPixel, ReleaseDC,
        };
        use windows::Win32::UI::Controls::{ODS_DISABLED, ODS_FLAGS, ODS_SELECTED};
        use windows::Win32::UI::WindowsAndMessaging::{
            BS_OWNERDRAW, CreateWindowExW, DestroyWindow, HMENU, SendMessageW, WINDOW_EX_STYLE,
            WINDOW_STYLE, WS_CHILD, WS_POPUP,
        };
        use windows::core::{PCWSTR, w};

        let at = |x: i32, y: i32| LPARAM(((y << 16) | (x & 0xFFFF)) as isize);
        // Three faces that no two states share, so the pixel says which won.
        let cold_rgb = 0x0011_2233u32;
        let hot_rgb = 0x0044_5566u32;
        let press_rgb = 0x0077_8899u32;

        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                40,
                20,
                None,
                Some(HMENU(0 as *mut core::ffi::c_void)),
                Some(super::super::view::instance()),
                None,
            )
            .expect("owner window");
            let btn = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | BS_OWNERDRAW as u32),
                0,
                0,
                40,
                20,
                Some(owner),
                Some(HMENU(1 as *mut core::ffi::c_void)),
                Some(super::super::view::instance()),
                None,
            )
            .expect("button");
            install_handle(btn);

            let (face, hot, press) = (
                CreateSolidBrush(COLORREF(cold_rgb)),
                CreateSolidBrush(COLORREF(hot_rgb)),
                CreateSolidBrush(COLORREF(press_rgb)),
            );
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, 40, 20);
            let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));

            let look = Look {
                face,
                hot,
                press,
                edge: Edge::None,
                ink: Ink {
                    normal: COLORREF(0),
                    pressed: COLORREF(0),
                    disabled: COLORREF(0),
                },
                font: HFONT::default(),
                under: None,
                inset: 0,
                press_offset: 0,
                live_rect: false,
            };
            let mut di = DRAWITEMSTRUCT {
                itemState: ODS_FLAGS(0),
                hwndItem: btn,
                hDC: dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: 40,
                    bottom: 20,
                },
                ..Default::default()
            };
            // `label` is not called: the font is empty and the text colour
            // would overwrite the face. This test is about the face only.
            let centre = |dc| GetPixel(dc, 20, 10);

            // Cold.
            let _ = frame(&di, &look);
            assert_eq!(centre(dc).0, cold_rgb, "resting face");

            // Hot — the point is inside the button.
            SendMessageW(btn, WM_MOUSEMOVE, Some(WPARAM(0)), Some(at(5, 5)));
            let _ = frame(&di, &look);
            assert_eq!(centre(dc).0, hot_rgb, "hover face");

            // Held down beats hover.
            di.itemState = ODS_FLAGS(ODS_SELECTED.0);
            let _ = frame(&di, &look);
            assert_eq!(centre(dc).0, press_rgb, "pressed face, even while hot");

            // Disabled beats both, and is not hot: it must fall back to the
            // resting face rather than the hover one.
            di.itemState = ODS_FLAGS(ODS_DISABLED.0);
            let _ = frame(&di, &look);
            assert_eq!(centre(dc).0, cold_rgb, "disabled is not hot");

            // Leaving the button takes it back to the resting face.
            di.itemState = ODS_FLAGS(0);
            SendMessageW(btn, WM_MOUSELEAVE, None, None);
            let _ = frame(&di, &look);
            assert_eq!(centre(dc).0, cold_rgb, "left the button");

            SelectObject(dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteObject(HGDIOBJ(face.0));
            let _ = DeleteObject(HGDIOBJ(hot.0));
            let _ = DeleteObject(HGDIOBJ(press.0));
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            let _ = DestroyWindow(btn);
            let _ = DestroyWindow(owner);
        }
    }

    /// The buffer is the text and nothing else. `DrawTextW` in `windows` 0.62
    /// takes `&mut [u16]` and passes `slice.len()` as `cchText`, so one
    /// trailing NUL is drawn as one more character: measured 「添加(A)」 52 px
    /// bare against 59 px with the NUL, which centres the label 3 px left.
    #[test]
    fn the_draw_text_buffer_is_exactly_the_text() {
        for s in ["", "a", "添加(A)", "&\u{0}b"] {
            assert_eq!(
                text_buf(s).len(),
                s.encode_utf16().count(),
                "buffer length for {s:?}"
            );
        }
        // A surrogate pair is two code units, so the count is UTF-16 units and
        // not `chars()`.
        assert_eq!(text_buf("😀").len(), 2);
        assert_eq!("😀".chars().count(), 1);
        // The last unit is the last character, never a terminator.
        assert_eq!(text_buf("ab").last(), Some(&u16::from(b'b')));
        assert!(text_buf("").is_empty());
    }

    /// No `DrawTextW` may be handed a NUL-terminated buffer.
    ///
    /// This trap has come back twice — once when this module was extracted and
    /// once in 自定义标记's tick glyph — and both times it compiled, ran, and
    /// showed up only as text sitting a few pixels off centre. So it is pinned
    /// over the source instead of by remembering.
    ///
    /// `SetWindowTextW` / `CreateWindowExW` / `MessageBoxW` **do** want the NUL,
    /// and there are ~100 `chain([0])` sites that feed those; this only looks
    /// at the buffers that reach `DrawTextW`.
    #[test]
    fn no_draw_text_buffer_is_nul_terminated() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/win32");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("src/win32") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("source file");
            let mut lines: Vec<&str> = src.lines().collect();
            // Tests do not paint; stop at the test module so this scan cannot
            // trip over its own search string. Not `#[cfg(test)]`: that also
            // marks test-only helpers in the middle of a file (`outputwin.rs`
            // has one at line 460, well before the painting).
            if let Some(at) = lines.iter().position(|l| l.trim() == "mod tests {") {
                lines.truncate(at);
            }
            for (i, line) in lines.iter().enumerate() {
                // Doc comments discuss `DrawTextW` without calling it, and a
                // quoted occurrence is somebody's search string.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let Some(col) = line.find("DrawTextW(") else {
                    continue;
                };
                if line[..col].trim_end().ends_with(['"', '\'']) {
                    continue;
                }
                let Some(arg) = args_of(&call_args(&lines, i, col)).get(1).cloned() else {
                    panic!("{}:{}: DrawTextW with no second argument", path.display(), i + 1);
                };
                let where_ = format!("{}:{}", path.display(), i + 1);
                checked += 1;
                assert!(
                    !arg.contains("chain([0") && !arg.contains("push(0"),
                    "{where_}: the buffer itself appends a NUL: {arg}"
                );
                // `&mut buf` — follow it back to its `let`, and to any
                // `buf.push(0)` between there and the call.
                let name: String = arg
                    .trim_start_matches("&mut ")
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if name.is_empty() || arg.contains('[') {
                    // `&mut label[..label_len]` is the *trimming* idiom: a
                    // NUL-terminated buffer with the terminator sliced off.
                    continue;
                }
                let Some((at, stmt)) = binding(&lines, i, &name) else {
                    continue;
                };
                assert!(
                    !stmt.contains("chain([0") && !stmt.contains("push(0)"),
                    "{where_}: `{name}` is bound with a NUL at line {}: {stmt}",
                    at + 1
                );
                for (k, between) in lines.iter().enumerate().take(i).skip(at) {
                    assert!(
                        !between.contains(&format!("{name}.push(0")),
                        "{where_}: `{name}` gets a NUL pushed at line {}",
                        k + 1
                    );
                }
            }
        }
        // If the scan ever stops finding the calls it would pass vacuously.
        // 16 today: complete 3, tabstrip 6, gotoany 2, menu 2, grepdlg 1,
        // outputwin 1, and this module 1.
        assert!(checked >= 16, "only found {checked} DrawTextW calls");
    }

    /// A call's argument list from the `(` at `col`, up to its matching `)`.
    fn call_args(lines: &[&str], line: usize, col: usize) -> String {
        let mut out = String::new();
        let mut depth = 0i32;
        for (k, l) in lines.iter().enumerate().skip(line) {
            let from = if k == line { col } else { 0 };
            for ch in l[from..].chars() {
                match ch {
                    '(' => {
                        depth += 1;
                        if depth == 1 {
                            continue;
                        }
                    }
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return out;
                        }
                    }
                    _ => {}
                }
                if depth >= 1 {
                    out.push(ch);
                }
            }
            // No newline before anything has been collected: a call that puts
            // its first argument on the next line would otherwise start with an
            // empty argument and shift every index by one.
            if !out.is_empty() {
                out.push('\n');
            }
        }
        out
    }

    /// The arguments of a call, split on the commas that are not inside
    /// brackets — `Some(f(a, b))` stays one argument. Empty pieces are dropped
    /// so a stray newline cannot shift the indices.
    fn args_of(args: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut depth = 0i32;
        for ch in args.chars() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    if !cur.trim().is_empty() {
                        out.push(cur.trim().to_string());
                    }
                    cur.clear();
                    continue;
                }
                _ => {}
            }
            cur.push(ch);
        }
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
        out
    }

    /// The statement that binds `name`, found by walking back from the call,
    /// as one string plus the line it starts on.
    fn binding(lines: &[&str], call: usize, name: &str) -> Option<(usize, String)> {
        for k in (0..call).rev() {
            let Some(rest) = lines[k].split("let ").nth(1) else {
                continue;
            };
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            if !rest.starts_with(name) {
                continue;
            }
            // `buf2` must not match `buf`.
            if rest[name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let mut stmt = String::new();
            for l in lines.iter().skip(k) {
                stmt.push_str(l);
                stmt.push('\n');
                if l.trim_end().ends_with(';') {
                    break;
                }
            }
            return Some((k, stmt));
        }
        None
    }
}
