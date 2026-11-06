//! The mouse cursor: a move decides a shape and stores it; `WM_SETCURSOR` is
//! the only thing that puts it on screen.
//!
//! Storing and applying are split because `SetCursor` inside `WM_MOUSEMOVE`
//! is undone by the `WM_SETCURSOR` Windows posts for the same move. The
//! shapes are I-beam, arrow, hand and cross for the text area, plus the
//! line-number strip's own cursor rebuilt from its bits.

#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::PtInRect;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCursor, HCURSOR, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_IBEAM, LoadCursorW,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// The plain arrow.
    Arrow,
    /// The I-beam over text; also the shape stored before the first move.
    Ibeam,
    /// The cross, applied mid-drag rather than stored.
    Cross,
    /// The hand, over a fold marker.
    Hand,
    /// The line-number strip's cursor.
    LineSelect,
}

impl Shape {
    fn index(self) -> usize {
        match self {
            Self::Arrow => 0,
            Self::Ibeam => 1,
            Self::Cross => 2,
            Self::Hand => 3,
            Self::LineSelect => 4,
        }
    }
}

/// One of the six strips the layout lays out across the gutter, in the order
/// the cursor state machine walks them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strip {
    /// The line-number strip.
    LineNumber,
    /// The fold-mark strip.
    Folding,
    /// The edit-state strip.
    EditState,
    /// The left-margin strip.
    LeftMargin,
    /// The diagnostic-icon strip.
    Icon,
    /// The bookmark strip.
    BookMark,
}

impl Strip {
    /// The cursor each strip asks for: the line numbers take the row-select
    /// arrow, the fold marks take the hand, and the other four keep the
    /// plain arrow.
    pub fn cursor(self) -> Shape {
        match self {
            Self::LineNumber => Shape::LineSelect,
            Self::Folding => Shape::Hand,
            Self::EditState | Self::LeftMargin | Self::Icon | Self::BookMark => Shape::Arrow,
        }
    }
}

/// What the move handler reads before it decides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Facts {
    /// `PtInRect(the view, pt)`.
    pub in_text: bool,
    /// the view && the view: a **column** selection drag is extending under the
    /// button. With the move that leaves the drag box, when column mode, Alt
    /// (`GetKeyState(18)`) or the middle button (`wParam & 0x10`) is on — and a
    /// plain left-drag selects text without it.
    pub drag_selecting: bool,
    /// the selection hit test: the point is on text that is already selected.
    pub over_selection: bool,
    /// the view: the press that may still become a drag owns the pointer.
    pub drag_pending: bool,
    /// The first strip whose rect holds the point, if any.
    pub strip: Option<Strip>,
}

/// The three exits the cursor state machine has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Put the shape on the screen and return without storing it: the only
    /// puts on the screen from a move.
    ApplyNow(Shape),
    /// Remember the shape for the next paint, without setting it now.
    Store(Shape),
    /// Nothing at all: the view keeps what the last move left there.
    Keep,
}

/// the cursor state machine's decision, in its order.
pub fn choose(f: &Facts) -> Decision {
    if f.in_text {
        if f.drag_selecting {
            return Decision::ApplyNow(Shape::Cross);
        }
        if f.over_selection {
            return Decision::Store(Shape::Arrow);
        }
        return Decision::Store(Shape::Ibeam);
    }
    if f.drag_pending {
        return Decision::Keep;
    }
    match f.strip {
        Some(strip) => Decision::Store(strip.cursor()),
        None => Decision::Keep,
    }
}

/// The read-only substitution: a document that will not take edits shows the
/// arrow whatever the move decided.
pub fn applied_shape(stored: Shape, read_only: bool) -> Shape {
    if read_only { Shape::Arrow } else { stored }
}

/// the cursor state machine's walk: the first strip whose rect holds the point. A strip
/// whose option is off was laid out zero-width, so `PtInRect` fails on it and
/// the walk moves on — that is how the strips the user toggled away drop out.
pub fn strip_at(strips: &[(Strip, RECT)], x: i32, y: i32) -> Option<Strip> {
    let pt = POINT { x, y };
    strips
        .iter()
        .find(|(_, r)| unsafe { PtInRect(r, pt).as_bool() })
        .map(|(strip, _)| *strip)
}

/// the view plus the shape globals: the stored decision, and the handles
/// loaded for it.
pub struct Cursors {
    stored: Shape,
    handles: [Option<HCURSOR>; 5],
}

impl Cursors {
    /// The constructor stores the I-beam as the initial shape — an editor shows
    /// an I-beam before its first mouse move.
    pub fn new() -> Self {
        Self {
            stored: Shape::Ibeam,
            handles: [None; 5],
        }
    }

