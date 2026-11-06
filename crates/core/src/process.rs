//! Starting child processes without giving them a window of their own.
//!
//! The editor is a Windows GUI-subsystem image and has no console. A console
//! child started from it (`rg`, `ctags`, a language server) makes Windows
//! allocate a fresh console for that child, which shows up as a DOS window
//! flashing on every search or symbol scan. The flag below suppresses that;
//! piped stdio is untouched, so the child still speaks over its pipes.

/// Keep a child process from opening a console window of its own.
pub fn no_console_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}
