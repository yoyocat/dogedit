//! overlay：行段片链编辑。
//!
//! 逻辑文档 = 段序列依序拼接：`Base{from,count}` 引用底座（mmap，不可
//! 变）的显示行区间，`Edit{lines}` 是堆上的新行。一次编辑 = 把受影响
//! 的逻辑行区间替换成一个 Edit 段（保留被替换行的前缀/后缀），相邻
//! Base 段合并。这是 dg-hex 片链（HexSnapShot 的 piece `{off,len,page}`）
//! 在行粒度上的同构：底座永不拷贝，编辑内存有界（预算 [`EDIT_BUDGET_BYTES`]）。
//!
//! 坐标系：与 [`crate::LargeDoc`] 相同的"内容区字符（含行尾符）"。
//! overlay 干净（无段）时所有查询直通底座 raw 路径，零开销。
//!
//! undo/redo 不在本层：上层 `dg_core::UndoStack` 以字符区间 EditOp 驱动
//! `LargeDoc::edit`，逆操作就是再编辑 —— 与 Mem 后端同一语义。

use crate::base::FileBase;
use crate::lineindex::LineSpan;
use crate::scanner::EncKind;
use encoding_rs::Encoding;
use parking_lot::{Mutex, RwLock};
use std::io::Write;

/// 编辑段文本总量预算（ASCII 字节计）。超出即拒绝编辑（plan §4.6
/// "受 overlay 预算约束"；EmEditor Fast View 的内存 overlay 同款）。
pub const EDIT_BUDGET_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// 编辑预算耗尽（编辑段文本总量超过 [`EDIT_BUDGET_BYTES`]）。
    Budget,
    /// 字符区间越界。
    OutOfRange,
}

/// wire 行尾 → ASCII 字节宽（预算估算用；实际写出按编码）。
fn eol_wire_bytes(kind: u8) -> u64 {
    match kind {
        1 => 2,
        2 | 3 => 1,
        _ => 0,
    }
}

/// 一行编辑内容（text 不含行尾符；kind 为 wire 编码 0..3）。
#[derive(Debug, Clone)]
pub struct EditLine {
    pub text: String,
    pub kind: u8,
}

impl EditLine {
    fn chars_incl(&self) -> u64 {
        self.text.chars().count() as u64 + EncKind::eol_chars(self.kind)
    }
    fn est_bytes(&self) -> u64 {
        self.text.len() as u64 + eol_wire_bytes(self.kind)
    }
}

/// Base 段行号存 u32：底座行数上限 2^32（4G 行 × ~4B/行索引远超 v1
/// 目标机器；打开时的文件大小天然排除越界）。`chars` 是该底座行区间
/// 的字符总量（含行尾符），构造时从底座行表取差值缓存 —— 让前缀和
/// 重建成为纯段表算术。
#[derive(Debug, Clone)]
enum Seg {
    Base { from: u32, count: u32, chars: u64 },
    /// `replaced`：该编辑段改写掉的底座行区间（raw from, count） —
    /// 落在其中的 raw 命中已无对应逻辑文本（map_base_char 返回 None）。
    Edit { lines: Vec<EditLine>, chars: u64, replaced: (u32, u32) },
}

/// 底座只读视图（raw 域，不过 overlay）。独立结构而非 `&LargeDoc`：
/// `LargeDoc::edit` 借此同时持有 `&mut overlay` 与底座共享借用（字段
/// 级不相交借用）。
pub(crate) struct BaseView<'a> {
    pub base: &'a FileBase,
    pub state: &'a Mutex<crate::ScanState>,
    pub lines_cache: &'a RwLock<crate::LineCache>,
    pub enc: &'static Encoding,
    pub enc_kind: EncKind,
    pub bom_len: u64,
    pub content_len: u64,
}

