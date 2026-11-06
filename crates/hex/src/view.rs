//! `HexView` — the platform-independent view state: flags, layout math,
//! address and byte formatting, row text and colour runs, keystroke
//! composition, caret and scroll rules. The Win32 shell in `dg-app` owns the
//! HWND, the GDI objects and the message loop.

use crate::snapshot::{Snapshot, KIND_DELETE};

/// Digits per byte per base, the digit table (the base mode is the low
/// two bits of the flag word).
pub const DIGITS: [u32; 4] = [2, 3, 3, 8];
/// Radix per base mode, the base table (16/10/8/2).
pub const BASES: [u32; 4] = [16, 10, 8, 2];

// Display flag bits.
pub const F_DECIMAL_ADDRESS: u32 = 1 << 2;
pub const F_SEGMENTED_ADDRESS: u32 = 1 << 5;
pub const F_HIDE_ADDRESS: u32 = 1 << 6;
pub const F_BPR_GROUP_ALIGNED: u32 = 1 << 7;
pub const F_ADDRESS_COLON: u32 = 1 << 4;
pub const F_UPPER_ADDRESS: u32 = 1 << 0x10;
pub const F_UPPER_HEX: u32 = 1 << 0x10; // one bit steers both address and digits
pub const F_HIDE_ASCII: u32 = 1 << 0xd;
pub const F_HIDE_HEX: u32 = 1 << 0xf;
pub const F_AUTO_FIT: u32 = 1 << 0x11;
pub const F_ALT_MOD_COLOR: u32 = 1 << 0x12;
pub const F_RAW_CONTROL: u32 = 1 << 0xb;
pub const F_RAW_HIGH: u32 = 1 << 0xc;
/// The thin rules inside the address and ASCII separators.
pub const F_SEPARATORS: u32 = 1 << 0x1e;

/// Edit modes.
pub const MODE_READONLY: u32 = 0;
pub const MODE_INSERT: u32 = 1;
pub const MODE_OVERWRITE: u32 = 2;

/// One bookmark / colour-range node of the overlay list:
/// `{start, len, fg, bg}`.
#[derive(Clone, Copy)]
pub struct ColorRange {
    pub start: u32,
    pub len: u32,
    pub fg: u32,
    pub bg: u32,
}

/// The colour table (24 entries) plus the separate fields the formatters
/// read. `0x8000_0000 | index` entries resolve through GetSysColor at draw
/// time; everything here stays raw and the shell resolves.
#[derive(Clone)]
pub struct Colors {
    pub table: [u32; 24],
    /// Address background.
    pub address_bg: u32,
    /// Modified-byte foreground.
    pub modified_fg: u32,
    /// Undo-group range background / foreground.
    pub undo_bg: u32,
    pub undo_fg: u32,
}

impl Default for Colors {
    /// The default colour table.
    fn default() -> Self {
        let mut table = [0u32; 24];
        table[0] = 0x8000_0005; // WINDOW
        table[1] = 0x8000_000d; // HIGHLIGHT
        table[2] = 0x8000_000d;
        table[3] = 0x8000_0008; // WINDOWTEXT
        table[4] = 0x00c4_0000;
        table[5] = 0x8000_000e; // 3DFACE
        table[6] = 0x8000_000e;
        table[7] = 0x0080_0000;
        table[8] = 0x8000_000e;
        table[9] = 0x8000_000e;
        table[10] = 0x8000_0008;
        table[11] = 0x8000_000e;
        table[12] = 0x8000_000e;
        table[13] = 0x0000_00ff;
        table[14] = 0x003c_3cc8;
        table[15] = 0x003c_32c8;
        table[16] = 0;
        table[17] = 0x8000_001b; // syscolor 27
        table[18] = 0x007f_ffff;
        table[19] = 0x8000_000f;
        table[20] = 0x00dd_dddd;
        table[21] = 0x8000_0008;
        table[22] = 0x0080_c8ff;
        table[23] = 0x003c_64c8;
        Self {
            table,
            address_bg: 0x00db_e2d8,
            modified_fg: 0x00c4_0000,
            undo_bg: 0x003c_64c8,
            undo_fg: 0x0080_c8ff,
        }
    }
}

