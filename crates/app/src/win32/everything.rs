//! Everything (voidtools) as the file source for Goto Anything.
//!
//! Everything keeps the whole NTFS MFT in an index and answers "which files
//! under this folder match this text" in a few milliseconds, where our own
//! walker needs 300–430 ms on a twenty-thousand-file tree. When it is running
//! we do not build a list at all: the box's text goes to Everything on every
//! keystroke and only the page that will be shown comes back. When it is not,
//! the caller falls back to [`dg_core::fileindex::walk_tree`] and the
//! directory-mtime revalidation that goes with it.
//!
//! Asking for the *whole* folder and filtering locally is what this used to do,
//! and it does not scale: `C:\Users\admin` is 116 159 files, 110 MB of reply,
//! and 5.1 s of parsing (9.2 s in a debug build) — for a dialog that shows a
//! hundred rows. The reply is a page (`max_results`), not a snapshot, so the
//! cost is bounded by [`crate::win32::gotoany::LIMIT`] rather than by the tree.
//!
//! The protocol is the documented WM_COPYDATA one (`Everything-SDK.zip`,
//! `ipc/everything_ipc.h`), not the SDK DLL — the DLL is not installed by
//! Everything, so depending on it would mean asking users to copy a file. The
//! wire format is worth spelling out because it is `#pragma pack(1)` and every
//! offset is a **byte** offset from the start of the list:
//!
//! ```text
//! query (EVERYTHING_IPC_QUERYW, sent to Everything):
//! u32 reply_hwnd, reply_copydata_message, search_flags, offset, max_results
//! u16 search_string[]   // UTF-16, NUL terminated
//!
//! reply (EVERYTHING_IPC_LISTW, sent back to reply_hwnd):
//! u32 totfolders, totfiles, totitems, numfolders, numfiles, numitems, offset
//! item[numitems] { u32 flags, filename_offset, path_offset }   // 12 bytes each
//!... strings, addressed by the two offsets, UTF-16 NUL terminated
//! ```
//!
//! `EVERYTHING_IPC_COPYDATAQUERYW` (2) as the `dwData` selects the Unicode
//! variant, so the reply's strings are UTF-16 too.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, HWND_MESSAGE,
    MSG, PM_REMOVE, PeekMessageW, RegisterClassW, TranslateMessage, WM_COPYDATA, WNDCLASSW,
    WS_POPUP,
};
use windows::core::w;

use dg_core::fileindex::FileEntry;

/// `EVERYTHING_IPC_WNDCLASS` — the window Everything registers while it runs.
const IPC_WNDCLASS: windows::core::PCWSTR = w!("EVERYTHING_TASKBAR_NOTIFICATION");
/// `EVERYTHING_IPC_COPYDATAQUERYW`: the Unicode query. Also our reply id, so the
/// answer is distinguishable from any other WM_COPYDATA that reaches the
/// message-only window (nothing else can, but the id has to be ours).
const COPYDATA_QUERY_W: usize = 2;

/// Our receiver class. Registered once per process; the windows it creates are
/// message-only and live for exactly one query.
const RECEIVER_CLASS: windows::core::PCWSTR = w!("DogEditGotoAnythingIpc");

/// How long a query may take before we give up and let the caller walk the
/// tree. Everything answers in single-digit milliseconds; anything near this
/// bound means it is busy rebuilding its index.
const REPLY_TIMEOUT: Duration = Duration::from_millis(2_000);

/// `EVERYTHING_IPC_FOLDER` — an item with this flag is a directory.
const ITEM_FOLDER: u32 = 0x0000_0001;

/// The list header is seven DWORDs (`totfolders … offset`); items follow.
const LIST_HEADER: usize = 28;
/// `EVERYTHING_IPC_ITEMW` is three DWORDs, and the header is `pack(1)`.
const ITEM_SIZE: usize = 12;

