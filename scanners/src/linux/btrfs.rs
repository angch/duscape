//! What the walk asks btrfs itself, by its own ioctls: which filesystem a subvolume belongs to
//! (`fsid`), what a compressed file occupies ([`extents`]), and which subvolumes are read-only
//! snapshots ([`subvolume`]). When to ask is decided where the walk meets a filesystem
//! ([`super::filesystem::classify_with`]) or crosses into one.

use ::std::path::Path;

pub(crate) mod extents;
pub(crate) mod subvolume;

pub const MAGIC: u32 = 0x9123_683e;

/// `_IOR(0x94, 31, struct btrfs_ioctl_fs_info_args)`, a 1024-byte struct.
const BTRFS_IOC_FS_INFO: libc::Ioctl = 0x8400_941f_u32 as libc::Ioctl;

/// The btrfs filesystem `path` is on, from its UUID, folded to 64 bits.
///
/// btrfs gives every subvolume and snapshot its own `st_dev` — and its own `f_fsid` — though
/// they all index one address space, so neither says which extents are the same. The UUID
/// does, and `BTRFS_IOC_FS_INFO` hands it to any user who can open the directory (checked on
/// kernel 7.0: the top level, a subvolume, a folder in it and a snapshot of it all agree,
/// where `st_dev` and `f_fsid` differ). The generic `FS_IOC_GETFSUUID` is not implemented by
/// btrfs.
pub fn fsid(path: &Path) -> Option<u64> {
    use ::rustix::fs::{Mode, OFlags, open};
    use ::std::os::fd::AsRawFd;
    let dir = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;
    // `max_id` and `num_devices`, then the 16-byte `fsid`, then fields not needed here.
    let mut args = [0u8; 1024];
    // SAFETY: `args` is the 1024 bytes the request's size says it writes, and `dir` is open
    // for the duration of the call.
    let result = unsafe { libc::ioctl(dir.as_raw_fd(), BTRFS_IOC_FS_INFO, args.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let mut id: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in &args[16..32] {
        id = (id ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(id)
}

/// Whether the walk leaves a folder it reaches empty as a read-only snapshot: the whole of what
/// the snapshot copied, again, unless `ScanOptions::snapshots` asks for it. Every subvolume has
/// a device of its own, so one is always a crossing, onto btrfs, at a root numbered 256; only
/// there is `read_only` asked — it opens the directory, which is rare, and never off btrfs,
/// where opening an automount point would mount it.
pub fn snapshot_left_out(
    snapshots: bool,
    on_btrfs: bool,
    inode: u64,
    read_only: impl FnOnce() -> bool,
) -> bool {
    !snapshots && on_btrfs && inode == subvolume::ROOT_INODE && read_only()
}
