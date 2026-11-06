//! Goto Anything (`cm_goto_anything`, 1760, Ctrl+P) — a modeless dialog with
//! a query edit, an owner-drawn list, a status line, a reindex button and a
//! close button.
//!
//! The file source is the in-memory index in [`dg_core::fileindex`] (or
//! Everything's IPC when available); `@` switches to workspace symbols.
//! Entries are scored, which also gives match positions for highlighting.

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, SIZE, WPARAM};

use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreatePen, CreateSolidBrush, DT_NOPREFIX, DT_SINGLELINE, DrawFocusRect,
    DrawTextW, FillRect, GetDC, GetTextExtentPoint32W, GetWindowDC, HBRUSH, HDC, HFONT, HGDIOBJ,
    HPEN, InvalidateRect, PS_SOLID, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
    REDRAW_WINDOW_FLAGS, RedrawWindow, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT, TextOutW,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETSEL, ODS_FOCUS, ODS_SELECTED,
};
use windows::core::PCWSTR;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_DOWN, VK_NEXT, VK_PRIOR, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateDialogIndirectParamW, DLGTEMPLATE, EN_CHANGE, GA_ROOT, GWL_EXSTYLE,
    GWL_STYLE, GWLP_USERDATA, GWLP_WNDPROC, GetAncestor, GetClientRect, GetDlgItem,
    GetParent, GetWindowLongPtrW, GetWindowRect, IsDialogMessageW, IsWindowVisible, MINMAXINFO, MSG,
    MoveWindow, PostMessageW, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SetWindowTextW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_EXITSIZEMOVE,
    WM_GETFONT, WM_GETMINMAXINFO, WM_INITDIALOG, WM_KEYDOWN, WM_NCDESTROY,
    WM_NCPAINT, WM_PAINT, WM_PRINTCLIENT, WM_SETTEXT, WM_SIZE, WNDPROC, WS_BORDER, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_VSCROLL,
};
use crate::win32::ctags;
use crate::win32::customdraw;
use crate::win32::ownerdraw;
use crate::win32::everything;
use crate::win32::optionsdlg::{
    self, ATOM_BUTTON, ATOM_EDIT, ATOM_LISTBOX, ATOM_STATIC, Class, Item,
};
use crate::win32::view::instance;
use crate::win32::window::to_colorref;
use dg_core::fileindex::{self, DirStamp, FileEntry, WalkOptions};
use dg_render::theme::{Theme, luma_blend};

/// Control ids, as the template numbers them. 2 is the visible 关闭 button and
/// therefore also `IDCANCEL`; 1 is a hidden `BS_DEFPUSHBUTTON` whose only job
/// is to make Enter arrive as `WM_COMMAND IDOK`.
const ID_QUERY: i32 = 1_200;
const ID_LIST: i32 = 1_201;
const ID_STATUS: i32 = 1_202;
const ID_REINDEX: i32 = 1_203;
const ID_CLOSE: i32 = 2;
const ID_OK: i32 = 1;

/// The message the dialog posts to the frame — its only way to act.
pub(super) const WM_GOTOANY: u32 = WM_APP + 0x4B;
/// Enter or a double-click: open the selected row.
pub(super) const GA_ACCEPT: usize = 1;
/// Escape or 关闭: hide and give the editor its focus back.
pub(super) const GA_DISMISS: usize = 2;
/// The walker finished; `lParam` is the boxed `(generation, entries)`.
pub(super) const GA_INDEX: usize = 3;
/// The opt-in ctags scan finished; `lParam` is the boxed
/// `(generation, tags)`.
pub(super) const GA_TAGS: usize = 4;
/// A live Everything query finished; `lParam` is the boxed
/// `(generation, pattern, answer)`.
pub(super) const GA_QUERY: usize = 5;

/// What a live query answers with: a page of files plus Everything's own total,
/// or the reason it could not. Named so the frame can rebuild the box without
/// spelling the type out.
pub(super) type QueryAnswer = Result<everything::Answer, everything::Why>;

/// `LB_SETCOUNT` (0x01A7) — the `LBS_NODATA` half of the list contract. The
/// message numbers are spelled out because the `windows` metadata omits this
/// one and this implementation only needs these four.
const LB_SETCOUNT: u32 = 0x01A7;
const LB_SETITEMHEIGHT: u32 = 0x01A0;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;

/// `LBN_DBLCLK` / `LBN_SELCHANGE` of `WM_COMMAND`'s notification word.
const LBN_SELCHANGE: u16 = 1;
const LBN_DBLCLK: u16 = 2;

const LIMIT: usize = 100;

/// The ctags scan is the expensive one (seconds on a C/C++ tree), and symbols
/// only change when a file's contents do — so a scan is reused for a minute.
const TAG_TTL: Duration = Duration::from_secs(60);

/// How many files a live Everything query asks for: four pages' worth, so the
/// local ranking has something to choose from and a row that matched only
/// because of the folder order does not push out a better one. Everything
/// serialises exactly this many, which is what keeps a 116 000-file folder
/// off the wire — the page, not the tree, is what costs.
const CANDIDATES: usize = LIMIT * 4;

/// Whether a scan started at `stamp` is old enough to redo. `None` — never
/// scanned — is always stale. Pure, so the policy is testable.
fn stale(stamp: Option<Instant>, ttl: Duration) -> bool {
    match stamp {
        None => true,
        Some(t) => t.elapsed() >= ttl,
    }
}

/// The 跳转 box's template geometry, in dialog units.
const DLG_W: u16 = 285;
const DLG_H: u16 = 208;
const TITLE: &str = "跳转";

/// How many pixels one dialog unit is worth in this dialog — on both axes. This
/// implementation cannot either. Its dialog font is the 15 px shell font (Microsoft
/// YaHei UI 9 pt at 125 %), and a manager-computed base unit for that font is 9 x 20:
/// the dialog would come out 641 x 520. A fixed 8 x 16 base, the classic 96-DPI dialog
/// base, applied whatever the DPI. Its text is still DPI-scaled, so the layout is the
/// one part of this dialog that does not scale; reproducing that is the point of
/// placing the controls by hand.
const DLU_PX: i32 = 2;

/// A template coordinate in pixels.
const fn dlu(v: i16) -> i32 {
    v as i32 * DLU_PX
}

/// How far below the top of the frame's client area the dialog's top edge sits,
/// in 96-DPI pixels.
const TOP_GAP: i32 = 24;

/// How much less than the client delta the relayout moves its children, in
/// pixels on both axes. The plain delta lands 2 px outside that, on both axes,
/// every resize — see [`relayout`].
const RELAYOUT_BIAS: i32 = 2;

/// [`TOP_GAP`] as arithmetic: `client` is the frame's client area as
/// `(origin_x, origin_y, width)`, and the answer is where the dialog's top-left
/// corner goes. Pure so the two measurements can be a test rather than a
/// comment; `place_in_frame` is the only caller.
fn placement(client: (i32, i32, i32), dlg_w: i32, gap: i32) -> (i32, i32) {
    (
        client.0 + ((client.2 - dlg_w) / 2).max(0),
        client.1 + gap,
    )
}

/// The 跳转 box's style: `WS_POPUP|WS_THICKFRAME` plus `DS_CENTER|DS_SETFONT|
/// DS_FIXEDSYS|DS_3DLOOK`. Its query edit sits 4 px from the window's client edge
/// and the 关闭 button is the close affordance (a bare `WS_THICKFRAME` popup is
/// resizable but not draggable, exactly as there). DS_SETFONT is kept, so the
/// dialog manager owns the font and hands it to every template child.
const STYLE: u32 = 0x8004_084C;

/// The hint caption `输入文本在快捷目录或者工程中查找文件\n@:查找符号` — one element per line.
/// It is what the list shows when it has no rows: two dim lines in the
/// list's top-left corner, measured 13 px below the list's top and 23 px
/// apart.
const HINT: [&str; 2] = ["输入文本在快捷目录或者工程中查找文件", "@:查找符号"];

/// The five visible children plus the hidden default button, item for item
/// from the 跳转 box. The recorded template keeps the bit even though the
/// measured client did not report it.
const ITEMS: &[Item] = &[
    Item {
        id: ID_QUERY,
        class: Class::Atom(ATOM_EDIT),
        style: 0x5081_0080, // WS_CHILD|WS_VISIBLE|WS_BORDER|WS_TABSTOP|ES_AUTOHSCROLL
        x: 2,
        y: 2,
        w: 280,
        h: 15,
        label: "",
    },
    Item {
        id: ID_LIST,
        class: Class::Atom(ATOM_LISTBOX),
        style: 0x50A1_2111, // + WS_VSCROLL, see above
        x: 2,
        y: 19,
        w: 280,
        h: 170,
        label: "",
    },
    Item {
        id: ID_STATUS,
        class: Class::Atom(ATOM_STATIC),
        style: 0x5002_0000, // WS_CHILD|WS_VISIBLE|SS_LEFT
        x: 2,
        y: 191,
        w: 185,
        h: 11,
        label: "",
    },
    Item {
        id: ID_REINDEX,
        class: Class::Atom(ATOM_BUTTON),
        style: 0x5000_000B,
        x: 193,
        y: 191,
        w: 50,
        h: 14,
        label: "重新索引",
    },
    Item {
        id: ID_CLOSE,
        class: Class::Atom(ATOM_BUTTON),
        style: 0x5000_000B,
        x: 244,
        y: 191,
        w: 38,
        h: 14,
        label: "关闭",
    },
    Item {
        id: ID_OK,
        class: Class::Atom(ATOM_BUTTON),
        style: 0x4001_0001, // WS_CHILD|BS_DEFPUSHBUTTON, never shown
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        label: "",
    },
];

/// What a row does when it is accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum GaKind {
    /// Open (or switch to) a file.
    File,
    /// Jump to a line of the active document.
    Line,
    /// Jump to a symbol of the active document.
    Symbol,
}

/// One rendered row. `positions` are the fuzzy match's byte offsets into
/// `title` — the renderer maps them onto the UTF-16 drawing buffer.
#[derive(Clone, Debug)]
pub(super) struct GaRow {
    pub(super) title: String,
    pub(super) positions: Vec<usize>,
    pub(super) secondary: String,
    pub(super) kind: GaKind,
    /// File to open for [`GaKind::File`]; empty for the other two.
    pub(super) path: String,
    /// 0-based target line for [`GaKind::Line`] and [`GaKind::Symbol`].
    pub(super) line: usize,
}

/// A row's identity, used to put the cursor back where it was after a
/// background rebuild. The title alone is not enough — `main.rs` exists in many
/// folders — so the path and the line disambiguate, and the kind keeps a
/// 「跳转到行 N」 row from matching a file of the same text.
type RowKey = (GaKind, String, usize, String);

fn row_key(r: &GaRow) -> RowKey {
    (r.kind, r.path.clone(), r.line, r.title.clone())
}

/// Everything the dialog needs, snapshotted by the frame when it is shown.
pub(super) struct Request<'a> {
    pub(super) frame: HWND,
    /// The walker's root: the active document's folder (or the working
    /// directory), the same choice `show_dock_tree` makes.
    pub(super) root: String,
    /// Paths of the open documents, in tab order — the empty query's rows.
    pub(super) open_docs: Vec<String>,
    /// The active document's symbols, 0-based lines (LSP today; the syntax
    /// tree later). Empty when no server is attached, which is what makes `@`
    /// show nothing rather than something stale.
    pub(super) symbols: Vec<(String, usize)>,
    /// The active document's line count, for the digits query.
    pub(super) line_count: usize,
    pub(super) theme: &'a Theme,
    /// The opt-in ctags index, already resolved from `setting.ini` — `None`
    /// when the user has not enabled it, which is the default.
    pub(super) ctags: Option<ctags::Config>,
    /// The active document's path: its own tags are already in `symbols`.
    pub(super) active_path: Option<String>,
    /// Text to put in the query box instead of keeping whatever the last show
    /// left there. `@` opens the dialog on its symbol rows; `None` — the plain
    /// 跳转到文件 command — leaves the remembered query in place.
    pub(super) seed: Option<&'a str>,
}

