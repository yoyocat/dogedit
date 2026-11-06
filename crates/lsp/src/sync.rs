use dg_core::linedoc::Pos;
use dg_core::Document;
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};

/// A document `(line, col)` -> LSP position. `character` counts
/// UTF-16 code units — the LSP wire contract, and what this editor's
/// positions serialize as.
pub fn pos_to_lsp(doc: &Document, p: Pos) -> Position {
    let line = p.line.min(doc.line_count().saturating_sub(1));
    let mut text = String::new();
    doc.line_text(line, &mut text);
    let col = p.col.min(text.chars().count());
    Position {
        line: line as u32,
        character: text.chars().take(col).map(char::len_utf16).sum::<usize>() as u32,
    }
}

/// LSP position -> document `(line, col)`. Out-of-range lines and
/// characters clamp (clients may address the end of the document;
/// a character landing inside a surrogate pair clamps before it).
pub fn lsp_to_pos(doc: &Document, pos: &Position) -> Pos {
    let line = (pos.line as usize).min(doc.line_count().saturating_sub(1));
    let mut text = String::new();
    doc.line_text(line, &mut text);
    let mut left = pos.character as usize;
    let mut col = 0;
    for c in text.chars() {
        if left == 0 {
            break;
        }
        let w = c.len_utf16();
        if w > left {
            break;
        }
        left -= w;
        col += 1;
    }
    Pos { line, col }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub start: Pos,
    pub end: Pos,
    pub text: String,
}

/// Convert `(line, col)` changes to LSP
/// `TextDocumentContentChangeEvent`s with ranges
/// (incremental sync).
pub fn to_lsp_changes(
    doc: &Document,
    changes: &[Change],
) -> Vec<TextDocumentContentChangeEvent> {
    changes
        .iter()
        .map(|c| {
            let start = pos_to_lsp(doc, c.start);
            let end = pos_to_lsp(doc, c.end);
            TextDocumentContentChangeEvent {
                range: Some(Range { start, end }),
                range_length: None,
                text: c.text.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn position_roundtrip() {
        let doc = Document::from_text("hello\nworld\n!");
        assert_eq!(pos_to_lsp(&doc, p(0, 0)), Position { line: 0, character: 0 });
        assert_eq!(pos_to_lsp(&doc, p(0, 5)), Position { line: 0, character: 5 });
        assert_eq!(pos_to_lsp(&doc, p(1, 0)), Position { line: 1, character: 0 });
        assert_eq!(pos_to_lsp(&doc, p(1, 5)), Position { line: 1, character: 5 });
        assert_eq!(lsp_to_pos(&doc, &Position { line: 0, character: 0 }), p(0, 0));
        assert_eq!(lsp_to_pos(&doc, &Position { line: 1, character: 2 }), p(1, 2));
        // past the document end clamps to the last line
        assert_eq!(lsp_to_pos(&doc, &Position { line: 5, character: 0 }), p(2, 0));
    }

    #[test]
    fn characters_count_utf16_units() {
        // 𝐀 (U+1D400) is one char, two UTF-16 units.
        let doc = Document::from_text("𝐀x\ny");
        assert_eq!(pos_to_lsp(&doc, p(0, 2)), Position { line: 0, character: 3 });
        assert_eq!(lsp_to_pos(&doc, &Position { line: 0, character: 3 }), p(0, 2));
        // A unit inside the surrogate pair clamps before it.
        assert_eq!(lsp_to_pos(&doc, &Position { line: 0, character: 2 }), p(0, 1));
        assert_eq!(lsp_to_pos(&doc, &Position { line: 0, character: 1 }), p(0, 0));
    }

    #[test]
    fn changes_carry_ranges() {
        let doc = Document::from_text("hello\nworld\n!");
        let changes = vec![Change {
            start: p(1, 0),
            end: p(1, 5),
            text: "there".into(),
        }];
        let lsp = to_lsp_changes(&doc, &changes);
        assert_eq!(lsp.len(), 1);
        let r = lsp[0].range.unwrap();
        assert_eq!(r.start, Position { line: 1, character: 0 });
        assert_eq!(r.end, Position { line: 1, character: 5 });
        assert_eq!(lsp[0].text, "there");
    }
}
