//! The find bar: a native modeless dialog in the view's top-right corner
//! (`cm_find_mini`). Showing it again only asks `IsWindow` / `IsWindowVisible` /
//! `ShowWindow(SW_SHOW)`, then prefills the edit and moves the caret into it. The
//! prefill deliberately goes through the view: the bar asks what is selected and the
//! view hands back the word under the caret as an `HGLOBAL`. The rectangles below are
//! the bar's layout and its only source. Like [`prompt`](crate::win32::prompt), the
//! children are created at pixel rectangles rather than from `DLGTEMPLATE` items —
//! dialog-unit padding is not observable on a bar with no items, while the child rects
//! are. The window itself still comes from `CreateDialogIndirectParamW`, so it is a
//! real `#32770` and the dialog manager owns Tab, Enter and Escape. That is the point:
//! those three, plus the caret, the IME and the focus return, stopped being this
//! project's code.
//!
//! Colour stays with `Theme`: the bar answers `WM_ERASEBKGND` and the edit
//! `WM_CTLCOLOREDIT` with theme brushes, and each button of the command strip is a
//! `BS_OWNERDRAW` `BUTTON` we paint at `WM_DRAWITEM` with `FillRect` and `DrawTextW`.
//! A toolbar would have been the natural widget and was dropped because it left no
//! colour hook: it answered `CDDS_PREPAINT` and never the item stage, ignored
//! `TBCUSTOMDRAW`'s `clrText`/`clrBack`/`hbrBkgnd` at the stage it did send, and
//! `TB_SETBKCOLOR` is no longer even in the SDK — so it stayed `COLOR_3DFACE` `#F0F0F0`
//! with black glyphs over our dark bar. An owner-drawn button keeps the parts that
//! have to be native — the hit-test, the focus, the toggle state, the `WM_COMMAND` —
//! and hands the pixels to us. The bar is `panel_bg` `#2D2D2D` over `background`
//! `#202020` with `indent_line` `#464646` edges. The editor has no all-matches painting
//! for the bar to toggle, and a control that answers clicks and changes nothing is not
//! a feature worth a pixel.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dpi::scale;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, ClientToScreen, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, FillRect, GetDC, GetTextExtentPoint32W, HBRUSH,
    HDC, HFONT, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETSEL, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_OWNERDRAW, CreateDialogIndirectParamW, CreateWindowExW, DLGTEMPLATE,
    DestroyWindow, EN_CHANGE, ES_AUTOHSCROLL, GA_ROOT, GWLP_USERDATA, GetAncestor, GetClientRect,
    GetDlgCtrlID, GetDlgItem, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDCANCEL, IDOK, IsDialogMessageW, MSG,
    PostMessageW, SM_CXBORDER, SW_SHOW, SWP_NOACTIVATE, SendMessageW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_INITDIALOG, WM_SETFONT,
    WM_SETTEXT, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_POPUP, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::win32::chrome;
use crate::win32::customdraw;
use crate::win32::ownerdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_render::theme::{Theme, luma_blend};

const BUTTON_CLASS: PCWSTR = w!("Button");
const EDIT_CLASS: PCWSTR = w!("Edit");

/// Control ids
pub(super) const ID_QUERY: i32 = 1_200;
pub(super) const ID_REGEX: i32 = 1_201;
pub(super) const ID_CASE: i32 = 1_202;
pub(super) const ID_WORD: i32 = 1_203;
pub(super) const ID_WRAP: i32 = 1_204;
pub(super) const ID_HIGHLIGHT: i32 = 1_205;
pub(super) const ID_CLOSE: i32 = 1_206;
pub(super) const ID_NEXT: i32 = 1_207;
pub(super) const ID_PREV: i32 = 1_208;
pub(super) const ID_FIND_ALL: i32 = 1_209;
/// The two rows the bar grows into (replacement text, find-in-files root). The
/// measured mini bar is one row, so these ids are ours.
pub(super) const ID_REPLACE: i32 = 1_210;
pub(super) const ID_WHERE: i32 = 1_211;
pub(super) const ID_REPLACE_ONE: i32 = 1_212;
pub(super) const ID_REPLACE_ALL: i32 = 1_213;
pub(super) const ID_SEARCH_FILES: i32 = 1_214;