/// The dialog's one instance
static DIALOG: AtomicIsize = AtomicIsize::new(0);

fn dialog_hwnd() -> Option<HWND> {
    let v = DIALOG.load(Ordering::Acquire);
    (v != 0).then_some(HWND(v as *mut core::ffi::c_void))
}

/// Whether the frame's loop should feed `msg` to the dialog's dialog manager.
/// `GA_ROOT` is what makes a keystroke in the query edit belong to the dialog.
pub(super) fn translate(msg: &MSG) -> bool {
    let Some(dlg) = dialog_hwnd() else {
        return false;
    };
    let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if root != dlg {
        return false;
    }
    unsafe { IsDialogMessageW(dlg, msg).as_bool() }
}

/// Where the file rows come from.
///
/// `Live` is the normal case when Everything is installed: the box's text goes
/// to Everything on every keystroke and only the page that will be shown comes
/// back, so there is no snapshot to build, revalidate or repair. The walker arm
/// carries *why* Everything was not used. "Everything is not running" needs no
/// explanation, but "Everything is running and this implementation still walked the tree"
/// is exactly the question a user asks, so the status line can answer it
/// (`everything::Why::note`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Source {
    Walk(everything::Why),
    Everything,
}

/// The status line's tail: where the rows came from, and — when Everything was
/// up but declined — why the walker ran instead. Where it came from is worth one
/// word: Everything answers live, the walker's snapshot is validated on open,
/// and that is exactly the difference the user can feel. But when Everything
/// *is* running and this implementation still walks the tree, the word alone reads as "the
/// implementation ignored Everything", so the reason goes in with it.
fn source_note(source: Source) -> String {
    match source {
        Source::Everything => crate::win32::lang::string(40035)
            .unwrap_or("（Everything 实时）")
            .to_string(),
        Source::Walk(why) => match why.note() {
            Some(note) => crate::win32::lang::format(40041, &[note])
                .unwrap_or_else(|| format!("（本地扫描·{note}）")),
            None => crate::win32::lang::string(40042)
                .unwrap_or("（本地扫描）")
                .to_string(),
        },
    }
}

/// Whether the box's text is something Everything can answer. `@name` is the
/// workspace symbol query — ctags, not files — and an empty box is the
/// open-document list, which needs no round trip at all.
fn wants_live_query(pattern: &str) -> bool {
    let p = pattern.trim();
    !p.is_empty() && !p.starts_with('@')
}

/// Whether the status line should admit the index was truncated. Only the walker
/// has a budget — it is there so a `C:\Windows` root cannot crawl — and a live
/// Everything query is a *page*, not a snapshot, so there is nothing to admit:
/// the count in the status is Everything's own total, not the size of a list.
fn capped(source: Source, files: usize) -> bool {
    matches!(source, Source::Walk(_)) && files >= WalkOptions::default().max_files
}

/// What the background thread should do. `Validate` is the cheap reopen path:
/// it only re-stats directories, so "nothing changed" costs 5–107 ms instead of
/// a full walk (see `dg_core::fileindex`).
enum Job {
    Full,
    Validate(Vec<DirStamp>),
}

/// What the background thread found. The dialog owns the repair, so the thread
/// never has to carry a copy of the entry list back across the boundary.
pub(super) enum Update {
    /// Nothing on disk moved; the rows on screen stand as they are.
    Unchanged,
    /// These directories' listings changed — indices into `Ga::dirs`.
    ChangedDirs(Vec<usize>),
    /// A whole new snapshot from the walker.
    Fresh {
        entries: Vec<FileEntry>,
        dirs: Vec<DirStamp>,
    },
}

/// More changed directories than this and the targeted repair stops being
/// cheaper than a walk; the thread falls back to `Job::Full` on its own.
const REPAIR_LIMIT: usize = 64;

/// Directory mtimes can lie (NTFS defers them while a handle is open; FAT and
/// some network shares do not maintain them at all), so a snapshot that has not
/// been rebuilt by a real walk for this long is rebuilt regardless of what the
/// stamps say.
const DRIFT: Duration = Duration::from_secs(300);

/// The worker for the *walker* path only. Everything never comes through here
/// any more: it answers the box directly ([`Ga::spawn_query`]), so there is no
/// snapshot of its tree to build, revalidate or repair.
fn run_index(root: &str, job: Job) -> Update {
    let opts = WalkOptions::default();
    let full = || {
        let r = fileindex::walk_tree(root, &opts);
        Update::Fresh {
            entries: r.entries,
            dirs: r.dirs,
        }
    };
    match job {
        Job::Full => full(),
        Job::Validate(dirs) => {
            let changed = fileindex::revalidate(&dirs);
            if changed.is_empty() {
                Update::Unchanged
            } else if changed.len() > REPAIR_LIMIT {
                // A big refactor: the targeted repair is no longer cheaper than
                // re-reading the tree.
                full()
            } else {
                Update::ChangedDirs(changed)
            }
        }
    }
}

/// Whether `path` sits directly in `dir` (one level, not the whole subtree).
fn is_direct_child(path: &str, dir: &str) -> bool {
    let Some(rest) = path.strip_prefix(dir) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('\\').or_else(|| rest.strip_prefix('/')) else {
        return false;
    };
    !rest.is_empty() && !rest.contains('\\') && !rest.contains('/')
}

/// The dialog's state, reached through `GWLP_USERDATA` like every other window
/// here. It owns the GDI objects, the index and the row model; nothing in it
/// is a copy of editor state — the frame hands over snapshots on every show.
struct Ga {
    me: HWND,
    frame: HWND,
    theme: Theme,
    /// The template's dialog font, fetched from a child (`WM_GETFONT`): the
    /// dialog manager created it from DS_SETFONT and handed it to every item.
    font: HFONT,
    /// The dialog surface and the sunken field the edit and the list sit on
    /// (`#2D2D2D`) — two different greys, which is why they are two brushes.
    bg: HBRUSH,
    field_bg: HBRUSH,
    sel_bg: HBRUSH,
    /// The owner-drawn buttons' faces (resting, hover, pressed) and outline
    /// pens (resting, hover), the list's own frame, and the query field's
    /// two-pixel ring.
    btn_face: HBRUSH,
    btn_hot: HBRUSH,
    btn_press: HBRUSH,
    btn_pen: HPEN,
    btn_pen_hot: HPEN,
    list_border: HBRUSH,
    field_ring: HBRUSH,
    field_edge: HBRUSH,
    /// Initial client rects by control id, for the `WM_SIZE` deltas.
    rects: Vec<(i32, RECT)>,
    base_client: (i32, i32),
    base_outer: (i32, i32),
    row_h: i32,
    text_h: i32,
    /// The edit's and the list's original window procs, installed by
    /// `WM_INITDIALOG`.
    edit_orig: isize,
    list_orig: isize,
    root: String,
    /// The rows' candidates. Live: the page Everything just answered for the
    /// current text (at most [`CANDIDATES`]); walker: the whole snapshot.
    entries: Vec<FileEntry>,
    /// One stamp per directory the walker listed — the cheap revalidation key.
    /// Empty on the live path, which keeps no snapshot to revalidate.
    dirs: Vec<DirStamp>,
    source: Source,
    /// Everything answers the box directly: no walk, no snapshot, one query per
    /// keystroke. Decided once per open from [`everything::available`], and
    /// dropped again if Everything stops answering mid-session.
    live: bool,
    /// How many files Everything says matched — the header's `totitems`, which
    /// counts the whole set even though only [`CANDIDATES`] came back. `None`
    /// until the first answer lands.
    live_total: Option<u32>,
    open_docs: Vec<String>,
    symbols: Vec<(String, usize)>,
    line_count: usize,
    rows: Vec<GaRow>,
    /// The opt-in workspace tag index. Always empty
    /// unless the user enabled ctags in `setting.ini`.
    tags: Vec<ctags::Tag>,
    tag_cfg: Option<ctags::Config>,
    active_path: Option<String>,
    walking: bool,
    tagging: bool,
    /// When the tag scan last started, for its TTL.
    tag_stamp: Option<Instant>,
    /// When the last **full** walk landed, for the drift backstop.
    full_stamp: Option<Instant>,
    /// A walk/scan that finishes after a newer one started is dropped by this.
    walk_gen: u64,
    tag_gen: u64,
    /// Same rule for live queries: a keystroke makes every answer still in
    /// flight obsolete.
    query_gen: u64,
}

unsafe fn state(hwnd: HWND) -> &'static mut Ga {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ga)
}

/// Show the dialog, creating it the first time
/// lifecycle. Every show re-syncs the snapshots the frame owns; a different
/// root restarts the walk.
pub(super) fn show(req: Request<'_>) {
    let seed = req.seed.map(str::to_string);
    unsafe {
        if let Some(dlg) = dialog_hwnd() {
            let ga = state(dlg);
            ga.sync(&req);
            // The frame may have moved or been resized since the last show.
            ga.place_in_frame();
            let _ = ShowWindow(dlg, SW_SHOW);
            match &seed {
                Some(text) => set_query(dlg, text),
                // Without a seed the query survives between shows; select it all
                // so the next keystroke replaces it.
                None => {
                    if let Ok(edit) = GetDlgItem(Some(dlg), ID_QUERY) {
                        let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                        let _ = SetFocus(Some(edit));
                    }
                }
            }
            return;
        }
        create(req);
        if let (Some(dlg), Some(text)) = (dialog_hwnd(), &seed) {
            set_query(dlg, text);
        }
    }
}

/// Writes the query box and parks the caret after the text. The edit's own
/// `EN_CHANGE` notification rebuilds the rows, so a seeded query goes through
/// the same path a typed one does.
unsafe fn set_query(dlg: HWND, text: &str) {
    let Ok(edit) = GetDlgItem(Some(dlg), ID_QUERY) else {
        return;
    };
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let end = wide.len() - 1;
    let _ = SetWindowTextW(edit, PCWSTR(wide.as_ptr()));
    let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(end)), Some(LPARAM(end as isize)));
    let _ = SetFocus(Some(edit));
}

/// Hide without destroying: the dialog keeps its index and its text, and the
/// frame gives the editor its focus back.
pub(super) fn hide() {
    if let Some(dlg) = dialog_hwnd() {
        unsafe {
            let _ = ShowWindow(dlg, SW_HIDE);
        }
    }
}

/// Keep a visible dialog inside the frame when the frame moves or is resized.
/// The dialog sits centred in the frame and 30 px below its top, so the two
/// coordinates follow every frame change; a popup left where it was would end
/// up hanging outside the frame it belongs to.
pub(super) fn reposition() {
    if let Some(dlg) = dialog_hwnd() {
        unsafe {
            if IsWindowVisible(dlg).as_bool() {
                state(dlg).place_in_frame();
            }
        }
    }
}

