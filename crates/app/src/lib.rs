pub mod app;
pub mod doctor;
pub mod download;
pub mod cmdids;
pub mod keys;
pub mod largehl;
pub mod markers;
pub mod safety;
pub mod settings;
pub mod skin;
pub mod snippets;
pub mod themes;

#[cfg(target_os = "windows")]
pub mod win32;

#[cfg(target_os = "macos")]
pub mod cocoa;

#[cfg(target_os = "linux")]
pub mod gtk;
