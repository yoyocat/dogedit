//! The document map: the strip at the right edge of the text area that shows
//! the document as one pixel per column and two per visual row.
//!
//! The strip is not scaled to fit the document. It shows a window of it, and
//! the window follows the view so that the viewport's own box stays over the
//! text it belongs to — the box's distance from the top of the strip is the
//! same share of the strip that the first visible row is of the document, with
//! both ends pinned, so a view at the first row keeps the box at the top and a
//! view at the last one keeps it at the bottom.

use crate::component::Rect;
use crate::theme::Color;

/// One run of columns on one visual row. `cols` is a column count, drawn one
/// pixel per column.
#[derive(Clone, Copy, Debug)]
pub struct Strip {
    /// Visual row, counted from the document's first row.
    pub row: u32,
    /// Columns the run covers.
    pub cols: u32,
    pub color: Color,
}

/// The height of one visual row in the strip, in pixels.
pub const ROW_PX: f32 = 2.0;

/// The strip's width for a text area `avail` pixels wide, whose character
/// cell is `char_w`: four fifths of the columns that fit, clamped to 50..160.
/// Four fifths is what makes a line of ordinary length reach most of the way
/// across the strip instead of stopping short of it.
pub fn width(avail: f32, char_w: f32) -> f32 {
    let cols = (avail / char_w.max(1.0)) as i32;
    ((4 * cols / 5) as f32).clamp(50.0, 160.0)
}

/// A run's colour on the strip: five eighths of the syntax colour over three
/// eighths of what is under it. Full strength would read as text at a size
/// nothing can read, and the other way round disappears on a dark theme —
/// the map has to stay legible when it is a hundred pixels wide.
pub fn dim(color: Color, background: Color) -> Color {
    let mix = |fg: u8, bg: u8| ((fg as u32 * 160 + bg as u32 * 95 + 128) >> 8) as u8;
    Color::rgb(
        mix(color.r, background.r),
        mix(color.g, background.g),
        mix(color.b, background.b),
    )
}

/// The viewport's box, filled: the background with three tenths of the
/// foreground stirred in. It reads as a lighter band over the strip while
/// staying well under the runs' own brightness, so the text drawn on top of
/// it is still the brightest thing in the map.
pub fn box_fill(foreground: Color, background: Color) -> Color {
    let mix = |bg: u8, fg: u8| ((bg as u32 * 180 + fg as u32 * 75 + 128) >> 8) as u8;
    Color::rgb(
        mix(background.r, foreground.r),
        mix(background.g, foreground.g),
        mix(background.b, foreground.b),
    )
}

/// The box's one pixel edge: the same pair [`dim`] uses, which lands a shade
/// under the fill and separates the box from the rows running into it.
pub fn box_edge(foreground: Color, background: Color) -> Color {
    let mix = |bg: u8, fg: u8| ((bg as u32 * 160 + fg as u32 * 95 + 128) >> 8) as u8;
    Color::rgb(
        mix(background.r, foreground.r),
        mix(background.g, foreground.g),
        mix(background.b, foreground.b),
    )
}

/// How far the runs start inside the strip's left edge: the gap keeps a
/// column of one character reading as a mark rather than as the strip's
/// border.
pub const LEFT_INSET: f32 = 4.0;

/// The whole-document map model. It holds runs for the rows the strip
/// currently shows — a window of a few hundred rows — rather than for the
/// document: the strip is two pixels a row, so everything outside the window
/// would be built and then never drawn.
pub struct MiniMapModel {
    /// Runs, ascending by row, covering `window`.
    pub strips: Vec<Strip>,
    /// The rows `strips` covers, `None` before the first rebuild.
    pub window: Option<(usize, usize)>,
    /// Visual rows in the document, set by [`Self::layout`].
    pub rows: usize,
    /// First row the strip shows.
    pub top: usize,
    /// The viewport's box in pixels relative to the strip's top-left. Its
    /// width is the strip's, so only the vertical half lives here.
    pub viewport: Rect,
}

