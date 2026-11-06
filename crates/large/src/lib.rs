//! dg-large：大文件模式核心。
//!
//! [`LargeDoc`] = 不可变文件底座（mmap/pread）+ 两级行偏移索引 +
//! 后台行扫描 + 按需解码的行读取。文件永不整体载入（UTF-8 直通、
//! 非 UTF-8 逐行解码），内存 ≈ 行索引（4 B/行）+ 行缓存。
//!
//! 打开策略（EmEditor Fast View 形态）：
//! - ≤ [`SYNC_SCAN_LIMIT`]：打开时同步建全索引；
//! - 更大：同步建首屏索引（[`FRONT_BYTES`]）与 EOF 尾区索引
//! （[`TAIL_BYTES`]，Ctrl+End 立即可达），后台线程补扫中间，
//! 完成后尾区并入主索引、行号转为精确值。
//!
//! v1 只读（编辑在 M6 的 overlay）；所有方法线程安全（UI 线程读 +
//! 扫描线程写，经一把 state 锁分段提交）。

pub mod base;
pub mod encoding;
pub mod lineindex;
pub mod overlay;
pub mod scanner;
pub mod search;

pub use overlay::EditError;
pub use search::{SearchEvent, SearchHandle, SearchOptions};

use base::{FileBase, READ_CHUNK};
use crossbeam_channel::{Receiver, Sender, unbounded};
use encoding_rs::Encoding;
use lineindex::{LineSpan, LineTable, TailTable};
use parking_lot::{Mutex, RwLock};
use scanner::{EncKind, LineFeed};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 超过此大小（内容区字节）打开时同步全量建索引；再大走
/// "首屏 + 尾区 + 后台补扫"。
pub const SYNC_SCAN_LIMIT: u64 = 8 << 20;
/// 大文件首屏同步索引的扫描量。
pub const FRONT_BYTES: u64 = 1 << 20;
/// 尾区同步索引的扫描量（Ctrl+End 立即可用）。
pub const TAIL_BYTES: u64 = 2 << 20;
/// 行文本 LRU 条目上限（可视区重绘的重复读取走缓存）。
const LINE_CACHE_CAP: usize = 512;

/// 扫描线程向 UI 报告的事件（应用层定时轮询 [`LargeDoc::poll_scan_event`]）。
pub enum ScanEvent {
    Progress { bytes_done: u64, total: u64, lines: u64 },
    /// 主索引完成：行号自此精确；尾区已并入。
    Done { lines: u64 },
    /// 扫描途中文件被外部重写（轮转/截断）。数据源已失效，
    /// 应用应提示重载（计划风险 R1）。
    Rotated,
}

pub(crate) struct ScanState {
    /// 主行表：内容区相对偏移（内容区 = BOM 之后）。
    table: LineTable,
    /// EOF 尾区（主索引到达尾区起点时并入丢弃）。
    tail: Option<TailZone>,
    done: bool,
}

pub(crate) struct TailZone {
    table: TailTable,
    /// 尾区第一个完整行的内容区偏移（主表 frontier 到达即并入）。
    start: u64,
}

/// 解码行文本 LRU（行不可变，仅轮转时整体失效）。
#[derive(Default)]
pub(crate) struct LineCache {
    map: HashMap<usize, String>,
    order: VecDeque<usize>,
}

impl LineCache {
    fn get(&self, line: usize) -> Option<&String> {
        self.map.get(&line)
    }
    fn put(&mut self, line: usize, text: String) {
        if self.map.insert(line, text).is_none() {
            self.order.push_back(line);
            if self.order.len() > LINE_CACHE_CAP {
                let evict = self.order.pop_front().unwrap();
                self.map.remove(&evict);
            }
        }
    }
    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

pub struct LargeDoc {
    base: Arc<FileBase>,
    enc: &'static Encoding,
    bom_len: u64,
    enc_kind: EncKind,
    is_utf8: bool,
    content_len: u64,
    state: Arc<Mutex<ScanState>>,
    lines: RwLock<LineCache>,
    stop: Arc<AtomicBool>,
    scan_tx: Sender<ScanEvent>,
    scan_rx: Receiver<ScanEvent>,
    handle: Option<JoinHandle<()>>,
    /// M6 编辑 overlay：行段片链（干净 = 查询直通底座）。
    overlay: overlay::Overlay,
    /// follow-tail 续扫的基线（发起续扫时的底座行数/字符数；Done 时
    /// 据此把增量行并入 overlay）。`None` = 无进行中的尾随续扫。
    tail_baseline: Option<(u64, u64)>,
}

impl LargeDoc {
    /// 打开并开始索引。大文件立即返回（首屏 + 尾区已建好，中间
    /// 后台补扫）；小文件同步完成。
    pub fn open(path: &Path) -> Result<Self, String> {
        let base = Arc::new(FileBase::open(path)?);
        let file_len = base.len();
        let mut prefix = vec![0u8; (file_len as usize).min(encoding::DETECT_PREFIX)];
        let n = base.read_span(0, &mut prefix);
        prefix.truncate(n);
        let info = encoding::detect_from_prefix(&prefix);
        let enc_kind = match info.enc.name() {
            "UTF-16LE" => EncKind::Utf16Le,
            "UTF-16BE" => EncKind::Utf16Be,
            "UTF-8" => EncKind::AsciiCompat { utf8: true },
            _ => EncKind::AsciiCompat { utf8: false },
        };
        let is_utf8 = matches!(enc_kind, EncKind::AsciiCompat { utf8: true });
        let content_len = file_len.saturating_sub(info.bom_len);

        // 单次 unbounded()：两次调用会得到两条互不相通的通道
        // （M2 起的潜伏 bug —— Done 事件从未到达过接收端，UI 靠
        // is_scanning 停表掩盖了症状）。
        let (scan_tx, scan_rx) = unbounded();
        let mut doc = Self {
            base,
            enc: info.enc,
            bom_len: info.bom_len,
            enc_kind,
            is_utf8,
            content_len,
            state: Arc::new(Mutex::new(ScanState { table: LineTable::new(), tail: None, done: false })),
            lines: RwLock::new(LineCache::default()),
            stop: Arc::new(AtomicBool::new(false)),
            scan_tx,
            scan_rx,
            handle: None,
            overlay: overlay::Overlay::default(),
            tail_baseline: None,
        };

        if file_len == 0 {
            // 空文件：与 Document::from_text("") 一致 —— 一行空行。
            {
                let mut st = doc.state.lock();
                st.table.push_line(0, 0, 0);
                st.done = true;
            }
            return Ok(doc);
        }

        if content_len <= SYNC_SCAN_LIMIT {
            let mut feed = LineFeed::new(enc_kind, 0);
            doc.scan_range(&mut feed, 0, content_len, &mut |_st, _start, bytes, kind, chars| {
                _st.table.push_line(bytes, kind, chars);
            });
            let mut st = doc.state.lock();
            feed.finish(&mut |_a, b, k, c| st.table.push_line(b, k, c));
            if st.table.is_empty() {
                // 内容区为空（仅 BOM）：一行空行，与 Document 一致。
                st.table.push_line(0, 0, 0);
            }
            st.done = true;
            let lines = st.table.len() as u64;
            drop(st);
            let _ = doc.scan_tx.send(ScanEvent::Done { lines });
            return Ok(doc);
        }

        // 大文件：首屏同步索引（只收整行，未闭合段留给后台续扫）。
        {
            let mut feed = LineFeed::new(enc_kind, 0);
            doc.scan_range(&mut feed, 0, FRONT_BYTES, &mut |_st, _start, bytes, kind, chars| {
                _st.table.push_line(bytes, kind, chars);
            });
        } // feed drop：未闭合行不计入 —— 主表 frontier 即续扫起点

        // 尾区：扫出完整行，丢弃第一行（跨区域起点的半行）。
        {
            let tail_start = content_len.saturating_sub(TAIL_BYTES);
            let mut feed = LineFeed::new(enc_kind, tail_start);
            let mut spans: Vec<(u64, u64, u8, u64)> = Vec::new();
            doc.scan_range(&mut feed, tail_start, content_len, &mut |_st, a, b, k, c| {
                spans.push((a, b, k, c));
            });
            feed.finish(&mut |a, b, k, c| spans.push((a, b, k, c)));
            let mut tail = TailTable::new();
            let mut first_full: Option<u64> = None;
            for (i, &(start, bytes, kind, chars)) in spans.iter().enumerate() {
                if i == 0 {
                    // 半行（或恰好在区域起点整行起始 —— 同样丢弃保守处理：
                    // 它会由主扫描在到达时补上）。记录其终点为尾区并入点。
                    first_full = Some(start + bytes);
                    continue;
                }
                tail.push_line(start, bytes, kind, chars);
            }
            let mut st = doc.state.lock();
            st.tail = Some(TailZone {
                table: tail,
                start: first_full.unwrap_or(content_len),
            });
        }

        // 后台补扫：从主表 frontier 到 EOF。
        let doc_ref = DocRef {
            base: doc.base.clone(),
            enc_kind,
            bom_len: doc.bom_len,
            content_len,
            state: doc.state.clone(),
            stop: doc.stop.clone(),
            tx: doc.scan_tx.clone(),
        };
        doc.handle = Some(std::thread::Builder::new()
            .name("dg-large-scan".into())
            .spawn(move || doc_ref.run())
            .map_err(|e| e.to_string())?);
        Ok(doc)
    }

