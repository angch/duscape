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

/// Bytes in use on the filesystem mounted at `path`, as `df` counts them, or `None` when `path` is
/// not a mount point — a subfolder's size cannot be compared with its filesystem's.
pub fn volume_used(path: &::std::path::Path) -> Option<u64> {
    let device = ::std::fs::metadata(path).ok()?.dev();
    let mount_point = path.parent().is_none_or(|parent| {
        ::std::fs::metadata(parent).map_or(true, |parent| parent.dev() != device)
    });
    if !mount_point {
        return None;
    }
    let fs = ::rustix::fs::statvfs(path).ok()?;
    Some(
        fs.f_blocks
            .saturating_sub(fs.f_bfree)
            .saturating_mul(fs.f_frsize),
    )
}