impl BaseView<'_> {
    fn raw_line_span(&self, line: usize) -> Option<LineSpan> {
        let st = self.state.lock();
        if line < st.table.len() {
            return st.table.line_span(line);
        }
        let idx = line - st.table.len();
        st.tail.as_ref()?.table.line_span(idx)
    }

    pub(crate) fn raw_line_count(&self) -> u64 {
        let st = self.state.lock();
        (st.table.len() + st.tail.as_ref().map(|t| t.table.len()).unwrap_or(0)) as u64
    }

    /// 底座字符总量（含行尾符）。
    pub(crate) fn raw_total_chars(&self) -> u64 {
        let st = self.state.lock();
        st.table.total_chars() + st.tail.as_ref().map(|t| t.table.total_chars()).unwrap_or(0)
    }

    fn raw_line_text(&self, line: usize) -> Option<String> {
        {
            let c = self.lines_cache.read();
            if let Some(s) = c.get(line) {
                return Some(s.clone());
            }
        }
        let span = self.raw_line_span(line)?;
        let eol = self.enc_kind.eol_bytes(span.kind) as u32;
        let content = span.bytes.saturating_sub(eol);
        let mut buf = vec![0u8; content as usize];
        let n = self.base.read_span(self.bom_len + span.start, &mut buf);
        buf.truncate(n);
        let text = match self.enc_kind {
            EncKind::AsciiCompat { utf8: true } => String::from_utf8_lossy(&buf).into_owned(),
            _ => self.enc.decode(&buf).0.into_owned(),
        };
        self.lines_cache.write().put(line, text.clone());
        Some(text)
    }

    fn raw_line_to_char(&self, line: usize) -> Option<u64> {
        let st = self.state.lock();
        if line < st.table.len() {
            return st.table.line_to_char(line);
        }
        let tail = st.tail.as_ref()?;
        let idx = line - st.table.len();
        if idx > tail.table.len() {
            return None;
        }
        Some(st.table.total_chars() + tail.table.chars_before(idx))
    }

    fn raw_char_to_line(&self, ch: u64) -> usize {
        let st = self.state.lock();
        let head = st.table.char_to_line(ch);
        if ch < st.table.total_chars() || st.tail.is_none() {
            return head;
        }
        let tail = st.tail.as_ref().unwrap();
        let in_tail = ch - st.table.total_chars();
        st.table.len() + tail.table.char_idx_of(in_tail)
    }

    /// 底座行区间的字符总量（含行尾符）。
    fn base_seg_chars(&self, from: u32, count: u32) -> u64 {
        let a = self.raw_line_to_char(from as usize).unwrap_or(0);
        let b = self
            .raw_line_to_char((from + count) as usize)
            .unwrap_or_else(|| self.raw_total_chars());
        b.saturating_sub(a)
    }
}

#[derive(Default)]
pub struct Overlay {
    segs: Vec<Seg>,
    /// `prefix[i]` = 段 i 之前的（行数, 字符数）累计；与 `segs` 等长。
    prefix: Vec<(u64, u64)>,
    total_lines: u64,
    total_chars: u64,
    edit_bytes: u64,
    /// 已应用编辑次数（上层判定查找结果时效的依据）。
    edits: u64,
}

impl Overlay {
    pub fn is_clean(&self) -> bool {
        self.segs.is_empty()
    }

    pub fn edit_count(&self) -> u64 {
        self.edits
    }

    pub fn edit_bytes(&self) -> u64 {
        self.edit_bytes
    }

    // ---- 查询（逻辑域；干净时直通 raw） ----

    pub(crate) fn line_count(&self, bv: &BaseView) -> u64 {
        if self.segs.is_empty() {
            bv.raw_line_count()
        } else {
            self.total_lines
        }
    }

    pub(crate) fn len_chars(&self, bv: &BaseView) -> u64 {
        if self.segs.is_empty() {
            bv.raw_total_chars()
        } else {
            self.total_chars
        }
    }

    /// 段定位：（段下标, 段内行偏移）。`line` 超界夹到最后一段。
    /// 段 i 覆盖行 [prefix[i].0, 下一模块起点)：起点恰为 `line` 的段
    /// 就是覆盖段 —— partition_point 返回"满足 lines_before ≤ line 的
    /// 段数"，须取最后一个这样的段。
    fn seg_of_line(&self, line: u64) -> (usize, u64) {
        debug_assert!(!self.segs.is_empty());
        let mut i = self.prefix.partition_point(|&(l, _)| l <= line);
        if i >= self.segs.len() || self.prefix[i].0 > line {
            i = i.saturating_sub(1);
        }
        let local = line.saturating_sub(self.prefix[i].0);
        (i, local)
    }

    pub(crate) fn line_to_char(&self, bv: &BaseView, line: u64) -> Option<u64> {
        if self.segs.is_empty() {
            return bv.raw_line_to_char(line as usize);
        }
        if line >= self.total_lines {
            return Some(self.total_chars);
        }
        let (i, local) = self.seg_of_line(line);
        let (_, chars_before) = self.prefix[i];
        match &self.segs[i] {
            Seg::Base { from, .. } => {
                let base_char = bv.raw_line_to_char((*from + local as u32) as usize)?;
                let seg_start = bv.raw_line_to_char(*from as usize)?;
                Some(chars_before + base_char - seg_start)
            }
            Seg::Edit { lines, .. } => {
                let mut acc = chars_before;
                for l in lines.iter().take(local as usize) {
                    acc += l.chars_incl();
                }
                Some(acc)
            }
        }
    }