    /// 顺序喂入 `[from, to)`（内容区偏移），产出行经 `sink` 入表
    /// （每块一次锁）。
    fn scan_range(
        &self,
        feed: &mut LineFeed,
        from: u64,
        to: u64,
        sink: &mut dyn FnMut(&mut ScanState, u64, u64, u8, u64),
    ) {
        let mut pos = from;
        let unit = if matches!(self.enc_kind, EncKind::AsciiCompat { .. }) { 1 } else { 2 };
        let mut buf = vec![0u8; READ_CHUNK];
        while pos < to {
            let want = ((to - pos).min(READ_CHUNK as u64) as usize / unit) * unit;
            if want == 0 {
                break;
            }
            let n = self.base.read_span(self.bom_len + pos, &mut buf[..want]);
            if n == 0 {
                break;
            }
            let mut st = self.state.lock();
            feed.feed(pos, &buf[..n], &mut |a, b, k, c| sink(&mut st, a, b, k, c));
            drop(st);
            pos += n as u64;
        }
    }

    // ---- 查询（UI 线程） ----

    pub fn path(&self) -> &Path {
        self.base.path()
    }

    pub fn encoding(&self) -> &'static Encoding {
        self.enc
    }

    pub fn has_bom(&self) -> bool {
        self.bom_len > 0
    }

    pub fn total_bytes(&self) -> u64 {
        self.base.len()
    }

    pub fn is_utf8(&self) -> bool {
        self.is_utf8
    }

