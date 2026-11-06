//! The single `DLGTEMPLATEEX` byte writer for the dialogs the Win32
//! backend builds by hand. It used to be copied into seven dialogs plus
//! `optionsdlg`; the copies differed only in the header's `exStyle`, the
//! font weight and whether a control named its window class. Keeping one
//! writer makes those three inputs explicit parameters instead of
//! accidental divergence, and keeps the byte layout (DWORD alignment,
//! `0xFFFF + atom` classes, `DS_SETFONT` block, creation-data count) in
//! one place.
//!
//! This module is pure byte serialization: no window handles, no Win32
//! calls. Font *measurement* (`FontBlock::shell`) stays in `optionsdlg`.

use crate::win32::wstr::push_wstr;

/// A control's window class: a built-in ordinal or a named class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    Atom(u16),
    Name(&'static str),
}

/// One `DLGITEMTEMPLATEEX`, in template order (which is the tab order).
#[derive(Clone, Copy)]
pub(super) struct Item {
    pub(super) id: i32,
    pub(super) class: Class,
    pub(super) style: u32,
    pub(super) x: i16,
    pub(super) y: i16,
    pub(super) w: i16,
    pub(super) h: i16,
    pub(super) label: &'static str,
}

// The window-class ordinals a `DLGITEMTEMPLATEEX` uses for the standard
// controls; anything else names its window class as a string.
pub(super) const ATOM_BUTTON: u16 = 0x0080;
pub(super) const ATOM_EDIT: u16 = 0x0081;
pub(super) const ATOM_STATIC: u16 = 0x0082;
pub(super) const ATOM_LISTBOX: u16 = 0x0083;
pub(super) const ATOM_COMBOBOX: u16 = 0x0085;

/// The `DS_SETFONT` block. Dialog units are derived from this font's
/// metrics, so points / charset / face are part of the layout, not just
/// the appearance.
#[derive(Clone, Copy)]
pub(super) struct Font<'a> {
    pub(super) points: u16,
    pub(super) weight: u16,
    pub(super) italic: u8,
    pub(super) charset: u8,
    pub(super) face: &'a str,
}

impl Font<'static> {
    /// The literal `8 pt "Ms Shell Dlg"` of a dialog template, written
    /// with `DEFAULT_CHARSET`. `weight` 0 means "the face's own", 400 is
    /// the explicit normal weight some templates carry.
    pub(super) const fn legacy(weight: u16) -> Self {
        Self {
            points: 8,
            weight,
            italic: 0,
            charset: 1,
            face: "Ms Shell Dlg",
        }
    }
}

