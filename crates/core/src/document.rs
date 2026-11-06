use std::rc::Rc;

use encoding_rs::Encoding;
use parking_lot::RwLock;

use crate::index::ByteIndex;
use crate::linedoc::{LineVector, Pos};

/// dg-large 后台扫描/搜索事件与句柄（重导出：上层轮询大文件状态
/// 时无需直接依赖 dg-large）。
pub use dg_large::{ScanEvent, SearchEvent, SearchHandle, SearchOptions};

/// End-of-line mode: PC (CR+LF),
/// Unix (LF) or classic Mac (CR).
/// per-document EOL setting
/// (Document menu → EOL).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Eol {
    Crlf,
    #[default]
    Lf,
    Cr,
}

impl Eol {
    /// The wire kind the LineVector
    /// stores for this mode.
    fn wire(self) -> u8 {
        match self {
            Eol::Crlf => 1,
            Eol::Lf => 2,
            Eol::Cr => 3,
        }
    }

    /// Normalizes every line ending
    /// in `text` to this mode.
    pub fn convert(self, text: &str) -> String {
        let lf = text.replace("\r\n", "\n").replace('\r', "\n");
        match self {
            Eol::Lf => lf,
            Eol::Crlf => lf.replace('\n', "\r\n"),
            Eol::Cr => lf.replace('\n', "\r"),
        }
    }

    /// Detects the dominant line
    /// ending in `text` (LF when
    /// the text has none).
    pub fn detect(text: &str) -> Eol {
        let (mut crlf, mut lf, mut cr) = (0, 0, 0);
        let mut prev_cr = false;
        for ch in text.chars() {
            match ch {
                '\n' => {
                    if prev_cr {
                        crlf += 1;
                    } else {
                        lf += 1;
                    }
                    prev_cr = false;
                }
                '\r' => prev_cr = true,
                _ => {
                    if prev_cr {
                        cr += 1;
                    }
                    prev_cr = false;
                }
            }
        }
        if prev_cr {
            cr += 1;
        }
        if crlf == 0 && lf == 0 && cr == 0 {
            return Eol::Lf;
        }
        if crlf >= lf && crlf >= cr {
            Eol::Crlf
        } else if lf >= cr {
            Eol::Lf
        } else {
            Eol::Cr
        }
    }

    /// Same vote over the raw block in its encoding — the load path
    /// decides the document EOL without decoding the file. UTF-16/32
    /// units are read at the encoding's stride; for the byte-oriented
    /// codepages (ANSI/GBK family) 0x0A/0x0D can never be trail bytes,
    /// so a straight byte walk is exact.
    pub fn detect_bytes(raw: &[u8], enc: &Encoding, bom_len: usize) -> Eol {
        let (stride, be) = if enc == encoding_rs::UTF_16LE {
            (2, false)
        } else if enc == encoding_rs::UTF_16BE {
            (2, true)
        } else {
            (1, false)
        };
        let unit = |off: usize| -> u32 {
            if off + stride > raw.len() {
                return 0;
            }
            match stride {
                1 => raw[off] as u32,
                2 => {
                    let s = [raw[off], raw[off + 1]];
                    if be {
                        u16::from_be_bytes(s) as u32
                    } else {
                        u16::from_le_bytes(s) as u32
                    }
                }
                _ => {
                    if off + 4 > raw.len() {
                        return 0;
                    }
                    let s = [raw[off], raw[off + 1], raw[off + 2], raw[off + 3]];
                    if be {
                        u32::from_be_bytes(s)
                    } else {
                        u32::from_le_bytes(s)
                    }
                }
            }
        };
        let (mut crlf, mut lf, mut cr) = (0u64, 0u64, 0u64);
        let mut off = bom_len.min(raw.len());
        while off + stride <= raw.len() {
            match unit(off) {
                13 => {
                    if unit(off + stride) == 10 {
                        crlf += 1;
                        off += 2 * stride;
                    } else {
                        cr += 1;
                        off += stride;
                    }
                }
                10 => {
                    lf += 1;
                    off += stride;
                }
                _ => off += stride,
            }
        }
        if crlf == 0 && lf == 0 && cr == 0 {
            return Eol::Lf;
        }
        if crlf >= lf && crlf >= cr {
            Eol::Crlf
        } else if lf >= cr {
            Eol::Lf
        } else {
            Eol::Cr
        }
    }
}

fn wire_to_eol(kind: u8) -> Option<Eol> {
    match kind {
        1 => Some(Eol::Crlf),
        2 => Some(Eol::Lf),
        3 => Some(Eol::Cr),
        _ => None,
    }
}

/// Pushes a line's terminator chars (its own stored kind) — the
/// range-text walk's separator.
fn push_term(out: &mut String, kind: u8) {
    match kind {
        1 => out.push_str("\r\n"),
        2 => out.push('\n'),
        3 => out.push('\r'),
        _ => {}
    }
}

