//! The line store: a gap buffer of fixed-size line records.
//!
//! The shape is a gap buffer over 24-byte line records: the vector keeps its capacity,
//! gap start, gap end and the record array, and a logical line `i` lives at physical
//! `i < gap_start ? i : i + (gap_end - gap_start)`. Each record holds a length, a
//! capacity, the buffer, a packed flags byte (EOL kind in bits 6-7: 1=CRLF, 2=LF, 3=CR,
//! 0=none), a lexer state byte, two nibble fields (low nibble = hidden marker) and a
//! flags byte (0x40 = needs re-lex). This crate's [`LineT`] is that record, byte for
//! byte (a `size_of` test pins it): `cap == 0` marks the undecoded form — the buffer slot
//! then holds the byte offset into the whole-file block — and `cap != 0` the decoded one,
//! where it points at a `char` buffer of `cap` slots.
//!
//! Storage model: the loader hands the **whole file as raw bytes** and
//! [`LineVector::from_raw`] splits it **without decoding** — every line starts undecoded.
//! Decoding is a function of `(raw, off, byte_len)` served through a small view cache,
//! never a stored copy, so unedited lines never materialize. Only edits materialize a
//! line ([`LineVector::ensure_owned`]).
//!
//! ## Ownership protocol (the one unsafe invariant)
//!
//! The record is `Copy` because the vector moves records with raw `memmove`s whose
//! ownership travels with the bytes: a physical slot inside the gap holds dead bytes that
//! are never read nor freed, and every site that retires a *live* record
//! ([`LineVector::remove_lines`], [`LineVector::set_line`], the `Drop` impl) frees its
//! buffer first. `data` stays private so a bitwise copy outside the vector cannot
//! duplicate a live pointer.

use std::alloc::{Layout, alloc, dealloc, realloc};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

/// Minimum owned capacity: `cap` doubles as the owned discriminant, so
/// even an emptied line keeps a non-zero allocation.
const MIN_OWNED_CAP: usize = 4;

/// 超过这个字符数的行按八分之一增长，不再翻倍：
/// 一条 5 MB 的行在末尾按一次键，翻倍会永久多占一半的缓冲。
const LARGE_LINE_CAP: usize = 1 << 16;

/// One line record: the 24-byte store for a single line — length,
/// capacity, buffer, then the flag bytes. `cap == 0` is the raw form:
/// `len` is the line's byte length and `data` its offset into the
/// whole-file block. `cap != 0` is the decoded, edited form: `len` the
/// char count, `data` the buffer pointer.
#[repr(C, align(8))]
#[derive(Clone, Copy, Default)]
pub struct LineT {
    /// Byte length while raw, char count once owned.
    len: u32,
    /// 0 = raw; owned lines keep their buffer capacity here.
    cap: u32,
    /// Raw byte offset, or the owned `char` buffer's pointer.
    data: u64,
    /// EOL kind in bits 6-7.
    pub b16: u8,
    /// Lexer state — kept so the record layout stays fixed; the
            /// highlighter here runs on tree-sitter instead.
    pub b17: u8,
    /// Nibble fields (low nibble = hidden marker).
    pub b18: u8,
    /// Flags (0x40 = needs re-lex).
    pub b19: u8,
}

impl LineT {
    /// EOL kind of the terminator following this line
    /// (the record's two EOL bits): 0=none, 1=CRLF, 2=LF, 3=CR.
    pub fn eol_kind(&self) -> u8 {
        self.b16 >> 6
    }

    pub fn set_eol_kind(&mut self, kind: u8) {
        // clear bits 6-7, then set.
        self.b16 &= 0x3F;
        self.b16 |= (kind & 3) << 6;
    }

    /// 行修改状态: the record's two-bit edit state — 0 untouched, 1 modified
    /// since the last save, 2 modified and written back by that save. The
    /// strip paints one band colour per value.
    pub fn state(&self) -> u8 {
        (self.b19 >> 4) & 3
    }

    pub fn set_state(&mut self, state: u8) {
        self.b19 &= !0x30;
        self.b19 |= (state & 3) << 4;
    }

    #[inline]
    fn is_raw(&self) -> bool {
        self.cap == 0
    }

    /// The raw byte range (offset within the post-BOM block, length).
    #[inline]
    fn raw_range(&self) -> (usize, usize) {
        debug_assert!(self.is_raw());
        (self.data as usize, self.len as usize)
    }

    #[inline]
    fn set_raw(&mut self, off: usize, byte_len: usize) {
        self.len = byte_len as u32;
        self.cap = 0;
        self.data = off as u64;
    }

    /// The owned chars. Callers guarantee `cap != 0` — every call site
    /// sits behind an `is_raw` check in the vector's edit/read paths.
    #[inline]
    fn chars(&self) -> &[char] {
        debug_assert!(self.cap != 0);
        unsafe { std::slice::from_raw_parts(self.data as *const char, self.len as usize) }
    }

    /// Grows the owned buffer to hold `need` chars (doubling, the way
    /// the record array itself grows).
    fn reserve(&mut self, need: usize) {
        debug_assert!(self.cap != 0);
        if need <= self.cap as usize {
            return;
        }
        // 小行翻倍（打字时重分配少）；已经到兆字节的行按八分之一长。
        let cap = self.cap as usize;
        let grow = if cap >= LARGE_LINE_CAP { cap + cap / 8 } else { cap * 2 };
        let new_cap = need.max(grow);
        let old = Layout::array::<char>(self.cap as usize).unwrap();
        // SAFETY: `data` was allocated for exactly `old`; realloc keeps
        // the contents and aborts on allocation failure.
        let ptr = unsafe { realloc(self.data as *mut u8, old, new_cap * std::mem::size_of::<char>()) };
        self.data = ptr as u64;
        self.cap = new_cap as u32;
    }

    /// Replaces the content with `chars`, freeing any owned buffer
    /// first. A raw record's flag bytes survive (EnsureDecoded keeps
    /// the EOL bits); a freed owned record is zeroed wholesale and
    /// the caller re-arms the flags.
    fn set_owned(&mut self, chars: &[char]) {
        self.free();
        let len = chars.len();
        let cap = len.max(MIN_OWNED_CAP);
        // SAFETY: `cap >= 1` keeps the layout non-empty, and the copy
        // stays within the fresh allocation.
        let ptr = unsafe {
            let p = alloc(Layout::array::<char>(cap).unwrap()) as *mut char;
            std::ptr::copy_nonoverlapping(chars.as_ptr(), p, len);
            p
        };
        self.len = len as u32;
        self.cap = cap as u32;
        self.data = ptr as u64;
    }

    /// Takes a buffer the caller already built as the record’s own —
    /// no second allocation, no copy. [`LineT::set_owned`] is the
    /// borrowed-slice form of the same thing.
    fn adopt(&mut self, chars: Vec<char>) {
        self.free();
        let len = chars.len();
        let mut cap = chars.capacity();
        let ptr = if cap == 0 {
            // `cap` doubles as the owned discriminant: an empty vector
            // still needs a live buffer to stay owned.
            cap = MIN_OWNED_CAP;
            unsafe { alloc(Layout::array::<char>(cap).unwrap()) as *mut char }
        } else {
            let p = chars.as_ptr() as *mut char;
            std::mem::forget(chars);
            p
        };
        self.len = len as u32;
        self.cap = cap as u32;
        self.data = ptr as u64;
    }

    /// Releases the owned buffer — the exact free pairing every
    /// allocation [`LineT::set_owned`]/[`LineT::reserve`] made. No-op
    /// for raw records. Only ever called on a record that owns its
    /// bytes: a live slot leaving the vector or being rewritten.
    fn free(&mut self) {
        if self.cap != 0 {
            // SAFETY: allocated at `cap` chars by set_owned/reserve.
            unsafe {
                dealloc(
                    self.data as *mut u8,
                    Layout::array::<char>(self.cap as usize).unwrap(),
                )
            };
            *self = Self::default();
        }
    }
}

