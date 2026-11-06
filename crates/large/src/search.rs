//! 大文件流式搜索。
//!
//! 绝不整体解码文档：搜索线程按块读文件底座，命中以**全局字符
//! 偏移**上报（与行表的计数约定一致：内容区字符、含行尾符 —
//! `LineTable::total_chars` 的同一坐标系），UI 侧换算成 `Match`
//! 后 `char_to_line` / `line_to_char` 直接可用。
//!
//! 路径按编码与查询挑选：
//! - UTF-8 + 大小写敏感（任意 needle），或 + 纯 ASCII 忽略大小写：
//! **字节路径**（memchr 锚定首字节 + 全量比对；UTF-8 自同步，
//! needle 的字节不会出现在多字节序列内部，跨块用 needle-1 字节
//! 重叠衔接）；
//! - UTF-8 + 忽略大小写 + 非 ASCII needle：解码路径（增量解码 +
//! 逐字符折叠比对；块尾不完整的字符序列按 decoder 报告的消耗
//! 量回退读取位置，余数下轮重读）；
//! - UTF-16：单元路径（u16 码元比对；字符数=码元数，对齐
//! `EncKind::count_chars` 的近似约定）；
//! - 其它 ASCII 兼容编码（GBK 等）：needle 用文档编码重编码后走
//! 字节路径（字符数=字节数，同表约定）；
//! - 正则：仅 UTF-8 —— 用 [`crate::scanner::LineFeed`] 流式逐行
//! （与行扫描同一台状态机，坐标同源），逐行 `find_iter`。跨行
//! 模式不生效（日志/jsonl 不需要）。
//!
//! 忽略大小写仅支持 UTF-8（单字节折叠会破坏 GBK 尾字节） —— 由
//! [`LargeDoc::start_search`] 同步拒绝。取消：`SearchHandle::cancel`
//! 置旗标，线程每块（或每 256 行）检查一次；句柄 Drop 即取消。命中
//! 数到达 `SearchOptions::limit` 即停（`Done { truncated: true }`）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use memchr::{memchr, memchr2};
use regex::RegexBuilder;

use crate::base::{FileBase, READ_CHUNK};
use crate::lineindex::MAX_LINE_BYTES;
use crate::scanner::{EncKind, LineFeed};

/// 进度事件的最小字节间隔。
const PROGRESS_EVERY: u64 = 32 << 20;
/// 单个 `Matches` 事件的条数上限。
const BATCH: usize = 512;

#[derive(Clone, Copy)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// 停止收集的命中数上限（防 "搜单字母" 把内存打爆）。
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            whole_word: false,
            regex: false,
            limit: 100_000,
        }
    }
}

/// 全局字符偏移区间 `[start, end)`。
pub enum SearchEvent {
    Matches(Vec<(u64, u64)>),
    Progress { bytes_done: u64, total: u64 },
    Done { truncated: bool },
}

pub struct SearchHandle {
    stop: Arc<AtomicBool>,
    rx: Receiver<SearchEvent>,
}

impl SearchHandle {
    /// 非阻塞取一个事件。
    pub fn poll(&self) -> Option<SearchEvent> {
        self.rx.try_recv().ok()
    }

    /// 阻塞取一个事件（测试用）。
    pub fn recv_timeout(&self, d: Duration) -> Option<SearchEvent> {
        self.rx.recv_timeout(d).ok()
    }