/// One formatted row: the text plus a `(fg, bg)` run entry per character —
/// exactly what the row formatter hands the shell's ExtTextOutW sweep.
pub struct FormattedRow {
    pub text: Vec<char>,
    pub runs: Vec<(u32, u32)>,
}

/// What `on_char` did, so the shell can beep / repaint / notify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharResult {
    /// Not a digit for the active base (the caller rings a beep).
    Rejected,
    /// Read-only mode: composed, then dropped without a write.
    ReadOnly,
    /// First digit committed through replace/insert.
    Committed,
    /// A later digit updated the scratch byte in place.
    Updated,
}

/// The `HexView` state: the layout, caret and formatting inputs the
/// shell paints from.
pub struct HexView {
    pub snap: Snapshot, // the byte model
    pub flags: u32,     // display flags, F_* above
    pub caret: u32,     // byte offset of the caret
    /// Which area the caret sits in: 0 = hex digits, 1 = ASCII column.
    pub column: u32,
    /// Value added to every address before display.
    pub address_base: u32,
    /// The undo-group anchors, also drawn as the group range highlight.
    pub anchor_a: u32,
    pub anchor_b: u32,
    pub line_height: i32,  // tmHeight of the row font
    pub char_width: i32,   // tmAveCharWidth of the row font
    pub visible_rows: i32, // rows fitting the client height
    pub visible_cols: i32, // characters fitting the client width
    pub bytes_per_row: i32, // 16 at construction
    pub group: i32,        // 2 at construction
    pub top_row: i32,      // first visible row
    pub hscroll: i32,      // in characters
    pub addr_digits: i32,  // 8 at construction
    pub hex_cols: i32,     // hex-area width
    pub addr_digits2: i32, // 8 at construction
    pub sep_addr: i32,     // 3 at construction
    pub sep_ascii: i32,    // 3 at construction
    pub chars_per_row: i32, // whole-row width
    pub ranges: Vec<ColorRange>, // colour overlay list
    pub colors: Colors,
    /// 0=readonly 1=insert 2=overwrite.
    pub edit_mode: u32,
    /// digits already typed for the byte under composition.
    pub typed: i32,
}

impl Default for HexView {
    fn default() -> Self {
        Self::new()
    }
}

impl HexView {
    /// The field defaults set at construction.
    pub fn new() -> Self {
        let mut v = Self {
            snap: Snapshot::new(),
            flags: F_SEPARATORS,
            caret: 0,
            column: 0,
            address_base: 0,
            anchor_a: 0,
            anchor_b: 0,
            line_height: 16,
            char_width: 8,
            visible_rows: 0,
            visible_cols: 0,
            bytes_per_row: 16,
            group: 2,
            top_row: 0,
            hscroll: 0,
            addr_digits: 8,
            hex_cols: 0,
            addr_digits2: 8,
            sep_addr: 3,
            sep_ascii: 3,
            chars_per_row: 0,
            ranges: Vec::new(),
            colors: Colors::default(),
            edit_mode: MODE_OVERWRITE,
            typed: 0,
        };
        v.set_group_size(2);
        v
    }

    pub fn base_mode(&self) -> usize {
        (self.flags & 3) as usize
    }

    pub fn digits(&self) -> u32 {
        DIGITS[self.base_mode()]
    }

    pub fn base(&self) -> u32 {
        BASES[self.base_mode()]
    }

    // ---- layout ----------------------------------------------------------