pub(super) fn take_selection() -> Option<GaRow> {
    let dlg = dialog_hwnd()?;
    unsafe {
        let ga = state(dlg);
        let list = GetDlgItem(Some(dlg), ID_LIST).ok()?;
        let sel = SendMessageW(list, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 as i32;
        let idx = if sel < 0 {
            if ga.rows.is_empty() {
                return None;
            }
            0
        } else {
            sel as usize
        };
        ga.rows.get(idx).cloned()
    }
}

/// The walker's result arrived (the frame took it out of the `lParam`). A
/// result from a superseded walk, or one that lands after the dialog is gone,
/// is dropped here — the box was already ours, so nothing leaks either way.
pub(super) fn accept_index(payload: *mut (u64, Update)) {
    let payload = unsafe { Box::from_raw(payload) };
    let Some(dlg) = dialog_hwnd() else {
        return;
    };
    let started = Instant::now();
    unsafe {
        let ga = state(dlg);
        if payload.0 != ga.walk_gen {
            return;
        }
        ga.walking = false;
        match payload.1 {
            Update::Unchanged => {}
            Update::ChangedDirs(changed) => ga.repair(&changed),
            Update::Fresh { entries, dirs } => {
                ga.entries = entries;
                ga.dirs = dirs;
                ga.full_stamp = Some(Instant::now());
                ga.merge_open_docs();
            }
        }
        // Keep the row the user is on: a background landing must never yank the
        // selection back to the top while they are typing or arrowing.
        ga.refilter_keep(true);
        ga.refresh_status();
        if tracing() {
            eprintln!(
                "ga:index ui files={} rows={} apply={}us",
                ga.entries.len(),
                ga.rows.len(),
                started.elapsed().as_micros()
            );
        }
    }
}

/// A live Everything query finished. `lParam` is the boxed
/// `(generation, pattern, answer)`.
///
/// Two things can be stale here and both are dropped: the dialog (already
/// gone, in which case the box was taken by [`hide`]) and the answer (a newer
/// keystroke spawned a newer query, so this one is about text the user has
/// already moved on from).
pub(super) fn accept_query(payload: *mut (u64, String, QueryAnswer)) {
    let payload = unsafe { Box::from_raw(payload) };
    let Some(dlg) = dialog_hwnd() else {
        return;
    };
    let started = Instant::now();
    unsafe {
        let ga = state(dlg);
        if payload.0 != ga.query_gen {
            return;
        }
        match payload.2 {
            Ok(answer) => {
                // The candidates replace the last page; `rebuild_rows` still
                // ranks and highlights them, so the ordering stays ours and a
                // row's identity does not depend on Everything's sort.
                ga.entries = answer.entries;
                ga.live_total = Some(answer.total);
                ga.live = true;
                ga.source = Source::Everything;
            }
            Err(why) => {
                // Everything answered the open and then stopped: fall back to
                // the walker for this root, and say which failure it was.
                ga.live = false;
                ga.live_total = None;
                ga.source = Source::Walk(why);
                ga.entries.clear();
                ga.dirs.clear();
                ga.walking = false;
                ga.walk_gen += 1;
                ga.start_walk(true);
            }
        }
        // A background landing keeps the cursor where the user put it.
        ga.rebuild_rows(true);
        ga.refresh_status();
        if tracing() {
            eprintln!(
                "ga:query ui pattern={:?} files={} rows={} apply={}us",
                payload.1,
                ga.entries.len(),
                ga.rows.len(),
                started.elapsed().as_micros()
            );
        }
    }
}

/// The tag scan's result arrived. Same rules: a superseded generation is
/// dropped, and a dialog that is gone has already taken the box.
pub(super) fn accept_tags(payload: *mut (u64, Vec<ctags::Tag>)) {
    let payload = unsafe { Box::from_raw(payload) };
    let Some(dlg) = dialog_hwnd() else {
        return;
    };
    unsafe {
        let ga = state(dlg);
        if payload.0 != ga.tag_gen {
            return;
        }
        ga.tagging = false;
        ga.tags = payload.1;
        // A background landing, so the cursor stays where the user put it —
        // the same rule as the file index. Getting this wrong is invisible in
        // a screenshot and very visible in use: the tag scan finishes a moment
        // after the file scan and would yank the selection to the top.
        ga.refilter_keep(true);
        ga.refresh_status();
    }
}

impl Ga {
    /// Put the dialog and its children where the template's dialog units
    /// say, at [`DLU_PX`] pixels per unit. Runs once, after creation and
    /// before the dialog is shown, replacing the manager's font-derived
    /// placement.
    unsafe fn apply_template_geometry(&mut self) {
        // The minimum size from `WM_GETMINMAXINFO` is the manager's own outer
        // size at this point; leaving it set would clamp the resize below.
        self.base_outer = (0, 0);
        // The box's extended style is `WS_EX_DLGMODALFRAME|WS_EX_WINDOWEDGE|
        // WS_EX_CONTROLPARENT` (measured `0x00010101`). The manager supplies the
        // last two for a thick-framed popup.
        let ex = GetWindowLongPtrW(self.me, GWL_EXSTYLE) | WS_EX_DLGMODALFRAME.0 as isize;
        SetWindowLongPtrW(self.me, GWL_EXSTYLE, ex);
        // the frame goes on top of that.
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: dlu(DLG_W as i16) - DLU_PX,
            bottom: dlu(DLG_H as i16) - DLU_PX,
        };
        let style = WINDOW_STYLE(GetWindowLongPtrW(self.me, GWL_STYLE) as u32);
        let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(self.me, GWL_EXSTYLE) as u32);
        let dpi = GetDpiForWindow(self.me);
        if AdjustWindowRectExForDpi(&mut rc, style, false, ex, dpi).is_ok() {
            let _ = SetWindowPos(
                self.me,
                None,
                0,
                0,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        for it in ITEMS {
            if it.w == 0 || it.h == 0 {
                continue; // the hidden default button
            }
            if let Ok(ctl) = GetDlgItem(Some(self.me), it.id) {
                let _ = SetWindowPos(
                    ctl,
                    None,
                    dlu(it.x),
                    dlu(it.y),
                    dlu(it.w),
                    dlu(it.h),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Park the dialog inside the frame: horizontally centred on the frame's
    /// client area, [`TOP_GAP`] below its top edge
    /// placement, recomputed on every show so the dialog never ends up outside
    /// a frame that has moved or been resized since the last time.
    unsafe fn place_in_frame(&self) {
        let mut client = RECT::default();
        let mut outer = RECT::default();
        if GetClientRect(self.frame, &mut client).is_err() || GetWindowRect(self.me, &mut outer).is_err()
        {
            return;
        }
        let mut origin = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(self.frame, &mut origin);
        let w = outer.right - outer.left;
        let dpi = GetDpiForWindow(self.frame) as i32;
        // A frame narrower than the dialog would put the left edge outside the
        // client area; the top-left corner is the floor.
        let gap = (TOP_GAP * dpi + 48) / 96;
        let (x, y) = placement((origin.x, origin.y, client.right), w, gap);
        let _ = SetWindowPos(
            self.me,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }

    /// The relayout baseline: client size, outer size and every child rect.
    unsafe fn capture_baseline(&mut self) {
        self.rects.clear();
        let mut cr = RECT::default();
        if GetClientRect(self.me, &mut cr).is_ok() {
            self.base_client = (cr.right, cr.bottom);
        }
        let mut wr = RECT::default();
        if GetWindowRect(self.me, &mut wr).is_ok() {
            self.base_outer = (wr.right - wr.left, wr.bottom - wr.top);
        }
        for id in [ID_QUERY, ID_LIST, ID_STATUS, ID_REINDEX, ID_CLOSE] {
            if let Some(entry) = child_rect(self.me, id) {
                self.rects.push(entry);
            }
        }
    }

    /// Push the frame's fresh snapshots in. Only the parts the frame owns are
    /// replaced; the rows are rebuilt from them, never from stale copies.
    ///
    /// The walker's index is a snapshot, so it goes stale as soon as anything
    /// writes to the tree behind the editor's back (`git checkout`, a build,
    /// another tool). Reopening therefore **revalidates** rather than re-walks:
    /// the worker re-stats the recorded directories (5–107 ms on trees that
    /// take 317–431 ms to walk) and only touches the ones whose mtime moved.
    ///
    /// With Everything running there is no snapshot at all: one query per open
    /// (for the count, and for whatever text the box still holds) and one per
    /// keystroke after that.
    unsafe fn sync(&mut self, req: &Request<'_>) {
        let root_changed = self.root != req.root;
        self.open_docs = req.open_docs.clone();
        self.symbols = req.symbols.clone();
        self.line_count = req.line_count;
        self.theme = req.theme.clone();
        self.tag_cfg = req.ctags.clone();
        self.active_path = req.active_path.clone();
        self.live = everything::available();
        if self.live {
            if root_changed {
                // A different document means a different tree, and the page in
                // hand belongs to the old one.
                self.root = req.root.clone();
                self.entries.clear();
                self.dirs.clear();
                self.tags.clear();
                self.walking = false;
                self.tagging = false;
                self.walk_gen += 1;
                self.tag_gen += 1;
                self.live_total = None;
                self.start_tags(true);
            } else {
                self.start_tags(false);
            }
            self.source = Source::Everything;
            // The query survives between shows, so ask for what is in the box:
            // empty text is the folder probe (its header carries the file
            // count), anything else is the same query a keystroke would make.
            // `rebuild_rows` rather than `refilter` — the query is already in
            // flight and a second one would be the same text twice.
            let pattern = dlg_text(self.me, ID_QUERY);
            self.spawn_query(&pattern);
            self.rebuild_rows(false);
            self.refresh_status();
            return;
        }
        if root_changed {
            // A different document means a different tree. Drop the old rows
            // and walk again; the old result is invalidated by the generation.
            self.root = req.root.clone();
            self.entries.clear();
            self.dirs.clear();
            self.tags.clear();
            self.walking = false;
            self.tagging = false;
            self.walk_gen += 1;
            self.tag_gen += 1;
            self.start_walk(true);
            self.start_tags(true);
        } else {
            self.merge_open_docs();
            // Revalidate: the previous snapshot stays on screen (and stays
            // clickable) until the answer lands, so nothing flashes.
            self.start_walk(false);
            self.start_tags(false);
        }
        self.refilter();
    }

    /// Files that are open but sit outside the walked tree still belong in the
    /// index: they are the most likely target of all.
    fn merge_open_docs(&mut self) {
        use std::collections::HashSet;
        let have: HashSet<&str> = self.entries.iter().map(|e| e.path.as_str()).collect();
        let mut add: Vec<FileEntry> = Vec::new();
        for path in &self.open_docs {
            if !have.contains(path.as_str()) && !add.iter().any(|e| e.path == *path) {
                add.push(FileEntry {
                    name: file_name_of(path),
                    path: path.clone(),
                });
            }
        }
        self.entries.extend(add);
    }

    /// A keystroke: rebuild the rows for the new text. On the live path this
    /// does not build anything locally — the text goes to Everything and the
    /// rows land when the answer does ([`accept_query`]), so what is on screen
    /// stays there instead of flashing empty on every key.
    unsafe fn refilter(&mut self) {
        self.refilter_keep(false);
    }

    /// Rebuilds the rows for the query edit. `keep` decides what happens to the
    /// cursor: a *background* landing keeps the row the user is on (by
    /// identity, since the row indices shift), while a new query starts at the
    /// top.
    unsafe fn refilter_keep(&mut self, keep: bool) {
        if self.live {
            let pattern = dlg_text(self.me, ID_QUERY);
            if wants_live_query(&pattern) {
                self.spawn_query(&pattern);
                return;
            }
        }
        self.rebuild_rows(keep);
    }

    /// Rows from whatever candidates are in hand, handed to the list. This is
    /// query, `LB_SETCOUNT`, `LB_SETCURSEL(0)`.
    ///
    /// On the live path the candidates are Everything's page for the text in
    /// the box, so the ranking still runs here — Everything decides *which*
    /// files are candidates, the fuzzy matcher decides their order and which
    /// characters to highlight.
    unsafe fn rebuild_rows(&mut self, keep: bool) {
        let pattern = dlg_text(self.me, ID_QUERY);
        let wanted = keep.then(|| self.selected_key()).flatten();
        let src = RowSource {
            entries: &self.entries,
            open_docs: &self.open_docs,
            symbols: &self.symbols,
            tags: &self.tags,
            active_path: self.active_path.as_deref(),
            line_count: self.line_count,
        };
        self.rows = build_rows(&src, &pattern);
        if let Ok(list) = GetDlgItem(Some(self.me), ID_LIST) {
            let _ = SendMessageW(
                list,
                LB_SETCOUNT,
                Some(WPARAM(self.rows.len())),
                Some(LPARAM(0)),
            );
            if !self.rows.is_empty() {
                let row = wanted
                    .and_then(|k| self.rows.iter().position(|r| row_key(r) == k))
                    .unwrap_or(0);
                let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(row)), Some(LPARAM(0)));
            }
            let _ = InvalidateRect(Some(list), None, false);
        }
        // The live path's status carries Everything's own total, which this
        // function cannot know; `refresh_status` owns that line.
        if !self.live && !pattern.trim().is_empty() && !self.walking {
            self.set_status(&format!("{} 个匹配", self.rows.len()));
        }
    }

    /// The identity of the row under the list's cursor, if there is one.
    unsafe fn selected_key(&self) -> Option<RowKey> {
        let list = GetDlgItem(Some(self.me), ID_LIST).ok()?;
        let sel = SendMessageW(list, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 as i32;
        if sel < 0 {
            return None;
        }
        self.rows.get(sel as usize).map(row_key)
    }

    /// The targeted repair: re-list only the directories whose mtime moved and
    /// splice their children into the snapshot. Everything that did not change
    /// keeps its entries and its stamps, which is what makes this O(changed)
    /// rather than O(tree).
    ///
    /// A directory that vanished takes its whole subtree with it; a directory
    /// that appeared is walked, because a fresh checkout can be a whole package.
    unsafe fn repair(&mut self, changed: &[usize]) {
        let opts = WalkOptions::default();
        let mut relist: Vec<String> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        for &i in changed {
            let Some(stamp) = self.dirs.get(i) else {
                continue;
            };
            if std::path::Path::new(&stamp.path).is_dir() {
                relist.push(stamp.path.clone());
            } else {
                gone.push(stamp.path.clone());
            }
        }
        // Anything the re-listed directories used to contribute is dropped and
        // rebuilt from the fresh listing; anything under a vanished directory
        // simply goes.
        let mut stale: Vec<&str> = relist.iter().map(|s| s.as_str()).collect();
        stale.extend(gone.iter().map(|s| s.as_str()));
        self.entries
            .retain(|e| !stale.iter().any(|d| is_direct_child(&e.path, d)));
        self.dirs.retain(|d| {
            !stale.iter().any(|s| *s == d.path)
                && !gone.iter().any(|g| d.path.starts_with(g.as_str()))
        });

        let known: std::collections::HashSet<String> =
            self.dirs.iter().map(|d| d.path.clone()).collect();
        let mut new_dirs: Vec<DirStamp> = Vec::new();
        for dir in &relist {
            // Stamp *before* listing, as `walk_tree` does: a change that lands
            // in between only makes us look stale next time, never the other
            // way round.
            let mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
            let listing = fileindex::list_dir(dir);
            self.entries.extend(listing.files);
            new_dirs.push(DirStamp {
                path: dir.clone(),
                mtime,
            });
            for sub in listing.subdirs {
                if known.contains(&sub.path) || new_dirs.iter().any(|d| d.path == sub.path) {
                    continue;
                }
                // A brand-new directory: a checkout can drop a whole package in
                // at once, so this one is walked rather than merely listed.
                let walked = fileindex::walk_tree(&sub.path, &opts);
                self.entries.extend(walked.entries);
                new_dirs.extend(walked.dirs);
            }
        }
        self.dirs.extend(new_dirs);
        if self.entries.len() > opts.max_files {
            self.entries.truncate(opts.max_files);
        }
        self.merge_open_docs();
    }

    /// The live path: hand the box's text to Everything and let it do the work.
    ///
    /// `limit` is the page size. An empty text asks for a single row — the
    /// dialog shows the open documents for it, so the rows are not what that
    /// query is for; its header's `totitems` is the folder's file count, which
    /// is the number the status line reports without anything being indexed.
    unsafe fn spawn_query(&mut self, pattern: &str) {
        self.query_gen += 1;
        let generation = self.query_gen;
        let limit = if pattern.trim().is_empty() {
            1
        } else {
            CANDIDATES
        };
        let root = self.root.clone();
        let pattern = pattern.to_string();
        let frame = self.frame.0 as isize;
        let echo = pattern.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let answer = everything::search(&root, &pattern, limit);
            if tracing() {
                eprintln!(
                    "ga:query gen={generation} {:?} limit={limit} -> {} in {}us",
                    echo,
                    match &answer {
                        Ok(a) => format!("ok total={} page={}", a.total, a.entries.len()),
                        Err(why) => format!("{why:?}"),
                    },
                    started.elapsed().as_micros()
                );
            }
            let payload: Box<(u64, String, QueryAnswer)> = Box::new((generation, echo, answer));
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(frame as *mut core::ffi::c_void)),
                    WM_GOTOANY,
                    WPARAM(GA_QUERY),
                    LPARAM(Box::into_raw(payload) as isize),
                );
            }
        });
    }

    /// Start the background index
    /// the re-entrancy latch. The thread owns nothing but its own copies and
    /// posts one message when it is done.
    ///
    /// `force` is the 重新索引 button and a root change: those always rebuild.
    /// Everything else (`sync` on a plain reopen) goes through the TTL and then
    /// asks only for a *validation*, which re-stats directories instead of
    /// enumerating files.
    ///
    /// On the live path there is no index to start: 重新索引 and a root change
    /// both just re-ask Everything for the text in the box.
    unsafe fn start_walk(&mut self, force: bool) {
        if self.live {
            let pattern = dlg_text(self.me, ID_QUERY);
            self.spawn_query(&pattern);
            return;
        }
        if self.walking {
            return;
        }
        self.walking = true;
        self.walk_gen += 1;
        let generation = self.walk_gen;
        // HWND is not `Send`; the handle crosses the thread boundary as its
        // value and is rebuilt on the other side, which is the same thing the
        // find-in-files worker does with its wake-up message.
        let frame = self.frame.0 as isize;
        let root = self.root.clone();
        let job = if force || self.dirs.is_empty() || stale(self.full_stamp, DRIFT) {
            Job::Full
        } else {
            Job::Validate(self.dirs.clone())
        };
        if self.entries.is_empty() {
            // Only announce the first scan. A validation is a handful of
            // stats; flashing 正在索引… over the previous result would be noise.
            self.set_status("正在索引…");
        }
        std::thread::spawn(move || {
            let started = Instant::now();
            let update = run_index(&root, job);
            let cost = started.elapsed();
            if tracing() {
                eprintln!(
                    "ga:index gen={generation} worker={}us root={root}",
                    cost.as_micros()
                );
            }
            let payload: Box<(u64, Update)> = Box::new((generation, update));
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(frame as *mut core::ffi::c_void)),
                    WM_GOTOANY,
                    WPARAM(GA_INDEX),
                    LPARAM(Box::into_raw(payload) as isize),
                );
            }
        });
    }

    unsafe fn set_status(&self, text: &str) {
        if let Ok(ctl) = GetDlgItem(Some(self.me), ID_STATUS) {
            let mut buf: Vec<u16> = text.encode_utf16().collect();
            buf.push(0);
            let _ = SendMessageW(
                ctl,
                WM_SETTEXT,
                Some(WPARAM(0)),
                Some(LPARAM(buf.as_ptr() as isize)),
            );
        }
    }

    /// One status line for whatever the background jobs are doing.
    ///
    /// The live path reports Everything's own count, which is the whole match
    /// set even though only a page of it came back — so "116 159 个匹配" is a
    /// fact about the folder, not about a list we hold.
    unsafe fn refresh_status(&self) {
        let tags = self.tags.len();
        if self.live {
            let empty = dlg_text(self.me, ID_QUERY).trim().is_empty();
            let mut s = match (self.live_total, empty) {
                (Some(n), true) => crate::win32::lang::format(40030, &[&n.to_string()])
                    .unwrap_or_else(|| format!("Everything 实时：{n} 个文件")),
                (Some(n), false) => crate::win32::lang::format(40031, &[&n.to_string()])
                    .unwrap_or_else(|| format!("{n} 个匹配（Everything 实时）")),
                (None, _) => crate::win32::lang::string(40032)
                    .unwrap_or("Everything 实时查询…")
                    .to_string(),
            };
            if self.tagging {
                s.push_str(
                    &crate::win32::lang::string(40037).unwrap_or("，正在索引符号…"),
                );
            } else if tags > 0 {
                s.push_str(
                    &crate::win32::lang::format(40036, &[&tags.to_string()])
                        .unwrap_or_else(|| format!("，{tags} 个符号")),
                );
            }
            self.set_status(&s);
            return;
        }
        let files = self.entries.len();
        if self.walking || self.tagging {
            let line = crate::win32::lang::format(
                40033,
                &[&files.to_string(), &tags.to_string()],
            )
            .unwrap_or_else(|| format!("正在索引… {files} 个文件，{tags} 个符号"));
            self.set_status(&line);
            return;
        }
        let mut s = crate::win32::lang::format(40034, &[&files.to_string()])
            .unwrap_or_else(|| format!("索引完成：{files} 个文件"));
        s.push_str(&source_note(self.source));
        if capped(self.source, files) {
            s.push_str(&crate::win32::lang::string(40038).unwrap_or("（已到上限）"));
        }
        if tags > 0 {
            s.push_str(
                &crate::win32::lang::format(40036, &[&tags.to_string()])
                    .unwrap_or_else(|| format!("，{tags} 个符号")),
            );
        }
        self.set_status(&s);
    }

    /// Without it `@` answers from the document's own symbols, which is the
    /// whole feature for anyone who has not opted in. The TTL is much longer
    /// than the walker's: a tag scan costs seconds on a real C/C++ tree, and
    /// symbols only change when a file's *contents* do, which no file-level
    /// watcher would tell us anyway.
    unsafe fn start_tags(&mut self, force: bool) {
        if self.tagging {
            return;
        }
        let Some(cfg) = self.tag_cfg.clone().filter(|c| c.enabled) else {
            // Not opted in: make sure nothing from a previous session lingers.
            self.tags.clear();
            return;
        };
        if !force && !stale(self.tag_stamp, TAG_TTL) {
            return;
        }
        self.tagging = true;
        self.tag_stamp = Some(Instant::now());
        self.tag_gen += 1;
        let generation = self.tag_gen;
        let frame = self.frame.0 as isize;
        std::thread::spawn(move || {
            let started = Instant::now();
            let tags = ctags::scan(&cfg);
            if tracing() {
                eprintln!(
                    "ga:tags gen={generation} dir={} files={} scan={}us",
                    cfg.dir,
                    tags.len(),
                    started.elapsed().as_micros()
                );
            }
            let payload: Box<(u64, Vec<ctags::Tag>)> = Box::new((generation, tags));
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(frame as *mut core::ffi::c_void)),
                    WM_GOTOANY,
                    WPARAM(GA_TAGS),
                    LPARAM(Box::into_raw(payload) as isize),
                );
            }
        });
    }
}

