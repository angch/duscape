//! Windows, with the window built in: embed `windows.manifest` in the binaries, so Explorer
//! starts `duscape.exe` with no console (`consoleAllocationPolicy`, see the manifest), and the
//! app's icon (`../windows/duscape.ico`, drawn by `duscape_viewer::icon`) as the `.exe`'s own,
//! with the package's version.
//! macOS, with the window built in: link no framework the binary names no symbol of, so AppKit
//! is loaded when the window starts (`duscape_mac`'s `appkit::load`), not by every terminal run.

include!("../windows/resources.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows.manifest");
    println!("cargo:rerun-if-changed=../windows/resources.rs");
    let windows = env::var_os("CARGO_CFG_WINDOWS").is_some();
    let window_built = env::var_os("CARGO_FEATURE_GUI").is_some();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") && window_built {
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
    }
    if windows && window_built {
        let icon = Path::new("../windows/duscape.ico");
        embed_resources(icon, "duscape", Path::new("windows.manifest"));
    }
}
