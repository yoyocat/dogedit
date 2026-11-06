//! The view options one document carries, as the bit indices the edit control
//! stores them in.
//!
//! The toggle family maps a menu id to a bit index, asks the edit control for
//! that bit, writes the new value back through the same pair of calls, and
//! persists the result. The other half is bit index → the `[Edit]` key that
//! `setting.ini` stores.
//!
//! The mask lives on the document, not on the window: two tabs may show line
//! numbers and no minimap while a third shows the reverse, and `设置` only
//! ever describes the one being edited.

/// One document's view options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ViewFlags(pub u64);

impl ViewFlags {
    pub const LINE_NUMBER: u32 = 0;
    pub const RULER: u32 = 1;
    pub const WHITESPACE: u32 = 2;
    pub const CONTROL_CHAR: u32 = 3;
    pub const END_OF_LINE: u32 = 4;
    pub const END_OF_FILE: u32 = 5;
    pub const ACTIVE_LINE: u32 = 6;
    pub const INDENT_LINE: u32 = 7;
    pub const LINE_STATE: u32 = 8;
    pub const FOLDING_TAIL: u32 = 9;
    pub const ACTIVE_ARROW: u32 = 10;
    pub const INVISIBLE_CHAR_IN_SEL: u32 = 11;
    pub const BOUNDARY_LINE: u32 = 12;
    pub const SOFT_TAB: u32 = 13;
    pub const ICON_AREA: u32 = 22;
    pub const EXTEND_ACTIVE: u32 = 23;
    pub const COL_MARKER: u32 = 24;
    pub const LEVEL_BG: u32 = 25;
    pub const INDENT_WRAP: u32 = 26;
    pub const MINIMAP: u32 = 27;
    pub const HIDE_VBAR: u32 = 28;
    pub const HIDE_HBAR: u32 = 29;
    pub const BOOKMARK: u32 = 31;
    pub const VSPACE: u32 = 32;
    pub const INDENT_STYLE: u32 = 33;
    pub const RESTORE_WRAP: u32 = 36;
    /// It is a pure runtime latch.
    pub const BRACKET_HIGHLIGHT: u32 = 21;

    pub fn get(self, bit: u32) -> bool {
        (self.0 >> bit) & 1 == 1
    }

    pub fn set(&mut self, bit: u32, on: bool) {
        let mask = 1u64 << bit;
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
    }

    pub fn toggle(&mut self, bit: u32) -> bool {
        let on = !self.get(bit);
        self.set(bit, on);
        on
    }

