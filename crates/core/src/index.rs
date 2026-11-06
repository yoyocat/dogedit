//! Sparse byte checkpoints over the line store.
//!
//! Callers work in `(line, col)` space — the `LineVector`
//! gap buffer answers line access in O(1), and nothing addresses the
//! document by a flat offset. This implementation retired its ropey-era flat
//! *char* domain along with the per-line index it needed (8 B/line);
//! the one flat coordinate that remains is the **UTF-8 byte offset**,
//! and only because tree-sitter's parse/edit API is byte-addressed.
//!
//! Design: checkpoints every [`CHECKPOINT_LINES`] lines caching the
//! cumulative byte offset at that line (terminators included). A query
//! walks at most one checkpoint span of lines — O(1) per raw UTF-8
//! line (`line_bytes_len` reads the record), O(len) only for owned or
//! non-UTF-8 raw lines. ~4 bytes/line amortized, rebuilt lazily after
//! any mutation (a rebuild is one O(N) walk of line records).

use crate::linedoc::LineVector;

/// Lines between byte checkpoints.
const CHECKPOINT_LINES: usize = 256;

/// Byte-offset checkpoints for the tree-sitter bridge. Not `Clone`;
/// rebuilt lazily from the buffer after mutations.
#[derive(Default)]
pub struct ByteIndex {
    /// `starts[i]` = UTF-8 byte offset (terminators included) of line
    /// `i * CHECKPOINT_LINES`. `starts[0] == 0`.
    starts: Vec<u64>,
    /// Line count `starts` was built for; a mismatch means stale.
    built_for: usize,
    /// Set by mutations that bypass the lazy rebuild trigger.
    dirty: bool,
}

impl ByteIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the checkpoints stale; the next query rebuilds them.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Rebuilds from the buffer. O(N) over line records.
    fn rebuild(&mut self, vec: &LineVector) {
        let n = vec.line_count();
        self.starts.clear();
        self.starts.reserve(n / CHECKPOINT_LINES + 1);
        self.starts.push(0);
        let mut acc = 0u64;
        for line in 0..n {
            acc += (vec.line_bytes_len(line) + eol_bytes(vec.eol_kind(line))) as u64;
            if (line + 1) % CHECKPOINT_LINES == 0 {
                self.starts.push(acc);
            }
        }
        self.built_for = n;
        self.dirty = false;
    }

    /// Brings the checkpoints current with `vec`.
    fn ensure(&mut self, vec: &LineVector) {
        if self.dirty || self.built_for != vec.line_count() {
            self.rebuild(vec);
        }
    }

    /// UTF-8 byte offset of `line`'s first byte (terminators of prior
    /// lines included). `line == line_count()` yields the total size.
    pub fn line_offset(&mut self, vec: &LineVector, line: usize) -> usize {
        self.ensure(vec);
        let line = line.min(vec.line_count());
        let block = line / CHECKPOINT_LINES;
        let mut acc = self.starts[block.min(self.starts.len() - 1)];
        for l in block * CHECKPOINT_LINES..line {
            acc += (vec.line_bytes_len(l) + eol_bytes(vec.eol_kind(l))) as u64;
        }
        acc as usize
    }

    /// Total UTF-8 byte size (every line plus its terminator).
    pub fn total_bytes(&mut self, vec: &LineVector) -> usize {
        self.line_offset(vec, vec.line_count())
    }

    /// The line a flat byte offset falls in (a terminator's bytes
    /// belong to their own line).
    pub fn byte_line(&mut self, vec: &LineVector, byte: usize) -> usize {
        self.ensure(vec);
        let n = vec.line_count();
        let last = self.starts.len().saturating_sub(1);
        // Binary search the checkpoints, then walk the span.
        let block = self.starts.partition_point(|&s| s <= byte as u64).saturating_sub(1).min(last);
        let mut acc = self.starts[block];
        for l in block * CHECKPOINT_LINES..n {
            let span = (vec.line_bytes_len(l) + eol_bytes(vec.eol_kind(l))) as u64;
            if (byte as u64) < acc + span {
                return l;
            }
            acc += span;
        }
        n.saturating_sub(1)
    }
}

