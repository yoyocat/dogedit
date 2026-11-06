//! Whole-line text transforms — the editor menu group, without a window. The
//! base64 pair and the sort key work on a whole block of text. Either way the
//! shell decides which text is in scope and pushes the result through the undo
//! stack, and nothing below this module knows about a device context. Every
//! function takes one line's text with the end-of-line marker already stripped —
//! [`crate::Document::line_text`] guarantees that — so none of them has to
//! reason about `\n` at the end of a run.

/// Consecutive characters of one class make a word, so a run of Chinese characters selects as a block
/// while blanks and punctuation each break it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharClass {
    /// Symbols and punctuation — the fall-through class.
    Other = 0,
    /// Space or tab. The same test drives smart Home.
    Blank = 1,
    /// Letter or digit plus `_`.
    Word = 2,
    /// CJK ideographs, hiragana, katakana.
    Cjk = 4,
    /// CJK symbols and punctuation (U+3000–U+303F).
    CjkPunct = 8,
}

/// Supplementary code points are `Other` here.
pub fn char_class(c: char) -> CharClass {
    let u = c as u32;
    if (0x4E00..=0x9FA5).contains(&u)
        || (0xF900..=0xFA2D).contains(&u)
        || (0x3040..=0x309F).contains(&u)
        || (0x30A0..=0x30FF).contains(&u)
    {
        return CharClass::Cjk;
    }
    if (0x3000..=0x303F).contains(&u) {
        return CharClass::CjkPunct;
    }
    if u > 0xFFFF {
        return CharClass::Other;
    }
    if c.is_alphanumeric() || c == '_' {
        return CharClass::Word;
    }
    if c == ' ' || c == '\t' {
        return CharClass::Blank;
    }
    CharClass::Other
}

/// `char_class(c) > 1`: the word-character
/// test the 字符统计 word counter uses. Word, Cjk and
/// CjkPunct make up a word; everything else — Blank
/// included
/// user-configurable, has no equivalent here, so a custom word
/// character counts as `Other` here.
pub fn is_stat_word_char(c: char) -> bool {
    (char_class(c) as u32) > 1
}

/// The C1_SPACE test behind the 字符统计 blank counter. Above it asks
/// `GetStringTypeW(CT_CTYPE1)` and tests the same bit, which yields
/// U+1680 / U+180E / U+2000-U+200A / U+2028 / U+2029 / U+202F /
/// U+205F and U+3000. U+3000 is therefore both a word character
/// (CjkPunct) and blank, and the two counters both count it.
pub fn is_c1_space(ch: char) -> bool {
    let u = ch as u32;
    if u < 0x100 {
        matches!(u, 0x09..=0x0D | 0x20 | 0x85 | 0xA0)
    } else {
        matches!(
            u,
            0x1680 | 0x180E | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
        )
    }
}

/// True for the characters that make up a word for 单词首字母大写: letters,
/// digits and `_`
/// inside a word.
fn is_word_char(c: char) -> bool {
    char_class(c) == CharClass::Word
}

/// 制表符转为空格 and 行头制表符转为空格: expand each tab to the next tab stop, counting
/// characters from the start of the line — the same column arithmetic the
/// renderer uses, so converting tabs leaves the text visually where it was.
/// `leading_only` restricts the expansion to the indentation: the first
/// non-whitespace character ends the run, and every tab after it is kept.
pub fn expand_tabs(line: &str, tab_size: usize, leading_only: bool) -> String {
    let step = tab_size.max(1);
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    let mut in_indent = true;
    for ch in line.chars() {
        if ch == '\t' && (!leading_only || in_indent) {
            let spaces = step - col % step;
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
            continue;
        }
        if !ch.is_whitespace() {
            in_indent = false;
        }
        col += 1;
        out.push(ch);
    }
    out
}