/// Base (100%) geometry in px: client 451x24, edit 252x24 at (1,1), toolbar
/// 196x24 at (256,1) — the 125% rectangles divided by 1.25.
pub(super) const ROW_H: i32 = 24;
pub(super) const CLIENT_W: i32 = 452;
const EDIT_X: i32 = 1;
const EDIT_W: i32 = 252;
/// Where the strip starts, and how much room it has: the 196 px, which
/// is `CLIENT_W - TOOL_X`.
const TOOL_X: i32 = 256;
/// Breathing room either side of a strip button's glyph.
const BUTTON_PAD: i32 = 5;

/// How far the bar's right edge sits inside the editor's right edge. The
/// measured bar's right edge was 25 px inside the view's; the frame's own
/// overlay inset is kept so the bar lines up with what was there before.
/// The frame subtracts it from the editor's right edge before passing the
/// corner in.
pub(super) const INSET: i32 = 8;

/// The bar's one instance
static BAR: AtomicIsize = AtomicIsize::new(0);

/// The message the bar posts to the frame — its only way to act.
pub(super) const WM_BAR: u32 = WM_APP + 0x41;
/// An edit's content changed; `lParam` is the edit's id.
pub(super) const BAR_TEXT: usize = 1;
/// Enter, in whichever edit has the caret; `lParam` is that edit's id.
pub(super) const BAR_ENTER: usize = 2;
/// Escape.
pub(super) const BAR_DISMISS: usize = 3;
/// A toolbar button was clicked; `lParam` is its id.
pub(super) const BAR_BUTTON: usize = 4;

fn bar_hwnd() -> Option<HWND> {
    let v = BAR.load(Ordering::Acquire);
    (v != 0).then_some(HWND(v as *mut core::ffi::c_void))
}

/// Whether the frame's loop should feed `msg` to the bar's dialog manager.
/// `GA_ROOT` is what makes a keystroke in the edit belong to the bar.
pub(super) fn translate(msg: &MSG) -> bool {
    let Some(bar) = bar_hwnd() else { return false };
    let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if root != bar {
        return false;
    }
    unsafe { IsDialogMessageW(bar, msg).as_bool() }
}

/// Everything the bar shows: a snapshot of the frame's find model, taken when
/// the bar is asked to appear.
pub(super) struct Request<'a> {
    pub frame: HWND,
    /// Frame-client point of the bar's top-right corner, so the bar hangs off
    /// the view's corner the way the bar's own does.
    pub anchor: POINT,
    /// 1 = query, 2 = +replacement, 3 = +find-in-files root.
    pub rows: usize,
    pub query: String,
    pub replace: String,
    pub where_: String,
    pub opts: dg_core::FindOptions,
    pub wrap: bool,
    pub highlight: bool,
    pub theme: &'a Theme,
}

/// The bar's state, reached through `GWLP_USERDATA` like every other window
/// here. It owns the GDI objects the controls are coloured with, so a bar that
/// is destroyed with a theme change cannot leak or paint half-and-half.
struct Bar {
    frame: HWND,
    rows: usize,
    theme: Theme,
    font: HFONT,
    bg: HBRUSH,
    lit_bg: HBRUSH,
    /// The two faces under the cursor: one step of ink over `bg` and over
    /// `lit_bg`, so a toggle that is on still reacts when the mouse arrives.
    bg_hot: HBRUSH,
    lit_hot: HBRUSH,
    edit_bg: HBRUSH,
    /// The strip's control tips: one shared
    /// tooltip control hosting every button's
    /// text.
    tip: Option<crate::win32::tiphost::TipHost>,
    /// The four option toggles, as the model last pushed them. The strip's
    /// buttons are `BS_OWNERDRAW`, which cannot also be `BS_AUTOCHECKBOX` (both
    /// live in `BS_TYPEMASK`), so the checked state is not something the control
    /// can hold for us. It is still not ours to *decide*: the frame owns
    /// `FindOptions` and this is only what the drawing reads.
    opts: AtomicU32,
}

