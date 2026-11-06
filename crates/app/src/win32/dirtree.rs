//! The left dock's directory view as a native `SysTreeView32` child window.
//!
//! This replaces the custom-drawn `dg_ui::treeview` widget and the routing that
//! came with it: the frame no longer asks which widget owns a client point, no
//! longer forwards mouse, wheel or key messages into a model, and no longer owns
//! a focus enum. The control does all of that — hit-testing, selection, caret
//! movement, keyboard expand/collapse, scrolling, and the rename edit box —
//! because that is what a window is for.
//!
//! Two things stay ours. The first is colour: the control is subclassed for the
//! background erase and `NM_CUSTOMDRAW` (which comctl32 sends to the *parent*) is
//! answered through [`DockTree::handle_notify`] with `Theme` values, so a dark
//! theme still comes from the `.ini` rather than from `COLOR_WINDOW`. The second
//! is the row set: `DirTree` remains the source of truth for expansion and for
//! the fuzzy filter, neither of which a native tree has a notion of. Items are
//! rebuilt from the model on every change, and `TVN_ITEMEXPANDING` is answered
//! by toggling the model and rebuilding instead of letting the control grow its
//! own children — which is also what keeps the filter honest, since a filtered
//! tree must hide rows the control would otherwise happily expand into.
//!
//! Anything the shell has to act on (open a file, show the popup menu, apply a
//! name) is queued as a [`TreeNotify`] and posted to the frame as `WM_APP`, the
//! same wake-up the search worker already uses.
//!
//! The filter box starts *hidden*
//! is a tree first, and the search field is a thing you ask for with `Ctrl+F`
//! while the tree has the focus (`Escape` on an empty box puts it away again).
//! The scrollbar is the one part the control draws that no `TVM_` message
//! recolours, so the pair also carries the per-window dark style
//! (`AllowDarkModeForWindow` + `DarkMode_Explorer`): without
//! it the bar stays the system's light grey over the theme's panel colour.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;

use windows::core::{PCWSTR, PWSTR, w};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HGDIOBJ, HDC, HBRUSH, ScreenToClient, SetBkColor,
    SetTextColor,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{IDropTarget, RegisterDragDrop, RevokeDragDrop};
use windows::Win32::UI::Controls::{
    HTREEITEM, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NM_CUSTOMDRAW,
    NM_DBLCLK, NM_RCLICK, NMTVCUSTOMDRAW, NMTREEVIEWW, NMTVDISPINFOW, TVHITTESTINFO, TVI_LAST,
    TVI_ROOT, TVE_EXPAND, TVGN_CARET, TVGN_PARENT, TVIF_CHILDREN, TVIF_PARAM, TVIF_STATE,
    TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVIS_EXPANDED, TVITEMEXW_CHILDREN, TVITEMW,
    TVN_BEGINLABELEDITW, TVN_ENDLABELEDITW, TVN_ITEMEXPANDINGW, TVN_SELCHANGEDW, TVS_EDITLABELS,
    TVS_HASBUTTONS, TVS_LINESATROOT, TVS_SHOWSELALWAYS, TVS_TRACKSELECT, TVM_DELETEITEM,
    TVM_EDITLABELW, TVM_ENSUREVISIBLE, TVM_GETITEMW, TVM_GETNEXTITEM, TVM_HITTEST,
    TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETBKCOLOR, TVM_SETTEXTCOLOR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, ES_AUTOHSCROLL, GetClientRect, GWLP_WNDPROC, HMENU,
    GetParent, GetMessagePos, GetWindowTextLengthW, GetWindowTextW, SendMessageW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW, SWP_NOACTIVATE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEWHEEL, WM_SETTEXT, WNDPROC,
    WS_BORDER, WS_CHILD, WS_EX_COMPOSITED,
};

use crate::win32::customdraw;
use crate::win32::window::{state_mut, to_colorref};
use dg_core::dirwatch::DirTree;
use dg_core::fuzzy::{fuzzy_match, FuzzyMode};
use dg_render::Rect;
use dg_render::theme::Theme;

/// Height of the filter edit above the tree.
const FILTER_H: i32 = 24;
/// Control ids, so a `WM_COMMAND` from the filter edit names it.
const TREE_ID: usize = 2010;
pub(super) const FILTER_ID: usize = 2011;
/// `lParam` of the temporary item a New File / New Folder edit names before the
/// entry exists on disk.
const TEMP_ROW: isize = -1;

/// What the shell is asked to do on the tree's behalf.
#[derive(Clone, Debug)]
pub(super) enum TreeNotify {
    /// Open this file in an editor tab.
    Open { path: String },
    /// Show the directory-view popup menu at these **screen** coordinates.
    ContextMenu { x: i32, y: i32 },
    /// A label edit finished with `text`.
    Label { text: String },
    /// A label edit was cancelled.
    LabelCancelled,
}

/// Which label edit the shell asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EditKind {
    NewFile,
    NewDir,
    Rename,
}

fn instance() -> HINSTANCE {
    unsafe {
        GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()))
    }
}

/// 两格是不是同一个文件夹：先按真身（`canonicalize`）比，比不出来就按大小写不
/// 敏感的完整路径比。清单里不该出现两遍同一个目录。
fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| match std::fs::canonicalize(p) {
        Ok(p) => p.to_string_lossy().to_lowercase(),
        Err(_) => p.replace('/', "\\").to_lowercase(),
    };
    norm(a) == norm(b)
}

