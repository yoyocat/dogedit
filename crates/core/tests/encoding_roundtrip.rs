use crossbeam_channel::unbounded;
use encoding_rs::{Encoding, UTF_16LE, UTF_8};
use dg_core::encoding::encode_text;
use dg_core::loader::{LoadEvent, ProgressiveLoader};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn temp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "dg_core_enc_test_{}_{}",
        std::process::id(),
        name
    ));
    p
}

/// Loads `path` on the loader thread and decodes it through the
/// raw-domain store (`Document::from_bytes`): the loader hands back
/// original-encoding bytes and the document decodes per line on
/// demand — this exercises the lazy-decode path end to end.
fn load(path: &Path) -> (String, &'static Encoding, bool) {
    let (tx, rx) = unbounded();
    let flag = Arc::new(AtomicBool::new(false));
    let handle =
        ProgressiveLoader::new(path.to_path_buf())
            .spawn(flag, tx);
    let mut text = String::new();
    let mut enc = UTF_8;
    let mut bom = false;
    while let Ok(ev) = rx.recv() {
        match ev {
            LoadEvent::Progress { .. } => {}
            LoadEvent::Done(raw, e, b) => {
                let doc = dg_core::Document::from_bytes(raw, e, b);
                text = doc.to_text();
                enc = e;
                bom = b > 0;
            }
            LoadEvent::Failed(e) => {
                panic!("load failed: {e}")
            }
        }
    }
    handle.join().unwrap();
    (text, enc, bom)
}

#[test]
fn utf16le_file_roundtrips() {
    let path = temp_path("utf16le.txt");
    let text = "Hello DogEdit \u{4e16}\u{754c}\nSecond line\n";
    let original: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(
            text.encode_utf16()
                .flat_map(|u| u.to_le_bytes()),
        )
        .collect();
    std::fs::write(&path, &original).unwrap();
    let (loaded, enc, bom) = load(&path);
    assert_eq!(loaded, text);
    assert_eq!(enc, UTF_16LE);
    assert!(bom);
    // save re-encodes in the source encoding
    let saved = encode_text(&loaded, enc, bom);
    assert_eq!(saved, original);
    std::fs::remove_file(&path).ok();
}

#[test]
fn utf8_bom_file_roundtrips() {
    let path = temp_path("utf8bom.txt");
    let text = "Hello \u{4e16}\u{754c}\n";
    let original: Vec<u8> = [0xEF, 0xBB, 0xBF]
        .into_iter()
        .chain(text.bytes())
        .collect();
    std::fs::write(&path, &original).unwrap();
    let (loaded, enc, bom) = load(&path);
    assert_eq!(loaded, text);
    assert_eq!(enc, UTF_8);
    assert!(bom);
    let saved = encode_text(&loaded, enc, bom);
    assert_eq!(saved, original);
    std::fs::remove_file(&path).ok();
}

#[test]
fn utf8_plain_file_roundtrips() {
    let path = temp_path("utf8.txt");
    let text = "plain ascii and \u{4e16}\u{754c}\n";
    let original: Vec<u8> = text.bytes().collect();
    std::fs::write(&path, &original).unwrap();
    let (loaded, enc, bom) = load(&path);
    assert_eq!(loaded, text);
    assert_eq!(enc, UTF_8);
    assert!(!bom);
    let saved = encode_text(&loaded, enc, bom);
    assert_eq!(saved, original);
    std::fs::remove_file(&path).ok();
}