    pub(crate) fn char_to_line(&self, bv: &BaseView, ch: u64) -> u64 {
        if self.segs.is_empty() {
            return bv.raw_char_to_line(ch) as u64;
        }
        if ch >= self.total_chars {
            return self.total_lines.saturating_sub(1);
        }
        // 段起点字符恰为 ch 时（区间起点含端点）取该段，否则取最后
        // 一个 chars_before <= ch 的段。
        let mut i = self.prefix.partition_point(|&(_, c)| c <= ch);
        if i >= self.segs.len() || self.prefix[i].1 > ch {
            i = i.saturating_sub(1);
        }
        let (lines_before, chars_before) = self.prefix[i];
        match &self.segs[i] {
            Seg::Base { from, count, .. } => {
                let seg_start = bv.raw_line_to_char(*from as usize).unwrap_or(chars_before);
                let local_ch = ch - chars_before;
                let raw_line = bv.raw_char_to_line(seg_start + local_ch);
                let clamped = (raw_line as u64).min((*from + *count) as u64);
                lines_before + clamped - *from as u64
            }
            Seg::Edit { lines, .. } => {
                let mut acc = chars_before;
                for (n, l) in lines.iter().enumerate() {
                    if ch < acc + l.chars_incl() {
                        return lines_before + n as u64;
                    }
                    acc += l.chars_incl();
                }
                lines_before + lines.len() as u64 - 1
            }
        }
    }

    /// 行的 EOL kind（0..3）。
    pub(crate) fn line_kind(&self, bv: &BaseView, line: u64) -> u8 {
        if self.segs.is_empty() {
            return bv.raw_line_span(line as usize).map(|s| s.kind).unwrap_or(0);
        }
        if line >= self.total_lines {
            return 0;
        }
        let (i, local) = self.seg_of_line(line);
        match &self.segs[i] {
            Seg::Base { from, .. } => bv
                .raw_line_span((*from + local as u32) as usize)
                .map(|s| s.kind)
                .unwrap_or(0),
            Seg::Edit { lines, .. } => lines.get(local as usize).map(|l| l.kind).unwrap_or(0),
        }
    }

    /// 行文本（不含行尾符）。
    pub(crate) fn line_text(&self, bv: &BaseView, line: u64) -> Option<String> {
        if self.segs.is_empty() {
            return bv.raw_line_text(line as usize);
        }
        if line >= self.total_lines {
            return None;
        }
        let (i, local) = self.seg_of_line(line);
        match &self.segs[i] {
            Seg::Base { from, .. } => bv.raw_line_text((*from + local as u32) as usize),
            Seg::Edit { lines, .. } => lines.get(local as usize).map(|l| l.text.clone()),
        }
    }

    /// 行内容字符数（不含行尾符）。
    pub(crate) fn line_len(&self, bv: &BaseView, line: u64) -> usize {
        self.line_text(bv, line).map(|t| t.chars().count()).unwrap_or(0)
    }

    /// 字符偏移 → (行, 列)。列夹在行内容长度内（行尾符属于本行尾，
    /// 与 MemDoc::pos_of_char 同语义）。
    fn pos_of_char(&self, bv: &BaseView, ch: u64) -> (u64, usize) {
        let line = self.char_to_line(bv, ch);
        let base = self.line_to_char(bv, line).unwrap_or(ch);
        let len = self.line_len(bv, line) as u64;
        (line, ch.saturating_sub(base).min(len) as usize)
    }

    // ---- 编辑 ----

    /// 预检（上层守卫用，与 [`Overlay::edit`] 同判据）。
    pub(crate) fn edit_would_fail(
        &self,
        bv: &BaseView,
        start: u64,
        end: u64,
        text: &str,
    ) -> Option<EditError> {
        self.check(bv, start, end, text).err()
    }

    fn check(&self, bv: &BaseView, start: u64, end: u64, text: &str) -> Result<(), EditError> {
        if start == end && text.is_empty() {
            return Ok(());
        }
        let total = self.len_chars(bv);
        if start > end || end > total {
            return Err(EditError::OutOfRange);
        }
        Ok(())
    }