    /// 底座只读视图（raw 域）。字段级拆分借用：`self.overlay` 可同时
    /// 可变借用。
    fn base_view(&self) -> overlay::BaseView<'_> {
        overlay::BaseView {
            base: &self.base,
            state: &self.state,
            lines_cache: &self.lines,
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        }
    }

    pub fn is_scanning(&self) -> bool {
        !self.state.lock().done
    }

    /// (已扫字节, 总内容字节)。
    pub fn scan_progress(&self) -> (u64, u64) {
        let st = self.state.lock();
        (st.table.total_bytes(), self.content_len)
    }

    /// 轮询扫描事件（非阻塞）。
    pub fn poll_scan_event(&self) -> Option<ScanEvent> {
        self.scan_rx.try_recv().ok()
    }

    /// 等待索引完成（测试/一次性跳转用）。
    pub fn wait_for_scan(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_scanning() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        !self.is_scanning()
    }

    /// 显示行数：主表 + 尾区（未扫完时中间行未计入，行号在完成
    /// 后转为精确值 —— v1 不做估算行，避免假行号）。overlay 编辑后
    /// 为逻辑行数。
    pub fn line_count(&self) -> usize {
        let bv = self.base_view();
        self.overlay.line_count(&bv) as usize
    }

    pub fn len_chars(&self) -> usize {
        let bv = self.base_view();
        self.overlay.len_chars(&bv) as usize
    }

    /// 行的区域事实（display 行号 → 内容区偏移等）。raw 域：编辑行
    /// 没有文件偏移。
    fn line_span(&self, line: usize) -> Option<LineSpan> {
        let st = self.state.lock();
        if line < st.table.len() {
            return st.table.line_span(line);
        }
        let idx = line - st.table.len();
        st.tail.as_ref()?.table.line_span(idx)
    }

    pub fn eol_kind(&self, line: usize) -> u8 {
        let bv = self.base_view();
        self.overlay.line_kind(&bv, line as u64)
    }

    pub fn line_has_eol(&self, line: usize) -> bool {
        self.eol_kind(line) != 0
    }

    /// 行首的绝对文件偏移（raw 域：底座行）。
    pub fn line_to_byte_abs(&self, line: usize) -> Option<u64> {
        self.line_span(line).map(|s| self.bom_len + s.start)
    }

    pub fn line_to_char(&self, line: usize) -> Option<usize> {
        let bv = self.base_view();
        self.overlay.line_to_char(&bv, line as u64).map(|c| c as usize)
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        let bv = self.base_view();
        self.overlay.char_to_line(&bv, char_idx as u64) as usize
    }

    /// 行文本（不含行尾符）。行未索引/越界返回 `false` 并清空 `out`。
    pub fn line_text(&self, line: usize, out: &mut String) -> bool {
        out.clear();
        let Some(text) = self.line_text_owned(line) else {
            return false;
        };
        out.push_str(&text);
        true
    }

    pub fn line_text_owned(&self, line: usize) -> Option<String> {
        let bv = self.base_view();
        self.overlay.line_text(&bv, line as u64)
    }

    /// 行的字符数（不含行尾符）。UTF-8 精确；其它编码按解码计。
    pub fn line_len(&self, line: usize) -> usize {
        let bv = self.base_view();
        self.overlay.line_len(&bv, line as u64)
    }

    /// 平坦字符区间的文本（含区间内的行尾符）。只解码涉及的行。
    pub fn text_range(&self, char_start: usize, char_end: usize) -> String {
        if char_start >= char_end {
            return String::new();
        }
        let mut out = String::new();
        let first = self.char_to_line(char_start);
        let last = self.char_to_line(char_end - 1);
        let mut line = first;
        while line <= last {
            let kind = self.eol_kind(line);
            let base_c = self.line_to_char(line).unwrap_or(0);
            let Some(text) = self.line_text_owned(line) else {
                break;
            };
            let total_chars = text.chars().count() + Self::eol_char_count(kind);
            let lo = char_start.saturating_sub(base_c).min(total_chars);
            let hi = (char_end - base_c).min(total_chars);
            // 切字符区间（lo..hi 覆盖行内容与行尾符）。
            let eol_at = text.chars().count();
            let mut idx = 0usize;
            for ch in text.chars() {
                if idx >= lo && idx < hi {
                    out.push(ch);
                }
                idx += 1;
            }
            if hi > eol_at {
                let sep = Self::eol_chars(kind);
                let skip = lo.saturating_sub(eol_at);
                let take = (hi - eol_at).saturating_sub(skip);
                out.extend(sep[skip..skip + take].iter());
            }
            line += 1;
        }
        out
    }

    fn eol_char_count(kind: u8) -> usize {
        match kind {
            1 => 2,
            2 | 3 => 1,
            _ => 0,
        }
    }

    fn eol_chars(kind: u8) -> &'static [char] {
        match kind {
            1 => &['\r', '\n'],
            2 => &['\n'],
            3 => &['\r'],
            _ => &[],
        }
    }

    /// 文件是否被外部重写；是则行缓存失效并返回 `true`。
    pub fn check_rotated(&self) -> bool {
        if self.base.file_changed() {
            self.lines.write().clear();
            true
        } else {
            false
        }
    }

    /// 流式导出显示行区间 `[from, to)`（M5"选区导出"，M6 起编辑态可
    /// 用）：干净时按原编码字节直拷（含 BOM，不经解码/重编码，1GB 选
    /// 区也是磁盘拷贝速度）；有编辑时逐行按逻辑视图编码写出。返回写
    /// 出字节数。
    pub fn export_lines(
        &self,
        from: usize,
        to: usize,
        w: &mut dyn std::io::Write,
    ) -> std::io::Result<u64> {
        let last = self.line_count();
        let from = from.min(last);
        let to = to.min(last);
        let mut written = 0u64;
        if self.bom_len > 0 {
            let mut bom = [0u8; 4];
            let n = self.base.read_span(0, &mut bom[..self.bom_len as usize]);
            w.write_all(&bom[..n])?;
            written += n as u64;
        }
        if from >= to {
            return Ok(written);
        }
        if self.overlay.is_clean() {
            let start = self.line_span(from).map(|s| s.start).unwrap_or(0);
            let end = self
                .line_span(to - 1)
                .map(|s| s.start + s.bytes as u64)
                .unwrap_or(start);
            let mut pos = start;
            let mut buf = vec![0u8; READ_CHUNK];
            while pos < end {
                let want = ((end - pos).min(READ_CHUNK as u64)) as usize;
                let n = self.base.read_span(self.bom_len + pos, &mut buf[..want]);
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n])?;
                written += n as u64;
                pos += n as u64;
            }
            return Ok(written);
        }
        // 编辑态：逻辑视图逐行编码。
        let bv = self.base_view();
        for line in from..to {
            let Some(text) = self.overlay.line_text(&bv, line as u64) else {
                continue;
            };
            let kind = self.overlay.line_kind(&bv, line as u64);
            overlay::write_encoded_pub(&bv, &text, kind, w)?;
            written += text.len() as u64 + overlay::eol_written_bytes(self.enc_kind, kind);
        }
        Ok(written)
    }

    /// 流式替换导出（M5"Replace→导出"）：needle→replacement 逐行应用，
    /// 结果写到 `w`。约束与流式搜索同源：仅 UTF-8 系文件；忽略大小写
    /// 仅支持 ASCII needle。返回 (写出字节数, 替换次数)。
    pub fn export_replaced(
        &self,
        needle: &str,
        replacement: &str,
        case_sensitive: bool,
        w: &mut dyn std::io::Write,
    ) -> Result<(u64, usize), String> {
        if needle.is_empty() {
            return Err("无查找内容".to_string());
        }
        if !self.is_utf8 {
            return Err("替换结果导出仅支持 UTF-8 文件".to_string());
        }
        if !case_sensitive && !needle.is_ascii() {
            return Err("忽略大小写仅支持 ASCII 查询".to_string());
        }
        let mut written = 0u64;
        let mut count = 0usize;
        if self.bom_len > 0 {
            let mut bom = [0u8; 4];
            let n = self.base.read_span(0, &mut bom[..self.bom_len as usize]);
            w.write_all(&bom[..n]).map_err(|e| e.to_string())?;
            written += n as u64;
        }
        for line in 0..self.line_count() {
            let Some(text) = self.line_text_owned(line) else {
                continue;
            };
            let out = if case_sensitive {
                if text.contains(needle) {
                    count += text.matches(needle).count();
                    text.replace(needle, replacement)
                } else {
                    text
                }
            } else {
                // ASCII 忽略大小写：滑到下一个首字符候选点再验证。命中
                // 起点必是独立 ASCII 字节（续字节 ≥0x80 不等于任何
                // ASCII needle 字节），切片天然落在字符边界上。
                let hay = text.as_bytes();
                let nd = needle.as_bytes();
                let mut starts: Vec<usize> = Vec::new();
                let mut at = 0usize;
                while at + nd.len() <= hay.len() {
                    let Some(rel) = hay[at..]
                        .iter()
                        .position(|&b| b.eq_ignore_ascii_case(&nd[0]))
                    else {
                        break;
                    };
                    let p = at + rel;
                    if hay[p..].len() >= nd.len() && hay[p..p + nd.len()].eq_ignore_ascii_case(nd)
                    {
                        starts.push(p);
                        at = p + nd.len();
                    } else {
                        // 验证失败只前进一位，避免 O(n²)。
                        at = p + 1;
                    }
                }
                if starts.is_empty() {
                    text
                } else {
                    count += starts.len();
                    let mut s = String::with_capacity(text.len());
                    let mut prev = 0usize;
                    for &p in &starts {
                        s.push_str(&text[prev..p]);
                        s.push_str(replacement);
                        prev = p + nd.len();
                    }
                    s.push_str(&text[prev..]);
                    s
                }
            };
            w.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
            written += out.len() as u64;
            let kind = self.eol_kind(line);
            let eol: &[u8] = match kind {
                1 => b"\r\n",
                2 => b"\n",
                3 => b"\r",
                _ => &[],
            };
            if !eol.is_empty() {
                w.write_all(eol).map_err(|e| e.to_string())?;
                written += eol.len() as u64;
            }
        }
        Ok((written, count))
    }

    /// 全文物化（M5"转普通模式"）：整文解码进一个 String。只在调用方
    /// 的内存守卫通过后使用（预估 text+行存储 ≈ 5× 文件大小）。
    pub fn to_text_full(&self) -> String {
        let mut out = String::with_capacity(self.content_len as usize + 16);
        for line in 0..self.line_count() {
            if let Some(text) = self.line_text_owned(line) {
                out.push_str(&text);
            }
            let kind = self.eol_kind(line);
            let eol = Self::eol_chars(kind);
            if !eol.is_empty() {
                out.extend(eol.iter());
            }
        }
        out
    }

    /// 流式搜索（M4）：后台线程按块扫描，命中以全局字符偏移上报。
    /// 返回 `None` = 查询被同步拒绝：空 needle、正则但非 UTF-8、
    /// 忽略大小写但非 UTF-8（单字节折叠会破坏 GBK 尾字节）。
    pub fn start_search(
        &self,
        needle: &str,
        opts: search::SearchOptions,
    ) -> Option<search::SearchHandle> {
        if needle.is_empty() {
            return None;
        }
        if opts.regex && !self.is_utf8 {
            return None;
        }
        if !opts.case_sensitive && !self.is_utf8 {
            return None;
        }
        let ctx = search::SearchCtx {
            base: self.base.clone(),
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        };
        search::spawn(ctx, needle, opts)
    }

    // ---- M6：overlay 编辑 ----

    /// 是否有未合并进底座的编辑。
    pub fn is_dirty(&self) -> bool {
        !self.overlay.is_clean()
    }

    /// 已应用编辑次数（上层判定查找结果时效）。
    pub fn edit_count(&self) -> u64 {
        self.overlay.edit_count()
    }

    /// 编辑预检（守卫用）：`Some(err)` = 该编辑会被拒绝（预算/越界）。
    pub fn edit_would_fail(
        &self,
        start: usize,
        end: usize,
        text: &str,
    ) -> Option<overlay::EditError> {
        let bv = self.base_view();
        self.overlay.edit_would_fail(&bv, start as u64, end as u64, text)
    }

    /// 替换字符区间 `[start, end)` 为 `text`（可含换行） —— 与
    /// `Document::edit`（Mem 后端）同语义。undo/redo 由上层 UndoStack
    /// 以逆编辑驱动，本层不存历史。
    pub fn edit(&mut self, start: usize, end: usize, text: &str) -> Result<(), overlay::EditError> {
        // 内联构造 BaseView（字段级拆分借用：overlay 可变、底座共享）。
        let bv = overlay::BaseView {
            base: &self.base,
            state: &self.state,
            lines_cache: &self.lines,
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        };
        self.overlay.edit(&bv, start as u64, end as u64, text)
    }

    /// 丢弃全部编辑（保存合并完成后调用）。
    pub fn clear_edits(&mut self) {
        self.overlay.clear();
    }

    /// raw（底座）字符偏移 → 逻辑字符偏移。底座行已被编辑删除时返回
    /// `None`。M4 流式搜索在 raw 域扫文件，命中经此映射到编辑后视图。
    pub fn map_base_char(&self, raw: u64) -> Option<u64> {
        let bv = self.base_view();
        self.overlay.map_base_char(&bv, raw)
    }

    /// 逻辑文档按原编码流式写出（含 BOM）：Base 段字节直拷，Edit 段
    /// 重编码 —— 保存 = 磁盘拷贝速度 + 编辑段编码开销（plan §4.6）。
    pub fn write_merged(&self, w: &mut dyn std::io::Write) -> std::io::Result<u64> {
        let bv = self.base_view();
        self.overlay.write_merged(&bv, w)
    }

    ///.dgdiff 序列化（未保存编辑的边车）。干净返回 `None`。
    pub fn overlay_serialize(&self) -> Option<Vec<u8>> {
        self.overlay.serialize()
    }

    ///.dgdiff 应用（校验失败返回 `false`，边车作废）。
    pub fn overlay_deserialize_apply(&mut self, bytes: &[u8]) -> bool {
        let Some(loaded) = overlay::Overlay::deserialize(bytes) else {
            return false;
        };
        let bv = overlay::BaseView {
            base: &self.base,
            state: &self.state,
            lines_cache: &self.lines,
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        };
        self.overlay.validate_and_apply(&bv, loaded)
    }

    // ---- M6：follow-tail 尾随 ----

    /// 尾随轮询（应用层定时驱动）。文件追加 → 采纳增长并后台续扫
    /// （增量行并入行表与 overlay，完成后照常发 `Done`）；缩短/重写
    /// → `Rotated`（行索引失效，应用提示重开）。
    pub fn follow_tail_tick(&mut self) -> Option<TailEvent> {
        if self.is_scanning() {
            return None;
        }
        let cur = self.base.current_len();
        let old = self.base.len();
        if cur == old {
            return None;
        }
        if cur < old {
            return Some(TailEvent::Rotated);
        }
        self.base.adopt_growth(cur);
        self.content_len = cur - self.bom_len;
        // 续扫基线：Done 时把增量行并入 overlay（dirty 时末段延长）。
        // 先弹出旧 EOF 的末空行记录 —— 该行是"文件以行尾符结束"的补
        // 记，增长后旧 EOF 处已不是行边界，续扫会把真正的行补上。
        // dirty overlay 的末 Base 段若引用了该行，同步收缩（空行 0 字
        // 符，段字符缓存不变）。
        {
            let mut st = self.state.lock();
            let old_raw =
                (st.table.len() + st.tail.as_ref().map(|t| t.table.len()).unwrap_or(0)) as u64;
            let popped = st.table.pop_empty_tail();
            if popped {
                self.overlay.pop_tail_base_line(old_raw);
            }
            let lines = (st.table.len() + st.tail.as_ref().map(|t| t.table.len()).unwrap_or(0)) as u64;
            let chars = st.table.total_chars()
                + st.tail.as_ref().map(|t| t.table.total_chars()).unwrap_or(0);
            self.tail_baseline = Some((lines, chars));
            st.done = false;
        }
        let doc_ref = DocRef {
            base: self.base.clone(),
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
            state: self.state.clone(),
            stop: self.stop.clone(),
            tx: self.scan_tx.clone(),
        };
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.handle = Some(
            std::thread::Builder::new()
                .name("dg-large-tail".into())
                .spawn(move || doc_ref.run())
                .ok()?,
        );
        Some(TailEvent::Appended { bytes: cur - old })
    }

    /// 尾随续扫完成（Done 后由应用层调用）：增量行并入 overlay。
    pub fn on_tail_done(&mut self) {
        let Some((old_lines, old_chars)) = self.tail_baseline.take() else {
            return;
        };
        let bv = overlay::BaseView {
            base: &self.base,
            state: &self.state,
            lines_cache: &self.lines,
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        };
        let new_lines = bv.raw_line_count();
        let new_chars = bv.raw_total_chars();
        if new_lines > old_lines {
            let delta = (new_lines - old_lines).min(u32::MAX as u64) as u32;
            self.overlay
                .extend_base(old_lines, delta, new_chars.saturating_sub(old_chars));
        }
    }

    /// 初扫期间已编辑而扫描此后完成（Done）：底座行表长全，overlay
    /// 末段采纳增长（与尾随的 on_tail_done 互补：那条路径记录
    /// tail_baseline，这条从段表自身推导已知行数）。
    pub fn adopt_base_growth(&mut self) {
        let bv = overlay::BaseView {
            base: &self.base,
            state: &self.state,
            lines_cache: &self.lines,
            enc: self.enc,
            enc_kind: self.enc_kind,
            bom_len: self.bom_len,
            content_len: self.content_len,
        };
        self.overlay.adopt_base_growth(&bv);
    }
}

