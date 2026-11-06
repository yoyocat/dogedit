//! Find & replace engine for the plain (non-regex) finder.
//! forward/backward scanner with case and whole-word modes over the
//! gap buffer; this implementation scans the line store and adds a
//! regex mode of its own. This implementation used to flatten the
//! document twice per query (`to_text` + `Vec<char>`). It now scans
//! line by line through the store's decode-on-demand view — one line
//! resident at a time — and reports matches in the document's native
//! `(line, col)` coordinates (the flat char domain is retired;
//! `Point` is the selection layer's `(line, col)` pair). Queries
//! whose needle contains an EOL character keep the legacy flat path:
//! a terminator can only be matched by a needle that itself carries
//! one, so the line-domain and flat semantics coincide for every
//! other needle.

use crate::document::Document;
use crate::selection::Point;
use crate::undo::{EditOp, UndoStack};

/// Search modes, parity with the finder
/// dialog checkboxes.
#[derive(Clone, Copy, Debug, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// false = forward (F3), true =
    /// backward (Shift+F3).
    pub backward: bool,
}

/// A match as a native `(line, col)` range —
/// end is exclusive, `end.col` may equal the
/// line length when the match runs up to a
/// terminator. A match and a selection range
/// have the same shape, so they are one type.
pub type Match = crate::selection::Range;

fn eq_folded(a: char, b: char) -> bool {
    a == b
        || a.to_lowercase().eq(b.to_lowercase())
}

/// True when case folding is the identity for `c`, so a case-insensitive
/// scan may compare its UTF-8 bytes exactly.
fn is_caseless(c: char) -> bool {
    c.to_lowercase().eq(c.to_uppercase())
}

fn boundary_ok(
    prev: Option<char>,
    next: Option<char>,
) -> bool {
    let is_word = |c: Option<char>| {
        c.map(|ch| ch.is_alphanumeric() || ch == '_')
            .unwrap_or(false)
    };
    // A whole-word match must not start or
    // end inside a word.
    !is_word(prev) && !is_word(next)
}

/// The flat scan over one document text —
/// the legacy path, kept only for needles
/// that contain an EOL character. A
/// line-start table converts flat offsets
/// back to native `(line, col)` pairs.
fn find_all_flat(
    doc: &Document,
    needle: &str,
    opts: FindOptions,
) -> Vec<Match> {
    let flat = doc.to_text();
    if needle.is_empty() || flat.is_empty() {
        return Vec::new();
    }
    // Flat offset of each line's first char (the
    // terminator belongs to the line before it).
    let mut line_starts = Vec::with_capacity(doc.line_count() + 1);
    let mut off = 0usize;
    for l in 0..doc.line_count() {
        line_starts.push(off);
        off += doc.line_len(l) + eol_len(doc.eol_kind(l));
    }
    line_starts.push(off);
    let to_point = |idx: usize| -> Point {
        let line = line_starts.partition_point(|&s| s <= idx) - 1;
        (line, idx - line_starts[line])
    };
    let scan = LiteralScan::new(needle, opts.case_sensitive);
    let mut out = Vec::new();
    let mut counted = 0usize;
    let mut chars = 0usize;
    let mut accept = |bs: usize, be: usize| -> Step {
        if opts.whole_word
            && !boundary_ok(flat[..bs].chars().next_back(), flat[be..].chars().next())
        {
            return Step::Reject;
        }
        chars += flat[counted..bs].chars().count();
        counted = bs;
        let sc = chars;
        let ec = sc + flat[bs..be].chars().count();
        out.push(Match {
            start: to_point(sc),
            end: to_point(ec),
        });
        Step::Accept
    };
    scan.scan(&flat, &mut accept);
    out
}

/// The line as text for the byte scanner. A raw UTF-8 line is already
/// stored in that form, so it is borrowed straight from the store;
/// edited lines and other encodings decode into `buf`.
fn line_hay<'a>(
    doc: &'a Document,
    l: usize,
    utf8: bool,
    buf: &'a mut String,
) -> &'a str {
    if utf8 {
        if let Some(bytes) = doc.line_vector().raw_line_bytes(l) {
            if let Ok(s) = std::str::from_utf8(bytes) {
                return s;
            }
        }
    }
    buf.clear();
    doc.line_text(l, buf);
    buf.as_str()
}

