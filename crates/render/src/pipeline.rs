use crate::component::{Rect, RenderComponent, PaintCtx};
use crate::theme::{Color, Theme};
use crate::viewflags::ViewFlags;
use dg_core::{Document, SelectionSet};
use dg_layout::wrap::WrapArray;
use dg_paint::{
    HighlightRun, STYLE_BOX, STYLE_DOTTED, STYLE_DOUBLE, STYLE_STRIKE, STYLE_UNDERLINE, STYLE_WAVY,
};

/// The fold-end indicator's colours: the box fill and the three dots drawn
/// after a folded opener's text.
const FOLD_END_FILL: Color = Color::rgb(0xF1, 0xF1, 0x46);
const FOLD_END_DOT: Color = Color::rgb(0x94, 0x94, 0x67);

/// Pixels a row materializes past the clip's right edge. The marks that hang off
/// the end of the text (换行符, `[EOF]`, the fold box) are placed by the pixel of
/// the last character, so a row whose end sits a hair outside the view would lose
/// them to the window if it stopped exactly at the edge.
const WINDOW_SLACK_PX: i32 = 64;

/// How many single-cell glyphs sit before `px` — the ceiling of `px / cell`,
/// i.e. the first column whose boundary is at or past the pixel. A row whose
/// characters are all one cell wide places its window with this instead of
/// measuring the text in front of it.
fn cells_before(px: i32, cell: i32) -> usize {
    if px <= 0 || cell <= 0 {
        return 0;
    }
    ((px as i64 + cell as i64 - 1) / cell as i64) as usize
}

/// Draw commands — the platform backends translate
/// these into TextOutW, ExtTextOutW and
/// AlphaBlend calls.
#[derive(Clone, Debug)]
pub enum DrawCmd {
    FillRect { rect: Rect, color: Color },
    /// `style` 是 `[Style]` 的字形位（`dg_paint::STYLE_BOLD` /
    /// `STYLE_ITALIC`）：后端的字号、行高、每一格的推进量都不看它 —— 网格
    /// 是布局的契约，只有**挑哪张字面**看它。
    TextRun { x: f32, y: f32, text: String, color: Color, style: u8 },
    AlphaBlend { rect: Rect, color: Color, alpha: f32 },
}

/// Accumulated draw list. The `token_buf` /
/// `flag_buf` fields are the reused per-view
/// arrays: nothing on the paint path allocates
/// beyond the draw list itself.
pub struct DrawList {
    pub cmds: Vec<DrawCmd>,
    pub token_buf: Vec<HighlightRun>,
    pub flag_buf: Vec<u8>,
}

impl Default for DrawList {
    fn default() -> Self {
        Self {
            cmds: Vec::new(),
            token_buf: Vec::new(),
            flag_buf: Vec::new(),
        }
    }
}

impl DrawList {
    pub fn clear(&mut self) {
        self.cmds.clear();
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.cmds.push(DrawCmd::FillRect { rect, color });
    }

    pub fn text(&mut self, x: f32, y: f32, text: &str, color: Color) {
        self.styled_text(x, y, text, color, 0);
    }

    /// 带字形位的 [`Self::text`] —— 语法着色那一条路走这里。
    pub fn styled_text(&mut self, x: f32, y: f32, text: &str, color: Color, style: u8) {
        self.cmds.push(DrawCmd::TextRun {
            x,
            y,
            text: text.to_string(),
            color,
            style,
        });
    }

    pub fn alpha_blend(&mut self, rect: Rect, color: Color, alpha: f32) {
        self.cmds.push(DrawCmd::AlphaBlend {
            rect,
            color,
            alpha,
        });
    }

    /// A 1 px line, walked as fills: the backends translate rectangles and text
    /// only, and every stroke in the 符号显示 family is a handful of pixels long
    /// (the tab chevron, the end-of-line arrow, a dotted indent guide).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = (x0 + (x1 - x0) * t).round();
            let y = (y0 + (y1 - y0) * t).round();
            self.fill(Rect::new(x, y, 1.0, 1.0), color);
        }
    }
}

/// 符号显示: the invisible-character options one document carries, plus the two
/// style numbers the marks are drawn in.
///
/// `whitespace`, `control`, `eol` and `eof` are the four menu rows; `in_sel` is
/// 选择时全部显示 — with it on, a mark also shows for every character a
/// selection covers, whatever the four rows say, which is how a block of blanks
/// can be read without turning the dots on for the whole document.
#[derive(Clone, Copy, Debug, Default)]
pub struct InvisibleChars {
    pub whitespace: bool,
    pub control: bool,
    pub eol: bool,
    pub eof: bool,
    pub in_sel: bool,
    /// `Edit/SpaceStyle`: 1 a square dot (the shipped look), 2 a single pixel,
    /// 3 the open-cup stroke.
    pub space_style: u8,
    /// `Edit/TabStyle`: 1 a chevron at the cell's left edge, 2 a rule with an
    /// end tick, 3 a rule with an arrow head, 4 the `»` glyph.
    pub tab_style: u8,
    /// 缩进线 (`IndentLine`): one guide per indent level.
    pub indent: bool,
    /// `Edit/IndentStyle`: 1 solid guides, 2 dotted.
    pub indent_style: u8,
    /// 同层背景色 (`LevelBG`): tint the rows that share the caret line's indent
    /// level.
    pub level_bg: bool,
    /// The caret line's indent level, for `level_bg`.
    pub caret_level: usize,
}

impl InvisibleChars {
    /// Whether the marks cost anything: the row painter skips the whole pass
    /// while every row is off.
    fn any(&self) -> bool {
        self.whitespace
            || self.control
            || self.eol
            || self.eof
            || self.in_sel
            || self.indent
            || self.level_bg
    }
}

/// The three-letter names 控制字符 draws inside the character's cell.
const CONTROL_NAMES: [&str; 32] = [
    "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "BEL", "BS", "HT", "LF", "VT", "FF", "CR",
    "SO", "SI", "DLE", "XON", "DC2", "XOFF", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC",
    "FS", "GS", "RS", "US",
];

/// The indent level of a row: its leading blanks in tab cells, capped at fifteen
/// guides.
pub fn indent_level(chars: &[char], tab_cells: usize) -> usize {
    let (cells, _) = indent_run(chars, tab_cells);
    (cells / tab_cells.max(1)).min(15)
}

/// The leading blanks of a row: the display cells they cover and how many
/// characters they took. One walk answers both the indent level and "is this row
/// nothing but blanks", so neither asks the line a second time.
fn indent_run(chars: &[char], tab_cells: usize) -> (usize, usize) {
    let tab = tab_cells.max(1);
    let mut cells = 0usize;
    let mut used = 0usize;
    for &ch in chars {
        match ch {
            ' ' => cells += 1,
            '\t' => cells += tab - cells % tab,
            _ => return (cells, used),
        }
        used += 1;
    }
    (cells, used)
}

/// Draws one invisible-character mark: the glyph substitution the 符号显示
/// family performs for a blank, a tab or a control character. `x`/`w` are the
/// character's cell; `in_sel` says whether a selection covers it.
#[allow(clippy::too_many_arguments)]
fn draw_char_mark(
    draw: &mut DrawList,
    theme: &Theme,
    opts: InvisibleChars,
    ch: char,
    x: f32,
    w: f32,
    row_y: f32,
    in_sel: bool,
) {
    let on = |global: bool| global || (opts.in_sel && in_sel);
    let fg = theme.whitespace_fg;
    // The marks sit on the font's own cell, not on the row pitch: the row is one
    // pixel taller than the cell.
    let cell_h = theme.font_height.max(1.0);
    let cy = row_y + (cell_h * 0.5).floor();
    let bottom = row_y + cell_h;
    match ch {
        '\t' => {
            if !on(opts.whitespace) {
                return;
            }
            match opts.tab_style {
                // The `»` glyph, drawn at the cell's left edge.
                4 => draw.text(x, row_y, "»", fg),
                // A rule across the cell with an end tick.
                2 => {
                    draw.line(x + 2.0, cy, x + w - 1.0, cy, fg);
                    draw.line(x + w - 1.0, cy - 2.0, x + w - 1.0, cy + 3.0, fg);
                }
                // A rule across the cell with an arrow head.
                3 => {
                    draw.line(x + 1.0, cy, x + w - 1.0, cy, fg);
                    draw.line(x + w - 4.0, cy - 3.0, x + w - 1.0, cy, fg);
                    draw.line(x + w - 4.0, cy + 3.0, x + w - 1.0, cy, fg);
                }
                // The shipped style: a small chevron at the cell's left edge.
                _ => {
                    draw.line(x + 1.0, cy - 2.0, x + 3.0, cy, fg);
                    draw.line(x + 3.0, cy, x + 1.0, cy + 2.0, fg);
                }
            }
        }
        // The ideographic space always takes the open cup, whatever SpaceStyle
        // says: its cell is two characters wide and a dot would read as a gap.
        '\u{3000}' => {
            if !on(opts.whitespace) {
                return;
            }
            draw.line(x + 1.0, bottom - 5.0, x + 1.0, bottom - 3.0, fg);
            draw.line(x + 1.0, bottom - 3.0, x + w - 3.0, bottom - 3.0, fg);
            draw.line(x + w - 3.0, bottom - 3.0, x + w - 3.0, bottom - 6.0, fg);
        }
        ' ' => {
            if !on(opts.whitespace) {
                return;
            }
            match opts.space_style {
                // A single pixel in the cell's centre.
                2 => draw.fill(Rect::new(x + (w * 0.5).floor(), cy, 1.0, 1.0), fg),
                // The open cup: an underline with both ends turned up.
                3 => {
                    draw.line(x + 1.0, bottom - 5.0, x + 1.0, bottom - 3.0, fg);
                    draw.line(x + 1.0, bottom - 3.0, x + w - 3.0, bottom - 3.0, fg);
                    draw.line(x + w - 3.0, bottom - 3.0, x + w - 3.0, bottom - 6.0, fg);
                }
                // The shipped style: a small square in the cell's centre, two
                // pixels across whatever the cell's height — on a tall cell the
                // fatter square read as a block instead of a mark.
                _ => draw.fill(
                    Rect::new(x + (w * 0.5).floor() - 1.0, cy - 1.0, 2.0, 2.0),
                    fg,
                ),
            }
        }
        // Every other control character: its cell filled with `ConChar`'s bg and
        // the three-letter name written into it.
        c if (c as u32) < 0x20 => {
            if !on(opts.control) {
                return;
            }
            let top = row_y + 1.0;
            draw.fill(
                Rect::new(x + 1.0, top, (w - 1.0).max(1.0), (cell_h - 1.0).max(1.0)),
                theme.con_char_bg,
            );
            let name = CONTROL_NAMES[(c as usize).min(31)];
            draw.text(x + 2.0, top, name, theme.con_char_fg);
        }
        _ => {}
    }
}

/// 换行符: the return arrow drawn after a row's last glyph, one cell wide. A
/// down stroke with a V head, sized by quartering the font cell.
fn draw_eol_mark(draw: &mut DrawList, theme: &Theme, x: f32, row_y: f32) {
    let fg = theme.whitespace_fg;
    let cell_h = theme.font_height.max(1.0);
    let w = theme.x_width.max(1.0);
    // The unit is the smaller of the cell height and the digit's advance,
    // quartered in whole pixels; the head and the shaft are built from it.
    let q = (((cell_h + 1.0).min(w) as i32 + 3) / 4).max(1) as f32;
    let top = row_y + ((cell_h - 4.0 * q) * 0.5).max(0.0);
    let cx = x + 2.0 * q;
    // The head: two strokes meeting at the bottom.
    draw.line(cx - q - 1.0, top + 3.0 * q, cx - 1.0, top + 4.0 * q, fg);
    draw.line(cx - 1.0, top + 4.0 * q, cx + q, top + 3.0 * q - 1.0, fg);
    // The shaft.
    draw.line(cx - 1.0, top + q, cx - 1.0, top + 4.0 * q, fg);
}

/// Paired-tag highlight. Range A is the tag under the caret, range B
/// its nesting-aware counterpart; coordinates are `(line, start_col,
/// end_col)`, end exclusive.
#[derive(Default)]
pub struct TagHighlight {
    pub a: Option<(usize, usize, usize)>,
    pub b: Option<(usize, usize, usize)>,
}

impl TagHighlight {
    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
    }

    /// The tag-name column range covering
    /// `line`, if either range lies on it.
    pub fn range_at(&self, line: usize) -> Option<(usize, usize)> {
        if let Some((l, s, e)) = self.a {
            if l == line {
                return Some((s, e));
            }
        }
        if let Some((l, s, e)) = self.b {
            if l == line {
                return Some((s, e));
            }
        }
        None
    }
}

/// 一段文字画完之后的装饰（`[Style]` 的矩/删/下/双/点/波）。
///
/// 全用 1 px 的 `FillRect` 拼：后端的命令表里没有"画线"，而 `DrawList::line`
/// 本来也是这么走的。竖直位置按**行盒**给（`row_y` / `row_h`），不按字体
/// 上升值 —— 渲染层拿不到 ascent（它在测量后端里），行盒就是它能看见的那
/// 个盒子，偏差不超过一两个像素。
fn draw_decorations(
    draw: &mut DrawList,
    x0: f32,
    x1: f32,
    row_y: f32,
    row_h: f32,
    color: Color,
    style: u8,
) {
    let w = x1 - x0;
    if w <= 0.5 {
        return;
    }
    let bottom = row_y + row_h - 2.0;
    if style & STYLE_BOX != 0 {
        draw.fill(Rect::new(x0, row_y + 1.0, w, 1.0), color);
        draw.fill(Rect::new(x0, bottom, w, 1.0), color);
        draw.fill(Rect::new(x0, row_y + 1.0, 1.0, row_h - 2.0), color);
        draw.fill(Rect::new(x1 - 1.0, row_y + 1.0, 1.0, row_h - 2.0), color);
    }
    if style & STYLE_STRIKE != 0 {
        draw.fill(Rect::new(x0, row_y + (row_h * 0.55).round(), w, 1.0), color);
    }
    // 下划线一族：互斥，同时置了好几位就画最花的那一种。
    if style & STYLE_WAVY != 0 {
        let mut i = 0i32;
        while (i as f32) < w {
            let up = (i / 2) % 2 == 0;
            draw.fill(
                Rect::new(x0 + i as f32, bottom - if up { 1.0 } else { 0.0 }, 1.0, 1.0),
                color,
            );
            i += 1;
        }
    } else if style & STYLE_DOTTED != 0 {
        let mut i = 0i32;
        while (i as f32) < w {
            draw.fill(Rect::new(x0 + i as f32, bottom, 1.0, 1.0), color);
            i += 2;
        }
    } else if style & STYLE_DOUBLE != 0 {
        draw.fill(Rect::new(x0, bottom, w, 1.0), color);
        draw.fill(Rect::new(x0, bottom - 2.0, w, 1.0), color);
    } else if style & STYLE_UNDERLINE != 0 {
        draw.fill(Rect::new(x0, bottom, w, 1.0), color);
    }
}

