//! 全仓唯一的 INI 实现：解码、解析、读取、编辑、写回都在这一个模块里。
//!
//! 方言（全仓所有 `.ini`/`.lng`/`.tool` 文件共用的唯一语义）：
//!
//! - 解码认 UTF-16LE/BE 的 BOM，UTF-8 的 BOM 剥掉；正文按**严格** UTF-8 解，
//!   混进无效字节时整个文件当「不存在」，写回路径也绝不碰它 —— 宁可不保存，
//!   也不把 U+FFFD 替进用户没碰的行。
//! - 段名、键名都不分大小写，两侧空白忽略（`Key = v` 也认）；值两侧去空白，
//!   **空值读成「没有」**。
//! - 行首 `;` 与 `//` 是注释；trim 后 `[` 开头、`]` 结尾的是段头。
//! - 同名键写两遍时**后写的赢**：读认最后一处非空值，改写也改最后一处，两边对得上。
//! - 写回一律 UTF-8（无 BOM）。行按原始文本保存，改动只落在目标行上；
//!   新行的换行符跟文件已有的走，新建文档用 CRLF。
//!
//! `.lng` 语言表是个例外：存在没有 BOM 的 UTF-16 文件，[`decode_with`]
//! 为它保留「按内容猜 UTF-16」的入口，其它文件不用。

use std::path::Path;

/// UTF-8 BOM。解码时剥掉；写回不再补。
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// 文件开头的 BOM 种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bom {
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// 认 BOM。UTF-16LE 优先：`FF FE 00 00`（UTF-32LE 的 BOM）按 UTF-16LE 读 ——
/// 这一直是这批配置文件的读法，手写文件不会被换一种方式解释。
fn bom(bytes: &[u8]) -> Bom {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        Bom::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Bom::Utf16Be
    } else if bytes.starts_with(UTF8_BOM) {
        Bom::Utf8
    } else {
        Bom::None
    }
}

/// 解 UTF-16 正文：传入**去掉 BOM 之后**的字节（无 BOM 的文件就传全文）。
/// `chunks_exact(2)` 丢掉落单的尾字节，`from_utf16_lossy` 把不成对的代理项
/// 换成 U+FFFD。
fn utf16_body(body: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// 解码一份 INI 的字节：认 UTF-16LE/BE 的 BOM，UTF-8 的 BOM 剥掉，正文按
/// **严格** UTF-8 解。混进无效字节时是 `None` —— 调用方把它当「没配过」，
/// 写回路径也就不会把 U+FFFD 替进用户没碰的行。
pub fn decode(bytes: &[u8]) -> Option<String> {
    decode_with(bytes, false)
}

/// [`decode`] 的语言表版：没有 BOM、但内容是 UTF-16LE 的文件也认 ——
/// 开头一段的高位零字节多到不像 UTF-8 时按 UTF-16LE 解。
pub fn decode_with(bytes: &[u8], bomless_utf16: bool) -> Option<String> {
    match bom(bytes) {
        Bom::Utf16Le => Some(utf16_body(&bytes[2..], false)),
        Bom::Utf16Be => Some(utf16_body(&bytes[2..], true)),
        _ => {
            if bomless_utf16 && looks_like_utf16(bytes) {
                return Some(utf16_body(bytes, false));
            }
            let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
            // 正文里混着 NUL 字节就不是 UTF-8 文本：那是没被认出来的
            // UTF-16LE，解出来的「键」全是噪声，宁可当文件不存在。
            String::from_utf8(body.to_vec())
                .ok()
                .filter(|t| !t.contains('\0'))
        }
    }
}

/// 无 BOM 的 UTF-16 猜测：ASCII 字符的高位字节是零，开头一段里偶数位上的
/// 零字节多到一定程度就当 UTF-16LE。
fn looks_like_utf16(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes
            .iter()
            .skip(1)
            .step_by(2)
            .take(64)
            .filter(|&&b| b == 0)
            .count()
            > 8
}

/// 一份 INI 文档：原始行加上两个写回时要用的记号。
///
/// 行保存的是**原始文本**（含行尾的 `\r`），注释、空行、缩进、键的原始拼写
/// 都原样在行里 —— 编辑只落在目标行上，其余行写回时一个字节都不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ini {
    lines: Vec<String>,
    /// 解析出来的文本以换行结尾；还原时要补回去。
    trailing_newline: bool,
    /// 新行的换行风格：文件里有 CRLF 就跟 CRLF，纯 LF 文件跟 LF，新建文档用 CRLF。
    crlf: bool,
}

