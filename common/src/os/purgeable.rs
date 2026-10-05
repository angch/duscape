//! macOS: how much of a volume the system says it could free — Time Machine's local snapshots,
//! and files it keeps as purgeable (caches, iCloud copies held locally) — as what is
//! available for important use less what is free (`NSURLVolumeAvailableCapacityForImportant
//! UsageKey`, through CoreFoundation's `CFURL`). 77.2 GB on a Mac with four snapshots
//! (2026-10-05). It counts purgeable *files*, which a scan finds, as well as snapshots, which
//! no walk does: a bound on what snapshots hold, never an amount to take off.
//!
//! CoreFoundation is opened with `dlopen`, as `duscape_scan::fsevents` opens CoreServices: the
//! binaries keep frameworks out of their load commands. The first answer in a process costs up
//! to a second (CoreFoundation opened: 0.98 s in a test; 44 ms where it was already loaded),
//! the next 8–9 ms (measured): asked once a scan, on a thread of its own beside the walk
//! (`duscape_scan`'s `ask_purgeable`), never at a relayout.

use ::std::ffi::{CStr, c_void};
use ::std::path::Path;
use ::std::sync::OnceLock;

type Ref = *const c_void;
type UrlFromPath = unsafe extern "C" fn(Ref, *const u8, isize, u8) -> Ref;
type CopyProperty = unsafe extern "C" fn(Ref, Ref, *mut Ref, *mut Ref) -> u8;
type NumberValue = unsafe extern "C" fn(Ref, isize, *mut c_void) -> u8;
type Release = unsafe extern "C" fn(Ref);

/// CoreFoundation's entry points and the two keys, resolved once.
struct Api {
    url_from_path: UrlFromPath,
    copy_property: CopyProperty,
    number_value: NumberValue,
    release: Release,
    important: Ref,
    free: Ref,
}

// SAFETY: the keys are immutable CFString constants, shared by every thread.
unsafe impl Send for Api {}
// SAFETY: as above; the functions are thread-safe CoreFoundation calls.
unsafe impl Sync for Api {}

/// `kCFNumberSInt64Type`.
const SINT64: isize = 4;

impl Api {
    fn load() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(Api::open).as_ref()
    }

    fn open() -> Option<Api> {
        let path = c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";
        // SAFETY: a NUL-terminated path; dlopen returns null on failure.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is open and `name` NUL-terminated; a symbol not found is null.
        let sym = |name: &CStr| {
            let pointer = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!pointer.is_null()).then_some(pointer)
        };
        // A key is a data symbol: a `CFStringRef` stored at the address dlsym gives.
        // SAFETY: the symbols are CoreFoundation's `CFStringRef` constants.
        let key = |name: &CStr| sym(name).map(|at| unsafe { *at.cast::<Ref>() });
        // SAFETY: each symbol is CoreFoundation's, with the signature `CFURL.h` and
        // `CFNumber.h` declare, and a function pointer transmutes from the address dlsym gave.
        unsafe {
            Some(Api {
                url_from_path: ::std::mem::transmute::<*mut c_void, UrlFromPath>(sym(
                    c"CFURLCreateFromFileSystemRepresentation",
                )?),
                copy_property: ::std::mem::transmute::<*mut c_void, CopyProperty>(sym(
                    c"CFURLCopyResourcePropertyForKey",
                )?),
                number_value: ::std::mem::transmute::<*mut c_void, NumberValue>(sym(
                    c"CFNumberGetValue",
                )?),
                release: ::std::mem::transmute::<*mut c_void, Release>(sym(c"CFRelease")?),
                important: key(c"kCFURLVolumeAvailableCapacityForImportantUsageKey")?,
                free: key(c"kCFURLVolumeAvailableCapacityKey")?,
            })
        }
    }

    /// The volume property `key` of `url`, a number.
    fn number(&self, url: Ref, key: Ref) -> Option<i64> {
        let mut value: Ref = ::std::ptr::null();
        // SAFETY: `url` is a live CFURL, `key` a resource key; the value is ours to release.
        let found =
            unsafe { (self.copy_property)(url, key, &raw mut value, ::std::ptr::null_mut()) };
        if found == 0 || value.is_null() {
            return None;
        }
        let mut number = 0i64;
        // SAFETY: `value` is the CFNumber the key gives, read as an `i64`; released once.
        let read = unsafe {
            let read = (self.number_value)(value, SINT64, (&raw mut number).cast());
            (self.release)(value);
            read
        };
        (read != 0).then_some(number)
    }
}

/// Bytes of the volume at `path` the system says it could free: its snapshots and purgeable
/// files together. `None` where it cannot be asked.
#[must_use]
pub fn volume_purgeable(path: &Path) -> Option<u64> {
    let api = Api::load()?;
    let bytes = path.as_os_str().as_encoded_bytes();
    // SAFETY: the bytes and their length go together; the URL is released below.
    let url = unsafe {
        (api.url_from_path)(
            ::std::ptr::null(),
            bytes.as_ptr(),
            isize::try_from(bytes.len()).ok()?,
            1,
        )
    };
    if url.is_null() {
        return None;
    }
    let important = api.number(url, api.important);
    let free = api.number(url, api.free);
    // SAFETY: created above, released once.
    unsafe { (api.release)(url) };
    u64::try_from(important?.checked_sub(free?)?).ok()
}

#[cfg(test)]
mod tests {
    /// This Mac's data volume says what it could free. `--ignored`, as CI has no Mac.
    #[test]
    #[ignore = "asks this Mac's data volume"]
    fn the_data_volume_says_what_it_could_free() {
        let path = ::std::path::Path::new("/System/Volumes/Data");
        let started = ::std::time::Instant::now();
        let bytes = super::volume_purgeable(path).expect("asked");
        eprintln!(
            "{bytes} purgeable in {:.3}s",
            started.elapsed().as_secs_f64()
        );
    }
}