    /// 替换字符区间 `[start, end)` 为 `text`（可含换行）。首次编辑把
    /// 干净状态物化为单个 Base 段再拼接。
    pub(crate) fn edit(
        &mut self,
        bv: &BaseView,
        start: u64,
        end: u64,
        text: &str,
    ) -> Result<(), EditError> {
        self.check(bv, start, end, text)?;
        if start == end && text.is_empty() {
            return Ok(());
        }
        if self.segs.is_empty() {
            let n = bv.raw_line_count();
            self.segs.push(Seg::Base { from: 0, count: n as u32, chars: bv.raw_total_chars() });
            self.total_lines = n;
            self.total_chars = bv.raw_total_chars();
            self.prefix.push((0, 0));
        }
        let (la, cola) = self.pos_of_char(bv, start);
        let (lb, colb) = self.pos_of_char(bv, end);
        let (sa0, la_local) = self.seg_of_line(la);
        let (sb0, lb_local) = self.seg_of_line(lb);
        // 被改写的底座行区间（Edit 段的 raw 背书，map_base_char 用）。
        // Edit 段按行数比例瓜分其 replaced 区间 —— 跨段单调，恒有
        // base_to >= base_from（见 base_line_of）。
        let base_from = Self::base_line_of(&self.segs[sa0], la_local);
        let base_to = Self::base_line_of(&self.segs[sb0], lb_local + 1);
        let text_a = self.line_text(bv, la).unwrap_or_default();
        let text_b = self.line_text(bv, lb).unwrap_or_default();
        let kind_b = self.line_kind(bv, lb);

        // 合成：前缀 + 插入 + 后缀，按 EOL 切行。中间行行尾来自插入
        // 文本自身；末行行尾跟终止行自己的 —— 删除按字节域收走的是
        // 起始行的行尾，终止行的留在原地，拼接行带的就是它（与
        // LineVector::delete_text 同一口径，混合行尾下两边的字节长
        // 才对得上）；终止行无行尾（文档末行）时为 0。
        let mut merged = String::with_capacity(text_a.len() + text.len() + text_b.len());
        merged.push_str(&text_a[..char_byte_off(&text_a, cola)]);
        merged.push_str(text);
        merged.push_str(&text_b[char_byte_off(&text_b, colb)..]);
        let last_kind = kind_b;
        let new_lines = split_edit_lines(&merged, last_kind);

        // 精确预算：新增 - 释放。
        let dropped = self.dropped_edit_bytes(la, lb);
        let added: u64 = new_lines.iter().map(|l| l.est_bytes()).sum();
        if self.edit_bytes - dropped + added > EDIT_BUDGET_BYTES {
            return Err(EditError::Budget);
        }

        self.splice(
            bv,
            la,
            lb,
            new_lines,
            (base_from as u32, base_to.saturating_sub(base_from) as u32),
        );
        self.edits += 1;
        Ok(())
    }

    /// `[la..=lb]` 逻辑行区间内将被释放的编辑段字节（预算核算）。
    fn dropped_edit_bytes(&self, la: u64, lb: u64) -> u64 {
        if self.segs.is_empty() {
            return 0;
        }
        let (sa, local_a) = self.seg_of_line(la);
        let (sb, local_b) = self.seg_of_line(lb);
        let mut dropped = 0u64;
        for (i, seg) in self.segs[sa..=sb].iter().enumerate() {
            if let Seg::Edit { lines, .. } = seg {
                let (from, to) = if sa == sb {
                    (local_a as usize, (local_b + 1) as usize)
                } else if i == 0 {
                    (local_a as usize, lines.len())
                } else if sa + i == sb {
                    (0, (local_b + 1) as usize)
                } else {
                    (0, lines.len())
                };
                dropped += lines[from..to.max(from)].iter().map(|l| l.est_bytes()).sum::<u64>();
            }
        }
        dropped
    }

    /// 段内逻辑行 → 底座行的虚构映射。Base 段线性直映；Edit 段把
    /// 其 replaced 底座行区间按行数**比例**瓜分给各逻辑行 —
    /// `base_of(0)` == 段起点、`base_of(L)` == 段终点，因此无论段
    /// 内行数相对被替换区间如何增缩，任何 head/mid/tail 切分都
    /// 连续覆盖原区间、跨段单调，后续编辑恒有 `base_to >= base_from`
    /// （行数与区间不对齐时"每逻辑行一行底库"的旧虚构会把段表的
    /// replaced.0 排乱，最终在 base_to - base_from 处下溢）。
    fn base_line_of(seg: &Seg, local: u64) -> u64 {
        match seg {
            Seg::Base { from, .. } => *from as u64 + local,
            Seg::Edit { lines, replaced, .. } => {
                let (rf, rc) = (replaced.0 as u64, replaced.1 as u64);
                let l = lines.len().max(1) as u64;
                rf + local * rc.max(1) / l
            }
        }
    }

