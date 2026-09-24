//! What the desktop viewers (macOS, Linux) share, with no toolkit in it: [`state::Viewer`] is
//! what a window shows and how it answers input, [`scan`] runs the first scan with its live
//! outline, and [`preview`] reads the file in hand. Each viewer turns its toolkit's events into
//! calls here and draws what is here, so the behaviour is the same from one to the next and is
//! tested once, on every platform.

pub mod preview;
pub mod scan;
pub mod state;
