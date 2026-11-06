//! Windows 代码页转换缝（`MultiByteToWideChar` / `WideCharToMultiByte`）。
//!
//! `encoding_rs` 复现不了的那批代码页 —— UTF-7、EBCDIC、ISCII、DOS、HZ、
//! Mac 页 —— 只能问系统。这三个函数是 `dg-core` 里**唯一**依赖 `windows`
//! crate 的模块，也是唯一的编解码 Win32 入口；`encoding.rs` 因此保持平台
//! 中立（探测、BOM、`encoding_rs` 重编码）。`process.rs` 另有一处
//! `#[cfg(windows)]`，但只用 `std::os::windows`，与本缝无关。
//!
//! 为什么留在 `dg-core` 而不搬去 `dg-app`：`dg-lsp`、`dg-render`、`dg-ui`
//! 都依赖 `dg-core` 而不依赖 `dg-app`，`filesearch` / `document` / `linedoc`
//! 在非 GUI 路径上也要转码。搬走会制造一条反向依赖。
//!
//! `dg_core::encoding::{decode_codepage, encode_codepage, codepage_available}`
//! 仍由 `encoding.rs` 原样 `pub use`，既有调用点不用改。

/// The Win32 conversion pair drives `MultiByteToWideChar` (11 call
/// sites) and `WideCharToMultiByte` (6) with the document's codepage
/// id, flags 0 (every call site passes zero), no default char, no
/// `MB_ERR_INVALID_CHARS`. This covers the codepages `encoding_rs`
/// cannot reproduce: UTF-7, the EBCDIC, ISCII, DOS, HZ and Mac pages.
/// The two UTF-32 pages used to ride this pair too, but current
/// Windows's kernel32 answers 0 for them — `codepage_available` is the
/// guard that keeps those picks from wiping a buffer.
/// `decode_codepage` runs the two-call pattern (size, then fill) and
/// loses nothing for the pages Windows knows; on other platforms the
/// seam falls back to lossy UTF-8, the shape `filesearch::decode_text`
/// has always had.
#[cfg(windows)]
pub fn decode_codepage(cp: u16, bytes: &[u8]) -> String {
    use windows::Win32::Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    if bytes.is_empty() {
        return String::new();
    }
    // Size query, then the fill — the API's own two-step, since this
    // implementation does not know the width yet.
    unsafe {
        let len = MultiByteToWideChar(
            cp as u32,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            None,
        );
        if len <= 0 {
            return String::new();
        }
        let mut units = vec![0u16; len as usize];
        let got = MultiByteToWideChar(
            cp as u32,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            Some(&mut units),
        );
        if got <= 0 {
            return String::new();
        }
        units.truncate(got as usize);
        String::from_utf16_lossy(&units)
    }
}

/// Non-Windows fallback: the codepage tables do not exist, so the
/// bytes survive as lossy UTF-8 (the pre-seam behavior).
#[cfg(not(windows))]
pub fn decode_codepage(_cp: u16, bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Encodes `text` (Rust UTF-8) into Windows codepage `cp`, unmappable
/// characters falling back to the API's own default char — flags 0,
/// `lpDefaultChar = NULL`, exactly the save path's call shape.
#[cfg(windows)]
pub fn encode_codepage(cp: u16, text: &str) -> Vec<u8> {
    use windows::Win32::Globalization::WideCharToMultiByte;
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return Vec::new();
    }
    unsafe {
        let len = WideCharToMultiByte(cp as u32, 0, &units, None, None, None);
        if len <= 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; len as usize];
        let got = WideCharToMultiByte(cp as u32, 0, &units, Some(&mut out), None, None);
        if got <= 0 {
            return Vec::new();
        }
        out.truncate(got as usize);
        out
    }
}

/// Non-Windows fallback: lossy UTF-8 bytes (nothing else is available).
#[cfg(not(windows))]
pub fn encode_codepage(_cp: u16, text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Whether Windows can actually convert with this codepage — the guard
/// the conversion applies before rebuilding the buffer (the 98-entry
/// list is static; a table entry the OS lacks must not produce an
/// empty document). Measured on this machine: `0` (ACP) converts
/// without being a "valid codepage" in `IsValidCodePage`'s sense.
#[cfg(windows)]
pub fn codepage_available(cp: u16) -> bool {
    if cp == 0 {
        return true;
    }
    unsafe { windows::Win32::Globalization::IsValidCodePage(cp as u32).as_bool() }
}

#[cfg(not(windows))]
pub fn codepage_available(_cp: u16) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn win32_pair_roundtrips_the_seam_pages() {
        // GBK: 中文.
        let gbk = vec![0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode_codepage(936, &gbk), "\u{4e2d}\u{6587}");
        assert_eq!(encode_codepage(936, "\u{4e2d}\u{6587}"), gbk);
        // Big5.
        let big5 = vec![0xA4, 0xA4, 0xA4, 0xE5];
        assert_eq!(decode_codepage(950, &big5), "\u{4e2d}\u{6587}");
        // Current Windows's kernel32 answers 0 for both — the recorded
        // behavior (empty) is what `codepage_available`'s guard sees
        // before any buffer is rebuilt.
        let u32le: Vec<u8> = [0x4e2du32, 0x6587u32]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode_codepage(12000, &u32le), "");
        assert_eq!(encode_codepage(12000, "\u{4e2d}\u{6587}"), Vec::<u8>::new());
        // Unmappable chars fall back to the API default ('?'), flags 0.
        assert_eq!(encode_codepage(20127, "\u{4e2d}\u{6587}"), b"??");
        // Availability: 936 is a real page; the UTF-32 pair answers
        // false on current Windows (the conversion API dropped them),
        // so the guard sends them to the seam.
        assert!(codepage_available(936));
        assert!(codepage_available(0));
        assert!(!codepage_available(12000));
    }

    #[cfg(not(windows))]
    #[test]
    fn fallback_keeps_utf8_bytes() {
        assert_eq!(decode_codepage(936, b"hi"), "hi");
        assert_eq!(encode_codepage(936, "hi"), b"hi");
        assert!(!codepage_available(0));
    }

    /// 旧导入路径必须还能用 —— `document` / `linedoc` / `filesearch` 都从
    /// `encoding::` 取这三个函数。
    #[test]
    fn the_encoding_reexport_stays_put() {
        assert_eq!(
            crate::encoding::decode_codepage(936, b"hi"),
            decode_codepage(936, b"hi")
        );
        assert_eq!(
            crate::encoding::encode_codepage(936, "hi"),
            encode_codepage(936, "hi")
        );
        assert_eq!(
            crate::encoding::codepage_available(0),
            codepage_available(0)
        );
    }
}
