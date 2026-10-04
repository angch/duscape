//! What the desktop viewers (Windows, macOS, Linux) share, with no toolkit in it: [`state::Viewer`]
//! is what a window shows and how it answers input, [`scan`] runs the first scan with its live
//! outline, [`preview`] reads the file in hand, [`menu`] is what the right-click menu offers,
//! [`passes`] which paint of a layout is in a hurry and which in full, and [`icon`] is the app's
//! icon, drawn by the treemap.
//! Each viewer turns its toolkit's events into calls here and draws what is here, so the
//! behaviour is the same from one to the next and is tested once, on every platform.

/// The app's ID, in reverse-DNS form: the Linux window's Wayland app ID and X11 class, and the
/// name of its `.desktop` file, its AppStream metadata and its icons (`packaging/linux/`). A
/// Wayland compositor takes a window's icon and name only from the `.desktop` file of its app
/// ID, so the two must match (`tests/packaging.rs`); Flathub wants this form.
pub const APP_ID: &str = "io.github.angch.duscape";

pub mod chooser;
pub mod icon;
pub mod menu;
pub mod passes;
pub mod preview;
pub mod scan;
pub mod state;
