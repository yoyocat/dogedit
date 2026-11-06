//! The caret: a system caret on the edit view's own HWND.
//!
//! The caret object holds the HWND plus the flags every call checks first;
//! four routines cover geometry, creation, positioning and the position
//! update (text rect, scroll, wrapped row, park off-screen).
//!
//! It has to be the system caret: rows are painted into the same DC the caret
//! lives in, and GDI draws the caret after the window's paint. A painted
//! strip would fight the gutter strips for the same column-0 pixel, which is
//! how the caret went missing in an earlier iteration.
//!
//! Blinking is the system's job: no `SetCaretBlinkTime` call and no caret
//! timer.

// The wrappers below are one call deep into the caret API each, the same elision
// the rest of the Win32 backend uses.
#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCaret, DestroyCaret, HideCaret, SetCaretPos, ShowCaret,
};

/// Caret style — `setting.ini`'s `Edit/CaretStyle`, and the four items the
/// caret-style menu offers (the values: 1 Default, 2 Thin, 3 Char Width, 4 Dos
/// Style). This repository's shipped default is 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaretStyle {
    /// A 2 px bar (3 px at high DPI).
    Default,
    /// 细线 — a 1 px bar (2 px at high DPI).
    Thin,
    /// 字符宽度 — the bar is as wide as the character under it.
    CharWidth,
    /// DOS 风格 — a 2 px underbar pinned to the row's bottom edge.
    Dos,
}

impl CaretStyle {
    pub fn from_setting(v: i64) -> Self {
        match v {
            2 => Self::Thin,
            3 => Self::CharWidth,
            4 => Self::Dos,
            _ => Self::Default,
        }
    }
}

/// `setting.ini`'s `Edit/CaretEffect` have. Every other value only changes
/// what the *layer* draws and how its opacity moves, never the caret's
/// geometry — `caret_size`/`caret_point` stay the single source of truth for
/// that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaretEffect {
    Off,
    /// 呼吸 — a sine fade between full and 55 %.
    Breath,
    /// 呼吸 + 柔光 — the breath, plus a baked gradient halo beside the bar.
    BreathGlow,
    /// 呼吸 + 柔光 + 击键脉冲 — a keystroke dips the layer and re-arms the breath.
    Pulse,
    /// 全套 — the above, plus the 90 ms glide on a caret move.
    Full,
}

impl CaretEffect {
    /// Values outside 0..=4 read as `Off`, the same tolerance `from_setting`
    /// gives the four reference styles.
    pub fn from_setting(v: i64) -> Self {
        match v {
            1 => Self::Breath,
            2 => Self::BreathGlow,
            3 => Self::Pulse,
            4 => Self::Full,
            _ => Self::Off,
        }
    }

    /// The fade replaces the hard blink from 呼吸 up.
    pub fn breath(self) -> bool {
        self != Self::Off
    }

    /// 柔光: the halo the layer bakes into its surface.
    pub fn glow(self) -> bool {
        matches!(self, Self::BreathGlow | Self::Pulse | Self::Full)
    }

    /// 击键脉冲.
    pub fn pulse(self) -> bool {
        matches!(self, Self::Pulse | Self::Full)
    }

    /// 滑动.
    pub fn glide(self) -> bool {
        self == Self::Full
    }
}

/// One full blink cycle in seconds — the system's on interval times two, which
/// is the pair `GetCaretBlinkTime` describes. `AddRepeat` loops on this, and the
/// breath uses the same period so the cadence does not change with the effect.
///
/// The floor is 200 ms: a system configured to blink faster than that would
/// strobe, and the compositor would be animating a layer nobody can read.
pub fn blink_period_s(blink_ms: u32) -> f64 {
    let ms = blink_ms.max(50) as f64;
    (ms * 2.0 / 1000.0).max(0.2)
}

/// The breath's curve: `bias + amplitude * sin(2π(f t + phase))`, starting at
/// its peak (phase 0.25) so a caret that just moved or just appeared is solid.
pub const BREATH_BIAS: f32 = 0.775;
pub const BREATH_AMPLITUDE: f32 = 0.225;
pub const BREATH_PHASE: f32 = 0.25;

