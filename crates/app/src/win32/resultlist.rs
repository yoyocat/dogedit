//! The bottom dock's find-in-files results as a native `SysTreeView32`.
//!
//! Same trade as [`crate::win32::dirtree`]: the control gets hit-testing,
//! selection, keyboard movement, scrolling and the fold/unfold expander, and the
//! shell keeps only what a control cannot know — that a row is a file header or a
//! match, and what colour the theme says each of those is.
//!
//! A tree rather than a list because the result set *is* a two-level tree: one
//! node per file, one node per match. Folding a file is then the control's own
//! expander at level 0 instead of a hand-drawn glyph and a hand-maintained row
//! list, which is what the previous custom widget needed.

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
    CreateWindowExW, DefWindowProcW, GetClientRect, GWLP_WNDPROC, GetParent, GetMessagePos, HMENU,
    SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW, SWP_NOACTIVATE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL, WNDPROC, WS_CHILD,
    WS_EX_COMPOSITED,
};

use crate::win32::customdraw;
use crate::win32::window::{state_mut, to_colorref};
use dg_core::filesearch::FileMatch;
use dg_render::Rect;
use dg_render::theme::Theme;

const LIST_ID: usize = 2020;

/// Matches grouped under one file, the unit the worker sends and the dock folds.
#[derive(Clone, Debug)]
pub(super) struct ResultGroup {
    pub path: String,
    pub matches: Vec<FileMatch>,
}

impl ResultGroup {
    pub fn new(path: String) -> Self {
        Self {
            path,
            matches: Vec::new(),
        }
    }
}

/// What the shell is asked to do on the list's behalf.
#[derive(Clone, Debug)]
pub(super) enum ListNotify {
    /// Open this match, positioning the caret at `line`/`col`.
    Open {
        path: String,
        line: usize,
        col: usize,
    },
}

/// One item the control holds, addressed by the index its `lParam` carries.
#[derive(Clone, Copy)]
enum Spec {
    /// A file header row: it selects, but it is not a place to go.
    Group,
    /// Match `mi` inside group `gi`.
    Match(usize, usize),
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

pub(super) struct ResultListWin {
    hwnd: HWND,
    orig: WNDPROC,
    frame: Rect,
    groups: Vec<ResultGroup>,
    /// Parallel to the items `rebuild_items` inserted: `lParam` is an index here.
    specs: Vec<Spec>,
    total: usize,
    pending: Vec<ListNotify>,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    sel_bg: COLORREF,
    brush: Option<HBRUSH>,
    brush_for: COLORREF,
}

impl ResultListWin {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            groups: Vec::new(),
            specs: Vec::new(),
            total: 0,
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
        // No TVS_EDITLABELS: a result row is not a name that can be changed.
        let style = TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_TRACKSELECT;
        let Ok(tree) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: every result batch rebuilds the tree as
            // delete-all then one insert per row, and the control does that as
            // erase-then-draw on its own surface — the panel face is on screen
            // for the whole fill. The style stages the pass off-screen and
            // blits it once, the same fix the directory tree wears.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            w!("SysTreeView32"),
            w!("results"),
            // Not WS_VISIBLE: `place` shows the list once the dock has a rect,
            // so a closed dock owns no pixels and no tab stop.
            WS_CHILD | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(LIST_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            tree,
            GWLP_WNDPROC,
            list_proc as *const () as isize,
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

    /// Replaces the result set. Every group starts expanded, as the previous
    /// widget did: a search that hides its hits behind a click is not a search.
    pub(super) fn set_results(&mut self, groups: Vec<ResultGroup>) {
        self.total = groups.iter().map(|g| g.matches.len()).sum();
        self.groups = groups;
        unsafe { self.rebuild_items() };
    }

    pub(super) fn match_count(&self) -> usize {
        self.total
    }

    pub(super) fn group_count(&self) -> usize {
        self.groups.len()
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
    pub(super) fn drain_notifies(&mut self) -> Vec<ListNotify> {
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
                    self.open_item(frame, item);
                }
                None
            }
            NM_RCLICK => {
                // The control only moves the selection on a left click, so a
                // right click on an unselected row would act on the old one.
                // Selecting first is what Explorer does and what the dock does.
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

    fn open_item(&mut self, frame: HWND, item: isize) {
        let Some(Spec::Match(gi, mi)) = self.spec_of(item) else {
            return;
        };
        let Some(m) = self.groups.get(gi).and_then(|g| g.matches.get(mi)) else {
            return;
        };
        self.pending.push(ListNotify::Open {
            path: m.path.clone(),
            line: m.line,
            col: m.col,
        });
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
        if h == 0 { None } else { Some(h) }
    }

    fn wake(&self, frame: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        const WM_APP: u32 = 0x8000;
        unsafe {
            let _ = PostMessageW(Some(frame), WM_APP, WPARAM(0), LPARAM(0));
        }
    }

    /// Rebuilds the two-level item set from `groups`.
    ///
    /// The rows are cloned out first, so each one's `lParam` — its index into
    /// `specs`, which is how a notification finds its way back to a match — is
    /// settled before the control is touched, and because inserting sends
    /// messages back into this window.
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
        let mut rows: Vec<(String, Spec)> = Vec::new();
        for (gi, group) in self.groups.iter().enumerate() {
            rows.push((
                format!("{} ({} matches)", group.path, group.matches.len()),
                Spec::Group,
            ));
            for (mi, m) in group.matches.iter().enumerate() {
                rows.push((
                    format!("{}:{}: {}", m.line + 1, m.col + 1, m.text.trim()),
                    Spec::Match(gi, mi),
                ));
            }
        }
        let mut header = 0isize;
        for (idx, (label, spec)) in rows.into_iter().enumerate() {
            let is_group = matches!(spec, Spec::Group);
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
                // button in the state the worker handed the set over: open.
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

impl Default for ResultListWin {
    fn default() -> Self {
        Self::new()
    }
}

/// The list's subclass: the background erase and Enter. Everything else,
/// including the expanders, is the control's.
unsafe extern "system" fn list_proc(
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
    let orig = st.panels.results_win.orig;
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = st.panels.results_win.bg_brush();
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
                let item = st.panels.results_win.selected_item();
                if item != 0 {
                    st.panels.results_win.open_item(frame, item);
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

    #[test]
    fn set_results_counts_groups_and_matches() {
        let mut w = ResultListWin::new();
        let mut a = ResultGroup::new("a.rs".to_string());
        a.matches.push(FileMatch {
            path: "a.rs".to_string(),
            line: 1,
            col: 2,
            text: "one".to_string(),
        });
        let mut b = ResultGroup::new("b.rs".to_string());
        b.matches.push(FileMatch {
            path: "b.rs".to_string(),
            line: 4,
            col: 0,
            text: "two".to_string(),
        });
        b.matches.push(FileMatch {
            path: "b.rs".to_string(),
            line: 9,
            col: 3,
            text: "three".to_string(),
        });
        // Headless: no window exists, so the rebuild is a no-op and only the
        // model counts are observable — which is what the status bar reads.
        w.set_results(vec![a, b]);
        assert_eq!(w.group_count(), 2);
        assert_eq!(w.match_count(), 3);
        assert!(!w.alive());
        assert_eq!(w.selected_item(), 0);
    }
}