    /// 把逻辑行 `[la..=lb]` 替换为 `new_lines`（≥1 行），重建前缀和。
    fn splice(&mut self, bv: &BaseView, la: u64, lb: u64, new_lines: Vec<EditLine>, replaced: (u32, u32)) {
        let (sa, local_a) = self.seg_of_line(la);
        let (sb, local_b) = self.seg_of_line(lb);

        let head = split_head(bv, &self.segs[sa], local_a as usize);
        let tail = split_tail(bv, &self.segs[sb], local_b as usize);
        let mid_chars: u64 = new_lines.iter().map(|l| l.chars_incl()).sum();
        let mid = Seg::Edit { lines: new_lines, chars: mid_chars, replaced };

        let mut replacement: Vec<Seg> = Vec::with_capacity(3);
        if let Some(h) = head {
            replacement.push(h);
        }
        replacement.push(mid);
        if let Some(t) = tail {
            replacement.push(t);
        }
        self.segs.splice(sa..=sb, replacement);
        self.rebuild();
    }

    /// 前缀和与总量重算（段表纯算术；Base 段字符数在段内缓存）。
    fn rebuild(&mut self) {
        // 相邻 Base 合并：多次编辑后段表不碎片化。
        let mut coalesced: Vec<Seg> = Vec::with_capacity(self.segs.len());
        for seg in self.segs.drain(..) {
            match (coalesced.last_mut(), &seg) {
                (
                    Some(Seg::Base { from: f1, count: c1, chars: ch1 }),
                    Seg::Base { from: f2, count: c2, chars: ch2 },
                ) if *f1 as u64 + *c1 as u64 == *f2 as u64 => {
                    *c1 += *c2;
                    *ch1 += *ch2;
                }
                _ => coalesced.push(seg),
            }
        }
        self.segs = coalesced;
        self.prefix.clear();
        let mut lines = 0u64;
        let mut chars = 0u64;
        let mut edit_bytes = 0u64;
        for seg in &self.segs {
            self.prefix.push((lines, chars));
            match seg {
                Seg::Base { count, chars: c, .. } => {
                    lines += *count as u64;
                    chars += *c;
                }
                Seg::Edit { lines: ls, chars: c, .. } => {
                    lines += ls.len() as u64;
                    chars += *c;
                    edit_bytes += ls.iter().map(|l| l.est_bytes()).sum::<u64>();
                }
            }
        }
        self.total_lines = lines;
        self.total_chars = chars;
        self.edit_bytes = edit_bytes;
    }

    /// follow-tail 弹出旧 EOF 末空行时，末 Base 段同步收缩一行（该行
    /// 0 字符，段字符缓存无需调整）。仅当末段 Base 且正好结束于
    /// `old_raw_count`；末段是 Edit（用户已改写文档尾）则无需处理。
    pub(crate) fn pop_tail_base_line(&mut self, old_raw_count: u64) {
        if self.segs.is_empty() {
            return;
        }
        let shrink = match self.segs.last() {
            Some(Seg::Base { from, count, .. }) => {
                *from as u64 + *count as u64 == old_raw_count && *count > 0
            }
            _ => false,
        };
        if shrink {
            if let Some(Seg::Base { count, .. }) = self.segs.last_mut() {
                *count -= 1;
            }
            self.rebuild();
        }
    }

    /// 底座追加了行（follow-tail）：末段 Base 结束于旧底座末行则延长，
    /// 否则（末段是 Edit）追加新 Base 段。`added_chars` 为新行字符量。
    pub fn extend_base(&mut self, old_raw_count: u64, delta: u32, added_chars: u64) {
        if delta == 0 || self.segs.is_empty() {
            return; // 干净：line_count 直通 raw，自动包含新行
        }
        let last = self.segs.last_mut().unwrap();
        match last {
            Seg::Base { from, count, chars } => {
                if (*from as u64 + *count as u64) == old_raw_count {
                    *count += delta;
                    *chars += added_chars;
                } else {
                    self.segs.push(Seg::Base {
                        from: old_raw_count as u32,
                        count: delta,
                        chars: added_chars,
                    });
                }
            }
            Seg::Edit { .. } => {
                self.segs.push(Seg::Base {
                    from: old_raw_count as u32,
                    count: delta,
                    chars: added_chars,
                });
            }
        }
        self.rebuild();
    }

    /// 初扫期间已编辑（段表按当时的部分行表物化）而扫描此后完成：
    /// 底座行表长全，末段采纳增长。段表已知行数从末段自身推导 —
    /// 尾部 Base 的 `from+count`，或尾部 Edit 段替换到的底座行界
    /// （`replaced` 的 (from, count)）。干净段表无需处理（查询直通
    /// raw，自动包含新行）。
    pub(crate) fn adopt_base_growth(&mut self, bv: &BaseView) {
        if self.segs.is_empty() {
            return;
        }
        let known = match self.segs.last() {
            Some(Seg::Base { from, count, .. }) => *from as u64 + *count as u64,
            Some(Seg::Edit { replaced, .. }) => replaced.0 as u64 + replaced.1 as u64,
            None => return,
        };
        let raw = bv.raw_line_count();
        if raw <= known {
            return;
        }
        // 增量行的字符量 = raw 总量 − known 行起点的 raw 偏移。
        let known_chars = bv.raw_line_to_char(known as usize).unwrap_or(u64::MAX);
        let delta_chars = bv.raw_total_chars().saturating_sub(known_chars);
        self.extend_base(known, (raw - known).min(u32::MAX as u64) as u32, delta_chars);
    }