    /// The `[Edit]` key for a bit
    /// has no name for returns `None` and is never written.
    pub fn ini_key(bit: u32) -> Option<&'static str> {
        Some(match bit {
            Self::LINE_NUMBER => "LineNumber",
            Self::RULER => "Ruler",
            Self::WHITESPACE => "WhiteSpace",
            Self::CONTROL_CHAR => "ControlChar",
            Self::END_OF_LINE => "EndOfLine",
            Self::END_OF_FILE => "EndOfFile",
            Self::ACTIVE_LINE => "ActiveLine",
            Self::INDENT_LINE => "IndentLine",
            Self::LINE_STATE => "LineState",
            Self::FOLDING_TAIL => "FoldingTail",
            Self::ACTIVE_ARROW => "ActiveArrow",
            Self::INVISIBLE_CHAR_IN_SEL => "InvisibleCharInSel",
            Self::BOUNDARY_LINE => "BoundaryLine",
            Self::SOFT_TAB => "SoftTab",
            Self::ICON_AREA => "IconArea",
            Self::COL_MARKER => "ColMarker",
            Self::EXTEND_ACTIVE => "ExtendActive",
            Self::LEVEL_BG => "LevelBG",
            Self::INDENT_WRAP => "IndentWrap",
            Self::MINIMAP => "MiniMap",
            Self::HIDE_VBAR => "HideVBar",
            Self::HIDE_HBAR => "HideHBar",
            Self::BOOKMARK => "Bookmark",
            Self::VSPACE => "VSpace",
            Self::INDENT_STYLE => "IndentStyle",
            Self::RESTORE_WRAP => "RestoreWrap",
            _ => return None,
        })
    }

    /// Every bit that has a `[Edit]` key, ascending — the whole set a config
    /// file can speak for.
    pub fn ini_bits() -> impl Iterator<Item = u32> {
        (0..64).filter(|bit| Self::ini_key(*bit).is_some())
    }

    /// The view-option rows' menu id → bit mapping, spelled out row by row rather
    /// than computed: the ids are neither contiguous nor ordered by bit (`1306` is
    /// bit 3, `1303` bit 4, `1301` bit 1), and four of them sit well outside the
    /// 1300s — minimap `1287`, line state `1675`, level background `1674`,
    /// bookmark `1676`. A row's name cannot recover its bit, so the menu dispatch
    /// goes from id to bit directly.
    pub fn bit_for_menu_id(id: u32) -> Option<u32> {
        Some(match id {
            1300 => Self::LINE_NUMBER,
            1301 => Self::RULER,
            1302 => Self::WHITESPACE,
            1306 => Self::CONTROL_CHAR,
            1303 => Self::END_OF_LINE,
            1304 => Self::END_OF_FILE,
            1305 => Self::ACTIVE_LINE,
            1307 => Self::INDENT_LINE,
            1675 => Self::LINE_STATE,
            1309 => Self::FOLDING_TAIL,
            1310 => Self::INVISIBLE_CHAR_IN_SEL,
            1311 => Self::BOUNDARY_LINE,
            1671 => Self::SOFT_TAB,
            1672 => Self::ICON_AREA,
            1337 => Self::COL_MARKER,
            1673 => Self::EXTEND_ACTIVE,
            1674 => Self::LEVEL_BG,
            1308 => Self::INDENT_WRAP,
            1287 => Self::MINIMAP,
            1676 => Self::BOOKMARK,
            1677 => Self::RESTORE_WRAP,
            // 使用虚拟空格 (1483): the caret may travel past a line's last
            // character; typing there fills the gap with spaces. The bit is the
            // document's own, so the row reads its check off the same place the
            // toggle writes.
            1483 => Self::VSPACE,
            _ => return None,
        })
    }

    /// Whether this implementation's renderer or editor has a code path that acts on the option at all, so the
    /// menu row can be live instead of greyed. The whole 符号显示 family is here: the invisible-character marks
    /// (whitespace, control characters, the end-of-line arrow and the end-of-file tag), the "show them inside
    /// the selection too" latch, the folding tail, the level background and the column markers. The two gutter
    /// strips are here because both take room in the layout and paint their band: `LINE_STATE` the
    /// `LESWidth`-wide default-background strip, `BOOKMARK` the DPI-scaled column left of the line numbers.
    pub fn is_rendered(bit: u32) -> bool {
        matches!(
            bit,
            Self::LINE_NUMBER
                | Self::RULER
                | Self::ACTIVE_LINE
                | Self::INDENT_LINE
                | Self::MINIMAP
                | Self::LINE_STATE
                | Self::BOOKMARK
                | Self::BRACKET_HIGHLIGHT
                | Self::WHITESPACE
                | Self::CONTROL_CHAR
                | Self::END_OF_LINE
                | Self::END_OF_FILE
                | Self::INVISIBLE_CHAR_IN_SEL
                | Self::FOLDING_TAIL
                | Self::BOUNDARY_LINE
                | Self::ICON_AREA
                | Self::EXTEND_ACTIVE
                | Self::LEVEL_BG
                | Self::COL_MARKER
                // 软空格不进渲染器：它换掉的是 Tab、退格、缩进三处输入路径的
                // 单位（一格制表符 ↔ 补到下一个制表列的空格），以及状态条那格
                // 制表符/软空格 的文字。开关翻的是文档自己的位，这些路径全都
                // 在位，所以这一行也是活的。
                | Self::SOFT_TAB
        )
    }

    /// The bookmark column is one of them: that default names no `Bookmark` key.
    pub fn shipped_defaults() -> Self {
        let mut flags = Self(0);
        for bit in [
            Self::LINE_NUMBER,
            Self::ACTIVE_LINE,
            Self::INDENT_LINE,
            Self::BOUNDARY_LINE,
            Self::LINE_STATE,
            Self::BOOKMARK,
            // The shipped config turns the "show invisible characters inside the
            // selection" latch on, so a selected block reads its own blanks.
            Self::INVISIBLE_CHAR_IN_SEL,
            // Bracket highlighting is on the moment a view exists — the view
            // constructor sets it, and `cm_highlight_brace` flips the same bit.
            // It has no `[Edit]` key (runtime latch, never written to ini), so
            // the default is the only place it can come from, and it defaults to on.
            Self::BRACKET_HIGHLIGHT,
            // The same constructor switches on the folding tail (折叠尾部字符) and
            // the active-line arrow. Only the tail is
            // rendered here, and it is on by default: the indicator box and the
            // collapsed region's last-line preview that follow a folded opener's
            // text.
            Self::FOLDING_TAIL,
        ] {
            flags.set(bit, true);
        }
        flags
    }

    /// A key the file leaves out leaves the default alone, which is what lets a
    /// config that only mentions `[Edit]/MiniMap=N` keep the line numbers.
    /// `lookup` is handed the key name; keeping it a closure is what lets the
    /// app read from a file, a test read from a table, and this type stay
    /// ignorant of both.
    pub fn with_ini(mut self, mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        for bit in Self::ini_bits() {
            if let Some(v) = lookup(Self::ini_key(bit).unwrap()) {
                self.set(bit, v.eq_ignore_ascii_case("Y"));
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_returns_the_landed_state() {
        let mut f = ViewFlags::default();
        assert!(f.toggle(ViewFlags::RULER));
        assert!(f.get(ViewFlags::RULER));
        assert!(!f.toggle(ViewFlags::RULER));
        assert!(!f.get(ViewFlags::LINE_NUMBER));
    }

    /// 软空格（使用软空格）不进渲染器，但它的输入路径都在：开关行必须
    /// 是活的，少了这条判定位，菜单里那一行永远点不动。
    #[test]
    fn soft_tab_toggle_is_live() {
        assert_eq!(ViewFlags::bit_for_menu_id(1671), Some(ViewFlags::SOFT_TAB));
        assert!(ViewFlags::is_rendered(ViewFlags::SOFT_TAB));
    }

    /// The id → bit table is the product, so the two ends of it are pinned:
    /// the names the view-flag table returns and the fact that only an explicit
    /// `Y` reads as on while an absent key leaves the default alone.
    #[test]
    fn ini_keys_are_the_view_option_spellings_and_read_back() {
        assert_eq!(
            ViewFlags::ini_key(ViewFlags::INVISIBLE_CHAR_IN_SEL),
            Some("InvisibleCharInSel")
        );
        assert_eq!(ViewFlags::ini_key(4), Some("EndOfLine"));
        assert_eq!(ViewFlags::ini_key(63), None);
        assert_eq!(
            ViewFlags::bit_for_menu_id(1306),
            Some(ViewFlags::CONTROL_CHAR)
        );
        assert_eq!(ViewFlags::bit_for_menu_id(1303), Some(ViewFlags::END_OF_LINE));
        assert_eq!(ViewFlags::bit_for_menu_id(1287), Some(ViewFlags::MINIMAP));
        assert_eq!(ViewFlags::bit_for_menu_id(1328), None);
        let read = |k: &str| match k {
            "LineNumber" => Some("Y".to_string()),
            "MiniMap" => Some("N".to_string()),
            "ActiveLine" => Some("y".to_string()),
            "IndentLine" => Some("N".to_string()),
            _ => None,
        };
        let f = ViewFlags::shipped_defaults().with_ini(read);
        assert!(f.get(ViewFlags::LINE_NUMBER));
        assert!(f.get(ViewFlags::ACTIVE_LINE));
        assert!(!f.get(ViewFlags::INDENT_LINE));
        assert!(!f.get(ViewFlags::MINIMAP));
        assert!(!f.get(ViewFlags::RULER));
        // a config that speaks about nothing keeps the default look
        assert!(
            ViewFlags::shipped_defaults()
                .with_ini(|_| None)
                .get(ViewFlags::INDENT_LINE)
        );
    }
}