thread_local! {
    /// The reply body, captured by [`receiver_proc`]. Thread-local because the
    /// window lives on the querying thread and only that thread pumps for it.
    static REPLY: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Why Everything did not answer the query.
///
/// The distinction matters to the user, not to us: "Everything is not running"
/// is the ordinary case and needs no explanation, but "Everything is running
/// and this implementation still walked the tree" is the one that looks like a bug — the
/// status line says so, with the reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Why {
    /// No `EVERYTHING_TASKBAR_NOTIFICATION` window: not installed, or not
    /// started yet.
    NotRunning,
    /// `DG_NO_EVERYTHING` asked for the walker.
    Forced,
    /// Everything answered with nothing: the folder is not in its index — a
    /// drive it does not index, a network share, an excluded path — or the
    /// whole tree sits under a directory this implementation skips.
    Empty,
    /// Everything refused the query (`SendMessageW` returned FALSE).
    Refused,
    /// No reply within [`REPLY_TIMEOUT`]; it is probably rebuilding its index.
    Timeout,
}

impl Why {
    /// The word the status line adds when Everything was up but did not help.
    /// `None` for the two cases the user already understands. Kept short: the
    /// status static is 370 px and the count and the cap note have to fit too.
    pub(super) fn note(self) -> Option<&'static str> {
        match self {
            Why::NotRunning | Why::Forced => None,
            Why::Empty => Some("Everything 无结果"),
            Why::Refused => Some("Everything 拒绝"),
            Why::Timeout => Some("Everything 超时"),
        }
    }
}

/// What Everything answered: the page of files to show, plus how many matched
/// in total. The header carries `totitems` even when the page is truncated,
/// which is how the status line can say "116 159 个文件" without ever having
/// received them.
pub(super) struct Answer {
    pub entries: Vec<FileEntry>,
    pub total: u32,
}

/// Whether Everything is up, without asking it anything. The dialog asks this
/// once per open, to decide between the live path and the walker.
pub(super) fn available() -> bool {
    if std::env::var_os("DG_NO_EVERYTHING").is_some() {
        return false;
    }
    unsafe { FindWindowW(IPC_WNDCLASS, None).is_ok() }
}

/// Asks Everything for the files under `root` whose name matches `pattern`,
/// returning at most `limit` of them plus the total number of matches.
///
/// `Err` means "use the walker instead", with the reason: Everything is not
/// running, refused the query, or timed out. `Why::Empty` is only reported for
/// the empty pattern — that query is the "is this folder in the index at all"
/// probe, so no answer means a drive Everything does not index (a network
/// share, an excluded path) and the walker is the only way to fill the box. A
/// *pattern* that matches nothing is a real answer, not a failure, and comes
/// back as `Ok` with an empty page.
///
/// `DG_NO_EVERYTHING=1` forces the walker even when Everything is up. That is
/// what the probes use to exercise the local-scan path on a machine that has
/// Everything installed, and it is the switch to reach for when a user reports
/// that Everything's answer looks wrong.
pub(super) fn search(root: &str, pattern: &str, limit: usize) -> Result<Answer, Why> {
    if std::env::var_os("DG_NO_EVERYTHING").is_some() {
        return Err(Why::Forced);
    }
    unsafe { search_inner(root, pattern, limit) }
}

/// Timing line for `DG_GA_TRACE` — the same switch `gotoany` uses. Everything
/// is supposed to answer in single-digit milliseconds, so when the dialog still
/// feels slow the phase that ate the time *is* the diagnosis: the query, its
/// reply, or our own parse of it.
fn trace(f: impl FnOnce() -> String) {
    if std::env::var_os("DG_GA_TRACE").is_some() {
        eprintln!("{}", f());
    }
}