/// The EOL scanner over already-decoded chars: from the start of `text`, measure
/// the run up to the next EOL (or `cap`). Returns the segment length, the EOL kind
/// found (0 when the text ends), the index past the EOL, and whether the text is
/// exhausted.
fn scan_eol(text: &[char], cap: usize) -> (usize, u8, usize, bool) {
    let mut i = 0;
    let mut eol_char = 0u32;
    let mut hit_cap = false;
    while i < text.len() {
        let c = text[i] as u32;
        if c == 13 || c == 10 {
            eol_char = c;
            break;
        }
        if i >= cap {
            hit_cap = true;
            break;
        }
        i += 1;
    }
    let mut kind = 0u8;
    if eol_char == 10 {
        kind = 2;
    }
    let mut next = i;
    let done;
    if i < text.len() {
        if !hit_cap {
            next += 1;
            if eol_char == 13 {
                if next < text.len() && text[next] as u32 == 10 {
                    next += 1;
                    kind = 1;
                } else {
                    kind = 3;
                }
            }
        }
        done = false;
    } else {
        done = true;
    }
    (i - 0, kind, next, done)
}

/// The EOL stepper over the raw block: walks
/// the encoding's code units — stride 2 for UTF-16 (byte-swapped unit
/// constants for 1201), single bytes otherwise — and reports the same
/// segment/kind/next triple as [`scan_eol`]. `cap` is measured in code
/// units
struct RawStepper<'a> {
    bytes: &'a [u8],
    pos: usize,
    stride: usize,
    be16: bool,
}

impl<'a> RawStepper<'a> {
    fn new(bytes: &'a [u8], enc: &Encoding, codepage: Option<u16>) -> Self {
        if let Some(cp) = codepage {
            // The conversion seam: the UTF-32 pages walk 4-byte units,
            // every other codepage byte-wise (the Win32 decode answers
            // for the characters, the walk only finds the EOLs).
            if crate::encoding::codepage_stride(cp) == 4 {
                return Self { bytes, pos: 0, stride: 4, be16: crate::encoding::codepage_be(cp) };
            }
            return Self { bytes, pos: 0, stride: 1, be16: false };
        }
        let (stride, be16) = if enc == encoding_rs::UTF_16LE {
            (2, false)
        } else if enc == encoding_rs::UTF_16BE {
            (2, true)
        } else {
            (1, false)
        };
        Self { bytes, pos: 0, stride, be16 }
    }

    /// Code unit at byte offset, 0 past the end.
    fn unit_at(&self, byte_off: usize) -> u32 {
        let b = self.bytes;
        if byte_off + self.stride <= b.len() {
            let s = &b[byte_off..byte_off + self.stride];
            match self.stride {
                1 => s[0] as u32,
                2 if self.be16 => u16::from_be_bytes([s[0], s[1]]) as u32,
                4 if self.be16 => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
                4 => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                _ => u16::from_le_bytes([s[0], s[1]]) as u32,
            }
        } else {
            0
        }
    }

    /// One [`scan_eol`] pass from `self.pos`. Returns the segment's
    /// byte length, its EOL kind, and whether the block is exhausted.
    /// A trailing EOL still yields a final empty segment on the next
    /// pull.
    fn next_segment(&mut self, cap: usize) -> (usize, u8, bool) {
        let start = self.pos;
        let mut units = 0usize;
        loop {
            if self.pos >= self.bytes.len() {
                return (self.pos - start, 0, true);
            }
            let at = self.pos;
            let u = self.unit_at(at);
            if u == 10 {
                self.pos = at + self.stride;
                return (at - start, 2, false);
            }
            if u == 13 {
                self.pos = at + self.stride;
                // unit_at reads 0 past the end, so a lone trailing CR
                // lands in the `else` arm here.
                let kind = if self.unit_at(self.pos) == 10 {
                    self.pos += self.stride;
                    1
                } else {
                    3
                };
                return (at - start, kind, false);
            }
            if units >= cap {
                return (self.pos - start, 0, false);
            }
            self.pos += self.stride;
            units += 1;
        }
    }
}

/// Ordered lexicographically — (line, col) is the document's native
/// order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl From<(usize, usize)> for Pos {
    fn from((line, col): (usize, usize)) -> Self {
        Pos { line, col }
    }
}

impl From<Pos> for (usize, usize) {
    fn from(p: Pos) -> Self {
        (p.line, p.col)
    }
}

/// A decoded line as seen by a caller: a borrow of an owned line, or
/// a cached decode of a raw one. Derefs to the line's chars.
pub enum LineView<'a> {
    Owned(&'a [char]),
    Decoded(Rc<Vec<char>>),
}

impl std::ops::Deref for LineView<'_> {
    type Target = [char];

    fn deref(&self) -> &[char] {
        match self {
            LineView::Owned(s) => s,
            LineView::Decoded(v) => v.as_slice(),
        }
    }
}

/// One cached decode: the characters, and whether every one of them takes a
/// single cell. The second half is what lets a row put its window on the
/// character it needs by division instead of by measuring the text before it.
struct View {
    chars: Rc<Vec<char>>,
    uniform: bool,
}

/// Whether every character of `s` is a printable ASCII glyph — no tab, no
/// wide glyph, no modifier. Such a line advances one cell per character, so
/// its pixel of column `n` is `n * width('x')`.
pub(crate) fn uniform_cells(chars: &[char]) -> bool {
    chars.iter().all(|c| matches!(c, ' '..='~'))
}

/// Transient decodes of raw lines (§2.4-1: never stored on the line).
/// Keyed by logical line index; any structural mutation clears it.
#[derive(Default)]
struct ViewCache {
    map: std::collections::HashMap<usize, View>,
}

/// Flush threshold for the view cache. Lines, not bytes — the scan
/// cap bounds a single line at 8 MB, but real lines are far smaller;
/// a full-screen repaint touches ~100 entries.
const VIEW_CACHE_MAX: usize = 8192;

/// The line store: gap buffer over [`LineT`].
pub struct LineVector {
    array: Vec<LineT>,
    gap_start: usize,
    gap_end: usize,
    /// First-segment scan cap: a single scan longer than this splits
    /// without an EOL.
    scan_cap: usize,
    /// The document's default EOL kind, set at load; the
    /// separator the deleted-text collect falls back to.
    default_eol: u8,
    /// The immutable whole-file block (the reader hands it over on
    /// completion). `None` for programmatically built
    /// documents — those only ever hold undecodable-empty records.
    raw: Option<Arc<Vec<u8>>>,
    /// BOM length skipped at the front of `raw`; raw offsets are
    /// relative to this base.
    raw_base: usize,
    enc: &'static Encoding,
    /// A Windows codepage the bytes decode through instead of `enc` —
    /// the conversion seam's carrier. `None` = the encoding_rs path;
    /// `enc` stays as a display marker and is never consulted while
    /// this is `Some`.
    codepage: Option<u16>,
    views: RefCell<ViewCache>,
    /// 一条原始行的字符数备忘：(行号, 字节偏移, 字节长度, 字符数)。
    /// 偏移 + 长度唯一定位一条记录，行号变动后旧条目自然失配，
    /// 所以这里不用跟着增删行去清。
    len_memo: std::cell::Cell<Option<(usize, usize, usize, usize)>>,
    /// 修改行标记: the marked lines as maximal runs, maintained in step with the
    /// records that carry them so reading it costs the number of runs and not the
    /// number of lines. See [`LineVector::state_runs`].
    runs: Vec<Run>,
}

/// One maximal run of lines sharing a non-zero edit state: `[first, last]`
/// inclusive. Kept sorted, non-overlapping and un-merged (touching runs always
/// differ in state).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Run {
    first: u32,
    last: u32,
    state: u8,
}

