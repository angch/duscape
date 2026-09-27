//! The mount table, for recognising a bind mount: a mount root whose directory the walk reaches by
//! another path as well.
//!
//! `st_dev` cannot say so. A bind mount of a folder has the same device as the folder, so to the
//! walk it looks like any other subdirectory and everything under it is counted a second time —
//! and `-x` does not help, since it never sees a boundary. `du` gets away with it by remembering
//! every directory's inode; a mount root is rare enough to afford the question properly instead.

use ::std::path::{Path, PathBuf};

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub id: u64,
    /// `st_dev` of what is mounted.
    pub device: u64,
    /// Which directory of that filesystem is shown: `/` for a whole filesystem, the source
    /// folder for a bind mount of one.
    pub root: PathBuf,
    /// Where it is shown.
    pub point: PathBuf,
    /// The filesystem type: `ext4`, `fuse.sshfs`.
    pub fstype: String,
    /// The superblock's options: `rw,compress=zstd:3,…`.
    pub options: String,
}

impl Mount {
    /// For FUSE, the subtype: `sshfs` for `fuse.sshfs`.
    pub fn fuse_subtype(&self) -> Option<&str> {
        self.fstype
            .strip_prefix("fuse.")
            .or_else(|| self.fstype.strip_prefix("fuseblk."))
    }
}

pub fn read() -> Option<Vec<Mount>> {
    ::std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .map(|table| parse(&table))
}

/// Parse a `mountinfo` table, in its order, which is the order things were mounted.
pub fn parse(table: &str) -> Vec<Mount> {
    table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let id = fields.next()?.parse().ok()?;
            let _parent = fields.next()?;
            let (major, minor) = fields.next()?.split_once(':')?;
            let device = ::rustix::fs::makedev(major.parse().ok()?, minor.parse().ok()?);
            let root = unescape(fields.next()?);
            let point = unescape(fields.next()?);
            // Optional fields end at a lone `-`: then the type, the source, the options.
            let mut after = fields.skip_while(|field| *field != "-").skip(1);
            let fstype = after.next().unwrap_or_default().to_string();
            let _source = after.next();
            let options = after.next().unwrap_or_default().to_string();
            Some(Mount {
                id,
                device,
                root,
                point,
                fstype,
                options,
            })
        })
        .collect()
}

/// Every mount point in `table` and the id of the mount that shows there: the last one
/// mounted at it, as the table lists them in the order they were mounted.
pub fn points(table: &[Mount]) -> ::std::collections::HashMap<PathBuf, u64> {
    table
        .iter()
        .map(|mount| (mount.point.clone(), mount.id))
        .collect()
}

/// `mountinfo` writes space, tab, newline and backslash in paths as `\040`, `\011`, `\012`
/// and `\134`.
fn unescape(field: &str) -> PathBuf {
    use ::std::os::unix::ffi::OsStringExt;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let octal = &bytes[index + 1..index + 4];
            out.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'));
            index += 4;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    PathBuf::from(::std::ffi::OsString::from_vec(out))
}

/// Whether mount `id`, reached at `at`, shows a directory the walk of `scan_root` reaches by
/// another path: through an *earlier* mount of the same filesystem whose view contains it.
/// The earlier one is kept, so of a folder and its bind mount it is the bind mount that is
/// left out, and of two mounts of one filesystem the second.
///
/// `same_directory(path)` is asked of each candidate path, and must say whether it is the very
/// directory at `at` — which also rules out a source hidden under another mount since.
pub fn reached_elsewhere(
    table: &[Mount],
    id: u64,
    at: &Path,
    scan_root: &Path,
    same_directory: impl Fn(&Path) -> bool,
) -> bool {
    let Some(position) = table.iter().position(|mount| mount.id == id) else {
        return false;
    };
    let this = &table[position];
    table[..position].iter().any(|earlier| {
        if earlier.device != this.device {
            return false;
        }
        let Ok(within) = this.root.strip_prefix(&earlier.root) else {
            return false;
        };
        let there = if within.as_os_str().is_empty() {
            earlier.point.clone()
        } else {
            earlier.point.join(within)
        };
        there.starts_with(scan_root)
            && there != at
            && !there.starts_with(at)
            && same_directory(&there)
    })
}

/// Whether a directory is a mount root, and the id of the mount shown there: `statx`'s own
/// answer from Linux 5.8, else the mount table's (`mounted_at`), since before 5.8 the kernel has
/// neither `STATX_ATTR_MOUNT_ROOT` nor `stx_mnt_id`, and a bind mount of a folder inside the scan
/// would be walked twice.
pub(crate) fn mount_of(
    stat: &::rustix::fs::Statx,
    mounted_at: impl FnOnce() -> Option<u64>,
) -> (bool, u64) {
    let root = ::rustix::fs::StatxAttributes::MOUNT_ROOT;
    if stat.stx_attributes_mask.contains(root) {
        (stat.stx_attributes.contains(root), stat.stx_mnt_id)
    } else {
        mounted_at().map_or((false, 0), |id| (true, id))
    }
}
