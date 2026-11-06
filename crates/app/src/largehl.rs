//! 大文件行式词法器。
//!
//! tree-sitter 在 GB 级文本上不可用（树内存 >10× 源文件、GLR 吞吐
//! 仅数十 MB/s），大文件回到 Scintilla 的 line-state 模型：每行独立
//! 词法化成 `(颜色, 字符长度)` run 列表 —— 与 [`crate::app::Editor`]
//! 的 `highlight_for` 消费 tree-sitter 输出的形状相同，paint 管线
//! 不需要任何新分支。run 从行首连续覆盖到行尾（default 色的区间
//! 也显式给出，查找结果行的 grep 路径同款），相邻同色合并。
//!
//! 代价与护栏：无跨行状态（jsonl 恰好每行一个值；log 的续行 v1 不
//! 染色），超长行（> [`MAX_HL_CHARS`]）直接放弃染色。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use dg_syntax::ThemeConfig;

/// 单行染色上限（字符）：超过即整行退回纯色。一个 20k 字符的行已
/// 经横跨几屏，染色的可读性收益趋零，而词法开销还在。
const MAX_HL_CHARS: usize = 20_000;

/// 行缓存容量：可视窗口 ~200 行，外加书标/跳转触碰的行；再多就是
/// 在为不存在的访问模式付内存。
const CACHE_LINES: usize = 4096;

/// 行式词法器的调色板，从 tree-sitter 主题派生（新增 error/warning
/// 两档，缺省 VS 红黄），保证与普通文档的观感一致。
#[derive(Clone, Copy)]
pub struct HlPalette {
    pub default: u32,
    pub timestamp: u32,
    pub error: u32,
    pub warning: u32,
    pub debug: u32,
    pub keyword: u32,
    pub key: u32,
    pub string: u32,
    pub number: u32,
    pub punct: u32,
}

impl HlPalette {
    pub fn from_theme(t: &ThemeConfig) -> Self {
        // 主题没有 error/warning 档时给固定的红/黄（Default 主题本身
        // 也是硬编码的 VS 色板）。
        let (error, warning) = (0xF14C4C, 0xD7BA7D);
        Self {
            default: t.default_color,
            timestamp: t.color_for("number"),
            error,
            warning,
            debug: t.color_for("comment"),
            keyword: t.color_for("keyword"),
            key: t.color_for("property"),
            string: t.color_for("string"),
            number: t.color_for("number"),
            punct: t.color_for("punctuation"),
        }
    }
}

/// Run 构造器：字符游标从行首推进，每个区间显式染色（不染的区间
/// 用 [`Runs::plain`] 补 default 色），相邻同色自动合并。
struct Runs {
    default: u32,
    out: Vec<(u32, u32)>,
    at: usize,
}

impl Runs {
    fn new(default: u32) -> Self {
        Self {
            default,
            out: Vec::new(),
            at: 0,
        }
    }

    /// 把 [`self.at`, end) 染成 `color` 并推进游标。
    fn span(&mut self, end: usize, color: u32) {
        if end <= self.at {
            return;
        }
        let len = (end - self.at) as u32;
        if let Some(last) = self.out.last_mut() {
            if last.0 == color {
                last.1 += len;
                self.at = end;
                return;
            }
        }
        self.out.push((color, len));
        self.at = end;
    }

    /// 把 [`self.at`, end) 补成 default 色并推进游标。
    fn plain(&mut self, end: usize) {
        let d = self.default;
        self.span(end, d);
    }

    /// 收尾：行尾剩余区间补 default。
    ///
    /// 字形位在这一层恒为 0：大文件走的是行式词法器，没有语法树也就没有
    /// 「哪一段是注释」的结论，`[Style]` 在这里无从下手。
    fn finish(mut self, line_chars: usize) -> Vec<(u32, u32, u8)> {
        self.plain(line_chars);
        self.out.into_iter().map(|(c, l)| (c, l, 0)).collect()
    }
}

/// 大文件的行词法器：按扩展名挑选，未识别的扩展名不染色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LargeLexer {
    Log,
    Jsonl,
    Csv { delim: char },
}