impl Drop for LineVector {
    fn drop(&mut self) {
        // Live records own their buffers; slots inside the gap hold
        // stale duplicates of records that moved and must not be
        // freed (the ownership protocol from the module docs).
        for i in 0..self.array.len() {
            if i < self.gap_start || i >= self.gap_end {
                self.array[i].free();
            }
        }
    }
}

impl LineVector {
    /// The vector ctor: capacity `max(estimated_lines + 8, 8)`, gap
    /// spanning the whole array.
    pub fn new(estimated_lines: usize) -> Self {
        let cap = (estimated_lines + 8).max(8);
        Self {
            array: vec![LineT::default(); cap],
            gap_start: 0,
            gap_end: cap,
            scan_cap: 0x800000,
            default_eol: 2,
            raw: None,
            raw_base: 0,
            enc: UTF_8,
            codepage: None,
            views: RefCell::new(ViewCache::default()),
            len_memo: std::cell::Cell::new(None),
            runs: Vec::new(),
        }
    }

    /// Builds the store straight from the raw block: first count the EOLs, then
    /// allocate the record array once (`lines + 8`) and fill it with raw byte
    /// ranges. **No decode happens here.**
    pub fn from_raw(raw: Arc<Vec<u8>>, enc: &'static Encoding, bom_len: usize) -> Self {
        Self::from_raw_with_cap(raw, enc, bom_len, 0x800000, None)
    }

    /// The conversion seam's landing point: a block re-encoded into a
    /// Windows codepage (`Some` carries the id the Win32 pair answers
    /// for). Same two-pass walk, the stepper told to stride 4-byte
    /// units for the UTF-32 pages.
    pub fn from_raw_codepage(
        raw: Arc<Vec<u8>>,
        cp: u16,
        bom_len: usize,
    ) -> Self {
        Self::from_raw_with_cap(raw, WINDOWS_1252, bom_len, 0x800000, Some(cp))
    }

    /// Same walk with an explicit scan cap, for tests.
    fn from_raw_with_cap(
        raw: Arc<Vec<u8>>,
        enc: &'static Encoding,
        bom_len: usize,
        cap: usize,
        codepage: Option<u16>,
    ) -> Self {
        let bytes = &raw[bom_len.min(raw.len())..];
        // Pass 1: count terminators.
        let mut stepper = RawStepper::new(bytes, enc, codepage);
        let mut eols = 0usize;
        loop {
            let (_, kind, done) = stepper.next_segment(cap);
            if kind != 0 {
                eols += 1;
            }
            if done {
                break;
            }
        }
        let mut v = Self::new(eols);
        // The store keeps its own Arc ref (refcount bump); the byte
        // borrow above stays on the local binding for pass 2.
        v.raw = Some(Arc::clone(&raw));
        v.raw_base = bom_len;
        v.enc = enc;
        v.codepage = codepage;
        // Pass 2 fills the records with byte ranges; the capacity above
        // makes every insert land in the gap without growth.
        let mut stepper = RawStepper::new(bytes, enc, codepage);
        let mut off = 0usize;
        loop {
            let (seg_len, kind, done) = stepper.next_segment(cap);
            let line = v.line_count();
            v.insert_line_at(line, LineT::default());
            let p = v.phys(line);
            v.array[p].set_raw(off, seg_len);
            v.array[p].set_eol_kind(kind);
            off += seg_len + eol_wire_bytes(kind, stepper.stride);
            if done {
                break;
            }
        }
        v
    }

    /// Compat constructor over decoded text: the text is re-encoded
    /// once and split raw-domain, so stores built this way share the
    /// load path's behavior (same EOL kinds, same scan-cap splits).
    pub fn from_text(text: &str) -> Self {
        Self::from_raw(Arc::new(text.as_bytes().to_vec()), UTF_8, 0)
    }

    pub fn with_scan_cap(mut self, cap: usize) -> Self {
        self.scan_cap = cap;
        self
    }

    /// The source encoding used to decode raw lines (set at load).
    pub fn set_encoding(&mut self, enc: &'static Encoding) {
        if self.enc != enc {
            self.enc = enc;
            self.views.borrow_mut().map.clear();
        }
    }

    #[inline]
    fn cap(&self) -> usize {
        self.array.len()
    }

    /// Logical line count: capacity minus the gap.
    pub fn line_count(&self) -> usize {
        self.cap() - (self.gap_end - self.gap_start)
    }

    /// Physical slot of a logical line under the gap rule.
    fn phys(&self, line: usize) -> usize {
        if line < self.gap_start {
            line
        } else {
            line + (self.gap_end - self.gap_start)
        }
    }

    /// Moves `count` records from physical `src` to `dst`. Ownership
    /// travels with the bytes; the vacated slots become gap space
    /// whose stale bytes are never read nor freed.
    fn move_range(&mut self, src: usize, dst: usize, count: usize) {
        // SAFETY: both ranges lie inside `array`, and records are
        // plain `Copy` bytes.
        unsafe {
            std::ptr::copy(
                self.array.as_ptr().add(src),
                self.array.as_mut_ptr().add(dst),
                count,
            );
        }
    }

    /// MoveLineGap: slides the gap to `line`.
    pub fn move_line_gap(&mut self, line: usize) {
        let gs = self.gap_start;
        if line >= gs {
            if line == gs {
                self.gap_start = line;
            } else {
                let ge = self.gap_end;
                let delta = line - gs;
                self.move_range(ge, gs, delta);
                self.gap_start = line;
                self.gap_end = ge + delta;
            }
        } else {
            let ge = self.gap_end;
            let count = gs - line;
            self.gap_end = ge - count;
            self.move_range(line, self.gap_end, count);
            self.gap_start = line;
        }
    }

    /// The gap-buffer insert: places `src` at
    /// logical `at`, growing 2x when the gap is exhausted. The growth
    /// test is emptiness (`gap_start == gap_end`), not the gap
    /// sitting at the array's end: `move_line_gap` can empty the gap
    /// anywhere, and writing through an empty gap would overwrite the
    /// live line after it and leave `gap_end < gap_start`. Ownership
    /// of `src`'s bytes moves into the slot; the dead bytes the gap
    /// slot held are discarded without a free — gap space never owns
    /// anything.
    pub fn insert_line_at(&mut self, at: usize, src: LineT) {
        let state = src.state();
        self.move_line_gap(at);
        if self.gap_start == self.gap_end {
            self.grow();
        }
        self.array[self.gap_start] = src;
        self.gap_start += 1;
        self.views.borrow_mut().map.clear();
        self.shift_for_insert(at);
        self.paint_runs(at, at, state);
    }

    /// Doubles the record array, keeping the gap's contents and
    /// appending the freed slots to the gap. `LineT: Copy` makes the
    /// region clones bitwise moves; the old array's gap-space bytes
    /// are simply discarded.
    fn grow(&mut self) {
        let old_cap = self.cap();
        let new_cap = 2 * old_cap;
        let after = old_cap - self.gap_end;
        let mut grown = vec![LineT::default(); new_cap];
        grown[..self.gap_start].clone_from_slice(&self.array[..self.gap_start]);
        grown[new_cap - after..].clone_from_slice(&self.array[self.gap_end..]);
        self.array = grown;
        self.gap_end = new_cap - after;
    }

    /// The removal path: removes `count` lines
    /// starting at `at` by absorbing them into the gap. Every zeroed
    /// slot below holds a *live* record being deleted — freed first.
    pub fn remove_lines(&mut self, at: usize, count: usize) {
        if at > self.gap_start || self.gap_start > at + count {
            self.move_line_gap(at);
        } else {
            for slot in at..self.gap_start {
                self.array[slot].free();
            }
            let extend = count + at - self.gap_start;
            let ge = self.gap_end;
            for slot in ge..ge + extend {
                self.array[slot].free();
            }
            self.gap_start = at;
            self.gap_end = ge + extend;
            self.views.borrow_mut().map.clear();
            self.shift_for_remove(at, count);
            return;
        }
        let ge = self.gap_end;
        let extend = count;
        for slot in ge..ge + extend {
            self.array[slot].free();
        }
        self.gap_end = ge + extend;
        self.views.borrow_mut().map.clear();
        self.shift_for_remove(at, count);
    }

