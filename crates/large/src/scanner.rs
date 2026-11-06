
use crate::lineindex::MAX_LINE_BYTES;

/// 编码类别（决定终止符的匹配方式与 EOL 的字节/字符宽度）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EncKind {
    /// UTF-8 与所有 ASCII 兼容单/双字节编码（GBK、Shift_JIS、
    /// Windows-1252…）。`\n`/`\r` 字节在这些编码里不会作为多字节
    /// 序列的尾字节出现，按字节切行是安全的（GBK 等双字节编码的
    /// 尾字节范围均不含 0x0A/0x0D）。
    AsciiCompat { utf8: bool },
    Utf16Le,
    Utf16Be,
}

impl EncKind {
    /// EOL kind 的字节宽度：CRLF=2，LF/CR=1（UTF-16 翻倍）。
    pub fn eol_bytes(&self, kind: u8) -> u64 {
        let base = match kind {
            1 => 2,
            2 | 3 => 1,
            _ => 0,
        };
        match self {
            EncKind::AsciiCompat { .. } => base,
            _ => base * 2,
        }
    }

    /// EOL kind 的字符宽度：CRLF=2，LF/CR=1，无=0（与编码无关）。
    pub fn eol_chars(kind: u8) -> u64 {
        match kind {
            1 => 2,
            2 | 3 => 1,
            _ => 0,
        }
    }

    /// 一段字节的解码字符数（UTF-8 精确；UTF-16=字节/2，代理对计
    /// 2 字符仅影响 emoji 行；其它编码按字节近似 —— 计划风险 R4）。
    pub fn count_chars(&self, seg: &[u8]) -> u64 {
        match self {
            EncKind::AsciiCompat { utf8: true } => {
                seg.iter().filter(|&&b| b & 0xC0 != 0x80).count() as u64
            }
            EncKind::AsciiCompat { utf8: false } => seg.len() as u64,
            _ => seg.len() as u64 / 2,
        }
    }

    /// chunk 内某偏移是否是给定编码的一个 CR code unit。
    fn cr_at(&self, data: &[u8], i: usize) -> bool {
        match self {
            EncKind::AsciiCompat { .. } => data.get(i) == Some(&b'\r'),
            EncKind::Utf16Le => data.get(i) == Some(&0x0D) && data.get(i + 1) == Some(&0x00),
            EncKind::Utf16Be => data.get(i) == Some(&0x00) && data.get(i + 1) == Some(&0x0D),
        }
    }

    fn unit(&self) -> usize {
        match self {
            EncKind::AsciiCompat { .. } => 1,
            _ => 2,
        }
    }
}

/// 产出一行：内容区绝对起点、含 EOL 的字节长、EOL kind、含 EOL 的
/// 字符数。
pub type LineSink<'a> = dyn FnMut(u64, u64, u8, u64) + 'a;

/// 喂入状态。一个实例从 `content_start` 起顺序喂到 EOF。
pub struct LineFeed {
    kind: EncKind,
    open_start: u64,
    open_bytes: u64,
    open_chars: u64,
    pending_cr: bool,
    /// 是否发出过带行尾符的行（EOF 时据此补末空行，对齐
    /// Document/scan_eol 的行数语义：以行尾符结尾的文件
    /// 会多出一行末空行）。
    emitted_eol: bool,
}

impl LineFeed {
    pub fn new(kind: EncKind, content_start: u64) -> Self {
        Self {
            kind,
            open_start: content_start,
            open_bytes: 0,
            open_chars: 0,
            pending_cr: false,
            emitted_eol: false,
        }
    }