/// The font numbers the caret geometry reads from the text font.
pub struct CaretFont {
    /// The painted row step — `theme.line_height` in this implementation. With the line
    /// spacing at 0 they are the same quantity. This implementation has one face and one
    /// row pitch (re-read from GDI on every DPI change), so one number carries both roles.
    pub row_pitch: i32,
    /// `tmAveCharWidth`.
    pub avg_width: i32,
    /// The `'x'` advance — the substitute when the average width never got
    /// measured. Tabs stop on multiples of it, so the model always has it.
    pub x_advance: i32,
}

impl CaretFont {
    /// The width of one average column: `avg_width`, or the `'x'` advance when
    /// that is not positive.
    pub fn avg(&self) -> i32 {
        if self.avg_width > 0 {
            self.avg_width
        } else if self.x_advance > 0 {
            self.x_advance
        } else {
            self.avg_width
        }
    }

    /// The row step here, for the same reason as above.
    pub fn caret_height(&self) -> i32 {
        self.row_pitch
    }
}

/// The bar styles' caret thickness: 2 px (默认) or 1 px (细线), one pixel fatter
/// from 144 DPI. The multi-caret marks the row pass paints copy this, because a
/// cell-sized block over every extra caret would bury the character under it.
pub fn caret_bar_width(style: CaretStyle, dpi_y: i32) -> i32 {
    let base = if style == CaretStyle::Thin { 1 } else { 2 };
    if dpi_y >= 144 { base + 1 } else { base }
}

/// the caret geometry routine — the caret's pixel box. This implementation's own dual-cell
/// advance, the same one the painter and the hit test use. `dpi_y` is
/// `GetDeviceCaps(LOGPIXELSY)`. Returns `(width, height)`.
pub fn caret_size(
    style: CaretStyle,
    overwrite: bool,
    dpi_y: i32,
    font: &CaretFont,
    under: Option<char>,
    char_advance: i32,
) -> (i32, i32) {
    let avg = font.avg();
    // The three styles whose width follows the text: 覆盖 mode, 字符宽度, and
    // DOS. They rebuild on every call, because the character under the caret
    // changed with it.
    if overwrite || style == CaretStyle::CharWidth || style == CaretStyle::Dos {
        let mut w = char_advance;
        // The average width is substituted for a character the measurer could
        // not size — unless that character is one of the two halves of a
        // surrogate pair. Nothing else sits in either. It reads as a range
        // check because the character comes out of the line's UTF-16 buffer,
        // where a lone surrogate can only be half of an astral character.
        if w <= 0 && !is_surrogate_half(under) {
            w = font.avg_width;
        }
        // End of line, a tab, or anything narrower than the average takes the
        // average.
        if under.is_none() || under == Some('\t') || w < avg {
            w = avg;
        }
        // 2 px is the DOS underbar, and only while inserting: 覆盖 mode takes
        // the full-height block even in DOS style.
        let h = if overwrite || style != CaretStyle::Dos {
            font.caret_height()
        } else {
            2
        };
        (w, h)
    } else {
        // The bar styles, and the branch the caret geometry routine reaches when the caret
        // already exists: it returns to the show-tail without rebuilding
        // (the created flag — a bar is built once and only ever re-shown).
        let w = caret_bar_width(style, dpi_y);
        (w, font.caret_height())
    }
}

/// In UTF-16 terms the character under the caret is a surrogate half — the first
/// WCHAR of an astral character, which the pixel measure legitimately answers zero
/// for. This implementation's caret column indexes whole `char`s.
fn is_surrogate_half(ch: Option<char>) -> bool {
    let Some(c) = ch else {
        return false;
    };
    c as u32 > 0xFFFF
}