/// 系统的选目录框走 COM 的 `FileOpenDialog` + `FOS_PICKFOLDERS`，不是选项对话框里
/// 用的旧框（`SHBrowseForFolder`）。取消返回 `None`。
pub(super) unsafe fn pick_folder(owner: HWND) -> Option<String> {
    let dialog: IFileOpenDialog =
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
    let options = dialog.GetOptions().ok()?;
    let wanted = options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
    let _ = dialog.SetOptions(wanted);
    // 标题和菜单那一行同一个 id：语言表里有就用表里的。
    let title = crate::win32::lang::menu_text(349, 1206, false, "添加文件夹");
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
    // 取消是 `ERROR_CANCELLED`，`ok()` 正好把它当成「没有结果」。
    dialog.Show(Some(owner)).ok()?;
    let item = dialog.GetResult().ok()?;
    let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let mut len = 0usize;
    while *name.0.add(len) != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(std::slice::from_raw_parts(name.0, len));
    CoTaskMemFree(Some(name.0 as *const c_void));
    Some(text)
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

/// The dock's directory view: a filter edit and a tree, plus the model that
/// decides what the tree's items are.
pub(super) struct DockTree {
    hwnd: HWND,
    filter_hwnd: HWND,
    orig: WNDPROC,
    filter_orig: WNDPROC,
    /// Last rect handed to `SetWindowPos`, so a layout that changes nothing
    /// costs no window message.
    frame: Rect,
    tree: Option<DirTree>,
    /// Display row -> model row, after expansion and the filter.
    visible: Vec<usize>,
    filter: String,
    /// Selected model row, or `TEMP_ROW` while a fresh item is being named.
    sel_row: isize,
    /// The selection `sel_row` held when a label edit started, to fall back on.
    prev_sel_row: isize,
    /// The 快捷目录 list: one top-level row per folder the user put there. Not a
    /// single root — dropping another folder adds a row instead of replacing
    /// what is already on screen.
    roots: Vec<String>,
    pending: Vec<TreeNotify>,
    /// Expand the control asked for while it was still inside its own
    /// notification, as `(model row, wanted state)`.
    deferred_expand: Option<(usize, bool)>,
    edit: Option<EditKind>,
    /// Item the running label edit belongs to.
    edit_item: isize,
    /// True when that item is the temporary one a New command inserted.
    edit_temp: bool,
    /// Whether the filter edit is on screen. It starts hidden and `Ctrl+F`
    /// (focus inside the panel) is what brings it up; `Escape` on an empty box
    /// puts it away. While it is hidden the tree owns the whole dock rect.
    filter_shown: bool,
    /// Mirror of the last visibility `place` issued, so toggling the filter
    /// re-places the pair even when the dock's rect itself did not move.
    shown_filter: bool,
    /// The dark/light style last named on the two windows. `apply_theme` runs
    /// on every layout — every pixel of a splitter drag — and re-naming the
    /// style re-opens the control's visual style, which is a full repaint of
    /// its own; only a *change* may pay for one.
    dark: bool,
    bg: COLORREF,
    fg: COLORREF,
    hover: COLORREF,
    sel_bg: COLORREF,
    /// Brush for the background erase and the filter edit, rebuilt only when the
    /// theme's panel colour moves.
    brush: Option<HBRUSH>,
    brush_for: COLORREF,
    /// The panel's own drop target, held so the registration keeps its object.
    drop_target: Option<IDropTarget>,
}

impl DockTree {
    pub(super) fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            filter_hwnd: HWND::default(),
            orig: None,
            filter_orig: None,
            frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            tree: None,
            visible: Vec::new(),
            filter: String::new(),
            sel_row: 0,
            prev_sel_row: 0,
            roots: Vec::new(),
            pending: Vec::new(),
            deferred_expand: None,
            edit: None,
            edit_item: 0,
            edit_temp: false,
            filter_shown: false,
            shown_filter: false,
            dark: false,
            bg: COLORREF(0),
            fg: COLORREF(0),
            hover: COLORREF(0),
            sel_bg: COLORREF(0),
            brush: None,
            brush_for: COLORREF(0xFFFFFFFE),
            drop_target: None,
        }
    }

    pub(super) fn alive(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub(super) fn filter_hwnd(&self) -> HWND {
        self.filter_hwnd
    }

    /// Creates the filter edit and the tree and subclasses both. Rows are not
    /// inserted here: they come from the model through `set_roots`, so creation
    /// is safe before any directory is known.
    pub(super) unsafe fn create(&mut self, parent: HWND, theme: &Theme) {
        if self.alive() {
            self.apply_theme(theme);
            return;
        }
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            // The structure is versioned by its own size.
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES,
        });
        let Ok(edit) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            // No window text: the old widget drew a "Filter" prompt it had to
            // recolour by hand, and a native edit's cue banner is not a theme
            // colour, so the box simply starts empty.
            PCWSTR::null(),
            WS_CHILD | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(FILTER_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.filter_orig = to_wndproc(SetWindowLongPtrW(
            edit,
            GWLP_WNDPROC,
            filter_proc as *const () as isize,
        ));
        // The call belongs at creation — a visual style is opened on first paint
        // — but it is re-issued from `apply_theme` only when the style actually
        // changes.
        let dark = crate::win32::chrome::theme_is_dark(theme);
        crate::win32::chrome::dark_theme(edit, dark);
        let style = TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_EDITLABELS
            | TVS_TRACKSELECT;
        let Ok(tree) = CreateWindowExW(
            // `WS_EX_COMPOSITED`: the control repaints its *whole* client on
            // every resize — item geometry is width-dependent — and it does
            // that as erase-then-draw on a single-buffered surface, so the
            // panel-coloured erase pass is on screen for as long as the items
            // take to draw. A splitter drag is a resize per mouse-move pixel,
            // which is exactly the strobe the user sees. The style makes the
            // system stage that repaint off-screen and blit once, the same
            // double-buffer the windows this implementation already paints through.
            // The filter box is deliberately *not* given it: it is where the
            // user types, and a composited edit strobes its caret.
            WINDOW_EX_STYLE(WS_EX_COMPOSITED.0),
            w!("SysTreeView32"),
            w!("dirview"),
            // Not WS_VISIBLE: `place` shows the pair once the dock has a rect,
            // so a closed dock owns no pixels and no tab stop.
            WS_CHILD | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(TREE_ID as *mut c_void)),
            Some(instance()),
            None,
        ) else {
            return;
        };
        self.orig = to_wndproc(SetWindowLongPtrW(
            tree,
            GWLP_WNDPROC,
            tree_proc as *const () as isize,
        ));
        // Neither `TVM_SETBKCOLOR` nor `NM_CUSTOMDRAW` reaches it.
        crate::win32::chrome::dark_theme(tree, dark);
        self.dark = dark;
        self.filter_hwnd = edit;
        self.hwnd = tree;
        // 拖放：拖进来的文件夹往快捷目录的清单里加一行，文件开成页签；拖影交给
        // shell 的 helper 贴，和正文那条路同一个做法。
        let target: IDropTarget = crate::win32::drop::DockTreeDropTarget::new(parent, tree).into();
        if RegisterDragDrop(tree, &target).is_ok() {
            self.drop_target = Some(target);
        }
        self.apply_theme(theme);
    }

    /// 面板跟框架一起销毁：拖放目标得赶在树窗口还在的时候撤掉。
    pub(super) unsafe fn revoke_drop(&mut self) {
        if self.drop_target.take().is_some() && self.alive() {
            let _ = RevokeDragDrop(self.hwnd);
        }
    }

    /// Places the pair in `rect` (frame-client coordinates), showing or hiding
    /// them with the dock: an empty rect means the dock is closed. The filter
    /// edit only takes its row when it is up (`filter_shown`); while it is
    /// hidden the tree owns the whole rect, which is the panel's resting state.
    pub(super) unsafe fn place(&mut self, rect: Rect) {
        if !self.alive() {
            return;
        }
        let open = rect.w > 0.0 && rect.h > 0.0;
        if !open {
            if self.frame.w > 0.0 {
                self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
                let _ = ShowWindow(self.filter_hwnd, SW_HIDE);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if self.frame == rect && self.shown_filter == self.filter_shown {
            return;
        }
        self.frame = rect;
        self.shown_filter = self.filter_shown;
        let x = rect.x.round() as i32;
        let y = rect.y.round() as i32;
        let w = rect.w.round() as i32;
        let h = rect.h.round() as i32;
        let (filter_h, tree_y, tree_h) = if self.filter_shown {
            (FILTER_H, y + FILTER_H, (h - FILTER_H).max(1))
        } else {
            (0, y, h)
        };
        if self.filter_shown {
            let _ = SetWindowPos(
                self.filter_hwnd,
                None,
                x,
                y,
                w,
                filter_h,
                SWP_NOACTIVATE,
            );
        }
        let _ = SetWindowPos(self.hwnd, None, x, tree_y, w, tree_h, SWP_NOACTIVATE);
        let _ = ShowWindow(
            self.filter_hwnd,
            if self.filter_shown { SW_SHOW } else { SW_HIDE },
        );
        let _ = ShowWindow(self.hwnd, SW_SHOW);
    }

    /// Pushes the theme into the control and into the brush the erase pass and
    /// the filter edit paint with.
    pub(super) unsafe fn apply_theme(&mut self, theme: &Theme) {
        let bg = to_colorref(theme.panel_bg);
        let fg = to_colorref(theme.panel_fg);
        // Sent on every layout, and a layout is a resize step; a resize that
        // re-sends the same colours still makes the control repaint, so only a
        // real colour change talks to it.
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
        if !self.alive() {
            return;
        }
        // The scrollbar the control draws itself is named by the style, not by
        // the colours, so a theme switch has to re-name it: dark → the
        // `DarkMode_Explorer` sub-app, light → NULL, which hands the window
        // back to the palette the process app mode chooses. Only a *change*
        // re-names: this runs on every layout, and a splitter drag is a layout
        // per mouse-move pixel — re-naming there would re-open the visual
        // style and repaint the whole control once per pixel.
        let dark = crate::win32::chrome::theme_is_dark(theme);
        if dark != self.dark {
            self.dark = dark;
            crate::win32::chrome::dark_theme(self.hwnd, dark);
            crate::win32::chrome::dark_theme(self.filter_hwnd, dark);
        }
        if !changed {
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

    /// Brings the filter edit up and gives it the focus — the `Ctrl+F` answer
    /// while the panel has the keyboard. The text is left as it was, so a
    /// second `Ctrl+F` re-opens the box over the filter it already held.
    pub(super) unsafe fn show_filter(&mut self) {
        self.filter_shown = true;
        if !self.alive() {
            return;
        }
        // Re-place even when the dock's rect did not move: the tree grows by
        // the row the box takes, and `place` is the only writer of that.
        let rect = self.frame;
        self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.shown_filter = false;
        self.place(rect);
        let _ = SetFocus(Some(self.filter_hwnd));
    }

    /// Puts the filter edit away: the box hides, the filter it held is dropped
    /// (a hidden filter would otherwise keep hiding rows), and the focus goes
    /// back to the tree. This is the `Escape`-on-an-empty-box path and the
    /// dock-closing one.
    pub(super) unsafe fn hide_filter(&mut self) {
        self.filter_shown = false;
        // The filter is dropped whether or not there is a window to hide: the
        // model is the source of truth, and a hidden box must not keep rows
        // out of the tree.
        if !self.filter.is_empty() {
            self.set_filter("");
            if self.alive() {
                let _ = SendMessageW(
                    self.filter_hwnd,
                    WM_SETTEXT,
                    Some(WPARAM(0)),
                    Some(LPARAM(PCWSTR::null().as_ptr() as isize)),
                );
            }
        }
        if !self.alive() {
            return;
        }
        let rect = self.frame;
        self.frame = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.shown_filter = false;
        self.place(rect);
        let _ = SetFocus(Some(self.hwnd));
    }

    /// `WM_CTLCOLOREDIT` for the filter edit: recolour the DC and hand back the
    /// panel brush.
    pub(super) unsafe fn color_edit(&mut self, hdc: HDC) -> HBRUSH {
        let _ = SetTextColor(hdc, self.fg);
        let _ = SetBkColor(hdc, self.bg);
        self.bg_brush()
    }

    fn bg_brush(&mut self) -> HBRUSH {
        if self.brush.is_none() {
            self.brush = Some(unsafe { CreateSolidBrush(self.bg) });
            self.brush_for = self.bg;
        }
        self.brush.unwrap_or(HBRUSH::default())
    }

    /// Replaces the whole list. Returns false when none of `roots` can be read,
    /// in which case the panel keeps what it had. Every root starts open one
    /// level — there is no state to keep on the first fill.
    pub(super) fn set_roots(&mut self, roots: &[String]) -> bool {
        let mut tree = DirTree::from_roots(roots);
        if tree.is_empty() {
            return false;
        }
        tree.expand_roots_one_level();
        self.roots = roots.to_vec();
        self.tree = Some(tree);
        self.sel_row = 0;
        self.rebuild_visible();
        unsafe { self.rebuild_items() };
        true
    }

    /// Adds one row to the list — the way a folder dropped on the panel lands.
    /// A folder that is already on the list changes nothing and returns false.
    ///
    /// The rows already on screen keep the shape the user left them in: only the
    /// folder that just arrived opens (one level), and it is the one selected and
    /// scrolled to.
    pub(super) fn add_root(&mut self, root: &str) -> bool {
        if self.roots.iter().any(|r| same_path(r, root)) {
            return false;
        }
        let expanded = self
            .tree
            .as_ref()
            .map(|t| t.expanded_paths())
            .unwrap_or_default();
        self.roots.push(root.to_string());
        let mut tree = DirTree::from_roots(&self.roots);
        tree.restore_expanded(&expanded);
        let row = tree
            .rows()
            .iter()
            .position(|r| r.depth == 0 && r.path == root);
        if let Some(row) = row {
            if tree.rows()[row].is_dir && !tree.rows()[row].expanded {
                tree.toggle(row);
            }
        }
        self.sel_row = row.map(|r| r as isize).unwrap_or(0);
        self.tree = Some(tree);
        self.rebuild_visible();
        let sel = unsafe { self.rebuild_items() };
        unsafe { self.ensure_visible(sel) };
        true
    }

    /// The list itself, for persistence and for "does the panel already have
    /// something to show".
    pub(super) fn roots(&self) -> &[String] {
        &self.roots
    }

    /// The selected model row — the menu acts on it from outside this module.
    pub(super) fn sel_row(&self) -> isize {
        self.sel_row
    }

    /// Whether the selected row is one of the list's own rows. 「从列表中移除」
    /// greys itself out when it is not, since a file inside a root has nothing to
    /// be removed from.
    pub(super) fn can_remove_root(&self) -> bool {
        if self.sel_row < 0 {
            return false;
        }
        self.tree
            .as_ref()
            .and_then(|t| t.rows().get(self.sel_row as usize))
            .map(|r| r.depth == 0)
            .unwrap_or(false)
    }

    /// Takes one row back out of the list — the counterpart of `add_root`, so a
    /// folder dropped by mistake is not stuck there for good. Only a top-level
    /// row counts: a nested row is a file *inside* a root, and Del there must not
    /// take the whole folder away.
    pub(super) fn remove_root(&mut self, row: usize) -> Option<String> {
        let (path, expanded) = {
            let tree = self.tree.as_ref()?;
            let entry = tree.rows().get(row)?;
            if entry.depth != 0 {
                return None;
            }
            (entry.path.clone(), tree.expanded_paths())
        };
        self.roots.retain(|r| !same_path(r, &path));
        let mut tree = DirTree::from_roots(&self.roots);
        tree.restore_expanded(&expanded);
        self.tree = Some(tree);
        let rows = self.tree.as_ref().map(|t| t.rows().len()).unwrap_or(0);
        if self.sel_row == TEMP_ROW || self.sel_row as usize >= rows {
            self.sel_row = rows.saturating_sub(1) as isize;
        }
        self.rebuild_visible();
        unsafe { self.rebuild_items() };
        Some(path)
    }

    /// Re-reads every root, keeping the selection on the same path.
    pub(super) fn refresh(&mut self) {
        if self.edit.is_some() {
            // A rename box is open; rebuilding would delete the edit control out
            // from under the caret. The watcher fires again on the next change,
            // so waiting costs nothing but a stale row.
            return;
        }
        if self.roots.is_empty() {
            return;
        }
        let sel_path = self.selected_path();
        // A pending New command leaves the selection on the temporary item; the
        // real row it came from is what should be restored.
        if self.sel_row == TEMP_ROW {
            self.sel_row = self.prev_sel_row;
        }
        self.edit_temp = false;
        self.edit_item = 0;
        // 重读目录不等于重排面板：开着的文件夹照旧开着，关着的照旧关着。
        let expanded = self
            .tree
            .as_ref()
            .map(|t| t.expanded_paths())
            .unwrap_or_default();
        let mut tree = DirTree::from_roots(&self.roots);
        tree.restore_expanded(&expanded);
        self.tree = Some(tree);
        self.rebuild_visible();
        if let Some(path) = sel_path {
            if let Some(row) = self
                .visible
                .iter()
                .copied()
                .find(|&r| self.tree.as_ref().and_then(|t| t.path_at(r)) == Some(path.as_str()))
            {
                self.sel_row = row as isize;
            }
        }
        unsafe { self.rebuild_items() };
    }

    pub(super) fn selected_path(&self) -> Option<String> {
        let tree = self.tree.as_ref()?;
        if self.sel_row < 0 {
            return None;
        }
        tree.path_at(self.sel_row as usize).map(|p| p.to_string())
    }

    /// Expands every directory in the model.
    pub(super) fn expand_all(&mut self) {
        let Some(tree) = self.tree.as_mut() else { return };
        loop {
            let next = tree
                .rows()
                .iter()
                .enumerate()
                .find(|(_, r)| r.is_dir && !r.expanded && r.has_children)
                .map(|(i, _)| i);
            match next {
                Some(i) => {
                    tree.toggle(i);
                }
                None => break,
            }
        }
        self.rebuild_visible();
        unsafe { self.rebuild_items() };
    }

    /// Collapses every directory except the root, which is the view's anchor:
    /// collapsing it would empty the whole panel.
    pub(super) fn collapse_all(&mut self) {
        let Some(tree) = self.tree.as_mut() else { return };
        loop {
            let next = tree
                .rows()
                .iter()
                .enumerate()
                .find(|(i, r)| *i > 0 && r.is_dir && r.expanded)
                .map(|(i, _)| i);
            match next {
                Some(i) => {
                    tree.toggle(i);
                }
                None => break,
            }
        }
        self.rebuild_visible();
        if self.sel_row >= 0 && !self.visible.iter().any(|&r| r as isize == self.sel_row) {
            self.sel_row = self.visible.first().copied().unwrap_or(0) as isize;
        }
        unsafe { self.rebuild_items() };
    }

    /// Starts the native label edit one of the three rename commands asked for.
    /// The New commands first insert a temporary item to name, the way Explorer
    /// does: the name reaches the disk only when the edit ends.
    pub(super) unsafe fn begin_edit(&mut self, kind: EditKind) {
        if !self.alive() || self.edit.is_some() || self.sel_row < 0 {
            return;
        }
        let Some(tree) = self.tree.as_ref() else { return };
        let Some(row) = tree.rows().get(self.sel_row as usize) else {
            return;
        };
        let is_dir = row.is_dir;
        let selected = self.selected_hitem();
        if selected == 0 {
            return;
        }
        self.edit = Some(kind);
        self.edit_temp = false;
        self.prev_sel_row = self.sel_row;
        self.edit_item = selected;
        if kind != EditKind::Rename {
            // The child of a directory, or the sibling of a file: in both cases
            // the parent item the temporary row hangs under.
            let parent = if is_dir {
                selected
            } else {
                let p = SendMessageW(
                    self.hwnd,
                    TVM_GETNEXTITEM,
                    Some(WPARAM(TVGN_PARENT as usize)),
                    Some(LPARAM(selected)),
                )
                .0;
                if p == 0 { TVI_ROOT.0 } else { p }
            };
            let name: Vec<u16> = if kind == EditKind::NewFile {
                "new file"
            } else {
                "new folder"
            }
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
            let mut tmp = TVITEMW::default();
            tmp.mask = TVIF_TEXT | TVIF_PARAM;
            tmp.pszText = PWSTR(name.as_ptr() as *mut u16);
            tmp.lParam = LPARAM(TEMP_ROW);
            let mut tvis = TVINSERTSTRUCTW::default();
            tvis.hParent = HTREEITEM(parent);
            tvis.hInsertAfter = TVI_LAST;
            tvis.Anonymous = TVINSERTSTRUCTW_0 { item: tmp };
            let h = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw mut tvis as isize)),
            )
            .0;
            if h == 0 {
                self.edit = None;
                return;
            }
            self.edit_temp = true;
            self.edit_item = h;
            let _ = SendMessageW(
                self.hwnd,
                TVM_SELECTITEM,
                Some(WPARAM(TVGN_CARET as usize)),
                Some(LPARAM(h)),
            );
        }
        let edit_hwnd = SendMessageW(
            self.hwnd,
            TVM_EDITLABELW,
            Some(WPARAM(0)),
            Some(LPARAM(self.edit_item)),
        );
        if edit_hwnd.0 == 0 {
            self.abandon_edit();
        }
    }

    fn selected_hitem(&self) -> isize {
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

    /// Drops a label edit that never produced a name: the temporary item goes
    /// back out of the tree, and a failed rename loses the stale label the
    /// control kept after `TVN_ENDLABELEDIT` answered FALSE.
    pub(super) fn abandon_edit(&mut self) {
        self.edit = None;
        if self.edit_temp && self.alive() && self.edit_item != 0 {
            unsafe {
                let _ = SendMessageW(
                    self.hwnd,
                    TVM_DELETEITEM,
                    Some(WPARAM(0)),
                    Some(LPARAM(self.edit_item)),
                );
            }
        }
        self.edit_temp = false;
        self.edit_item = 0;
        self.sel_row = self.prev_sel_row;
        unsafe { self.rebuild_items() };
    }

    /// Takes the queued notifications, leaving the queue empty.
    pub(super) fn drain_notifies(&mut self) -> Vec<TreeNotify> {
        std::mem::take(&mut self.pending)
    }

    /// Runs the expand `TVN_ITEMEXPANDING` parked, and returns true when it
    /// rebuilt the list. The frame calls this from its own pump, which is the
    /// first moment outside the notification: the item list is model-owned, so
    /// showing a directory's children means deleting and re-inserting every row,
    /// and doing that while the control is inside its expand code frees the item
    /// it is about to walk again.
    pub(super) unsafe fn apply_deferred_expand(&mut self) -> bool {
        let Some((row, expand)) = self.deferred_expand.take() else {
            return false;
        };
        if let Some(tree) = self.tree.as_mut() {
            if tree.rows().get(row).is_some_and(|e| e.expanded != expand) {
                tree.toggle(row);
            }
        }
        self.rebuild_visible();
        self.rebuild_items();
        true
    }

    /// `WM_NOTIFY` from the tree, answered by the frame's procedure. Returns the
    /// message result when the notification belongs to this control.
    pub(super) unsafe fn handle_notify(
        &mut self,
        frame: HWND,
        code: u32,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match code {
            TVN_SELCHANGEDW => {
                let nm = &*(lparam.0 as *const NMTREEVIEWW);
                // The notification's `itemNew` carries the handle; `lParam` is
                // read back from the control rather than trusted, because the
                // mask comctl32 fills here is not documented to include it.
                self.sel_row = self.row_of(nm.itemNew.hItem.0).unwrap_or(TEMP_ROW);
                Some(LRESULT(0))
            }
            TVN_ITEMEXPANDINGW => {
                let nm = &*(lparam.0 as *const NMTREEVIEWW);
                let want_expand = nm.action == TVE_EXPAND;
                if let Some(row) =
                    self.row_of(nm.itemNew.hItem.0).filter(|r| *r >= 0)
                {
                    let row = row as usize;
                    let changed = self
                        .tree
                        .as_ref()
                        .and_then(|t| t.rows().get(row))
                        .is_some_and(|e| e.expanded != want_expand);
                    if changed {
                        self.deferred_expand = Some((row, want_expand));
                        self.wake(frame);
                    }
                }
                // TRUE: the control must not expand on its own. Its children are
                // the model's, so they only exist after the rebuild the deferred
                // toggle runs, and that rebuild cannot happen here: emptying the
                // item list inside this notification frees the very item the
                // control is expanding, and it goes on walking the stale handle.
                Some(LRESULT(1))
            }
            TVN_BEGINLABELEDITW => {
                // Only the three shell commands may name an item: a slow
                // double-click on a label must not open a box nobody asked for.
                Some(LRESULT(if self.edit.is_none() { 1 } else { 0 }))
            }
            TVN_ENDLABELEDITW => {
                let nm = &*(lparam.0 as *const NMTVDISPINFOW);
                let text = if nm.item.pszText.0.is_null() {
                    None
                } else {
                    let units = nm.item.pszText.0;
                    let mut len = 0usize;
                    while unsafe { *units.add(len) != 0 } {
                        len += 1;
                    }
                    let s = String::from_utf16_lossy(unsafe {
                        std::slice::from_raw_parts(units, len)
                    });
                    if s.is_empty() { None } else { Some(s) }
                };
                self.edit = None;
                match text {
                    Some(text) => self.pending.push(TreeNotify::Label { text }),
                    None => {
                        self.abandon_edit();
                        self.pending.push(TreeNotify::LabelCancelled);
                    }
                }
                self.wake(frame);
                // FALSE: the control keeps its copy of the text only until the
                // shell answers, because the shell either refreshes from disk or
                // deletes the temporary row.
                Some(LRESULT(0))
            }
            NM_CUSTOMDRAW => {
                // Same ladder as `resultlist` and the two dialogs; only the
                // colour pair is this window's business.
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
                if let Some(row) = self.hit_row(pos) {
                    self.open_row(frame, row);
                }
                None
            }
            NM_RCLICK => {
                let pos = unsafe { GetMessagePos() };
                if let Some(row) = self.hit_row(pos) {
                    if row >= 0 {
                        self.sel_row = row;
                    }
                    if let Some(h) = self.hit_item(pos) {
                        let _ = SendMessageW(
                            self.hwnd,
                            TVM_SELECTITEM,
                            Some(WPARAM(TVGN_CARET as usize)),
                            Some(LPARAM(h)),
                        );
                    }
                }
                let x = (pos & 0xFFFF) as i16 as i32;
                let y = ((pos >> 16) & 0xFFFF) as i16 as i32;
                self.pending.push(TreeNotify::ContextMenu { x, y });
                self.wake(frame);
                None
            }
            _ => None,
        }
    }

    /// The filter edit's text changed (`EN_CHANGE` reaches the parent as
    /// `WM_COMMAND`).
    pub(super) unsafe fn on_filter_change(&mut self, edit: HWND) {
        let len = GetWindowTextLengthW(edit);
        let text = if len <= 0 {
            String::new()
        } else {
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(edit, &mut buf);
            buf.truncate(n.max(0) as usize);
            String::from_utf16_lossy(&buf)
        };
        self.set_filter(&text);
    }

    /// Applies filter text and re-creates the item list from the model.
    pub(super) fn set_filter(&mut self, text: &str) {
        if self.filter == text {
            return;
        }
        self.filter = text.to_string();
        self.rebuild_visible();
        unsafe { self.rebuild_items() };
    }

    pub(super) fn filter(&self) -> &str {
        &self.filter
    }

    fn row_of(&self, item: isize) -> Option<isize> {
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
        if ok.0 == 0 { None } else { Some(it.lParam.0) }
    }

    /// Opens the file at model row `row`, or ignores a directory (expansion is
    /// the control's business, not the shell's).
    fn open_row(&mut self, frame: HWND, row: isize) {
        if row < 0 {
            return;
        }
        let Some(tree) = self.tree.as_ref() else {
            return;
        };
        let Some(entry) = tree.rows().get(row as usize) else {
            return;
        };
        if entry.is_dir {
            return;
        }
        let Some(path) = tree.path_at(row as usize) else {
            return;
        };
        self.pending.push(TreeNotify::Open {
            path: path.to_string(),
        });
        self.wake(frame);
    }

    fn hit_row(&self, pos: u32) -> Option<isize> {
        let h = self.hit_item(pos)?;
        self.row_of(h)
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

    /// Posts the frame's own wake-up message so a queued notify is drained
    /// immediately instead of on the next timer tick.
    fn wake(&self, frame: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        const WM_APP: u32 = 0x8000;
        unsafe {
            let _ = PostMessageW(Some(frame), WM_APP, WPARAM(0), LPARAM(0));
        }
    }

    /// Recreates the item list from `visible`. The control is emptied first, so
    /// the rows can be handed over in the model's display order with the
    /// expansion state the model holds. Returns the item the selection was put
    /// back on, for callers that also want it scrolled into view.
    unsafe fn rebuild_items(&mut self) -> isize {
        if !self.alive() {
            return 0;
        }
        let _ = SendMessageW(
            self.hwnd,
            TVM_DELETEITEM,
            Some(WPARAM(0)),
            Some(LPARAM(TVI_ROOT.0)),
        );
        let Some(tree) = self.tree.as_ref() else {
            return 0;
        };
        // Cloned out before the insert loop: inserting sends messages back into
        // the control, and `rows()` borrows the tree it is being built from.
        let rows: Vec<(String, usize, bool, bool, usize)> = self
            .visible
            .iter()
            .map(|&r| {
                let e = &tree.rows()[r];
                (e.name.clone(), e.depth, e.expanded, e.is_dir, r)
            })
            .collect();
        let mut parents: Vec<isize> = Vec::new();
        let mut reselect = 0isize;
        for (name, depth, expanded, is_dir, row) in rows {
            let parent = if depth == 0 {
                TVI_ROOT.0
            } else {
                *parents.get(depth - 1).unwrap_or(&TVI_ROOT.0)
            };
            let mut text: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut item = TVITEMW::default();
            item.mask = TVIF_TEXT | TVIF_PARAM | TVIF_STATE | TVIF_CHILDREN;
            item.pszText = PWSTR(text.as_mut_ptr());
            item.lParam = LPARAM(row as isize);
            item.state = if expanded { TVIS_EXPANDED } else { Default::default() };
            item.stateMask = TVIS_EXPANDED;
            item.cChildren = TVITEMEXW_CHILDREN(if is_dir { 1 } else { 0 });
            let mut tvis = TVINSERTSTRUCTW::default();
            tvis.hParent = HTREEITEM(parent);
            tvis.hInsertAfter = TVI_LAST;
            tvis.Anonymous = TVINSERTSTRUCTW_0 { item };
            let h = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&raw mut tvis as isize)),
            )
            .0;
            if h == 0 {
                continue;
            }
            if depth >= parents.len() {
                parents.resize(depth + 1, 0);
            }
            parents[depth] = h;
            if row as isize == self.sel_row {
                reselect = h;
            }
        }
        if reselect != 0 {
            let _ = SendMessageW(
                self.hwnd,
                TVM_SELECTITEM,
                Some(WPARAM(TVGN_CARET as usize)),
                Some(LPARAM(reselect)),
            );
        }
        reselect
    }

    /// Scrolls `item` into view — what "定位到这一格" means once the list has been
    /// rebuilt around it.
    unsafe fn ensure_visible(&self, item: isize) {
        if item == 0 || !self.alive() {
            return;
        }
        let _ = SendMessageW(
            self.hwnd,
            TVM_ENSUREVISIBLE,
            Some(WPARAM(0)),
            Some(LPARAM(item)),
        );
    }

    /// Display rows for the model under the current filter: the rows in
    /// expansion order, minus the descendants of a collapsed directory, minus —
    /// when a filter is set — everything that neither matches nor ancestors a
    /// match.
    fn rebuild_visible(&mut self) {
        let Some(tree) = self.tree.as_ref() else {
            self.visible.clear();
            return;
        };
        let rows = tree.rows();
        let mut order: Vec<usize> = Vec::with_capacity(rows.len());
        let mut i = 0usize;
        while i < rows.len() {
            order.push(i);
            if rows[i].is_dir && rows[i].expanded {
                i += 1;
            } else {
                let depth = rows[i].depth;
                i += 1;
                while i < rows.len() && rows[i].depth > depth {
                    i += 1;
                }
            }
        }
        if self.filter.is_empty() {
            self.visible = order;
        } else {
            let pat = self.filter.clone();
            let mut matched = vec![false; rows.len()];
            let mut keep = vec![false; rows.len()];
            for &row in &order {
                if fuzzy_match(&pat, &rows[row].name, FuzzyMode::Fuzzy).is_some() {
                    matched[row] = true;
                }
            }
            let mut ancestors: Vec<(usize, usize)> = Vec::new();
            for &row in &order {
                let depth = rows[row].depth;
                while ancestors.last().is_some_and(|&(d, _)| d >= depth) {
                    ancestors.pop();
                }
                if matched[row] {
                    keep[row] = true;
                    for &(_, a) in &ancestors {
                        keep[a] = true;
                    }
                }
                ancestors.push((depth, row));
            }
            self.visible = order.into_iter().filter(|&r| keep[r]).collect();
        }
        if self.sel_row >= 0 && !self.visible.iter().any(|&r| r as isize == self.sel_row) {
            self.sel_row = self.visible.first().copied().unwrap_or(0) as isize;
        }
    }
}

