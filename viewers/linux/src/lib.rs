//! A Linux window on duscape, with no toolkit.
//!
//! The scan comes from `duscape-scan`, and the tree, the squarified layout, deletion and file
//! previews from `libduscape` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `duscape_viewer::state`, shared with the macOS viewer. Here that is
//! drawn into a software canvas (`canvas`, text by `font`) and put on a window by one of two
//! backends behind one trait (`backend`): native Wayland through `wayland-client` (`wayland`,
//! with `xkb` reading the keymap) or X11 through `x11rb` (`x11`) — both pure Rust with no C
//! library, so the binary builds static and runs wherever either is served. Linux and FreeBSD:
//! the frame's memory is a `memfd`, which the other BSDs lack.

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod app;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod backend;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod canvas;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod draw;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod font;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod trash;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod wayland;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod x11;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod xkb;

/// The window, from the command line: `[-a] [FOLDER]`. Returns when the window is closed; exits
/// the process on a mistake in the arguments or when no window can be opened.
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub fn run() {
    eprintln!("duscape-linux is a Wayland/X11 GUI for Linux and FreeBSD. Use `duscape` here.");
    std::process::exit(1);
}

/// The window, from the command line: `[-a] [FOLDER]`. Returns when the window is closed; exits
/// the process on a mistake in the arguments or when no window can be opened.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn run() {
    use ::std::path::PathBuf;

    let mut folder: Option<PathBuf> = None;
    let mut apparent = false;
    for arg in ::std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-a" | "--apparent-size") => apparent = true,
            // Which viewer: `duscape` has already chosen this one.
            Some("--gui") => {}
            Some("-h" | "--help") => {
                println!(
                    "duscape-linux [-a|--apparent-size] [FOLDER]\n\n\
                     A window on where the disk space went. Without a folder, the current one.\n\n\
                     DUSCAPE_BACKEND=…    wayland or x11 (else Wayland if WAYLAND_DISPLAY is set)\n\
                     DUSCAPE_SCALE=2      twice the size on X11 (else Xft.dpi, GDK_SCALE; Wayland scales itself)\n\
                     DUSCAPE_FONT=FILE    the font to use (else fontconfig's sans-serif)\n\
                     DUSCAPE_SNAPSHOT=PNG write the window to a PNG after the scan, and quit"
                );
                return;
            }
            Some(flag) if flag.starts_with('-') => {
                eprintln!("duscape-linux: unknown option {flag} (see --help)");
                ::std::process::exit(2);
            }
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let options = libduscape::ScanOptions {
        show_apparent_size: apparent,
        ..libduscape::ScanOptions::default()
    };
    run_with(folder, options);
}

/// The window on `folder` (else the current one), scanning with `options`: the entry point for
/// `duscape`, which has read its own command line. Exits the process if it is not a folder
/// or no window can be opened.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn run_with(folder: Option<::std::path::PathBuf>, options: libduscape::ScanOptions) {
    let root = folder.unwrap_or_else(|| ::std::path::PathBuf::from("."));
    let root = root.canonicalize().unwrap_or(root);
    if !root.is_dir() {
        eprintln!("duscape: “{}” is not a folder", root.display());
        ::std::process::exit(2);
    }
    let outcome = app::App::new(&root, options).and_then(app::App::run);
    if let Err(error) = outcome {
        eprintln!("duscape-linux: {error}");
        ::std::process::exit(1);
    }
}
