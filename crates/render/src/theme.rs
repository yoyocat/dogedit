use std::collections::BTreeMap;

/// sRGB color with alpha.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// 0xRRGGBB
    /// colors use.
    pub const fn from_u32(v: u32) -> Self {
        Self {
            r: ((v >> 16) & 0xFF) as u8,
            g: ((v >> 8) & 0xFF) as u8,
            b: (v & 0xFF) as u8,
            a: 255,
        }
    }

    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | self.b as u32
    }
}

/// The stock multi-caret indicator (#FFA500): what a theme with no `SelIndicator`
/// key gets, and the hue the 保底 reaches for when a theme's own value cannot be
/// told apart from its caret.
const STOCK_SEL_INDICATOR: Color = Color::rgb(0xFF, 0xA5, 0x00);

/// Integer Rec. 601 brightness, unshifted: a 0..=765 scale.
fn luma(c: Color) -> u32 {
    3 * c.r as u32 + 6 * c.g as u32 + c.b as u32
}

/// Whether two colours belong to the same colour family — the same proportions
/// between their channels once each is scaled to its own brightness. Every pair of
/// greys does, and so does a mid grey beside a light grey. That is the shape of
/// `Caret` and `SelIndicator` in all 27 bundled themes, and why the mark there
/// reads as a second caret instead of as a marker.
fn same_family(a: Color, b: Color) -> bool {
    let (la, lb) = (luma(a), luma(b));
    if la == 0 || lb == 0 {
        // A channel-less black has no proportions to compare.
        return true;
    }
    let share = |c: Color, l: u32| {
        [
            c.r as u32 * 1000 / l,
            c.g as u32 * 1000 / l,
            c.b as u32 * 1000 / l,
        ]
    };
    let (sa, sb) = (share(a, la), share(b, lb));
    (sa[0] as i64 - sb[0] as i64).abs() <= 60
        && (sa[1] as i64 - sb[1] as i64).abs() <= 60
        && (sa[2] as i64 - sb[2] as i64).abs() <= 60
}