    /// The address text's field width in characters: its digits, the one
    /// character the format's leading space takes, plus a cell each for the
    /// segmented separator and the trailing colon when those modes are on.
    /// Zero when the address column is hidden.
    pub fn address_field(&self) -> i32 {
        if self.flags & F_HIDE_ADDRESS != 0 {
            return 0;
        }
        let mut n = self.addr_digits;
        if self.flags & F_SEGMENTED_ADDRESS != 0 && self.flags & F_DECIMAL_ADDRESS == 0 {
            n += 1;
        }
        if self.flags & F_ADDRESS_COLON != 0 {
            n += 1;
        }
        n + 1
    }

    /// CharsPerRow: address + separator + hex area + separator
    /// + ASCII width + 1, with each area dropped when its hide flag is on.
    pub fn chars_per_row_calc(&self) -> i32 {
        let mut n: i32 = 0;
        if self.flags & F_HIDE_ADDRESS == 0 {
            n += self.address_field();
        }
        if self.flags & F_HIDE_HEX == 0 {
            n += self.sep_addr + self.hex_cols;
        }
        if self.flags & F_HIDE_ASCII == 0 {
            n += self.sep_ascii + self.bytes_per_row;
        }
        n + 1
    }

    /// SetGroupSize (1..=31; out of range ignores). Recomputes
    /// the hex-area column count and the row width; the shell repaints.
    pub fn set_group_size(&mut self, group: i32) {
        if !(1..=0x1f).contains(&group) {
            return;
        }
        let digits = self.digits() as i32;
        self.group = group;
        if self.flags & F_HIDE_HEX == 0 {
            // Hex width: a cell of digits*group bytes plus a separator,
            // times the whole-group count, minus the trailing separator;
            // a partial trailing group adds r*digits plus one separator.
            let bpr = self.bytes_per_row;
            let per_group = digits * group + 1;
            self.hex_cols = per_group * (bpr / group) - 1;
            let r = bpr % group;
            if r != 0 {
                self.hex_cols += r * digits + 1;
            }
        } else {
            self.hex_cols = 0;
        }
        self.chars_per_row = self.chars_per_row_calc();
    }

    /// OnSize body: `cx`/`cy` are the client size. Returns true when the
    /// layout changed and the caller should repaint and refresh
    /// scrollbars.
    pub fn on_size(&mut self, cx: i32, cy: i32) -> bool {
        let old_bpr = self.bytes_per_row;
        if self.flags & F_AUTO_FIT != 0 {
            let mut usable = cx / self.char_width.max(1);
            usable -= self.address_field();
            if self.flags & F_HIDE_HEX != 0 {
                // Text-only view: the ASCII separator column and one extra.
                usable -= self.sep_ascii + 1;
                self.bytes_per_row = usable;
            } else {
                // digits+1 per byte when the ASCII column follows, digits
                // when it does not.
                let digits2 = if self.flags & F_HIDE_ASCII != 0 {
                    self.digits() as i32
                } else {
                    usable -= self.sep_ascii;
                    self.digits() as i32 + 1
                };
                usable -= self.sep_addr;
                let group = self.group;
                self.bytes_per_row = (usable * group) / (digits2 * group + 1);
            }
            // Align down to the group (or 1), keeping at least the base.
            let align = if self.flags & F_BPR_GROUP_ALIGNED != 0 { self.group } else { 1 };
            let bpr = self.bytes_per_row;
            let aligned = bpr - bpr % self.group;
            self.bytes_per_row = if aligned > align { aligned } else { align };
            if self.bytes_per_row != old_bpr {
                self.hscroll = 0;
            }
        }
        self.set_group_size(self.group);
        // Visible rows, clamped to the rows the bytes occupy: a partial
        // tail row counts, an empty snapshot has none.
        let content_rows = self.snap.len().div_ceil(self.bytes_per_row.max(1) as u32);
        let vis = (cy.max(0) / self.line_height.max(1)) as u32;
        self.visible_rows = vis.min(content_rows) as i32;
        // Visible columns.
        self.visible_cols = (cx / self.char_width.max(1)).min(self.chars_per_row);
        let changed = self.bytes_per_row != old_bpr;
        if changed {
            self.clamp_top_row();
        }
        changed
    }