/// 尾随轮询结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailEvent {
    /// 文件追加了 `bytes` 字节，续扫已启动（完成照常走 `ScanEvent::Done`）。
    Appended { bytes: u64 },
    /// 文件被截断/重写（轮转），行索引失效。
    Rotated,
}

impl Drop for LargeDoc {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 扫描线程的载荷（持有 state 锁的 Arc 形态）。
struct DocRef {
    base: Arc<FileBase>,
    enc_kind: EncKind,
    bom_len: u64,
    content_len: u64,
    state: Arc<Mutex<ScanState>>,
    stop: Arc<AtomicBool>,
    tx: Sender<ScanEvent>,
}

impl DocRef {
    fn run(&self) {
        // 从主表 frontier（首屏扫描留下的未闭合行起点）续扫。
        let from0 = self.state.lock().table.total_bytes();
        let mut feed = LineFeed::new(self.enc_kind, from0);
        let unit = match self.enc_kind {
            EncKind::AsciiCompat { .. } => 1usize,
            _ => 2usize,
        };
        // 读取位置必须独立于行表推进：table.total_bytes() 只计已
        // 闭合的行，块尾悬挂的未闭合字节若按它回退读取，会被重复
        // 喂入（块尾 open 字节双重累计，行界从此漂移）。
        let mut read_pos = from0;
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            if self.base.file_changed() {
                let _ = self.tx.send(ScanEvent::Rotated);
                return;
            }
            let tail_start = {
                let st = self.state.lock();
                st.tail.as_ref().map(|t| t.start)
            };
            if read_pos >= self.content_len {
                break;
            }
            let want = ((self.content_len - read_pos).min(READ_CHUNK as u64) as usize / unit) * unit;
            if want == 0 {
                break;
            }
            let n = self.base.read_span(self.bom_len + read_pos, &mut buf[..want]);
            read_pos += n as u64;
            if n == 0 {
                break;
            }
            {
                let mut st = self.state.lock();
                let mut closed_any = false;
                feed.feed(read_pos, &buf[..n], &mut |_a, b, k, c| {
                    st.table.push_line(b, k, c);
                    closed_any = true;
                });
                // 主表越过尾区起点（ts 处的行已闭合）：尾区行即将由
                // 主表覆盖，丢弃避免重复计数。
                if let Some(ts) = tail_start {
                    if st.table.total_bytes() > ts {
                        st.tail = None;
                    }
                }
                if closed_any {
                    let _ = self.tx.send(ScanEvent::Progress {
                        bytes_done: st.table.total_bytes(),
                        total: self.content_len,
                        lines: st.table.len() as u64,
                    });
                }
            }
            if n < want {
                break;
            }
        }
        // EOF：冲刷未闭合行，标记完成。
        {
            let mut st = self.state.lock();
            feed.finish(&mut |_a, b, k, c| {
                st.table.push_line(b, k, c);
            });
            st.done = true;
            let lines = st.table.len() as u64;
            let _ = self.tx.send(ScanEvent::Done { lines });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("evlarge-{name}-{}", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    /// 基线口径：按文本切行（与 Document::from_text 同一语义）。
    fn naive_lines(text: &str) -> Vec<(String, u8)> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(p) = rest.find(['\n', '\r']) {
            let (line, after) = rest.split_at(p);
            let (eol, next) = match after.as_bytes()[0] {
                b'\r' if after.starts_with("\r\n") => (1u8, &after[2..]),
                b'\r' => (3u8, &after[1..]),
                _ => (2u8, &after[1..]),
            };
            out.push((line.to_string(), eol));
            rest = next;
        }
        out.push((rest.to_string(), 0));
        out
    }

    fn assert_matches_naive_named(name: &str, text: &str) {
        let p = temp_file(name, text.as_bytes());
        let doc = LargeDoc::open(&p).unwrap();
        assert!(doc.wait_for_scan(Duration::from_secs(30)), "scan timed out");
        let naive = naive_lines(text);
        assert_eq!(
            doc.line_count(),
            naive.len(),
            "line count: {} vs naive {}",
            doc.line_count(),
            naive.len()
        );
        for (i, (want, kind)) in naive.iter().enumerate() {
            let mut got = String::new();
            assert!(doc.line_text(i, &mut got), "line {i}");
            assert_eq!(&got, want, "line {i} text");
            assert_eq!(doc.eol_kind(i), *kind, "line {i} eol");
            assert_eq!(doc.line_len(i), want.chars().count(), "line {i} len");
        }
        // 字符映射抽查
        let total: usize = naive
            .iter()
            .map(|(t, k)| t.chars().count() + match k {
                1 => 2,
                2 | 3 => 1,
                _ => 0,
            })
            .sum();
        assert_eq!(doc.len_chars(), total);
        let mut acc = 0usize;
        for (i, (t, k)) in naive.iter().enumerate() {
            assert_eq!(doc.line_to_char(i), Some(acc));
            assert_eq!(doc.char_to_line(acc.min(total)), i);
            acc += t.chars().count()
                + match k {
                    1 => 2,
                    2 | 3 => 1,
                    _ => 0,
                };
        }
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn oracle_ascii_lf() {
        assert_matches_naive_named("oracle-one-ntwo-nthree", "one\ntwo\nthree");
    }

    #[test]
    fn oracle_mixed_eols() {
        assert_matches_naive_named("oracle-a-r-nb-rc-nd-r-ne", "a\r\nb\rc\nd\r\ne");
    }

    #[test]
    fn oracle_empty_and_single() {
        assert_matches_naive_named("oracle-empty", "");
        assert_matches_naive_named("oracle-noeol", "no eol at all");
        assert_matches_naive_named("oracle-n", "\n");
        assert_matches_naive_named("oracle-r-n-r-n", "\r\n\r\n");
    }

    #[test]
    fn oracle_utf8_cjk() {
        assert_matches_naive_named("oracle-n-r-n", "第一行\n第二行\r\n第三行");
    }

    #[test]
    fn text_range_spans_eols() {
        let p = temp_file("range", b"ab\ncd\r\nef");
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        assert_eq!(doc.text_range(0, 2), "ab");
        assert_eq!(doc.text_range(2, 3), "\n");
        assert_eq!(doc.text_range(2, 6), "\ncd\r");
        assert_eq!(doc.len_chars(), 9);
        std::fs::remove_file(&p).ok();
    }

    /// M5 选区导出：BOM 保留 + 行区间按原字节直拷。
    #[test]
    fn export_lines_slices_bytes() {
        // 带 BOM 的 CRLF/LF 混合文件。
        let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"alpha\r\nbeta\ngamma\r\ndelta");
        let p = temp_file("export", &bytes);
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        let mut out = Vec::new();
        // 行 1..3 = BOM + "beta\ngamma\r\n"（12 字节）
        let n = doc.export_lines(1, 3, &mut out).unwrap();
        assert_eq!(n, 15);
        assert_eq!(&out, &b"\xEF\xBB\xBFbeta\ngamma\r\n"[..]);
        // 全文件
        let mut all = Vec::new();
        doc.export_lines(0, doc.line_count(), &mut all).unwrap();
        assert_eq!(all, bytes);
        // 空区间只写 BOM
        let mut bom_only = Vec::new();
        let n = doc.export_lines(2, 2, &mut bom_only).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&bom_only, b"\xEF\xBB\xBF");
        std::fs::remove_file(&p).ok();
    }