unsafe fn state(hwnd: HWND) -> &'static Bar {
    &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Bar)
}

fn outer_size(rows: usize, dpi: f32) -> (i32, i32) {
    let edge = unsafe { GetSystemMetrics(SM_CXBORDER) }.max(1);
    (
        scale(CLIENT_W, dpi) + 2 * edge,
        scale(ROW_H * rows as i32, dpi) + 2 * edge,
    )
}

/// Show the bar, creating it when it is not there yet
/// `IsWindow` / `IsWindowVisible` / `ShowWindow` sequence.
pub(super) fn show(req: Request<'_>) {
    unsafe {
        let dpi = GetDpiForWindow(req.frame).max(96) as f32 / 96.0;
        let (w, h) = outer_size(req.rows, dpi);
        let pos = bar_pos(req.frame, req.anchor, w);

        if let Some(existing) = bar_hwnd() {
            if state(existing).rows == req.rows {
                set_pos(existing, pos, w, h);
                sync(existing, &req);
                let _ = ShowWindow(existing, SW_SHOW);
                focus_edit(existing, ID_QUERY);
                return;
            }
            // A different row count means different children.
            let _ = DestroyWindow(existing);
        }
        create(req, pos, w, h, dpi);
    }
}

/// Follow the frame: `WM_SIZE`/`WM_MOVE`, and any change to the dock bands,
/// keep the bar pinned to the corner. `anchor` is the editor's top-right
/// corner in frame-client coordinates — the frame's own origin is above the
/// toolbar and tab bands, which is why this module does not compute it.
pub(super) fn reposition(frame: HWND, anchor: POINT) {
    let Some(bar) = bar_hwnd() else { return };
    unsafe {
        let rows = state(bar).rows;
        let dpi = GetDpiForWindow(frame).max(96) as f32 / 96.0;
        let (w, h) = outer_size(rows, dpi);
        let pos = bar_pos(frame, anchor, w);
        set_pos(bar, pos, w, h);
    }
}

/// The bar's screen position from a frame-client anchor. An editor narrower
/// than the bar (a wide host, a small window) must not push it off the left
/// edge of the frame, so the anchor's x has a floor of one bar width. The y
/// is left alone: pulling it into a short frame would put the bar back over
/// the toolbar bands.
unsafe fn bar_pos(frame: HWND, anchor: POINT, w: i32) -> (i32, i32) {
    let mut rc = RECT::default();
    let _ = GetClientRect(frame, &mut rc);
    let mut pt = POINT {
        x: anchor_x(&rc, anchor.x, w),
        y: anchor.y,
    };
    let _ = ClientToScreen(frame, &mut pt);
    (pt.x - w, pt.y)
}

fn anchor_x(rc: &RECT, x: i32, w: i32) -> i32 {
    x.max(rc.left + w)
}

unsafe fn set_pos(bar: HWND, pos: (i32, i32), w: i32, h: i32) {
    // A layout pass re-anchors the bar even when nothing moved. Skipping the
    // move keeps the caret's window from repainting on every wrap refresh.
    let mut rc = RECT::default();
    if GetWindowRect(bar, &mut rc).is_ok()
        && rc.left == pos.0
        && rc.top == pos.1
        && rc.right - rc.left == w
        && rc.bottom - rc.top == h
    {
        return;
    }
    let _ = SetWindowPos(bar, None, pos.0, pos.1, w, h, SWP_NOACTIVATE);
}

