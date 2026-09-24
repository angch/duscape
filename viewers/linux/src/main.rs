//! A Linux window on diskonaut-angch, with no toolkit.
//!
//! The scan comes from `diskonaut-scan`, and the tree, the squarified layout, deletion and file
//! previews from `libdiskonaut` — the code the terminal viewer runs. What the window shows and
//! how it answers input is `diskonaut_viewer::state`, shared with the macOS viewer. Here that is
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

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn main() {
    eprintln!("diskonaut-linux is a Wayland/X11 GUI for Linux and FreeBSD. Use `diskonaut` here.");
    std::process::exit(1);
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn main() {
    use ::std::path::PathBuf;

    let mut folder: Option<PathBuf> = None;
    let mut apparent = false;
    for arg in ::std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-a" | "--apparent-size") => apparent = true,
            Some("-h" | "--help") => {
                println!(
                    "diskonaut-linux [-a|--apparent-size] [FOLDER]\n\n\
                     A window on where the disk space went. Without a folder, the current one.\n\n\
                     DISKONAUT_BACKEND=…    wayland or x11 (else Wayland if WAYLAND_DISPLAY is set)\n\
                     DISKONAUT_SCALE=2      twice the size on X11 (else Xft.dpi, GDK_SCALE; Wayland scales itself)\n\
                     DISKONAUT_FONT=FILE    the font to use (else fontconfig's sans-serif)\n\
                     DISKONAUT_SNAPSHOT=PNG write the window to a PNG after the scan, and quit"
                );
                return;
            }
            Some(flag) if flag.starts_with('-') => {
                eprintln!("diskonaut-linux: unknown option {flag} (see --help)");
                ::std::process::exit(2);
            }
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let root = folder.unwrap_or_else(|| PathBuf::from("."));
    let root = root.canonicalize().unwrap_or(root);
    if !root.is_dir() {
        eprintln!("diskonaut-linux: “{}” is not a folder", root.display());
        ::std::process::exit(2);
    }
    let options = libdiskonaut::ScanOptions {
        show_apparent_size: apparent,
        ..libdiskonaut::ScanOptions::default()
    };
    let outcome = app::App::new(&root, options).and_then(app::App::run);
    if let Err(error) = outcome {
        eprintln!("diskonaut-linux: {error}");
        ::std::process::exit(1);
    }
}
