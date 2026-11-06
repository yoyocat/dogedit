//! 书签管理器: the bookmarks of every open document, as a tree the native control
//! draws.
//!
//! Same trade as [`crate::win32::resultlist`]: the `SysTreeView32` gets
//! hit-testing, selection, keyboard movement, scrolling and the expander buttons,
//! and this window keeps only what a control cannot know — which row belongs to
//! which document and line, and what colour the theme says a row is.
//!
//! 管理多文件中的书签 is the caption's own description of the pane, so the rows are
//! grouped by document and not by the tab on screen: a bookmark set is worth reading
//! across the files it spans. Each group is named the way the tab strip names its
//! document, so the pane and the strip never call the same file two things.
//!
//! Choosing a row goes to that document's line — switching tab first when the
//! document is not the one on screen. The pane does not add or remove bookmarks:
//! 切换书签 does, and this list is what it produces.

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
    CreateWindowExW, DefWindowProcW, GetClientRect, GetMessagePos, GetParent, GWLP_WNDPROC, HMENU,
    PostMessageW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL,
    WNDPROC, WS_CHILD, WS_EX_COMPOSITED,
};

use crate::win32::customdraw;
use crate::win32::window::{state_mut, to_colorref};
use dg_render::Rect;
use dg_render::theme::Theme;

const BOOKMARK_ID: usize = 2032;

/// One document's bookmark group, already read off the document: what its tab
/// calls it, and the (line, text) pairs it holds.
pub(super) struct DocMarks {
    pub name: String,
    pub marks: Vec<(usize, String)>,
}

/// Which row a tree item stands for: a group header, or one bookmark in one
/// document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Spec {
    Group(usize),
    Mark(usize, usize),
}

/// What the frame has to do when a row is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BookmarkNotify {
    Goto { doc: usize, line: usize },
}

/// Turns the open documents into a two-level row set.
///
/// Documents with no bookmarks are left out — a group you cannot act on is
/// clutter in a list whose whole point is the marks. The rest keep the document
/// order, and inside a group the bookmark order, which is the line order the
/// document stores them in.
pub(super) fn bookmark_rows(docs: &[DocMarks]) -> Vec<(String, Spec)> {
    let mut rows = Vec::new();
    for (di, doc) in docs.iter().enumerate() {
        if doc.marks.is_empty() {
            continue;
        }
        rows.push((
            format!("{} ({})", doc.name, doc.marks.len()),
            Spec::Group(di),
        ));
        for (line, text) in &doc.marks {
            rows.push((
                format!("{}: {}", line + 1, text.trim()),
                Spec::Mark(di, *line),
            ));
        }
    }
    rows
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

fn call_orig(orig: WNDPROC, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match orig {
        Some(p) => unsafe { p(hwnd, msg, wparam, lparam) },
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub(super) struct BookmarkWin {
    hwnd: HWND,
    orig: WNDPROC,
    frame: Rect,
    /// How many marks `set_rows` handed over, headers excluded.
    count: usize,
    groups: Vec<DocMarks>,
    /// Parallel to the items `rebuild_items` inserted: `lParam` is an index here.
    specs: Vec<Spec>,
    pending: Vec<BookmarkNotify>,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    sel_bg: COLORREF,
    brush: Option<HBRUSH>,
    brush_for: COLORREF,
}

impl BookmarkWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            count: 0,
            groups: Vec::new(),
            specs: Vec::new(),
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
        // No TVS_EDITLABELS: a bookmark row names a line, and a line is not a name
        // to be retyped here.
        let style = TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_TRACKSELECT;
        let Ok(tree) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the list is rebuilt as delete-all then
            // one insert per row, and the control erases before it draws —
            // on a single-buffered surface the face is on screen for the
            // whole fill. The style stages that pass off-screen and blits
            // once, the same fix the directory tree wears.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            w!("SysTreeView32"),
            w!("bookmarks"),
            // Not WS_VISIBLE: `place` shows the list once the dock has a rect, so
            // a closed dock owns no pixels and no tab stop.
            WS_CHILD | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(BOOKMARK_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            tree,
            GWLP_WNDPROC,
            bookmark_proc as *const () as isize,
        ));
        self.hwnd = tree;
        self.apply_theme(theme);
    }

    /// Places the list in `rect`; an empty rect means the dock is closed.
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

    /// Replaces the bookmark set. Every group starts expanded, as the results list
    /// does: a mark that hides behind a click is not a list of marks.
    pub(super) fn set_rows(&mut self, groups: Vec<DocMarks>) {
        self.count = groups.iter().map(|g| g.marks.len()).sum();
        self.groups = groups;
        unsafe { self.rebuild_items() };
    }

    /// How many bookmarks the pane holds, headers excluded. The control cannot
    /// answer for a window that was never created, so the tests read the set
    /// through this.
    #[cfg(test)]
    pub(super) fn mark_count(&self) -> usize {
        self.count
    }

    /// The caret item's handle, or `0` when the list holds no selection.
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
    pub(super) fn drain_notifies(&mut self) -> Vec<BookmarkNotify> {
        std::mem::take(&mut self.pending)
    }

    /// `WM_NOTIFY` from the list, answered by the frame's procedure.
    pub(super) unsafe fn handle_notify(
        &mut self,
        frame: HWND,
        code: u32,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match code {
            TVN_BEGINLABELEDITW => Some(LRESULT(1)),
            NM_CUSTOMDRAW => {
                // Same ladder as `dirtree` and the two dialogs; only the colour
                // pair is this window's business.
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
                    self.goto_item(frame, item);
                }
                None
            }
            NM_RCLICK => {
                // The control only moves the selection on a left click, so a right
                // click on an unselected row would act on the old one.
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

    fn spec_of(&self, item: isize) -> Option<Spec> {
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
        self.specs.get(it.lParam.0 as usize).copied()
    }

    /// A header chooses nothing: it is a document, not a line.
    fn goto_item(&mut self, frame: HWND, item: isize) {
        let Some(Spec::Mark(di, line)) = self.spec_of(item) else {
            return;
        };
        self.pending.push(BookmarkNotify::Goto { doc: di, line });
        self.wake(frame);
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
        if h == 0 {
            None
        } else {
            Some(h)
        }
    }

    fn wake(&self, frame: HWND) {
        const WM_APP: u32 = 0x8000;
        unsafe {
            let _ = PostMessageW(Some(frame), WM_APP, WPARAM(0), LPARAM(0));
        }
    }

    /// Rebuilds the two-level item set from the groups.
    ///
    /// The labels are cloned out first, so each one's `lParam` — its index into
    /// `specs`, which is how a notification finds its way back to a bookmark — is
    /// settled before the control is touched, and because inserting sends messages
    /// back into this window.
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
        self.specs.clear();
        // One builder for both the labels and what they stand for: the frame's
        // refresh and this rebuild can never disagree about the row set.
        let rows = bookmark_rows(&self.groups);
        let mut header = 0isize;
        for (idx, (label, spec)) in rows.into_iter().enumerate() {
            // A group header is the document's slot, which is what the frame
            // switches to; the marks under it carry their own line.
            let is_group = matches!(spec, Spec::Group(_));
            let mut buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
            let mut item = TVITEMEXW::default();
            item.mask = TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN;
            item.pszText = PWSTR(buf.as_mut_ptr());
            item.lParam = LPARAM(idx as isize);
            item.cChildren = TVITEMEXW_CHILDREN(if is_group { 1 } else { 0 });
            let mut tvis = TVINSERTSTRUCTW::default();
            tvis.hParent = HTREEITEM(if is_group { TVI_ROOT.0 } else { header });
            tvis.hInsertAfter = TVI_LAST;
            tvis.Anonymous = TVINSERTSTRUCTW_0 { itemex: item };
            let h = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw mut tvis as isize)),
            )
            .0;
            self.specs.push(spec);
            if is_group {
                header = h;
            } else if header != 0 && h != 0 {
                // Expanding after the group's children exist is what shows the
                // button in the state the frame handed the set over: open.
                let _ = SendMessageW(
                    self.hwnd,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_EXPAND.0 as usize)),
                    Some(LPARAM(header)),
                );
            }
        }
    }
}