    /// M5 替换结果导出：cs/ci 命中与约束拒绝。
    #[test]
    fn export_replaced_streams() {
        let text = "error one\nERROR two\nok error\ntail";
        let p = temp_file("repl", text.as_bytes());
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        // cs：只命中小写 error
        let mut out = Vec::new();
        let (_, n) = doc.export_replaced("error", "WARN", true, &mut out).unwrap();
        assert_eq!(n, 2);
        assert_eq!(String::from_utf8(out).unwrap(), "WARN one\nERROR two\nok WARN\ntail");
        // ci（ASCII）：三处命中
        let mut out = Vec::new();
        let (_, n) = doc.export_replaced("error", "X", false, &mut out).unwrap();
        assert_eq!(n, 3);
        assert_eq!(String::from_utf8(out).unwrap(), "X one\nX two\nok X\ntail");
        // 非 ASCII needle + ci → 拒绝
        assert!(doc.export_replaced("错误", "X", false, &mut Vec::new()).is_err());
        // 空 needle → 拒绝
        assert!(doc.export_replaced("", "X", true, &mut Vec::new()).is_err());
        std::fs::remove_file(&p).ok();
    }

    /// M5 转普通模式的物化：与逐行读取一致（含 CJK 与混合 EOL）。
    #[test]
    fn to_text_full_materializes() {
        let text = "第一行\r\nsecond\n第三行";
        let p = temp_file("mat", text.as_bytes());
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        assert_eq!(doc.to_text_full(), text);
        std::fs::remove_file(&p).ok();
    }