/// The display name of a path, or the whole string when it has no file name.
fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Everything a query may draw from.
struct RowSource<'a> {
    entries: &'a [FileEntry],
    open_docs: &'a [String],
    symbols: &'a [(String, usize)],
    tags: &'a [ctags::Tag],
    active_path: Option<&'a str>,
    line_count: usize,
}

/// One candidate in an `@` query, before it becomes a [`GaRow`]: what to show,
/// what ranks it, and which tag it came from (`None` when it is one of the
/// active document's own symbols).
struct TagHit<'a> {
    name: &'a str,
    line0: usize,
    score: i64,
    positions: Vec<usize>,
    tag: Option<&'a ctags::Tag>,
}

impl<'a> TagHit<'a> {
    /// `None` when `pattern` does not match the name. An empty pattern matches
    /// everything — that is what a bare `@` means.
    fn new(name: &'a str, line0: usize, tag: Option<&'a ctags::Tag>, pattern: &str) -> Option<Self> {
        if pattern.is_empty() {
            return Some(Self {
                name,
                line0,
                score: 0,
                positions: Vec::new(),
                tag,
            });
        }
        let m = dg_core::fuzzy::fuzzy_match(pattern, name, dg_core::fuzzy::FuzzyMode::Fuzzy)?;
        Some(Self {
            name,
            line0,
            score: m.score,
            positions: m.positions,
            tag,
        })
    }
}

