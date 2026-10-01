//! macOS: whether a mounted volume is a disk image whose file the scan counts already.
//!
//! A disk image's volume is mounted from a file: the iOS simulator's runtime, on this machine
//! 21.6 GiB at `/Library/Developer/CoreSimulator/Volumes/iOS_23F77`, from
//! `/System/Library/AssetsV2/…/iOSSimulatorRuntime_Cryptex.dmg`. A scan of `/` walked both — the
//! image as a file on the data volume and its volume as a mount — and counted those blocks
//! twice. So the walker leaves such a mount empty, noted, when the image is inside the scan; an
//! image elsewhere (a share, another disk not walked) is the only route to its files, and walked.
//! The rule asks where the image is, not whether the walk read it: an image in a folder the scan
//! is refused, or past `--max-depth`, leaves its volume uncounted either way — rare, and an
//! undercount the status line's "not seen" takes in, where walking it would overcount unseen.
//!
//! IOKit's registry says which file: the volume's device (`f_mntfromname`, `/dev/disk5s1`) has
//! an `AppleDiskImageDevice` above it, whose `DiskImageURL` is the image. IOKit and
//! CoreFoundation are opened with `dlopen` when a mount is first crossed, as `fsevents.rs` opens
//! CoreServices: the viewers keep frameworks out of their load commands.

use ::std::ffi::{CStr, CString, c_char, c_void};
use ::std::os::unix::ffi::OsStrExt;
use ::std::path::{Path, PathBuf};

/// IOKit's and CoreFoundation's entry points, resolved once.
struct Api {
    bsd_name_matching: unsafe extern "C" fn(u32, u32, *const c_char) -> *mut c_void,
    get_matching_service: unsafe extern "C" fn(u32, *mut c_void) -> u32,
    search_property: unsafe extern "C" fn(
        u32,
        *const c_char,
        *const c_void,
        *const c_void,
        u32,
    ) -> *const c_void,
    object_release: unsafe extern "C" fn(u32) -> i32,
    string_create: unsafe extern "C" fn(*const c_void, *const c_char, u32) -> *const c_void,
    get_type_id: unsafe extern "C" fn(*const c_void) -> usize,
    string_type_id: unsafe extern "C" fn() -> usize,
    data_type_id: unsafe extern "C" fn() -> usize,
    data_length: unsafe extern "C" fn(*const c_void) -> isize,
    data_bytes: unsafe extern "C" fn(*const c_void) -> *const u8,
    url_create: unsafe extern "C" fn(*const c_void, *const c_void, *const c_void) -> *const c_void,
    url_path: unsafe extern "C" fn(*const c_void, u8, *mut u8, isize) -> u8,
    release: unsafe extern "C" fn(*const c_void),
}

const UTF8: u32 = 0x0800_0100;
/// `kIORegistryIterateRecursively | kIORegistryIterateParents`: up through the device's
/// providers to the disk image above it.
const UP_THE_TREE: u32 = 0x1 | 0x2;