/// Chars an EOL kind contributes to the flat text.
fn eol_len(kind: u8) -> usize {
    match kind {
        1 => 2,
        2 | 3 => 1,
        _ => 0,
    }
}

/// All literal/regex matches in document
/// order. Line-domain: one line decoded at
/// a time, nothing document-sized is ever
/// allocated.
pub fn find_all(
    doc: &Document,
    needle: &str,
    opts: FindOptions,
) -> Vec<Match> {
    if needle.is_empty() {
        return Vec::new();
    }
    let crosses_eol = needle.contains('\n') || needle.contains('\r');
    if opts.regex {
        let Some(re) = build_regex(needle, opts.case_sensitive) else {
            return Vec::new();
        };
        if crosses_eol {
            // A pattern that can match a terminator keeps the flat
            // walk (rare; single-line patterns never need it).
            return find_all_flat_regex(doc, &re);
        }
        let mut out = Vec::new();
        let mut line = String::new();
        for l in 0..doc.line_count() {
            // line_text pushes into `out` without clearing:
            // callers pre-clear.
            line.clear();
            doc.line_text(l, &mut line);
            if line.is_empty() {
                continue;
            }
            for m in re.find_iter(&line) {
                out.push(Match {
                    start: (l, line[..m.start()].chars().count()),
                    end: (l, line[..m.end()].chars().count()),
                });
            }
        }
        return out;
    }
    if crosses_eol {
        return find_all_flat(doc, needle, opts);
    }
    let utf8 = doc.encoding() == encoding_rs::UTF_8 && doc.codepage().is_none();
    let scan = LiteralScan::new(needle, opts.case_sensitive);
    let mut out = Vec::new();
    let mut buf = String::new();
    for l in 0..doc.line_count() {
        let line = line_hay(doc, l, utf8, &mut buf);
        if line.is_empty() {
            continue;
        }
        let mut counted = 0usize;
        let mut chars = 0usize;
        let mut accept = |bs: usize, be: usize| -> Step {
            if opts.whole_word && !line_word_ok(doc, l, line, bs, be) {
                return Step::Reject;
            }
            chars += line[counted..bs].chars().count();
            counted = bs;
            let sc = chars;
            let ec = sc + line[bs..be].chars().count();
            out.push(Match {
                start: (l, sc),
                end: (l, ec),
            });
            Step::Accept
        };
        scan.scan(line, &mut accept);
    }
    out
}

/// What [`scan_literal`] does after one candidate.
enum Step {
    /// Take the hit and resume after it (non-overlapping).
    Accept,
    /// Skip this candidate so one starting a character later can win.
    Reject,
    /// Take the hit and end the scan.
    Stop,
}

/// One query's literal scanner, prepared once and reused for every line:
/// building the byte searcher or collecting the needle's characters per
/// line would dominate a document-wide scan.
struct LiteralScan<'n> {
    needle: &'n str,
    needle_chars: Vec<char>,
    mode: ScanMode<'n>,
}

enum ScanMode<'n> {
    /// Exact byte search (SIMD memmem) — also the case-insensitive answer
    /// when every character is caseless.
    Exact(memchr::memmem::Finder<'n>),
    /// Case-insensitive ASCII: a two-case byte anchor plus a slice compare.
    Ascii { lower: u8, upper: u8 },
    /// A folded needle with one usable anchor character: the window starts
    /// `k` characters before the anchor and is verified char by char.
    Anchored { k: usize, anchor: Anchor<'n> },
    /// No byte-stable anchor: character windows.
    Windows,
}

enum Anchor<'n> {
    Ascii(u8, u8),
    Fixed {
        finder: memchr::memmem::Finder<'n>,
        len: usize,
    },
}