impl LargeLexer {
    pub fn for_extension(ext: &str) -> Option<Self> {
        match ext {
            "log" | "txt" | "out" | "trace" => Some(LargeLexer::Log),
            "jsonl" | "ndjson" => Some(LargeLexer::Jsonl),
            "csv" => Some(LargeLexer::Csv { delim: ',' }),
            "tsv" | "tab" => Some(LargeLexer::Csv { delim: '\t' }),
            _ => None,
        }
    }

    /// 一行的 run 列表。`line_idx` 供需要"首行"语义的词法器（CSV
    /// 表头）使用；超长行返回 `None`（纯色）。
    pub fn runs(self, line_idx: usize, text: &str, p: &HlPalette) -> Option<Vec<(u32, u32, u8)>> {
        let n = text.chars().count();
        if n > MAX_HL_CHARS {
            return None;
        }
        let mut b = Runs::new(p.default);
        match self {
            LargeLexer::Log => log_line(text, p, &mut b),
            LargeLexer::Jsonl => json_line(text, p, &mut b),
            LargeLexer::Csv { delim } => csv_line(line_idx, text, p, &mut b, delim),
        }
        Some(b.finish(n))
    }
}

fn log_line(text: &str, p: &HlPalette, b: &mut Runs) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    // 前导空白
    while i < n && chars[i] == ' ' {
        i += 1;
    }
    b.plain(i);
    // 时间戳：[2026-10-25 12:34:56,789] 或裸的 2026-10-25T12:34:56.123Z
    if i < n && chars[i] == '[' {
        if let Some(close) = chars[i..].iter().position(|&c| c == ']') {
            if chars[i + 1].is_ascii_digit() {
                b.span(i + close + 1, p.timestamp);
                i += close + 1;
            }
        }
    } else if i + 4 < n
        && chars[i..i + 4].iter().all(|c| c.is_ascii_digit())
        && chars[i + 4] == '-'
    {
        let ts_start = i;
        while i < n
            && (chars[i].is_ascii_digit() || matches!(chars[i], '-' | ':' | '.' | ',' | 'T' | 'Z' | '+' | ' '))
        {
            i += 1;
        }
        // 时间戳字符集里有空格（"2026-10-25 12:34:56"），日期与级别
        // 之间的分隔空格不能被染进时间戳。
        let mut e = i;
        while e > ts_start && chars[e - 1] == ' ' {
            e -= 1;
        }
        b.span(e, p.timestamp);
        i = e;
    }
    while i < n && chars[i] == ' ' {
        i += 1;
    }
    b.plain(i);
    // 级别词：ERROR/WARN/INFO/DEBUG…（大小写不敏感）
    let word_start = i;
    while i < n && (chars[i].is_ascii_alphabetic() || chars[i] == '_') {
        i += 1;
    }
    if i > word_start {
        let word: String = chars[word_start..i].iter().collect();
        let color = match word.to_ascii_uppercase().as_str() {
            "ERROR" | "FATAL" | "SEVERE" | "CRITICAL" | "CRIT" | "PANIC" => p.error,
            "WARN" | "WARNING" => p.warning,
            "DEBUG" | "TRACE" | "FINE" | "FINER" | "FINEST" => p.debug,
            _ => p.default,
        };
        b.span(i, color);
    }
    // 余下部分：key=value 的 key 染色，"引号串" 染色。
    while i < n {
        match chars[i] {
            '"' => {
                i += 1;
                while i < n {
                    if chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                b.span(i.min(n), p.string);
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let start = i;
                while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                // key=value / key: value 的词是 key（纯字母词才配冒号，
                // 免得把 "12:34" 的 12 染成 key）
                let mut j = i;
                while j < n && chars[j] == ' ' {
                    j += 1;
                }
                let is_key = j < n
                    && (chars[j] == '='
                        || (chars[j] == ':'
                            && chars[start..i]
                                .iter()
                                .all(|c| c.is_ascii_alphabetic() || *c == '_')));
                if is_key {
                    b.span(i, p.key);
                } else {
                    b.plain(i);
                }
            }
            _ => {
                i += 1;
                b.plain(i);
            }
        }
    }
}

