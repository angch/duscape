//! On Windows: the app's icon (`duscape.ico`, drawn by `duscape_viewer::icon`) as the
//! binary's own, the one Explorer shows for the `.exe`; the package's version; and the manifest
//! (`duscape-windows.manifest`).

include!("resources.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources.rs");
    println!("cargo:rerun-if-changed=duscape-windows.manifest");
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let icon = Path::new("duscape.ico");
        let manifest = Path::new("duscape-windows.manifest");
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            // The manifest by linker options, the icon and version as a resource file.
            embed_manifest::embed_manifest_file(manifest)
                .expect("embedding duscape-windows.manifest in the binary");
            embed_resources(icon, "duscape-windows", None);
        } else {
            // One resource object for all: a second would not link.
            embed_resources(icon, "duscape-windows", Some(manifest));
        }
    }
}
