//! Platform-neutral UI models.
//!
//! What is left in this crate is state a platform cannot decide for itself:
//! how the client area is partitioned between the docks ([`panel`]), which
//! panes the status strip carries and in what order ([`statusbar`]), and where
//! each overlay scrollbar's thumb sits ([`scrollbar`]).
//!
//! The widgets that used to live here are gone. Every panel that had pixels —
//! the directory tree, the search results, the status strip, the popup menu,
//! the scrollbars — is now a window in the platform backend, which hit-tests,
//! selects, scrolls and focuses itself. Re-implementing that in a crate shared
//! by every platform was the thing this layer kept getting bigger for, and a
//! common control already does it.
//!
//! Rendering stays the same protocol: whatever a backend does paint is emitted
//! as [`dg_render::DrawList`] commands and replayed onto its own system drawing
//! API (Win32 GDI, macOS CoreGraphics, Linux cairo). No third-party drawing
//! library is used anywhere; the only dependencies are `dg-core` (data models)
//! and `dg-render` (the draw command protocol).

pub mod event;
pub mod menu;
pub mod panel;
pub mod scrollbar;
pub mod statusbar;
pub mod surface;
pub mod widget;

pub use event::{Event, MouseButton, Point};
pub use menu::MenuItem;
pub use surface::{ChildProc, Surface};
pub use widget::{Action, WidgetCtx};
