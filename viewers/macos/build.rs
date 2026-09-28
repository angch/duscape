//! Link no framework the binary names no symbol of: AppKit is loaded when the window starts
//! (`appkit::load`), as `duscape` does (its `build.rs`), so `smoke.sh` runs that way too.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
    }
}