impl Default for DockTree {
    fn default() -> Self {
        Self::new()
    }
}

/// The tree's subclass. It owns the background erase and one key; every other
/// message, including the whole of the control's own selection, expansion and
/// scrolling behaviour, goes to the control unchanged.
unsafe extern "system" fn tree_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let frame = match GetParent(hwnd) {
        Ok(f) if !f.0.is_null() => f,
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    let Some(st) = state_mut(frame) else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    let orig = st.panels.dock_tree.orig;
    match msg {
        WM_ERASEBKGND => {
            // The theme's panel colour, not the system's window colour: this is
            // the one pixel the control picks up without being asked.
            let hdc = HDC(wparam.0 as *mut c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let brush = st.panels.dock_tree.bg_brush();
            let _ = FillRect(hdc, &rc, brush);
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            // A wheel message goes to the *focused* window, so pointing at a
            // dock that has not been clicked would scroll the text. Taking the
            // focus here — the way Explorer does — is what makes the wheel
            // answer the panel it is over; the control then scrolls itself.
            if GetFocus() != hwnd {
                let _ = SetFocus(Some(hwnd));
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            // Ctrl+F asks for the filter box
            // does: the panel owns the keyboard while it has the focus, and
            // the frame's own Ctrl+F (the find bar) only ever sees the
            // editor's. The box appears and takes the focus.
            if wparam.0 as u32 == 0x46 && crate::win32::window::ctrl_held() {
                st.panels.dock_tree.show_filter();
                return LRESULT(0);
            }
            if wparam.0 as u32 == 13 {
                // Enter opens the selected file. A directory is left to the
                // control, which already expands or collapses it here.
                let row = st.panels.dock_tree.sel_row;
                st.panels.dock_tree.open_row(frame, row);
                return LRESULT(0);
            }
            if wparam.0 as u32 == 0x2E {
                // 从列表中移除 Del：清单是用户攒的，得有拿掉的键。只在顶层的格上生效，
                // 里面的文件照旧交给控件。
                let row = st.panels.dock_tree.sel_row;
                if row >= 0 && st.panels.dock_tree.remove_root(row as usize).is_some() {
                    st.quick_dirs_changed();
                    return LRESULT(0);
                }
            }
            if wparam.0 as u32 == 0x44 && crate::win32::window::ctrl_held() {
                // 添加文件夹 Ctrl+D，`[MENU349] 1206` 那一行。挑中的目录
                // 和拖进来的一视同仁：往清单里加一行。
                if let Some(dir) = pick_folder(frame) {
                    st.add_quick_dirs(frame, &[dir]);
                }
                return LRESULT(0);
            }
            call_orig(orig, hwnd, msg, wparam, lparam)
        }
        _ => call_orig(orig, hwnd, msg, wparam, lparam),
    }
}

/// The filter edit's subclass: Escape empties the box and, once it is empty,
/// puts the box away and leaves for the tree; Up, Down and Enter move the caret
/// into the tree; Ctrl+F is the same ask the tree answers, so a press with the
/// box already up keeps it. Text itself is the control's, and the resulting
/// `EN_CHANGE` is what drives the filter.
unsafe extern "system" fn filter_proc(
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
    let orig = st.panels.dock_tree.filter_orig;
    if msg == WM_KEYDOWN {
        let tree = st.panels.dock_tree.hwnd();
        match wparam.0 as u32 {
            27 => {
                if st.panels.dock_tree.filter().is_empty() {
                    // Nothing left to clear: the box itself goes away and the
                    // tree takes the keyboard back.
                    st.panels.dock_tree.hide_filter();
                } else {
                    st.panels.dock_tree.set_filter("");
                    let _ = SendMessageW(
                        hwnd,
                        WM_SETTEXT,
                        Some(WPARAM(0)),
                        Some(LPARAM(PCWSTR::null().as_ptr() as isize)),
                    );
                }
                return LRESULT(0);
            }
            // Ctrl+F with the box already up: nothing to reveal, and the key
            // must not fall through to the edit.
            0x46 if crate::win32::window::ctrl_held() => {
                return LRESULT(0);
            }
            38 | 40 | 13 => {
                let _ = SetFocus(Some(tree));
                return LRESULT(0);
            }
            _ => {}
        }
    }
    call_orig(orig, hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dg_dirtree_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("readme.md"), b"hi").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), b"fn main(){}").unwrap();
        dir
    }

    /// The row set is what this module owns, so it is the part worth testing
    /// without a window: creation, expansion and the filter all reduce to it.
    #[test]
    fn visible_rows_follow_expansion_and_filter() {
        let root = temp_root("visible");
        let mut t = DockTree::new();
        assert!(t.set_roots(&[root.to_str().unwrap().to_string()]));
        // root + src + readme.md, one level deep as the model loads it.
        assert_eq!(t.visible.len(), 3);
        t.expand_all();
        assert_eq!(t.visible.len(), 4);
        t.set_filter("mai");
        // main.rs matches; src and the root stay because they ancestor it.
        let names: Vec<String> = t
            .visible
            .iter()
            .map(|&r| t.tree.as_ref().unwrap().rows()[r].name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "main.rs"), "got {names:?}");
        assert!(names.iter().any(|n| n == "src"), "got {names:?}");
        assert!(!names.iter().any(|n| n == "readme.md"), "got {names:?}");
        t.set_filter("");
        assert_eq!(t.visible.len(), 4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Dropping a second folder on the panel adds a row, it does not replace the
    /// first one — and the folders already there keep the shape the user left
    /// them in: a collapsed one stays collapsed.
    #[test]
    fn adding_a_root_keeps_the_ones_already_there() {
        let a = temp_root("add_a");
        let b = temp_root("add_b");
        let mut t = DockTree::new();
        assert!(t.set_roots(&[a.to_str().unwrap().to_string()]));
        assert_eq!(t.roots().len(), 1);
        // a + src + readme.md: a root opens one level when the panel fills.
        assert_eq!(t.visible.len(), 3);
        // 先把 a 折起来 —— 加一行不该把它撑开。
        assert!(!t.tree.as_mut().unwrap().toggle(0));
        t.rebuild_visible();
        assert_eq!(t.visible.len(), 1);

        assert!(t.add_root(b.to_str().unwrap()));
        assert_eq!(t.roots().len(), 2);
        // a 还折着，b 开一层：a、b、b/src、b/readme.md。
        assert_eq!(t.visible.len(), 4);
        assert!(!t.tree.as_ref().unwrap().rows()[0].expanded);
        assert!(t.tree.as_ref().unwrap().rows()[1].expanded);
        // The selection lands on the row that just arrived.
        assert_eq!(t.selected_path().unwrap(), b.to_str().unwrap());
        // The same folder twice is not a second row.
        assert!(!t.add_root(b.to_str().unwrap()));
        assert_eq!(t.roots().len(), 2);
        // A row *inside* a root is not a root, and must not take the folder away.
        let nested = t
            .tree
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .position(|r| r.depth != 0)
            .unwrap();
        assert!(t.remove_root(nested).is_none());
        assert_eq!(t.roots().len(), 2);
        // Del on a top-level row drops that root and the rows under it.
        let row = t.sel_row() as usize;
        assert_eq!(t.remove_root(row).unwrap(), b.to_str().unwrap());
        assert_eq!(t.roots().len(), 1);
        assert_eq!(t.visible.len(), 1, "a 还是折着的");
        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn selection_tracks_a_model_row() {
        let root = temp_root("sel");
        let mut t = DockTree::new();
        assert!(t.set_roots(&[root.to_str().unwrap().to_string()]));
        assert!(t.selected_path().unwrap().ends_with(root.file_name().unwrap().to_str().unwrap()));
        // `sel_row` is the model row an item's `lParam` carries, so the test has
        // to name the row rather than count to it.
        let src = t
            .tree
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .position(|r| r.name == "src")
            .unwrap();
        t.sel_row = src as isize;
        assert!(t.selected_path().unwrap().ends_with("src"));
        t.sel_row = TEMP_ROW;
        assert!(t.selected_path().is_none(), "a nameless item opens nothing");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The filter box is a thing you ask for: it starts hidden, `Ctrl+F` brings
    /// it up, and `Escape` on an empty box puts it away again — with the filter
    /// it held dropped, since a hidden filter would keep hiding rows.
    #[test]
    fn the_filter_box_starts_hidden_and_toggles() {
        let root = temp_root("filterbox");
        let mut t = DockTree::new();
        assert!(t.set_roots(&[root.to_str().unwrap().to_string()]));
        assert!(!t.filter_shown, "the panel rests as a plain tree");
        unsafe {
            t.show_filter();
        }
        assert!(t.filter_shown);
        // A filter set while the box was up is cleared when it goes away.
        t.set_filter("mai");
        assert!(t.visible.len() < 3, "the filter hides rows");
        unsafe {
            t.hide_filter();
        }
        assert!(!t.filter_shown);
        assert!(t.filter.is_empty(), "a hidden box holds no filter");
        assert_eq!(t.visible.len(), 3, "and the rows come back");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
