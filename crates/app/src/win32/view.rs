//! The editor area as its own child window. Why a window and not another pass over the
//! shared [`dg_render::DrawList`]: the shell has to say which pixels belong to the
//! text. As long as the editor is a region of the frame's client, the frame repaints
//! the whole window for a caret move, a popup has to be painted before or after the
//! shell by hand, and anything drawn over the text is an ordering agreement between
//! functions rather than a property of the window tree. Given a `WS_CHILD` whose client
//! rect *is* the dock's editor rect, `WS_CLIPCHILDREN` on the frame answers the overlap
//! question the way Windows documents it: the frame cannot paint into the text at all,
//! and the text's window cannot paint into the docks. Coordinate contract, deliberately
//! the boring one: the view's client space is the frame's client space translated by
//! the editor origin. Input that arrives here is therefore re-packed into frame
//! coordinates and handed to the same [`input_arms`](crate::win32::window) dispatch the
//! frame uses, so `hit_test`, `caret_client_pos` and the overlay bars' strips keep
//! working in one space. Painting is the part that changes: the editor's draw commands
//! are no longer shifted by the dock offset, because its window already sits at the
//! offset. Focus lives here: the window owns the system caret.
//! `WM_SETFOCUS`/`WM_KILLFOCUS` build and destroy the caret. The keyboard messages that
//! arrive with that focus are re-targeted to the frame before reaching
//! [`input_arms`](crate::win32::window), because the scrollbars, the popups and the
//! invalidations still live there.

// The message loop below is one long translation from a native message into a
// model event, the same elision the frame's and the bar's handlers use.
#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::Once;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, GWLP_USERDATA,
    GetClientRect, GetWindowLongPtrW, KillTimer, MA_ACTIVATE, RegisterClassW, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE, WM_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION, WM_KILLFOCUS, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN,
    WM_SETCURSOR,
    WM_SETFOCUS, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_CHILD,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::win32::window::{EditorState, is_view_cursor_message, shift_lparam, state_mut};
use dg_render::Rect;

/// Registered once, like the overlay bars. The name is part of the harness
/// contract: `tools/uicheck` enumerates the child tree by class, so the window
/// tree is visible from outside the process instead of asserted from inside it.
const VIEW_CLASS: PCWSTR = w!("dogedit_editor_view");

static REGISTER_CLASS: Once = Once::new();

pub(crate) fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

/// The editor window: the frame HWND, its own paint message loop, and the rect
/// it occupies in the frame's client space.
pub(crate) struct EditorView {
    hwnd: HWND,
    /// Last rect handed to `SetWindowPos`, so a place call that changes
    /// nothing costs a comparison rather than a window message.
    frame: Rect,
}

impl EditorView {
    pub(crate) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    pub(crate) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Creates the child. It must be created **before** the overlay bars:
    /// children are placed above their siblings when they are created, and the
    /// bars have to stay on top of the text they float over.
    pub(crate) unsafe fn create(&mut self, parent: HWND, theme: &dg_render::Theme) {
        REGISTER_CLASS.call_once(|| unsafe {
            let wc = WNDCLASSW {
                // Both redraw flags: the editor's layout follows the width it is given,
                // so a resize is a full repaint, not a blit of stretched rows. Without
                // this style Windows never delivers the double-click at all.
                style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
                lpfnWndProc: Some(view_proc),
                hInstance: instance(),
                lpszClassName: VIEW_CLASS,
                // The theme's editor background, not the default white: this
                // window is shown before it is painted (the session restore
                // runs ahead of the first `WM_PAINT`), and the class brush is
                // what that first surface is filled with. `paint_view` still
                // covers every pixel of it.
                hbrBackground: crate::win32::classbg::editor_brush(theme),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        });
        if self.alive() {
            return;
        }
        if let Ok(hwnd) = CreateWindowExW(
            // No extended styles: this window is not translucent, and
            // `WS_EX_LAYERED` is what makes the overlay strip above it a
            // compositing question rather than a blending one.
            WINDOW_EX_STYLE::default(),
            VIEW_CLASS,
            w!("editor"),
            // WS_VISIBLE, unlike the overlay bars: a bar hides itself when it
            // fades, but a hidden editor window is a blank document — the
            // frame cannot paint these pixels any more.
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(instance()),
            None,
        ) {
            // The handler reaches the editor state through the frame window,
            // exactly as the bars' handler does.
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, parent.0 as isize);
            self.hwnd = hwnd;
        }
    }

