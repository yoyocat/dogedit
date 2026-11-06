//! 大纲视图: the document's symbols as a tree the native control draws.
//!
//! Same trade as [`crate::win32::resultlist`]: the `SysTreeView32` gets
//! hit-testing, selection, keyboard movement, scrolling and the expander buttons,
//! and this window keeps only what a control cannot know — which row names which
//! line, and what colour the theme says a row is.
//!
//! The rows come from [`outline_rows`], a pure function over the symbol list the
//! 符号 sources already produce (LSP, or ctags when no server is configured), so
//! the pane and the 跳转框's `@` mode never disagree about what a symbol is. A
//! row's depth follows the indentation of the line it names, which is what makes
//! a nested function sit under the one that holds it without needing a grammar.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::core::{PWSTR, w};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HGDIOBJ, HBRUSH, HDC, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    HTREEITEM, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NM_CUSTOMDRAW,
    NM_DBLCLK, NM_RCLICK, NMTVCUSTOMDRAW, TVHITTESTINFO, TVI_LAST, TVI_ROOT, TVIF_CHILDREN,
    TVIF_PARAM, TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVITEMEXW, TVITEMEXW_CHILDREN,
    TVITEMW, TVN_BEGINLABELEDITW, TVS_HASBUTTONS, TVS_LINESATROOT, TVS_SHOWSELALWAYS,
    TVS_TRACKSELECT, TVE_EXPAND, TVM_DELETEITEM, TVM_EXPAND, TVM_GETITEMW, TVM_GETNEXTITEM,
    TVM_HITTEST, TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETBKCOLOR, TVM_SETTEXTCOLOR, TVGN_CARET,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetMessagePos, GetParent, GetClientRect, GWLP_WNDPROC, HMENU,
    PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL,
    WNDPROC, WS_CHILD, WS_EX_COMPOSITED,
};

use crate::win32::customdraw;
use crate::win32::window::{state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::Theme;

const OUTLINE_ID: usize = 2030;

/// How many rows the pane offers. Past this the list says so with its own last
/// row rather than going silent; the number is a screenfuls of symbols, not a
/// correctness boundary.
pub(super) const MAX_ROWS: usize = 2000;

/// One outline row: what it is called, the line it names, and how deep it sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OutlineRow {
    pub label: String,
    pub line: usize,
    pub level: usize,
}

/// The row that tells the reader the list was cut short.
pub(super) const TRUNCATED: &str = "…";

/// Turns a symbol list into outline rows.
///
/// The sources hand over symbols in whatever order the reply came back in, so
/// the list is put in document order first: an outline that is not in the order
/// of the file is not an outline. `indent_of` answers for a line's leading
/// indentation in columns, which is what decides a row's depth.
pub(super) fn outline_rows(
    symbols: &[(String, usize)],
    max: usize,
    indent_of: impl Fn(usize) -> usize,
) -> Vec<OutlineRow> {
    let mut ordered: Vec<(String, usize)> = symbols.to_vec();
    ordered.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut rows: Vec<OutlineRow> = Vec::new();
    let mut last: Option<(String, usize)> = None;
    for (name, line) in ordered {
        if last.as_ref() == Some(&(name.clone(), line)) {
            continue;
        }
        last = Some((name.clone(), line));
        let level = (indent_of(line) / 4).min(8);
        rows.push(OutlineRow {
            label: name,
            line,
            level,
        });
        if rows.len() >= max {
            rows.push(OutlineRow {
                label: TRUNCATED.to_string(),
                line,
                level: 0,
            });
            break;
        }
    }
    rows
}

/// What the frame has to do when a row is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OutlineNotify {
    Goto { line: usize },
}

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

fn to_wndproc(v: isize) -> WNDPROC {
    if v == 0 {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&v) })
    }
}