    /// Total rows the bytes occupy: one per full row plus the partial tail
    /// (a file shorter than one row still has its row); an empty snapshot
    /// has none.
    pub fn total_rows(&self) -> u32 {
        self.snap.len().div_ceil(self.bytes_per_row.max(1) as u32)
    }

    fn clamp_top_row(&mut self) {
        let max = self.total_rows().saturating_sub(self.visible_rows.max(0) as u32) as i32;
        self.top_row = self.top_row.clamp(0, max.max(0));
    }

    // ---- address / byte formatting ----------------------------------------

    /// FormatAddress: the row's address text, leading space
    /// included (the format strings all start with one).
    pub fn format_address(&self, offset: u32) -> String {
        if self.flags & F_HIDE_ADDRESS != 0 {
            return String::new();
        }
        let val = (offset as u64).wrapping_add(self.address_base as u64);
        let width = self.addr_digits2 as usize;
        let text = if self.flags & F_DECIMAL_ADDRESS != 0 {
            format!(" {:0width$}", val, width = width)
        } else if self.flags & F_SEGMENTED_ADDRESS != 0 {
            let hi = val >> 16;
            let lo = (val & 0xffff) as u32;
            let body = if self.flags & F_UPPER_ADDRESS != 0 {
                format!("{hi:0width$X}:{lo:04X}", width = width.saturating_sub(4))
            } else {
                format!("{hi:0width$x}:{lo:04x}", width = width.saturating_sub(4))
            };
            format!(" {body}")
        } else if self.flags & F_UPPER_ADDRESS != 0 {
            format!(" {:0width$X}", val, width = width)
        } else {
            format!(" {:0width$x}", val, width = width)
        };
        // Bit 4 appends a colon after the address.
        if self.flags & F_ADDRESS_COLON != 0 {
            format!("{text}:")
        } else {
            text
        }
    }

    /// FormatByte: the hex-area text for one byte.
    pub fn format_byte(&self, b: u8) -> String {
        match self.base_mode() {
            0 => {
                if self.flags & F_UPPER_HEX != 0 {
                    format!("{b:02X}")
                } else {
                    format!("{b:02x}")
                }
            }
            1 => format!("{b:03}"),
            2 => format!("{b:03o}"),
            _ => {
                // Eight chars, MSB first.
                let mut s = String::with_capacity(8);
                for i in (0..8).rev() {
                    s.push(if b & (1 << i) != 0 { '1' } else { '0' });
                }
                s
            }
        }
    }

    /// ASCII-column display char: control and (unless the
    /// raw flags are on) 0x80..0xA0 substitute as '.'.
    pub fn ascii_char(&self, b: u8) -> char {
        if self.flags & F_RAW_CONTROL == 0 && b < 0x20 {
            return '.';
        }
        if self.flags & F_RAW_HIGH == 0 && b.wrapping_add(0x80) <= 0x20 {
            return '.';
        }
        b as char
    }

    // ---- colours -----------------------------------------------------------

