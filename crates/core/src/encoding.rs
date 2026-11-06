//! Encoding detection and
//! re-encoding. The detector is `chardetng`, a Rust port of the
//! universal character-detection algorithm, so no detection library
//! is linked in.

use encoding_rs::Encoding;
#[cfg(test)]
use encoding_rs::{UTF_16BE, UTF_16LE, UTF_8};

/// Heuristic encoding detection.
/// The decision rule lives in `dg_large::encoding::detect` — the
/// leaf of the `dg-core -> dg-large` dependency edge — so that the
/// normal document loader and the >50 MB prefix probe cannot drift.
/// Order: BOM, UTF-16 byte-pattern
/// heuristics, strict UTF-8
/// validation, then the chardetng
/// probe. Returns the encoding and
/// the BOM length to skip (0 when
/// absent).
pub fn detect_encoding(
    bytes: &[u8],
) -> (&'static Encoding, usize) {
    dg_large::encoding::detect(bytes)
}

/// Encodes `text` with `enc`,
/// prefixing the BOM when the
/// source carried one (save
/// re-encodes in the detected
/// encoding).
pub fn encode_text(
    text: &str,
    enc: &'static Encoding,
    bom: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        if let Some(b) = bom_bytes(enc) {
            out.extend_from_slice(b);
        }
    }
    match enc.name() {
        // `Encoding::encode` outputs
        // UTF-8 for the UTF-16
        // encodings (see
        // `output_encoding`), so
        // UTF-16 is encoded here
        // directly.
        "UTF-16LE" => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(
                    &unit.to_le_bytes(),
                );
            }
        }
        "UTF-16BE" => {
            for unit in text.encode_utf16() {
                out.extend_from_slice(
                    &unit.to_be_bytes(),
                );
            }
        }
        _ => {
            let (cow, _, _) = enc.encode(text);
            out.extend_from_slice(&cow);
        }
    }
    out
}

/// BOM bytes for the common
/// encodings (encoding_rs has
/// no accessor for them).
pub(crate) fn bom_bytes(
    enc: &'static Encoding,
) -> Option<&'static [u8]> {
    match enc.name() {
        "UTF-8" => Some(&[0xEF, 0xBB, 0xBF]),
        "UTF-16LE" => Some(&[0xFF, 0xFE]),
        "UTF-16BE" => Some(&[0xFE, 0xFF]),
        _ => None,
    }
}

/// The Win32 conversion seam (`MultiByteToWideChar` /
/// `WideCharToMultiByte`) lives in `codepage.rs`; these three
/// functions are re-exported here so the existing
/// `dg_core::encoding::{...}` call sites in `document`,
/// `linedoc` and `filesearch` stay untouched.
pub use crate::codepage::{codepage_available, decode_codepage, encode_codepage};

/// The BOM a codepage document carries when its BOM flag is set: the
/// two UTF-32 pages (the same two bytes `bom_bytes` has for the
/// UTF-16 pair) and UTF-8's own.
pub fn codepage_bom(cp: u16) -> Option<&'static [u8]> {
    match cp {
        65001 => Some(&[0xEF, 0xBB, 0xBF]),
        12000 => Some(&[0xFF, 0xFE, 0x00, 0x00]),
        12001 => Some(&[0x00, 0x00, 0xFE, 0xFF]),
        _ => None,
    }
}

/// The raw-domain walk stride for a codepage document: the two UTF-32
/// pages step 4-byte units, every other page walks bytes.
pub fn codepage_stride(cp: u16) -> usize {
    if matches!(cp, 12000 | 12001) { 4 } else { 1 }
}

