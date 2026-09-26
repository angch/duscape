//! A Windows viewer for duscape: the terminal viewer's features in a native window.
//!
//! The scan comes from `duscape-scan` and the tree, the treemap, deletion, previews and the
//! clipboard from `libduscape` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `duscape_viewer::state::Viewer`, shared with the macOS and Linux
//! viewers and tested on every platform; [`preview`] prepares a picture for GDI; `win` is the
//! window, drawing with GDI and turning input into `Viewer` calls. It is built on `windows-sys`
//! rather than a GUI framework, to keep the executable small. `docs/features.md` compares the
//! viewers.

// Compiled and tested on every platform; only the window is Windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod cli;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod elevate;
#[cfg_attr(not(windows), allow(dead_code))]
mod preview;
#[cfg(windows)]
mod win;

/// The window, from the command line (`cli::Opt`). Returns when it is closed. A mistake in the
/// arguments is told in a message box: a window's process may have no console to print to.
#[cfg(not(windows))]
pub fn run() {
    eprintln!("duscape-windows is a Windows-only GUI. Use `duscape` on this platform.");
    std::process::exit(1);
}

/// The window, from the command line (`cli::Opt`). Returns when it is closed. A mistake in the
/// arguments is told in a message box: a window's process may have no console to print to.
#[cfg(windows)]
pub fn run() {
    win::run();
}

/// The window on `folder` (else one picked in a dialog), scanning with `options`: the entry
/// point for `duscape`, which has read its own command line. `no_elevate` never asks to run
/// as administrator.
#[cfg(windows)]
pub fn run_with(
    folder: Option<::std::path::PathBuf>,
    options: libduscape::ScanOptions,
    no_elevate: bool,
) {
    win::run_with(folder, options, no_elevate);
}
