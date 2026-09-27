//! Windows, with the window built in: embed `windows.manifest` in the binaries, so Explorer
//! starts `duscape.exe` with no console (`consoleAllocationPolicy`, see the manifest), and the
//! app's icon (`../windows/duscape.ico`, drawn by `duscape_viewer::icon`) as the `.exe`'s own.

include!("../windows/resources.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows.manifest");
    println!("cargo:rerun-if-changed=../windows/resources.rs");
    let windows = env::var_os("CARGO_CFG_WINDOWS").is_some();
    let window_built = env::var_os("CARGO_FEATURE_GUI").is_some();
    if windows && window_built {
        let icon = Path::new("../windows/duscape.ico");
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            // The manifest by linker options, the icon as a resource file.
            embed_manifest::embed_manifest_file("windows.manifest")
                .expect("embedding windows.manifest in the binaries");
            embed_resources(icon, None);
        } else {
            // One resource object for both: a second would not link.
            embed_resources(icon, Some(Path::new("windows.manifest")));
        }
    }
}
