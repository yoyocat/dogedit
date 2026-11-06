//! `Snapshot` — the data model behind `HexView`: the piece chain, page
//! table and undo machinery.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Files below this size load fully into memory; larger ones keep the
/// handle and page reads through it. The cut-off is 10 MiB.
pub const SMALL_FILE_LIMIT: u64 = 0xA0_0000;

/// Paged reads go at most 100 bytes at a time.
const PAGED_READ_CHUNK: u32 = 0x64;

pub const KIND_INSERT: u32 = 1;
pub const KIND_DELETE: u32 = 2;
pub const KIND_REPLACE: u32 = 3;

const HEAD: usize = 0;
const TAIL: usize = 1;

/// A page of bytes. Small files and the edit buffer are `Mem`; large files
/// keep the handle and read on demand.
enum Page {
    Mem { buf: Vec<u8> },
    File { file: File, size: u32 },
}

impl Page {
    fn read_span(&self, off: u32, len: u32, out: &mut [u8]) -> bool {
        match self {
            Page::Mem { buf } => {
                let s = off as usize;
                let e = s + len as usize;
                if e > buf.len() {
                    return false;
                }
                out[..len as usize].copy_from_slice(&buf[s..e]);
                true
            }
            Page::File { file, size } => {
                if off >= *size || len > *size - off || len as usize > out.len() {
                    return false;
                }
                let mut f = file;
                // Chunked: each request is at most PAGED_READ_CHUNK bytes.
                let mut done = 0u32;
                while done < len {
                    let want = (len - done).min(PAGED_READ_CHUNK) as usize;
                    if f
                        .seek(SeekFrom::Start((off + done) as u64))
                        .and_then(|_| f.read_exact(&mut out[done as usize..done as usize + want]))
                        .is_err()
                    {
                        return false;
                    }
                    done += want as u32;
                }
                true
            }
        }
    }
}

/// One piece: a node of the doubly linked chain, walked through `next`
/// from the head sentinel.
#[derive(Clone, Copy)]
struct Piece {
    next: usize,
    prev: usize,
    off: u32,
    len: u32,
    page: usize,
}

/// An undo marker: the edit's bytes plus the bookkeeping an undo step
/// needs. `is_delete_side` markers (the delete half of a replace,
/// or a plain delete) carry the bytes that left the chain together with
/// their per-byte modified flags, so an undo can bring them back on the
/// same kind of page they lived on; insert-side markers carry the bytes
/// that entered it (always edit-page bytes) at their edit-page offset —
/// the mid-number scratch write patches both the page and the marker in
/// place. Undo/redo splice the opposite direction;
/// `pos`/`len`/`kind`/`total_len`/`from_file`/`counter` are the length,
/// kind and grouping bookkeeping.
struct Marker {
    is_delete_side: bool,
    data: Vec<u8>,
    /// delete-side: per-byte "was on the edit page" flags. insert-side:
    /// unused.
    data_modified: Vec<bool>,
    /// insert-side: offset of `data` in the edit page. delete-side:
    /// unused.
    edit_off: u32,
    total_len: u32,
    pos: u32,
    len: u32,
    kind: u32,
    from_file: bool,
    counter: u64,
}

/// Which side of an edit a marker records: the bytes that entered the
/// chain (already on the edit page, at `edit_off`) or the bytes that left
/// it, each with the modified flag it carried.
enum MarkerSide {
    Insert { edit_off: u32 },
    Delete { data_modified: Vec<bool> },
}

