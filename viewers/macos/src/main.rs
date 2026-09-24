//! A native macOS window on diskonaut-angch.
//!
//! The scan comes from `diskonaut-scan`, and the tree, the squarified layout, deletion and file
//! previews from `libdiskonaut` — the code the terminal viewer runs. `state` is what the window
//! shows and how it answers input, with no AppKit in it; `mac` is AppKit: the window, its menus,
//! drawing, dialogs, the Trash, the pasteboard, Finder and Quick Look.

// Elsewhere the platform-independent modules are built only for their tests, which run on the
// Linux CI, and have no caller.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(any(target_os = "macos", test))]
mod preview;
#[cfg(any(target_os = "macos", test))]
mod scan;
#[cfg(any(target_os = "macos", test))]
mod state;

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
