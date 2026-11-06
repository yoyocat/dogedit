//! dg-hex tests: piece-chain edits, marker undo/redo, digit composition,
//! row formatting, auto-fit layout and the save/load round-trip.

use dg_hex::snapshot::Snapshot;
use dg_hex::view::{CharResult, HexView, MODE_INSERT, MODE_OVERWRITE};

// ---- snapshot ----------------------------------------------------------

#[test]
fn snapshot_set_data_read_roundtrip() {
    let mut s = Snapshot::new();
    s.set_data(&(0u8..=255).collect::<Vec<_>>());
    assert_eq!(s.len(), 256);
    assert!(!s.from_file());
    let mut buf = [0u8; 300];
    assert_eq!(s.read(0, &mut buf), 256);
    assert_eq!(&buf[..256], &(0u8..=255).collect::<Vec<_>>()[..]);
    assert_eq!(s.byte(0), Some(0));
    assert_eq!(s.byte(255), Some(255));
    assert_eq!(s.byte(256), None);
}

#[test]
fn snapshot_insert_middle_undo_redo() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    assert!(s.insert(4, b"ab", 1));
    assert_eq!(s.to_vec(), b"0123ab456789");
    assert_eq!(s.len(), 12);
    // Undo: one insert marker.
    // Insert undo: kind 1 undoing selects no group length.
    let (base, top) = s.undo().unwrap();
    assert_eq!((base, top), (4, 4));
    assert_eq!(s.to_vec(), b"0123456789");
    // Redo.
    s.redo().unwrap();
    assert_eq!(s.to_vec(), b"0123ab456789");
}

#[test]
fn snapshot_insert_at_end_and_start() {
    let mut s = Snapshot::new();
    s.set_data(b"mid");
    assert!(s.insert(0, b"H", 1));
    assert!(s.insert(4, b"!", 1));
    assert_eq!(s.to_vec(), b"Hmid!");
}

#[test]
fn snapshot_delete_undo() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    assert!(s.delete(3, 4)); // removes "3456"
    assert_eq!(s.to_vec(), b"012789");
    // Delete undo: kind 2 undoing keeps the deleted span as group length.
    let (base, top) = s.undo().unwrap();
    assert_eq!((base, top), (3, 7));
    assert_eq!(s.to_vec(), b"0123456789");
}

#[test]
fn snapshot_delete_crossing_pieces() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    // Each edit in its own counter group, the way the view's keystroke
    // path does it; raw markers all share counter 0 and TransferGroup
    // would undo them in one step.
    s.begin_group();
    assert!(s.insert(5, b"XY", 1)); // 01234XY56789
    s.end_group();
    s.begin_group();
    assert!(s.insert(0, b"Z", 1)); // Z01234XY56789
    s.end_group();
    assert!(s.delete(2, 8)); // removes 1234XY56
    assert_eq!(s.to_vec(), b"Z0789");
    s.undo().unwrap();
    assert_eq!(s.to_vec(), b"Z01234XY56789");
    s.undo().unwrap();
    assert_eq!(s.to_vec(), b"01234XY56789");
    s.undo().unwrap();
    assert_eq!(s.to_vec(), b"0123456789");
    assert!(!s.can_undo());
}

#[test]
fn snapshot_replace_keeps_length() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    assert!(s.replace(4, b"AB", 2));
    assert_eq!(s.len(), 10);
    assert_eq!(s.to_vec(), b"0123AB6789");
    // Two markers (region delete + insert), one counter: one undo step.
    let (base, top) = s.undo().unwrap();
    assert_eq!((base, top), (4, 4));
    assert_eq!(s.to_vec(), b"0123456789");
    s.redo().unwrap();
    assert_eq!(s.to_vec(), b"0123AB6789");
}

#[test]
fn snapshot_replace_growth_when_limit_smaller() {
    let mut s = Snapshot::new();
    s.set_data(b"abcdef");
    // Region of 1 byte replaced by 3 bytes: total grows by 2.
    assert!(s.replace(2, b"XYZ", 1));
    assert_eq!(s.to_vec(), b"abXYZdef");
}

#[test]
fn snapshot_replace_at_end_appends() {
    // An empty snapshot takes its first byte: nothing to delete, one to
    // insert.
    let mut s = Snapshot::new();
    assert!(s.replace(0, &[0x40], 1));
    assert_eq!(s.to_vec(), vec![0x40]);
    assert_eq!(s.len(), 1);
    assert!(s.undo().is_some());
    assert_eq!(s.len(), 0);
    // Same at the last byte of a loaded one: the overwrite runs past the
    // end instead of failing.
    s.set_data(&[0x61, 0x62]);
    assert!(s.replace(2, &[0x43], 1));
    assert_eq!(s.to_vec(), b"abC");
    // Past the end is still out of range.
    assert!(!s.replace(4, &[0x44], 1));
}

