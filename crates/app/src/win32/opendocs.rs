//! The 文件列表 pane: the documents that are open, in the order the tab strip
//! holds them, as a list that switches to the row it is clicked on.
//!
//! The pane is a window of its own because the rows are the whole of its
//! content: an icon, a name, and a face that alternates so a long list can be
//! followed across. The rows are drawn here rather than by a list control so
//! the row height, the icon column and the selected row's ink come from the
//! theme instead of from the system's light-mode defaults.
//!
//! A click leaves as a [`DocsNotify`] through the same `WM_APP` queue the
//! directory tree and the results list post to: switching a document re-enters
//! the frame's state, which the click handler is already sitting on.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::Cell;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetSysColor, HFONT,
    HGDIOBJ, InvalidateRect, PAINTSTRUCT, SRCCOPY, SYS_COLOR_INDEX, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TrackMouseEvent, VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyIcon, DrawIconEx, GetClientRect, GetParent,
    HICON, HMENU, IDC_ARROW, LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_APP, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETFOCUS, WNDCLASSW,
    WS_CHILD,
};
use windows::core::{PCWSTR, w};

use crate::win32::uifont;
use crate::win32::window::{state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::Theme;

/// The pane's class.
const DOCS_CLASS: PCWSTR = w!("OpenDocList");

/// The list's control id.
const CTRL_ID: usize = 2041;

/// One row at 96 DPI: a 16 px icon with five rows of air above and below.
const ROW_H: i32 = 26;
/// The icon column's left inset.
const ICON_LEFT: i32 = 4;
/// The icon box.
const ICON: i32 = 16;
/// The label's left inset: past the icon and its gap.
const TEXT_LEFT: i32 = 27;
/// Lines one wheel notch moves, the same step the editor's wheel uses.
const WHEEL_ROWS: i32 = 3;

/// One open document, as the list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenDocEntry {
    /// The tab's label: the same text the strip and the 窗口 menu use.
    pub label: String,
    /// The file behind the document, for the row's icon. `None` for a buffer
    /// that has never been saved.
    pub path: Option<String>,
}

/// What a click or a key asks the frame to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DocsNotify {
    /// Bring this document to the front.
    Activate(usize),
}

pub(super) struct OpenDocsWin {
    hwnd: HWND,
    frame: Rect,
    entries: Vec<OpenDocEntry>,
    /// The document on screen; the row the list paints as selected.
    active: usize,
    /// First row on screen.
    top: usize,
    /// The row the pointer is over, when it is over one.
    hot: Option<usize>,
    /// One shell icon per entry, `0` for an entry that has no file yet. The
    /// icons live as long as the entry list does.
    icons: Vec<isize>,
    dpi: i32,
    font: HFONT,
    bg: COLORREF,
    alt: COLORREF,
    fg: COLORREF,
    sel_bg: COLORREF,
    sel_fg: COLORREF,
    hover: COLORREF,
    pending: Cell<Vec<DocsNotify>>,
    tracking: Cell<bool>,
}

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

static REGISTER_CLASS: Once = Once::new();

/// One row's height at `dpi`.
fn row_h(dpi: i32) -> i32 {
    uifont::scaled(ROW_H, dpi)
}

