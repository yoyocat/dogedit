//! Answers the colour questions Windows asks a control about itself.
//!
//! A dark control does not paint itself — it is asked. Two messages carry the
//! question, and both had grown a copy per dialog:
//!
//! - `NM_CUSTOMDRAW` for a list or tree row: the control says which stage it is
//! at, the parent sets `clrText`/`clrTextBk` and answers with a `CDRF_*`.
//! - `WM_CTLCOLOR*` for an edit, a static or a combo: the parent sets the text
//! and background colours on the DC and hands back the brush to fill behind.
//!
//! Neither is about *drawing* — that is [`super::ownerdraw`]'s job — so this is
//! a separate module rather than more of that one. What is shared here is the
//! **ladder**: every row handler in the tree walked the same four stages, and
//! every ctlcolor arm wrote the same two `Set*Color` calls before returning its
//! brush. What stays with the dialog is which colours, because that is the only
//! part any two of them disagreed about.

#![allow(unsafe_op_in_unsafe_fn)]

use dg_render::theme::Color;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{HBRUSH, HDC, SetBkColor, SetTextColor};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDIS_HOT, CDIS_SELECTED, CDRF_NEWFONT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, LVIS_SELECTED, NMLVCUSTOMDRAW, NMTVCUSTOMDRAW,
};

use super::window::to_colorref;

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// The bits of a `*CUSTOMDRAW` this module needs, for the two shapes that have
/// them. A tree and a list carry the same information behind different structs
/// and different state flags — `CDIS_*` here, `LVIS_*` there — so each decodes
/// its own and everything else is shared.
pub(super) trait Row {
    fn stage(&self) -> u32;
    /// The row's index, as `dwItemSpec`. 自定义标记 uses it to look up which
    /// marker slot painted this row.
    fn item(&self) -> usize;
    fn selected(&self) -> bool;
    fn hot(&self) -> bool;
    fn set_colours(&mut self, fg: COLORREF, bg: COLORREF);
}

impl Row for NMLVCUSTOMDRAW {
    fn stage(&self) -> u32 {
        self.nmcd.dwDrawStage.0
    }

    fn item(&self) -> usize {
        self.nmcd.dwItemSpec as usize
    }

    fn selected(&self) -> bool {
        self.nmcd.uItemState.0 & LVIS_SELECTED.0 != 0
    }

    /// A list view does report `LVIS_HOT` for a hot-tracked row, but neither of
    /// the two dialogs that answer here (自定义标记, 快捷键管理) turns hot
    /// tracking on, so there is nothing to report. A dialog that wants a hovered
    /// row would have to track it itself, the way the dock tree does.
    fn hot(&self) -> bool {
        false
    }

    fn set_colours(&mut self, fg: COLORREF, bg: COLORREF) {
        self.clrText = fg;
        self.clrTextBk = bg;
    }
}

impl Row for NMTVCUSTOMDRAW {
    fn stage(&self) -> u32 {
        self.nmcd.dwDrawStage.0
    }

    fn item(&self) -> usize {
        self.nmcd.dwItemSpec as usize
    }

    fn selected(&self) -> bool {
        self.nmcd.uItemState.contains(CDIS_SELECTED)
    }

    fn hot(&self) -> bool {
        self.nmcd.uItemState.contains(CDIS_HOT)
    }

    fn set_colours(&mut self, fg: COLORREF, bg: COLORREF) {
        self.clrText = fg;
        self.clrTextBk = bg;
    }
}

/// The whole `NM_CUSTOMDRAW` answer for one row, given a function that picks the
/// pair of colours.
///
/// `subitems` is for a list whose rows are painted per column: the item stage
/// then only asks to be called again per subitem, which is the extra step
/// 自定义标记 needs and the other three do not. `pick` is called once, with the
/// row index and the two states the control reported.
///
/// The return values are load-bearing: `CDRF_NOTIFYITEMDRAW` at `PREPAINT` is
/// what makes the item stage arrive at all, and `CDRF_NEWFONT` is what makes the
/// colours take effect. Getting either wrong leaves the rows black on white
/// rather than failing to compile.
pub(super) fn row<T: Row>(
    cd: &mut T,
    subitems: bool,
    pick: impl FnOnce(usize, bool, bool) -> (COLORREF, COLORREF),
) -> isize {
    let stage = cd.stage();
    if stage == CDDS_PREPAINT.0 {
        return CDRF_NOTIFYITEMDRAW as isize;
    }
    if stage == CDDS_ITEMPREPAINT.0 && subitems {
        return CDRF_NOTIFYSUBITEMDRAW as isize;
    }
    if stage == CDDS_ITEMPREPAINT.0 || stage == (CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) {
        let (fg, bg) = pick(cd.item(), cd.selected(), cd.hot());
        cd.set_colours(fg, bg);
        return CDRF_NEWFONT as isize;
    }
    0
}