impl Default for MiniMapModel {
    fn default() -> Self {
        Self {
            strips: Vec::new(),
            window: None,
            rows: 0,
            top: 0,
            viewport: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }
}

impl MiniMapModel {
    /// Rebuilds the runs from per-visual-row `{color, len}` lists, starting at
    /// visual row `first_row`.
    pub fn rebuild(&mut self, first_row: usize, row_runs: Vec<Vec<(u32, u32)>>) {
        self.strips.clear();
        let rows = row_runs.len();
        for (i, runs) in row_runs.into_iter().enumerate() {
            for (color, len) in runs {
                if len == 0 {
                    continue;
                }
                self.strips.push(Strip {
                    row: (first_row + i).min(u32::MAX as usize) as u32,
                    cols: len,
                    color: Color::from_u32(color),
                });
            }
        }
        self.window = Some((first_row, first_row + rows.saturating_sub(1)));
    }

    /// Whether the rows `first..=last` are already built, so a paint that
    /// shows the same window again — a scroll of a few rows, a keystroke —
    /// costs no highlighting at all.
    pub fn covers(&self, first: usize, last: usize) -> bool {
        match self.window {
            Some((lo, hi)) => first >= lo && last <= hi,
            None => false,
        }
    }

    /// The scale a strip of `map_h` pixels scrolls at: how far the box sits
    /// below the strip's top, and which row the strip starts at, for a view
    /// whose first row is `first_row` and which shows `visible_rows` rows of
    /// a document with `total_rows` visual rows.
    ///
    /// The box's offset is `half * first / (total + total * visible / half)`
    /// with `half` the strip's half height — the row share of the strip,
    /// corrected by the share of a screen the strip shows, which is what pins
    /// both ends: at the first row the offset is zero, and the last row lands
    /// with the box's bottom on the strip's bottom.
    pub fn layout(
        &mut self,
        total_rows: usize,
        first_row: usize,
        visible_rows: usize,
        map_h: f32,
    ) {
        self.rows = total_rows;
        let half = (map_h / 2.0).max(1.0);
        let denom = self.spread(visible_rows, half);
        let offset = (half * first_row as f32 / denom).round() as usize;
        let offset = offset.min(first_row);
        self.top = first_row - offset;
        let height = (visible_rows.max(1) as f32 * ROW_PX).max(4.0);
        self.viewport = Rect::new(0.0, offset as f32 * ROW_PX, 0.0, height);
    }

    /// The inverse of [`MiniMapModel::layout`]: the first row a box top `y`
    /// pixels below the strip's top stands for. This is what a press and a
    /// drag on the strip scroll to.
    pub fn row_at(&self, y: f32, visible_rows: usize, map_h: f32) -> usize {
        let half = (map_h / 2.0).max(1.0);
        let denom = self.spread(visible_rows, half);
        let row = (y.max(0.0) * denom / map_h.max(1.0)).round() as usize;
        row.min(self.rows.saturating_sub(1))
    }