/// 空格转为制表符 and 行头空格转为制表符: fold each run of spaces into as many tabs as fit and
/// leave the remainder as spaces. A run is only folded when it is at least one
/// whole tab wide, so `x y` never becomes `x\ty` at tab size 4. `leading_only`
/// folds the indentation and leaves spaces inside the text alone.
pub fn compress_spaces(line: &str, tab_size: usize, leading_only: bool) -> String {
    let step = tab_size.max(1);
    let mut out = String::with_capacity(line.len());
    let mut run = 0usize;
    let mut past_indent = false;
    for ch in line.chars() {
        if ch == ' ' {
            run += 1;
            continue;
        }
        // The run ends here; whether it may fold depends on where it sat.
        let fold = !leading_only || !past_indent;
        if fold {
            for _ in 0..run / step {
                out.push('\t');
            }
            for _ in 0..run % step {
                out.push(' ');
            }
        } else {
            for _ in 0..run {
                out.push(' ');
            }
        }
        run = 0;
        if ch != '\t' {
            past_indent = true;
        }
        out.push(ch);
    }
    let fold = !leading_only || !past_indent;
    if fold {
        for _ in 0..run / step {
            out.push('\t');
        }
        for _ in 0..run % step {
            out.push(' ');
        }
    } else {
        for _ in 0..run {
            out.push(' ');
        }
    }
    out
}

/// 删除行头空白.
pub fn trim_line_head(line: &str) -> String {
    line.trim_start().to_string()
}

/// 删除行尾空白.
pub fn trim_line_tail(line: &str) -> String {
    line.trim_end().to_string()
}

/// 转为大写.
pub fn to_upper(line: &str) -> String {
    line.to_uppercase()
}

/// 转为小写.
pub fn to_lower(line: &str) -> String {
    line.to_lowercase()
}

