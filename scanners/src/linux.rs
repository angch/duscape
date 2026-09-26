//! Native Linux directory walk: `getdents64` for names, `statx` for sizes.
//!
//! Replaces the `dua-core` walk on Linux. The reason is not the syscalls — measured, no portable
//! change to *what* is asked per entry makes it cheaper (see `docs/scan-performance.md`) — but the
//! thread model. `dua-core`'s walk peaks at six workers on a 32-core machine and is three times
//! slower by sixteen, while sixteen independent walker processes over the same tree scale
//! linearly. The kernel is not the limit, so a walker that keeps its workers busy is worth having.
//!
//! The shape is deliberately plain: one shared queue of directories behind one mutex, N workers,
//! results to the consumer over a bounded channel. A directory is read by exactly one worker and
//! leaves as exactly one [`DirEntries`], so the tree builder resolves each parent once. Within a
//! directory the names are listed first and the stats made after, in inode order, and a directory
//! of thousands has its stats shared over helper threads: both for the cold cache, where a stat
//! of an inode not in core is a disk read (see `docs/scan-performance.md`, "Cold cache").

use ::std::ffi::OsStr;
use ::std::mem::MaybeUninit;
use ::std::os::fd::{AsFd, BorrowedFd};
use ::std::os::unix::ffi::OsStrExt;
use ::std::path::{Path, PathBuf};
use ::std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use ::std::sync::{Arc, Condvar, Mutex};
use ::std::thread::JoinHandle;

use ::rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, StatxFlags, openat, statx};

use super::{DirEntries, EntryMeta, ScanOptions};

/// One directory waiting to be read.
struct Job {
    path: Arc<Path>,
    /// Depth below the scan root, which is depth 0.
    depth: usize,
    /// The filesystem this directory lives on. An entry inside it reporting a different device is
    /// a mount point, which is the only place the walk has to decide whether to cross.
    device: u64,
    /// Whether *this* filesystem can share extents between files, decided once when the walk
    /// entered it. Not a property of the scan root: on btrfs every subvolume and snapshot is its
    /// own `st_dev`, and they are precisely where the sharing is.
    reflinks: bool,
    /// What this filesystem's physical extent offsets are offsets *into*, for telling apart the
    /// same offset on two filesystems. The device, except on btrfs: there every subvolume and
    /// snapshot has a device of its own over one shared address space, and a snapshot's files
    /// are the live ones' extents under another `st_dev`.
    extent_space: u64,
    /// Whether this filesystem will say what a file's extents occupy after compression: btrfs,
    /// to a caller with `CAP_SYS_ADMIN`. See [`btrfs_extents`].
    compressed_sizes: filesystem::Compressed,
    /// The block device under this filesystem, open for reading, where the caller may (root):
    /// the directories' blocks are read ahead through it. See [`dirblocks`].
    dirblocks: Option<Arc<dirblocks::Device>>,
}

struct Shared {
    /// Directories no worker has claimed. Workers keep most of their own findings on a local
    /// stack and only publish here when nobody else has anything to do, so this lock is taken a
    /// few thousand times rather than once per directory.
    jobs: Mutex<Vec<Job>>,
    ready: Condvar,
    /// `jobs.len()`, readable without taking the lock, to decide whether donating is worthwhile.
    queued: AtomicUsize,
    /// Directories discovered but not yet finished, wherever they are currently held. The walk is
    /// over exactly when this reaches zero, which a worker looking at an empty queue cannot tell
    /// from "the others have not published yet".
    pending: AtomicUsize,
    /// Set when the consumer goes away, so workers stop rather than walk the rest of the tree.
    ///
    /// Draining the channel on drop is not enough on its own: draining makes every send succeed,
    /// so the workers happily finish the whole scan while the consumer waits to join them.
    stop: AtomicBool,
    /// The scan root, for deciding whether a bind mount's source is inside the scan.
    root: Arc<Path>,
    /// How many workers the walk was given: the most helpers one directory's stats are shared
    /// over, so that a huge directory does not fan out past what the machine was asked to give.
    threads: usize,
    /// The mount table, read the first time the walk meets a mount root, which most walks of a
    /// home directory never do.
    mounts: ::std::sync::OnceLock<Vec<mounts::Mount>>,
    /// The block devices opened for [`dirblocks`], by `st_dev`; `None` where one could not be.
    devices: Mutex<::std::collections::HashMap<u64, Option<Arc<dirblocks::Device>>>>,
}

impl Shared {
    /// The mount table, read once per walk, the first time it is needed.
    fn mount_table(&self) -> &[mounts::Mount] {
        self.mounts
            .get_or_init(|| mounts::read().unwrap_or_default())
    }

    /// The block device under `device`, opened the first time it is asked for.
    fn device_for(&self, device: u64) -> Option<Arc<dirblocks::Device>> {
        self.devices
            .lock()
            .expect("device table poisoned")
            .entry(device)
            .or_insert_with(|| dirblocks::Device::open(device).map(Arc::new))
            .clone()
    }

    /// Wait for a directory published by another worker, or for the walk to end.
    fn steal(&self) -> Option<Job> {
        let mut jobs = self.jobs.lock().expect("scan queue poisoned");
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(job) = jobs.pop() {
                self.queued.store(jobs.len(), Ordering::Relaxed);
                return Some(job);
            }
            if self.pending.load(Ordering::Acquire) == 0 {
                return None;
            }
            jobs = self.ready.wait(jobs).expect("scan queue poisoned");
        }
    }

    /// Publish half of a worker's local stack so idle workers have something to take.
    fn donate(&self, local: &mut Vec<Job>) {
        let donated = local.len() / 2;
        let mut jobs = self.jobs.lock().expect("scan queue poisoned");
        // The oldest half goes: those are the shallowest, and so the most likely to have deep
        // subtrees of their own to spread around in turn.
        jobs.extend(local.drain(..donated));
        self.queued.store(jobs.len(), Ordering::Relaxed);
        self.ready.notify_all();
    }

    /// Retire `count` directories that will not be walked after all, waking everyone if that was
    /// the last outstanding work.
    fn retire(&self, count: usize) {
        if count == 0 {
            return;
        }
        if self.pending.fetch_sub(count, Ordering::AcqRel) == count {
            // Taking the lock before signalling is what makes this safe against a worker that has
            // just read `pending` as non-zero and is about to wait: it still holds the lock, so
            // this blocks until it has released it inside `wait`.
            let _jobs = self.jobs.lock().expect("scan queue poisoned");
            self.ready.notify_all();
        }
    }
}

