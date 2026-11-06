//! The word-completion popup.
//!
//! The popup is a real system LISTBOX — `WS_POPUP | WS_VSCROLL |
//! LBS_NODATA | LBS_NOINTEGRALHEIGHT | LBS_OWNERDRAWFIXED` — subclassed so
//! the control stays the item store and the scrollbar while every pixel is
//! painted here. It is `WS_EX_NOACTIVATE`: the editor keeps the keyboard and
//! filters keys itself.
//!
//! Rows are painted `3 px | digit cell | 4 px | source marker | 4 px | text`
//! in the editor font over the `WordComplete` theme pair, with the selected
//! row blended toward the foreground. Matching is a prefix match of at least
//! two characters over the document's words and the configured sources.

#![allow(unsafe_op_in_unsafe_fn)]

use dg_core::linedoc::Pos;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{SIZE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
    DC_PEN, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetStockObject,
    GetTextExtentPoint32W, InvalidateRect, LineTo, MoveToEx, SelectObject, SetBkMode, SetDCPenColor,
    SetTextColor, SetViewportOrgEx, DRAW_TEXT_FORMAT, HGDIOBJ, HFONT, PAINTSTRUCT, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, GWLP_USERDATA, GetCaretPos, GetClientRect, GetWindowLongPtrW,
    WM_NCCALCSIZE,
    IsWindowVisible, KillTimer, MA_NOACTIVATE, SendMessageW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWLP_WNDPROC, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SIZE, WM_TIMER,
    WNDPROC,
};

unsafe fn lb_send(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    SendMessageW(hwnd, msg, Some(wparam), Some(lparam))
}

use crate::win32::window::{high_word, low_word, state_mut, to_colorref, EditorState};
use dg_render::pipeline::LineMetrics;
use dg_render::theme::Color;

/// `CreateWindowExW` style word from the completion list creator.
const POPUP_STYLE: u32 = 0x8020_2110;
/// Its extended style word.
const POPUP_EX_STYLE: u32 = 0x0800_0100;

// They all go through the subclassed window into the system proc, which owns
// the item array and the scrollbar.
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const LB_GETCOUNT: u32 = 0x018B;
const LB_GETTOPINDEX: u32 = 0x018E;
const LB_GETITEMHEIGHT: u32 = 0x01A1;
const LB_SETITEMHEIGHT: u32 = 0x01A0;
const LB_SETCOUNT: u32 = 0x01A7;

/// The popup's own `SetTimer(hwnd, 1, 150)`, the deferred
/// hide. It lives on the popup window, so the id never meets the frame's.
const POPUP_HIDE_TIMER_ID: usize = 1;

/// The frame's 150 ms re-evaluation timer
/// on the view at the view (the completion source).
pub(super) const COMPLETE_REFRESH_TIMER_ID: usize = 10;
pub(super) const COMPLETE_TIMER_MS: u32 = 150;

/// `DT_WORD_ELLIPSIS | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER`.
const DT_ROW: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(0x0004_0824);

/// How far the selected row's bar moves from the popup background toward the
/// text colour, out of 255. It has to stay well short of the text: the row is
/// still drawn *in* that colour, so a bar that approaches it erases the text.
const SEL_STEP: u8 = 64;

/// The same for the 1 px frame around the client.
const BORDER_STEP: u8 = 48;

/// How the typed prefix has to appear in a candidate: `FuzzyMatch`, 1..3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MatchMode {
    Prefix = 1,
    Contains = 2,
    /// The prefix's characters appear in order, anywhere in the word.
    Fuzzy = 3,
}

impl MatchMode {
    pub(super) fn from_setting(v: u32) -> Self {
        match v {
            2 => Self::Contains,
            3 => Self::Fuzzy,
            _ => Self::Prefix,
        }
    }
}

/// One row: the candidate and the source's marker character (the source
/// objects' `vt[1]`; 2 = context words answers 45 = `'-'`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub text: String,
    pub marker: char,
}

/// What an accepted row becomes: the text, and the caret range it replaces.
pub(super) struct Accepted {
    pub text: String,
    pub range: ((usize, usize), (usize, usize)),
}

/// The key filter's verdict, the word-completion key map in one enum.
pub(super) enum KeyAction {
    /// Not the popup's key.
    None,
    /// The key was consumed and did nothing visible (a wrap around the list).
    Swallow,
    /// Consume the key and put this row over the prefix.
    Accept(usize),
}

pub(super) struct ShowInput {
    pub range: ((usize, usize), (usize, usize)),
    pub items: Vec<Entry>,
    /// `GetCaretPos` in the view's client space, and the same point on screen.
    pub caret_client: POINT,
    pub caret_screen: (i32, i32),
    pub view_h: i32,
    pub prefix_px: i32,
    pub row_h: i32,
    pub cell: i32,
}

pub(super) struct WordComplete {
    pub(super) hwnd: HWND,
    /// The system `LISTBOX` proc the subclassed window forwards to.
    old_proc: isize,
    /// `[obj+152]`
    /// visibility: a deferred hide clears the flag at once so the keys are the
    /// editor's again, and the window follows 150 ms later.
    visible: bool,
    /// `[obj+96]` / `[obj+104]`: the candidate array and its count.
    pub(super) items: Vec<Entry>,
    /// `[obj+112]` / `[obj+120]`: the caret at show time and the start of the
    /// typed prefix — the range an accepted candidate replaces.
    range: ((usize, usize), (usize, usize)),
    /// `[obj+144]`: the deferred-hide timer, 0 when it is not armed.
    hide_timer: usize,
    /// A refresh was asked for; the 150 ms timer decides at fire time.
    pub(super) refresh_armed: bool,
    /// The dark ask last given to the list box, so the per-window theme call
    /// only runs when the answer changes.
    dark: Option<bool>,
}