/// 单词首字母大写: uppercase the first letter of every word and leave the rest
/// of the word as the author wrote it.
pub fn capitalize_words(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut at_start = true;
    for ch in line.chars() {
        if is_word_char(ch) {
            if at_start {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            at_start = false;
        } else {
            out.push(ch);
            at_start = true;
        }
    }
    out
}

/// 反转大小写.
pub fn invert_case(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch.is_uppercase() {
            out.extend(ch.to_lowercase());
        } else if ch.is_lowercase() {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// 转为全角 and 转为半角: the two directions of the width conversion. "Make selected
/// text as full width" / "…as half width" So these map *characters*, not lines,
/// and the shell feeds them the selection rather than a line. The range they
/// convert is the reversible one the two forms share — the ASCII block
/// `0x21..=0x7E` against the fullwidth forms `0xFF01..=0xFF5E`, plus the space
/// against U+3000. Anything else ( CJK, punctuation outside that block, the
/// control characters that end a line) passes through untouched.
pub fn to_full_width(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == ' ' {
                '\u{3000}'
            } else if (0x21..=0x7E).contains(&(c as u32)) {
                char::from_u32(c as u32 + 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// The inverse of [`to_full_width`], which is why half-widthing a full-width
/// CJK character leaves it alone rather than guessing at one.
pub fn to_half_width(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\u{3000}' {
                ' '
            } else if (0xFF01..=0xFF5E).contains(&(c as u32)) {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// 粘贴并缩进, "Indent all lines of clipboard applied with the first line's
/// indentation": the pasted block's *own* first line donates its leading
/// whitespace, and every line after it gets that prefix. A line that is only
/// whitespace is left alone, so the command never paints trailing spaces into
/// the document — 删除行尾空白 exists for that.
pub fn apply_first_line_indent(text: &str) -> String {
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or("");
    let indent = &first[..first.len() - first.trim_start().len()];
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(first);
    for line in lines {
        out.push('\n');
        if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str(indent);
            out.push_str(line);
        }
    }
    out
}

/// The comment markers the Format group writes (行注释符 / 块注释符). That is why uncommenting a Python
/// block comment works: the row has no pair to remove, so it removes the `#`. An empty field
/// means the language has no such form, which is exactly what the fallback tests for (a
/// non-empty opener *and* a non-empty closer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentTokens {
    /// `//`, `#`, `;`, `--` … Empty when the language has no line form.
    pub line: &'static str,
    /// `/*`, `<!--`, `=begin` … Empty when the language has no block form.
    pub block_start: &'static str,
    /// The closer that pairs with [`CommentTokens::block_start`].
    pub block_end: &'static str,
}

const SCRIPT_HASH: CommentTokens = CommentTokens {
    line: "#",
    block_start: "=begin",
    block_end: "=end",
};

/// The markers for a syntax-language name, `None` when the name is unknown (the
/// rows then do nothing, the same as a document with no lexer).
///
/// The names are this own — they are the keys the grammar registry uses,
/// so the Format group and the highlighter always agree. The strings are the
/// markers each language declares, trailing space included where it declares
/// one: `// ` for C++ but `//` for Java and Go, so the two really do read
/// differently on screen.
pub fn comment_tokens(lang: &str) -> Option<CommentTokens> {
    let t = match lang.to_ascii_lowercase().as_str() {
        "cpp" | "c++" | "rust" => CommentTokens {
            line: "// ",
            block_start: "/*",
            block_end: "*/",
        },
        "c" | "h" | "hpp" | "java" | "javascript" | "js" | "typescript" | "ts" | "go" | "swift"
        | "kotlin" | "php" | "csharp" | "cs" | "c-sharp" | "dart" | "scala" | "json" | "jsonc" => {
            CommentTokens {
                line: "//",
                block_start: "/*",
                block_end: "*/",
            }
        }
        "python" | "py" => CommentTokens {
            line: "#",
            block_start: "\"\"\"",
            block_end: "\"\"\"",
        },
        "shell" | "sh" | "bash" | "toml" | "perl" | "makefile" | "make" | "cmake"
        | "dockerfile" | "diff" | "turtle" => CommentTokens {
            line: "#",
            block_start: "",
            block_end: "",
        },
        "yaml" | "yml" => CommentTokens {
            line: "# ",
            block_start: "",
            block_end: "",
        },
        "ini" => CommentTokens {
            line: ";",
            block_start: "",
            block_end: "",
        },
        "ruby" => SCRIPT_HASH,
        "lua" => CommentTokens {
            line: "--",
            block_start: "--[[",
            block_end: "]]",
        },
        "sql" => CommentTokens {
            line: "-- ",
            block_start: "/*",
            block_end: "*/",
        },
        "html" | "xml" | "markdown" => CommentTokens {
            line: "",
            block_start: "<!--",
            block_end: "-->",
        },
        "css" => CommentTokens {
            line: "",
            block_start: "/*",
            block_end: "*/",
        },
        "scss" | "less" => CommentTokens {
            line: "//",
            block_start: "/*",
            block_end: "*/",
        },
        _ => return None,
    };
    Some(t)
}

/// The blanks the comment columns step over: space, tab and the ideographic
/// space. Any other blank (a no-break space, say) counts as text, so the
/// markers never pick a column the user cannot see.
pub fn comment_blank(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\u{3000}'
}

/// A line's indentation in characters, `None` when the line is blank — the
/// columns comment blanks and nothing else.
pub fn line_indent(line: &str) -> Option<usize> {
    for (i, c) in line.chars().enumerate() {
        if !comment_blank(c) {
            return Some(i);
        }
    }
    None
}

/// The column every marker of a block goes in: the smallest indentation among
/// the lines that carry text, so the markers line up and the code keeps its
/// relative shape. A block with no text at all puts them at column 0.
pub fn shared_indent<'a>(lines: impl IntoIterator<Item = &'a str>) -> usize {
    lines.into_iter().filter_map(line_indent).min().unwrap_or(0)
}

/// Insert `text` at character column `col` — a column past the end of the line
/// appends, which is what a marker for a short line means.
pub fn insert_at_col(line: &str, col: usize, text: &str) -> String {
    let mut out = String::with_capacity(line.len() + text.len());
    for (i, c) in line.chars().enumerate() {
        if i == col {
            out.push_str(text);
        }
        out.push(c);
    }
    if col >= line.chars().count() {
        out.push_str(text);
    }
    out
}

/// 删除行注释符: where the marker sits on a line — its first text column and
/// the number of characters to drop. The exact marker wins, then the marker
/// with its trailing blanks trimmed, so a block that was commented with `// `
/// and one that was hand-written `//x` both come back clean.
pub fn line_comment_span(line: &str, marker: &str) -> Option<(usize, usize)> {
    let col = line_indent(line)?;
    let rest: Vec<char> = line.chars().skip(col).collect();
    let marker: Vec<char> = marker.chars().collect();
    let trimmed_len = marker
        .iter()
        .rposition(|c| !comment_blank(*c))
        .map(|i| i + 1)
        .unwrap_or(0);
    if rest.starts_with(&marker) {
        return Some((col, marker.len()));
    }
    if trimmed_len > 0 && trimmed_len < marker.len() && rest.starts_with(&marker[..trimmed_len]) {
        return Some((col, trimmed_len));
    }
    None
}

/// The column a block opener takes: the selection start, or the first text
/// column when the selection starts in front of the line's own text.
pub fn block_open_col(first_line: &str, start_col: usize) -> usize {
    match line_indent(first_line) {
        Some(indent) if indent > start_col => indent,
        _ => start_col,
    }
}

/// 删除块注释符: the opener touching the selection start and the closer
/// touching its end, as `(column, characters)` cuts on the two boundary lines.
/// A marker just outside the selection is preferred to one just inside it,
/// which is where 块注释符 left them.
pub fn block_comment_cuts(
    start_line: &str,
    start_col: usize,
    end_line: &str,
    end_col: usize,
    open: &str,
    close: &str,
) -> (Option<(usize, usize)>, Option<(usize, usize)>) {
    fn cut_at(line: &str, col: usize, marker: &str) -> Option<(usize, usize)> {
        if marker.is_empty() {
            return None;
        }
        let chars: Vec<char> = line.chars().collect();
        let m: Vec<char> = marker.chars().collect();
        if col >= m.len() && chars.get(col - m.len()..col) == Some(&m[..]) {
            return Some((col - m.len(), m.len()));
        }
        if chars.get(col..).is_some_and(|tail| tail.starts_with(&m)) {
            return Some((col, m.len()));
        }
        None
    }
    (
        cut_at(start_line, start_col, open),
        cut_at(end_line, end_col, close),
    )
}

/// Cut `(col, len)` characters out of a line.
pub fn remove_at_col(line: &str, col: usize, len: usize) -> String {
    line.chars()
        .enumerate()
        .filter(|(i, _)| *i < col || *i >= col + len)
        .map(|(_, c)| c)
        .collect()
}

const B64: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64 of the block's UTF-8 bytes — 编码.
pub fn base64_encode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or_default(),
            chunk.get(2).copied().unwrap_or_default(),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The block read back as base64 — 解码. CR, LF, TAB and space are dropped first. `None` is
/// this implementation's answer to input that is not a base64 text or whose bytes are not
/// UTF-8: the row leaves the document alone instead of writing replacement characters into
/// it.
pub fn base64_decode(text: &str) -> Option<String> {
    let mut bytes: Vec<u8> = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for &c in text.as_bytes() {
        if matches!(c, b'\r' | b'\n' | b'\t' | b' ') {
            continue;
        }
        if c == b'=' {
            pad += 1;
            if pad > 2 {
                return None;
            }
            continue;
        }
        if pad > 0 {
            return None;
        }
        let v = B64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc & ((1 << bits) - 1) != 0 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// GNU `sort -f`, which is the key the two sort rows ask for: the comparison
/// folds ASCII lower case onto upper case. Non-ASCII bytes are left alone, the
/// way `toupper` behaves in the C locale the child process runs in.
pub fn fold_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ea = a.bytes();
    let mut eb = b.bytes();
    loop {
        match (ea.next(), eb.next()) {
            (Some(x), Some(y)) => {
                let (ux, uy) = (x.to_ascii_uppercase(), y.to_ascii_uppercase());
                if ux != uy {
                    return ux.cmp(&uy);
                }
            }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => {
                return a.as_bytes().cmp(b.as_bytes());
            }
        }
    }
}

/// 按照长度排序: the shorter row comes first, and rows the same length keep the
/// letter order — so the answer does not depend on how the text arrived. The count
/// is of characters, not bytes, which is what makes a Chinese row's length the
/// number a reader would say.
pub fn len_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars()
        .count()
        .cmp(&b.chars().count())
        .then_with(|| fold_cmp(a, b))
}

/// Where the run of digits starting at `from` ends.
fn run_end(chars: &[char], from: usize) -> usize {
    let mut k = from;
    while k < chars.len() && chars[k].is_ascii_digit() {
        k += 1;
    }
    k
}

/// A digit run read as a number: the digits with their leading zeros dropped, and
/// how many characters the run took to write.
fn run_value(digits: &[char]) -> (String, usize) {
    let raw: String = digits.iter().collect();
    (raw.trim_start_matches('0').to_string(), raw.len())
}

/// Two digit runs compared as the numbers they write. Equal value, more digits
/// comes first: `01` before `1`, which is the point of writing the zero at all.
fn cmp_runs(av: &str, al: usize, bv: &str, bl: usize) -> std::cmp::Ordering {
    av.len()
        .cmp(&bv.len())
        .then_with(|| av.cmp(bv))
        .then_with(|| al.cmp(&bl))
}

/// 按照数字排序: a run of digits compares as the number it writes, so `2` sorts
/// before `10` and the `01,02 … 09,10` the option's own label spells out comes out
/// in that order. Everything that is not a digit compares the way [`fold_cmp`]
/// compares it, and the row that runs out first is the shorter one.
pub fn num_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        if i >= ca.len() || j >= cb.len() {
            // Whatever is left is the longer row; when both ran out together the
            // rows read the same number-for-number, so the letters decide.
            return (i < ca.len())
                .cmp(&(j < cb.len()))
                .then_with(|| fold_cmp(a, b));
        }
        if ca[i].is_ascii_digit() && cb[j].is_ascii_digit() {
            let (ea, eb) = (run_end(&ca, i), run_end(&cb, j));
            let (av, al) = run_value(&ca[i..ea]);
            let (bv, bl) = run_value(&cb[j..eb]);
            match cmp_runs(&av, al, &bv, bl) {
                Ordering::Equal => {
                    i = ea;
                    j = eb;
                    continue;
                }
                other => return other,
            }
        }
        let (ac, bc) = (ca[i].to_ascii_uppercase(), cb[j].to_ascii_uppercase());
        if ac != bc {
            return ac.cmp(&bc);
        }
        i += 1;
        j += 1;
    }
}

/// of tokenising on whitespace, so a number glued to a word still counts —
/// `apple 2` contributes 2. A sign directly in front of a digit run belongs to
/// that run ( `5-3` is 5 and -3), and a run that is not a valid number (
/// `1.2.3`) contributes nothing rather than a partial value.
pub fn sum_numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut total = 0f64;
    let mut i = 0;
    while i < chars.len() {
        let signed = (chars[i] == '-' || chars[i] == '+')
            && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
        let digits = if signed { i + 1 } else { i };
        if !chars[digits].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut end = digits;
        while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
            end += 1;
        }
        let run: String = chars[i..end].iter().collect();
        if let Ok(v) = run.parse::<f64>() {
            total += v;
        }
        i = end;
    }
    format_sum(total)
}

