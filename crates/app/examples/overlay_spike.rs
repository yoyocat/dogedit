//! M1b step 0 — the overlay-scrollbar spike.
//!
//! Option B (thin, self-drawn, theme-coloured, arrow-less overlay bar) is
//! already decided; what is *not* decided is whether "overlay" is possible at
//! all on this OS. Three unknowns, each of which has a fallback:
//!
//! 1. alpha: can a `WS_EX_LAYERED` + `LWA_ALPHA` **child** window composite
//! over the sibling content painted underneath it? The spike answers it by
//! sampling rather than trusting the API contract: the host paints alternating
//! red/white rows, the bar paints one flat
//! grey, and the verdict is whether the two sampled pixels come back
//! different from each other *and* equal to the arithmetic blend.
//! Fallback if this fails: opaque bar + inset of the text area.
//! 2. passthrough: does adding `WS_EX_TRANSPARENT` to the layered child make
//! hit-testing skip it **without recreating the window**? That is what
//! lets the faded-out bar stop eating clicks on the last text column.
//! `WindowFromPoint` is the router Windows itself uses, so it measures
//! what a real click would do.
//! 3. no focus steal: the system caret and the IME window live on the focused
//! window, so the bar must never take focus. `WM_MOUSEACTIVATE` ->
//! `MA_NOACTIVATE`, and a drag must hold capture while focus stays put.
//!
//! Verdicts go to the JSON path given as argv[1]; argv[2] keeps the windows up
//! that many milliseconds so `tools/uicheck/capture.ps1` can make the picture.
//!
//! Run: `cargo run --release --example overlay_spike -- out.json [hold_ms]`

// The verdict document is a deeply nested json!() literal; the default macro
// recursion budget runs out before it expands.
#![recursion_limit = "256"]

#[cfg(windows)]
mod spike {
    use serde_json::json;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows::Win32::Foundation::{
        COLORREF, GetLastError, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT,
        SetLastError, WIN32_ERROR, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, ETO_OPAQUE, EndPaint, ExtTextOutW, GetDC, GetPixel, HDC, InvalidateRect,
        PAINTSTRUCT, ReleaseDC, SetBkColor, UpdateWindow,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetCapture, GetFocus, ReleaseCapture, SetCapture, SetFocus,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GWL_EXSTYLE, GetClientRect, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
        HWND_TOP, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SendMessageW,
        SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
        WINDOW_EX_STYLE, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
        WS_CHILD, WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_TRANSPARENT, WS_OVERLAPPEDWINDOW, WS_POPUP,
        WS_VISIBLE, WindowFromPoint,
    };
    use windows::core::{PCWSTR, w};

    const HOST: PCWSTR = w!("dg_spike_host");
    const BAR: PCWSTR = w!("dg_spike_bar");
    const PLAIN: PCWSTR = w!("dg_spike_plain");
    const BAR_W: i32 = 20;
    const ALPHA: u8 = 0xB0;
    const BAR_GREY: (u32, u32, u32) = (0x30, 0x30, 0x30);
    const ROW_H: i32 = 20;

