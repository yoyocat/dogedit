//! 查找 / 替换 — the two full dialogs behind `cm_find` and `cm_replace`.
//!
//! These are the big counterparts of the find bar (`minifind`): a modeless
//! 查找 box (324×99 dialog units) and a 替换 box (319×124), both with the
//! option grid, the 下一个 / 上一个 / 全部 / 替换 buttons and the 高级 /
//! 特殊字符 / 多行 / 停靠 helpers.  The templates below are the recorded
//! layouts: sizes, control ids, styles and tab order.  Captions come from the
//! language table's 查找 / 替换 dialog sections, so the code carries only the
//! English fallbacks the templates themselves were built with.
//!
//! The dialogs own pixels, not search state: the frame's `EditorState` keeps
//! the query, the options and the match list, and every control notification
//! travels back there as an [`Event`].  That keeps one copy of the model in the
//! process, the same contract the find bar follows.
//!
//! Two helpers are part of the box and live here: the `>` buttons open the
//! regex token menu into the adjacent combo, and 高级 opens the match-actions
//! menu.  The 多行 button grows the dialog down to its hidden multi-line edits;
//! the 停靠 button remembers to park the box at the bottom edge of the frame.

#![allow(unsafe_op_in_unsafe_fn)]

use std::path::Path;
use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, DeleteObject, FillRect, FrameRect, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, Polygon, SelectObject,
};
use windows::Win32::UI::Controls::{
    IsDlgButtonChecked, DRAWITEMSTRUCT, EM_GETSEL, EM_REPLACESEL, EM_SCROLLCARET, EM_SETSEL,
    ODS_SELECTED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, SetFocus, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_SETCHECK, BN_CLICKED, CreateDialogIndirectParamW, CreatePopupMenu, DLGTEMPLATE,
    DestroyMenu, DestroyWindow, GA_ROOT, GW_CHILD, GWL_STYLE, GWLP_USERDATA, GetAncestor,
    GetClientRect, GetDlgItem, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDCANCEL, IsDialogMessageW, IsWindow, MF_POPUP, MF_SEPARATOR, MF_STRING,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MSG, MapDialogRect, IDYES,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, SW_HIDE,
    SW_SHOW, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM,
    WM_ERASEBKGND, WM_INITDIALOG, WM_MOVE, WS_CLIPCHILDREN,
};
use windows::core::PCWSTR;

use crate::win32::customdraw;
use crate::win32::dlgtpl::{self, Class, Item};
use crate::win32::ownerdraw;
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_render::theme::Theme;

// ---------------------------------------------------------------------------
// The model the frame keeps
// ---------------------------------------------------------------------------

/// The replace dialog's 范围 radio group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    #[default]
    CurrentFile,
    Selection,
    AllOpenFiles,
}

/// Everything the dialogs remember between runs. The four core find knobs
/// (case / word / regex / backward) live in `dg_core::FindOptions`; this is
/// the UI half that core does not need, plus the copies so one dialog open can
/// seed the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Options {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub wrap: bool,
    pub extended: bool,
    pub close_if_found: bool,
    pub incremental: bool,
    pub highlight: bool,
    pub continue_next: bool,
    pub ultra: bool,
    /// 停靠: park the box at the bottom edge of the frame.
    pub bottom: bool,
    /// 多行: the dialog is grown down to the multi-line edits.
    pub multiline: bool,
    pub fast_replace: bool,
    pub backward: bool,
    pub scope: Scope,
}

impl Options {
    /// The option relationships the 查找 / 替换 boxes enforce. It is a pure
    /// function so the rules are testable without a window.
    pub(super) fn normalize(&mut self) {
        if self.regex {
            // The regex engine supplies its own whole-word / escape grammar.
            self.whole_word = false;
            self.extended = false;
        }
        if self.ultra {
            self.regex = false;
        }
        if self.multiline {
            // The multi-line edits own the query text; the engine's
            // incremental pass and the single-line high-speed scanner
            // cannot run beside them.
            self.incremental = false;
            self.ultra = false;
        }
        if self.wrap {
            self.continue_next = false;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Model {
    pub query: String,
    pub replace: String,
    pub opts: Options,
}

/// One request from a dialog control to the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// The find text changed (combo or multi-line edit).
    Query,
    /// The replacement text changed.
    ReplaceText,
    /// A checkbox / radio / the two helper toggles changed.
    Option,
    Next,
    Prev,
    FindAll,
    ReplaceOne,
    ReplaceAll,
    Batch,
    SwitchToFind,
    SwitchToReplace,
    /// A row of the 高级 match-actions menu.
    Advanced(i32),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Find,
    Replace,
}

impl Kind {
    fn dialog_id(self) -> u32 {
        match self {
            Kind::Find => 205,
            Kind::Replace => 211,
        }
    }

    fn slot(self) -> &'static AtomicIsize {
        match self {
            Kind::Find => &FIND,
            Kind::Replace => &REPLACE,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Kind::Find => "Find",
            Kind::Replace => "Replace",
        }
    }

    fn width(self) -> u16 {
        match self {
            Kind::Find => 324,
            Kind::Replace => 319,
        }
    }

    fn collapsed_h(self) -> u16 {
        match self {
            Kind::Find => 99,
            Kind::Replace => 124,
        }
    }

    fn expanded_h(self) -> u16 {
        match self {
            Kind::Find => 169,
            Kind::Replace => 255,
        }
    }
}

static FIND: AtomicIsize = AtomicIsize::new(0);
static REPLACE: AtomicIsize = AtomicIsize::new(0);
/// Last moved-to corner, in screen pixels, so switching between the two boxes
/// does not snap them back to the screen centre. 0/0 means "not placed yet".
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);

// ---------------------------------------------------------------------------
// Control ids and the recorded templates
// ---------------------------------------------------------------------------

const ID_QUERY: i32 = 1200;
const ID_NEXT: i32 = 1201;
const ID_PREV: i32 = 1202;
const ID_CASE: i32 = 1204;
const ID_WORD: i32 = 1205;
const ID_REGEX: i32 = 1206;
const ID_WRAP: i32 = 1207;
const ID_CLOSE: i32 = 1208;
const ID_HIGHLIGHT: i32 = 1209;
const ID_TOKENS: i32 = 1211;
const ID_FIND_ALL: i32 = 1212;
const ID_REPLACE_ONE: i32 = 1213;
const ID_REPLACE_ALL: i32 = 1214;
const ID_REPLACE_TEXT: i32 = 1215;
const ID_TOKENS_REPLACE: i32 = 1217;
const ID_INCR: i32 = 1218;
const ID_TO_FIND: i32 = 1220;
const ID_ADVANCED: i32 = 1226;
const ID_FAST_REPLACE: i32 = 1227;
const ID_MULTILINE: i32 = 1238;
const ID_DOCK: i32 = 1241;
const ID_FAV: i32 = 1242;
const ID_ULTRA: i32 = 1243;
const ID_BATCH: i32 = 1240;
const ID_TO_REPLACE: i32 = 1219;
const ID_EXTENDED: i32 = 1234;
const ID_CONTINUE: i32 = 1235;
const ID_SCOPE_CURRENT: i32 = 1229;
const ID_SCOPE_SELECTION: i32 = 1230;
const ID_SCOPE_ALL: i32 = 1231;
const ID_BIG_QUERY: i32 = 1237;
const ID_BIG_REPLACE: i32 = 1239;
const ID_QUERY_LABEL: i32 = 3000;
const ID_REPLACE_LABEL: i32 = 3001;
const ID_SCOPE_LABEL: i32 = 3002;
const ID_BIG_QUERY_LABEL: i32 = 3003;
const ID_BIG_REPLACE_LABEL: i32 = 3004;
const ID_RULE: i32 = 1228;