    // ---- M6 overlay ----

    ///.dgdiff 序列化往返 + raw→logical 映射 + 预算拒绝。
    #[test]
    fn overlay_sidecar_and_mapping() {
        let p = temp_file("ovl-sc", b"alpha\nbeta\ngamma\n");
        let mut doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        // 编辑第 2 行（"beta" → "BETA!"），行 0/2 不动。
        let beta_start = doc.line_to_char(1).unwrap();
        doc.edit(beta_start, beta_start + 4, "BETA!").unwrap();
        assert_eq!(doc.line_count(), 4); // 3 行 + 末空行
        let mut s = String::new();
        doc.line_text(1, &mut s);
        assert_eq!(s, "BETA!");
        // raw→logical：未触碰的行映射（编辑后行 1 变长 1 字符，其后
        // 偏移漂移一个字符）；被改写的行（beta）命中为陈旧 → None。
        assert_eq!(doc.map_base_char(0), Some(0)); // 'a'
        let beta_raw = 6; // "alpha\n" = 6 chars
        assert_eq!(doc.map_base_char(beta_raw), None); // beta 已被改写
        let gamma_raw = 11; // 6 + "beta\n"
        assert_eq!(doc.map_base_char(gamma_raw), Some(12)); // 6 + "BETA!\n"
        // 序列化 → 丢弃 → 应用（模拟重开恢复）。
        let bytes = doc.overlay_serialize().unwrap();
        let mut doc2 = LargeDoc::open(&p).unwrap();
        doc2.wait_for_scan(Duration::from_secs(10));
        assert!(doc2.overlay_deserialize_apply(&bytes));
        let mut s2 = String::new();
        doc2.line_text(1, &mut s2);
        assert_eq!(s2, "BETA!");
        assert_eq!(doc2.len_chars(), doc.len_chars());
        // 写出等价。
        let (mut a, mut b) = (Vec::new(), Vec::new());
        doc.write_merged(&mut a).unwrap();
        doc2.write_merged(&mut b).unwrap();
        assert_eq!(a, b);
        std::fs::remove_file(&p).ok();
    }

