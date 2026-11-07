//! macOS AppKit backend — milestone M6.
//! Structure parity with `win32`: own an
//! `Editor`, a CoreText `TextMeasurer`, and
//! an `NSView` whose `drawRect:` runs the
//! `PaintPipeline`. Platform wiring is
//! pending the M6 milestone.

pub fn run(_launch: &crate::launch::Launch) -> i32 {
    eprintln!("cocoa backend: milestone M6 (not implemented)");
    1
}
