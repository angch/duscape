use ::std::fs::Metadata;
use ::std::os::unix::fs::MetadataExt;

use rustix::process::{Uid, geteuid};

pub fn is_user_admin() -> bool {
    geteuid() == Uid::ROOT
}

/// Allocated size on disk from directory-walk metadata (`st_blocks` × 512-byte units).
pub fn size_on_disk_fast(metadata: &Metadata) -> u64 {
    metadata.blocks().saturating_mul(512)
}

pub fn volume_id(path: &::std::path::Path) -> Option<u64> {
    ::std::fs::metadata(path).map(|m| m.dev()).ok()
}

pub fn link_count(path: &::std::path::Path) -> u64 {
    ::std::fs::metadata(path).map(|m| m.nlink()).unwrap_or(1)
}

/// Bytes free on the filesystem mounted at `path`, as `df` counts them (what root may use, not
/// what a user may), or `None` when `path` is not a mount point: what a view of the whole
/// filesystem shows beside what is used.
pub fn volume_free(path: &::std::path::Path) -> Option<u64> {
    if !is_mount_point(path) {
        return None;
    }
    let fs = ::rustix::fs::statvfs(path).ok()?;
    Some(fs.f_bfree.saturating_mul(fs.f_frsize))
}

/// Whether `path` is on another machine's filesystem, as far as this platform says: on macOS a
/// mount not marked local (`MNT_LOCAL`); elsewhere `false` — Linux's answer is the scanner's,
/// which knows its network filesystems (`duscape_scan::is_network`).
#[must_use]
pub fn is_network(path: &::std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use ::std::os::unix::ffi::OsStrExt;
        let Ok(name) = ::std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `name` is NUL-terminated, and `fs` a zeroed struct the call fills.
        let mut fs: libc::statfs = unsafe { ::std::mem::zeroed() };
        // SAFETY: as above; the struct outlives the call.
        if unsafe { libc::statfs(name.as_ptr(), &raw mut fs) } != 0 {
            return false;
        }
        fs.f_flags & libc::MNT_LOCAL as u32 == 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// Whether `path` is where a filesystem is mounted: its device differs from its parent's.
fn is_mount_point(path: &::std::path::Path) -> bool {
    let Ok(device) = ::std::fs::metadata(path).map(|meta| meta.dev()) else {
        return false;
    };
    path.parent().is_none_or(|parent| {
        ::std::fs::metadata(parent).map_or(true, |parent| parent.dev() != device)
    })
}

/// Bytes in use on the filesystem mounted at `path`, as `df` counts them, or `None` when `path` is
/// not a mount point — a subfolder's size cannot be compared with its filesystem's.
pub fn volume_used(path: &::std::path::Path) -> Option<u64> {
    if !is_mount_point(path) {
        return None;
    }
    #[cfg(target_os = "macos")]
    if path != ::std::path::Path::new("/")
        && let Some(used) = apfs_volume_used(path)
    {
        return Some(used);
    }
    let fs = ::rustix::fs::statvfs(path).ok()?;
    Some(
        fs.f_blocks
            .saturating_sub(fs.f_bfree)
            .saturating_mul(fs.f_frsize),
    )
}

/// The bytes the volume at `path` itself uses (`ATTR_VOL_SPACEUSED`, what `df` prints), where
/// `statvfs` cannot say: on APFS every volume of a container reports the container's blocks and
/// free blocks, so their difference is every volume's use — on `/System/Volumes/Preboot`, 805 GB
/// of the data volume's beside its own 8 GB, all of it counted as not found by its scan. `/` is
/// left to the container's figure: its scan reaches the data volume through the firmlinks and
/// the other volumes mounted under it (`macos::walk_macos` enters every mount but its own
/// device), so what it can find is nearly the container's whole use, and its own volume's 12 GB
/// would leave the found 800 GB no room. `None` where the attribute is not answered.
#[cfg(target_os = "macos")]
fn apfs_volume_used(path: &::std::path::Path) -> Option<u64> {
    use ::std::os::unix::ffi::OsStrExt;
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        used: libc::off_t,
    }
    let path = ::std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = Reply { length: 0, used: 0 };
    // SAFETY: `path` is a NUL-terminated string, `request` a valid attribute list, and `reply`
    // a buffer of the size given, which the call writes no further than.
    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut request).cast(),
            (&raw mut reply).cast(),
            ::std::mem::size_of::<Reply>(),
            0,
        )
    };
    let (length, used) = (reply.length, reply.used);
    (status == 0 && length as usize >= ::std::mem::size_of::<Reply>())
        .then(|| u64::try_from(used).ok())
        .flatten()
}

/// A file whose unwritten length occupies nothing. Unix filesystems make the hole without
/// being asked when the length is set, so there is nothing to do; see the Windows one.
pub fn set_sparse(_file: &::std::fs::File) -> bool {
    true
}
