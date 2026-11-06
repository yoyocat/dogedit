//! External-tool runs, off the UI thread.
//!
//! The 输出窗口's bar carries 终结 (command 1282), a button that
//! can only mean something while the child is still writing: the run's
//! container answers the click as the command's output arrives. This implementation used
//! to `read_to_string` and `wait` on the UI thread, so the window could not
//! answer any click — including that one — until the tool had ended. This
//! module moves the pipes and the wait to a worker and wakes the frame with
//! chunks, which is also what makes a long run stream into the pane instead of
//! appearing all at once at the end.
//!
//! The UI thread stays the only one that touches an editor; a worker only fills
//! the queue and posts `WM_TOOL`. 终结 terminates the pid the run holds
//! (`TerminateProcess`, the same call `std::process::Child::kill` makes) and
//! lets the reader hit EOF, so the footer and the completion notification still
//! come from one path.

use std::ffi::c_void;
use std::io::Read;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Globalization::{
    CP_ACP, MB_ERR_INVALID_CHARS, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// The wake-up the run's readers post. `WM_APP + 0x49` is the one id the
/// other workers leave free (0x41 bar, 0x47 marks, 0x48 grep).
pub(super) const WM_TOOL: u32 = WM_APP + 0x49;

/// Where the finished output goes. The pane modes stream as they arrive; the
/// four document modes keep the text until the child is gone, because they
/// edit the buffer the user is in and a chunk-at-a-time edit would put the
/// undo stack through every pipe read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Deliver {
    Pane,
    Doc(dg_core::CaptureMode),
}

pub(super) struct ToolRun {
    /// The child's process id, which is all 终结 needs.
    pub(super) pid: u32,
    pub(super) deliver: Deliver,
    /// The id of the output pane that started the run. An id survives the
    /// captions renumbering, so a closed caption cannot misroute the chunks.
    pub(super) target: u64,
    /// Everything a document mode collected; unused by a pane run.
    pub(super) all: String,
    queue: Arc<Mutex<Queue>>,
    /// A wake-up is already in the message queue, so readers stop posting.
    posted: Arc<AtomicBool>,
    frame: usize,
}

#[derive(Default)]
struct Queue {
    chunks: Vec<String>,
    /// `Some` once the child has been reaped; the inner option is the exit code.
    done: Option<Option<i32>>,
}

impl ToolRun {
    /// Starts the readers and the waiter for a child the caller has already
    /// spawned and wired (stdin fed, pipes selected). The child is moved to the
    /// waiter, so the UI thread never blocks on it.
    pub(super) fn start(
        mut child: Child,
        frame: HWND,
        capture: bool,
        deliver: Deliver,
        encoding: i32,
        target: u64,
    ) -> Self {
        let pid = child.id();
        let queue = Arc::new(Mutex::new(Queue::default()));
        let posted = Arc::new(AtomicBool::new(false));
        let addr = frame.0 as usize;
        let mut readers = Vec::new();
        if capture {
            if let Some(out) = child.stdout.take() {
                readers.push(reader(out, queue.clone(), posted.clone(), addr, encoding));
            }
            if let Some(err) = child.stderr.take() {
                readers.push(reader(err, queue.clone(), posted.clone(), addr, encoding));
            }
        }
        let waiter_queue = queue.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            for r in readers {
                let _ = r.join();
            }
            waiter_queue.lock().unwrap().done = Some(code);
            wake(addr);
        });
        Self {
            pid,
            deliver,
            target,
            all: String::new(),
            queue,
            posted,
            frame: addr,
        }
    }

    /// Takes what the workers have queued. The flag handshake is what keeps a
    /// chunk that lands between the take and the flag clear from being lost:
    /// the producer either posted (because it saw `false`) or the re-check
    /// below posts for it.
    pub(super) fn take(&self) -> (Vec<String>, Option<Option<i32>>) {
        let (chunks, done) = {
            let mut q = self.queue.lock().unwrap();
            (std::mem::take(&mut q.chunks), q.done)
        };
        self.posted.store(false, Ordering::Release);
        let leftover = !self.queue.lock().unwrap().chunks.is_empty();
        if leftover && !self.posted.swap(true, Ordering::AcqRel) {
            wake(self.frame);
        }
        (chunks, done)
    }
}