    /// 阻塞直到通道关闭（测试用）：线程退出且事件排空。
    pub fn wait_closed(&self, d: Duration) {
        let deadline = std::time::Instant::now() + d;
        while let Ok(_ev) = self.rx.recv_timeout(Duration::from_millis(200)) {
            if std::time::Instant::now() > deadline {
                break;
            }
        }
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// 搜索线程共享的只读事实。
pub(crate) struct SearchCtx {
    pub base: Arc<FileBase>,
    pub enc: &'static encoding_rs::Encoding,
    pub enc_kind: EncKind,
    pub bom_len: u64,
    pub content_len: u64,
}

/// 发射器：批量缓冲 + 上限 + 进度节流 + 取消检查。
/// 方法返回 false = 应立即停止（接收端已关闭或到达上限）；
/// 收尾统一由 [`spawn`] 的闭包做一次 `finish`。
struct Emitter {
    tx: Sender<SearchEvent>,
    stop: Arc<AtomicBool>,
    pending: Vec<(u64, u64)>,
    hits: u64,
    limit: u64,
    truncated: bool,
    last_progress: u64,
    content_len: u64,
}

impl Emitter {
    fn new(
        tx: Sender<SearchEvent>,
        stop: Arc<AtomicBool>,
        opts: SearchOptions,
        content_len: u64,
    ) -> Self {
        Self {
            tx,
            stop,
            pending: Vec::new(),
            hits: 0,
            limit: opts.limit as u64,
            truncated: false,
            last_progress: 0,
            content_len,
        }
    }

    fn hit(&mut self, s: u64, e: u64) -> bool {
        if self.hits >= self.limit {
            self.truncated = true;
            return false;
        }
        self.pending.push((s, e));
        self.hits += 1;
        if self.pending.len() >= BATCH {
            self.flush()
        } else {
            true
        }
    }

    fn flush(&mut self) -> bool {
        if self.pending.is_empty() {
            return true;
        }
        let batch = std::mem::take(&mut self.pending);
        self.tx.send(SearchEvent::Matches(batch)).is_ok()
    }

    fn progress(&mut self, bytes_done: u64) -> bool {
        if bytes_done.saturating_sub(self.last_progress) < PROGRESS_EVERY {
            return true;
        }
        self.last_progress = bytes_done;
        self.tx
            .send(SearchEvent::Progress {
                bytes_done,
                total: self.content_len,
            })
            .is_ok()
    }

    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn finish(mut self) {
        let _ = self.flush();
        let _ = self.tx.send(SearchEvent::Done { truncated: self.truncated });
    }
}

/// 词边界：与 dg-core find.rs 的 `boundary_ok` 同语义（字母数字或
/// `_` 算词字符）。字节判断把非 ASCII 邻居视为边界 —— 与按字符判断
/// 在 CJK 邻接处有分歧，v1 接受（模块头注释）。
fn byte_is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn char_is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 字节路径：needle 已按文档编码编码（忽略大小写时双方都已折叠成
/// 小写 ASCII）。region = carry ++ chunk，命中后从命中点+1 继续
/// memchr（非重叠推进与 find.rs 一致：命中即跳过整个 needle……
/// 实际是跳 1 字节重新锚定，重叠命中也会报，与 find_all 相同）。
fn run_byte_path(ctx: &SearchCtx, em: &mut Emitter, needle: &[u8], opts: SearchOptions) {
    let mut carry: Vec<u8> = Vec::new();
    // carry 之前的累计字符数（坐标系同 count_chars）。
    let mut chars_before: u64 = 0;
    let mut read_pos: u64 = 0; // 内容区相对
    let mut buf = vec![0u8; READ_CHUNK];
    let overlap = needle.len().saturating_sub(1);
    loop {
        if em.cancelled() {
            return;
        }
        let want = (ctx.content_len - read_pos).min(buf.len() as u64) as usize;
        if want == 0 {
            break;
        }
        let n = ctx.base.read_span(ctx.bom_len + read_pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        read_pos += n as u64;
        let mut region = std::mem::take(&mut carry);
        region.extend_from_slice(&buf[..n]);
        let hay = &region[..];
        let mut at = 0usize;
        // hay[..counted] 的字符数已累计进 chars_at。忽略大小写时用
        // memchr2 双锚定首字节的大小写变体、eq_ignore_ascii_case
        // 验证 —— 不做整块折叠（省一遍 pass）。
        let mut counted = 0usize;
        let mut chars_at = 0u64;
        let anchor1 = needle[0];
        let anchor2 = needle[0].to_ascii_uppercase();
        let ci = !opts.case_sensitive;
        while let Some(rel) = if ci && anchor1 != anchor2 {
            memchr2(anchor1, anchor2, &hay[at..])
        } else {
            memchr(anchor1, &hay[at..])
        } {
            let p = at + rel;
            // 与 find_all 同款推进：命中且过词检 ⇒ 跳过整个 needle
            // （不重叠），否则前进 1。字符偏移按命中顺序增量累计 —
            // 每次命中从头 count_chars 是 O(n²)（5MB 块 × 10 万命中
            // ≈ 数百 GB 的重复扫描）。
            let mut advance = 1usize;
            let matched = p + needle.len() <= hay.len()
                && (if ci {
                    hay[p..p + needle.len()].eq_ignore_ascii_case(needle)
                } else {
                    &hay[p..p + needle.len()] == needle
                });
            if matched {
                let ok_word = !opts.whole_word
                    || (p
                        .checked_sub(1)
                        .map(|q| !byte_is_word(hay[q]))
                        .unwrap_or(true)
                        && hay
                            .get(p + needle.len())
                            .map(|b| !byte_is_word(*b))
                            .unwrap_or(true));
                if ok_word {
                    chars_at += ctx.enc_kind.count_chars(&hay[counted..p]);
                    counted = p;
                    let char_off = chars_before + chars_at;
                    let char_len = ctx.enc_kind.count_chars(needle);
                    if !em.hit(char_off, char_off + char_len) {
                        return;
                    }
                    advance = needle.len();
                }
            }
            at = p + advance;
        }
        let keep = overlap.min(region.len());
        let consumed = region.len() - keep;
        chars_before += ctx.enc_kind.count_chars(&region[..consumed]);
        carry = region.split_off(consumed);
        if !em.progress(read_pos) {
            return;
        }
        if read_pos >= ctx.content_len {
            break;
        }
    }
}

/// 解码路径：UTF-8 + 忽略大小写 + 非 ASCII needle。增量解码，逐字
/// 符折叠比对，跨块用 needle-1 字符重叠。块尾不完整的多字节序列
/// 未被 decoder 消耗时，读取位置只推进到消耗量，余数下轮重读。
fn run_decoded_path(ctx: &SearchCtx, em: &mut Emitter, needle: &[char], opts: SearchOptions) {
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut carry: Vec<char> = Vec::new();
    let mut chars_before: u64 = 0;
    let mut read_pos: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK];
    // decode_to_string 要求 output 容量 ≥ input 长度（clear 只清内容
    // 不缩容量）。
    let mut text = String::with_capacity(READ_CHUNK + 8);
    let overlap = needle.len().saturating_sub(1);
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let needle_fold: Vec<char> = needle.iter().map(|&c| fold(c)).collect();
    loop {
        if em.cancelled() {
            return;
        }
        let want = (ctx.content_len - read_pos).min(buf.len() as u64) as usize;
        if want == 0 {
            break;
        }
        let n = ctx.base.read_span(ctx.bom_len + read_pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        let last = read_pos + n as u64 >= ctx.content_len;
        text.clear();
        let (_res, consumed, _last) =
            decoder.decode_to_string(&buf[..n], &mut text, last);
        read_pos += consumed as u64;
        let mut region = std::mem::take(&mut carry);
        region.extend(text.chars().map(fold));
        let hay: &[char] = &region[..];
        let nl = needle_fold.len();
        let mut at = 0usize;
        while at + nl <= hay.len() {
            if hay[at..at + nl] == needle_fold[..] {
                let ok_word = !opts.whole_word
                    || (at
                        .checked_sub(1)
                        .map(|q| !char_is_word(hay[q]))
                        .unwrap_or(true)
                        && hay.get(at + nl).map(|c| !char_is_word(*c)).unwrap_or(true));
                if ok_word {
                    if !em.hit(chars_before + at as u64, chars_before + (at + nl) as u64) {
                        return;
                    }
                    at += nl;
                    continue;
                }
            }
            at += 1;
        }
        let keep = overlap.min(region.len());
        chars_before += (region.len() - keep) as u64;
        carry = region.split_off(region.len() - keep);
        if !em.progress(read_pos) {
            return;
        }
        if read_pos >= ctx.content_len && consumed == n {
            break;
        }
    }
}

/// 单元路径：UTF-16。needle 编码为同序码元，逐码元比对；字符数
/// = 码元数（对齐 count_chars 的近似约定 —— 代理对计 2）。
fn run_unit16_path(
    ctx: &SearchCtx,
    em: &mut Emitter,
    needle: &[u16],
    opts: SearchOptions,
    big_endian: bool,
) {
    let mut carry: Vec<u16> = Vec::new();
    let mut chars_before: u64 = 0;
    let mut read_pos: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK];
    let overlap = needle.len().saturating_sub(1);
    let unit_is_word = |u: u16| char::from_u32(u as u32).map(char_is_word).unwrap_or(false);
    loop {
        if em.cancelled() {
            return;
        }
        let remain = ctx.content_len - read_pos;
        if remain < 2 {
            break;
        }
        let want = (remain.min(buf.len() as u64) as usize) & !1;
        if want == 0 {
            break;
        }
        let n = ctx.base.read_span(ctx.bom_len + read_pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        read_pos += n as u64;
        let mut region = std::mem::take(&mut carry);
        region.extend(
            buf[..n].chunks_exact(2).map(|p| {
                let u = u16::from_ne_bytes([p[0], p[1]]);
                if big_endian {
                    u.swap_bytes()
                } else {
                    u
                }
            }),
        );
        let hay: &[u16] = &region[..];
        let nl = needle.len();
        let mut at = 0usize;
        while at + nl <= hay.len() {
            if &hay[at..at + nl] == needle {
                let ok_word = !opts.whole_word
                    || (at
                        .checked_sub(1)
                        .map(|q| !unit_is_word(hay[q]))
                        .unwrap_or(true)
                        && hay.get(at + nl).map(|u| !unit_is_word(*u)).unwrap_or(true));
                if ok_word {
                    if !em.hit(
                        chars_before + at as u64,
                        chars_before + (at + nl) as u64,
                    ) {
                        return;
                    }
                    at += nl;
                    continue;
                }
            }
            at += 1;
        }
        let keep = overlap.min(region.len());
        chars_before += (region.len() - keep) as u64;
        carry = region.split_off(region.len() - keep);
        if !em.progress(read_pos) {
            return;
        }
        if read_pos >= ctx.content_len {
            break;
        }
    }
}

/// 正则路径：仅 UTF-8。LineFeed 流式逐行（同一台行状态机），行闭
/// 合后重读该行字节解码并 `find_iter`；行的全局字符起点 = 已闭合
/// 行 chars 的累计。超长行由 LineFeed 按 MAX_LINE_BYTES 切分（与
/// 行表一致）。
fn run_regex_path(ctx: &SearchCtx, em: &mut Emitter, re: &regex::Regex, opts: SearchOptions) {
    let mut feed = LineFeed::new(ctx.enc_kind, 0);
    let mut read_pos: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK];
    let mut line_buf = vec![0u8; MAX_LINE_BYTES as usize + 8];
    let mut chars_running: u64 = 0;
    let mut lines_since_check: u32 = 0;
    let mut process = |em: &mut Emitter,
                       chars_running: &mut u64,
                       start: u64,
                       bytes_incl: u64,
                       kind: u8,
                       chars_incl: u64| {
        let content_bytes = bytes_incl - ctx.enc_kind.eol_bytes(kind);
        let line_base = *chars_running;
        *chars_running += chars_incl;
        if content_bytes == 0 {
            return true;
        }
        let want = (content_bytes as usize).min(line_buf.len());
        let got = ctx.base.read_span(ctx.bom_len + start, &mut line_buf[..want]);
        let text = String::from_utf8_lossy(&line_buf[..got]).into_owned();
        // 命中按序增量计数（m.start() 从头数 chars 是 O(行²) —
        // 8MB minified 行 × 10 万命中同款二次方）。
        let mut byte_at = 0usize;
        let mut char_at = 0u64;
        for m in re.find_iter(&text) {
            char_at += text[byte_at..m.start()].chars().count() as u64;
            byte_at = m.start();
            let col = char_at;
            let len = text[m.start()..m.end()].chars().count() as u64;
            let ok_word = !opts.whole_word
                || (m
                    .start()
                    .checked_sub(1)
                    .map(|q| !char_is_word(text.as_bytes()[q] as char))
                    .unwrap_or(true)
                    && text
                        .as_bytes()
                        .get(m.end())
                        .map(|b| !char_is_word(*b as char))
                        .unwrap_or(true));
            if ok_word && !em.hit(line_base + col, line_base + col + len) {
                return false;
            }
        }
        true
    };
    loop {
        if em.cancelled() {
            return;
        }
        let remain = ctx.content_len - read_pos;
        if remain == 0 {
            break;
        }
        let want = remain.min(buf.len() as u64) as usize;
        if want == 0 {
            break;
        }
        let n = ctx.base.read_span(ctx.bom_len + read_pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        read_pos += n as u64;
        let mut lines: Vec<(u64, u64, u8, u64)> = Vec::new();
        feed.feed(
            read_pos,
            &buf[..n],
            &mut |s, b, k, c| lines.push((s, b, k, c)),
        );
        let mut stop = false;
        for (s, b, k, c) in lines {
            lines_since_check += 1;
            if lines_since_check >= 256 {
                lines_since_check = 0;
                if em.cancelled() {
                    return;
                }
            }
            if !process(&mut *em, &mut chars_running, s, b, k, c) {
                stop = true;
                break;
            }
        }
        if stop {
            return;
        }
        if !em.progress(read_pos) {
            return;
        }
    }
    // EOF：收最后一行（无行尾符的开放行）。
    let mut lines: Vec<(u64, u64, u8, u64)> = Vec::new();
    feed.finish(&mut |s, b, k, c| lines.push((s, b, k, c)));
    for (s, b, k, c) in lines {
        let _ = process(&mut *em, &mut chars_running, s, b, k, c);
    }
}

/// 启动搜索线程。返回 `None` = 查询被同步拒绝（空 needle 由
/// `LargeDoc::start_search` 前置检查；这里防御性再查一次）。
pub(crate) fn spawn(ctx: SearchCtx, needle: &str, opts: SearchOptions) -> Option<SearchHandle> {
    if needle.is_empty() || ctx.content_len == 0 {
        return None;
    }
    let (tx, rx) = unbounded();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let needle = needle.to_string();
    let enc = ctx.enc;
    let builder = thread::Builder::new().name("dg-large-search".into());
    builder
        .spawn(move || {
            let mut em = Emitter::new(tx, stop2, opts, ctx.content_len);
            if opts.regex {
                if let Ok(re) = RegexBuilder::new(&needle)
                    .case_insensitive(!opts.case_sensitive)
                    .build()
                {
                    run_regex_path(&ctx, &mut em, &re, opts);
                }
            } else {
                match ctx.enc_kind {
                    EncKind::AsciiCompat { utf8: true } => {
                        if opts.case_sensitive || needle.is_ascii() {
                            let nb: Vec<u8> = if opts.case_sensitive {
                                needle.as_bytes().to_vec()
                            } else {
                                needle.to_ascii_lowercase().into_bytes()
                            };
                            run_byte_path(&ctx, &mut em, &nb, opts);
                        } else {
                            let nc: Vec<char> = needle.chars().collect();
                            run_decoded_path(&ctx, &mut em, &nc, opts);
                        }
                    }
                    EncKind::AsciiCompat { utf8: false } => {
                        let (nb, _used, had_errors) = enc.encode(&needle);
                        if !had_errors && !nb.is_empty() {
                            run_byte_path(&ctx, &mut em, &nb, opts);
                        }
                    }
                    EncKind::Utf16Le | EncKind::Utf16Be => {
                        let be = matches!(ctx.enc_kind, EncKind::Utf16Be);
                        let units: Vec<u16> = needle
                            .encode_utf16()
                            .map(|u| if be { u.swap_bytes() } else { u })
                            .collect();
                        run_unit16_path(&ctx, &mut em, &units, opts, be);
                    }
                }
            }
            em.finish();
        })
        .ok()?;
    Some(SearchHandle { stop, rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LargeDoc;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(tag: &str, content: &[u8]) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("evlarge-search-{tag}-{}.bin", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    /// 收完一个搜索的全部事件（Done 为止）。
    fn collect(h: &SearchHandle) -> (Vec<(u64, u64)>, bool) {
        let mut ms = Vec::new();
        let truncated;
        loop {
            match h.recv_timeout(Duration::from_secs(30)) {
                Some(SearchEvent::Matches(v)) => ms.extend(v),
                Some(SearchEvent::Progress { .. }) => {}
                Some(SearchEvent::Done { truncated: t }) => {
                    truncated = t;
                    break;
                }
                None => panic!("search channel closed before Done"),
            }
        }
        (ms, truncated)
    }

    fn doc_of(tag: &str, content: &str) -> (LargeDoc, PathBuf) {
        let p = temp_file(tag, content.as_bytes());
        let d = LargeDoc::open(&p).unwrap();
        d.wait_for_scan(Duration::from_secs(10));
        (d, p)
    }

    fn cleanup(d: LargeDoc, p: &PathBuf) {
        drop(d);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn literal_matches_and_char_offsets() {
        let (d, p) = doc_of("lit", "abc XabcY abc\nzzabc");
        let h = d.start_search("abc", SearchOptions::default()).unwrap();
        let (ms, tr) = collect(&h);
        assert!(!tr);
        // 全局字符偏移（含 \n 计入坐标系）：0, 5, 10, 16
        let starts: Vec<u64> = ms.iter().map(|m| m.0).collect();
        assert_eq!(starts, vec![0, 5, 10, 16]);
        // 第三个命中在第 0 行（\n 在偏移 13）
        assert_eq!(d.char_to_line(10), 0);
        assert_eq!(d.char_to_line(16), 1);
        cleanup(d, &p);
    }

    #[test]
    fn case_insensitive_ascii() {
        let (d, p) = doc_of("ci", "AbC xabcy ABC");
        let h = d
            .start_search(
                "aBc",
                SearchOptions {
                    case_sensitive: false,
                    ..Default::default()
                },
            )
            .unwrap();
        let (ms, _) = collect(&h);
        assert_eq!(ms, vec![(0, 3), (5, 8), (10, 13)]);
        cleanup(d, &p);
    }

    #[test]
    fn case_insensitive_non_ascii_takes_decoded_path() {
        let (d, p) = doc_of("cjk", "αβγ xαβγy ΑΒΓ");
        let h = d
            .start_search(
                "αβγ",
                SearchOptions {
                    case_sensitive: false,
                    ..Default::default()
                },
            )
            .unwrap();
        let (ms, _) = collect(&h);
        // xαβγy 内的 αβγ 与句尾 ΑΒΓ 折叠后都命中（无全字限制）
        assert_eq!(ms.len(), 3);
        cleanup(d, &p);
    }

    #[test]
    fn whole_word_filter() {
        let (d, p) = doc_of("ww", "abc abcd xabc abc");
        let h = d
            .start_search(
                "abc",
                SearchOptions {
                    whole_word: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let (ms, _) = collect(&h);
        let starts: Vec<u64> = ms.iter().map(|m| m.0).collect();
        assert_eq!(starts, vec![0, 14]);
        cleanup(d, &p);
    }

    #[test]
    fn match_crossing_chunk_boundary() {
        // READ_CHUNK=5MB：把 needle 摆在块边界上跨过去。
        let mut s = String::with_capacity(6 << 20);
        s.push_str(&"a".repeat((5 << 20) - 2));
        s.push_str("XY\n");
        s.push_str(&"b".repeat(1 << 20));
        let (d, p) = doc_of("cross", &s);
        let h = d.start_search("aXY", SearchOptions::default()).unwrap();
        let (ms, _) = collect(&h);
        assert_eq!(ms.len(), 1);
        assert_eq!(d.char_to_line(ms[0].0 as usize), 0);
        cleanup(d, &p);
    }

    #[test]
    fn regex_line_mode_and_last_unterminated_line() {
        // 末行无行尾符：feed.finish() 也要收它。
        let (d, p) = doc_of("re", "id=12 ok\nnope\nid=345 x");
        let h = d
            .start_search(
                r"id=\d+",
                SearchOptions {
                    regex: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let (ms, _) = collect(&h);
        assert_eq!(ms, vec![(0, 5), (14, 20)]);
        assert_eq!(d.char_to_line(ms[1].0 as usize), 2);
        cleanup(d, &p);
    }

    #[test]
    fn limit_truncates() {
        let (d, p) = doc_of("lim", &"ab ".repeat(500));
        let h = d
            .start_search(
                "ab",
                SearchOptions {
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        let (ms, tr) = collect(&h);
        assert!(tr);
        assert_eq!(ms.len(), 10);
        cleanup(d, &p);
    }

    #[test]
    fn cancel_stops_the_thread() {
        let mut s = String::with_capacity(120 << 20);
        s.push_str(&"filler filler\n".repeat(8_000_000)); // ~120MB
        let p = temp_file("cancel", s.as_bytes());
        let d = LargeDoc::open(&p).unwrap();
        let h = d.start_search("zzz-not-there", SearchOptions::default()).unwrap();
        h.cancel();
        // 线程应在下一块边界退出：通道随后关闭（Disconnected）。
        h.wait_closed(Duration::from_secs(10));
        cleanup(d, &p);
    }

    #[test]
    fn utf16_search() {
        // "abc 中文 xyz" → UTF-16LE + BOM
        let text = "abc 中文 xyz";
        let mut units: Vec<u8> = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            units.extend_from_slice(&u.to_le_bytes());
        }
        let p = temp_file("u16", &units);
        let d = LargeDoc::open(&p).unwrap();
        d.wait_for_scan(Duration::from_secs(10));
        let h = d.start_search("中文", SearchOptions::default()).unwrap();
        let (ms, _) = collect(&h);
        assert_eq!(ms.len(), 1);
        // 字符=码元：a(0) b(1) c(2) ' '(3) 中(4) 文(5)
        assert_eq!(ms[0], (4, 6));
        cleanup(d, &p);
    }

    #[test]
    fn empty_needle_starts_nothing() {
        let (d, p) = doc_of("empty", "abc");
        assert!(d.start_search("", SearchOptions::default()).is_none());
        cleanup(d, &p);
    }
}