impl Default for Ini {
    fn default() -> Self {
        Self::new()
    }
}

impl Ini {
    /// 一份空文档：CRLF、以换行结尾 —— 新写出来的文件从这一格起步。
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            trailing_newline: true,
            crlf: true,
        }
    }

    /// 从文本解析。文本应当已经解码（不含 BOM）。
    pub fn parse(text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let trailing_newline = lines.last().is_some_and(|l| l.is_empty());
        if trailing_newline {
            lines.pop();
        }
        let crlf = lines.is_empty() || lines.iter().any(|l| l.ends_with('\r'));
        // 行首的 BOM 字符不属于内容：UTF-16 表里手工拼接过的行会带上它。
        let lines = lines
            .into_iter()
            .map(|l| {
                l.strip_prefix('\u{feff}')
                    .map(str::to_string)
                    .unwrap_or(l)
            })
            .collect();
        Self {
            lines,
            trailing_newline,
            crlf,
        }
    }

    /// 从字节解析：[`decode`] 加解析一步到位。`None` 就是「文件当不存在」。
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Self::from_bytes_with(bytes, false)
    }

    /// [`Ini::from_bytes`] 的语言表版：多认一种无 BOM 的 UTF-16LE。
    pub fn from_bytes_with(bytes: &[u8], bomless_utf16: bool) -> Option<Self> {
        decode_with(bytes, bomless_utf16).map(|t| Self::parse(&t))
    }

    /// 从文件读。文件不存在或解不出来都是 `None`。
    pub fn load(path: &Path) -> Option<Self> {
        Self::from_bytes(&std::fs::read(path).ok()?)
    }

    /// 一行的正文（行尾的 `\r` 不算内容）。
    fn body(line: &str) -> &str {
        line.strip_suffix('\r').unwrap_or(line)
    }

    /// 段头名：trim 后 `[` 开头、`]` 结尾才认，名字两侧的空白忽略。
    fn header_of(line: &str) -> Option<&str> {
        let t = line.trim();
        (t.len() > 2 && t.starts_with('[') && t.ends_with(']')).then(|| t[1..t.len() - 1].trim())
    }

    /// 注释：行首 `;` 或 `//`。
    fn is_comment(body: &str) -> bool {
        body.starts_with(';') || body.starts_with("//")
    }

    /// `[section]` 下的一个键值：段名键名不分大小写，值两侧去空白，
    /// 空值当「没有」，同名键后写的赢。
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let mut hit: Option<&str> = None;
        let mut now = "";
        for line in &self.lines {
            let body = Self::body(line);
            if let Some(name) = Self::header_of(body) {
                now = name;
                continue;
            }
            let t = body.trim();
            if t.is_empty() || Self::is_comment(t) {
                continue;
            }
            if now.eq_ignore_ascii_case(section)
                && let Some((k, v)) = t.split_once('=')
                && k.trim().eq_ignore_ascii_case(key)
            {
                let v = v.trim();
                if !v.is_empty() {
                    hit = Some(v);
                }
            }
        }
        hit
    }

    /// [`Ini::get`] 的带默认值版：没写就用给的那个。
    pub fn get_or(&self, section: &str, key: &str, default: &str) -> String {
        self.get(section, key)
            .map(str::to_string)
            .unwrap_or_else(|| default.to_string())
    }

    /// 读取一个开关：`y/yes/1/true/on` 是开，`n/no/0/false/off` 是关，
    /// 其它写法（包括没写）都用默认值。
    pub fn get_bool(&self, section: &str, key: &str, default: bool) -> bool {
        match self.get(section, key) {
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "y" | "yes" | "1" | "true" | "on" => true,
                "n" | "no" | "0" | "false" | "off" => false,
                _ => default,
            },
            None => default,
        }
    }

    /// 读取一个无符号数：写法不对或没写都用默认值。
    pub fn get_u32(&self, section: &str, key: &str, default: u32) -> u32 {
        self.get(section, key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
    }

    /// [`Ini::get_u32`] 的有符号版。
    pub fn get_i32(&self, section: &str, key: &str, default: i32) -> i32 {
        self.get(section, key)
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(default)
    }

    /// 各段的名字，文件序；同名段出现两次就出现两次。
    pub fn section_names(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| Self::header_of(Self::body(l)).map(str::to_string))
            .collect()
    }

    /// 每个段一份（同名段各算一份），段名 + 段里的原始键值对，文件序。
    /// 第一个段头之前的内容不属于任何段，不进来 —— 扁平格式走 [`Ini::rows`]。
    pub fn sections(&self) -> Vec<(String, Vec<(String, String)>)> {
        let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for line in &self.lines {
            let body = Self::body(line);
            if let Some(name) = Self::header_of(body) {
                out.push((name.to_string(), Vec::new()));
                continue;
            }
            let t = body.trim();
            if t.is_empty() || Self::is_comment(t) {
                continue;
            }
            if let Some((k, v)) = t.split_once('=')
                && let Some((_, kv)) = out.last_mut()
            {
                kv.push((k.to_string(), v.to_string()));
            }
        }
        out
    }

    /// 一个段的全部原始键值对（同名段合并，文件序；键值不去空白 ——
    /// 要 trim 的语义由调用方定）。
    pub fn rows_in(&self, section: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut now = "";
        for line in &self.lines {
            let body = Self::body(line);
            if let Some(name) = Self::header_of(body) {
                now = name;
                continue;
            }
            if !now.eq_ignore_ascii_case(section) {
                continue;
            }
            let t = body.trim();
            if t.is_empty() || Self::is_comment(t) {
                continue;
            }
            if let Some((k, v)) = t.split_once('=') {
                out.push((k.to_string(), v.to_string()));
            }
        }
        out
    }

    /// 全文件的原始键值对，不看段 —— 扁平格式（只有键值行、没有段头的
    /// 元数据文件）用这个读。
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for line in &self.lines {
            let body = Self::body(line);
            if Self::header_of(body).is_some() {
                continue;
            }
            let t = body.trim();
            if t.is_empty() || Self::is_comment(t) {
                continue;
            }
            if let Some((k, v)) = t.split_once('=') {
                out.push((k.to_string(), v.to_string()));
            }
        }
        out
    }

    /// 名字以 `prefix` 开头（不分大小写）的段名，文件序。
    pub fn sections_with_prefix(&self, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in &self.lines {
            if let Some(name) = Self::header_of(Self::body(line))
                && name.len() > prefix.len()
                && name[..prefix.len()].eq_ignore_ascii_case(prefix)
            {
                out.push(name.to_string());
            }
        }
        out
    }

    /// 写一个键：段里已有同名键就改**最后一处**（保留那一行的缩进与换行符），
    /// 没有就插在这一段最后一行的后面，段不存在就在文件末尾补一段
    /// （前面空一行，让段与段之间隔开）。
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let mut now = String::new();
        let mut hit: Option<usize> = None;
        let mut last_header: Option<usize> = None;
        let mut insert_after: Option<usize> = None;
        for (i, line) in self.lines.iter().enumerate() {
            let body = Self::body(line);
            if let Some(name) = Self::header_of(body) {
                now.clear();
                now.push_str(name);
                if name.eq_ignore_ascii_case(section) {
                    last_header = Some(i);
                    // 段里还没有内容时，追加点就是段头本身。
                    insert_after = Some(i);
                }
                continue;
            }
            if !now.eq_ignore_ascii_case(section) {
                continue;
            }
            let t = body.trim();
            if t.is_empty() || Self::is_comment(t) {
                continue;
            }
            insert_after = Some(i);
            if let Some((k, _)) = t.split_once('=')
                && k.trim().eq_ignore_ascii_case(key)
            {
                hit = Some(i);
            }
        }
        if let Some(i) = hit {
            self.lines[i] = Self::rewritten_row(&self.lines[i], key, value);
            return;
        }
        if let (Some(h), Some(after)) = (last_header, insert_after) {
            // 新行的换行符跟段头走，别在 CRLF 文件里混进一个裸 LF。
            let cr = if self.lines[h].ends_with('\r') { "\r" } else { "" };
            self.lines
                .insert(after + 1, format!("{key}={value}{cr}"));
            return;
        }
        self.append_section(section, &[(key.to_string(), value.to_string())]);
    }

    /// 把一个段的正文换成 `rows`（段在文件里的位置不动；同名段取最后一处），
    /// 段不存在就在文件末尾补一段。
    pub fn replace_section(&mut self, section: &str, rows: &[(String, String)]) {
        let mut header: Option<usize> = None;
        for (i, line) in self.lines.iter().enumerate() {
            if let Some(name) = Self::header_of(Self::body(line))
                && name.eq_ignore_ascii_case(section)
            {
                header = Some(i);
            }
        }
        let Some(h) = header else {
            self.append_section(section, rows);
            return;
        };
        let cr = if self.lines[h].ends_with('\r') { "\r" } else { "" };
        let mut end = h + 1;
        while end < self.lines.len() && Self::header_of(Self::body(&self.lines[end])).is_none() {
            end += 1;
        }
        let block: Vec<String> = std::iter::once(format!("[{section}]{cr}"))
            .chain(rows.iter().map(|(k, v)| format!("{k}={v}{cr}")))
            .collect();
        self.lines.splice(h..end, block);
    }

    /// 删掉**所有**同名段：段头连同段里的行。
    pub fn remove_section(&mut self, section: &str) {
        let mut now = String::new();
        let mut kept: Vec<bool> = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            if let Some(name) = Self::header_of(Self::body(line)) {
                now.clear();
                now.push_str(name);
            }
            kept.push(!now.eq_ignore_ascii_case(section));
        }
        let mut i = 0;
        self.lines.retain(|_| {
            let keep = kept[i];
            i += 1;
            keep
        });
    }

    /// 在文件末尾补一段：前面空一行（跟随文件的换行风格），段名用调用方给的拼写。
    fn append_section(&mut self, section: &str, rows: &[(String, String)]) {
        let cr = if self.crlf { "\r" } else { "" };
        if !self.lines.is_empty() {
            self.lines.push(cr.to_string());
        }
        self.lines.push(format!("[{section}]{cr}"));
        for (k, v) in rows {
            self.lines.push(format!("{k}={v}{cr}"));
        }
    }

    /// 重写一行为 `key=value`，保留原来的缩进和行尾的 `\r` —— 主题文件是
    /// 人排过版的，换一个键不该把版式也换了。
    fn rewritten_row(original: &str, key: &str, value: &str) -> String {
        let body = Self::body(original);
        let indent = &body[..body.len() - body.trim_start().len()];
        let cr = if original.ends_with('\r') { "\r" } else { "" };
        format!("{indent}{key}={value}{cr}")
    }

    /// 还原成文本：行按原始内容用 `\n` 连接（行尾的 `\r` 留在行里），
    /// 解析时以换行结尾的文件还原后也以换行结尾。
    pub fn to_text(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let mut out = self.lines.join("\n");
        if self.trailing_newline {
            out.push('\n');
        }
        out
    }

    /// 写回：UTF-8、无 BOM。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_text())
    }
}