/// Whether a codepage's raw units are big-endian (12001).
pub fn codepage_be(cp: u16) -> bool {
    cp == 12001
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_wins() {
        let utf8_bom: Vec<u8> =
            [0xEF, 0xBB, 0xBF]
                .into_iter()
                .chain(b"hi".iter().copied())
                .collect();
        let (enc, used) =
            detect_encoding(&utf8_bom);
        assert_eq!(enc, UTF_8);
        assert_eq!(used, 3);
        let utf16: Vec<u8> =
            [0xFF, 0xFE]
                .into_iter()
                .chain("hi".encode_utf16().flat_map(|u| u.to_le_bytes()))
                .collect();
        let (enc, used) = detect_encoding(&utf16);
        assert_eq!(enc, UTF_16LE);
        assert_eq!(used, 2);
    }

    #[test]
    fn valid_utf8_detected() {
        let (enc, used) =
            detect_encoding("hello \u{4e16}\u{754c}".as_bytes());
        assert_eq!(enc, UTF_8);
        assert_eq!(used, 0);
    }

    #[test]
    fn utf16_le_without_bom() {
        let bytes: Vec<u8> =
            "Abc".encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect();
        let (enc, used) =
            detect_encoding(&bytes);
        assert_eq!(enc, UTF_16LE);
        assert_eq!(used, 0);
    }

    #[test]
    fn utf16_be_without_bom() {
        let bytes: Vec<u8> =
            "Abc".encode_utf16()
                .flat_map(|u| u.to_be_bytes())
                .collect();
        let (enc, used) =
            detect_encoding(&bytes);
        assert_eq!(enc, UTF_16BE);
        assert_eq!(used, 0);
    }

    #[test]
    fn detection_matches_the_large_file_rule() {
        // 普通文档与大文件必须走同一条判定链；这些输入里包含两版
        // UTF-16 阈值曾经给出不同答案的边界样本。
        let cases: Vec<Vec<u8>> = vec![
            b"hello".to_vec(),
            vec![0xEF, 0xBB, 0xBF, b'h', b'i'],
            "Abc".encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect(),
            "Abc".encode_utf16()
                .flat_map(|u| u.to_be_bytes())
                .collect(),
            vec![0x41, 0x00, 0x42, 0x00, 0x43, 0x42, 0x44, 0x42, 0x45, 0x42],
        ];
        for bytes in cases {
            assert_eq!(
                detect_encoding(&bytes),
                dg_large::encoding::detect(&bytes)
            );
        }
    }

    #[test]
    fn gbk_bytes_detected_by_the_probe() {
        // GBK "你好世界，这是一个测试。…": not
        // valid UTF-8 and not UTF-16, so
        // the chardetng probe
        // answers — its GB18030/HZ-GB-2312
        // guess is 936, which is GBK. A statistical
        // detector needs a real paragraph, not four
        // bytes.
        let gbk: &[u8] = &[
            0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0,
            0xBD, 0xE7, 0xA3, 0xAC, 0xD5, 0xE2,
            0xCA, 0xC7, 0xD2, 0xBB, 0xB8, 0xF6,
            0xB2, 0xE2, 0xCA, 0xD4, 0xA1, 0xA3,
            0xB1, 0xE0, 0xC2, 0xEB, 0xCC, 0xBD,
            0xB2, 0xE2, 0xD0, 0xE8, 0xD2, 0xAA,
            0xD7, 0xE3, 0xB9, 0xBB, 0xB3, 0xA4,
            0xB5, 0xC4, 0xCE, 0xC4, 0xB1, 0xBE,
            0xB2, 0xC5, 0xC4, 0xDC, 0xBF, 0xC9,
            0xBF, 0xBF, 0xC5, 0xD0, 0xB6, 0xCF,
            0xA3, 0xAC, 0xB6, 0xE0, 0xC0, 0xB4,
            0xD2, 0xBB, 0xD0, 0xA9, 0xBA, 0xBA,
            0xD7, 0xD6, 0xB0, 0xC9, 0xA1, 0xA3,
            0xD6, 0xD0, 0xBB, 0xAA, 0xC8, 0xCB,
            0xC3, 0xF1, 0xB9, 0xB2, 0xBA, 0xCD,
            0xB9, 0xFA,
        ];
        let (enc, used) = detect_encoding(gbk);
        assert_eq!(enc, encoding_rs::GBK);
        assert_eq!(used, 0);
        // and it round-trips back to the text
        let (text, _, _) = enc.decode(gbk);
        assert_eq!(
            &*text,
            "你好世界，这是一个测试。编码探测需要足够长的文本才能可靠判断，多来一些汉字吧。中华人民共和国"
        );
    }

    #[test]
    fn shift_jis_detected_by_the_probe() {
        // Shift_JIS "日本語のテキストです。…":
        // SHIFT_JIS to codepage 932.
        let sjis: &[u8] = &[
            0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA,
            0x82, 0xCC, 0x83, 0x65, 0x83, 0x4C,
            0x83, 0x58, 0x83, 0x67, 0x82, 0xC5,
            0x82, 0xB7, 0x81, 0x42, 0x82, 0xB1,
            0x82, 0xEA, 0x82, 0xCD, 0x83, 0x47,
            0x83, 0x93, 0x83, 0x52, 0x81, 0x5B,
            0x83, 0x66, 0x83, 0x42, 0x83, 0x93,
            0x83, 0x4F, 0x8C, 0x9F, 0x8F, 0x6F,
            0x82, 0xCC, 0x83, 0x65, 0x83, 0x58,
            0x83, 0x67, 0x82, 0xC5, 0x82, 0xB7,
            0x81, 0x42, 0x82, 0xE0, 0x82, 0xC1,
            0x82, 0xC6, 0x92, 0xB7, 0x82, 0xA2,
            0x95, 0xB6, 0x8F, 0xCD, 0x82, 0xAA,
            0x95, 0x4B, 0x97, 0x76, 0x82, 0xC5,
            0x82, 0xB7, 0x81, 0x42, 0x93, 0x8C,
            0x8B, 0x9E, 0x82, 0xCD, 0x93, 0xFA,
            0x96, 0x7B, 0x82, 0xCC, 0x8E, 0xF1,
            0x93, 0x73, 0x82, 0xC5, 0x82, 0xB7,
            0x81, 0x42,
        ];
        let (enc, _) = detect_encoding(sjis);
        assert_eq!(enc, encoding_rs::SHIFT_JIS);
        let (text, _, _) = enc.decode(sjis);
        assert_eq!(
            &*text,
            "日本語のテキストです。これはエンコーディング検出のテストです。もっと長い文章が必要です。東京は日本の首都です。"
        );
    }

    #[test]
    fn latin1_detected_by_the_probe() {
        // French text in Windows-1252: not
        // UTF-8, so the probe answers
        // WINDOWS_1252
        // table maps WINDOWS_1252 to
        // codepage 1252 and ISO-8859-1 to
        // 28591.
        let latin: &[u8] = &[
            0x43, 0x61, 0x66, 0xE9, 0x20, 0x63,
            0x72, 0xE8, 0x6D, 0x65, 0x2C, 0x20,
            0x73, 0x27, 0x69, 0x6C, 0x20, 0x76,
            0x6F, 0x75, 0x73, 0x20, 0x70, 0x6C,
            0x61, 0xEE, 0x74, 0x2E, 0x20, 0xC0,
            0x20, 0x62, 0x69, 0x65, 0x6E, 0x74,
            0xF4, 0x74, 0x20, 0x6C, 0x61, 0x20,
            0x66, 0x69, 0x6E, 0x2E, 0x20, 0xC7,
            0x61, 0x20, 0x76, 0x61, 0x20, 0x74,
            0x72, 0xE8, 0x73, 0x20, 0x62, 0x69,
            0x65, 0x6E, 0x20, 0x61, 0x75, 0x6A,
            0x6F, 0x75, 0x72, 0x64, 0x27, 0x68,
            0x75, 0x69, 0x2C, 0x20, 0x6E, 0x27,
            0x65, 0x73, 0x74, 0x2D, 0x63, 0x65,
            0x20, 0x70, 0x61, 0x73, 0x3F,
        ];
        let (enc, used) = detect_encoding(latin);
        assert_eq!(enc, encoding_rs::WINDOWS_1252);
        assert_eq!(used, 0);
    }

    #[test]
    fn encode_roundtrips() {
        let text = "hi \u{4e16}";
        let bytes = encode_text(text, UTF_8, false);
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            text
        );
        // with a BOM
        let bytes = encode_text(text, UTF_8, true);
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        // UTF-16LE with BOM: one
        // code unit per char
        let bytes =
            encode_text(text, UTF_16LE, true);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(
            bytes.len(),
            2 + text.encode_utf16().count() * 2
        );
        let (round, _, _) =
            UTF_16LE.decode(&bytes[2..]);
        assert_eq!(&*round, text);
    }
    #[test]
    fn codepage_helpers() {
        assert_eq!(codepage_stride(12000), 4);
        assert_eq!(codepage_stride(12001), 4);
        assert_eq!(codepage_stride(936), 1);
        assert!(codepage_be(12001));
        assert!(!codepage_be(12000));
        assert_eq!(codepage_bom(12000), Some(&[0xFF, 0xFE, 0x00, 0x00][..]));
        assert_eq!(codepage_bom(12001), Some(&[0x00, 0x00, 0xFE, 0xFF][..]));
        assert_eq!(codepage_bom(65001), Some(&[0xEF, 0xBB, 0xBF][..]));
        assert_eq!(codepage_bom(936), None);
    }

}
