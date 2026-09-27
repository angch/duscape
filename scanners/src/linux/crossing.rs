//! Whether the walk goes into a folder where it meets another filesystem: the rules a rescan
//! of one folder must keep too (`walk_would_enter`), `-x`'s boundary, and mount loops.

use ::std::path::{Path, PathBuf};

use ::rustix::fs::{AtFlags, StatxFlags};

use super::stat::{device_of, statx};
use super::{filesystem, mount_of, mounts};
use crate::ScanOptions;

/// Whether a walk of `scan_root` goes into the folder `folder` inside it, which a rescan of that
/// folder alone must respect: its own walk starts there, and would otherwise skip every rule the
/// walk applies at a mount point — pseudo and network filesystems, `-x`, bind mounts of folders
/// the walk reaches anyway. Its ancestors were walked, or it would not be in the tree.
pub fn walk_would_enter(scan_root: &Path, folder: &Path, options: ScanOptions) -> bool {
    let wanted = StatxFlags::INO.union(StatxFlags::MNT_ID);
    let stat_of = |path: &Path| {
        statx(
            rustix::fs::CWD,
            path.as_os_str(),
            AtFlags::NO_AUTOMOUNT,
            wanted,
        )
    };
    let (Ok(stat), Ok(parent), Ok(root)) = (
        stat_of(folder),
        stat_of(folder.parent().unwrap_or(folder)),
        stat_of(scan_root),
    ) else {
        return true;
    };
    let device = device_of(&stat);
    // Read only if it is needed: at a mount point, or on a kernel that cannot say whether this is
    // one (before 5.8), which a rescan of a folder asks every time.
    let table = ::std::cell::OnceCell::new();
    let table = || table.get_or_init(|| mounts::read().unwrap_or_default());
    let (mount_root, mount_id) = mount_of(&stat, || mounts::points(table()).get(folder).copied());
    if device == device_of(&parent) && !mount_root {
        return true;
    }
    let table = table();
    let kind = filesystem::classify_with(folder, table);
    // A read-only snapshot is left empty by the walk, so a rescan leaves it too. A subvolume is
    // always a crossing, so this is reached for one.
    // A mount of one of its own ancestors is not walked, so not rescanned either.
    if folder != scan_root && loops_back(folder, device, stat.stx_ino).is_some() {
        return false;
    }
    // (The scan's own root is scanned whatever it is: a snapshot named is a snapshot wanted.)
    if folder != scan_root
        && super::btrfs::snapshot_left_out(options.snapshots, kind.btrfs, stat.stx_ino, || {
            super::btrfs::subvolume::path_is_read_only(folder)
        })
    {
        return false;
    }
    let root_device = device_of(&root);
    let root_space = || {
        filesystem::classify_with(scan_root, table)
            .extent_space
            .unwrap_or(root_device)
    };
    let crossing_allowed = (!options.one_file_system
        || same_filesystem(device, &kind, root_device, root_space()))
        && !kind.pseudo
        && !kind.network;
    let duplicate = mount_root
        && mounts::reached_elsewhere(table, mount_id, folder, scan_root, |path| {
            stat_of(path)
                .is_ok_and(|other| device_of(&other) == device && other.stx_ino == stat.stx_ino)
        });
    crossing_allowed && !duplicate
}

/// Whether a crossing onto `device`, of `kind`, stays on the scan root's filesystem, as `-x` asks:
/// the same device, or on btrfs the same filesystem — every subvolume has a device of its own,
/// and a Synology share is one, so by device alone `-x` would leave out every share of the
/// volume it was asked to scan. `root_space` is the root's [`filesystem::Kind::extent_space`],
/// which on btrfs is the filesystem's UUID (and a device number elsewhere, which no UUID hash
/// will match).
pub(super) fn same_filesystem(
    device: u64,
    kind: &filesystem::Kind,
    scan_device: u64,
    root_space: u64,
) -> bool {
    device == scan_device || (kind.btrfs && kind.extent_space == Some(root_space))
}

/// The ancestor of `path` that is the very directory `(device, inode)` names, if one is: `path`
/// is then a mount of that directory inside itself, and walked it holds itself again. Every
/// ancestor up to `/` is asked, not only those in the scan: a mount of a folder above the scan
/// loops as surely, and the duplicate check (`mounts::reached_elsewhere`) looks only inside it.
/// A folder mounted onto itself, as Synology mounts `/volume1/@docker`, is not one: it is the
/// same place, not an ancestor.
pub(super) fn loops_back(path: &Path, device: u64, inode: u64) -> Option<PathBuf> {
    path.ancestors().skip(1).find_map(|ancestor| {
        let stat = statx(
            rustix::fs::CWD,
            ancestor.as_os_str(),
            AtFlags::NO_AUTOMOUNT,
            StatxFlags::INO,
        )
        .ok()?;
        (device_of(&stat) == device && stat.stx_ino == inode).then(|| ancestor.to_path_buf())
    })
}