/// UTF-8 bytes a terminator contributes (kept here to avoid a
/// document.rs dependency cycle in the rebuild walk).
fn eol_bytes(kind: u8) -> usize {
    match kind {
        1 => 2,
        2 | 3 => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn vec(text: &str) -> LineVector {
        LineVector::from_text(text)
    }

    #[test]
    fn offsets_match_a_full_walk() {
        let v = vec("alpha\r\nbeta\rgamma\ndelta");
        let mut ix = ByteIndex::new();
        // Hand-computed cumulative offsets (terminators included).
        let expect = [0usize, 7, 12, 18, 23];
        for (l, &e) in expect.iter().enumerate() {
            assert_eq!(ix.line_offset(&v, l), e, "line {l}");
        }
        assert_eq!(ix.total_bytes(&v), 23);
    }

    #[test]
    fn byte_line_walks_backwards_and_forwards() {
        let v = vec("alpha\r\nbeta\rgamma\ndelta");
        let mut ix = ByteIndex::new();
        assert_eq!(ix.byte_line(&v, 0), 0);
        assert_eq!(ix.byte_line(&v, 5), 0); // inside "alpha"
        assert_eq!(ix.byte_line(&v, 6), 0); // the CRLF's own line
        assert_eq!(ix.byte_line(&v, 7), 1);
        assert_eq!(ix.byte_line(&v, 11), 1); // the CR's own line
        assert_eq!(ix.byte_line(&v, 12), 2);
        assert_eq!(ix.byte_line(&v, 22), 3);
        assert_eq!(ix.byte_line(&v, 100), 3);
    }

    #[test]
    fn checkpoints_span_multiple_blocks() {
        // 600 lines × 2 bytes: 3+ checkpoints, queries cross blocks.
        let mut text = String::new();
        for _ in 0..599 {
            text.push_str("ab\n");
        }
        text.push_str("ab");
        let v = vec(&text);
        let mut ix = ByteIndex::new();
        assert_eq!(ix.line_offset(&v, 0), 0);
        assert_eq!(ix.line_offset(&v, 256), 768);
        // 599 terminator-carrying lines, then a bare "ab".
        assert_eq!(ix.line_offset(&v, 599), 599 * 3);
        assert_eq!(ix.total_bytes(&v), 599 * 3 + 2);
        assert_eq!(ix.byte_line(&v, 769), 256);
        assert_eq!(ix.byte_line(&v, 1799), 599);
    }

    #[test]
    fn dirty_rebuild_tracks_edits() {
        let mut v = vec("aaa\nbbb\nccc");
        let mut ix = ByteIndex::new();
        assert_eq!(ix.line_offset(&v, 1), 4);
        v.insert_text(crate::linedoc::Pos { line: 0, col: 0 }, &['x', 'y', '\n']);
        ix.mark_dirty();
        // "xy\naaa\nbbb\nccc" — line 1 now starts at byte 3.
        assert_eq!(ix.line_offset(&v, 1), 3);
        assert_eq!(ix.total_bytes(&v), 14);
    }

    #[test]
    fn empty_document_yields_zero() {
        let v = vec("");
        let mut ix = ByteIndex::new();
        assert_eq!(ix.total_bytes(&v), 0);
        assert_eq!(ix.byte_line(&v, 0), 0);
    }

    #[test]
    fn utf16_offsets_are_in_the_utf8_domain() {
        // "𝐀b\n" — 𝐀 is 4 UTF-8 bytes, plus 'b' and the terminator.
        let units: Vec<u8> = "𝐀b\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let v = LineVector::from_raw(Arc::new(units), encoding_rs::UTF_16LE, 0);
        let mut ix = ByteIndex::new();
        assert_eq!(ix.total_bytes(&v), 6);
        assert_eq!(ix.line_offset(&v, 1), 6);
    }
}