    /// UTF-16 文件上的编辑与合并写回（BOM + 码元行尾重编码）。
    #[test]
    fn overlay_utf16_roundtrip() {
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        bytes.extend("one\r\ntwo\n".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let p = temp_file("ovl16", &bytes);
        let mut doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        let two = doc.line_to_char(1).unwrap();
        doc.edit(two, two + 3, "二").unwrap();
        let mut s = String::new();
        doc.line_text(1, &mut s);
        assert_eq!(s, "二");
        let mut out = Vec::new();
        doc.write_merged(&mut out).unwrap();
        let mut want: Vec<u8> = vec![0xFF, 0xFE];
        want.extend("one\r\n二\n".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(out, want);
        std::fs::remove_file(&p).ok();
    }

    /// follow-tail：追加即增量续扫，行表/overlay 同步增长；缩短报轮转。
    #[test]
    fn follow_tail_appends_and_rotates() {
        let p = temp_file("tail", b"l0\nl1\nl2\n");
        let mut doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        assert_eq!(doc.line_count(), 4);
        // 追加两行（Windows 允许向被映射文件追加）。
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(b"l3\nl4\n").unwrap();
        }
        match doc.follow_tail_tick() {
            Some(TailEvent::Appended { bytes }) => assert_eq!(bytes, 6),
            other => panic!("expected append, got {other:?}"),
        }
        assert!(doc.is_scanning());
        assert!(doc.wait_for_scan(Duration::from_secs(10)));
        doc.on_tail_done();
        assert_eq!(doc.line_count(), 6);
        let mut s = String::new();
        doc.line_text(4, &mut s);
        assert_eq!(s, "l4");
        // 追加 + overlay 编辑共存：编辑行 0，再追加一行。
        doc.edit(0, 1, "L").unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            f.write_all(b"l5\n").unwrap();
        }
        assert!(matches!(
            doc.follow_tail_tick(),
            Some(TailEvent::Appended { .. })
        ));
        assert!(doc.wait_for_scan(Duration::from_secs(10)));
        doc.on_tail_done();
        assert_eq!(doc.line_count(), 7);
        let mut s = String::new();
        doc.line_text(6, &mut s);
        assert_eq!(s, ""); // 末空行
        doc.line_text(5, &mut s);
        assert_eq!(s, "l5");
        // 轮转（缩短）不走这里验证：Windows 不允许截断被映射的文件
        // （ERROR_USER_MAPPED_FILE），长度变化检测本身由 base.rs 的
        // rotation_detected_by_length_change（pread 路径）覆盖。
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn utf16_with_bom() {
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        bytes.extend("a\r\nbc\nd".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let p = temp_file("u16", &bytes);
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(10));
        assert_eq!(doc.line_count(), 3);
        let mut s = String::new();
        assert!(doc.line_text(0, &mut s));
        assert_eq!(s, "a");
        assert_eq!(doc.eol_kind(0), 1);
        assert!(doc.line_text(1, &mut s));
        assert_eq!(s, "bc");
        assert_eq!(doc.eol_kind(1), 2);
        assert!(doc.line_text(2, &mut s));
        assert_eq!(s, "d");
        assert_eq!(doc.eol_kind(2), 0);
        std::fs::remove_file(&p).ok();
    }

    /// 大文件路径：打开即有首屏 + 尾区，后台补扫后行号精确。
    #[test]
    fn large_file_progressive() {
        // ~12MB，超过 SYNC_SCAN_LIMIT：走后台扫描路径。
        let line = b"0123456789abcdefghijklmnopqrstuvwxyz SECOND-PART third part\n";
        let n_lines = (12 << 20) / line.len() as u64 + 16;
        let mut f = std::fs::File::create(std::env::temp_dir().join("evlarge-big")).unwrap();
        let mut written = 0u64;
        while written < n_lines * line.len() as u64 {
            f.write_all(line).unwrap();
            written += line.len() as u64;
        }
        drop(f);
        let p = std::env::temp_dir().join("evlarge-big");
        let doc = LargeDoc::open(&p).unwrap();
        // 打开即返回：首屏与尾区可用（主表 + 尾区行数有限）。
        let immediate = doc.line_count();
        assert!(immediate > 0, "front index must exist at open");
        assert!(doc.is_scanning() || doc.line_count() > 0);
        // 尾区最后一行立即正确（Ctrl+End 场景）。
        doc.wait_for_scan(Duration::from_secs(60));
        let full = doc.line_count();
        assert!(full > immediate, "background scan must add lines: {full} vs {immediate}");
        // 抽查首/中/尾行内容。
        let mut s = String::new();
        assert!(doc.line_text(0, &mut s));
        assert_eq!(s.as_bytes(), &line[..line.len() - 1]);
        let mid = full / 2;
        assert!(doc.line_text(mid, &mut s));
        assert_eq!(s.as_bytes(), &line[..line.len() - 1]);
        // 文件以行尾符结尾：最后一行是末空行，有效内容行是 full-2
        let last = full - 2;
        assert!(doc.line_text(last, &mut s));
        assert_eq!(s.as_bytes(), &line[..line.len() - 1]);
        assert_eq!(doc.len_chars(), (full - 1) * line.len());
        assert_eq!(doc.line_to_char(last), Some((last * line.len()) as usize));
        std::fs::remove_file(&p).ok();
    }

    /// M6 基准（plan §7）：1GB + 100 编辑保存 ≈ 磁盘拷贝；边车恢复
    /// 编辑态 < 300ms。ignore 常规套件（生成 1GB + 拷贝对照，分钟级）：
    /// `cargo test -p dg-large --release -- --ignored bench_m6 --nocapture`
    #[test]
    #[ignore = "1GB 基准，手动跑"]
    fn bench_m6_save_and_sidecar() {
        let p = std::env::temp_dir().join(format!("evlarge-bench-m6-{}.log", std::process::id()));
        // 1GB / ~19.4M 行：55B/行，分块生成避免一次性 1GB String。
        let line = b"2026-10-25T01:02:03.000Z line 00000000 payload padding
";
        let chunk_lines = 20_000usize;
        let chunk: Vec<u8> = line.repeat(chunk_lines);
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&p).unwrap();
            for _ in 0..(1 << 30) / (line.len() * chunk_lines as u32 as usize) {
                f.write_all(&chunk).unwrap();
            }
        }
        let file_len = std::fs::metadata(&p).unwrap().len();
        let mut doc = LargeDoc::open(&p).unwrap();
        assert!(doc.wait_for_scan(Duration::from_secs(120)));
        let total = doc.line_count() as u64;
        eprintln!("bench: file={}B lines={}", file_len, total);

