//! The hex editor window shell.
//!
//! Each binary document gets its own child window; the platform-neutral state lives
//! in [`dg_hex::HexView`] and is parked and unparked as tabs switch.

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, ETO_CLIPPED, ETO_OPAQUE, ExtTextOutW, FillRect, GetDC, GetSysColor,
    GetTextMetricsW, InvalidateRect, ReleaseDC, SelectObject, SetBkColor, SetTextColor,
    SRCCOPY, TEXTMETRICW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCaret, CreateWindowExW, CS_DBLCLKS, DefWindowProcW, DestroyCaret, GetClientRect,
    GetWindowLongPtrW, HideCaret, RegisterClassW, SetCaretPos, SetWindowLongPtrW,
    ShowCaret, ShowWindow, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WM_CHAR,
    WM_ERASEBKGND, WM_GETDLGCODE, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE,
    WNDCLASSW, WS_CHILD, GWLP_USERDATA, HTCLIENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWHEELSCROLLLINES, SystemParametersInfoW,
};
use windows::core::{PCWSTR, w};

use dg_render::Rect;

const HEXVIEW_CLASS: PCWSTR = w!("HexView32");

static REGISTER_CLASS: Once = Once::new();

pub(crate) fn instance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| windows::Win32::Foundation::HINSTANCE(m.0))
            .unwrap_or(windows::Win32::Foundation::HINSTANCE(std::ptr::null_mut()))
    }
}

/// The mouse drag: the byte the press landed on, held while the button is
/// down so `WM_MOUSEMOVE` keeps `[anchor_a, caret]` stretched to it.
struct Drag {
    anchor: u32,
}

/// Everything the window itself owns. The view is `None` while the frame
/// has no hex tab on screen (the window is hidden then, so nothing paints
/// or types into a stale file). `font` is the frame's editor font.
struct HexWinState {
    view: Option<dg_hex::HexView>,
    drag: Option<Drag>,
    /// `true` while `SetCapture` owns the pointer (the move handler extends the
    /// selection on every move only while the press is held).
    captured: bool,
    font: isize,
}

pub(crate) struct HexWindow {
    hwnd: HWND,
    /// Last rect handed to [`Self::place`], so an unchanged place call
    /// costs a comparison rather than window messages.
    frame: Rect,
}

impl HexWindow {
    pub(crate) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    pub(crate) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    #[allow(dead_code)] // the shell surface mirrors EditorView's
    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Registers `HexView32` and creates the child — the two steps
    /// `DllMain` + the host's `CreateWindowExW` perform. Created after the
    /// text editor view so it sits above it in z-order while shown.
    pub(crate) unsafe fn create(&mut self, parent: HWND) {
        REGISTER_CLASS.call_once(|| unsafe {
            let wc = WNDCLASSW {
                // DllMain: `CS_DBLCLKS` — a double-click selects a byte.
                style: CS_DBLCLKS,
                lpfnWndProc: Some(hexview_proc),
                hInstance: instance(),
                lpszClassName: HEXVIEW_CLASS,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        });
        if self.alive() {
            return;
        }
        if let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            HEXVIEW_CLASS,
            w!("hexview"),
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(instance()),
            None,
        ) {
            self.hwnd = hwnd;
        }
    }