    /// The (fg, bg) pair for one byte. `in_ascii_column` says which loop
    /// asked (the hex loop passes false, the ASCII loop true). Entries
    /// with the high bit set resolve through GetSysColor; left raw for
    /// the shell.
    pub fn resolve_colors(&self, pos: u32, in_ascii_column: bool, modified: bool) -> (u32, u32) {
        let mut idx = if in_ascii_column {
            10
        } else {
            // Alternating byte colours: `pos % bpr` is divided by the
            // group and the low bit of that quotient picks the pair, so
            // whole groups alternate — with group 1 that is every byte.
            let col = pos % self.bytes_per_row.max(1) as u32;
            if (col / self.group.max(1) as u32) & 1 == 1 {
                7
            } else {
                4
            }
        };
        if modified {
            idx = if self.flags & F_ALT_MOD_COLOR != 0 { 13 } else { 7 };
        }
        // The flash variant (idx 24 / 23) only occurs from search hits,
        // driven by a per-row flag the host sets; not modelled here.
        if let Some(r) = self.range_at(pos) {
            return (r.fg, r.bg);
        }
        let fg = if modified { self.colors.modified_fg } else { self.colors.table[idx] };
        let bg = self.colors.table[0];
        let mut pair = (fg, bg);
        // The caret byte renders inverse in the column the caret is
        // NOT in.
        if (in_ascii_column as u32) != self.column && pos == self.caret {
            pair = (pair.1, pair.0);
        }
        // Bytes inside the anchor pair [lo, hi) paint with the
        // undo-group colours.
        let lo = self.anchor_a.min(self.anchor_b);
        let hi = self.anchor_a.max(self.anchor_b);
        if hi != 0 && lo != hi && pos >= lo && pos < hi {
            pair = (self.colors.undo_fg, self.colors.undo_bg);
        }
        pair
    }

    fn range_at(&self, pos: u32) -> Option<ColorRange> {
        self.ranges
            .iter()
            .copied()
            .find(|r| pos >= r.start && pos < r.start + r.len)
    }

    // ---- row assembly -------------------------------------------------------

    /// FormatRow for the row starting at `row_start` with
    /// `count` valid bytes (the tail row's count is smaller). `modified`
    /// marks bytes the snapshot sees as edited.
    pub fn format_row(&self, row_start: u32, bytes: &[u8], modified: &[bool]) -> FormattedRow {
        let mut text = Vec::new();
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let count = bytes.len();

        let addr = self.format_address(row_start);
        let addr_run = (self.colors.table[3], self.colors.address_bg);
        for ch in addr.chars() {
            text.push(ch);
            runs.push(addr_run);
        }
        // Pad the address field out to sep_addr columns (matters when the
        // address is hidden); separator carries the same run.
        let sep_run = (self.colors.table[3], self.colors.table[0]);
        let hex_start = self.address_field() + self.sep_addr;
        while (text.len() as i32) < hex_start {
            text.push(' ');
            runs.push(sep_run);
        }

        if self.flags & F_HIDE_HEX == 0 {
            let group = self.group.max(1) as usize;
            for i in 0..count {
                let digits = self.format_byte(bytes[i]);
                let colors = self.resolve_colors(row_start + i as u32, false, modified[i]);
                for ch in digits.chars() {
                    text.push(ch);
                    runs.push(colors);
                }
                // Group separator, same colours as the byte.
                if (i + 1) % group == 0 && i + 1 != count {
                    text.push(' ');
                    runs.push(colors);
                }
            }
            // Pad the hex area out to the hex-area width. The count
            // starts where the hex column starts, clamp so a hidden
            // address cannot underflow.
            let plain = (self.colors.table[10], self.colors.table[0]);
            let hex_start = hex_start.max(0) as usize;
            while text.len().saturating_sub(hex_start) < self.hex_cols as usize {
                text.push(' ');
                runs.push(plain);
            }
        }

        if self.flags & F_HIDE_ASCII == 0 {
            let plain = (self.colors.table[10], self.colors.table[0]);
            for _ in 0..self.sep_ascii {
                text.push(' ');
                runs.push(plain);
            }
            for (i, &b) in bytes.iter().enumerate() {
                let ch = self.ascii_char(b);
                let colors = self.resolve_colors(row_start + i as u32, true, modified[i]);
                text.push(ch);
                runs.push(colors);
            }
            for _ in count..self.bytes_per_row.max(0) as usize {
                text.push(' ');
                runs.push(plain);
            }
        }

        FormattedRow { text, runs }
    }

    /// Per-byte "modified" flags for a row: a byte is modified when its
    /// piece lives in the edit page, the same reading the row formatter
    /// makes.
    pub fn modified_flags(&self, row_start: u32, count: usize) -> Vec<bool> {
        (0..count)
            .map(|i| self.snap.byte_is_modified(row_start + i as u32))
            .collect()
    }