impl<'n> LiteralScan<'n> {
    fn new(needle: &'n str, case_sensitive: bool) -> Self {
        let needle_chars: Vec<char> = needle.chars().collect();
        // Case folding cannot change a needle made only of caseless
        // characters (CJK, digits, punctuation), so the exact byte search
        // is also the case-insensitive answer for it.
        let exact = case_sensitive || needle_chars.iter().all(|c| is_caseless(*c));
        let mode = if exact {
            ScanMode::Exact(memchr::memmem::Finder::new(needle.as_bytes()))
        } else if needle.is_ascii() {
            let first = needle.as_bytes()[0];
            ScanMode::Ascii {
                lower: first.to_ascii_lowercase(),
                upper: first.to_ascii_uppercase(),
            }
        } else if let Some(k) = needle_chars.iter().position(|c| c.is_ascii_alphabetic()) {
            let c = needle_chars[k];
            ScanMode::Anchored {
                k,
                anchor: Anchor::Ascii(
                    c.to_ascii_lowercase() as u8,
                    c.to_ascii_uppercase() as u8,
                ),
            }
        } else if let Some(k) = needle_chars.iter().position(|c| is_caseless(*c)) {
            // A caseless character's encoding is the same in every case,
            // so its own bytes are a valid anchor.
            let (start, c) = needle
                .char_indices()
                .nth(k)
                .expect("index of a known character");
            let end = start + c.len_utf8();
            let bytes = &needle.as_bytes()[start..end];
            ScanMode::Anchored {
                k,
                anchor: Anchor::Fixed {
                    finder: memchr::memmem::Finder::new(bytes),
                    len: end - start,
                },
            }
        } else {
            ScanMode::Windows
        };
        Self {
            needle,
            needle_chars,
            mode,
        }
    }

    /// Hands `accept` each candidate's byte range in increasing order. A
    /// rejected candidate advances one character, so a whole-word rejection
    /// still lets an overlapping candidate starting one char later be found.
    /// Returns the hit that answered [`Step::Stop`].
    fn scan(
        &self,
        hay: &str,
        accept: &mut dyn FnMut(usize, usize) -> Step,
    ) -> Option<(usize, usize)> {
        if self.needle.is_empty() || hay.is_empty() {
            return None;
        }
        let bytes = hay.as_bytes();
        match &self.mode {
            ScanMode::Exact(finder) => {
                let mut at = 0usize;
                while at + self.needle.len() <= bytes.len() {
                    let Some(rel) = finder.find(&bytes[at..]) else {
                        break;
                    };
                    let bs = at + rel;
                    let be = bs + self.needle.len();
                    match accept(bs, be) {
                        Step::Accept => at = be,
                        Step::Reject => at = bs + 1,
                        Step::Stop => return Some((bs, be)),
                    }
                }
                None
            }
            ScanMode::Ascii { lower, upper } => {
                let n = self.needle.as_bytes();
                let mut at = 0usize;
                while at + n.len() <= bytes.len() {
                    let found = if lower == upper {
                        memchr::memchr(*lower, &bytes[at..])
                    } else {
                        memchr::memchr2(*lower, *upper, &bytes[at..])
                    };
                    let Some(rel) = found else {
                        break;
                    };
                    let bs = at + rel;
                    let be = bs + n.len();
                    if be > bytes.len() {
                        break;
                    }
                    if bytes[bs..be].eq_ignore_ascii_case(n) {
                        match accept(bs, be) {
                            Step::Accept => {
                                at = be;
                                continue;
                            }
                            Step::Reject => {}
                            Step::Stop => return Some((bs, be)),
                        }
                    }
                    at = bs + 1;
                }
                None
            }
            ScanMode::Anchored { k, anchor } => {
                let m = self.needle_chars.len();
                let mut at = 0usize;
                while at < bytes.len() {
                    let (p, step) = match anchor {
                        Anchor::Ascii(lo, up) => {
                            let found = if lo == up {
                                memchr::memchr(*lo, &bytes[at..])
                            } else {
                                memchr::memchr2(*lo, *up, &bytes[at..])
                            };
                            let Some(rel) = found else {
                                break;
                            };
                            (at + rel, 1)
                        }
                        Anchor::Fixed { finder, len } => {
                            let Some(rel) = finder.find(&bytes[at..]) else {
                                break;
                            };
                            (at + rel, *len)
                        }
                    };
                    // The window starts `k` characters before the anchor.
                    let start = if *k == 0 {
                        p
                    } else {
                        match hay[..p].char_indices().rev().nth(*k - 1) {
                            Some((b, _)) => b,
                            None => break,
                        }
                    };
                    let end = match hay[start..].char_indices().nth(m) {
                        Some((b, _)) => start + b,
                        None => hay.len(),
                    };
                    let mut got = 0usize;
                    let mut ok = true;
                    for (h, want) in hay[start..].chars().zip(self.needle_chars.iter()) {
                        if !eq_folded(h, *want) {
                            ok = false;
                            break;
                        }
                        got += 1;
                    }
                    if ok && got == m {
                        match accept(start, end) {
                            Step::Accept => {
                                at = end.max(p + step);
                                continue;
                            }
                            Step::Reject => {}
                            Step::Stop => return Some((start, end)),
                        }
                    }
                    at = p + step;
                }
                None
            }
            ScanMode::Windows => scan_literal_windows(hay, &self.needle_chars, accept),
        }
    }
}