/// 单行 JSON（JSONL）：字符串按后随 `:` 区分 key/value；数字、字面
/// 量、标点各自染色。非法行（比如多行 JSON 的中段）自然退化为大
/// 部分纯色，无碍。
fn json_line(text: &str, p: &HlPalette, b: &mut Runs) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        match chars[i] {
            '"' => {
                let mut j = i + 1;
                while j < n {
                    if chars[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if chars[j] == '"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                let end = j.min(n);
                // 后一个非空白字符是 ':' ⇒ 这是一个 key
                let mut k = end;
                while k < n && chars[k] == ' ' {
                    k += 1;
                }
                let color = if k < n && chars[k] == ':' { p.key } else { p.string };
                b.span(end, color);
                i = end;
            }
            c if c.is_ascii_digit() || (c == '-' && i + 1 < n && chars[i + 1].is_ascii_digit()) => {
                let mut j = i;
                while j < n && matches!(chars[j], '0'..='9' | '.' | 'e' | 'E' | '+' | '-') {
                    j += 1;
                }
                b.span(j, p.number);
                i = j;
            }
            't' | 'f' | 'n' => {
                let rest: String = chars[i..(i + 5).min(n)].iter().collect();
                match ["true", "false", "null"].iter().find(|l| rest.starts_with(**l)) {
                    Some(l) => {
                        b.span(i + l.len(), p.keyword);
                        i += l.len();
                    }
                    None => {
                        i += 1;
                        b.plain(i);
                    }
                }
            }
            '{' | '}' | '[' | ']' | ',' | ':' => {
                b.span(i + 1, p.punct);
                i += 1;
            }
            _ => {
                i += 1;
                b.plain(i);
            }
        }
    }
}

