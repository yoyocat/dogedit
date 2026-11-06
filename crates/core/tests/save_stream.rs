//! The streaming save: raw lines write back their
//! original bytes untouched, owned (edited) lines re-encode in the
//! source encoding, every line keeps its own terminator, and nothing
//! document-sized is allocated on the way out.

use encoding_rs::{GBK, UTF_16LE, UTF_8};
use dg_core::linedoc::Pos;
use dg_core::Document;
use std::sync::Arc;

fn p(line: usize, col: usize) -> Pos {
    Pos { line, col }
}

/// Feeds the document through the save walk.
fn save(doc: &Document) -> Vec<u8> {
    let mut out = Vec::new();
    let n = doc.save_to(&mut out).unwrap();
    assert_eq!(n as usize, out.len(), "the byte count must match");
    out
}

#[test]
fn untouched_documents_save_byte_identical() {
    // BOM + mixed terminators + no trailing EOL — every line is still
    // raw, so the save is the load block written back verbatim.
    let file: Vec<u8> = [b"\xEF\xBB\xBF".as_slice(), b"hello\r\nworld\rlast".as_slice()].concat();
    let doc = Document::from_bytes(Arc::new(file.clone()), UTF_8, 3);
    assert_eq!(save(&doc), file);
}

#[test]
fn save_reencodes_edited_lines_and_keeps_terminals() {
    let file: Vec<u8> = [b"\xEF\xBB\xBF".as_slice(), b"hello\r\nworld\rlast".as_slice()].concat();
    let mut doc = Document::from_bytes(Arc::new(file.clone()), UTF_8, 3);
    // "world" is line 1, cols 0..5 (the BOM is not part of the text).
    doc.edit_pos(p(1, 0), p(1, 5), "WORLD");
    assert_eq!(
        save(&doc),
        [b"\xEF\xBB\xBF".as_slice(), b"hello\r\nWORLD\rlast".as_slice()].concat()
    );
}

#[test]
fn gbk_edited_line_reencodes_in_source_encoding() {
    let text = "你好\r\n世界";
    let (raw, _, _) = GBK.encode(text);
    let mut doc = Document::from_bytes(Arc::new(raw.to_vec()), GBK, 0);
    // line 1 is 世界 → 再见.
    doc.edit_pos(p(1, 0), p(1, 2), "再见");
    let mut expected = GBK.encode("你好\r\n").0.into_owned();
    expected.extend_from_slice(&GBK.encode("再见").0);
    assert_eq!(save(&doc), expected);
}

#[test]
fn utf16le_writes_back_units_and_bom() {
    let mut file: Vec<u8> = vec![0xFF, 0xFE];
    for u in "ab\rcd".encode_utf16() {
        file.extend_from_slice(&u.to_le_bytes());
    }
    let mut doc = Document::from_bytes(Arc::new(file), UTF_16LE, 2);
    // line 1 is "cd" → "Ç" (one UTF-16 unit).
    doc.edit_pos(p(1, 0), p(1, 2), "Ç");
    let mut expected: Vec<u8> = vec![0xFF, 0xFE];
    for u in "ab\rÇ".encode_utf16() {
        expected.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(save(&doc), expected);
}

#[test]
fn bom_only_empty_document() {
    let mut doc = Document::from_text("");
    doc.set_encoding(UTF_8, true);
    assert_eq!(save(&doc), b"\xEF\xBB\xBF".to_vec());
}

#[test]
fn edited_terminator_semantics_follow_the_line_records() {
    // A cross-line delete removes the byte range the (line, col) domain
    // names: the START line's terminator goes, the END line's stays in
    // place, and the joined line carries the END line's kind. Here the
    // range is the CRLF between "a" and "b", so the joined "ab" line
    // carries the LF that used to end the "b" line, and the save
    // reflects exactly the stored per-line kinds.
    let mut doc = Document::from_text("a\r\nb\nc");
    // The terminator is the gap from one line's end to the next line's
    // start in the (line, col) domain — deleting it joins the lines.
    doc.edit_pos(p(0, 1), p(1, 0), "");
    assert_eq!(save(&doc), b"ab\nc".to_vec());
}

#[test]
fn save_round_trips_through_reopen() {
    // The saved bytes re-open to the same text, proving the save is a
    // faithful encoding of the document, not just byte-identical for
    // the untouched case.
    let mut doc = Document::from_text("one\r\ntwo\rthree");
    doc.edit_pos(p(0, 0), p(0, 3), "ONE");
    let bytes = save(&doc);
    let reopened = Document::from_bytes(Arc::new(bytes), UTF_8, 0);
    assert_eq!(reopened.to_text(), "ONE\r\ntwo\rthree");
}