unsafe fn search_inner(root: &str, pattern: &str, limit: usize) -> Result<Answer, Why> {
    let start = Instant::now();
    // Microseconds at each boundary: found the IPC window, query sent,
    // Everything answered, page parsed, junk filtered out.
    let mut at = [0u128; 5];
    let everything = FindWindowW(IPC_WNDCLASS, None).map_err(|_| Why::NotRunning)?;
    let receiver = create_receiver().ok_or(Why::Refused)?;
    at[0] = start.elapsed().as_micros();
    let mut bytes = 0;
    let mut items = 0;
    let mut header = [0u32; 7];
    let result = (|| {
        let search = search_string(root, pattern);
        let mut query = build_query(receiver, &search, limit);
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_QUERY_W,
            cbData: query.len() as u32,
            lpData: query.as_mut_ptr() as *mut core::ffi::c_void,
        };
        // Everything returns TRUE when it accepted the query; the results come
        // back as a WM_COPYDATA to `receiver`, which only arrives once this
        // thread pumps.
        let accepted = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            everything,
            WM_COPYDATA,
            Some(WPARAM(receiver.0 as usize)),
            Some(LPARAM(&cds as *const _ as isize)),
        );
        if accepted.0 == 0 {
            return Err(Why::Refused);
        }
        at[1] = start.elapsed().as_micros();
        let body = wait_for_reply().ok_or(Why::Timeout)?;
        at[2] = start.elapsed().as_micros();
        bytes = body.len();
        for (i, slot) in header.iter_mut().enumerate() {
            *slot = read_u32(&body, i * 4).unwrap_or(0);
        }
        let total = total_of(&body);
        let mut entries = parse_list(&body);
        at[3] = start.elapsed().as_micros();
        entries.retain(|e| !is_skipped_path(&e.path, root));
        at[4] = start.elapsed().as_micros();
        items = entries.len();
        // Only the "is this folder indexed" probe treats an empty answer as a
        // failure. A pattern that matches nothing is an answer.
        if total == 0 && pattern.trim().is_empty() {
            return Err(Why::Empty);
        }
        Ok(Answer { entries, total })
    })();
    trace(|| {
        let us = |i: usize| at[i] - if i == 0 { 0 } else { at[i - 1] };
        format!(
            "ga:everything root={root} pattern={pattern:?} limit={limit} find={}us send={}us \
             wait={}us parse={}us retain={}us items={items} bytes={bytes} hdr={header:?} -> {}",
            at[0],
            us(1),
            us(2),
            us(3),
            us(4),
            match &result {
                Ok(a) => format!("ok total={}", a.total),
                Err(Why::NotRunning) => "not-running".to_string(),
                Err(Why::Forced) => "forced".to_string(),
                Err(Why::Empty) => "empty".to_string(),
                Err(Why::Refused) => "refused".to_string(),
                Err(Why::Timeout) => "timeout".to_string(),
            }
        )
    });
    let _ = DestroyWindow(receiver);
    result
}

/// Everything's syntax for "everything under this folder, minus the build and
/// dependency trees": the path quoted (so spaces and commas survive) with a
/// trailing backslash, then one `!term` per skip directory.
///
/// Everything has no idea about [`dg_core::fileindex::skip_dirs`], and without
/// this a `target\` swallows the entire result budget before a single real
/// source file comes back — measured on this repository: 15 180 files with
/// Everything's own idea of the tree against 398 that the walker indexes.
///
/// A `!term` matches the item's **whole path**, so it also matches a root that
/// itself lives in one of those directories: opened from `…\target\debug` (which
/// is what an untitled buffer gets, its root being the working directory) the
/// term `!\target\` excludes every single result and Everything answers with
/// nothing. The root's own path is the user's choice — the same rule
/// [`is_skipped_path`] applies, only what sits *below* the root is judged — so
/// a skip directory the root already lives in gets no term. The client-side
/// filter still drops a `target\` nested *inside* such a root.
fn folder_query(root: &str) -> String {
    let trimmed = root.trim_end_matches(['\\', '/']);
    let base = if trimmed.is_empty() { root } else { trimmed };
    let mut q = format!("\"{base}\\\"");
    for dir in dg_core::fileindex::skip_dirs() {
        let inside = base
            .split(['\\', '/'])
            .any(|seg| seg.eq_ignore_ascii_case(dir));
        if inside {
            continue;
        }
        q.push_str(&format!(" !\\{dir}\\"));
    }
    q
}