    /// raw（底座）字符偏移 → 逻辑字符偏移。底座行已被编辑删除时返回
    /// `None`（命中不存在于当前视图）。搜索（M4）在 raw 域扫文件，命中
    /// 经此映射到编辑后视图。
    pub(crate) fn map_base_char(&self, bv: &BaseView, raw_ch: u64) -> Option<u64> {
        if self.segs.is_empty() {
            return Some(raw_ch);
        }
        if raw_ch >= bv.raw_total_chars() {
            return None;
        }
        let raw_line = bv.raw_char_to_line(raw_ch) as u64;
        let raw_base = bv.raw_line_to_char(raw_line as usize)?;
        let col = (raw_ch - raw_base).min({
            let span = bv.raw_line_span(raw_line as usize)?;
            (span.chars as u64).saturating_sub(EncKind::eol_chars(span.kind))
        });
        let mut lines_before = 0u64;
        for seg in &self.segs {
            match seg {
                Seg::Base { from, count, .. } => {
                    let to = *from as u64 + *count as u64;
                    if raw_line >= *from as u64 && raw_line < to {
                        let logical_line = lines_before + raw_line - *from as u64;
                        let logical_base = self.line_to_char(bv, logical_line)?;
                        return Some(logical_base + col);
                    }
                    lines_before += *count as u64;
                }
                Seg::Edit { lines, replaced, .. } => {
                    let (rf, rc) = (replaced.0 as u64, replaced.1 as u64);
                    if raw_line >= rf && raw_line < rf + rc {
                        return None; // 该底座行已被改写，raw 命中无对应文本
                    }
                    lines_before += lines.len() as u64;
                }
            }
        }
        None // 底座行落在被删除/替换的区间
    }

    // ---- 流式合并写出（保存） ----

