//! Overlay scrollbar: the thin, arrow-less, theme-coloured bar that floats on
//! top of the text instead of taking a column away from it.
//!
//! Two widths, one bar. A bar that is merely *there* is a thin strip at the
//! window edge — enough to say "there is more document this way" without
//! drawing a control over the text. Pointing at it (or scrolling) wakes it to
//! its full width, which is also when it claims the whole strip: a thin bar
//! claims only the pixels it paints, so the text under the rest of the strip
//! stays clickable.
//!
//! This is a *model*, deliberately not a [`crate::widget::Widget`]: its pixels go
//! to the bar's own layered child HWND (`dg-app/src/win32/overlay.rs`), because
//! "see through the bar" has to be real DWM compositing over the sibling that
//! painted the text underneath — a DrawList cannot blend with what another
//! window drew. `LWA_ALPHA` gives one window-wide alpha, which is exactly what a
//! uniform bar needs; per-pixel alpha (rounded corners) would mean
//! `UpdateLayeredWindow` and is not taken here.
//!
//! Coordinates are **window-local**: the bar's HWND is the strip, so the native
//! messages arrive in the space this model works in and the frame never has to
//! hand-route a click into the bar (that routing is what this design removes).
//!
//! Model semantics match the rest of this implementation: `range` is the maximum scrollable
//! position (`content_len - page`, clamped to 0) and `page` is the visible
//! amount. The thumb is sized `page / (range + page)` of the track.

use crate::event::{Event, MouseButton, Point};
use crate::widget::{Action, WidgetCtx};
use dg_render::Rect;

/// Timer ids for the fade-out, one per bar.
pub const FADE_V: u64 = 0x1000;
pub const FADE_H: u64 = 0x1001;
/// Minimum thumb length in DIP so it stays grabbable.
const MIN_THUMB: f32 = 20.0;
/// 滑块的最短长度还有一条占比下限：百万行的文件按占比只有零点几像素，
/// 光靠固定那 20 会退化成轨道上的一个点。占比只是「别小到看不见」的兜底，
/// 不是可见行数，所以只要 2%：再高就等于在长文件上撒谎，还把行程吃掉一截。
const MIN_THUMB_SHARE: f32 = 0.02;
/// Wheel scroll amount (lines per notch).
const WHEEL_LINES: i32 = 3;

/// Hit zones inside the strip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    None,
    Thumb,
    TrackStart,
    TrackEnd,
}

/// One 修改行 mark on the bar: the document rows `[start, end)` it stands for,
/// and which edit state it shows — the same `1` modified / `2` written back the
/// gutter's own strip paints with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub start: i32,
    pub end: i32,
    pub state: u8,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Idle,
    /// Dragging the thumb; `grab` is the distance
    /// from the thumb leading edge to the pointer.
    DragThumb { grab: f32 },
}

/// Theme-driven geometry (`Theme::scrollbar_*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Thickness of the thumb pill — the only pixels the bar paints.
    pub width: f32,
    /// Thickness of the strip window the pill floats in, centred. The
    /// geometry never changes with hover, drag or fade state: pointing at
    /// the bar recolours the pill, it does not reshape it.
    pub strip: f32,
    /// How long the bar stays engaged (whole-strip hit testing) after the
    /// last scroll.
    pub fade_ms: u32,
}

/// One scrollbar, vertical or horizontal.
pub struct Scrollbar {
    pub vertical: bool,
    /// Current scroll position, `[0, range]`.
    pos: i32,
    /// Max scrollable position (`content - page`).
    range: i32,
    /// Visible amount (page size).
    page: i32,
    /// The strip, in the bar window's own client coords.
    rect: Rect,
    state: State,
    hover: Zone,
    metrics: Metrics,
    /// Engaged: the pointer is on the bar, or the view moved recently. While
    /// engaged the strip claims its whole window for hit testing (track
    /// clicks page-jump); resting, it claims only the pill it paints.
    /// A bar that is merely present is never hidden — a scrollbar nobody can
    /// see is a scrollbar nobody can find.
    awake: bool,
    /// 修改行标记: where the document's marked lines fall on this track. Only
    /// the vertical bar is ever handed any.
    marks: Vec<Mark>,
    /// Bumped whenever [`Self::set_marks`] actually swaps the list, so the
    /// host's repaint key can compare one integer instead of the marks.
    marks_rev: u64,
}