/// Everything's one-term spelling of "nothing whose path has a dot-segment":
/// a backslash, a dot, anything, a backslash.
///
/// The walker hides everything under a dot-name; Everything has no such rule,
/// and the page is only a few hundred rows, so a handful of `.cargo\` files can
/// fill it and leave the dialog with nothing to show — measured on
/// `C:\Users\admin`, where the first 400 files matching `m` are all inside
/// dot-directories and the client-side filter then drops every one of them.
///
/// This used to be one `!\.name\` term per dot-directory found under the root,
/// which is correct and *slow*: a NOT term costs about 8–10 ns per file under
/// the root, and `C:\Users\admin` has 51 dot-directories — 50 terms against
/// 260 034 matches came to 130 ms of the query. One wildcard term says the same
/// thing in a single pass (measured, same pattern `mai`, same reply):
///
/// | terms | `wait` | `total` |
/// |---|---|---|
/// | 11 skip + 50 dot | 166–173 ms | 669 |
/// | 11 skip + this one | **56–64 ms** | 669 |
///
/// Dropping the exclusions altogether is not an option, and that is measured
/// too: `"C:\Users\admin\" file: "m"` answers in 25.8 ms but every one of its
/// 400 items is inside a dot-directory, so the client filter leaves the dialog
/// with zero rows. The skip terms are worth keeping for a second reason as
/// well: they prune `node_modules`/`target` before this term runs, which is why
/// this one alone costs 148–165 ms and paired with them only 56–64 ms.
const DOT_TERM: &str = " !\\.*\\";

/// The whole search: the folder, the extra NOT terms, the user's text, and the
/// skip terms. `file:` is not decoration. The reply is a *page*, and Everything's
/// order puts folders in it too: `C:\Users\admin` + `m` answered with a 400-item
/// page that was 400 folders and no files at all (`hdr=[11282, 190704, 201986,
/// 400, 0, 400, 0]` — the first three counts are the folder/file/item totals, the
/// next three the page's), which the dialog then rendered as an empty list. The
/// filter is Everything's own search function, so it costs it a predicate, not a
/// round trip; it also makes the count in the header a *file* count. A substring
/// of the name, not a set of ANDed words (`main rs` is one phrase, not two terms)
/// and not an operator soup (`!`, `|`, `<`, `>` are ordinary characters inside
/// quotes). The one character that cannot be quoted is `"` itself, which would
/// end the phrase early, so it is dropped. A backslash keeps its meaning on
/// purpose: `src\main` is a path term, which is exactly what someone typing it
/// means.
fn search_string(root: &str, pattern: &str) -> String {
    let mut q = folder_query(root);
    q.push_str(DOT_TERM);
    q.push_str(" file:");
    let cleaned: String = pattern.chars().filter(|c| *c != '"').collect();
    let cleaned = cleaned.trim();
    if !cleaned.is_empty() {
        q.push_str(&format!(" \"{cleaned}\""));
    }
    q
}

/// The same rules the walker applies, applied to Everything's answer: nothing
/// under a skip directory, and nothing whose name starts with a dot.
///
/// Since the query asks for [`ALL_RESULTS`] this is no longer about a budget —
/// it is about **parity**: the walker never lists a `target\` file, so the list
/// the user sees must not either, whichever source filled it. The NOT terms keep
/// that junk off the wire in the first place; this filter is what catches what
/// they cannot express (the hidden dot-names) and the nested case a root-inside-
/// `target` query deliberately stops excluding.
fn is_skipped_path(path: &str, root: &str) -> bool {
    // Only what sits *below* the root counts: a project that happens to live in
    // a folder called `target` is the user's business.
    let rel = if path.len() > root.len() && path[..root.len()].eq_ignore_ascii_case(root) {
        &path[root.len()..]
    } else {
        return false;
    };
    let rel = rel.trim_start_matches(['\\', '/']);
    // The file name counts for the hidden rule, the directories for both.
    let mut segs = rel.split(['\\', '/']);
    let Some(name) = segs.next_back() else {
        return false;
    };
    if name.starts_with('.') {
        return true;
    }
    segs.any(|seg| {
        seg.starts_with('.')
            || dg_core::fileindex::skip_dirs()
                .iter()
                .any(|s| s.eq_ignore_ascii_case(seg))
    })
}