/// The row model for a query — pure, so the semantics are testable without a
/// window. Mirrors §3.3 of `docs/goto_anything_plan.md`.
fn build_rows(src: &RowSource<'_>, pattern: &str) -> Vec<GaRow> {
    let mut rows: Vec<GaRow> = Vec::new();
    let p = pattern.trim();
    if p.is_empty() {
        // the open documents are what the user most likely wants here.
        for path in src.open_docs.iter().take(LIMIT) {
            rows.push(GaRow {
                title: file_name_of(path),
                positions: Vec::new(),
                secondary: path.clone(),
                kind: GaKind::File,
                path: path.clone(),
                line: 0,
            });
        }
        return rows;
    }
    if let Some(sym) = p.strip_prefix('@') {
        // A tag the symbol list already covers is skipped so the list does
        // not show the same function twice.
        let covered: std::collections::HashSet<(&str, usize)> =
            src.symbols.iter().map(|(n, l)| (n.as_str(), *l)).collect();
        let active = src.active_path.map(ctags::norm_path);
        let mut hits: Vec<TagHit<'_>> = Vec::new();
        for (name, line0) in src.symbols {
            hits.extend(TagHit::new(name, *line0, None, sym));
        }
        for tag in src.tags {
            let same_doc = active.as_deref() == Some(tag.path.as_str());
            if same_doc && covered.contains(&(tag.name.as_str(), tag.line0)) {
                continue;
            }
            hits.extend(TagHit::new(&tag.name, tag.line0, Some(tag), sym));
        }
        if sym.is_empty() {
            // Parity with `order by LENGTH(name)`: shortest name first.
            hits.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(b.name)));
        } else {
            hits.sort_by_key(|h| std::cmp::Reverse(h.score));
        }
        hits.truncate(LIMIT);
        for h in hits {
            match h.tag {
                Some(tag) => rows.push(GaRow {
                    title: h.name.to_string(),
                    positions: h.positions,
                    secondary: format!("{}:{}", tag.path, h.line0 + 1),
                    // A tag row opens a file at a line, which is the File arm
                    // of `goto_any_accept` with a non-zero line.
                    kind: GaKind::File,
                    path: tag.path.clone(),
                    line: h.line0,
                }),
                None => rows.push(GaRow {
                    title: h.name.to_string(),
                    positions: h.positions,
                    secondary: format!("行 {}", h.line0 + 1),
                    kind: GaKind::Symbol,
                    path: String::new(),
                    line: h.line0,
                }),
            }
        }
        return rows;
    }
    // A digits-only query prepends the line jump. Digits still run through the
    // file match as well
    if let Ok(n) = p.parse::<usize>() {
        if n >= 1 && n <= src.line_count {
            rows.push(GaRow {
                title: crate::win32::lang::format(40039, &[&n.to_string()])
                    .unwrap_or_else(|| format!("跳转到行 {n}")),
                positions: Vec::new(),
                secondary: "当前文档".to_string(),
                kind: GaKind::Line,
                path: String::new(),
                line: n - 1,
            });
        }
    }
    let room = LIMIT - rows.len();
    for hit in fileindex::query(src.entries, p, room) {
        let e = &src.entries[hit.entry];
        rows.push(GaRow {
            title: e.name.clone(),
            positions: hit.m.positions,
            secondary: e.path.clone(),
            kind: GaKind::File,
            path: e.path.clone(),
            line: 0,
        });
    }
    rows
}

unsafe fn create(req: Request<'_>) {
    // The shell dialog font, not the literal 8 pt "MS Shell Dlg" the other
    // reimplemented dialogs still use: the 跳转 box's plain font block resolves
    // to Microsoft YaHei UI 9 pt here. With the legacy block the dialog comes
    // out 499 x 416 instead of 568 x 414.
    let tmpl = optionsdlg::build_template_font(
        TITLE,
        STYLE,
        DLG_W,
        DLG_H,
        &optionsdlg::FontBlock::shell(),
        ITEMS,
    );
    // The dialog manager reads the template's first fields as DWORDs, so the
    // byte vector is copied into a DWORD-aligned buffer (the options dialog's
    // pages do the same).
    let mut aligned = vec![0u32; tmpl.len().div_ceil(4)];
    std::ptr::copy_nonoverlapping(tmpl.as_ptr(), aligned.as_mut_ptr() as *mut u8, tmpl.len());
    optionsdlg::clip_children(&mut aligned);

    let boxed = Box::new(Ga {
        me: HWND::default(),
        frame: req.frame,
        theme: req.theme.clone(),
        font: HFONT::default(),
        // The edit and the list are the lighter field grey on top.
        bg: CreateSolidBrush(to_colorref(req.theme.toolbar_bg)),
        field_bg: CreateSolidBrush(to_colorref(req.theme.toolbar_field)),
        // the theme's `selection_bg` is that colour (its alpha is dropped by
        // the solid brush, which is what a list row needs).
        sel_bg: CreateSolidBrush(to_colorref(req.theme.selection_bg)),
        btn_face: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.toolbar_fg,
            req.theme.toolbar_bg,
            0.10,
        ))),
        // The hover step sits between resting and pressed: a themed button
        // lightens under the cursor, and an owner-drawn one gets nothing at
        // all unless it is painted here. Same two greys, one nudge up.
        btn_hot: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.toolbar_fg,
            req.theme.toolbar_bg,
            0.16,
        ))),
        btn_press: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.toolbar_fg,
            req.theme.toolbar_bg,
            0.22,
        ))),
        btn_pen: CreatePen(
            PS_SOLID,
            1,
            to_colorref(luma_blend(req.theme.toolbar_fg, req.theme.toolbar_bg, 0.64)),
        ),
        // The outline follows the face: on a dark dialog a 6 % face step alone
        // is easy to miss, and the border is the part the eye catches.
        btn_pen_hot: CreatePen(
            PS_SOLID,
            1,
            to_colorref(luma_blend(req.theme.toolbar_fg, req.theme.toolbar_bg, 0.82)),
        ),
        // The 1 px rule around the list is the plain toolbar border, and the
        // query field's two-pixel frame is that same border softened to #383838
        // inside a 1 px ink ring.
        list_border: CreateSolidBrush(to_colorref(req.theme.toolbar_border)),
        field_ring: CreateSolidBrush(to_colorref(req.theme.toolbar_fg)),
        field_edge: CreateSolidBrush(to_colorref(luma_blend(
            req.theme.toolbar_border,
            req.theme.toolbar_bg,
            0.63,
        ))),
        rects: Vec::new(),
        base_client: (0, 0),
        base_outer: (0, 0),
        row_h: 20,
        text_h: 14,
        edit_orig: 0,
        list_orig: 0,
        root: req.root.clone(),
        entries: Vec::new(),
        dirs: Vec::new(),
        source: Source::Walk(everything::Why::NotRunning),
        // Decided here, once: Everything being up is what makes the whole
        // walker unnecessary, so it also decides whether `start_walk` walks.
        live: everything::available(),
        live_total: None,
        open_docs: req.open_docs.clone(),
        symbols: req.symbols.clone(),
        line_count: req.line_count,
        rows: Vec::new(),
        tags: Vec::new(),
        tag_cfg: req.ctags.clone(),
        active_path: req.active_path.clone(),
        walking: false,
        tagging: false,
        tag_stamp: None,
        full_stamp: None,
        walk_gen: 0,
        tag_gen: 0,
        query_gen: 0,
    });
    let param = LPARAM(Box::into_raw(boxed) as isize);
    let dlg = match CreateDialogIndirectParamW(
        Some(instance()),
        aligned.as_ptr() as *const DLGTEMPLATE,
        Some(req.frame),
        Some(dialog_proc),
        param,
    ) {
        Ok(h) => h,
        Err(_) => {
            drop(Box::from_raw(param.0 as *mut Ga));
            return;
        }
    };
    DIALOG.store(dlg.0 as isize, Ordering::Release);
    let ga = state(dlg);
    ga.me = dlg;
    // Place the controls before the first paint, then record that layout as
    // the relayout baseline. The dialog is still hidden here (STYLE has no
    // WS_VISIBLE), so the manager's own font-derived layout never shows.
    ga.apply_template_geometry();
    ga.place_in_frame();
    ga.capture_baseline();
    // The dialog's 9 px sizing frame is drawn by DWM, not by the app: a red
    // brush on the window DC never reaches it (measured — the captured frame
    // stayed `#F3F3F3`). Same call the 查找 dialog makes.
    crate::win32::chrome::dark_theme_window(dlg, crate::win32::chrome::theme_is_dark(&req.theme));
    ga.start_walk(true);
    ga.start_tags(true);
    // The first query is already in flight; say so instead of leaving whatever
    // the template's static started with.
    ga.refresh_status();
    let _ = ShowWindow(dlg, SW_SHOW);
    if let Ok(edit) = GetDlgItem(Some(dlg), ID_QUERY) {
        let _ = SetFocus(Some(edit));
    }
}

/// One child's rectangle, in the dialog's client coordinates.
unsafe fn child_rect(dlg: HWND, id: i32) -> Option<(i32, RECT)> {
    let ctl = GetDlgItem(Some(dlg), id).ok()?;
    let mut wr = RECT::default();
    GetWindowRect(ctl, &mut wr).ok()?;
    let mut pts = [
        POINT {
            x: wr.left,
            y: wr.top,
        },
        POINT {
            x: wr.right,
            y: wr.bottom,
        },
    ];
    windows::Win32::Graphics::Gdi::MapWindowPoints(None, Some(dlg), &mut pts);
    Some((
        id,
        RECT {
            left: pts[0].x,
            top: pts[0].y,
            right: pts[1].x,
            bottom: pts[1].y,
        },
    ))
}

/// The bottom row (status + both buttons) stays pinned to the bottom edge. A 6
/// px inset where the template's own is 4. Its baseline is therefore two pixels
/// larger than the client it reports, and that is all this constant reproduces.
unsafe fn relayout(dlg: HWND) {
    let ga = state(dlg);
    if ga.base_client == (0, 0) {
        return; // WM_SIZE before WM_INITDIALOG
    }
    let mut rc = RECT::default();
    if GetClientRect(dlg, &mut rc).is_err() {
        return;
    }
    let dx = rc.right - ga.base_client.0 - RELAYOUT_BIAS;
    let dy = rc.bottom - ga.base_client.1 - RELAYOUT_BIAS;
    if tracing() {
        eprintln!(
            "ga:size client={}x{} base={}x{} dx={} dy={}",
            rc.right, rc.bottom, ga.base_client.0, ga.base_client.1, dx, dy
        );
    }
    let mut moved = false;
    for (id, r0) in &ga.rects {
        let mut r = *r0;
        match *id {
            ID_QUERY => r.right += dx,
            ID_LIST => {
                r.right += dx;
                r.bottom += dy;
            }
            // The bottom row keeps its height and rides the bottom edge.
            ID_STATUS => {
                r.top += dy;
                r.bottom += dy;
            }
            ID_REINDEX | ID_CLOSE => {
                r.left += dx;
                r.right += dx;
                r.top += dy;
                r.bottom += dy;
            }
            _ => continue,
        }
        if let Ok(ctl) = GetDlgItem(Some(dlg), *id) {
            if tracing() {
                eprintln!(
                    "ga:move id={} -> ({},{}) {}x{}",
                    *id,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top
                );
            }
            let _ = MoveWindow(
                ctl,
                r.left,
                r.top,
                (r.right - r.left).max(0),
                (r.bottom - r.top).max(0),
                // No per-child repaint: each one erased and redrew separately,
                // which is the resize flash. One invalidate below lands them in
                // a single composed pass.
                false,
            );
            moved = true;
        }
    }
    if moved {
        let _ = RedrawWindow(
            Some(dlg),
            None,
            None,
            REDRAW_WINDOW_FLAGS(RDW_INVALIDATE.0 | RDW_ALLCHILDREN.0),
        );
    }
}