/// The inputs the caret position routine works from — every one of them is view
/// state the caller reads.
pub struct CaretPlacement {
    /// The loader thread is still alive or the document is still opening: both
    /// park the caret at (−100, −100) rather than guess at a position the
    /// loader has not reached
    /// yet. The other early exit — no caret object at all — is "the view
    /// window does not exist" in this implementation, and is checked by the
    /// caller.
    pub loading: bool,
    /// The text area — left, top, right, bottom — in the view's client space:
    /// right of the gutter, below the ruler.
    pub rect: (i32, i32, i32, i32),
    /// The caret's pixel offset inside its line, after the
    /// cell-grid snap.
    pub x_in_line: i32,
    /// The caret's **absolute** visual row: its line's first row
    /// plus the sub-row inside the line. It has to be absolute,
    /// because a row number on its own never reaches the y formula below — the y is
    /// `first_row` and this row times the same row step, so a wrapped caret
    /// only lands on its own row through this field.
    pub subrow: i32,
    /// The first visible visual row.
    pub first_row: i32,
    /// Horizontal scroll, in character columns.
    pub h_scroll_cols: i32,
    /// The row step: the cell height, the line spacing and one more pixel added
    /// together; this implementation's `theme.line_height`, which is the same
    /// sum with the line spacing (`Theme::line_spacing`) folded in.
    pub row_pitch: i32,
    /// 字符宽度/覆盖/DOS all measure the caret's x in real advances, but the
    /// scroll term stays in average-width columns (the average width, or the `'x'`
    /// advance when it was never measured).
    pub avg: i32,
    /// The DOS style: the underbar moves to the row's bottom edge.
    pub dos_style: bool,
    /// The caret's own height — the value the underbar shift is measured
    /// against.
    pub caret_height: i32,
    /// 额外行距（设置行高）：插入符的 y 在行顶之上再加它的一半，和行的
    /// 文字一样在多出来的空间里居中。整除，与行步进的整数语义一致。
    pub line_spacing: i32,
}

/// the caret position routine — the caret's point in the view's client space. The caret's x
/// is already on the grid. `the_grid_snap_is_an_identity_in_the_port` pins that.
pub fn caret_point(p: &CaretPlacement) -> (i32, i32) {
    if p.loading {
        return (-100, -100);
    }
    let (left, top, right, bottom) = p.rect;
    let step = p.row_pitch;
    let x = left - p.h_scroll_cols * p.avg + p.x_in_line;
    let mut y = top - p.first_row * step + p.subrow * step + p.line_spacing / 2;
    // `IsRectEmpty(...) || PtInRect(...)`, the single test that decides both
    // fates: an empty text area counts as "inside", and only a non-empty area
    // the point misses parks the caret
    // x.
    let empty = left >= right || top >= bottom;
    let inside = empty || (x >= left && x < right && y >= top && y < bottom);
    if !inside {
        y = -100;
    }
    // The underbar's shift is the row step minus the caret height plus one, and
    // it lives *inside* the is-in-area branch: a parked
    // underbar is not re-based, so it stays parked at −100 rather than sliding
    // back into the text area by the height difference.
    if inside && p.dos_style {
        y += step - p.caret_height + 1;
    }
    // The bar straddles the cell boundary it sits on: one pixel left of the
    // advance, which is where the caret's own left edge lands on the grid.
    (x - 1, y)
}

/// The rectangle the composition layer clips itself to, given the text area:
/// the text area grown one pixel to the left.
///
/// The bar is placed at `x - 1` (see [`caret_point`]) so it straddles its
/// column's left boundary, which means the leftmost column's bar starts one
/// pixel *outside* the text area. A clip flush with the text area would shave
/// that pixel off and leave a 1 px hairline exactly where the caret spends most
/// of its time. The clip is there to keep a halo out of the line numbers, not
/// to trim the bar, so it starts where the bar does.
pub fn caret_clip(rect: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    (rect.0 - 1, rect.1, rect.2, rect.3)
}

/// The caret object the view keeps: its window, whether it has been created,
/// whether its last position landed on screen, whether it is visible, and its
/// width and height. A custom bitmap and a caret colour are left out.
pub struct Caret {
    /// The window the caret belongs to. `CreateCaret` binds the
    /// caret to the thread's focus window, which is why this has to be the edit
    /// view and not the frame: a caret created on the frame is clipped away by
    /// the child window that covers the text.
    pub hwnd: HWND,
    created: bool,
    /// Whether the last `SetCaretPos` landed on screen.
    pub pos_valid: bool,
    visible: bool,
    pub width: i32,
    pub height: i32,
}