impl Api {
    fn load() -> Option<&'static Api> {
        use ::std::sync::OnceLock;
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(Api::open).as_ref()
    }

    fn open() -> Option<Api> {
        let open = |path: &CStr| {
            // SAFETY: a NUL-terminated path; dlopen returns null on failure.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            (!handle.is_null()).then_some(handle)
        };
        let iokit = open(c"/System/Library/Frameworks/IOKit.framework/IOKit")?;
        let cf = open(c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")?;
        macro_rules! function {
            ($handle:expr, $name:literal, $signature:ty) => {{
                // SAFETY: the handle is open and the name NUL-terminated; not found is null.
                let pointer = unsafe { libc::dlsym($handle, $name.as_ptr()) };
                if pointer.is_null() {
                    return None;
                }
                // SAFETY: the symbol is IOKit's or CoreFoundation's, whose signature is the one
                // given (`IOKitLib.h`, `CFString.h`, `CFURL.h`, `CFBase.h`), and a function
                // pointer transmutes from the address `dlsym` gave.
                unsafe { ::std::mem::transmute::<*mut c_void, $signature>(pointer) }
            }};
        }
        Some(Api {
            bsd_name_matching: function!(
                iokit,
                c"IOBSDNameMatching",
                unsafe extern "C" fn(u32, u32, *const c_char) -> *mut c_void
            ),
            get_matching_service: function!(
                iokit,
                c"IOServiceGetMatchingService",
                unsafe extern "C" fn(u32, *mut c_void) -> u32
            ),
            search_property: function!(
                iokit,
                c"IORegistryEntrySearchCFProperty",
                unsafe extern "C" fn(
                    u32,
                    *const c_char,
                    *const c_void,
                    *const c_void,
                    u32,
                ) -> *const c_void
            ),
            object_release: function!(iokit, c"IOObjectRelease", unsafe extern "C" fn(u32) -> i32),
            string_create: function!(
                cf,
                c"CFStringCreateWithCString",
                unsafe extern "C" fn(*const c_void, *const c_char, u32) -> *const c_void
            ),
            get_type_id: function!(
                cf,
                c"CFGetTypeID",
                unsafe extern "C" fn(*const c_void) -> usize
            ),
            string_type_id: function!(cf, c"CFStringGetTypeID", unsafe extern "C" fn() -> usize),
            data_type_id: function!(cf, c"CFDataGetTypeID", unsafe extern "C" fn() -> usize),
            data_length: function!(
                cf,
                c"CFDataGetLength",
                unsafe extern "C" fn(*const c_void) -> isize
            ),
            data_bytes: function!(
                cf,
                c"CFDataGetBytePtr",
                unsafe extern "C" fn(*const c_void) -> *const u8
            ),
            url_create: function!(
                cf,
                c"CFURLCreateWithString",
                unsafe extern "C" fn(*const c_void, *const c_void, *const c_void) -> *const c_void
            ),
            url_path: function!(
                cf,
                c"CFURLGetFileSystemRepresentation",
                unsafe extern "C" fn(*const c_void, u8, *mut u8, isize) -> u8
            ),
            release: function!(cf, c"CFRelease", unsafe extern "C" fn(*const c_void)),
        })
    }

    /// The image file behind the BSD device `name` (`disk5s1`), if it is a disk image's: the
    /// registry names it `image-path` (the path's bytes) for an image `hdiutil` or the Finder
    /// attached, `DiskImageURL` (a `file://` URL) for one the system did, the simulator's.
    fn image_of(&self, name: &CStr) -> Option<PathBuf> {
        // SAFETY: the calls are IOKit's with the arguments `IOKitLib.h` asks for; the service
        // found is released here.
        unsafe {
            // Port 0 is `kIOMainPortDefault`. The matching dictionary is consumed by the lookup.
            let matching = (self.bsd_name_matching)(0, 0, name.as_ptr());
            if matching.is_null() {
                return None;
            }
            let media = (self.get_matching_service)(0, matching);
            if media == 0 {
                return None;
            }
            let path = self
                .property(media, c"image-path")
                .or_else(|| self.property(media, c"DiskImageURL"));
            (self.object_release)(media);
            path
        }
    }

    /// The path `key` names, searched for up the registry from `entry`: a URL string, or a
    /// path's bytes.
    ///
    /// # Safety
    /// `entry` is a live registry entry.
    unsafe fn property(&self, entry: u32, key: &CStr) -> Option<PathBuf> {
        // SAFETY: as the caller promises; every object created here is released here, and a
        // null is never passed on.
        unsafe {
            let key = (self.string_create)(::std::ptr::null(), key.as_ptr(), UTF8);
            if key.is_null() {
                return None;
            }
            let value = (self.search_property)(
                entry,
                c"IOService".as_ptr(),
                key,
                ::std::ptr::null(),
                UP_THE_TREE,
            );
            (self.release)(key);
            if value.is_null() {
                return None;
            }
            let kind = (self.get_type_id)(value);
            let path = if kind == (self.string_type_id)() {
                self.file_path(value)
            } else if kind == (self.data_type_id)() {
                let length = usize::try_from((self.data_length)(value)).unwrap_or(0);
                let bytes = (self.data_bytes)(value);
                (!bytes.is_null() && length > 0).then(|| {
                    let bytes = ::std::slice::from_raw_parts(bytes, length);
                    let bytes = bytes.split(|&byte| byte == 0).next().unwrap_or(bytes);
                    PathBuf::from(::std::ffi::OsStr::from_bytes(bytes))
                })
            } else {
                None
            };
            (self.release)(value);
            path
        }
    }

    /// The path a `file://` URL string names, percent escapes undone.
    ///
    /// # Safety
    /// `url_string` is a live `CFString`.
    unsafe fn file_path(&self, url_string: *const c_void) -> Option<PathBuf> {
        // SAFETY: as the caller promises; the URL made here is released here.
        unsafe {
            let url = (self.url_create)(::std::ptr::null(), url_string, ::std::ptr::null());
            if url.is_null() {
                return None;
            }
            let mut buffer = vec![0u8; libc::PATH_MAX as usize];
            let ok = (self.url_path)(url, 1, buffer.as_mut_ptr(), buffer.len() as isize);
            (self.release)(url);
            if ok == 0 {
                return None;
            }
            let path = CStr::from_bytes_until_nul(&buffer).ok()?;
            Some(PathBuf::from(::std::ffi::OsStr::from_bytes(
                path.to_bytes(),
            )))
        }
    }
}