/// Hide and forget: the bar holds no state the frame does not already own.
pub(super) fn hide() {
    unsafe {
        if let Some(bar) = bar_hwnd() {
            let _ = DestroyWindow(bar);
        }
    }
}

/// Hand the query edit new text with the whole line selected, so typing
/// replaces it
pub(super) fn set_query(text: &str) {
    let Some(bar) = bar_hwnd() else { return };
    unsafe {
        let Ok(edit) = GetDlgItem(Some(bar), ID_QUERY) else {
            return;
        };
        set_text(edit, text);
        let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        let _ = SetFocus(Some(edit));
    }
}

/// Put the caret back in the query field. A toolbar click moves focus into the
/// toolbar, and the next keystroke would otherwise be lost there.
pub(super) fn focus_query() {
    if let Some(bar) = bar_hwnd() {
        unsafe {
            focus_edit(bar, ID_QUERY);
        }
    }
}

pub(super) fn text(id: i32) -> String {
    let Some(bar) = bar_hwnd() else {
        return String::new();
    };
    unsafe {
        let Ok(ctl) = GetDlgItem(Some(bar), id) else {
            return String::new();
        };
        let len = GetWindowTextLengthW(ctl);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(ctl, &mut buf);
        buf.truncate(n.max(0) as usize);
        String::from_utf16_lossy(&buf)
    }
}

unsafe fn set_text(edit: HWND, text: &str) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let _ = SendMessageW(
        edit,
        WM_SETTEXT,
        Some(WPARAM(0)),
        Some(LPARAM(buf.as_ptr() as isize)),
    );
}

unsafe fn focus_edit(bar: HWND, id: i32) {
    if let Ok(edit) = GetDlgItem(Some(bar), id) {
        let _ = SetFocus(Some(edit));
    }
}

/// Push the model into an existing bar: text that differs, and the option
/// checks. A field the user is typing in is never overwritten, which is why
/// empty model text is skipped rather than pushed.
unsafe fn sync(bar: HWND, req: &Request<'_>) {
    if req.rows > 0 && !req.query.is_empty() {
        set_query(&req.query);
    }
    for (id, want) in [(ID_REPLACE, &req.replace), (ID_WHERE, &req.where_)]
        .into_iter()
        .take(req.rows.saturating_sub(1))
    {
        if want.is_empty() {
            continue;
        }
        if let Ok(edit) = GetDlgItem(Some(bar), id) {
            if text(id) != *want {
                set_text(edit, want);
            }
        }
    }
    apply_opts(bar, req.opts, req.wrap, req.highlight);
}

/// Re-push the option bits after the frame flipped one. The bar is a view: it
/// never decides what is on, it only draws what it was told.
pub(super) fn set_opts(opts: dg_core::FindOptions, wrap: bool, highlight: bool) {
    if let Some(bar) = bar_hwnd() {
        unsafe { apply_opts(bar, opts, wrap, highlight) };
    }
}

/// The five toggle buttons, in the order their bits are stored.
const OPT_IDS: [i32; 5] = [ID_REGEX, ID_CASE, ID_WORD, ID_WRAP, ID_HIGHLIGHT];

/// Push the model's option bits into the bar, and repaint the buttons whose lit
/// state they drive.
unsafe fn apply_opts(bar: HWND, opts: dg_core::FindOptions, wrap: bool, highlight: bool) {
    let mask = (opts.regex as u32)
        | ((opts.case_sensitive as u32) << 1)
        | ((opts.whole_word as u32) << 2)
        | ((wrap as u32) << 3)
        | ((highlight as u32) << 4);
    let bar_state = state(bar);
    bar_state.opts.store(mask, Ordering::Relaxed);
    for id in OPT_IDS {
        if let Ok(btn) = GetDlgItem(Some(bar), id) {
            // An owner-drawn control is not redrawn by a state change it did not
            // see coming. No erase: `WM_DRAWITEM` fills the whole face itself,
            // and an erase first would put the button class's light face under
            // a dark strip for one frame.
            let _ = InvalidateRect(Some(btn), None, false);
        }
    }
}

