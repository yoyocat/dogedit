//! The widget protocol shared by every custom-drawn control.

use crate::event::{Event, Point};
use dg_render::theme::Theme;
use dg_render::{DrawList, Rect};

/// Side effects a widget may request from its host
/// after handling an event.  The host applies the
/// actions and invalidates if any widget repainted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Action {
    /// A value changed (e.g. scrollbar position);
    /// the host should re-read the widget model.
    pub value_changed: bool,
    /// The widget needs a repaint (e.g. hover).
    pub repaint: bool,
    /// Focus moved to this widget.
    pub take_focus: bool,
    /// The event was fully consumed; stop routing.
    pub consumed: bool,
}

impl Action {
    pub const NONE: Action = Action {
        value_changed: false,
        repaint: false,
        take_focus: false,
        consumed: false,
    };
    pub const REPAINT: Action = Action {
        repaint: true,
        ..Action::NONE
    };
    pub const VALUE: Action = Action {
        value_changed: true,
        repaint: true,
        ..Action::NONE
    };
    pub const CONSUMED: Action = Action {
        consumed: true,
        repaint: true,
        ..Action::NONE
    };
    pub fn merge(&mut self, other: Action) {
        self.value_changed |= other.value_changed;
        self.repaint |= other.repaint;
        self.take_focus |= other.take_focus;
        self.consumed |= other.consumed;
    }
}

/// Host hooks a widget can request through
/// [`WidgetCtx`].  The platform shell matches on
/// these and performs the actual work (popup menus
/// are custom-drawn but anchored via the native
/// window).
#[derive(Clone, Debug)]
pub enum HostCmd {
    /// Show a custom-drawn popup menu; `id` selects
    /// the menu definition, `pos` is in client
    /// coordinates.
    PopupMenu { id: u32, pos: Point },
    /// Open a file and move the caret (search
    /// results, directory view).
    OpenFile { path: String, line: usize, col: usize },
    /// Generic notification (status bar field
    /// changed, watcher fired,...).
    Notify(u32),
}

/// Context passed to [`Widget::on_event`]: timer
/// scheduling plus host hooks.  Implemented by the
/// platform shell; tests use a mock.
pub trait WidgetCtx {
    /// Schedule a repeating timer; it is delivered
    /// back as [`Event::Timer`] with the same id.
    fn set_timer(&mut self, id: u64, ms: u32);
    /// Stop a timer.
    fn kill_timer(&mut self, id: u64);
    /// Request a host action; returns the action the
    /// host wants applied.
    fn host_command(&mut self, cmd: HostCmd) -> Action;
}

/// A custom-drawn control.
///
/// Widgets own their layout rect, do their own hit
/// testing, handle platform-neutral events, and
/// append draw commands to the shared [`DrawList`] —
/// the backend replays that list onto its system
/// drawing API (GDI / CoreGraphics / cairo).
pub trait Widget {
    /// Assign the widget's area within the client
    /// rect.
    fn layout(&mut self, rect: Rect);
    /// Current area.
    fn rect(&self) -> Rect;
    /// Whether `pos` (client coords) falls inside the
    /// widget's interactive area.
    fn hit_test(&self, pos: Point) -> bool;
    /// Handle an event; returns the requested
    /// actions.
    fn on_event(&mut self, ev: Event, ctx: &mut dyn WidgetCtx) -> Action;
    /// Append draw commands for this widget.
    fn paint(&self, dl: &mut DrawList, theme: &Theme);
}
