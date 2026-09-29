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

use ::rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, StatxFlags, openat};

use super::{DirEntries, EntryMeta, ScanOptions};
use crate::focus::{self, Focus, FocusWatch};

pub(crate) mod btrfs;
mod crossing;
mod dirblocks;
mod environment;
pub(crate) mod filesystem;
pub(crate) mod mounts;
mod reflink;
pub(crate) mod stat;

pub use crossing::walk_would_enter;
use crossing::{loops_back, same_filesystem};
pub use environment::environment;
pub(crate) use mounts::mount_of;
pub(crate) use stat::statx;
use stat::{NO_STATX, device_of};

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
    /// to a caller with `CAP_SYS_ADMIN`. See [`btrfs::extents`].
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
    /// What the scan root's extents index: on btrfs its filesystem's UUID, shared by every
    /// subvolume of it; the root's device elsewhere. For `-x`: see [`same_filesystem`].
    root_space: u64,
    /// Each mount point in the table and the id of what shows there, built from it the first
    /// time a kernel that cannot say (before 5.8) leaves the walk to ask; see [`Shared::mounted_at`].
    points: ::std::sync::OnceLock<::std::collections::HashMap<PathBuf, u64>>,
    /// The block devices opened for [`dirblocks`], by `st_dev`; `None` where one could not be.
    devices: Mutex<::std::collections::HashMap<u64, Option<Arc<dirblocks::Device>>>>,
    /// The folder the user is in, which the walk reads toward first (`crate::focus`).
    focus: Focus,
}

impl Shared {
    /// The mount table, read once per walk, the first time it is needed.
    fn mount_table(&self) -> &[mounts::Mount] {
        self.mounts
            .get_or_init(|| mounts::read().unwrap_or_default())
    }