unsafe fn dlg_text(dlg: HWND, id: i32) -> String {
    let Ok(ctl) = GetDlgItem(Some(dlg), id) else {
        return String::new();
    };
    let len = windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(ctl);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(ctl, &mut buf);
    buf.truncate(n.max(0) as usize);
    String::from_utf16_lossy(&buf)
}

/// Move the list's cursor by `delta` rows
/// does with the `VK_UP`/`VK_DOWN` it forwards from its edit subclass; Page
/// Up/Down are added because a hundred-row list is unusable without them.
unsafe fn move_sel(dlg: HWND, delta: i32) {
    let ga = state(dlg);
    if ga.rows.is_empty() {
        return;
    }
    let Ok(list) = GetDlgItem(Some(dlg), ID_LIST) else {
        return;
    };
    let sel = SendMessageW(list, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 as i32;
    let next = if sel < 0 {
        0
    } else {
        (sel + delta).clamp(0, ga.rows.len() as i32 - 1)
    };
    let _ = SendMessageW(
        list,
        LB_SETCURSEL,
        Some(WPARAM(next as usize)),
        Some(LPARAM(0)),
    );
    let _ = InvalidateRect(Some(list), None, false);
}

/// The list's frame. `WS_BORDER` (which the template carries) is painted by the
/// system in the light "window frame" colour, which reads as a white outline
/// around a dark list subclass does too. Everything else goes to the listbox.
unsafe extern "system" fn list_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    let Ok(dlg) = GetParent(hwnd) else {
        return 0;
    };
    let Some(ga) = state_opt(dlg) else {
        return 0;
    };
    if msg == WM_NCPAINT {
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_ok() {
            let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
            let hdc = GetWindowDC(Some(hwnd));
            for r in [
                RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: 1,
                },
                RECT {
                    left: 0,
                    top: h - 1,
                    right: w,
                    bottom: h,
                },
                RECT {
                    left: 0,
                    top: 1,
                    right: 1,
                    bottom: h - 1,
                },
                RECT {
                    left: w - 1,
                    top: 1,
                    right: w,
                    bottom: h - 1,
                },
            ] {
                FillRect(hdc, &r, ga.list_border);
            }
            let _ = ReleaseDC(Some(hwnd), hdc);
        }
        return 0;
    }
    if ga.rows.is_empty() && (msg == WM_PAINT || msg == WM_PRINTCLIENT) {
        // The empty text (see [`HINT`]) sits on the face the listbox paints
        // itself
        // drawn over it. `WM_PRINTCLIENT` is here for the same reason: a
        // `PrintWindow` capture never sends `WM_PAINT`.
        let done = if ga.list_orig != 0 {
            let old: WNDPROC = core::mem::transmute(ga.list_orig);
            CallWindowProcW(old, hwnd, msg, wparam, lparam).0 as isize
        } else {
            0
        };
        let printed = msg == WM_PRINTCLIENT;
        let hdc = if printed {
            HDC(wparam.0 as *mut _)
        } else {
            GetDC(Some(hwnd))
        };
        if !hdc.is_invalid() {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let old_font = SelectObject(hdc, HGDIOBJ(ga.font.0));
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, to_colorref(ga.theme.menu_disabled));
                for (i, line) in HINT.iter().enumerate() {
                    let mut buf: Vec<u16> = line.encode_utf16().collect();
                    let mut trc = RECT {
                        left: rc.left + 4,
                        top: rc.top + 10 + 22 * i as i32,
                        right: rc.right,
                        bottom: rc.bottom,
                    };
                    DrawTextW(hdc, &mut buf, &mut trc, DT_SINGLELINE | DT_NOPREFIX);
                }
                SelectObject(hdc, old_font);
            }
            if !printed {
                let _ = ReleaseDC(Some(hwnd), hdc);
            }
        }
        return done;
    }
    if ga.list_orig != 0 {
        let old: WNDPROC = core::mem::transmute(ga.list_orig);
        return CallWindowProcW(old, hwnd, msg, wparam, lparam).0 as isize;
    }
    0
}

/// The query edit's subclass: arrows move the list's selection while the caret
/// stays in the edit
/// and Escape are deliberately not touched; the dialog manager owns them. The
/// two-pixel frame is repainted here as well (see the `WM_NCPAINT` arm).
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    let Ok(dlg) = GetParent(hwnd) else {
        return 0;
    };
    if dlg.is_invalid() {
        return 0;
    }
    if let Some(ga) = state_opt(dlg) {
        if msg == WM_NCPAINT {
            // White on the outside, #383838 on the inside — where the system
            // paints its own (a near-white ring with the accent blue along the
            // bottom edge on a focused field). Only the colours need replacing.
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_ok() {
                let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
                let hdc = GetWindowDC(Some(hwnd));
                for r in [
                    RECT {
                        left: 0,
                        top: 0,
                        right: w,
                        bottom: 1,
                    },
                    RECT {
                        left: 0,
                        top: h - 1,
                        right: w,
                        bottom: h,
                    },
                    RECT {
                        left: 0,
                        top: 1,
                        right: 1,
                        bottom: h - 1,
                    },
                    RECT {
                        left: w - 1,
                        top: 1,
                        right: w,
                        bottom: h - 1,
                    },
                ] {
                    FillRect(hdc, &r, ga.field_ring);
                }
                for r in [
                    RECT {
                        left: 1,
                        top: 1,
                        right: w - 1,
                        bottom: 2,
                    },
                    RECT {
                        left: 1,
                        top: h - 2,
                        right: w - 1,
                        bottom: h - 1,
                    },
                    RECT {
                        left: 1,
                        top: 2,
                        right: 2,
                        bottom: h - 2,
                    },
                    RECT {
                        left: w - 2,
                        top: 2,
                        right: w - 1,
                        bottom: h - 2,
                    },
                ] {
                    FillRect(hdc, &r, ga.field_edge);
                }
                let _ = ReleaseDC(Some(hwnd), hdc);
            }
            return 0;
        }
        if msg == WM_KEYDOWN {
            let page = match GetDlgItem(Some(dlg), ID_LIST) {
                Ok(list) => {
                    let mut rc = RECT::default();
                    if GetClientRect(list, &mut rc).is_ok() && ga.row_h > 0 {
                        (rc.bottom / ga.row_h).max(1)
                    } else {
                        10
                    }
                }
                Err(_) => 10,
            };
            match wparam.0 as u16 as u32 {
                vk if vk == VK_UP.0 as u32 => {
                    move_sel(dlg, -1);
                    return 0;
                }
                vk if vk == VK_DOWN.0 as u32 => {
                    move_sel(dlg, 1);
                    return 0;
                }
                vk if vk == VK_PRIOR.0 as u32 => {
                    move_sel(dlg, -page);
                    return 0;
                }
                vk if vk == VK_NEXT.0 as u32 => {
                    move_sel(dlg, page);
                    return 0;
                }
                _ => {}
            }
        }
        if ga.edit_orig != 0 {
            let old: WNDPROC = core::mem::transmute(ga.edit_orig);
            return CallWindowProcW(old, hwnd, msg, wparam, lparam).0 as isize;
        }
    }
    0
}

/// `GWLP_USERDATA` as an `Option`, for the subclass which runs during teardown
/// too and must not assume the box is there.
unsafe fn state_opt(hwnd: HWND) -> Option<&'static Ga> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Ga;
    (!p.is_null()).then_some(&*p)
}

/// The dialog proc. `DLGPROC` is a plain `-> isize` fn pointer in the `windows`
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
            crate::win32::lang::localize_dialog(hwnd, 304);
            let ga = state(hwnd);
            ga.me = hwnd;
            // The template's font, as the dialog manager handed it to the
            // children. Row height is that font's cell plus breathing room.
            if let Ok(edit) = GetDlgItem(Some(hwnd), ID_QUERY) {
                let f = SendMessageW(
                    edit,
                    windows::Win32::UI::WindowsAndMessaging::WM_GETFONT,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                );
                ga.font = HFONT(f.0 as *mut core::ffi::c_void);
            }
            let mut sz = SIZE::default();
            if !ga.font.is_invalid() {
                let hdc = GetDC(Some(hwnd));
                let old = SelectObject(hdc, HGDIOBJ(ga.font.0));
                let probe: Vec<u16> = "Zg".encode_utf16().collect();
                let _ = GetTextExtentPoint32W(hdc, &probe, &mut sz);
                SelectObject(hdc, old);
                let _ = ReleaseDC(Some(hwnd), hdc);
            }
            ga.text_h = if sz.cy > 0 { sz.cy } else { 16 };
            ga.row_h = 2 * ga.text_h + 10;
            if let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) {
                let _ = SendMessageW(
                    list,
                    LB_SETITEMHEIGHT,
                    Some(WPARAM(0)),
                    Some(LPARAM(ga.row_h as isize)),
                );
            }
            if let Ok(edit) = GetDlgItem(Some(hwnd), ID_QUERY) {
                ga.edit_orig =
                    SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
            }
            // The template carries no `WS_CLIPCHILDREN`, and the dialog's
            // `WM_ERASEBKGND` fills the whole client with the theme face —
            // over every control, which only comes back when it paints. One
            // resize or activation would then start from blank children.
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_CLIPCHILDREN.0) as isize);
            // The two owner-drawn buttons get `btn_proc`, which is only there
            // for the hover state: `BS_OWNERDRAW` means the control itself
            // never paints, so nothing happens under the cursor unless the
            // subclass notices the mouse and the paint reads it back. A flat
            // button with no feedback reads as broken next to every other
            // control on the desktop.
            ownerdraw::install(hwnd, &[ID_REINDEX, ID_CLOSE]);
            // The list's frame (see `list_proc`). Two things are put back to the
            // template's own values first: the dialog manager turns a listbox's
            // `WS_BORDER` into a two-pixel `WS_EX_CLIENTEDGE` on this build.
            if let Ok(list) = GetDlgItem(Some(hwnd), ID_LIST) {
                let st = GetWindowLongPtrW(list, GWL_STYLE) as u32;
                SetWindowLongPtrW(list, GWL_STYLE, (st | WS_BORDER.0 | WS_VSCROLL.0) as isize);
                let ex = GetWindowLongPtrW(list, GWL_EXSTYLE) as u32;
                SetWindowLongPtrW(list, GWL_EXSTYLE, (ex & !WS_EX_CLIENTEDGE.0) as isize);
                let _ = SetWindowPos(
                    list,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                ga.list_orig =
                    SetWindowLongPtrW(list, GWLP_WNDPROC, list_proc as *const () as usize as isize);
            }
            ga.capture_baseline();
            ga.set_status("正在索引…");
            // TRUE: the manager sets the initial focus, which lands on the
            // query edit because it is the first tab stop.
            1
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_DRAWITEM => on_draw_item(hwnd, lparam),
        WM_ERASEBKGND => {
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, state(hwnd).bg);
            }
            1
        }
        WM_CTLCOLOREDIT => {
            let ga = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(ga.theme.foreground),
                to_colorref(ga.theme.toolbar_field),
                ga.field_bg,
            )
        }
        WM_CTLCOLORSTATIC => {
            let ga = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(ga.theme.toolbar_fg),
                to_colorref(ga.theme.toolbar_bg),
                ga.bg,
            )
        }
        WM_CTLCOLORLISTBOX => {
            let ga = state(hwnd);
            customdraw::ctlcolor(
                HDC(wparam.0 as *mut _),
                to_colorref(ga.theme.panel_fg),
                to_colorref(ga.theme.toolbar_field),
                ga.field_bg,
            )
        }
        WM_SIZE => {
            relayout(hwnd);
            0
        }
        WM_EXITSIZEMOVE => {
            // The end of a drag-resize. The sizing loop paints the dialog many
            // times while the mouse is down, and every one of those paints is
            // clipped to that frame's update region, so whatever the children
            // had at the time stays on screen wherever a later paint did not
            // cover it. One forced repaint of the dialog *and* every child is
            // what makes "I resized it and the buttons went wrong" impossible:
            // the owner-draw buttons are invalidated too, so they send their
            // WM_DRAWITEM again and repaint their whole client area.
            let _ = RedrawWindow(
                Some(hwnd),
                None,
                None,
                REDRAW_WINDOW_FLAGS(
                    RDW_INVALIDATE.0 | RDW_ALLCHILDREN.0 | RDW_UPDATENOW.0,
                ),
            );
            0
        }
        WM_GETMINMAXINFO => {
            // the minimum is the template size, the maximum is left to the
            // system.
            let ga = state(hwnd);
            if ga.base_outer != (0, 0) {
                let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT {
                    x: ga.base_outer.0,
                    y: ga.base_outer.1,
                };
            }
            0
        }
        WM_CLOSE => {
            // Hide, do not destroy: the singleton keeps its index and text.
            let _ = ShowWindow(hwnd, SW_HIDE);
            1
        }
        WM_DESTROY => {
            DIALOG.store(0, Ordering::Release);
            0
        }
        WM_NCDESTROY => {
            // Every child (including the subclassed edit) has had its last
            // message by now, so this is the safe place to free the box.
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ga;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !ptr.is_null() {
                let ga = Box::from_raw(ptr);
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.bg.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.field_bg.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.sel_bg.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.btn_face.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.btn_hot.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.btn_press.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.btn_pen.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.btn_pen_hot.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.list_border.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.field_ring.0));
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(ga.field_edge.0));
            }
            0
        }
        _ => 0,
    }
}