    /// 逻辑文档按原编码写出（含 BOM）：Base 段 = 底座字节区间直拷，
    /// Edit 段 = 文本按文件编码重编码。返回字节数。
    pub(crate) fn write_merged(&self, bv: &BaseView, w: &mut dyn Write) -> std::io::Result<u64> {
        let mut written = 0u64;
        if bv.bom_len > 0 {
            let mut bom = [0u8; 4];
            let n = bv.base.read_span(0, &mut bom[..bv.bom_len as usize]);
            w.write_all(&bom[..n])?;
            written += n as u64;
        }
        let raw_n = bv.raw_line_count();
        let mut buf = vec![0u8; crate::base::READ_CHUNK];
        let mut copy_base_range =
            |from_line: u64, count: u64, w: &mut dyn Write, written: &mut u64| {
                let start = bv.raw_line_span(from_line as usize)?.start;
                let last = bv.raw_line_span((from_line + count - 1) as usize)?;
                let end = last.start + last.bytes as u64;
                let mut pos = start;
                while pos < end {
                    let want = ((end - pos).min(buf.len() as u64)) as usize;
                    let n = bv.base.read_span(bv.bom_len + pos, &mut buf[..want]);
                    if n == 0 {
                        break;
                    }
                    w.write_all(&buf[..n]).ok()?;
                    *written += n as u64;
                    pos += n as u64;
                }
                Some(())
            };
        let _ = raw_n;
        if self.segs.is_empty() {
            // 干净：整文件内容直拷。
            let mut pos = 0u64;
            while pos < bv.content_len {
                let want = ((bv.content_len - pos).min(buf.len() as u64)) as usize;
                let n = bv.base.read_span(bv.bom_len + pos, &mut buf[..want]);
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n])?;
                written += n as u64;
                pos += n as u64;
            }
            return Ok(written);
        }
        for seg in &self.segs {
            match seg {
                Seg::Base { from, count, .. } => {
                    copy_base_range(*from as u64, *count as u64, w, &mut written)
                        .ok_or_else(|| std::io::Error::other("base range out of sync"))?;
                }
                Seg::Edit { lines, .. } => {
                    for l in lines {
                        write_encoded(bv, &l.text, l.kind, w)?;
                        written += estimate_written(bv, l);
                    }
                }
            }
        }
        Ok(written)
    }

    // ----.dgdiff 序列化（未保存编辑的边车） ----

    /// 序列化段表。干净时返回 `None`（无可持久化编辑）。
    pub fn serialize(&self) -> Option<Vec<u8>> {
        if self.segs.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(64 + self.edit_bytes as usize / 4);
        out.extend_from_slice(b"EED1");
        out.extend_from_slice(&(self.segs.len() as u32).to_le_bytes());
        for seg in &self.segs {
            match seg {
                Seg::Base { from, count, .. } => {
                    out.push(0);
                    out.extend_from_slice(&from.to_le_bytes());
                    out.extend_from_slice(&count.to_le_bytes());
                }
                Seg::Edit { lines, replaced, .. } => {
                    out.push(1);
                    out.extend_from_slice(&replaced.0.to_le_bytes());
                    out.extend_from_slice(&replaced.1.to_le_bytes());
                    out.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                    for l in lines {
                        out.push(l.kind);
                        out.extend_from_slice(&(l.text.len() as u32).to_le_bytes());
                        out.extend_from_slice(l.text.as_bytes());
                    }
                }
            }
        }
        Some(out)
    }

    /// 反序列化（[`Overlay::serialize`] 的逆）。段表越界由应用方对照
    /// 底座行数校验（`validate`）。
    pub fn deserialize(bytes: &[u8]) -> Option<Overlay> {
        if bytes.len() < 8 || &bytes[..4] != b"EED1" {
            return None;
        }
        let n = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let mut r = 8usize;
        let mut take = |len: usize| -> Option<&[u8]> {
            let end = r.checked_add(len)?;
            let s = bytes.get(r..end)?;
            r = end;
            Some(s)
        };
        let mut segs = Vec::with_capacity(n);
        for _ in 0..n {
            let tag = take(1)?[0];
            match tag {
                0 => {
                    let from = u32::from_le_bytes(take(4)?.try_into().ok()?);
                    let count = u32::from_le_bytes(take(4)?.try_into().ok()?);
                    segs.push(Seg::Base { from, count, chars: 0 });
                }
                1 => {
                    let r0 = u32::from_le_bytes(take(4)?.try_into().ok()?);
                    let r1 = u32::from_le_bytes(take(4)?.try_into().ok()?);
                    let ln = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
                    let mut lines = Vec::with_capacity(ln.min(1 << 20));
                    for _ in 0..ln {
                        let kind = take(1)?[0];
                        let tl = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
                        let tb = take(tl)?;
                        let text = String::from_utf8(tb.to_vec()).ok()?;
                        lines.push(EditLine { text, kind });
                    }
                    let chars = lines.iter().map(|l| l.chars_incl()).sum();
                    segs.push(Seg::Edit { lines, chars, replaced: (r0, r1) });
                }
                _ => return None,
            }
        }
        let ov = Overlay {
            segs,
            ..Default::default()
        };
        // Base 段字符数在应用时（validate_and_apply）由底座回填。
        Some(ov)
    }

    /// 应用反序列化的段表：校验 Base 段落在底座行范围内并回填字符数，
    /// 失败返回 `false`（边车作废）。
    pub(crate) fn validate_and_apply(&mut self, bv: &BaseView, loaded: Overlay) -> bool {
        let raw_n = bv.raw_line_count();
        for seg in &loaded.segs {
            if let Seg::Base { from, count, .. } = seg {
                if *from as u64 + *count as u64 > raw_n {
                    return false;
                }
            }
        }
        let mut segs = loaded.segs;
        for seg in &mut segs {
            if let Seg::Base { from, count, chars } = seg {
                *chars = bv.base_seg_chars(*from, *count);
            }
        }
        self.segs = segs;
        self.rebuild();
        // 应用边车本身不算一次编辑（edit_count 只跟踪用户编辑）。
        true
    }

    /// 丢弃全部编辑（保存合并完成后调用）。
    pub fn clear(&mut self) {
        self.segs.clear();
        self.prefix.clear();
        self.total_lines = 0;
        self.total_chars = 0;
        self.edit_bytes = 0;
    }
}

/// 文本 + 行尾按文件编码写出。
pub(crate) fn write_encoded_pub(
    bv: &BaseView,
    text: &str,
    kind: u8,
    w: &mut dyn Write,
) -> std::io::Result<()> {
    write_encoded(bv, text, kind, w)
}

/// 行尾在该编码下的实际写出字节数（统计口径）。
pub(crate) fn eol_written_bytes(kind_enc: EncKind, kind: u8) -> u64 {
    kind_enc.eol_bytes(kind)
}

