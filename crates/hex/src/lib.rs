//! The platform-neutral half of the hex editor view.
//!
//! `dg-hex` owns the snapshot model and the view state (scroll offsets,
//! byte/ASCII columns, colour table); the Win32 shell in `dg-app` owns the
//! window, the caret and the clipboard.

pub mod snapshot;
pub mod view;

pub use snapshot::Snapshot;
pub use view::{Colors, HexView};