    /// 喂入一块（`data` 从绝对偏移 `off` 开始）。UTF-16 时
    /// `off - open_start` 的奇偶必须与块序列一致（调用方保证等长
    /// 等齐的块）。产出按文件顺序触发行回调。
    pub fn feed(&mut self, off: u64, data: &[u8], out: &mut LineSink) {
        let _ = off; // 偏移由 open_start/open_bytes 隐含
        let unit = self.kind.unit();
        let mut pos = 0usize; // chunk 内段起点（字节）
        // 上块尾悬挂的 \r：由本块首单元裁决 CRLF 还是 lone CR。
        if self.pending_cr {
            self.pending_cr = false;
            let first_is_lf = match self.kind {
                EncKind::AsciiCompat { .. } => data.first() == Some(&b'\n'),
                EncKind::Utf16Le => data.starts_with(&[0x0A, 0x00]),
                EncKind::Utf16Be => data.starts_with(&[0x00, 0x0A]),
            };
            if first_is_lf {
                self.emit(out, 1);
                pos = unit;
            } else {
                self.emit(out, 3);
            }
        }
        // ASCII 兼容：SIMD 迭代器建一次，跨 while 复用。
        let mut ascii_it = match self.kind {
            EncKind::AsciiCompat { .. } => Some(memchr::memchr2_iter(b'\r', b'\n', data)),
            _ => None,
        };
        let mut manual = pos; // UTF-16 手扫游标
        loop {
            let (t, is_cr) = match &mut ascii_it {
                Some(it) => match it.next() {
                    Some(i) => (i, data[i] == b'\r'),
                    None => break,
                },
                None => {
                    let mut i = manual;
                    let mut found = None;
                    while i + 1 < data.len() {
                        if self.kind.cr_at(data, i) {
                            found = Some((i, true));
                            break;
                        }
                        let (lf_le, lf_be) = (data[i] == 0x0A && data[i + 1] == 0x00, data[i] == 0x00 && data[i + 1] == 0x0A);
                        if matches!(self.kind, EncKind::Utf16Le) && lf_le
                            || matches!(self.kind, EncKind::Utf16Be) && lf_be
                        {
                            found = Some((i, false));
                            break;
                        }
                        i += 2;
                    }
                    match found {
                        Some(v) => {
                            manual = v.0 + unit;
                            v
                        }
                        None => break,
                    }
                }
            };
            if t < pos {
                continue; // 块首裁决已消费的终止符
            }
            let seg = &data[pos..t];
            self.accumulate(seg, out);
            if is_cr {
                // CR：下一单元是 LF 则并入下一段（由 LF 处合并成
                // CRLF）；块尾则悬挂；否则 lone CR 立即收行。
                let next_is_lf = match self.kind {
                        EncKind::AsciiCompat { .. } => data.get(t + unit) == Some(&b'\n'),
                        EncKind::Utf16Le => {
                            data.get(t + unit) == Some(&0x0A) && data.get(t + unit + 1) == Some(&0x00)
                        }
                        EncKind::Utf16Be => {
                            data.get(t + unit) == Some(&0x00) && data.get(t + unit + 1) == Some(&0x0A)
                        }
                    };
                if next_is_lf {
                    pos = t + unit; // \r 计入下一段
                } else if t + unit >= data.len() {
                    // 块尾悬挂 \r：段（data[pos..t]）已在上方累计，
                    // \r 本身不计入 open，等下块首字节裁决。
                    self.pending_cr = true;
                    return;
                } else {
                    self.emit(out, 3);
                    pos = t + unit;
                }
            } else {
                // LF：紧邻前一单元是 \r → CRLF（该 \r 已在段内）。
                let kind = if self.kind.cr_at(data, t.saturating_sub(unit)) && t >= unit {
                    1
                } else {
                    2
                };
                self.emit(out, kind);
                pos = t + unit;
            }
        }
        // 块尾：剩余未闭合段计入 open。
        self.accumulate(&data[pos..], out);
    }