    /// The mount shown at `path`, by id, if `path` is a mount point: what `statx` says itself
    /// from Linux 5.8 (`STATX_ATTR_MOUNT_ROOT`, `stx_mnt_id`), found in the mount table before
    /// then. The last mount at a point is the one that shows there.
    fn mounted_at(&self, path: &Path) -> Option<u64> {
        self.points
            .get_or_init(|| mounts::points(self.mount_table()))
            .get(path)
            .copied()
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

    /// Wait for a directory published by another worker, or for the walk to end: one toward
    /// the focus if there is one, else the last published.
    fn steal(&self, focus: &mut FocusWatch) -> Option<Job> {
        let mut jobs = self.jobs.lock().expect("scan queue poisoned");
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(job) =
                focus::take_toward(focus, &mut jobs, |job| &job.path).or_else(|| jobs.pop())
            {
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
    /// A directory not walked because it is one of its own ancestors again: which one.
    loops_to: Option<PathBuf>,
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
) -> Result<Inspected, ::rustix::io::Errno> {
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
    )?;

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
        btrfs::extents::on_disk(dir, stat.stx_ino).unwrap_or(allocated)
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
    let mut loops_to = None;
    if is_dir && descend {
        let path = job.path.join(OsStr::from_bytes(name.to_bytes()));
        // A mount root showing a directory the walk reaches anyway — a bind mount of a folder
        // inside the scan, or a second mount of a filesystem already in it — is left empty:
        // same `st_dev`, often, so nothing below would notice the files were seen already.
        let (mount_root, mount_id) = mount_of(&stat, || shared.mounted_at(&path));
        let duplicate = mount_root && {
            mounts::reached_elsewhere(
                shared.mount_table(),
                mount_id,
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
        let (allowed, reflinks, extent_space, compressed_sizes, dirblocks, btrfs) = if crossing {
            let kind = filesystem::classify_with(&path, shared.mount_table());
            let same_filesystem = same_filesystem(device, &kind, scan_device, shared.root_space);
            (
                (!options.one_file_system || same_filesystem) && !kind.pseudo && !kind.network,
                kind.reflinks,
                kind.extent_space.unwrap_or(device),
                kind.compressed_sizes,
                kind.ext.then(|| shared.device_for(device)).flatten(),
                kind.btrfs,
            )
        } else {
            (
                true,
                job.reflinks,
                job.extent_space,
                job.compressed_sizes,
                job.dirblocks.clone(),
                false,
            )
        };
        // A mount of a directory inside itself, or of one above the scan: walked, it would hold
        // itself again, and again. Only a mount can do this — Linux has no directory hard links —
        // so the ancestors are asked only at a mount root or a crossing, which is rare.
        loops_to = (mount_root || crossing)
            .then(|| loops_back(&path, device, stat.stx_ino))
            .flatten();
        let snapshot = btrfs::snapshot_left_out(options.snapshots, btrfs, stat.stx_ino, || {
            btrfs::subvolume::is_read_only(dir, name)
        });
        if allowed && !duplicate && !snapshot && loops_to.is_none() {
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

    Ok(Inspected {
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
        loops_to,
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
        Err(error) => {
            directory.fail("open", None, ::std::io::Error::from(error));
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
            Err(error) => {
                // Otherwise stop reading this directory rather than asking again. A `getdents64`
                // error is a property of the descriptor, not of one entry, so it is still there on
                // the next call: `/proc/<pid>/net` for a process that has since died returns
                // `EINVAL` every time, and retrying it is an infinite loop. `std`'s `ReadDir` ends
                // on the first error too.
                directory.fail("list", None, ::std::io::Error::from(error));
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
    let mut keep =
        |directory: &mut DirEntries, l: &Listed, found: Result<Inspected, ::rustix::io::Errno>| {
            let found = match found {
                Ok(found) => found,
                Err(error) => {
                    // A file that vanished between the readdir and the stat, or one whose parent we
                    // may list but not interrogate. Counted, not guessed at.
                    let name = OsStr::from_bytes(name_of(l).to_bytes());
                    directory.fail("stat", Some(name), ::std::io::Error::from(error));
                    return;
                }
            };
            // A share's snapshots, seen over the network: listed, not entered.
            let name = OsStr::from_bytes(name_of(l).to_bytes());
            let left_out = directory.leave_out(name, found.child.is_some(), options.snapshots);
            if !left_out && let Some(child) = found.child {
                children.push(child);
            }
            if let Some(ancestor) = &found.loops_to {
                directory.note(
                    "loop",
                    Some(name),
                    format!(
                        "a mount of {} inside itself; not walked again",
                        ancestor.display()
                    ),
                );
            }
            if found.later {
                directory
                    .later
                    .push(u32::try_from(directory.len()).unwrap_or(u32::MAX));
            }
            directory.push(name, found.meta);
        };

    let helpers = (listed.len() / STAT_CHUNK).min(shared.threads);
    if listed.len() >= STAT_SHARED_ABOVE && helpers > 1 {
        // This worker takes the first chunk itself; the helpers take the rest, and hand their
        // findings back in listing order so the packed directory comes out the same either way.
        let chunk = listed.len().div_ceil(helpers);
        let found: Vec<Vec<Result<Inspected, ::rustix::io::Errno>>> =
            ::std::thread::scope(|scope| {
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
pub fn walk_linux(root: &Path, threads: usize, options: ScanOptions, focus: &Focus) -> LinuxWalk {
    if ::std::env::var_os("DUSCAPE_NO_STATX").is_some() {
        NO_STATX.store(true, Ordering::Relaxed);
    }
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
        root_space: root_kind.extent_space.unwrap_or(scan_device),
        points: ::std::sync::OnceLock::new(),
        devices: Mutex::new(::std::collections::HashMap::new()),
        focus: focus.clone(),
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
    let mut focus = shared.focus.watch_under(&shared.root);

    // Every job on `local` is counted in `pending`, so any path out of this function has to hand
    // the whole stack back or the walk never finishes.
    macro_rules! abandon {
        () => {{
            shared.retire(1 + local.len());
            return;
        }};
    }

    while let Some(job) = local.pop().or_else(|| shared.steal(&mut focus)) {
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
        let pushed = children.len();
        local.extend(children.into_iter().rev());
        // The folder the user is in, and the way down to it, before anything else.
        focus::arrange(&mut focus, &mut local, pushed, |job| &job.path);
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