/// The disk image the volume mounted at `mount` is read from, when it is one.
#[must_use]
pub fn backing_file(mount: &Path) -> Option<PathBuf> {
    let path = CString::new(mount.as_os_str().as_bytes()).ok()?;
    let mut status = ::std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `status` is a writable `statfs` allocation.
    if unsafe { libc::statfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returning zero means it initialized the structure.
    let status = unsafe { status.assume_init() };
    let from: Vec<u8> = status
        .f_mntfromname
        .iter()
        .take_while(|character| **character != 0)
        .map(|character| *character as u8)
        .collect();
    let device = from.strip_prefix(b"/dev/")?;
    Api::load()?.image_of(&CString::new(device).ok()?)
}

/// `path` as it is on its volume, firmlinks undone (`ATTR_CMNEXT_NOFIRMLINKPATH`):
/// `/System/Library/AssetsV2` is `/System/Volumes/Data/System/Library/AssetsV2`, so paths
/// reached through either route compare.
#[must_use]
pub fn real_path(path: &Path) -> Option<PathBuf> {
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        offset: i32,
        size: u32,
        text: [u8; libc::PATH_MAX as usize],
    }
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_NOFIRMLINKPATH,
    };
    let mut reply = Reply {
        length: 0,
        offset: 0,
        size: 0,
        text: [0; libc::PATH_MAX as usize],
    };
    // SAFETY: `path` is NUL-terminated, `request` a valid attribute list, and `reply` a buffer
    // of the size given, which the call writes no further than.
    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut request).cast(),
            (&raw mut reply).cast(),
            ::std::mem::size_of::<Reply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if status != 0 {
        return None;
    }
    // The text is out of line, `offset` bytes from the reference itself (after `length`).
    let (offset, size) = (reply.offset, reply.size);
    let start = usize::try_from(offset).ok()?.checked_sub(8)?;
    let text = reply.text.get(start..start + size as usize)?;
    let text = CStr::from_bytes_until_nul(text).ok()?;
    Some(PathBuf::from(::std::ffi::OsStr::from_bytes(
        text.to_bytes(),
    )))
}

/// The image the volume mounted at `mount` is read from, when that image is under `root` (as
/// [`real_path`] gives it), so the scan counts it as a file and its volume would be counted
/// twice. An image the walk will not reach — on a share — is not.
#[must_use]
pub fn counted_by_the_scan(mount: &Path, root: &Path) -> Option<PathBuf> {
    let image = backing_file(mount)?;
    if crate::macos::is_remote(&image) {
        return None;
    }
    real_path(&image)?.starts_with(root).then_some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root of the system's volume group is no disk image, and a firmlinked path resolves to
    /// the data volume's.
    #[test]
    fn a_firmlinked_path_resolves_to_the_data_volume() {
        assert!(backing_file(Path::new("/")).is_none());
        let real = real_path(Path::new("/Users")).expect("the real path of /Users");
        assert!(
            real == Path::new("/System/Volumes/Data/Users") || real == Path::new("/Users"),
            "{}",
            real.display()
        );
    }
}