    /// EOF 冲刷：未闭合行以 kind=0 收行（无行尾，或续行切分）。 文件以行尾符结尾时补一行末空行（与
    /// `Document::from_text` 的行数语义一致）。
    pub fn finish(&mut self, out: &mut LineSink) {
        if self.open_bytes > 0 {
            (out)(self.open_start, self.open_bytes, 0, self.open_chars);
            self.open_start += self.open_bytes;
            self.open_bytes = 0;
            self.open_chars = 0;
        } else if self.emitted_eol {
            (out)(self.open_start, 0, 0, 0);
        }
        self.pending_cr = false;
    }

    /// 未闭合行的累计字节数。
    pub fn open_bytes(&self) -> u64 {
        self.open_bytes
    }

    /// 把一段（无终止符的）行内容并入 open，超过 [`MAX_LINE_BYTES`]
    /// 的部分切成 kind=0 续行。维持 `open_bytes < MAX` 不变量。
    fn accumulate(&mut self, seg: &[u8], out: &mut LineSink) {
        if self.open_bytes + (seg.len() as u64) < MAX_LINE_BYTES {
            self.open_bytes += seg.len() as u64;
            self.open_chars += self.kind.count_chars(seg);
            return;
        }
        let mut taken = 0usize;
        while self.open_bytes + (seg.len() - taken) as u64 >= MAX_LINE_BYTES {
            let take = ((MAX_LINE_BYTES - self.open_bytes) as usize).min(seg.len() - taken);
            let chars = self.kind.count_chars(&seg[taken..taken + take]);
            (out)(
                self.open_start,
                self.open_bytes + take as u64,
                0,
                self.open_chars + chars,
            );
            self.open_start += self.open_bytes + take as u64;
            self.open_bytes = 0;
            self.open_chars = 0;
            taken += take;
        }
        let rest = &seg[taken..];
        self.open_bytes += rest.len() as u64;
        self.open_chars += self.kind.count_chars(rest);
    }