    // ---- caret / scrolling --------------------------------------------------

    /// ScrollToCaret: bring the caret's row into view.
    pub fn scroll_to_caret(&mut self) {
        let bpr = self.bytes_per_row.max(1) as u32;
        let row = (self.caret / bpr) as i32;
        let visible = self.visible_rows.max(0);
        if row < self.top_row {
            self.top_row = row;
        } else if visible > 0 && row > self.top_row + visible - 1 {
            self.top_row = row - visible + 1;
        }
        self.clamp_top_row();
    }

    /// Column (in characters from the row start) and row of the caret,
    /// for SetCaretPos.
    pub fn caret_pos(&self) -> (i32, i32) {
        let bpr = self.bytes_per_row.max(1) as u32;
        let row = (self.caret / bpr) as i32;
        let mut col = (self.caret % bpr) as i32;
        let digits = self.digits() as i32;
        let group = self.group.max(1);
        if self.column == 0 {
            // Inside the hex area: digit position within the byte plus the
            // separators of the full groups before it.
            let full_groups = col / group;
            let in_group = col % group;
            let nibble = self.typed.min(digits - 1);
            col = full_groups * (digits * group + 1) + in_group * digits + nibble;
        }
        (col, row)
    }

    /// Pixel position of the caret: the y is the caret row relative to
    /// the top row, the x is the column's character base times the char
    /// width plus the within-column offset.
    ///
    /// Hex column: `col/group − hscroll + digits*col` over the address
    /// base, −1 char at one past the row end
    /// when the row divides evenly (the separator char that is not there).
    /// The typed-nibble offset rides along so the block sits on the digit
    /// being typed. ASCII column: `col − hscroll` over
    /// the address + hex-column base.
    pub fn caret_pixel(&self) -> (i32, i32) {
        let bpr = self.bytes_per_row.max(1) as u32;
        let len = self.snap.len() as i32;
        let mut row = (self.caret / bpr) as i32;
        let mut col = (self.caret % bpr) as i32;
        // One past the end of a full row (END at EOF): the caret stays on
        // the finished row at the bpr column, which the `col == bpr`
        // comparison below decides.
        if col == 0 && len > 0 && self.caret as i32 == len {
            row -= 1;
            col = bpr as i32;
        }
        let digits = self.digits().max(1) as i32;
        let group = self.group.max(1) as i32;
        let addr_part = self.address_field()
            + if self.flags & F_HIDE_HEX != 0 { 0 } else { self.sep_addr };
        let x = if self.column == 0 {
            let mut ebx = 0;
            if col == bpr as i32 && bpr as i32 % group == 0 {
                ebx = -1;
            }
            let mut x = col / group + digits * col + self.typed.min(digits - 1)
                + addr_part + ebx - self.hscroll;
            // With a live selection the separator before a
            // group-aligned caret column is not part of the row.
            if self.anchor_a < self.anchor_b && ebx == 0 && col > 0 && col % group == 0 {
                x -= 1;
            }
            x * self.char_width
        } else {
            (col - self.hscroll + addr_part + self.hex_cols + self.sep_ascii)
                * self.char_width
        };
        (x, (row - self.top_row) * self.line_height)
    }

    /// Pixel x of the two column rules: one inside the gap after the address
    /// field, one inside the gap before the ASCII column. Both ride the
    /// horizontal scroll and sit half a cell past the gap's middle; the
    /// height is the caller's client.
    pub fn separator_pixels(&self) -> (i32, i32) {
        let cw = self.char_width.max(1);
        let addr = self.address_field() - self.hscroll;
        let x1 = addr * cw + self.sep_addr * cw / 2 + cw / 2;
        let after_hex = if self.flags & F_HIDE_HEX != 0 {
            self.sep_ascii + self.bytes_per_row
        } else {
            self.sep_addr + self.hex_cols
        };
        let x2 = (addr + after_hex) * cw + self.sep_ascii * cw / 2;
        (x1, x2)
    }