/// Item-less `DLGTEMPLATE`. The three trailing words are "no menu, the default
/// dialog class, no title", which is what makes the window a `#32770`.
#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

/// A row's buttons: id, the glyph or word on it, whether it is a toggle, and
/// the control tip text the strip's tooltip shows over it.
type Buttons<'a> = &'a [(i32, &'a str, bool, &'a str)];

const FIND_BUTTONS: Buttons = &[
    (ID_NEXT, "▼", false, "Find next"),
    (ID_PREV, "▲", false, "Find previous"),
    (ID_REGEX, ".*", true, "Regular expression"),
    (ID_CASE, "Aa", true, "Case sensitive"),
    (ID_WORD, "\"\"", true, "Whole word"),
    (ID_WRAP, "↺", true, "Wrap"),
    (ID_HIGHLIGHT, "▤", true, "Highlight found text"),
    (ID_FIND_ALL, "≡", false, "Find all"),
    (ID_CLOSE, "✖", false, "Close"),
];
const REPLACE_BUTTONS: Buttons = &[
    (ID_REPLACE_ONE, "替换", false, "替换当前匹配"),
    (ID_REPLACE_ALL, "全部", false, "替换全部匹配"),
];
const FILES_BUTTONS: Buttons = &[(ID_SEARCH_FILES, "搜文件", false, "在文件中查找")];

unsafe fn create(req: Request<'_>, pos: (i32, i32), w: i32, h: i32, dpi: f32) {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_BAR_CLASSES,
        ..Default::default()
    });
    let tmpl = Template {
        head: DLGTEMPLATE {
            style: (WS_POPUP | WS_BORDER | WS_CLIPCHILDREN).0,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 64,
            cy: 16,
        },
        // All three optional words zero, exactly as `RC.exe` emits them: no
        // menu, the predefined `#32770` class, an empty caption. A `0xFFFF`
        // here is not "none" to the dialog manager — it reads the *next* word
        // as a menu ordinal and fails with `ERROR_RESOURCE_TYPE_NOT_FOUND`
        // looking for a menu in an executable that has none.
        tail: [0, 0, 0],
    };
    let boxed = Box::new(Bar {
        frame: req.frame,
        rows: req.rows,
        theme: req.theme.clone(),
        font: ui_font(dpi),
        bg: CreateSolidBrush(to_colorref(req.theme.panel_bg)),
        lit_bg: CreateSolidBrush(to_colorref(req.theme.panel_active)),
        bg_hot: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.foreground,
            req.theme.panel_bg,
            0.08,
        ))),
        lit_hot: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.foreground,
            req.theme.panel_active,
            0.08,
        ))),
        edit_bg: CreateSolidBrush(to_colorref(req.theme.background)),
        // The control tips hang off the frame — the bar window
        // does not exist until the dialog manager returns, and
        // every row builder below reads this field.
        tip: crate::win32::tiphost::TipHost::create(req.frame),
        opts: AtomicU32::new(0),
    });
    let param = LPARAM(Box::into_raw(boxed) as isize);
    let bar = match CreateDialogIndirectParamW(
        Some(instance()),
        &tmpl.head,
        Some(req.frame),
        Some(dialog_proc),
        param,
    ) {
        Ok(h) => h,
        Err(_) => {
            drop(Box::from_raw(param.0 as *mut Bar));
            return;
        }
    };
    BAR.store(bar.0 as isize, Ordering::Release);
    // The find bar owns every control it shows and never asks for the dark
    // children pass, so `WM_CTLCOLORDLG` has to be hooked on it explicitly.
    chrome::hook_dialog_colours(bar);

    let rows: [(i32, Buttons, &str); 3] = [
        (ID_QUERY, FIND_BUTTONS, &req.query),
        (ID_REPLACE, REPLACE_BUTTONS, &req.replace),
        (ID_WHERE, FILES_BUTTONS, &req.where_),
    ];
    for row in 0..req.rows.min(3) {
        let (edit_id, buttons, text) = rows[row];
        build_row(bar, row, edit_id, text, buttons, dpi);
    }
    // Enter and Escape only become `WM_COMMAND IDOK`/`IDCANCEL` if the dialog
    // has a default and a cancel button to click. These two are the ones the
    // manager reads.
    hidden_button(bar, IDOK.0, BS_DEFPUSHBUTTON);
    hidden_button(bar, IDCANCEL.0, 0);

    apply_opts(bar, req.opts, req.wrap, req.highlight);
    set_pos(bar, pos, w, h);
    let _ = ShowWindow(bar, SW_SHOW);
    focus_edit(bar, ID_QUERY);
    if !req.query.is_empty() {
        if let Ok(edit) = GetDlgItem(Some(bar), ID_QUERY) {
            let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }
}