    pub(crate) unsafe fn place(&mut self, frame: Rect) -> bool {
        if !self.alive() {
            return false;
        }
        if self.frame == frame {
            return false;
        }
        self.frame = frame;
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
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

    pub(crate) unsafe fn show(&self, on: bool) {
        if self.alive() {
            let _ = ShowWindow(self.hwnd, if on { SW_SHOW } else { SW_HIDE });
        }
    }

    /// WM_SETFONT: the frame's editor font, with the metrics measured
    /// from it. The view's layout depends on both, so a font
    /// change re-runs the whole fit.
    pub(crate) unsafe fn set_font(&self, hfont: isize) {
        if !self.alive() {
            return;
        }
        if let Some(st) = state_of(self.hwnd) {
            st.font = hfont;
            if let Some(view) = st.view.as_mut() {
                measure(self.hwnd, st.font, view);
                let (cx, cy) = self.client();
                view.on_size(cx, cy);
            }
        }
        notify_frame_scroll(self.hwnd);
        let _ = InvalidateRect(Some(self.hwnd), None, false);
    }

    // ---- the host messages ------------------------------------------------

    /// Parks a document's view state in the window and repaints — the
    /// data half of the tab-switch handoff. The window itself stays a
    /// dumb surface.
    pub(crate) fn attach(&mut self, view: dg_hex::HexView) {
        if !self.alive() {
            return;
        }
        unsafe {
            let (cx, cy) = self.client();
            if let Some(st) = state_of(self.hwnd) {
                st.view = Some(view);
                st.drag = None;
                st.captured = false;
                if let Some(v) = st.view.as_mut() {
                    measure(self.hwnd, st.font, v);
                    v.on_size(cx, cy);
                }
            }
            notify_frame_scroll(self.hwnd);
            self.show(true);
            self.invalidate();
        }
    }

    /// Takes the view state back out (tab switched away, document closed).
    pub(crate) fn detach(&mut self) -> Option<dg_hex::HexView> {
        if !self.alive() {
            return None;
        }
        let taken = unsafe { state_of(self.hwnd) }.and_then(|st| st.view.take());
        unsafe {
            self.show(false);
        }
        taken
    }

    #[allow(dead_code)] // part of the shell surface; the frame polls the doc instead
    pub(crate) fn has_view(&self) -> bool {
        self.alive() && unsafe { state_of(self.hwnd) }.map_or(false, |st| st.view.is_some())
    }

    /// Runs `f` against the parked view — the read/write surface the
    /// frame's commands (save, undo/redo, title/modified polling) use.
    pub(crate) fn with_view<R>(&self, f: impl FnOnce(&mut dg_hex::HexView) -> R) -> Option<R> {
        if !self.alive() {
            return None;
        }
        let st = unsafe { state_of(self.hwnd) }?;
        let view = st.view.as_mut()?;
        Some(f(view))
    }

    /// Re-creates the system caret after the edit mode changed its width
    /// (the status strip's 插入/覆写 pane). Only a focused window owns one.
    pub(crate) fn refresh_caret(&self) {
        if !self.alive() {
            return;
        }
        unsafe {
            if GetFocus() != self.hwnd {
                return;
            }
            if let Some(st) = state_of(self.hwnd) {
                if let Some(view) = st.view.as_ref() {
                    let w = view.caret_width();
                    let h = view.line_height.max(1);
                    let _ = DestroyCaret();
                    let _ = CreateCaret(self.hwnd, None, w, h);
                    update_caret(view);
                    let _ = ShowCaret(Some(self.hwnd));
                }
            }
        }
    }

    /// The frame's overlay scrollbar strips drive this: clamp and store
    /// both scroll offsets (the V bar's row and the H bar's character
    /// column), keep the system caret on the caret byte, repaint.
    pub(crate) fn set_scroll(&self, row: i32, col: i32) {
        if !self.alive() {
            return;
        }
        unsafe {
            if let Some(st) = state_of(self.hwnd) {
                if let Some(view) = st.view.as_mut() {
                    view.top_row = row.clamp(0, max_top_row(view));
                    view.hscroll =
                        col.clamp(0, (view.chars_per_row - view.visible_cols).max(0));
                    update_caret(view);
                }
            }
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
}

/// Measures `view`'s line height and character width from `font`.
unsafe fn measure(hwnd: HWND, font: isize, view: &mut dg_hex::HexView) {
    if font == 0 {
        return;
    }
    let hdc = GetDC(Some(hwnd));
    let old = SelectObject(hdc, windows::Win32::Graphics::Gdi::HGDIOBJ(font as *mut _));
    let mut tm = TEXTMETRICW::default();
    if GetTextMetricsW(hdc, &mut tm).as_bool() {
        // The line height is tmHeight plus the external leading; the cell width
        // is tmAveCharWidth.
        view.line_height = tm.tmHeight + tm.tmExternalLeading;
        view.char_width = tm.tmAveCharWidth;
    }
    SelectObject(hdc, old);
    let _ = ReleaseDC(Some(hwnd), hdc);
}

/// The `GWLP_USERDATA` slot holds the `Box<HexWinState>`
/// `HexView` object hangs off the window the same way, made once at
/// `WM_NCCREATE`.
unsafe fn state_of(hwnd: HWND) -> Option<&'static mut HexWinState> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if ptr == 0 {
        return None;
    }
    Some(&mut *(ptr as *mut HexWinState))
}

/// The view window's dispatch. The message ids in the comments name
/// what each arm answers.
unsafe extern "system" fn hexview_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // WM_CREATE (0x01): the state object is born with the window. The
        // document state is attached explicitly afterwards.
        0x01 => {
            let st = Box::new(HexWinState {
                view: None,
                drag: None,
                captured: false,
                font: 0,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);
            // the hex engine imports no IMM32 and the host types raw keys at the
            // view: a live IME turns every digit into VK_PROCESSKEY (0xE5)
            // composition instead of a WM_CHAR, so the view carries no IME
            // context at all.
            let _ = windows::Win32::UI::Input::Ime::ImmAssociateContext(
                hwnd,
                windows::Win32::UI::Input::Ime::HIMC::default(),
            );
            LRESULT(0)
        }
        0x02 => {
            // WM_DESTROY: drop the state.
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                drop(Box::from_raw(ptr as *mut HexWinState));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        // WM_SIZE: refit the view to the new client.
        WM_SIZE => {
            if let Some(st) = state_of(hwnd) {
                if st.view.is_some() {
                    let cx = (lparam.0 & 0xffff) as u16 as i32;
                    let cy = ((lparam.0 >> 16) & 0xffff) as u16 as i32;
                    if let Some(view) = st.view.as_mut() {
                        view.on_size(cx, cy);
                    }
                }
            }
            notify_frame_scroll(hwnd);
            LRESULT(0)
        }
        // WM_SETFOCUS: create the caret (w = char_width in
        // overwrite mode, else the 2 px bar; h = line height) and show it.
        WM_SETFOCUS => {
            if let Some(st) = state_of(hwnd) {
                if let Some(view) = st.view.as_mut() {
                    let w = view.caret_width();
                    let h = view.line_height.max(1);
                    let _ = CreateCaret(hwnd, None, w, h);
                    update_caret(view);
                    let _ = ShowCaret(Some(hwnd));
                }
            }
            LRESULT(0)
        }
        // WM_KILLFOCUS: HideCaret + DestroyCaret + InvalidateRect.
        WM_KILLFOCUS => {
            let _ = HideCaret(Some(hwnd));
            let _ = DestroyCaret();
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        // WM_PAINT: the double-buffered sweep below.
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        // WM_ERASEBKGND: return 1 (the paint fills everything).
        WM_ERASEBKGND => LRESULT(1),
        // WM_SETCURSOR: over the client the answer is 1 —
        // the class carries no cursor of its own and the view keeps
        // whatever the system last set; everywhere else DefWindowProc
        // runs the parent chain.
        WM_SETCURSOR => {
            if lparam.0 as usize & 0xffff == HTCLIENT as usize {
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        // WM_GETDLGCODE answers 0x81.
        WM_GETDLGCODE => LRESULT(0x81),
        // WM_CHAR: the character edit path.
        WM_CHAR => {
            if let Some(st) = state_of(hwnd) {
                if let Some(view) = st.view.as_mut() {
                    let ch = wparam.0 as u32;
                    // the ASCII column entry The ASCII column's handler takes printable
                    // characters while the caret sits in the ASCII area.
                    let r = if ch >= 0x20 && view.column == 1 {
                        view.on_ascii_char(ch)
                    } else if ch >= 0x20 {
                        view.on_char(ch)
                    } else {
                        return LRESULT(0);
                    };
                    match r {
                        dg_hex::view::CharResult::Rejected => {
                            let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
                                windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x40),
                            );
                        }
                        dg_hex::view::CharResult::ReadOnly => {}
                        dg_hex::view::CharResult::Committed
                        | dg_hex::view::CharResult::Updated => {
                            // The first byte turns an empty pane into a
                            // one-row one: the content-row fit (visible rows,
                            // scrollbar page) is read from the length, so it
                            // re-runs with the new one.
                            let mut rc = RECT::default();
                            let _ = GetClientRect(hwnd, &mut rc);
                            view.on_size(rc.right - rc.left, rc.bottom - rc.top);
                            view.scroll_to_caret();
                            view.anchor_a = view.caret;
                            view.anchor_b = view.caret;
                            update_caret(view);
                        }
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            notify_frame_scroll(hwnd);
            LRESULT(0)
        }
        // Movement keys: the hex engine itself hands `WM_KEYDOWN` to
        // DefWindowProc — the host exe decides what arrows mean. The frame
        // is that host here.
        WM_KEYDOWN => {
            let handled = state_of(hwnd).map_or(false, |st| frame_key(hwnd, st, wparam.0 as u32));
            if handled {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        // WM_HSCROLL/WM_VSCROLL: the native bars do not
        // exist on this window (the frame's overlay strips are the
        // scrollbars), so a scroll arrives only from the frame driving
        // `set_scroll` — nothing to answer here.
        // WM_MOUSEMOVE: while captured, the caret and selection follow.
        WM_MOUSEMOVE => {
            if let Some(st) = state_of(hwnd) {
                if st.captured {
                    let x = (lparam.0 & 0xffff) as u16 as i32;
                    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i32;
                    if let Some(view) = st.view.as_mut() {
                        if let Some(byte) = hit_test(view, x, y) {
                            view.caret = byte;
                            if let Some(d) = st.drag.as_ref() {
                                view.anchor_a = d.anchor;
                                view.anchor_b = view.caret;
                            }
                            view.scroll_to_caret();
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
            }
            notify_frame_scroll(hwnd);
            LRESULT(0)
        }
        // WM_LBUTTONDOWN: hit-test, place the caret, start
        // the drag, take capture. The focus move (the
        // WM_MOUSEACTIVATE arm does the same on the activation path) is
        // what brings the caret up.
        WM_LBUTTONDOWN => {
            let _ = SetFocus(Some(hwnd));
            if let Some(st) = state_of(hwnd) {
                let x = (lparam.0 & 0xffff) as u16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as u16 as i32;
                if let Some(view) = st.view.as_mut() {
                    if let Some(byte) = hit_test(view, x, y) {
                        view.caret = byte;
                        st.drag = Some(Drag { anchor: byte });
                        view.anchor_a = byte;
                        view.anchor_b = byte;
                        view.scroll_to_caret();
                        let _ = SetCapture(hwnd);
                        st.captured = true;
                        update_caret(view);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            }
            notify_frame_scroll(hwnd);
            LRESULT(0)
        }
        // WM_LBUTTONUP: release capture, keep the range.
        WM_LBUTTONUP => {
            if let Some(st) = state_of(hwnd) {
                if st.captured {
                    let _ = ReleaseCapture();
                    st.captured = false;
                }
                st.drag = None;
            }
            LRESULT(0)
        }
        // WM_LBUTTONDBLCLK: a double-click selects the byte
        // under it.
        WM_LBUTTONDBLCLK => {
            if let Some(st) = state_of(hwnd) {
                let x = (lparam.0 & 0xffff) as u16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as u16 as i32;
                if let Some(view) = st.view.as_mut() {
                    if let Some(byte) = hit_test(view, x, y) {
                        view.caret = byte;
                        view.anchor_a = byte;
                        view.anchor_b = byte + 1;
                        update_caret(view);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            }
            LRESULT(0)
        }
        // WM_MOUSEWHEEL: `SPI_GETWHEELSCROLLLINES` rows per
        // notch, 3 when the system says 0/1/paged.
        WM_MOUSEWHEEL => {
            if let Some(st) = state_of(hwnd) {
                if let Some(view) = st.view.as_mut() {
                    let delta = ((wparam.0 >> 16) as u16) as i16 as i32;
                    view.top_row -= delta / 120 * wheel_lines();
                    view.top_row = view.top_row.clamp(0, max_top_row(view));
                }
            }
            notify_frame_scroll(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The host half of the keyboard: the arrow/page/home/end keys
/// the hex engine left to the host. Shift extends the selection (the
/// anchor pair), a plain move collapses it onto the
/// caret. Returns whether the key was one of ours.
fn frame_key(hwnd: HWND, st: &mut HexWinState, vk: u32) -> bool {
    let Some(view) = st.view.as_mut() else { return false };
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 };
    let moved = |view: &mut dg_hex::HexView, delta: i64| {
        view.move_caret(delta);
        if !shift {
            view.anchor_a = view.caret;
            view.anchor_b = view.caret;
        }
    };
    match vk {
        // VK_LEFT / VK_RIGHT
        0x25 => moved(view, -1),
        0x27 => moved(view, 1),
        // VK_UP / VK_DOWN
        0x26 => moved(view, -(view.bytes_per_row as i64)),
        0x28 => moved(view, view.bytes_per_row as i64),
        // VK_PRIOR / VK_NEXT
        0x21 => moved(view, -(view.page_rows() as i64) * view.bytes_per_row as i64),
        0x22 => moved(view, view.page_rows() as i64 * view.bytes_per_row as i64),
        // VK_HOME / VK_END
        0x24 => {
            view.move_home();
            if !shift {
                view.anchor_a = view.caret;
                view.anchor_b = view.caret;
            }
        }
        0x23 => {
            view.move_end();
            if !shift {
                view.anchor_a = view.caret;
                view.anchor_b = view.caret;
            }
        }
        _ => return false,
    }
    view.scroll_to_caret();
    unsafe {
        update_caret(view);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    true
}

/// Tells the frame the hex view's scroll state moved: the custom
/// scrollbar strips (`OverlayBars`) live on the frame and read the view
/// back through `update_scrollbars`. Called with no view borrow held —
/// the frame re-enters this window's state.
fn notify_frame_scroll(hwnd: HWND) {
    let frame = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default()
    };
    if let Some(st) = crate::win32::window::state_mut(frame) {
        st.update_scrollbars(frame);
        // The strip's 选区/偏移 pane reads the same view state, and the caret
        // moves without any edit the frame would otherwise repaint for.
        st.refresh_status_bar();
    }
}

/// The wheel step: the system's wheel-rows setting, 3 when it says 0, 1 or
/// page-at-a-time.
fn wheel_lines() -> i32 {
    let mut lines: u32 = 3;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some(&mut lines as *mut u32 as *mut _),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    if lines == 0 || lines == u32::MAX || lines == 1 {
        3
    } else {
        lines as i32
    }
}

/// The largest `top_row`: the last windowful's first row — the wheel's descent
/// is clamped to the rows on screen the same way.
fn max_top_row(view: &dg_hex::HexView) -> i32 {
    (view.total_rows() as i32 - view.visible_rows.max(0)).max(0)
}

/// UpdateCaret: `SetCaretPos` to the caret's pixel spot.
unsafe fn update_caret(view: &dg_hex::HexView) {
    let (x, y) = view.caret_pixel();
    if x >= 0 && y >= 0 {
        let _ = SetCaretPos(x, y);
    } else {
        // Off-screen: park the caret out of sight while the owner keeps it.
        let _ = SetCaretPos(-32000, -32000);
    }
}

/// OnPaint: visible rows, per-row `format_row`, per-run
/// `SetTextColor`/`SetBkColor` + `ExtTextOutW`, all into a memory DC that
/// reaches the screen as one `BitBlt` — the same buffer `paint_view` gives
/// the text, and what keeps a resize from tearing the grid.
unsafe fn paint(hwnd: HWND) {
    let Some(st) = state_of(hwnd) else { return };
    let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.0.is_null() {
        let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let cw_px = (rc.right - rc.left).max(0);
    let ch_px = (rc.bottom - rc.top).max(0);
    if cw_px == 0 || ch_px == 0 {
        let _ = EndPaint(hwnd, &ps);
        return;
    }
    // One fill + one blit per paint; everything below draws into `mem`.
    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, cw_px, ch_px);
    if mem.0.is_null() || bmp.0.is_null() {
        if !mem.0.is_null() {
            let _ = DeleteDC(mem);
        }
        let _ = EndPaint(hwnd, &ps);
        return;
    }
    let old_bmp = SelectObject(mem, windows::Win32::Graphics::Gdi::HGDIOBJ(bmp.0));
    let Some(view) = st.view.as_ref() else {
        fill(&mem, &rc, view_bg(None));
        blit(&hdc, &rc, &mem);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
        return;
    };
    if st.font != 0 {
        SelectObject(mem, windows::Win32::Graphics::Gdi::HGDIOBJ(st.font as *mut _));
    }
    let bpr = view.bytes_per_row.max(1) as u32;
    let lh = view.line_height.max(1);
    let cw = view.char_width.max(1);

    fill(&mem, &rc, view_bg(Some(view)));

    let first = view.top_row.max(0) as u32;
    let visible = (rc.bottom / lh + 1).max(0) as u32;
    let total = view.total_rows();
    if total > 0 {
        let last = (first + visible).min(total - 1);
        let mut row = first;
        while row <= last {
            let row_start = row * bpr;
            if row_start >= view.snap.len() {
                break;
            }
            let count = bpr.min(view.snap.len() - row_start) as usize;
            let mut bytes = vec![0u8; count];
            if view.snap.read(row_start, &mut bytes) != count {
                break;
            }
            let modified = view.modified_flags(row_start, count);
            let fr = view.format_row(row_start, &bytes, &modified);
            let wide: Vec<u16> = fr.text.iter().map(|c| *c as u16).collect();
            let y = ((row as i64 - view.top_row.max(0) as i64) as i32) * lh;
            // Walk the runs, merging neighbours with the same colours. This
            // implementation advances the same x by the monospace cell width, and
            // ETO_OPAQUE paints each run's background, which is how the address column
            // and the caret byte get theirs.
            let mut run_x = -view.hscroll * cw;
            let mut i = 0usize;
            while i < wide.len() {
                let (fg, bg) = fr.runs[i];
                let mut j = i + 1;
                while j < wide.len() && fr.runs[j] == (fg, bg) {
                    j += 1;
                }
                let run_w = (j - i) as i32 * cw;
                let _ = SetTextColor(mem, COLORREF(color(fg)));
                let _ = SetBkColor(mem, COLORREF(color(bg)));
                let mut clip = RECT {
                    left: run_x,
                    top: y,
                    right: run_x + run_w,
                    bottom: y + lh,
                };
                let _ = ExtTextOutW(
                    mem,
                    run_x,
                    y,
                    ETO_CLIPPED | ETO_OPAQUE,
                    Some(&mut clip),
                    windows::core::PCWSTR::from_raw(wide.as_ptr().add(i)),
                    (j - i) as u32,
                    None,
                );
                run_x += run_w;
                i = j;
            }
            row += 1;
        }
    }
    if view.flags & dg_hex::view::F_SEPARATORS != 0 {
        // The two rules stand in the column gaps for the pane's whole
        // height, past the last row as well; the row backgrounds go down
        // first so the 1 px line is never covered.
        let (x1, x2) = view.separator_pixels();
        let rule = view.colors.table[19];
        for x in [x1, x2] {
            if (0..cw_px).contains(&x) {
                let line = RECT {
                    left: x,
                    top: 0,
                    right: x + 1,
                    bottom: ch_px,
                };
                fill(&mem, &line, rule);
            }
        }
    }
    blit(&hdc, &rc, &mem);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(bmp.0));
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

/// The one transfer to the screen.
unsafe fn blit(hdc: &windows::Win32::Graphics::Gdi::HDC, rc: &RECT, mem: &windows::Win32::Graphics::Gdi::HDC) {
    let _ = BitBlt(
        *hdc,
        0,
        0,
        rc.right.max(0),
        rc.bottom.max(0),
        Some(*mem),
        0,
        0,
        SRCCOPY,
    );
}

/// The window background: the colour table's entry 0 — the default background —
/// resolved through `GetSysColor` when it names one.
fn view_bg(view: Option<&dg_hex::HexView>) -> u32 {
    view.map_or(0x00ff_ffff, |v| v.colors.table[0])
}

/// `0x8000_0000 | index` entries resolve through `GetSysColor`; plain
/// values are COLORREFs (0x00bbggrr — GDI's order, as we store
/// them).
fn color(c: u32) -> u32 {
    if c & 0xff00_0000 != 0 {
        (unsafe { GetSysColor(windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX((c & 0xffff) as i32)) }) as u32
    } else {
        c
    }
}

fn fill(hdc: &windows::Win32::Graphics::Gdi::HDC, rc: &RECT, rgb: u32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color(rgb)));
        let _ = FillRect(*hdc, rc, brush);
        let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(brush.0));
    }
}

/// The hit test both columns perform: a point in client space to a byte
/// offset, deciding which column (hex or ASCII) the x fell into. The
/// column boundaries are `sep_addr` after the address, then `hex_cols`,
/// then `sep_ascii`, then `bpr` bytes — all in characters.
fn hit_test(view: &mut dg_hex::HexView, x: i32, y: i32) -> Option<u32> {
    if view.bytes_per_row <= 0 {
        return None;
    }
    let bpr = view.bytes_per_row as u32;
    let cw = view.char_width.max(1);
    let lh = view.line_height.max(1);
    let col = x / cw + view.hscroll;
    let total_rows = view.total_rows() as i32;
    // An empty snapshot has no rows to aim at, but byte 0 is still the one
    // caret position it has; a click anywhere lands there.
    let row = if total_rows == 0 {
        0
    } else {
        (view.top_row + y / lh).clamp(0, total_rows - 1)
    };
    let row_start = (row as u32).saturating_mul(bpr);
    let len = view.snap.len();
    let row_bytes = bpr.min(len.saturating_sub(row_start));
    let clamp = |b: u32| {
        if row_bytes == 0 {
            len
        } else {
            row_start + b.min(row_bytes - 1)
        }
    };
    let addr_end = view.address_field()
        + if view.flags & dg_hex::view::F_HIDE_HEX != 0 {
            0
        } else {
            view.sep_addr
        };
    let hex_end = if view.flags & dg_hex::view::F_HIDE_HEX != 0 {
        addr_end
    } else {
        addr_end + view.hex_cols
    };
    let ascii_start = hex_end
        + if view.flags & dg_hex::view::F_HIDE_ASCII != 0 {
            0
        } else {
            view.sep_ascii
        };
    if view.flags & dg_hex::view::F_HIDE_HEX == 0 && col >= addr_end && col < hex_end {
        // The hex column: full group cells of digits * group + 1 characters,
        // the byte inside the cell from the digits before the cell's
        // separating space (the hit test's group arithmetic).
        let digits = view.digits().max(1) as i32;
        let group = view.group.max(1);
        let within = (col - addr_end).max(0);
        let cell = digits * group + 1;
        let g = within / cell;
        let inside = within % cell;
        let byte_in_group = (inside / digits).min(group - 1);
        view.column = 0;
        return Some(clamp((g * group + byte_in_group) as u32));
    }
    if view.flags & dg_hex::view::F_HIDE_ASCII == 0 && col >= ascii_start {
        view.column = 1;
        return Some(clamp((col - ascii_start) as u32));
    }
    // The address gutter or the separator between the columns: the closer
    // side wins (the hit test switches the column only on a real column).
    view.column = if col >= hex_end && view.flags & dg_hex::view::F_HIDE_ASCII == 0 {
        1
    } else {
        0
    };
    Some(clamp(((col - addr_end).max(0) / (view.digits().max(1) as i32)) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_view() -> dg_hex::HexView {
        let mut v = dg_hex::HexView::new();
        v.snap.set_data(&[]);
        v.on_size(800, 400);
        v
    }

    #[test]
    fn empty_view_hit_test_lands_on_byte_zero() {
        let mut v = empty_view();
        assert_eq!(hit_test(&mut v, 0, 0), Some(0));
        assert_eq!(hit_test(&mut v, 500, 300), Some(0));
        assert_eq!(v.snap.len(), 0);
    }

    #[test]
    fn empty_view_hit_test_keeps_the_column() {
        let mut v = empty_view();
        let ascii_x = (v.address_field() + v.sep_addr + v.hex_cols + v.sep_ascii) * v.char_width;
        assert_eq!(hit_test(&mut v, ascii_x, 0), Some(0));
        assert_eq!(v.column, 1);
        assert_eq!(hit_test(&mut v, 0, 0), Some(0));
        assert_eq!(v.column, 0);
    }
}