const DLG_STYLE: u32 = 0x80cc_08cc;
const DLG_EXSTYLE: u32 = 0x0000_0080;

const ATOM_BUTTON: u16 = 0x0080;
const ATOM_EDIT: u16 = 0x0081;
const ATOM_STATIC: u16 = 0x0082;
const ATOM_COMBO: u16 = 0x0085;

const CHECK: u32 = 0x5001_0003;
const RADIO: u32 = 0x5000_0009;
const RADIO_GROUP: u32 = 0x5002_0009;
const PUSH: u32 = 0x5001_0000;
const PUSH_DEF: u32 = 0x5001_0001;
const PUSH_NOTIFY: u32 = 0x5001_0100;
const PUSH_GROUP: u32 = 0x5001_1000;
const STATIC: u32 = 0x5002_0000;
const RULE: u32 = 0x5000_1007;
const COMBO: u32 = 0x5001_0242;
const MULTI_EDIT: u32 = 0x5081_0084;

/// 查找: the recorded 23 items in template (tab) order.
const FIND_ITEMS: &[Item] = &[
    Item { id: ID_QUERY_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 8, y: 9, w: 30, h: 12, label: "&Find:" },
    Item { id: ID_QUERY, class: Class::Atom(ATOM_COMBO), style: COMBO, x: 40, y: 7, w: 194, h: 12, label: "" },
    Item { id: ID_RULE, class: Class::Atom(ATOM_STATIC), style: RULE, x: 8, y: 78, w: 308, h: 1, label: "" },
    Item { id: ID_TOKENS, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 238, y: 6, w: 15, h: 14, label: ">" },
    Item { id: ID_CASE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 25, w: 105, h: 10, label: "Case &sensitive" },
    Item { id: ID_WORD, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 120, y: 25, w: 110, h: 10, label: "&Whole word" },
    Item { id: ID_REGEX, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 38, w: 105, h: 10, label: "Regular &expression" },
    Item { id: ID_WRAP, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 120, y: 37, w: 112, h: 12, label: "Wrap at end &of file" },
    Item { id: ID_EXTENDED, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 51, w: 110, h: 10, label: "E&xtended (\\n,\\t)" },
    Item { id: ID_CONTINUE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 120, y: 48, w: 110, h: 15, label: "Con&tinue to next file" },
    Item { id: ID_CLOSE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 64, w: 110, h: 10, label: "Close if foun&d" },
    Item { id: ID_ULTRA, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 120, y: 63, w: 115, h: 10, label: "&Ultra-high-speed find" },
    Item { id: ID_HIGHLIGHT, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 84, w: 105, h: 10, label: "&Highlight result" },
    Item { id: ID_INCR, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 120, y: 84, w: 85, h: 10, label: "&Incremental Search" },
    Item { id: ID_NEXT, class: Class::Atom(ATOM_BUTTON), style: PUSH_DEF, x: 257, y: 6, w: 60, h: 14, label: "Find &Next" },
    Item { id: ID_PREV, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 257, y: 22, w: 60, h: 14, label: "&Previous" },
    Item { id: ID_FIND_ALL, class: Class::Atom(ATOM_BUTTON), style: PUSH_NOTIFY, x: 257, y: 38, w: 60, h: 14, label: "Find &All" },
    Item { id: ID_TO_REPLACE, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 257, y: 54, w: 60, h: 14, label: "Re&place>>" },
    Item { id: ID_DOCK, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 221, y: 81, w: 15, h: 14, label: "" },
    Item { id: ID_MULTILINE, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 240, y: 81, w: 15, h: 14, label: "." },
    Item { id: ID_ADVANCED, class: Class::Atom(ATOM_BUTTON), style: PUSH_GROUP, x: 257, y: 81, w: 60, h: 14, label: "Ad&vanced" },
    Item { id: ID_BIG_QUERY, class: Class::Atom(ATOM_EDIT), style: MULTI_EDIT, x: 7, y: 112, w: 279, h: 50, label: "" },
    Item { id: ID_BIG_QUERY_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 8, y: 99, w: 190, h: 10, label: "Find:" },
];

/// 替换: the recorded 31 items in template (tab) order.
const REPLACE_ITEMS: &[Item] = &[
    Item { id: ID_QUERY_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 7, y: 9, w: 30, h: 13, label: "&Find:" },
    Item { id: ID_QUERY, class: Class::Atom(ATOM_COMBO), style: COMBO, x: 40, y: 7, w: 172, h: 12, label: "" },
    Item { id: ID_REPLACE_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 7, y: 25, w: 36, h: 15, label: "Repla&ce:" },
    Item { id: ID_REPLACE_TEXT, class: Class::Atom(ATOM_COMBO), style: COMBO, x: 40, y: 23, w: 172, h: 12, label: "" },
    Item { id: ID_TOKENS, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 216, y: 7, w: 15, h: 14, label: ">" },
    Item { id: ID_TOKENS_REPLACE, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 216, y: 22, w: 15, h: 14, label: ">" },
    Item { id: ID_FAV, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 234, y: 7, w: 16, h: 14, label: "" },
    Item { id: ID_CASE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 41, w: 110, h: 12, label: "Case &sensitive" },
    Item { id: ID_WORD, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 122, y: 41, w: 110, h: 13, label: "&Whole word" },
    Item { id: ID_REGEX, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 53, w: 110, h: 16, label: "Regular &expression" },
    Item { id: ID_WRAP, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 122, y: 55, w: 113, h: 12, label: "Wrap at end &of file" },
    Item { id: ID_EXTENDED, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 70, w: 105, h: 10, label: "E&xtended (\\n,\\t)" },
    Item { id: ID_CONTINUE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 122, y: 70, w: 115, h: 10, label: "Con&tinue to next file" },
    Item { id: ID_HIGHLIGHT, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 7, y: 82, w: 93, h: 15, label: "&Highlight result" },
    Item { id: ID_FAST_REPLACE, class: Class::Atom(ATOM_BUTTON), style: CHECK, x: 122, y: 83, w: 125, h: 12, label: "Fast ReplaceAll &mode" },
    Item { id: ID_SCOPE_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 7, y: 99, w: 29, h: 10, label: "Scope" },
    Item { id: ID_RULE, class: Class::Atom(ATOM_STATIC), style: RULE, x: 42, y: 103, w: 270, h: 1, label: "" },
    Item { id: ID_SCOPE_CURRENT, class: Class::Atom(ATOM_BUTTON), style: RADIO_GROUP, x: 7, y: 109, w: 64, h: 11, label: "&Current File" },
    Item { id: ID_SCOPE_SELECTION, class: Class::Atom(ATOM_BUTTON), style: RADIO, x: 100, y: 109, w: 62, h: 11, label: "Se&lection" },
    Item { id: ID_SCOPE_ALL, class: Class::Atom(ATOM_BUTTON), style: RADIO, x: 183, y: 108, w: 83, h: 12, label: "All Opene&d Files" },
    Item { id: ID_NEXT, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 252, y: 7, w: 60, h: 14, label: "Find &Next" },
    Item { id: ID_REPLACE_ONE, class: Class::Atom(ATOM_BUTTON), style: PUSH_DEF, x: 252, y: 23, w: 60, h: 14, label: "&Replace" },
    Item { id: ID_REPLACE_ALL, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 252, y: 39, w: 60, h: 14, label: "Replace &All" },
    Item { id: ID_BATCH, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 252, y: 54, w: 60, h: 14, label: "Batch" },
    Item { id: ID_TO_FIND, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 252, y: 69, w: 60, h: 14, label: "<<F&ind" },
    Item { id: ID_DOCK, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 279, y: 87, w: 16, h: 14, label: "" },
    Item { id: ID_MULTILINE, class: Class::Atom(ATOM_BUTTON), style: PUSH, x: 297, y: 87, w: 15, h: 14, label: "." },
    Item { id: ID_BIG_QUERY_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 9, y: 123, w: 190, h: 10, label: "Find:" },
    Item { id: ID_BIG_QUERY, class: Class::Atom(ATOM_EDIT), style: MULTI_EDIT, x: 7, y: 135, w: 279, h: 47, label: "" },
    Item { id: ID_BIG_REPLACE_LABEL, class: Class::Atom(ATOM_STATIC), style: STATIC, x: 7, y: 187, w: 155, h: 11, label: "Replace:" },
    Item { id: ID_BIG_REPLACE, class: Class::Atom(ATOM_EDIT), style: MULTI_EDIT, x: 7, y: 200, w: 279, h: 48, label: "" },
];