/// One row: the edit, then its toolbar of text buttons, at the row's
/// offsets.
unsafe fn build_row(bar: HWND, row: usize, edit_id: i32, text: &str, buttons: Buttons, dpi: f32) {
    let font = state(bar).font;
    // The row starts 1 px inside the client and is a whole row tall.
    let top = scale(ROW_H * row as i32, dpi) + 1;
    let h = scale(ROW_H, dpi);
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    if let Ok(edit) = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        EDIT_CLASS,
        PCWSTR::from_raw(buf.as_ptr()),
        WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32),
        scale(EDIT_X, dpi),
        top,
        scale(EDIT_W, dpi),
        h,
        Some(bar),
        Some(HMENU(edit_id as usize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    ) {
        set_font(edit, font);
    }
    // The command strip: one owner-drawn `BUTTON` per command, laid out left to
    // right from `TOOL_X`. A text-only toolbar in this process offered no
    // colour hook at all (see the module header), so the pixels are ours while
    // everything that has to stay native does — the hit-test, the press, and the
    // `WM_COMMAND` that reports the click. None of
    // these are tab stops.
    let mut x = scale(TOOL_X, dpi);
    for (id, label, _, tip_text) in buttons {
        let w = 2 * scale(BUTTON_PAD, dpi) + text_width(label, font, dpi);
        let style = WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0 | BS_OWNERDRAW as u32);
        let Ok(btn) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            BUTTON_CLASS,
            PCWSTR::null(),
            style,
            x,
            top,
            w,
            h,
            Some(bar),
            Some(HMENU(*id as usize as *mut core::ffi::c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        // The strip's buttons are the only owner-drawn controls in this bar.
        ownerdraw::install_handle(btn);
        set_font(btn, font);
        // The button's control tip: the dialog section's caption for its id,
        // which is also what the language files localize.
        if !tip_text.is_empty() {
            if let Some(tip) = &state(bar).tip {
                let text = crate::win32::lang::dlg(295, *id as u32).unwrap_or(tip_text);
                tip.add_window_tool(btn, text);
            }
        }
        x += w;
    }
}

/// The label's advance in the bar's font, so a button is as wide as its glyph
/// plus [`BUTTON_PAD`] on each side.
unsafe fn text_width(label: &str, font: HFONT, dpi: f32) -> i32 {
    let hdc = GetDC(None);
    let face = SelectObject(hdc, HGDIOBJ(font.0));
    let buf: Vec<u16> = label.encode_utf16().collect();
    let mut sz = SIZE::default();
    let ok = GetTextExtentPoint32W(hdc, &buf, &mut sz).as_bool();
    SelectObject(hdc, face);
    let _ = ReleaseDC(None, hdc);
    if ok {
        sz.cx
    } else {
        scale(BUTTON_PAD * 4, dpi)
    }
}

unsafe fn set_font(ctl: HWND, font: HFONT) {
    let _ = SendMessageW(
        ctl,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
}

/// A `BUTTON` that is never shown: it exists so the dialog manager has a
/// default and a cancel command to post.
unsafe fn hidden_button(bar: HWND, id: i32, extra: i32) {
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        BUTTON_CLASS,
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | extra as u32),
        0,
        0,
        0,
        0,
        Some(bar),
        Some(HMENU(id as usize as *mut core::ffi::c_void)),
        Some(instance()),
        None,
    );
}

