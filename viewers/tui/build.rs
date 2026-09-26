//! Windows, with the window built in: embed `windows.manifest` in the binaries, so Explorer
//! starts `duscape.exe` with no console (`consoleAllocationPolicy`, see the manifest).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows.manifest");
    let windows = ::std::env::var_os("CARGO_CFG_WINDOWS").is_some();
    let window_built = ::std::env::var_os("CARGO_FEATURE_GUI").is_some();
    if windows && window_built {
        embed_manifest::embed_manifest_file("windows.manifest")
            .expect("embedding windows.manifest in the binaries");
    }
}
