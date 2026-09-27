//! Which filesystems hold disk usage worth reporting, and which only look like they do.
//!
//! `/proc` and `/sys` are not small versions of a disk — they are kernel interfaces wearing a
//! directory shape. Walking them costs a million `statx` calls to total zero bytes, and `/proc` is
//! not even bounded: it grows a subtree per process and per thread while the scan runs, and some
//! of its directories block or fail permanently when the process they describe dies mid-walk.

use ::std::path::Path;

/// Magic numbers of filesystems that report no meaningful disk usage.
///
/// `tmpfs` is deliberately absent: `/tmp` and `/dev/shm` hold real files that really occupy
/// memory, and `du` counts them. `devtmpfs` reports the same magic, so `/dev` is walked too —
/// it is small and bounded, unlike the rest of this list.
const PSEUDO: &[u32] = &[
    0x9fa0,      // proc
    0x6265_6572, // sysfs
    0x6462_6720, // debugfs
    0x7472_6163, // tracefs
    0x7363_6673, // securityfs
    0xf97c_ff8c, // selinuxfs
    0x0027_e0eb, // cgroup
    0x6367_7270, // cgroup2
    0xcafe_4a11, // bpf
    0x6165_676c, // pstore
    0x4249_4e4d, // binfmt_misc
    0x1980_0202, // mqueue
    0x6265_6570, // configfs
    0x0000_1cd1, // devpts
    0x6573_5543, // fusectl
    0x6e73_6673, // nsfs — as bind-mounted by `ip netns`; unreachable inside /proc, where the
    //              ns entries are symlinks on procfs itself
    0x9584_58f6, // hugetlbfs
];

// `autofs` is deliberately NOT here, though it is tempting: descending an autofs placeholder
// triggers the real mount, which on a dead NFS server can block. But an autofs mount point
// stands in for a filesystem that may hold a user's actual home directory, and skipping it
// would silently under-report real data that `du` counts. That is a wrong number, which is
// worse than the slow one it would avoid. Every entry above holds no data at all; autofs is a
// different argument and does not belong in the same list. Merely *stating* an autofs
// placeholder does not mount it, because the walk passes `AT_NO_AUTOMOUNT`.

/// Magic numbers of filesystems whose files are on another machine.
///
/// What they hold is not using this disk, so it does not answer where this disk's space went;
/// and walking one costs a round trip per directory, a dead server can hang the scan, and a
/// share holding terabytes swamps the treemap with someone else's files. A scan never crosses
/// into one, whatever `-x` says; naming one as the scan root still scans it, as with `/proc`.
const NETWORK: &[u32] = &[
    0x6969,      // nfs
    0x517b,      // smb (the old smbfs)
    0xff53_4d42, // cifs
    0xfe53_4d42, // smb2/smb3
    0x7375_7245, // coda
    0x5346_414f, // afs (OpenAFS)
    0x6b41_4653, // kafs
    0x00c3_6400, // ceph
    0x0102_1997, // 9p — also WSL2's view of the Windows drives, which are not this disk either
    0x564c,      // ncp
    0x0bd0_0bd0, // lustre
    0x4750_4653, // gpfs
    0x1366_1366, // orangefs
    0x1983_0326, // beegfs
];

/// FUSE, which serves local filesystems (ntfs-3g, exFAT, mergerfs) as well as remote ones, so
/// its subtype decides.
const FUSE_MAGIC: u32 = 0x6573_5546;

/// FUSE filesystems, by the subtype `/proc/self/mountinfo` names (`fuse.sshfs`), that reach
/// another machine or a cloud store.
const NETWORK_FUSE: &[&str] = &[
    "sshfs",
    "rclone",
    "s3fs",
    "gcsfuse",
    "goofys",
    "mountpoint-s3",
    "blobfuse",
    "blobfuse2",
    "juicefs",
    "glusterfs",
    "ceph-fuse",
    "davfs",
    "curlftpfs",
    "gvfsd-fuse",
    "google-drive-ocamlfuse",
    "onedriver",
    "seaweedfs",
    "cvmfs",
    "moosefs",
    "lizardfs",
];

/// The mount `path` is the root of, in `table`, found by mount id so that no path has to be
/// matched. Asked only at a mount point, which is rare.
fn mount_at<'a>(
    path: &Path,
    table: &'a [super::mounts::Mount],
) -> Option<&'a super::mounts::Mount> {
    let stat = super::statx(
        ::rustix::fs::CWD,
        path,
        ::rustix::fs::AtFlags::NO_AUTOMOUNT,
        ::rustix::fs::StatxFlags::MNT_ID,
    )
    .ok()?;
    if stat.stx_mask & ::rustix::fs::StatxFlags::MNT_ID.bits() != 0 {
        return table.iter().find(|mount| mount.id == stat.stx_mnt_id);
    }
    // No mount id before Linux 5.8: the last mount at this point is the one that shows.
    table.iter().rev().find(|mount| mount.point == path)
}