/// The line terminator's bytes **in the file's encoding**.
fn save_eol_bytes(kind: u8, enc: &'static Encoding, codepage: Option<u16>) -> &'static [u8] {
    if let Some(cp) = codepage {
        // The seam's two UTF-32 pages write 4-byte terminators; every
        // other codepage page is byte-wise ASCII EOLs.
        if cp == 12000 {
            return match kind {
                1 => &[0x0D, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00],
                2 => &[0x0A, 0x00, 0x00, 0x00],
                3 => &[0x0D, 0x00, 0x00, 0x00],
                _ => b"",
            };
        }
        if cp == 12001 {
            return match kind {
                1 => &[0x00, 0x00, 0x00, 0x0D, 0x00, 0x00, 0x00, 0x0A],
                2 => &[0x00, 0x00, 0x00, 0x0A],
                3 => &[0x00, 0x00, 0x00, 0x0D],
                _ => b"",
            };
        }
        return match kind {
            1 => b"\r\n",
            2 => b"\n",
            3 => b"\r",
            _ => b"",
        };
    }
    match (kind, enc.name()) {
        (1, "UTF-16LE") => &[0x0D, 0x00, 0x0A, 0x00],
        (2, "UTF-16LE") => &[0x0A, 0x00],
        (3, "UTF-16LE") => &[0x0D, 0x00],
        (1, "UTF-16BE") => &[0x00, 0x0D, 0x00, 0x0A],
        (2, "UTF-16BE") => &[0x00, 0x0A],
        (3, "UTF-16BE") => &[0x00, 0x0D],
        (1, _) => b"\r\n",
        (2, _) => b"\n",
        (3, _) => b"\r",
        _ => b"",
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Single-line decode cache: the last decoded line, its index, its length
/// and its capacity. So do we.
#[derive(Default)]
struct LineCache {
    idx: Option<usize>,
    text: String,
}

/// Ordinary files (< the large-file threshold) live here; the layout/paint paths
/// address everything through the [`Document`] facade.
struct MemDoc {
    vec: LineVector,
    cache: RwLock<LineCache>,
    /// Sparse UTF-8 byte checkpoints (256 lines apart), the
    /// tree-sitter bridge's only flat coordinate. Rebuilt lazily
    /// after mutations — see [`crate::index::ByteIndex`].
    bytes: RwLock<ByteIndex>,
    loading: Arc<AtomicBool>,
    /// Detected source encoding —
    /// save re-encodes with it
    /// (P8; this arm keeps
    /// the file format per
    /// document).
    encoding: &'static Encoding,
    /// Whether the source carried
    /// a BOM (re-emitted on save).
    encoding_bom: bool,
    /// A Windows codepage the raw bytes decode/encode through instead
    /// of `encoding` — the conversion seam's carrier, driven through
    /// MultiByteToWideChar/WideCharToMultiByte with the same id.
    /// `None` = the encoding_rs path.
    codepage: Option<u16>,
}

/// the Large arm is the large-file mode's read-only v1.
pub struct Document {
    backend: Backend,
    /// The large arm's decoded lines. Its rows are read a line at a time
    /// through the overlay, so the painter, the caret and the hit test each
    /// rebuild whatever they touch unless the decode is shared.
    large_views: std::cell::RefCell<LargeViews>,
}

/// One version of the large arm's decodes: the overlay's edit count they were
/// read under, the lines, and the characters they hold — the bound that keeps a
/// table of megabyte-long lines from outgrowing the file it caches.
#[derive(Default)]
struct LargeViews {
    version: u64,
    lines: std::collections::HashMap<usize, (Rc<Vec<char>>, bool)>,
    chars: usize,
}

/// How many lines the large arm's decode cache holds, and how many characters
/// all of them together. Past either bound the table starts over.
const LARGE_VIEW_MAX_LINES: usize = 4096;
const LARGE_VIEW_MAX_CHARS: usize = 8 * 1024 * 1024;

enum Backend {
    Mem(MemDoc),
    Large(Box<dg_large::LargeDoc>),
}

impl Default for Document {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Document {
    pub fn empty() -> Self {
        Self::default()
    }

    /// The loader's landing point: the decoded text, split into
    /// lines with the EOL kinds the bytes carried.
    pub fn from_text(text: &str) -> Self {
        Self {
            backend: Backend::Mem(MemDoc::from_text(text)),
            large_views: std::cell::RefCell::default(),
        }
    }

    /// The loader's raw landing point: the whole file as
    /// original-encoding bytes — decoded lazily per line, so opening
    /// costs the one block plus the line records and nothing else.
    /// `bom_len` bytes at the front are skipped (the reader's
    /// post-BOM window).
    pub fn from_bytes(
        raw: std::sync::Arc<Vec<u8>>,
        enc: &'static Encoding,
        bom_len: usize,
    ) -> Self {
        Self {
            backend: Backend::Mem(MemDoc::from_bytes(raw, enc, bom_len)),
            large_views: std::cell::RefCell::default(),
        }
    }

    /// UTF-8 convenience constructor (lossy on invalid input).
    pub fn from_utf8(bytes: &[u8]) -> Self {
        Self::from_text(&String::from_utf8_lossy(bytes))
    }

    /// 大文件模式：mmap/pread 底座 + 后台行索引，立即返回（首屏
    /// 与尾区已可用）。v1 只读；编辑接口被门面拦截为 no-op。
    pub fn open_large(path: &std::path::Path) -> Result<Self, String> {
        Ok(Self {
            backend: Backend::Large(Box::new(dg_large::LargeDoc::open(path)?)),
            large_views: std::cell::RefCell::default(),
        })
    }

    /// True when the document is backed by the large-file store.
    pub fn is_large(&self) -> bool {
        matches!(self.backend, Backend::Large(_))
    }

    /// The large-file store, when this document is one.
    pub fn large(&self) -> Option<&dg_large::LargeDoc> {
        match &self.backend {
            Backend::Large(d) => Some(d),
            _ => None,
        }
    }

    /// 大文件后台扫描事件（非大文件恒 `None`）。
    pub fn poll_large_event(&self) -> Option<ScanEvent> {
        self.large().and_then(|d| d.poll_scan_event())
    }

    /// 大文件流式搜索（M4）。非大文件或查询被拒（空 needle、非
    /// UTF-8 上的正则/忽略大小写）返回 `None`。
    pub fn start_large_search(
        &self,
        needle: &str,
        opts: SearchOptions,
    ) -> Option<SearchHandle> {
        self.large().and_then(|d| d.start_search(needle, opts))
    }

    pub fn large_mut(&mut self) -> Option<&mut dg_large::LargeDoc> {
        match &mut self.backend {
            Backend::Large(d) => Some(d),
            _ => None,
        }
    }
}

impl MemDoc {
    fn from_text(text: &str) -> Self {
        let d = Self {
            vec: LineVector::from_text(text),
            cache: RwLock::new(LineCache::default()),
            bytes: RwLock::new(ByteIndex::new()),
            loading: Arc::new(AtomicBool::new(false)),
            encoding: encoding_rs::UTF_8,
            encoding_bom: false,
            codepage: None,
        };
        // The byte checkpoints build lazily on the first bridge query;
        // nothing document-sized happens at construction.
        d
    }

    fn from_bytes(raw: Arc<Vec<u8>>, enc: &'static Encoding, bom_len: usize) -> Self {
        Self {
            vec: LineVector::from_raw(raw, enc, bom_len),
            cache: RwLock::new(LineCache::default()),
            bytes: RwLock::new(ByteIndex::new()),
            loading: Arc::new(AtomicBool::new(false)),
            encoding: enc,
            encoding_bom: bom_len > 0,
            codepage: None,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading.load(Ordering::SeqCst)
    }

    /// The detected source encoding
    /// (save re-encodes with it).
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// Whether the source carried a
    /// BOM (re-emitted on save).
    pub fn has_bom(&self) -> bool {
        self.encoding_bom
    }

    /// The document's codepage when its bytes decode through the Win32
    /// pair (the conversion seam), `None` for the encoding_rs path.
    pub fn codepage(&self) -> Option<u16> {
        self.codepage
    }

    /// 转换编码's document side: decode the block with the current
    /// encoding and re-encode with the target. The **bytes change,
    /// the text does not** — the buffer keeps its raw domain, now
    /// written in the new encoding, and the view caches are rebuilt
    /// empty. A same-encoding call only moves the BOM flag (the bytes
    /// are identical).
    pub fn set_encoding(
        &mut self,
        enc: &'static Encoding,
        bom: bool,
    ) {
        if self.codepage.is_none() && self.encoding == enc {
            self.encoding_bom = bom;
            return;
        }
        let text = self.to_text();
        let bytes = crate::encoding::encode_text(&text, enc, false);
        self.vec = LineVector::from_raw(std::sync::Arc::new(bytes), enc, 0);
        self.cache = RwLock::new(LineCache::default());
        self.bytes = RwLock::new(ByteIndex::new());
        self.encoding = enc;
        self.codepage = None;
        self.encoding_bom = bom;
    }

    /// The seam's landing point: convert to a codepage encoding_rs
    /// cannot reproduce. Same transcode, the re-encode through
    /// `WideCharToMultiByte` and the rebuilt store decoding through
    /// `MultiByteToWideChar` from here on (edits re-encode, and save
    /// writes the codepage's bytes).
    pub fn set_codepage(&mut self, cp: u16, bom: bool) {
        if self.codepage == Some(cp) {
            self.encoding_bom = bom;
            return;
        }
        let text = self.to_text();
        let bytes = crate::encoding::encode_codepage(cp, &text);
        self.vec = LineVector::from_raw_codepage(std::sync::Arc::new(bytes), cp, 0);
        self.cache = RwLock::new(LineCache::default());
        self.bytes = RwLock::new(ByteIndex::new());
        // The encoding_rs static stays as a display marker and is never
        // consulted while `codepage` is `Some`; it must not be UTF-8 or
        // a UTF-16 static, whose byte-pattern fast paths would misread
        // the codepage bytes.
        self.encoding = encoding_rs::WINDOWS_1252;
        self.codepage = Some(cp);
        self.encoding_bom = bom;
    }

    /// Set by the loader thread — while true, every read returns
    /// empty.
    pub fn set_loading(&self, v: bool) {
        self.loading.store(v, Ordering::SeqCst);
    }

    pub fn loading_flag(&self) -> Arc<AtomicBool> {
        self.loading.clone()
    }

    pub fn line_count(&self) -> usize {
        self.vec.line_count()
    }

    /// Whether the line carries its own terminator — the
    /// `LineT` EOL kind (non-zero, the record's top two bits) the
    /// 字符统计 walk counts physical lines with.
    pub fn line_has_eol(&self, line: usize) -> bool {
        self.vec.eol_kind(line) != 0
    }

    /// The UTF-8 byte offset of `line`'s first char (prior lines'
    /// terminators included) — the tree-sitter bridge's only flat
    /// coordinate. Sparse checkpoints ([`crate::index::ByteIndex`]),
    /// rebuilt lazily after any mutation.
    pub fn line_byte_offset(&self, line: usize) -> usize {
        let mut g = self.bytes.write();
        g.line_offset(&self.vec, line)
    }

    /// The document's UTF-8 byte length (the tree-sitter parse size).
    pub fn total_bytes(&self) -> usize {
        let mut g = self.bytes.write();
        g.total_bytes(&self.vec)
    }

    /// The line a UTF-8 byte offset falls in (a terminator's bytes
    /// belong to their own line).
    pub fn byte_line(&self, byte: usize) -> usize {
        let mut g = self.bytes.write();
        g.byte_line(&self.vec, byte)
    }

    /// UTF-8 byte offset of `col` on `line` — the in-line half of the
    /// tree-sitter byte domain. `col` clamps to the line length.
    pub fn line_byte_col(&self, line: usize, col: usize) -> usize {
        let len = self.vec.line_len(line);
        self.line_byte_offset(line)
            + self.vec.line_view(line)[..col.min(len)]
                .iter()
                .map(|c| c.len_utf8())
                .sum::<usize>()
    }

    /// The text of a `(line, col)` range, each interior line closed
    /// by the terminator it currently carries — the inverse read of
    /// [`crate::undo::text_end_pos`]
    /// form. Coordinates clamp to the document as it stands.
    pub fn range_text(&self, start: Pos, end: Pos) -> String {
        let last = self.vec.line_count().saturating_sub(1);
        let sl = start.line.min(last);
        let el = end.line.max(sl).min(last);
        let mut out = String::new();
        for line in sl..=el {
            let len = self.vec.line_len(line);
            let s = if line == sl { start.col.min(len) } else { 0 };
            let e = if line == el {
                end.col.min(len).max(s)
            } else {
                len
            };
            out.extend(self.vec.line_view(line)[s..e].iter());
            if line != el {
                push_term(&mut out, self.vec.eol_kind(line));
            }
        }
        out
    }

    /// The whole document as text, each line carrying the
    /// terminator it was loaded or edited with.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for line in 0..self.vec.line_count() {
            out.extend(self.vec.line_view(line).iter());
            push_term(&mut out, self.vec.eol_kind(line));
        }
        out
    }

    /// The streaming save walk: BOM first, then
    /// per line — raw lines write back their original bytes straight
    /// from the load block, owned (edited) lines re-encode in the
    /// source encoding, and every line keeps its own terminator
    /// (LineT bits 6-7; a file without a trailing EOL saves without
    /// one). Nothing document-sized is ever allocated.
    fn save_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<u64> {
        let mut n: u64 = 0;
        if self.encoding_bom {
            let bom = match self.codepage {
                Some(cp) => crate::encoding::codepage_bom(cp),
                None => crate::encoding::bom_bytes(self.encoding),
            };
            if let Some(b) = bom {
                w.write_all(b)?;
                n += b.len() as u64;
            }
        }
        for line in 0..self.vec.line_count() {
            let encoded;
            let content: &[u8] = match self.vec.raw_line_bytes(line) {
                Some(raw) => raw,
                None => {
                    let mut s = String::new();
                    s.extend(self.vec.line_view(line).iter());
                    encoded = match self.codepage {
                        Some(cp) => crate::encoding::encode_codepage(cp, &s),
                        None => crate::encoding::encode_text(&s, self.encoding, false),
                    };
                    &encoded
                }
            };
            w.write_all(content)?;
            n += content.len() as u64;
            let term = save_eol_bytes(self.vec.eol_kind(line), self.encoding, self.codepage);
            w.write_all(term)?;
            n += term.len() as u64;
        }
        Ok(n)
    }

    /// The terminator a line carries, `None` when it has none.
    pub fn line_eol(&self, line: usize) -> Option<Eol> {
        wire_to_eol(self.vec.eol_kind(line))
    }

    /// The raw EOL kind byte (the per-line state's terminator code:
    /// 1=CRLF, 2=LF, 3=CR, 0=none). The
    /// gutter reads it for its `~` / `.` continuation
    /// markers, where 0 means "no terminator".
    pub fn eol_kind(&self, line: usize) -> u8 {
        self.vec.eol_kind(line)
    }

    /// 行修改状态: the line's two-bit edit state — 0 untouched, 1 modified
    /// since the last save, 2 written back by that save.
    pub fn line_state(&self, line: usize) -> u8 {
        self.vec.line_state(line)
    }

    /// The marked lines as `(first, last, state)` runs in line order.
    pub fn state_runs(&self) -> Vec<(u32, u32, u8)> {
        self.vec.state_runs()
    }

    /// The save's pass over the whole document: modified lines become
    /// "saved". Called once the bytes are on disk.
    pub fn mark_all_saved(&mut self) {
        self.vec.mark_all_saved();
    }

    /// Rewrites every terminator to `eol` (Document menu →
    /// EOL). The final line keeps its missing terminator.
    pub fn set_eol_all(&mut self, eol: Eol) {
        let kind = eol.wire();
        let last = self.vec.line_count().saturating_sub(1);
        for line in 0..self.vec.line_count() {
            if line != last && self.vec.eol_kind(line) != 0 {
                self.vec.set_eol_kind(line, kind);
                // 行修改状态: the terminator of this line changed, so the
                // line is modified until a save writes it back.
                self.vec.mark_line_dirty(line);
            }
        }
        self.cache.write().idx = None;
        // Line byte spans may have moved (1 vs 2 bytes): the
        // tree-sitter checkpoints rebuild on the next query.
        self.bytes.write().mark_dirty();
    }

    /// The line-text read: returns `false` and clears `out`
    /// while loading; otherwise decodes on demand through the
    /// single-line cache. A line cached by the store's own view
    /// cache (a just-painted raw line) is served from there instead
    /// of re-decoding.
    pub fn line_text(&self, line: usize, out: &mut String) -> bool {
        if self.is_loading() {
            out.clear();
            return false;
        }
        {
            let c = self.cache.read();
            if c.idx == Some(line) {
                out.push_str(&c.text);
                return true;
            }
        }
        if self.vec.cached_view_matches(line, out) {
            let mut w = self.cache.write();
            w.idx = Some(line);
            w.text = out.clone();
            return true;
        }
        let text: String = if line < self.vec.line_count() {
            self.vec.line_view(line).iter().collect()
        } else {
            String::new()
        };
        let mut w = self.cache.write();
        w.idx = Some(line);
        w.text = text.clone();
        out.push_str(&text);
        true
    }

    /// Number of chars on a line (excluding the line terminator).
    pub fn line_len(&self, line: usize) -> usize {
        self.vec.line_len(line)
    }

    /// The core store, for the algorithms that work in
    /// (line, col) directly (undo collection parity).
    pub fn line_vector(&self) -> &LineVector {
        &self.vec
    }

    /// Collect the text of a (line, col) range as it stands —
    /// the undo stack's record of what a delete removed.
    pub fn collect_deleted_text(
        &self,
        start: Pos,
        end: Pos,
        eol_kind: u8,
    ) -> String {
        self.vec.collect_deleted_text(start, end, eol_kind)
    }

    /// Edit in (line, col) coordinates
    /// form, the facade the undo stack speaks. Positions clamp to the
    /// document as it stands (keystrokes may arrive referencing
    /// positions a prior edit removed); the returned end position
    /// derives from the inserted text alone
    /// ([`crate::undo::text_end_pos`], no re-measure of the doc).
    pub fn edit_pos(&mut self, start: Pos, end: Pos, text: &str) -> Pos {
        self.cache.write().idx = None;
        let last = self.vec.line_count().saturating_sub(1);
        let clamp = |p: Pos| -> Pos {
            let line = p.line.min(last);
            Pos {
                line,
                col: p.col.min(self.vec.line_len(line)),
            }
        };
        let (start, end) = (clamp(start), clamp(end));
        self.vec.clear_views();
        let out = crate::undo::text_end_pos(start, text);
        if start != end {
            self.vec.delete_text(start, end);
        }
        if !text.is_empty() {
            let chars: Vec<char> = text.chars().collect();
            self.vec.insert_text(start, &chars);
        }
        // 行修改状态: the edit makes the lines it touches dirty. The range is
        // the ones the text lands on (a deletion merges into the first), taken
        // after the edit so the indices are the document's own.
        let last = self.vec.line_count().saturating_sub(1);
        let touched = start.line + crate::undo::text_lines(text).max(0) as usize;
        for line in start.line..=touched.min(last) {
            self.vec.mark_line_dirty(line);
        }
        // The tree-sitter byte checkpoints rebuild on the next query.
        self.bytes.write().mark_dirty();
        out
    }
}

/// The consumer-facing surface: every method dispatches to the Mem arm or
/// the Large arm (large-file mode, read-only for now). Mem behavior is
/// byte-identical to the pre-large-refactor `Document`.
impl Document {
    pub fn is_loading(&self) -> bool {
        match &self.backend {
            Backend::Mem(m) => m.is_loading(),
            // 大文件模式的行读取在扫描期间即可用；扫描进度走
            // [`Document::large`] 的专用接口，不占用加载语义。
            Backend::Large(_) => false,
        }
    }

    pub fn set_loading(&self, v: bool) {
        if let Backend::Mem(m) = &self.backend {
            m.set_loading(v);
        }
    }

    pub fn loading_flag(&self) -> Arc<AtomicBool> {
        match &self.backend {
            Backend::Mem(m) => m.loading_flag(),
            Backend::Large(_) => Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn encoding(&self) -> &'static Encoding {
        match &self.backend {
            Backend::Mem(m) => m.encoding(),
            Backend::Large(d) => d.encoding(),
        }
    }

    pub fn has_bom(&self) -> bool {
        match &self.backend {
            Backend::Mem(m) => m.has_bom(),
            Backend::Large(d) => d.has_bom(),
        }
    }

    pub fn set_encoding(&mut self, enc: &'static Encoding, bom: bool) {
        if let Backend::Mem(m) = &mut self.backend {
            m.set_encoding(enc, bom);
        }
    }

    /// The document's codepage when its bytes decode through the Win32
    /// pair. Large documents never carry one (the conversion seam is
    /// mem-store only, like `set_encoding`'s large arm).
    pub fn codepage(&self) -> Option<u16> {
        match &self.backend {
            Backend::Mem(m) => m.codepage(),
            Backend::Large(_) => None,
        }
    }

    /// 转换编码's seam landing: convert to a Windows codepage the
    /// encoding_rs path cannot reproduce. No-op on the large store,
    /// the way `set_encoding`'s large arm is.
    pub fn set_codepage(&mut self, cp: u16, bom: bool) {
        if let Backend::Mem(m) = &mut self.backend {
            m.set_codepage(cp, bom);
        }
    }

    pub fn line_count(&self) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.line_count(),
            Backend::Large(d) => d.line_count(),
        }
    }

    pub fn line_has_eol(&self, line: usize) -> bool {
        match &self.backend {
            Backend::Mem(m) => m.line_has_eol(line),
            Backend::Large(d) => d.line_has_eol(line),
        }
    }

    /// UTF-8 byte offset of `line`'s first char — the tree-sitter
    /// bridge's coordinate. Mem-only: the large arm runs the
    /// line-based lexer and never enters the byte bridge.
    pub fn line_byte_offset(&self, line: usize) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.line_byte_offset(line),
            Backend::Large(_) => panic!("byte bridge: not on the large arm"),
        }
    }

    /// The document's UTF-8 byte length (the tree-sitter parse size).
    /// Both arms answer — the large store knows its own length, and the
    /// size gates (语法附加的体积闸、切换文件类型) read this without
    /// wanting a byte coordinate, so they must not trip the byte-bridge
    /// panic just to learn how big the file is.
    pub fn total_bytes(&self) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.total_bytes(),
            Backend::Large(d) => d.total_bytes() as usize,
        }
    }

    /// The line a UTF-8 byte offset falls in.
    pub fn byte_line(&self, byte: usize) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.byte_line(byte),
            Backend::Large(_) => panic!("byte bridge: not on the large arm"),
        }
    }

    /// UTF-8 byte offset of `col` on `line` (tree-sitter bridge).
    pub fn line_byte_col(&self, line: usize, col: usize) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.line_byte_col(line, col),
            Backend::Large(_) => panic!("byte bridge: not on the large arm"),
        }
    }

    /// The text of a `(line, col)` range. The large arm converts
    /// through its overlay.
    pub fn range_text(&self, start: Pos, end: Pos) -> String {
        match &self.backend {
            Backend::Mem(m) => m.range_text(start, end),
            Backend::Large(d) => {
                let to_char = |p: Pos| -> usize {
                    d.line_to_char(p.line).unwrap_or_else(|| d.len_chars()) + p.col
                };
                d.text_range(
                    to_char(start).min(d.len_chars()),
                    to_char(end).min(d.len_chars()),
                )
            }
        }
    }

    /// Whole-document text. O(N) on both arms — the large arm
    /// materializes the entire file; large-mode callers (find/syntax
    /// sync) must be gated off before reaching this.
    pub fn to_text(&self) -> String {
        match &self.backend {
            Backend::Mem(m) => m.to_text(),
            Backend::Large(d) => d.text_range(0, d.len_chars()),
        }
    }

    /// Stream the document to `w` in the source
    /// encoding without materializing the full text — raw lines go
    /// back byte-for-byte, owned lines re-encode, each line keeps its
    /// own terminator. Returns the byte count. (Large documents keep
    /// their own `LargeDoc::write_merged` path.)
    pub fn save_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<u64> {
        match &self.backend {
            Backend::Mem(m) => m.save_to(w),
            Backend::Large(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "large documents save through LargeDoc::write_merged",
            )),
        }
    }

    pub fn line_eol(&self, line: usize) -> Option<Eol> {
        match &self.backend {
            Backend::Mem(m) => m.line_eol(line),
            Backend::Large(d) => wire_to_eol(d.eol_kind(line)),
        }
    }

    /// Chars the line's terminator contributes (2 for CRLF, 1 for
    /// LF/CR, 0 when none) — the selection/character-count walks
    /// that reproduce the old flat span length.
    pub fn line_eol_chars(&self, line: usize) -> usize {
        match self.eol_kind(line) {
            1 => 2,
            2 | 3 => 1,
            _ => 0,
        }
    }

    pub fn eol_kind(&self, line: usize) -> u8 {
        match &self.backend {
            Backend::Mem(m) => m.eol_kind(line),
            Backend::Large(d) => d.eol_kind(line),
        }
    }

    /// 行修改状态: the line's two-bit edit state. The large arm keeps no
    /// per-line records, so every line reads as untouched there — its strip
    /// shows no marks.
    pub fn line_state(&self, line: usize) -> u8 {
        match &self.backend {
            Backend::Mem(m) => m.line_state(line),
            Backend::Large(_) => 0,
        }
    }

    /// The marked lines as `(first, last, state)` runs in line order; empty on
    /// the large arm, which keeps no per-line records. Costs the run count, not
    /// the document's.
    pub fn state_runs(&self) -> Vec<(u32, u32, u8)> {
        match &self.backend {
            Backend::Mem(m) => m.state_runs(),
            Backend::Large(_) => Vec::new(),
        }
    }

    pub fn mark_all_saved(&mut self) {
        if let Backend::Mem(m) = &mut self.backend {
            m.mark_all_saved();
        }
    }

    /// Rewrites every terminator (Document menu → EOL). No-op on the
    /// large arm (read-only for now).
    pub fn set_eol_all(&mut self, eol: Eol) {
        if let Backend::Mem(m) = &mut self.backend {
            m.set_eol_all(eol);
        }
    }

    pub fn line_text(&self, line: usize, out: &mut String) -> bool {
        match &self.backend {
            Backend::Mem(m) => m.line_text(line, out),
            Backend::Large(d) => d.line_text(line, out),
        }
    }

    /// The line's chars as a shared handle: repeat readers (the row
    /// painter, the caret, the hit test) share one decode per line
    /// version instead of re-collecting the text. `None` while the
    /// document still loads or past its last line.
    pub fn line_chars(&self, line: usize) -> Option<Rc<Vec<char>>> {
        match &self.backend {
            Backend::Mem(m) => {
                if m.is_loading() || line >= m.line_count() {
                    return None;
                }
                Some(m.vec.line_chars_rc(line))
            }
            Backend::Large(d) => Some(self.large_view(line, d)?.0),
        }
    }

    /// Whether every character of `line` takes exactly one cell — the
    /// property a row uses to place its window by division rather than by
    /// measuring the text before it. It rides the large arm's decode, so
    /// answering costs what reading the line already costs.
    pub fn line_uniform(&self, line: usize) -> bool {
        match &self.backend {
            Backend::Mem(m) => {
                !m.is_loading() && line < m.line_count() && m.vec.line_uniform(line)
            }
            Backend::Large(d) => self.large_view(line, d).map_or(false, |(_, u)| u),
        }
    }

    /// The large arm's line, decoded once per version: the overlay's edit
    /// count keys the cache, so an entry cannot outlive the text it was read
    /// from, and a reload — which opens a fresh document — starts empty.
    fn large_view(&self, line: usize, d: &dg_large::LargeDoc) -> Option<(Rc<Vec<char>>, bool)> {
        let version = d.edit_count();
        {
            let c = self.large_views.borrow();
            if c.version == version {
                if let Some(v) = c.lines.get(&line) {
                    return Some(v.clone());
                }
            }
        }
        let mut buf = String::new();
        if !d.line_text(line, &mut buf) {
            return None;
        }
        let mut chars = Vec::with_capacity(buf.len());
        for ch in buf.chars() {
            chars.push(ch);
        }
        let uniform = crate::linedoc::uniform_cells(&chars);
        let v = (Rc::new(chars), uniform);
        let mut c = self.large_views.borrow_mut();
        if c.version != version {
            c.version = version;
            c.lines.clear();
            c.chars = 0;
        }
        if c.lines.len() >= LARGE_VIEW_MAX_LINES || c.chars + v.0.len() > LARGE_VIEW_MAX_CHARS {
            c.lines.clear();
            c.chars = 0;
        }
        c.chars += v.0.len();
        c.lines.insert(line, v.clone());
        Some(v)
    }

    pub fn line_len(&self, line: usize) -> usize {
        match &self.backend {
            Backend::Mem(m) => m.line_len(line),
            Backend::Large(d) => d.line_len(line),
        }
    }

    /// 大文件编辑预检（M6 守卫）：`Some(err)` = 该编辑会被拒绝。上层
    /// （apply_edit）在推入 undo 栈前调用，避免"栈里记录了未生效的
    /// 编辑"造成 undo 视图漂移。
    pub fn large_edit_would_fail(
        &self,
        char_start: usize,
        char_end: usize,
        text: &str,
    ) -> Option<dg_large::EditError> {
        self.large()
            .map(|d| d.edit_would_fail(char_start, char_end, text))
            .unwrap_or(None)
    }

    /// The Mem store, for the algorithms that work in (line, col)
    /// directly (undo collection parity). Panics on the large arm —
    /// those algorithms are editing-path only.
    pub fn line_vector(&self) -> &LineVector {
        match &self.backend {
            Backend::Mem(m) => m.line_vector(),
            Backend::Large(_) => panic!("line_vector: large documents are read-only (v1)"),
        }
    }

    /// The undo stack's record of what a delete removed. The large arm
    /// reads through the overlay（text_range 已是编辑态视图）。
    pub fn collect_deleted_text(
        &self,
        start: Pos,
        end: Pos,
        eol_kind: u8,
    ) -> String {
        match &self.backend {
            Backend::Mem(m) => m.collect_deleted_text(start, end, eol_kind),
            Backend::Large(d) => {
                let to_char = |p: Pos| -> usize {
                    d.line_to_char(p.line).unwrap_or_else(|| d.len_chars()) + p.col
                };
                d.text_range(to_char(start).min(d.len_chars()), to_char(end).min(d.len_chars()))
            }
        }
    }

    /// Edit in (line, col) coordinates
    /// the only edit entry the flat-domain retirement leaves. Returns
    /// the position `text` ends at (the caret after a plain insert).
    /// The large arm converts to its overlay's flat char domain.
    pub fn edit_pos(&mut self, start: Pos, end: Pos, text: &str) -> Pos {
        match &mut self.backend {
            Backend::Mem(m) => m.edit_pos(start, end, text),
            Backend::Large(d) => {
                let to_char = |p: Pos| -> usize {
                    d.line_to_char(p.line).unwrap_or_else(|| d.len_chars()) + p.col
                };
                let (cs, ce) = (to_char(start), to_char(end));
                let out = if text.is_empty() {
                    end
                } else {
                    crate::undo::text_end_pos(start, text)
                };
                if let Err(e) = d.edit(cs, ce, text) {
                    eprintln!("large edit_pos rejected: {e:?}");
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod eol_tests {
    use super::Eol;

    #[test]
    fn detect_dominant_ending() {
        assert_eq!(Eol::detect("a\r\nb\r\n"), Eol::Crlf);
        assert_eq!(Eol::detect("a\nb\n"), Eol::Lf);
        assert_eq!(Eol::detect("a\rb\r"), Eol::Cr);
        // no line ending at all -> LF
        assert_eq!(Eol::detect("abc"), Eol::Lf);
        assert_eq!(Eol::detect(""), Eol::Lf);
        // majority wins
        assert_eq!(
            Eol::detect("a\r\nb\nc\rd\r\n"),
            Eol::Crlf
        );
        assert_eq!(
            Eol::detect("a\r\nb\nc\nd\n"),
            Eol::Lf
        );
        // trailing lone CR counts
        assert_eq!(Eol::detect("a\rb\r"), Eol::Cr);
    }

    #[test]
    fn convert_normalizes_all_endings() {
        // mixed endings -> LF
        assert_eq!(
            Eol::Lf.convert("a\r\nb\rc\nd"),
            "a\nb\nc\nd"
        );
        // mixed endings -> CRLF
        assert_eq!(
            Eol::Crlf.convert("a\r\nb\rc\nd"),
            "a\r\nb\r\nc\r\nd"
        );
        // mixed endings -> CR
        assert_eq!(
            Eol::Cr.convert("a\r\nb\rc\nd"),
            "a\rb\rc\rd"
        );
        // already-clean text passes through
        assert_eq!(
            Eol::Crlf.convert("a\r\nb"),
            "a\r\nb"
        );
        assert_eq!(Eol::Lf.convert("ab"), "ab");
    }

    #[test]
    fn roundtrip_detect_then_convert() {
        let text = "line1\r\nline2\r\nline3\r\n";
        let eol = Eol::detect(text);
        assert_eq!(eol, Eol::Crlf);
        assert_eq!(eol.convert(text), text);
        let text = "x\ny\nz\n";
        let eol = Eol::detect(text);
        assert_eq!(eol, Eol::Lf);
        assert_eq!(eol.convert(text), text);
    }
}

#[cfg(test)]
mod document_tests {
    use super::Document;
    use crate::linedoc::Pos;

    fn p(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn from_text_round_trips() {
        let d = Document::from_text("a\r\nb\rc\nd");
        assert_eq!(d.line_count(), 4);
        assert_eq!(d.to_text(), "a\r\nb\rc\nd");
        assert_eq!(d.total_bytes(), 8);
        assert_eq!(d.line_len(0), 1);
        assert_eq!(d.line_byte_offset(1), 3);
        assert_eq!(d.byte_line(4), 1);
    }

    /// 往 LF 文档里插入 CRLF 再按 undo 的范围删掉：删除收走的字节数必须
    /// 等于记录的文本长（2），行数、字节数都要回到原样。此前跨行删除保留
    /// 起点行的 CRLF、丢掉终点行的 LF，删 1 字节记 2 字节 —— 撤销的语法
    /// 记账跟着错，undo 一旦退过挂载点就把字节长度减穿。
    #[test]
    fn cross_line_delete_takes_the_byte_range_the_positions_name() {
        let mut d = Document::from_text("hello\nworld");
        let base_bytes = d.total_bytes();
        d.edit_pos(p(0, 5), p(0, 5), "\r\n");
        assert_eq!(d.line_count(), 3);
        assert_eq!(d.total_bytes(), base_bytes + 2, "插入按记录的文本长落账");
        // undo 的反向删除：end 是 text_end_pos((0,5), "\r\n")。
        d.edit_pos(p(0, 5), p(1, 0), "");
        assert_eq!(d.to_text(), "hello\nworld");
        assert_eq!(d.total_bytes(), base_bytes, "删除收走的字节和记录一致");
    }

    /// 跨行删除拼起来的行，行尾跟终点行的（字节域里留在原地的那一个）：
    /// CRLF 行删到 LF 行，结果行是 LF —— 和 range 文本、撤销记账同一口径。
    #[test]
    fn cross_line_delete_joins_with_the_end_lines_terminator() {
        let mut d = Document::from_text("ab\r\ncd\nef");
        // 删 (0,1)..(1,1)：收走 "b" + CRLF + "c"，终点行的 LF 留在原地。
        d.edit_pos(p(0, 1), p(1, 1), "");
        assert_eq!(d.to_text(), "ad\nef");
        assert_eq!(d.line_eol(0), Some(super::Eol::Lf));
    }

    #[test]
    fn edit_pos_inserts_and_deletes() {
        let mut d = Document::from_text("hello");
        let end = d.edit_pos(p(0, 5), p(0, 5), " world");
        assert_eq!(d.to_text(), "hello world");
        assert_eq!(end, p(0, 11));
        d.edit_pos(p(0, 0), p(0, 6), "");
        assert_eq!(d.to_text(), "world");
    }

    #[test]
    fn edit_pos_splits_lines() {
        let mut d = Document::from_text("ab");
        d.edit_pos(p(0, 1), p(0, 1), "X\nY");
        assert_eq!(d.to_text(), "aX\nYb");
        assert_eq!(d.line_count(), 2);
        assert_eq!(d.line_eol(0), Some(super::Eol::Lf));
        assert_eq!(d.line_eol(1), None);
    }

    #[test]
    fn edit_pos_cross_line_joins() {
        let mut d = Document::from_text("ab\ncd");
        d.edit_pos(p(0, 1), p(1, 1), "");
        assert_eq!(d.to_text(), "ad");
        assert_eq!(d.line_count(), 1);
    }

    /// 行修改状态: a fresh document has no line states, an edit marks the lines
    /// it lands on, and the save's pass turns them into "written back".
    #[test]
    fn an_edit_marks_its_lines_until_the_save_writes_them_back() {
        let mut d = Document::from_text("one\ntwo\nthree");
        assert!((0..3).all(|l| d.line_state(l) == 0));
        d.edit_pos(p(1, 3), p(1, 3), "!");
        assert_eq!(d.line_state(0), 0);
        assert_eq!(d.line_state(1), 1);
        assert_eq!(d.line_state(2), 0);
        d.mark_all_saved();
        assert_eq!(d.line_state(0), 0, "an untouched line stays clean");
        assert_eq!(d.line_state(1), 2, "the written line reads as saved");
        // and editing it again makes it dirty once more
        d.edit_pos(p(1, 4), p(1, 4), "?");
        assert_eq!(d.line_state(1), 1);
    }

    /// 修改行标记: the marked lines come back as `(first, last, state)` runs in
    /// line order; filling a gap merges them, a save recolours them in place, and
    /// re-editing one splits the run again.
    #[test]
    fn the_marked_lines_come_back_as_runs() {
        let mut d = Document::from_text("a\nb\nc\nd\ne");
        assert!(d.state_runs().is_empty(), "nothing marked yet");
        d.edit_pos(p(1, 1), p(1, 1), "X");
        d.edit_pos(p(3, 1), p(3, 1), "Y");
        assert_eq!(d.state_runs(), vec![(1, 1, 1), (3, 3, 1)]);
        // Filling the gap between them merges the two runs into one.
        d.edit_pos(p(2, 1), p(2, 1), "Z");
        assert_eq!(d.state_runs(), vec![(1, 3, 1)]);
        // The save's pass turns every modified run into "written back".
        d.mark_all_saved();
        assert_eq!(d.state_runs(), vec![(1, 3, 2)]);
        // Re-editing a line the save wrote back splits its run again.
        d.edit_pos(p(2, 1), p(2, 1), "!");
        assert_eq!(d.state_runs(), vec![(1, 1, 2), (2, 2, 1), (3, 3, 2)]);
    }

    /// The run list is carried by hand through inserts, removals and state
    /// writes, so it is checked against the thing it replaces: a random edit
    /// script, with the marked lines re-derived from the records after every
    /// step.
    #[test]
    fn the_run_list_tracks_a_random_edit_script() {
        let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut d = Document::from_text(&text);
        // A deterministic xorshift: a failing script has to be reproducible.
        let mut seed = 0x243f_6a88_85a3_08d3u64;
        let mut roll = move |mask: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % (mask + 1)
        };
        for step in 0..3000 {
            let lines = d.line_count();
            let line = roll(lines as u64 - 1) as usize;
            let col = roll(d.line_len(line) as u64) as usize;
            match roll(9) {
                0..=3 => {
                    d.edit_pos(p(line, col), p(line, col), "x\ny");
                }
                4..=5 => {
                    d.edit_pos(p(line, col), p(line, col), "z");
                }
                6..=7 if line + 1 < lines => {
                    let to = (line + 1 + roll(3) as usize).min(lines - 1);
                    d.edit_pos(p(line, col), p(to, 0), "");
                }
                8 => {
                    d.mark_all_saved();
                }
                _ => {
                    d.edit_pos(p(line, col), p(line, col), "\n");
                }
            }
            assert_eq!(
                d.state_runs(),
                scan_runs(&d),
                "step {step} diverged over {:?}",
                d.to_text()
            );
        }
    }

    /// What the run list claims, re-derived one line at a time from the records.
    fn scan_runs(d: &Document) -> Vec<(u32, u32, u8)> {
        let mut out: Vec<(u32, u32, u8)> = Vec::new();
        for line in 0..d.line_count() as u32 {
            let state = d.line_state(line as usize);
            if state == 0 {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.1 + 1 == line && last.2 == state => last.1 = line,
                _ => out.push((line, line, state)),
            }
        }
        out
    }

    /// A newline marks both halves of the split — the text landed on the first
    /// and the rest of the old line moved into the second.
    #[test]
    fn a_newline_marks_both_lines_it_splits_into() {
        let mut d = Document::from_text("abcdef");
        d.edit_pos(p(0, 3), p(0, 3), "\n");
        assert_eq!(d.line_count(), 2);
        assert_eq!(d.line_state(0), 1);
        assert_eq!(d.line_state(1), 1);
    }

    /// Deleting a line moves the states along with the records: the line below
    /// the hole keeps its own state as it slides up.
    #[test]
    fn a_deleted_line_takes_its_state_with_it() {
        let mut d = Document::from_text("a\nb\nc");
        // Only the last line is touched, so it is the one carrying a state.
        d.edit_pos(p(2, 1), p(2, 1), "!");
        d.mark_all_saved();
        assert_eq!(d.line_state(2), 2);
        assert_eq!(d.line_state(0), 0);
        // Deleting the first line outright moves "c!" down to index 1.
        d.edit_pos(p(0, 0), p(1, 0), "");
        assert_eq!(d.line_count(), 2);
        assert_eq!(d.line_state(1), 2, "the record carried its state up");
        assert_eq!(d.line_state(0), 1, "the join marks the merged line");
    }

    #[test]
    fn range_text_walks_eols() {
        let d = Document::from_text("ab\ncd");
        assert_eq!(d.range_text(p(0, 0), p(0, 2)), "ab");
        // A position inside a terminator clamps to the line end —
        // the Pos domain never addresses a terminator, so the
        // past-end range collapses to nothing.
        assert_eq!(d.range_text(p(0, 2), p(0, 3)), "");
        assert_eq!(d.range_text(p(0, 2), p(1, 2)), "\ncd");
        assert_eq!(d.range_text(p(0, 1), p(1, 1)), "b\nc");
        // The byte bridge agrees with the text walk.
        assert_eq!(d.line_byte_col(1, 0), 3);
        assert_eq!(d.line_byte_col(1, 1), 4);
        assert_eq!(d.total_bytes(), 5);
    }

    #[test]
    fn set_eol_all_rewrites_terminators() {
        let mut d = Document::from_text("a\nb\nc");
        d.set_eol_all(super::Eol::Crlf);
        assert_eq!(d.to_text(), "a\r\nb\r\nc");
        // the final line stays terminator-free
        assert_eq!(d.line_eol(2), None);
        // 行修改状态: the two lines whose terminator moved are modified, the
        // one the conversion left alone stays plain.
        assert_eq!(d.line_state(0), 1);
        assert_eq!(d.line_state(1), 1);
        assert_eq!(d.line_state(2), 0);
    }

    #[test]
    fn line_text_while_loading_is_empty() {
        let d = Document::from_text("abc");
        d.set_loading(true);
        let mut out = String::from("x");
        assert!(!d.line_text(0, &mut out));
        assert_eq!(out, "");
        d.set_loading(false);
        assert!(d.line_text(0, &mut out));
        assert_eq!(out, "abc");
    }

    #[test]
    fn byte_checkpoints_match_a_full_walk_under_random_edits() {
        // A seeded LCG drives inserts (plain, EOL, unicode) and
        // deletes at random (line, col) positions; after every op
        // the byte checkpoints must equal a full-text walk.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut d = Document::from_text("fn main() {\n    println!(\"héllo 世界\");\n}\n");
        let alphabet = ['x', '\n', '\r', '(', '世', 'é', ' '];
        for _ in 0..600 {
            let lines = d.line_count();
            match next() % 3 {
                0 => {
                    // plain insert
                    let line = (next() as usize) % lines;
                    let col = (next() as usize) % (d.line_len(line) + 1);
                    let c = alphabet[(next() as usize) % alphabet.len()];
                    d.edit_pos(p(line, col), p(line, col), &c.to_string());
                }
                1 => {
                    // multi-char block insert (a paste)
                    let line = (next() as usize) % lines;
                    let col = (next() as usize) % (d.line_len(line) + 1);
                    let len = 1 + (next() as usize) % 12;
                    let s: String = (0..len)
                        .map(|_| alphabet[(next() as usize) % alphabet.len()])
                        .collect();
                    d.edit_pos(p(line, col), p(line, col), &s);
                }
                _ => {
                    // delete a range spanning up to two lines
                    let a_line = (next() as usize) % lines;
                    let a_col = (next() as usize) % (d.line_len(a_line) + 1);
                    let b_line = (a_line + 1 + (next() as usize) % 2).min(lines - 1);
                    let b_col = (next() as usize) % (d.line_len(b_line) + 1);
                    let (a_col, b_col) = if a_line == b_line {
                        (a_col.min(b_col), a_col.max(b_col))
                    } else {
                        (a_col, b_col)
                    };
                    if (a_line, a_col) != (b_line, b_col) {
                        d.edit_pos(p(a_line, a_col), p(b_line, b_col), "");
                    }
                }
            }
            // Reference: the full walk over line texts and kinds.
            let mut ref_text = String::new();
            let mut starts: Vec<usize> = Vec::new();
            for line in 0..d.line_count() {
                starts.push(ref_text.len());
                d.line_text(line, &mut ref_text);
                match d.eol_kind(line) {
                    1 => ref_text.push_str("\r\n"),
                    2 => ref_text.push('\n'),
                    3 => ref_text.push('\r'),
                    _ => {}
                }
            }
            assert_eq!(d.to_text(), ref_text, "text drift");
            assert_eq!(d.total_bytes(), ref_text.len(), "total bytes drift");
            for line in 0..d.line_count() {
                assert_eq!(d.line_byte_offset(line), starts[line], "line {line} byte start");
                let mut buf = String::new();
                d.line_text(line, &mut buf);
                let col = buf.chars().count();
                assert_eq!(
                    d.line_byte_col(line, col),
                    starts[line] + buf.len(),
                    "line {line} byte end"
                );
            }
        }
    }

    /// The large arm shares one decode per line between the readers of a row —
    /// the painter, the caret, the hit test — and drops it when the overlay
    /// moves under it.
    #[test]
    fn the_large_arm_shares_one_decode_per_line() {
        let path = std::env::temp_dir().join(format!("dg_views_{}.txt", std::process::id()));
        std::fs::write(&path, "一二三 abcdef\nsecond line\n").unwrap();
        let d = Document::open_large(&path).expect("open");
        let a = d.line_chars(0).expect("the first line");
        let b = d.line_chars(0).expect("the same line again");
        assert!(std::rc::Rc::ptr_eq(&a, &b), "the second read decoded the line again");
        assert!(!d.line_uniform(0), "a wide glyph sits in that line");
        let c = d.line_chars(1).expect("the second line");
        assert_eq!(c.iter().collect::<String>(), "second line");
        assert!(d.line_uniform(1), "plain ASCII takes one cell each");
        let mut d = d;
        d.edit_pos(Pos { line: 0, col: 0 }, Pos { line: 0, col: 0 }, "X");
        let e = d.line_chars(0).expect("the edited line");
        assert!(
            !std::rc::Rc::ptr_eq(&a, &e),
            "the edited line served the old decode"
        );
        assert_eq!(e[0], 'X');
        // The version moved, so the whole table is rebuilt lazily — the same
        // contract the memory arm keeps, where a line count change shifts every
        // index the entries were keyed by. Reads after the edit share again.
        assert!(
            std::rc::Rc::ptr_eq(&e, &d.line_chars(0).unwrap()),
            "the line was decoded twice under one version"
        );
        let _ = std::fs::remove_file(&path);
    }
}