#[test]
fn snapshot_two_replaces_are_two_undo_steps() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    s.replace(0, &[0xAA], 1);
    s.replace(1, &[0xBB], 1);
    assert_eq!(&s.to_vec()[..2], &[0xAA, 0xBB]);
    s.undo().unwrap(); // second byte back
    assert_eq!(&s.to_vec()[..2], &[0xAA, 0x31]);
    s.undo().unwrap(); // first byte back
    assert_eq!(s.to_vec(), b"0123456789");
}

#[test]
fn snapshot_edit_clears_redo() {
    let mut s = Snapshot::new();
    s.set_data(b"0123456789");
    s.replace(0, &[0xAA], 1);
    s.undo().unwrap();
    assert!(s.can_redo());
    s.replace(5, &[0xBB], 1); // new edit kills the redo stack
    assert!(!s.can_redo());
    assert_eq!(s.to_vec()[0], b'0');
}

#[test]
fn snapshot_modified_flags_follow_edit_page() {
    let mut s = Snapshot::new();
    s.set_data(&[0x10, 0x20, 0x30, 0x40]);
    assert!(!s.byte_is_modified(1));
    s.replace(1, &[0x99], 1);
    assert!(s.byte_is_modified(1));
    assert!(!s.byte_is_modified(0));
    s.undo().unwrap();
    assert!(!s.byte_is_modified(1));
}

#[test]
fn snapshot_save_load_roundtrip_with_edits() {
    let mut s = Snapshot::new();
    let dir = std::env::temp_dir();
    let path = dir.join("evhex_test_round.bin");
    std::fs::write(&path, b"0123456789").unwrap();
    assert!(s.load_file(&path, false, false));
    assert!(s.from_file());
    s.replace(2, &[b'Z'], 1);
    s.insert(10, b"!", 1);
    assert!(!s.from_file());
    assert!(s.save_file(None));
    assert_eq!(std::fs::read(&path).unwrap(), b"01Z3456789!");
    // After save the snapshot reloaded: clean, no undo.
    assert!(s.from_file());
    assert!(!s.can_undo());
    assert_eq!(s.to_vec(), b"01Z3456789!");
    let _ = std::fs::remove_file(&path);
}

// ---- view: typing ------------------------------------------------------

fn view_with(data: &[u8]) -> HexView {
    let mut v = HexView::new();
    v.snap.set_data(data);
    v.on_size(80 * 8, 25 * 16);
    v
}

#[test]
fn view_hex_two_digit_composition() {
    let mut v = view_with(&[0x00, 0x11, 0x22]);
    v.edit_mode = MODE_OVERWRITE;
    assert_eq!(v.on_char('a' as u32), CharResult::Committed);
    // First digit replaces the high nibble; caret not advanced yet.
    assert_eq!(v.snap.byte(0), Some(0xa0));
    assert_eq!(v.caret, 0);
    assert_eq!(v.typed, 1);
    assert_eq!(v.on_char('b' as u32), CharResult::Updated);
    // Second digit lands in the same byte (in-place), then the caret moves.
    assert_eq!(v.snap.byte(0), Some(0xab));
    assert_eq!(v.caret, 1);
    assert_eq!(v.typed, 0);
    assert_eq!(v.snap.len(), 3); // overwrite never grows
}

#[test]
fn view_hex_uppercase_digits() {
    let mut v = view_with(&[0x00]);
    v.on_char('F' as u32);
    v.on_char('E' as u32);
    assert_eq!(v.snap.byte(0), Some(0xfe));
}

#[test]
fn view_invalid_char_rejected() {
    let mut v = view_with(&[0x00]);
    assert_eq!(v.on_char('g' as u32), CharResult::Rejected);
    assert_eq!(v.on_char(' ' as u32), CharResult::Rejected);
}

#[test]
fn view_decimal_composition_255() {
    let mut v = view_with(&[0x00]);
    v.flags = 1; // decimal, 3 digits
    v.on_char('2' as u32);
    assert_eq!(v.snap.byte(0), Some(200));
    v.on_char('5' as u32);
    assert_eq!(v.snap.byte(0), Some(250));
    v.on_char('5' as u32);
    assert_eq!(v.snap.byte(0), Some(255));
    assert_eq!(v.caret, 1);
}