    fn emit(&mut self, out: &mut LineSink, kind: u8) {
        if kind != 0 {
            self.emitted_eol = true;
        }
        let eol_b = self.kind.eol_bytes(kind);
        let eol_c = EncKind::eol_chars(kind);
        (out)(
            self.open_start,
            self.open_bytes + eol_b,
            kind,
            self.open_chars + eol_c,
        );
        self.open_start += self.open_bytes + eol_b;
        self.open_bytes = 0;
        self.open_chars = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(kind: EncKind, start: u64, chunks: &[&[u8]]) -> Vec<(u64, u64, u8, u64)> {
        let mut f = LineFeed::new(kind, start);
        let mut out = Vec::new();
        let mut off = start;
        for c in chunks {
            let mut sink = |a: u64, b: u64, k: u8, ch: u64| out.push((a, b, k, ch));
            f.feed(off, c, &mut sink);
            off += c.len() as u64;
        }
        let mut sink = |a: u64, b: u64, k: u8, ch: u64| out.push((a, b, k, ch));
        f.finish(&mut sink);
        out
    }

    fn ascii(utf8: bool) -> EncKind {
        EncKind::AsciiCompat { utf8 }
    }

    #[test]
    fn lf_lines() {
        let v = collect(ascii(true), 0, &[b"ab\ncd\n"]);
        // 以行尾符结尾：补一行末空行（Document 同语义）
        assert_eq!(v, vec![(0, 3, 2, 3), (3, 3, 2, 3), (6, 0, 0, 0)]);
    }

    #[test]
    fn crlf_and_cr() {
        let v = collect(ascii(true), 0, &[b"a\r\nb\rc"]);
        assert_eq!(v, vec![(0, 3, 1, 3), (3, 2, 3, 2), (5, 1, 0, 1)]);
    }

    #[test]
    fn cr_lf_directly_after_cr() {
        // "a\r\r\n"：行0 = "a"+CR，行1 = CRLF 空行，行2 = 末空行
        let v = collect(ascii(true), 0, &[b"a\r\r\n"]);
        assert_eq!(v, vec![(0, 2, 3, 2), (2, 2, 1, 2), (4, 0, 0, 0)]);
    }

    #[test]
    fn no_trailing_eol() {
        let v = collect(ascii(true), 0, &[b"hello"]);
        assert_eq!(v, vec![(0, 5, 0, 5)]);
        let v = collect(ascii(true), 0, &[b"a\nb"]);
        assert_eq!(v, vec![(0, 2, 2, 2), (2, 1, 0, 1)]);
    }

    #[test]
    fn empty_chunk_stream() {
        let v = collect(ascii(true), 0, &[b""]);
        assert!(v.is_empty());
    }

    /// 块边界切开 CRLF / CR / 行本体：悬挂状态跨块正确裁决。
    #[test]
    fn terminator_split_across_chunks() {
        // CRLF 跨块
        let v = collect(ascii(true), 0, &[b"ab\r", b"\ncd"]);
        assert_eq!(v, vec![(0, 4, 1, 4), (4, 2, 0, 2)]);
        // lone CR 跨块（下一块首不是 \n）
        let v = collect(ascii(true), 0, &[b"ab\r", b"cd"]);
        assert_eq!(v, vec![(0, 3, 3, 3), (3, 2, 0, 2)]);
        // 行本体跨块
        let v = collect(ascii(true), 0, &[b"ab", b"cd\n"]);
        assert_eq!(v, vec![(0, 5, 2, 5), (5, 0, 0, 0)]);
        // 块边界正落在 \r\n 中间且 \r 之后无内容再接新行
        let v = collect(ascii(true), 0, &[b"x\r", b"\n", b"y\n"]);
        assert_eq!(v, vec![(0, 3, 1, 3), (3, 2, 2, 2), (5, 0, 0, 0)]);
    }

    #[test]
    fn utf8_chars_exact() {
        let v = collect(ascii(true), 0, &["世界\n".as_bytes()]);
        assert_eq!(v, vec![(0, 7, 2, 3), (7, 0, 0, 0)]);
    }

    #[test]
    fn utf16le_lines() {
        let bytes: Vec<u8> = "a\r\nb\nc"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        // 结尾无行尾符：不补末空行
        let v = collect(EncKind::Utf16Le, 2, &[&bytes]);
        assert_eq!(v, vec![(2, 6, 1, 3), (8, 4, 2, 2), (12, 2, 0, 1)]);
    }

    #[test]
    fn utf16le_crlf_across_chunks() {
        let units: Vec<u8> = "a\r".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let rest: Vec<u8> = "\nb".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let v = collect(EncKind::Utf16Le, 0, &[&units, &rest]);
        assert_eq!(v, vec![(0, 6, 1, 3), (6, 2, 0, 1)]);
    }

    /// 单行超过 MAX_LINE_BYTES 切成 kind=0 续行；跨块累积同样生效。
    #[test]
    fn overlong_line_splits() {
        let mut buf = vec![b'a'; MAX_LINE_BYTES as usize + 10];
        buf.push(b'\n');
        let v = collect(ascii(true), 0, &[&buf]);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], (0, MAX_LINE_BYTES, 0, MAX_LINE_BYTES));
        assert_eq!(v[1], (MAX_LINE_BYTES, 11, 2, 11));
        assert_eq!(v[2], (MAX_LINE_BYTES + 11, 0, 0, 0));
    }

    #[test]
    fn overlong_line_splits_across_chunks() {
        let total = MAX_LINE_BYTES as usize * 2 + 100;
        let a = vec![b'a'; total / 2];
        let b = vec![b'a'; total - total / 2];
        let v = collect(ascii(true), 0, &[&a, &b]);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], (0, MAX_LINE_BYTES, 0, MAX_LINE_BYTES));
        assert_eq!(v[1], (MAX_LINE_BYTES, MAX_LINE_BYTES, 0, MAX_LINE_BYTES));
        assert_eq!(
            v[2],
            (MAX_LINE_BYTES * 2, 100, 0, 100)
        );
    }
}