// ---------------------------------------------------------------------------
// Control colours
// ---------------------------------------------------------------------------

/// Lets a caller pass either an `dg_render` colour or one it already converted,
/// so the 27 call sites do not each repeat (or each omit) `to_colorref`.
pub(super) trait AsColorref {
    fn as_colorref(self) -> COLORREF;
}

impl AsColorref for Color {
    fn as_colorref(self) -> COLORREF {
        to_colorref(self)
    }
}

impl AsColorref for COLORREF {
    fn as_colorref(self) -> COLORREF {
        self
    }
}

/// The answer to `WM_CTLCOLOR*`: set the two colours on the DC and hand back the
/// brush the control fills its background with.
///
/// `SetBkColor` is the one that gets forgotten, and forgetting it is not subtle —
/// the control keeps the system's `COLOR_WINDOW` behind its text, which on a dark
/// dialog is a white block around every label.
pub(super) fn ctlcolor(
    hdc: HDC,
    ink: impl AsColorref,
    face: impl AsColorref,
    brush: HBRUSH,
) -> isize {
    unsafe {
        SetTextColor(hdc, ink.as_colorref());
        SetBkColor(hdc, face.as_colorref());
    }
    brush.0 as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list view at `stage`, with no state flags.
    fn list_at(stage: u32) -> NMLVCUSTOMDRAW {
        let mut cd = NMLVCUSTOMDRAW::default();
        cd.nmcd.dwDrawStage = windows::Win32::UI::Controls::NMCUSTOMDRAW_DRAW_STAGE(stage);
        cd
    }

    /// The ladder asks for the item stage, then answers it with a colour pair.
    #[test]
    fn the_row_ladder_walks_prepaint_then_item() {
        let mut cd = list_at(CDDS_PREPAINT.0);
        assert_eq!(
            row(&mut cd, false, |_, _, _| panic!("pick must not run at PREPAINT")),
            CDRF_NOTIFYITEMDRAW as isize
        );

        cd = list_at(CDDS_ITEMPREPAINT.0);
        cd.nmcd.dwItemSpec = 7;
        let mut seen = None;
        let r = row(&mut cd, false, |item, sel, hot| {
            seen = Some((item, sel, hot));
            (COLORREF(0x11), COLORREF(0x22))
        });
        assert_eq!(r, CDRF_NEWFONT as isize);
        assert_eq!(seen, Some((7, false, false)), "the row index reaches pick");
        assert_eq!((cd.clrText.0, cd.clrTextBk.0), (0x11, 0x22));

        // Any other stage is not ours to answer.
        assert_eq!(row(&mut list_at(0), false, |_, _, _| unreachable!()), 0);
    }

    /// `subitems` inserts one more step: the item stage only asks to be called
    /// per column, and the colour pair is chosen at the combined stage.
    #[test]
    fn subitems_adds_a_stage_before_the_colours() {
        let mut cd = list_at(CDDS_ITEMPREPAINT.0);
        assert_eq!(
            row(&mut cd, true, |_, _, _| panic!("not yet")),
            CDRF_NOTIFYSUBITEMDRAW as isize
        );

        let mut cd = list_at(CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0);
        let r = row(&mut cd, true, |_, _, _| (COLORREF(0x33), COLORREF(0x44)));
        assert_eq!(r, CDRF_NEWFONT as isize);
        assert_eq!((cd.clrText.0, cd.clrTextBk.0), (0x33, 0x44));
    }

    /// A list decodes `LVIS_SELECTED` and never claims hot; a tree decodes both
    /// of its own flags.
    #[test]
    fn each_shape_decodes_its_own_state_flags() {
        let mut list = list_at(CDDS_ITEMPREPAINT.0);
        list.nmcd.uItemState = windows::Win32::UI::Controls::NMCUSTOMDRAW_DRAW_STATE_FLAGS(
            LVIS_SELECTED.0,
        );
        assert!(list.selected());
        assert!(!list.hot(), "a list row is never hot here");

        let mut tree = NMTVCUSTOMDRAW::default();
        tree.nmcd.uItemState = CDIS_SELECTED | CDIS_HOT;
        assert!(tree.selected());
        assert!(tree.hot());

        tree.nmcd.uItemState = CDIS_HOT;
        assert!(!tree.selected());
        assert!(tree.hot());
    }

    /// The brush comes back untouched — that is what the control fills with.
    #[test]
    fn ctlcolor_returns_the_brush_it_was_given() {
        // A null DC makes the two Set*Color calls fail harmlessly; the point of
        // the test is the return value, which is the brush either way.
        let brush = HBRUSH(0x1234 as *mut core::ffi::c_void);
        assert_eq!(ctlcolor(HDC::default(), COLORREF(1), COLORREF(2), brush), 0x1234);
    }
}
