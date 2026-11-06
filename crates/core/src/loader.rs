use crossbeam_channel::Sender;
use encoding_rs::{Encoding, UTF_8};
use crate::encoding::detect_encoding;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{spawn, JoinHandle};

pub enum LoadEvent {
    Progress { bytes_done: u64, bytes_total: u64 },
    /// The whole file as original-encoding bytes plus the detected
    /// encoding (and its BOM length, 0 when bare) so the document
    /// splits raw-domain and save re-encodes. The loader never
    /// decodes — the store decodes per line on demand.
    Done(Arc<Vec<u8>>, &'static Encoding, usize),
    Failed(String),
}

/// Background file loader
/// thread. While the thread is alive the
/// document reports every line as empty; progress is streamed to
/// the GUI thread as events.
pub struct ProgressiveLoader {
    pub path: PathBuf,
    pub chunk_bytes: usize,
    /// When set, the file is decoded with this encoding instead of the
    /// detector's answer — what a reload through a chosen encoding is.
    pub encoding: Option<&'static Encoding>,
    /// Called once after the terminal event is in the channel. A GUI that
    /// polls the channel on a timer would otherwise sit out the timer's
    /// whole interval before it learns the bytes are there; this is how it
    /// gets told instead.
    pub on_done: Option<Box<dyn FnOnce() + Send>>,
}

impl ProgressiveLoader {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            chunk_bytes: 256 * 1024,
            encoding: None,
            on_done: None,
        }
    }

    /// Loads the bytes as `enc`, taking the BOM length only when the
    /// file's own header is that encoding's BOM.
    pub fn with_encoding(mut self, enc: &'static Encoding) -> Self {
        self.encoding = Some(enc);
        self
    }

    /// Runs `f` on the loader thread once the file has been read.
    pub fn on_done(mut self, f: impl FnOnce() + Send + 'static) -> Self {
        self.on_done = Some(Box::new(f));
        self
    }

    /// Spawns the loader. `loading_flag` is the document's shared
    /// flag; `tx` delivers events to the GUI thread.
    pub fn spawn(
        self,
        loading_flag: Arc<std::sync::atomic::AtomicBool>,
        tx: Sender<LoadEvent>,
    ) -> JoinHandle<()> {
        spawn(move || {
            loading_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            let result = load_file(&self.path, self.chunk_bytes, &tx, self.encoding);
            loading_flag.store(false, std::sync::atomic::Ordering::SeqCst);
            match result {
                Ok((raw, enc, bom_len)) => {
                    let _ = tx.send(LoadEvent::Done(raw, enc, bom_len));
                }
                Err(e) => {
                    let _ = tx.send(LoadEvent::Failed(e));
                }
            }
            if let Some(f) = self.on_done {
                f();
            }
        })
    }
}

fn load_file(
    path: &std::path::Path,
    chunk_bytes: usize,
    tx: &Sender<LoadEvent>,
    forced: Option<&'static Encoding>,
) -> Result<(Arc<Vec<u8>>, &'static Encoding, usize), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let total = file.metadata().map_err(|e| e.to_string())?.len();
    let mut raw = Vec::new();
    let mut chunk = vec![0u8; chunk_bytes];
    loop {
        let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        let _ = tx.send(LoadEvent::Progress {
            bytes_done: raw.len() as u64,
            bytes_total: total,
        });
    }
    // Encoding detection only (BOM, UTF-16 heuristics, strict UTF-8
    // validation, then the chardetng probe; the shared rule is
    // `dg_large::encoding::detect`) — the order the
    // codepage switch follows (1200/1201 UTF-16, 12000/12001
    // UTF-8, else ANSI). `used` is the
    // BOM length the store skips. A forced encoding skips the
    // detection and only keeps a header that is that encoding's own BOM.
    let (enc, used) = match forced {
        Some(enc) => {
            let used = Encoding::for_bom(&raw)
                .filter(|(bom_enc, _)| *bom_enc == enc)
                .map_or(0, |(_, len)| len);
            (enc, used)
        }
        None => detect_encoding(&raw),
    };
    Ok((Arc::new(raw), enc, used))
}

/// Convenience: decode bytes synchronously with BOM sniffing.
pub fn decode_bytes(bytes: &[u8]) -> String {
    let (enc, used) = Encoding::for_bom(bytes).unwrap_or((UTF_8, 0));
    let (text, _, _) = enc.decode(&bytes[used..]);
    text.into_owned()
}