/// Attributes worth asking `statx` for. Asking for less does not make XFS or ext4 do less, but
/// asking for more would oblige some filesystems to, so the set is the model's needs exactly.
const WANTED: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::MODE)
    .union(StatxFlags::INO)
    .union(StatxFlags::NLINK)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::BLOCKS)
    // Free with the rest, and only looked at on a mount root: see `mounts`.
    .union(StatxFlags::MNT_ID);

/// Read one directory, returning its entries and the subdirectories to descend into.
/// What one entry came to.
struct Inspected {
    meta: EntryMeta,
    child: Option<Job>,
    later: bool,
}

/// One entry of a listing: `statx` and everything decided from it.
#[allow(clippy::too_many_lines)] // quality debt: the Linux walker's directory read
fn inspect(
    dir: BorrowedFd<'_>,
    name: &::std::ffi::CStr,
    job: &Job,
    options: &ScanOptions,
    scan_device: u64,
    shared: &Shared,
    descend: bool,
) -> Option<Inspected> {
    // NO_AUTOMOUNT matters as much as SYMLINK_NOFOLLOW here. `stat`, `lstat` and `fstatat` all
    // behave as though it were set; bare `statx` does not, and the man page names this exact
    // case: "can be used in tools that scan directories to prevent mass-automounting of a
    // directory of automount points". Without it, merely *looking at* an autofs placeholder
    // mounts it, so a directory of NFS home maps would be mounted wholesale and one dead
    // server would hang the scan. This fires before the descent decision, so the care taken
    // over autofs in `filesystem` would not have covered it.
    let stat = statx(
        dir,
        name,
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        WANTED,
    )
    .ok()?;

    let kind = FileType::from_raw_mode(u32::from(stat.stx_mode));
    let is_dir = kind == FileType::Directory;
    let allocated = stat.stx_blocks.saturating_mul(512);
    // `stx_blocks` on btrfs is the size before compression. Where the extents can be read,
    // what they occupy is the size on disk. A single block cannot be made smaller, so a file
    // of one block (or an inline one) is not asked about.
    let compressed = match job.compressed_sizes {
        filesystem::Compressed::Never => false,
        filesystem::Compressed::Marked => stat
            .stx_attributes
            .contains(::rustix::fs::StatxAttributes::COMPRESSED),
        filesystem::Compressed::Maybe => true,
    };
    let on_disk = if compressed && kind == FileType::RegularFile && allocated > 4096 {
        btrfs_extents::on_disk(dir, stat.stx_ino).unwrap_or(allocated)
    } else {
        allocated
    };

    let device = device_of(&stat);
    // Only where the filesystem this directory sits on was found to support sharing at all —
    // not merely where it is the scan root's. Probing means *opening* the file, so an NFS, SMB
    // or FUSE mount reached part-way through a scan must not be probed: those are excluded by
    // magic number rather than by device, which also lets btrfs subvolumes and a second XFS
    // volume be probed, and they are exactly where the sharing lives.
    let may_share = job.reflinks && device == job.device && kind == FileType::RegularFile;
    let shared_extent = if may_share && allocated >= reflink::PROBE_ABOVE_BYTES {
        reflink::shared_identity(dir, name, job.extent_space).unwrap_or(0)
    } else {
        0
    };
    // Too small to probe now, but not too small to matter across a snapshot: left for the
    // second pass (`refine`). A hard-linked one is already held once by its inode, and one of
    // 2 KiB or less is likely inline on btrfs, where there is no extent to share.
    let later = may_share
        && (super::refine::REFINE_ABOVE_BYTES..reflink::PROBE_ABOVE_BYTES).contains(&allocated)
        && stat.stx_size > 2048
        && stat.stx_nlink == 1;

    let mut child = None;
    if is_dir && descend {
        let path = job.path.join(OsStr::from_bytes(name.to_bytes()));
        // A mount root showing a directory the walk reaches anyway — a bind mount of a folder
        // inside the scan, or a second mount of a filesystem already in it — is left empty:
        // same `st_dev`, often, so nothing below would notice the files were seen already.
        let mount_root = stat
            .stx_attributes_mask
            .contains(::rustix::fs::StatxAttributes::MOUNT_ROOT)
            && stat
                .stx_attributes
                .contains(::rustix::fs::StatxAttributes::MOUNT_ROOT);
        let duplicate = mount_root && {
            mounts::reached_elsewhere(
                shared.mount_table(),
                stat.stx_mnt_id,
                &path,
                &shared.root,
                |path| {
                    statx(
                        rustix::fs::CWD,
                        path.as_os_str(),
                        AtFlags::NO_AUTOMOUNT,
                        StatxFlags::INO,
                    )
                    .is_ok_and(|other| device_of(&other) == device && other.stx_ino == stat.stx_ino)
                },
            )
        };
        // A different device means this entry is a mount point, and the only point at which
        // the walk has a decision to make. Everything below it is on one filesystem, already
        // judged, so the `statfs` costs one call per mount rather than one per directory.
        let crossing = device != job.device;
        let (allowed, reflinks, extent_space, compressed_sizes, dirblocks) = if crossing {
            let same_filesystem = device == scan_device;
            let kind = filesystem::classify_with(&path, shared.mount_table());
            (
                (!options.one_file_system || same_filesystem) && !kind.pseudo && !kind.network,
                kind.reflinks,
                kind.extent_space.unwrap_or(device),
                kind.compressed_sizes,
                kind.ext.then(|| shared.device_for(device)).flatten(),
            )
        } else {
            (
                true,
                job.reflinks,
                job.extent_space,
                job.compressed_sizes,
                job.dirblocks.clone(),
            )
        };
        if allowed && !duplicate {
            child = Some(Job {
                path: Arc::from(path.as_path()),
                depth: job.depth + 1,
                device,
                reflinks,
                extent_space,
                compressed_sizes,
                dirblocks,
            });
        }
    }

    Some(Inspected {
        meta: EntryMeta {
            size: on_disk,
            apparent: stat.stx_size,
            inode: stat.stx_ino,
            links: u64::from(stat.stx_nlink),
            is_dir,
            shared_extent,
        },
        child,
        later,
    })
}