#[test]
fn view_octal_composition() {
    let mut v = view_with(&[0x00]);
    v.flags = 2;
    v.on_char('7' as u32);
    // 7 in the 64s place overflows a byte; the composed value is stored
    // as its low byte (0o700 & 0xff = 0o300).
    assert_eq!(v.snap.byte(0), Some(0o300));
    v.on_char('0' as u32);
    v.on_char('1' as u32);
    assert_eq!(v.snap.byte(0), Some(0o301));
    assert_eq!(v.caret, 1);
}

#[test]
fn view_binary_composition() {
    let mut v = view_with(&[0xff]);
    v.flags = 3;
    for ch in "10110010".chars() {
        v.on_char(ch as u32);
    }
    assert_eq!(v.snap.byte(0), Some(0b1011_0010));
    assert_eq!(v.caret, 1);
}

#[test]
fn view_readonly_rejects() {
    let mut v = view_with(&[0x00]);
    v.edit_mode = 0;
    assert_eq!(v.on_char('a' as u32), CharResult::ReadOnly);
    assert_eq!(v.snap.byte(0), Some(0x00));
}

#[test]
fn view_insert_mode_grows() {
    let mut v = view_with(&[0x10, 0x20]);
    v.edit_mode = MODE_INSERT;
    v.on_char('a' as u32);
    v.on_char('b' as u32);
    assert_eq!(v.snap.to_vec(), vec![0xab, 0x10, 0x20]);
    assert_eq!(v.snap.len(), 3);
    assert_eq!(v.caret, 1);
}

#[test]
fn view_typing_starts_an_empty_document() {
    let mut v = view_with(&[]);
    assert_eq!(v.total_rows(), 0);
    // Overwrite mode has no byte to overwrite: the composed byte is
    // appended, and the second digit rewrites it in place.
    assert_eq!(v.on_char('4' as u32), CharResult::Committed);
    assert_eq!(v.snap.to_vec(), vec![0x40]);
    assert_eq!(v.on_char('1' as u32), CharResult::Updated);
    assert_eq!(v.snap.to_vec(), vec![0x41]);
    assert_eq!(v.caret, 1);
    // The appended byte is content now: one row to paint and scroll.
    assert_eq!(v.total_rows(), 1);
    assert!(v.undo());
    assert_eq!(v.snap.len(), 0);
}

#[test]
fn view_ascii_typing_starts_an_empty_document() {
    let mut v = view_with(&[]);
    v.column = 1;
    assert_eq!(v.on_ascii_char(b'Z' as u32), CharResult::Committed);
    assert_eq!(v.snap.to_vec(), vec![b'Z']);
    assert_eq!(v.caret, 1);
}

#[test]
fn view_ascii_column_char() {
    let mut v = view_with(&[0x41, 0x00, 0xa0]);
    v.column = 1;
    assert_eq!(v.on_ascii_char('Z' as u32), CharResult::Committed);
    assert_eq!(v.snap.byte(0), Some(b'Z'));
    assert_eq!(v.caret, 1);
    // Insert mode with a selection: the selection drops out in the same
    // undo group as the inserted byte.
    let mut v2 = view_with(&[1, 2, 3, 4]);
    v2.edit_mode = MODE_INSERT;
    v2.anchor_a = 1;
    v2.anchor_b = 3;
    v2.caret = 1;
    v2.column = 1;
    v2.on_ascii_char(b'X' as u32);
    assert_eq!(v2.snap.to_vec(), vec![1, b'X', 4]);
    assert!(v2.undo());
    assert_eq!(v2.snap.to_vec(), vec![1, 2, 3, 4]);
}

#[test]
fn view_typing_undo_step_per_byte() {
    let mut v = view_with(&[0x00, 0x00, 0x00]);
    for (hi, lo) in [('1' as u32, '1' as u32), ('2' as u32, '2' as u32), ('3' as u32, '3' as u32)] {
        v.on_char(hi);
        v.on_char(lo);
    }
    assert_eq!(v.snap.to_vec(), vec![0x11, 0x22, 0x33]);
    assert!(v.undo());
    assert_eq!(v.snap.to_vec(), vec![0x11, 0x22, 0x00]);
    assert!(v.undo());
    assert_eq!(v.snap.to_vec(), vec![0x11, 0x00, 0x00]);
    assert!(v.undo());
    assert_eq!(v.snap.to_vec(), vec![0x00, 0x00, 0x00]);
    assert!(!v.snap.can_undo());
    assert!(v.redo());
    assert_eq!(v.snap.to_vec(), vec![0x11, 0x00, 0x00]);
}

