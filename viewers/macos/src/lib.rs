//! A native macOS window on duscape.
//!
//! The scan comes from `duscape-scan`, and the tree, the squarified layout, deletion and file
//! previews from `libduscape` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `duscape_viewer::state`, shared with the Linux viewer and free of
//! AppKit; `mac` is AppKit: the window, its menus, drawing, dialogs, the Trash, the pasteboard,
//! Finder and Quick Look.

#[cfg(target_os = "macos")]
mod mac;

/// The window, from the command line: `[-a] [FOLDER]`. Returns when the app quits. AppKit runs
/// on the main thread, so this is called from it.
#[cfg(not(target_os = "macos"))]
pub fn run() {
    eprintln!("duscape-mac is a macOS-only GUI. Use `duscape` on this platform.");
    std::process::exit(1);
}

/// The window, from the command line: `[-a] [FOLDER]`. Returns when the app quits. AppKit runs
/// on the main thread, so this is called from it.
#[cfg(target_os = "macos")]
pub fn run() {
    mac::run();
}

/// The window on `folder` (else it asks for one), scanning with `options`: the entry point for
/// `duscape`, which has read its own command line.
#[cfg(target_os = "macos")]
pub fn run_with(folder: Option<::std::path::PathBuf>, options: libduscape::ScanOptions) {
    mac::run_with(folder, options);
}