// ---------------------------------------------------------------------------
// Dialog object
// ---------------------------------------------------------------------------

struct Inner {
    frame: HWND,
    kind: Kind,
    model: Model,
    theme: Theme,
    bg: HBRUSH,
    field: HBRUSH,
    ink: COLORREF,
    face: COLORREF,
    field_face: COLORREF,
    face_brush: HBRUSH,
    hot_brush: HBRUSH,
    press_brush: HBRUSH,
    edge_brush: HBRUSH,
    ink_brush: HBRUSH,
    tip_host: Option<crate::win32::tiphost::TipHost>,
    /// The UI font for the children; the template's 8 pt `DS_SETFONT` block
    /// stays put because the dialog manager measures the layout with it.
    font: HFONT,
}

unsafe fn state(hwnd: HWND) -> &'static mut Inner {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Inner)
}

fn hwnd_of(kind: Kind) -> Option<HWND> {
    let raw = kind.slot().load(Ordering::Acquire);
    if raw == 0 {
        None
    } else {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        unsafe { IsWindow(Some(hwnd)).as_bool().then_some(hwnd) }
    }
}

pub(super) fn is_open(kind: Kind) -> bool {
    hwnd_of(kind).is_some()
}

/// Builds one template. The language table rewrites every caption after
/// creation, so the labels here are the English fallbacks.
fn template(kind: Kind) -> Vec<u8> {
    let items = match kind {
        Kind::Find => FIND_ITEMS,
        Kind::Replace => REPLACE_ITEMS,
    };
    dlgtpl::build_legacy(
        kind.title(),
        DLG_STYLE,
        DLG_EXSTYLE,
        kind.width(),
        kind.collapsed_h(),
        400,
        items,
    )
}

/// Opens (or re-focuses) one dialog. `model` is the frame's current query,
/// replacement and options.
pub(super) fn show(frame: HWND, kind: Kind, model: &Model, theme: &Theme) {
    if let Some(hwnd) = hwnd_of(kind) {
        unsafe {
            write_model(hwnd, kind, model);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            focus_query(hwnd, kind);
        }
        return;
    }
    hide_all();
    let other = match kind {
        Kind::Find => Kind::Replace,
        Kind::Replace => Kind::Find,
    };
    hide(other);

    let inner = Box::new(Inner {
        frame,
        kind,
        model: model.clone(),
        theme: theme.clone(),
        bg: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        ink: to_colorref(theme.toolbar_fg),
        face: to_colorref(theme.toolbar_bg),
        field_face: to_colorref(theme.toolbar_field),
        face_brush: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_bg)) },
        hot_brush: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_hot)) },
        press_brush: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_selected)) },
        edge_brush: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_border)) },
        ink_brush: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_fg)) },
        tip_host: None,
        font: unsafe {
            crate::win32::uifont::create(crate::win32::uifont::window_dpi(frame))
        },
    });
    let param = LPARAM(Box::into_raw(inner) as isize);
    let tmpl = template(kind);
    let hwnd = match unsafe {
        CreateDialogIndirectParamW(
            Some(instance()),
            &*(tmpl.as_ptr() as *const DLGTEMPLATE),
            Some(frame),
            Some(dialog_proc),
            param,
        )
    } {
        Ok(h) => h,
        Err(_) => {
            unsafe {
                let inner = Box::from_raw(param.0 as *mut Inner);
                let _ = DeleteObject(inner.font.into());
            }
            return;
        }
    };
    kind.slot().store(hwnd.0 as isize, Ordering::Release);
    unsafe {
        let dark = crate::win32::chrome::theme_is_dark(theme);
        crate::win32::chrome::set_chrome_theme(theme);
        crate::win32::chrome::dark_theme_children(hwnd, dark);
        crate::win32::chrome::title_bar(hwnd, dark);
        // The template has no WS_CLIPCHILDREN; without it the parent erase
        // would also cover the buttons each time the dialog background changes.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) | (WS_CLIPCHILDREN.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        focus_query(hwnd, kind);
    }
}

pub(super) fn hide(kind: Kind) {
    if let Some(hwnd) = hwnd_of(kind) {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        kind.slot().store(0, Ordering::Release);
    }
}

pub(super) fn hide_all() {
    hide(Kind::Find);
    hide(Kind::Replace);
}

unsafe fn focus_query(hwnd: HWND, kind: Kind) {
    let combo = kind_combo(kind);
    if let Ok(c) = GetDlgItem(Some(hwnd), combo) {
        let _ = SetFocus(Some(c));
    }
}

fn kind_combo(kind: Kind) -> i32 {
    if kind == Kind::Find {
        ID_QUERY
    } else {
        ID_QUERY
    }
}