/// Editor theme.
#[derive(Clone)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub caret: Color,
    /// The 柔光 halo around the caret (`CaretGlow` theme key). A theme that sets
    /// `Caret` but no `CaretGlow` gets its caret colour at 35 % alpha, which is
    /// what the stock dark theme's grey bar wants.
    pub caret_glow: Color,
    /// The multi-caret indicator line (`SelIndicator` theme key: #FF8000 in
    /// the stock `the stock theme`, #F78C6C in `the dark theme.ini`). Every
    /// caret past the first wears it: the caret-line painter answers each range
    /// end's range flag with `SetDCPenColor` and a
    /// `MoveToEx`/`LineTo` pair down the row, so a multi-caret set paints
    /// one bar per extra caret, as thick as the primary caret's and in this
    /// colour, while the primary caret stays the system caret. The key's own
    /// ctor default is 42495 (0x00A5FF) —
    /// #FFA500 — which is what a theme with no `SelIndicator` key gets.
    pub sel_indicator: Color,
    pub selection_bg: Color,
    /// `Selection` 的第二色。选区不保持语法高亮前景时，被选中的字就涂这一色；
    /// 深色主题给的是近白（`#F3F5F6`），浅色主题给的是近黑（`#1A1F23`），
    /// 两边都是为了让饱和的选区底色上仍有足够对比。
    pub selection_fg: Color,
    pub current_line_bg: Color,
    pub gutter_fg: Color,
    pub gutter_bg: Color,
    /// Folding gutter strip (`Folding` theme
    /// key: #1B1C1F/#666E80 in the dark theme).
    /// The bg fills the strip, the fg paints the
    /// collapse boxes (the DC brush/pen colour).
    pub folding_bg: Color,
    pub folding_fg: Color,
    /// Icon gutter strip (`IconArea` theme key:
    /// #1B1C1F/#D4D7DD in the dark theme). The bg
    /// fills the strip, the fg paints the bookmark
    /// marker's inner bar.
    pub icon_bg: Color,
    pub icon_fg: Color,
    /// The area below the last text row
    /// (`DefaultBgNull` theme key: #1E1F22 in
    /// the dark theme)
    /// ExtTextOutWs over after the last line.
    pub below_bg: Color,
    /// Bookmark gutter marker
    /// (`BookMark` theme key,
    /// #82AAFF in the dark theme).
    pub bookmark_fg: Color,
    /// Paired-tag highlight color (the
    /// class-11 recolor of `TagHighlight`).
    pub tag_fg: Color,
    /// 括弧高亮 (cm_highlight_brace, 1326): the `Pair` theme key's background
    /// half. The fg half is the `tag_fg` this implementation already reads
    /// from the same key.
    pub pair_bg: Color,
    /// 选区高亮 (cm_highlight_sel, 1313): the two halves of the `Found`
    /// theme key (#5C4A16/#FFECB3 in the dark theme).
    pub found_bg: Color,
    pub found_fg: Color,
    /// 高亮网址和邮件 (cm_highlight_urlemail, 1327): the two
    /// halves of the `Url` theme key, a `(bg, fg)` pair.
    pub url_bg: Color,
    pub url_fg: Color,
    /// The two halves of the `Email` theme key.
    pub email_bg: Color,
    pub email_fg: Color,
    /// The two halves of the `Word2` theme key — attribute class 17, which the
    /// 查找结果 document puts on every `"\t(line,col):"` prefix
    /// (the tokenizer's class, resolved by the colour resolve).
    pub word2_bg: Color,
    pub word2_fg: Color,
    /// The `WordComplete` key: the background and text of the automatic completion
    /// popup (the 自动完成 style entry). The popup's paint fills its client with the bg,
    /// draws every row in the fg. Stock themes: #000080/#FFFFFF, #2A2D34/#D4D7DD in
    /// the dark theme.
    pub word_complete_bg: Color,
    pub word_complete_fg: Color,
    pub margin_bg: Color,
    /// Ruler tick and column-number
    /// color (the `Ruler` fg key,
    /// #4A5162 in the dark theme).
    pub ruler_fg: Color,
    /// 1 px guide line at the right
    /// edge of the line-number strip
    /// (the `IndentLine` key,
    /// #33363D in the dark theme).
    pub indent_line: Color,
    /// The 符号显示 marks' colour: the whitespace dots, the tab arrows, the
    /// end-of-line arrow and the `[EOF]` tag all take it (the `WhiteSpace` key's
    /// fg half, #C0C0C0 in the stock themes). The key's bg half would fill a
    /// tab's cell; the row background already provides that here.
    pub whitespace_fg: Color,
    /// 边界线 (the `BoundaryLine` key, a single colour): the 1 px separator a
    /// gutter strip draws down its right edge.
    pub boundary_line: Color,
    /// 控制字符 (the `ConChar` pair): the rounded cell behind a control
    /// character's name, and the name itself.
    pub con_char_bg: Color,
    pub con_char_fg: Color,
    /// 行修改状态 (`LineState`): the two colours the edit-state strip fills a
    /// line's band with — modified since the last save, and modified but
    /// written back by it. Neither has a `[Theme]` key; both are the built-in
    /// pair a theme file cannot override.
    pub line_state_modified: Color,
    pub line_state_saved: Color,
    pub minimap_viewport: Color,
    pub line_height: f32,
    /// 额外行距（整像素，默认 0）：行步进在标准值之上再加它，行的文字与
    /// 插入符往下挪它的一半（在多出来的空间里居中），背景带不动。
    /// 落盘为 `[Edit] LineSpacingEx`。
    pub line_spacing: i32,
    /// The font's cell height (`tmHeight`). The row pitch is one more than this
    /// and the ruler band two more
    /// the font's cell-height field (the view update), not from the pitch.
    pub font_height: f32,
    pub x_width: f32,
    /// Gutter (line-number strip)
    /// width in px: the text area
    /// starts at this offset. Set by
    /// the platform layer; 0 in
    /// headless tests.
    pub gutter_x: f32,
    /// Advance width of CJK glyphs
    /// (cp >= 0x2000): two cells,
    /// dual-font model.
    pub cjk_width: f32,
    /// The primary caret's bar thickness in device pixels — 2 px, or 3 px from
    /// 144 DPI, 1 px under the 细线 style. Set by the platform from the same
    /// rule that gives the system caret its box, so the multi-caret marks the
    /// row pass paints are as thick as the caret the user is looking at.
    pub caret_w: f32,
    /// Characters whose real advance differs from the two-cell grid, measured
    /// on the face that resolves them — colour emoji, most often, whose glyph
    /// is wider than the CJK cell. Sorted by character. The row layout and the
    /// glyph shaper read this same list, so a caret after an emoji lands on the
    /// glyph's own edge instead of inside it. Filled by the platform from its
    /// font; empty keeps the grid.
    pub measured: Vec<(char, i32)>,
    /// Custom-drawn scrollbar (dg-ui).
    pub scrollbar_bg: Color,
    pub scrollbar_thumb: Color,
    pub scrollbar_thumb_hover: Color,
    pub scrollbar_arrow: Color,
    /// Overlay bar geometry. The translucency is real DWM compositing
    /// (per-pixel alpha through `UpdateLayeredWindow` on the bar's own child
    /// HWND), not a DrawList trick, so it belongs to the theme next to the
    /// colours it composites with.
    ///
    /// `scrollbar_thumb_width` is the thumb pill's thickness — the only
    /// pixels a bar paints — and `scrollbar_strip_width` the strip window it
    /// floats in, centred. Both are fixed: pointing at the bar or scrolling
    /// recolours the pill, it never reshapes it.
    pub scrollbar_thumb_width: f32,
    pub scrollbar_strip_width: f32,
    /// The resting pill's opacity; the highlighted (pointed or dragged) pill
    /// is the more solid one — see the paint in `win32/overlay.rs`.
    pub scrollbar_alpha: u8,
    pub scrollbar_fade_ms: u32,
    /// Custom-drawn status bar (dg-ui).
    pub statusbar_bg: Color,
    pub statusbar_fg: Color,
    pub statusbar_divider: Color,
    /// Custom-drawn dock panels (dg-ui).
    pub panel_bg: Color,
    pub panel_fg: Color,
    pub panel_hover: Color,
    pub panel_active: Color,
    /// Find-in-files match highlight.
    pub match_fg: Color,
    /// 自定义标记 (`cm_misc_usermarker_*`): the eight slot colours, as a
    /// background and a text colour per slot. Keeping them here is what lets
    /// a theme restyle them. The defaults match its dark theme.
    pub marker_bg: [Color; 8],
    pub marker_fg: [Color; 8],
    /// Custom-drawn popup menu (dg-ui).
    pub menu_bg: Color,
    pub menu_fg: Color,
    pub menu_hover: Color,
    pub menu_disabled: Color,
    /// The 1 px rule between menu groups. The menu manager's own
    /// `DrawEdge`/line is a low-contrast rule, measured `#3E3E3E` on the
    /// dark menu.
    pub menu_separator: Color,
    /// The main toolbar (`ToolbarWindow32`).
    pub toolbar_bg: Color,
    pub toolbar_border: Color,
    /// The band's ink and state fills, read by the toolbar's
    /// `NM_CUSTOMDRAW` handler: text/disabled, hot/selected and the 1 px
    /// button frame. These are process-wide constants, never written at
    /// runtime, so they are theme entries rather than colours handed to the
    /// OS.
    pub toolbar_fg: Color,
    pub toolbar_disabled: Color,
    pub toolbar_hot: Color,
    pub toolbar_selected: Color,
    pub toolbar_frame: Color,
    /// The same block's control face, which the
    /// dialogs' buttons and fields sit on. The toolbar band itself never uses
    /// it, but it belongs to that one chrome table rather than to a second one.
    pub toolbar_field: Color,
    /// The document tab strip (`the tab control`). The band and the label inks
    /// are system-derived there, so this implementation keeps them as theme
    /// entries instead of handing the OS a colour to invent. `tab_frame` is the
    /// bar row handler's `SetDCPenColor(0x9C9B91)` — the 1-px outline every tab
    /// gets, which is what separates two adjacent tabs. `tab_mark_fg` is the same
    /// function's state band: item state 1 (modified) paints it `255`, i.e. red,
    /// across the tab's top two rows, and `tab_hot_band` is what the hovered tab
    /// gets in its place — the bar row handler's hover colour, i.e. `#FF8000`,
    /// which overrides the state colour for any tab, selected or not.
    pub tab_bg: Color,
    pub tab_active_bg: Color,
    pub tab_frame: Color,
    pub tab_mark_fg: Color,
    pub tab_hot_band: Color,
    pub tab_fg: Color,
    pub tab_active_fg: Color,
    /// The tail glyphs (◀ ▶ ▾ ✕) in the global-close glyphs, from the
    /// nibble state the control carries: `8421504` idle, `0` hovered,
    /// `255` pressed-with-the-cursor-still-inside, `12632256` when the cell
    /// cannot act (◀ ▶ with nothing to scroll that way).
    pub tab_glyph_fg: Color,
    pub tab_glyph_hover: Color,
    pub tab_glyph_pressed: Color,
    pub tab_glyph_disabled: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::rgb(0x1E, 0x1E, 0x1E),
            foreground: Color::from_u32(0xE2E8F0),
            caret: Color::rgb(0xAE, 0xAF, 0xAD),
            caret_glow: Color::rgba(0xAE, 0xAF, 0xAD, 90),
            sel_indicator: STOCK_SEL_INDICATOR,
            selection_bg: Color::rgba(0x26, 0x4F, 0x78, 0x80),
            selection_fg: Color::rgb(0xF3, 0xF5, 0xF6),
            current_line_bg: Color::rgba(0x2A, 0x2A, 0x2A, 0xFF),
            gutter_fg: Color::rgb(0x85, 0x85, 0x85),
            gutter_bg: Color::rgb(0x1E, 0x1E, 0x1E),
            folding_bg: Color::rgb(0x1E, 0x1E, 0x1E),
            folding_fg: Color::rgb(0x85, 0x85, 0x85),
            icon_bg: Color::rgb(0x1E, 0x1E, 0x1E),
            icon_fg: Color::from_u32(0xE2E8F0),
            below_bg: Color::rgb(0x1E, 0x1E, 0x1E),
            bookmark_fg: Color::rgb(0x82, 0xAA, 0xFF),
            tag_fg: Color::rgb(0xFF, 0xCC, 0x66),
            // the dark theme measurements: Pair/Url/Email all
            // carry the editor background as bg, so only the fg
            // half moves the pixels.
            pair_bg: Color::rgb(0x1E, 0x1F, 0x22),
            found_bg: Color::rgb(0x5C, 0x4A, 0x16),
            found_fg: Color::rgb(0xFF, 0xEC, 0xB3),
            url_bg: Color::rgb(0x1E, 0x1F, 0x22),
            url_fg: Color::rgb(0x80, 0xCB, 0xC4),
            email_bg: Color::rgb(0x1E, 0x1F, 0x22),
            email_fg: Color::rgb(0x80, 0xCB, 0xC4),
            word2_bg: Color::rgb(0x1E, 0x1F, 0x22),
            word2_fg: Color::rgb(0xFF, 0xCB, 0x6B),
            // the dark theme's `WordComplete=#2A2D34,#D4D7DD`, the pair the rest
            // of this default palette is measured from.
            word_complete_bg: Color::rgb(0x2A, 0x2D, 0x34),
            word_complete_fg: Color::rgb(0xD4, 0xD7, 0xDD),
            margin_bg: Color::rgb(0x25, 0x25, 0x25),
            ruler_fg: Color::rgb(0x4A, 0x51, 0x62),
            indent_line: Color::rgb(0x33, 0x36, 0x3D),
            // 符号显示: the stock themes' `WhiteSpace=#000000,#C0C0C0`, the
            // `BoundaryLine` grey, and the `ConChar` pair.
            whitespace_fg: Color::rgb(0xC0, 0xC0, 0xC0),
            boundary_line: Color::rgb(0x38, 0x38, 0x38),
            con_char_bg: Color::rgb(0x24, 0x24, 0x24),
            con_char_fg: Color::rgb(0xC0, 0xC0, 0xC0),
            // 行修改状态: the strip's built-in pair — a modified line red, a
            // line the last save wrote back green.
            line_state_modified: Color::rgb(0xFF, 0x00, 0x00),
            line_state_saved: Color::rgb(0x00, 0xFF, 0x00),
            minimap_viewport: Color::rgba(0xFF, 0xFF, 0xFF, 0x40),
            line_height: 18.0,
            line_spacing: 0,
            font_height: 17.0,
            x_width: 8.0,
            gutter_x: 0.0,
            cjk_width: 16.0,
            caret_w: 2.0,
            measured: Vec::new(),
            scrollbar_bg: Color::rgb(0x1E, 0x1E, 0x1E),
            // The resting pill is a translucent line at the edge: solid enough
            // to find without the pointer near it, quiet enough to read the
            // text under it. Pointing at the bar or dragging lights it up to
            // the near-solid hover colour.
            scrollbar_thumb: Color::rgb(0x54, 0x54, 0x54),
            scrollbar_thumb_hover: Color::rgb(0xC8, 0xC8, 0xC8),
            scrollbar_arrow: Color::rgb(0x25, 0x25, 0x25),
            // A 6 px pill floating in a 12 px strip — the proportions of the
            // capsule the bar paints. Geometry never changes with state.
            scrollbar_thumb_width: 6.0,
            scrollbar_strip_width: 12.0,
            scrollbar_alpha: 0x9A,
            scrollbar_fade_ms: 700,
            statusbar_bg: Color::rgb(0x25, 0x25, 0x26),
            statusbar_fg: Color::rgb(0xCC, 0xCC, 0xCC),
            statusbar_divider: Color::rgb(0x3C, 0x3C, 0x3C),
            panel_bg: Color::rgb(0x25, 0x25, 0x26),
            panel_fg: Color::rgb(0xCC, 0xCC, 0xCC),
            panel_hover: Color::rgb(0x2A, 0x2D, 0x2E),
            panel_active: Color::rgb(0x37, 0x37, 0x3D),
            match_fg: Color::rgb(0xFF, 0xCC, 0x66),
            // Slot order = 使用标记1 … 使用标记8. Red, orange, yellow, green,
            // blue, purple, teal
            // for a marker on a plain row.
            marker_bg: [
                Color::rgb(0x7A, 0x2E, 0x2E),
                Color::rgb(0x7A, 0x4A, 0x1E),
                Color::rgb(0x6E, 0x64, 0x20),
                Color::rgb(0x2E, 0x5E, 0x36),
                Color::rgb(0x27, 0x49, 0x6D),
                Color::rgb(0x3F, 0x34, 0x70),
                Color::rgb(0x1F, 0x5A, 0x57),
                Color::rgb(0x4A, 0x4A, 0x52),
            ],
            marker_fg: [
                Color::rgb(0xFF, 0xD9, 0xD9),
                Color::rgb(0xFF, 0xE6, 0xC7),
                Color::rgb(0xFF, 0xF6, 0xC2),
                Color::rgb(0xD6, 0xF5, 0xDA),
                Color::rgb(0xD6, 0xE7, 0xFA),
                Color::rgb(0xE3, 0xDA, 0xFB),
                Color::rgb(0xCF, 0xF3, 0xEF),
                Color::rgb(0xE8, 0xEA, 0xED),
            ],
            menu_bg: Color::rgb(0x2B, 0x2B, 0x2B),
            menu_fg: Color::rgb(0xF0, 0xF0, 0xF0),
            menu_hover: Color::rgb(0x09, 0x47, 0x74),
            menu_disabled: Color::rgb(0x6D, 0x6D, 0x6D),
            menu_separator: Color::rgb(0x3E, 0x3E, 0x3E),
            toolbar_bg: Color::rgb(0x20, 0x20, 0x20),
            toolbar_border: Color::rgb(0x46, 0x46, 0x46),
            toolbar_fg: Color::rgb(0xE0, 0xE0, 0xE0),
            toolbar_disabled: Color::rgb(0x8C, 0x8C, 0x8C),
            toolbar_hot: Color::rgb(0x3A, 0x3A, 0x3A),
            toolbar_selected: Color::rgb(0x48, 0x48, 0x48),
            toolbar_frame: Color::rgb(0x5C, 0x5C, 0x5C),
            toolbar_field: Color::rgb(0x2D, 0x2D, 0x2D),
            tab_bg: Color::rgb(0x20, 0x20, 0x20),
            tab_active_bg: Color::rgb(0x40, 0x40, 0x40),
            tab_frame: Color::rgb(0x91, 0x9B, 0x9C),
            tab_mark_fg: Color::rgb(0xFF, 0x00, 0x00),
            tab_hot_band: Color::rgb(0xFF, 0x80, 0x00),
            tab_fg: Color::rgb(0x8C, 0x8C, 0x8C),
            tab_active_fg: Color::rgb(0xE0, 0xE0, 0xE0),
            tab_glyph_fg: Color::rgb(0x80, 0x80, 0x80),
            tab_glyph_hover: Color::rgb(0x00, 0x00, 0x00),
            tab_glyph_pressed: Color::rgb(0xFF, 0x00, 0x00),
            tab_glyph_disabled: Color::rgb(0xC0, 0xC0, 0xC0),
        }
    }
}