    /// Caret width: a character block in overwrite mode, a 2px bar in
    /// insert mode.
    pub fn caret_width(&self) -> i32 {
        if self.edit_mode == MODE_OVERWRITE {
            self.char_width
        } else {
            2
        }
    }

    /// SetCaret: move, and when the anchors move apart from the
    /// caret reset both (the run highlight follows).
    pub fn set_caret(&mut self, pos: u32) {
        self.caret = pos.min(self.snap.len());
        self.typed = 0;
        if self.anchor_a != self.caret || self.anchor_b != self.caret {
            self.anchor_a = self.caret;
            self.anchor_b = self.caret;
        }
        self.scroll_to_caret();
    }

    // ---- movement (the host's caret commands) --------------------------------

    pub fn move_caret(&mut self, delta: i64) {
        let len = self.snap.len() as i64;
        let pos = (self.caret as i64 + delta).clamp(0, len);
        self.set_caret(pos as u32);
    }

    pub fn move_rows(&mut self, rows: i32) {
        let bpr = self.bytes_per_row.max(1) as i64;
        self.move_caret(rows as i64 * bpr);
    }

    pub fn page_rows(&self) -> i32 {
        self.visible_rows.max(1)
    }

    pub fn move_home(&mut self) {
        let bpr = self.bytes_per_row.max(1) as u32;
        self.set_caret(self.caret - self.caret % bpr);
    }

    pub fn move_end(&mut self) {
        let bpr = self.bytes_per_row.max(1) as u32;
        let len = self.snap.len();
        let row_end = ((self.caret / bpr) + 1).saturating_mul(bpr).min(len);
        self.set_caret(row_end);
    }

    // ---- typing --------------------------------------------------------------

    /// OnChar for a digit key. The composition replaces the
    /// digit at the current place value, the first digit commits
    /// through replace/insert and later digits rewrite the scratch byte
    /// in place until the digit count completes.
    pub fn on_char(&mut self, ch: u32) -> CharResult {
        let mode = self.base_mode();
        let val: i32 = match mode {
            0 => {
                // Hex: the accepted digits are 0-9 and a-f/A-F, and the
                // value is the digit's face value.
                match ch {
                    0x30..=0x39 => ch as i32 - 0x30,
                    0x41..=0x46 => ch as i32 - 0x37,
                    0x61..=0x66 => ch as i32 - 0x57,
                    _ => return CharResult::Rejected,
                }
            }
            1 => {
                if !(0x30..=0x39).contains(&ch) {
                    return CharResult::Rejected;
                }
                ch as i32 - 0x30
            }
            2 => {
                if !(0x30..=0x37).contains(&ch) {
                    return CharResult::Rejected;
                }
                ch as i32 - 0x30
            }
            _ => {
                if ch != 0x30 && ch != 0x31 {
                    return CharResult::Rejected;
                }
                ch as i32 - 0x30
            }
        };
        if ch > 0xff {
            return CharResult::Rejected;
        }

        let digits = self.digits();
        let base = self.base();
        let typed = self.typed as u32;
        // Place value for this digit.
        let mut place: u32 = 1;
        let mut k = digits - 1;
        while k > typed {
            place *= base;
            k -= 1;
        }
        // Compose: replace the digit at `place`. The commit stores the
        // composed value's low byte, so an out-of-range compose truncates
        // rather than clamps — octal's first digit can exceed 3 that way.
        let old = self.snap.byte(self.caret).unwrap_or(0) as i32;
        let below = old % place as i32;
        let cur_digit = (old / place as i32) % base as i32;
        let mut new = (val - cur_digit) * place as i32 + old;
        if new > 0xff {
            new -= below;
        }
        let byte = (new as u32 & 0xff) as u8;

        self.typed += 1;

        if typed != 0 {
            // Mid-composition rewrite of the last edit-page byte.
            self.snap.overwrite_last_edit_byte(byte);
            if self.typed == digits as i32 {
                self.caret += 1;
                self.typed = 0;
            }
            self.scroll_to_caret();
            return CharResult::Updated;
        }

        // First digit.
        if self.edit_mode == MODE_READONLY {
            return CharResult::ReadOnly;
        }
        let in_ascii = self.column != 0;
        let mut coalesce = true;
        if self.edit_mode == MODE_INSERT && self.anchor_a != self.anchor_b {
            // The selection goes away as part of the same undo
            // group as the inserted byte; the group stays open until the
            // commit below, so both markers share the counter.
            self.snap.begin_group();
            let lo = self.anchor_a.min(self.anchor_b);
            let hi = self.anchor_a.max(self.anchor_b);
            self.snap.delete_region(lo, hi - lo, KIND_DELETE);
        } else if self.edit_mode == MODE_INSERT {
            coalesce = false;
        }
        // Collapse the caret onto the nearer anchor.
        let a = self.anchor_a;
        let b = self.anchor_b;
        if a != b && (self.caret == a || self.caret == b) {
            self.caret = a.min(b);
        }
        let ok = if self.edit_mode == MODE_OVERWRITE {
            self.snap.replace(self.caret, &[byte], 1)
        } else {
            self.snap.insert_bytes(self.caret, &[byte])
        };
        if !ok {
            if coalesce {
                self.snap.end_group();
            }
            self.typed = 0;
            return CharResult::Rejected;
        }
        if in_ascii {
            self.caret += 1;
        }
        self.anchor_a = self.caret;
        self.anchor_b = self.caret;
        if coalesce {
            self.snap.end_group();
        }
        self.scroll_to_caret();
        CharResult::Committed
    }