impl WordComplete {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND(std::ptr::null_mut()),
            old_proc: 0,
            visible: false,
            items: Vec::new(),
            range: ((0, 0), (0, 0)),
            hide_timer: 0,
            refresh_armed: false,
            dark: None,
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    fn instance() -> HINSTANCE {
        unsafe {
            GetModuleHandleW(None)
                .map(|m| HINSTANCE(m.0))
                .unwrap_or(HINSTANCE(std::ptr::null_mut()))
        }
    }

    /// The lazy `CreateWindowExW` of the completion list creator, once per process.
    pub(super) fn create(&mut self, frame: HWND) {
        if self.alive() {
            return;
        }
        unsafe {
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE(POPUP_EX_STYLE),
                w!("LISTBOX"),
                PCWSTR::null(),
                WINDOW_STYLE(POPUP_STYLE),
                0,
                0,
                0,
                0,
                Some(frame),
                None,
                Some(Self::instance()),
                None,
            ) else {
                return;
            };
            // The handler reaches the editor state through the parent frame,
            // the same route the overlay bars take.
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, frame.0 as isize);
            self.old_proc = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wc_proc as *const () as usize as isize);
            self.hwnd = hwnd;
        }
    }

    /// The popup's one native part is the scrollbar: the rows are painted here,
    /// the track and the thumb are not. A list box left unnamed opens the light
    /// visual style, so the bar draws `#F0F0F0` beside a dark row. Naming it
    /// `DarkMode_Explorer` is the same per-window ask the dialogs give their
    /// list views; measured on this control it turns the track `#232323` and
    /// the thumb `#959595`.
    pub(super) fn set_dark(&mut self, dark: bool) {
        if !self.alive() || self.dark == Some(dark) {
            return;
        }
        self.dark = Some(dark);
        crate::win32::chrome::dark_theme(self.hwnd, dark);
    }

    /// hide at once and give up the flag.
    pub(super) fn hide_now(&mut self) {
        self.visible = false;
        self.refresh_armed = false;
        self.items.clear();
        if !self.alive() {
            return;
        }
        unsafe {
            if self.hide_timer != 0 {
                let _ = KillTimer(Some(self.hwnd), self.hide_timer);
                self.hide_timer = 0;
            }
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// the flag dies now (so the keyboard is the editor's
    /// again), the window follows on a 150 ms timer unless a refresh cancels it.
    pub(super) fn hide_soon(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        if !self.alive() {
            return;
        }
        unsafe {
            if self.hide_timer != 0 {
                let _ = KillTimer(Some(self.hwnd), self.hide_timer);
            }
            let id = SetTimer(Some(self.hwnd), POPUP_HIDE_TIMER_ID, COMPLETE_TIMER_MS, None);
            self.hide_timer = if id == 0 { 0 } else { POPUP_HIDE_TIMER_ID };
        }
    }

    /// the caret moved, so a visible list stops taking keys.
    pub(super) fn on_caret_moved(&mut self) {
        self.hide_soon();
    }

    /// the completion source's tail: a typed word character arms the view's 150 ms
    /// re-evaluation timer instead of rebuilding the list on every keystroke.
    pub(super) fn schedule(&mut self, frame: HWND) {
        self.refresh_armed = true;
        unsafe {
            let _ = KillTimer(Some(frame), COMPLETE_REFRESH_TIMER_ID);
            let _ = SetTimer(Some(frame), COMPLETE_REFRESH_TIMER_ID, COMPLETE_TIMER_MS, None);
        }
    }

    /// the completion list creator's fill-and-place half, given the data its view queries
    /// would have answered.
    pub(super) fn show(&mut self, input: ShowInput) {
        if !self.alive() {
            return;
        }
        // A deferred hide must not outlive the re-show: the completion list creator kills
        // `[obj+144]` before it fills the list.
        if self.hide_timer != 0 {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), self.hide_timer);
            }
            self.hide_timer = 0;
        }
        self.range = input.range;
        self.items = input.items;
        let longest = self
            .items
            .iter()
            .map(|e| e.text.chars().count())
            .max()
            .unwrap_or(0);
        let g = popup_geometry(
            input.caret_screen,
            input.caret_client.y,
            input.view_h,
            input.prefix_px,
            longest,
            self.items.len(),
            input.row_h,
            input.cell,
        );
        unsafe {
            // Fill on a window that already has its geometry. A list box filled while it
            // is still zero-sized answers `LB_SETCURSEL(0)` by scrolling item 0 out of
            // view, and the opened list then starts at row 2 with nothing highlighted.
            // Its fill never sees the empty size.
            if !IsWindowVisible(self.hwnd).as_bool() {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    g.0,
                    g.1,
                    g.2,
                    g.3,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        let selected_first = self.fill_list();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                g.0,
                g.1,
                g.2,
                g.3,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            // The stored row: the row chosen while the list was still being
            // filled is asserted again once the window has its final size. A
            // list box that is given a selection before it is sized scrolls
            // item 0 out of view, and the opened list then starts on row 2 with
            // nothing highlighted.
            if selected_first {
                let _ = lb_send(self.hwnd, LB_SETCURSEL, WPARAM(0), LPARAM(0));
            }
            // the completion list creator sets the row height only after the window has its
            // final size, for the same reason.
            if !self.items.is_empty() {
                let _ = lb_send(
                    self.hwnd,
                    LB_SETITEMHEIGHT,
                    WPARAM(0),
                    LPARAM(input.row_h as isize),
                );
            }
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
        self.visible = true;
    }

    fn fill_list(&mut self) -> bool {
        unsafe {
            let _ = lb_send(self.hwnd, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
            let _ = lb_send(self.hwnd, LB_SETCOUNT, WPARAM(self.items.len()), LPARAM(0));
            if lb_send(self.hwnd, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 == -1
                && lb_send(self.hwnd, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 > 1
            {
                let _ = lb_send(self.hwnd, LB_SETCURSEL, WPARAM(0), LPARAM(0));
                return true;
            }
            false
        }
    }

    fn count(&self) -> i32 {
        unsafe { lb_send(self.hwnd, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 as i32 }
    }

    fn cursel(&self) -> i32 {
        unsafe { lb_send(self.hwnd, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32 }
    }

    fn set_cursel(&self, idx: i32) {
        unsafe {
            let _ = lb_send(self.hwnd, LB_SETCURSEL, WPARAM(idx as usize), LPARAM(0));
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    /// the word-completion key map — the editor's key filter while the list is up.
    pub(super) fn filter_key(
        &mut self,
        vk: u32,
        alt: bool,
        ctrl: bool,
        shift: bool,
        trigger: u32,
        use_num_key: bool,
    ) -> KeyAction {
        if !self.visible {
            return KeyAction::None;
        }
        if !alt && !ctrl && !shift && vk == trigger {
            return KeyAction::Accept(self.cursel().max(0) as usize);
        }
        if vk == 0x09 && !alt && !ctrl && !shift {
            // Tab confirms
            // key would type is swallowed by the caller's chord-echo gate.
            return KeyAction::Accept(self.cursel().max(0) as usize);
        }
        if vk == 0x28 || vk == 0x26 {
            // Down (40) / Up (38), claimed whatever is held. Both ends wrap
            // around: Down on the last row lands on the first and the other
            // way round, for a list of any length.
            let count = self.count();
            let cur = self.cursel();
            if count > 0 {
                if vk == 0x28 {
                    self.set_cursel(if cur + 1 < count { cur + 1 } else { 0 });
                } else {
                    self.set_cursel(if cur > 0 { cur - 1 } else { count - 1 });
                }
            }
            return KeyAction::Swallow;
        }
        if (0x31..=0x39).contains(&vk) {
            // 1..9. Plain digits need `[WordComplete] UseNumKey`; Alt+digit is
            // the shipped accelerator binding ("默认是ALT+1~9"), so it always
            // works. Ctrl/Shift keep their own meaning.
            let plain = !alt && !ctrl && !shift && use_num_key;
            let accel = alt && !ctrl && !shift;
            if plain || accel {
                let idx = (vk - 0x31) as i32;
                if idx < self.count() {
                    self.set_cursel(idx);
                    return KeyAction::Accept(idx as usize);
                }
            }
        }
        KeyAction::None
    }

    /// WM_CHAR 27 from the word-completion key map: hide at once, but do not claim the
    /// character — the editor still gets its Escape.
    pub(super) fn hide_on_escape(&mut self) {
        if self.visible {
            self.hide_now();
        }
    }

    pub(super) fn accept_action(&mut self, idx: i32) -> Option<Accepted> {
        if idx < 0 || idx as usize >= self.items.len() {
            self.hide_now();
            return None;
        }
        let text = self.items[idx as usize].text.clone();
        let range = self.range;
        self.hide_now();
        Some(Accepted { text, range })
    }

    /// the row under a client point, -1 when none.
    fn hit(&self, pt: POINT) -> i32 {
        unsafe {
            let count = lb_send(self.hwnd, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 as i32;
            if count <= 0 {
                return -1;
            }
            let mut rc = RECT::default();
            if GetClientRect(self.hwnd, &mut rc).is_err() {
                return -1;
            }
            let mut idx = lb_send(self.hwnd, LB_GETTOPINDEX, WPARAM(0), LPARAM(0)).0 as i32;
            let mut y = 0;
            let row_h = lb_send(self.hwnd, LB_GETITEMHEIGHT, WPARAM(0), LPARAM(0)).0 as i32;
            let row_h = if row_h > 0 { row_h } else { 1 };
            while idx < count {
                let row = RECT {
                    left: rc.left,
                    top: y,
                    right: rc.right,
                    bottom: y + row_h,
                };
                if pt.x >= row.left && pt.x < row.right && pt.y >= row.top && pt.y < row.bottom {
                    return idx;
                }
                y += row_h;
                if y > rc.bottom {
                    break;
                }
                idx += 1;
            }
            -1
        }
    }

    /// WM_LBUTTONDOWN: select the row, no accept.
    pub(super) fn click(&mut self, lparam: LPARAM) {
        let pt = POINT {
            x: low_word(lparam),
            y: high_word(lparam),
        };
        let idx = self.hit(pt);
        if idx != -1 {
            self.set_cursel(idx);
        }
    }

    /// WM_LBUTTONDBLCLK: select the row and take it.
    pub(super) fn double_click(&mut self, lparam: LPARAM) -> Option<Accepted> {
        let pt = POINT {
            x: low_word(lparam),
            y: high_word(lparam),
        };
        let idx = self.hit(pt);
        if idx == -1 {
            return None;
        }
        self.set_cursel(idx);
        self.accept_action(idx)
    }

    /// `WM_COMMAND` ids 1204..=1212 from the digit path: select, take.
    pub(super) fn digit_command(&mut self, id: u32) -> Option<Accepted> {
        let idx = id.wrapping_sub(1204) as i32;
        if idx >= 0 && idx < self.count() {
            self.set_cursel(idx);
            return self.accept_action(idx);
        }
        None
    }

    /// The popup's `WM_PAINT`
    pub(super) fn paint(&mut self, bg: Color, fg: Color, font: HFONT, row_h: i32, cell: i32) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            if hdc.0.is_null() {
                return;
            }
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let count = lb_send(self.hwnd, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 as i32;
            if count == 0 || w <= 0 || h <= 0 {
                let _ = EndPaint(self.hwnd, &ps);
                return;
            }
            // The selected row steps a quarter of the way toward the text —
            // far enough to read as a bar, close enough to the background that
            // the text on it keeps its contrast. A step of 160 (the 63 % this
            // used to take) left a light bar under light text on a dark popup.
            let sel_bg = blend(bg, fg, SEL_STEP);
            // The 1 px frame is quieter than the row it encloses.
            let border = blend(bg, fg, BORDER_STEP);
            let row_h = row_h.max(1);
            let cell = cell.max(1);
            // A partially invalidated window never shows a half list.
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
            let _ = SetViewportOrgEx(mem, -ps.rcPaint.left, -ps.rcPaint.top, None);
            let bg_brush = CreateSolidBrush(to_colorref(bg));
            let _ = FillRect(mem, &rc, bg_brush);
            let _ = DeleteObject(HGDIOBJ(bg_brush.0));
            let old_font = SelectObject(mem, HGDIOBJ(font.0));
            let _ = SetBkMode(mem, TRANSPARENT);
            let _ = SetTextColor(mem, to_colorref(fg));
            let _ = SelectObject(mem, GetStockObject(DC_PEN));
            let _ = SetDCPenColor(mem, to_colorref(fg));
            // The digit column is one capital `X` wide, measured in
            // the editor's own font.
            let mut xsz = SIZE::default();
            let _ = GetTextExtentPoint32W(mem, &[b'X' as u16], &mut xsz);
            let cell_x = if xsz.cx > 0 { xsz.cx } else { cell };
            let top = lb_send(self.hwnd, LB_GETTOPINDEX, WPARAM(0), LPARAM(0)).0 as i32;
            let cursel = lb_send(self.hwnd, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
            let end = (top + 10).min(count);
            let mut y = 0;
            for i in top..end {
                let row = RECT {
                    left: rc.left,
                    top: y,
                    right: rc.right,
                    bottom: y + row_h,
                };
                y += row_h;
                if i == cursel {
                    let b = CreateSolidBrush(to_colorref(sel_bg));
                    let _ = FillRect(mem, &row, b);
                    let _ = DeleteObject(HGDIOBJ(b.0));
                }
                let Some(entry) = self.items.get(i as usize) else {
                    continue;
                };
                let mut r = row;
                r.left += 3;
                let mut left = r.left;
                if (0..=8).contains(&i) {
                    // Rows 1..9 wear their digit and its accent underline.
                    let mut digit = [(i as u16) + 0x31];
                    let _ = DrawTextW(mem, &mut digit, &mut r, DT_ROW);
                    let _ = MoveToEx(mem, r.left, r.bottom - 3, None);
                    let _ = LineTo(mem, r.left + cell_x, r.bottom - 3);
                    left = r.left;
                }
                r.left = left + cell_x + 4;
                let mut marker = [if entry.marker == '\0' {
                    ' ' as u16
                } else {
                    entry.marker as u16
                }];
                let _ = DrawTextW(mem, &mut marker, &mut r, DT_ROW);
                let mut msz = SIZE::default();
                let _ = GetTextExtentPoint32W(mem, &marker, &mut msz);
                r.left += 4 + msz.cx;
                let mut text: Vec<u16> = entry.text.encode_utf16().collect();
                let _ = DrawTextW(mem, &mut text, &mut r, DT_ROW);
            }
            // The 1 px frame, `Polyline` over the client corners with
            // `blend(bg, fg, 200)`.
            let frame_brush = CreateSolidBrush(to_colorref(border));
            let _ = FrameRect(mem, &rc, frame_brush);
            let _ = DeleteObject(HGDIOBJ(frame_brush.0));
            let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            let _ = SelectObject(mem, old_font);
            let _ = SelectObject(mem, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

/// The selected row's 160 and the frame's 200.
pub(super) fn blend(bg: Color, fg: Color, alpha: u8) -> Color {
    let t = alpha as u32;
    let ch = |a: u8, b: u8| -> u8 { ((a as u32 * (255 - t) + b as u32 * t) / 255) as u8 };
    Color::rgb(ch(bg.r, fg.r), ch(bg.g, fg.g), ch(bg.b, fg.b))
}

/// the completion list creator's tail. `caret` is in screen space; `caret_client_y` and
/// `view_h` are the view's client space
/// before flipping the list above the caret.
pub(super) fn popup_geometry(
    caret: (i32, i32),
    caret_client_y: i32,
    view_h: i32,
    prefix_px: i32,
    longest: usize,
    count: usize,
    row_h: i32,
    cell: i32,
) -> (i32, i32, i32, i32) {
    let cols = (longest as i32 + 8).min(64);
    let w = ((cols + 1) * cell + 4).max(200);
    let rows = count.min(10) as i32;
    let h = row_h * rows;
    let y = if row_h + h + caret_client_y >= view_h {
        caret.1 - h
    } else {
        caret.1 + row_h
    };
    (caret.0 - prefix_px, y, w, h)
}

/// Lines either side of the caret the word scan reads. A document smaller than
/// twice this is read whole.
const WORD_SCAN_LINES: usize = 5000;
/// The scan stops once it has this many candidates plus one: the popup shows
/// ten rows and the rest are a scroll away, so reading further lines would cost
/// time nobody sees the result of.
const WORD_SCAN_CANDIDATES: usize = 30;
/// Characters of a line the scan reads; the rest of a very long line is not
/// worth the walk.
const WORD_SCAN_LINE_CHARS: usize = 1024;

/// A word character: a letter, a digit or `_`, CJK included.
///
/// The ASCII range answers first — `is_alphanumeric` is a Unicode table
/// lookup, and the completion scan asks this once per character it reads.
pub(super) fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && c.is_alphanumeric())
}

/// Case folding for the match: `to_lowercase` on both sides, with the ASCII
/// range answered by a byte compare.
fn fold_eq(a: char, b: char) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a.to_lowercase().eq(b.to_lowercase())
}

/// The word ending at `col`: its start column and the text.
pub(super) fn word_before_caret(line: &[char], col: usize) -> (usize, String) {
    let col = col.min(line.len());
    let mut start = col;
    while start > 0 && is_word_char(line[start - 1]) {
        start -= 1;
    }
    (start, line[start..col].iter().collect())
}

/// the prefix's characters appear in order in the word.
pub(super) fn fuzzy_subsequence(word: &[char], prefix: &[char], case_sensitive: bool) -> bool {
    if word.len() < prefix.len() {
        return false;
    }
    let mut w = 0;
    for &p in prefix {
        loop {
            if w >= word.len() {
                return false;
            }
            let hit = if case_sensitive {
                word[w] == p
            } else {
                fold_eq(word[w], p)
            };
            w += 1;
            if hit {
                break;
            }
        }
    }
    true
}

/// Whether `word` matches `prefix` under `mode`. `word` is at least as long as
/// the prefix — the caller has already dropped the short ones.
fn word_matches(word: &[char], prefix: &[char], mode: MatchMode) -> bool {
    let hit = |w: &[char]| w.iter().zip(prefix).all(|(a, b)| fold_eq(*a, *b));
    match mode {
        MatchMode::Prefix => hit(&word[..prefix.len()]),
        MatchMode::Contains => word.windows(prefix.len()).any(hit),
        MatchMode::Fuzzy => fuzzy_subsequence(word, prefix, false),
    }
}

/// One pass of the word scan: `push_line` for each line the caller wants read,
/// then `finish`.
///
/// The scan walks the line store's own char slices — it never materializes the
/// document, and a word that does not match allocates nothing. Candidates come
/// out deduplicated case-insensitively, in document order, and (in fuzzy mode)
/// sorted by character count with the sort stable, so equally long words keep
/// their document order.
pub(super) struct WordScan {
    prefix: Vec<char>,
    mode: MatchMode,
    /// Lowercased keys already taken, so a word repeated on every line of the
    /// window still fills one row.
    seen: std::collections::HashSet<Vec<char>>,
    /// The key of the word being tested, reused by every word.
    key: Vec<char>,
    out: Vec<String>,
}

impl WordScan {
    /// `None` when the prefix is shorter than the two characters the match
    /// needs — there is nothing to scan for.
    pub(super) fn new(prefix: &str, mode: MatchMode) -> Option<Self> {
        let p: Vec<char> = prefix.chars().collect();
        if p.len() < 2 {
            return None;
        }
        Some(Self {
            prefix: p,
            mode,
            seen: std::collections::HashSet::new(),
            key: Vec::new(),
            out: Vec::new(),
        })
    }

    pub(super) fn push_line(&mut self, line: &[char]) {
        let mut i = 0;
        while i < line.len() {
            if !is_word_char(line[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < line.len() && is_word_char(line[i]) {
                i += 1;
            }
            self.consider(&line[start..i]);
        }
    }

    fn consider(&mut self, word: &[char]) {
        if word.len() < self.prefix.len() || !word_matches(word, &self.prefix, self.mode) {
            return;
        }
        // The key is the lowercased word. ASCII folds one to one, which lets
        // the key live in a buffer reused by every word; anything else goes
        // through `to_lowercase`, whose result can be longer than its input.
        self.key.clear();
        if word.iter().all(|c| c.is_ascii()) {
            self.key.extend(word.iter().map(|c| c.to_ascii_lowercase()));
        } else {
            self.key.extend(word.iter().flat_map(|c| c.to_lowercase()));
        }
        if self.seen.contains(&self.key[..]) {
            return;
        }
        self.seen.insert(self.key.clone());
        self.out.push(word.iter().collect());
    }

    /// Whether the list is long enough that reading more lines would only add
    /// rows nobody scrolls to.
    pub(super) fn full(&self) -> bool {
        self.out.len() > WORD_SCAN_CANDIDATES
    }

    pub(super) fn finish(mut self) -> Vec<String> {
        if self.mode == MatchMode::Fuzzy {
            self.out.sort_by_key(|w| w.chars().count());
        }
        self.out
    }
}

// ---- the editor-state glue --------------------------------------------------

impl EditorState {
    /// The popup's creation, called from the frame's `WM_CREATE`.
    pub(super) fn complete_create(&mut self, frame: HWND) {
        self.complete.create(frame);
    }

    /// the completion list creator: build the list from the word under the caret and place
    /// the window. `manual` is the 显示自动完成候选词 command (1456), which
    /// skips the "automatic completion switched off" gate
    /// command path calls the same function.
    pub(super) fn complete_refresh(&mut self, frame: HWND, manual: bool) {
        self.complete.refresh_armed = false;
        unsafe {
            let _ = KillTimer(Some(frame), COMPLETE_REFRESH_TIMER_ID);
        }
        if !self.complete.alive() {
            return;
        }
        if !manual && !self.setting_bool("Edit", "AutoComplete", true) {
            self.complete.hide_now();
            return;
        }
        // A live selection is not a word.
        if !self.docs.act().selection.is_empty() || !self.editable() {
            self.complete.hide_now();
            return;
        }
        let (line, col) = self.docs.act().caret;
        let line = line.min(self.docs.act().doc.line_count().saturating_sub(1));
        let mut chars: Vec<char> = Vec::new();
        {
            let mut buf = String::new();
            self.docs.act().doc.line_text(line, &mut buf);
            chars.extend(buf.chars());
        }
        let (start, prefix) = word_before_caret(&chars, col.min(chars.len()));
        // The character before the caret has to be a word character, and two
        // of them is the floor for the prefix match.
        if prefix.chars().count() < 2 {
            self.complete.hide_now();
            return;
        }
        let items = self.complete_candidates(&prefix, manual);
        if items.is_empty() {
            self.complete.hide_now();
            return;
        }
        if items.len() == 1 {
            // the completion list creator's single-candidate arm asks the source whether
            // the candidate is exactly what was typed; if it is, there is
            // nothing to complete and the list stays shut.
            let same = items[0].text.chars().count() == prefix.chars().count()
                && items[0].text.to_lowercase() == prefix.to_lowercase();
            if same {
                self.complete.hide_now();
                return;
            }
        }

        // ---- the position, the completion list creator's tail.
        let mut caret = POINT::default();
        if unsafe { GetCaretPos(&mut caret) }.is_err() {
            self.complete.hide_now();
            return;
        }
        let view = self.view.hwnd();
        let mut origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(view, &mut origin);
        }
        let caret_screen = (origin.x + caret.x, origin.y + caret.y);
        let view_h = self.view.client().1;
        let (row_h, cell) = {
            let t = &self.docs.act().theme;
            (
                t.line_height.max(1.0).round() as i32,
                t.x_width.max(1.0).round() as i32,
            )
        };
        let m = LineMetrics::new(&self.docs.act().theme, self.docs.act().tab_size as usize);
        let prefix_px = m.x_at(&chars, col.min(chars.len()), &[]) - m.x_at(&chars, start, &[]);
        let input = ShowInput {
            range: ((line, start), (line, col.min(chars.len()))),
            items,
            caret_client: caret,
            caret_screen,
            view_h,
            prefix_px,
            row_h,
            cell,
        };
        // 滚动条是弹窗上唯一交给系统的部件：深浅跟着主题的底色走。
        let dark = crate::win32::chrome::theme_is_dark(&self.docs.act().theme);
        self.complete.set_dark(dark);
        self.complete.show(input);
    }

    /// The document's words matching `prefix`, read from the lines around the
    /// caret and no further than a list worth scrolling through.
    ///
    /// The walk is the expensive half — a megabyte of text is a megabyte of
    /// character tests — and it used to run over the whole document on every
    /// refresh, which is what made typing stall. Two bounds keep it small: the
    /// lines within [`WORD_SCAN_LINES`] of the caret (a smaller document is
    /// read whole), and [`WORD_SCAN_CANDIDATES`] rows, after which further
    /// lines would only add rows nobody scrolls to.
    fn doc_words(&mut self, prefix: &str, mode: MatchMode) -> Vec<String> {
        let Some(mut scan) = WordScan::new(prefix, mode) else {
            return Vec::new();
        };
        let (line, count) = {
            let ed = self.docs.act();
            (ed.caret.0, ed.doc.line_count())
        };
        let line = line.min(count.saturating_sub(1));
        let (start, end) = if count <= WORD_SCAN_LINES * 2 {
            (0, count)
        } else {
            (
                line.saturating_sub(WORD_SCAN_LINES),
                (line + WORD_SCAN_LINES).min(count),
            )
        };
        // 逐行读行存储自己的字符切片：不复制整篇文档，没命中的词一个字
        // 节都不分配。
        let lv = self.docs.act().doc.line_vector();
        for i in start..end {
            let view = lv.line_view(i);
            let len = view.len().min(WORD_SCAN_LINE_CHARS);
            scan.push_line(&view[..len]);
            if scan.full() {
                break;
            }
        }
        scan.finish()
    }

    /// The candidate sources this implementation has. This implementation has the
    /// document's words and the LSP, so those are the two rows a user sees.
    pub(super) fn complete_candidates(&mut self, prefix: &str, manual: bool) -> Vec<Entry> {
        let mode = MatchMode::from_setting(
            self.setting_section_key("WordComplete", "FuzzyMatch")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(1),
        );
        let selected = self
            .setting_section_key("WordComplete", "Selected")
            .unwrap_or_else(|| "2".to_string());
        let mut out = Vec::new();
        // 大文件两条来源都不给。词表那一路要读行存储（大文件底下没有它，读一次
        // 就 panic）；LSP 那一路是 5 秒阻塞请求，而且重载把文件撑过阈值时上一轮
        // 的会话还留着，服务器手上只有旧文本。与折叠/tree-sitter 的降级同一条
        // 线。
        let large = self.docs.act().doc.is_large();
        if selected.contains('2') && !large {
            for w in self.doc_words(prefix, mode) {
                out.push(Entry {
                    text: w,
                    marker: '-',
                });
            }
        }
        if manual && self.lsp_allowed() {
            // The own source
            // asked for with Ctrl+Space. Its rows wear `L` beside the marker
            // column so the two kinds stay apart.
            for label in self.lsp_completion_items() {
                if out.iter().any(|e| e.text == label) {
                    continue;
                }
                out.push(Entry {
                    text: label,
                    marker: 'L',
                });
            }
        }
        out
    }

    /// the word-completion key map's entry point from the frame's key dispatch.
    pub(super) fn complete_key(&mut self, frame: HWND, vk: u32) -> bool {
        let trigger = self
            .setting_section_key("WordComplete", "WordTrigger")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|v| matches!(v, 9 | 13 | 27 | 32))
            .unwrap_or(13);
        let use_num_key = self.setting_bool("WordComplete", "UseNumKey", false);
        let action = self.complete.filter_key(
            vk,
            crate::win32::window::alt_held(),
            crate::win32::window::ctrl_held(),
            crate::win32::window::shift_held(),
            trigger,
            use_num_key,
        );
        match action {
            KeyAction::None => false,
            KeyAction::Swallow => true,
            KeyAction::Accept(idx) => {
                self.complete_accept(frame, idx as i32);
                true
            }
        }
    }

    /// put the chosen row over the recorded prefix range.
    pub(super) fn complete_accept(&mut self, frame: HWND, idx: i32) -> bool {
        let Some(a) = self.complete.accept_action(idx) else {
            return false;
        };
        self.complete_apply(frame, a);
        true
    }

    /// The popup's `WM_PAINT`.
    pub(super) fn complete_paint(&mut self) {
        let (bg, fg, font, row_h, cell) = {
            let t = &self.docs.act().theme;
            (
                t.word_complete_bg,
                t.word_complete_fg,
                self.font,
                t.line_height.max(1.0).round() as i32,
                t.x_width.max(1.0).round() as i32,
            )
        };
        self.complete.paint(bg, fg, font, row_h, cell);
    }

    /// The frame's 150 ms timer: re-evaluate the word under the caret.
    pub(super) fn complete_refresh_due(&mut self, frame: HWND) {
        if self.complete.refresh_armed {
            self.complete_refresh(frame, false);
        }
    }
}

// ---- the subclassed window -------------------------------------------------

unsafe extern "system" fn wc_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(st) = state_of(hwnd) else {
        return LRESULT(0);
    };
    if st.complete.hwnd != hwnd {
        return LRESULT(0);
    }
    let old: WNDPROC = core::mem::transmute(st.complete.old_proc);
    let frame = frame_of(hwnd);
    match msg {
        WM_PAINT => {
            st.complete_paint();
            LRESULT(0)
        }
        // The paint fills every pixel; a background erase would flash the
        // system brush through the theme.
        WM_ERASEBKGND => LRESULT(1),
        // MA_NOACTIVATE — the editor keeps the keyboard.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            st.complete.click(lparam);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let accepted = st.complete.double_click(lparam);
            if let Some(a) = accepted {
                st.complete_apply(frame, a);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => LRESULT(0),
        // the wheel and the right button go to the list box
        // (its scrollbar and its top index), then the paint follows.
        WM_RBUTTONDOWN | WM_MOUSEWHEEL => {
            let r = CallWindowProcW(old, hwnd, msg, wparam, lparam);
            let _ = InvalidateRect(Some(hwnd), None, true);
            r
        }
        WM_TIMER if wparam.0 == POPUP_HIDE_TIMER_ID => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = KillTimer(Some(hwnd), POPUP_HIDE_TIMER_ID);
            st.complete.hide_timer = 0;
            LRESULT(0)
        }
        // the digit commands 1204..=1212.
        WM_COMMAND => {
            let id = wparam.0 as u32 & 0xFFFF;
            if (1204..=1212).contains(&id) {
                let accepted = st.complete.digit_command(id);
                if let Some(a) = accepted {
                    st.complete_apply(frame, a);
                }
                return LRESULT(0);
            }
            CallWindowProcW(old, hwnd, msg, wparam, lparam)
        }
        WM_SIZE => {
            // the handler answers the size change by sending itself
            // `WM_NCCALCSIZE` (0x83) and then repainting. That round trip is
            // what lets the system list box recompute its client area: the row
            // page and the scrollbar both depend on it, and without it the box
            // keeps the page it had at 0 x 0 — the opened list then starts on
            // row 2 and grows a scrollbar it does not need. A window of this
            // class also repaints its page from `WM_SIZE` alone; a list box
            // created at its final size never needs the round trip, which is
            // why skipping it looks harmless until the box is created empty.
            let _ = CallWindowProcW(old, hwnd, msg, wparam, lparam);
            let _ = lb_send(hwnd, WM_NCCALCSIZE, WPARAM(0), LPARAM(0));
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        _ => CallWindowProcW(old, hwnd, msg, wparam, lparam),
    }
}

impl EditorState {
    /// The half of the accept only the editor can run: the text is already
    /// chosen and the popup is closed.
    pub(super) fn complete_apply(&mut self, frame: HWND, a: Accepted) {
        let ((sl, sc), (el, ec)) = a.range;
        let lines = self.docs.act().doc.line_count();
        if sl >= lines || el >= lines {
            return;
        }
        let start = Pos {
            line: sl,
            col: sc.min(self.docs.act().doc.line_len(sl)),
        };
        let end = Pos {
            line: el,
            col: ec.min(self.docs.act().doc.line_len(el)),
        };
        if end < start {
            return;
        }
        self.docs.act_mut().apply_replace(start, end, &a.text);
        self.docs.act_mut().caret = (sl, sc + a.text.chars().count());
        self.after_edit(frame);
    }
}

/// The editor state of the frame this popup was created on.
unsafe fn state_of(hwnd: HWND) -> Option<&'static mut EditorState> {
    let root = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if root == 0 {
        return None;
    }
    state_mut(HWND(root as *mut core::ffi::c_void))
}

unsafe fn frame_of(hwnd: HWND) -> HWND {
    HWND(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut core::ffi::c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_walks_from_the_background_toward_the_text() {
        let bg = Color::rgb(0, 0, 128);
        let fg = Color::rgb(255, 255, 255);
        // 160/255 = 0.627 of the way to fg
        let sel = blend(bg, fg, 160);
        assert_eq!(sel.r, 160);
        assert_eq!(sel.g, 160);
        assert_eq!(sel.b, (128 + ((255u32 - 128) * 160) / 255) as u8);
        let edge = blend(bg, fg, 200);
        assert_eq!(edge.r, 200);
        assert_eq!(blend(bg, fg, 0), bg);
    }

    /// 选中行的底和字色之间得留够距离：那一行**还是用字色画的**，底色走近了字就
    /// 没了。以前走 160/255，深色弹窗上是浅底浅字（1.9:1），整块弹窗又一整片蓝。
    #[test]
    fn the_selected_row_leaves_its_text_readable() {
        // WCAG 的相对亮度与对比度。
        fn lum(c: Color) -> f32 {
            fn ch(v: u8) -> f32 {
                let v = v as f32 / 255.0;
                if v <= 0.039_28 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
        }
        let ratio = |a: Color, b: Color| {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        // 随包的深色主题（`DogEdit_Dark.ini` 的 `WordComplete`）。
        let bg = Color::rgb(0x2A, 0x2D, 0x34);
        let fg = Color::rgb(0xD9, 0xDF, 0xE8);
        let bar = blend(bg, fg, SEL_STEP);
        assert!(
            ratio(bar, fg) > 4.5,
            "选中行上的字只剩 {:.2}:1",
            ratio(bar, fg)
        );
        // 反过来也得看得出来是选中了。
        assert!(ratio(bar, bg) > 1.3, "选中行和别的行分不出来");
    }

    #[test]
    fn the_prefix_is_the_word_ending_at_the_caret() {
        let line: Vec<char> = "foo bar_baz qux".chars().collect();
        // `_` is a word character too.
        assert_eq!(word_before_caret(&line, 8), (4, "bar_".into()));
        assert_eq!(word_before_caret(&line, 4), (4, "".into()));
        assert_eq!(word_before_caret(&line, 3), (0, "foo".into()));
        assert_eq!(word_before_caret(&line, 99), (12, "qux".into()));
    }

    /// The whole-text form of the scan: one chunk holding every line.
    fn scan(text: &str, prefix: &str, mode: MatchMode) -> Vec<String> {
        let line: Vec<char> = text.chars().collect();
        let Some(mut scan) = WordScan::new(prefix, mode) else {
            return Vec::new();
        };
        scan.push_line(&line);
        scan.finish()
    }

    #[test]
    fn a_prefix_shorter_than_two_letters_makes_no_list() {
        let text = "alpha beta gamma";
        for mode in [MatchMode::Prefix, MatchMode::Contains, MatchMode::Fuzzy] {
            assert!(scan(text, "a", mode).is_empty());
        }
        assert_eq!(scan(text, "al", MatchMode::Prefix), vec!["alpha".to_string()]);
    }

    #[test]
    fn the_three_modes_are_prefix_contains_and_subsequence() {
        let text = "alpha beta gamma alphabet";
        assert_eq!(
            scan(text, "alp", MatchMode::Prefix),
            vec!["alpha".to_string(), "alphabet".to_string()]
        );
        assert_eq!(
            scan(text, "pha", MatchMode::Contains),
            vec!["alpha".to_string(), "alphabet".to_string()]
        );
        // g..m..a appears inside gamma only
        assert_eq!(scan(text, "gma", MatchMode::Fuzzy), vec!["gamma".to_string()]);
        // fuzzy keeps length order: the shorter word first
        assert_eq!(
            scan("gamut gamma", "gm", MatchMode::Fuzzy),
            vec!["gamut".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn matching_ignores_case_and_keeps_the_first_spelling() {
        let text = "Alpha alpha ALPHA";
        assert_eq!(scan(text, "al", MatchMode::Prefix), vec!["Alpha".to_string()]);
    }

    /// A word never spans a line break, so scanning line by line and scanning
    /// the joined text answer the same list — the document path relies on it.
    #[test]
    fn line_by_line_scanning_matches_the_joined_text() {
        let lines: Vec<Vec<char>> = ["alpha beta", "alphabet", "  alpha_beta_"]
            .iter()
            .map(|l| l.chars().collect())
            .collect();
        let mut walk = WordScan::new("al", MatchMode::Prefix).unwrap();
        for l in &lines {
            walk.push_line(l);
        }
        assert_eq!(
            walk.finish(),
            scan("alpha beta\nalphabet\n  alpha_beta_", "al", MatchMode::Prefix)
        );
        // and the CJK range still counts as word characters
        assert_eq!(
            scan("\u{4e2d}\u{6587}\u{6d4b}\u{8bd5}", "\u{4e2d}\u{6587}", MatchMode::Prefix),
            vec!["\u{4e2d}\u{6587}\u{6d4b}\u{8bd5}".to_string()]
        );
    }

    #[test]
    fn the_fuzzy_match_is_an_in_order_subsequence() {
        let w: Vec<char> = "gamma".chars().collect();
        let p: Vec<char> = "gma".chars().collect();
        assert!(fuzzy_subsequence(&w, &p, false));
        let p2: Vec<char> = "mag".chars().collect();
        assert!(!fuzzy_subsequence(&w, &p2, false));
    }

    /// the completion list creator: width `(min(longest + 8, 64) + 1) × cell + 4`, floored at
    /// 200 px; height `row × min(count, 10)`; the list flips above the caret
    /// when a row and the list do not fit below it.
    #[test]
    fn popup_geometry_measures_cells_and_flips_above_the_bottom_edge() {
        // 8 px cells, 20 px rows, the caret a fifth of the way down the view:
        // `(min(10 + 8, 64) + 1) × 8 + 4` is 156, under the 200 px floor.
        let below = popup_geometry((300, 400), 100, 1000, 24, 10, 5, 20, 8);
        assert_eq!(below, (300 - 24, 400 + 20, 200, 100));
        // the same caret near the bottom flips the list up
        let above = popup_geometry((300, 400), 950, 1000, 24, 10, 5, 20, 8);
        assert_eq!(above.1, 400 - 100);
        // 64 columns is the cap, and 200 px the floor
        let wide = popup_geometry((0, 0), 0, 1000, 0, 1000, 1, 20, 8);
        assert_eq!(wide.2, 524);
        let narrow = popup_geometry((0, 0), 0, 1000, 0, 0, 1, 20, 8);
        assert_eq!(narrow.2, 200);
        // ten rows at most
        let many = popup_geometry((0, 0), 0, 5000, 0, 0, 40, 20, 8);
        assert_eq!(many.3, 200);
    }
}