/// At a suppressed character, which the run gate skips whole. Each piece starts at
/// its own advance. `base` is the first column the two flag slices describe, so
/// the caller can hand over one screenful of them instead of the whole line.
#[allow(clippy::too_many_arguments)]
fn emit_text_run(
    draw: &mut DrawList,
    chars: &[char],
    base: usize,
    row_y: f32,
    row_h: f32,
    from: usize,
    to: usize,
    color: Color,
    style: u8,
    suppressed: &[bool],
    special: &[bool],
    x_at: &dyn Fn(usize) -> f32,
    mark: &mut dyn FnMut(&mut DrawList, usize),
) {
    let mut i = from.max(base);
    while i < to {
        let hidden = suppressed.get(i - base).copied().unwrap_or(false);
        // A hidden character takes neither glyph nor width, and a mark is a
        // glyph: the 查找结果 path rows' `@~` stays bare.
        if hidden {
            i += 1;
            continue;
        }
        if special.get(i - base).copied().unwrap_or(false) {
            mark(draw, i);
            i += 1;
            continue;
        }
        let start = i;
        let mut piece = String::new();
        while i < to {
            if chars[i] == '\t'
                || special.get(i - base).copied().unwrap_or(false)
                || suppressed.get(i - base).copied().unwrap_or(false)
            {
                break;
            }
            piece.push(chars[i]);
            i += 1;
        }
        if i == start {
            // The cut character itself: the tab, or the next suppressed one.
            i += 1;
            continue;
        }
        let (x0, x1) = (x_at(start), x_at(i));
        draw.styled_text(x0, row_y, &piece, color, style);
        draw_decorations(draw, x0, x1, row_y, row_h, color, style);
    }
}

/// Colours reach this implementation through the token runs and the marks, so
/// only [`CLASS_SUPPRESSED`] is read here: it is the one class whose effect is
/// on the geometry, not the palette.
pub type CharClass = u8;

/// Attribute class 30, the caret-line painter's masked-class gate: the
/// character is neither drawn nor advanced on a row the caret is not on, and
/// while a selection covers it. The row reads as the bare path until the caret
/// lands on it.
pub const CLASS_SUPPRESSED: CharClass = 30;

/// The character metrics of one document row, in whole pixels. The text-out fill advances
/// the pen over a tab — and each of them reads the advance of one character through the
/// pixel measure, whose answer is an integer because GDI's is. That is what makes a click, a
/// caret and a glyph land on the same pixel, so this implementation keeps exactly one copy
/// of this table and every consumer asks it instead of re-deriving the arithmetic: a second
/// copy is how a line containing a tab came to select the wrong text.
#[derive(Clone, Copy, Debug)]
pub struct LineMetrics<'a> {
    /// `width('x')` — the cell an ASCII character and a space take.
    pub x: i32,
    /// The advance of a codepoint at or above 0x2000, which the dual-font model
    /// lays out in the CJK face.
    pub wide: i32,
    /// The document's tab size: the tab grid, in cells.
    pub tab_cells: usize,
    /// The platform's measured advances, sorted by character — the same table
    /// the glyph shaper draws with. A character found here takes its face's
    /// real width instead of a grid cell.
    measured: &'a [(char, i32)],
}

impl<'a> LineMetrics<'a> {
    pub fn new(theme: &'a Theme, tab_cells: usize) -> Self {
        let x = theme.x_width.max(1.0).round() as i32;
        let wide = theme.cjk_width.max(theme.x_width).round() as i32;
        Self {
            x,
            wide: wide.max(x),
            tab_cells: tab_cells.max(1),
            measured: &theme.measured,
        }
    }

    /// One character's advance: the codepoints at or above 0x2000 go to the CJK
    /// face and take its width, everything else one cell. The pixel measure measures
    /// each character and answers the space with `width('x')`; in a fixed-pitch
    /// face both are the same number, which is why one cell is this implementation's answer
    /// for every character below 0x2000. A character the platform measured on a
    /// fallback face (a colour emoji) keeps that face's width, because the very
    /// glyph the shaper draws is that wide. A tab is not measured here — it is a
    /// stop, see [`Self::tab_from`].
    ///
    /// A modifier takes no column: U+FE0F draws nothing, so `☕` and `☕️` are
    /// different strings that must still end in the same place, or pasting an
    /// emoji shifts everything after it by a cell. See
    /// [`attaches_to_previous`](dg_layout::measure::attaches_to_previous).
    pub fn advance(&self, ch: char) -> i32 {
        if dg_layout::measure::attaches_to_previous(ch) {
            return 0;
        }
        if let Ok(i) = self.measured.binary_search_by_key(&ch, |&(c, _)| c) {
            return self.measured[i].1;
        }
        if (ch as u32) >= 0x2000 {
            self.wide
        } else {
            self.x
        }
    }

    /// The advance a tab takes from pixel `px`: the text-out fill, whose stop is a
    /// *pixel* modulo of `tab cells × width('x')`, and which takes the whole
    /// period *and* the remaining distance when it lands with less than one
    /// character left before that stop.
    pub fn tab_from(&self, px: i32) -> i32 {
        let period = self.tab_cells as i32 * self.x;
        let mut adv = period - px % period;
        if adv < self.x {
            adv += period;
        }
        adv
    }

    /// The pixel after one character, from `px`: nothing for a hidden character,
    /// the next stop for a tab, the advance otherwise.
    fn step(&self, px: i32, ch: char, hidden: bool) -> i32 {
        if hidden {
            px
        } else if ch == '\t' {
            px + self.tab_from(px)
        } else {
            px + self.advance(ch)
        }
    }

    /// the pixel-x measure walked one character at a time: the pixel of every
    /// boundary of `chars`, starting at 0, so `xs.len() == chars.len() + 1`. A
    /// hidden character takes neither glyph nor width, which is how a 查找结果
    /// path row's `@~` costs nothing; `hidden` may be shorter than `chars`, the
    /// tail then counts as visible.
    pub fn bounds(&self, chars: &[char], hidden: &[bool]) -> Vec<i32> {
        let mut xs = Vec::with_capacity(chars.len() + 1);
        let mut px = 0;
        xs.push(px);
        for (i, &ch) in chars.iter().enumerate() {
            px = self.step(px, ch, hidden.get(i).copied().unwrap_or(false));
            xs.push(px);
        }
        xs
    }

    /// [`Self::col_at_px`] starting from a known pixel: `from_px` is the pixel
    /// of `chars[0]`, so a caller that already knows where a column sits
    /// walks only the distance from there. `hidden` is indexed from `chars[0]`.
    pub fn col_at_px_from(
        &self,
        chars: &[char],
        hidden: &[bool],
        from_px: i32,
        px_budget: i32,
    ) -> (usize, i32) {
        let mut px = from_px;
        for (i, &ch) in chars.iter().enumerate() {
            if px >= px_budget {
                return (i, px);
            }
            px = self.step(px, ch, hidden.get(i).copied().unwrap_or(false));
        }
        (chars.len(), px)
    }

    /// The first column whose left boundary is at or past `px_budget`, and that
    /// boundary's pixel. The walk stops at the budget, so reaching a window deep
    /// inside a very long line costs the columns before it, not the whole line.
    /// `hidden` is indexed from `chars[0]`, like [`Self::bounds`].
    pub fn col_at_px(&self, chars: &[char], hidden: &[bool], px_budget: i32) -> (usize, i32) {
        self.col_at_px_from(chars, hidden, 0, px_budget)
    }

    /// [`Self::col_at_px`] walking *back* from a known column: the smallest
    /// column at or past `px_budget`, found by subtracting the advances in
    /// between. `chars[from]` is the known column and `from_px` its pixel.
    ///
    /// `None` when the walk crosses a tab: a tab's advance is a function of the
    /// pixel it starts at, so it cannot be subtracted without knowing that
    /// pixel. The caller then measures from the segment's start instead, which
    /// is what it would have done anyway.
    pub fn col_at_px_back(
        &self,
        chars: &[char],
        hidden: &[bool],
        from: usize,
        from_px: i32,
        px_budget: i32,
    ) -> Option<(usize, i32)> {
        let mut i = from.min(chars.len());
        let mut px = from_px;
        while i > 0 {
            let ch = chars[i - 1];
            if ch == '\t' {
                return None;
            }
            if hidden.get(i - 1).copied().unwrap_or(false) {
                i -= 1;
                continue;
            }
            let prev = px - self.advance(ch);
            if prev < px_budget {
                break;
            }
            px = prev;
            i -= 1;
        }
        Some((i, px))
    }

    /// [`Self::bounds`] over `[from, to)` only, continuing from `start_px`: the
    /// window's own slice of the table, so the tail of a long line is never
    /// measured. `out[0]` is `start_px` and `out[k]` the boundary after
    /// `from + k`; `hidden` is indexed from `from` (a windowed slice).
    pub fn bounds_range(
        &self,
        chars: &[char],
        from: usize,
        to: usize,
        start_px: i32,
        hidden: &[bool],
        out: &mut Vec<i32>,
    ) {
        let to = to.min(chars.len());
        let from = from.min(to);
        out.clear();
        out.reserve(to - from + 1);
        out.push(start_px);
        let mut px = start_px;
        for (k, &ch) in chars[from..to].iter().enumerate() {
            px = self.step(px, ch, hidden.get(k).copied().unwrap_or(false));
            out.push(px);
        }
    }

    /// The pixel of the boundary after `col` characters of `chars` —
    /// the pixel-x measure, which walks the characters and sums the advances rather
    /// than building the table.
    pub fn x_at(&self, chars: &[char], col: usize, hidden: &[bool]) -> i32 {
        let mut px = 0;
        for (i, &ch) in chars.iter().enumerate().take(col.min(chars.len())) {
            px = self.step(px, ch, hidden.get(i).copied().unwrap_or(false));
        }
        px
    }

    /// the point-to-caret hit test's walk: the column whose cell the pointer has crossed,
    /// where a cell is crossed at its midpoint. `xs` is [`Self::bounds`]-shaped; a target
    /// past the last boundary answers it.
    pub fn col_at(&self, xs: &[i32], target: i32) -> usize {
        let mut col = 0usize;
        while col + 1 < xs.len() {
            let (prev, next) = (xs[col], xs[col + 1]);
            if next + (prev - next) / 2 < target {
                col += 1;
            } else {
                break;
            }
        }
        col
    }

    /// [`Self::col_at`] without the table: the same midpoint rule walked from the
    /// first character and stopped at the column the target lands in, so a click
    /// deep inside a long line costs the columns before the pointer. `hidden` is
    /// indexed from `chars[0]`.
    pub fn col_at_target(&self, chars: &[char], hidden: &[bool], target: i32) -> usize {
        let mut px = 0;
        for (i, &ch) in chars.iter().enumerate() {
            let next = self.step(px, ch, hidden.get(i).copied().unwrap_or(false));
            if next + (px - next) / 2 < target {
                px = next;
            } else {
                return i;
            }
        }
        chars.len()
    }
}

/// the caret-line painter's class-30 gate, one flag per character of the row: a
/// character of that attribute class takes neither glyph nor width on a row the
/// caret is not on and no selection covers. The painter, the caret and the mouse
/// all have to answer the same question about the same character, so this is the
/// one place it is asked.
///
/// Only `[from, to)` is answered — `out[k]` belongs to column `from + k` — so a
/// row that materializes one screenful of a very long line asks about one
/// screenful of characters.
pub fn hidden_flags(
    flags: &[CharClass],
    fline: usize,
    caret_line: usize,
    sel: &SelectionSet,
    from: usize,
    to: usize,
) -> Vec<bool> {
    let to = to.min(flags.len());
    if caret_line == fline || from >= to {
        return Vec::new();
    }
    flags[from..to]
        .iter()
        .enumerate()
        .map(|(k, &f)| f == CLASS_SUPPRESSED && !sel.contains(fline, from + k))
        .collect()
}

/// The window the last paint put on a row.
///
/// A repaint that leaves the view where it was — a caret blink, a keystroke at
/// the line's end, a status-bar refresh — answers with these columns and never
/// measures the text in front of them. A repaint that moved the view to the
/// right walks the difference from here instead of from the row's first
/// column; one that moved it back to the left, or landed somewhere else
/// entirely, measures its way in from the first column and re-anchors.
///
/// `budget` and `measured` are part of the key because they are what the
/// window was chosen *for*: the same left edge under the same advance table
/// places every column at the same pixel, and a table that has grown can place
/// one somewhere else. The segment is part of it too, since `px` is measured
/// from the segment's first character — a wrapped row's pixels and an
/// unwrapped one's are not the same ruler.
#[derive(Clone, Copy, Default)]
pub struct WindowMemo {
    pub line: usize,
    pub seg: usize,
    /// The row's left edge in pixels, and the size of the advance table when
    /// the window was chosen.
    pub budget: i32,
    pub measured: usize,
    /// The window's first column and that column's pixel.
    pub col: usize,
    pub px: i32,
}

pub type WindowAnchor = std::cell::RefCell<WindowAnchors>;

