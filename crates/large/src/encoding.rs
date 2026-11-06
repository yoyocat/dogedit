//! 编码检测（前缀检测只看前 64 KB —— UltraEdit 同款做法）。
//!
//! 这里是判定规则的唯一实现所在：dg-core 因为 `Document::Backend::Large`
//! 反向依赖 dg-large，不能让 dg-large 依赖 dg-core，所以把共享规则
//! 放在依赖链的叶子上。`dg_core::encoding::detect_encoding` 直接调用
//! [`detect`]，普通文档与大文件不可能再产生判定漂移。

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};

/// 检测用的前缀长度。
pub const DETECT_PREFIX: usize = 64 * 1024;

/// 检测结果。
pub struct EncInfo {
    pub enc: &'static Encoding,
    /// BOM 长度（内容区从文件头偏移这么多字节开始）。
    pub bom_len: u64,
}

pub fn detect_from_prefix(prefix: &[u8]) -> EncInfo {
    let (enc, bom_len) = detect(prefix);
    EncInfo { enc, bom_len: bom_len as u64 }
}

/// 唯一的编码判定规则：BOM → UTF-16 无 BOM 启发 → 严格 UTF-8 →
/// chardetng 探测。返回编码与要跳过的 BOM 长度（无 BOM 时为 0）。
///
/// `bytes` 由调用方决定范围：普通文档（dg-core）传整个文件，大文件
/// 传前 [`DETECT_PREFIX`] 字节。范围不同是有意的（大文件不能整体
/// 扫描），但判定次序与阈值必须完全一致。
pub fn detect(bytes: &[u8]) -> (&'static Encoding, usize) {
    if let Some((enc, used)) = Encoding::for_bom(bytes) {
        return (enc, used);
    }
    // UTF-16 启发必须排在严格 UTF-8 之前：纯 ASCII 的 UTF-16
    //（字节间夹 NUL）本身也是合法 UTF-8，会被后者抢走。
    if let Some(enc) = utf16_heuristic(bytes) {
        return (enc, 0);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, 0);
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    (detector.guess(None, Utf8Detection::Allow), 0)
}

/// UTF-16 无 BOM：偶长度且每隔一个字节是 NUL（ASCII 文本）。
/// 阈值取「至少两个 NUL 且交替位里半数以上是 NUL」这个较严的一侧：
/// 偶尔含零字节的二进制块不应被整篇误判成 UTF-16。
fn utf16_heuristic(bytes: &[u8]) -> Option<&'static Encoding> {
    if bytes.len() < 2 || bytes.len() % 2 != 0 {
        return None;
    }
    let (mut even_nuls, mut odd_nuls) = (0usize, 0usize);
    for (i, b) in bytes.iter().enumerate() {
        if *b == 0 {
            if i % 2 == 0 {
                even_nuls += 1;
            } else {
                odd_nuls += 1;
            }
        }
    }
    // 长度为偶数时，奇数位与偶数位各有 half 个。
    let half = bytes.len() / 2;
    if odd_nuls >= 2 && odd_nuls * 2 >= half {
        return Some(UTF_16LE);
    }
    if even_nuls >= 2 && even_nuls * 2 >= half {
        return Some(UTF_16BE);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_wins() {
        let bytes: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(b"hi".iter().copied()).collect();
        let e = detect_from_prefix(&bytes);
        assert_eq!(e.enc, UTF_8);
        assert_eq!(e.bom_len, 3);
        let bytes: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hi".encode_utf16().flat_map(|u| u.to_le_bytes()))
            .collect();
        let e = detect_from_prefix(&bytes);
        assert_eq!(e.enc, UTF_16LE);
        assert_eq!(e.bom_len, 2);
    }

    #[test]
    fn plain_utf8() {
        let e = detect_from_prefix("hello 世界".as_bytes());
        assert_eq!(e.enc, UTF_8);
        assert_eq!(e.bom_len, 0);
    }

    #[test]
    fn utf16_no_bom() {
        let bytes: Vec<u8> = "Abcdef".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect_from_prefix(&bytes).enc, UTF_16LE);
        let bytes: Vec<u8> = "Abcdef".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(detect_from_prefix(&bytes).enc, UTF_16BE);
    }

    #[test]
    fn utf16_threshold_is_conservative() {
        // 交替位只有 2/5、3/7 个 NUL：旧的大文件规则允许「半数减 2」，
        // 会把这种夹了几个零字节的普通数据整篇误判成 UTF-16。统一后的
        // 规则要求真正过半，这里应当落到严格 UTF-8。
        let le_like: &[u8] = &[0x41, 0x00, 0x42, 0x00, 0x43, 0x42, 0x44, 0x42, 0x45, 0x42];
        let be_like: &[u8] = &[
            0x00, 0x41, 0x00, 0x42, 0x00, 0x43, 0x41, 0x44,
            0x41, 0x45, 0x41, 0x46, 0x41, 0x47,
        ];
        for bytes in [le_like, be_like] {
            let (enc, bom) = detect(bytes);
            assert_eq!(enc, UTF_8);
            assert_eq!(bom, 0);
            let info = detect_from_prefix(bytes);
            assert_eq!(info.enc, enc);
            assert_eq!(info.bom_len, bom as u64);
        }
    }

    #[test]
    fn prefix_wrapper_shares_the_decision() {
        let cases: Vec<Vec<u8>> = vec![
            b"hello".to_vec(),
            "你好世界".as_bytes().to_vec(),
            vec![0xEF, 0xBB, 0xBF, b'h', b'i'],
            "Abcdef".encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
            "Abcdef".encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
            vec![0xC4, 0xE3, 0xBA, 0xC3], // 非法 UTF-8 → 交给 chardetng
        ];
        for bytes in cases {
            let (enc, bom) = detect(&bytes);
            let info = detect_from_prefix(&bytes);
            assert_eq!(info.enc, enc);
            assert_eq!(info.bom_len, bom as u64);
        }
    }
}
