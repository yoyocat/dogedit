//! The backend capability contract.
//!
//! Every platform shell (`dg-app` win32 / cocoa / gtk)
//! implements [`Surface`].  Widgets and the app layer call
//! only through this trait, which is why the widget layer
//! carries no platform dependency.

use crate::event::Point;

/// A spawned child process (e.g. `rg` for find-in-files).
pub trait ChildProc {
    /// Read one line of stdout (UTF-8, LF-terminated).
    /// Returns `None` at EOF.
    fn read_line(&mut self) -> Option<String>;
    /// Read all remaining stdout into a string.
    fn read_to_end(&mut self) -> Option<String> {
        let mut out = String::new();
        while let Some(line) = self.read_line() {
            out.push_str(&line);
        }
        Some(out)
    }
    /// Terminate the process.
    fn kill(&mut self);
    /// Wait for exit; returns the exit code.
    fn wait(&mut self) -> Option<i32>;
}

/// Platform capabilities the UI layer needs.
pub trait Surface {
    /// Client area size in DIP.
    fn size(&self) -> (u32, u32);
    /// DPI scale (1.0 at 96 DPI).
    fn dpi(&self) -> f32;
    /// Invalidate the whole client area (repaint soon).
    fn invalidate(&self);
    /// Set a repeating timer; `id` is reported back in
    /// [`crate::event::Event::Timer`].
    fn set_timer(&self, id: u64, ms: u32);
    /// Stop a timer.
    fn kill_timer(&self, id: u64);
    /// Clipboard access (plain text).
    fn clipboard_text(&self) -> Option<String>;
    fn set_clipboard_text(&self, text: &str);
    /// Spawn a child process (used by the ripgrep backend
    /// and external tools).
    fn spawn(&self, program: &str, args: &[&str]) -> Option<Box<dyn ChildProc>>;
    /// Native file dialogs — deliberately kept native for
    /// cross-platform consistency and stability.
    fn open_file_dialog(&self, filters: &[(&str, &str)]) -> Option<String>;
    fn save_file_dialog(&self, filters: &[(&str, &str)]) -> Option<String>;
    /// Open a path with the default system program
    /// (shell-open semantics).
    fn open_with_default_program(&self, path: &str) -> bool;
    /// Show a modal yes/no question; `true` = yes.
    fn confirm(&self, title: &str, message: &str) -> bool;
    /// Screen coordinate of a client point (for popup
    /// positioning of custom-drawn menus).
    fn client_to_screen(&self, pos: Point) -> Point;
}