/// The bar's font: the shell's dialog face at the bar's DPI, not the editor's
/// mono face — these glyphs are proportional.
fn ui_font(dpi: f32) -> HFONT {
    unsafe {
        let mut face: Vec<u16> = "Segoe UI".encode_utf16().collect();
        face.push(0);
        CreateFontW(
            -scale(12, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR::from_raw(face.as_ptr()),
        )
    }
}

/// The bar proc. `DLGPROC` is a plain `-> isize` fn pointer in the `windows`
/// metadata, so the returns are 0/1 rather than `LRESULT`.
unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            // TRUE: the manager sets the initial focus, which lands on the
            // query edit because it is the first tab stop in the creation
            // order.
            1
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_DRAWITEM => on_draw_item(hwnd, lparam),
        WM_ERASEBKGND => {
            // The `#32770` class ships a `COLOR_3DFACE` brush; the strip
            // between and behind the controls has to come from Theme.
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state(hwnd).bg);
            }
            1
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let bar = state(hwnd);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut _));
            let is_edit = id == ID_QUERY || id == ID_REPLACE || id == ID_WHERE;
            let (fg, bg, brush) = if is_edit {
                (bar.theme.foreground, bar.theme.background, bar.edit_bg)
            } else {
                (bar.theme.panel_fg, bar.theme.panel_bg, bar.bg)
            };
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), to_colorref(fg), to_colorref(bg), brush)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Bar;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            BAR.store(0, Ordering::Release);
            if !ptr.is_null() {
                let mut b = Box::from_raw(ptr);
                let _ = DeleteObject(HGDIOBJ(b.font.0));
                let _ = DeleteObject(HGDIOBJ(b.bg.0));
                let _ = DeleteObject(HGDIOBJ(b.lit_bg.0));
                let _ = DeleteObject(HGDIOBJ(b.bg_hot.0));
                let _ = DeleteObject(HGDIOBJ(b.lit_hot.0));
                let _ = DeleteObject(HGDIOBJ(b.edit_bg.0));
                // The tooltip control dies with its
                // own `TipHost::drop`.
                b.tip = None;
            }
            0
        }
        _ => 0,
    }
}

/// Enter, Escape and clicks, translated into a message for the frame. The bar
/// never touches the document, which is what keeps this window from owning a
/// second copy of the editor's state.
unsafe fn on_command(hwnd: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
    let frame = state(hwnd).frame;
    if id == IDOK.0 {
        post(frame, BAR_ENTER, GetDlgCtrlID(GetFocus()) as isize);
        return 1;
    }
    if id == IDCANCEL.0 {
        post(frame, BAR_DISMISS, 0);
        return 1;
    }
    if code == EN_CHANGE as u16 {
        post(frame, BAR_TEXT, id as isize);
        return 1;
    }
    if code == 0 {
        // A toolbar click arrives with no notification code, carrying the
        // button id.
        post(frame, BAR_BUTTON, id as isize);
        return 1;
    }
    0
}

