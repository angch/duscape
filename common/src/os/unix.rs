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
    let fs = ::rustix::fs::statvfs(path).ok()?;
    Some(
        fs.f_blocks
            .saturating_sub(fs.f_bfree)
            .saturating_mul(fs.f_frsize),
    )
}

/// A file whose unwritten length occupies nothing. Unix filesystems make the hole without
/// being asked when the length is set, so there is nothing to do; see the Windows one.
pub fn set_sparse(_file: &::std::fs::File) -> bool {
    true
}