/// The packed query struct plus the NUL-terminated UTF-16 search string.
///
/// `max_results` is the page size the caller asked for. It is not a budget in
/// the walker's sense — the caller re-asks on the next keystroke — it is what
/// keeps Everything from serialising a 110 MB list to answer a 100-row
/// question. The offset stays 0 because the dialog only ever shows the first
/// page; there is no paging UI to feed.
unsafe fn build_query(receiver: HWND, search: &str, max_results: usize) -> Vec<u8> {
    let text: Vec<u16> = search.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = Vec::with_capacity(20 + text.len() * 2);
    for field in [
        receiver.0 as u32,
        COPYDATA_QUERY_W as u32,
        0, // search_flags: no match-case, no regex, no match-path
        0, // offset
        max_results as u32,
    ] {
        buf.extend_from_slice(&field.to_le_bytes());
    }
    for unit in text {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    buf
}

/// Pumps this thread's queue until the reply lands or [`REPLY_TIMEOUT`] runs
/// out. The reply is a *sent* message, so it is dispatched to
/// [`receiver_proc`] during the `PeekMessageW` call rather than returned.
unsafe fn wait_for_reply() -> Option<Vec<u8>> {
    REPLY.with(|r| *r.borrow_mut() = None);
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut msg = MSG::default();
    loop {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(body) = REPLY.with(|r| r.borrow_mut().take()) {
            return Some(body);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The message-only window the reply is addressed to. A `STATIC` control is
/// enough — the window exists only to own a WNDPROC.
unsafe fn create_receiver() -> Option<HWND> {
    let module = GetModuleHandleW(None).ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(receiver_proc),
        hInstance: module.into(),
        lpszClassName: RECEIVER_CLASS,
        ..Default::default()
    };
    // A second call for the same class fails harmlessly (ERROR_CLASS_ALREADY_EXISTS);
    // the class outlives every query, so it is never unregistered.
    RegisterClassW(&class);
    CreateWindowExW(
        Default::default(),
        RECEIVER_CLASS,
        w!(""),
        WS_POPUP,
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(module.into()),
        None,
    )
    .ok()
}

unsafe extern "system" fn receiver_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COPYDATA && lparam.0 != 0 {
        let cds = &*(lparam.0 as *const COPYDATASTRUCT);
        if cds.dwData == COPYDATA_QUERY_W && !cds.lpData.is_null() {
            // The buffer belongs to the sender and dies when we return, so the
            // copy has to happen here.
            let bytes =
                std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize).to_vec();
            REPLY.with(|r| *r.borrow_mut() = Some(bytes));
            return LRESULT(1);
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Parses an `EVERYTHING_IPC_LISTW` body into file entries, dropping
/// directories and any item whose offsets fall outside the buffer. Pure, so the
/// wire format is testable without Everything installed.
fn parse_list(buf: &[u8]) -> Vec<FileEntry> {
    let numitems = read_u32(buf, 20).unwrap_or(0) as usize;
    let mut out = Vec::new();
    for i in 0..numitems {
        let base = LIST_HEADER + i * ITEM_SIZE;
        let (Some(flags), Some(name_at), Some(path_at)) = (
            read_u32(buf, base),
            read_u32(buf, base + 4),
            read_u32(buf, base + 8),
        ) else {
            break;
        };
        if flags & ITEM_FOLDER != 0 {
            continue;
        }
        let (Some(name), Some(dir)) = (
            read_utf16(buf, name_at as usize),
            read_utf16(buf, path_at as usize),
        ) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // Everything reports the folder without a trailing separator, except at
        // a drive root, where it keeps the backslash.
        let path = if dir.ends_with('\\') || dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}\\{name}")
        };
        out.push(FileEntry { name, path });
    }
    out
}

/// `totitems` — every match Everything found, not just the ones it sent. The
/// header reports the whole set even when `max_results` cut the page short.
fn total_of(buf: &[u8]) -> u32 {
    read_u32(buf, 8).unwrap_or(0)
}

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// A NUL-terminated UTF-16LE string at a byte offset, bounded by the buffer.
fn read_utf16(buf: &[u8], at: usize) -> Option<String> {
    if at >= buf.len() {
        return None;
    }
    let mut units: Vec<u16> = Vec::new();
    let mut i = at;
    while i + 1 < buf.len() {
        let unit = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
        i += 2;
    }
    Some(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an `EVERYTHING_IPC_LISTW` the way Everything does: header, then
    /// the item array, then the strings the offsets point at.
    fn build_list(items: &[(u32, &str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        // totfolders, totfiles, totitems, numfolders, numfiles, numitems, offset
        let folders = items
            .iter()
            .filter(|(f, _, _)| f & ITEM_FOLDER != 0)
            .count();
        for v in [
            folders as u32,
            (items.len() - folders) as u32,
            items.len() as u32,
            folders as u32,
            (items.len() - folders) as u32,
            items.len() as u32,
            0,
        ] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        let items_at = buf.len();
        let strings_at = items_at + items.len() * ITEM_SIZE;
        let mut strings: Vec<u8> = Vec::new();
        let mut offsets = Vec::new();
        for (_, name, dir) in items {
            let name_at = strings_at + strings.len();
            for unit in name.encode_utf16().chain(std::iter::once(0)) {
                strings.extend_from_slice(&unit.to_le_bytes());
            }
            let dir_at = strings_at + strings.len();
            for unit in dir.encode_utf16().chain(std::iter::once(0)) {
                strings.extend_from_slice(&unit.to_le_bytes());
            }
            offsets.push((name_at, dir_at));
        }
        for ((flags, _, _), (name_at, dir_at)) in items.iter().zip(offsets) {
            buf.extend_from_slice(&flags.to_le_bytes());
            buf.extend_from_slice(&(name_at as u32).to_le_bytes());
            buf.extend_from_slice(&(dir_at as u32).to_le_bytes());
        }
        buf.extend_from_slice(&strings);
        buf
    }

    #[test]
    fn parse_list_reads_names_paths_and_skips_folders() {
        let buf = build_list(&[
            (0, "main.rs", r"E:\proj\src"),
            (ITEM_FOLDER, "src", r"E:\proj"),
            (0, "lib.rs", r"E:\proj\src"),
        ]);
        let got = parse_list(&buf);
        assert_eq!(got.len(), 2, "the folder item is dropped");
        assert_eq!(got[0].name, "main.rs");
        assert_eq!(got[0].path, r"E:\proj\src\main.rs");
        assert_eq!(got[1].name, "lib.rs");
    }

    /// A drive root arrives with its backslash already on it; do not double it.
    #[test]
    fn parse_list_does_not_double_a_root_separator() {
        let buf = build_list(&[(0, "boot.ini", r"C:\")]);
        assert_eq!(parse_list(&buf)[0].path, r"C:\boot.ini");
    }

    /// Truncated or hostile input stops at the buffer edge instead of reading
    /// past it.
    #[test]
    fn parse_list_survives_a_truncated_buffer() {
        let full = build_list(&[(0, "a.txt", r"E:\x")]);
        assert_eq!(parse_list(&full).len(), 1);
        for cut in [0usize, 4, 27, 28, 30, full.len() - 3] {
            let _ = parse_list(&full[..cut.min(full.len())]);
        }
        // A bogus item count must not index past the end.
        let mut lying = full.clone();
        lying[20..24].copy_from_slice(&9_999u32.to_le_bytes());
        assert_eq!(parse_list(&lying).len(), 1);
    }

    /// The query is `pack(1)`: five DWORDs, then the UTF-16 string. The result
    /// budget is the page the caller asked for — Everything serialises only
    /// that many, which is what keeps a 116 000-file folder off the wire.
    #[test]
    fn build_query_matches_the_packed_layout() {
        let fake = HWND(0x1234 as *mut core::ffi::c_void);
        let q = unsafe { build_query(fake, "abc", 400) };
        assert_eq!(q.len(), 20 + 4 * 2, "5 dwords + 3 chars + NUL");
        assert_eq!(read_u32(&q, 0), Some(0x1234));
        assert_eq!(read_u32(&q, 4), Some(COPYDATA_QUERY_W as u32));
        assert_eq!(read_u32(&q, 16), Some(400), "the page size");
        assert_eq!(read_utf16(&q, 20).as_deref(), Some("abc"));
    }

    /// The page is truncated but the header still counts the whole set: that
    /// is the number the status line shows.
    #[test]
    fn total_of_reads_the_whole_match_count_not_the_page() {
        let mut buf = build_list(&[(0, "a.txt", r"E:\x"), (0, "b.txt", r"E:\x")]);
        // totitems sits at byte 8; claim 116 159 matches for a two-item page.
        buf[8..12].copy_from_slice(&116_159u32.to_le_bytes());
        assert_eq!(total_of(&buf), 116_159);
        assert_eq!(parse_list(&buf).len(), 2);
        assert_eq!(total_of(&[]), 0, "a truncated reply is a zero, not a panic");
    }

    /// The pattern is a quoted phrase: spaces stay inside it, operators stop
    /// being operators, and a `"` — the one character quoting cannot hold — is
    /// dropped rather than allowed to end the phrase early.
    #[test]
    fn search_string_quotes_the_pattern_and_drops_stray_quotes() {
        let q = search_string(r"E:\proj", "main rs");
        assert!(q.starts_with(r#""E:\proj\""#), "{q}");
        assert!(q.contains(r#""main rs""#), "one phrase, not two terms: {q}");
        assert!(q.contains(r" !\target\"), "{q}");
        assert!(!search_string(r"E:\proj", r#"a"b"#).contains(r#""a"b""#));
        assert!(search_string(r"E:\proj", r#"a"b"#).contains(r#""ab""#));
        // A backslash in the pattern stays a path term: that is what it means.
        assert!(search_string(r"E:\proj", r"src\main").contains(r#""src\main""#));
        // A blank pattern adds no term at all — that query is the folder probe.
        let blank = search_string(r"E:\proj", "   ");
        assert!(!blank.contains("\"\""), "{blank}");
        assert_eq!(
            blank,
            format!("{}{DOT_TERM} file:", folder_query(r"E:\proj"))
        );
        // The dot-directory exclusion lands before `file:`, as one wildcard term.
        let with = search_string(r"E:\proj", "main");
        assert!(with.contains(r"!\.*\ file:"), "{with}");
    }

    /// Every query asks for files only. Without `file:` the page is filled with
    /// folders — Everything sorts them first, and `parse_list` drops them, so a
    /// 400-item page can parse to nothing (measured on `C:\Users\admin`).
    #[test]
    fn search_string_always_asks_for_files_not_folders() {
        for pattern in ["", "m", "main.rs", "@sym"] {
            let q = search_string(r"E:\proj", pattern);
            assert!(q.contains(" file:"), "{pattern:?} -> {q}");
        }
    }

    /// One wildcard term replaces a `!\.name\` per dot-directory. Measured on
    /// `C:\Users\admin`: same reply (`total=669` for `mai`), 166–173 ms down to
    /// 56–64 ms, because a NOT term costs ~8–10 ns per file under the root and
    /// that root has 51 dot-directories. It says exactly the same thing as the
    /// long form — "a backslash, a dot, anything, a backslash" — and unlike
    /// `regex:` it needs nothing beyond wildcard support.
    #[test]
    fn the_dot_exclusion_is_one_wildcard_term_not_a_list_of_directories() {
        assert_eq!(DOT_TERM, " !\\.*\\", "one term, one backslash-dot segment");
        assert_eq!(DOT_TERM.matches('!').count(), 1, "a single NOT term");
        // The dot-segment rule, spelled out: what it must and must not match.
        let excluded = [
            r"C:\Users\admin\.cargo\registry\cache\a.crate",
            r"C:\Users\admin\.rustup\toolchains\stable\bin\rustc.exe",
            r"E:\proj\src\.git\config",
        ];
        let kept = [
            r"C:\Users\admin\Documents\notes.txt",
            r"E:\proj\src\main.rs",
            r"E:\proj\my.folder\a.txt",   // a dot in a name is not a dot-segment
            r"C:\Users\admin\.gitignore", // a dot-*file*: the client filter's job
        ];
        // The term is `\` `.` `*` `\`: a dot-segment is a backslash, a dot, and
        // another backslash somewhere after it. A dot-*file* has no second
        // separator, so the term leaves it alone and the client filter (which
        // hides every dot-name) deals with it — same as before.
        let hits = |p: &str| {
            let b = p.as_bytes();
            (0..b.len()).any(|i| {
                b[i] == b'\\' && b.get(i + 1) == Some(&b'.') && b[i + 2..].contains(&b'\\')
            })
        };
        assert!(excluded.iter().all(|p| hits(p)), "{excluded:?}");
        assert!(!kept.iter().any(|p| hits(p)), "{kept:?}");
    }

    /// Everything's syntax for "everything under this folder, minus the build and
    /// dependency trees": the path quoted (so spaces and commas survive) with a
    /// trailing backslash, then one `!term` per skip directory.
    #[test]
    fn folder_query_quotes_appends_a_separator_and_excludes_junk() {
        let q = folder_query(r"C:\projects\dogedit");
        assert!(q.starts_with(r#""C:\projects\dogedit\""#), "{q}");
        assert!(q.contains(r" !\node_modules\"), "{q}");
        assert!(q.contains(r" !\target\"), "{q}");
        assert!(q.contains(r" !\.git\"), "{q}");
        // Trailing separators are normalised, not doubled.
        assert!(folder_query(r"C:\projects\dogedit\").starts_with(r#""C:\projects\dogedit\""#));
        assert!(folder_query(r"C:\").starts_with(r#""C:\""#));
        assert!(folder_query(r"E:\my docs").starts_with(r#""E:\my docs\""#));
    }

    /// A `!term` matches the whole path, so a root that lives inside one of the
    /// skip directories would exclude *everything* and Everything would answer
    /// with nothing — the untitled-buffer-from-`target\debug` case. Only the
    /// terms for directories the root does not already sit in may appear.
    #[test]
    fn folder_query_keeps_a_root_that_lives_in_a_skip_dir_indexable() {
        let q = folder_query(r"C:\projects\dogedit\dogedit\target\debug");
        assert!(!q.contains(r"!\target\"), "would exclude the root itself: {q}");
        assert!(q.contains(r"!\node_modules\"), "the others still apply: {q}");
        assert!(q.contains(r"!\.git\"), "{q}");
        // Same for the other skip names, and case-insensitively.
        assert!(!folder_query(r"E:\w\Node_Modules\pkg").contains(r"!\node_modules\"));
        assert!(!folder_query(r"E:\repo\.git\hooks").contains(r"!\.git\"));
        // A root that merely *contains* such a directory below still gets the
        // term: that is exactly the case the exclusion is for.
        assert!(folder_query(r"C:\projects\dogedit\dogedit").contains(r"!\target\"));
        assert!(folder_query(r"C:\projects\dogedit").contains(r"!\target\"));
    }

    /// The client-side half of the walker's rules: only what sits below the root
    /// is judged, only directories are matched against the skip list, and a
    /// dot-name is hidden either way.
    #[test]
    fn is_skipped_path_judges_below_the_root_only() {
        let root = r"E:\proj";
        assert!(is_skipped_path(r"E:\proj\target\debug\a.txt", root));
        assert!(is_skipped_path(r"E:\proj\src\node_modules\x\i.js", root));
        assert!(!is_skipped_path(r"E:\proj\src\main.rs", root));
        assert!(
            !is_skipped_path(r"E:\proj\target.rs", root),
            "a file, not a dir"
        );
        assert!(
            is_skipped_path(r"E:\proj\.github\ci.yml", root),
            "hidden dir"
        );
        assert!(
            is_skipped_path(r"E:\proj\src\.gitignore", root),
            "hidden file"
        );
        assert!(!is_skipped_path(r"E:\proj\src\gitignore", root));
        // The root itself may legitimately live inside a folder called `target`.
        let nested = r"E:\build\target\proj";
        assert!(!is_skipped_path(r"E:\build\target\proj\src\a.rs", nested));
        // Paths outside the root are not judged at all.
        assert!(!is_skipped_path(r"D:\elsewhere\target\a.txt", root));
    }
}