// ---- view: formatting --------------------------------------------------

#[test]
fn format_address_variants() {
    let v = view_with(&[]);
    assert_eq!(v.format_address(0x10), " 00000010");
    let mut v2 = view_with(&[]);
    v2.address_base = 0x1000;
    assert_eq!(v2.format_address(0x10), " 00001010");
    let mut v3 = view_with(&[]);
    v3.flags |= 1 << 0x10; // upper
    assert_eq!(v3.format_address(0xab), " 000000AB");
    let mut v4 = view_with(&[]);
    v4.flags |= 1 << 2; // decimal
    assert_eq!(v4.format_address(16), " 00000016");
    let mut v5 = view_with(&[]);
    v5.flags |= 1 << 5; // segmented
    assert_eq!(v5.format_address(0x12345), " 0001:2345");
    let mut v6 = view_with(&[]);
    v6.flags |= 1 << 4; // trailing colon
    assert_eq!(v6.format_address(0), " 00000000:");
    let mut v7 = view_with(&[]);
    v7.flags |= 1 << 6; // hidden
    assert_eq!(v7.format_address(0), "");
}

#[test]
fn format_byte_variants() {
    let v = view_with(&[]);
    assert_eq!(v.format_byte(0xab), "ab");
    let mut v2 = view_with(&[]);
    v2.flags = 1;
    assert_eq!(v2.format_byte(255), "255");
    assert_eq!(v2.format_byte(5), "005");
    v2.flags = 2;
    assert_eq!(v2.format_byte(0o377), "377");
    v2.flags = 3;
    assert_eq!(v2.format_byte(0b1010_1010), "10101010");
    v2.flags = 1 << 0x10;
    assert_eq!(v2.format_byte(0xab), "AB");
}

#[test]
fn format_row_layout_default() {
    let data: Vec<u8> = (0x30..0x40).collect();
    let v = view_with(&data);
    let row = v.format_row(0, &data, &vec![false; 16]);
    let text: String = row.text.iter().collect();
    // address(9, the format's leading space included) + pad(3) + 16*2
    // digits + 7 group seps + 3 + 16 ascii.
    assert_eq!(text.len(), 70);
    assert_eq!(&text[..12], " 00000000   ");
    assert_eq!(&text[12..51], "3031 3233 3435 3637 3839 3a3b 3c3d 3e3f");
    assert_eq!(&text[51..54], "   ");
    assert_eq!(&text[54..70], "0123456789:;<=>?");
    assert_eq!(row.runs.len(), row.text.len());
    assert_eq!(v.chars_per_row, 71);
}

#[test]
fn format_row_tail_row_pads_hex_area() {
    let data = vec![0x41, 0x42];
    let v = view_with(&data);
    let row = v.format_row(0, &data, &vec![false, false]);
    let text: String = row.text.iter().collect();
    // Hex area still occupies hex_cols columns, ascii row padded to bpr.
    assert_eq!(text.len(), 70);
    assert_eq!(&text[12..18], "4142  ");
    assert_eq!(&text[47..51], "    ");
    assert_eq!(&text[54..70], "AB              ");
}

#[test]
fn format_row_ascii_substitution() {
    let data = vec![0x00, 0x41, 0xa0];
    let v = view_with(&data);
    let row = v.format_row(0, &data, &vec![false; 3]);
    let text: String = row.text.iter().collect();
    assert!(text.ends_with(".A.             "));
    // Raw flags keep the characters.
    let mut v2 = view_with(&data);
    v2.flags |= 1 << 0xb | 1 << 0xc;
    let row2 = v2.format_row(0, &data, &vec![false; 3]);
    let text2: String = row2.text.iter().collect();
    assert!(text2.ends_with("\0A\u{a0}             "));
}

#[test]
fn format_row_modified_colors() {
    let mut v = view_with(&[0x00, 0x11]);
    // Group 1 so byte 1 resolves through colour entry 7 (`(col / group)`
    // is taken at its low bit — the division QUOTIENT, so whole groups
    // alternate); the constructor default 2 would put byte 1 in
    // entry 4, whose default happens to equal `modified_fg`.
    v.set_group_size(1);
    v.snap.replace(0, &[0xff], 1);
    let modified = v.modified_flags(0, 2);
    assert_eq!(modified, vec![true, false]);
    let row = v.format_row(0, &[0xff, 0x11], &modified);
    let plain = v.resolve_colors(1, false, false);
    let modp = v.resolve_colors(0, false, true);
    assert_ne!(modp, plain);
    assert_eq!(row.runs[12], modp); // first hex digit of byte 0
    assert_eq!(row.runs[15], plain); // first hex digit of byte 1
}