    /// The denominator both directions share: the document's row count, plus
    /// the rows a whole strip covers relative to half of it. Sharing it is
    /// what keeps a drag from drifting away from the box it drags.
    fn spread(&self, visible_rows: usize, half: f32) -> f32 {
        let total = self.rows.max(1) as f32;
        total + total * visible_rows.max(1) as f32 / half
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(rows: usize) -> MiniMapModel {
        let mut m = MiniMapModel::default();
        m.layout(rows, 0, 40, 800.0);
        m.rebuild(0, (0..rows).map(|_| vec![(0xFFFFFF, 10)]).collect());
        m
    }

    #[test]
    fn the_strip_width_is_four_fifths_of_the_columns_clamped() {
        // 100 columns of 8 px: 80 px of strip
        assert_eq!(width(800.0, 8.0), 80.0);
        // a narrow area still gets the 50 px floor
        assert_eq!(width(200.0, 8.0), 50.0);
        // a very wide one stops at 160
        assert_eq!(width(4000.0, 8.0), 160.0);
        // a zero cell cannot divide by zero
        assert_eq!(width(800.0, 0.0), 160.0);
    }

    #[test]
    fn the_box_pins_both_ends_and_stays_inside_the_strip() {
        let mut m = model(2000);
        let map_h = 800.0;
        let visible = 40;
        m.layout(2000, 0, visible, map_h);
        assert_eq!(m.viewport.y, 0.0);
        assert_eq!(m.top, 0);
        // the last row's box bottom lands on the strip's bottom, give or take
        // the rounding of the row it starts on
        let last = 2000 - visible;
        m.layout(2000, last, visible, map_h);
        let box_bottom = m.viewport.y + m.viewport.h;
        assert!(
            (box_bottom - map_h).abs() <= ROW_PX * 5.0,
            "box bottom {box_bottom} vs strip {map_h}"
        );
        assert_eq!(m.top + (m.viewport.y / ROW_PX) as usize, last);
        // half way down the document is half way down the strip
        m.layout(2000, 1000, visible, map_h);
        assert!(
            (m.viewport.y - map_h / 2.0).abs() < map_h / 20.0,
            "middle offset {}",
            m.viewport.y
        );
    }

    #[test]
    fn a_document_shorter_than_the_strip_keeps_the_box_at_the_top() {
        let mut m = model(20);
        m.layout(20, 0, 40, 800.0);
        assert_eq!(m.viewport.y, 0.0);
        assert_eq!(m.top, 0);
        // the strip only holds the rows the document has
        assert_eq!(m.rows, 20);
    }

    #[test]
    fn a_press_scrolls_to_the_row_it_names_and_the_drag_reads_it_back() {
        // Long enough that the box's whole travel maps inside the document.
        // Past the last row the box's bottom pins to the strip's bottom
        // instead, and there a press no longer lands under its own row.
        let mut m = model(8000);
        let map_h = 800.0;
        let visible = 40;
        let last_y = map_h - visible as f32 * ROW_PX;
        for y in [0.0, 100.0, 400.0, last_y] {
            let row = m.row_at(y, visible, map_h);
            m.layout(8000, row, visible, map_h);
            // laying the strip out for that row puts the box back under the
            // pointer, which is what makes a drag follow the hand
            assert!(
                (m.viewport.y - y).abs() <= ROW_PX * 1.5,
                "y {y} -> row {row} -> box {}",
                m.viewport.y
            );
        }
        // the very bottom of the strip names the document's last row
        assert_eq!(m.row_at(map_h, visible, map_h), 7999);
    }

    #[test]
    fn the_run_colours_are_dimmed_towards_the_background() {
        let bg = Color::rgb(30, 31, 34);
        assert_eq!(dim(bg, bg), bg);
        // a white run on a near-black background keeps five eighths of itself
        let c = dim(Color::rgb(255, 255, 255), bg);
        assert!((c.r as i32 - (255 * 160 + 30 * 95) / 255).abs() <= 2, "{c:?}");
        // the box is lighter than the background but darker than the runs
        let fill = box_fill(Color::rgb(210, 212, 215), bg);
        assert!(fill.r > bg.r && fill.r < c.r, "fill {fill:?} run {c:?}");
        // the edge is a shade above the fill, which is what separates the box
        // from the runs that stop just inside it
        assert!(box_edge(Color::rgb(210, 212, 215), bg).r > fill.r);
    }

    #[test]
    fn rebuilding_keeps_the_runs_in_row_order_and_skips_empty_rows() {
        let mut m = MiniMapModel::default();
        m.rebuild(
            7,
            vec![
                vec![(1, 5)],
                vec![],
                vec![(2, 3), (3, 4)],
            ],
        );
        // the window is the rows that were built, and only those
        assert_eq!(m.window, Some((7, 9)));
        assert_eq!(m.strips.len(), 3);
        assert_eq!(m.strips[0].row, 7);
        assert_eq!(m.strips[1].row, 9);
        assert_eq!(m.strips[2].row, 9);
        assert_eq!(m.strips[2].cols, 4);
    }

    #[test]
    fn a_window_covers_the_paints_it_was_built_for_and_no_others() {
        let mut m = MiniMapModel::default();
        assert!(!m.covers(0, 0), "nothing built yet");
        m.rebuild(100, (0..50).map(|_| vec![(1, 2)]).collect());
        assert_eq!(m.window, Some((100, 149)));
        // a paint inside the window costs nothing
        assert!(m.covers(120, 140));
        assert!(m.covers(100, 149));
        // one row above or below it needs a rebuild
        assert!(!m.covers(99, 140));
        assert!(!m.covers(120, 150));
        // an empty rebuild still marks its row as built
        m.rebuild(5, Vec::new());
        assert_eq!(m.window, Some((5, 5)));
        assert!(m.covers(5, 5));
    }
}
