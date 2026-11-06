//! This implementation's macro script file format. This implementation
//! records `EditOp`s instead of script lines, so the on-disk format is
//! this implementation's own line encoding of those ops — same container
//! (UTF-8 BOM, `.mac` extension, `<exe_dir>\macro` directory), different
//! payload.

use crate::undo::EditOp;

pub const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Escapes one text field for a line: the backslash, the line
/// breaks and the tab the recorder spelled as `Chr(9)` all
/// become two-character sequences, and a plain space becomes
/// `\s` so the single-space field separator stays
/// unambiguous.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' ' => out.push_str("\\s"),
            _ => out.push(ch),
        }
    }
    out
}

/// Reverses [`escape`]: an unrecognized two-character sequence
/// keeps its backslash, matching the escape side exactly.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('s') => out.push(' '),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Serializes ops to file bytes: the BOM, then one line per
/// op — `I <line:col> <text>`, `D <start> <end> <text>`,
/// `R <start> <end> <old> <new>` — fields split on single
/// spaces, an empty field preserved. A position serializes as
/// `line:col` (no spaces, so the single-space split holds).
pub fn serialize(ops: &[EditOp]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(BOM);
    let pos_str = |p: crate::linedoc::Pos| format!("{}:{}", p.line, p.col);
    for op in ops {
        let line = match op {
            EditOp::Insert { pos, text } => {
                format!("I {} {}", pos_str(*pos), escape(text))
            }
            EditOp::Delete { start, end, text } => {
                format!("D {} {} {}", pos_str(*start), pos_str(*end), escape(text))
            }
            EditOp::Replace {
                start,
                end,
                old_text,
                new_text,
            } => format!(
                "R {} {} {} {}",
                pos_str(*start),
                pos_str(*end),
                escape(old_text),
                escape(new_text)
            ),
        };
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Parses file bytes back into ops: a UTF-16 LE BOM (FF FE) or a
/// UTF-8 BOM selects the encoding, otherwise the bytes must be strict
/// UTF-8. Empty content is an empty macro; a malformed line fails the
/// whole file.
pub fn parse(bytes: &[u8]) -> Option<Vec<EditOp>> {
    let text = decode(bytes)?;
    let mut ops = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(' ');
        let kind = fields.next()?;
        let pos = |f: Option<&str>| -> Option<crate::linedoc::Pos> {
            let (l, c) = f?.split_once(':')?;
            Some(crate::linedoc::Pos {
                line: l.parse().ok()?,
                col: c.parse().ok()?,
            })
        };
        let op = match kind {
            "I" => {
                let pos = pos(fields.next())?;
                // the text field is always written, even
                // empty ("I 3 "), so a missing one is a
                // malformed line
                let text = fields.next()?;
                if fields.next().is_some() {
                    return None;
                }
                EditOp::Insert {
                    pos,
                    text: unescape(text),
                }
            }
            "D" => {
                let start = pos(fields.next())?;
                let end = pos(fields.next())?;
                let text = fields.next()?;
                if fields.next().is_some() {
                    return None;
                }
                EditOp::Delete {
                    start,
                    end,
                    text: unescape(text),
                }
            }
            "R" => {
                let start = pos(fields.next())?;
                let end = pos(fields.next())?;
                let old = fields.next()?;
                let new = fields.next()?;
                if fields.next().is_some() {
                    return None;
                }
                EditOp::Replace {
                    start,
                    end,
                    old_text: unescape(old),
                    new_text: unescape(new),
                }
            }
            _ => return None,
        };
        ops.push(op);
    }
    Some(ops)
}

/// Decodes the file body: UTF-16 LE behind an FF FE BOM,
/// UTF-8 otherwise (the BOM stripped when present).
fn decode(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let body = &bytes[2..];
        if !body.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = body
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    let body = bytes.strip_prefix(BOM).unwrap_or(bytes);
    String::from_utf8(body.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linedoc::Pos;

    fn p(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn roundtrip_mixed_ops() {
        let ops = vec![
            EditOp::Insert {
                pos: p(0, 0),
                text: "ab".to_string(),
            },
            EditOp::Delete {
                start: p(0, 2),
                end: p(1, 2),
                text: "c d".to_string(),
            },
            EditOp::Replace {
                start: p(1, 2),
                end: p(2, 0),
                old_text: "x\ty".to_string(),
                new_text: "line1\nline2".to_string(),
            },
        ];
        let bytes = serialize(&ops);
        assert!(bytes.starts_with(BOM));
        assert_eq!(parse(&bytes), Some(ops));
    }

    #[test]
    fn empty_text_field_roundtrips() {
        let ops = vec![EditOp::Insert {
            pos: p(0, 3),
            text: String::new(),
        }];
        let bytes = serialize(&ops);
        // "I 0:3 " — the trailing space keeps the empty
        // field, and split(' ') must not drop it.
        assert!(bytes.ends_with(b"I 0:3 \n"));
        assert_eq!(parse(&bytes), Some(ops));
    }

    #[test]
    fn empty_macro_is_empty() {
        let bytes = serialize(&[]);
        assert_eq!(bytes, BOM);
        assert_eq!(parse(&bytes), Some(Vec::new()));
        assert_eq!(parse(&[]), Some(Vec::new()));
    }

    #[test]
    fn utf16_le_bom_is_decoded() {
        let body: Vec<u8> = "I 0:0 hi\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(&body);
        assert_eq!(
            parse(&bytes),
            Some(vec![EditOp::Insert {
                pos: p(0, 0),
                text: "hi".to_string(),
            }])
        );
    }

    #[test]
    fn bomless_utf8_and_bom_are_equal() {
        let ops = vec![EditOp::Insert {
            pos: p(0, 1),
            text: "z".to_string(),
        }];
        let with_bom = serialize(&ops);
        let without = &with_bom[BOM.len()..];
        assert_eq!(parse(without), Some(ops.clone()));
        assert_eq!(parse(&with_bom), Some(ops));
    }

    #[test]
    fn malformed_lines_fail_the_file() {
        assert_eq!(parse(b"X 1 2\n"), None);
        assert_eq!(parse(b"I notanumber x\n"), None);
        assert_eq!(parse(b"I 1\n"), None);
        assert_eq!(parse(b"I 1:2\n"), None);
        assert_eq!(parse(b"D 1 2 a b\n"), None);
        assert_eq!(parse(b"R 1:0 2:0 a b c\n"), None);
        // an odd-length UTF-16 body cannot decode
        assert_eq!(parse(&[0xFF, 0xFE, b'I']), None);
    }

    #[test]
    fn escape_pairs_roundtrip() {
        let text = "a\\b\nc\rd\te f";
        assert_eq!(unescape(&escape(text)), text);
        // an unknown escape keeps its backslash
        assert_eq!(unescape("a\\qb"), "a\\qb");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }
}