/// How many rows keep a window. A scroll through a long document touches one
/// entry per row it passes; past the bound the table starts over, which only
/// costs the rows the next paint has to measure in.
const WINDOW_ANCHOR_MAX: usize = 4096;

/// The windows of the rows the last paint drew, keyed by `(line, segment)`.
/// One entry per row, so a viewport of sixty wrapped rows of the same line
/// keeps sixty windows and each of them finds its own.
#[derive(Default)]
pub struct WindowAnchors {
    rows: std::collections::HashMap<(usize, usize), WindowMemo>,
}

impl WindowAnchors {
    /// The window the last paint put on this row.
    pub fn get(&self, line: usize, seg: usize) -> Option<WindowMemo> {
        self.rows.get(&(line, seg)).copied()
    }

    /// Records the window this paint put on a row, starting the table over
    /// once it has as many rows as [`WINDOW_ANCHOR_MAX`].
    pub fn set(&mut self, memo: WindowMemo) {
        if self.rows.len() >= WINDOW_ANCHOR_MAX {
            self.rows.clear();
        }
        self.rows.insert((memo.line, memo.seg), memo);
    }

    /// Drops the windows of one line's rows — the rows a re-wrap rebuilt.
    pub fn drop_line(&mut self, line: usize) {
        self.rows.retain(|&(l, _), _| l != line);
    }

    /// The recorded windows, so a caller can keep the ones an edit did not
    /// move.
    pub fn windows(&self) -> impl Iterator<Item = WindowMemo> + '_ {
        self.rows.values().copied()
    }

    /// Whether any row of `line` keeps a window — the caller's cue to watch
    /// that line's break array while it re-wraps the line.
    pub fn has_line(&self, line: usize) -> bool {
        self.rows.keys().any(|&(l, _)| l == line)
    }

    pub fn clear(&mut self) {
        self.rows.clear();
    }
}

/// The paint pipeline — the text view's whole-window paint and its
/// per-row draw.
pub struct PaintPipeline;