    pub fn stored(&self) -> Shape {
        self.stored
    }

    pub fn store(&mut self, shape: Shape) {
        self.stored = shape;
    }

    pub fn handle(&mut self, shape: Shape) -> Option<HCURSOR> {
        let i = shape.index();
        if self.handles[i].is_none() {
            self.handles[i] = load(shape);
        }
        self.handles[i]
    }
}

/// The four standard ones are shared system cursors.
fn load(shape: Shape) -> Option<HCURSOR> {
    let standard = match shape {
        Shape::Arrow => IDC_ARROW,
        Shape::Ibeam => IDC_IBEAM,
        Shape::Cross => IDC_CROSS,
        Shape::Hand => IDC_HAND,
        Shape::LineSelect => return line_select_cursor(),
    };
    unsafe { LoadCursorW(None, standard).ok() }
}

/// A monochrome 32×32 arrow pointing left-up at the line, hotspot `(21, 5)` — the
/// point that makes a press in the strip read as that row. The cursor is rebuilt
/// from the two mask planes read out of the loaded handle (`GetIconInfo`'s 32×64 1
/// bpp mask bitmap: the AND plane, then the XOR plane).
fn line_select_cursor() -> Option<HCURSOR> {
    unsafe {
        CreateCursor(
            None,
            LINE_SELECT_HOTSPOT.0,
            LINE_SELECT_HOTSPOT.1,
            32,
            32,
            LINE_SELECT_AND.as_ptr().cast(),
            LINE_SELECT_XOR.as_ptr().cast(),
        )
        .ok()
    }
}

/// `(x, y)` of the arrow glyph's hotspot.
const LINE_SELECT_HOTSPOT: (i32, i32) = (21, 5);

/// The AND plane: 32 rows of 4 bytes, 1 bpp, bit `0x80` the leftmost pixel.
/// All but the arrow's outline is `0xff`, which is what makes the rest of the
/// bitmap transparent.
pub static LINE_SELECT_AND: [u8; 128] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xf3, 0xff, 0xff, 0xff, 0xe3, 0xff, 0xff, 0xff, 0xc3, 0xff,
    0xff, 0xff, 0x83, 0xff, 0xff, 0xff, 0x03, 0xff, 0xff, 0xfe, 0x03, 0xff, 0xff, 0xfc, 0x03, 0xff,
    0xff, 0xf8, 0x03, 0xff, 0xff, 0xf0, 0x03, 0xff, 0xff, 0xe0, 0x03, 0xff, 0xff, 0xc0, 0x03, 0xff,
    0xff, 0xfc, 0x03, 0xff, 0xff, 0xfc, 0x03, 0xff, 0xff, 0xf8, 0x63, 0xff, 0xff, 0xf8, 0x73, 0xff,
    0xff, 0xf0, 0xfb, 0xff, 0xff, 0xf0, 0xff, 0xff, 0xff, 0xe1, 0xff, 0xff, 0xff, 0xe1, 0xff, 0xff,
    0xff, 0xf3, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];

