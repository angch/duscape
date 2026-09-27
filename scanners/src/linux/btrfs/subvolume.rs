//! btrfs subvolumes, for leaving read-only snapshots out of a walk (`ScanOptions::snapshots`).

use ::std::ffi::CStr;
use ::std::os::fd::{AsRawFd, BorrowedFd};

use ::rustix::fs::{Mode, OFlags, openat};

/// The inode number of every btrfs subvolume's root directory (`BTRFS_FIRST_FREE_OBJECTID`).
/// The walk asks only where it crosses onto btrfs; elsewhere the question would just get
/// `ENOTTY`.
pub const ROOT_INODE: u64 = 256;

/// `_IOR(0x94, 25, __u64)`.
const BTRFS_IOC_SUBVOL_GETFLAGS: libc::Ioctl = 0x8008_9419_u32 as libc::Ioctl;
/// `BTRFS_SUBVOL_RDONLY`, in what `BTRFS_IOC_SUBVOL_GETFLAGS` returns.
const READ_ONLY: u64 = 1 << 1;

/// Whether `name` in `dir` is the root of a read-only btrfs subvolume: a snapshot, taken
/// read-only as Synology DSM, snapper and `btrfs subvolume snapshot -r` take them. No
/// privilege is needed to ask. Anything that cannot be asked is not one.
pub fn is_read_only(dir: BorrowedFd<'_>, name: &CStr) -> bool {
    let Ok(subvolume) = openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    flags(&subvolume).is_some_and(|flags| flags & READ_ONLY != 0)
}

/// Whether the directory at `path` is one; see [`is_read_only`].
pub fn path_is_read_only(path: &::std::path::Path) -> bool {
    ::rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()
    .and_then(|subvolume| flags(&subvolume))
    .is_some_and(|flags| flags & READ_ONLY != 0)
}

fn flags(subvolume: &impl ::std::os::fd::AsFd) -> Option<u64> {
    let mut flags = 0u64;
    // SAFETY: the descriptor is open for the call, and the kernel writes one `u64`, the
    // size the request encodes, to `flags`.
    let done = unsafe {
        libc::ioctl(
            subvolume.as_fd().as_raw_fd(),
            BTRFS_IOC_SUBVOL_GETFLAGS,
            &raw mut flags,
        )
    };
    (done == 0).then_some(flags)
}