impl Default for BookmarkWin {
    fn default() -> Self {
        Self::new()
    }
}

/// The list's subclass: the background erase and Enter. Everything else,
/// including the expanders, is the control's.
unsafe extern "system" fn bookmark_proc(
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
    let orig = st.panels.bookmark_win.orig;
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = st.panels.bookmark_win.bg_brush();
            let _ = FillRect(hdc, &rc, brush);
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            // Same as the directory tree: the wheel belongs to the panel it is
            // pointing at, and taking the focus is what lets the control scroll.
            if GetFocus() != hwnd {
                let _ = SetFocus(Some(hwnd));
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            if wparam.0 as u32 == 13 {
                let item = st.panels.bookmark_win.selected_item();
                if item != 0 {
                    st.panels.bookmark_win.goto_item(frame, item);
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

    fn doc(name: &str, marks: &[(usize, &str)]) -> DocMarks {
        DocMarks {
            name: name.to_string(),
            marks: marks
                .iter()
                .map(|(l, t)| (*l, t.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_mark_row_is_the_line_and_its_text() {
        let rows = bookmark_rows(&[doc("a.rs", &[(2, "  let x = 1;  ")])]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "a.rs (1)");
        assert_eq!(rows[1].0, "3: let x = 1;", "the line counts from one, the text is trimmed");
    }

    #[test]
    fn a_document_without_bookmarks_is_not_a_group() {
        let rows = bookmark_rows(&[doc("a.rs", &[]), doc("b.rs", &[(0, "x")])]);
        assert_eq!(rows.len(), 2, "one header and one mark, from b.rs alone");
        assert_eq!(rows[0].0, "b.rs (1)");
    }

    #[test]
    fn groups_keep_the_document_order_and_their_marks_the_line_order() {
        let rows = bookmark_rows(&[
            doc("one.rs", &[(0, "a"), (5, "b")]),
            doc("two.rs", &[(1, "c")]),
        ]);
        let labels: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            labels,
            vec!["one.rs (2)", "1: a", "6: b", "two.rs (1)", "2: c"]
        );
    }

    #[test]
    fn a_header_names_its_document_and_a_mark_names_its_line() {
        let rows = bookmark_rows(&[doc("one.rs", &[(7, "a")]), doc("two.rs", &[(3, "b")])]);
        assert_eq!(rows[0].1, Spec::Group(0));
        assert_eq!(rows[1].1, Spec::Mark(0, 7));
        assert_eq!(rows[2].1, Spec::Group(1));
        assert_eq!(rows[3].1, Spec::Mark(1, 3), "the slot, not the on-screen tab");
    }

    #[test]
    fn nothing_open_is_an_empty_list() {
        assert!(bookmark_rows(&[]).is_empty());
    }

    #[test]
    fn the_mark_count_excludes_the_headers() {
        let mut w = BookmarkWin::new();
        w.set_rows(vec![doc("a.rs", &[(0, "x"), (4, "y")]), doc("b.rs", &[])]);
        assert_eq!(w.mark_count(), 2);
    }
}
