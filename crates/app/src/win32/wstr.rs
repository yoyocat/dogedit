//! UTF-16 string primitives shared by the Win32 dialogs and panes.
//!
//! Win32 text APIs want NUL-terminated UTF-16 in memory; a `DLGTEMPLATEEX`
//! wants those same units written straight into a byte buffer in little-endian
//! order. Keeping both conversions here means every caller gets the same
//! terminator and the same byte order.

/// A NUL-terminated UTF-16 copy of `s`, for `PCWSTR` and `CreateWindowExW`.
pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Append `s` as NUL-terminated UTF-16LE bytes to a `DLGTEMPLATEEX` buffer.
pub(super) fn push_wstr(b: &mut Vec<u8>, s: &str) {
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b.extend_from_slice(&[0, 0]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminates_and_keeps_surrogate_pairs() {
        let v = wide("A\u{1F600}");
        assert_eq!(v, vec![b'A' as u16, 0xD83D, 0xDE00, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn push_wstr_matches_wide_bytes() {
        let mut b = Vec::new();
        push_wstr(&mut b, "Hi");
        assert_eq!(b, vec![b'H' as u8, 0, b'i' as u8, 0, 0, 0]);
    }
}