        // 100 处编辑：均布全文，每处 6 字符替换。
        for i in 0..100u64 {
            let at = (total - 2) * (i + 1) / 101;
            let s = doc.line_to_char(at as usize).unwrap() + 30;
            doc.edit(s, s + 6, "EDITED").unwrap();
        }
        assert!(doc.is_dirty());

        // 保存：流式合并（tmp → rename 由 UI 层做，这里量核心写）。
        let tmp = p.with_extension("eesave");
        let t_save = std::time::Instant::now();
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            let n = doc.write_merged(&mut f).unwrap();
            use std::io::Write as _;
            f.flush().unwrap();
            assert_eq!(n, file_len);
        }
        let t_save = t_save.elapsed();

        // 对照：纯磁盘拷贝同容量。
        let cp = p.with_extension("copy");
        let t_copy = std::time::Instant::now();
        std::fs::copy(&p, &cp).unwrap();
        let t_copy = t_copy.elapsed();

        // 边车：序列化（关闭时写）。
        let t_ser = std::time::Instant::now();
        let side = doc.overlay_serialize().expect("serialize");
        let t_ser = t_ser.elapsed();

        // 重开（保存后的新底座）+ 应用边车 = 恢复编辑态。
        std::fs::rename(&tmp, &p).unwrap();
        let _ = std::fs::remove_file(&cp);
        let t_reopen = std::time::Instant::now();
        let mut doc2 = LargeDoc::open(&p).unwrap();
        let scanned = doc2.wait_for_scan(Duration::from_secs(120));
        let t_scan = t_reopen.elapsed();
        let t_apply = std::time::Instant::now();
        assert!(doc2.overlay_deserialize_apply(&side));
        let t_apply = t_apply.elapsed();
        assert!(doc2.is_dirty());
        let mut s = String::new();
        doc2.line_text(((total - 2) / 101) as usize, &mut s);
        assert!(s.contains("EDITED"), "恢复后应含编辑: {s}");

        eprintln!(
            "bench: save(merged)={:?} disk_copy={:?} ratio={:.2}x | sidecar_ser={:?} ({}B) reopen_scan={:?}(done={scanned}) dgdiff_apply={:?}",
            t_save,
            t_copy,
            t_save.as_secs_f64() / t_copy.as_secs_f64(),
            t_ser,
            side.len(),
            t_scan,
            t_apply,
        );
        // 保存 ≈ 磁盘拷贝（放宽到 3×：合并含 100 处解码-重编码点）。
        assert!(
            t_save < t_copy.mul_f64(3.0),
            "合并保存 {} 超过磁盘拷贝 3 倍 {}",
            t_save.as_secs_f32(),
            t_copy.as_secs_f32()
        );
        // 编辑态恢复（.dgdiff 应用）< 300ms。
        assert!(
            t_apply < Duration::from_millis(300),
            "边车应用 {:?} 超过 300ms",
            t_apply
        );
        std::fs::remove_file(&p).ok();
    }
}

#[cfg(test)]
mod debug_large {
    use super::*;
    use std::io::Write;

    #[test]
    fn large_file_full_table_verified() {
        let line = b"0123456789abcdefghijklmnopqrstuvwxyz SECOND-PART third part\n";
        assert_eq!(line.len(), 60);
        let n = (9 << 20) / 60 + 3; // ~9MB，超过 SYNC_SCAN_LIMIT
        let p = std::env::temp_dir().join(format!("evlarge-dbg-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&p).unwrap();
            for _ in 0..n {
                f.write_all(line).unwrap();
            }
        }
        let doc = LargeDoc::open(&p).unwrap();
        doc.wait_for_scan(Duration::from_secs(60));
        // 文件以行尾符结尾：n 行内容 + 1 行末空行
        assert_eq!(doc.line_count(), n + 1, "divergence: line count");
        // 全表逐行校验：起点、长度、kind、文本
        let mut buf = vec![0u8; 64];
        for i in 0..n {
            let span = doc.line_span(i).unwrap();
            if span.start != (i as u64) * 60 || span.bytes != 60 || span.kind != 2 {
                panic!(
                    "divergence: line {i} span=({},{},{}) want=({},60,2) tail_start_diff",
                    span.start,
                    span.bytes,
                    span.kind,
                    i * 60
                );
            }
            let got = doc.base.read_span(doc.bom_len + span.start, &mut buf[..59]);
            if got != 59 || &buf[..59] != &line[..59] {
                panic!("divergence: line {i} content mismatch got {got}");
            }
        }
        std::fs::remove_file(&p).ok();
    }
}

impl LargeDoc {
    /// 平坦字符索引 → 文件字节偏移（含 BOM）。UTF-8 精确；其它
    /// 编码给出"解码后再编码为 UTF-8"的视角偏移（近似）。
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        let Some(span) = self.line_span(self.char_to_line(char_idx)) else {
            return self.bom_len as usize + self.content_len as usize;
        };
        let line = self.char_to_line(char_idx);
        let base_char = self.line_to_char(line).unwrap_or(0);
        let col = char_idx.saturating_sub(base_char);
        let text = self.line_text_owned(line).unwrap_or_default();
        let content_chars = text.chars().count();
        let base_byte = self.bom_len + span.start;
        if col <= content_chars {
            base_byte as usize
                + text
                    .chars()
                    .take(col)
                    .map(|c| c.len_utf8())
                    .sum::<usize>()
        } else {
            // 行尾符或行尾之后：整行末尾
            (base_byte + span.bytes as u64) as usize
        }
    }

    /// 文件字节偏移 → 平坦字符索引（UTF-8 精确，其它编码近似）。
    pub fn byte_to_char(&self, byte_idx: usize) -> usize {
        let off = byte_idx as u64;
        let line = match self.byte_to_line_display(off) {
            Some(l) => l,
            None => return self.len_chars(),
        };
        let Some(span) = self.line_span(line) else {
            return self.len_chars();
        };
        let base_byte = self.bom_len + span.start;
        let in_line = off.saturating_sub(base_byte).min(span.bytes as u64) as usize;
        let text = self.line_text_owned(line).unwrap_or_default();
        let mut bytes = 0usize;
        let mut chars = 0usize;
        for ch in text.chars() {
            if bytes >= in_line {
                break;
            }
            bytes += ch.len_utf8();
            chars += 1;
        }
        self.line_to_char(line).unwrap_or(0) + chars
    }

    /// 文件字节偏移所在的显示行。
    pub fn byte_to_line_display(&self, abs_off: u64) -> Option<usize> {
        let st = self.state.lock();
        let content_off = abs_off.saturating_sub(self.bom_len);
        if content_off < st.table.total_bytes() {
            return Some(st.table.byte_to_line(content_off));
        }
        let tail = st.tail.as_ref()?;
        Some(st.table.len() + tail.table.byte_to_idx(content_off))
    }
}