/// Whether the FUSE filesystem mounted at `path` is one of [`NETWORK_FUSE`]: `statfs` says only
/// "FUSE", and the subtype is in the mount table.
fn fuse_is_network(path: &Path, table: &[super::mounts::Mount]) -> bool {
    mount_at(path, table)
        .and_then(super::mounts::Mount::fuse_subtype)
        .is_some_and(|subtype| NETWORK_FUSE.contains(&subtype))
}

/// Magic numbers of filesystems that can share extents between files.
const REFLINK: &[u32] = &[
    0x5846_5342, // xfs
    super::btrfs::MAGIC,
];

/// What the walk needs to know about a filesystem, from one `statfs`.
#[derive(Clone, Copy, Debug)]
pub struct Kind {
    /// Reports no meaningful disk usage; not worth descending into.
    pub pseudo: bool,
    /// On another machine: not this disk's space, and not worth descending into.
    pub network: bool,
    /// Can share extents between files, so reflinks are worth looking for.
    pub reflinks: bool,
    /// Names the address space extent offsets index, when it is not just the device; see
    /// [`super::btrfs::fsid`].
    pub extent_space: Option<u64>,
    /// Which files to ask what their extents occupy after compression (btrfs, as root).
    pub compressed_sizes: Compressed,
    /// ext2, ext3 or ext4: a directory's blocks can be found with FIEMAP and read ahead
    /// through the device, given the right to open it. See [`super::dirblocks`].
    pub ext: bool,
    /// btrfs: what is reached here may be a subvolume, and a read-only one a snapshot.
    pub btrfs: bool,
}

/// Which files of a filesystem may be compressed, and so are worth reading the extents of.
///
/// Reading them costs about a microsecond a file — several seconds on a whole btrfs root —
/// so it is paid only where compression can be: on a filesystem mounted with `compress` or
/// `compress-force`, every file; elsewhere on btrfs, the files marked for it (`chattr +c`,
/// which `statx` reports). What was written compressed under an earlier mount option, on a
/// volume since mounted without it, is not found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compressed {
    /// Not btrfs, or not root: `st_blocks` it is.
    Never,
    /// Files `statx` says are compressed.
    Marked,
    /// Every file.
    Maybe,
}

/// Whether the filesystem mounted at `path` has compression on for everything written: the
/// `compress` or `compress-force` superblock option.
fn mounted_compressed(path: &Path, table: &[super::mounts::Mount]) -> bool {
    mount_at(path, table).is_some_and(|mount| {
        mount
            .options
            .split(',')
            .any(|option| option.starts_with("compress") && !option.ends_with("=no"))
    })
}

/// ext2, ext3 and ext4 all report `EXT4_SUPER_MAGIC`.
const EXT_MAGIC: u32 = 0xEF53;

/// Classify the filesystem `path` sits on.
///
/// Asked once per mount point crossed, never per entry. A filesystem that cannot be asked is
/// treated as ordinary and non-sharing, which descends into it but does not open its files.
/// Reads the mount table afresh; the walk uses [`classify_with`] and its own copy.
pub fn classify(path: &Path) -> Kind {
    classify_with(path, &super::mounts::read().unwrap_or_default())
}

/// [`classify`], with the mount table already read.
pub fn classify_with(path: &Path, table: &[super::mounts::Mount]) -> Kind {
    ::rustix::fs::statfs(path).map_or(
        Kind {
            pseudo: false,
            network: false,
            reflinks: false,
            extent_space: None,
            compressed_sizes: Compressed::Never,
            ext: false,
            btrfs: false,
        },
        |fs| {
            let magic = magic_of(&fs);
            Kind {
                pseudo: PSEUDO.contains(&magic),
                network: NETWORK.contains(&magic)
                    || (magic == FUSE_MAGIC && fuse_is_network(path, table)),
                reflinks: REFLINK.contains(&magic),
                extent_space: (magic == super::btrfs::MAGIC)
                    .then(|| super::btrfs::fsid(path))
                    .flatten(),
                compressed_sizes: if magic != super::btrfs::MAGIC
                    || !super::btrfs::extents::allowed(path)
                {
                    Compressed::Never
                } else if mounted_compressed(path, table) {
                    Compressed::Maybe
                } else {
                    Compressed::Marked
                },
                ext: magic == EXT_MAGIC,
                btrfs: magic == super::btrfs::MAGIC,
            }
        },
    )
}

/// A filesystem's magic number as an unsigned 32-bit value.
///
/// `f_type` is a *signed* `__fsword_t`, 32 bits wide on i686 and armv7. Widening it would
/// sign-extend, and every magic with the top bit set — btrfs, selinuxfs, bpf, hugetlbfs —
/// would then never match. Truncating is lossless: the magics are 32-bit constants.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn magic_of(fs: &::rustix::fs::StatFs) -> u32 {
    fs.f_type as u32
}