fn reader<R: Read + Send + 'static>(
    mut pipe: R,
    queue: Arc<Mutex<Queue>>,
    posted: Arc<AtomicBool>,
    frame: usize,
    encoding: i32,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut decoder = Decoder::new(encoding);
        let mut buf = [0u8; 8192];
        loop {
            let n = match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let text = decoder.push(&buf[..n]);
            if text.is_empty() {
                continue;
            }
            queue.lock().unwrap().chunks.push(text);
            if !posted.swap(true, Ordering::AcqRel) {
                wake(frame);
            }
        }
    })
}

/// A pipe's bytes in the tool's own `Encoding` key (0 = the system ANSI code
/// page)
/// in the middle of a multi-byte character, so the tail waits for the next
/// read instead of turning into U+FFFD.
struct Decoder {
    cp: u32,
    pending: Vec<u8>,
}

impl Decoder {
    fn new(encoding: i32) -> Self {
        Self {
            cp: if encoding <= 0 {
                CP_ACP
            } else {
                encoding as u32
            },
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut take = self.pending.len();
        let mut trimmed = 0;
        while take > 0 {
            if let Some(text) = decode(self.cp, &self.pending[..take], MB_ERR_INVALID_CHARS) {
                self.pending.drain(..take);
                return text;
            }
            // A character is at most four bytes, so three trims cover an
            // incomplete tail; past that the bytes are bad.
            if trimmed == 3 {
                break;
            }
            trimmed += 1;
            take -= 1;
        }
        if take == 0 {
            // Less than a character so far: wait for the next read. If the
            // buffer grows past any possible character it falls through below.
            if self.pending.len() <= 3 {
                return String::new();
            }
        }
        let text = decode(self.cp, &self.pending, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0))
            .unwrap_or_default();
        self.pending.clear();
        text
    }
}

fn decode(cp: u32, bytes: &[u8], flags: MULTI_BYTE_TO_WIDE_CHAR_FLAGS) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    let need = unsafe { MultiByteToWideChar(cp, flags, bytes, None) };
    if need <= 0 {
        return None;
    }
    let mut wide = vec![0u16; need as usize];
    let got = unsafe { MultiByteToWideChar(cp, flags, bytes, Some(&mut wide)) };
    if got <= 0 {
        return None;
    }
    wide.truncate(got as usize);
    Some(String::from_utf16_lossy(&wide))
}

fn wake(frame: usize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(frame as *mut c_void)),
            WM_TOOL,
            Default::default(),
            Default::default(),
        );
    }
}

/// 终结: ends the process the run holds. A pid that has already gone is a
/// no-op — `OpenProcess` fails, or the handle is a zombie that
/// `TerminateProcess` refuses.
pub(super) fn terminate(pid: u32) {
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GBK character split across two reads waits for its second byte — the
    /// reason the decoder carries a tail instead of decoding each read alone.
    #[test]
    fn a_gbk_character_split_across_reads_comes_out_whole() {
        let mut d = Decoder::new(936);
        // 中 = D6 D0, 文 = CE C4 in GBK.
        assert_eq!(d.push(&[0xD6]), "");
        assert_eq!(d.push(&[0xD0, 0xCE]), "中");
        assert_eq!(d.push(&[0xC4]), "文");
    }

    #[test]
    fn a_utf8_character_split_across_reads_comes_out_whole() {
        let mut d = Decoder::new(65001);
        // 中 = E4 B8 AD.
        assert_eq!(d.push(&[0xE4, 0xB8]), "");
        assert_eq!(d.push(&[0xAD]), "中");
    }

    /// Invalid bytes must not wedge the tail buffer: the fallback decodes with
    /// replacements, clears, and the next valid read still decodes.
    #[test]
    fn invalid_bytes_fall_back_instead_of_stalling() {
        let mut d = Decoder::new(65001);
        let bad = d.push(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(bad.chars().count(), 4);
        assert_eq!(d.push(b"ok"), "ok");
    }

    /// Encoding 0 is the system ANSI code page
    /// 输出编码 default means.
    #[test]
    fn encoding_zero_reads_the_ansi_code_page() {
        assert_eq!(Decoder::new(0).cp, CP_ACP);
        assert_eq!(Decoder::new(65001).cp, 65001);
    }
}
