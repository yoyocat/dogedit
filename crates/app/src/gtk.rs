//! Linux GTK4 backend — milestone M7.
//! Structure parity with `win32`: own an
//! `Editor`, a Pango `TextMeasurer`, and a
//! `GtkDrawingArea` whose `draw` signal
//! runs the `PaintPipeline`. Platform wiring
//! is pending the M7 milestone.

pub fn run(_path: Option<&str>) -> i32 {
    eprintln!("gtk backend: milestone M7 (not implemented)");
    1
}