    /// A plain character typed in the ASCII column:
    /// the whole byte lands in one step, the selection (insert mode)
    /// dropping out inside the same undo group.
    pub fn on_ascii_char(&mut self, ch: u32) -> CharResult {
        if self.edit_mode == MODE_READONLY {
            return CharResult::ReadOnly;
        }
        let byte = ch.clamp(0, 0xff) as u8;
        if self.edit_mode == MODE_INSERT && self.anchor_a != self.anchor_b {
            // Same group pattern: the selection delete and the insert
            // share a group.
            self.snap.begin_group();
            let lo = self.anchor_a.min(self.anchor_b);
            let hi = self.anchor_a.max(self.anchor_b);
            self.snap.delete_region(lo, hi - lo, KIND_DELETE);
        }
        let a = self.anchor_a;
        let b = self.anchor_b;
        if a != b && (self.caret == a || self.caret == b) {
            self.caret = a.min(b);
        }
        let ok = if self.edit_mode == MODE_OVERWRITE {
            self.snap.replace(self.caret, &[byte], 1)
        } else {
            self.snap.insert_bytes(self.caret, &[byte])
        };
        if !ok {
            self.snap.end_group();
            return CharResult::Rejected;
        }
        self.caret += 1;
        self.anchor_a = self.caret;
        self.anchor_b = self.caret;
        self.snap.end_group();
        self.scroll_to_caret();
        CharResult::Committed
    }

    // ---- undo / redo ----------------------------------------------------------

    /// UndoExec: the snapshot transfers the group, then the
    /// anchors follow the saved range.
    pub fn undo(&mut self) -> bool {
        let Some((base, top)) = self.snap.undo() else {
            return false;
        };
        self.anchor_a = base;
        self.anchor_b = top;
        self.caret = top.min(self.snap.len());
        self.typed = 0;
        self.scroll_to_caret();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((base, top)) = self.snap.redo() else {
            return false;
        };
        self.anchor_a = base;
        self.anchor_b = top;
        self.caret = top.min(self.snap.len());
        self.typed = 0;
        self.scroll_to_caret();
        true
    }
}