impl Caret {
    pub fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            created: false,
            pos_valid: false,
            visible: false,
            width: 0,
            height: 0,
        }
    }

    /// the caret create — rebuild the caret at `w` x `h`. This implementation has no
    /// vertical layout, so the swap is left out rather than carried as a flag that is
    /// always off. A repeated call with the same size still costs a `CreateCaret` — which
    /// is why the caret geometry routine guards the bar styles with an `if created` early
    /// return.
    pub unsafe fn create(&mut self, w: i32, h: i32) {
        self.width = w;
        self.height = h;
        if self.created {
            self.visible = false;
            self.created = false;
            let _ = DestroyCaret();
        }
        self.created = true;
        let _ = CreateCaret(self.hwnd, None, w, h);
    }

    /// the caret position setter — move the caret, and record whether it landed.
    pub unsafe fn set_pos(&mut self, x: i32, y: i32) -> bool {
        if !self.created {
            self.pos_valid = false;
            return false;
        }
        let _ = SetCaretPos(x, y);
        self.pos_valid = x >= 0 && y >= 0;
        self.pos_valid
    }

    /// The tail of the caret geometry routine: `if (created && !visible) ShowCaret`.
    pub unsafe fn show(&mut self) {
        if self.created && !self.visible {
            let _ = ShowCaret(Some(self.hwnd));
            self.visible = true;
        }
    }

    /// What `WM_KILLFOCUS` does, and what the editor does whenever
    /// its selection stops being visible.
    pub unsafe fn hide(&mut self) {
        if self.created && self.visible {
            let _ = HideCaret(Some(self.hwnd));
            self.visible = false;
        }
    }

    /// `WM_KILLFOCUS` after the hide: both flags down, caret gone.
    pub unsafe fn destroy(&mut self) {
        if self.created {
            self.visible = false;
            self.pos_valid = false;
            self.created = false;
            let _ = DestroyCaret();
        }
    }

    pub fn created(&self) -> bool {
        self.created
    }
}

impl Default for Caret {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consolas at 100%: an 8 px average advance and an 18 px row, the numbers
    /// the measurer here reports at that scale.
    const FONT: CaretFont = CaretFont {
        row_pitch: 18,
        avg_width: 8,
        x_advance: 8,
    };

    fn size(style: CaretStyle, overwrite: bool, dpi: i32, ch: Option<char>, adv: i32) -> (i32, i32) {
        caret_size(style, overwrite, dpi, &FONT, ch, adv)
    }

    #[test]
    fn bar_styles_are_the_menu302_widths() {
        // 默认 2 px, 细线 1 px, both one row tall.
        assert_eq!(size(CaretStyle::Default, false, 96, Some('a'), 8), (2, 18));
        assert_eq!(size(CaretStyle::Thin, false, 96, Some('a'), 8), (1, 18));
        // ≥144 dpi adds a pixel to both (DPI >= 144).
        assert_eq!(size(CaretStyle::Default, false, 144, Some('a'), 8), (3, 18));
        assert_eq!(size(CaretStyle::Thin, false, 144, Some('a'), 8), (2, 18));
        assert_eq!(size(CaretStyle::Default, false, 143, Some('a'), 8), (2, 18));
    }

    /// The thickness the multi-caret marks copy. It follows the style and the DPI,
    /// and never the character under the caret: a cell-sized block on every extra
    /// caret would bury the text there.
    #[test]
    fn bar_width_is_the_multi_caret_thickness() {
        assert_eq!(caret_bar_width(CaretStyle::Default, 96), 2);
        assert_eq!(caret_bar_width(CaretStyle::Thin, 96), 1);
        assert_eq!(caret_bar_width(CaretStyle::Default, 144), 3);
        // The styles whose own caret follows the text still leave the marks bars,
        // the same bar 默认 wears at that DPI.
        assert_eq!(caret_bar_width(CaretStyle::CharWidth, 96), 2);
        assert_eq!(caret_bar_width(CaretStyle::Dos, 144), 3);
        assert_eq!(caret_bar_width(CaretStyle::Default, 96), size(CaretStyle::Default, false, 96, Some('a'), 8).0);
    }

    #[test]
    fn char_width_style_follows_the_character() {
        assert_eq!(size(CaretStyle::CharWidth, false, 96, Some('世'), 16), (16, 18));
        // End of line, a tab, or anything narrower than the average all take
        // the average width.
        assert_eq!(size(CaretStyle::CharWidth, false, 96, Some('a'), 3), (8, 18));
        assert_eq!(size(CaretStyle::CharWidth, false, 96, Some('\t'), 24), (8, 18));
        assert_eq!(size(CaretStyle::CharWidth, false, 96, None, 0), (8, 18));
    }

    #[test]
    fn overwrite_block_and_dos_underbar() {
        // 覆盖 mode: a character-sized block, full height, in any style.
        assert_eq!(size(CaretStyle::Default, true, 96, Some('a'), 8), (8, 18));
        assert_eq!(size(CaretStyle::Dos, true, 96, Some('世'), 16), (16, 18));
        // DOS while inserting: the 2 px underbar, at the row's bottom.
        assert_eq!(size(CaretStyle::Dos, false, 96, Some('a'), 8), (8, 2));
    }

