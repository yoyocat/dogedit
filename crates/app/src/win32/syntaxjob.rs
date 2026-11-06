//! 打开文件时的那次整文件解析，放在工作线程上。
//!
//! 文本不需要等树：1.3 MB 的源文件在 UI 线程上解析要 200 ms（优化构建里量的），
//! 这期间窗口一直是空的。把解析交出去，第一个像素先出来，颜色晚一拍落地 ——
//! 交换回来的树只认它解析的那批字节，所以窗口那边还要核对文档没被动过。

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 解析跑完了，请窗口来取（[`EditorState::take_syntax_jobs`]）。
pub(super) const WM_SYNTAX: u32 = WM_APP + 0x50;

/// 工作线程交回来的东西：整篇（树或快照），或编辑后补的那一段颜色。
pub(super) enum Payload {
    Full(dg_syntax::Highlighter),
    Window(dg_syntax::WindowPaint),
}

/// 一份跑完的解析。`token` 认它是哪一次装载要的东西。
pub(super) struct Finished {
    pub(super) token: u64,
    /// worker 真正解析掉的字节数 —— 和文档现在的字节数对不上，说明文本动过了。
    pub(super) bytes: usize,
    pub(super) payload: Payload,
    pub(super) query_error: Option<String>,
}

/// 只有 UTF-8 能这么算：文档给 tree-sitter 的字节域就是 BOM 之后的原始字节，
/// 逐行解码不会动它一个字节。别的编码（UTF-16、代码页）在窗口这边同步解析 ——
/// 它们要先把整篇解出来，那份拷贝本来就不是白得的。
pub(super) fn eligible(enc: &'static encoding_rs::Encoding) -> bool {
    enc == encoding_rs::UTF_8
}

/// 一次解析的号：窗口发出去、worker 带回来，两边对上才算同一趟。
pub(super) fn next_token() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 起一次解析。`raw` 是要解析的字节：整篇任务带全文，窗口任务只带窗口
/// 那一段（窗口这边先把切片做好，worker 不再拿整篇来切）。`bom` 是全文
/// 任务要跳过的头部字节数。跑完把结果塞进 `tx` 并敲一下 `frame`。
/// `snapshot`：大文档不把树带回界面线程，就地压成区间快照（整树是源文件的
/// 几十倍，1.4 MB 的 Rust 文件约 42 MB）。
/// `window`：只重解析这一段 —— `Some((start, _))` 的 `start` 是窗口在整篇
/// 字节域里的起点，压出来的区间从它换算。这一趟不算折叠区，折叠区由
/// 停手后的整篇重算补。
/// `seed_total`：这一窗是**立表**那一趟（整篇还没回来）。值是整个文档的字节
/// 数。结果不是一段颜色而是整只高亮器：query 已经在这一趟里编译好、区间表
/// 已经装进去，界面线程直接收下就有色，不必自己再编译一次（那是几百毫秒）。
/// `snapshot_stale` 仍为真 —— 折叠区和远处的语法状态还等整篇。
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    frame: HWND,
    tx: &crossbeam_channel::Sender<Finished>,
    token: u64,
    raw: std::sync::Arc<Vec<u8>>,
    bom: usize,
    lang: tree_sitter::Language,
    highlights: Option<String>,
    folds: Option<String>,
    snapshot: bool,
    window: Option<(usize, usize)>,
    seed_total: Option<usize>,
) {
    let tx = tx.clone();
    // `HWND` 是个裸指针，不过线程边界：带它的数值走，回来再拼回去。
    let frame = frame.0 as isize;
    std::thread::spawn(move || {
        let mut h = dg_syntax::Highlighter::new(lang);
        let mut errs: Vec<String> = Vec::new();
        let body: &[u8] = &raw[bom.min(raw.len())..];
        let payload = if let Some((start, _)) = window {
            if let Some(scm) = highlights.as_deref() {
                // 和窗口那边同一个口径：query 编译失败只退回 kind 配色，原因留着报。
                if let Err(e) = h.set_query(scm) {
                    errs.push(format!("highlights.scm: {e}"));
                }
            }
            // `raw` 就是窗口那一段，偏移从 `start` 换算成整篇字节域。
            let slice: &[u8] = body;
            h.reset_feed(slice.len(), &mut |off| {
                slice[off.min(slice.len())..].to_vec()
            });
            let node_text =
                |s: usize, e: usize| slice[s.min(slice.len())..e.min(slice.len())].to_vec();
            let paint = h.window_paint(start, slice.len(), &node_text);
            match seed_total {
                // 立表：把这一窗的区间当成第一张表装进整只高亮器，正文长度
                // 按整篇记（编辑的记账从它起步）。
                Some(total) => {
                    h.defer_snapshot(total);
                    h.apply_window(paint);
                    Payload::Full(h)
                }
                None => Payload::Window(paint),
            }
        } else {
            if let Some(scm) = highlights.as_deref() {
                if let Err(e) = h.set_query(scm) {
                    errs.push(format!("highlights.scm: {e}"));
                }
            }
            if let Some(scm) = folds.as_deref() {
                if let Err(e) = h.set_fold_query(scm) {
                    errs.push(format!("folds.scm: {e}"));
                }
            }
            let mut feed = |off: usize| -> Vec<u8> {
                let from = off.min(body.len());
                body[from..(from + 64 * 1024).min(body.len())].to_vec()
            };
            h.reset_feed(body.len(), &mut feed);
            if snapshot {
                let node_text =
                    |s: usize, e: usize| body[s.min(body.len())..e.min(body.len())].to_vec();
                h.compact(&node_text);
            }
            Payload::Full(h)
        };
        let _ = tx.send(Finished {
            token,
            bytes: body.len(),
            payload,
            query_error: if errs.is_empty() {
                None
            } else {
                Some(errs.join("; "))
            },
        });
        unsafe {
            let _ = PostMessageW(
                Some(HWND(frame as *mut core::ffi::c_void)),
                WM_SYNTAX,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
}

/// 取走已经跑完的解析。
pub(super) fn take(rx: &crossbeam_channel::Receiver<Finished>) -> Vec<Finished> {
    let mut out = Vec::new();
    while let Ok(f) = rx.try_recv() {
        out.push(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有 UTF-8 的原始字节等于文档给 tree-sitter 的那条字节流；UTF-16 和代码页
    /// 要先解一遍，那份拷贝不是白得的，所以留在窗口这边同步解析。
    #[test]
    fn only_utf8_parses_off_the_thread() {
        assert!(eligible(encoding_rs::UTF_8));
        assert!(!eligible(encoding_rs::UTF_16LE));
        assert!(!eligible(encoding_rs::WINDOWS_1252));
    }

    /// 号要一次比一次新：窗口靠它把结果对回当初派出去的那一趟。
    #[test]
    fn tokens_do_not_repeat() {
        assert_ne!(next_token(), next_token());
    }
}