unsafe fn call_orig(
    orig: WNDPROC,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match orig {
        Some(p) => unsafe { p(hwnd, msg, wparam, lparam) },
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub(super) struct OutlineWin {
    hwnd: HWND,
    orig: WNDPROC,
    frame: Rect,
    rows: Vec<OutlineRow>,
    /// Parallel to the items `rebuild_items` inserted: `lParam` is an index here.
    index: Vec<usize>,
    pending: Vec<OutlineNotify>,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    sel_bg: COLORREF,
    brush: Option<HBRUSH>,
    brush_for: COLORREF,
}

impl OutlineWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            rows: Vec::new(),
            index: Vec::new(),
            pending: Vec::new(),
            bg: COLORREF(0),
            fg: COLORREF(0),
            hover: COLORREF(0),
            sel_bg: COLORREF(0),
            brush: None,
            brush_for: COLORREF(0xFFFFFFFE),
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(super) unsafe fn create(&mut self, parent: HWND, theme: &Theme) {
        if self.alive() {
            self.apply_theme(theme);
            return;
        }
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES,
        });
        let style = TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_TRACKSELECT;
        let Ok(tree) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the list is rebuilt as delete-all then
            // one insert per row, and the control erases before it draws —
            // on a single-buffered surface the face is on screen for the
            // whole fill. The style stages that pass off-screen and blits
            // once, the same fix the directory tree wears.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            w!("SysTreeView32"),
            w!("outline"),
            // Not WS_VISIBLE: `place` shows the tree once the dock has a rect, so
            // a closed pane owns no pixels and no tab stop.
            WS_CHILD | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(OUTLINE_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            tree,
            GWLP_WNDPROC,
            outline_proc as *const () as isize,
        ));
        self.hwnd = tree;
        self.apply_theme(theme);
    }

    /// Places the tree in `rect`; an empty rect means the dock is closed.
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
    }

    pub(super) unsafe fn apply_theme(&mut self, theme: &Theme) {
        let bg = to_colorref(theme.panel_bg);
        let fg = to_colorref(theme.panel_fg);
        // This runs on every layout, and a layout is a resize step; re-sending
        // the colours makes the control repaint even when the theme did not
        // change, so only a real colour change talks to it.
        let changed = self.bg != bg || self.fg != fg;
        self.bg = bg;
        self.fg = fg;
        self.hover = to_colorref(theme.panel_hover);
        self.sel_bg = to_colorref(theme.panel_active);
        if self.brush.is_none() || self.brush_for != self.bg {
            if let Some(old) = self.brush.take() {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
            self.brush = Some(CreateSolidBrush(self.bg));
            self.brush_for = self.bg;
        }
        if !self.alive() || !changed {
            return;
        }
        let _ = SendMessageW(
            self.hwnd,
            TVM_SETBKCOLOR,
            Some(WPARAM(0)),
            Some(LPARAM(self.bg.0 as isize)),
        );
        let _ = SendMessageW(
            self.hwnd,
            TVM_SETTEXTCOLOR,
            Some(WPARAM(0)),
            Some(LPARAM(self.fg.0 as isize)),
        );
    }

    fn bg_brush(&mut self) -> HBRUSH {
        if self.brush.is_none() {
            self.brush = Some(unsafe { CreateSolidBrush(self.bg) });
            self.brush_for = self.bg;
        }
        self.brush.unwrap_or(HBRUSH::default())
    }

    /// Replaces the outline. Every row starts expanded: a symbol you have to
    /// click twice to see is not an outline.
    pub(super) fn set_rows(&mut self, rows: Vec<OutlineRow>) {
        self.rows = rows;
        unsafe { self.rebuild_items() };
    }

    /// The rows the pane holds, including a truncation marker. The control cannot
    /// answer for a window that was never created, so the tests read the set
    /// through this.
    #[cfg(test)]
    pub(super) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The caret item's handle, or `0` when the tree holds no selection.
    fn selected_item(&self) -> isize {
        if !self.alive() {
            return 0;
        }
        unsafe {
            SendMessageW(
                self.hwnd,
                TVM_GETNEXTITEM,
                Some(WPARAM(TVGN_CARET as usize)),
                Some(LPARAM(0)),
            )
            .0
        }
    }

    /// Takes the queued notifications, leaving the queue empty.
    pub(super) fn drain_notifies(&mut self) -> Vec<OutlineNotify> {
        std::mem::take(&mut self.pending)
    }

    /// `WM_NOTIFY` from the tree, answered by the frame's procedure.
    pub(super) unsafe fn handle_notify(
        &mut self,
        frame: HWND,
        code: u32,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match code {
            TVN_BEGINLABELEDITW => Some(LRESULT(1)),
            NM_CUSTOMDRAW => {
                let cd = unsafe { &mut *(lparam.0 as *mut NMTVCUSTOMDRAW) };
                let r = customdraw::row(cd, false, |_, selected, hot| {
                    (
                        self.fg,
                        if selected {
                            self.sel_bg
                        } else if hot {
                            self.hover
                        } else {
                            self.bg
                        },
                    )
                });
                Some(LRESULT(r))
            }
            NM_DBLCLK => {
                let pos = unsafe { GetMessagePos() };
                if let Some(item) = self.hit_item(pos) {
                    self.open_item(frame, item);
                }
                None
            }
            NM_RCLICK => {
                // The control moves the selection on a left click only, so a
                // right click on an unselected row would act on the old one.
                let pos = unsafe { GetMessagePos() };
                if let Some(item) = self.hit_item(pos) {
                    let _ = SendMessageW(
                        self.hwnd,
                        TVM_SELECTITEM,
                        Some(WPARAM(TVGN_CARET as usize)),
                        Some(LPARAM(item)),
                    );
                }
                None
            }
            _ => None,
        }
    }

    fn row_of(&self, item: isize) -> Option<&OutlineRow> {
        if item == 0 || !self.alive() {
            return None;
        }
        let mut it = TVITEMW::default();
        it.mask = TVIF_PARAM;
        it.hItem = HTREEITEM(item);
        let ok = unsafe {
            SendMessageW(
                self.hwnd,
                TVM_GETITEMW,
                Some(WPARAM(0)),
                Some(LPARAM(&raw mut it as isize)),
            )
        };
        if ok.0 == 0 {
            return None;
        }
        self.rows.get(self.index.get(it.lParam.0 as usize).copied()?)
    }

    fn open_item(&mut self, frame: HWND, item: isize) {
        let Some(row) = self.row_of(item) else {
            return;
        };
        if row.label == TRUNCATED {
            return;
        }
        let line = row.line;
        self.pending.push(OutlineNotify::Goto { line });
        unsafe {
            let _ = PostMessageW(Some(frame), 0x8000, WPARAM(0), LPARAM(0));
        }
    }

    fn hit_item(&self, pos: u32) -> Option<isize> {
        if !self.alive() {
            return None;
        }
        let mut pt = POINT {
            x: (pos & 0xFFFF) as i16 as i32,
            y: ((pos >> 16) & 0xFFFF) as i16 as i32,
        };
        let _ = unsafe { ScreenToClient(self.hwnd, &mut pt) };
        let mut hti = TVHITTESTINFO {
            pt,
            ..Default::default()
        };
        let h = unsafe {
            SendMessageW(
                self.hwnd,
                TVM_HITTEST,
                Some(WPARAM(0)),
                Some(LPARAM(&raw mut hti as isize)),
            )
            .0
        };
        if h == 0 { None } else { Some(h) }
    }

    /// Rebuilds the item set from `rows`, nesting each row under the closest
    /// shallower one before it.
    unsafe fn rebuild_items(&mut self) {
        if !self.alive() {
            return;
        }
        let _ = SendMessageW(
            self.hwnd,
            TVM_DELETEITEM,
            Some(WPARAM(0)),
            Some(LPARAM(TVI_ROOT.0)),
        );
        self.index.clear();
        // (level, item) of the chain the next row hangs off.
        let mut stack: Vec<(usize, isize)> = Vec::new();
        for (idx, row) in self.rows.iter().enumerate() {
            while stack.last().is_some_and(|(l, _)| *l >= row.level) {
                stack.pop();
            }
            let parent = stack
                .last()
                .map(|(_, h)| *h)
                .unwrap_or(TVI_ROOT.0);
            let mut buf: Vec<u16> = row.label.encode_utf16().chain([0]).collect();
            let mut item = TVITEMEXW::default();
            item.mask = TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN;
            item.pszText = PWSTR(buf.as_mut_ptr());
            item.lParam = LPARAM(self.index.len() as isize);
            item.cChildren = TVITEMEXW_CHILDREN(0);
            let mut tvis = TVINSERTSTRUCTW::default();
            tvis.hParent = HTREEITEM(parent);
            tvis.hInsertAfter = TVI_LAST;
            tvis.Anonymous = TVINSERTSTRUCTW_0 { itemex: item };
            let h = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw mut tvis as isize)),
            )
            .0;
            self.index.push(idx);
            stack.push((row.level, h));
            if h != 0 {
                let _ = SendMessageW(
                    self.hwnd,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_EXPAND.0 as usize)),
                    Some(LPARAM(h)),
                );
            }
        }
    }
}