/// The hex snapshot: the byte range a hex view pane shows.
pub struct Snapshot {
    pieces: Vec<Piece>,
    free: Vec<usize>,
    len: u32,
    pages: Vec<Page>,
    edit_page: Option<usize>,
    /// Undo-restored original bytes land here, not on the edit page, so
    /// they keep reading as unmodified — restored bytes simply
    /// re-enter the chain from their own (file) pages.
    restore_page: Option<usize>,
    file_page: Option<usize>,
    undo: Vec<Marker>,
    redo: Vec<Marker>,
    mod_a: u64,
    mod_b: u64,
    saved_pos: u32,
    saved_group: u32,
    pub path: Option<PathBuf>,
    from_file: bool,
    last_pos: u32,
    last_kind: u32,
    /// Set once the loaded file refuses writes.
    pub readonly: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshot {
    /// Sentinels and empty vectors. The chain starts empty
    /// (the sentinels are list nodes themselves).
    pub fn new() -> Self {
        let mut s = Self {
            pieces: Vec::with_capacity(16),
            free: Vec::new(),
            len: 0,
            pages: vec![Page::Mem { buf: Vec::new() }],
            edit_page: None,
            restore_page: None,
            file_page: None,
            undo: Vec::new(),
            redo: Vec::new(),
            mod_a: 0,
            mod_b: 0,
            saved_pos: 0,
            saved_group: 0,
            path: None,
            from_file: false,
            last_pos: 0,
            last_kind: 0,
            readonly: false,
        };
        s.pieces.push(Piece { next: TAIL, prev: TAIL, off: 0, len: 0, page: 0 });
        s.pieces.push(Piece { next: HEAD, prev: HEAD, off: 0, len: 0, page: 0 });
        s
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The clean-from-file flag: any edit clears it, save/load
    /// restores it. The host's "modified?" query is built on it.
    pub fn from_file(&self) -> bool {
        self.from_file
    }

    /// Restores the clean bit without a disk round-trip. The host path
    /// that needs it is the HEX模式 toggle: the bytes it sets come from
    /// the document's own saved buffer, so the snapshot equals its file
    /// exactly as a load would leave it.
    pub fn mark_clean(&mut self) {
        self.from_file = true;
    }

    // ---- piece plumbing -------------------------------------------------

    fn alloc_piece(&mut self, off: u32, len: u32, page: usize) -> usize {
        if let Some(i) = self.free.pop() {
            self.pieces[i] = Piece { next: usize::MAX, prev: usize::MAX, off, len, page };
            i
        } else {
            self.pieces.push(Piece { next: usize::MAX, prev: usize::MAX, off, len, page });
            self.pieces.len() - 1
        }
    }

    fn free_piece(&mut self, id: usize) {
        self.free.push(id);
    }

    fn link_after(&mut self, anchor: usize, id: usize) {
        let next = self.pieces[anchor].next;
        self.pieces[id].prev = anchor;
        self.pieces[id].next = next;
        self.pieces[next].prev = id;
        self.pieces[anchor].next = id;
    }

    fn unlink(&mut self, id: usize) {
        let prev = self.pieces[id].prev;
        let next = self.pieces[id].next;
        self.pieces[prev].next = next;
        self.pieces[next].prev = prev;
    }

    /// Walks to the piece containing `pos` (half-open): the walk tests
    /// `pos ∈ [cum, cum+len)`, falling through to the tail sentinel when
    /// `pos == len`.
    fn locate(&self, pos: u32) -> (usize, u32) {
        let mut cur = self.pieces[HEAD].next;
        let mut cum = 0u32;
        while cur != TAIL {
            let p = self.pieces[cur];
            if pos >= cum && pos < cum + p.len {
                return (cur, cum);
            }
            cum += p.len;
            cur = p.next;
        }
        (TAIL, cum)
    }

    // ---- pages -----------------------------------------------------------

    /// Appends `data` to the edit page, creating it on first use, or, for
    /// undo restores, the restore page. Returns `(page index, offset)`.
    fn append_page(&mut self, restore: bool, data: &[u8]) -> (usize, u32) {
        let slot = if restore { &mut self.restore_page } else { &mut self.edit_page };
        let idx = match *slot {
            Some(i) => i,
            None => {
                self.pages.push(Page::Mem { buf: Vec::new() });
                let i = self.pages.len() - 1;
                *slot = Some(i);
                i
            }
        };
        let off;
        if let Page::Mem { buf } = &mut self.pages[idx] {
            off = buf.len() as u32;
            buf.extend_from_slice(data);
        } else {
            unreachable!("append pages are always in memory");
        }
        (idx, off)
    }

    // ---- mod counters ----------------------------------------------------

    /// TouchMod — open or extend an undo group: when the open-run depth is
    /// zero a fresh group id is issued (never 0, wrap guard), then the
    /// depth counts up. Markers pushed while the depth is non-zero carry
    /// that id, and one undo step chains every marker sharing it.
    fn touch_mod(&mut self) {
        if self.mod_b == 0 {
            self.mod_a = self.mod_a.wrapping_add(1);
            if self.mod_a == 0 {
                self.mod_a = 1;
            }
        }
        self.mod_b += 1;
    }

    /// The open-run decrement the replace path makes exactly once per
    /// call, success or failure.
    fn untouch_mod(&mut self) {
        if self.mod_b != 0 {
            self.mod_b -= 1;
        }
    }

    /// Opens an undo group for a compound edit: the digit-typing selection
    /// path does TouchMod inline, deletes the selection, inserts the byte
    /// and closes with one decrement, so both markers share the group id.
    pub fn begin_group(&mut self) {
        self.touch_mod();
    }

    pub fn end_group(&mut self) {
        self.untouch_mod();
    }

    /// The counter stamped on markers: 0 while no run is open.
    fn current_counter(&self) -> u64 {
        if self.mod_b == 0 {
            0
        } else {
            self.mod_a
        }
    }

    // ---- markers ---------------------------------------------------------

    fn push_marker(&mut self, side: MarkerSide, data: Vec<u8>, pos: u32, len: u32, kind: u32) {
        let (is_delete_side, data_modified, edit_off) = match side {
            MarkerSide::Insert { edit_off } => (false, Vec::new(), edit_off),
            MarkerSide::Delete { data_modified } => (true, data_modified, 0),
        };
        self.undo.push(Marker {
            is_delete_side,
            data,
            data_modified,
            edit_off,
            total_len: self.len,
            pos,
            len,
            kind,
            from_file: self.from_file,
            counter: self.current_counter(),
        });
    }

    /// ClearRedo: the redo markers die with their bytes.
    fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// The inverse of a marker: a delete-side marker restores its bytes,
    /// an insert-side marker splices its range back out. The forward
    /// direction is the redo mirror.
    fn apply_marker(&mut self, mi: &mut Marker, undoing: bool) {
        let delete_side = mi.is_delete_side;
        // remove range when: redo of a delete, or undo of an insert.
        let undoing_op = delete_side != undoing;
        // The length of the state being left, for the mirror direction.
        let leaving_len = self.len;
        if undoing_op {
            // remove range: redo of a delete, or undo of an insert
            if mi.len > 0 && mi.pos <= self.len && mi.len <= self.len - mi.pos {
                self.splice_out(mi.pos, mi.len);
            }
        } else if delete_side {
            // insert the original bytes back, run-split by their captured
            // modified flags so each run returns to the kind of page it
            // came from
            let mut at = mi.pos;
            let mut i = 0usize;
            while i < mi.data.len() && at <= self.len {
                let flag = mi.data_modified[i];
                let j = i + 1 + mi.data_modified[i + 1..].iter().take_while(|f| **f == flag).count();
                self.splice_in(at, &mi.data[i..j], !flag);
                at += (j - i) as u32;
                i = j;
            }
        } else {
            // insert-side: the edit's own bytes come back on the edit page
            if !mi.data.is_empty() && mi.pos <= self.len {
                let (_, off) = self.splice_in(mi.pos, &mi.data, false);
                mi.edit_off = off;
            }
        }
        // The splices keep `len` current for the new state; the marker
        // keeps the length (and the clean-file flag, which the splices
        // never touch, so a swap suffices) of the state on the other side.
        mi.total_len = leaving_len;
        std::mem::swap(&mut self.from_file, &mut mi.from_file);
        // The saved position takes `pos`; the saved group length takes `len`
        // only for the direction/kind combination the undo step selects.
        self.saved_pos = mi.pos;
        if (mi.kind == KIND_DELETE && undoing) || (mi.kind != KIND_DELETE && !undoing) {
            self.saved_group = mi.len;
        } else {
            self.saved_group = 0;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// One undo step: pop a marker, apply, move it to redo, and keep
    /// going while the next marker carries the same counter; one
    /// keystroke run is one undo step. Returns `(saved_pos, saved_pos +
    /// saved_group)`, the values the view copies into its group anchors
    /// and its caret.
    pub fn undo(&mut self) -> Option<(u32, u32)> {
        if self.undo.is_empty() {
            return None;
        }
        loop {
            let counter = self.undo.last().map(|m| m.counter);
            let mut m = self.undo.pop().unwrap();
            self.apply_marker(&mut m, true);
            self.redo.push(m);
            match (self.undo.last(), counter) {
                (Some(next), Some(c)) if next.counter == c => continue,
                _ => break,
            }
        }
        Some((self.saved_pos, self.saved_pos + self.saved_group))
    }

    /// The redo mirror: markers travel back to the undo vector with the
    /// other direction flag.
    pub fn redo(&mut self) -> Option<(u32, u32)> {
        if self.redo.is_empty() {
            return None;
        }
        loop {
            let counter = self.redo.last().map(|m| m.counter);
            let mut m = self.redo.pop().unwrap();
            self.apply_marker(&mut m, false);
            self.undo.push(m);
            match (self.redo.last(), counter) {
                (Some(next), Some(c)) if next.counter == c => continue,
                _ => break,
            }
        }
        Some((self.saved_pos, self.saved_pos + self.saved_group))
    }

    // ---- chain splices (the edits and the marker inverses) -----------------

    /// Removes `[pos, pos+len)` from the chain: split at the ends and
    /// walk the covered pieces. No marker, no counters: callers bookkeep.
    fn splice_out(&mut self, pos: u32, len: u32) {
        if len == 0 || len > self.len || pos > self.len - len {
            return;
        }
        let (mut at, mut cum) = self.locate(pos);
        if at == TAIL {
            return;
        }
        if self.pieces[at].len > 0 && pos - cum == self.pieces[at].len {
            cum += self.pieces[at].len;
            at = self.pieces[at].next;
        }
        if at == TAIL {
            return;
        }
        // First split.
        let off = pos - cum;
        if off != 0 {
            let orig = self.pieces[at];
            let left = self.alloc_piece(orig.off, off, orig.page);
            self.link_after(self.pieces[at].prev, left);
            self.pieces[at].off += off;
            self.pieces[at].len -= off;
        }
        let mut remaining = len;
        while remaining > 0 && at != TAIL {
            let p = self.pieces[at];
            if p.len > remaining {
                let right = self.alloc_piece(p.off + remaining, p.len - remaining, p.page);
                self.link_after(at, right);
                self.pieces[at].len = remaining;
            }
            let next = self.pieces[at].next;
            let dead = self.pieces[at].len;
            self.unlink(at);
            self.free_piece(at);
            self.len -= dead;
            remaining -= dead;
            at = next;
        }
    }

    /// Splices `data` in at `pos` as one piece on an appended page. No
    /// marker, no counters. Returns `(page, offset)`.
    fn splice_in(&mut self, pos: u32, data: &[u8], restore: bool) -> (usize, u32) {
        let (page_idx, edit_off) = self.append_page(restore, data);
        let n = data.len() as u32;
        let new_p = self.alloc_piece(edit_off, n, page_idx);
        let (p, cum) = self.locate(pos);
        let off_in_piece = pos - cum;
        if off_in_piece != 0 {
            // Split: left {off, k} + new + right {off+k, len-k}; both
            // halves are fresh pieces and the piece being split drops out.
            let orig = self.pieces[p];
            let left = self.alloc_piece(orig.off, off_in_piece, orig.page);
            let right = self.alloc_piece(orig.off + off_in_piece, orig.len - off_in_piece, orig.page);
            self.link_after(p, left);
            self.link_after(left, new_p);
            self.link_after(new_p, right);
            self.unlink(p);
            self.free_piece(p);
        } else {
            self.link_after(self.pieces[p].prev, new_p);
        }
        self.len += n;
        (page_idx, edit_off)
    }

    // ---- reads -----------------------------------------------------------

    /// Reads up to `buf.len()` bytes at `pos`, returning the byte count.
    /// Collects the spans the range covers, then copies them out piece by
    /// piece.
    pub fn read(&self, pos: u32, buf: &mut [u8]) -> usize {
        if buf.is_empty() || pos >= self.len {
            return 0;
        }
        let avail = (self.len - pos) as usize;
        let want_total = buf.len().min(avail);
        let mut done = 0usize;
        let (mut cur, cum) = self.locate(pos);
        if cur == TAIL {
            return 0;
        }
        let mut skip = pos - cum;
        while cur != TAIL && done < want_total {
            let p = self.pieces[cur];
            let take = (p.len - skip).min((want_total - done) as u32) as usize;
            if take > 0 {
                if !self.pages[p.page].read_span(p.off + skip, take as u32, &mut buf[done..done + take]) {
                    break;
                }
                done += take;
            }
            skip = 0;
            cur = p.next;
        }
        done
    }

    pub fn byte(&self, pos: u32) -> Option<u8> {
        let mut b = [0u8; 1];
        if pos < self.len && self.read(pos, &mut b) == 1 {
            Some(b[0])
        } else {
            None
        }
    }

    /// True when the byte's piece lives in the edit page — the "modified"
    /// attribute the paint pipeline reads off the span's page while
    /// filling the per-byte attributes.
    pub fn byte_is_modified(&self, pos: u32) -> bool {
        if pos >= self.len {
            return false;
        }
        let (cur, _) = self.locate(pos);
        cur != TAIL && Some(self.pieces[cur].page) == self.edit_page
    }

    /// Serialises the whole chain — what a save writes to the file.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len as usize];
        let mut done = 0usize;
        let mut cur = self.pieces[HEAD].next;
        while cur != TAIL && done < out.len() {
            let p = self.pieces[cur];
            if p.len > 0 {
                let take = p.len as usize;
                if !self.pages[p.page].read_span(p.off, p.len, &mut out[done..done + take]) {
                    break;
                }
                done += take;
            }
            cur = p.next;
        }
        out.truncate(done);
        out
    }

    // ---- edits -----------------------------------------------------------

    /// Insert(pos, data, kind): splice the edit-page bytes in and record
    /// the insert-side marker.
    pub fn insert(&mut self, pos: u32, data: &[u8], kind: u32) -> bool {
        if pos > self.len || data.is_empty() {
            return false;
        }
        self.clear_redo();
        let n = data.len() as u32;
        let (_, edit_off) = self.splice_in(pos, data, false);
        self.push_marker(MarkerSide::Insert { edit_off }, data.to_vec(), pos, n, kind);
        self.last_pos = pos + n;
        self.last_kind = kind;
        true
    }

    /// The region step shared by delete and replace: capture the
    /// bytes and their modified flags, splice the region out, record the
    /// delete-side marker. No counter side effects — callers open the
    /// group.
    pub fn delete_region(&mut self, pos: u32, len: u32, kind: u32) -> bool {
        if len == 0 || len > self.len || pos > self.len - len {
            return false;
        }
        let mut data = vec![0u8; len as usize];
        if self.read(pos, &mut data) != len as usize {
            return false;
        }
        let mut data_modified = vec![false; len as usize];
        for i in 0..len {
            data_modified[i as usize] = self.byte_is_modified(pos + i);
        }
        self.clear_redo();
        self.push_marker(MarkerSide::Delete { data_modified }, data, pos, len, kind);
        self.splice_out(pos, len);
        // Any delete drops the "clean from file" flag.
        self.from_file = false;
        self.last_pos = pos;
        self.last_kind = kind;
        true
    }

    /// InsertBytes — the insert-mode write path. The raw insert
    /// never touches the clean-from-file flag; this wrapper clears it
    /// unless the insert lands exactly at the end, and stamps kind 1.
    pub fn insert_bytes(&mut self, pos: u32, data: &[u8]) -> bool {
        let at_end = pos == self.len;
        if !self.insert(pos, data, KIND_INSERT) {
            return false;
        }
        if self.from_file && !at_end {
            self.from_file = false;
        }
        self.last_kind = KIND_INSERT;
        self.last_pos = pos + data.len() as u32;
        true
    }

    /// Replace/Overwrite(pos, data, limit): counter bookkeeping
    /// inline (one bump, exactly one compensation whatever happens), region
    /// delete kind 3, then insert. The deleted region stops at the end of the
    /// data but the inserted one does not, so overwriting at the last byte —
    /// or in an empty snapshot — appends. Total length is unchanged while
    /// `limit == data.len()` and the region fits; the keystroke path passes
    /// both as 1.
    pub fn replace(&mut self, pos: u32, data: &[u8], limit: u32) -> bool {
        let len = data.len() as u32;
        if len == 0 || pos > self.len {
            return false;
        }
        let limit = limit.min(self.len - pos);
        self.touch_mod();
        if limit != 0 && !self.delete_region(pos, limit, KIND_REPLACE) {
            self.untouch_mod();
            return false;
        }
        if !self.insert(pos, data, KIND_REPLACE) {
            self.untouch_mod();
            // Roll the region marker back.
            if let Some(mut m) = self.undo.pop() {
                self.apply_marker(&mut m, true);
            }
            self.last_pos = 0;
            self.last_kind = 0;
            return false;
        }
        self.untouch_mod();
        self.last_kind = KIND_REPLACE;
        self.last_pos = pos + len;
        true
    }

    /// Delete (the wrapper form): kind 2 in its own counter group.
    pub fn delete(&mut self, pos: u32, len: u32) -> bool {
        self.touch_mod();
        if !self.delete_region(pos, len, KIND_DELETE) {
            self.untouch_mod();
            return false;
        }
        self.untouch_mod();
        self.last_kind = KIND_DELETE;
        self.last_pos = pos;
        true
    }

    /// The in-place scratch write for mid-number input: overwrite the
    /// last byte appended to the edit page. The markers keep their own
    /// copy of the bytes, so the owning insert-side marker (the newest
    /// one whose span ends at the edit-page tail) carries the patch too.
    /// Only valid right after a keystroke commit, which is exactly when
    /// the digit-composition path calls it.
    pub fn overwrite_last_edit_byte(&mut self, value: u8) -> bool {
        let Some(i) = self.edit_page else { return false };
        let Page::Mem { buf } = &mut self.pages[i] else { return false };
        let Some(last) = buf.len().checked_sub(1) else { return false };
        buf[last] = value;
        let edit_len = buf.len();
        for m in self.undo.iter_mut().rev() {
            if !m.is_delete_side
                && !m.data.is_empty()
                && m.edit_off as usize + m.data.len() == edit_len
            {
                let n = m.data.len() - 1;
                m.data[n] = value;
                break;
            }
        }
        true
    }

    // ---- file round-trip -------------------------------------------------

    /// LoadFile: clear, load the page, one piece over the whole file.
    /// `force_ro` forces read-only whatever the file's own permissions
    /// say.
    pub fn load_file(&mut self, path: &Path, force_ro: bool, small: bool) -> bool {
        self.clear();
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let size = meta.len();
        let ro = force_ro || meta.permissions().readonly();
        let page = if !small && size < SMALL_FILE_LIMIT {
            // Full read into memory; the handle never opens.
            match std::fs::read(path) {
                Ok(buf) => Page::Mem { buf },
                Err(_) => return false,
            }
        } else {
            match File::open(path) {
                Ok(f) => Page::File { file: f, size: size as u32 },
                Err(_) => return false,
            }
        };
        self.readonly = ro;
        self.pages.push(page);
        let page_idx = self.pages.len() - 1;
        self.file_page = Some(page_idx);
        if size > 0 {
            let p = self.alloc_piece(0, size as u32, page_idx);
            self.link_after(self.pieces[TAIL].prev, p);
        }
        self.len = size as u32;
        self.path = Some(path.to_path_buf());
        self.from_file = true;
        self.last_pos = 0;
        self.last_kind = 0;
        true
    }

    /// Data set from a buffer rather than a file: the host pushes a byte
    /// block straight into the pane.
    pub fn set_data(&mut self, data: &[u8]) {
        self.clear();
        if !data.is_empty() {
            self.pages.push(Page::Mem { buf: data.to_vec() });
            let page_idx = self.pages.len() - 1;
            let p = self.alloc_piece(0, data.len() as u32, page_idx);
            self.link_after(self.pieces[TAIL].prev, p);
            self.file_page = Some(page_idx);
        }
        self.len = data.len() as u32;
        self.from_file = false;
        self.readonly = false;
    }

    /// SaveFile: write the chain to a temp file, truncate to the exact
    /// length, move it over the target with replace semantics, then clear
    /// and reload from the target.
    pub fn save_file(&mut self, target: Option<&Path>) -> bool {
        let Some(src) = self.path.clone() else {
            return false;
        };
        let target = target.unwrap_or(&src).to_path_buf();
        // Staging beside the target keeps the final move on one volume.
        let tmp = target.with_extension("evhex_tmp");
        let data = self.to_vec();
        let write = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(&data)?;
            f.set_len(data.len() as u64)?;
            let _ = f.sync_all();
            Ok::<(), std::io::Error>(())
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        if target.exists() && std::fs::remove_file(&target).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        if std::fs::rename(&tmp, &target).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        // Reload through the ordinary load path.
        self.load_file(&target, false, false)
    }

    /// Clear: reset to the fresh state.
    pub fn clear(&mut self) {
        *self = Snapshot::new();
    }
}