#[test]
fn address_field_holds_the_leading_space() {
    let v = view_with(&[0u8; 1]);
    assert_eq!(v.address_field(), 9);
    // field 9 + gap 3 + hex 39 + gap 3 + ascii 16 + the trailing cell.
    assert_eq!(v.chars_per_row, 71);
    let mut colon = view_with(&[0u8; 1]);
    colon.flags |= dg_hex::view::F_ADDRESS_COLON;
    assert_eq!(colon.address_field(), 10);
    let mut hidden = view_with(&[0u8; 1]);
    hidden.flags |= dg_hex::view::F_HIDE_ADDRESS;
    assert_eq!(hidden.address_field(), 0);
}

#[test]
fn separators_ride_the_column_gaps() {
    let mut v = view_with(&[0u8; 16]); // bpr 16, group 2 → hex_cols 39
    v.char_width = 10;
    let (x1, x2) = v.separator_pixels();
    // The first rule is centred in the address gap's middle cell; the
    // second sits half the ASCII gap past the hex area.
    assert_eq!(x1, 9 * 10 + 3 * 10 / 2 + 10 / 2);
    assert_eq!(x2, (9 + 3 + 39) * 10 + 3 * 10 / 2);
    // Both follow the horizontal scroll.
    v.hscroll = 2;
    assert_eq!(v.separator_pixels(), (x1 - 20, x2 - 20));
}

// ---- view: layout ------------------------------------------------------

#[test]
fn set_group_size_hex_cols() {
    let mut v = HexView::new(); // bpr 16, group 2, hex
    v.set_group_size(2);
    // cell 2*2 + 1 = 5 columns, 16/2 = 8 cells per row, minus the last
    // separator: 39.
    assert_eq!(v.hex_cols, 39);
    v.set_group_size(4);
    // cell 2*4 + 1 = 9 columns, 16/4 = 4 cells per row, minus the last
    // separator: 35.
    assert_eq!(v.hex_cols, 35);
    v.set_group_size(3);
    // cell 2*3 + 1 = 7 columns, 16/3 = 5 whole cells minus the last
    // separator = 34; the leftover byte adds 1*2 + 1 = 3, so 37.
    assert_eq!(v.hex_cols, 37);
    assert_eq!(v.bytes_per_row, 16);
}

#[test]
fn on_size_auto_fit() {
    let mut v = HexView::new();
    v.char_width = 8;
    v.flags = 1 << 0x11; // auto-fit
    v.on_size(1000, 800);
    // usable = 125 - 9(addr, the leading space included) - 3(sep_ascii)
    // - 3(sep_addr) = 110; digits2 = 3, so a cell is 3*2 + 1 = 7 columns:
    // bpr = 110*2/7 = 31 → aligned 30.
    assert_eq!(v.bytes_per_row, 30);
    assert!(v.chars_per_row > 0);
    // Narrow window.
    v.on_size(300, 800);
    // usable = 37-9-3 = 25; bpr = 25*2/7 = 7 → aligned 6.
    assert_eq!(v.bytes_per_row, 6);
}

#[test]
fn on_size_group_aligned_flag() {
    let mut v = HexView::new();
    v.char_width = 8;
    v.flags = 1 << 0x11 | 1 << 7;
    v.on_size(300, 800);
    // Same 7 as above, aligned down to a group multiple.
    assert_eq!(v.bytes_per_row % v.group, 0);
}

#[test]
fn total_rows_counts_the_tail_row() {
    let v = view_with(&[0u8; 32]); // bpr 16
    assert_eq!(v.total_rows(), 2);
    let v2 = view_with(&[0u8; 17]);
    assert_eq!(v2.total_rows(), 2); // partial tail row counts
    let v3 = view_with(&[]);
    assert_eq!(v3.total_rows(), 0);
    let v4 = view_with(&[0u8; 1]);
    assert_eq!(v4.total_rows(), 1); // shorter than one row still shows it
    let v5 = view_with(&[0u8; 256]);
    assert_eq!(v5.total_rows(), 16);
}

// ---- view: caret / scrolling -------------------------------------------