/// Enter, Escape, typing and the two buttons, translated into messages for the
/// frame. The dialog never touches a document.
unsafe fn on_command(hwnd: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
    let frame = state(hwnd).frame;
    if id == ID_OK {
        post(frame, GA_ACCEPT, 0);
        return 1;
    }
    if id == ID_CLOSE {
        // Both Escape (IDCANCEL) and a click on 关闭 land here.
        post(frame, GA_DISMISS, 0);
        return 1;
    }
    if id == ID_QUERY && code == EN_CHANGE as u16 {
        state(hwnd).refilter();
        return 1;
    }
    if id == ID_LIST {
        if code == LBN_DBLCLK {
            post(frame, GA_ACCEPT, 0);
            return 1;
        }
        if code == LBN_SELCHANGE {
            // The list owns its cursor; nothing to mirror.
            return 1;
        }
        return 0;
    }
    if id == ID_REINDEX && code == 0 {
        // This implementation rebuilds both tables, since 重新索引 reads as
        // "rebuild the index", not "rebuild half of it" — the tag half is a
        // no-op unless the user enabled ctags. It also bypasses the
        // revalidation TTLs: a button press means "now, not in a minute".
        let ga = state(hwnd);
        ga.start_walk(true);
        ga.start_tags(true);
        return 1;
    }
    0
}

/// 重新索引 and 关闭 shade above the dialog background, a one-pixel border in the
/// muted ink, and the label centred in white. The shades come from the theme so
/// a light theme still reads correctly — the ink is `toolbar_fg` at 10 % over
/// `toolbar_bg` for the face and 64 % for the border, which lands on the
/// measured pair, and the label uses `menu_fg` (`#F0F0F0`, the nearest theme
/// ink to pure white). When the pixels and the geometry disagree — a button
/// sitting in the right place but painted with the wrong rect — this is the
/// only way to see what the dialog actually asked for. One `var_os` lookup per
/// paint when off.
fn tracing() -> bool {
    std::env::var_os("DG_GA_TRACE").is_some()
}

unsafe fn draw_button(hwnd: HWND, di: &DRAWITEMSTRUCT) -> isize {
    let ga = state(hwnd);
    let label = ITEMS
        .iter()
        .find(|it| it.id == di.CtlID as i32)
        .map(|it| it.label)
        .unwrap_or("");
    let label = crate::win32::lang::dlg(304, di.CtlID as u32).unwrap_or(label);
    // The font the button actually carries, not the one the dialog cached in
    // `WM_INITDIALOG`: they are the same HFONT (both come from the template),
    // but the cached one can outlive the font it names, and a label drawn with
    // a dead handle is exactly the "text turns into a smear" failure.
    let font = GetDlgItem(Some(hwnd), di.CtlID as i32)
        .map(|c| {
            let f = SendMessageW(c, WM_GETFONT, Some(WPARAM(0)), Some(LPARAM(0)));
            HFONT(f.0 as *mut core::ffi::c_void)
        })
        .ok()
        .filter(|f| !f.is_invalid())
        .unwrap_or(ga.font);
    // The dialog surface shows as a ring around it: measured, the control
    // sits at y 382…410 and the 1 px border runs at y 383 and y 408 with the
    // face between them. `RoundRect`'s right/bottom edges are exclusive, so
    // the inner rect's right/bottom are the control's minus one as well —
    // that is `inset: 1` here.
    let look = ownerdraw::Look {
        face: ga.btn_face,
        hot: ga.btn_hot,
        press: ga.btn_press,
        edge: ownerdraw::Edge::Round {
            pen: ga.btn_pen,
            hot_pen: Some(ga.btn_pen_hot),
            // An 8×8 corner ellipse, i.e. a 4 px radius spans four pixels.
            // GDI cannot anti-alias a `RoundRect`. That is the one
            // difference left in the button's paint.
            diameter: 8,
        },
        ink: ownerdraw::Ink {
            normal: to_colorref(ga.theme.menu_fg),
            pressed: to_colorref(ga.theme.menu_fg),
            disabled: to_colorref(ga.theme.menu_disabled),
        },
        font,
        // A button erases itself to the system face colour first; without this
        // the ring and the pixels outside the corner arc keep that light colour
        // and show as a white notch.
        under: Some(ga.bg),
        inset: 1,
        // A pressed button's label travels one pixel, as the system's own does.
        press_offset: 1,
        live_rect: true,
    };
    let rc = ownerdraw::frame(di, &look);
    ownerdraw::label(di, rc, &look, label, ownerdraw::Align::Center);
    if tracing() {
        eprintln!(
            "ga:drawitem id={} item={} state={:#06x} rc=({},{})-({},{}) rcItem=({},{})-({},{})",
            di.CtlID,
            di.itemID,
            di.itemState.0,
            rc.left,
            rc.top,
            rc.right,
            rc.bottom,
            di.rcItem.left,
            di.rcItem.top,
            di.rcItem.right,
            di.rcItem.bottom
        );
    }
    1
}

unsafe fn on_draw_item(hwnd: HWND, lparam: LPARAM) -> isize {
    let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
    if di.CtlID as i32 == ID_REINDEX || di.CtlID as i32 == ID_CLOSE {
        return draw_button(hwnd, di);
    }
    if di.CtlID as i32 != ID_LIST {
        return 0;
    }
    let ga = state(hwnd);
    let Some(row) = ga.rows.get(di.itemID as usize) else {
        return 1;
    };
    let sel = di.itemState.0 & ODS_SELECTED.0 != 0;
    let hdc = di.hDC;
    let rc = di.rcItem;
    FillRect(hdc, &rc, if sel { ga.sel_bg } else { ga.field_bg });
    let old = SelectObject(hdc, HGDIOBJ(ga.font.0));
    SetBkMode(hdc, TRANSPARENT);
    let fg = if sel {
        ga.theme.foreground
    } else {
        ga.theme.panel_fg
    };
    // Two lines
    // characters in the accent colour, the path (or 行 N) dimmed underneath.
    let y0 = rc.top + 4;
    let _ = draw_runs(
        hdc,
        rc.left + 6,
        y0,
        &row.title,
        &row.positions,
        to_colorref(fg),
        to_colorref(ga.theme.match_fg),
    );
    if !row.secondary.is_empty() {
        let dim = if sel {
            ga.theme.foreground
        } else {
            ga.theme.menu_disabled
        };
        let max_w = rc.right - rc.left - 12;
        let mut buf = fit_u16(hdc, &row.secondary, max_w);
        let mut srect = RECT {
            left: rc.left + 6,
            top: y0 + ga.text_h,
            right: rc.right - 6,
            bottom: y0 + 2 * ga.text_h,
        };
        SetTextColor(hdc, to_colorref(dim));
        DrawTextW(hdc, &mut buf, &mut srect, DT_SINGLELINE | DT_NOPREFIX);
    }
    if di.itemState.0 & ODS_FOCUS.0 != 0 {
        let _ = DrawFocusRect(hdc, &rc);
    }
    SelectObject(hdc, old);
    1
}

/// Draw `text` from `x`, colouring the characters whose **byte** offsets are in
/// `positions` (what `fuzzy_match` reports) with `hi`. Returns the x after the
/// last character. The offsets are mapped onto the UTF-16 drawing buffer, so a
/// CJK name highlights the same characters it matched.
unsafe fn draw_runs(
    hdc: HDC,
    mut x: i32,
    y: i32,
    text: &str,
    positions: &[usize],
    normal: COLORREF,
    hi: COLORREF,
) -> i32 {
    let buf: Vec<u16> = text.encode_utf16().collect();
    // byte offset -> UTF-16 index, valid at char starts.
    let mut map = vec![0usize; text.len() + 1];
    let (mut b, mut u) = (0usize, 0usize);
    for ch in text.chars() {
        map[b] = u;
        b += ch.len_utf8();
        u += ch.len_utf16();
    }
    map[b] = u;
    let mut pos: Vec<usize> = positions.to_vec();
    pos.sort_unstable();
    pos.dedup();
    let marks: Vec<usize> = pos
        .iter()
        .map(|&p| map[p.min(text.len())])
        .filter(|&p| p < buf.len())
        .collect();
    let mut start = 0usize;
    for p in marks {
        if p > start {
            x = draw_seg(hdc, x, y, &buf[start..p], normal);
        }
        x = draw_seg(hdc, x, y, &buf[p..p + 1], hi);
        start = p + 1;
    }
    if start < buf.len() {
        x = draw_seg(hdc, x, y, &buf[start..], normal);
    }
    x
}

unsafe fn draw_seg(hdc: HDC, x: i32, y: i32, seg: &[u16], color: COLORREF) -> i32 {
    if seg.is_empty() {
        return x;
    }
    SetTextColor(hdc, color);
    let _ = TextOutW(hdc, x, y, seg);
    let mut sz = SIZE::default();
    if GetTextExtentPoint32W(hdc, seg, &mut sz).as_bool() {
        x + sz.cx
    } else {
        x
    }
}

/// `s` cut to `max_w` pixels with a trailing ellipsis. Returns UTF-16 units
/// ready for `DrawTextW`.
unsafe fn fit_u16(hdc: HDC, s: &str, max_w: i32) -> Vec<u16> {
    const ELLIPSIS: u16 = 0x2026;
    let buf: Vec<u16> = s.encode_utf16().collect();
    if max_w <= 0 {
        return vec![ELLIPSIS];
    }
    let mut sz = SIZE::default();
    if GetTextExtentPoint32W(hdc, &buf, &mut sz).as_bool() && sz.cx <= max_w {
        return buf;
    }
    let mut n = buf.len();
    while n > 0 {
        let mut t = buf[..n].to_vec();
        t.push(ELLIPSIS);
        if GetTextExtentPoint32W(hdc, &t, &mut sz).as_bool() && sz.cx <= max_w {
            return t;
        }
        n -= 1;
    }
    vec![ELLIPSIS]
}