/// The no-anchor fallback for a case-folded needle: character windows.
fn scan_literal_windows(
    hay: &str,
    needle_chars: &[char],
    accept: &mut dyn FnMut(usize, usize) -> Step,
) -> Option<(usize, usize)> {
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    let m = needle_chars.len();
    let mut start = 0usize;
    while start + m <= chars.len() {
        if needle_chars
            .iter()
            .enumerate()
            .all(|(k, n)| eq_folded(chars[start + k].1, *n))
        {
            let bs = chars[start].0;
            let be = if start + m < chars.len() {
                chars[start + m].0
            } else {
                hay.len()
            };
            match accept(bs, be) {
                Step::Accept => {
                    start += m;
                    continue;
                }
                Step::Reject => {}
                Step::Stop => return Some((bs, be)),
            }
        }
        start += 1;
    }
    None
}

/// Whole-word test for one line-domain hit, including the neighbours that
/// live on the adjacent line when the line has no terminator of its own.
fn line_word_ok(doc: &Document, l: usize, line: &str, bs: usize, be: usize) -> bool {
    let prev = if bs > 0 {
        line[..bs].chars().next_back()
    } else if l == 0 || doc.line_has_eol(l - 1) {
        None
    } else {
        let mut p = String::new();
        doc.line_text(l - 1, &mut p);
        p.chars().next_back()
    };
    let next = if be < line.len() {
        line[be..].chars().next()
    } else if doc.line_has_eol(l) {
        None
    } else if l + 1 < doc.line_count() {
        let mut n = String::new();
        doc.line_text(l + 1, &mut n);
        n.chars().next()
    } else {
        None
    };
    boundary_ok(prev, next)
}

/// The flat regex walk for EOL-crossing
/// patterns, with the same line-start table
/// conversion as [`find_all_flat`].
fn find_all_flat_regex(doc: &Document, re: &regex::Regex) -> Vec<Match> {
    let text = doc.to_text();
    let mut line_starts = Vec::with_capacity(doc.line_count() + 1);
    let mut off = 0usize;
    for l in 0..doc.line_count() {
        line_starts.push(off);
        off += doc.line_len(l) + eol_len(doc.eol_kind(l));
    }
    line_starts.push(off);
    let to_point = |idx: usize| -> Point {
        let line = line_starts.partition_point(|&s| s <= idx) - 1;
        (line, idx - line_starts[line])
    };
    re.find_iter(&text)
        .map(|m| Match {
            start: to_point(text[..m.start()].chars().count()),
            end: to_point(text[..m.end()].chars().count()),
        })
        .collect()
}

fn build_regex(
    needle: &str,
    case_sensitive: bool,
) -> Option<regex::Regex> {
    let mut b = regex::RegexBuilder::new(needle);
    b.case_insensitive(!case_sensitive);
    b.build().ok()
}

/// Pick from a match list: the first match at
/// or after `from` (forward), or the last one
/// starting strictly before it (backward).
/// Wraps around. `from` is a native
/// `(line, col)` pair; the comparison is
/// document order.
pub fn pick_match(
    matches: &[Match],
    from: Point,
    backward: bool,
) -> Option<Match> {
    if matches.is_empty() {
        return None;
    }
    if backward {
        // Last match whose start is strictly
        // before `from`; wrap to the end.
        matches
            .iter()
            .rev()
            .find(|m| m.start < from)
            .or_else(|| matches.last())
            .copied()
    } else {
        // First match whose start is at or
        // after `from`; wrap to the start.
        matches
            .iter()
            .find(|m| m.start >= from)
            .or_else(|| matches.first())
            .copied()
    }
}

/// Next match at or after `from` (forward),
/// or the last match ending at or before
/// `from` (backward). Wraps around.
pub fn find_from(
    doc: &Document,
    from: Point,
    needle: &str,
    opts: FindOptions,
) -> Option<Match> {
    pick_match(
        &find_all(doc, needle, opts),
        from,
        opts.backward,
    )
}