#[test]
fn gbk_file_is_probed_and_saves_back() {
    // The chardetng probe is what
    // makes a BOM-less GBK file load as text
    // instead of replacement characters, and
    // save re-encodes in the detected
    // encoding.
    let path = temp_path("gbk.txt");
    let text = "你好世界，这是一个测试。编码探测需要足够长的文本才能可靠判断，多来一些汉字吧。中华人民共和国\n";
    let original: Vec<u8> = [
        0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7, 0xA3, 0xAC,
        0xD5, 0xE2, 0xCA, 0xC7, 0xD2, 0xBB, 0xB8, 0xF6, 0xB2, 0xE2,
        0xCA, 0xD4, 0xA1, 0xA3, 0xB1, 0xE0, 0xC2, 0xEB, 0xCC, 0xBD,
        0xB2, 0xE2, 0xD0, 0xE8, 0xD2, 0xAA, 0xD7, 0xE3, 0xB9, 0xBB,
        0xB3, 0xA4, 0xB5, 0xC4, 0xCE, 0xC4, 0xB1, 0xBE, 0xB2, 0xC5,
        0xC4, 0xDC, 0xBF, 0xC9, 0xBF, 0xBF, 0xC5, 0xD0, 0xB6, 0xCF,
        0xA3, 0xAC, 0xB6, 0xE0, 0xC0, 0xB4, 0xD2, 0xBB, 0xD0, 0xA9,
        0xBA, 0xBA, 0xD7, 0xD6, 0xB0, 0xC9, 0xA1, 0xA3, 0xD6, 0xD0,
        0xBB, 0xAA, 0xC8, 0xCB, 0xC3, 0xF1, 0xB9, 0xB2, 0xBA, 0xCD,
        0xB9, 0xFA, 0x0A,
    ]
    .to_vec();
    std::fs::write(&path, &original).unwrap();
    let (loaded, enc, bom) = load(&path);
    assert_eq!(loaded, text);
    assert_eq!(enc.name(), "GBK");
    assert!(!bom);
    let saved = encode_text(&loaded, enc, bom);
    assert_eq!(saved, original);
    std::fs::remove_file(&path).ok();
}

/// [`load`] with an encoding the detector must not second-guess.
fn load_with(path: &Path, forced: &'static Encoding) -> (String, &'static Encoding, usize) {
    let (tx, rx) = unbounded();
    let flag = Arc::new(AtomicBool::new(false));
    let handle =
        ProgressiveLoader::new(path.to_path_buf())
            .with_encoding(forced)
            .spawn(flag, tx);
    let mut text = String::new();
    let mut enc = UTF_8;
    let mut bom_len = 0;
    while let Ok(ev) = rx.recv() {
        match ev {
            LoadEvent::Progress { .. } => {}
            LoadEvent::Done(raw, e, b) => {
                let doc = dg_core::Document::from_bytes(raw, e, b);
                text = doc.to_text();
                enc = e;
                bom_len = b;
            }
            LoadEvent::Failed(e) => panic!("load failed: {e}"),
        }
    }
    handle.join().unwrap();
    (text, enc, bom_len)
}

/// 指定编码加载: the forced page wins over the probe for a BOM-less file,
/// and only a header that belongs to that page is skipped.
#[test]
fn a_forced_encoding_overrides_the_probe() {
    let path = temp_path("forced_gbk_as_utf8.txt");
    let gbk: Vec<u8> = vec![0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7, 0x0A];
    std::fs::write(&path, &gbk).unwrap();
    let (text, enc, bom_len) = load_with(&path, UTF_8);
    assert_eq!(enc, UTF_8);
    assert_eq!(bom_len, 0);
    // GBK bytes are not valid UTF-8, so the forced decode replaces them.
    assert!(text.contains('\u{FFFD}'), "decoded: {text:?}");
    std::fs::remove_file(&path).ok();
}

/// A UTF-8 BOM is skipped only when UTF-8 is the forced page.
#[test]
fn a_forced_encoding_keeps_only_its_own_bom() {
    let path = temp_path("forced_bom.txt");
    let mut original: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    original.extend_from_slice(b"hello\n");
    std::fs::write(&path, &original).unwrap();
    let (text, enc, bom_len) = load_with(&path, UTF_8);
    assert_eq!(enc, UTF_8);
    assert_eq!(bom_len, 3);
    assert_eq!(text, "hello\n");
    // Forced to GBK, the same header is ordinary text bytes.
    let (text, enc, bom_len) = load_with(&path, encoding_rs::GBK);
    assert_eq!(enc, encoding_rs::GBK);
    assert_eq!(bom_len, 0);
    assert_ne!(text, "hello\n");
    std::fs::remove_file(&path).ok();
}