/// 写回 UTF-16LE（带 BOM）的字节。测试构造宽字节文件用。
#[cfg(test)]
fn utf16le_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    out.extend_from_slice(&[0xFF, 0xFE]);
    for u in text.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 字节层 ─────────────────────────────────────────────────────────

    #[test]
    fn bom_order_matches_every_reader() {
        assert_eq!(bom(b""), Bom::None);
        assert_eq!(bom(b"[A]\r\nx=1"), Bom::None);
        assert_eq!(bom(&[0xFF]), Bom::None);
        assert_eq!(bom(&[0xEF, 0xBB, 0xBF, b'[', b'A', b']']), Bom::Utf8);
        assert_eq!(bom(&[0xFF, 0xFE, b'A', 0]), Bom::Utf16Le);
        assert_eq!(bom(&[0xFE, 0xFF, 0, b'A']), Bom::Utf16Be);
        // UTF-32LE 的 BOM 以 UTF-16LE 的开头：历来按 UTF-16LE 处理。
        assert_eq!(bom(&[0xFF, 0xFE, 0, 0]), Bom::Utf16Le);
    }

    #[test]
    fn utf16_body_is_the_original_chunks_exact_walk() {
        let le: Vec<u8> = "A\u{4e2d}"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let be: Vec<u8> = "A\u{4e2d}"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(utf16_body(&le, false), "A\u{4e2d}");
        assert_eq!(utf16_body(&be, true), "A\u{4e2d}");
        // 落单的尾字节被丢掉，不并进最后一个单元。
        let mut odd = le.clone();
        odd.push(0x41);
        assert_eq!(utf16_body(&odd, false), "A\u{4e2d}");
        // 不成对的代理项 -> U+FFFD，不 panic。
        assert_eq!(utf16_body(&[0x00, 0xD8], false), "\u{fffd}");
    }

    #[test]
    fn utf16le_bytes_round_trips_through_utf16_body() {
        let text = "[Setting]\r\nLanguage=2052\r\n";
        let bytes = utf16le_bytes(text);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(bom(&bytes), Bom::Utf16Le);
        assert_eq!(utf16_body(&bytes[2..], false), text);
    }

    #[test]
    fn every_bom_shape_decodes_and_loses_its_bom() {
        let body = "[A]\r\nk=v\r\n";
        let utf8_bom = {
            let mut b = UTF8_BOM.to_vec();
            b.extend_from_slice(body.as_bytes());
            b
        };
        let utf16le = utf16le_bytes(body);
        let utf16be = {
            let mut b = vec![0xFE, 0xFF];
            for u in body.encode_utf16() {
                b.extend_from_slice(&u.to_be_bytes());
            }
            b
        };
        for bytes in [&utf8_bom[..], body.as_bytes(), &utf16le[..], &utf16be[..]] {
            let ini = Ini::from_bytes(bytes).expect("认得的编码不该解失败");
            assert_eq!(ini.get("A", "k").unwrap(), "v");
        }
    }

    /// 混进一个非 UTF-8 字节时宁可当「没配过」，也不要把 U+FFFD 写回去 ——
    /// 那等于替用户改了他没碰的行。
    #[test]
    fn a_non_utf8_file_reads_as_nothing() {
        let bytes = b"[Scripts]\r\nDisabled=\xff\xfe\xff\r\n";
        assert!(Ini::from_bytes(bytes).is_none());
    }

    /// 没有 BOM 的 UTF-16LE 只在语言表那一路认：不带开关时它是「文件不存在」，
    /// 而不是一串 NUL 噪声。
    #[test]
    fn bomless_utf16_needs_the_opt_in() {
        let with_bom = utf16le_bytes("[Info]\r\nAuthor=x\r\n");
        let bytes = &with_bom[2..];
        assert_eq!(bom(bytes), Bom::None);
        assert!(Ini::from_bytes(bytes).is_none());
        assert!(Ini::from_bytes_with(bytes, true).is_some());
    }

    // ── 读取 ───────────────────────────────────────────────────────────

    /// 段名键名不分大小写，两侧空白忽略，值去空白。
    #[test]
    fn lookups_are_case_insensitive_and_trimmed() {
        let ini = Ini::parse("[scripts]\r\n  Disabled = a,b  \r\n");
        assert_eq!(ini.get("Scripts", "disabled").unwrap(), "a,b");
    }

    /// 空值当「没有」；同名键后写的赢 —— 认的是最后一处**非空**值，
    /// 空行不算数。整段只有空值就是「没有」。
    #[test]
    fn a_blank_value_reads_as_absent_and_the_last_duplicate_wins() {
        let ini = Ini::parse("[A]\r\nk=1\r\nk=2\r\nk=3\r\n");
        assert_eq!(ini.get("A", "k").unwrap(), "3");
        assert_eq!(Ini::parse("[A]\r\nk=1\r\nk=\r\n").get("A", "k").unwrap(), "1");
        assert!(Ini::parse("[A]\r\nk=\r\n").get("A", "k").is_none());
    }

    /// 行首 `;` 与 `//` 是注释，段头与键值照常认。
    #[test]
    fn comments_are_skipped() {
        let ini = Ini::parse("[A]\r\n; 说明\r\n// 备注\r\nk=v\r\n");
        assert_eq!(ini.get("A", "k").unwrap(), "v");
        assert_eq!(ini.rows_in("A").len(), 1);
    }

    #[test]
    fn typed_reads_follow_the_ini_spellings() {
        let ini = Ini::parse("[A]\r\non=Y\r\noff=0\r\nodd=maybe\r\nn=257\r\ns=-4\r\n");
        assert!(ini.get_bool("A", "on", false));
        assert!(!ini.get_bool("A", "off", true));
        // 认不得的写法用默认值。
        assert!(ini.get_bool("A", "odd", true));
        assert!(!ini.get_bool("A", "odd", false));
        assert_eq!(ini.get_u32("A", "n", 0), 257);
        assert_eq!(ini.get_i32("A", "s", 0), -4);
        assert_eq!(ini.get_u32("A", "missing", 7), 7);
    }

    #[test]
    fn sections_and_rows_come_back_in_file_order() {
        let ini = Ini::parse("[B]\r\nb=1\r\n[A]\r\na=1\r\na=2\r\n[B]\r\nb2=2\r\n");
        assert_eq!(ini.section_names(), ["B", "A", "B"]);
        let names: Vec<_> = ini.sections().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["B", "A", "B"]);
        assert_eq!(
            ini.rows_in("A"),
            [("a".to_string(), "1".to_string()), ("a".to_string(), "2".to_string())]
        );
        // 扁平读法不看段。
        assert_eq!(ini.rows().len(), 4);
        // 前缀认的是「还有下文」的段名：`[Temp]` 自己不算 `Temp*` 的一员。
        let temps = Ini::parse("[Temp]\r\nk=1\r\n[Temp1]\r\nx=2\r\n[TEMP2]\r\n");
        assert_eq!(temps.sections_with_prefix("Temp"), ["Temp1", "TEMP2"]);
    }

    // ── 编辑与写回 ─────────────────────────────────────────────────────

    /// 改一个已写的键：只动那一行，注释、空行、别的段一字不动；缩进保留。
    #[test]
    fn set_rewrites_the_last_occurrence_and_keeps_the_layout() {
        let mut ini = Ini::parse("[A]\r\nk=1\r\n; 注释\r\nk=2\r\n[B]\r\nb=1\r\n");
        ini.set("A", "k", "9");
        assert_eq!(
            ini.to_text(),
            "[A]\r\nk=1\r\n; 注释\r\nk=9\r\n[B]\r\nb=1\r\n"
        );
        let mut indented = Ini::parse("[A]\n  k = 1\n");
        indented.set("A", "k", "2");
        assert_eq!(indented.to_text(), "[A]\n  k=2\n");
    }

    /// 键还没有：追加在这一段**末尾**，不是文件末尾。
    #[test]
    fn set_appends_at_the_end_of_its_section() {
        let mut ini = Ini::parse("[A]\nk=1\n[B]\nb=1\n");
        ini.set("A", "k2", "2");
        assert_eq!(ini.to_text(), "[A]\nk=1\nk2=2\n[B]\nb=1\n");
        // 空段也要能写：新行插在段头后面，换行符跟段头走。
        let mut crlf = Ini::parse("[A]\r\n[B]\r\n");
        crlf.set("B", "b", "1");
        assert_eq!(crlf.to_text(), "[A]\r\n[B]\r\nb=1\r\n");
    }

    /// 段不在：整段补在文件末尾，前面空一行；新行跟文件的换行风格。
    #[test]
    fn set_appends_a_new_section_after_a_blank_line() {
        let mut lf = Ini::parse("[A]\nk=1\n");
        lf.set("B", "b", "1");
        assert_eq!(lf.to_text(), "[A]\nk=1\n\n[B]\nb=1\n");
        let mut crlf = Ini::parse("[A]\r\nk=1\r\n");
        crlf.set("B", "b", "1");
        assert_eq!(crlf.to_text(), "[A]\r\nk=1\r\n\r\n[B]\r\nb=1\r\n");
        let mut fresh = Ini::new();
        fresh.set("A", "k", "1");
        assert_eq!(fresh.to_text(), "[A]\r\nk=1\r\n");
    }

    /// 同名段全删，别的一段不动。
    #[test]
    fn remove_section_drops_every_copy() {
        let mut ini = Ini::parse("[A]\r\na=1\r\n[B]\r\nb=1\r\n[A]\r\na2=2\r\n");
        ini.remove_section("a");
        assert_eq!(ini.to_text(), "[B]\r\nb=1\r\n");
    }

    /// 换段身：段的位置不动，正文整块替换；段不在就补在末尾。
    #[test]
    fn replace_section_keeps_its_place() {
        let mut ini = Ini::parse("[A]\r\nold=1\r\n[B]\r\nb=1\r\n");
        ini.replace_section(
            "A",
            &[("new".to_string(), "2".to_string())],
        );
        assert_eq!(ini.to_text(), "[A]\r\nnew=2\r\n[B]\r\nb=1\r\n");
        ini.replace_section("C", &[("c".to_string(), "3".to_string())]);
        assert_eq!(ini.to_text(), "[A]\r\nnew=2\r\n[B]\r\nb=1\r\n\r\n[C]\r\nc=3\r\n");
    }

    /// 以换行结尾的文件还原后也以换行结尾；没结尾的就不添。
    #[test]
    fn to_text_preserves_the_trailing_newline() {
        assert_eq!(Ini::parse("[A]\nk=1\n").to_text(), "[A]\nk=1\n");
        assert_eq!(Ini::parse("[A]\nk=1").to_text(), "[A]\nk=1");
        assert_eq!(Ini::new().to_text(), "");
    }

    /// 文件往返：解析再还原，一个字节都不变。
    #[test]
    fn a_round_trip_is_byte_identical() {
        let src = ";; 我的主题\r\n[Theme]\r\nCaret=#E6E9EF\r\n\r\n;; 页签\r\nTab=#2B2B2B\r\n[Style]\r\nPair=257\r\n";
        assert_eq!(Ini::parse(src).to_text(), src);
    }
}
