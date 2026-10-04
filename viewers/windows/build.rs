//! On Windows: the app's icon (`duscape.ico`, drawn by `duscape_viewer::icon`) as the
//! binary's own, the one Explorer shows for the `.exe`; the package's version; and the manifest
//! (`duscape-windows.manifest`).

include!("resources.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources.rs");
    println!("cargo:rerun-if-changed=duscape-windows.manifest");
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resources(
            Path::new("duscape.ico"),
            "duscape-windows",
            Path::new("duscape-windows.manifest"),
        );
    }
}