/// The model as the controls currently show it.
pub(super) fn read(kind: Kind) -> Model {
    let Some(hwnd) = hwnd_of(kind) else {
        return Model::default();
    };
    unsafe {
        let st = state(hwnd);
        let query = if st.model.opts.multiline {
            text_of(hwnd, ID_BIG_QUERY)
        } else {
            text_of(hwnd, ID_QUERY)
        };
        let replace = if kind == Kind::Replace {
            if st.model.opts.multiline {
                text_of(hwnd, ID_BIG_REPLACE)
            } else {
                text_of(hwnd, ID_REPLACE_TEXT)
            }
        } else {
            st.model.replace.clone()
        };
        let mut opts = Options {
            case_sensitive: checked(hwnd, ID_CASE),
            whole_word: checked(hwnd, ID_WORD),
            regex: checked(hwnd, ID_REGEX),
            wrap: checked(hwnd, ID_WRAP),
            extended: checked(hwnd, ID_EXTENDED),
            // These two live in the 查找 box only; the 替换 box keeps the
            // value the shared model already carries.
            close_if_found: if kind == Kind::Find {
                checked(hwnd, ID_CLOSE)
            } else {
                st.model.opts.close_if_found
            },
            incremental: if kind == Kind::Find {
                checked(hwnd, ID_INCR)
            } else {
                st.model.opts.incremental
            },
            highlight: checked(hwnd, ID_HIGHLIGHT),
            continue_next: checked(hwnd, ID_CONTINUE),
            ultra: if kind == Kind::Find {
                checked(hwnd, ID_ULTRA)
            } else {
                st.model.opts.ultra
            },
            fast_replace: kind == Kind::Replace && checked(hwnd, ID_FAST_REPLACE),
            bottom: st.model.opts.bottom,
            multiline: st.model.opts.multiline,
            backward: st.model.opts.backward,
            scope: if kind == Kind::Replace {
                if checked(hwnd, ID_SCOPE_SELECTION) {
                    Scope::Selection
                } else if checked(hwnd, ID_SCOPE_ALL) {
                    Scope::AllOpenFiles
                } else {
                    Scope::CurrentFile
                }
            } else {
                Scope::CurrentFile
            },
        };
        opts.normalize();
        Model { query, replace, opts }
    }
}

/// Pushes `model` into the controls. Text fields are only rewritten when they
/// differ, so a refresh does not move the caret while the user types.
pub(super) fn write(kind: Kind, model: &Model) {
    if let Some(hwnd) = hwnd_of(kind) {
        unsafe {
            write_model(hwnd, kind, model);
        }
    }
}