/// A listed name, before it is looked at: where it sits in the arena, and its inode.
#[derive(Clone, Copy)]
struct Listed {
    ino: u64,
    offset: u32,
    len: u32,
}

/// A directory with at least this many entries has its `statx` calls shared out over helper
/// threads, `STAT_CHUNK` each, rather than made one after another by the worker that listed it.
///
/// On a cold cache every `statx` of an inode not yet in core is a disk read the worker sits
/// through, and one directory of 43,000 entries took 0.40s alone while the rest of the walk's
/// workers had nothing left to do; shared out it took 0.15s. Warm, the calls are a few
/// microseconds each and the threads are not worth spawning below a few thousand of them.
const STAT_SHARED_ABOVE: usize = 2048;
const STAT_CHUNK: usize = 1024;

#[allow(clippy::too_many_lines)] // quality debt: the Linux walker's entry loop
fn read_directory(
    job: &Job,
    options: &ScanOptions,
    scan_device: u64,
    shared: &Shared,
    prefetch: &mut dirblocks::Prefetcher,
) -> (DirEntries, Vec<Job>) {
    let mut directory = DirEntries::new(Arc::clone(&job.path));
    directory.extent_space = job.extent_space;
    let mut children = Vec::new();

    let dir = match openat(
        rustix::fs::CWD,
        job.path.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(dir) => dir,
        Err(_) => {
            directory.failed = 1;
            return (directory, children);
        }
    };

    // Before this directory's blocks are read: ask where they are, and read the run they are in
    // through the device, so that the next directories' blocks are in core when they are opened.
    if let Some(device) = &job.dirblocks
        && let Some(at) = reflink::first_extent(dir.as_fd())
    {
        prefetch.ahead(device, at);
    }

    let descend = options.max_depth.is_none_or(|max| job.depth + 1 < max);
    let mut buffer = [MaybeUninit::<u8>::uninit(); 64 * 1024];
    let mut reader = RawDir::new(&dir, &mut buffer);

    // The whole listing first, then the stats, in inode order. ext4 and XFS hand back a
    // directory in name-hash order, which is uncorrelated with where the inodes sit on disk;
    // sorted by `d_ino` — which `getdents64` returns for free — the inode table is read forwards,
    // so the kernel's inode readahead (32 blocks on ext4) is used rather than wasted. Warm it
    // changes nothing; cold it was 8% of a scan of 375k entries on an SSD, and a spinning disk
    // has far more to gain from a seek order than that. The names are copied into one arena
    // with their NULs, since `RawDir` reuses its buffer.
    let mut arena: Vec<u8> = Vec::new();
    let mut listed: Vec<Listed> = Vec::new();
    while let Some(entry) = reader.next() {
        let entry = match entry {
            Ok(entry) => entry,
            // A signal arriving mid-call is the one readdir error that is not about the directory.
            // It matters here: the TUI resizes on `SIGWINCH` while a scan is running, and treating
            // that as a dead directory would silently drop the rest of it and its whole subtree.
            Err(::rustix::io::Errno::INTR) => continue,
            Err(_) => {
                // Otherwise stop reading this directory rather than asking again. A `getdents64`
                // error is a property of the descriptor, not of one entry, so it is still there on
                // the next call: `/proc/<pid>/net` for a process that has since died returns
                // `EINVAL` every time, and retrying it is an infinite loop. `std`'s `ReadDir` ends
                // on the first error too.
                directory.failed += 1;
                break;
            }
        };
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let bytes = name.to_bytes_with_nul();
        listed.push(Listed {
            ino: entry.ino(),
            offset: u32::try_from(arena.len()).expect("a directory's names fit in 4 GiB"),
            len: u32::try_from(bytes.len()).expect("a name fits in 4 GiB"),
        });
        arena.extend_from_slice(bytes);
    }
    listed.sort_unstable_by_key(|l| l.ino);

    let name_of = |l: &Listed| -> &::std::ffi::CStr {
        let start = l.offset as usize;
        ::std::ffi::CStr::from_bytes_with_nul(&arena[start..start + l.len as usize])
            .expect("stored with its NUL")
    };
    let look = |l: &Listed| {
        inspect(
            dir.as_fd(),
            name_of(l),
            job,
            options,
            scan_device,
            shared,
            descend,
        )
    };
    let mut keep = |directory: &mut DirEntries, l: &Listed, found: Option<Inspected>| {
        let Some(found) = found else {
            // A file that vanished between the readdir and the stat, or one whose parent we may
            // list but not interrogate. Counted, not guessed at.
            directory.failed += 1;
            return;
        };
        if let Some(child) = found.child {
            children.push(child);
        }
        if found.later {
            directory
                .later
                .push(u32::try_from(directory.len()).unwrap_or(u32::MAX));
        }
        directory.push(OsStr::from_bytes(name_of(l).to_bytes()), found.meta);
    };

    let helpers = (listed.len() / STAT_CHUNK).min(shared.threads);
    if listed.len() >= STAT_SHARED_ABOVE && helpers > 1 {
        // This worker takes the first chunk itself; the helpers take the rest, and hand their
        // findings back in listing order so the packed directory comes out the same either way.
        let chunk = listed.len().div_ceil(helpers);
        let found: Vec<Vec<Option<Inspected>>> = ::std::thread::scope(|scope| {
            let mut chunks = listed.chunks(chunk);
            let first = chunks.next().unwrap_or(&[]);
            let handles: Vec<_> = chunks
                .map(|part| scope.spawn(move || part.iter().map(look).collect::<Vec<_>>()))
                .collect();
            let mut all = vec![first.iter().map(look).collect::<Vec<_>>()];
            all.extend(handles.into_iter().map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| ::std::panic::resume_unwind(panic))
            }));
            all
        });
        for (l, found) in listed.iter().zip(found.into_iter().flatten()) {
            keep(&mut directory, l, found);
        }
    } else {
        for l in &listed {
            keep(&mut directory, l, look(l));
        }
    }

    directory.shrink();
    (directory, children)
}