    /// Moves and resizes the window to `frame` (frame-client coordinates).
    /// Returns whether anything moved, which is when the whole client needs a
    /// fresh paint.
    pub(crate) unsafe fn place(&mut self, frame: Rect) -> bool {
        if !self.alive() {
            return false;
        }
        if self.frame == frame {
            return false;
        }
        self.frame = frame;
        let _ = SetWindowPos(
            self.hwnd,
            None,
            frame.x.round() as i32,
            frame.y.round() as i32,
            frame.w.round() as i32,
            frame.h.round() as i32,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        true
    }

    /// The window's origin in the frame's client space: what to add to a point
    /// this window received in order to read it in frame space, and what to
    /// subtract from a frame-space rect in order to paint it here.
    pub(crate) fn origin(&self) -> (i32, i32) {
        (self.frame.x.round() as i32, self.frame.y.round() as i32)
    }

    /// Asks the window itself how big it is. `place` was given a request, and
    /// rounding, a border or a creation that has not been laid out yet can make
    /// the answer differ; painting from the request would leave a strip of the
    /// previous frame on screen.
    pub(crate) fn client(&self) -> (i32, i32) {
        if !self.alive() {
            return (0, 0);
        }
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    pub(crate) unsafe fn invalidate(&self) {
        if self.alive() {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
}

/// `WS_EX_LAYERED`'s absence is the point of this window: the text is opaque,
/// and the only translucency in the tree is the overlay strip above it.

/// The view's message loop. Two kinds of message live here: paint, which is
/// this window's own business, and input, which belongs to the frame's dispatch
/// and is only *translated* here — coordinates into frame space, the frame's
/// HWND as the target, so a click on the text behaves exactly as it did when
/// the text was a region of the frame.
unsafe extern "system" fn view_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(st) = state_of(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    // 分割把同一个类建成两个子窗口：认得出是哪只就照常走，认不出
    // （这个句柄不是窗格窗口）交回默认处理。
    let Some(pane) = st.pane_of_window(hwnd) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let frame = st.hwnd;
    match msg {
        WM_PAINT => {
            st.paint_view(hwnd);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // The splitter's grab band is the text window's last row, so the
            // pointer that wants the resize beam is standing on this window:
            // the frame's answer has to be given here too.
            if st.show_split_cursor(frame) {
                LRESULT(1)
            } else if is_view_cursor_message(hwnd, wparam, lparam) {
                // The command router's WM_SETCURSOR arm: the shape for the
                // pointer's own spot goes onto the screen here and nowhere
                // else. A message that is not the view's own — the frame's
                // cursor, a hit on a scrollbar or on the non-client edge — is
                // handed back to `DefWindowProcW`, which is what lets the class
                // cursor stay out of the editor's way without the editor having
                // to fight it.
                st.apply_cursor_at_pointer();
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEACTIVATE => {
            // The command router's WM_MOUSEACTIVATE arm: `SetFocus(the view)` and answer
            // `MA_ACTIVATE`. The click has to land on the window that owns the
            // caret, because the system caret belongs to the focus window — and
            // naming the window explicitly is what keeps a click on the text
            // from activating whatever Windows would have picked instead.
            // 分割后这一下还把被点的窗格变成活动的：键盘、滚动条和页签
            // 条都跟着它走。
            st.activate_pane(frame, pane);
            let _ = unsafe { SetFocus(Some(hwnd)) };
            LRESULT(MA_ACTIVATE as isize)
        }
        WM_SETFOCUS => {
            st.activate_pane(frame, pane);
            st.on_set_focus();
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            st.on_kill_focus();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == crate::win32::window::DX_CARET_TIMER_ID => {
            // The DX caret's blink: only this window ever owns timers, so the
            // id names the one state flip.
            st.on_dx_caret_blink();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == crate::win32::window::DX_RETRY_TIMER_ID => {
            let _ = KillTimer(Some(hwnd), crate::win32::window::DX_RETRY_TIMER_ID);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == crate::win32::window::CARET_PULSE_TIMER_ID => {
            // 击键脉冲 ended: hand the layer's opacity back to the blink. The
            // timer is one-shot by construction — kill it before doing
            // anything else so a slow frame cannot stack another.
            let _ = KillTimer(Some(hwnd), crate::win32::window::CARET_PULSE_TIMER_ID);
            st.caret_pulse_done();
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR
        | WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION | WM_IME_ENDCOMPOSITION => {
            // The keyboard belongs to this window now, but the dispatch does
            // not: the frame is still the window the scrollbars, the popups and
            // the invalidation live on, so the message is re-targeted to it.
            crate::win32::window::input_arms(st, frame, msg, wparam, lparam)
                .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_MOUSEMOVE => {
            let (ox, oy) = st.pane_origin(pane);
            crate::win32::window::input_arms(st, frame, msg, wparam, shift_lparam(lparam, ox, oy))
                .unwrap_or(LRESULT(0))
        }
        // The leave request the document map arms is asked of this window —
        // the one the pointer is over — so it has to reach the frame's
        // dispatch like every other input message does.
        WM_MOUSELEAVE => crate::win32::window::input_arms(st, frame, msg, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam)),
        WM_MOUSEWHEEL => {
            // lParam is screen coordinates here — the one message that must
            // reach the dispatch untouched.
            crate::win32::window::input_arms(st, frame, msg, wparam, lparam).unwrap_or(LRESULT(0))
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The editor state of the frame window this view was created on.
unsafe fn state_of(hwnd: HWND) -> Option<&'static mut EditorState> {
    let root = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if root == 0 {
        return None;
    }
    state_mut(HWND(root as *mut core::ffi::c_void))
}
