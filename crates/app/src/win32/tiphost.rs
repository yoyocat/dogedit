//! 共享的提示宿主: one `tooltips_class32` window per
//! parent, plus the tool bookkeeping every surface needs.
//!
//! Three tool shapes:
//! - window tools (`TTF_IDISHWND | TTF_SUBCLASS`) — a child control
//!   whose whole rect is the hover zone (dialog controls, find-bar
//!   buttons); the tooltip subclasses the control and shows by itself;
//! - tracking tips (`TTF_TRACK | TTF_ABSOLUTE`) — one tip pinned at a
//!   screen point, shown and hidden by the owner (the editor's icon
//!   strip hover);
//! - rect tools — a named rectangle inside one owner window, shown
//!   when the pointer enters it.
//!
//! Every surface (the find bar, the dialogs, the editor's icon strip)
//! hosts its texts through this module instead of building its own
//! tooltip control.

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{
    TTF_ABSOLUTE, TTF_IDISHWND, TTF_SUBCLASS, TTF_TRACK, TOOLTIP_FLAGS, TTM_ACTIVATE,
    TTM_ADDTOOLW, TTM_SETDELAYTIME, TTM_SETMAXTIPWIDTH, TTM_TRACKACTIVATE, TTM_TRACKPOSITION,
    TTM_UPDATETIPTEXTW, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetParent, SendMessageW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_POPUP,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::win32::view::instance;

/// The tracking tip's fixed tool id inside its
/// owner window.
const TRACK_ID: usize = 0x5449_4450; // 'TIDP'

/// One tooltip control owned by `parent`.
pub struct TipHost {
    hwnd: HWND,
    /// The tracking tip's owner (the window the
    /// rect/track tools are named against).
    track_owner: HWND,
    /// Whether the tracking tool is currently
    /// registered with the control.
    track_added: bool,
}

impl Drop for TipHost {
    fn drop(&mut self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

impl TipHost {
    /// Creates the control (inactive until
    /// [`activate`](Self::activate) turns it on).
    pub fn create(parent: HWND) -> Option<Self> {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("tooltips_class32"),
                PCWSTR::null(),
                // Always show, and treat `&` in a tip as text rather than an
                // accelerator prefix.
                WINDOW_STYLE(WS_POPUP.0 | 0x0000_0003),
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                Some(instance()),
                None,
            )
            .ok()?;
            // autopop 5000 ms, wide enough for long tip lines.
            let _ = SendMessageW(hwnd, TTM_SETDELAYTIME, Some(WPARAM(2)), Some(LPARAM(5000)));
            let _ = SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, Some(WPARAM(0)), Some(LPARAM(600)));
            let _ = SendMessageW(hwnd, TTM_ACTIVATE, Some(WPARAM(0)), None);
            Some(Self {
                hwnd,
                track_owner: parent,
                track_added: false,
            })
        }
    }

    /// The whole of `target` becomes a hover zone showing
    /// `text` (id-is-handle; the tooltip subclasses the
    /// control for the mouse, so no relay is needed).
    pub fn add_window_tool(&self, target: HWND, text: &str) {
        let owner = unsafe { GetParent(target).unwrap_or_default() };
        self.upsert_tool(TTF_IDISHWND | TTF_SUBCLASS, owner, target.0 as usize, true, text);
        // The control is created disabled so a tracking tip can wait for its
        // owner. A window tool shows by itself; without this it never does.
        unsafe {
            let _ = SendMessageW(self.hwnd, TTM_ACTIVATE, Some(WPARAM(1)), None);
        }
    }

    /// Shows the tracking tip with `text` pinned at
    /// the screen point (`x`, `y`). A second call
    /// while up rewrites the text in place.
    pub fn show_tracking(&mut self, text: &str, x: i32, y: i32) {
        unsafe {
            if !self.track_added {
                let ti = TTTOOLINFOW {
                    cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                    uFlags: TTF_TRACK | TTF_ABSOLUTE,
                    hwnd: self.track_owner,
                    uId: TRACK_ID,
                    ..Default::default()
                };
                let _ = SendMessageW(
                    self.hwnd,
                    TTM_ADDTOOLW,
                    None,
                    Some(LPARAM(&raw const ti as isize)),
                );
                self.track_added = true;
            }
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: self.track_owner,
                uId: TRACK_ID,
                ..Default::default()
            };
            let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
            ti.lpszText = PWSTR(buf.as_ptr() as *mut u16);
            let _ = SendMessageW(
                self.hwnd,
                TTM_UPDATETIPTEXTW,
                None,
                Some(LPARAM(&raw const ti as isize)),
            );
            let _ = SendMessageW(
                self.hwnd,
                TTM_TRACKPOSITION,
                None,
                Some(LPARAM((y as isize) << 16 | (x as isize & 0xFFFF))),
            );
            let _ = SendMessageW(self.hwnd, TTM_ACTIVATE, Some(WPARAM(1)), None);
            // **`TTM_ACTIVATE` 只是把控件使能**，让一条 `TTF_TRACK` 的提示真的
            // 出现在屏幕上的是 `TTM_TRACKACTIVATE`（还要带上那条 tool 本身）。
            // 少了这一条，tool 注册得再对、位置设得再准，屏幕上也是空的。
            let ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: self.track_owner,
                uId: TRACK_ID,
                ..Default::default()
            };
            let _ = SendMessageW(
                self.hwnd,
                TTM_TRACKACTIVATE,
                Some(WPARAM(1)),
                Some(LPARAM(&raw const ti as isize)),
            );
        }
    }

    /// Hides the tracking tip (the tool stays
    /// registered; the next show reuses it).
    pub fn hide_tracking(&mut self) {
        if !self.track_added {
            return;
        }
        unsafe {
            let ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: self.track_owner,
                uId: TRACK_ID,
                ..Default::default()
            };
            let _ = SendMessageW(
                self.hwnd,
                TTM_TRACKACTIVATE,
                Some(WPARAM(0)),
                Some(LPARAM(&raw const ti as isize)),
            );
        }
    }

    /// Adds-or-updates one window tool. The call
    /// sends `TTM_ADDTOOLW` for a fresh tool and
    /// always follows with `TTM_UPDATETIPTEXTW`
    /// (the tooltip copies the text during the
    /// message, so the buffer only has to live
    /// that long).
    fn upsert_tool(&self, flags: TOOLTIP_FLAGS, owner: HWND, id: usize, add: bool, text: &str) {
        unsafe {
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: flags,
                hwnd: owner,
                uId: id,
                ..Default::default()
            };
            if add {
                let _ = SendMessageW(
                    self.hwnd,
                    TTM_ADDTOOLW,
                    None,
                    Some(LPARAM(&raw const ti as isize)),
                );
            }
            let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
            ti.lpszText = PWSTR(buf.as_ptr() as *mut u16);
            let _ = SendMessageW(
                self.hwnd,
                TTM_UPDATETIPTEXTW,
                None,
                Some(LPARAM(&raw const ti as isize)),
            );
        }
    }
}
