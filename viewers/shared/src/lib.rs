//! What the desktop viewers (Windows, macOS, Linux) share, with no toolkit in it: [`state::Viewer`]
//! is what a window shows and how it answers input, [`scan`] runs the first scan with its live
//! outline, [`preview`] reads the file in hand, and [`menu`] is what the right-click menu offers.
//! Each viewer turns its toolkit's events into calls here and draws what is here, so the
//! behaviour is the same from one to the next and is tested once, on every platform.

pub mod menu;
pub mod preview;
pub mod scan;
pub mod state;