/// The strip's buttons, every pixel of them: background, glyph and pressed or
/// checked state, all from `Theme`. The control still decides when to ask and
/// still owns the click; the pixels come from [`ownerdraw`], which is also what
/// gives the button its hover state.
unsafe fn on_draw_item(hwnd: HWND, lparam: LPARAM) -> isize {
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    let id = di.CtlID as i32;
    let Some((label, toggle)) = button_of(id) else {
        return 0;
    };
    let bar = state(hwnd);
    // `ODS_FLAGS` is a plain newtype in this metadata, not a bitflags set. A
    // button is lit while it is pressed, or while the option it stands for is
    // on — which is the model's answer, mirrored by `apply_opts`. Pressed is
    // handled by the shared paint (through `press`), the toggle is not: that is
    // this bar's own idea of "on", so it swaps the resting face.
    let lit = opt_is_on(bar, id, toggle);
    let look = bar.button_look(lit);
    let rc = ownerdraw::frame(di, &look);
    ownerdraw::label(di, rc, &look, label, ownerdraw::Align::Center);
    1
}

/// The label and toggle-ness of a strip button, by command id. Anything else —
/// the two hidden `IDOK`/`IDCANCEL` buttons — is not ours to draw.
fn button_of(id: i32) -> Option<(&'static str, bool)> {
    [FIND_BUTTONS, REPLACE_BUTTONS, FILES_BUTTONS]
        .into_iter()
        .flatten()
        .find(|(bid, _, _, _)| *bid == id)
        .map(|(_, label, toggle, _)| (*label, *toggle))
}

impl Bar {
    /// What [`ownerdraw`] needs to paint one of the strip's buttons. `lit` is
    /// the bar's own toggle state: an "on" button rests on the active face, and
    /// still brightens under the cursor.
    fn button_look(&self, lit: bool) -> ownerdraw::Look {
        ownerdraw::Look {
            face: if lit { self.lit_bg } else { self.bg },
            hot: if lit { self.lit_hot } else { self.bg_hot },
            press: self.lit_bg,
            // The command strip has no frame at all — a flat run of faces.
            edge: ownerdraw::Edge::None,
            ink: ownerdraw::Ink {
                normal: to_colorref(self.theme.panel_fg),
                pressed: to_colorref(self.theme.foreground),
                disabled: to_colorref(self.theme.menu_disabled),
            },
            font: self.font,
            under: None,
            inset: 0,
            press_offset: 0,
            live_rect: false,
        }
    }
}

/// Whether the option a toggle button stands for is on.
fn opt_is_on(bar: &Bar, id: i32, toggle: bool) -> bool {
    let Some(bit) = OPT_IDS.iter().position(|o| *o == id) else {
        return false;
    };
    toggle && (bar.opts.load(Ordering::Relaxed) >> bit) & 1 == 1
}

unsafe fn post(frame: HWND, kind: usize, code: isize) {
    let _ = PostMessageW(Some(frame), WM_BAR, WPARAM(kind), LPARAM(code));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar's rectangles are this module's layout constants.
    #[test]
    fn measured_bar_geometry_is_kept() {
        assert_eq!((ROW_H, CLIENT_W), (24, 452));
        assert_eq!(EDIT_X + EDIT_W, 253);
        assert_eq!(CLIENT_W - TOOL_X, 196);
        assert_eq!(scale(315, 1.25), 394);
    }

    /// A narrow editor must not push the query box off the frame's left edge;
    /// the y is never clamped, or a short frame would lift the bar back over
    /// the toolbar.
    #[test]
    fn a_narrow_editor_keeps_the_query_box_on_the_frame() {
        let rc = RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 100,
        };
        assert_eq!(anchor_x(&rc, 200, 567), 567);
        assert_eq!(anchor_x(&rc, 392, 200), 392);
    }

    /// Every row's buttons have distinct ids, or a click would be ambiguous.
    #[test]
    fn button_ids_are_unique() {
        let mut ids: Vec<i32> = FIND_BUTTONS
            .iter()
            .chain(REPLACE_BUTTONS.iter())
            .chain(FILES_BUTTONS.iter())
            .map(|(id, _, _, _)| *id)
            .collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }
}
