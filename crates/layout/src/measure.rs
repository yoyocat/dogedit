use std::collections::HashMap;

/// Font selector
/// a primary and a CJK face (used when
/// `ch >= 0x2000` and the CJK flag is set).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Font {
    Primary,
    CJK,
}

/// Whether `ch` modifies the character before it instead of taking a cell of
/// its own.
///
/// U+FE0F is the one that bites. `☕` is U+2615 and `☕️` U+2615 U+FE0F, two
/// different strings — the second asks for the *emoji* presentation, and U+FE0E
/// asks for the text one — while looking exactly alike, so a layout difference
/// between them reads as a rendering difference. In a fixed grid it is the
/// layout: the selector draws nothing at all, so a grid that hands it a cell
/// makes two strings that paint the same glyphs occupy different columns, and
/// a shaper that hands it a glyph paints a missing-glyph box after every emoji.
///
/// Covered: U+FE00..=U+FE0F and U+E0100..=U+E01EF (VARIATION SELECTOR-1..256),
/// U+20E3 (COMBINING ENCLOSING KEYCAP, the `1️⃣` family) and U+E0020..=U+E007F
/// (the tag characters that spell subdivision flags).
///
/// Deliberately not covered: U+200D (ZERO WIDTH JOINER) and the combining
/// diacritics. They are zero-width for the same reason, but without cluster
/// shaping `👨‍👩‍👧` would be three glyphs pretending to be one and `e` + U+0301
/// an accent floating over an empty cell. That is a grapheme-cluster model the
/// fixed grid does not have
pub fn attaches_to_previous(ch: char) -> bool {
    matches!(
        ch as u32,
        0x20E3 | 0xFE00..=0xFE0F | 0xE0020..=0xE007F | 0xE0100..=0xE01EF
    )
}

/// Platform text measurement — one implementation per OS:
/// DirectWrite (Windows), CoreText (macOS), Pango (Linux).
pub trait TextMeasurer {
    /// Width of the cell `ch` occupies, in pixels.
    ///
    /// The backend's own advance, except for the code points that are
    /// modifiers rather than cells ([`attaches_to_previous`]): those take no
    /// column anywhere, so asking the font about them is both wasted work and
    /// the source of the drift this method exists to close.
    fn char_width(&mut self, ch: char, font: Font) -> u8 {
        if attaches_to_previous(ch) {
            return 0;
        }
        self.glyph_width(ch, font)
    }
    /// What the backend's own metrics say one character is wide, before the
    /// modifier rule above. Backends implement this, not [`Self::char_width`]:
    /// the grid invariant lives in the provided method, where no new backend
    /// can miss it.
    fn glyph_width(&mut self, ch: char, font: Font) -> u8;
    /// Width of a run.
    fn measure_run(&mut self, s: &str, font: Font) -> f32;
    /// Average character width,
    /// used for surrogate sanity checks and char-mode limits.
    fn avg_width(&self, font: Font) -> u8;

    /// Tab stop width: `tabSize * width('x')`
    /// .
    fn tab_width(&mut self, tab_size: u32, font: Font) -> u32 {
        self.char_width('x', font) as u32 * tab_size
    }
}

/// Headless software measurer with a fixed width table.
/// Used by tests, by the layout oracle, and whenever no
/// platform backend is attached.
pub struct TableMeasurer {
    pub ascii: [u8; 128],
    pub cjk_width: u8,
    pub fallback: u8,
    pub avg: u8,
}

impl Default for TableMeasurer {
    fn default() -> Self {
        Self {
            ascii: [8; 128],
            cjk_width: 16,
            fallback: 8,
            avg: 8,
        }
    }
}

impl TextMeasurer for TableMeasurer {
    fn glyph_width(&mut self, ch: char, font: Font) -> u8 {
        match font {
            Font::Primary => match ch {
                c if (c as u32) < 128 => self.ascii[c as usize],
                c if (c as u32) >= 0x2000 => self.cjk_width,
                _ => self.fallback,
            },
            Font::CJK => self.cjk_width,
        }
    }

    fn measure_run(&mut self, s: &str, font: Font) -> f32 {
        s.chars().map(|c| self.char_width(c, font) as f32).sum()
    }