impl OpenDocsWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            entries: Vec::new(),
            active: 0,
            top: 0,
            hot: None,
            icons: Vec::new(),
            dpi: 96,
            font: HFONT::default(),
            bg: COLORREF(0),
            alt: COLORREF(0),
            fg: COLORREF(0),
            sel_bg: COLORREF(0),
            sel_fg: COLORREF(0),
            hover: COLORREF(0),
            pending: Cell::new(Vec::new()),
            tracking: Cell::new(false),
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(super) unsafe fn create(&mut self, parent: HWND, dpi: i32, theme: &Theme) {
        self.dpi = dpi.max(96);
        if self.font.0.is_null() {
            self.font = uifont::create(self.dpi);
        }
        self.apply_theme(theme);
        if self.alive() {
            return;
        }
        REGISTER_CLASS.call_once(|| {
            let _ = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(docs_proc),
                hInstance: instance(),
                lpszClassName: DOCS_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            });
        });
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            DOCS_CLASS,
            w!("open documents"),
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(CTRL_ID as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.hwnd = hwnd;
    }

    /// Places the list in `rect`; an empty rect means the pane is not showing.
    pub(super) unsafe fn place(&mut self, rect: Rect) {
        if !self.alive() {
            return;
        }
        let open = rect.w > 0.0 && rect.h > 0.0;
        if !open {
            if self.frame.w > 0.0 {
                self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if self.frame == rect {
            return;
        }
        self.frame = rect;
        let _ = SetWindowPos(
            self.hwnd,
            None,
            rect.x.round() as i32,
            rect.y.round() as i32,
            rect.w.round() as i32,
            rect.h.round() as i32,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(self.hwnd, SW_SHOW);
        self.clamp_top();
    }

    /// Hands the list the open documents and which of them is on screen. The
    /// frame calls it on every paint; only a real change costs a reload.
    pub(super) fn set_entries(&mut self, entries: Vec<OpenDocEntry>, active: usize) {
        if self.entries == entries && self.active == active {
            return;
        }
        let reload = self.entries != entries;
        if reload {
            self.drop_icons();
            self.entries = entries;
            self.icons = vec![0; self.entries.len()];
        }
        self.active = active;
        // The list follows the document: switching tabs from anywhere else
        // scrolls the selected row into view rather than leaving it off screen.
        self.ensure_visible(active);
        self.clamp_top();
        if self.alive() {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }
    }

    pub(super) fn apply_theme(&mut self, theme: &Theme) {
        let bg = to_colorref(theme.panel_bg);
        // The stripe is the same face pulled a step toward the text colour, so
        // a light theme alternates downward and a dark one upward without a
        // key of its own.
        let alt = to_colorref(mix(theme.panel_bg, theme.panel_fg, 0.06));
        let fg = to_colorref(theme.panel_fg);
        let hover = to_colorref(theme.panel_hover);
        // Called from every layout; repainting a list whose colours did not
        // change turns each resize step into a full-list repaint.
        let changed = self.bg != bg || self.alt != alt || self.fg != fg || self.hover != hover;
        self.bg = bg;
        self.alt = alt;
        self.fg = fg;
        self.hover = hover;
        self.sel_bg = system(COLOR_HIGHLIGHT);
        self.sel_fg = system(COLOR_HIGHLIGHTTEXT);
        if changed && self.alive() {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }
    }

    /// The row holding `(x, y)` in client space.
    fn row_at(&self, y: i32) -> Option<usize> {
        let h = row_h(self.dpi);
        if h <= 0 || y < 0 {
            return None;
        }
        let index = self.top + (y / h) as usize;
        (index < self.entries.len()).then_some(index)
    }

    /// Rows that fit in the current height, never zero: a pane one pixel tall
    /// still has to answer a hit test.
    fn visible_rows(&self) -> usize {
        let h = self.frame.h.round() as i32;
        ((h / row_h(self.dpi)).max(1)) as usize
    }

    fn clamp_top(&mut self) {
        let rows = self.visible_rows();
        let max = self.entries.len().saturating_sub(rows);
        self.top = self.top.min(max);
    }

    /// Scrolls so `index` is on screen, moving as little as possible.
    fn ensure_visible(&mut self, index: usize) {
        let rows = self.visible_rows().max(1);
        if index < self.top {
            self.top = index;
        } else if index >= self.top + rows {
            self.top = index + 1 - rows;
        }
    }

    /// Scrolls by `delta` rows, clamped to the list.
    fn scroll_by(&mut self, delta: i32) {
        let max = self.entries.len().saturating_sub(self.visible_rows());
        let next = self.top as i32 + delta;
        self.top = next.clamp(0, max as i32) as usize;
        if self.alive() {
            let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
        }
    }

    pub(super) fn drain_notifies(&self) -> Vec<DocsNotify> {
        self.pending.take()
    }

    fn queue(&self, n: DocsNotify) {
        let mut list = self.pending.take();
        list.push(n);
        self.pending.set(list);
        unsafe {
            if let Ok(frame) = GetParent(self.hwnd) {
                if !frame.0.is_null() {
                    let _ = PostMessageW(Some(frame), WM_APP, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    /// The shell icon of entry `index`, loaded on first use.
    fn icon_of(&mut self, index: usize) -> isize {
        let Some(entry) = self.entries.get(index) else {
            return 0;
        };
        if let Some(slot) = self.icons.get(index) {
            if *slot != 0 {
                return *slot;
            }
        }
        let Some(path) = entry.path.as_deref() else {
            return 0;
        };
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut info = SHFILEINFOW::default();
        let ok = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_SMALLICON,
            )
        };
        if ok == 0 {
            return 0;
        }
        let icon = info.hIcon.0 as isize;
        if let Some(slot) = self.icons.get_mut(index) {
            *slot = icon;
        }
        icon
    }

    fn drop_icons(&mut self) {
        for icon in self.icons.drain(..) {
            if icon != 0 {
                let _ = unsafe { DestroyIcon(HICON(icon as *mut core::ffi::c_void)) };
            }
        }
    }

    fn paint(&mut self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if hdc.0.is_null() {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
                return;
            }
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let w = (rc.right - rc.left).max(1);
            let h = (rc.bottom - rc.top).max(1);
            // One memory DC and a single blit: a wheel notch moves every row,
            // so painting direct would show the face before the rows land.
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old_bmp = SelectObject(mem, bmp.into());
            let old_font = SelectObject(mem, HGDIOBJ(self.font.0));
            let _ = SetBkMode(mem, TRANSPARENT);
            let rh = row_h(self.dpi);
            let icon = uifont::scaled(ICON, self.dpi);
            let icon_left = uifont::scaled(ICON_LEFT, self.dpi);
            let text_left = uifont::scaled(TEXT_LEFT, self.dpi);
            let rows = ((h + rh - 1) / rh).max(1) as usize;
            for slot in 0..rows {
                let index = self.top + slot;
                let top = slot as i32 * rh;
                let cell = RECT {
                    left: 0,
                    top,
                    right: w,
                    bottom: (top + rh).min(h),
                };
                let selected = index == self.active && index < self.entries.len();
                let hot = self.hot == Some(index) && index < self.entries.len();
                let face = if selected {
                    self.sel_bg
                } else if hot {
                    self.hover
                } else if index % 2 == 1 {
                    self.alt
                } else {
                    self.bg
                };
                let brush = CreateSolidBrush(face);
                let _ = FillRect(mem, &cell, brush);
                let _ = DeleteObject(brush.into());
                let ink = if selected { self.sel_fg } else { self.fg };
                let glyph = self.icon_of(index);
                let Some(entry) = self.entries.get(index) else {
                    continue;
                };
                if glyph != 0 {
                    let y = top + (rh - icon) / 2;
                    let _ = DrawIconEx(
                        mem,
                        icon_left,
                        y,
                        HICON(glyph as *mut core::ffi::c_void),
                        icon,
                        icon,
                        0,
                        None,
                        DI_NORMAL,
                    );
                } else {
                    // A buffer with no file behind it: the page shape the dock's
                    // captions use — a sheet with its top-right corner knocked
                    // out — in the row's ink.
                    let page = RECT {
                        left: icon_left + icon / 8,
                        top: top + (rh - icon) / 2,
                        right: icon_left + icon - icon / 8,
                        bottom: top + (rh - icon) / 2 + icon,
                    };
                    let body = CreateSolidBrush(ink);
                    let _ = FillRect(mem, &page, body);
                    let _ = DeleteObject(body.into());
                    let fold = RECT {
                        left: page.right - icon / 3,
                        top: page.top,
                        right: page.right + 1,
                        bottom: page.top + icon / 3,
                    };
                    let corner = CreateSolidBrush(face);
                    let _ = FillRect(mem, &fold, corner);
                    let _ = DeleteObject(corner.into());
                }
                let _ = SetTextColor(mem, ink);
                let mut label: Vec<u16> = entry.label.encode_utf16().collect();
                let mut text = RECT {
                    left: text_left,
                    top,
                    right: (w - uifont::scaled(4, self.dpi)).max(text_left),
                    bottom: top + rh,
                };
                DrawTextW(
                    mem,
                    &mut label,
                    &mut text as *mut RECT,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
            }
            SelectObject(mem, old_font);
            let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

/// `a` moved `t` of the way toward `b`, component by component.
fn mix(a: dg_render::theme::Color, b: dg_render::theme::Color, t: f32) -> dg_render::theme::Color {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    dg_render::theme::Color::rgb(lerp(a.r, b.r), lerp(a.g, b.g), lerp(a.b, b.b))
}

/// A system colour as a `COLORREF`. The selected row wears the desktop's own
/// highlight, which is what makes the list match the shell rather than the
/// editor's theme.
fn system(index: SYS_COLOR_INDEX) -> COLORREF {
    COLORREF(unsafe { GetSysColor(index) })
}

unsafe extern "system" fn docs_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let frame = match unsafe { GetParent(hwnd) } {
        Ok(f) if !f.0.is_null() => f,
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    let Some(st) = state_mut(frame) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            st.panels.files_win.paint(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let _ = unsafe { SetFocus(Some(hwnd)) };
            let win = &mut st.panels.files_win;
            if let Some(index) = win.row_at(y) {
                win.queue(DocsNotify::Activate(index));
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let win = &mut st.panels.files_win;
            let hot = win.row_at(y);
            if hot != win.hot {
                win.hot = hot;
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            if !win.tracking.get() {
                win.tracking.set(true);
                let mut track = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<
                        windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT,
                    >() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = unsafe { TrackMouseEvent(&mut track) };
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let win = &mut st.panels.files_win;
            win.tracking.set(false);
            if win.hot.take().is_some() {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let step = if delta > 0 { -WHEEL_ROWS } else { WHEEL_ROWS };
            st.panels.files_win.scroll_by(step);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let win = &mut st.panels.files_win;
            let count = win.entries.len();
            if count == 0 {
                return LRESULT(0);
            }
            let active = win.active.min(count - 1);
            let page = win.visible_rows().max(1) as i32;
            let target = match wparam.0 as u16 {
                v if v == VK_UP.0 => Some(active as i32 - 1),
                v if v == VK_DOWN.0 => Some(active as i32 + 1),
                v if v == VK_PRIOR.0 => Some(active as i32 - page),
                v if v == VK_NEXT.0 => Some(active as i32 + page),
                v if v == VK_HOME.0 => Some(0),
                v if v == VK_END.0 => Some(count as i32 - 1),
                _ => None,
            };
            if let Some(t) = target {
                let t = t.clamp(0, count as i32 - 1) as usize;
                win.queue(DocsNotify::Activate(t));
            }
            LRESULT(0)
        }
        WM_SETFOCUS | WM_KILLFOCUS => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(entries: usize, active: usize, height: f32) -> OpenDocsWin {
        let mut w = OpenDocsWin::new();
        w.dpi = 96;
        w.frame = Rect::new(0.0, 0.0, 240.0, height);
        w.entries = (0..entries)
            .map(|i| OpenDocEntry {
                label: format!("doc{i}"),
                path: None,
            })
            .collect();
        w.icons = vec![0; entries];
        w.active = active;
        w
    }

    /// A row is 26 px and a click names the document under it, not the row
    /// number on screen.
    #[test]
    fn a_click_names_the_row_it_lands_on() {
        let mut w = win(10, 0, 260.0);
        assert_eq!(w.visible_rows(), 10);
        assert_eq!(w.row_at(0), Some(0));
        assert_eq!(w.row_at(25), Some(0));
        assert_eq!(w.row_at(26), Some(1));
        // Past the last document there is no row, even inside the client.
        let w2 = win(2, 0, 260.0);
        assert_eq!(w2.row_at(26), Some(1));
        assert_eq!(w2.row_at(52), None);
        // Scrolled: the top row is the first one on screen.
        w.top = 3;
        assert_eq!(w.row_at(0), Some(3));
        assert_eq!(w.row_at(27), Some(4));
    }

    /// The list follows the document: a switch from anywhere else scrolls the
    /// selected row into view, and the window never scrolls past its end.
    #[test]
    fn the_selected_row_is_kept_on_screen() {
        let mut w = win(10, 0, 26.0 * 4.0);
        assert_eq!(w.visible_rows(), 4);
        w.ensure_visible(0);
        assert_eq!(w.top, 0);
        w.ensure_visible(9);
        assert_eq!(w.top, 6);
        // A row already on screen moves nothing: rows 6..9 are up there.
        w.ensure_visible(7);
        assert_eq!(w.top, 6);
        // A row above the window pulls the top up to it.
        w.ensure_visible(2);
        assert_eq!(w.top, 2);
        // A list that shrank under the window pulls its top back in range.
        w.top = 9;
        w.clamp_top();
        assert_eq!(w.top, 6);
        // A pane with room for four rows and three documents cannot scroll.
        let mut w = win(3, 0, 26.0 * 4.0);
        w.top = 2;
        w.clamp_top();
        assert_eq!(w.top, 0);
    }

    /// A wheel notch is three rows and clamps at both ends.
    #[test]
    fn the_wheel_stops_at_the_ends() {
        let mut w = win(10, 0, 26.0 * 4.0);
        w.scroll_by(-3);
        assert_eq!(w.top, 0);
        w.scroll_by(3);
        assert_eq!(w.top, 3);
        w.scroll_by(30);
        assert_eq!(w.top, 6);
    }

    /// The stripe is a step from the panel's face toward its ink, so both a
    /// light and a dark theme alternate without a key of their own.
    #[test]
    fn the_stripe_follows_the_panel_face() {
        use dg_render::theme::Color;
        let dark = mix(Color::rgb(0x25, 0x25, 0x26), Color::rgb(0xCC, 0xCC, 0xCC), 0.06);
        assert!(dark.r > 0x25 && dark.r < 0x40, "{dark:?}");
        let light = mix(Color::rgb(0xFF, 0xFF, 0xFF), Color::rgb(0x10, 0x10, 0x10), 0.06);
        assert!(light.r < 0xFF && light.r > 0xE0, "{light:?}");
    }
}