    /// One raw span through the store's encoding: the encoding_rs
    /// static by default, or the Win32 pair when the store carries a
    /// Windows codepage (the conversion seam's decode side).
    fn decode_raw(&self, bytes: &[u8]) -> String {
        match self.codepage {
            Some(cp) => crate::encoding::decode_codepage(cp, bytes),
            None => {
                // No BOM sniffing: the loader already claimed the file
                // header, so a line that happens to start with those
                // bytes must decode as the store's encoding.
                let (text, _) = self.enc.decode_without_bom_handling(bytes);
                text.into_owned()
            }
        }
    }

    /// Decodes a raw line into the view cache and hands it out.
    fn decode_line(&self, line: usize) -> View {
        let mut views = self.views.borrow_mut();
        if let Some(v) = views.map.get(&line) {
            return View {
                chars: v.chars.clone(),
                uniform: v.uniform,
            };
        }
        let (off, byte_len) = self.array[self.phys(line)].raw_range();
        let bytes = &self.raw.as_ref().unwrap()
            [self.raw_base + off..self.raw_base + off + byte_len];
        let text = self.decode_raw(bytes);
        // The cell property rides the decode: one pass answers both.
        let mut chars = Vec::with_capacity(text.len());
        let mut uniform = true;
        for ch in text.chars() {
            if !matches!(ch, ' '..='~') {
                uniform = false;
            }
            chars.push(ch);
        }
        let rc = Rc::new(chars);
        if views.map.len() >= VIEW_CACHE_MAX {
            views.map.clear();
        }
        views.map.insert(
            line,
            View {
                chars: rc.clone(),
                uniform,
            },
        );
        View { chars: rc, uniform }
    }

