//! The Linux desktop files (`packaging/linux/`) agree with the app: named by its ID, naming its
//! icon and window class by it, and the metadata's newest release the workspace's version. A
//! Wayland compositor finds a window's icon and name only through the `.desktop` file of the
//! window's app ID, so a rename on one side alone would leave the window without either.

use ::std::fs;
use ::std::path::{Path, PathBuf};

use duscape_viewer::APP_ID;

fn packaging() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux")
}

fn read(name: &str) -> String {
    let path = packaging().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The value of `key` in the `.desktop` file's main group.
fn desktop_value<'a>(desktop: &'a str, key: &str) -> Option<&'a str> {
    desktop
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

#[test]
fn the_desktop_file_is_named_by_the_app_id_and_names_it() {
    let desktop = read(&format!("{APP_ID}.desktop"));
    // `lines`, not a `\n`: a Windows checkout may have made the file's line ends `\r\n`.
    assert_eq!(desktop.lines().next(), Some("[Desktop Entry]"));
    assert_eq!(desktop_value(&desktop, "Icon"), Some(APP_ID));
    assert_eq!(desktop_value(&desktop, "StartupWMClass"), Some(APP_ID));
    // The window, whatever the launcher's terminal: `--gui` makes `front::choose` pick it.
    let exec = desktop_value(&desktop, "Exec").expect("Exec");
    assert!(exec.starts_with("duscape --gui"), "{exec}");
    assert_eq!(desktop_value(&desktop, "Terminal"), Some("false"));
    // No MimeType: claiming inode/directory can make duscape what opens every folder.
    assert_eq!(desktop_value(&desktop, "MimeType"), None);
}

/// The macOS About panel's copyright (Info.plist's `NSHumanReadableCopyright`) is LICENSE's
/// copyright lines, both, as the other viewers' About says.
#[test]
fn the_macos_copyright_is_the_licences() {
    let plist =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../macos/Info.plist"))
            .expect("Info.plist");
    let copyright = plist
        .split("<key>NSHumanReadableCopyright</key>")
        .nth(1)
        .and_then(|rest| rest.split("<string>").nth(1))
        .and_then(|rest| rest.split("</string>").next())
        .expect("NSHumanReadableCopyright");
    for line in libduscape::about::copyrights() {
        assert!(copyright.contains(line), "{copyright:?} lacks {line:?}");
    }
}

#[test]
fn the_metadata_names_the_app_id_and_this_version() {
    let metainfo = read(&format!("{APP_ID}.metainfo.xml"));
    assert!(metainfo.contains(&format!("<id>{APP_ID}</id>")));
    assert!(metainfo.contains(&format!(
        "<launchable type=\"desktop-id\">{APP_ID}.desktop</launchable>"
    )));
    assert!(metainfo.contains(&format!("<icon type=\"stock\">{APP_ID}</icon>")));
    // The newest release first, and it is this one: a version bump updates the metadata too.
    let newest = metainfo
        .split("<release version=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a release");
    assert_eq!(newest, env!("CARGO_PKG_VERSION"));
}