fn write_encoded(bv: &BaseView, text: &str, kind: u8, w: &mut dyn Write) -> std::io::Result<()> {
    match bv.enc_kind {
        EncKind::AsciiCompat { utf8: true } => {
            w.write_all(text.as_bytes())?;
            w.write_all(eol_ascii(kind))?;
        }
        EncKind::Utf16Le | EncKind::Utf16Be => {
            let be = matches!(bv.enc_kind, EncKind::Utf16Be);
            let mut units: Vec<u8> = Vec::with_capacity(text.len() * 2 + 4);
            for u in text.encode_utf16() {
                let b = if be { u.to_be_bytes() } else { u.to_le_bytes() };
                units.extend_from_slice(&b);
            }
            for ch in eol_chars_of(kind) {
                let u = *ch as u16;
                let b = if be { u.to_be_bytes() } else { u.to_le_bytes() };
                units.extend_from_slice(&b);
            }
            w.write_all(&units)?;
        }
        EncKind::AsciiCompat { utf8: false } => {
            w.write_all(&bv.enc.encode(text).0)?;
            let eol: &str = match kind {
                1 => "\r\n",
                2 => "\n",
                3 => "\r",
                _ => "",
            };
            w.write_all(&bv.enc.encode(eol).0)?;
        }
    }
    Ok(())
}

/// 写出一行的近似字节数（统计口径，非精确编码后长度）。
fn estimate_written(bv: &BaseView, l: &EditLine) -> u64 {
    match bv.enc_kind {
        EncKind::AsciiCompat { .. } => l.est_bytes(),
        _ => (l.text.chars().count() as u64 + EncKind::eol_chars(l.kind)) * 2,
    }
}

fn eol_ascii(kind: u8) -> &'static [u8] {
    match kind {
        1 => b"\r\n",
        2 => b"\n",
        3 => b"\r",
        _ => b"",
    }
}

fn eol_chars_of(kind: u8) -> &'static [char] {
    match kind {
        1 => &['\r', '\n'],
        2 => &['\n'],
        3 => &['\r'],
        _ => &[],
    }
}

/// Edit 段行子区间的 replaced 切片：按比例瓜分（base_line_of），
/// 子区间连续覆盖、总和恰为原区间。
fn split_replaced(seg: &Seg, from: usize, to: usize) -> (u32, u32) {
    if let Seg::Edit { .. } = seg {
        let a = Overlay::base_line_of(seg, from as u64);
        let b = Overlay::base_line_of(seg, to as u64);
        (a as u32, b.saturating_sub(a) as u32)
    } else {
        (0, 0)
    }
}

fn char_byte_off(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// 把 `merged` 按 EOL 切行；末行行尾用 `last_kind`（区域终点的行尾）。
fn split_edit_lines(merged: &str, last_kind: u8) -> Vec<EditLine> {
    let mut out = Vec::new();
    let mut rest = merged;
    loop {
        match rest.find(['\n', '\r']) {
            None => {
                out.push(EditLine { text: rest.to_string(), kind: last_kind });
                return out;
            }
            Some(p) => {
                let (line, after) = rest.split_at(p);
                let (kind, next) = match after.as_bytes()[0] {
                    b'\r' if after.starts_with("\r\n") => (1u8, &after[2..]),
                    b'\r' => (3u8, &after[1..]),
                    _ => (2u8, &after[1..]),
                };
                out.push(EditLine { text: line.to_string(), kind });
                rest = next;
            }
        }
    }
}

fn split_head(bv: &BaseView, seg: &Seg, local: usize) -> Option<Seg> {
    match seg {
        Seg::Base { from, .. } => {
            if local == 0 {
                None
            } else {
                Some(Seg::Base {
                    from: *from,
                    count: local as u32,
                    chars: bv.base_seg_chars(*from, local as u32),
                })
            }
        }
        Seg::Edit { lines, .. } => {
            if local == 0 {
                return None;
            }
            let head = &lines[..local];
            Some(Seg::Edit {
                lines: head.to_vec(),
                chars: head.iter().map(|l| l.chars_incl()).sum(),
                replaced: split_replaced(seg, 0, local),
            })
        }
    }
}

fn split_tail(bv: &BaseView, seg: &Seg, local: usize) -> Option<Seg> {
    match seg {
        Seg::Base { from, count, .. } => {
            let remain = *count as usize - (local + 1);
            if remain == 0 {
                None
            } else {
                let nfrom = *from + local as u32 + 1;
                Some(Seg::Base {
                    from: nfrom,
                    count: remain as u32,
                    chars: bv.base_seg_chars(nfrom, remain as u32),
                })
            }
        }
        Seg::Edit { lines, .. } => {
            let tail = &lines[local + 1..];
            if tail.is_empty() {
                return None;
            }
            Some(Seg::Edit {
                lines: tail.to_vec(),
                chars: tail.iter().map(|l| l.chars_incl()).sum(),
                replaced: split_replaced(seg, local + 1, lines.len()),
            })
        }
    }
}
