//! Placement shared by the dialogs that build their own template: size
//! the client rect for the DPI, let the window manager add the non-client
//! frame, then centre the result over the owner frame's client area.
//!
//! The dialogs carry DPI in two different shapes — a `f32` ratio (with
//! `scale`) or a raw `i32` DPI (with `uifont::scaled`). Rather than pick a
//! winner, callers pass an already-scaled client size plus the raw DPI
//! that `AdjustWindowRectExForDpi` wants. The geometry itself is one copy.

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WINDOW_STYLE,
};

/// `client_w` / `client_h` are pixels; `dpi` is the raw DPI for the
/// non-client metrics; `style` is the dialog template's own style (some
/// dialogs add `WS_THICKFRAME`, which changes the frame size).
pub(super) unsafe fn size_and_place(
    dlg: HWND,
    frame: HWND,
    client_w: i32,
    client_h: i32,
    dpi: u32,
    style: u32,
) {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: client_w,
        bottom: client_h,
    };
    let _ = AdjustWindowRectExForDpi(
        &mut rc,
        WINDOW_STYLE(style),
        false,
        WINDOW_EX_STYLE::default(),
        dpi,
    );
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let mut client = RECT::default();
    let _ = GetClientRect(frame, &mut client);
    let mut pt = POINT {
        x: (client.right - w) / 2,
        y: (client.bottom - h) / 3,
    };
    let _ = ClientToScreen(frame, &mut pt);
    let _ = SetWindowPos(dlg, None, pt.x, pt.y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}
