//! A native macOS window on diskonaut-angch.
//!
//! The scan comes from `diskonaut-scan`, and the tree, the squarified layout, deletion and file
//! previews from `libdiskonaut` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `diskonaut_viewer::state`, shared with the Linux viewer and free of
//! AppKit; `mac` is AppKit: the window, its menus, drawing, dialogs, the Trash, the pasteboard,
//! Finder and Quick Look.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("diskonaut-mac is a macOS-only GUI. Use `diskonaut` on this platform.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}