impl Theme {
    /// 标准行步进：不含额外行距的行高（字体单元格 + 1）。设置行高输入的
    /// 「标准行高的倍数」以它为基准。前提是 `line_height` 此刻**已含**额外
    /// 行距；拿到刚量出的标准值就直接用那个数，不要走这里。
    pub fn standard_line_height(&self) -> f32 {
        self.line_height - self.line_spacing as f32
    }

    /// 把额外行距折进行步进：行步进 = 标准 + 额外。测量后端每次量出的都是
    /// 标准值，落进 theme 前都要过这一步，额外行距才不会在换字体/换 DPI
    /// 时被冲掉。
    pub fn apply_line_spacing(&mut self, standard: f32) {
        self.line_height = standard + self.line_spacing as f32;
    }

    /// 行内容的垂直偏移：额外行距的一半（整除，和行步进的整数语义一致）。
    /// 行里的文字与插入符往下挪它；背景带、选区、缩进线留在行顶。
    pub fn content_offset(&self) -> f32 {
        (self.line_spacing / 2) as f32
    }

    /// The colour a multi-caret mark wears.
    ///
    /// The theme's own `SelIndicator` is honoured whenever it is a different kind
    /// of colour from the caret it sits beside. When it is not — every bundled
    /// theme names a grey next to a grey caret — the mark takes the stock hue at
    /// that value's own brightness, so it reads as a marker on all 27 without
    /// glaring on the light themes. A theme that wants its grey back names a
    /// `SelIndicator` of its own hue.
    pub fn caret_indicator(&self) -> Color {
        let ind = self.sel_indicator;
        if !same_family(ind, self.caret) {
            return ind;
        }
        let stock = STOCK_SEL_INDICATOR;
        // Never scale below a third of the stock brightness: a theme that names
        // near-black would otherwise get a near-black mark, the thing being
        // avoided.
        let l = luma(ind).max(luma(stock) / 3);
        let scale = |c: u32| (c * l / luma(stock)).min(255) as u8;
        Color::rgba(
            scale(stock.r as u32),
            scale(stock.g as u32),
            scale(stock.b as u32),
            ind.a,
        )
    }
}

/// One parsed theme color entry:
/// `Key=#RRGGBB,#RRGGBB` (bg, fg) or
/// `Key=#RRGGBB` (single color).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemeColor {
    pub bg: Option<Color>,
    pub fg: Option<Color>,
}

impl ThemeColor {
    /// Single-color form — both
    /// channels carry the color.
    pub fn single(c: Color) -> Self {
        Self {
            bg: Some(c),
            fg: Some(c),
        }
    }
}

/// Parsed `theme/*.ini` — the
/// `[Theme]` color table, the
/// `[Style]` font-style flags,
/// the `[Metrics]` shell geometry,
/// the `[Shape]` per-region shape
/// and the `[Fonts]` named fonts.
#[derive(Clone, Debug, Default)]
pub struct ThemeFile {
    pub name: String,
    pub colors: BTreeMap<String, ThemeColor>,
    pub styles: BTreeMap<String, u32>,
    /// Shell geometry (`[Metrics]`): `status.height=26`, `tab.height=30`…
    /// The key names are exactly the skin's `[metrics]` names, so one
    /// spelling covers both files. Values are *unclamped* here: this crate
    /// does not know the legal range of a shell metric (`dg-skin` owns
    /// `MetricId` and its `Clamp`), and `dg-render` must not depend on
    /// `dg-skin`. `dg_skin::Metrics::apply_theme` does the name lookup and
    /// the clamp.
    pub metrics: BTreeMap<String, f32>,
    /// Per-region shape (`[Shape]`): `tab.radius=4`,
    /// `tab.states.active.radius=0`, `tab.states.active.font=font.ui`.
    ///
    /// Keys are `region[.states.<state>].<key>`; the values stay **text**
    /// because one of the keys (`font`) names a font rather than being a
    /// number, and because `dg-render` must not know `ShapeKey` — the same
    /// reason `metrics` is unclamped here. `dg_skin::Recipe::from_theme_file`
    /// splits the key and validates each value.
    ///
    /// `states` is an infix, not a level: `tab.states.active.radius` is the
    /// `(tab, active, radius)` cell. A region name may itself contain dots
    /// (`dock.tab`), so the split is "everything before `.states.`", not
    /// "the first segment".
    pub shapes: BTreeMap<String, String>,
    /// Named fonts (`[Fonts]`): `mono.face=Consolas`, `mono.size=11`.
    ///
    /// Values stay text for the same reason as `shapes`: `dg-render` has no
    /// opinion on what a font spec is.
    pub fonts: BTreeMap<String, String>,
}

impl ThemeFile {
    /// Looks up a color entry.
    pub fn color(&self, key: &str) -> Option<ThemeColor> {
        self.colors.get(key).copied()
    }

    /// Looks up a `[Style]` numeric entry.
    pub fn style(&self, key: &str) -> Option<u32> {
        self.styles.get(key).copied()
    }

    /// Looks up a `[Metrics]` entry, unclamped.
    pub fn metric(&self, key: &str) -> Option<f32> {
        self.metrics.get(key).copied()
    }

    /// Looks up a `[Shape]` entry, as written (`"4"`, `"font.ui"`).
    ///
    /// No splitting and no parsing: the caller owns the key table. See
    /// [`ThemeFile::shapes`] for the key spelling.
    pub fn shape(&self, key: &str) -> Option<&str> {
        self.shapes.get(key).map(String::as_str)
    }

    /// Looks up a `[Fonts]` entry, as written (`"Consolas"`, `"11"`).
    pub fn font(&self, key: &str) -> Option<&str> {
        self.fonts.get(key).map(String::as_str)
    }

    /// Lays `over` on top of `self`: every key `over` carries wins, and a key
    /// it does not carry keeps `self`'s value.
    ///
    /// This is what turns "bundled theme + the user's `themes/<name>.ini`" into
    /// one file that applies in a single pass. Without it an override file
    /// would *replace* the bundled one, so a file carrying only `[Metrics]`
    /// would be rejected outright (no `[Theme]` colours) and a file carrying
    /// one colour would push every other colour back to `Theme::default`
    /// instead of the bundled value.
    pub fn merge(&mut self, over: &ThemeFile) {
        for (k, v) in &over.colors {
            self.colors.insert(k.clone(), *v);
        }
        for (k, v) in &over.styles {
            self.styles.insert(k.clone(), *v);
        }
        for (k, v) in &over.metrics {
            self.metrics.insert(k.clone(), *v);
        }
        for (k, v) in &over.shapes {
            self.shapes.insert(k.clone(), v.clone());
        }
        for (k, v) in &over.fonts {
            self.fonts.insert(k.clone(), v.clone());
        }
        if !over.name.is_empty() {
            self.name = over.name.clone();
        }
    }

    /// The entry's fg, falling back
    /// to its bg.
    pub fn fg(&self, key: &str) -> Option<Color> {
        let c = self.colors.get(key)?;
        c.fg.or(c.bg)
    }

    /// The entry's bg, falling back
    /// to its fg.
    pub fn bg(&self, key: &str) -> Option<Color> {
        let c = self.colors.get(key)?;
        c.bg.or(c.fg)
    }
}