    /// The decoded chars of `line`. An
    /// owned line is borrowed; a raw line is served through the view
    /// cache — never materialized.
    pub fn line_view(&self, line: usize) -> LineView<'_> {
        let p = self.phys(line);
        if self.array[p].is_raw() {
            LineView::Decoded(self.decode_line(line).chars)
        } else {
            LineView::Owned(self.array[p].chars())
        }
    }

    /// The line's chars as a shared handle. An owned line joins the
    /// cache here too (a copy once, then reused), so the repeat reads
    /// of a repaint or a caret walk — one per visible row per frame —
    /// share one allocation instead of re-collecting the line. The
    /// cache is cleared by every mutation path ([`Self::clear_views`]),
    /// which is what keeps an owned entry honest.
    pub fn line_chars_rc(&self, line: usize) -> Rc<Vec<char>> {
        self.cached_view(line).chars
    }

    /// Whether every character of `line` takes exactly one cell. The
    /// answer rides the cached decode, so a repaint asks for a lookup.
    pub fn line_uniform(&self, line: usize) -> bool {
        self.cached_view(line).uniform
    }

    /// The cached decode of `line`, decoding and caching it when absent.
    fn cached_view(&self, line: usize) -> View {
        if let Some(v) = self.views.borrow().map.get(&line) {
            return View {
                chars: v.chars.clone(),
                uniform: v.uniform,
            };
        }
        let p = self.phys(line);
        if self.array[p].is_raw() {
            return self.decode_line(line);
        }
        let text = self.array[p].chars();
        let v = View {
            chars: Rc::new(text.to_vec()),
            uniform: uniform_cells(text),
        };
        let mut views = self.views.borrow_mut();
        if views.map.len() >= VIEW_CACHE_MAX {
            views.map.clear();
        }
        views.map.insert(
            line,
            View {
                chars: v.chars.clone(),
                uniform: v.uniform,
            },
        );
        v
    }

    /// Whether the store's view cache already holds `line`'s decode
    /// and it equals `out` — the facade's single-line cache can then
    /// adopt it without a second decode + collect.
    pub fn cached_view_matches(&self, line: usize, out: &str) -> bool {
        match self.views.borrow().map.get(&line) {
            Some(v) => v.chars.iter().copied().eq(out.chars()),
            None => false,
        }
    }

    /// Drops every transient decode. Called on the mutation paths —
    /// [`LineView`] borrows must not outlive a store edit.
    pub fn clear_views(&self) {
        self.views.borrow_mut().map.clear();
    }

    /// Number of chars on `line` (the facade's column unit). Raw
    /// lines count their code points straight from the bytes — no
    /// decode, no allocation — and the vector keeps the last answer
    /// with the record it came from, so a 5 MB line is walked once,
    /// not once per caller.
    pub fn line_len(&self, line: usize) -> usize {
        let p = self.phys(line);
        let r = &self.array[p];
        if !r.is_raw() {
            return r.len as usize;
        }
        let (off, byte_len) = r.raw_range();
        if let Some((l, o, bl, n)) = self.len_memo.get() {
            if l == line && o == off && bl == byte_len {
                return n;
            }
        }
        let bytes = &self.raw.as_ref().unwrap()
            [self.raw_base + off..self.raw_base + off + byte_len];
        let n = if self.codepage.is_some() {
            // A codepage store has no byte pattern to count: decode
            // through the Win32 pair and count chars.
            self.decode_line(line).chars.len()
        } else if self.enc == UTF_8 {
            bytes.iter().filter(|b| *b & 0xC0 != 0x80).count()
        } else if self.enc == encoding_rs::UTF_16LE
            || self.enc == encoding_rs::UTF_16BE
        {
            let be = self.enc == encoding_rs::UTF_16BE;
            bytes
                .chunks_exact(2)
                .filter(|c| {
                    let u = if be {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    };
                    !(0xDC00..=0xDFFF).contains(&u)
                })
                .count()
        } else {
            self.decode_line(line).chars.len()
        };
        self.len_memo.set(Some((line, off, byte_len, n)));
        n
    }

    /// UTF-8 byte length of the line's text (the span index's unit).
    /// UTF-8 raw lines are already bytes; other encodings decode
    /// through the view cache.
    pub fn line_bytes_len(&self, line: usize) -> usize {
        let p = self.phys(line);
        let r = &self.array[p];
        if !r.is_raw() {
            return r.chars().iter().map(|c| c.len_utf8()).sum();
        }
        if self.codepage.is_none() && self.enc == UTF_8 {
            r.len as usize
        } else {
            self.line_view(line).iter().map(|c| c.len_utf8()).sum()
        }
    }

    /// The line's original bytes while the line is still raw — the
    /// save path's direct write-back. `None` for
    /// owned (edited) lines, which the saver re-encodes.
    pub fn raw_line_bytes(&self, line: usize) -> Option<&[u8]> {
        let p = self.phys(line);
        let r = &self.array[p];
        if !r.is_raw() {
            return None;
        }
        let raw = self.raw.as_ref()?;
        let (off, byte_len) = r.raw_range();
        let start = self.raw_base + off;
        Some(&raw[start..start + byte_len])
    }

    pub fn eol_kind(&self, line: usize) -> u8 {
        self.array[self.phys(line)].eol_kind()
    }

    pub fn set_eol_kind(&mut self, line: usize, kind: u8) {
        let p = self.phys(line);
        self.array[p].set_eol_kind(kind);
    }

    /// 行修改状态: the line's two-bit edit state (see [`LineT::state`]).
    pub fn line_state(&self, line: usize) -> u8 {
        self.array[self.phys(line)].state()
    }

    pub fn set_line_state(&mut self, line: usize, state: u8) {
        let p = self.phys(line);
        if self.array[p].state() == state {
            return;
        }
        self.array[p].set_state(state);
        self.paint_runs(line, line, state);
    }

    /// Marks a line modified: 0 and 2 both become 1, 1 stays put. Editing a
    /// line the last save wrote back (2) makes it dirty again.
    pub fn mark_line_dirty(&mut self, line: usize) {
        self.set_line_state(line, 1);
    }

    /// The marked lines as `(first, last, state)` runs, in line order. Costs the
    /// run count, not the document's: the list is carried along by the same
    /// inserts, removals and state writes that move the records themselves.
    pub fn state_runs(&self) -> Vec<(u32, u32, u8)> {
        self.runs
            .iter()
            .map(|r| (r.first, r.last, r.state))
            .collect()
    }

    /// Writes `state` over the line window `[lo, hi]` in the run list. A run the
    /// window cuts into keeps the part it held outside it; the window itself
    /// becomes one run of `state`, or nothing when `state` is 0. Neighbours of
    /// the same state close up behind it, which is what keeps the list maximal.
    fn paint_runs(&mut self, lo: usize, hi: usize, state: u8) {
        let (lo, hi) = (lo as u32, hi as u32);
        if hi < lo {
            return;
        }
        let first = self.runs.partition_point(|r| r.last < lo);
        let after = self.runs.partition_point(|r| r.first <= hi);
        let cut = first < after;
        let mut keep: Vec<Run> = Vec::with_capacity(3);
        // The leftmost run the window cuts survives as whatever it held before
        // it, and the rightmost as whatever it held after it.
        if cut && self.runs[first].first < lo {
            keep.push(Run {
                first: self.runs[first].first,
                last: lo - 1,
                state: self.runs[first].state,
            });
        }
        if state != 0 {
            keep.push(Run {
                first: lo,
                last: hi,
                state,
            });
        }
        if cut && self.runs[after - 1].last > hi {
            keep.push(Run {
                first: hi + 1,
                last: self.runs[after - 1].last,
                state: self.runs[after - 1].state,
            });
        }
        if keep.is_empty() && !cut {
            return;
        }
        self.runs.splice(first..after, keep);
        self.close_runs(first.saturating_sub(1));
    }

    /// Merges touching runs of one state from `i` onward. Only a window's own
    /// edges can have become neighbours, so this stops at the first pair that
    /// does not match.
    fn close_runs(&mut self, mut i: usize) {
        while i + 1 < self.runs.len() {
            if self.runs[i].state == self.runs[i + 1].state
                && self.runs[i].last + 1 == self.runs[i + 1].first
            {
                let joined = self.runs[i + 1].last;
                self.runs[i].last = joined;
                self.runs.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }

    /// The same merge, over the whole list.
    fn close_runs_all(&mut self) {
        let mut w = 0usize;
        for i in 0..self.runs.len() {
            let touch = w > 0
                && self.runs[w - 1].state == self.runs[i].state
                && self.runs[w - 1].last + 1 == self.runs[i].first;
            if !touch {
                self.runs[w] = self.runs[i];
                w += 1;
            } else {
                let last = self.runs[i].last;
                self.runs[w - 1].last = last;
            }
        }
        self.runs.truncate(w);
    }

    /// Opens a line at `at`: every index from there up steps one, and the run
    /// that spans the point provisionally swallows the newcomer — `paint_runs`
    /// then gives it its own state, which is what closes it back up when the two
    /// differ.
    fn shift_for_insert(&mut self, at: usize) {
        let at = at as u32;
        for r in &mut self.runs {
            if r.last < at {
                // Wholly above the new line: its indices do not move.
                continue;
            }
            if r.first >= at {
                r.first += 1;
                r.last += 1;
            } else {
                r.last += 1;
            }
        }
    }

    /// Closes `count` lines from `at`: each run loses exactly the part it held
    /// inside the window, and everything past the window steps down by it.
    fn shift_for_remove(&mut self, at: usize, count: usize) {
        if count == 0 {
            return;
        }
        let (n, hi) = (count as u32, (at + count) as u32);
        let lo = at as u32;
        self.runs.retain_mut(|r| {
            if r.last < lo {
                return true;
            }
            if r.first >= hi {
                r.first -= n;
                r.last -= n;
                return true;
            }
            if r.first < lo {
                // Starts before the window: it survives as whatever it held to
                // the left, plus whatever it held to the right of the window.
                if r.last >= hi {
                    r.last -= n;
                } else {
                    r.last = lo - 1;
                }
                return true;
            }
            r.first = lo;
            r.last = if r.last >= hi { r.last - n } else { return false };
            r.last >= r.first
        });
        self.close_runs_all();
    }

    /// The save's own pass: every modified line (1) has just been written, so
    /// it becomes "saved" (2). Untouched lines stay 0 and a line already
    /// marked 2 stays 2 — the state survives the save that wrote it.
    pub fn mark_all_saved(&mut self) {
        for line in 0..self.line_count() {
            if self.line_state(line) == 1 {
                self.set_line_state(line, 2);
            }
        }
    }

    /// Replace the content of a line in place with owned text (used
    /// by tests and programmatic builders).
    pub fn set_line(&mut self, line: usize, text: &[char], kind: u8) {
        let p = self.phys(line);
        self.array[p].set_owned(text);
        self.array[p].set_eol_kind(kind);
        self.views.borrow_mut().map.clear();
    }

    /// EnsuresDecoded: materialize `line` into an owned buffer. This
    /// implementation materializes only on **edit** — reads stay transient
    /// (§2.4-1).
    fn ensure_owned(&mut self, line: usize) {
        let p = self.phys(line);
        if !self.array[p].is_raw() {
            return;
        }
        let (off, byte_len) = self.array[p].raw_range();
        let bytes = &self.raw.as_ref().unwrap()
            [self.raw_base + off..self.raw_base + off + byte_len];
        // UTF-8 needs no intermediate `String`: the block is already a
        // `str`, so the chars come straight out of it. A codepage store
        // still decodes through the Win32 pair.
        let chars: Vec<char> = match self.codepage {
            None if self.enc == UTF_8 => match std::str::from_utf8(bytes) {
                Ok(text) => text.chars().collect(),
                Err(_) => self.decode_raw(bytes).chars().collect(),
            },
            _ => self.decode_raw(bytes).chars().collect(),
        };
        // adopt keeps a raw record's flag bytes — the EOL kind rides
        // through materialization.
        self.array[p].adopt(chars);
        self.views.borrow_mut().map.clear();
        self.drop_raw_when_fully_owned();
    }

    /// Releases the whole-file block once every line carries its own
    /// buffer — from then on the bytes are dead weight (5.9 MB on a
    /// one-line file). The scan is bounded: a document with more lines
    /// than this keeps the block, and those are the documents that
    /// still have raw lines to read from it.
    fn drop_raw_when_fully_owned(&mut self) {
        const SCAN_MAX: usize = 4096;
        if self.raw.is_none() {
            return;
        }
        let n = self.line_count();
        if n > SCAN_MAX {
            return;
        }
        for line in 0..n {
            if self.array[self.phys(line)].is_raw() {
                return;
            }
        }
        self.raw = None;
        self.raw_base = 0;
    }

    /// The record truncate: shorten to `new_len`. The
    /// buffer keeps its capacity — repeated edits on a line never
    /// realloc downward.
    fn truncate_line(lt: &mut LineT, new_len: usize) {
        debug_assert!(new_len <= lt.len as usize);
        lt.len = new_len as u32;
    }

    /// The record append: splice `src` in at `dst_len`,
    /// shifting the tail right.
    fn append_line(lt: &mut LineT, dst_len: usize, src: &[char]) {
        if src.is_empty() {
            return;
        }
        let old_len = lt.len as usize;
        let new_len = dst_len + src.len();
        lt.reserve(new_len);
        // SAFETY: reserve() covers `new_len`; the tail shift writes
        // raw memory past the initialized `old_len`, then the source
        // lands in the hole — pointers only, no references into the
        // uninitialized stretch.
        unsafe {
            let ptr = lt.data as *mut char;
            std::ptr::copy(
                ptr.add(dst_len),
                ptr.add(dst_len + src.len()),
                old_len - dst_len,
            );
            std::ptr::copy_nonoverlapping(src.as_ptr(), ptr.add(dst_len), src.len());
        }
        lt.len = new_len as u32;
    }

    /// The text-insert path: insert `text` at
    /// `pos`, splitting lines at EOLs. Returns the position after the
    /// inserted text.
    pub fn insert_text(&mut self, pos: Pos, text: &[char]) -> Pos {
        // The store's view cache describes the text as it stood: an edit that
        // lands in an already-owned line must drop it here, not only at the
        // facade that happens to call this.
        self.clear_views();
        self.move_line_gap(pos.line);
        self.ensure_owned(pos.line);
        let line = self.phys(pos.line);
        // The original line's EOL kind, saved before the splice — the
        // final line inherits it.
        let old_kind = self.array[line].eol_kind();
        // First segment: scan to the first EOL (bounded by the scan cap).
        let (first_seg, kind, end, mut no_eol) = scan_eol(text, self.scan_cap);
        if first_seg == text.len() {
            no_eol = true;
        }
        // The original line takes the first EOL's kind; with no EOL it
        // keeps its own (or loses it when the scan cap was hit).
        if kind != 0 {
            self.array[line].set_eol_kind(kind);
        } else if first_seg >= self.scan_cap {
            self.array[line].set_eol_kind(0);
        }
        // Splice the first segment in at the column.
        if first_seg > 0 {
            let lt = &mut self.array[line];
            let old_len = lt.len as usize;
            let new_len = old_len + first_seg;
            lt.reserve(new_len);
            // SAFETY: as in append_line — raw pointers into the
            // freshly reserved stretch, no references to uninit.
            unsafe {
                let ptr = lt.data as *mut char;
                std::ptr::copy(
                    ptr.add(pos.col),
                    ptr.add(pos.col + first_seg),
                    old_len - pos.col,
                );
                std::ptr::copy_nonoverlapping(text.as_ptr(), ptr.add(pos.col), first_seg);
            }
            lt.len = new_len as u32;
        }
        let mut out = Pos {
            line: pos.line,
            col: pos.col + first_seg,
        };
        if no_eol {
            return out;
        }
        // One physical line per further segment, each carrying its own EOL.
        let mut scan_pos = end;
        let mut last_seg_len;
        loop {
            let seg_start = scan_pos;
            let (seg_len, seg_kind, next, done) = scan_eol(&text[scan_pos..], self.scan_cap);
            out.line += 1;
            self.insert_line_at(out.line, LineT::default());
            let p = self.phys(out.line);
            self.array[p].set_owned(&text[seg_start..seg_start + seg_len]);
            self.array[p].set_eol_kind(seg_kind);
            last_seg_len = seg_len;
            scan_pos = seg_start + next;
            if done {
                break;
            }
        }
        // The final line takes the original line's old EOL kind, then the
        // original line's remaining text is appended to it and the original
        // line is cut at the insert point.
        self.ensure_owned(out.line);
        let final_line = self.phys(out.line);
        let first_line = self.phys(pos.line);
        self.array[final_line].set_eol_kind(old_kind);
        let tail_start = pos.col + first_seg;
        let first_len = self.array[first_line].len as usize;
        let tail: Vec<char> = self.array[first_line].chars()[tail_start..first_len].to_vec();
        // the move is skipped only when the
        // split line is already cut at the insert point.
        if self.array[first_line].len as usize != tail_start {
            let flen = self.array[final_line].len as usize;
            Self::append_line(&mut self.array[final_line], flen, &tail);
            Self::truncate_line(&mut self.array[first_line], tail_start);
        }
        out.col = last_seg_len;
        out
    }

    /// The text-delete path: remove the char range
    /// `[start, end)`, joining lines across the boundary.
    pub fn delete_text(&mut self, start: Pos, end: Pos) {
        self.clear_views();
        self.move_line_gap(start.line);
        self.ensure_owned(start.line);
        if start.line == end.line {
            let line = self.phys(start.line);
            let del = end.col - start.col;
            if del != 0 {
                let lt = &mut self.array[line];
                let len = lt.len as usize;
                // SAFETY: the shifted tail stays within the
                // initialized chars.
                unsafe {
                    std::ptr::copy(
                        (lt.data as *mut char).add(start.col + del),
                        (lt.data as *mut char).add(start.col),
                        len - start.col - del,
                    );
                }
                lt.len = (len - del) as u32;
            }
            return;
        }
        // The end line's EOL kind, read before the gap moves to it.
        let end_kind = self.eol_kind(end.line);
        self.ensure_owned(end.line);
        self.move_line_gap(end.line);
        let start_line = self.phys(start.line);
        let end_line = self.phys(end.line);
        Self::truncate_line(&mut self.array[start_line], start.col);
        let end_len = self.array[end_line].len as usize;
        let tail: Vec<char> = self.array[end_line].chars()[end.col..end_len].to_vec();
        let slen = self.array[start_line].len as usize;
        Self::append_line(&mut self.array[start_line], slen, &tail);
        self.remove_lines(start.line + 1, end.line - start.line);
        // 拼起来的这一行，行尾跟的是终点行自己的：删除按字节域
        // [byte(start), byte(end)) 收走的是起点行的行尾，留在原地的
        // 是终点行的。混合行尾的文档里两边字节长可以不一样（CRLF 对
        // LF），按起点留会让删除掉的字节数和 (line, col) 域、
        // [`Self::collect_deleted_text`]、range 文本三者对不上。
        self.set_eol_kind(start.line, end_kind);
    }

    /// The deleted text of `[start, end)` as it stood, with one separator per
    /// stored line kind. `eol_kind` 0 means "the document default", the same
    /// convention the store writes.
    pub fn collect_deleted_text(&self, start: Pos, end: Pos, eol_kind: u8) -> String {
        let mut out = String::new();
        if start.line == end.line {
            let line = self.line_view(start.line);
            out.extend(line[start.col..].iter());
            return out;
        }
        let kind = if eol_kind != 0 {
            eol_kind
        } else {
            self.default_eol
        };
        let sep: Vec<char> = match kind {
            2 => vec!['\n'],
            3 => vec!['\r'],
            _ => vec!['\r', '\n'],
        };
        let first = self.line_view(start.line);
        out.extend(first[start.col..].iter());
        if self.eol_kind(start.line) != 0 {
            out.extend(sep.iter());
        }
        for line in start.line + 1..end.line {
            let t = self.line_view(line);
            out.extend(t.iter());
            if self.eol_kind(line) != 0 {
                out.extend(sep.iter());
            }
        }
        let last = self.line_view(end.line);
        out.extend(last[..end.col].iter());
        out
    }
}

/// Wire byte length of an EOL kind at `stride` bytes per unit.
fn eol_wire_bytes(kind: u8, stride: usize) -> usize {
    match kind {
        1 => 2 * stride,
        2 | 3 => stride,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn text(v: &LineVector) -> String {
        let mut s = String::new();
        for l in 0..v.line_count() {
            s.extend(v.line_view(l).iter());
            match v.eol_kind(l) {
                1 => s.push_str("\r\n"),
                2 => s.push('\n'),
                3 => s.push('\r'),
                _ => {}
            }
        }
        s
    }

    #[test]
    fn a_line_record_is_24_bytes_and_starts_raw() {
        // One line record: length, capacity, buffer, then the flag bytes.
        assert_eq!(std::mem::size_of::<LineT>(), 24);
        assert_eq!(std::mem::align_of::<LineT>(), 8);
        // The zeroed record is the raw form — the vector's fresh slots.
        let r = LineT::default();
        assert!(r.is_raw());
        assert_eq!(r.len, 0);
        assert_eq!(r.cap, 0);
    }

    #[test]
    fn from_text_splits_and_keeps_kinds() {
        let v = LineVector::from_text("");
        assert_eq!(v.line_count(), 1);
        assert_eq!(v.line_len(0), 0);
        let v = LineVector::from_text("a\r\nb\rc\nd");
        assert_eq!(v.line_count(), 4);
        assert_eq!(text(&v), "a\r\nb\rc\nd");
        assert_eq!(v.eol_kind(0), 1);
        assert_eq!(v.eol_kind(1), 3);
        assert_eq!(v.eol_kind(2), 2);
        assert_eq!(v.eol_kind(3), 0);
        let v = LineVector::from_text("a\n");
        assert_eq!(v.line_count(), 2);
        assert_eq!(text(&v), "a\n");
    }

    #[test]
    fn raw_lines_are_not_materialized() {
        // §2.4-1: a load must leave every line in raw form.
        let v = LineVector::from_text("hello\r\nworld\n!");
        for l in 0..v.line_count() {
            assert!(v.array[v.phys(l)].is_raw());
        }
        // Views serve the same text the old eager decode produced.
        assert_eq!(v.line_view(0).to_vec(), chars("hello"));
        assert_eq!(v.line_len(0), 5);
        assert_eq!(v.line_bytes_len(0), 5);
    }

    #[test]
    fn from_raw_utf16_walker() {
        // "a\r\nb\rc\nd" in UTF-16LE: EOLs are 0A 00 / 0D 00 units.
        let units: Vec<u8> = "a\r\nb\rc\nd"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let v = LineVector::from_raw(Arc::new(units), encoding_rs::UTF_16LE, 0);
        assert_eq!(v.line_count(), 4);
        assert_eq!(text(&v), "a\r\nb\rc\nd");
        // astral pair = 1 char (4 UTF-8 bytes), 3 UTF-16 units —
        // the decode-domain (char) column semantics
        let units: Vec<u8> = "𝐀b"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let v = LineVector::from_raw(Arc::new(units), encoding_rs::UTF_16LE, 0);
        assert_eq!(v.line_len(0), 2);
        assert_eq!(v.line_bytes_len(0), 5);
    }

    #[test]
    fn raw_line_bytes_write_back() {
        // R5: raw lines hand out exactly their load-time bytes.
        let v = LineVector::from_text("hello\r\nworld");
        assert_eq!(v.line_count(), 2);
        assert_eq!(v.raw_line_bytes(0), Some(&b"hello"[..]));
        assert_eq!(v.raw_line_bytes(1), Some(&b"world"[..]));
        let mut v = LineVector::from_text("hello\r\nworld");
        v.insert_text(Pos { line: 1, col: 0 }, &chars("!"));
        assert_eq!(v.raw_line_bytes(1), None);
        assert_eq!(v.raw_line_bytes(0), Some(&b"hello"[..]));
    }

    #[test]
    fn a_line_view_is_shared_between_readers() {
        // The row painter, the caret and the hit test all read the same line
        // version: one decode, one allocation, shared handles.
        let v = LineVector::from_text("hello");
        let a = v.line_chars_rc(0);
        let b = v.line_chars_rc(0);
        assert!(Rc::ptr_eq(&a, &b));
        assert_eq!(&a[..], &chars("hello")[..]);
        // An edit drops the cached decode, so the next read is the new text.
        let mut v = v;
        v.insert_text(Pos { line: 0, col: 5 }, &chars("!"));
        let c = v.line_chars_rc(0);
        assert!(!Rc::ptr_eq(&a, &c));
        assert_eq!(&c[..], &chars("hello!")[..]);
    }

    #[test]
    fn a_line_reports_whether_every_character_takes_one_cell() {
        // The property a row's window uses to place itself by division.
        let v = LineVector::from_text("hello, world");
        assert!(v.line_uniform(0));
        // A tab, a wide glyph and a modifier all break the grid.
        for text in ["a\tb", "a\u{4e2d}b", "a\u{fe0f}b"] {
            let v = LineVector::from_text(text);
            assert!(!v.line_uniform(0), "{text:?}");
        }
        // An edit re-answers from the new text: the cached decode is gone.
        let mut v = v;
        v.insert_text(Pos { line: 0, col: 5 }, &chars("\t"));
        assert!(!v.line_uniform(0));
    }

    /// 跨行删除拼起来的行带终点行的行尾种类：字节域 [byte(start),
    /// byte(end)) 收走的是起点行的行尾，终点行的留在原地。混合行尾下
    /// 两边字节长不同，跟反了删除掉的字节数就和 (line, col) 域对不上。
    #[test]
    fn delete_joins_with_the_end_lines_kind() {
        let mut v = LineVector::from_text("ab\r\ncd\nef");
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ad\nef");
        assert_eq!(v.eol_kind(0), 2, "终点行的 LF 留在拼接行上");
        // 反向：LF 行删到 CRLF 行，拼接行带 CRLF。
        let mut v = LineVector::from_text("ab\ncd\r\nef");
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ad\r\nef");
        assert_eq!(v.eol_kind(0), 1);
        // 终点行没有行尾（文档末行）：拼接行也没有。
        let mut v = LineVector::from_text("ab\r\ncd");
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ad");
        assert_eq!(v.eol_kind(0), 0);
    }

    #[test]
    fn insert_plain_text() {
        let mut v = LineVector::new(1);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("hello"), 0);
        let end = v.insert_text(Pos { line: 0, col: 5 }, &chars(" world"));
        assert_eq!((end.line, end.col), (0, 11));
        assert_eq!(text(&v), "hello world");
    }

    #[test]
    fn insert_splits_lines() {
        let mut v = LineVector::new(1);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("ab"), 0);
        let end = v.insert_text(Pos { line: 0, col: 1 }, &chars("X\nY"));
        assert_eq!((end.line, end.col), (1, 1));
        assert_eq!(text(&v), "aX\nYb");
        assert_eq!(v.eol_kind(0), 2);
        assert_eq!(v.eol_kind(1), 0);
    }

    #[test]
    fn insert_at_eol_keeps_old_kind_on_last_line() {
        let mut v = LineVector::new(1);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("old"), 3);
        v.insert_text(Pos { line: 0, col: 0 }, &chars("new\n"));
        assert_eq!(text(&v), "new\nold\r");
        assert_eq!(v.eol_kind(0), 2);
        assert_eq!(v.eol_kind(1), 3);
    }

    #[test]
    fn insert_into_raw_line_materializes_only_it() {
        // Editing one line of a raw-loaded store: the edited line
        // becomes owned, every other stays raw.
        let mut v = LineVector::from_text("aaa\nbbb\nccc");
        v.insert_text(Pos { line: 1, col: 3 }, &chars("!"));
        assert_eq!(text(&v), "aaa\nbbb!\nccc");
        assert!(v.array[v.phys(0)].is_raw());
        assert!(!v.array[v.phys(1)].is_raw());
        assert!(v.array[v.phys(2)].is_raw());
    }

    #[test]
    fn materializing_keeps_the_eol_kind() {
        // EnsureDecoded preserves the flag bytes: the EOL kind rides
        // through the raw → owned transition.
        let mut v = LineVector::from_text("a\r\nb");
        assert_eq!(v.eol_kind(0), 1);
        v.insert_text(Pos { line: 0, col: 1 }, &chars("!"));
        assert_eq!(text(&v), "a!\r\nb");
        assert_eq!(v.eol_kind(0), 1);
    }

    #[test]
    fn owned_edits_reuse_the_buffer() {
        let mut v = LineVector::new(1);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("abcdef"), 0);
        let cap_after_set = v.array[v.phys(0)].cap as usize;
        assert!(cap_after_set >= MIN_OWNED_CAP);
        v.insert_text(Pos { line: 0, col: 6 }, &chars("ghij"));
        // growth doubles rather than nudging
        assert!(v.array[v.phys(0)].cap as usize >= cap_after_set * 2);
        assert_eq!(text(&v), "abcdefghij");
        v.delete_text(Pos { line: 0, col: 0 }, Pos { line: 0, col: 10 });
        // an emptied line stays owned — cap != 0 keeps the discriminant
        assert_ne!(v.array[v.phys(0)].cap, 0);
        assert_eq!(v.line_len(0), 0);
        assert_eq!(text(&v), "");
    }

    #[test]
    fn delete_same_line() {
        let mut v = LineVector::new(1);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("hello world"), 0);
        v.delete_text(Pos { line: 0, col: 5 }, Pos { line: 0, col: 11 });
        assert_eq!(text(&v), "hello");
    }

    #[test]
    fn delete_cross_line_joins() {
        let mut v = LineVector::new(2);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("ab"), 2);
        v.insert_line_at(1, LineT::default());
        v.set_line(1, &chars("cd"), 0);
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ad");
        assert_eq!(v.line_count(), 1);
    }

    #[test]
    fn delete_cross_line_on_raw_store() {
        let mut v = LineVector::from_text("ab\ncd");
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ad");
        assert!(!v.array[v.phys(0)].is_raw());
    }

    #[test]
    fn delete_last_eol_kind_zero_propagates() {
        let mut v = LineVector::new(3);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars("a"), 2);
        v.insert_line_at(1, LineT::default());
        v.set_line(1, &chars("b"), 0);
        v.insert_line_at(2, LineT::default());
        v.set_line(2, &chars("c"), 0);
        v.delete_text(Pos { line: 0, col: 1 }, Pos { line: 1, col: 1 });
        assert_eq!(text(&v), "ac");
        assert_eq!(v.eol_kind(0), 0);
    }

    #[test]
    fn collect_deleted_text_plain() {
        let mut v = LineVector::new(3);
        for (i, (t, k)) in [("a", 2u8), ("b", 2u8), ("c", 0u8)].iter().enumerate() {
            v.insert_line_at(i, LineT::default());
            v.set_line(i, &chars(t), *k);
        }
        let got = v.collect_deleted_text(Pos { line: 0, col: 1 }, Pos { line: 2, col: 1 }, 0);
        assert_eq!(got, "\nb\nc");
    }

    #[test]
    fn collect_deleted_text_on_raw_store() {
        let v = LineVector::from_text("aaa\nbbb\nccc");
        let got = v.collect_deleted_text(Pos { line: 0, col: 1 }, Pos { line: 2, col: 1 }, 0);
        assert_eq!(got, "aa\nbbb\nc");
    }

    #[test]
    fn scan_cap_splits_long_line() {
        let mut v = LineVector::new(1).with_scan_cap(2);
        v.insert_line_at(0, LineT::default());
        v.set_line(0, &chars(""), 0);
        v.insert_text(Pos { line: 0, col: 0 }, &chars("abcdef"));
        // No EOL ever found; each 2-char scan cap segment becomes its own
        // line with kind 0
        assert_eq!(v.line_count(), 3);
        assert_eq!(text(&v), "abcdef");
    }

    #[test]
    fn from_raw_scan_cap_splits_huge_line() {
        // The raw walker honors the same scan cap semantics: a
        // segment longer than the cap splits into continuation lines.
        let v = LineVector::from_raw_with_cap(Arc::new(b"abcdefgh".to_vec()), UTF_8, 0, 4, None);
        assert_eq!(v.line_count(), 2);
        assert_eq!(text(&v), "abcdefgh");
        assert_eq!(v.eol_kind(0), 0);
        assert_eq!(v.eol_kind(1), 0);
    }

    #[test]
    fn a_utf16_line_materializes_as_chars_not_as_bytes() {
        // "cd" in UTF-16LE is `63 00 64 00` — valid UTF-8 too, so the
        // no-copy path must not be taken for a store that is not UTF-8.
        let mut file = Vec::new();
        for u in "ab\rcd".encode_utf16() {
            file.extend_from_slice(&u.to_le_bytes());
        }
        let mut v = LineVector::from_raw(Arc::new(file), encoding_rs::UTF_16LE, 0);
        assert_eq!(v.line_len(1), 2);
        v.delete_text(Pos { line: 1, col: 0 }, Pos { line: 1, col: 2 });
        assert_eq!(v.line_len(1), 0);
        v.insert_text(Pos { line: 1, col: 0 }, &['Ç']);
        assert_eq!(v.line_chars_rc(1).iter().collect::<String>(), "Ç");
    }

    #[test]
    fn a_raw_lines_length_is_remembered_until_the_line_changes() {
        let mut v = LineVector::from_text("中文字符宽度\nsecond");
        assert_eq!(v.line_len(0), 6);
        // The memo pins the record it counted: line, offset, bytes, chars.
        assert_eq!(v.len_memo.get(), Some((0, 0, 18, 6)));
        assert_eq!(v.line_len(0), 6);
        v.insert_text(Pos { line: 0, col: 6 }, &['X']);
        assert_eq!(v.line_len(0), 7);
        assert_eq!(v.line_len(1), 6);
    }

    #[test]
    fn materializing_an_empty_line_leaves_it_owned() {
        // The capacity is the owned discriminant: an empty decode still
        // has to get a live buffer from `adopt`.
        let mut v = LineVector::from_text("\nabc");
        v.delete_text(Pos { line: 0, col: 0 }, Pos { line: 0, col: 0 });
        let r = &v.array[v.phys(0)];
        assert!(!r.is_raw());
        assert!(r.cap >= MIN_OWNED_CAP as u32);
        assert_eq!(v.line_len(0), 0);
    }

    #[test]
    fn a_large_line_grows_by_an_eighth_not_by_doubling() {
        let big = "a".repeat(1 << 16);
        let mut r = LineT::default();
        r.set_owned(&big.chars().collect::<Vec<_>>());
        r.reserve((1 << 16) + 1);
        assert_eq!(r.cap as usize, (1 << 16) + (1 << 13), "a 64K line adds an eighth");
        // Small lines keep the doubling that keeps typing reallocation-free.
        let mut small = LineT::default();
        small.set_owned(&['a'; 8]);
        small.reserve(9);
        assert_eq!(small.cap as usize, 16);
    }

    #[test]
    fn the_whole_file_block_goes_away_once_every_line_is_owned() {
        let mut v = LineVector::from_text("one\ntwo");
        assert!(v.raw.is_some());
        v.insert_text(Pos { line: 0, col: 0 }, &['x']);
        assert!(v.raw.is_some(), "the second line still reads from the block");
        v.insert_text(Pos { line: 1, col: 0 }, &['y']);
        assert!(v.raw.is_none(), "nothing is raw any more");
        assert_eq!(text(&v), "xone\nytwo");
    }

    #[test]
    fn many_edits_across_the_store_stay_consistent() {
        // Gap churn: the memmove protocol must survive inserts and
        // deletes scattered over the store (every freed slot was live,
        // every moved record lands exactly once).
        let mut v = LineVector::from_text("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7");
        for round in 0..8 {
            let line = round % v.line_count();
            v.insert_text(Pos { line, col: 0 }, &chars(format!("x{round}\n").as_str()));
            let last = v.line_count() - 1;
            // delete_text's column range must be valid — trim the last
            // char it has.
            let ll = v.line_len(last);
            if ll > 0 {
                v.delete_text(Pos { line: last, col: ll - 1 }, Pos { line: last, col: ll });
            }
        }
        // Re-derive the content and put it back together: no crashes,
        // no duplicated or lost records.
        let count = v.line_count();
        assert!(count >= 8);
        for l in 0..count {
            let _ = v.line_len(l);
        }
    }
}