/// Serialize a `DLGTEMPLATEEX`. `ex_style` is the dialog's own extended
/// style (0 for most dialogs, `WS_EX_TOOLWINDOW`-ish 0x80 for the find
/// dialog); it is *not* the per-item `exStyle`, which is always 0 here.
pub(super) fn build(
    title: &str,
    style: u32,
    ex_style: u32,
    w: u16,
    h: u16,
    font: Font<'_>,
    items: &[Item],
) -> Vec<u8> {
    let mut b: Vec<u8> = Vec::with_capacity(2048);
    b.extend_from_slice(&1u16.to_le_bytes()); // dlgVer
    b.extend_from_slice(&0xFFFFu16.to_le_bytes()); // signature
    b.extend_from_slice(&0u32.to_le_bytes()); // helpID
    b.extend_from_slice(&ex_style.to_le_bytes());
    b.extend_from_slice(&style.to_le_bytes());
    b.extend_from_slice(&(items.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // x
    b.extend_from_slice(&0u16.to_le_bytes()); // y
    b.extend_from_slice(&w.to_le_bytes());
    b.extend_from_slice(&h.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // menu: none
    b.extend_from_slice(&0u16.to_le_bytes()); // class: #32770
    push_wstr(&mut b, title);
    // DS_SETFONT block.
    b.extend_from_slice(&font.points.to_le_bytes());
    b.extend_from_slice(&font.weight.to_le_bytes());
    b.push(font.italic);
    b.push(font.charset);
    push_wstr(&mut b, font.face);
    for it in items {
        // Each DLGITEMTEMPLATEEX starts on a DWORD boundary.
        while b.len() % 4 != 0 {
            b.push(0);
        }
        b.extend_from_slice(&0u32.to_le_bytes()); // helpID
        b.extend_from_slice(&0u32.to_le_bytes()); // exStyle
        b.extend_from_slice(&it.style.to_le_bytes());
        b.extend_from_slice(&(it.x as u16).to_le_bytes());
        b.extend_from_slice(&(it.y as u16).to_le_bytes());
        b.extend_from_slice(&(it.w as u16).to_le_bytes());
        b.extend_from_slice(&(it.h as u16).to_le_bytes());
        b.extend_from_slice(&(it.id as u32).to_le_bytes());
        match it.class {
            Class::Atom(a) => {
                b.extend_from_slice(&0xFFFFu16.to_le_bytes());
                b.extend_from_slice(&a.to_le_bytes());
            }
            Class::Name(n) => push_wstr(&mut b, n),
        }
        push_wstr(&mut b, it.label);
        b.extend_from_slice(&0u16.to_le_bytes()); // creation data count
    }
    b
}

/// [`build`] with the legacy font — the one every reimplemented dialog
/// uses. `weight` is the only part that varies between templates.
pub(super) fn build_legacy(
    title: &str,
    style: u32,
    ex_style: u32,
    w: u16,
    h: u16,
    weight: u16,
    items: &[Item],
) -> Vec<u8> {
    build(
        title,
        style,
        ex_style,
        w,
        h,
        Font::legacy(weight),
        items,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACE: &str = "Ms Shell Dlg";

    /// Header is 26 bytes, then menu + class (4), then the title WSTR.
    fn font_offset(title: &str) -> usize {
        30 + (title.encode_utf16().count() + 1) * 2
    }

    /// Font block is 6 bytes + the face WSTR; items are DWORD-aligned.
    fn item_offset(title: &str) -> usize {
        let face_end = font_offset(title) + 6 + (FACE.encode_utf16().count() + 1) * 2;
        (face_end + 3) & !3
    }

    /// Hand-written expectation, independent of `build`.
    #[test]
    fn header_and_one_item_are_byte_exact() {
        let items = [Item {
            id: 1,
            class: Class::Atom(ATOM_BUTTON),
            style: 0x5001_0001,
            x: 7,
            y: 9,
            w: 50,
            h: 14,
            label: "OK",
        }];
        let b = build_legacy("T", 0x80c8_00c0, 0, 100, 50, 0, &items);
        assert_eq!(&b[0..4], &[1, 0, 0xFF, 0xFF]); // dlgVer + signature
        assert_eq!(&b[4..8], &0u32.to_le_bytes()); // helpID
        assert_eq!(&b[8..12], &0u32.to_le_bytes()); // exStyle
        assert_eq!(&b[12..16], &0x80c8_00c0u32.to_le_bytes());
        assert_eq!(&b[16..18], &1u16.to_le_bytes()); // cdit
        assert_eq!(&b[18..26], &[0, 0, 0, 0, 100, 0, 50, 0]); // x,y,cx,cy
        assert_eq!(&b[26..30], &[0, 0, 0, 0]); // menu + class
        assert_eq!(&b[30..34], &[b'T', 0, 0, 0]); // title WSTR + NUL
        let f = font_offset("T");
        assert_eq!(f, 34);
        // 8 pt, weight 0, no italic, DEFAULT_CHARSET.
        assert_eq!(&b[f..f + 6], &[8, 0, 0, 0, 0, 1]);
        let item = item_offset("T");
        assert_eq!(item, 68);
        assert_eq!(&b[item..item + 4], &0u32.to_le_bytes()); // helpID
        assert_eq!(&b[item + 4..item + 8], &0u32.to_le_bytes()); // exStyle
        assert_eq!(&b[item + 8..item + 12], &0x5001_0001u32.to_le_bytes());
        assert_eq!(&b[item + 12..item + 14], &7u16.to_le_bytes()); // x
        assert_eq!(&b[item + 20..item + 24], &1u32.to_le_bytes()); // id
        assert_eq!(&b[item + 24..item + 26], &0xFFFFu16.to_le_bytes());
        assert_eq!(&b[item + 26..item + 28], &ATOM_BUTTON.to_le_bytes());
        assert_eq!(&b[item + 28..item + 34], &[b'O', 0, b'K', 0, 0, 0]);
    }

    #[test]
    fn ex_style_is_a_distinct_header_field() {
        let a = build_legacy("T", 0, 0, 10, 10, 0, &[]);
        let b = build_legacy("T", 0, 0x80, 10, 10, 0, &[]);
        assert_eq!(a.len(), b.len());
        assert_eq!(&a[8..12], &0u32.to_le_bytes());
        assert_eq!(&b[8..12], &0x80u32.to_le_bytes());
        // Nothing else may move.
        assert_eq!(&a[0..8], &b[0..8]);
        assert_eq!(&a[12..], &b[12..]);
    }

    #[test]
    fn named_class_and_weight_are_serialized() {
        let items = [Item {
            id: 7,
            class: Class::Name("SysTabControl32"),
            style: 0,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            label: "",
        }];
        let b = build_legacy("X", 0, 0, 1, 1, 400, &items);
        let f = font_offset("X");
        assert_eq!(&b[f + 2..f + 4], &400u16.to_le_bytes());
        let item = item_offset("X");
        assert_eq!(&b[item + 20..item + 24], &7u32.to_le_bytes()); // id
        // A named class is a WSTR: no 0xFFFF atom marker.
        let name: Vec<u16> = "SysTabControl32".encode_utf16().collect();
        let start = item + 24;
        for (i, u) in name.iter().enumerate() {
            assert_eq!(&b[start + i * 2..start + i * 2 + 2], &u.to_le_bytes());
        }
    }
}