impl Default for OutlineWin {
    fn default() -> Self {
        Self::new()
    }
}

/// The tree's subclass: the background erase and Enter. Everything else,
/// including the expanders, is the control's.
unsafe extern "system" fn outline_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let orig = st.panels.outline_win.orig;
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = st.panels.outline_win.bg_brush();
            let _ = FillRect(hdc, &rc, brush);
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            if GetFocus() != hwnd {
                let _ = SetFocus(Some(hwnd));
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            if wparam.0 as u32 == 13 {
                let item = st.panels.outline_win.selected_item();
                if item != 0 {
                    st.panels.outline_win.open_item(frame, item);
                    return LRESULT(0);
                }
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        _ => call_orig(orig, hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sy(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(n, l)| (n.to_string(), *l)).collect()
    }

    /// The reply order is whatever the source sent; the outline has to be in
    /// document order or it is a list of names, not a map of the file.
    #[test]
    fn rows_come_back_in_document_order() {
        let rows = outline_rows(&sy(&[("zeta", 9), ("alpha", 1)]), MAX_ROWS, |_| 0);
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(rows[0].line, 1);
    }

    /// Depth follows the indentation of the line the symbol sits on, in columns
    /// of four.
    #[test]
    fn depth_follows_the_indent_of_the_named_line() {
        let indents = [0usize, 4, 8];
        let rows = outline_rows(&sy(&[("top", 0), ("mid", 1), ("inner", 2)]), MAX_ROWS, |l| {
            indents[l]
        });
        assert_eq!(
            rows.iter().map(|r| r.level).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    /// A cut list says so: the last row is the marker, and it names no symbol.
    #[test]
    fn a_cut_list_ends_with_the_marker() {
        let many: Vec<(String, usize)> = (0..10).map(|i| (format!("s{i}"), i)).collect();
        let rows = outline_rows(&many, 3, |_| 0);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].label, TRUNCATED);
        assert_eq!(rows.iter().filter(|r| r.label != TRUNCATED).count(), 3);
    }

    /// The same name on the same line twice is one row: a duplicate would give
    /// the reader two ways to land on the same place.
    #[test]
    fn a_repeated_symbol_shows_once() {
        let rows = outline_rows(&sy(&[("same", 4), ("same", 4)]), MAX_ROWS, |_| 0);
        assert_eq!(rows.len(), 1);
    }

    /// Empty input is an empty outline, not a marker row.
    #[test]
    fn no_symbols_is_no_rows() {
        assert!(outline_rows(&[], MAX_ROWS, |_| 0).is_empty());
    }

    #[test]
    fn a_row_count_answers_before_the_window_exists() {
        let mut w = OutlineWin::new();
        assert!(!w.alive());
        assert_eq!(w.row_count(), 0);
        w.set_rows(outline_rows(&sy(&[("a", 0)]), MAX_ROWS, |_| 0));
        assert_eq!(w.row_count(), 1, "the rows are kept even with no control");
    }
}
