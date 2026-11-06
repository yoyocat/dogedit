//! 打开 .rs / .json 时的那次语言服务器握手，放在工作线程上。
//!
//! 起进程、等 `initialize` 回信、把整篇文档交出去 —— 这三件事原来都在 UI
//! 线程上排队（1.3 MB 的文件实测 66 ms，其中 `did_open` 还要先把整文物化成
//! 一个 `String`）。`initialize` 的等待上限是 10 秒，挂在一个慢服务器上就是
//! 界面冻 10 秒，所以它不该出现在输入线程里。
//!
//! 交换回来的只有一个 [`LspClient`]：它是 `Send` 的（通道 + 两个线程句柄），
//! 但只归 UI 线程用 —— 候选、悬停这些请求仍是输入路径上的同步等待。

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 握手跑完了，请窗口来取。
pub(super) const WM_LSP: u32 = WM_APP + 0x51;

/// 一次握手的结果。`client` 是 `None` 时（起不来、握手失败）窗口什么都不换。
pub(super) struct Answer {
    /// 握手是为哪个标签起的：文档早换了人，这条就作废。
    pub(super) doc: u64,
    pub(super) path: String,
    pub(super) uri: String,
    pub(super) client: Option<dg_lsp::LspClient>,
}

/// 交给服务器的那份文本：整篇解码一次，和 `Document::to_text()` 同一条字节流
/// （每行留着它自己的行尾）。在 worker 上做，为的是 UI 线程不再碰 1.3 MB。
fn document_text(raw: &[u8], enc: &'static encoding_rs::Encoding, bom: usize) -> String {
    let body = &raw[bom.min(raw.len())..];
    if enc == encoding_rs::UTF_8 {
        String::from_utf8_lossy(body).into_owned()
    } else {
        enc.decode(body).0.into_owned()
    }
}

/// 起服务器、握手、发 `didOpen`，然后把客户端送回 `frame`。
pub(super) fn spawn(
    frame: HWND,
    tx: &crossbeam_channel::Sender<Answer>,
    doc: u64,
    path: &str,
    uri: String,
    lang: &'static str,
    raw: std::sync::Arc<Vec<u8>>,
    enc: &'static encoding_rs::Encoding,
    bom: usize,
) {
    let tx = tx.clone();
    let path = path.to_string();
    // `HWND` 是个裸指针，不过线程边界：带它的数值走，回来再拼回去。
    let frame = frame.0 as isize;
    let root = std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let bytes = raw;
    std::thread::spawn(move || {
        let answer = |client: Option<dg_lsp::LspClient>| Answer {
            doc,
            path: path.clone(),
            uri: uri.clone(),
            client,
        };
        // rust-analyzer / json 语言服务器都在 PATH 上（rustup 装的那条）。
        let client = match dg_lsp::LspClient::start("rust-analyzer", &[]) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lsp start failed: {e}");
                let _ = tx.send(answer(None));
                return;
            }
        };
        let mut client = client;
        if client.initialize(&root).is_err() {
            eprintln!("lsp initialize failed");
            let _ = tx.send(answer(None));
            return;
        }
        // 握手不走完，服务器不会去处理 didOpen。
        client.initialized();
        let text = document_text(&bytes, enc, bom);
        client.did_open(&uri, lang, &text);
        let _ = tx.send(answer(Some(client)));
        unsafe {
            let _ = PostMessageW(
                Some(HWND(frame as *mut core::ffi::c_void)),
                WM_LSP,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
}

/// 取走最近一次跑完的握手。
pub(super) fn take(
    rx: &crossbeam_channel::Receiver<Answer>,
) -> Option<Answer> {
    let mut last = None;
    while let Ok(a) = rx.try_recv() {
        last = Some(a);
    }
    last
}