/// The XOR plane: the colour bits, black wherever it reads `0x00`.
pub static LINE_SELECT_XOR: [u8; 128] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x18, 0x00,
    0x00, 0x00, 0x38, 0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0xf8, 0x00, 0x00, 0x01, 0xf8, 0x00,
    0x00, 0x03, 0xf8, 0x00, 0x00, 0x07, 0xf8, 0x00, 0x00, 0x0f, 0xf8, 0x00, 0x00, 0x01, 0xf8, 0x00,
    0x00, 0x01, 0xb8, 0x00, 0x00, 0x01, 0x98, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00,
    0x00, 0x06, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, right: i32) -> RECT {
        RECT {
            left,
            top: 0,
            right,
            bottom: 100,
        }
    }

    fn facts(in_text: bool, strip: Option<Strip>) -> Facts {
        Facts {
            in_text,
            drag_selecting: false,
            over_selection: false,
            drag_pending: false,
            strip,
        }
    }

    #[test]
    fn over_the_text_it_is_an_ibeam_and_over_the_selection_an_arrow() {
        assert_eq!(choose(&facts(true, None)), Decision::Store(Shape::Ibeam));
        let mut f = facts(true, None);
        f.over_selection = true;
        assert_eq!(choose(&f), Decision::Store(Shape::Arrow));
        // The selection wins over the strip list: inside the text the walk
        // never happens.
        f.strip = Some(Strip::LineNumber);
        assert_eq!(choose(&f), Decision::Store(Shape::Arrow));
    }

    #[test]
    fn a_drag_selecting_shows_the_cross_and_stores_nothing() {
        let mut f = facts(true, None);
        f.drag_selecting = true;
        f.over_selection = true;
        assert_eq!(choose(&f), Decision::ApplyNow(Shape::Cross));
    }

    /// the cursor state machine's drag branch arms the view only for a
    /// rectangle drag — the cross never belongs to a plain left-drag,
    /// which selects text with the I-beam showing (the drag end is the
    /// caret, and the selection hit test's interval is `[start, end)`).
    #[test]
    fn a_plain_drag_select_keeps_the_ibeam() {
        let mut f = facts(true, None);
        f.drag_selecting = false;
        f.over_selection = false;
        assert_eq!(choose(&f), Decision::Store(Shape::Ibeam));
        // Mid-selection the same drag shows the arrow: the point is
        // inside the range but is not its caret end.
        f.over_selection = true;
        assert_eq!(choose(&f), Decision::Store(Shape::Arrow));
    }

    #[test]
    fn each_strip_answers_with_its_own_shape() {
        for (strip, shape) in [
            (Strip::LineNumber, Shape::LineSelect),
            (Strip::Folding, Shape::Hand),
            (Strip::EditState, Shape::Arrow),
            (Strip::LeftMargin, Shape::Arrow),
            (Strip::Icon, Shape::Arrow),
            (Strip::BookMark, Shape::Arrow),
        ] {
            assert_eq!(choose(&facts(false, Some(strip))), Decision::Store(shape));
            // Inside the text, no strip is consulted at all.
            assert_eq!(
                choose(&facts(true, Some(strip))),
                Decision::Store(Shape::Ibeam)
            );
        }
    }

    #[test]
    fn outside_the_text_and_off_the_strips_nothing_is_touched() {
        assert_eq!(choose(&facts(false, None)), Decision::Keep);
        // A press still able to become a drag owns the pointer anywhere: the
        // stored shape must not change as it passes over the gutter.
        let mut f = facts(false, Some(Strip::Folding));
        f.drag_pending = true;
        assert_eq!(choose(&f), Decision::Keep);
        //... and the same is true while it drags inside the text, where the
        // cross is applied directly instead.
        f.in_text = true;
        f.drag_selecting = true;
        assert_eq!(choose(&f), Decision::ApplyNow(Shape::Cross));
    }

    #[test]
    fn the_walk_takes_the_first_strip_holding_the_point_in_memory_order() {
        let strips = [
            (Strip::LineNumber, rect(7, 40)),
            (Strip::Folding, rect(40, 52)),
            (Strip::EditState, rect(52, 54)),
            (Strip::LeftMargin, rect(54, 56)),
            (Strip::Icon, rect(0, 0)),
            (Strip::BookMark, rect(0, 7)),
        ];
        assert_eq!(strip_at(&strips, 20, 50), Some(Strip::LineNumber));
        assert_eq!(strip_at(&strips, 45, 50), Some(Strip::Folding));
        assert_eq!(strip_at(&strips, 55, 50), Some(Strip::LeftMargin));
        // A hidden strip is a zero-width rect, and `PtInRect` requires area —
        // that is how the strips the user toggled off drop out of the walk.
        assert_eq!(strip_at(&strips, 0, 50), Some(Strip::BookMark));
        // `PtInRect` is `left <= p < right`: the pixel on the line between two
        // strips belongs to the one on its right, which is what makes the
        // layout's `x += width` tile the gutter with no double claim.
        assert_eq!(strip_at(&strips, 40, 50), Some(Strip::Folding));
        assert_eq!(strip_at(&strips, 54, 50), Some(Strip::LeftMargin));
        assert_eq!(strip_at(&strips, 7, 50), Some(Strip::LineNumber));
        assert_eq!(strip_at(&strips, 6, 50), Some(Strip::BookMark));
        assert_eq!(strip_at(&strips, 56, 50), None);
        assert_eq!(strip_at(&strips, 57, 50), None);
        // The band is the window's, not the text's: the layout runs every strip
        // down to `Rect.bottom`, so the gutter keeps its own cursor below the
        // document's last row.
        assert_eq!(strip_at(&strips, 20, 99), Some(Strip::LineNumber));
        assert_eq!(strip_at(&strips, 20, 101), None);
    }

    #[test]
    fn a_read_only_document_forces_the_arrow_at_the_cursor_message() {
        assert_eq!(applied_shape(Shape::Ibeam, true), Shape::Arrow);
        assert_eq!(applied_shape(Shape::LineSelect, true), Shape::Arrow);
        assert_eq!(applied_shape(Shape::Ibeam, false), Shape::Ibeam);
        assert_eq!(applied_shape(Shape::Cross, false), Shape::Cross);
    }
}