/// First match at or after `from`, **no
/// wrap** — the quick-select-next
/// semantics (`chars[i..]` scan that stops
/// at the end). Line-domain like
/// [`find_all`]; nothing document-sized is
/// allocated (the match list is not built —
/// the scan stops at the first hit).
pub fn find_next_at(
    doc: &Document,
    from: Point,
    needle: &str,
    opts: FindOptions,
) -> Option<Match> {
    if needle.is_empty() {
        return None;
    }
    let crosses_eol = needle.contains('\n') || needle.contains('\r');
    if opts.regex || crosses_eol {
        // Flat/list path (rare shapes): enumerate, take the first at
        // or after `from`.
        return find_all(doc, needle, opts)
            .into_iter()
            .find(|m| m.start >= from);
    }
    let utf8 = doc.encoding() == encoding_rs::UTF_8 && doc.codepage().is_none();
    let scan = LiteralScan::new(needle, opts.case_sensitive);
    let mut buf = String::new();
    for l in from.0..doc.line_count() {
        let line = line_hay(doc, l, utf8, &mut buf);
        if line.is_empty() {
            continue;
        }
        let begin = if l == from.0 {
            line.char_indices()
                .nth(from.1)
                .map(|(b, _)| b)
                .unwrap_or(line.len())
        } else {
            0
        };
        let mut found = None;
        scan.scan(line, &mut |bs, be| {
            if bs < begin {
                return Step::Reject;
            }
            if opts.whole_word && !line_word_ok(doc, l, line, bs, be) {
                return Step::Reject;
            }
            let sc = line[..bs].chars().count();
            found = Some(Match {
                start: (l, sc),
                end: (l, sc + line[bs..be].chars().count()),
            });
            Step::Stop
        });
        if let Some(hit) = found {
            return Some(hit);
        }
    }
    None
}

/// Number of non-overlapping matches.
pub fn count(doc: &Document, needle: &str, opts: FindOptions) -> usize {
    find_all(doc, needle, opts).len()
}

/// Replace every match with `replacement`.
/// Each replacement is one ReplaceCommand
/// on the undo stack; they are applied
/// back-to-front so earlier positions stay
/// valid. Returns the number replaced.
pub fn replace_all(
    doc: &mut Document,
    undo: &mut UndoStack,
    needle: &str,
    replacement: &str,
    opts: FindOptions,
) -> usize {
    replace_all_within(doc, undo, needle, replacement, opts, None)
}

/// [`replace_all`] restricted to the matches fully inside `within`, the
/// 替换对话框's 选区 scope. `None` is the whole document.
pub fn replace_all_within(
    doc: &mut Document,
    undo: &mut UndoStack,
    needle: &str,
    replacement: &str,
    opts: FindOptions,
    within: Option<Match>,
) -> usize {
    let matches: Vec<Match> = find_all(doc, needle, opts)
        .into_iter()
        .filter(|m| within.is_none_or(|w| m.start >= w.start && m.end <= w.end))
        .collect();
    let n = matches.len();
    // Regex replacement supports $1/$2 group
    // references via the regex crate; literal
    // mode uses the raw replacement.
    for m in matches.iter().rev() {
        let rep = if opts.regex {
            expand_regex_replacement(needle, replacement, doc, m)
        } else {
            replacement.to_string()
        };
        let old = UndoStack::capture(doc, pos_of(m.start), pos_of(m.end));
        undo.apply(
            doc,
            EditOp::Replace {
                start: pos_of(m.start),
                end: pos_of(m.end),
                old_text: old,
                new_text: rep,
            },
        );
    }
    n
}

fn pos_of(p: Point) -> crate::linedoc::Pos {
    crate::linedoc::Pos { line: p.0, col: p.1 }
}

