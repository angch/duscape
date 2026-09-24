//! A Windows viewer for diskonaut-angch: the terminal viewer's features in a native window.
//!
//! The scan comes from `diskonaut-scan` and the tree, the treemap, deletion, previews and the
//! clipboard from `libdiskonaut` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `diskonaut_viewer::state::Viewer`, shared with the macOS and Linux
//! viewers and tested on every platform; [`preview`] prepares a picture for GDI; `win` is the
//! window, drawing with GDI and turning input into `Viewer` calls. It is built on `windows-sys`
//! rather than a GUI framework, to keep the executable small. `docs/features.md` compares the
//! viewers.

#![cfg_attr(windows, windows_subsystem = "windows")]

// Compiled and tested on every platform; only the window is Windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
#[cfg_attr(not(windows), allow(dead_code))]
mod preview;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
fn main() {
    eprintln!("diskonaut-windows is a Windows-only GUI. Use `diskonaut` on this platform.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    win::run();
}