pub(crate) use reflink::shared_identity;

/// Reading the directories' blocks ahead, through the block device. Root, in practice.
///
/// A cold walk pays one synchronous 4 KiB read per directory for its data block, inside
/// `getdents64`, with no readahead across directories: 72% of every read a cold ext4 scan makes
/// (see `docs/scan-performance.md`, "Cold cache"). ext4 puts a directory's blocks in its inode's
/// block group and the walk reads children smallest inode first, so those blocks come in
/// contiguous runs of five to nine. Where the device can be opened for reading — root, or the
/// `disk` group — one `posix_fadvise(WILLNEED)` on it from the first block of a run fetches the
/// run in one read; the buffer cache ext4 reads directories from is the device's page cache, so
/// the siblings' blocks are then in core. Where it cannot be opened, nothing changes: the walk
/// is exactly what an unprivileged one is.
///
/// `DISKONAUT_DIRBLOCKS_DEVICE=<path>` names the file to advise instead of the device, for
/// exercising this path without the device: on a regular file the advice is harmless.
mod dirblocks {
    use ::std::os::fd::{AsRawFd, OwnedFd};

    use ::rustix::fs::{Mode, OFlags, open};

    /// How much is read ahead from a run's first block. Runs average five to nine 4 KiB blocks;
    /// what is read past the run is bandwidth spent for nothing, and a run longer than this is
    /// read in two.
    const WINDOW: u64 = 32 * 1024;

    /// A block device open for reading.
    pub struct Device {
        fd: OwnedFd,
    }

    impl Device {
        /// The device `st_dev` names, if it can be opened for reading.
        pub fn open(device: u64) -> Option<Self> {
            if let Ok(path) = ::std::env::var("DISKONAUT_DIRBLOCKS_DEVICE") {
                let fd = open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).ok()?;
                return Some(Self { fd });
            }
            #[allow(clippy::cast_possible_truncation)]
            let (major, minor) = (
                libc::major(device as libc::dev_t),
                libc::minor(device as libc::dev_t),
            );
            let uevent =
                ::std::fs::read_to_string(format!("/sys/dev/block/{major}:{minor}/uevent")).ok()?;
            let name = uevent
                .lines()
                .find_map(|line| line.strip_prefix("DEVNAME="))?;
            let fd = open(
                format!("/dev/{name}"),
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .ok()?;
            // The device the filesystem is on, and nothing else, whatever `/dev` says.
            let stat = ::rustix::fs::fstat(&fd).ok()?;
            // `st_rdev` is not the same width on every libc.
            #[allow(clippy::unnecessary_cast)]
            let rdev = stat.st_rdev as u64;
            (rdev == device).then_some(Self { fd })
        }
    }

    /// One worker's view of what has been asked for: the end of the last window, so that the
    /// directories inside it are not asked for again. Workers walk different subtrees, so each
    /// keeps its own.
    #[derive(Default)]
    pub struct Prefetcher {
        /// Where the last window started and ended.
        window: Option<(u64, u64)>,
    }

    impl Prefetcher {
        /// A directory's first block is at byte `at`: read the window from there, unless the
        /// last one covers it.
        pub fn ahead(&mut self, device: &Device, at: u64) {
            if let Some((start, end)) = self.window
                && (start..end).contains(&at)
            {
                return;
            }
            #[allow(clippy::cast_possible_wrap)]
            // SAFETY: `posix_fadvise` takes a descriptor and three integers, and `fd` is open.
            unsafe {
                libc::posix_fadvise(
                    device.fd.as_raw_fd(),
                    at as libc::off_t,
                    WINDOW as libc::off_t,
                    libc::POSIX_FADV_WILLNEED,
                );
            }
            self.window = Some((at, at.saturating_add(WINDOW)));
        }
    }
}

/// Copy-on-write sharing, asked about one file at a time.
///
/// There is no bulk answer available to an ordinary user: `XFS_IOC_GETFSMAP` would give the whole
/// volume's extent ownership in one sweep, but the kernel redacts every owner for callers without
/// `CAP_SYS_ADMIN`, and `XFS_IOC_BULKSTAT` is refused outright. `FS_IOC_FIEMAP` needs the file
/// open, so the cost is an `openat` and an `ioctl` per file asked about — which is why only files
/// big enough to matter are asked about at all.
mod reflink {
    use ::std::ffi::CStr;
    use ::std::os::fd::{AsRawFd, BorrowedFd};

    use ::rustix::fs::{Mode, OFlags, openat};

