//! 两级行偏移索引（plan §4.2；klogg 的 128 行/块方案，v1 用裸 u32
//! 增量不做 varint 压缩 —— 4 B/行 + 8 B/128 行）。
//!
//! 每行两条记录：
//! - `packed`：字节长（含行尾符，低 30 位）| EOL kind（高 2 位，
//! 与 `dg_core::linedoc` 的 wire 编码一致：1=CRLF 2=LF 3=CR 0=无）；
//! - `chars`：解码后的字符数（含行尾符字符）。UTF-8 精确（扫描时数
//! 续字节），UTF-16 = 字节/2（代理对计 2 字符，仅 emoji 行有偏差），
//! 其它编码按字节近似（GBK 大行有偏差，计划风险 R4 已记录）。
//!
//! 每 128 行一个块头 `{first_byte, cum_chars}`（块首行的绝对字节
//! 偏移与字符前缀和），查询 = 二分块头 + 块内 ≤127 次加法。

pub const LINES_PER_BLOCK: usize = 128;

/// `scan_cap`（0x800000 上限）。同时保证块内增量
/// 不超过 u30。
pub const MAX_LINE_BYTES: u64 = 0x800_000;

const KIND_SHIFT: u32 = 30;
const LEN_MASK: u32 = (1 << KIND_SHIFT) - 1;

#[derive(Clone, Copy, Default)]
pub struct BlockHead {
    /// 块首行的文件字节偏移（内容区起点起的绝对偏移）。
    pub first_byte: u64,
    /// 块首行之前的字符前缀和。
    pub cum_chars: u64,
}

/// 一行的索引事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpan {
    /// 行首（内容区）字节偏移。
    pub start: u64,
    /// 含行尾符的字节长。
    pub bytes: u32,
    /// EOL kind（1=CRLF 2=LF 3=CR 0=无/续行切分）。
    pub kind: u8,
    /// 含行尾符的字符数。
    pub chars: u32,
}

/// 从文件头开始正向增长的主行表。
#[derive(Default)]
pub struct LineTable {
    packed: Vec<u32>,
    chars: Vec<u32>,
    heads: Vec<BlockHead>,
    total_bytes: u64,
    total_chars: u64,
}