unsafe fn post(frame: HWND, kind: usize, code: isize) {
    let _ = PostMessageW(Some(frame), WM_GOTOANY, WPARAM(kind), LPARAM(code));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: format!(r"E:\tmp\{name}"),
        }
    }

    /// The status line has to answer "why is it not using Everything?" on its
    /// own. Everything's *absence* needs no words; Everything declining does.
    #[test]
    fn source_note_says_why_the_walker_ran() {
        assert_eq!(source_note(Source::Everything), "（Everything 实时）");
        for quiet in [everything::Why::NotRunning, everything::Why::Forced] {
            assert_eq!(
                source_note(Source::Walk(quiet)),
                "（本地扫描）",
                "{quiet:?} is the ordinary case and stays silent"
            );
        }
        for loud in [
            everything::Why::Empty,
            everything::Why::Refused,
            everything::Why::Timeout,
        ] {
            let note = source_note(Source::Walk(loud));
            assert!(
                note.starts_with("（本地扫描·Everything "),
                "{loud:?} -> {note}"
            );
        }
    }

    /// The 20 000 budget belongs to the walker alone. Everything is asked for
    /// the whole folder, so its count sitting above 20 000 is normal and must
    /// not be reported as a truncated index.
    #[test]
    fn only_the_walker_reports_a_cap() {
        let max = WalkOptions::default().max_files;
        assert!(capped(Source::Walk(everything::Why::NotRunning), max));
        assert!(capped(Source::Walk(everything::Why::NotRunning), max + 1));
        assert!(!capped(Source::Walk(everything::Why::NotRunning), max - 1));
        assert!(
            !capped(Source::Everything, max),
            "Everything is never truncated"
        );
        assert!(!capped(Source::Everything, 116_750));
    }

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    /// A `RowSource` with nothing but the files; tests fill in what they mean.
    fn src<'a>(
        entries: &'a [FileEntry],
        open_docs: &'a [String],
        symbols: &'a [(String, usize)],
        tags: &'a [ctags::Tag],
        line_count: usize,
    ) -> RowSource<'a> {
        RowSource {
            entries,
            open_docs,
            symbols,
            tags,
            active_path: None,
            line_count,
        }
    }

    /// The template has to be the 跳转 box: the EX signature, the literal style
    /// word (no caption — this box has none), six items and the
    /// 285x208 DLU box.
    #[test]
    fn template_is_dlg304() {
        let b = optionsdlg::build_template(TITLE, STYLE, DLG_W, DLG_H, 400, ITEMS);
        assert_eq!(u16_at(&b, 0), 1, "dlgVer");
        assert_eq!(u16_at(&b, 2), 0xFFFF, "DLGTEMPLATEEX signature");
        assert_eq!(u32_at(&b, 12), STYLE);
        assert_eq!(u16_at(&b, 16) as usize, ITEMS.len(), "cDlgItems");
        assert_eq!(u16_at(&b, 22), DLG_W, "cx");
        assert_eq!(u16_at(&b, 24), DLG_H, "cy");
        assert_eq!(STYLE & 0x00C0_0000, 0, "no WS_CAPTION");
        assert_eq!(STYLE & 0x0008_0000, 0, "no WS_SYSMENU");
        assert_ne!(STYLE & 0x0004_0000, 0, "WS_THICKFRAME");
    }

    /// See `draw_button`.
    #[test]
    fn only_the_buttons_are_owner_drawn() {
        for it in ITEMS {
            let ownerdraw = it.style & 0x0000_000F == 0x0000_000B; // BS_OWNERDRAW
            let want = it.id == ID_REINDEX || it.id == ID_CLOSE;
            assert_eq!(ownerdraw, want, "id {}", it.id);
        }
    }

    /// Not `MapDialogRect`, which would hand back 7x16 base units for the shell
    /// font and a dialog 100 px too wide.
    #[test]
    fn controls_land_on_the_declared_rectangles() {
        let want = [
            (ID_QUERY, 4, 4, 560, 30),
            (ID_LIST, 4, 38, 560, 340),
            (ID_STATUS, 4, 382, 370, 22),
            (ID_REINDEX, 386, 382, 100, 28),
            (ID_CLOSE, 488, 382, 76, 28),
        ];
        for (id, x, y, w, h) in want {
            let it = ITEMS.iter().find(|i| i.id == id).expect("template item");
            assert_eq!(
                (dlu(it.x), dlu(it.y), dlu(it.w), dlu(it.h)),
                (x, y, w, h),
                "id {id}"
            );
        }
        assert_eq!(dlu(DLG_W as i16) - DLU_PX, 568, "client width");
        assert_eq!(dlu(DLG_H as i16) - DLU_PX, 414, "client height");
    }

    /// The control ids are the template's, and no two items share one.
    #[test]
    fn control_ids_are_the_resources() {
        let ids: Vec<i32> = ITEMS.iter().map(|i| i.id).collect();
        assert!(ids.contains(&1200) && ids.contains(&1201) && ids.contains(&1202));
        assert!(ids.contains(&1203) && ids.contains(&2));
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    /// An empty query lists the open documents (intentional difference).
    #[test]
    fn empty_query_lists_open_docs() {
        let docs = vec![r"E:\a\main.rs".to_string(), r"E:\b\lib.rs".to_string()];
        let rows = build_rows(&src(&[], &docs, &[], &[], 0), "");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "main.rs");
        assert_eq!(rows[0].kind, GaKind::File);
        assert_eq!(rows[0].secondary, r"E:\a\main.rs");
    }

    /// Digits prepend the line jump, and only inside the document.
    #[test]
    fn digits_prepend_a_line_row_inside_the_document() {
        let rows = build_rows(&src(&[], &[], &[], &[], 200), "123");
        assert_eq!(rows[0].kind, GaKind::Line);
        assert_eq!(rows[0].title, "跳转到行 123");
        assert_eq!(rows[0].line, 122, "0-based target line");
        // Past the end: no line row.
        let rows = build_rows(&src(&[], &[], &[], &[], 200), "999");
        assert!(!rows.iter().any(|r| r.kind == GaKind::Line));
        // Not digits at all.
        let rows = build_rows(&src(&[], &[], &[], &[], 200), "12a");
        assert!(!rows.iter().any(|r| r.kind == GaKind::Line));
    }

    /// `@` filters the current document's symbols; 0-based lines become 1-based
    /// display text.
    #[test]
    fn at_prefix_filters_symbols() {
        let syms = vec![("main".to_string(), 4usize), ("domain".to_string(), 9usize)];
        let rows = build_rows(&src(&[], &[], &syms, &[], 100), "@mai");
        assert_eq!(rows.len(), 2, "'main' and 'domain' are both subsequences");
        assert_eq!(rows[0].title, "main", "the prefix match ranks first");
        assert_eq!(rows[0].kind, GaKind::Symbol);
        assert_eq!(rows[0].line, 4);
        assert_eq!(rows[0].secondary, "行 5");
        assert!(build_rows(&src(&[], &[], &syms, &[], 100), "@zzz").is_empty());
    }

    #[test]
    fn file_query_is_a_subsequence_with_positions_and_a_cap() {
        let entries = vec![entry("changelog.txt"), entry("readme.md")];
        let rows = build_rows(&src(&entries, &[], &[], &[], 0), "chg");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, GaKind::File);
        assert_eq!(rows[0].positions, vec![0, 1, 4]);

        let many: Vec<FileEntry> = (0..150).map(|i| entry(&format!("file{i}.txt"))).collect();
        let rows = build_rows(&src(&many, &[], &[], &[], 0), "file");
        assert_eq!(rows.len(), LIMIT);
    }

    /// `@` merges the document's symbols with the (opt-in) workspace tags,
    /// drops a tag the symbol list already covers, and keeps the rest as
    /// file+line rows. With ctags off the tag slice is empty and none of this
    /// runs — that is the shipped default.
    #[test]
    fn at_prefix_merges_workspace_tags() {
        let syms = vec![("main".to_string(), 11usize)];
        let tags = vec![
            ctags::Tag {
                name: "main".to_string(),
                path: r"E:\r\src\main.rs".to_string(),
                line0: 11,
            },
            ctags::Tag {
                name: "helper".to_string(),
                path: r"E:\r\src\util.rs".to_string(),
                line0: 4,
            },
        ];
        let mut s = src(&[], &[], &syms, &tags, 0);
        s.active_path = Some(r"E:\r\./src/main.rs");
        let rows = build_rows(&s, "@");
        assert_eq!(rows.len(), 2, "the duplicate 'main' is dropped");
        let helper = rows.iter().find(|r| r.title == "helper").expect("tag row");
        assert_eq!(helper.kind, GaKind::File, "a tag opens its file");
        assert_eq!(helper.path, r"E:\r\src\util.rs");
        assert_eq!(helper.line, 4);
        assert_eq!(helper.secondary, r"E:\r\src\util.rs:5");
        let main = rows.iter().find(|r| r.title == "main").expect("symbol row");
        assert_eq!(main.kind, GaKind::Symbol, "the document's own symbol wins");
    }

    /// The drift backstop: never walked counts as stale, and a stamp is reused
    /// until its TTL runs out.
    #[test]
    fn stale_honours_the_drift_backstop() {
        assert!(stale(None, DRIFT), "never scanned is always stale");
        assert!(!stale(Some(Instant::now()), DRIFT), "just scanned");
        assert!(
            stale(Some(Instant::now() - DRIFT), DRIFT),
            "a stamp at exactly the TTL is due"
        );
        assert!(stale(Some(Instant::now() - DRIFT), DRIFT), "drift is due");
    }

    /// The repair splices a directory's children by dropping everything that
    /// sits **directly** in it — a subdirectory's files must survive.
    #[test]
    fn direct_child_is_one_level_only() {
        assert!(is_direct_child(r"E:\p\a.txt", r"E:\p"));
        assert!(!is_direct_child(r"E:\p\sub\a.txt", r"E:\p"), "grandchild");
        assert!(!is_direct_child(r"E:\p", r"E:\p"), "the directory itself");
        assert!(!is_direct_child(r"E:\pp\a.txt", r"E:\p"), "sibling prefix");
        assert!(is_direct_child("E:/p/a.txt", "E:/p"), "forward slashes too");
    }

    /// Restoring the cursor after a background rebuild compares identities, so
    /// two rows that only look alike must not be confused.
    #[test]
    fn row_key_separates_same_named_rows() {
        let a = GaRow {
            title: "main.rs".into(),
            positions: Vec::new(),
            secondary: r"E:\a\main.rs".into(),
            kind: GaKind::File,
            path: r"E:\a\main.rs".into(),
            line: 0,
        };
        let b = GaRow {
            path: r"E:\b\main.rs".into(),
            ..a.clone()
        };
        let c = GaRow {
            title: "跳转到行 1".into(),
            kind: GaKind::Line,
            path: String::new(),
            line: 0,
            ..a.clone()
        };
        assert_ne!(row_key(&a), row_key(&b), "same name, different folder");
        assert_ne!(row_key(&a), row_key(&c), "different kind");
        assert_eq!(row_key(&a), row_key(&a.clone()));
    }

    /// The highlight mapper walks chars, not bytes: a CJK title's byte offsets
    /// must land on the right UTF-16 units.
    #[test]
    fn highlight_positions_map_onto_utf16_units() {
        let text = "测试abc";
        // byte offsets of 'a' (6), 'b' (7) -> u16 2,3
        let mut map = vec![0usize; text.len() + 1];
        let (mut b, mut u) = (0usize, 0usize);
        for ch in text.chars() {
            map[b] = u;
            b += ch.len_utf8();
            u += ch.len_utf16();
        }
        map[b] = u;
        assert_eq!(map[6], 2);
        assert_eq!(map[7], 3);
        assert_eq!(map[8], 4);
    }

    /// It follows the frame.
    #[test]
    fn placement_centres_the_dialog_in_the_frame_client() {
        // frame (144,147) 1105x1021 -> client (153,210) 1087x949 -> dlg (403,240)
        assert_eq!(placement((153, 210, 1087), 586, 30), (403, 240));
        // same dialog after the frame moved to (120,90) 1000x800:
        // client (129,153) 982x728 -> dlg (327,183)
        assert_eq!(placement((129, 153, 982), 586, 30), (327, 183));
        // a frame narrower than the dialog keeps the left edge in the client
        assert_eq!(placement((100, 100, 400), 586, 30), (100, 130));
    }
}
