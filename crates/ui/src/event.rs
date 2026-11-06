//! Platform-neutral input events.
//!
//! Backends translate native messages (WM_*, NSEvent,
//! GTK events) into this enum; widgets only ever see
//! [`Event`], so the widget layer stays cross-platform.

/// 2D point in client coordinates (DIP).
///
/// The single DIP-point type lives in `dg_render`; re-exported here so event
/// consumers keep their `event::Point` path.
pub use dg_render::Point;

/// Mouse buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Keyboard modifier bit flags.
pub mod mods {
    pub const NONE: u8 = 0;
    pub const SHIFT: u8 = 1;
    pub const CTRL: u8 = 2;
    pub const ALT: u8 = 4;
    pub const WIN: u8 = 8;
}

/// Common virtual-key codes. The Win32
/// backend passes these through; the
/// macOS and GTK backends map their
/// native key codes onto the same
/// values so widgets see one space.
pub mod vk {
    pub const BACK: u32 = 0x08;
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const DELETE: u32 = 0x2E;
    pub const F2: u32 = 0x71;
}

/// Input event, platform-neutral.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Mouse moved; `pos` in client coords.
    MouseMove { pos: Point },
    /// Button pressed.
    MouseDown { pos: Point, button: MouseButton, mods: u8 },
    /// Button released.
    MouseUp { pos: Point, button: MouseButton, mods: u8 },
    /// Double click.
    DoubleClick { pos: Point, button: MouseButton },
    /// Pointer left the widget's area. The overlay scrollbar needs this to stop
    /// holding itself up: while the pointer is on the bar it must not fade.
    MouseLeave,
    /// Mouse wheel. `delta` in wheel-delta units (120 =
    /// one notch on Windows); `horiz` selects the axis.
    Wheel { delta: i32, horiz: bool, pos: Point, mods: u8 },
    /// Virtual key down; `code` is the platform key code
    /// (Win32 VK / macOS keyCode / GDK keyval).
    KeyDown { code: u32, mods: u8 },
    /// Text character (after IME composition completes).
    Char { ch: char, mods: u8 },
    /// Repeating timer fired.
    Timer { id: u64 },
    FocusIn,
    FocusOut,
}
