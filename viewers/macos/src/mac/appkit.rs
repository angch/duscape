//! AppKit, loaded when the window starts rather than when the program does.
//!
//! `duscape` is the terminal viewer too, and linked to AppKit it loaded the framework and its
//! closure (Foundation, CoreGraphics, CloudKit… fifteen libraries) on every start: 0.35 s on a
//! cold cache before a terminal scan could begin. objc2 finds classes by name at run time, so the
//! binaries are linked with `-dead_strip_dylibs` (the build scripts) and name no framework:
//! [`load`] opens AppKit and Quick Look before any class is asked for, and the few constants
//! AppKit exports as data are read here by name rather than linked, since a reference to one
//! would keep the framework in the load commands.

use ::std::ffi::{CStr, c_void};

use objc2_foundation::NSString;

/// What the window uses: AppKit (with Foundation and the rest it loads) and Quick Look's panel.
const FRAMEWORKS: [&CStr; 2] = [
    c"/System/Library/Frameworks/AppKit.framework/AppKit",
    c"/System/Library/Frameworks/QuickLookUI.framework/QuickLookUI",
];

/// Opens the frameworks, for good. Called first thing when the window starts: a class looked up
/// before it is not found.
pub fn load() {
    for framework in FRAMEWORKS {
        // SAFETY: a NUL-terminated path; the handle is never closed, so what it loads stays
        // loaded for the life of the process, as a linked framework would.
        let handle =
            unsafe { libc::dlopen(framework.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            // SAFETY: `dlerror` says why the last `dlopen` on this thread failed, as a C string
            // or null.
            let why = unsafe { libc::dlerror() };
            let why = if why.is_null() {
                "no reason given".into()
            } else {
                // SAFETY: a non-null `dlerror` is a NUL-terminated string, valid until the
                // next `dl*` call on this thread; it is copied at once.
                unsafe { CStr::from_ptr(why) }.to_string_lossy()
            };
            panic!("cannot load {}: {why}", framework.to_string_lossy());
        }
    }
}

/// The address of a symbol the loaded frameworks export.
fn address(name: &CStr) -> *const c_void {
    // SAFETY: a NUL-terminated name, looked up in everything loaded.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    assert!(
        !address.is_null(),
        "AppKit has no {}: was appkit::load called?",
        name.to_string_lossy()
    );
    address.cast()
}

/// One of AppKit's `NSString *const` constants.
fn string(name: &CStr) -> &'static NSString {
    // SAFETY: every name passed is an `NSString *const` AppKit exports: the symbol holds a
    // pointer to a string the framework never frees, and it stays loaded (see `load`).
    unsafe { *address(name).cast::<&'static NSString>() }
}

/// One of AppKit's `NSFontWeight` constants, a `CGFloat`.
fn weight(name: &CStr) -> f64 {
    // SAFETY: every name passed is an `NSFontWeight` constant, a `CGFloat` (`f64` on every Mac
    // this builds for).
    unsafe { *address(name).cast::<f64>() }
}

pub fn font_attribute() -> &'static NSString {
    string(c"NSFontAttributeName")
}

pub fn foreground_color_attribute() -> &'static NSString {
    string(c"NSForegroundColorAttributeName")
}

pub fn paragraph_style_attribute() -> &'static NSString {
    string(c"NSParagraphStyleAttributeName")
}

pub fn pasteboard_type_string() -> &'static NSString {
    string(c"NSPasteboardTypeString")
}

pub fn pasteboard_type_file_url() -> &'static NSString {
    string(c"NSPasteboardTypeFileURL")
}

pub fn font_weight_regular() -> f64 {
    weight(c"NSFontWeightRegular")
}

pub fn font_weight_medium() -> f64 {
    weight(c"NSFontWeightMedium")
}

pub fn font_weight_semibold() -> f64 {
    weight(c"NSFontWeightSemibold")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_are_found_once_loaded() {
        load();
        assert_eq!(font_attribute().to_string(), "NSFont");
        assert_eq!(
            pasteboard_type_string().to_string(),
            "public.utf8-plain-text"
        );
        assert_eq!(pasteboard_type_file_url().to_string(), "public.file-url");
        assert_eq!(font_weight_regular(), 0.0);
        assert!(font_weight_regular() < font_weight_medium());
        assert!(font_weight_medium() < font_weight_semibold());
    }
}
