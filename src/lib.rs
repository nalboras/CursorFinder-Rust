//! Pure-logic modules are platform independent and unit tested on any OS.
//! Everything that touches Win32 lives in `windows_app` (Windows only).

pub mod arrow;
pub mod detect;
pub mod geometry;
pub mod ico;
pub mod settings;
pub mod visibility;

#[cfg(windows)]
pub mod windows_app;