    /// This extent is shared with another file.
    const FIEMAP_EXTENT_SHARED: u32 = 0x0000_2000;
    /// The last extent of the file; there is nothing beyond it.
    const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
    /// Extents asked for in one go; a longer map is read a page at a time.
    const MAX_EXTENTS: usize = 64;
    /// Extents read before giving up on a file, a page of [`MAX_EXTENTS`] at a time: a 32 GiB
    /// file compressed in 128 KiB extents.
    const MAX_EXTENTS_IN_ALL: usize = 1 << 18;
    /// `_IOWR('f', 11, struct fiemap)`, where `struct fiemap` is 32 bytes. `ioctl`'s request type is
    /// `c_ulong` on glibc but `c_int` on musl, so the bits are cast into whichever it is.
    const FS_IOC_FIEMAP: libc::Ioctl = 0xC020_660B_u32 as libc::Ioctl;

    /// Below this, a file is not worth an `openat` and an `ioctl` to ask about.
    ///
    /// Reflinks of small files exist, but they cost the same two syscalls to find as a large one
    /// and are worth a rounding error of the total. On a 4.2M-entry volume this leaves under 4% of
    /// files to probe. The threshold is on blocks allocated, so a sparse file that is mostly hole
    /// is judged on what it actually occupies.
    pub const PROBE_ABOVE_BYTES: u64 = 64 * 1024;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }

    #[repr(C)]
    struct Request {
        start: u64,
        length: u64,
        flags: u32,
        mapped_extents: u32,
        extent_count: u32,
        reserved: u32,
        extents: [Extent; MAX_EXTENTS],
    }

    /// Where the first extent of the open file or directory `fd` is, as a byte offset into the
    /// device, if it has one. Inline data (ext4 keeps a very small directory in its inode) has no
    /// extent. Asks for one extent, so the cost is the `ioctl` alone.
    pub fn first_extent(fd: BorrowedFd<'_>) -> Option<u64> {
        let mut request = Request {
            start: 0,
            length: u64::MAX,
            flags: 0,
            mapped_extents: 0,
            extent_count: 1,
            reserved: 0,
            extents: [Extent::default(); MAX_EXTENTS],
        };
        // SAFETY: `request` is a live, correctly shaped `struct fiemap` with room for the extent
        // it promises, and `fd` is open for the duration of the call.
        let result = unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_FIEMAP, &raw mut request) };
        (result == 0 && request.mapped_extents >= 1).then_some(request.extents[0].physical)
    }

    /// An identity for `name`'s blocks, if every one of them is shared with another file.
    ///
    /// The first extent alone is not enough, and assuming it was is a way to *understate*. Two
    /// files of equal size that share only their opening extent — a reflink copy with its middle
    /// overwritten, say — would be merged, and one of them counted as nothing. So the whole map is
    /// read and folded into the identity, and anything less than wholly shared is refused:
    ///
    /// * an extent that is not shared means the file is only partly a copy, so it is counted in
    ///   full, which overstates rather than understates
    /// * no `LAST` flag means the file has more extents than were asked for, and what was not seen
    ///   cannot be vouched for
    ///
    /// Two files agreeing on this identity have the same extents at the same places, so they are
    /// the same blocks. The size check in the ledger still applies on top.
    pub fn shared_identity(dir: BorrowedFd<'_>, name: &CStr, extent_space: u64) -> Option<u64> {
        let file = openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()?;

        // FNV-1a over the filesystem and then the extent map. The filesystem has to be in there:
        // a physical block offset means nothing without the address space it indexes, and the walk
        // probes every sharing filesystem it meets rather than only the scan root's. Two unrelated
        // files on two volumes sharing an offset and a size is otherwise an easy collision, and it
        // would merge them — the same undercount that keying on one extent used to cause. It is
        // the filesystem and not the device: see `Job::extent_space`.
        let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fold = |value: u64| {
            identity = (identity ^ value).wrapping_mul(0x0000_0100_0000_01b3);
        };
        fold(extent_space);

        // A page of extents at a time, until the last. btrfs compresses in 128 KiB extents, so a
        // compressed file has one per 128 KiB of it: stopping at the first page would leave every
        // compressed file over 8 MiB counted once per snapshot.
        let mut start = 0u64;
        let mut seen = 0usize;
        loop {
            let mut request = Request {
                start,
                length: u64::MAX - start,
                flags: 0,
                mapped_extents: 0,
                extent_count: MAX_EXTENTS as u32,
                reserved: 0,
                extents: [Extent::default(); MAX_EXTENTS],
            };
            // SAFETY: `request` is a live, correctly shaped `struct fiemap` with room for the
            // `extent_count` extents it promises, and `file` is open for the duration of the call.
            let result = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &raw mut request) };
            let mapped = request.mapped_extents as usize;
            if result != 0 || mapped == 0 || mapped > MAX_EXTENTS {
                return None;
            }
            let mut saw_last = false;
            for extent in &request.extents[..mapped] {
                if extent.flags & FIEMAP_EXTENT_SHARED == 0 {
                    return None;
                }
                fold(extent.physical);
                fold(extent.length);
                saw_last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
            }
            if saw_last {
                break;
            }
            seen += mapped;
            let last = &request.extents[mapped - 1];
            let next = last.logical.saturating_add(last.length);
            // What was not seen cannot be vouched for: a file past the cap, or a map that does not
            // move forward, is counted in full.
            if seen >= MAX_EXTENTS_IN_ALL || next <= start {
                return None;
            }
            start = next;
        }

        // Zero is how `EntryMeta` spells "not shared", so it cannot also mean an identity.
        Some(if identity == 0 { 1 } else { identity })
    }
}