    #[test]
    fn avg_falls_back_to_the_x_advance() {
        let f = CaretFont {
            row_pitch: 18,
            avg_width: 0,
            x_advance: 7,
        };
        assert_eq!(f.avg(), 7);
        assert_eq!(caret_size(CaretStyle::Default, true, 96, &f, None, 0), (7, 18));
    }

    fn placement(x_in_line: i32, subrow: i32, first_row: i32) -> CaretPlacement {
        CaretPlacement {
            loading: false,
            rect: (64, 20, 800, 600),
            x_in_line,
            subrow,
            first_row,
            h_scroll_cols: 0,
            row_pitch: 18,
            avg: 8,
            dos_style: false,
            caret_height: 18,
            line_spacing: 0,
        }
    }

    #[test]
    fn point_is_the_text_rect_origin_plus_the_advance() {
        // The first row of the first visible line: the caret sits one pixel left
        // of the cell boundary.
        assert_eq!(caret_point(&placement(0, 0, 0)), (63, 20));
        assert_eq!(caret_point(&placement(80, 0, 0)), (143, 20));
        // Scrolling is relative: the caret's absolute visual row against the
        // first visible one. Same row as the top of the view → the top.
        assert_eq!(caret_point(&placement(0, 4, 4)), (63, 20));
        // Two rows further down costs two row pitches.
        assert_eq!(caret_point(&placement(0, 6, 4)), (63, 56));
        // A wrap sub-row is the same step, because the absolute row the caller
        // passes already includes it.
        assert_eq!(caret_point(&placement(0, 2, 0)), (63, 56));
        // Horizontal scroll is charged in average-width columns, not in the
        // caret's own advances: a caret 5 cells into its line with the view
        // scrolled 3 columns right lands 2 columns inside the text area.
        let mut p = placement(40, 0, 0);
        p.h_scroll_cols = 3;
        assert_eq!(caret_point(&p), (64 + 16 - 1, 20));
    }

    /// 设置行高：插入符的 y 在行顶之上再加额外行距的一半（整除），x 不动。
    /// 3 px 的额外行距挪 1 px —— 和行步进的整数语义一致。
    #[test]
    fn extra_line_spacing_shifts_the_caret_down_half_of_itself() {
        let mut p = placement(0, 0, 0);
        p.line_spacing = 4;
        assert_eq!(caret_point(&p), (63, 22));
        p.line_spacing = 3;
        assert_eq!(caret_point(&p), (63, 21));
    }

    #[test]
    fn point_parks_off_screen_while_loading_or_outside_the_text_area() {
        let mut p = placement(0, 0, 0);
        p.loading = true;
        assert_eq!(caret_point(&p), (-100, -100));
        // Below the last row: only y is parked, and the caret reports no valid
        // position, which is what keeps the paint path from
        // trying to reveal it.
        let mut p = placement(0, 0, 0);
        p.subrow = 40;
        assert_eq!(caret_point(&p), (63, -100));
        // The DOS underbar's row-bottom shift lives *inside* the in-area branch,
        // so a caret that already missed the area is not slid back into it.
        let mut p = placement(0, 40, 0);
        p.dos_style = true;
        p.caret_height = 2;
        assert_eq!(caret_point(&p), (63, -100));
        // Above the first visible row is a miss too — the caret's line has been
        // scrolled off
        // ruler.
        let p = placement(0, 2, 4);
        assert_eq!(caret_point(&p), (63, -100));
        // Scrolled past on the x axis: the scroll charge leaves x to the left of
        // the text area, `PtInRect` fails
        // a caret scrolled off the edge disappears instead of drawing into the
        // gutter.
        let mut p = placement(0, 0, 0);
        p.h_scroll_cols = 3;
        assert_eq!(caret_point(&p), (39, -100));
    }

    #[test]
    fn the_grid_snap_is_an_identity_in_the_port() {
        // Every advance this implementation measures is an integer multiple of the
        // average width — ASCII is one cell, CJK exactly two — so the snap cannot move
        // it: a caret at column 4 sits where the plain advance puts it, for both cell
        // widths.
        for col in 0..24 {
            let mut p = placement(col * 8, 0, 0);
            assert_eq!(caret_point(&p).0, 64 + col * 8 - 1);
            p.x_in_line = col * 16;
            assert_eq!(caret_point(&p).0, 64 + col * 16 - 1);
        }
    }