/// The total as the box shows it: a whole number prints without a decimal
/// point, and a fractional one keeps at most six digits with no trailing zeros.
pub fn format_sum(total: f64) -> String {
    if total == total.trunc() && total.abs() < 1e15 {
        return format!("{}", total as i64);
    }
    format!("{total:.6}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two rows are inverses over anything that round-trips, and the encode
    /// side has to agree with the padding a decoder expects.
    #[test]
    fn base64_round_trips_and_pads() {
        assert_eq!(base64_encode("ab"), "YWI=");
        assert_eq!(base64_encode("abc"), "YWJj");
        assert_eq!(base64_encode("aé"), "YcOp");
        assert_eq!(base64_decode("YWI="), Some("ab".to_string()));
        assert_eq!(base64_decode("YW\nJj\r\n "), Some("abc".to_string()));
        // Not base64 at all, and bytes that are not UTF-8: no change to make.
        assert_eq!(base64_decode("!!!!"), None);
        assert_eq!(base64_decode("/3+/"), None);
    }

    /// `-f` folds but does not merge: `sort` still puts `b` after `B`.
    #[test]
    fn fold_cmp_folds_the_key_and_raw_bytes_break_the_tie() {
        use std::cmp::Ordering;
        assert_eq!(fold_cmp("Apple", "banana"), Ordering::Less);
        assert_eq!(fold_cmp("b", "B"), Ordering::Greater);
        assert_eq!(fold_cmp("B", "b"), Ordering::Less);
        assert_eq!(fold_cmp("a", "ab"), Ordering::Less);
    }

    /// 按照数字排序 is the order its own label spells out: the number decides, not
    /// the first character, and a run written with more digits is the longer one.
    #[test]
    fn num_cmp_reads_a_digit_run_as_its_number() {
        use std::cmp::Ordering;
        let mut rows: Vec<&str> = vec!["10", "02", "09", "1", "20", "007"];
        rows.sort_by(|a, b| num_cmp(a, b));
        assert_eq!(rows, vec!["1", "02", "007", "09", "10", "20"]);
        assert_eq!(num_cmp("item2", "item10"), Ordering::Less);
        assert_eq!(num_cmp("2 apples", "10 apples"), Ordering::Less);
        assert_eq!(num_cmp("apple", "Apple"), Ordering::Greater);
        assert_eq!(num_cmp("", "1"), Ordering::Less, "nothing comes first");
        assert_eq!(num_cmp("a1", "a1"), Ordering::Equal);
    }

    /// 按照长度排序 counts characters, not bytes, and equal lengths keep the letter
    /// order so the answer does not depend on how the rows arrived.
    #[test]
    fn len_cmp_sorts_by_the_number_of_characters() {
        use std::cmp::Ordering;
        let mut rows: Vec<&str> = vec!["ccc", "a", "bb", "中文"];
        rows.sort_by(|a, b| len_cmp(a, b));
        assert_eq!(rows, vec!["a", "bb", "中文", "ccc"]);
        assert_eq!(len_cmp("ab", "cd"), Ordering::Less);
        assert_eq!(len_cmp("中文", "ab"), Ordering::Greater, "two bytes make no character");
    }

    /// The three edge cases the scanner's rules were written for.
    #[test]
    fn sum_numbers_adds_the_runs_it_scans() {
        assert_eq!(
            sum_numbers("banana\napple 2\ncherry\n5\n10\n  7 \n0.5\napple\nzzz\n-3"),
            "21.5"
        );
        assert_eq!(sum_numbers("no digits here"), "0");
        assert_eq!(sum_numbers("v1.2.3 4"), "4");
        assert_eq!(sum_numbers("5-3"), "2");
    }

    #[test]
    fn expand_tabs_stops_at_the_next_tab_stop() {
        assert_eq!(expand_tabs("\tx", 4, false), "    x");
        assert_eq!(expand_tabs("a\tb", 4, false), "a   b");
        assert_eq!(expand_tabs("ab\tc", 4, false), "ab  c");
        assert_eq!(expand_tabs("\t", 8, false), "        ");
    }

    #[test]
    fn expand_tabs_leading_only_keeps_tabs_after_the_text() {
        assert_eq!(expand_tabs("\t x\ty", 4, true), "     x\ty");
        assert_eq!(expand_tabs("  \ta", 4, true), "    a");
    }

    #[test]
    fn compress_spaces_folds_whole_tabs_only() {
        assert_eq!(compress_spaces("    x", 4, false), "\tx");
        assert_eq!(compress_spaces("x    y", 4, false), "x\ty");
        assert_eq!(compress_spaces("  x", 4, false), "  x");
        assert_eq!(compress_spaces("      x", 4, false), "\t  x");
    }

    /// The row-84 half of the pair: the indentation folds, the text does not.
    #[test]
    fn compress_spaces_leading_only_stops_at_the_first_word() {
        assert_eq!(compress_spaces("    x    y", 4, true), "\tx    y");
        assert_eq!(compress_spaces("  x", 4, true), "  x");
    }

    #[test]
    fn space_runs_at_eol_survive_the_fold() {
        // Dropping them would delete text the transform was never asked to
        // touch, and the trailing blank is what 删除行尾空白 exists for.
        assert_eq!(compress_spaces("x  ", 4, false), "x  ");
        assert_eq!(compress_spaces("    ", 4, false), "\t");
        assert_eq!(expand_tabs("x\t", 4, false), "x   ");
    }

    #[test]
    fn trim_helpers_take_one_side_each() {
        assert_eq!(trim_line_head("   a b  "), "a b  ");
        assert_eq!(trim_line_tail("   a b  "), "   a b");
        assert_eq!(trim_line_head("\ta"), "a");
    }

    #[test]
    fn capitalize_uppercases_only_the_first_letter() {
        assert_eq!(capitalize_words("hello WORLD x2"), "Hello WORLD X2");
        // `_` is inside a word for this editor — the same boundary
        // `find::boundary_ok` uses for whole-word search — so an identifier is
        // one word and only its leading letter moves.
        assert_eq!(capitalize_words("snake_case_word"), "Snake_case_word");
        assert_eq!(capitalize_words("hello world again"), "Hello World Again");
        assert_eq!(capitalize_words("  already 1st"), "  Already 1st");
        assert_eq!(capitalize_words("1st place"), "1st Place");
    }

    #[test]
    fn invert_case_is_its_own_inverse() {
        let once = invert_case("HeLLo 世界");
        assert_eq!(once, "hEllO 世界");
        assert_eq!(invert_case(&once), "HeLLo 世界");
    }

    #[test]
    fn case_helpers_follow_rust_not_ascii() {
        assert_eq!(to_upper("straße"), "STRASSE");
        assert_eq!(to_lower("STRASSE"), "strasse");
    }

    #[test]
    fn comment_markers_are_the_ones_each_language_declares() {
        // C++ writes the space, Java and Go do not; that difference is the marker
        // itself, not a rule the rows invent.
        assert_eq!(comment_tokens("cpp").expect("cpp").line, "// ");
        assert_eq!(comment_tokens("java").expect("java").line, "//");
        assert_eq!(comment_tokens("go").expect("go").line, "//");
        assert_eq!(comment_tokens("sql").expect("sql").line, "-- ");
        assert_eq!(comment_tokens("lua").expect("lua").line, "--");
        assert_eq!(comment_tokens("yaml").expect("yaml").line, "# ");
        assert_eq!(comment_tokens("ini").expect("ini").line, ";");
        assert_eq!(comment_tokens("python").expect("python").block_start, "\"\"\"");
        assert_eq!(comment_tokens("html").expect("html").line, "");
        assert_eq!(comment_tokens("brainfuck"), None);
    }

    #[test]
    fn the_line_form_lines_every_marker_up_on_one_column() {
        // The column is the smallest indentation among the lines with text; the
        // blank ones do not drag it to zero.
        assert_eq!(shared_indent(["    x", "  y", ""]), 2);
        assert_eq!(shared_indent(["", "  "]), 0);
        assert_eq!(line_indent("  \t x"), Some(4));
        assert_eq!(line_indent("   "), None);
        assert_eq!(insert_at_col("    x", 2, "// "), "  //   x");
        assert_eq!(insert_at_col("", 0, "// "), "// ");
    }

    #[test]
    fn the_marker_comes_off_whole_or_trimmed() {
        // The exact marker wins, then the marker without the blanks it ends in:
        // `//  x` loses `// ` and a hand-written `//x` loses `//`.
        assert_eq!(line_comment_span("  //  x", "// "), Some((2, 3)));
        assert_eq!(line_comment_span("  //x", "// "), Some((2, 2)));
        assert_eq!(line_comment_span("  x", "// "), None);
        assert_eq!(line_comment_span("   ", "// "), None);
        assert_eq!(remove_at_col("  //  x", 2, 3), "   x");
    }

    #[test]
    fn the_block_pair_sits_on_the_text_and_strips_from_both_ends() {
        assert_eq!(block_open_col("    x", 0), 4);
        assert_eq!(block_open_col("    x", 6), 6);
        // An opener inside the selection is taken too, so the pair that this
        // editor wrote comes back off whichever end the caret moved.
        assert_eq!(
            block_comment_cuts("/*a*/", 2, "/*a*/", 3, "/*", "*/"),
            (Some((0, 2)), Some((3, 2)))
        );
        assert_eq!(
            block_comment_cuts("x", 0, "y", 1, "/*", "*/"),
            (None, None)
        );
    }
}
