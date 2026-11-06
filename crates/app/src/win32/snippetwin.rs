//! 代码片段: the snippets the document's language ships, as a list the native
//! control draws.
//!
//! Same trade as [`crate::win32::resultlist`] and [`crate::win32::outlinewin`]:
//! the `SysTreeView32` gets hit-testing, selection, keyboard movement and
//! scrolling, and this window keeps only what a control cannot know — which row
//! names which snippet, and what colour the theme says a row is.
//!
//! The rows are a projection of the table the trigger expansion already reads
//! ([`crate::snippets::bundled`]), so the pane and typing the trigger word by hand
//! can never offer different snippets. Choosing a row runs that same expansion at
//! the caret; the pane holds no copy of the bodies and edits none of them.

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
    NM_DBLCLK, NM_RCLICK, NMTVCUSTOMDRAW, TVHITTESTINFO, TVI_LAST, TVI_ROOT, TVIF_PARAM, TVIF_TEXT,
    TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVITEMEXW, TVITEMW, TVN_BEGINLABELEDITW, TVS_HASBUTTONS,
    TVS_SHOWSELALWAYS, TVS_TRACKSELECT, TVM_DELETEITEM, TVM_GETITEMW, TVM_GETNEXTITEM,
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

const SNIPPET_ID: usize = 2031;

/// One row: the text the list shows and which snippet in the table it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SnippetRow {
    pub label: String,
    pub index: usize,
}

/// Turns a snippet table into rows.
///
/// The table's own order is kept: it is the order the shipped `.snippet` file
/// lists its entries, and re-sorting the list would move the rows a reader has
/// learned to find by eye. The trigger is part of the label because it is what
/// the row teaches — typing the trigger and the expansion key is the other way
/// in, and two snippets can share a title while never sharing a trigger.
pub(super) fn snippet_rows(snippets: &[dg_core::Snippet]) -> Vec<SnippetRow> {
    snippets
        .iter()
        .enumerate()
        .map(|(index, s)| SnippetRow {
            label: format!("{} ({})", s.title, s.trigger),
            index,
        })
        .collect()
}

/// What the frame has to do when a row is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SnippetNotify {
    Insert { index: usize },
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

pub(super) struct SnippetWin {
    hwnd: HWND,
    orig: WNDPROC,
    frame: Rect,
    rows: Vec<SnippetRow>,
    pending: Vec<SnippetNotify>,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    sel_bg: COLORREF,
    brush: Option<HBRUSH>,
    brush_for: COLORREF,
}

impl SnippetWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            rows: Vec::new(),
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
        // No TVS_EDITLABELS: a snippet row is a name the table owns, not a label
        // to be retyped here.
        let style = TVS_HASBUTTONS | TVS_SHOWSELALWAYS | TVS_TRACKSELECT;
        let Ok(tree) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the list is rebuilt as delete-all then
            // one insert per row, and the control erases before it draws —
            // on a single-buffered surface the face is on screen for the
            // whole fill. The style stages that pass off-screen and blits
            // once, the same fix the directory tree wears.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            w!("SysTreeView32"),
            w!("snippets"),
            // Not WS_VISIBLE: `place` shows the list once the dock has a rect, so
            // a closed dock owns no pixels and no tab stop.
            WS_CHILD | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(SNIPPET_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            tree,
            GWLP_WNDPROC,
            snippet_proc as *const () as isize,
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

    /// Replaces the list. Which snippets are in it is the document's answer — a
    /// Python buffer and an HTML one do not offer the same rows.
    pub(super) fn set_rows(&mut self, rows: Vec<SnippetRow>) {
        self.rows = rows;
        unsafe { self.rebuild_items() };
    }

    #[cfg(test)]
    pub(super) fn row_count(&self) -> usize {
        self.rows.len()
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
    pub(super) fn drain_notifies(&mut self) -> Vec<SnippetNotify> {
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
                    self.insert_item(frame, item);
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

    fn row_of(&self, item: isize) -> Option<&SnippetRow> {
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
        self.rows.get(it.lParam.0 as usize)
    }

    fn insert_item(&mut self, frame: HWND, item: isize) {
        let Some(row) = self.row_of(item).cloned() else {
            return;
        };
        self.pending.push(SnippetNotify::Insert { index: row.index });
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

    /// Rebuilds the flat item set from `rows`.
    ///
    /// The labels are cloned out first, so each one's `lParam` — its index into
    /// `rows`, which is how a notification finds its way back to a snippet — is
    /// settled before the control is touched.
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
        for (idx, row) in self.rows.iter().enumerate() {
            let mut buf: Vec<u16> = row.label.encode_utf16().chain([0]).collect();
            let mut item = TVITEMEXW::default();
            item.mask = TVIF_TEXT | TVIF_PARAM;
            item.pszText = PWSTR(buf.as_mut_ptr());
            item.lParam = LPARAM(idx as isize);
            let mut tvis = TVINSERTSTRUCTW::default();
            tvis.hParent = TVI_ROOT;
            tvis.hInsertAfter = TVI_LAST;
            tvis.Anonymous = TVINSERTSTRUCTW_0 { itemex: item };
            let _ = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw mut tvis as isize)),
            );
        }
    }
}

impl Default for SnippetWin {
    fn default() -> Self {
        Self::new()
    }
}

/// The list's subclass: the background erase and Enter. Everything else,
/// including the keyboard movement, is the control's.
unsafe extern "system" fn snippet_proc(
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
    let orig = st.panels.snippet_win.orig;
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = st.panels.snippet_win.bg_brush();
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
                let item = st.panels.snippet_win.selected_item();
                if item != 0 {
                    st.panels.snippet_win.insert_item(frame, item);
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

    fn sn(title: &str, trigger: &str, body: &str) -> dg_core::Snippet {
        dg_core::Snippet {
            title: title.to_string(),
            trigger: trigger.to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn rows_keep_the_table_order_and_carry_the_trigger() {
        let rows = snippet_rows(&[sn("for loop", "for", "x"), sn("if", "if", "y")]);
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            vec!["for loop (for)", "if (if)"]
        );
        assert_eq!(rows[1].index, 1);
    }

    #[test]
    fn an_empty_table_is_an_empty_list() {
        assert!(snippet_rows(&[]).is_empty());
    }

    #[test]
    fn two_snippets_sharing_a_title_stay_two_rows() {
        let rows = snippet_rows(&[sn("log", "lg", "a"), sn("log", "lw", "b")]);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].label, rows[1].label);
    }

    #[test]
    fn a_row_count_answers_before_the_window_exists() {
        let mut w = SnippetWin::new();
        assert!(!w.alive());
        w.set_rows(vec![SnippetRow {
            label: "for".to_string(),
            index: 0,
        }]);
        assert_eq!(w.row_count(), 1);
    }
}