    #[test]
    fn the_layer_clip_keeps_the_leftmost_column_whole() {
        // Column 0 of the first visible row: the bar is [x, x + w), and the
        // clip has to contain all of it — a clip at the text area's own left
        // edge cut the overhanging pixel and left a 1 px hairline.
        let p = placement(0, 0, 0);
        let (x, _) = caret_point(&p);
        let (w, _) = caret_size(CaretStyle::Default, false, 96, &FONT, Some('a'), 8);
        let clip = caret_clip(p.rect);
        assert_eq!(clip.0, x);
        assert!(x + w <= clip.2);
        // Everything else is the text area untouched.
        assert_eq!((clip.1, clip.2, clip.3), (p.rect.1, p.rect.2, p.rect.3));
    }

    #[test]
    fn dos_underbar_sits_at_the_row_bottom() {
        let mut p = placement(0, 0, 0);
        p.dos_style = true;
        p.caret_height = 2;
        // `y += row_pitch - caret_height + 1` — the last two pixels of the row.
        assert_eq!(caret_point(&p), (63, 20 + 17));
    }

    #[test]
    fn menu302_values_map_to_the_four_styles() {
        assert_eq!(CaretStyle::from_setting(1), CaretStyle::Default);
        assert_eq!(CaretStyle::from_setting(2), CaretStyle::Thin);
        assert_eq!(CaretStyle::from_setting(3), CaretStyle::CharWidth);
        assert_eq!(CaretStyle::from_setting(4), CaretStyle::Dos);
        assert_eq!(CaretStyle::from_setting(0), CaretStyle::Default);
        assert_eq!(CaretStyle::from_setting(99), CaretStyle::Default);
    }

    #[test]
    fn caret_effect_presets_escalate() {
        assert_eq!(CaretEffect::from_setting(0), CaretEffect::Off);
        assert_eq!(CaretEffect::from_setting(4), CaretEffect::Full);
        // Outside the menu's range is 关, never a guess.
        assert_eq!(CaretEffect::from_setting(9), CaretEffect::Off);
        assert_eq!(CaretEffect::from_setting(-1), CaretEffect::Off);
        // Each preset is a superset of the one below it, which is what lets the
        // settings row be a single list instead of four independent switches.
        for e in [CaretEffect::Off, CaretEffect::Breath, CaretEffect::BreathGlow, CaretEffect::Pulse] {
            let next = CaretEffect::from_setting(match e {
                CaretEffect::Off => 1,
                CaretEffect::Breath => 2,
                CaretEffect::BreathGlow => 3,
                CaretEffect::Pulse => 4,
                CaretEffect::Full => 4,
            });
            assert!(next.glow() >= e.glow());
            assert!(next.pulse() >= e.pulse());
            assert!(next.glide() >= e.glide());
            assert!(next.breath() >= e.breath());
        }
        assert!(!CaretEffect::Off.breath());
        assert!(!CaretEffect::Breath.glow());
        assert!(!CaretEffect::BreathGlow.pulse());
        assert!(!CaretEffect::Pulse.glide());
    }

    #[test]
    fn blink_period_is_the_system_pair_with_a_floor() {
        // 530 ms — the default — is a 1.06 s cycle, which is exactly what the
        // hard blink and the breath both loop on.
        assert!((blink_period_s(530) - 1.06).abs() < 1e-9);
        // A strobing configuration is clamped rather than followed.
        assert!((blink_period_s(10) - 0.2).abs() < 1e-9);
        assert!((blink_period_s(0) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn breath_curve_starts_at_full_and_bottoms_out_at_55_percent() {
        // phase 0.25 → sin(π/2) = 1 at t = 0: a caret that just moved is solid.
        let at = |t: f64, period: f64| {
            BREATH_BIAS
                + BREATH_AMPLITUDE * (2.0 * std::f64::consts::PI * (t / period + 0.25)).sin() as f32
        };
        let period = blink_period_s(530);
        assert!((at(0.0, period) - 1.0).abs() < 1e-5);
        // Half a cycle later it is the dim end — 0.55, still readable.
        assert!((at(period * 0.5, period) - 0.55).abs() < 1e-5);
        // And it never leaves [0.55, 1.0], so the caret is never invisible.
        for i in 0..200 {
            let v = at(period * (i as f64) / 200.0, period);
            assert!(v >= 0.549 && v <= 1.001, "{v}");
        }
    }
}