/// Which filesystems hold disk usage worth reporting, and which only look like they do.
///
/// `/proc` and `/sys` are not small versions of a disk — they are kernel interfaces wearing a
/// directory shape. Walking them costs a million `statx` calls to total zero bytes, and `/proc` is
/// not even bounded: it grows a subtree per process and per thread while the scan runs, and some
/// of its directories block or fail permanently when the process they describe dies mid-walk.
pub(crate) mod filesystem {
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
        let stat = ::rustix::fs::statx(
            ::rustix::fs::CWD,
            path,
            ::rustix::fs::AtFlags::NO_AUTOMOUNT,
            ::rustix::fs::StatxFlags::MNT_ID,
        )
        .ok()?;
        table.iter().find(|mount| mount.id == stat.stx_mnt_id)
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
        BTRFS_MAGIC,
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
        /// [`btrfs_fsid`].
        pub extent_space: Option<u64>,
        /// Which files to ask what their extents occupy after compression (btrfs, as root).
        pub compressed_sizes: Compressed,
        /// ext2, ext3 or ext4: a directory's blocks can be found with FIEMAP and read ahead
        /// through the device, given the right to open it. See [`super::dirblocks`].
        pub ext: bool,
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

    const BTRFS_MAGIC: u32 = 0x9123_683e;
    /// ext2, ext3 and ext4 all report `EXT4_SUPER_MAGIC`.
    const EXT_MAGIC: u32 = 0xEF53;

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
    fn btrfs_fsid(path: &Path) -> Option<u64> {
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
            },
            |fs| {
                let magic = magic_of(&fs);
                Kind {
                    pseudo: PSEUDO.contains(&magic),
                    network: NETWORK.contains(&magic)
                        || (magic == FUSE_MAGIC && fuse_is_network(path, table)),
                    reflinks: REFLINK.contains(&magic),
                    extent_space: (magic == BTRFS_MAGIC).then(|| btrfs_fsid(path)).flatten(),
                    compressed_sizes: if magic != BTRFS_MAGIC
                        || !super::btrfs_extents::allowed(path)
                    {
                        Compressed::Never
                    } else if mounted_compressed(path, table) {
                        Compressed::Maybe
                    } else {
                        Compressed::Marked
                    },
                    ext: magic == EXT_MAGIC,
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
}

/// What a btrfs file's extents occupy on disk, compression and holes accounted for.
///
/// `stat` cannot say: btrfs reports `st_blocks` as the uncompressed size, so 64 MiB of text under
/// `compress-force=zstd` that holds 2 MiB of data reads as 64 MiB. The file-extent items in the
/// filesystem tree carry the answer — `disk_num_bytes` for what an extent takes, `num_bytes` for
/// how much of it this file refers to — and `BTRFS_IOC_TREE_SEARCH_V2` reads them, one call per
/// file on the directory's descriptor, without opening the file. It needs `CAP_SYS_ADMIN`, so an
/// ordinary user's scan keeps `st_blocks`; this is what `compsize` reads too.
pub(crate) mod btrfs_extents {
    use ::std::os::fd::{AsRawFd, BorrowedFd};
    use ::std::path::Path;

    /// `_IOWR(0x94, 17, struct btrfs_ioctl_search_args_v2)`, a 112-byte header before the buffer.
    const BTRFS_IOC_TREE_SEARCH_V2: libc::Ioctl = 0xc070_9411_u32 as libc::Ioctl;
    const EXTENT_DATA_KEY: u32 = 108;
    /// `struct btrfs_ioctl_search_key`, then `buf_size`.
    const ARGS: usize = 112;
    const HEADER: usize = 32;
    /// Bytes of a file-extent item before `disk_bytenr`: all an inline extent has before its data.
    const EXTENT_HEAD: usize = 21;
    /// A regular or preallocated extent item: the head and four `u64`s.
    const EXTENT_ITEM: usize = EXTENT_HEAD + 32;
    const BUFFER: usize = 16 * 1024;

    /// Whether this caller may search the tree `path` is on: `CAP_SYS_ADMIN`, in practice root.
    pub fn allowed(path: &Path) -> bool {
        use ::rustix::fs::{Mode, OFlags, open};
        let Ok(dir) = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) else {
            return false;
        };
        let mut args = Args::new(0, 0);
        args.set_items(1);
        // SAFETY: `args` is laid out as the kernel expects and holds `BUFFER` bytes after it.
        unsafe {
            libc::ioctl(
                dir.as_raw_fd(),
                BTRFS_IOC_TREE_SEARCH_V2,
                args.bytes.as_mut_ptr(),
            ) == 0
        }
    }

    /// What the extents of inode `inode`, in the subvolume `dir` is in, occupy on disk; `None` if
    /// they cannot be read.
    pub fn on_disk(dir: BorrowedFd<'_>, inode: u64) -> Option<u64> {
        // One buffer per walker thread, reused for every file: this runs once a file.
        thread_local! {
            static ARGS_BUFFER: ::std::cell::RefCell<Args> =
                ::std::cell::RefCell::new(Args::new(0, 0));
        }
        ARGS_BUFFER.with(|args| on_disk_with(dir, inode, &mut args.borrow_mut()))
    }

    fn on_disk_with(dir: BorrowedFd<'_>, inode: u64, args: &mut Args) -> Option<u64> {
        let mut total = 0u64;
        let mut from = 0u64;
        loop {
            args.ask(inode, from);
            // SAFETY: `args` is laid out as the kernel expects and holds `BUFFER` bytes after it.
            let result = unsafe {
                libc::ioctl(
                    dir.as_raw_fd(),
                    BTRFS_IOC_TREE_SEARCH_V2,
                    args.bytes.as_mut_ptr(),
                )
            };
            if result != 0 {
                return None;
            }
            let parsed = parse(&args.bytes[ARGS..], args.items())?;
            total = total.saturating_add(parsed.bytes);
            // The kernel stops when the items run out or the buffer fills. Room left for another
            // item means they ran out — most files, answered in one call.
            let room_for_more = parsed.used + HEADER + EXTENT_ITEM <= BUFFER;
            match parsed.last {
                Some(offset) if offset < u64::MAX && !room_for_more => from = offset + 1,
                _ => return Some(total),
            }
        }
    }

    /// What one search returned: what its extents occupy, the file offset of the last one (to
    /// continue from), and how much of the buffer the items filled.
    pub(crate) struct Parsed {
        pub bytes: u64,
        pub last: Option<u64>,
        pub used: usize,
    }

    /// Sum what the extent items in `buffer` occupy.
    pub(crate) fn parse(buffer: &[u8], items: u32) -> Option<Parsed> {
        let u64_at = |at: usize| -> Option<u64> {
            Some(u64::from_ne_bytes(buffer.get(at..at + 8)?.try_into().ok()?))
        };
        let mut at = 0usize;
        let mut total = 0u64;
        let mut last = None;
        for _ in 0..items {
            let offset = u64_at(at + 16)?;
            let len = u32::from_ne_bytes(buffer.get(at + 28..at + 32)?.try_into().ok()?) as usize;
            let item = buffer.get(at + HEADER..at + HEADER + len)?;
            at += HEADER + len;
            last = Some(offset);
            let ram_bytes = u64::from_ne_bytes(item.get(8..16)?.try_into().ok()?);
            let compression = *item.get(16)?;
            let kind = *item.get(20)?;
            if kind == 0 {
                // Inline: the data is in the item itself, compressed or not.
                total = total.saturating_add((len - EXTENT_HEAD.min(len)) as u64);
                continue;
            }
            let field = |index: usize| -> Option<u64> {
                Some(u64::from_ne_bytes(
                    item.get(EXTENT_HEAD + index * 8..EXTENT_HEAD + index * 8 + 8)?
                        .try_into()
                        .ok()?,
                ))
            };
            let (disk_bytenr, disk_num_bytes, num_bytes) = (field(0)?, field(1)?, field(3)?);
            if disk_bytenr == 0 {
                continue; // a hole
            }
            total = total.saturating_add(if compression == 0 || ram_bytes == 0 {
                num_bytes
            } else {
                // A compressed extent is stored whole; this file's share is the part it refers to.
                (u128::from(disk_num_bytes) * u128::from(num_bytes)).div_ceil(u128::from(ram_bytes))
                    as u64
            });
        }
        Some(Parsed {
            bytes: total,
            last,
            used: at,
        })
    }

    /// `struct btrfs_ioctl_search_args_v2` with its buffer, asking for inode `inode`'s extent
    /// items from file offset `from` on, in the subvolume of the descriptor it is used on.
    struct Args {
        bytes: Vec<u8>,
    }

    impl Args {
        fn new(inode: u64, from: u64) -> Self {
            let mut args = Self {
                bytes: vec![0u8; ARGS + BUFFER],
            };
            args.ask(inode, from);
            args
        }
        /// Set the key to ask for inode `inode`'s extent items from file offset `from` on. Only
        /// the key is rewritten; the next answer overwrites what the last one left in the buffer.
        fn ask(&mut self, inode: u64, from: u64) {
            let bytes = &mut self.bytes;
            let mut put =
                |at: usize, value: u64| bytes[at..at + 8].copy_from_slice(&value.to_ne_bytes());
            put(0, 0); // tree_id: the descriptor's subvolume
            put(8, inode); // min_objectid
            put(16, inode); // max_objectid
            put(24, from); // min_offset
            put(32, u64::MAX); // max_offset
            put(40, 0); // min_transid
            put(48, u64::MAX); // max_transid
            put(104, BUFFER as u64); // buf_size
            bytes[56..60].copy_from_slice(&EXTENT_DATA_KEY.to_ne_bytes()); // min_type
            bytes[60..64].copy_from_slice(&EXTENT_DATA_KEY.to_ne_bytes()); // max_type
            self.set_items(u32::MAX);
        }
        fn set_items(&mut self, items: u32) {
            self.bytes[64..68].copy_from_slice(&items.to_ne_bytes());
        }
        /// How many items the kernel returned.
        fn items(&self) -> u32 {
            u32::from_ne_bytes(self.bytes[64..68].try_into().expect("four bytes"))
        }
    }
}

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
    let mount_root = stat
        .stx_attributes_mask
        .contains(::rustix::fs::StatxAttributes::MOUNT_ROOT)
        && stat
            .stx_attributes
            .contains(::rustix::fs::StatxAttributes::MOUNT_ROOT);
    if device == device_of(&parent) && !mount_root {
        return true;
    }
    let table = mounts::read().unwrap_or_default();
    let kind = filesystem::classify_with(folder, &table);
    let crossing_allowed =
        (!options.one_file_system || device == device_of(&root)) && !kind.pseudo && !kind.network;
    let duplicate = mount_root
        && mounts::reached_elsewhere(&table, stat.stx_mnt_id, folder, scan_root, |path| {
            stat_of(path)
                .is_ok_and(|other| device_of(&other) == device && other.stx_ino == stat.stx_ino)
        });
    crossing_allowed && !duplicate
}