unsafe fn write_model(hwnd: HWND, kind: Kind, model: &Model) {
    let st = state(hwnd);
    let old_multiline = st.model.opts.multiline;
    let old_bottom = st.model.opts.bottom;
    st.model = model.clone();
    if text_of(hwnd, ID_QUERY) != model.query {
        set_text(hwnd, ID_QUERY, &model.query);
    }
    if kind == Kind::Replace && text_of(hwnd, ID_REPLACE_TEXT) != model.replace {
        set_text(hwnd, ID_REPLACE_TEXT, &model.replace);
    }
    if old_multiline != model.opts.multiline {
        set_expanded(hwnd, kind, model.opts.multiline);
    }
    if model.opts.multiline {
        if text_of(hwnd, ID_BIG_QUERY) != model.query {
            set_text(hwnd, ID_BIG_QUERY, &model.query);
        }
        if kind == Kind::Replace && text_of(hwnd, ID_BIG_REPLACE) != model.replace {
            set_text(hwnd, ID_BIG_REPLACE, &model.replace);
        }
    }
    let _ = checked_set(hwnd, ID_CASE, model.opts.case_sensitive);
    let _ = checked_set(hwnd, ID_WORD, model.opts.whole_word);
    let _ = checked_set(hwnd, ID_REGEX, model.opts.regex);
    let _ = checked_set(hwnd, ID_WRAP, model.opts.wrap);
    let _ = checked_set(hwnd, ID_EXTENDED, model.opts.extended);
    let _ = checked_set(hwnd, ID_CLOSE, model.opts.close_if_found);
    let _ = checked_set(hwnd, ID_INCR, model.opts.incremental);
    let _ = checked_set(hwnd, ID_HIGHLIGHT, model.opts.highlight);
    let _ = checked_set(hwnd, ID_CONTINUE, model.opts.continue_next);
    if kind == Kind::Find {
        let _ = checked_set(hwnd, ID_ULTRA, model.opts.ultra);
    }
    if kind == Kind::Replace {
        let _ = checked_set(hwnd, ID_FAST_REPLACE, model.opts.fast_replace);
        let scope = match model.opts.scope {
            Scope::CurrentFile => ID_SCOPE_CURRENT,
            Scope::Selection => ID_SCOPE_SELECTION,
            Scope::AllOpenFiles => ID_SCOPE_ALL,
        };
        let _ = windows::Win32::UI::Controls::CheckRadioButton(
            hwnd,
            ID_SCOPE_CURRENT,
            ID_SCOPE_ALL,
            scope,
        );
    }
    // The regex engine owns the whole-word boundary and the escape grammar;
    // the multi-line edits suspend the single-line helpers.
    for (id, on) in [
        (ID_WORD, !model.opts.regex),
        (ID_EXTENDED, !model.opts.regex && !model.opts.multiline),
        (ID_INCR, !model.opts.multiline),
        (ID_REGEX, !model.opts.ultra),
        (ID_ULTRA, !model.opts.multiline),
        (ID_ADVANCED, !model.opts.multiline),
        (ID_FIND_ALL, !model.opts.multiline),
    ] {
        if let Ok(c) = GetDlgItem(Some(hwnd), id) {
            let _ = EnableWindow(c, on);
        }
    }
    if old_bottom != model.opts.bottom {
        place(hwnd, st);
    }
    if model.opts.multiline {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

// ---------------------------------------------------------------------------
// Text / check helpers
// ---------------------------------------------------------------------------

unsafe fn text_of(hwnd: HWND, id: i32) -> String {
    let Ok(c) = GetDlgItem(Some(hwnd), id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(c).max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let n = GetWindowTextW(c, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

unsafe fn set_text(hwnd: HWND, id: i32, text: &str) {
    let Ok(c) = GetDlgItem(Some(hwnd), id) else {
        return;
    };
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(c, PCWSTR(buf.as_ptr()));
}

unsafe fn checked(hwnd: HWND, id: i32) -> bool {
    IsDlgButtonChecked(hwnd, id) != 0
}

unsafe fn checked_set(hwnd: HWND, id: i32, on: bool) -> windows::core::Result<()> {
    let c = GetDlgItem(Some(hwnd), id)?;
    let _ = SendMessageW(
        c,
        BM_SETCHECK,
        Some(WPARAM(usize::from(on))),
        None,
    );
    Ok(())
}

/// 多行: show or hide the lower edit block and resize the box to fit it.
unsafe fn set_expanded(hwnd: HWND, kind: Kind, on: bool) {
    for id in [
        Some(ID_BIG_QUERY),
        Some(ID_BIG_QUERY_LABEL),
        (kind == Kind::Replace).then_some(ID_BIG_REPLACE),
        (kind == Kind::Replace).then_some(ID_BIG_REPLACE_LABEL),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(c) = GetDlgItem(Some(hwnd), id) {
            let _ = ShowWindow(c, if on { SW_SHOW } else { SW_HIDE });
        }
    }
    resize_to(hwnd, kind, on);
}

unsafe fn resize_to(hwnd: HWND, kind: Kind, expanded: bool) {
    let mut outer = RECT::default();
    let mut client = RECT::default();
    if GetWindowRect(hwnd, &mut outer).is_err() || GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let nc_h = (outer.bottom - outer.top) - client.bottom;
    let h = if expanded {
        kind.expanded_h()
    } else {
        kind.collapsed_h()
    };
    let mut units = RECT { left: 0, top: 0, right: 0, bottom: h as i32 };
    if MapDialogRect(hwnd, &mut units).is_err() {
        return;
    }
    let h = units.bottom + nc_h;
    let w = outer.right - outer.left;
    let _ = SetWindowPos(
        hwnd,
        None,
        0,
        0,
        w,
        h,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

/// Places a docked box at the frame's bottom-right corner, and leaves a
/// floating one where the user left it.
unsafe fn place(hwnd: HWND, st: &Inner) {
    if !st.model.opts.bottom {
        return;
    }
    let mut fr = RECT::default();
    let mut origin = POINT { x: 0, y: 0 };
    let mut wr = RECT::default();
    if GetClientRect(st.frame, &mut fr).is_err()
        || GetWindowRect(hwnd, &mut wr).is_err()
    {
        return;
    }
    let _ = ClientToScreen(st.frame, &mut origin);
    let w = wr.right - wr.left;
    let h = wr.bottom - wr.top;
    let x = origin.x + fr.right - w - 8;
    let y = origin.y + fr.bottom - h - 8;
    let _ = SetWindowPos(
        hwnd,
        None,
        x.max(origin.x),
        y.max(origin.y),
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
}

/// Keeps docked boxes at the frame's bottom edge as the frame moves/sizes.
pub(super) fn reposition(_frame: HWND) {
    for kind in [Kind::Find, Kind::Replace] {
        if let Some(hwnd) = hwnd_of(kind) {
            unsafe {
                let st = state(hwnd);
                if st.model.opts.bottom {
                    place(hwnd, st);
                }
            }
        }
    }
}

/// Whether the frame's loop should route `msg` through the dialog manager
/// (Tab, Enter, Escape).
pub(super) fn translate(msg: &MSG) -> bool {
    for kind in [Kind::Find, Kind::Replace] {
        let Some(hwnd) = hwnd_of(kind) else {
            continue;
        };
        if unsafe { GetAncestor(msg.hwnd, GA_ROOT) } != hwnd {
            continue;
        }
        if unsafe { IsDialogMessageW(hwnd, msg).as_bool() } {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------

/// The regex token menu. The main list is the recorded order; a 0 is the
/// separator the menu draws between the escape, anchor and group families.
const TOKEN_MAIN: &[u32] = &[
    1, 2, 0, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 0, 28, 29, 30, 31,
];
const TOKEN_MORE: &[u32] = &[
    201, 202, 203, 204, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 245,
];
const TOKEN_MORE_ID: usize = 201;

/// 高级: the match-actions menu.
const ADVANCED_ROWS: &[(i32, &str)] = &[
    (1221, "过滤匹配"),
    (1224, "统计匹配数"),
    (1225, "设置书签"),
    (1233, "筛选匹配的行"),
    (1244, "筛选不匹配的行"),
    (1245, "选择所有匹配"),
];

fn menu_label(section: u32, id: u32, popup: bool, fallback: &str) -> String {
    crate::win32::lang::menu_text(section, id, popup, fallback)
}

/// The text a token row inserts: the caption up to its first tab.
fn token_of(label: &str) -> &str {
    label.split('\t').next().unwrap_or(label)
}

unsafe fn append_menu(menu: HMENU, id: usize, label: &str, flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS) -> bool {
    let buf: Vec<u16> = label.encode_utf16().chain([0]).collect();
    AppendMenuW(menu, flags, id, PCWSTR(buf.as_ptr())).is_ok()
}

/// Tracks a popup at `button`'s bottom-left and returns the chosen id.
unsafe fn track_at(menu: HMENU, dlg: HWND, button: i32) -> i32 {
    let Ok(btn) = GetDlgItem(Some(dlg), button) else {
        let _ = DestroyMenu(menu);
        return 0;
    };
    let mut rect = RECT::default();
    if GetWindowRect(btn, &mut rect).is_err() {
        let _ = DestroyMenu(menu);
        return 0;
    }
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY,
        rect.left,
        rect.bottom,
        None,
        dlg,
        None,
    )
    .0 as i32;
    let _ = DestroyMenu(menu);
    chosen
}

/// The `>` history: insert the chosen token into the combo's edit child.
unsafe fn on_token_menu(dlg: HWND, combo: i32, button: i32) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let mut complete = true;
    for id in TOKEN_MAIN {
        if *id == 0 {
            if !append_menu(menu, 0, "", MF_SEPARATOR) {
                complete = false;
                break;
            }
            continue;
        }
        let fallback = match id {
            1 => "\\n\t换行符",
            2 => "\\t\t制表符",
            _ => "",
        };
        let label = menu_label(307, *id, false, fallback);
        if label.is_empty() || !append_menu(menu, *id as usize, &label, MF_STRING) {
            complete = false;
            break;
        }
    }
    if complete {
        let more = CreatePopupMenu().unwrap_or_default();
        let mut more_ok = !more.0.is_null();
        if more_ok {
            for id in TOKEN_MORE {
                let label = menu_label(307, *id, false, "");
                if label.is_empty() || !append_menu(more, *id as usize, &label, MF_STRING) {
                    more_ok = false;
                    break;
                }
            }
        }
        if more_ok {
            let caption = menu_label(307, TOKEN_MORE_ID as u32, true, "更多...");
            let _ = AppendMenuW(menu, MF_POPUP, more.0 as usize, PCWSTR::from_raw(wide(&caption).as_ptr()));
        } else {
            let _ = DestroyMenu(more);
        }
    }
    if !complete {
        let _ = DestroyMenu(menu);
        return;
    }
    let chosen = track_at(menu, dlg, button);
    if chosen == 0 {
        return;
    }
    // The row's caption before the tab is the literal token.
    let label = menu_label(307, chosen as u32, false, "");
    if label.is_empty() {
        return;
    }
    let token = token_of(&label).to_string();
    let Ok(c) = GetDlgItem(Some(dlg), combo) else {
        return;
    };
    let edit = GetWindow(c, GW_CHILD).unwrap_or_default();
    if edit.0.is_null() {
        return;
    }
    let mut start = 0i32;
    let mut end = 0i32;
    let _ = SendMessageW(
        edit,
        EM_GETSEL,
        Some(WPARAM(&raw mut start as usize)),
        Some(LPARAM(&raw mut end as isize)),
    );
    let _ = SendMessageW(
        edit,
        EM_SETSEL,
        Some(WPARAM(start as usize)),
        Some(LPARAM(start as isize)),
    );
    let buf: Vec<u16> = token.encode_utf16().chain([0]).collect();
    let _ = SendMessageW(
        edit,
        EM_REPLACESEL,
        Some(WPARAM(1)),
        Some(LPARAM(buf.as_ptr() as isize)),
    );
    let _ = SetFocus(Some(edit));
    let at = start + token.encode_utf16().count() as i32;
    let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(at as usize)), Some(LPARAM(at as isize)));
    let _ = SendMessageW(edit, EM_SCROLLCARET, None, None);
}

unsafe fn on_advanced_menu(dlg: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let mut complete = true;
    for (id, fallback) in ADVANCED_ROWS {
        let label = menu_label(329, *id as u32, false, fallback);
        if !append_menu(menu, *id as usize, &label, MF_STRING) {
            complete = false;
            break;
        }
    }
    if !complete {
        let _ = DestroyMenu(menu);
        return;
    }
    let chosen = track_at(menu, dlg, ID_ADVANCED);
    if chosen != 0 {
        let st = state(dlg);
        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Advanced(chosen));
    }
}

/// 替换收藏: one saved find/replace pair. `flag` packs the option grid the
/// pair was saved with; the bits are the shared set the 查找 box carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FavEntry {
    title: String,
    find: String,
    replace: String,
    flag: i64,
}

const FAV_ADD: usize = 1200;
const FAV_DELETE: usize = 1201;
const FAV_ITEM_BASE: usize = 10;

const FAV_WORD: i64 = 0x01;
const FAV_CASE: i64 = 0x02;
const FAV_REGEX: i64 = 0x04;
const FAV_CLOSE: i64 = 0x40;
const FAV_HIGHLIGHT: i64 = 0x80;
const FAV_INCR: i64 = 0x200;
const FAV_MULTILINE: i64 = 0x400;

fn fav_flag(o: &Options) -> i64 {
    let mut f = 0;
    if o.whole_word {
        f |= FAV_WORD;
    }
    if o.case_sensitive {
        f |= FAV_CASE;
    }
    if o.regex {
        f |= FAV_REGEX;
    }
    if o.close_if_found {
        f |= FAV_CLOSE;
    }
    if o.highlight {
        f |= FAV_HIGHLIGHT;
    }
    if o.incremental {
        f |= FAV_INCR;
    }
    if o.multiline {
        f |= FAV_MULTILINE;
    }
    f
}

/// Restores the packed options; everything outside the pack (wrap, 扩展,
/// 继续, 高速, 高速替换, 范围) keeps the value the open box already has.
fn fav_apply(o: &mut Options, flag: i64) {
    o.whole_word = flag & FAV_WORD != 0;
    o.case_sensitive = flag & FAV_CASE != 0;
    o.regex = flag & FAV_REGEX != 0;
    o.close_if_found = flag & FAV_CLOSE != 0;
    o.highlight = flag & FAV_HIGHLIGHT != 0;
    o.incremental = flag & FAV_INCR != 0;
    o.multiline = flag & FAV_MULTILINE != 0;
}

fn fav_load(path: &Path) -> Vec<FavEntry> {
    let ini = crate::win32::grepdlg::ini_load(path);
    let count = crate::win32::grepdlg::ini_get(&ini, "ReplaceFav", "Count", "0")
        .trim()
        .parse::<usize>()
        .unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..count.min(64) {
        let sec = format!("ReplaceFav{i}");
        let title = crate::win32::grepdlg::ini_get(&ini, &sec, "Title", "");
        if title.is_empty() {
            continue;
        }
        out.push(FavEntry {
            title,
            find: crate::win32::grepdlg::ini_get(&ini, &sec, "Find", ""),
            replace: crate::win32::grepdlg::ini_get(&ini, &sec, "Replace", ""),
            flag: crate::win32::grepdlg::ini_get(&ini, &sec, "Flag", "0")
                .trim()
                .parse()
                .unwrap_or(0),
        });
    }
    out
}

fn fav_save(path: &Path, entries: &[FavEntry]) {
    let mut ini = crate::win32::grepdlg::ini_load(path);
    for name in ini.sections_with_prefix("ReplaceFav") {
        if name != "ReplaceFav" {
            crate::win32::grepdlg::ini_remove_section(&mut ini, &name);
        }
    }
    for (i, e) in entries.iter().enumerate() {
        let sec = format!("ReplaceFav{i}");
        crate::win32::grepdlg::ini_set(&mut ini, &sec, "Title", &e.title);
        crate::win32::grepdlg::ini_set(&mut ini, &sec, "Find", &e.find);
        crate::win32::grepdlg::ini_set(&mut ini, &sec, "Replace", &e.replace);
        crate::win32::grepdlg::ini_set(&mut ini, &sec, "Flag", &e.flag.to_string());
    }
    crate::win32::grepdlg::ini_set(&mut ini, "ReplaceFav", "Count", &entries.len().to_string());
    crate::win32::grepdlg::ini_save(&ini, path);
}

fn lang_text(id: u32, fallback: &str) -> String {
    crate::win32::lang::string(id)
        .unwrap_or(fallback)
        .to_string()
}

/// 替换收藏: the star opens the saved pairs, 添加当前替换 and 删除项目.
/// Shift-clicking a pair deletes it; a plain click loads its texts and the
/// option grid it was saved with.
unsafe fn on_fav_menu(dlg: HWND) {
    let kind = state(dlg).kind;
    let frame = state(dlg).frame;
    let theme = state(dlg).theme.clone();
    let path = crate::win32::grepdlg::ini_path();
    let entries = fav_load(&path);
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    for (i, e) in entries.iter().enumerate() {
        let caption: String = e.title.chars().take(60).collect();
        let _ = append_menu(menu, FAV_ITEM_BASE + i, &caption, MF_STRING);
    }
    if !entries.is_empty() {
        let _ = append_menu(menu, 0, "", MF_SEPARATOR);
    }
    let add = menu_label(354, FAV_ADD as u32, false, "添加当前替换");
    let delete = menu_label(354, FAV_DELETE as u32, false, "删除项目");
    let _ = append_menu(menu, FAV_ADD, &add, MF_STRING);
    let _ = append_menu(menu, FAV_DELETE, &delete, MF_STRING);
    let chosen = track_at(menu, dlg, ID_FAV) as usize;
    if chosen == 0 {
        return;
    }
    if chosen == FAV_DELETE {
        let text = lang_text(15295, "请按住Shift键的同时点击希望删除的项目!");
        crate::win32::chrome::message_box(
            Some(dlg),
            &text,
            "DogEdit",
            MB_OK | MB_ICONINFORMATION,
            &theme,
        );
        return;
    }
    if chosen == FAV_ADD {
        let model = read(kind);
        if model.query.is_empty() {
            return;
        }
        let question = lang_text(15296, "请为当前查找-替换对起一个名称:");
        let Some(title) = crate::win32::prompt::prompt(dlg, "替换收藏", &question, &model.query)
        else {
            return;
        };
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let mut entries = fav_load(&path);
        entries.push(FavEntry {
            title: title.to_string(),
            find: model.query,
            replace: model.replace,
            flag: fav_flag(&model.opts),
        });
        fav_save(&path, &entries);
        return;
    }
    let idx = chosen.saturating_sub(FAV_ITEM_BASE);
    let Some(entry) = entries.get(idx).cloned() else {
        return;
    };
    if GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0 {
        let text = lang_text(15297, "该操作不可撤销,确定继续吗?");
        let answer = crate::win32::chrome::message_box(
            Some(dlg),
            &text,
            "DogEdit",
            MB_YESNO | MB_ICONWARNING,
            &theme,
        );
        if answer == IDYES.0 {
            let mut entries = entries;
            entries.remove(idx);
            fav_save(&path, &entries);
        }
        return;
    }
    let mut model = read(kind);
    model.query = entry.find;
    model.replace = entry.replace;
    fav_apply(&mut model.opts, entry.flag);
    model.opts.normalize();
    write_model(dlg, kind, &model);
    crate::win32::window::find_dlg_event(frame, kind, Event::Option);
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

unsafe fn draw_icon_button(dlg: HWND, lparam: LPARAM) -> isize {
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    let st = state(dlg);
    let id = di.CtlID as i32;
    if id != ID_MULTILINE && id != ID_DOCK && id != ID_FAV {
        return 0;
    }
    let face = if di.itemState.0 & ODS_SELECTED.0 != 0 {
        st.press_brush
    } else if ownerdraw::is_hot(di.hwndItem) {
        st.hot_brush
    } else {
        st.face_brush
    };
    FillRect(di.hDC, &di.rcItem, face);
    FrameRect(di.hDC, &di.rcItem, st.edge_brush);
    let cx = (di.rcItem.left + di.rcItem.right) / 2;
    let cy = (di.rcItem.top + di.rcItem.bottom) / 2;
    let old = SelectObject(di.hDC, HGDIOBJ(st.ink_brush.0));
    if id == ID_FAV {
        // 替换收藏: a five-point star.
        const STAR: [(i32, i32); 10] = [
            (0, -5),
            (1, -1),
            (5, -1),
            (2, 2),
            (3, 6),
            (0, 4),
            (-3, 6),
            (-2, 2),
            (-5, -1),
            (-1, -1),
        ];
        let pts: Vec<POINT> = STAR
            .iter()
            .map(|&(x, y)| POINT { x: cx + x, y: cy + y })
            .collect();
        let _ = Polygon(di.hDC, &pts);
    } else if id == ID_MULTILINE {
        let (a, b) = if st.model.opts.multiline {
            // Expanded: the triangle points up, back towards the boxes.
            (cy + 3, cy - 3)
        } else {
            // Collapsed: it points down, at the block it will reveal.
            (cy - 3, cy + 3)
        };
        let pts = [
            POINT { x: cx - 4, y: a },
            POINT { x: cx + 4, y: a },
            POINT { x: cx, y: b },
        ];
        let _ = Polygon(di.hDC, &pts);
    } else {
        // A short shaft with a head and a floor line: 停靠到底部.
        let bar = RECT { left: cx - 5, top: cy + 4, right: cx + 5, bottom: cy + 5 };
        FillRect(di.hDC, &bar, st.ink_brush);
        let shaft = RECT { left: cx - 1, top: cy - 4, right: cx + 1, bottom: cy + 2 };
        FillRect(di.hDC, &shaft, st.ink_brush);
        let pts = [
            POINT { x: cx - 4, y: cy },
            POINT { x: cx + 4, y: cy },
            POINT { x: cx, y: cy + 4 },
        ];
        let _ = Polygon(di.hDC, &pts);
    }
    SelectObject(di.hDC, old);
    1
}

// ---------------------------------------------------------------------------
// The window proc
// ---------------------------------------------------------------------------

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0 as isize);
            let st = state(hwnd);
            let kind = st.kind;
            // Before anything measures a combo: the children's 8 pt template
            // font becomes the UI font. `DS_SETFONT` itself stays, or every
            // dialog-unit rect below would be rescaled.
            crate::win32::uifont::set_children(hwnd, st.font);
            crate::win32::lang::localize_dialog(hwnd, kind.dialog_id());
            // Every combo starts with room for a dropdown list; the template's
            // 12-unit closed height is all a combo needs in a dialog unit box.
            for id in [ID_QUERY, ID_REPLACE_TEXT] {
                if let Ok(c) = GetDlgItem(Some(hwnd), id) {
                    let mut r = RECT::default();
                    if GetWindowRect(c, &mut r).is_ok() {
                        let w = r.right - r.left;
                        let _ = SetWindowPos(
                            c,
                            None,
                            0,
                            0,
                            w,
                            160,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            let model = st.model.clone();
            write_model(hwnd, kind, &model);
            set_expanded(hwnd, kind, model.opts.multiline);
            for id in [ID_MULTILINE, ID_DOCK, ID_FAV] {
                if let Ok(btn) = GetDlgItem(Some(hwnd), id) {
                    let style = GetWindowLongPtrW(btn, GWL_STYLE);
                    SetWindowLongPtrW(btn, GWL_STYLE, (style & !0xF) | 0xB);
                    let _ = InvalidateRect(Some(btn), None, true);
                }
            }
            ownerdraw::install(hwnd, &[ID_MULTILINE, ID_DOCK]);
            init_tooltips(hwnd);
            if let (0, 0) = (LAST_X.load(Ordering::Relaxed), LAST_Y.load(Ordering::Relaxed)) {
            } else if !model.opts.bottom {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    LAST_X.load(Ordering::Relaxed),
                    LAST_Y.load(Ordering::Relaxed),
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if model.opts.bottom {
                place(hwnd, st);
            }
            1
        }
        WM_ERASEBKGND => {
            let st = state(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state(hwnd);
            customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.face, st.bg)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let st = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                st.ink,
                st.field_face,
                st.field,
            )
        }
        WM_DRAWITEM => draw_icon_button(hwnd, lparam),
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            let st = state(hwnd);
            if id == IDCANCEL.0 || id == 2 {
                crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Close);
                return 1;
            }
            if id == 1 {
                // Enter reaches the default button as IDOK: 查找下一个 in 查找
                // and 替换 in 替换.
                let event = if st.kind == Kind::Find {
                    Event::Next
                } else {
                    Event::ReplaceOne
                };
                crate::win32::window::find_dlg_event(st.frame, st.kind, event);
                return 1;
            }
            match id {
                ID_QUERY => {
                    if code == 5 || code == 1 || code == 0x300 {
                        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Query);
                    }
                }
                ID_REPLACE_TEXT => {
                    if code == 5 || code == 1 || code == 0x300 {
                        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::ReplaceText);
                    }
                }
                ID_BIG_QUERY => {
                    if code == 0x300 {
                        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Query);
                    }
                }
                ID_BIG_REPLACE => {
                    if code == 0x300 {
                        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::ReplaceText);
                    }
                }
                ID_NEXT => crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Next),
                ID_PREV => crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Prev),
                ID_FIND_ALL => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::FindAll)
                }
                ID_REPLACE_ONE => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::ReplaceOne)
                }
                ID_REPLACE_ALL => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::ReplaceAll)
                }
                ID_BATCH => crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Batch),
                ID_TO_REPLACE => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::SwitchToReplace)
                }
                // 高级 rows also arrive as commands, the way their menu
                // reports a choice.
                1221 | 1224 | 1225 | 1233 | 1244 | 1245 => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Advanced(id))
                }
                ID_TO_FIND => {
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::SwitchToFind)
                }
                ID_ADVANCED => on_advanced_menu(hwnd),
                ID_TOKENS => on_token_menu(hwnd, ID_QUERY, ID_TOKENS),
                ID_TOKENS_REPLACE => on_token_menu(hwnd, ID_REPLACE_TEXT, ID_TOKENS_REPLACE),
                ID_FAV => on_fav_menu(hwnd),
                ID_MULTILINE => {
                    let kind = st.kind;
                    let on = !st.model.opts.multiline;
                    if on {
                        let combo = text_of(hwnd, ID_QUERY);
                        set_text(hwnd, ID_BIG_QUERY, &combo);
                        if kind == Kind::Replace {
                            let rep = text_of(hwnd, ID_REPLACE_TEXT);
                            set_text(hwnd, ID_BIG_REPLACE, &rep);
                        }
                        st.model.opts.incremental = false;
                    } else {
                        let big = text_of(hwnd, ID_BIG_QUERY);
                        set_text(hwnd, ID_QUERY, &big.replace(['\r', '\n'], " "));
                        if kind == Kind::Replace {
                            let big = text_of(hwnd, ID_BIG_REPLACE);
                            set_text(hwnd, ID_REPLACE_TEXT, &big.replace(['\r', '\n'], " "));
                        }
                    }
                    st.model.opts.multiline = on;
                    set_expanded(hwnd, kind, on);
                    if let Ok(c) = GetDlgItem(Some(hwnd), ID_INCR) {
                        let _ = EnableWindow(c, !on);
                    }
                    crate::win32::window::find_dlg_event(st.frame, kind, Event::Option);
                }
                ID_DOCK => {
                    st.model.opts.bottom = !st.model.opts.bottom;
                    place(hwnd, st);
                    crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Option);
                }
                _ if (ID_CASE..=ID_INCR).contains(&id)
                    || (ID_EXTENDED..=ID_CONTINUE).contains(&id)
                    || id == ID_ULTRA
                    || id == ID_FAST_REPLACE
                    || (ID_SCOPE_CURRENT..=ID_SCOPE_ALL).contains(&id) =>
                {
                    if code == BN_CLICKED {
                        // Regex and 高速模式查找 are two engines: picking one
                        // releases the other.
                        if id == ID_REGEX && checked(hwnd, ID_REGEX) {
                            let _ = checked_set(hwnd, ID_ULTRA, false);
                        }
                        if id == ID_ULTRA && checked(hwnd, ID_ULTRA) {
                            let _ = checked_set(hwnd, ID_REGEX, false);
                        }
                        crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Option);
                    }
                }
                _ => {}
            }
            1
        }
        WM_CLOSE => {
            let st = state(hwnd);
            crate::win32::window::find_dlg_event(st.frame, st.kind, Event::Close);
            1
        }
        WM_MOVE => {
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok() {
                let st = state(hwnd);
                if !st.model.opts.bottom {
                    LAST_X.store(r.left, Ordering::Relaxed);
                    LAST_Y.store(r.top, Ordering::Relaxed);
                }
            }
            0
        }
        WM_DESTROY => {
            let st = state(hwnd);
            st.kind.slot().store(0, Ordering::Release);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let ptr = Box::from_raw(st as *mut Inner);
            let _ = DeleteObject(ptr.bg.into());
            let _ = DeleteObject(ptr.field.into());
            let _ = DeleteObject(ptr.face_brush.into());
            let _ = DeleteObject(ptr.hot_brush.into());
            let _ = DeleteObject(ptr.press_brush.into());
            let _ = DeleteObject(ptr.edge_brush.into());
            let _ = DeleteObject(ptr.ink_brush.into());
            let _ = DeleteObject(ptr.font.into());
            0
        }
        _ => 0,
    }
}