impl PaintPipeline {
    /// Draws one visual row into `draw`. Steps: 1. `line_text` — lazy decode
    /// (dg-core). 2. tokenize (dg-syntax highlight runs). 3. compute line flags
    /// (selection bits). 4. segment loop: start char = `wrap.data[seg]`, per-segment
    /// `rect ∩ clip`, h-scroll offset = the view x `xWidth`. 5. background fill, token
    /// runs via `TextRun` (TextOutW), selection and caret overlays. `caret_line` is
    /// the primary caret's logical line — it only picks the active-line band. GDI
    /// paints it after this row pass and nothing can overwrite it. `extra_carets` are
    /// the implementation's multi-caret additions, which have no system caret to be.
    pub fn draw_line(
        doc: &Document,
        fline: usize,
        seg: usize,
        wrap: &WrapArray,
        // `(color, len, style)` —— `style` 是 `[Style]` 的字形位，和颜色
        // 同源（同一个 `[Theme]`/`[Style]` 键）。
        highlight: Option<&[(u32, u32, u8)]>,
        tag_hl: Option<&TagHighlight>,
        // 自定义标记 spans of this line, as `(start, end, bg, fg)` sorted by
        // start. Empty when the marker set is off, which is the common case.
        marks: &[(usize, usize, Color, Color)],
        // 颜色高亮 (cm_highlight_color, 1271): color-literal
        // ranges as `(start, end, color)` on this line, painted
        // as a 3 px band at the row's bottom edge.
        color_bands: &[(usize, usize, Color)],
        // 选区高亮 (cm_highlight_sel, 1313): single-line
        // selection ranges, painted as solid bands under the
        // text — the text keeps its syntax colour.
        sel_bands: &[(usize, usize, Color)],
        flags: &[u8],
        sel: &SelectionSet,
        theme: &Theme,
        // `tab_cells`: the tab grid in characters, which the platform layer takes
        // from the document's own tab width.
        tab_cells: usize,
        view: ViewFlags,
        // 符号显示: the invisible-character marks, the indent guides and the
        // level background one row draws.
        chars_opt: InvisibleChars,
        clip: Rect,
        // Right edge of the caret row's band: where the text column ends while
        // the lines wrap, the scrollbar strip's left edge while they do not.
        // The row's background still spans `clip`; only the band stops here.
        band_right: f32,
        row_y: f32,
        h_scroll: i32,
        caret_line: usize,
        extra_carets: &[(usize, usize)],
        // 折叠尾部指示: true when the line's
        // next line is hidden under a fold, which stamps the
        // indicator box after the row's text on its last wrap
        // segment.
        fold_end: bool,
        // 折叠尾部字符 (FoldingTail, view option 9,
        // the update step): the collapsed region's last line as a
        // preview, drawn after the box. `None` when the option is
        // off or the line holds no fold.
        fold_tail: Option<&str>,
        anchor: &WindowAnchor,
        draw: &mut DrawList,
    ) {
        // 1. the line's characters, shared from the store's view cache: a row
        // reads a handle, never a fresh copy of the text.
        let Some(chars) = doc.line_chars(fline) else {
            return; // still loading
        };
        let line_len = chars.len();

        // 4. segment bounds
        let start = if seg == 0 {
            0
        } else {
            wrap.data.get(seg - 1).copied().unwrap_or(0).max(0) as usize
        };
        let end = wrap.data.get(seg).copied().unwrap_or(line_len as i32) as usize;
        let end = end.min(line_len);
        let start = start.min(end);

        let row_h = theme.line_height;
        let row_rect = Rect::new(0.0, row_y, f32::MAX, row_h);
        let clipped = row_rect.intersect(&clip);
        if clipped.is_empty() {
            return;
        }
        // 行内容的垂直偏移（设置行高）：额外行距的一半，文字、插入符和挂
        // 在行尾的记号在多出来的空间里往下居中；背景带、选区、缩进线留在
        // 行顶。
        let content_y = row_y + theme.content_offset();

        // 5a. background. The caret row's band is `Edit/ActiveLine`
        // (`cm_view_activeline`, bit 6): with the option off the row is an
        // ordinary row, which is why the gate is here rather than in the theme.
        // The band stops at `band_right`, so the margin beside the text column
        // and the scrollbar strip stay plain background instead of carrying a
        // slab of the band's colour to the window edge.
        let caret_row = view.get(ViewFlags::ACTIVE_LINE) && caret_line == fline;
        draw.fill(clipped, theme.background);
        if caret_row {
            let band = Rect::new(0.0, row_y, band_right.max(0.0), row_h).intersect(&clip);
            if !band.is_empty() {
                draw.fill(band, theme.current_line_bg);
            }
        }

        // selection underlay: one blended rect over the selected
        // span of this segment.
        let m = LineMetrics::new(theme, tab_cells);
        let x_off = theme.gutter_x - (h_scroll as f32) * m.x as f32;
        // The columns this row can put ink on. The clip rect inverted through
        // `x_off` gives the pixel range; the marks that hang off the end of the
        // line (EOL, EOF, the fold box) take a little slack past the right edge
        // so a row whose last character sits just off screen still stamps them
        // where it used to. Everything below works in absolute columns — the
        // window only bounds how many of them are materialized.
        let left_px = (clip.x - x_off).floor().max(0.0) as i32;
        let right_px = (clip.x + clip.w - x_off).ceil() as i32 + WINDOW_SLACK_PX;
        // The walk that places the window and the geometry that lays the row out
        // have to agree about which characters take no glyph: a hidden run in
        // front of the window moves the column the window belongs on. Only a
        // 查找结果 row hides anything, and its line is a screenful at most, so
        // the segment's flags are cheap to answer once and slice from.
        let hidden_seg: Vec<bool> = if flags.is_empty() {
            Vec::new()
        } else {
            hidden_flags(flags, fline, caret_line, sel, start, end)
        };
        // A row of single-cell glyphs has a closed form for its window:
        // column `n` sits at `n * x`, so the window is a division. Every
        // other row has to measure its way in, which costs the columns
        // before the window — that is the fallback, not the common case.
        // A measured advance only breaks the grid when it is not the cell
        // width; the platform measures plenty of glyphs that are. A hidden
        // character breaks it too: it takes a column and no pixel.
        let uniform = m.x > 0
            && !hidden_seg.contains(&true)
            && doc.line_uniform(fline)
            && m.measured.iter().all(|&(_, w)| w == m.x);
        // The flags from column `col` to the segment's end, indexed from `col` —
        // what the forward walks want. An empty segment is the common case:
        // nothing on the row is hidden, and every slice below answers `&[]`.
        let hidden_from = |col: usize| -> &[bool] {
            let from = col.saturating_sub(start).min(hidden_seg.len());
            &hidden_seg[from..]
        };
        let (win_start, win_px) = if uniform {
            let col = start + cells_before(left_px, m.x).min(end - start);
            (col, (col - start) as i32 * m.x)
        } else {
            // The memo of this row, if it still describes a column of this
            // segment.
            let memo = anchor
                .borrow()
                .get(fline, seg)
                .filter(|w| (start..=end).contains(&w.col));
            if let Some(w) = memo.filter(|w| w.budget == left_px && w.measured == m.measured.len())
            {
                // Nothing moved: the window this row showed last time is the
                // window it shows now, down to the pixel.
                (w.col, w.px)
            } else if let Some(w) = memo.filter(|w| w.px <= left_px) {
                // The view moved right since the last paint, or the memo was
                // taken under a smaller advance table: walk the difference,
                // not the text in front of it. A walk that runs out of row
                // without reaching the budget means the memo does not
                // describe this position after all — measure from the first
                // column instead of trusting it.
                let (n, px) = m.col_at_px_from(&chars[w.col..end], hidden_from(w.col), w.px, left_px);
                if px >= left_px || w.col + n >= end {
                    (w.col + n, px)
                } else {
                    let (col, px) = m.col_at_px(&chars[start..end], hidden_from(start), left_px);
                    (start + col, px)
                }
            } else if let Some((col, px)) = memo.and_then(|w| {
                // The view moved left: the window sits before the memo's
                // column. Subtracting the advances between the two is the
                // same walk run the other way — a scroll of one cell costs
                // one character instead of the line in front of it.
                let to = (w.col + 1).min(chars.len()).min(end);
                let hidden = &hidden_seg[..(to - start).min(hidden_seg.len())];
                m.col_at_px_back(&chars[start..to], hidden, w.col - start, w.px, left_px)
                    .map(|(c, px)| (start + c, px))
            }) {
                (col, px)
            } else {
                let (col, px) = m.col_at_px(&chars[start..end], hidden_from(start), left_px);
                (start + col, px)
            }
        };
        anchor.borrow_mut().set(WindowMemo {
            line: fline,
            seg,
            budget: left_px,
            measured: m.measured.len(),
            col: win_start,
            px: win_px,
        });
        let win_end = if uniform {
            win_start + cells_before(right_px - win_px, m.x).min(end - win_start)
        } else {
            win_start
                + m.col_at_px(
                    &chars[win_start..end],
                    hidden_from(win_start),
                    right_px - win_px,
                )
                .0
        };
        let suppressed: &[bool] = &hidden_seg[(win_start - start).min(hidden_seg.len())
            ..(win_end - start).min(hidden_seg.len())];
        // 同层背景色 (`LevelBG`, 1674): every row that shares the caret line's
        // indent level carries a faint tint, so the block the caret sits in
        // reads as one region. The tint is the same 8 % foreground the hatch
        // averages to, laid down as one blended rectangle.
        if chars_opt.level_bg && chars_opt.caret_level > 0 {
            if indent_level(&chars, tab_cells) == chars_opt.caret_level {
                let band = Rect::new(0.0, row_y, band_right.max(0.0), row_h).intersect(&clip);
                if !band.is_empty() {
                    draw.alpha_blend(band, theme.foreground, 0.08);
                }
            }
        }
        // 符号显示: the cells the mark pass draws instead of a glyph. A tab is
        // always one of them (it is never handed to the text run); a blank or a
        // control character joins it when its option is on, or when 选择时全部显示
        // is on and the selection covers it. Only the window is classified.
        let special: Vec<bool> = if chars_opt.any() {
            chars[win_start..win_end]
                .iter()
                .enumerate()
                .map(|(k, &ch)| {
                    let in_sel = sel.contains(fline, win_start + k);
                    match ch {
                        '\t' => chars_opt.whitespace || (chars_opt.in_sel && in_sel),
                        ' ' | '\u{3000}' => {
                            chars_opt.whitespace || (chars_opt.in_sel && in_sel)
                        }
                        c if (c as u32) < 0x20 => {
                            chars_opt.control || (chars_opt.in_sel && in_sel)
                        }
                        _ => false,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        // The segment keeps its own stop grid: the text-out fill takes the modulo of
        // the pen's accumulated x, and the caret-line painter seeds that accumulator with
        // the line's leading indent only while 子行按缩进换行 is
        // on. This implementation draws every segment at the text rect's left edge, so there
        // are no indent pixels in front of one and the seed is 0 — which is what
        // the caret and the hit test seed too.
        let mut xs: Vec<i32> = Vec::new();
        m.bounds_range(&chars, win_start, win_end, win_px, &suppressed, &mut xs);
        // `x_of` takes a segment-relative index and `x_at` an absolute column;
        // both answer the window's edge for a column outside it, so a rect that
        // starts or ends off screen keeps its visible part.
        let base = win_start - start;
        let x_of = |i: usize| x_off + xs[i.saturating_sub(base).min(xs.len() - 1)] as f32;
        // The run walk indexes the line by character, and every piece a tab or a
        // suppressed character splits it out of a run starts at its own advance.
        let x_at = |i: usize| x_of(i - start);
        // 符号显示: the mark for one cell, handed to the run walk. The cell's
        // width is the distance to the next boundary, which is where the stop
        // grid already put the pen.
        let mut mark = |draw: &mut DrawList, i: usize| {
            let in_sel = sel.contains(fline, i);
            let x = x_at(i);
            let w = (x_at(i + 1) - x).max(1.0);
            draw_char_mark(draw, theme, chars_opt, chars[i], x, w, content_y, in_sel);
        };
        let mut sel_start: Option<usize> = None;
        // A window that opens inside a selection starts with its band already
        // open, or the row would lose the left part of it.
        if win_start < win_end && sel.contains(fline, win_start) {
            sel_start = Some(win_start);
        }
        for i in win_start..win_end {
            let inside = sel.contains(fline, i);
            if inside && sel_start.is_none() {
                sel_start = Some(i);
            }
            if !inside {
                if let Some(s) = sel_start.take() {
                    let r = Rect::new(
                        x_of(s - start),
                        row_y,
                        x_of(i - start) - x_of(s - start),
                        row_h,
                    );
                    draw.alpha_blend(r, theme.selection_bg, 0.5);
                }
            }
        }
        if let Some(s) = sel_start.take() {
            let r = Rect::new(
                x_of(s - start),
                row_y,
                x_of(win_end - start) - x_of(s - start),
                row_h,
            );
            draw.alpha_blend(r, theme.selection_bg, 0.5);
        }

        // 选区高亮 (cm_highlight_sel, 1313): a solid band over
        // each single-line selection, stronger than the 50% blend
        // the selection underlay just drew. Multi-line selections
        // are left to the blend
        // interval only covers single-line ranges.
        for &(ss, se, scol) in sel_bands {
            let s = ss.max(win_start).min(win_end);
            let e = se.max(s).min(win_end);
            if e <= s {
                continue;
            }
            draw.fill(
                Rect::new(x_of(s - start), row_y, x_of(e - start) - x_of(s - start), row_h),
                scol,
            );
        }

        // 选区文本的背景色扩展到窗口右侧 (`SelExtend`): 这一行整行都在选区里
        // 时，行尾那片没有字的空白也铺上选区底色，带子顶到文本区右缘。
        // 「整行都在」只有单段选区才谈得上 —— 多光标各自一段，任何一行都未必
        // 被同一段盖满，所以多段时这一条不铺。选区的最后一行也不算：那一行只
        // 覆盖到选区的结束列，往后是正文，不是选区。
        if view.get(ViewFlags::SEL_EXTEND)
            && sel.ranges.len() == 1
            && fline >= sel.ranges[0].start.0
            && fline < sel.ranges[0].end.0
        {
            let left = x_of(win_end - start);
            let tail = Rect::new(left, row_y, (clip.x + clip.w - left).max(0.0), row_h)
                .intersect(&clip);
            if !tail.is_empty() {
                draw.alpha_blend(tail, theme.selection_bg, 0.5);
            }
        }

        // 自定义标记: an opaque band over the selection, then the active-line band
        // composited over *it* at half strength. A marker on the caret row
        // measures as the average of the marker colour and `ActiveLine`, and
        // covers a selection of the same word.
        for &(ms, me, mc, _mc_fg) in marks {
            let s = ms.max(win_start).min(win_end);
            let e = me.max(s).min(win_end);
            if e <= s {
                continue;
            }
            let r = Rect::new(x_of(s - start), row_y, x_of(e - start) - x_of(s - start), row_h);
            draw.fill(r, mc);
            if caret_row {
                draw.alpha_blend(r, theme.current_line_bg, 0.5);
            }
        }

        // 5b. token runs -> TextRun commands
        // (TextOutW,). `highlight` holds
        // {color, char_len} pairs for the whole
        // line; walk it alongside the segment.
        let mut mk_idx = 0usize; // cursor into the sorted `marks`
        let mut char_pos = 0usize; // cursor within line
        let mut hl_idx = 0usize;
        let mut consumed_in_run = 0u32;
        while char_pos < start {
            // skip runs before the segment
            if let Some(&(color, len, _style)) = highlight.and_then(|h| h.get(hl_idx)) {
                let _ = color;
                let remaining = (start - char_pos) as u32;
                if remaining >= len - consumed_in_run {
                    char_pos += (len - consumed_in_run) as usize;
                    hl_idx += 1;
                    consumed_in_run = 0;
                } else {
                    consumed_in_run += remaining;
                    char_pos = start;
                }
            } else {
                char_pos = start;
                break;
            }
        }
        let _x = char_pos as i32 - start as i32;

        let tag_range = tag_hl.and_then(|t| t.range_at(fline));

        let mut seg_char = start;
        // a line the tokenizer covered only partly must still
        // use the theme's plain-text color, not a constant that
        // survives every theme switch
        let default_fg = theme.foreground.to_u32();
        // 选区文本保持语法高亮的前景色 (`KeepSelFG`): 这一行不写就是关，关时被
        // 选中的字不再用自己那一份语法色，整段换成主题的选区前景 —— 深色主题给
        // 近白、浅色主题给近黑，为的都是饱和的选区底色上仍有足够对比。深色主题的
        // 选区前景和默认前景差得很小，所以纯文字被选中几乎看不出换了色，挑一个语法
        // 着色的词才看得出来；写 `Y` 才是开着，那时选区只剩底色。所以一次 emit 要
        // 按「在不在选区里」拆成若干段分别发。
        let keep_sel_fg = view.get(ViewFlags::KEEP_SEL_FG);
        // 没有选区时逐列问「在不在选区里」是白问，直接整段发。
        let split_sel_fg = !keep_sel_fg && !sel.is_empty();
        // The run walk still visits every run of the segment — it has to, to keep
        // the cursors honest — but only the part inside the window reaches the
        // draw list, so a 5 MB line costs one screenful of glyphs.
        let mut emit = |draw: &mut DrawList, from: usize, to: usize, color: Color, style: u8| {
            let from = from.max(win_start);
            let to = to.min(win_end);
            if to <= from {
                return;
            }
            if !split_sel_fg {
                emit_text_run(
                    draw, &chars, win_start, content_y, row_h, from, to, color, style, &suppressed,
                    &special, &x_at, &mut mark,
                );
                return;
            }
            let mut i = from;
            while i < to {
                let inside = sel.contains(fline, i);
                let mut j = i + 1;
                while j < to && sel.contains(fline, j) == inside {
                    j += 1;
                }
                let piece = if inside { theme.selection_fg } else { color };
                emit_text_run(
                    draw, &chars, win_start, content_y, row_h, i, j, piece, style, &suppressed,
                    &special, &x_at, &mut mark,
                );
                i = j;
            }
        };
        while seg_char < end {
            let (color, len, style) = match highlight.and_then(|h| h.get(hl_idx)) {
                Some(&(color, len, style)) => (color, len, style),
                None => (default_fg, (end - seg_char) as u32, 0),
            };
            // The run's unemitted chars begin at the segment cursor itself:
            // the run tiles the line contiguously, so whatever part of it
            // lies before `start` is already accounted for by
            // `consumed_in_run` in the *length* below. Adding it to the
            // position too skipped that many characters — a token crossing a
            // wrap boundary then read past its segment (`end - run_start`
            // underflowing on the short tail segment) and drew the wrong
            // glyphs on the rows that survived.
            let run_start = seg_char;
            let take = (len - consumed_in_run).min((end - run_start) as u32);
            if take == 0 {
                hl_idx += 1;
                consumed_in_run = 0;
                continue;
            }
            let chunk_end = run_start + take as usize;
            // tag overlay: re-color the [ts, te)
            // span inside this run — up to three
            // emitted pieces (before / tag / after)
            let mut pos = run_start;
            if let Some((ts, te)) = tag_range {
                let ov_start = pos.max(ts).min(chunk_end);
                let ov_end = chunk_end.min(te);
                if ov_start > pos {
                    emit(draw, pos, ov_start, Color::from_u32(color), style);
                    pos = ov_start;
                }
                if ov_end > pos {
                    emit(draw, pos, ov_end, theme.tag_fg, style);
                    pos = ov_end;
                }
            }
            // marker recolour over whatever the tag pass left: a marked word
            // loses its syntax colour, which is the expected behaviour (the
            // glyph takes the marker's own colour).
            while pos < chunk_end {
                let Some(&(ms, me, _mbg, mc)) = marks.get(mk_idx) else {
                    break;
                };
                if me <= pos {
                    mk_idx += 1;
                    continue;
                }
                if ms >= chunk_end {
                    break;
                }
                if ms > pos {
                    emit(draw, pos, ms, Color::from_u32(color), style);
                    pos = ms;
                    continue;
                }
                let e = me.min(chunk_end);
                emit(draw, pos, e, mc, style);
                pos = e;
            }
            if chunk_end > pos {
                emit(draw, pos, chunk_end, Color::from_u32(color), style);
            }
            seg_char = chunk_end;
            if seg_char >= end {
                break;
            }
            hl_idx += 1;
            consumed_in_run = 0;
        }

        // 颜色高亮 (cm_highlight_color, 1271): a 3 px band at
        // the row's bottom edge under each color literal, in the
        // literal's own color. It is painted over the syntax runs
        //  but under
        // the caret, so a caret inside a literal still shows.
        for &(bs, be, bcol) in color_bands {
            let s = bs.max(win_start).min(win_end);
            let e = be.max(s).min(win_end);
            if e <= s {
                continue;
            }
            let bh = 3.0f32.min(row_h);
            draw.fill(
                Rect::new(
                    x_of(s - start),
                    row_y + row_h - bh,
                    x_of(e - start) - x_of(s - start),
                    bh,
                ),
                bcol,
            );
        }

        // 5b. the fold-end indicator: a row whose next line is hidden
        // under a fold stamps a small box after its text on the last wrap
        // segment. A 15×10 rounded box with three dots — at `x + 3`. The
        // glyph comes in two colourings, cyan (#48F3F3/#699999) and
        // yellow (#F1F146/#949467); [`FOLD_END_FILL`] and
        // [`FOLD_END_DOT`] carry the yellow, so this implementation draws
        // the same geometry in the yellow.
        if fold_end && wrap.data.get(seg).is_none() && end == win_end {
            let x = x_of(end - start);
            let box_w = 21.0;
            if x + box_w <= clip.x + clip.w && x >= clip.x {
                let box_h = 10.0;
                let by = row_y + (row_h - box_h) * 0.5;
                draw.fill(Rect::new(x, by, box_w, box_h), FOLD_END_FILL);
                // icon 131's three dots, 2×2 each at (2,5), (6,5)
                // and (10,5) of the icon stamped at x + 3.
                let ix = x + 3.0;
                for k in 0..3 {
                    draw.fill(
                        Rect::new(ix + 2.0 + 4.0 * k as f32, by + 5.0, 2.0, 2.0),
                        FOLD_END_DOT,
                    );
                }
                // 折叠尾部字符 (FoldingTail, view option 9,
                // the update step): the collapsed run's last line as
                // a preview after the box — leading blanks skipped
                // Capped at 0x80 characters.
                if let Some(t) = fold_tail {
                    draw.text(x + box_w, content_y, t, theme.foreground);
                }
            }
        }

        // 5c. the extra carets' indicator lines (multi-caret editing — the
        // primary caret is the system caret and never reaches the draw list five
        // matches picked by `选择所有匹配`, the active-line band lands on the primary
        // caret's row and the indicator line on the other four). The paint loop
        // answers it with `SetDCPenColor` with the `SelIndicator` theme key plus
        // a `MoveToEx`/`LineTo` pair — as thick as the primary caret's bar
        // (`Theme::caret_w`), inset by half the line spacing (the view, zero by
        // default), so a default-spacing row gets the full row height.
        let indicator = theme.caret_indicator();
        for &(cl, cc) in extra_carets {
            if cl == fline && cc >= win_start && cc <= win_end {
                draw.fill(
                    Rect::new(x_of(cc - start), content_y, theme.caret_w.max(1.0), row_h),
                    indicator,
                );
            }
        }

        // 缩进线 (`IndentLine`, 1307): a guide at every tab stop the row's leading
        // blanks run past, so a row indented to level k carries k-1 of them and
        // none falls under its first character. A row of nothing but blanks has
        // no text to keep clear: it takes its predecessor's level and carries the
        // guide at its own stop too, which keeps a guide unbroken across a
        // paragraph gap. Solid while `IndentStyle` is 1 (the shipped look), a
        // dotted column otherwise.
        if chars_opt.indent && end == chars.len() && wrap.data.get(seg).is_none() {
            let (cells, blanks) = indent_run(&chars, tab_cells);
            let mut level = (cells / tab_cells.max(1)).min(15);
            // A row of nothing but blanks is exactly the row whose indent walk
            // reached the end of the text: the guide pass does not need a second
            // look at the line to learn that.
            let all_blanks = blanks == chars.len();
            if level == 0 && all_blanks && fline > 0 {
                if let Some(prev) = doc.line_chars(fline - 1) {
                    level = indent_level(&prev, tab_cells);
                }
            }
            let stops = if all_blanks {
                level
            } else {
                level.saturating_sub(1)
            };
            let xw = theme.x_width;
            let step = tab_cells.max(1) as f32 * xw;
            for j in 1..=stops {
                let x = theme.gutter_x - (h_scroll as f32) * xw + j as f32 * step;
                if x < clip.x || x > clip.x + clip.w {
                    continue;
                }
                if chars_opt.indent_style == 1 {
                    draw.fill(Rect::new(x, row_y, 1.0, row_h), theme.indent_line);
                } else {
                    let mut y = row_y;
                    while y < row_y + row_h {
                        draw.fill(Rect::new(x, y, 1.0, 1.0), theme.indent_line);
                        y += 2.0;
                    }
                }
            }
        }

        // 换行符 / 文档结束符: the two marks that hang off the end of a line's
        // text, on its last wrap segment only — and only while that end is on
        // screen, since the window is the only part of the line whose pixels are
        // known.
        if wrap.data.get(seg).is_none() && end == win_end {
            let x_end = x_of(end - start);
            if chars_opt.eol && doc.eol_kind(fline) != 0 {
                draw_eol_mark(draw, theme, x_end, content_y);
            }
            if chars_opt.eof && fline + 1 == doc.line_count() {
                draw.text(x_end, content_y, "[EOF]", theme.whitespace_fg);
            }
        }

        let _ = flags; // selection flags buffer reused: membership
                       // is recomputed here from the interval set.
    }

    /// Paints the visible rows: the row loop of
    /// `OnPaint`.
    pub fn paint_visible(
        doc: &Document,
        rows: &std::ops::Range<usize>,
        row_to_line_segment: impl Fn(usize) -> Option<(usize, usize)>,
        wrap_store: &dg_layout::wrap::WrapStore,
        highlight_for: impl Fn(usize) -> Option<Vec<(u32, u32, u8)>>,
        // The row's marker-family spans. The row is named by `(line, segment)`
        // because two of the families scan the line's text and bound themselves
        // to the columns this row can show.
        marks_for: impl Fn(usize, usize) -> Vec<(usize, usize, Color, Color)>,
        // The per-character attribute classes of one line — empty for every
        // document but a 查找结果 one, which marks its path rows' `@~`.
        flags_for: impl Fn(usize) -> Vec<u8>,
        color_bands_for: impl Fn(usize, usize) -> Vec<(usize, usize, Color)>,
        sel_bands_for: impl Fn(usize) -> Vec<(usize, usize, Color)>,
        // 折叠尾部: whether the line's next line is hidden under a
        // fold (the indicator box), and the collapsed run's last
        // line as a preview text when 折叠尾部字符 is on.
        fold_end_for: impl Fn(usize) -> bool,
        fold_tail_for: impl Fn(usize) -> Option<String>,
        tag_hl: Option<&TagHighlight>,
        sel: &SelectionSet,
        theme: &Theme,
        // `tab_cells`: the tab grid in characters.
        tab_cells: usize,
        view: ViewFlags,
        // 符号显示: the marks, the indent guides and the level background the
        // row pass draws — see [`InvisibleChars`].
        chars_opt: InvisibleChars,
        clip: Rect,
        // Right edge of the caret row's band, in view pixels — see
        // [`Self::draw_line`].
        band_right: f32,
        first_row_y: f32,
        h_scroll: i32,
        caret: (usize, usize),
        extra_carets: &[(usize, usize)],
        components: &mut [Box<dyn RenderComponent>],
        anchor: &WindowAnchor,
        draw: &mut DrawList,
    ) {
        let row_h = theme.line_height;
        for row in rows.clone() {
            let (fline, seg) = match row_to_line_segment(row) {
                Some(v) => v,
                None => continue,
            };
            // 大文件模式不建包装数组（千万行 × 每行一个 Vec 不可
            // 接受）：恒等行索引下行号直达，这里回退到一个空数组
            // —— 单段渲染与未包装行完全一致。
            static UNWRAPPED: WrapArray = WrapArray {
                data: Vec::new(),
                dirty: false,
                cells: 0,
            };
            let wrap = wrap_store.arrays.get(fline).unwrap_or(&UNWRAPPED);
            let highlight = highlight_for(fline);
            let marks = marks_for(fline, seg);
            let line_flags = flags_for(fline);
            let color_bands = color_bands_for(fline, seg);
            let sel_bands = sel_bands_for(fline);
            let fold_end = fold_end_for(fline);
            // the tail text only reaches the last wrap segment, so
            // the closure is only consulted there.
            let fold_tail = if fold_end && wrap.data.get(seg).is_none() {
                fold_tail_for(fline)
            } else {
                None
            };
            Self::draw_line(
                doc,
                fline,
                seg,
                wrap,
                highlight.as_deref(),
                tag_hl,
                &marks,
                &color_bands,
                &sel_bands,
                &line_flags,
                sel,
                theme,
                tab_cells,
                view,
                chars_opt,
                clip,
                band_right,
                first_row_y + (row - rows.start) as f32 * row_h,
                h_scroll,
                caret.0,
                extra_carets,
                fold_end,
                fold_tail.as_deref(),
                anchor,
                draw,
            );
        }
        // render-component pass (margin, gutter,...):
        // per logical line, each component
        // owning the line's whole wrap band
        let mut row = rows.start;
        let mut y = first_row_y;
        while row < rows.end {
            let Some((fline, seg)) = row_to_line_segment(row) else {
                break;
            };
            let top_row = row - seg;
            let line_y = y - seg as f32 * row_h;
            let wrap_rows = wrap_store
                .arrays
                .get(fline)
                .map_or(1, |a| a.count() + 1);
            let row_rect = Rect::new(
                0.0,
                line_y,
                clip.w,
                wrap_rows as f32 * row_h,
            );
            let ctx_rect = row_rect.intersect(&clip);
            if !ctx_rect.is_empty() {
                // The active-line band and the gutter's continuation markers
                // follow the primary caret only
                // multi-caret strip has no line of its own to highlight.
                let caret_subrow = wrap_store
                    .arrays
                    .get(fline)
                    .map_or(0, |a| {
                        a.data
                            .iter()
                            .filter(|&&b| (b as usize) < caret.1)
                            .count()
                    });
                let mut ctx = PaintCtx {
                    clip: ctx_rect,
                    row,
                    line: fline,
                    row_rect,
                    theme,
                    view,
                    draw,
                    display_no: (fline + 1) as i64,
                    tilde: fline > 0 && doc.eol_kind(fline - 1) == 0,
                    wrap_rows,
                    eol_kind: doc.eol_kind(fline),
                    line_state: doc.line_state(fline),
                    folding_on: true,
                    active_subrow: caret_subrow,
                    caret_line: caret.0,
                };
                for c in components.iter_mut() {
                    c.paint(&mut ctx);
                }
            }
            row = top_row + wrap_rows;
            y = line_y + wrap_rows as f32 * row_h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_core::{Document, Range, SelectionSet};

    /// One segment covering the whole line.
    fn wrap_for(line_text: &str) -> WrapArray {
        // the real engine stores break positions only — an unwrapped
        // line's array is empty and the single segment's end falls
        // back to the line length
        WrapArray {
            data: Vec::new(),
            dirty: false,
            cells: line_text.chars().count() as i32,
        }
    }

    fn row(
        text: &str,
        sel: &SelectionSet,
        caret: Option<(usize, usize)>,
        extra_carets: &[(usize, usize)],
    ) -> DrawList {
        row_fold(
            text,
            sel,
            caret,
            extra_carets,
            false,
            None,
            4,
            &[],
            Theme::default().caret_w,
        )
    }

    /// [`row`] with the fold-end indicator armed: `fold_end`
    /// stamps the box, `fold_tail` the preview text after it. `tab_cells` and
    /// `flags` are the two inputs a 查找结果 row brings: the tab grid, and the
    /// per-character attribute classes the painter reads for class 30. `caret_w`
    /// is the primary caret's bar thickness the extra carets copy.
    fn row_fold(
        text: &str,
        sel: &SelectionSet,
        caret: Option<(usize, usize)>,
        extra_carets: &[(usize, usize)],
        fold_end: bool,
        fold_tail: Option<&str>,
        tab_cells: usize,
        flags: &[u8],
        caret_w: f32,
    ) -> DrawList {
        let doc = Document::from_utf8(text.as_bytes());
        let wrap = wrap_for(text);
        let mut theme = Theme::default();
        theme.caret_w = caret_w;
        let clip = Rect::new(0.0, 0.0, 1e6, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            flags,
            sel,
            &theme,
            tab_cells,
            ViewFlags::default(),
            InvisibleChars::default(),
            clip,
            clip.w,
            0.0,
            0,
            caret.map_or(usize::MAX, |c| c.0),
            extra_carets,
            fold_end,
            fold_tail,
            &Default::default(),
            &mut draw,
        );
        draw
    }

    fn paint(
        text: &str,
        sel: &SelectionSet,
        caret: Option<(usize, usize)>,
    ) -> DrawList {
        row(text, sel, caret, &[])
    }

    /// One wrap segment of a line, painted with the whole line's token
    /// runs — the shape a wrapped, syntax-highlighted row brings.
    fn row_seg(text: &str, highlight: &[(u32, u32, u8)], wrap: &WrapArray, seg: usize) -> DrawList {
        let doc = Document::from_utf8(text.as_bytes());
        let theme = Theme::default();
        let clip = Rect::new(0.0, 0.0, 1e6, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            seg,
            wrap,
            Some(highlight),
            None,
            &[],
            &[],
            &[],
            &[],
            &SelectionSet::default(),
            &theme,
            4,
            ViewFlags::default(),
            InvisibleChars::default(),
            clip,
            clip.w,
            0.0,
            0,
            usize::MAX,
            &[],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        draw
    }

    fn text_runs(draw: &DrawList) -> String {
        draw.cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::TextRun { text, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect()
    }

    /// The x a named text run starts at.
    fn run_x(draw: &DrawList, text: &str) -> Option<f32> {
        draw.cmds.iter().find_map(|c| match c {
            DrawCmd::TextRun { x, text: t, .. } if t == text => Some(*x),
            _ => None,
        })
    }

    /// One row drawn with the 符号显示 options armed — the shape every mark
    /// test below brings.
    fn row_chars(text: &str, line: usize, sel: &SelectionSet, chars_opt: InvisibleChars) -> DrawList {
        let doc = Document::from_utf8(text.as_bytes());
        let wrap = wrap_for(text);
        let theme = Theme::default();
        let clip = Rect::new(0.0, 0.0, 1e6, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            line,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            sel,
            &theme,
            4,
            ViewFlags::default(),
            chars_opt,
            clip,
            clip.w,
            0.0,
            0,
            usize::MAX,
            &[],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        draw
    }

    /// 一行按给定的视图选项位绘制，文本区右缘钉在 `clip_w`。选区的两个绘制开关
    /// 都读视图位，而带子要铺到「窗口右侧」就必须有个能量出右缘的裁剪矩形。
    fn row_view(text: &str, sel: &SelectionSet, view: ViewFlags, clip_w: f32) -> DrawList {
        let doc = Document::from_utf8(text.as_bytes());
        let wrap = wrap_for(text);
        let theme = Theme::default();
        let clip = Rect::new(0.0, 0.0, clip_w, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            sel,
            &theme,
            4,
            view,
            InvisibleChars::default(),
            clip,
            clip.w,
            0.0,
            0,
            usize::MAX,
            &[],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        draw
    }

    /// 选区里每一段字的颜色和它的起止。
    fn coloured_runs(draw: &DrawList) -> Vec<(String, Color)> {
        draw.cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::TextRun { text, color, .. } => Some((text.clone(), *color)),
                _ => None,
            })
            .collect()
    }

    /// 「选区文本保持语法高亮的前景色」关掉（出厂值，也就是 `setting.ini` 里没写
    /// 这一行）时选区里的字换成主题的选区前景、选区外不变；打开时两边都用语法色，
    /// 选区只剩底色。
    #[test]
    fn keep_sel_fg_switches_the_selected_text_colour() {
        let mut theme = Theme::default();
        let syntax = Color::rgb(0x00, 0x70, 0x70);
        theme.selection_fg = Color::rgb(0xFF, 0xEE, 0x00);
        let doc = Document::from_utf8(b"ab cd".as_slice());
        let wrap = wrap_for("ab cd");
        let mut sel = SelectionSet::default();
        sel.add(Range {
            start: (0, 3),
            end: (0, 5),
        });
        let flag = |keep: bool| {
            let mut v = ViewFlags::default();
            v.set(ViewFlags::KEEP_SEL_FG, keep);
            v
        };
        let runs = |view: ViewFlags| {
            let clip = Rect::new(0.0, 0.0, 1e6, 1e6);
            let mut draw = DrawList::default();
            PaintPipeline::draw_line(
                &doc,
                0,
                0,
                &wrap,
                Some(&[(syntax.to_u32(), 5, 0)]),
                None,
                &[],
                &[],
                &[],
                &[],
                &sel,
                &theme,
                4,
                view,
                InvisibleChars::default(),
                clip,
                clip.w,
                0.0,
                0,
                usize::MAX,
                &[],
                false,
                None,
                &Default::default(),
                &mut draw,
            );
            coloured_runs(&draw)
        };
        // 关掉：选区前的 `ab ` 保持语法色，选区里的 `cd` 换成选区前景。
        let off = runs(flag(false));
        assert_eq!(
            off,
            vec![
                ("ab ".to_string(), syntax),
                ("cd".to_string(), theme.selection_fg),
            ],
            "关掉时选区里的字要换成选区前景"
        );
        // 打开：整行一种颜色，选区只剩底色。
        let on = runs(flag(true));
        assert_eq!(on, vec![("ab cd".to_string(), syntax)]);
        // 出厂的那张位图没写这一位，所以落回关掉那一种画法。
        assert_eq!(runs(ViewFlags::shipped_defaults()), off);
    }

    /// 命中那一族（`Found`、自定义标记）画的是 span 自己的底色带子，字换成 span
    /// 自己的前景色。带子要是和正文同色就等于没画 —— 随包主题那一份份都得给出
    /// 分得开的底色，`themes.rs` 里按每一份钉一遍，这里钉的是绘制端确实把这两色
    /// 都发了出去。
    #[test]
    fn a_mark_span_lays_its_band_and_writes_its_own_colour() {
        let theme = Theme::default();
        let band = Color::rgb(0x59, 0x49, 0x31);
        let ink = Color::rgb(0xF0, 0xAF, 0x4C);
        assert_ne!(band, theme.background, "带子本身得和正文底色不同，不然测不出什么");
        let text = "ab cd";
        let doc = Document::from_utf8(text.as_bytes());
        let wrap = wrap_for(text);
        let sel = SelectionSet::default();
        let marks = [(3usize, 5usize, band, ink)];
        let clip = Rect::new(0.0, 0.0, 1e6, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            0,
            &wrap,
            Some(&[(theme.foreground.to_u32(), 5, 0)]),
            None,
            &marks,
            &[],
            &[],
            &[],
            &sel,
            &theme,
            4,
            ViewFlags::shipped_defaults(),
            InvisibleChars::default(),
            clip,
            clip.w,
            0.0,
            0,
            usize::MAX,
            &[],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        let filled: Vec<Color> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { color, .. } => Some(*color),
                _ => None,
            })
            .collect();
        assert!(filled.contains(&band), "标记的底色没铺：{filled:?}");
        assert!(
            coloured_runs(&draw)
                .iter()
                .any(|(t, c)| t == "cd" && *c == ink),
            "标记里的字没换成标记的前景: {:?}",
            coloured_runs(&draw)
        );
    }

    /// 「选区文本的背景色扩展到窗口右侧」：整行被同一段选区盖住时，行尾的空白
    /// 也铺上选区底色，一直顶到文本区右缘。选区的最后一行不铺 —— 那行只覆盖到
    /// 选区的结束列。
    #[test]
    fn sel_extend_lays_the_selection_band_to_the_window_edge() {
        let theme = Theme::default();
        let clip_w = 400.0;
        let mut sel = SelectionSet::default();
        sel.add(Range {
            start: (0, 0),
            end: (2, 2),
        });
        let band_right = |extend: bool| {
            let mut view = ViewFlags::default();
            view.set(ViewFlags::SEL_EXTEND, extend);
            let draw = row_view("abc", &sel, view, clip_w);
            draw.cmds
                .iter()
                .filter_map(|c| match c {
                    DrawCmd::AlphaBlend { rect, color, .. } if *color == theme.selection_bg => {
                        Some(rect.x + rect.w)
                    }
                    _ => None,
                })
                .max_by(|a, b| a.total_cmp(b))
                .unwrap_or_default()
        };
        // 关掉（出厂值）：带子只到第三个字。
        let off = band_right(false);
        assert!(off < clip_w, "关掉时不该铺到右缘，实测 {off}");
        // 打开：这一行不是选区的最后一行，带子顶到右缘。
        assert_eq!(band_right(true), clip_w);
        // 换成选区的最后一行，两种姿态都不该多出右缘那一段。
        let mut last = SelectionSet::default();
        last.add(Range {
            start: (0, 0),
            end: (0, 3),
        });
        let mut view = ViewFlags::default();
        view.set(ViewFlags::SEL_EXTEND, true);
        let draw = row_view("abc", &last, view, clip_w);
        assert!(
            !draw.cmds.iter().any(|c| matches!(
                c,
                DrawCmd::AlphaBlend { rect, color, .. }
                    if *color == theme.selection_bg && rect.x + rect.w >= clip_w
            )),
            "选区的最后一行不该扩展到右缘"
        );
    }

    /// in the cell's centre — 2×2 at the shipped style, on the font's cell and
    /// not on the row pitch.
    #[test]
    fn whitespace_marks_a_blank_only_while_the_option_is_on() {
        let theme = Theme::default();
        let off = row_chars("a b", 0, &SelectionSet::default(), InvisibleChars::default());
        assert!(
            !off.cmds.iter().any(|c| matches!(
                c,
                DrawCmd::FillRect { rect, color }
                    if *color == theme.whitespace_fg && rect.w == 2.0 && rect.h == 2.0
            )),
            "the blank must stay bare while 空白字符 is off"
        );
        let on = row_chars(
            "a b",
            0,
            &SelectionSet::default(),
            InvisibleChars {
                whitespace: true,
                ..Default::default()
            },
        );
        let dot = on.cmds.iter().find_map(|c| match c {
            DrawCmd::FillRect { rect, color }
                if *color == theme.whitespace_fg && rect.w == 2.0 && rect.h == 2.0 =>
            {
                Some(*rect)
            }
            _ => None,
        });
        let dot = dot.expect("the blank's dot");
        // The blank is the second cell: x = x_width, centred at +4, minus half
        // the 2 px square.
        assert_eq!(dot.x, theme.x_width + theme.x_width * 0.5 - 1.0);
        assert_eq!(dot.y, (theme.font_height * 0.5).floor() - 1.0);
        // and the blank itself is no longer handed to the text run
        assert_eq!(text_runs(&on), "ab");
    }

    /// 空白字符: the dot keeps its 2×2 size on a tall cell too. A high-DPI row
    /// used to take a 4×4 square, which read as a block on a selected line.
    #[test]
    fn the_blank_dot_stays_two_pixels_on_a_tall_cell() {
        let mut theme = Theme::default();
        theme.font_height = 26.0;
        let opts = InvisibleChars {
            whitespace: true,
            ..Default::default()
        };
        let mut draw = DrawList::default();
        draw_char_mark(&mut draw, &theme, opts, ' ', 0.0, 8.0, 0.0, true);
        let dots: Vec<Rect> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color }
                    if *color == theme.whitespace_fg && rect.w == 2.0 && rect.h == 2.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(dots.len(), 1, "the blank takes one 2×2 dot");
        // centred in the 26 px cell, not in the 18 px row
        assert_eq!(dots[0].y, (26.0f32 * 0.5).floor() - 1.0);
    }

    /// 选择时全部显示: with the latch on, only the blanks a selection covers get
    /// their mark.
    #[test]
    fn show_all_in_selection_marks_the_selected_blanks_only() {
        let theme = Theme::default();
        let mut sel = SelectionSet::default();
        sel.add(dg_core::Range {
            start: (0, 1),
            end: (0, 2),
        });
        let opts = InvisibleChars {
            in_sel: true,
            ..Default::default()
        };
        let draw = row_chars("a b c", 0, &sel, opts);
        let dots = draw
            .cmds
            .iter()
            .filter(|c| {
                matches!(c, DrawCmd::FillRect { color, .. } if *color == theme.whitespace_fg)
            })
            .count();
        assert_eq!(dots, 1, "one blank in the selection, one dot");
        // The selected blank is the one that became a mark; the unselected one
        // still reaches the text run as an ordinary space.
        assert_eq!(text_runs(&draw), "ab c");
    }

    /// A tab is never handed to the text run; 空白字符 draws it in the configured
    /// style, and the shipped style is the `»` glyph.
    #[test]
    fn a_tab_is_drawn_as_its_mark_not_as_text() {
        let theme = Theme::default();
        let off = row_chars("a\tb", 0, &SelectionSet::default(), InvisibleChars::default());
        assert!(!text_runs(&off).contains('\t'));
        assert!(!off.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun { text, .. } if text == "»"
        )));
        let on = row_chars(
            "a\tb",
            0,
            &SelectionSet::default(),
            InvisibleChars {
                whitespace: true,
                tab_style: 4,
                ..Default::default()
            },
        );
        assert!(on.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun { text, color, .. }
                if text == "»" && *color == theme.whitespace_fg
        )));
    }

    /// 控制字符: the three-letter name, in the `ConChar` colours, inside the
    /// character's own cell.
    #[test]
    fn a_control_character_draws_its_name() {
        let theme = Theme::default();
        let off = row_chars("a\u{1}b", 0, &SelectionSet::default(), InvisibleChars::default());
        assert!(!text_runs(&off).contains("SOH"));
        let on = row_chars(
            "a\u{1}b",
            0,
            &SelectionSet::default(),
            InvisibleChars {
                control: true,
                ..Default::default()
            },
        );
        assert!(on.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::TextRun { text, color, .. } if text == "SOH" && *color == theme.con_char_fg
        )));
        assert!(on.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if *color == theme.con_char_bg && rect.x == theme.x_width + 1.0
        )));
    }

    /// 换行符 / 文档结束符: the return arrow lands after the last glyph of a line
    /// that has a terminator, and `[EOF]` after the document's last line — and
    /// only there.
    #[test]
    fn the_eol_arrow_and_the_eof_tag_hang_off_the_text() {
        let theme = Theme::default();
        let opts = InvisibleChars {
            eol: true,
            eof: true,
            ..Default::default()
        };
        // Line 0 of a two-line document: the arrow, no tag.
        let first = row_chars("a\nb", 0, &SelectionSet::default(), opts);
        assert!(!text_runs(&first).contains("[EOF]"));
        assert!(first.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::FillRect { rect, color }
                if *color == theme.whitespace_fg && rect.x >= theme.x_width
        )));
        // The last line: the tag, and no arrow (it carries no terminator).
        let last = row_chars("a\nb", 1, &SelectionSet::default(), opts);
        assert_eq!(text_runs(&last), "b[EOF]");
    }

    /// 缩进线: a guide at every tab stop the leading blanks run past, so the
    /// text's own start column stays clear.
    #[test]
    fn indent_guides_land_on_the_tab_stops() {
        let theme = Theme::default();
        let draw = row_chars(
            "        x",
            0,
            &SelectionSet::default(),
            InvisibleChars {
                indent: true,
                indent_style: 1,
                ..Default::default()
            },
        );
        let guides = |draw: &DrawList| -> Vec<f32> {
            draw.cmds
                .iter()
                .filter_map(|c| match c {
                    DrawCmd::FillRect { rect, color }
                        if *color == theme.indent_line && rect.w == 1.0
                            && rect.h == theme.line_height =>
                    {
                        Some(rect.x)
                    }
                    _ => None,
                })
                .collect()
        };
        // Eight blanks at four cells per stop: the guide at the first stop, and
        // none at 8 — that column is where the `x` sits.
        assert_eq!(guides(&draw), vec![4.0 * theme.x_width]);
        // A row of nothing but blanks keeps the guide at its own stop, so the
        // column does not break across the gap.
        let gap = row_chars(
            "        x\n\n        y",
            1,
            &SelectionSet::default(),
            InvisibleChars {
                indent: true,
                indent_style: 1,
                ..Default::default()
            },
        );
        assert_eq!(
            guides(&gap),
            vec![4.0 * theme.x_width, 8.0 * theme.x_width]
        );
    }

    /// 同层背景色: the rows that share the caret line's indent level get the
    /// tint, and the ones that do not stay plain.
    #[test]
    fn the_level_background_tints_only_the_caret_level() {
        let theme = Theme::default();
        let opts = InvisibleChars {
            level_bg: true,
            caret_level: 2,
            ..Default::default()
        };
        let inside = row_chars("        x", 0, &SelectionSet::default(), opts);
        assert!(inside.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::AlphaBlend { rect, color, .. }
                if *color == theme.foreground && rect.w > 0.0
        )));
        let outside = row_chars("    x", 0, &SelectionSet::default(), opts);
        assert!(!outside.cmds.iter().any(|c| matches!(
            c,
            DrawCmd::AlphaBlend { .. }
        )));
    }

    /// A character the platform measured on a fallback face takes that width,
    /// not a grid cell: the emoji's coloured bitmap is wider than the CJK cell,
    /// so laying it out on the cell put the caret inside the glyph. Everything
    /// else keeps the two-cell grid.
    #[test]
    fn a_measured_char_takes_its_real_advance() {
        let mut t = Theme::default();
        t.measured = vec![('\u{1F600}', 22)];
        let m = LineMetrics::new(&t, 4);
        let line: Vec<char> = "a\u{1F600}中".chars().collect();
        assert_eq!(m.bounds(&line, &[]), vec![0, 8, 30, 46]);
        assert_eq!(m.x_at(&line, 2, &[]), 30);
        assert_eq!(m.x_at(&line, 3, &[]), 46);
        assert_eq!(m.col_at(&m.bounds(&line, &[]), 15), 1);
    }

    /// `☕` is U+2615 and `☕️` U+2615 U+FE0F: different strings and the
    /// same layout. The selector asks for the emoji presentation and draws
    /// nothing, so a column for it moves every character after it one cell to
    /// the right of where the glyph run paints them — the caret lands inside the
    /// text and a click selects the wrong character.
    #[test]
    fn a_variation_selector_takes_no_column() {
        let t = Theme::default();
        let m = LineMetrics::new(&t, 4);
        let bare: Vec<char> = "a\u{2615}b".chars().collect();
        let emoji: Vec<char> = "a\u{2615}\u{FE0F}b".chars().collect();
        for ch in ['\u{FE0F}', '\u{FE0E}', '\u{20E3}'] {
            assert_eq!(m.advance(ch), 0, "U+{:04X}", ch as u32);
        }
        // The two caret stops inside the sequence — before the selector and
        // after it — sit at the same pixel.
        let xs = m.bounds(&emoji, &[]);
        assert_eq!(xs[3], xs[2], "the selector adds no pixel");
        // …and the line ends where the bare string ends.
        assert_eq!(
            m.x_at(&emoji, emoji.len(), &[]),
            m.x_at(&bare, bare.len(), &[])
        );
        // A click just past the coffee lands on the coffee in both spellings.
        assert_eq!(m.col_at(&xs, 10), 1);
        assert_eq!(m.col_at(&m.bounds(&bare, &[]), 10), 1);
    }

    /// The one table three consumers read: the advances, the tab's stop, and the
    /// inverse that answers a pixel's column. `x_at` and `col_at` must be exact
    /// inverses at every boundary — the reported bug was a click on a line with a
    /// tab selecting text to the left of the pointer, which is what a second copy
    /// of this arithmetic drifts into.
    #[test]
    fn line_metrics_round_trip_over_tabs() {        let t = Theme::default();
        let m = LineMetrics::new(&t, 4);
        let line: Vec<char> = "\tab 世界\tx".chars().collect();
        // 0 → 32 (the first stop) → 40 → 48 → 56 (space) → 72 → 88 (two wide
        // cells) → 96 (88 % 32 = 24, so exactly 8 remains to the stop at 96 and the
        // tab takes just that) → 104.
        assert_eq!(
            m.bounds(&line, &[]),
            vec![0, 32, 40, 48, 56, 72, 88, 96, 104]
        );
        for col in 0..=line.len() {
            assert_eq!(
                m.col_at(&m.bounds(&line, &[]), m.x_at(&line, col, &[])),
                col,
                "boundary {col}"
            );
        }
        // The comparison is `>=`, so a stop exactly one character away is
        // the plain case.
        assert_eq!(m.tab_from(25), 39);
        assert_eq!(m.tab_from(24), 8);
    }

    /// Not the single cell the painter used to give it, which is why a 查找结果 row's text no
    /// longer sat under its own caret.
    #[test]
    fn draw_line_tab_lands_on_the_line_stop_grid() {
        let t = Theme::default();
        let draw = row("\tab", &SelectionSet::default(), None, &[]);
        assert_eq!(run_x(&draw, "ab"), Some(4.0 * t.x_width));
        let draw = row("a\tb", &SelectionSet::default(), None, &[]);
        assert_eq!(
            run_x(&draw, "b"),
            Some(4.0 * t.x_width),
            "the stop after column 1, not column 2"
        );
        // The grid is in pixels, so a double-width cell moves the phase by two:
        // after 世界 the tab takes the whole next period.
        let draw = row("世界\tx", &SelectionSet::default(), None, &[]);
        assert_eq!(run_x(&draw, "x"), Some(8.0 * t.x_width));
    }

    /// Attribute class 30 (the caret-line painter's masked-class gate): the
    /// `@~` that opens every 查找结果 path row is drawn only on the caret's row or
    /// under a selection, and takes no width either way — so the path itself
    /// starts flush against the gutter.
    #[test]
    fn draw_line_class_30_is_hidden_off_the_caret_row() {
        let flags = [CLASS_SUPPRESSED, CLASS_SUPPRESSED, 0, 0];
        let draw = row_fold(
            "@~ab",
            &SelectionSet::default(),
            None,
            &[],
            false,
            None,
            4,
            &flags,
            Theme::default().caret_w,
        );
        assert_eq!(text_runs(&draw), "ab");
        assert_eq!(run_x(&draw, "ab"), Some(0.0));
        let draw = row_fold(
            "@~ab",
            &SelectionSet::default(),
            Some((0, 3)),
            &[],
            false,
            None,
            4,
            &flags,
            Theme::default().caret_w,
        );
        assert_eq!(text_runs(&draw), "@~ab");
        let mut sel = SelectionSet::default();
        sel.add(Range {
            start: (0, 0),
            end: (0, 2),
        });
        let draw = row_fold("@~ab", &sel, None, &[], false, None, 4, &flags, Theme::default().caret_w);
        assert_eq!(text_runs(&draw), "@~ab");
    }

    #[test]
    fn draw_line_cjk_does_not_panic() {
        // regression: wrap bounds are char
        // indices; `&line[start..end]` sliced
        // them as byte indices and panicked
        // on CJK ("end byte index … is not a
        // char boundary") inside the
        // non-unwinding WindowProc
        let draw = paint(
            "Hello DogEdit 世界",
            &SelectionSet::default(),
            None,
        );
        let runs = text_runs(&draw);
        assert!(runs.contains("世"));
        assert!(runs.contains("界"));
    }

    /// A token crossing a wrap boundary: the tail segment starts 45 chars
    /// into a 65-char token but is only 20 chars long. The run walk used to
    /// re-add the consumed offset to the emit position
    /// (`run_start = seg_char + consumed_in_run`), so `end - run_start`
    /// underflowed on that short tail — the scrollbar-drag crash on a
    /// wrapped string literal — and every row that survived drew the wrong
    /// characters from the middle of the token.
    #[test]
    fn draw_line_token_spanning_a_wrap_boundary() {
        let line = "abcd ".to_string() + &"t".repeat(65);
        let wrap = WrapArray {
            data: vec![50],
            dirty: false,
            cells: 50,
        };
        let hl = [(0x00ff0000, 5, 0), (0x000000ff, 65, 0)];
        // Head segment: keyword, then the token's first 45 chars.
        let draw = row_seg(&line, &hl, &wrap, 0);
        assert_eq!(text_runs(&draw), format!("abcd {}", "t".repeat(45)));
        // Tail segment: the token's last 20 chars, in the token's colour —
        // the shape that underflowed.
        let draw = row_seg(&line, &hl, &wrap, 1);
        assert_eq!(text_runs(&draw), "t".repeat(20));
    }

    /// 字形位要一路跟到命令上：绘制那一层只认 `DrawCmd::TextRun::style`，
    /// 半路丢掉就只剩颜色了（`[Style]` 白读）。
    #[test]
    fn draw_line_keeps_the_run_style() {
        let hl = [(0x00ff0000u32, 3u32, 1u8), (0x000000ff, 2, 0)];
        let draw = row_seg("abcde", &hl, &wrap_for("abcde"), 0);
        let styles: Vec<u8> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::TextRun { style, .. } => Some(*style),
                _ => None,
            })
            .collect();
        assert_eq!(styles, vec![1, 0], "前三格粗体、后两格正体");
    }

    /// `[Style]` 的装饰位画成 1 px 填色：下划线横跨那一段文字、落在行盒底部，
    /// 双下划线是两条、删除线在中间、框是四条边。
    #[test]
    fn draw_line_decorations_follow_the_style_bits() {
        let ink = Color::from_u32(0x00ff0000);
        let fills = |style: u8| -> Vec<Rect> {
            let hl = [(0x00ff0000u32, 5u32, style)];
            let draw = row_seg("abcde", &hl, &wrap_for("abcde"), 0);
            draw.cmds
                .iter()
                .filter_map(|c| match c {
                    DrawCmd::FillRect { rect, color } if *color == ink => Some(*rect),
                    _ => None,
                })
                .collect()
        };
        let bottom = Theme::default().line_height - 2.0;

        assert!(fills(0).is_empty(), "正体不画装饰");
        let under = fills(STYLE_UNDERLINE);
        assert_eq!(under.len(), 1, "下划线一条: {under:?}");
        assert_eq!((under[0].y, under[0].h), (bottom, 1.0));
        assert!(under[0].w > 10.0, "横跨整段文字: {}", under[0].w);

        assert_eq!(fills(STYLE_DOUBLE).len(), 2, "双下划线两条");
        let strike = fills(STYLE_STRIKE);
        assert_eq!(strike.len(), 1, "删除线一条");
        assert!(strike[0].y > 0.0 && strike[0].y < bottom, "删除线在行中间");
        assert_eq!(fills(STYLE_BOX).len(), 4, "框是四条边");
        // 点线隔一格画一格，波浪线每个像素都画 —— 只比"比实线多"。
        assert!(fills(STYLE_DOTTED).len() > 3, "点线是断续的");
        assert!(fills(STYLE_WAVY).len() > fills(STYLE_UNDERLINE).len(), "波浪线不是一条直线");
        // 一族里同时置好几位：画最花的那一种，不叠加。
        let mixed = fills(STYLE_UNDERLINE | STYLE_WAVY);
        assert_eq!(mixed.len(), fills(STYLE_WAVY).len(), "波浪线吃掉下划线");
    }

    #[test]
    fn draw_line_cjk_selection_overlay() {
        // the overlay walk mixes segment-
        // relative offsets with line char
        // indices — byte offsets mis-measured
        // CJK selections
        let mut sel = SelectionSet::default();
        sel.add(Range {
            start: (0, 0),
            end: (0, 2),
        });
        let draw = paint("世界abc", &sel, None);
        // one blended rect over the 2-char
        // CJK span
        let blends: Vec<&DrawCmd> = draw
            .cmds
            .iter()
            .filter(|c| {
                matches!(c, DrawCmd::AlphaBlend { .. })
            })
            .collect();
        assert_eq!(blends.len(), 1);
        assert_eq!(text_runs(&draw), "世界abc");
    }

    #[test]
    fn draw_line_cjk_caret_overlay() {
        let draw = paint(
            "世界abc",
            &SelectionSet::default(),
            Some((0, 2)),
        );
        assert_eq!(text_runs(&draw), "世界abc");
    }

    #[test]
    fn the_primary_caret_is_not_in_the_draw_list() {
        // The caret that goes missing is the one a strip of pixels has to fight
        // for: the gutter's `LeftMargin` band covers exactly the column-0 pixel
        // a self-drawn caret used. Its caret is the system caret, drawn by GDI
        // outside the paint. So the row pass must emit nothing in the caret
        // colour, on the caret's own row or anywhere else.
        for caret in [(0, 0), (0, 1), (0, 3), (0, 7)] {
            let draw = paint("世界abc", &SelectionSet::default(), Some(caret));
            let theme = Theme::default();
            let strips: Vec<&DrawCmd> = draw
                .cmds
                .iter()
                .filter(|c| {
                    matches!(c, DrawCmd::FillRect { color, .. } if *color == theme.caret)
                })
                .collect();
            assert!(strips.is_empty(), "caret {caret:?} painted {strips:?}");
        }
    }

    /// The caret row's band stops at the text column's right edge: the margin
    /// beside the text and the scrollbar strip keep the plain background, so
    /// the band never runs into the bar.
    #[test]
    fn the_active_line_band_stops_at_the_text_column() {
        let doc = Document::from_utf8("abc".as_bytes());
        let wrap = wrap_for("abc");
        let theme = Theme::default();
        let mut view = ViewFlags::default();
        view.set(ViewFlags::ACTIVE_LINE, true);
        let clip = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            &SelectionSet::default(),
            &theme,
            4,
            view,
            InvisibleChars::default(),
            clip,
            604.0,
            0.0,
            0,
            0,
            &[],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        let bands: Vec<Rect> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color } if *color == theme.current_line_bg => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(bands.len(), 1, "one band on the caret row");
        assert_eq!(bands[0].w, 604.0, "the band ends at the text column");
        // The row behind it still covers the whole viewport, so the margin the
        // band leaves keeps the plain background rather than stale pixels.
        let bg: Vec<Rect> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color } if *color == theme.background => Some(*rect),
                _ => None,
            })
            .collect();
        assert!(
            bg.iter().any(|r| r.w >= 800.0),
            "the row background spans the clip: {bg:?}"
        );
    }

    #[test]
    fn draw_line_cjk_caret_tracks_double_width() {
        // The extra (multi-caret) indicator lines are still self-drawn, and
        // their x must follow the true glyph advances: after "世界" (two
        // double-width cells) a line sits at gutter_x + 2 * cjk_width, not
        // gutter_x + 2 * x_width — and the text runs start after the gutter
        let doc = Document::from_utf8("世界abc".as_bytes());
        let wrap = wrap_for("世界abc");
        let mut theme = Theme::default();
        theme.gutter_x = 64.0;
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            &doc,
            0,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            &SelectionSet::default(),
            &theme,
            4,
            ViewFlags::default(),
            InvisibleChars::default(),
            Rect::new(0.0, 0.0, 1e6, 1e6),
            1e6,
            0.0,
            0,
            usize::MAX,
            &[(0, 2)],
            false,
            None,
            &Default::default(),
            &mut draw,
        );
        let caret = draw.cmds.iter().find_map(|c| match c {
            DrawCmd::FillRect { rect, color }
                if *color == theme.caret_indicator() && rect.w == theme.caret_w =>
            {
                Some(*rect)
            }
            _ => None,
        });
        let caret = caret.expect("caret line");
        assert_eq!(caret.x, 64.0 + 2.0 * theme.cjk_width);
        let first_run = draw.cmds.iter().find_map(|c| match c {
            DrawCmd::TextRun { x, text, .. }
                if text == "世界abc" =>
            {
                Some(*x)
            }
            _ => None,
        });
        assert_eq!(first_run, Some(64.0));
    }

    #[test]
    fn every_extra_caret_gets_one_sel_indicator_line() {
        // The primary caret itself paints nothing here — it is the system
        // caret, drawn outside the paint.
        let theme = Theme::default();
        // two extra carets on this row, at columns 1 and 4
        let draw = row("abcdef", &SelectionSet::default(), Some((0, 3)), &[(0, 1), (0, 4)]);
        let lines: Vec<(f32, f32, f32)> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color } if *color == theme.caret_indicator() => {
                    Some((rect.x, rect.w, rect.h))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                (theme.x_width, theme.caret_w, theme.line_height),
                (4.0 * theme.x_width, theme.caret_w, theme.line_height),
            ],
            "one caret-thick full-height bar per extra caret, in SelIndicator"
        );
        // the colour is the one thing that must not match the primary caret, or
        // the set reads as one caret repeated
        assert_ne!(theme.caret_indicator(), theme.caret);
        // the caret column (3) of the primary row is absent: it is the
        // system caret's, and a caret colour must never reach the draw list
        assert!(
            !lines.iter().any(|&(x, ..)| x == 3.0 * theme.x_width),
            "the primary caret paints no line"
        );
    }

    /// The bar takes the thickness the platform measured for the primary caret —
    /// 2 px at 96 DPI, 3 px from 144, 1 px under 细线 — and a theme that names none
    /// still paints a visible bar.
    #[test]
    fn extra_carets_wear_the_primary_caret_thickness() {
        let theme = Theme::default();
        let bar_w = |caret_w: f32| {
            let draw = row_fold(
                "abcdef",
                &SelectionSet::default(),
                Some((0, 3)),
                &[(0, 1)],
                false,
                None,
                4,
                &[],
                caret_w,
            );
            draw.cmds
                .iter()
                .find_map(|c| match c {
                    DrawCmd::FillRect { rect, color } if *color == theme.caret_indicator() => {
                        Some(rect.w)
                    }
                    _ => None,
                })
                .expect("extra caret bar")
        };
        assert_eq!(bar_w(2.0), 2.0);
        assert_eq!(bar_w(3.0), 3.0, "high DPI fattens the bar with the caret");
        assert_eq!(bar_w(1.0), 1.0, "细线");
        assert_eq!(bar_w(0.0), 1.0, "an unset thickness still paints a visible bar");
    }

    #[test]
    fn an_extra_caret_on_another_row_is_not_painted_here() {
        // draw_line owns one row; a caret that lives on a different logical
        // line belongs to that row's pass
        let theme = Theme::default();
        let draw = row("abcdef", &SelectionSet::default(), Some((1, 0)), &[(2, 1)]);
        assert!(
            !draw
                .cmds
                .iter()
                .any(|c| matches!(c, DrawCmd::FillRect { color, .. } if *color == theme.caret_indicator())),
            "off-row caret painted"
        );
    }

    #[test]
    fn the_indicator_line_is_not_the_caret_color() {
        // The regression this guards: the extra carets used to be painted as
        // a 2-px strip in the primary caret's own colour.
        let theme = Theme::default();
        assert_ne!(theme.caret_indicator(), theme.caret);
        let draw = row("abcdef", &SelectionSet::default(), None, &[(0, 2)]);
        let caret_colored: Vec<&DrawCmd> = draw
            .cmds
            .iter()
            .filter(|c| matches!(c, DrawCmd::FillRect { color, .. } if *color == theme.caret))
            .collect();
        assert!(caret_colored.is_empty(), "{caret_colored:?}");
    }

    #[test]
    fn a_folded_opener_stamps_the_indicator_after_its_text() {
        // 折叠尾部: the box lands at the text's
        // right edge — gutter_x + the line's advance — 21 px wide
        // and 10 tall, with three 2×2 dots inside it. The tail
        // preview text (the update step, FoldingTail) follows the
        // box.
        let theme = Theme::default();
        let draw = row_fold(
            "for x in xs:",
            &SelectionSet::default(),
            None,
            &[],
            true,
            Some("last"),
            4,
            &[],
            Theme::default().caret_w,
        );
        let end_x = theme.gutter_x + 12.0 * theme.x_width;
        let box_rect = draw
            .cmds
            .iter()
            .find_map(|c| match c {
                DrawCmd::FillRect { rect, color }
                    if *color == FOLD_END_FILL && rect.w == 21.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .expect("indicator box");
        assert_eq!(box_rect.x, end_x);
        assert_eq!(box_rect.h, 10.0);
        // three dots, 2×2, at the icon's (2,5), (6,5) and (10,5)
        let dots: Vec<(f32, f32)> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color }
                    if *color == FOLD_END_DOT && rect.w == 2.0 =>
                {
                    Some((rect.x, rect.y))
                }
                _ => None,
            })
            .collect();
        assert_eq!(dots.len(), 3);
        assert_eq!(dots[0].0, end_x + 5.0);
        assert_eq!(dots[1].0, end_x + 9.0);
        assert_eq!(dots[2].0, end_x + 13.0);
        // the preview text right after the box
        let tail = draw.cmds.iter().find_map(|c| match c {
            DrawCmd::TextRun { x, text, .. } if text == "last" => Some(*x),
            _ => None,
        });
        assert_eq!(tail, Some(end_x + 21.0));
    }

    #[test]
    fn an_unfolded_line_stamps_no_indicator() {
        // the box follows `folded[line + 1]` alone — a line with
        // nothing hidden under it paints no tail at all
        let draw = row_fold(
            "plain line",
            &SelectionSet::default(),
            None,
            &[],
            false,
            None,
            4,
            &[],
            Theme::default().caret_w,
        );
        assert!(
            !draw
                .cmds
                .iter()
                .any(|c| matches!(c, DrawCmd::FillRect { color, .. } if *color == FOLD_END_FILL)),
            "no indicator without a fold"
        );
    }

    /// One row of `text` drawn into a `width` px viewport scrolled `h_scroll`
    /// columns in — the deep-scroll case the windowing exists for.
    fn row_scrolled(text: &str, width: f32, h_scroll: i32) -> DrawList {
        let anchor = WindowAnchor::default();
        row_scrolled_memo(text, width, h_scroll, &anchor)
    }

    /// [`row_scrolled`] into a caller-owned window memo: two frames of the same
    /// row, the second one scrolled, share what the first settled on.
    fn row_scrolled_memo(
        text: &str,
        width: f32,
        h_scroll: i32,
        anchor: &WindowAnchor,
    ) -> DrawList {
        let doc = Document::from_utf8(text.as_bytes());
        row_line_scrolled(&doc, 0, width, h_scroll, anchor)
    }

    /// One logical row of `doc` — line `line`, first segment — into the same
    /// kind of viewport. Several rows share one `anchor` the way the rows of a
    /// paint do.
    fn row_line_scrolled(
        doc: &Document,
        line: usize,
        width: f32,
        h_scroll: i32,
        anchor: &WindowAnchor,
    ) -> DrawList {
        row_line_flags(doc, line, width, h_scroll, anchor, &[])
    }

    /// [`row_line_scrolled`] with the row's attribute classes, which is how a
    /// 查找结果 row's hidden `@~` marker reaches the painter.
    fn row_line_flags(
        doc: &Document,
        line: usize,
        width: f32,
        h_scroll: i32,
        anchor: &WindowAnchor,
        flags: &[CharClass],
    ) -> DrawList {
        let mut text = String::new();
        doc.line_text(line, &mut text);
        let wrap = wrap_for(&text);
        let theme = Theme::default();
        let clip = Rect::new(0.0, 0.0, width, 1e6);
        let mut draw = DrawList::default();
        PaintPipeline::draw_line(
            doc,
            line,
            0,
            &wrap,
            None,
            None,
            &[],
            &[],
            &[],
            flags,
            &SelectionSet::default(),
            &theme,
            4,
            ViewFlags::default(),
            InvisibleChars::default(),
            clip,
            clip.w,
            0.0,
            h_scroll,
            usize::MAX,
            &[],
            false,
            None,
            anchor,
            &mut draw,
        );
        draw
    }

    #[test]
    fn a_leftward_scroll_walks_back_from_the_last_window() {
        // `é` is not a printable ASCII cell, so the row takes the measuring
        // path — the one that needs the memo — while still advancing one cell
        // per character.
        let text = "é".repeat(1200);
        let anchor = WindowAnchor::default();
        let x = LineMetrics::new(&Theme::default(), 4).x;
        let _ = row_scrolled_memo(&text, 1e6, 300, &anchor);
        let first = anchor
            .borrow()
            .get(0, 0)
            .expect("the first paint records its window");
        assert!(first.col > 0, "the row did not scroll");
        // One cell to the left: the next paint subtracts one advance instead of
        // measuring the line from its first character.
        let _ = row_scrolled_memo(&text, 1e6, 299, &anchor);
        let second = anchor
            .borrow()
            .get(0, 0)
            .expect("the second paint records its window");
        assert_eq!(second.col, first.col - 1);
        assert_eq!(second.px, first.px - x);
    }

    #[test]
    fn each_row_of_a_paint_keeps_its_own_window() {
        // The rows of one paint share the anchor table. Held to a single row,
        // every row but the first would read the window of the row above it —
        // a different line, or the next segment of the same one — and either
        // miss and re-measure from the segment's first character or reuse a
        // column that belongs to other text.
        let text = format!(
            "{}\n{}",
            "é".repeat(4000),
            "ō".repeat(4000),
        );
        let doc = Document::from_utf8(text.as_bytes());
        let anchor = WindowAnchor::default();
        let _ = row_line_scrolled(&doc, 0, 800.0, 300, &anchor);
        let _ = row_line_scrolled(&doc, 1, 800.0, 300, &anchor);
        let w0 = anchor.borrow().get(0, 0).expect("row 0 kept its window");
        let w1 = anchor.borrow().get(1, 0).expect("row 1 kept its window");
        assert!(w0.col > 0 && w1.col > 0, "neither row scrolled");
        assert_eq!(w0.line, 0);
        assert_eq!(w1.line, 1);
        // Scrolling the second row rewrites only its own entry.
        let _ = row_line_scrolled(&doc, 1, 800.0, 600, &anchor);
        assert_eq!(
            anchor.borrow().get(0, 0).map(|w| w.col),
            Some(w0.col),
            "row 0's window was overwritten by row 1"
        );
        assert!(
            anchor.borrow().get(1, 0).unwrap().col > w1.col,
            "row 1 did not move its window"
        );
    }

    /// The row's drawn text, in the order it lands: the window the row put on
    /// screen, as the draw list shows it.
    fn drawn_texts(draw: &DrawList) -> Vec<String> {
        draw.cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::TextRun { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_warm_window_draws_what_a_cold_paint_draws() {
        // Two lines, neither of single-cell glyphs, so every row takes the
        // measuring path and reads the shared table. The rows of one paint
        // interleave, and a scroll jumps back and forth: whatever a row finds
        // in the table, it has to draw the window a row drawing into a fresh
        // table would draw.
        let text = format!("{}\n{}", "é".repeat(3000), "ō".repeat(3000));
        let doc = Document::from_utf8(text.as_bytes());
        let anchor = WindowAnchor::default();
        for scroll in [0, 137, 900, 640, 1200, 640] {
            for line in [0usize, 1] {
                let warm = row_line_scrolled(&doc, line, 800.0, scroll, &anchor);
                let cold = row_line_scrolled(
                    &doc,
                    line,
                    800.0,
                    scroll,
                    &WindowAnchor::default(),
                );
                assert_eq!(
                    drawn_texts(&warm),
                    drawn_texts(&cold),
                    "line {line} scrolled {scroll}: the table gave a different window"
                );
                assert!(!drawn_texts(&warm).is_empty(), "line {line} drew nothing");
            }
        }
    }

    #[test]
    fn a_hidden_run_costs_the_window_no_width_either() {
        // The `@~` marker of a 查找结果 row takes neither glyph nor width. The
        // window has to be placed under that same rule the row's geometry uses,
        // or the part of the viewport the hidden run "ate" stays blank.
        let text = format!("{}{}", "@".repeat(60), "abcdefghij".repeat(60));
        let doc = Document::from_utf8(text.as_bytes());
        let flags: Vec<CharClass> = (0..text.chars().count())
            .map(|i| if i < 60 { CLASS_SUPPRESSED } else { 0 })
            .collect();
        let anchor = WindowAnchor::default();
        let warm = row_line_flags(&doc, 0, 800.0, 0, &anchor, &flags);
        let drawn: String = drawn_texts(&warm).concat();
        let x = LineMetrics::new(&Theme::default(), 4).x.max(1);
        let cells = (800.0 as i32 / x) as usize;
        assert!(!drawn.contains('@'), "a hidden character was drawn");
        assert!(
            drawn.chars().count() >= cells,
            "the row drew {} of the {} columns the viewport holds — the hidden run \
             was charged width while placing the window",
            drawn.chars().count(),
            cells
        );
        // The second paint reads the table the first one wrote and draws the
        // same columns.
        let again = row_line_flags(&doc, 0, 800.0, 0, &anchor, &flags);
        assert_eq!(drawn_texts(&again), drawn_texts(&warm));
    }

    #[test]
    fn a_backwards_walk_subtracts_advances_and_refuses_tabs() {
        let theme = Theme::default();
        let m = LineMetrics::new(&theme, 4);
        let x = m.x;
        let chars: Vec<char> = "abcdefghij".chars().collect();
        assert_eq!(m.col_at_px_back(&chars, &[], 6, 6 * x, 4 * x), Some((4, 4 * x)));
        // A tab's advance depends on the pixel it starts at, so it cannot be
        // subtracted: the caller measures from the segment's start instead.
        let tabbed: Vec<char> = "abc\tdef".chars().collect();
        assert_eq!(m.col_at_px_back(&tabbed, &[], 6, 6 * x, 2 * x), None);
        // Walking back from the first column stays there.
        assert_eq!(m.col_at_px_back(&chars, &[], 0, 0, 0), Some((0, 0)));
    }

    #[test]
    fn a_million_char_row_only_materializes_its_window() {
        // 1 M characters on one line, scrolled to the middle: the row hands the
        // draw list one screenful of glyphs. A regression here shows up as a run
        // the length of the line.
        let text: String = "0123456789".repeat(100_000);
        let chars: Vec<char> = text.chars().collect();
        let draw = row_scrolled(&text, 800.0, 600_000);
        let runs: Vec<(&f32, &String)> = draw
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::TextRun { x, text, .. } => Some((x, text)),
                _ => None,
            })
            .collect();
        assert!(!runs.is_empty(), "the window drew nothing");
        for (_, t) in &runs {
            assert!(
                t.chars().count() <= 200,
                "run of {} chars — the whole line leaked into the draw list",
                t.chars().count()
            );
        }
        assert!(draw.cmds.len() < 16, "{} commands", draw.cmds.len());
        // The window starts at the column the scroll put at the left edge.
        assert_eq!(runs[0].1.chars().next(), Some(chars[600_000]));
        assert!(*runs[0].0 >= -1.0 && *runs[0].0 <= 1.0);
    }

    #[test]
    fn a_row_scrolled_past_its_end_draws_nothing() {
        // The window is empty once the line is entirely left of the view; the
        // EOL and EOF marks hang off the end and must not be stamped at a pixel
        // the row never measured.
        let draw = row_scrolled("short", 800.0, 100_000);
        assert!(!draw.cmds.iter().any(|c| matches!(c, DrawCmd::TextRun { .. })));
    }

    #[test]
    fn bounds_range_continues_the_full_table() {
        let text = "ab\tcd\u{4e2d}e";
        let chars: Vec<char> = text.chars().collect();
        let theme = Theme::default();
        let m = LineMetrics::new(&theme, 4);
        let full = m.bounds(&chars, &[]);
        let (from, px) = m.col_at_px(&chars, &[], full[2]);
        assert_eq!((from, px), (2, full[2]));
        let mut out = Vec::new();
        m.bounds_range(&chars, from, chars.len(), px, &[], &mut out);
        assert_eq!(out, full[2..]);
    }

    #[test]
    fn col_at_target_agrees_with_the_table_walk() {
        let text = "ab\tcd\u{4e2d}ef";
        let chars: Vec<char> = text.chars().collect();
        let theme = Theme::default();
        let m = LineMetrics::new(&theme, 4);
        let xs = m.bounds(&chars, &[]);
        for target in -20..xs.last().copied().unwrap() + 20 {
            assert_eq!(
                m.col_at_target(&chars, &[], target),
                m.col_at(&xs, target),
                "target {target}"
            );
        }
    }

    #[test]
    fn a_uniform_row_places_its_window_by_division() {
        // The closed form the window uses on a row of single-cell glyphs has
        // to land on the same column the walk does, at every budget.
        let text = "a".repeat(4000);
        let chars: Vec<char> = text.chars().collect();
        let theme = Theme::default();
        let m = LineMetrics::new(&theme, 4);
        for budget in [-8, 0, 1, m.x - 1, m.x, m.x + 1, 3 * m.x + 5, 4000 * m.x - 1] {
            assert_eq!(
                cells_before(budget, m.x),
                m.col_at_px(&chars, &[], budget).0,
                "budget {budget}"
            );
        }
    }
}