impl Scrollbar {
    pub fn new(vertical: bool) -> Self {
        Self {
            vertical,
            pos: 0,
            range: 0,
            page: 0,
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            state: State::Idle,
            hover: Zone::None,
            metrics: Metrics {
                width: 6.0,
                strip: 12.0,
                fade_ms: 700,
            },
            awake: false,
            marks: Vec::new(),
            marks_rev: 0,
        }
    }

    /// Update the model (host calls after every edit /
    /// relayout / scroll).  `range` = max position.
    /// Returns `true` when the *host* moved the view, which is
    /// the signal to pop the bar back up: a bar that only
    /// reflects its own drag must not re-arm the countdown
    /// from the sync that follows it.
    pub fn set_model(&mut self, pos: i32, range: i32, page: i32) -> bool {
        self.range = range.max(0);
        self.page = page.max(0);
        let clamped = pos.clamp(0, self.range);
        let moved = clamped != self.pos;
        self.pos = clamped;
        moved
    }

    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics = metrics;
    }

    /// Hands over the 修改行 marks for this track. Returns `true` when the list
    /// changed; the host reads that back through [`Self::marks_rev`] as part of
    /// what a paint depends on, so an unchanged document costs nothing.
    pub fn set_marks(&mut self, marks: Vec<Mark>) -> bool {
        if marks == self.marks {
            return false;
        }
        self.marks = marks;
        self.marks_rev += 1;
        true
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// The revision the host keys its repaint cache on. A bar with no marks has
    /// a revision of its own that only ever moves when marks arrive.
    pub fn marks_rev(&self) -> u64 {
        self.marks_rev
    }

    /// Where one mark lands along the track, in this window's own coordinates:
    /// the same rows-to-pixels map the thumb uses, so a mark sits at the rows
    /// it names. A run shorter than a pixel is grown to two, which is what
    /// makes a single changed line — or a changed line hidden under a fold —
    /// still findable.
    pub fn mark_span(&self, m: &Mark) -> (f32, f32) {
        let track = self.track_len();
        // 最后一屏的行号越过 `range`：夹回来，标记就停在滑块够得到的最底处。
        let y = |rows: i32| {
            self.rows_to_track(rows.max(0).min(self.range) as f32)
                .clamp(0.0, track)
        };
        let y0 = y(m.start);
        let y1 = y(m.end).max(y0);
        let h = (y1 - y0).max(2.0).min((track - y0).max(2.0));
        (y0, h)
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Whether there is anything to scroll.
    pub fn visible(&self) -> bool {
        self.range > 0
    }

    /// Whether the bar claims its whole strip for hit testing — the pointer
    /// is over it, a drag is live, or the view just moved under it
    /// (engagement). This is *hit* state only: the painted pill is the same
    /// shape either way.
    ///
    /// An engaged bar claims the whole strip, which is what makes a click
    /// above or below the thumb page-jump. A resting bar claims only the
    /// pill it paints, so the strip's transparent margins still belong to
    /// the text under them.
    pub fn engaged(&self) -> bool {
        self.visible() && (self.awake || self.dragging() || self.hover != Zone::None)
    }

    /// `true` when the width changed, i.e. the host has to repaint the bar
    /// window.
    pub fn reveal(&mut self, ctx: &mut dyn WidgetCtx) -> bool {
        if !self.visible() {
            return false;
        }
        // Every scroll re-arms the same countdown; `SetTimer` with an existing
        // id replaces it, so no kill is needed first.
        ctx.set_timer(self.fade_timer(), self.metrics.fade_ms);
        if self.awake {
            false
        } else {
            self.awake = true;
            true
        }
    }

    pub fn pos(&self) -> i32 {
        self.pos
    }

    pub fn range(&self) -> i32 {
        self.range
    }

    /// Rows visible at once — the thumb's length is a function of this, so a
    /// host that caches its paint has to be able to read it.
    pub fn page(&self) -> i32 {
        self.page
    }

    /// Current hover zone (host uses it for the cursor).
    pub fn zone(&self) -> Zone {
        self.hover
    }

    pub fn dragging(&self) -> bool {
        matches!(self.state, State::DragThumb { .. })
    }

    /// The strip the bar window occupies.
    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
        if self.rect_size() < MIN_THUMB {
            self.hover = Zone::None;
            self.state = State::Idle;
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    fn rect_size(&self) -> f32 {
        if self.vertical { self.rect.h } else { self.rect.w }
    }

    /// The overlay strip is the whole window: no arrow buttons take ends off it.
    fn track_len(&self) -> f32 {
        self.rect_size().max(0.0)
    }

    /// Content length in scroll units.
    fn content(&self) -> i32 {
        self.range + self.page.max(1)
    }

    /// Thumb length in DIP.
    fn thumb_len(&self) -> f32 {
        if self.range <= 0 {
            return 0.0;
        }
        let track = self.track_len();
        let t = track * self.page as f32 / self.content() as f32;
        t.min(track).max(self.min_thumb())
    }

    /// 滑块最短长度：固定下限与轨道占比里大的那个，但不超过轨道本身。
    fn min_thumb(&self) -> f32 {
        (self.track_len() * MIN_THUMB_SHARE)
            .max(MIN_THUMB)
            .min(self.track_len())
    }

    /// 滑块沿轨道能走的距离 —— 轨道减去滑块自己。
    fn travel(&self) -> f32 {
        (self.track_len() - self.thumb_len()).max(0.0)
    }

    /// 行 → 轨道像素，按**行程**换算。滑块被下限撑大时行程相应变短，
    /// 行程的末端才是文档的末端；按整条轨道换算的话，多出来的那截像素
    /// 谁也不代表，拖到底也停在最后几万行之前（百万行的文件差出三成）。
    fn rows_to_track(&self, rows: f32) -> f32 {
        if self.range <= 0 {
            return 0.0;
        }
        self.travel() * rows / self.range as f32
    }

    /// Thumb leading edge along the track, in DIP.
    fn thumb_along(&self) -> f32 {
        self.rows_to_track(self.pos as f32)
    }

    /// The strip window's thickness — fixed; the dock reserves the same
    /// number and the paint never reshapes it.
    pub fn strip_width(&self) -> f32 {
        self.metrics.strip.max(self.metrics.width)
    }

    /// The thumb pill's thickness — fixed, centred in [`Self::strip_width`],
    /// and the only pixels the bar paints.
    pub fn pill_width(&self) -> f32 {
        self.metrics.width.min(self.strip_width())
    }

    /// Where to fill the thumb, in this window's client coords: a pill of
    /// [`Self::pill_width`] centred in the strip, its length the usual
    /// page/content share of the track. Everything else in the strip stays
    /// unpainted — those pixels are fully transparent and the text under
    /// them shows through untouched rather than tinted.
    pub fn thumb_rect(&self) -> Rect {
        let w = self.pill_width();
        let len = self.thumb_len();
        let along = self.thumb_along();
        if self.vertical {
            Rect::new((self.rect.w - w) / 2.0, along, w, len)
        } else {
            Rect::new(along, (self.rect.h - w) / 2.0, len, w)
        }
    }

    /// Whether the bar claims the pointer at `pos` (window-local). This is the
    /// question the host's `WM_NCHITTEST` asks: `true` means the bar is under
    /// the pointer, `false` means those pixels are transparent and belong to
    /// the text beneath.
    ///
    /// An engaged bar claims the whole strip, so the track above and below
    /// the thumb page-jumps like a real scrollbar's does. A resting bar
    /// claims only the pill it paints — the transparent margin is text, and
    /// stealing it would make the last column of every long line unclickable.
    pub fn interactive_at(&self, pos: Point) -> bool {
        if !self.visible() || !self.rect_contains(pos) {
            return false;
        }
        if self.engaged() {
            return true;
        }
        let t = self.thumb_rect();
        pos.x >= t.x && pos.x < t.x + t.w && pos.y >= t.y && pos.y < t.y + t.h
    }

    fn fade_timer(&self) -> u64 {
        if self.vertical { FADE_V } else { FADE_H }
    }

    /// Map a pointer position (window-local) to a zone.
    fn zone_at(&self, pos: Point) -> Zone {
        if !self.visible() || !self.rect_contains(pos) {
            return Zone::None;
        }
        let along = if self.vertical {
            pos.y - self.rect.y
        } else {
            pos.x - self.rect.x
        };
        let thumb = self.thumb_along();
        if along < thumb {
            Zone::TrackStart
        } else if along > thumb + self.thumb_len() {
            Zone::TrackEnd
        } else {
            Zone::Thumb
        }
    }

    fn rect_contains(&self, pos: Point) -> bool {
        pos.x >= self.rect.x
            && pos.x < self.rect.x + self.rect.w
            && pos.y >= self.rect.y
            && pos.y < self.rect.y + self.rect.h
    }

    /// Apply a position change; returns true when it
    /// actually moved.
    fn scroll_to(&mut self, new_pos: i32) -> bool {
        let new_pos = new_pos.clamp(0, self.range);
        if new_pos != self.pos {
            self.pos = new_pos;
            true
        } else {
            false
        }
    }

    /// Drag the thumb so `track` (pointer position along the track) is the grab
    /// point.
    fn drag_to(&mut self, track: f32, grab: f32) -> bool {
        let travel = self.travel();
        if travel <= 0.0 {
            return false;
        }
        let along = (track - grab).clamp(0.0, travel);
        let new_pos = (along * self.range as f32 / travel).round() as i32;
        self.scroll_to(new_pos)
    }

    /// Pointer position along the track, in window-local coords.
    fn pointer_track(&self, pos: Point) -> f32 {
        if self.vertical {
            pos.y - self.rect.y
        } else {
            pos.x - self.rect.x
        }
    }

    /// Handle a native message translated to [`Event`]; returns the requested
    /// actions. The host then writes `pos()` back into the document and
    /// invalidates this window.
    pub fn on_event(&mut self, ev: Event, ctx: &mut dyn WidgetCtx) -> Action {
        let mut action = Action::NONE;
        match ev {
            Event::MouseMove { pos } => match self.state {
                State::DragThumb { grab } => {
                    if self.drag_to(self.pointer_track(pos), grab) {
                        action.merge(Action::VALUE);
                    }
                }
                _ => {
                    let zone = self.zone_at(pos);
                    if zone != self.hover {
                        self.hover = zone;
                        // Hovering recolours the pill (the highlighted
                        // state), so the bar window has to redraw even when
                        // the document did not move.
                        action.merge(Action::REPAINT);
                    }
                }
            },
            Event::MouseDown { pos, button, .. } => {
                if button != MouseButton::Left || !self.visible() {
                    return action;
                }
                match self.zone_at(pos) {
                    Zone::Thumb => {
                        let grab = (self.pointer_track(pos) - self.thumb_along()).max(0.0);
                        self.state = State::DragThumb { grab };
                        action.merge(Action::CONSUMED);
                    }
                    Zone::TrackStart | Zone::TrackEnd => {
                        // Jump to the click point and follow the drag.
                        let grab = self.thumb_len() / 2.0;
                        if self.drag_to(self.pointer_track(pos), grab) {
                            action.merge(Action::VALUE);
                        }
                        self.state = State::DragThumb { grab };
                        action.merge(Action::CONSUMED);
                    }
                    Zone::None => {
                        // A press outside the strip belongs to the text under
                        // it; the bar must not claim it.
                    }
                }
            }
            Event::MouseUp { .. } => {
                if !matches!(self.state, State::Idle) {
                    self.state = State::Idle;
                    // Release starts the fade countdown; until then the thumb
                    // stays where the drag left it.
                    ctx.set_timer(self.fade_timer(), self.metrics.fade_ms);
                    action.merge(Action::REPAINT);
                }
            }
            Event::MouseLeave => {
                if self.hover != Zone::None {
                    self.hover = Zone::None;
                    action.merge(Action::REPAINT);
                }
            }
            Event::Wheel { delta, horiz, .. } => {
                let on_axis = if self.vertical { !horiz } else { horiz };
                if !self.visible() || !on_axis {
                    return action;
                }
                let dir = -WHEEL_LINES * delta.signum();
                if self.scroll_to(self.pos + dir) {
                    action.merge(Action::VALUE);
                }
                if self.reveal(ctx) {
                    action.merge(Action::REPAINT);
                }
            }
            Event::Timer { id } if id == self.fade_timer() => {
                // Held up by the pointer or a live drag: disengaging now would
                // take the whole-strip hit testing out from under the hand
                // that is using it. Otherwise it goes back to resting — the
                // pill itself never changes shape, only the claim does.
                if matches!(self.state, State::Idle)
                    && self.hover == Zone::None
                    && self.awake
                {
                    self.awake = false;
                    action.merge(Action::REPAINT);
                }
            }
            _ => {}
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::HostCmd;

    /// Records the timers the model asks for, so the fade countdown is testable
    /// without a message loop.
    struct RecCtx {
        timers: Vec<(u64, u32)>,
    }
    impl RecCtx {
        fn new() -> Self {
            Self { timers: Vec::new() }
        }
        fn armed(&self, id: u64) -> Option<u32> {
            self.timers.iter().rev().find(|(t, _)| *t == id).map(|(_, ms)| *ms)
        }
    }
    impl WidgetCtx for RecCtx {
        fn set_timer(&mut self, id: u64, ms: u32) {
            self.timers.push((id, ms));
        }
        fn kill_timer(&mut self, id: u64) {
            self.timers.retain(|(t, _)| *t != id);
        }
        fn host_command(&mut self, _cmd: HostCmd) -> Action {
            Action::NONE
        }
    }

    /// Vertical bar, strip 12 x 400 in its own window, pill 6.
    fn bar() -> Scrollbar {
        let mut s = Scrollbar::new(true);
        s.set_metrics(Metrics {
            width: 6.0,
            strip: 12.0,
            fade_ms: 700,
        });
        s.layout(Rect::new(0.0, 0.0, 12.0, 400.0));
        s.set_model(0, 960, 40);
        s
    }

    #[test]
    fn model_clamps_and_page_math() {
        let s = bar();
        assert!(s.visible());
        // content = 1000 rows, page = 40 -> range = 960. The overlay track is
        // the whole window: no arrow buttons at the ends.
        assert_eq!(s.track_len(), 400.0);
        // natural thumb = 40/1000 of 400 = 16, so the floor decides: the fixed
        // 20 beats 2% of the track (8) here.
        assert!((s.thumb_len() - 20.0).abs() < 0.01);
        assert!((s.min_thumb() - 20.0).abs() < 0.01);
        // 行程 = 轨道 - 滑块，两个端点都落在真实行号上。
        assert!((s.travel() - 380.0).abs() < 0.01);
    }

    #[test]
    fn a_scrollable_bar_is_always_on_screen_as_a_fixed_pill() {
        let s = bar();
        assert!(s.visible());
        assert!(!s.engaged(), "nothing has woken it yet");
        let t = s.thumb_rect();
        assert_eq!(t.w, 6.0, "the pill is the thumb thickness");
        assert_eq!(t.x, 3.0, "centred in the 12 px strip");
        // Nothing to scroll => no bar at all, however often the host asks.
        let mut flat = Scrollbar::new(true);
        flat.layout(Rect::new(0.0, 0.0, 12.0, 400.0));
        flat.set_model(0, 0, 40);
        let mut ctx = RecCtx::new();
        assert!(!flat.visible());
        assert!(!flat.engaged());
        assert!(!flat.reveal(&mut ctx));
        assert!(!flat.visible());
    }

    #[test]
    fn a_resting_bar_claims_only_the_pill_it_paints() {
        let s = bar();
        let t = s.thumb_rect();
        // the painted pill: the bar's
        assert!(s.interactive_at(Point::new(t.x + t.w / 2.0, t.y + t.h / 2.0)));
        // the transparent margin left of it: the text's, so the last column of
        // a long line stays clickable
        assert!(!s.interactive_at(Point::new(t.x - 2.0, t.y + t.h / 2.0)));
        // above the thumb, still inside the strip: also the text's until the
        // bar is engaged
        assert!(!s.interactive_at(Point::new(6.0, 300.0)));
        // outside the window entirely
        assert!(!s.interactive_at(Point::new(40.0, 300.0)));
    }

    #[test]
    fn engagement_takes_the_whole_strip_but_never_reshapes_the_pill() {
        let mut s = bar();
        let resting = s.thumb_rect();
        let mut ctx = RecCtx::new();
        assert!(s.reveal(&mut ctx), "first reveal engages the strip");
        assert!(s.engaged());
        assert_eq!(ctx.armed(FADE_V), Some(700));
        // a track click is the bar's now: that is what page-jumps
        assert!(s.interactive_at(Point::new(6.0, 300.0)));
        // but the pixels are the same pill either way — the shape is fixed
        assert_eq!(s.thumb_rect(), resting);
        // the tick disengages: the claim narrows, the pill does not move
        let a = s.on_event(Event::Timer { id: FADE_V }, &mut ctx);
        assert!(a.repaint);
        assert!(!s.engaged());
        assert!(s.visible(), "resting, never hidden");
        assert_eq!(s.thumb_rect(), resting);
    }

    #[test]
    fn fade_holds_while_pointed_or_dragged() {
        let mut s = bar();
        let mut ctx = RecCtx::new();
        s.reveal(&mut ctx);
        // pointer inside the strip: the countdown must not disengage it
        s.on_event(Event::MouseMove { pos: Point::new(6.0, 200.0) }, &mut ctx);
        s.on_event(Event::Timer { id: FADE_V }, &mut ctx);
        assert!(s.engaged());
        s.on_event(Event::MouseLeave, &mut ctx);
        s.on_event(Event::Timer { id: FADE_V }, &mut ctx);
        assert!(!s.engaged(), "resting again once nothing holds it up");
        assert!(s.visible(), "but never gone");
        // and the other axis' timer must not touch it
        s.on_event(Event::Timer { id: FADE_H }, &mut ctx);
        assert!(!s.engaged());
    }

    #[test]
    fn drag_thumb_follows_without_requiring_focus() {
        let mut s = bar();
        let mut ctx = RecCtx::new();
        s.reveal(&mut ctx);
        let thumb = s.thumb_along();
        s.on_event(
            Event::MouseDown {
                pos: Point::new(6.0, thumb + 5.0),
                button: MouseButton::Left,
                mods: 0,
            },
            &mut ctx,
        );
        assert!(s.dragging());
        let before = s.pos();
        s.on_event(Event::MouseMove { pos: Point::new(6.0, 200.0) }, &mut ctx);
        assert!(s.pos() > before, "drag moves the view");
        s.on_event(
            Event::MouseUp {
                pos: Point::new(6.0, 200.0),
                button: MouseButton::Left,
                mods: 0,
            },
            &mut ctx,
        );
        assert!(!s.dragging());
        assert_eq!(ctx.armed(FADE_V), Some(700), "release re-arms the fade");
    }

    #[test]
    fn track_click_jumps_to_the_pointer() {
        let mut s = bar();
        let mut ctx = RecCtx::new();
        s.reveal(&mut ctx);
        s.on_event(
            Event::MouseDown {
                pos: Point::new(6.0, 300.0),
                button: MouseButton::Left,
                mods: 0,
            },
            &mut ctx,
        );
        assert!(s.pos() > 0);
    }

    #[test]
    fn hovering_recolours_but_never_reshapes() {
        let mut s = bar();
        let mut ctx = RecCtx::new();
        // untouched: the fixed pill, centred in the strip
        let resting = s.thumb_rect();
        assert_eq!(resting.w, 6.0);
        assert_eq!(resting.x, 3.0);
        s.on_event(Event::MouseMove { pos: Point::new(6.0, 200.0) }, &mut ctx);
        let hovered = s.thumb_rect();
        assert_eq!(hovered, resting, "the geometry is state-independent");
        assert_eq!(hovered.y, resting.y, "and does not move along");
    }

    #[test]
    fn wheel_on_the_bar_scrolls_three_lines_and_reveals() {
        let mut s = bar();
        s.set_model(10, 960, 40);
        let mut ctx = RecCtx::new();
        let a = s.on_event(
            Event::Wheel {
                delta: -120,
                horiz: false,
                pos: Point::new(9.0, 200.0),
                mods: 0,
            },
            &mut ctx,
        );
        assert!(a.value_changed);
        assert_eq!(s.pos(), 13);
        assert!(s.engaged(), "a scroll engages whole-strip hit testing");
        // the other axis is the editor's business, not the bar's
        let b = s.on_event(
            Event::Wheel {
                delta: -120,
                horiz: true,
                pos: Point::new(9.0, 200.0),
                mods: 0,
            },
            &mut ctx,
        );
        assert!(!b.value_changed);
        assert_eq!(s.pos(), 13);
    }

    /// 修改行标记: a mark lands where the thumb's own row→pixel map puts its
    /// rows, and a run thinner than a pixel keeps the two-pixel floor so a
    /// single changed line still reads on the bar.
    #[test]
    fn marks_land_on_the_rows_their_lines_occupy() {
        let s = bar();
        // 标记和滑块共用同一张行→像素的表：滑块停在第 500 行的地方，
        // 就是第 500 行的标记所在的地方。
        for row in [0i32, 100, 500, 960] {
            let mut t = bar();
            t.set_model(row, 960, 40);
            let (y, _) = s.mark_span(&Mark {
                start: row,
                end: row + 1,
                state: 1,
            });
            assert_eq!(y, t.thumb_along(), "第 {row} 行");
        }
        // 不足一像素的一行仍然保住 2 px 的下限，单行改动才看得见。
        let (_, h) = s.mark_span(&Mark {
            start: 500,
            end: 501,
            state: 2,
        });
        assert_eq!(h, 2.0);
        // 最后一行下面没有空间：下限照样给，多出的由绘制裁掉。
        let (y, h) = s.mark_span(&Mark {
            start: 999,
            end: 1000,
            state: 1,
        });
        assert!(y < s.track_len(), "{y} 还在轨道里");
        assert_eq!(h, 2.0);
    }

    /// 百万行：滑块被最短下限撑大，行程就比轨道短。拖到轨道末端必须正好
    /// 落在文档末端 —— 按整条轨道换算的话，最后几万行永远拖不到。
    #[test]
    fn dragging_to_the_bottom_of_the_track_reaches_the_last_row() {
        let mut s = Scrollbar::new(true);
        s.set_metrics(Metrics {
            width: 6.0,
            strip: 12.0,
            fade_ms: 700,
        });
        s.layout(Rect::new(0.0, 0.0, 12.0, 700.0));
        s.set_model(0, 1_000_000, 40);
        let mut ctx = RecCtx::new();
        s.reveal(&mut ctx);
        assert!(
            s.thumb_len() >= 20.0,
            "百万行的滑块也要抓得住: {}",
            s.thumb_len()
        );
        s.on_event(
            Event::MouseDown {
                pos: Point::new(6.0, 699.0),
                button: MouseButton::Left,
                mods: 0,
            },
            &mut ctx,
        );
        assert_eq!(s.pos(), s.range(), "轨道末端就是文档末端");
        // 停在那里的滑块，下沿正好贴在轨道末尾。
        assert!((s.thumb_along() + s.thumb_len() - 700.0).abs() < 0.01);
    }

    /// Handing the same list back must not wake the strip — `set_marks` runs on
    /// every relayout, which is every keystroke, while the changed lines stay
    /// exactly where they were.
    #[test]
    fn set_marks_bumps_the_revision_only_when_the_list_moves() {
        let mut s = bar();
        assert!(!s.set_marks(Vec::new()), "nothing to nothing");
        assert!(s.set_marks(vec![Mark {
            start: 4,
            end: 6,
            state: 1
        }]));
        let rev = s.marks_rev();
        assert!(!s.set_marks(vec![Mark {
            start: 4,
            end: 6,
            state: 1
        }]));
        assert_eq!(s.marks_rev(), rev, "an unchanged list leaves it alone");
        assert!(s.set_marks(vec![Mark {
            start: 40,
            end: 60,
            state: 1
        }]));
        assert_eq!(s.marks().len(), 1);
    }

    #[test]
    fn horizontal_bar_is_a_centred_fixed_pill() {
        let mut s = Scrollbar::new(false);
        s.set_metrics(Metrics {
            width: 6.0,
            strip: 12.0,
            fade_ms: 500,
        });
        s.layout(Rect::new(0.0, 0.0, 400.0, 12.0));
        s.set_model(0, 600, 40);
        let r = s.thumb_rect();
        assert_eq!(r.h, 6.0, "the pill is the thumb thickness");
        assert_eq!(r.y, 3.0, "centred in the strip");
        let mut ctx = RecCtx::new();
        assert!(s.reveal(&mut ctx));
        let r = s.thumb_rect();
        assert_eq!(r.h, 6.0, "engagement does not reshape the pill");
        assert_eq!(r.y, 3.0);
        assert_eq!(ctx.armed(FADE_H), Some(500));
    }
}