/// Expands `$1`-style group references in
/// `replacement` using the match on its own
/// line.
fn expand_regex_replacement(
    needle: &str,
    replacement: &str,
    doc: &Document,
    m: &Match,
) -> String {
    let Some(re) = build_regex(needle, true) else {
        return replacement.to_string();
    };
    // Re-derive the regex match on the match's
    // line text for group extraction.
    let mut slice = String::new();
    doc.line_text(m.start.0, &mut slice);
    if let Some(caps) = re.captures(&slice) {
        let mut out = String::new();
        let mut rest = replacement;
        while let Some(dollar) = rest.find('$') {
            out.push_str(&rest[..dollar]);
            let after = &rest[dollar + 1..];
            let digits: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(g) = digits.parse::<usize>() {
                if let Some(gm) = caps.get(g) {
                    out.push_str(&slice[gm.start()..gm.end()]);
                }
                rest = &after[digits.len()..];
            } else {
                out.push('$');
                rest = after;
            }
        }
        out.push_str(rest);
        out
    } else {
        replacement.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;


    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn m(sl: usize, sc: usize, el: usize, ec: usize) -> Match {
        Match { start: (sl, sc), end: (el, ec) }
    }

    #[test]
    fn literal_forward_match() {
        let d = doc("hello world hello");
        let hit = find_from(&d, (0, 0), "hello", FindOptions::default());
        assert_eq!(hit, Some(m(0, 0, 0, 5)));
        let m2 = find_from(&d, (0, 1), "hello", FindOptions::default());
        assert_eq!(m2, Some(m(0, 12, 0, 17)));
        // wraps around
        let m3 = find_from(&d, (0, 13), "hello", FindOptions::default());
        assert_eq!(m3, Some(m(0, 0, 0, 5)));
    }

    #[test]
    fn backward_wraps() {
        let d = doc("aa bb aa");
        let hit = find_from(&d, (0, 8), "aa", FindOptions {
            backward: true,
            ..Default::default()
        });
        assert_eq!(hit, Some(m(0, 6, 0, 8)));
        let m2 = find_from(&d, (0, 6), "aa", FindOptions {
            backward: true,
            ..Default::default()
        });
        assert_eq!(m2, Some(m(0, 0, 0, 2)));
    }

    #[test]
    fn case_insensitive() {
        let d = doc("Hello WORLD");
        let hit = find_from(&d, (0, 0), "world", FindOptions::default());
        assert_eq!(hit, Some(m(0, 6, 0, 11)));
        let none = find_from(&d, (0, 0), "world", FindOptions {
            case_sensitive: true,
            ..Default::default()
        });
        assert_eq!(none, None);
    }

    #[test]
    fn whole_word_boundary() {
        let d = doc("foo foobar foo");
        let hit = find_from(&d, (0, 0), "foo", FindOptions {
            whole_word: true,
            ..Default::default()
        });
        assert_eq!(hit, Some(m(0, 0, 0, 3)));
        let all = find_all(&d, "foo", FindOptions {
            whole_word: true,
            ..Default::default()
        });
        // "foobar" must not match
        assert_eq!(all.len(), 2);
        assert_eq!(all[1], m(0, 11, 0, 14));
    }

    /// The flat scanner's boundary semantics at line edges: a match
    /// touching a stored terminator is word-bounded (terminators are
    /// non-word); one spanning two terminator-less lines is not.
    #[test]
    fn whole_word_across_lines() {
        let d = doc("foo\nfoo");
        let all = find_all(&d, "foo", FindOptions {
            whole_word: true,
            ..Default::default()
        });
        // both lines carry a terminator at the seam
        assert_eq!(all.len(), 2);
        let d = Document::from_text("xfoo");
        let all = find_all(&d, "foo", FindOptions {
            whole_word: true,
            ..Default::default()
        });
        assert_eq!(all.len(), 0, "letter prefix blocks the match");
    }

    #[test]
    fn regex_mode() {
        let d = doc("abc123 def456");
        let hit = find_from(&d, (0, 0), r"\d+", FindOptions {
            regex: true,
            ..Default::default()
        });
        assert_eq!(hit, Some(m(0, 3, 0, 6)));
        assert_eq!(count(&d, r"\d+", FindOptions {
            regex: true,
            ..Default::default()
        }), 2);
    }

    /// Regex matches per line, positions native.
    #[test]
    fn regex_multiline_document() {
        let d = doc("a1\nb22\nc333");
        let all = find_all(&d, r"\d+", FindOptions {
            regex: true,
            ..Default::default()
        });
        assert_eq!(
            all,
            vec![
                m(0, 1, 0, 2),
                m(1, 1, 1, 3),
                m(2, 1, 2, 4),
            ]
        );
    }

    /// A needle carrying an EOL keeps the flat walk and still finds
    /// the cross-line hit, reported as a native two-line range.
    #[test]
    fn eol_needle_flat_fallback() {
        let d = doc("ab\ncd");
        let hit = find_from(&d, (0, 0), "b\nc", FindOptions::default());
        assert_eq!(hit, Some(m(0, 1, 1, 1)));
        let none = find_from(&d, (0, 0), "x\ny", FindOptions::default());
        assert_eq!(none, None);
    }

    #[test]
    fn replace_all_undoable() {
        let mut d = doc("cat dog cat");
        let mut undo = UndoStack::new();
        let n = replace_all(&mut d, &mut undo, "cat", "cow", FindOptions::default());
        assert_eq!(n, 2);
        assert_eq!(d.to_text(), "cow dog cow");
        // each replacement is its own undo step;
        // edits were applied back-to-front, so the
        // first edit undoes first
        assert!(!undo.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "cat dog cow");
        assert!(!undo.undo(&mut d).is_empty());
        assert_eq!(d.to_text(), "cat dog cat");
    }

    #[test]
    fn replace_all_within_leaves_the_rest_alone() {
        let mut d = doc("cat dog cat\ncat");
        let mut undo = UndoStack::new();
        let n = replace_all_within(
            &mut d,
            &mut undo,
            "cat",
            "cow",
            FindOptions::default(),
            Some(m(0, 8, 1, 3)),
        );
        assert_eq!(n, 2);
        assert_eq!(d.to_text(), "cat dog cow\ncow");
    }

    /// The scanner stays linear in the document size. It used to restart the
    /// rope character iterator at index 0 for every candidate offset, which
    /// turned a find on a 300 KB file into a minutes-long stall inside
    /// `WM_CHAR`. The bound is loose on purpose: the fix costs tens of
    /// milliseconds, the quadratic version needs thousands of seconds.
    #[test]
    fn find_all_scans_large_documents() {
        let unit = "fn demo(a: i32) -> i32 {\n    let b = a + 1;\n    return b;\n}\n";
        let d = doc(&unit.repeat(2000));
        let t = std::time::Instant::now();
        let hits = find_all(&d, "demo", FindOptions::default()).len();
        let ms = t.elapsed().as_millis();
        assert_eq!(hits, 2000);
        assert!(ms < 2000, "2000-unit scan took {ms} ms");
    }

    #[test]
    fn replace_all_regex_groups() {
        let mut d = doc("foo123bar");
        let mut undo = UndoStack::new();
        let n = replace_all(
            &mut d,
            &mut undo,
            r"foo(\d+)bar",
            "num-$1!",
            FindOptions {
                regex: true,
                ..Default::default()
            },
        );
        assert_eq!(n, 1);
        assert_eq!(d.to_text(), "num-123!");
    }

    #[test]
    fn find_next_at_stops_without_wrapping() {
        let d = doc("aa bb aa");
        let hit = find_next_at(&d, (0, 1), "aa", FindOptions::default());
        assert_eq!(hit, Some(m(0, 6, 0, 8)));
        let none = find_next_at(&d, (0, 7), "aa", FindOptions::default());
        assert_eq!(none, None);
    }

    /// The byte scanner reports character columns, not byte offsets.
    #[test]
    fn literal_hits_report_character_columns() {
        let d = doc("中文abc中文abc");
        let hits = find_all(&d, "abc", FindOptions::default());
        assert_eq!(hits, vec![m(0, 2, 0, 5), m(0, 7, 0, 10)]);
    }

    /// Case folding outside ASCII goes through the character-window path and
    /// still reports the right byte span.
    #[test]
    fn case_folded_multibyte_needle_matches() {
        let d = doc("ÄBC äbc");
        let hits = find_all(&d, "äbc", FindOptions {
            case_sensitive: false,
            ..Default::default()
        });
        assert_eq!(hits, vec![m(0, 0, 0, 3), m(0, 4, 0, 7)]);
    }

    /// A whole-word rejection advances one character, so an overlapping
    /// candidate that starts later can still be accepted.
    #[test]
    fn rejected_hit_keeps_overlapping_candidates_alive() {
        let d = doc("xa.a.");
        let hits = find_all(&d, "a.", FindOptions {
            whole_word: true,
            ..Default::default()
        });
        // The hit at column 1 is rejected ('x' is a word character); the
        // one at column 3 borders a '.', so it is kept.
        assert_eq!(hits, vec![m(0, 3, 0, 5)]);
    }

    /// A case-insensitive needle built from caseless characters (CJK,
    /// emoji, digits) can use the exact byte search unchanged.
    #[test]
    fn caseless_needle_scans_exactly() {
        let d = doc("版本1：中文 emoji😀 中文");
        let hits = find_all(&d, "中文", FindOptions::default());
        assert_eq!(hits, vec![m(0, 4, 0, 6), m(0, 14, 0, 16)]);
        let emoji = find_all(&d, "😀", FindOptions::default());
        assert_eq!(emoji, vec![m(0, 12, 0, 13)]);
    }

    /// A mixed needle anchors on its caseless CJK character and folds the
    /// rest of the window.
    #[test]
    fn caseless_anchor_with_folded_neighbour() {
        let d = doc("x中Ä y中ä");
        let hits = find_all(&d, "中Ä", FindOptions::default());
        assert_eq!(hits, vec![m(0, 1, 0, 3), m(0, 5, 0, 7)]);
        // The cache start must count characters, not bytes, when the
        // anchor is not the first character of the needle.
        let hits = find_all(&d, "Ä中", FindOptions::default());
        assert!(hits.is_empty());
        let d = doc("ä中 ä中");
        let hits = find_all(&d, "Ä中", FindOptions::default());
        assert_eq!(hits, vec![m(0, 0, 0, 2), m(0, 3, 0, 5)]);
    }

    /// An EOL-crossing needle uses the byte scan on the flat text and still
    /// reports native line/column pairs.
    #[test]
    fn mixed_case_multiline_needle() {
        let d = doc("中文\nabc\n中文");
        let hits = find_all(&d, "文\nA", FindOptions::default());
        assert_eq!(hits, vec![m(0, 1, 1, 1)]);
    }

    /// An edited line is no longer raw; the scanner still sees the new text
    /// through the decode fallback.
    #[test]
    fn edited_line_is_scanned_after_decode_fallback() {
        let mut d = doc("alpha beta\ngamma delta");
        d.edit_pos(pos_of((1, 0)), pos_of((1, 5)), "中文");
        let hits = find_all(&d, "中文", FindOptions::default());
        assert_eq!(hits, vec![m(1, 0, 1, 2)]);
        let hits = find_all(&d, "delta", FindOptions::default());
        assert_eq!(hits, vec![m(1, 3, 1, 8)]);
    }

    /// 快速下一个 starts from a character column and stops at the first hit.
    #[test]
    fn find_next_at_after_multibyte_text() {
        let d = doc("😀aa😀aa");
        let hit = find_next_at(&d, (0, 2), "aa", FindOptions::default());
        assert_eq!(hit, Some(m(0, 4, 0, 6)));
        let none = find_next_at(&d, (0, 5), "aa", FindOptions::default());
        assert_eq!(none, None);
    }

    /// The byte scanner agrees with a plain character-window scan on a
    /// mixed corpus, in both case modes.
    #[test]
    fn byte_scanner_agrees_with_character_scan() {
        let text =
            "Alpha beta GAMMA\n中文abc中文ABC\nemoji😀abc😀abc\nxxA.A.\nx中Ä y中ä\nä中 Ä中\n";
        let d = doc(text);
        for needle in [
            "abc", "a.", "A", "中文", "😀", "gamma", "中Ä", "Ä中", "ä中", "ÄÖ",
        ] {
            for case_sensitive in [true, false] {
                let got = find_all(&d, needle, FindOptions {
                    case_sensitive,
                    ..Default::default()
                });
                let want = naive_line_scan(text, needle, case_sensitive);
                assert_eq!(got, want, "needle {needle:?} case={case_sensitive}");
            }
        }
    }

    /// A plain character-window scan to check the byte scanner against: one
    /// line at a time, skip the needle on a hit else advance one.
    fn naive_line_scan(text: &str, needle: &str, case_sensitive: bool) -> Vec<Match> {
        let needle_chars: Vec<char> = needle.chars().collect();
        let mut out = Vec::new();
        for (l, line) in text.split('\n').enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let mut start = 0usize;
            while start + needle_chars.len() <= chars.len() {
                let ok = needle_chars.iter().enumerate().all(|(k, n)| {
                    if case_sensitive {
                        chars[start + k] == *n
                    } else {
                        eq_folded(chars[start + k], *n)
                    }
                });
                if ok {
                    out.push(m(l, start, l, start + needle_chars.len()));
                    start += needle_chars.len();
                } else {
                    start += 1;
                }
            }
        }
        out
    }
}