impl LineTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.packed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packed.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn total_chars(&self) -> u64 {
        self.total_chars
    }

    /// 已索引区域末尾（下一行起点）的绝对偏移。
    pub fn frontier(&self) -> u64 {
        self.total_bytes
    }

    /// 弹出末尾的 0 字节行（原 EOF 以行尾符结束时补的末空行记录）。
    /// follow-tail 采纳增长前调用：旧 EOF 处已不再是无内容行。返回是
    /// 否弹出。
    pub fn pop_empty_tail(&mut self) -> bool {
        let Some(&p) = self.packed.last() else {
            return false;
        };
        if p & LEN_MASK != 0 {
            return false;
        }
        self.packed.pop();
        self.chars.pop();
        // 空行记录不改字节/字符总量（0 字长），但行数与块头要对齐：
        // 弹出后块变空则一并弹头。
        if self.packed.len() % LINES_PER_BLOCK == 0 && self.heads.len() > 1 {
            self.heads.pop();
        }
        true
    }

    pub fn push_line(&mut self, bytes_incl_eol: u64, kind: u8, chars_incl_eol: u64) {
        debug_assert!(bytes_incl_eol <= LEN_MASK as u64);
        debug_assert!(chars_incl_eol <= u32::MAX as u64);
        if self.packed.len() % LINES_PER_BLOCK == 0 {
            self.heads.push(BlockHead {
                first_byte: self.total_bytes,
                cum_chars: self.total_chars,
            });
        }
        self.packed
            .push(((bytes_incl_eol as u32) & LEN_MASK) | ((kind as u32 & 3) << KIND_SHIFT));
        self.chars.push(chars_incl_eol as u32);
        self.total_bytes += bytes_incl_eol;
        self.total_chars += chars_incl_eol;
    }

    /// 行 `i` 的区间事实；越界返回 `None`。
    pub fn line_span(&self, line: usize) -> Option<LineSpan> {
        if line >= self.packed.len() {
            return None;
        }
        let bi = line / LINES_PER_BLOCK;
        let head = self.heads[bi];
        let block_start = bi * LINES_PER_BLOCK;
        let mut start = head.first_byte;
        for i in block_start..line {
            start += (self.packed[i] & LEN_MASK) as u64;
        }
        let p = self.packed[line];
        Some(LineSpan {
            start,
            bytes: p & LEN_MASK,
            kind: (p >> KIND_SHIFT) as u8,
            chars: self.chars[line],
        })
    }

    pub fn line_to_byte(&self, line: usize) -> Option<u64> {
        self.line_span(line).map(|s| s.start)
    }

    pub fn line_to_char(&self, line: usize) -> Option<u64> {
        if line > self.packed.len() {
            return None;
        }
        if line == self.packed.len() {
            return Some(self.total_chars);
        }
        let bi = line / LINES_PER_BLOCK;
        let head = self.heads[bi];
        let block_start = bi * LINES_PER_BLOCK;
        let mut chars = head.cum_chars;
        for i in block_start..line.min(self.packed.len()) {
            chars += self.chars[i] as u64;
        }
        Some(chars)
    }

    /// 字节偏移所在的行（行尾符字节属于本行）。`off` 超出已索引
    /// 范围时返回最后一行。
    pub fn byte_to_line(&self, off: u64) -> usize {
        if self.packed.is_empty() {
            return 0;
        }
        // 二分找最后一个 first_byte <= off 的块。
        let mut lo = 0usize;
        let mut hi = self.heads.len();
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.heads[mid].first_byte <= off {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let block_start = lo * LINES_PER_BLOCK;
        let block_end = (block_start + LINES_PER_BLOCK).min(self.packed.len());
        let mut start = self.heads[lo].first_byte;
        for i in block_start..block_end {
            let len = (self.packed[i] & LEN_MASK) as u64;
            if off < start + len {
                return i;
            }
            start += len;
        }
        block_end - 1
    }

    /// 字符偏移所在的行。`off` 超界返回最后一行。
    pub fn char_to_line(&self, off: u64) -> usize {
        if self.packed.is_empty() {
            return 0;
        }
        let mut lo = 0usize;
        let mut hi = self.heads.len();
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.heads[mid].cum_chars <= off {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let block_start = lo * LINES_PER_BLOCK;
        let block_end = (block_start + LINES_PER_BLOCK).min(self.packed.len());
        let mut chars = self.heads[lo].cum_chars;
        for i in block_start..block_end {
            let len = self.chars[i] as u64;
            if off < chars + len {
                return i;
            }
            chars += len;
        }
        block_end - 1
    }
}

/// EOF 尾区行表（Ctrl+End 在主扫描完成前即可用的"Fast View 尾区"）。
/// 只存扁平数组：尾区行数 ≤ 数百万，二分足够。
#[derive(Default)]
pub struct TailTable {
    starts: Vec<u64>,
    packed: Vec<u32>,
    chars: Vec<u32>,
}

impl TailTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        // 尾区不连续（起点由构造方给出），总字节数没有意义；这里
        // 返回累计行长仅供统计。
        self.starts.last().map(|&s| s).unwrap_or(0)
    }

    pub fn total_chars(&self) -> u64 {
        self.chars.iter().map(|&c| c as u64).sum()
    }

    pub fn push_line(&mut self, start: u64, bytes_incl_eol: u64, kind: u8, chars_incl_eol: u64) {
        debug_assert!(bytes_incl_eol <= LEN_MASK as u64);
        self.starts.push(start);
        self.packed.push(
            ((bytes_incl_eol as u32) & LEN_MASK) | ((kind as u32 & 3) << KIND_SHIFT),
        );
        self.chars.push(chars_incl_eol as u32);
    }

    pub fn line_span(&self, idx: usize) -> Option<LineSpan> {
        if idx >= self.starts.len() {
            return None;
        }
        let p = self.packed[idx];
        Some(LineSpan {
            start: self.starts[idx],
            bytes: p & LEN_MASK,
            kind: (p >> KIND_SHIFT) as u8,
            chars: self.chars[idx],
        })
    }

    /// 尾区内第 `idx` 行之前的字符前缀和（含尾区首行之前已有
    /// `table` 的字符总量由调用方叠加）。
    pub fn chars_before(&self, idx: usize) -> u64 {
        self.chars.iter().take(idx).map(|&c| c as u64).sum()
    }

    pub fn byte_to_idx(&self, off: u64) -> usize {
        match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// 尾区内字符偏移 → 尾区行号。
    pub fn char_idx_of(&self, chars_in: u64) -> usize {
        let mut acc = 0u64;
        for (i, &c) in self.chars.iter().enumerate() {
            if chars_in < acc + c as u64 {
                return i;
            }
            acc += c as u64;
        }
        self.chars.len().saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_query_roundtrip() {
        let mut t = LineTable::new();
        // 三行：10B(LF) 5B(CRLF) 7B(无)
        t.push_line(10, 2, 8);
        t.push_line(5, 1, 3);
        t.push_line(7, 0, 7);
        assert_eq!(t.len(), 3);
        assert_eq!(t.total_bytes(), 22);
        assert_eq!(t.total_chars(), 18);
        assert_eq!(t.frontier(), 22);
        let s0 = t.line_span(0).unwrap();
        assert_eq!((s0.start, s0.bytes, s0.kind), (0, 10, 2));
        let s1 = t.line_span(1).unwrap();
        assert_eq!((s1.start, s1.bytes, s1.kind), (10, 5, 1));
        let s2 = t.line_span(2).unwrap();
        assert_eq!((s2.start, s2.bytes, s2.kind), (15, 7, 0));
        assert_eq!(t.line_span(3), None);
        // byte→line
        assert_eq!(t.byte_to_line(0), 0);
        assert_eq!(t.byte_to_line(9), 0); // LF 属于行0
        assert_eq!(t.byte_to_line(10), 1);
        assert_eq!(t.byte_to_line(14), 1); // CRLF 属于行1
        assert_eq!(t.byte_to_line(15), 2);
        assert_eq!(t.byte_to_line(100), 2); // 超界夹取
        // char→line
        assert_eq!(t.char_to_line(0), 0);
        assert_eq!(t.char_to_line(7), 0);
        assert_eq!(t.char_to_line(8), 1);
        assert_eq!(t.char_to_line(10), 1);
        assert_eq!(t.char_to_line(11), 2);
        // line→char
        assert_eq!(t.line_to_char(0), Some(0));
        assert_eq!(t.line_to_char(1), Some(8));
        assert_eq!(t.line_to_char(2), Some(11));
        assert_eq!(t.line_to_char(3), Some(18));
    }

    #[test]
    fn many_lines_block_boundary() {
        let mut t = LineTable::new();
        for i in 0..500 {
            t.push_line((i % 7 + 1) as u64, 2, (i % 7 + 1) as u64);
        }
        assert_eq!(t.len(), 500);
        // 抽查块边界两侧
        for probe in [0, 1, 127, 128, 129, 255, 256, 499] {
            let s = t.line_span(probe).unwrap();
            assert_eq!(t.byte_to_line(s.start), probe, "probe {probe}");
            assert_eq!(t.byte_to_line(s.start + s.bytes as u64 - 1), probe);
            let c = t.line_to_char(probe).unwrap();
            assert_eq!(t.char_to_line(c), probe, "probe {probe}");
            assert_eq!(t.char_to_line(c + s.chars as u64 - 1), probe);
        }
        let total: u64 = (0..500).map(|i| (i % 7 + 1) as u64).sum();
        assert_eq!(t.total_bytes(), total);
        assert_eq!(t.total_chars(), total);
        assert_eq!(t.byte_to_line(total), 499);
    }
}