/// Parses a `theme/*.ini` file:
/// `[Theme]` (`Key=#RRGGBB,#RRGGBB`),
/// `[Style]` (`Key=<flags>`),
/// `[Metrics]` (`key.name=<pixels>`),
/// `;;` comments, last duplicate
/// wins. An unknown section is
/// skipped whole, which is what
/// lets an older build read a
/// theme that carries `[Metrics]`.
///
/// 行的切分、段名大小写、注释、重复键这些都在 [`dg_core::ini`] 一处；
/// 这里只做每个段自己的类型化解析，解析不了的值**丢掉而不是给默认值**
/// （见下面 metrics 那条）。
pub fn parse_theme_ini(text: &str) -> ThemeFile {
    let ini = dg_core::ini::Ini::parse(text);
    let mut file = ThemeFile::default();
    for (key, value) in ini.rows_in("Theme") {
        if let Some(c) = parse_theme_color(value.trim()) {
            file.colors.insert(key.trim().to_string(), c);
        }
    }
    for (key, value) in ini.rows_in("Style") {
        if let Ok(n) = value.trim().parse::<u32>() {
            file.styles.insert(key.trim().to_string(), n);
        }
    }
    for (key, value) in ini.rows_in("Metrics") {
        // A value that does not parse is dropped rather than defaulted:
        // a typo must not silently become 0, which some metrics read as
        // "derive it from the font".
        if let Ok(v) = value.trim().parse::<f32>()
            && v.is_finite()
        {
            file.metrics.insert(key.trim().to_string(), v);
        }
    }
    // Shape and font values are kept as written. `dg-skin` does the
    // splitting and the validation — it owns the key tables, and
    // validating here would mean this crate knowing what a shape key
    // is. Dropping nothing here is deliberate: a bad value has to
    // reach the layer that can name it in a message.
    for (key, value) in ini.rows_in("Shape") {
        file.shapes
            .insert(key.trim().to_string(), value.trim().to_string());
    }
    for section in ["Fonts", "Font"] {
        for (key, value) in ini.rows_in(section) {
            file.fonts
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    file
}

/// 往主题文件里写一个键，返回**整篇**新文本。
///
/// 主题文件的写入口只有这一个 —— 选项页改一个颜色、脚本 `style:set` 想持久化，
/// 最后都落到「某一段的某个键变成某个值」。重写整篇再落盘是不行的：主题文件是
/// 人写的，`;;` 注释、分组空行、键的原始拼写（`Caret=` 和 `editor.caret=` 都能
/// 认）都得原样留着，所以这里只动**一行**。
///
/// 「改最后一处同名键、找不到就插在段末、段不在就整段补在文件末尾、新行跟着
/// 原文件的换行符」这些规则都在 [`dg_core::ini::Ini::set`] 一处实现，解析和
/// 写回认的是同一套 —— 不会出现「写进去的读不出来」。
pub fn patch_theme_ini(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut ini = dg_core::ini::Ini::parse(text);
    ini.set(section.trim(), key.trim(), value);
    ini.to_text()
}

/// Parses `#RRGGBB` or
/// `#RRGGBB,#RRGGBB` (bg, fg).
fn parse_theme_color(value: &str) -> Option<ThemeColor> {
    let mut parts = value.split(',');
    let bg = parse_hex_color(parts.next()?.trim())?;
    let fg = match parts.next().map(str::trim) {
        Some(s) if !s.is_empty() => Some(parse_hex_color(s)?),
        _ => None,
    };
    Some(ThemeColor { bg: Some(bg), fg })
}

/// `#RRGGBB` / `#RRGGBB,#RRGGBB` —— `parse_theme_color` 的反方向。
///
/// 写主题的那几处（对话框改色、`+` 另存为、把外部覆盖合成整份）要的是同一个
/// 拼法：两段色写成 `#背景,#前景`，只有一段就只写那一段，两个都没有就是空串
/// （写一个空值等于把键删掉，调用方自己决定要不要写）。
pub fn theme_color_ini(bg: Option<Color>, fg: Option<Color>) -> String {
    let hex = |c: Color| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
    match (bg, fg) {
        (Some(b), Some(f)) => format!("{},{}", hex(b), hex(f)),
        (Some(b), None) => hex(b),
        (None, Some(f)) => hex(f),
        (None, None) => String::new(),
    }
}

/// Parses `#RRGGBB`.
///
/// `pub` because a skin's `[style]` section takes the same colour spelling as a
/// theme `.ini` — one parser means one behaviour, and a skin cannot end up
/// accepting `#RGB` while a theme rejects it.
pub fn parse_hex_color(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::rgb(r, g, b))
}

impl Theme {
    /// How many 自定义标记 slots the palette covers — the same eight the
    /// 使用标记1 … 使用标记8 menu rows address.
    pub const MARKER_SLOTS: usize = 8;

    /// Applies a parsed theme file over
    /// the current theme — keys the
    /// file does not carry keep their
    /// previous values.
    /// 外壳那一整块 —— 弹出菜单、工具条、页签条、状态条、停靠面板、悬浮滚动条 ——
    /// 每装一套主题都重新落回所属那一副面孔的默认值，主题文件写过的键再盖上来。
    ///
    /// 为什么不能只靠 [`Default`] 那一张：随包的 27 份主题**一格都没写**这些键
    /// （它们只写 `Ruler=`），所以浅色主题装完只有正文变白，四条带子还停在出厂的
    /// 深色表上 —— 关掉「深色主题」重启，看见的仍然是黑的。反方向同一个毛病：
    /// 上一套用 `Toolbar=` 写过一组颜色，下一套没写这个键，那组就一路跟着走。
    ///
    /// 深浅的判据是这一套的正文底色（Rec. 601 权重，和系统那一格深浅色用的同一
    /// 条式子），不是某个开关 —— 主题是自己说话的。
    fn apply_chrome(&mut self, file: &ThemeFile) {
        let body = file.bg("Default").unwrap_or(self.background);
        let dark = (u32::from(body.r) * 299 + u32::from(body.g) * 587 + u32::from(body.b) * 114)
            / 1000
            < 128;
        let base = Self::default();
        macro_rules! chrome {
            ($field:ident, $have:expr, $light:expr) => {
                if $have.is_none() {
                    self.$field = if dark {
                        base.$field
                    } else {
                        Color::from_u32($light)
                    };
                }
            };
        }
        // 弹出菜单：面、字、分隔线、选中条、灰掉的字。
        chrome!(menu_bg, file.bg("Menu"), 0x00F2_F2F2);
        chrome!(menu_fg, file.fg("Menu"), 0x001F_1F1F);
        chrome!(menu_separator, file.bg("MenuSeparator"), 0x00D6_D6D6);
        chrome!(menu_hover, file.bg("MenuHover"), 0x00CC_E4F7);
        chrome!(menu_disabled, file.fg("MenuDisabled"), 0x00A6_A6A6);
        // 工具条那一条带子：面、下边线、字的三种状态、按钮框、以及对话框上那片
        // 同族的面。
        chrome!(toolbar_bg, file.bg("Toolbar"), 0x00E7_E9EC);
        chrome!(toolbar_border, file.bg("ToolbarBorder"), 0x00B7_BAC0);
        chrome!(toolbar_fg, file.fg("Toolbar"), 0x002E_3238);
        chrome!(
            toolbar_disabled,
            file.fg("ToolbarDisabled"),
            0x00A3_A7AD
        );
        chrome!(toolbar_hot, file.bg("ToolbarHot"), 0x00D8_DBDF);
        chrome!(toolbar_selected, file.bg("ToolbarSelected"), 0x00C9_CDD2);
        chrome!(toolbar_frame, file.bg("ToolbarFrame"), 0x009A_A0A6);
        chrome!(toolbar_field, file.bg("ToolbarField"), 0x00FD_FDFD);
        // 页签条：带子、活动页、框线、两种字色、尾巴上那四个字形，以及改过 /
        // 悬停那两条状态带（浅底上红色太艳，压暗一档）。
        chrome!(tab_bg, file.bg("Tab"), 0x00DE_E1E6);
        chrome!(tab_active_bg, file.bg("TabActive"), 0x00FD_FDFD);
        chrome!(tab_fg, file.fg("Tab"), 0x005A_5F66);
        chrome!(tab_active_fg, file.fg("TabActive"), 0x0020_2226);
        chrome!(tab_frame, file.bg("TabFrame"), 0x00B0_B4BA);
        chrome!(tab_mark_fg, file.fg("TabMark"), 0x00C0_392B);
        chrome!(tab_hot_band, file.fg("TabHotBand"), 0x00E0_7A00);
        chrome!(tab_glyph_fg, file.fg("TabGlyph"), 0x006B_7178);
        chrome!(tab_glyph_hover, file.fg("TabGlyphHover"), 0x000A_0A0A);
        chrome!(tab_glyph_pressed, file.fg("TabGlyphPressed"), 0x00C0_392B);
        chrome!(tab_glyph_disabled, file.fg("TabGlyphDisabled"), 0x00C4_C8CC);
        // 状态条和停靠面板：浅面上配深色字，分隔线比面亮一档、比字暗一档。
        chrome!(statusbar_bg, file.bg("StatusBar"), 0x00E7_E9EC);
        chrome!(statusbar_fg, file.fg("StatusBar"), 0x0033_373D);
        chrome!(statusbar_divider, file.bg("StatusBarDivider"), 0x00C0_C4C9);
        chrome!(panel_bg, file.bg("Panel"), 0x00E7_E9EC);
        chrome!(panel_fg, file.fg("Panel"), 0x0033_373D);
        chrome!(panel_hover, file.bg("PanelHover"), 0x00DC_E0E5);
        chrome!(panel_active, file.bg("PanelActive"), 0x00CF_D4DA);
        // 悬浮滚动条：条子贴在正文边上，所以面跟着正文走，药丸和箭头反过来。
        chrome!(scrollbar_bg, file.bg("ScrollBar"), 0x00F6_F6F6);
        chrome!(scrollbar_thumb, file.fg("ScrollBar"), 0x00B8_BCC2);
        chrome!(
            scrollbar_thumb_hover,
            file.fg("ScrollBarHover"),
            0x007C_828A
        );
        chrome!(scrollbar_arrow, file.bg("ScrollBarArrow"), 0x00E0_E0E0);
    }

    pub fn apply_theme_file(&mut self, file: &ThemeFile) {
        // 外壳那一整块先按这一套主题的深浅落回默认表，下面写了键的格子再盖上来。
        self.apply_chrome(file);
        // `Default` 是正文底色，**只有它写 `background`**。别再让 `DefaultBgNull`
        // 也写这一格：那个键是「正文下面那片空」的底色（`below_bg`，见 `IconArea`
        // 之后那段）。27 份随包主题里两个键同值，所以把它接过来在出厂主题上看不出
        // 差别 —— 但它排在后面，会把用户手写的 `Default=` 整行盖掉，改色的人只会
        // 看到「写了没用」。
        if let Some(c) = file.bg("Default") {
            self.background = c;
        }
        if let Some(c) = file.fg("Default") {
            self.foreground = c;
        }
        if let Some(c) = file.bg("Selection") {
            // this implementation blends the
            // selection rect at 50%, so
            // the opaque theme color is
            // carried at half alpha
            self.selection_bg = Color::rgba(c.r, c.g, c.b, 0x80);
        }
        // 只取显式写出的第二色，不走 `fg()` 那条「没有 fg 就退回 bg」：
        // `Selection=#296899` 这种单色写法要是也落到前景，被选中的字就涂在
        // 自己那条底色带上，等于选区里的文字消失。
        if let Some(c) = file.colors.get("Selection").and_then(|c| c.fg) {
            self.selection_fg = c;
        }
        if let Some(c) = file.bg("LineNumber") {
            self.gutter_bg = c;
        }
        if let Some(c) = file.fg("LineNumber") {
            self.gutter_fg = c;
        }
        if let Some(c) = file.bg("Folding") {
            self.folding_bg = c;
        }
        if let Some(c) = file.fg("Folding") {
            self.folding_fg = c;
        }
        if let Some(c) = file.bg("IconArea") {
            self.icon_bg = c;
        }
        if let Some(c) = file.fg("IconArea") {
            self.icon_fg = c;
        }
        if let Some(c) = file.bg("DefaultBgNull") {
            self.below_bg = c;
        }
        if let Some(c) = file.fg("BookMark") {
            self.bookmark_fg = c;
        }
        if let Some(c) = file.fg("Pair") {
            self.tag_fg = c;
        }
        if let Some(c) = file.bg("Pair") {
            self.pair_bg = c;
        }
        if let Some(c) = file.bg("Found") {
            self.found_bg = c;
        }
        if let Some(c) = file.fg("Found") {
            self.found_fg = c;
        }
        if let Some(c) = file.bg("Url") {
            self.url_bg = c;
        }
        if let Some(c) = file.fg("Url") {
            self.url_fg = c;
        }
        if let Some(c) = file.bg("Email") {
            self.email_bg = c;
        }
        if let Some(c) = file.fg("Email") {
            self.email_fg = c;
        }
        if let Some(c) = file.bg("Word2") {
            self.word2_bg = c;
        }
        if let Some(c) = file.fg("Word2") {
            self.word2_fg = c;
        }
        if let Some(c) = file.bg("WordComplete") {
            self.word_complete_bg = c;
        }
        if let Some(c) = file.fg("WordComplete") {
            self.word_complete_fg = c;
        }
        // Custom-drawn UI (dg-ui): optional
        // overrides; dark defaults otherwise.
        if let Some(c) = file.bg("ScrollBar") {
            self.scrollbar_bg = c;
        }
        if let Some(c) = file.fg("ScrollBar") {
            self.scrollbar_thumb = c;
        }
        if let Some(c) = file.bg("ScrollBarArrow") {
            self.scrollbar_arrow = c;
        }
        if let Some(c) = file.fg("ScrollBarHover") {
            self.scrollbar_thumb_hover = c;
        }
        // Overlay geometry. `[Style]` values come from a file on disk, so they
        // are clamped into the range where the bar is still grabbable and
        // still thin — a typo there must not be able to cover the text area.
        // (ScrollBarHoverWidth keeps its historical key name; it is the strip
        // width the pill floats in, and the geometry no longer changes with
        // hover state.)
        if let Some(w) = file.style("ScrollBarWidth") {
            self.scrollbar_thumb_width = w.clamp(2, 40) as f32;
        }
        if let Some(w) = file.style("ScrollBarHoverWidth") {
            self.scrollbar_strip_width = w.clamp(4, 60) as f32;
        }
        if let Some(a) = file.style("ScrollBarAlpha") {
            self.scrollbar_alpha = a.clamp(0x40, 0xFF) as u8;
        }
        if let Some(ms) = file.style("ScrollBarFadeMs") {
            self.scrollbar_fade_ms = ms.clamp(100, 10_000);
        }
        if let Some(c) = file.bg("StatusBar") {
            self.statusbar_bg = c;
        }
        if let Some(c) = file.fg("StatusBar") {
            self.statusbar_fg = c;
        }
        // The shell's own keys. Every one of these colours already existed as a
        // `Theme` field with a built-in default; the keys were the missing half,
        // so a theme could not reach them and the only way to restyle a status
        // bar rule or a tab's hover band was to write a skin. Single-value form
        // works on either accessor: `bg()` falls back to fg and `fg()` to bg, so
        // `StatusBarDivider=#3C3C3C` and `MenuDisabled=#6D6D6D` both read.
        if let Some(c) = file.bg("StatusBarDivider") {
            self.statusbar_divider = c;
        }
        if let Some(c) = file.bg("Panel") {
            self.panel_bg = c;
        }
        if let Some(c) = file.fg("Panel") {
            self.panel_fg = c;
        }
        if let Some(c) = file.bg("PanelHover") {
            self.panel_hover = c;
        }
        if let Some(c) = file.bg("PanelActive") {
            self.panel_active = c;
        }
        if let Some(c) = file.fg("Match") {
            self.match_fg = c;
        }
        if let Some(c) = file.bg("Menu") {
            self.menu_bg = c;
        }
        if let Some(c) = file.fg("Menu") {
            self.menu_fg = c;
        }
        if let Some(c) = file.bg("MenuSeparator") {
            self.menu_separator = c;
        }
        if let Some(c) = file.bg("MenuHover") {
            self.menu_hover = c;
        }
        if let Some(c) = file.fg("MenuDisabled") {
            self.menu_disabled = c;
        }
        if let Some(c) = file.bg("Toolbar") {
            self.toolbar_bg = c;
        }
        if let Some(c) = file.bg("ToolbarBorder") {
            self.toolbar_border = c;
        }
        if let Some(c) = file.fg("Toolbar") {
            self.toolbar_fg = c;
        }
        if let Some(c) = file.fg("ToolbarDisabled") {
            self.toolbar_disabled = c;
        }
        if let Some(c) = file.bg("ToolbarHot") {
            self.toolbar_hot = c;
        }
        if let Some(c) = file.bg("ToolbarSelected") {
            self.toolbar_selected = c;
        }
        if let Some(c) = file.bg("ToolbarFrame") {
            self.toolbar_frame = c;
        }
        if let Some(c) = file.bg("ToolbarField") {
            self.toolbar_field = c;
        }
        if let Some(c) = file.bg("Tab") {
            self.tab_bg = c;
        }
        if let Some(c) = file.bg("TabActive") {
            self.tab_active_bg = c;
        }
        if let Some(c) = file.fg("Tab") {
            self.tab_fg = c;
        }
        if let Some(c) = file.fg("TabActive") {
            self.tab_active_fg = c;
        }
        // The strip's outline and its two state bands. `TabMark` is the band a
        // modified tab wears, `TabHotBand` the one a hovered tab wears instead
        // (it wins over the state band, selected or not).
        if let Some(c) = file.bg("TabFrame") {
            self.tab_frame = c;
        }
        if let Some(c) = file.fg("TabMark") {
            self.tab_mark_fg = c;
        }
        if let Some(c) = file.fg("TabHotBand") {
            self.tab_hot_band = c;
        }
        // The tail glyphs (◀ ▶ ▾ ✕) and the tab's own close box, one key per
        // state the control reports.
        if let Some(c) = file.fg("TabGlyph") {
            self.tab_glyph_fg = c;
        }
        if let Some(c) = file.fg("TabGlyphHover") {
            self.tab_glyph_hover = c;
        }
        if let Some(c) = file.fg("TabGlyphPressed") {
            self.tab_glyph_pressed = c;
        }
        if let Some(c) = file.fg("TabGlyphDisabled") {
            self.tab_glyph_disabled = c;
        }
        if let Some(c) = file.bg("Ruler") {
            self.margin_bg = c;
        }
        if let Some(c) = file.fg("Ruler") {
            self.ruler_fg = c;
        }
        if let Some(c) = file.bg("IndentLine") {
            self.indent_line = c;
        }
        // 符号显示: the marks' own key, the boundary separator, and the control
        // character's cell. A theme that predates them keeps the defaults.
        if let Some(c) = file.fg("WhiteSpace") {
            self.whitespace_fg = c;
        }
        if let Some(c) = file.bg("BoundaryLine") {
            self.boundary_line = c;
        }
        if let Some(c) = file.bg("ConChar") {
            self.con_char_bg = c;
        }
        if let Some(c) = file.fg("ConChar") {
            self.con_char_fg = c;
        }
        if let Some(c) = file.bg("ActiveLine") {
            self.current_line_bg = Color::rgba(c.r, c.g, c.b, 0xFF);
        }
        if let Some(c) = file.fg("Caret").or_else(|| file.bg("Caret")) {
            self.caret = c;
            // The halo follows the caret unless the theme names its own: a
            // theme written before `CaretGlow` existed still gets a sane one.
            self.caret_glow = Color::rgba(c.r, c.g, c.b, 90);
        }
        if let Some(c) = file.fg("CaretGlow").or_else(|| file.bg("CaretGlow")) {
            self.caret_glow = c;
        }
        if let Some(c) = file.fg("SelIndicator").or_else(|| file.bg("SelIndicator")) {
            self.sel_indicator = c;
        }
        // 最后一遍：**token 名字**。`[Theme]` 里写 `editor.caret=#FF0000` 和
        // `Caret=#FF0000` 等价，但 token 名字是唯一完整的那一套 —— 89 个字段
        // 全在，而上面那些 CamelCase 老键只覆盖 74 个（漏的是行修改状态、
        // 小地图视口和八对书签标记色，它们以前在主题文件里根本改不了）。
        //
        // 必须放在**函数最后**：`Caret` / `CaretGlow` / `ActiveLine` 这几组老键
        // 自己也在后半段，插在中间会被它们盖掉。
        //
        // 这条循环保证 token 表里没有一个名字是主题文件写不到的：每个名字
        // 都过一遍 `file.color(name)`，不存在「脚本能改、主题改不了」的那一格。
        // token 键是**单色**：写 `#RRGGBB` 就够；写了两色取第一个。
        for name in crate::tokens::TOKEN_NAMES {
            if let Some(c) = file.color(name) {
                self.set_token(name, c.bg.or(c.fg).unwrap_or_default());
            }
        }
    }
}

pub fn blend_channel(a: u8, b: u8, wa: f32) -> u8 {
    let v = (a as f32) * wa + (b as f32) * (1.0 - wa);
    v.round().clamp(0.0, 255.0) as u8
}

/// Luma-weighted blend of two colors.
pub fn luma_blend(c1: Color, c2: Color, wa: f32) -> Color {
    Color::rgba(
        blend_channel(c1.r, c2.r, wa),
        blend_channel(c1.g, c2.g, wa),
        blend_channel(c1.b, c2.b, wa),
        blend_channel(c1.a, c2.a, wa),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设置行高的折算：额外行距折进行步进，标准值与内容偏移能往返取回。
    /// 1.4 倍的标准 18 px 行高 → 额外 7 px，行步进 25，内容下移 3 px
    /// （7 的一半整除）。标准值在动额外行距**之前**取——量出来的行高
    /// 那一刻还是标准值。
    #[test]
    fn line_spacing_folds_into_the_row_pitch_and_back() {
        let mut t = Theme::default();
        t.line_height = 18.0;
        assert_eq!(t.standard_line_height(), 18.0, "0 额外行距是恒等");
        assert_eq!(t.content_offset(), 0.0);
        let standard = t.standard_line_height();
        t.line_spacing = 7;
        t.apply_line_spacing(standard);
        assert_eq!(t.line_height, 25.0);
        assert_eq!(t.standard_line_height(), 18.0);
        assert_eq!(t.content_offset(), 3.0);
    }

    #[test]
    fn u32_roundtrip() {
        let c = Color::from_u32(0x569CD6);
        assert_eq!(c.r, 0x56);
        assert_eq!(c.g, 0x9C);
        assert_eq!(c.b, 0xD6);
        assert_eq!(c.to_u32(), 0x569CD6);
    }

    #[test]
    fn luma_weights() {
        let c = luma_blend(Color::rgb(255, 255, 255), Color::rgb(0, 0, 0), 0.5);
        assert_eq!(c.r, 128);
        let c = luma_blend(Color::rgb(255, 0, 0), Color::rgb(0, 0, 0), 0.294);
        assert_eq!(c.r, 75); // 255 * 0.294 = 74.97
    }

    #[test]
    fn caret_glow_follows_the_caret_until_the_theme_names_its_own() {
        let mut t = Theme::default();
        // A theme written before `CaretGlow` existed: the 柔光 halo is the
        // caret's own colour at 35 %, so an old theme still gets a sane one.
        t.apply_theme_file(&parse_theme_ini("[Theme]\nCaret=#E6E9EF\n"));
        assert_eq!((t.caret.r, t.caret.g, t.caret.b), (0xE6, 0xE9, 0xEF));
        assert_eq!(
            (t.caret_glow.r, t.caret_glow.g, t.caret_glow.b),
            (0xE6, 0xE9, 0xEF)
        );
        assert_eq!(t.caret_glow.a, 90);

        //...and an explicit key wins, fg or bg.
        t.apply_theme_file(&parse_theme_ini(
            "[Theme]\nCaret=#E6E9EF\nCaretGlow=#FF0000\n",
        ));
        assert_eq!((t.caret_glow.r, t.caret_glow.g, t.caret_glow.b), (0xFF, 0, 0));
    }
}


#[cfg(test)]
mod theme_file_tests {
    use super::*;

    const DOGEDIT_DARK: &str = "\
[Theme]
Default=#1E1F22,#D4D7DD
Selection=#2F4A63,#EAEDF2
LineNumber=#1B1C1F,#666E80
BookMark=#1B1C1F,#82AAFF
Pair=#1E1F22,#FFCB6B
Url=#1E1F22,#80CBC4
Email=#1E1F22,#008080
Ruler=#24262B,#4A5162
ActiveLine=#26282E
Caret=#E6E9EF
SelIndicator=#F78C6C
DefaultBgNull=#1E1F22
[Style]
Pair=257
Comment1=2
";

    /// 写出去再读回来还是同一个颜色 —— 对话框改色、`+` 另存为都靠这一对。
    #[test]
    fn theme_color_ini_round_trips() {
        let file = parse_theme_ini(DOGEDIT_DARK);
        for key in ["Default", "ActiveLine", "Caret"] {
            let c = file.color(key).unwrap();
            let text = theme_color_ini(c.bg, c.fg);
            let back = parse_theme_ini(&format!("[Theme]\n{key}={text}\n"))
                .color(key)
                .unwrap()
                .clone();
            assert_eq!((back.bg, back.fg), (c.bg, c.fg), "{key} 写出去是 {text}");
        }
        assert_eq!(theme_color_ini(None, None), "", "没有颜色就没有值");
        assert_eq!(
            theme_color_ini(Some(Color::rgb(0x0A, 0x0B, 0x0C)), None),
            "#0A0B0C",
            "大写、补零"
        );
    }

    #[test]
    fn parses_theme_pairs_and_singles() {        let file = parse_theme_ini(DOGEDIT_DARK);
        let default = file.color("Default").unwrap();
        assert_eq!(default.bg, Some(Color::rgb(0x1E, 0x1F, 0x22)));
        assert_eq!(default.fg, Some(Color::rgb(0xD4, 0xD7, 0xDD)));
        // single-color form
        let active = file.color("ActiveLine").unwrap();
        assert_eq!(active.bg, Some(Color::rgb(0x26, 0x28, 0x2E)));
        assert_eq!(active.fg, None);
        // fg falls back to bg
        assert_eq!(
            file.fg("ActiveLine"),
            Some(Color::rgb(0x26, 0x28, 0x2E))
        );
        // style flags
        assert_eq!(file.styles.get("Pair"), Some(&257));
    }

    #[test]
    fn overlay_bar_geometry_and_hover_color_come_from_the_ini() {
        let d = Theme::default();
        assert_eq!(d.scrollbar_alpha, 0x9A, "the resting pill is translucent");
        assert_eq!(d.scrollbar_thumb, Color::rgb(0x54, 0x54, 0x54));
        assert_eq!(d.scrollbar_thumb_hover, Color::rgb(0xC8, 0xC8, 0xC8));

        let file = parse_theme_ini(
            "\
[Theme]
ScrollBar=#101010,#808080
ScrollBarHover=#101010,#C0C0C0
[Style]
ScrollBarWidth=6
ScrollBarHoverWidth=20
ScrollBarAlpha=200
ScrollBarFadeMs=1200
",
        );
        let mut theme = Theme::default();
        theme.apply_theme_file(&file);
        assert_eq!(theme.scrollbar_thumb, Color::rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.scrollbar_thumb_hover, Color::rgb(0xC0, 0xC0, 0xC0));
        assert_eq!(theme.scrollbar_thumb_width, 6.0);
        assert_eq!(theme.scrollbar_strip_width, 20.0);
        assert_eq!(theme.scrollbar_alpha, 200);
        assert_eq!(theme.scrollbar_fade_ms, 1200);

        // A hand-edited ini must not be able to turn the bar into a slab over
        // the text, or to make it invisible.
        let wild = parse_theme_ini("[Style]\nScrollBarWidth=4000\nScrollBarAlpha=0\n");
        let mut theme = Theme::default();
        theme.apply_theme_file(&wild);
        assert_eq!(theme.scrollbar_thumb_width, 40.0);
        assert_eq!(theme.scrollbar_alpha, 0x40);
    }

    #[test]
    fn applies_over_defaults() {
        let file = parse_theme_ini(DOGEDIT_DARK);
        let mut theme = Theme::default();
        theme.apply_theme_file(&file);
        assert_eq!(theme.background, Color::rgb(0x1E, 0x1F, 0x22));
        assert_eq!(theme.foreground, Color::rgb(0xD4, 0xD7, 0xDD));
        assert_eq!(theme.gutter_fg, Color::rgb(0x66, 0x6E, 0x80));
        assert_eq!(theme.bookmark_fg, Color::rgb(0x82, 0xAA, 0xFF));
        assert_eq!(theme.tag_fg, Color::rgb(0xFF, 0xCB, 0x6B));
        assert_eq!(theme.pair_bg, Color::rgb(0x1E, 0x1F, 0x22));
        assert_eq!(theme.url_bg, Color::rgb(0x1E, 0x1F, 0x22));
        assert_eq!(theme.url_fg, Color::rgb(0x80, 0xCB, 0xC4));
        assert_eq!(theme.email_bg, Color::rgb(0x1E, 0x1F, 0x22));
        assert_eq!(theme.email_fg, Color::rgb(0x00, 0x80, 0x80));
        assert_eq!(theme.margin_bg, Color::rgb(0x24, 0x26, 0x2B));
        assert_eq!(theme.ruler_fg, Color::rgb(0x4A, 0x51, 0x62));
        assert_eq!(theme.indent_line, Color::rgb(0x33, 0x36, 0x3D));
        assert_eq!(theme.caret, Color::rgb(0xE6, 0xE9, 0xEF));
        // the multi-caret indicator line
        assert_eq!(theme.sel_indicator, Color::rgb(0xF7, 0x8C, 0x6C));
        // selection keeps half alpha
        assert_eq!(theme.selection_bg.a, 0x80);
        assert_eq!(theme.selection_bg.r, 0x2F);
    }

    /// The 保底, in the shape every bundled theme actually has: a mid grey named
    /// beside a light grey caret. That pair reads as one caret and a spare, so the
    /// mark takes the stock hue at the theme's own brightness.
    #[test]
    fn a_grey_indicator_becomes_the_stock_hue_at_its_brightness() {
        let mut theme = Theme::default();
        // DogEdit_Dark's two values
        theme.caret = Color::from_u32(0xD9DFE8);
        theme.sel_indicator = Color::from_u32(0x787C83);
        let mark = theme.caret_indicator();
        assert_ne!(mark, theme.sel_indicator, "the grey reads as a second caret");
        assert_ne!(mark, theme.caret);
        assert!(mark.r > mark.g && mark.g > mark.b, "amber, not grey: {mark:?}");
        // the theme's brightness carries over, so it neither glares nor vanishes
        let l = 3 * mark.r as u32 + 6 * mark.g as u32 + mark.b as u32;
        let want = 3 * 0x78 + 6 * 0x7C + 0x83;
        assert!(l.abs_diff(want) <= want / 4, "brightness {l}, theme's own {want}");
    }

    /// A theme that names a colour of its own hue is honoured exactly.
    #[test]
    fn a_chromatic_indicator_is_honoured_as_written() {
        let mut theme = Theme::default();
        theme.caret = Color::from_u32(0xD9DFE8);
        theme.sel_indicator = Color::from_u32(0xC11B15);
        assert_eq!(theme.caret_indicator(), Color::from_u32(0xC11B15));
    }

    /// The floor: a theme that names near-black does not get a near-black mark,
    /// which would be the same invisibility the 保底 exists to remove.
    #[test]
    fn a_near_black_indicator_is_kept_readable() {
        let mut theme = Theme::default();
        theme.caret = Color::from_u32(0xD9DFE8);
        theme.sel_indicator = Color::from_u32(0x000000);
        let mark = theme.caret_indicator();
        assert!(3 * mark.r as u32 + 6 * mark.g as u32 + mark.b as u32 > 400, "{mark:?}");
        assert!(mark.r > mark.b, "still the stock hue at the floor brightness");
    }

    #[test]
    fn sel_indicator_defaults_to_the_ctor_value() {
        // A theme file without the key keeps the default.
        assert_eq!(Theme::default().sel_indicator, Color::rgb(0xFF, 0xA5, 0x00));
        let file = parse_theme_ini("[Theme]\nCaret=#000000\n");
        let mut theme = Theme::default();
        theme.apply_theme_file(&file);
        assert_eq!(theme.sel_indicator, Color::rgb(0xFF, 0xA5, 0x00));
        // and a theme file's own value lands through the key
        let light = parse_theme_ini("[Theme]\nSelIndicator=#FF8000\n");
        theme.apply_theme_file(&light);
        assert_eq!(theme.sel_indicator, Color::rgb(0xFF, 0x80, 0x00));
    }

    #[test]
    fn skips_comments_and_unknown_sections() {
        let text = ";; header comment
[Theme]
Default=#FFFFFF,#000000
[Other]
Default=#123456
";
        let file = parse_theme_ini(text);
        assert_eq!(file.colors.len(), 1);
        assert_eq!(file.fg("Default"), Some(Color::rgb(0, 0, 0)));
    }

    #[test]
    fn bad_colors_are_ignored() {
        let text = "[Theme]
Default=notacolor
Pair=#12345
BookMark=#GGGGGG
Caret=#FF0000
";
        let file = parse_theme_ini(text);
        assert_eq!(file.colors.len(), 1);
        assert_eq!(file.fg("Caret"), Some(Color::rgb(0xFF, 0, 0)));
    }

    #[test]
    fn every_token_can_be_set_from_the_ini() {
        // 「主题文件能表达的东西」必须覆盖「皮肤能表达的东西」—— 两边是同一套
        // token 名字，差一个就是「皮肤里写了有效、主题里写了没反应」。
        // 这条测试逐个数一遍，漏一个就红。
        for name in crate::tokens::TOKEN_NAMES {
            let text = format!("[Theme]\n{name}=#010203\n");
            let file = parse_theme_ini(&text);
            let mut theme = Theme::default();
            theme.apply_theme_file(&file);
            assert_eq!(
                theme.token(name),
                Some(Color::rgb(1, 2, 3)),
                "{name} 从主题文件里设不进去"
            );
        }
    }

    #[test]
    fn token_names_beat_the_camel_case_keys() {
        // 同一个文件里两种写法并存时，后写的 token 名字赢 —— 否则
        // 「老键设了一次、token 又设了一次」的结果取决于源码里的顺序。
        let text = "[Theme]\nCaret=#FF0000\neditor.caret=#00FF00\n";
        let mut theme = Theme::default();
        theme.apply_theme_file(&parse_theme_ini(text));
        assert_eq!(theme.caret, Color::rgb(0, 0xFF, 0));
    }

    #[test]
    fn metrics_section_is_parsed_and_bad_values_are_dropped() {
        let text = "[Metrics]
status.height=26
tab.height=30.5
toolbar.icon=24
menu.pad=oops
split.band_h=
status.pad=NaN
";
        let file = parse_theme_ini(text);
        // The four that parse land; `oops`, an empty value and a NaN do not.
        // A NaN is rejected rather than stored because the clamp in
        // `dg_skin::Metrics` is `f32::clamp`, which propagates NaN.
        assert_eq!(file.metric("status.height"), Some(26.0));
        assert_eq!(file.metric("tab.height"), Some(30.5));
        assert_eq!(file.metric("toolbar.icon"), Some(24.0));
        assert_eq!(file.metric("menu.pad"), None);
        assert_eq!(file.metric("split.band_h"), None);
        assert_eq!(file.metric("status.pad"), None);
        assert_eq!(file.metrics.len(), 3);
    }

    #[test]
    fn shape_and_font_sections_are_kept_verbatim() {
        // 这两个段的值**不在这里解析**：`[Shape]` 的键表在 `dg-skin` 手里，
        // 而 `font` 这一项本来就是名字不是数字。所以这里只保证「原文进来了、
        // 键里的点没被吃掉」—— 包括 `dock.tab` 这种自带点的区域名。
        let text = "[Shape]
tab.radius=4
tab.states.active.radius=0
tab.states.active.font=font.ui
dock.tab.gap=2

[Fonts]
mono.face=Consolas
mono.size=11
mono.italic=true
";
        let file = parse_theme_ini(text);
        assert_eq!(file.shape("tab.radius"), Some("4"));
        assert_eq!(file.shape("tab.states.active.radius"), Some("0"));
        assert_eq!(file.shape("tab.states.active.font"), Some("font.ui"));
        assert_eq!(file.shape("dock.tab.gap"), Some("2"));
        assert_eq!(file.font("mono.face"), Some("Consolas"));
        assert_eq!(file.font("mono.size"), Some("11"));
        assert_eq!(file.font("mono.italic"), Some("true"));
        assert_eq!(file.shapes.len(), 4);
        assert_eq!(file.fonts.len(), 3);
        // 两个新段不串到颜色/度量里去。
        assert!(file.colors.is_empty());
        assert!(file.metrics.is_empty());
    }

    #[test]
    fn a_bad_shape_value_is_kept_for_the_layer_that_can_name_it() {
        // 主题文件这边**不做校验**：扔掉只会让用户看到「改了没反应」，而
        // `dg-skin` 能说出「`radius` 要数字，你写的是 `big`」。所以脏值必须
        // 活着到达那一层。
        let file = parse_theme_ini("[Shape]\ntab.radius=big\n[Fonts]\nmono.size=eleven\n");
        assert_eq!(file.shape("tab.radius"), Some("big"));
        assert_eq!(file.font("mono.size"), Some("eleven"));
    }

    #[test]
    fn shape_and_font_entries_merge_key_by_key() {
        // 覆盖文件只写一条时，另一条不能跟着回到出厂值 —— 和颜色/度量同一条规则。
        let mut base = parse_theme_ini("[Shape]\ntab.radius=4\ntab.gap=2\n[Fonts]\nmono.size=11\n");
        let over = parse_theme_ini("[Shape]\ntab.radius=0\n[Fonts]\nmono.face=Consolas\n");
        base.merge(&over);
        assert_eq!(base.shape("tab.radius"), Some("0"), "覆盖的赢");
        assert_eq!(base.shape("tab.gap"), Some("2"), "没写的留着");
        assert_eq!(base.font("mono.size"), Some("11"), "另一段不受影响");
        assert_eq!(base.font("mono.face"), Some("Consolas"));
    }

    #[test]
    fn metrics_do_not_touch_colors_or_styles() {
        // `[Metrics]` is a third table, not a spelling of `[Style]`: a file
        // that carries only metrics must not be mistaken for one that carries
        // font flags, and `colors` stays empty so the caller can tell "this
        // file says nothing about colour" from "this file paints everything".
        let file = parse_theme_ini("[Metrics]\nstatus.height=26\n");
        assert!(file.colors.is_empty());
        assert!(file.styles.is_empty());
        assert_eq!(file.metrics.len(), 1);
    }

    #[test]
    fn merge_keeps_unmentioned_keys_and_lets_the_override_win() {
        let mut base = parse_theme_ini(
            "\
[Theme]
Default=#1E1F22,#D4D7DD
StatusBar=#111318,#C8CDD6
[Style]
ScrollBarWidth=8
[metrics]
status.height=23
",
        );
        // A user override that names one colour and one metric — the shape
        // `themes/<name>.ini` takes when someone tweaks a shipped theme.
        let over = parse_theme_ini(
            "\
[Theme]
StatusBar=#000000
[Metrics]
status.height=26
",
        );
        base.merge(&over);

        // The override wins where it speaks...
        assert_eq!(base.bg("StatusBar"), Some(Color::rgb(0, 0, 0)));
        assert_eq!(base.metric("status.height"), Some(26.0));
        // ...and the bundled values survive where it does not. This is the
        // whole point: before `merge`, the override *replaced* the bundled
        // file, so `Default` fell back to `Theme::default` and a metrics-only
        // override was rejected outright for having no colours.
        assert_eq!(base.bg("Default"), Some(Color::rgb(0x1E, 0x1F, 0x22)));
        assert_eq!(base.fg("Default"), Some(Color::rgb(0xD4, 0xD7, 0xDD)));
        assert_eq!(base.style("ScrollBarWidth"), Some(8));
    }

    /// 手写一行 `Default=` 必须真的改到正文底色。
    ///
    /// 曾经改不到：`DefaultBgNull`（正文下面那片空的底色）在 `apply_theme_file`
    /// 后半段也往 `background` 写了一次，而它排在 `Default` 之后。27 份随包主题
    /// 里两个键同值，所以出厂主题上看不出任何差别 —— 只有真的去改色的人会看到
    /// 「写了没用」。这条测试用**真的**随包主题，就是为了守住那个同值巧合。
    #[test]
    fn a_hand_written_default_reaches_the_background() {
        let mut file = parse_theme_ini(include_str!("../../../themes/DogEdit_Dark.ini"));
        assert_ne!(
            file.bg("Default").map(|c| (c.r, c.g, c.b)),
            Some((0x7F, 0x00, 0x00)),
            "随包主题自己不该是红的"
        );
        file.merge(&parse_theme_ini("[Theme]\r\nDefault=#7F0000,#FFFFFF\r\n"));
        let mut theme = Theme::default();
        theme.apply_theme_file(&file);
        assert_eq!(
            (theme.background.r, theme.background.g, theme.background.b),
            (0x7F, 0x00, 0x00),
            "外部文件写的 Default 要落到正文底色上"
        );
        // 正文下面那片空跟自己的键走，不跟着 `Default` 跑。
        assert_eq!(
            (theme.below_bg.r, theme.below_bg.g, theme.below_bg.b),
            (0x1E, 0x21, 0x26),
            "DefaultBgNull 仍然只写 below_bg"
        );
    }

    #[test]
    fn merge_of_a_metrics_only_override_still_yields_a_theme() {
        let mut base = parse_theme_ini("[Theme]\nDefault=#1E1F22,#D4D7DD\n");
        base.merge(&parse_theme_ini("[Metrics]\nstatus.height=26\n"));
        assert!(!base.colors.is_empty(), "the base still supplies the colours");
        let mut theme = Theme::default();
        theme.apply_theme_file(&base);
        assert_eq!(theme.background, Color::rgb(0x1E, 0x1F, 0x22));
    }

    #[test]
    fn shell_keys_reach_their_fields() {
        let file = parse_theme_ini(
            "\
[Theme]
StatusBarDivider=#3C3C3C
PanelHover=#2A2D2E
PanelActive=#37373D
MenuHover=#094774
MenuDisabled=#6D6D6D
TabFrame=#919B9C
TabMark=#FF0000
TabHotBand=#FF8000
TabGlyph=#808080
TabGlyphHover=#000000
TabGlyphPressed=#FF0000
TabGlyphDisabled=#C0C0C0
",
        );
        let mut theme = Theme::default();
        // Every one of these keys was missing before: the field existed and the
        // painters read it, so the only way to change a status-bar rule or a
        // tab's hover band was a skin.
        theme.apply_theme_file(&file);
        assert_eq!(theme.statusbar_divider, Color::rgb(0x3C, 0x3C, 0x3C));
        assert_eq!(theme.panel_hover, Color::rgb(0x2A, 0x2D, 0x2E));
        assert_eq!(theme.panel_active, Color::rgb(0x37, 0x37, 0x3D));
        assert_eq!(theme.menu_hover, Color::rgb(0x09, 0x47, 0x74));
        assert_eq!(theme.menu_disabled, Color::rgb(0x6D, 0x6D, 0x6D));
        assert_eq!(theme.tab_frame, Color::rgb(0x91, 0x9B, 0x9C));
        assert_eq!(theme.tab_mark_fg, Color::rgb(0xFF, 0x00, 0x00));
        assert_eq!(theme.tab_hot_band, Color::rgb(0xFF, 0x80, 0x00));
        assert_eq!(theme.tab_glyph_fg, Color::rgb(0x80, 0x80, 0x80));
        assert_eq!(theme.tab_glyph_hover, Color::rgb(0x00, 0x00, 0x00));
        assert_eq!(theme.tab_glyph_pressed, Color::rgb(0xFF, 0x00, 0x00));
        assert_eq!(theme.tab_glyph_disabled, Color::rgb(0xC0, 0xC0, 0xC0));
    }

    #[test]
    fn a_theme_without_the_new_shell_keys_keeps_the_builtin_values() {
        // The pixel baseline: a bundled theme written before these keys existed
        // must paint exactly what it painted before.
        let mut theme = Theme::default();
        theme.apply_theme_file(&parse_theme_ini(DOGEDIT_DARK));
        let d = Theme::default();
        assert_eq!(theme.statusbar_divider, d.statusbar_divider);
        assert_eq!(theme.panel_hover, d.panel_hover);
        assert_eq!(theme.panel_active, d.panel_active);
        assert_eq!(theme.menu_hover, d.menu_hover);
        assert_eq!(theme.menu_disabled, d.menu_disabled);
        assert_eq!(theme.tab_frame, d.tab_frame);
        assert_eq!(theme.tab_mark_fg, d.tab_mark_fg);
        assert_eq!(theme.tab_hot_band, d.tab_hot_band);
        assert_eq!(theme.tab_glyph_fg, d.tab_glyph_fg);
        assert_eq!(theme.tab_glyph_hover, d.tab_glyph_hover);
        assert_eq!(theme.tab_glyph_pressed, d.tab_glyph_pressed);
        assert_eq!(theme.tab_glyph_disabled, d.tab_glyph_disabled);
    }

    // ── patch_theme_ini ────────────────────────────────────────────────

    /// 改一个已经写着的键：只动那一行，注释、空行、别的段一字不动。
    #[test]
    fn patching_a_key_touches_exactly_one_line() {
        let src = ";; 我的主题\n[Theme]\nCaret=#E6E9EF\n\n;; 页签\nTab=#2B2B2B,#B0BEC5\n[Style]\nPair=257\n";
        let out = patch_theme_ini(src, "Theme", "Caret", "#FF0000");
        assert_eq!(
            out,
            ";; 我的主题\n[Theme]\nCaret=#FF0000\n\n;; 页签\nTab=#2B2B2B,#B0BEC5\n[Style]\nPair=257\n"
        );
        // 写进去的必须读得出来 —— 拼写对不上就是「改了没反应」。
        assert_eq!(
            parse_theme_ini(&out).color("Caret").unwrap().bg,
            Some(Color::rgb(0xFF, 0, 0))
        );
    }

    /// 键还没有：追加在这一段**末尾**，不是文件末尾 —— 否则它跑到 `[Style]`
    /// 底下，解析器按段读，等于没写。
    #[test]
    fn a_new_key_lands_at_the_end_of_its_section() {
        let src = "[Theme]\nCaret=#E6E9EF\n[Style]\nPair=257\n";
        let out = patch_theme_ini(src, "Theme", "editor.bg", "#101010");
        assert_eq!(out, "[Theme]\nCaret=#E6E9EF\neditor.bg=#101010\n[Style]\nPair=257\n");
        assert_eq!(
            parse_theme_ini(&out).color("editor.bg").unwrap().bg,
            Some(Color::rgb(0x10, 0x10, 0x10))
        );
    }

    /// 段都不在：整段补在末尾，前面留一个空行。
    #[test]
    fn a_missing_section_is_appended_whole() {
        let src = "[Theme]\nCaret=#E6E9EF\n";
        let out = patch_theme_ini(src, "Metrics", "status.height", "26");
        assert_eq!(out, "[Theme]\nCaret=#E6E9EF\n\n[Metrics]\nstatus.height=26\n");
        assert_eq!(parse_theme_ini(&out).metrics.get("status.height"), Some(&26.0));
    }

    /// 空段也要能写：新行插在段头后面，并且跟着原文件的换行符。
    #[test]
    fn an_empty_section_still_takes_a_key() {
        let src = "[Theme]\r\n[Fonts]\r\n";
        let out = patch_theme_ini(src, "Fonts", "mono.face", "Consolas");
        assert_eq!(out, "[Theme]\r\n[Fonts]\r\nmono.face=Consolas\r\n");
        assert!(!out.contains("\n\n"), "CRLF 文件里不该混进裸 LF：{out:?}");
    }

    /// 同一个键写了两遍：改**最后**一个，因为解析器认的也是最后一个。
    #[test]
    fn a_duplicate_key_patches_the_last_one() {
        let src = "[Theme]\nCaret=#111111\nCaret=#222222\n";
        let out = patch_theme_ini(src, "Theme", "Caret", "#333333");
        assert_eq!(out, "[Theme]\nCaret=#111111\nCaret=#333333\n");
        assert_eq!(
            parse_theme_ini(&out).color("Caret").unwrap().bg,
            Some(Color::rgb(0x33, 0x33, 0x33))
        );
    }

    /// 段名不分大小写：文件里写 `[theme]` 也得认，并且别再加一个 `[Theme]`。
    #[test]
    fn the_section_name_is_case_insensitive() {
        let out = patch_theme_ini("[theme]\nCaret=#111111\n", "Theme", "Caret", "#222222");
        assert_eq!(out, "[theme]\nCaret=#222222\n");
        assert!(!out.contains("[Theme]"));
    }

    /// 缩进和行尾空白原样保留：主题文件是人排过版的。
    #[test]
    fn indentation_survives_the_rewrite() {
        let out = patch_theme_ini("[Theme]\n  Caret = #111111\n", "Theme", "Caret", "#222222");
        assert_eq!(out, "[Theme]\n  Caret=#222222\n");
    }

    fn light_and_dark() -> (ThemeFile, ThemeFile) {
        (
            parse_theme_ini("[Theme]\nDefault=#1E1E1E,#E2E8F0\n"),
            parse_theme_ini("[Theme]\nDefault=#FBFBFB,#202226\n"),
        )
    }

    /// 外壳那几格跟着这一套主题的深浅走。随包的 27 份主题一格都没写它们，所以
    /// 浅色主题装完必须是浅带子；换回深色又得回到出厂那张表，不能把上一套的浅色
    /// 留在下面。
    #[test]
    fn chrome_follows_the_theme_own_darkness() {
        let (dark, light) = light_and_dark();
        let factory = Theme::default();
        let mut t = Theme::default();
        t.apply_theme_file(&dark);
        assert_eq!(t.toolbar_bg, factory.toolbar_bg);
        assert_eq!(t.menu_bg, factory.menu_bg);
        assert_eq!(t.statusbar_bg, factory.statusbar_bg);
        t.apply_theme_file(&light);
        for (name, c) in [
            ("toolbar", t.toolbar_bg),
            ("menu", t.menu_bg),
            ("tab", t.tab_bg),
            ("status", t.statusbar_bg),
            ("panel", t.panel_bg),
            ("scrollbar", t.scrollbar_thumb),
        ] {
            assert!(u32::from(c.r) > 0x80, "{name} chrome stays dark: {c:?}");
        }
        // 字的深浅反过来，不然浅面上是浅字。
        assert!(u32::from(t.toolbar_fg.r) < 0x80);
        assert!(u32::from(t.menu_fg.r) < 0x80);
        assert!(u32::from(t.statusbar_fg.r) < 0x80);
        t.apply_theme_file(&dark);
        assert_eq!(t.toolbar_bg, factory.toolbar_bg);
        assert_eq!(t.tab_bg, factory.tab_bg);
    }

    /// 写过的那一格仍然以主题文件为准 —— 默认表只管没写的。
    #[test]
    fn a_written_chrome_key_beats_the_default_table() {
        let (dark, _) = light_and_dark();
        let mut file = dark;
        file.colors.insert(
            "Toolbar".into(),
            ThemeColor::single(Color::from_u32(0x123456)),
        );
        let mut t = Theme::default();
        t.apply_theme_file(&file);
        assert_eq!(t.toolbar_bg.to_u32(), 0x123456);
    }
}