/// The mount table, for recognising a bind mount: a mount root whose directory the walk reaches by
/// another path as well.
///
/// `st_dev` cannot say so. A bind mount of a folder has the same device as the folder, so to the
/// walk it looks like any other subdirectory and everything under it is counted a second time —
/// and `-x` does not help, since it never sees a boundary. `du` gets away with it by remembering
/// every directory's inode; a mount root is rare enough to afford the question properly instead.
pub(crate) mod mounts {
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
}

/// The device a `statx` result names, in the same encoding `st_dev` uses.
fn device_of(stat: &rustix::fs::Statx) -> u64 {
    ::rustix::fs::makedev(stat.stx_dev_major, stat.stx_dev_minor)
}

/// A walk in progress. Dropping it stops the workers rather than waiting for the tree.
pub struct LinuxWalk {
    batches: Receiver<Vec<DirEntries>>,
    /// The batch currently being handed out one directory at a time.
    current: ::std::vec::IntoIter<DirEntries>,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Iterator for LinuxWalk {
    type Item = DirEntries;

    fn next(&mut self) -> Option<DirEntries> {
        loop {
            if let Some(directory) = self.current.next() {
                return Some(directory);
            }
            self.current = self.batches.recv().ok()?.into_iter();
        }
    }
}

impl Drop for LinuxWalk {
    fn drop(&mut self) {
        {
            // Under the lock, for the reason `retire` gives: a worker that has read `pending` as
            // non-zero and is about to `wait` still holds it, so it cannot miss this. Setting the
            // flag outside the lock happens to be rescued by the retire-to-zero path, but only by
            // an argument several steps long, and this removes the need for it.
            let _jobs = self.shared.jobs.lock().expect("scan queue poisoned");
            self.shared.stop.store(true, Ordering::Relaxed);
            self.shared.ready.notify_all();
        }
        // Waking them is not enough on its own: the flag alone leaves a blocked sender blocked,
        // and draining alone lets the workers finish the entire scan.
        while self.batches.recv().is_ok() {}
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Walk `root` with `threads` workers, yielding one [`DirEntries`] per directory.
pub fn walk_linux(root: &Path, threads: usize, options: ScanOptions) -> LinuxWalk {
    let root: PathBuf = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root: Arc<Path> = Arc::from(root.as_path());

    let scan_device = statx(
        rustix::fs::CWD,
        root.as_os_str(),
        AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS,
    )
    .map(|stat| device_of(&stat))
    .unwrap_or_default();

    let threads = threads.max(1);
    let root_kind = filesystem::classify(&root);
    let shared = Arc::new(Shared {
        jobs: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        queued: AtomicUsize::new(1),
        pending: AtomicUsize::new(1),
        stop: AtomicBool::new(false),
        root: Arc::clone(&root),
        threads,
        mounts: ::std::sync::OnceLock::new(),
        devices: Mutex::new(::std::collections::HashMap::new()),
    });
    shared.jobs.lock().expect("scan queue poisoned").push(Job {
        path: Arc::clone(&root),
        depth: 0,
        device: scan_device,
        reflinks: root_kind.reflinks,
        extent_space: root_kind.extent_space.unwrap_or(scan_device),
        compressed_sizes: root_kind.compressed_sizes,
        dirblocks: root_kind
            .ext
            .then(|| shared.device_for(scan_device))
            .flatten(),
    });

    // Bounded so a slow consumer bounds the walk's memory rather than letting it run ahead of the
    // tree by the whole filesystem.
    let (sender, batches): (SyncSender<Vec<DirEntries>>, Receiver<Vec<DirEntries>>) =
        sync_channel(64);

    let donate_below = threads;

    let workers = (0..threads)
        .map(|_| {
            let shared = Arc::clone(&shared);
            let sender = sender.clone();
            ::std::thread::spawn(move || {
                worker(&shared, &sender, options, scan_device, donate_below)
            })
        })
        .collect();

    LinuxWalk {
        batches,
        current: Vec::new().into_iter(),
        shared,
        workers,
    }
}

/// How many entries a worker accumulates before handing a batch to the consumer.
///
/// One directory per channel message costs a lock per directory and wakes the consumer 300,000
/// times on a whole-disk scan; batching makes the channel disappear from the profile.
const SEND_BATCH: usize = 4096;

fn worker(
    shared: &Shared,
    sender: &SyncSender<Vec<DirEntries>>,
    options: ScanOptions,
    scan_device: u64,
    donate_below: usize,
) {
    let mut local: Vec<Job> = Vec::new();
    let mut outbox: Vec<DirEntries> = Vec::new();
    let mut outbox_entries = 0usize;
    let mut prefetch = dirblocks::Prefetcher::default();

    // Every job on `local` is counted in `pending`, so any path out of this function has to hand
    // the whole stack back or the walk never finishes.
    macro_rules! abandon {
        () => {{
            shared.retire(1 + local.len());
            return;
        }};
    }

    while let Some(job) = local.pop().or_else(|| shared.steal()) {
        let (directory, children) =
            read_directory(&job, &options, scan_device, shared, &mut prefetch);

        // `max(1)` so that a run of empty directories still flushes. Counting only entries, a
        // worker walking a wide tree of empty directories would hold every one of them until it
        // ran out of work, and the consumer would sit idle waiting for a batch that never filled.
        outbox_entries += directory.len().max(1);
        outbox.push(directory);
        if outbox_entries >= SEND_BATCH {
            outbox_entries = 0;
            if sender.send(::std::mem::take(&mut outbox)).is_err() {
                abandon!();
            }
        }
        if shared.stop.load(Ordering::Relaxed) {
            abandon!();
        }

        shared.pending.fetch_add(children.len(), Ordering::Relaxed);
        // The children come in inode order and the stack is popped from the end, so they go on
        // reversed: the smallest inode is read next. On ext4 a directory's blocks are allocated in
        // its inode's block group, so this walks the disk forwards through each subtree — the
        // directory blocks of a 31k-directory tree fall into 6k contiguous runs this way against
        // 25k in readdir order, and a single-threaded cold walk took 6.2s against 7.3s. (Serving
        // the whole queue in inode order, not just each directory's children, halves the runs
        // again but cost 6% warm in sorting; see `docs/scan-performance.md`, "Cold cache".)
        local.extend(children.into_iter().rev());
        // Publish while the queue is thin enough that a worker could be about to go idle, rather
        // than only when it is empty: by the time it is empty the others are already asleep.
        if local.len() > 1 && shared.queued.load(Ordering::Relaxed) < donate_below {
            shared.donate(&mut local);
        }
        shared.retire(1);
    }

    if !outbox.is_empty() {
        let _ = sender.send(outbox);
    }
}