    fn avg_width(&self, _font: Font) -> u8 {
        self.avg
    }
}

/// Per-WCHAR memoized width cache
/// leading byte cache (`255` = uncached). The BMP is a flat
/// 64 KiB table; supplementary planes use a HashMap.
///
/// Invariant: the font used for a
/// character is a pure function of the character — CJK font iff
/// `ch >= 0x2000` — so a codepoint-keyed cache is correct.
pub struct CachedMeasurer<M: TextMeasurer> {
    inner: M,
    bmp: [u8; 65536],
    extra: HashMap<u32, u8>,
}

impl<M: TextMeasurer> CachedMeasurer<M> {
    pub fn new(inner: M) -> Self {
        Self {
            inner,
            bmp: [255u8; 65536],
            extra: HashMap::new(),
        }
    }

    pub fn inner(&self) -> &M {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut M {
        &mut self.inner
    }

    fn font_for(ch: char) -> Font {
        if (ch as u32) >= 0x2000 {
            Font::CJK
        } else {
            Font::Primary
        }
    }
}

impl<M: TextMeasurer> TextMeasurer for CachedMeasurer<M> {
    fn glyph_width(&mut self, ch: char, _font: Font) -> u8 {
        let font = Self::font_for(ch);
        let cp = ch as u32;
        if cp < 65536 {
            let b = self.bmp[cp as usize];
            if b != 255 {
                return b;
            }
            let w = self.inner.glyph_width(ch, font);
            self.bmp[cp as usize] = w;
            w
        } else {
            if let Some(w) = self.extra.get(&cp) {
                return *w;
            }
            let w = self.inner.glyph_width(ch, font);
            self.extra.insert(cp, w);
            w
        }
    }

    fn measure_run(&mut self, s: &str, font: Font) -> f32 {
        self.inner.measure_run(s, font)
    }

    fn avg_width(&self, font: Font) -> u8 {
        self.inner.avg_width(font)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The predicate is the one rule the wrapper, the layout and the shaper all
    /// read, so both the ranges it covers and the ones it refuses are pinned.
    #[test]
    fn only_modifiers_attach_to_the_previous_cell() {
        for ch in [
            '\u{FE00}', '\u{FE0E}', '\u{FE0F}', '\u{20E3}', '\u{E0100}', '\u{E01EF}',
            '\u{E0020}', '\u{E007F}',
        ] {
            assert!(attaches_to_previous(ch), "U+{:04X} is a modifier", ch as u32);
        }
        // The edge of every range, the enclosing characters either side of the
        // keycap, and the two zero-width characters this model deliberately
        // leaves out: a joiner and a combining accent need clusters, not a
        // wider predicate.
        for ch in [
            '\u{FDFF}', '\u{FE10}', '\u{20E2}', '\u{20E4}', '\u{E0000}', '\u{E001F}',
            '\u{E0080}', '\u{E01F0}', '\u{200D}', '\u{0301}', '\u{2615}', 'a', '\u{4E00}',
        ] {
            assert!(!attaches_to_previous(ch), "U+{:04X} is a cell", ch as u32);
        }
    }

    /// The trait's provided `char_width` is what the grid reads: a modifier is
    /// no column, and the backend underneath is never asked about it — here it
    /// would have said 16, the CJK cell.
    #[test]
    fn a_modifier_is_no_column() {
        let mut m = CachedMeasurer::new(TableMeasurer::default());
        for ch in ['\u{FE0F}', '\u{FE0E}', '\u{20E3}'] {
            assert_eq!(m.char_width(ch, Font::Primary), 0, "U+{:04X}", ch as u32);
            assert_eq!(
                m.glyph_width(ch, Font::Primary),
                16,
                "the backend still sees a wide cell"
            );
        }
        // The base keeps its cell, and the pair is one cell wide, not two.
        assert_eq!(m.char_width('\u{2615}', Font::Primary), 16);
        assert_eq!(m.measure_run("\u{2615}\u{FE0F}", Font::Primary), 16.0);
        assert_eq!(m.measure_run("\u{2615}", Font::Primary), 16.0);
    }
}