unsafe fn init_tooltips(hwnd: HWND) {
    let Some(host) = crate::win32::tiphost::TipHost::create(hwnd) else {
        return;
    };
    for (id, text) in [
        (ID_MULTILINE, 16015u32),
        (ID_DOCK, 16014),
        (ID_FAV, 16012),
        (ID_ULTRA, 16013),
        (ID_FAST_REPLACE, 16011),
    ] {
        let Some(tip) = crate::win32::lang::string(text) else {
            continue;
        };
        if let Ok(c) = GetDlgItem(Some(hwnd), id) {
            host.add_window_tool(c, tip);
        }
    }
    state(hwnd).tip_host = Some(host);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_template_keeps_the_recorded_shape() {
        let b = template(Kind::Find);
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]);
        assert_eq!(&b[12..16], &DLG_STYLE.to_le_bytes());
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 23);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), 324);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), 99);
    }

    #[test]
    fn replace_template_keeps_the_recorded_shape() {
        let b = template(Kind::Replace);
        assert_eq!(u16::from_le_bytes([b[16], b[17]]), 31);
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), 319);
        assert_eq!(u16::from_le_bytes([b[24], b[25]]), 124);
    }

    #[test]
    fn regex_owns_word_and_escape_modes() {
        let mut o = Options {
            regex: true,
            whole_word: true,
            extended: true,
            ..Options::default()
        };
        o.normalize();
        assert!(!o.whole_word);
        assert!(!o.extended);
    }

    #[test]
    fn wrap_and_continue_are_exclusive() {
        let mut o = Options {
            wrap: true,
            continue_next: true,
            ..Options::default()
        };
        o.normalize();
        assert!(!o.continue_next);
    }

    #[test]
    fn multiline_drops_incremental_and_ultra() {
        let mut o = Options {
            multiline: true,
            incremental: true,
            ultra: true,
            ..Options::default()
        };
        o.normalize();
        assert!(!o.incremental);
        assert!(!o.ultra);
    }

    #[test]
    fn ultra_drops_regex() {
        let mut o = Options {
            ultra: true,
            regex: true,
            ..Options::default()
        };
        o.normalize();
        assert!(!o.regex);
    }

    #[test]
    fn favorite_flags_round_trip() {
        let o = Options {
            case_sensitive: true,
            whole_word: true,
            close_if_found: true,
            incremental: true,
            highlight: true,
            ..Options::default()
        };
        let mut back = Options::default();
        fav_apply(&mut back, fav_flag(&o));
        assert_eq!(back, o);
        let re = Options {
            regex: true,
            ..Options::default()
        };
        let mut back = Options::default();
        fav_apply(&mut back, fav_flag(&re));
        assert_eq!(back, re);
        assert_eq!(fav_flag(&re), FAV_REGEX);
    }

    #[test]
    fn favorites_round_trip_through_the_ini() {
        let path = std::env::temp_dir().join(format!(
            "dogedit-favorites-{}-{:?}.ini",
            std::process::id(),
            std::thread::current().id()
        ));
        let all = vec![
            FavEntry {
                title: "first".into(),
                find: "a".into(),
                replace: "b".into(),
                flag: FAV_CASE | FAV_WORD,
            },
            FavEntry {
                title: "second".into(),
                find: "中文".into(),
                replace: "替换".into(),
                flag: FAV_REGEX,
            },
        ];
        fav_save(&path, &all);
        assert_eq!(fav_load(&path), all);
        fav_save(&path, &all[..1]);
        assert_eq!(fav_load(&path), all[..1].to_vec());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn token_rows_insert_the_part_before_the_tab() {
        assert_eq!(token_of("\\n\t换行符"), "\\n");
        assert_eq!(token_of("\\w\t单词"), "\\w");
    }
}