/// CSV/TSV：首行是表头（整行 key 色），数据行里引号字段染 string
/// 色、分隔符染 punct 色。
fn csv_line(line_idx: usize, text: &str, p: &HlPalette, b: &mut Runs, delim: char) {
    let chars: Vec<char> = text.chars().collect();
    if line_idx == 0 {
        b.span(chars.len(), p.key);
        return;
    }
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        match chars[i] {
            '"' => {
                let mut j = i + 1;
                while j < n {
                    if chars[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if chars[j] == '"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                let end = j.min(n);
                b.span(end, p.string);
                i = end;
            }
            c if c == delim => {
                b.span(i + 1, p.punct);
                i += 1;
            }
            _ => {
                i += 1;
                b.plain(i);
            }
        }
    }
}

/// 每行 runs 的 LRU 缓存（滚动时命中可视窗口，跳转时最多冷一行）。
pub struct LargeHl {
    pub lexer: LargeLexer,
    pub palette: HlPalette,
    cache: RefCell<HashMap<usize, Vec<(u32, u32, u8)>>>,
    order: RefCell<VecDeque<usize>>,
}

impl LargeHl {
    pub fn new(lexer: LargeLexer, theme: &ThemeConfig) -> Self {
        Self {
            lexer,
            palette: HlPalette::from_theme(theme),
            cache: RefCell::new(HashMap::new()),
            order: RefCell::new(VecDeque::new()),
        }
    }

    pub fn get(&self, line: usize) -> Option<Vec<(u32, u32, u8)>> {
        self.cache.borrow().get(&line).cloned()
    }

    pub fn insert(&self, line: usize, runs: Vec<(u32, u32, u8)>) {
        let mut cache = self.cache.borrow_mut();
        let mut order = self.order.borrow_mut();
        if cache.len() >= CACHE_LINES {
            if let Some(old) = order.pop_front() {
                cache.remove(&old);
            }
        }
        cache.insert(line, runs);
        order.push_back(line);
    }

    /// 行号在编辑后整体失效（片链 splice 改变行号）：全清。
    /// LRU 容量本就只有 4096 行，重建是按需的（paint 触到才词法化）。
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
        self.order.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> HlPalette {
        HlPalette::from_theme(&ThemeConfig::default())
    }

    /// 展开成 (颜色, 片段)：run 从行首连续覆盖，逐段切片即可。
    fn colors(lexer: LargeLexer, line_idx: usize, text: &str) -> Vec<(u32, String)> {
        let p = pal();
        let runs = lexer.runs(line_idx, text, &p).unwrap();
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0usize;
        runs.into_iter()
            .map(|(c, l, _)| {
                let s: String = chars[at..at + l as usize].iter().collect();
                at += l as usize;
                (c, s)
            })
            .collect()
    }

    #[test]
    fn log_timestamp_and_level() {
        let runs = colors(
            LargeLexer::Log,
            0,
            "2026-10-25T12:34:56.789Z ERROR disk full",
        );
        let p = pal();
        assert!(runs.contains(&(p.timestamp, "2026-10-25T12:34:56.789Z".into())));
        assert!(runs.contains(&(p.error, "ERROR".into())));
    }

    #[test]
    fn log_bracketed_timestamp_and_kv() {
        let runs = colors(
            LargeLexer::Log,
            0,
            "[2026-10-25 12:34:56] WARN path=/tmp/x status=\"bad\"",
        );
        let p = pal();
        assert!(runs.contains(&(p.timestamp, "[2026-10-25 12:34:56]".into())));
        assert!(runs.contains(&(p.warning, "WARN".into())));
        assert!(runs.contains(&(p.key, "path".into())));
        assert!(runs.contains(&(p.string, "\"bad\"".into())));
    }

    #[test]
    fn log_level_word_not_a_key() {
        // "disk full" 里的普通词不应被染成 key（后随的不是 = / :）；
        // INFO 保持 default 色（只有 ERROR/WARN/DEBUG 换色）。
        let runs = colors(LargeLexer::Log, 0, "INFO disk full");
        let p = pal();
        assert!(!runs.contains(&(p.key, "disk".into())));
        assert_eq!(runs, vec![(p.default, "INFO disk full".into())]);
    }

    #[test]
    fn jsonl_keys_strings_numbers_literals() {
        let runs = colors(
            LargeLexer::Jsonl,
            0,
            "{\"ts\": 12.5, \"ok\": true, \"err\": null, \"name\": \"a\\\"b\"}",
        );
        let p = pal();
        assert!(runs.contains(&(p.key, "\"ts\"".into())));
        assert!(runs.contains(&(p.number, "12.5".into())));
        assert!(runs.contains(&(p.keyword, "true".into())));
        assert!(runs.contains(&(p.keyword, "null".into())));
        assert!(runs.contains(&(p.string, "\"a\\\"b\"".into())));
        assert!(runs.contains(&(p.punct, "{".into())));
    }

    #[test]
    fn jsonl_value_string_vs_key() {
        let p = pal();
        let runs = colors(LargeLexer::Jsonl, 3, "{\"a\": \"b\"}");
        assert!(runs.contains(&(p.key, "\"a\"".into())));
        assert!(runs.contains(&(p.string, "\"b\"".into())));
    }

    #[test]
    fn csv_header_and_quoted_fields() {
        let p = pal();
        let head = colors(LargeLexer::Csv { delim: ',' }, 0, "id,name,score");
        assert_eq!(head, vec![(p.key, "id,name,score".into())]);
        let row = colors(LargeLexer::Csv { delim: ',' }, 1, "7,\"Smith, J\",9");
        assert!(row.contains(&(p.punct, ",".into())));
        assert!(row.contains(&(p.string, "\"Smith, J\"".into())));
    }

    #[test]
    fn runs_cover_the_whole_line() {
        // run 连续覆盖整行：总长度 == 行字符数（消费端逐段推进）。
        for (lexer, text) in [
            (LargeLexer::Log, "2026-10-25 INFO a=b c"),
            (LargeLexer::Jsonl, "{\"x\": [1, -2.5e3]}"),
            (LargeLexer::Csv { delim: ',' }, "a,b,\"c\""),
        ] {
            let runs = lexer.runs(0, text, &pal()).unwrap();
            let total: u32 = runs.iter().map(|r| r.1).sum();
            assert_eq!(total as usize, text.chars().count(), "{text}");
        }
    }

    #[test]
    fn overlong_line_gets_no_color() {
        let long = "x".repeat(MAX_HL_CHARS + 1);
        assert!(LargeLexer::Log.runs(0, &long, &pal()).is_none());
    }

    #[test]
    fn unknown_extension_has_no_lexer() {
        assert!(LargeLexer::for_extension("bin").is_none());
        assert!(LargeLexer::for_extension("log").is_some());
        assert!(LargeLexer::for_extension("jsonl").is_some());
        assert!(LargeLexer::for_extension("tsv").is_some());
    }
}