    // Layered *child* windows are a DWM feature (Win8+). Whether composition is
    // running decides whether a "refused" verdict is about Windows at all, so it
    // is measured instead of assumed. Declared here rather than through a new
    // windows-crate feature, because this is a probe, not product code.
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmIsCompositionEnabled(penabled: *mut i32) -> i32;
    }
    /// The bars have to sit *inside* the editor child's columns, not in the
    /// strip of host client area beside it: compositing over a sibling child
    /// window is the product case, and measuring next to it would only prove
    /// the bar can cover its own parent's painting.
    const BAR_X_A: i32 = 200;
    const BAR_X_B: i32 = 170;
    const BAR_X_C: i32 = 140;

    /// Recorded by the bar's window proc during the synthetic click, because
    /// that is the only place the real message routing is visible.
    static FOCUS_AT_CLICK: AtomicIsize = AtomicIsize::new(-1);
    /// 1 while the bar is in its faded-out state: a faded bar must stop eating
    /// clicks on the last text column, and for a *child* window the documented
    /// mechanism for that is HTTRANSPARENT, not WS_EX_TRANSPARENT.
    static FADED: AtomicIsize = AtomicIsize::new(0);

    fn as_ptr(h: HWND) -> isize {
        h.0 as isize
    }

    unsafe fn fill(hdc: HDC, l: i32, t: i32, r: i32, b: i32, rgb: (u32, u32, u32)) {
        unsafe {
            let rc = RECT {
                left: l,
                top: t,
                right: r,
                bottom: b,
            };
            // COLORREF is 0x00BBGGRR
            let cr = COLORREF((rgb.2 << 16) | (rgb.1 << 8) | rgb.0);
            let _ = SetBkColor(hdc, cr);
            let _ = ExtTextOutW(hdc, 0, 0, ETO_OPAQUE, Some(&rc), PCWSTR::null(), 0, None);
        }
    }

    unsafe extern "system" fn host_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            if msg == WM_PAINT {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                // Alternating full-width rows: a pixel under the bar is either the
                // red row or the white row, so "see-through" cannot be faked by a
                // uniform blend.
                let mut y = 0;
                let mut i = 0;
                while y < client.bottom {
                    let rgb = if i % 2 == 0 {
                        (255, 0, 0)
                    } else {
                        (255, 255, 255)
                    };
                    fill(hdc, 0, y, client.right, (y + ROW_H).min(client.bottom), rgb);
                    y += ROW_H;
                    i += 1;
                }
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    unsafe extern "system" fn bar_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    let mut client = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);
                    fill(hdc, 0, 0, client.right, client.bottom, BAR_GREY);
                    let _ = EndPaint(hwnd, &ps);
                    LRESULT(0)
                }
                // HTTRANSPARENT is the child-window way to hand a click to the
                // sibling underneath; -1 is that constant.
                WM_NCHITTEST => {
                    if FADED.load(Ordering::SeqCst) == 1 {
                        LRESULT(-1)
                    } else {
                        DefWindowProcW(hwnd, msg, wparam, lparam)
                    }
                }
                // The bar must never take the focus: the caret and the IME
                // composition window are bound to the focused window.
                WM_MOUSEACTIVATE => LRESULT(3),
                WM_LBUTTONDOWN => {
                    // A thumb drag has to work from here, without focus.
                    FOCUS_AT_CLICK.store(as_ptr(GetFocus()), Ordering::SeqCst);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    /// Route C's class: paints the same flat grey, and forwards *everything*
    /// else to DefWindowProc. If even this cannot become a layered child, the
    /// refusal is about the window structure, not about our message handling.
    unsafe extern "system" fn plain_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            if msg == WM_PAINT {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                fill(hdc, 0, 0, client.right, client.bottom, BAR_GREY);
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    unsafe fn register(
        name: PCWSTR,
        proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    ) {
        unsafe {
            let instance = GetModuleHandleW(None)
                .map(|m: HMODULE| HINSTANCE(m.0))
                .unwrap_or(HINSTANCE(std::ptr::null_mut()));
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: name,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                panic!("RegisterClassW failed");
            }
        }
    }

    /// Pump the thread's queue so paint messages actually run; without this the
    /// windows exist but never receive WM_PAINT.
    unsafe fn pump(ms: u32) {
        unsafe {
            let start = std::time::Instant::now();
            let mut msg = MSG::default();
            while start.elapsed().as_millis() < ms as u128 {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    pub fn run(out_path: &str, hold_ms: u32) -> i32 {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register(HOST, host_proc);
            register(BAR, bar_proc);
            register(PLAIN, plain_proc);
            let instance = GetModuleHandleW(None)
                .map(|m: HMODULE| HINSTANCE(m.0))
                .unwrap_or(HINSTANCE(std::ptr::null_mut()));

            // Physical pixels: this process is DPI aware, so the geometry here
            // is the geometry the sampler reads.
            let host = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                HOST,
                w!("overlay-spike"),
                WS_POPUP | WS_VISIBLE,
                60,
                60,
                260,
                160,
                None,
                None,
                Some(instance),
                None,
            )
            .expect("host window");
            let mut host_rect = RECT::default();
            let _ = GetWindowRect(host, &mut host_rect);
            let _ = UpdateWindow(host);
            // The screen read below only means something if nothing is on top of
            // the probe. A background process's popup is not, so it is raised and
            // the raise is verified rather than assumed.
            let _ = SetWindowPos(host, Some(HWND_TOPMOST), 60, 60, 260, 160, SWP_SHOWWINDOW);

            // The product geometry: the rows are painted by an editor *child*
            // HWND, and the bar has to composite over that sibling rather than
            // over its owner's own client. WS_CLIPSIBLINGS on both matters:
            // without it the editor's WM_PAINT fills over the bar, and then
            // neither the pixels nor the hit-test say what the window structure
            // says — which is exactly the kind of result that gets mistaken for
            // "the OS refuses this".
            let editor = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                HOST,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                240,
                160,
                Some(host),
                None,
                Some(instance),
                None,
            )
            .expect("editor child");
            let _ = UpdateWindow(editor);

            // Two ways to make a *child* window layered, measured separately,
            // because "this OS refuses layered child windows" is a claim about
            // documented Win8+ behaviour and must not be concluded from one
            // failed call:
            // A: WS_EX_LAYERED passed in dwExStyle at create time;
            // B: created plain, then GWL_EXSTYLE |= WS_EX_LAYERED.
            // Both sit inside the editor child's columns, so a blend at their
            // centres can only come from compositing over a *sibling*.
            let _ = SetLastError(WIN32_ERROR(0));
            let bar_a = CreateWindowExW(
                WS_EX_LAYERED,
                BAR,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE,
                BAR_X_A,
                0,
                BAR_W,
                160,
                Some(host),
                None,
                Some(instance),
                None,
            );
            let a_created = bar_a.is_ok();
            // GetLastError() only means anything when the call actually failed:
            // read after a success it returns whatever an earlier call left
            // behind, which is how "supported" can be reported as
            // "unsupported, error 0".
            let a_create_error = if a_created { 0 } else { GetLastError().0 };
            let a_hresult = bar_a.as_ref().err().map(|e| e.code().0).unwrap_or(0);
            let mut a_lwa_error = 0u32;
            if let Ok(b) = bar_a {
                // Explicitly above the editor sibling: creation order alone is
                // not something this probe should be relying on.
                let _ = SetWindowPos(
                    b,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                let _ = SetLastError(WIN32_ERROR(0));
                let _ = SetLayeredWindowAttributes(b, COLORREF(0), ALPHA, LWA_ALPHA);
                a_lwa_error = GetLastError().0;
                let _ = InvalidateRect(Some(b), None, true);
                let _ = UpdateWindow(b);
            }

            let bar_b = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                BAR,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE,
                BAR_X_B,
                0,
                BAR_W,
                160,
                Some(host),
                None,
                Some(instance),
                None,
            )
            .expect("plain child B");
            let _ = SetWindowPos(
                bar_b,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let ex_b = GetWindowLongPtrW(bar_b, GWL_EXSTYLE);
            let _ = SetLastError(WIN32_ERROR(0));
            let ex_prev = SetWindowLongPtrW(bar_b, GWL_EXSTYLE, ex_b | (WS_EX_LAYERED.0 as isize));
            let b_flip_error = GetLastError().0;
            // Did the style actually stick? Reading it back separates "child
            // windows cannot be layered here" from "SetWindowLongPtr wrote
            // nothing", and those two have opposite consequences for the design.
            let ex_after = GetWindowLongPtrW(bar_b, GWL_EXSTYLE);
            let b_layered_style_set = (ex_after & (WS_EX_LAYERED.0 as isize)) != 0;
            println!(
                "  raw exstyle: before={ex_b:#x} prev_return={ex_prev:#x} after={ex_after:#x} want_bit={:#x}",
                WS_EX_LAYERED.0 as isize
            );
            let _ = SetLastError(WIN32_ERROR(0));
            let _ = SetLayeredWindowAttributes(bar_b, COLORREF(0), ALPHA, LWA_ALPHA);
            let b_lwa_error = GetLastError().0;
            let _ = InvalidateRect(Some(bar_b), None, true);
            let _ = UpdateWindow(bar_b);

            // Route C: create-time WS_EX_LAYERED child again, but from the
            // paint-only class. If this is refused too, our own message handling
            // (HTTRANSPARENT / MA_NOACTIVATE) is not what blocks layered children.
            let _ = SetLastError(WIN32_ERROR(0));
            let bar_c = CreateWindowExW(
                WS_EX_LAYERED,
                PLAIN,
                PCWSTR::null(),
                // No WS_CLIPSIBLINGS on the bar itself: what has to be clipped
                // is the sibling that paints *under* it, and measuring the bar
                // both ways separates "the style blocks layering" from "the
                // neighbour overpainted it".
                WS_CHILD | WS_VISIBLE,
                BAR_X_C,
                0,
                BAR_W,
                160,
                Some(host),
                None,
                Some(instance),
                None,
            );
            let c_created = bar_c.is_ok();
            let c_create_error = if c_created { 0 } else { GetLastError().0 };
            let mut c_lwa_error = 0u32;
            if let Ok(h) = bar_c {
                let _ = SetWindowPos(
                    h,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                let _ = SetLastError(WIN32_ERROR(0));
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), ALPHA, LWA_ALPHA);
                c_lwa_error = GetLastError().0;
                let _ = InvalidateRect(Some(h), None, true);
                let _ = UpdateWindow(h);
            }

            // The same extended style on an owned *top-level* popup, placed over
            // a different column so the two do not compete: this separates
            // "this session refuses layered windows" from "it refuses layered
            // child windows". Only the latter leaves the overlay design open;
            // the former forces the inset fallback.
            let _ = SetLastError(WIN32_ERROR(0));
            let tl = CreateWindowExW(
                WS_EX_LAYERED,
                BAR,
                PCWSTR::null(),
                WS_POPUP | WS_VISIBLE,
                160,
                60,
                BAR_W,
                160,
                Some(host),
                None,
                Some(instance),
                None,
            );
            let toplevel_layered_supported = tl.is_ok();
            let toplevel_win32_error = GetLastError().0;
            let mut toplevel_rect = RECT {
                left: -1,
                top: -1,
                right: -1,
                bottom: -1,
            };
            let mut toplevel_lwa_error = 0u32;
            if let Ok(t) = tl {
                let _ = SetLastError(WIN32_ERROR(0));
                let _ = SetLayeredWindowAttributes(t, COLORREF(0), ALPHA, LWA_ALPHA);
                toplevel_lwa_error = GetLastError().0;
                let _ = GetWindowRect(t, &mut toplevel_rect);
                let _ = UpdateWindow(t);
            }
            let mut dwm_enabled = -1i32;
            let dwm_hr = DwmIsCompositionEnabled(&mut dwm_enabled);

            // Route D: the product geometry exactly, but under a
            // WS_OVERLAPPEDWINDOW frame instead of a borderless WS_POPUP host —
            // which is what the real main window is. Every child route above
            // shares one parent style, so if *that* is what blocks layered
            // children, testing only against WS_POPUP would have thrown away a
            // working design. No WS_CLIPCHILDREN on the frame: the parent's
            // children are the backdrop, and sibling clipping is done with
            // WS_CLIPSIBLINGS on them.
            let frame = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                HOST,
                w!("overlay-spike-frame"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                360,
                60,
                260,
                200,
                None,
                None,
                Some(instance),
                None,
            )
            .expect("frame window");
            let _ = SetWindowPos(frame, Some(HWND_TOPMOST), 360, 60, 260, 200, SWP_SHOWWINDOW);
            let mut client = RECT::default();
            let _ = GetClientRect(frame, &mut client);
            let backdrop = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                HOST,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                240,
                client.bottom,
                Some(frame),
                None,
                Some(instance),
                None,
            )
            .expect("backdrop child");
            let _ = UpdateWindow(backdrop);
            let _ = SetLastError(WIN32_ERROR(0));
            let bar_d = CreateWindowExW(
                WS_EX_LAYERED,
                PLAIN,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE,
                BAR_X_C,
                0,
                BAR_W,
                client.bottom,
                Some(frame),
                None,
                Some(instance),
                None,
            );
            let d_created = bar_d.is_ok();
            let d_create_error = if d_created { 0 } else { GetLastError().0 };
            let mut d_lwa_error = 0u32;
            let mut d_rect = RECT {
                left: -1,
                top: -1,
                right: -1,
                bottom: -1,
            };
            if let Ok(h) = bar_d {
                let _ = SetWindowPos(
                    h,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                let _ = SetLastError(WIN32_ERROR(0));
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), ALPHA, LWA_ALPHA);
                d_lwa_error = GetLastError().0;
                let _ = GetWindowRect(h, &mut d_rect);
                let _ = InvalidateRect(Some(h), None, true);
                let _ = UpdateWindow(h);
            }
            pump(400);

            let mut a_rect = RECT {
                left: -1,
                top: -1,
                right: -1,
                bottom: -1,
            };
            if let Ok(h) = bar_a {
                let _ = GetWindowRect(h, &mut a_rect);
            }
            let mut b_rect = RECT::default();
            let _ = GetWindowRect(bar_b, &mut b_rect);
            let mut c_rect = RECT {
                left: -1,
                top: -1,
                right: -1,
                bottom: -1,
            };
            if let Ok(h) = bar_c {
                let _ = GetWindowRect(h, &mut c_rect);
            }
            // The hit-test and focus legs run against a layered child,
            // preferring the create-time one; if A was refused, B is layered.
            let bar = bar_a.unwrap_or(bar_b);
            let mut bar_rect = RECT::default();
            let _ = GetWindowRect(bar, &mut bar_rect);
            let sx = (bar_rect.left + bar_rect.right) / 2;

            // The composited screen is the only source that can show a child
            // on top of its sibling: a window DC never contains its siblings.
            let screen = GetDC(None);
            let px = |dc: HDC, x: i32, y: i32| -> (u32, u32, u32) {
                let v = GetPixel(dc, x, y).0;
                (v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff)
            };
            let over_red = px(screen, sx, bar_rect.top + ROW_H / 2);
            let over_white = px(screen, sx, bar_rect.top + ROW_H + ROW_H / 2);
            let host_row = px(screen, host_rect.left + 5, bar_rect.top + ROW_H / 2);
            let host_row_white = px(screen, host_rect.left + 5, bar_rect.top + ROW_H + ROW_H / 2);
            // Each child route is judged on its own pixels, so a refusal of one
            // cannot be read as a refusal of the other.
            let a_cx = (a_rect.left + a_rect.right) / 2;
            let a_over_red = px(screen, a_cx, a_rect.top + ROW_H / 2);
            let a_over_white = px(screen, a_cx, a_rect.top + ROW_H + ROW_H / 2);
            let b_cx = (b_rect.left + b_rect.right) / 2;
            let b_over_red = px(screen, b_cx, b_rect.top + ROW_H / 2);
            let b_over_white = px(screen, b_cx, b_rect.top + ROW_H + ROW_H / 2);
            let c_cx = (c_rect.left + c_rect.right) / 2;
            let c_over_red = px(screen, c_cx, c_rect.top + ROW_H / 2);
            let c_over_white = px(screen, c_cx, c_rect.top + ROW_H + ROW_H / 2);
            let d_cx = (d_rect.left + d_rect.right) / 2;
            let d_over_red = px(screen, d_cx, d_rect.top + ROW_H / 2);
            let d_over_white = px(screen, d_cx, d_rect.top + ROW_H + ROW_H / 2);
            let tl_cx = (toplevel_rect.left + toplevel_rect.right) / 2;
            let tl_over_red = px(screen, tl_cx, toplevel_rect.top + ROW_H / 2);
            let tl_over_white = px(screen, tl_cx, toplevel_rect.top + ROW_H + ROW_H / 2);
            let _ = ReleaseDC(None, screen);
            let not_occluded = as_ptr(WindowFromPoint(POINT {
                x: host_rect.left + 5,
                y: bar_rect.top + ROW_H / 2,
            })) == as_ptr(editor);

            // Whether the *gate* path (PrintWindow, as tools/uicheck/capture.ps1
            // uses it) shows the same thing is measured by that harness against
            // `--hold`, not duplicated here: one capture method per measurement.

            let a = ALPHA as f64 / 255.0;
            let blend = |bg: (u32, u32, u32)| -> (u32, u32, u32) {
                let ch = |b: u32, g: u32| ((g as f64 * a) + (b as f64 * (1.0 - a))).round() as u32;
                (
                    ch(bg.0, BAR_GREY.0),
                    ch(bg.1, BAR_GREY.1),
                    ch(bg.2, BAR_GREY.2),
                )
            };
            let want_red = blend((255, 0, 0));
            let want_white = blend((255, 255, 255));
            let near = |p: (u32, u32, u32), q: (u32, u32, u32)| {
                p.0.abs_diff(q.0) <= 8 && p.1.abs_diff(q.1) <= 8 && p.2.abs_diff(q.2) <= 8
            };
            // Two control samples in a column with no bar over it: the sibling's
            // red row and its white row. If the screen DC cannot show *those*, it
            // cannot show a blend either, and every alpha verdict below is worth
            // nothing. A locked workstation reads flat white everywhere — that is
            // what turned a working design into "the OS refuses this" once
            // already, so the sampler is judged before the OS is.
            let desktop_readable =
                near(host_row, (255, 0, 0)) && near(host_row_white, (255, 255, 255));
            // "See-through" needs the two sampled rows to differ from each other
            // *and* to match the arithmetic blend; a uniform grey band would come
            // from an opaque bar and is not a pass.
            let composited = |red: (u32, u32, u32), white: (u32, u32, u32)| {
                desktop_readable
                    && not_occluded
                    && near(red, want_red)
                    && near(white, want_white)
                    && red != white
            };
            let a_alpha = a_created && a_lwa_error == 0 && composited(a_over_red, a_over_white);
            let b_alpha = b_lwa_error == 0 && composited(b_over_red, b_over_white);
            let c_alpha = c_created && c_lwa_error == 0 && composited(c_over_red, c_over_white);
            let d_alpha = d_created && d_lwa_error == 0 && composited(d_over_red, d_over_white);
            let toplevel_alpha_ok =
                toplevel_layered_supported && composited(tl_over_red, tl_over_white);
            let child_alpha_ok = a_alpha || b_alpha || c_alpha || d_alpha;
            // Whether Windows *accepts* a layered child is a style-level fact and
            // needs no compositor: the handle gets created, the extended-style bit
            // reads back, and SetLayeredWindowAttributes reports no error. Kept
            // separate from the pixel verdict so an unreadable desktop cannot be
            // filed as an OS refusal — and so a refusal cannot be softened into
            // "supported" by those three calls alone.
            let layered_child_accepted = (a_created && a_lwa_error == 0)
                || (b_layered_style_set && b_lwa_error == 0)
                || (c_created && c_lwa_error == 0)
                || (d_created && d_lwa_error == 0);
            // Either child route is enough for "text passes under a translucent
            // bar" *and* stays visible to the gate; the top-level popup composites
            // but is outside PrintWindow's reach (measured with capture.ps1).
            let alpha_ok = child_alpha_ok || toplevel_alpha_ok;
            let alpha_mechanism = if d_alpha {
                "layered_child_in_overlapped_frame"
            } else if a_alpha {
                "layered_child_at_create"
            } else if b_alpha {
                "layered_child_via_exstyle"
            } else if c_alpha {
                "layered_child_paint_only_class"
            } else if toplevel_alpha_ok {
                "layered_toplevel_popup"
            } else {
                "none_use_inset"
            };

            // Passthrough: same window, same handle, no recreate — only the
            // extended style changes.
            let inside = POINT {
                x: sx,
                y: bar_rect.top + 5,
            };
            let hit_opaque = WindowFromPoint(inside);
            let ex = GetWindowLongPtrW(bar, GWL_EXSTYLE);
            let _ = SetWindowLongPtrW(bar, GWL_EXSTYLE, ex | (WS_EX_TRANSPARENT.0 as isize));
            let hit_ex_transparent = WindowFromPoint(inside);
            let _ = SetWindowLongPtrW(bar, GWL_EXSTYLE, ex);
            FADED.store(1, Ordering::SeqCst);
            let hit_ht_transparent = WindowFromPoint(inside);
            FADED.store(0, Ordering::SeqCst);
            let hit_again = WindowFromPoint(inside);

            // The bar answers MA_NOACTIVATE; the host, through DefWindowProc,
            // answers MA_ACTIVATE — the contrast is what shows the handler is
            // the thing changing behaviour.
            let act_bar = SendMessageW(
                bar,
                WM_MOUSEACTIVATE,
                Some(WPARAM(host.0 as usize)),
                Some(LPARAM(1)),
            );
            let act_host = SendMessageW(
                host,
                WM_MOUSEACTIVATE,
                Some(WPARAM(host.0 as usize)),
                Some(LPARAM(1)),
            );

            FOCUS_AT_CLICK.store(-1, Ordering::SeqCst);
            let focus_before = as_ptr(GetFocus());
            let _ = SetFocus(Some(host));
            let focus_host = as_ptr(GetFocus());
            let _ = SendMessageW(
                bar,
                WM_LBUTTONDOWN,
                Some(WPARAM(0)),
                Some(LPARAM(0x0005_000A)),
            );
            let focus_at_click = FOCUS_AT_CLICK.load(Ordering::SeqCst);
            // The drag path captures without ever asking for focus.
            let _ = SetCapture(bar);
            let capture_held = as_ptr(GetCapture());
            let _ = ReleaseCapture();
            let focus_after = as_ptr(GetFocus());

            if hold_ms > 0 {
                pump(hold_ms);
            }

            let ex_skips = as_ptr(hit_ex_transparent) != as_ptr(bar);
            let ht_skips = as_ptr(hit_ht_transparent) != as_ptr(bar);
            let passthrough_ok = as_ptr(hit_opaque) == as_ptr(bar)
                && (ex_skips || ht_skips)
                && as_ptr(hit_again) == as_ptr(bar);
            let focus_ok = act_bar.0 == 3
                && act_host.0 != 3
                && focus_at_click == focus_host
                && focus_after == focus_host
                && capture_held == as_ptr(bar);

            let verdicts = json!({
                "alpha": {
                    "over_red_row": over_red,
                    "over_white_row": over_white,
                    "expected_over_red": want_red,
                    "expected_over_white": want_white,
                    "host_row_untouched": host_row,
                    "host_row_white_control": host_row_white,
                    "desktop_readable": desktop_readable,
                    "not_occluded": not_occluded,
                    "bar_window": [bar_rect.left, bar_rect.top, bar_rect.right, bar_rect.bottom],
                    "host_window": [host_rect.left, host_rect.top, host_rect.right, host_rect.bottom],
                    "alpha": ALPHA,
                    "ok": alpha_ok,
                },
                "layering": {
                    "mechanism": alpha_mechanism,
                    "child_alpha_ok": child_alpha_ok,
                    // Style-level, so it survives a desktop that cannot be
                    // sampled. Without a <supportedOS> Win8+ application
                    // manifest this is the false branch, and the pixel legs
                    // then say nothing about the OS at all.
                    "layered_child_accepted": layered_child_accepted,
                    "requires_manifest_supportedos_win8": true,
                    // Route A: WS_EX_LAYERED passed at create time.
                    "a_created": a_created,
                    "a_win32_error": a_create_error,
                    "a_hresult": a_hresult,
                    "a_lwa_error": a_lwa_error,
                    "a_alpha_ok": a_alpha,
                    "a_rect_screen": [a_rect.left, a_rect.top, a_rect.right, a_rect.bottom],
                    "a_client_x": [BAR_X_A, BAR_X_A + BAR_W],
                    "a_over_red": a_over_red,
                    "a_over_white": a_over_white,
                    // Route B: plain child, then GWL_EXSTYLE |= WS_EX_LAYERED.
                    "b_flip_win32_error": b_flip_error,
                    "b_layered_style_stuck": b_layered_style_set,
                    "b_lwa_error": b_lwa_error,
                    "b_alpha_ok": b_alpha,
                    "b_rect_screen": [b_rect.left, b_rect.top, b_rect.right, b_rect.bottom],
                    "b_client_x": [BAR_X_B, BAR_X_B + BAR_W],
                    "b_over_red": b_over_red,
                    "b_over_white": b_over_white,
                    // Route C: create-time style, paint-only class.
                    "c_created": c_created,
                    "c_win32_error": c_create_error,
                    "c_lwa_error": c_lwa_error,
                    "c_alpha_ok": c_alpha,
                    "c_rect_screen": [c_rect.left, c_rect.top, c_rect.right, c_rect.bottom],
                    "c_client_x": [BAR_X_C, BAR_X_C + BAR_W],
                    "c_over_red": c_over_red,
                    "c_over_white": c_over_white,
                    // Route D: overlapped (product-shaped) frame, layered child.
                    "d_created": d_created,
                    "d_win32_error": d_create_error,
                    "d_lwa_error": d_lwa_error,
                    "d_alpha_ok": d_alpha,
                    "d_rect_screen": [d_rect.left, d_rect.top, d_rect.right, d_rect.bottom],
                    "d_over_red": d_over_red,
                    "d_over_white": d_over_white,
                    // PNG coordinate for the gate leg = capture meta client_x/y
                    // plus this, because an overlapped frame has a title bar.
                    "d_bar_client": [BAR_X_C + BAR_W / 2, ROW_H / 2],
                    // Contrast leg: the same style on an owned top-level popup.
                    "toplevel_layered_supported": toplevel_layered_supported,
                    "toplevel_win32_error": toplevel_win32_error,
                    "toplevel_lwa_error": toplevel_lwa_error,
                    "toplevel_rect": [toplevel_rect.left, toplevel_rect.top, toplevel_rect.right, toplevel_rect.bottom],
                    "toplevel_over_red": tl_over_red,
                    "toplevel_over_white": tl_over_white,
                    "toplevel_alpha_ok": toplevel_alpha_ok,
                },
                "environment": {
                    // A "layered child windows are refused" verdict is only
                    // about Windows if composition is actually running.
                    "dwm_hr": dwm_hr,
                    "dwm_composition_enabled": dwm_enabled,
                },
                "passthrough": {
                    "hit_normal": as_ptr(hit_opaque),
                    "hit_with_ws_ex_transparent": as_ptr(hit_ex_transparent),
                    "hit_with_ht_transparent": as_ptr(hit_ht_transparent),
                    "hit_after_restoring": as_ptr(hit_again),
                    "ex_style_skips": ex_skips,
                    "nchittest_skips": ht_skips,
                    "bar": as_ptr(bar),
                    "host": as_ptr(host),
                    "ok": passthrough_ok,
                },
                "no_focus_steal": {
                    "ma_from_bar": act_bar.0,
                    "ma_from_host_defproc": act_host.0,
                    "focus_before_spike": focus_before,
                    "focus_on_host": focus_host,
                    "focus_at_bar_click": focus_at_click,
                    "focus_after_all": focus_after,
                    "capture_held_by_bar": capture_held,
                    "foreground_window": as_ptr(GetForegroundWindow()),
                    "ok": focus_ok,
                },
                "host_title": "overlay-spike",
            });
            let _ = std::fs::write(out_path, serde_json::to_string_pretty(&verdicts).unwrap());
            println!("alpha={alpha_ok} passthrough={passthrough_ok} no_focus_steal={focus_ok}");
            println!(
                "  child A (create-time WS_EX_LAYERED): created={a_created} err={a_create_error} hr={a_hresult:x} lwa_err={a_lwa_error} alpha={a_alpha} over_red={a_over_red:?} over_white={a_over_white:?}"
            );
            println!(
                "  child B (GWL_EXSTYLE flip): flip_err={b_flip_error} stuck={b_layered_style_set} lwa_err={b_lwa_error} alpha={b_alpha} over_red={b_over_red:?} over_white={b_over_white:?}"
            );
            println!(
                "  child C (paint-only class, create-time): created={c_created} err={c_create_error} lwa_err={c_lwa_error} alpha={c_alpha} over_red={c_over_red:?} over_white={c_over_white:?}"
            );
            println!(
                "  child D (overlapped frame): created={d_created} err={d_create_error} lwa_err={d_lwa_error} alpha={d_alpha} over_red={d_over_red:?} over_white={d_over_white:?}"
            );
            println!(
                "  popup lwa_err={toplevel_lwa_error}  dwm: hr={dwm_hr} composition_enabled={dwm_enabled}"
            );
            println!(
                "  want: over_red={want_red:?} over_white={want_white:?} host_row={host_row:?}/{host_row_white:?} readable={desktop_readable} not_occluded={not_occluded} layered_child_accepted={layered_child_accepted}"
            );
            println!(
                "  gate leg: capture.ps1 then probe png x={} (A) / x={} (B), y={} and {} ; expect {:?} / {:?}",
                BAR_X_A + BAR_W / 2,
                BAR_X_B + BAR_W / 2,
                ROW_H / 2,
                ROW_H + ROW_H / 2,
                want_red,
                want_white
            );
            println!(
                "  mechanism={alpha_mechanism} child={child_alpha_ok} toplevel={toplevel_alpha_ok} tl_err={toplevel_win32_error}"
            );
            println!(
                "  hwnds: host={} editor={} bar={} toplevel={} bar_rect={:?} tl_rect={:?}",
                as_ptr(host),
                as_ptr(editor),
                as_ptr(bar),
                tl.as_ref().map(|h| as_ptr(*h)).unwrap_or(-1),
                bar_rect,
                toplevel_rect
            );
            println!(
                "  hit_normal={} ex_transparent={} ht_transparent={} restored={} bar={}",
                as_ptr(hit_opaque),
                as_ptr(hit_ex_transparent),
                as_ptr(hit_ht_transparent),
                as_ptr(hit_again),
                as_ptr(bar)
            );
            println!(
                "  ma_bar={} ma_host={} focus_host={} focus_at_click={} capture={} foreground={}",
                act_bar.0,
                act_host.0,
                focus_host,
                focus_at_click,
                capture_held,
                as_ptr(GetForegroundWindow())
            );
            let _ = DestroyWindow(bar);
            let _ = DestroyWindow(host);
            if alpha_ok && passthrough_ok && focus_ok {
                0
            } else if !desktop_readable {
                // A distinct code, so "the probe could not see the screen" never
                // gets filed as "Windows refuses layered child windows".
                println!(
                    "  INCONCLUSIVE: control rows did not come back ({host_row:?}/{host_row_white:?}) — the desktop is locked or not being composited; no pixel leg means anything. Style-level verdict: layered_child_accepted={layered_child_accepted}"
                );
                2
            } else {
                1
            }
        }
    }
}

#[cfg(not(windows))]
mod spike {
    pub fn run(_out_path: &str, _hold_ms: u32) -> i32 {
        eprintln!("overlay spike is Windows-only");
        2
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "overlay_spike.json".to_string());
    let hold = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0u32);
    std::process::exit(spike::run(&out, hold));
}