#[test]
fn scroll_to_caret_pages() {
    let mut v = view_with(&[0u8; 1024]); // 64 rows at bpr 16
    v.visible_rows = 10;
    v.caret = 20 * 16; // row 20
    v.scroll_to_caret();
    assert_eq!(v.top_row, 11); // row - visible + 1
    v.caret = 0;
    v.scroll_to_caret();
    assert_eq!(v.top_row, 0);
}

#[test]
fn set_caret_resets_anchors() {
    let mut v = view_with(&[0u8; 100]);
    v.anchor_a = 5;
    v.anchor_b = 9;
    v.set_caret(42);
    assert_eq!((v.caret, v.anchor_a, v.anchor_b), (42, 42, 42));
    assert_eq!(v.typed, 0);
}

#[test]
fn caret_pos_hex_and_ascii() {
    let mut v = view_with(&[0u8; 64]);
    v.caret = 5;
    let (col, row) = v.caret_pos();
    // full_groups=2, a cell is 2*2 + 1 = 5 columns → 10, in_group=1 adds
    // 2 more, nibble 0.
    assert_eq!((col, row), (12, 0));
    v.typed = 1;
    assert_eq!(v.caret_pos().0, 13); // second digit
    v.typed = 0;
    v.column = 1;
    assert_eq!(v.caret_pos().0, 5); // ascii column: byte index
    v.caret = 18;
    assert_eq!(v.caret_pos(), (2, 1));
}

#[test]
fn caret_width_by_mode() {
    let mut v = view_with(&[0]);
    v.char_width = 9;
    v.edit_mode = MODE_OVERWRITE;
    assert_eq!(v.caret_width(), 9);
    v.edit_mode = MODE_INSERT;
    assert_eq!(v.caret_width(), 2);
}

#[test]
fn movement_commands() {
    let mut v = view_with(&[0u8; 40]);
    v.set_caret(20);
    v.move_rows(-1);
    assert_eq!(v.caret, 4);
    v.move_caret(2);
    assert_eq!(v.caret, 6);
    v.move_home();
    assert_eq!(v.caret, 0);
    v.move_end();
    assert_eq!(v.caret, 16);
    v.set_caret(38);
    v.move_rows(1); // row 3 start 48 > len 40 → clamp to 40
    assert_eq!(v.caret, 40);
    v.move_caret(-100);
    assert_eq!(v.caret, 0);
}

// ---- stress: interleaved edits + undo/redo --------------------------------

/// Deterministic LCG so failures replay.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn stress_edit_undo_redo_chain_stays_consistent() {
    let mut s = Snapshot::new();
    s.set_data(&(0u8..64).collect::<Vec<_>>());
    let mut reference: Vec<u8> = (0..64).collect();
    let mut rng = Rng(0xC0FFEE);
    // One state per applied edit group; undo pops it, redo restores it.
    let mut undo_states: Vec<Vec<u8>> = Vec::new();
    let mut redo_states: Vec<Vec<u8>> = Vec::new();
    for _ in 0..800 {
        let choice = rng.next() % 5;
        if choice == 3 && !undo_states.is_empty() {
            redo_states.push(reference.clone());
            assert!(s.undo().is_some());
            reference = undo_states.pop().unwrap();
        } else if choice == 4 && !redo_states.is_empty() {
            undo_states.push(reference.clone());
            assert!(s.redo().is_some());
            reference = redo_states.pop().unwrap();
        } else {
            let pos = (rng.next() % (s.len() as u64 + 1)) as u32;
            let n = (1 + rng.next() % 7) as u32;
            redo_states.clear();
            undo_states.push(reference.clone());
            if choice == 0 {
                let data: Vec<u8> = (0..n).map(|_| (rng.next() & 0xff) as u8).collect();
                let p = pos.min(s.len());
                s.begin_group();
                assert!(s.insert(p, &data, 1));
                s.end_group();
                let at = p as usize;
                reference.splice(at..at, data.iter().copied());
            } else if s.len() > 0 {
                let len = n.min(s.len());
                let p = pos.min(s.len() - len);
                if choice == 1 {
                    s.begin_group();
                    assert!(s.delete(p, len));
                    s.end_group();
                    reference.drain(p as usize..(p + len) as usize);
                } else {
                    let data: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
                    assert!(s.replace(p, &data, len));
                    reference[p as usize..(p + len) as usize].copy_from_slice(&data);
                }
            } else {
                undo_states.pop();
                continue;
            }
        }
        assert_eq!(s.to_vec(), reference, "chain diverged (len {})", s.len());
        assert_eq!(s.len(), reference.len() as u32);
    }
}
