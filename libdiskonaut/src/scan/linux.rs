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
//! leaves as exactly one [`DirEntries`], so the tree builder resolves each parent once.

use ::std::ffi::OsStr;
use ::std::mem::MaybeUninit;
use ::std::os::fd::AsFd;
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
}

impl Shared {
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
    .union(StatxFlags::BLOCKS);

/// Read one directory, returning its entries and the subdirectories to descend into.
fn read_directory(job: &Job, options: &ScanOptions, scan_device: u64) -> (DirEntries, Vec<Job>) {
    let mut directory = DirEntries::new(Arc::clone(&job.path));
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

    let descend = options.max_depth.is_none_or(|max| job.depth + 1 < max);
    let mut buffer = [MaybeUninit::<u8>::uninit(); 64 * 1024];
    let mut reader = RawDir::new(&dir, &mut buffer);

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

        // NO_AUTOMOUNT matters as much as SYMLINK_NOFOLLOW here. `stat`, `lstat` and `fstatat` all
        // behave as though it were set; bare `statx` does not, and the man page names this exact
        // case: "can be used in tools that scan directories to prevent mass-automounting of a
        // directory of automount points". Without it, merely *looking at* an autofs placeholder
        // mounts it, so a directory of NFS home maps would be mounted wholesale and one dead
        // server would hang the scan. This fires before the descent decision, so the care taken
        // over autofs in `filesystem` would not have covered it.
        let Ok(stat) = statx(
            &dir,
            name,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
            WANTED,
        ) else {
            // A file that vanished between the readdir and the stat, or one whose parent we may
            // list but not interrogate. Counted, not guessed at.
            directory.failed += 1;
            continue;
        };

        let kind = FileType::from_raw_mode(u32::from(stat.stx_mode));
        let is_dir = kind == FileType::Directory;
        let allocated = stat.stx_blocks.saturating_mul(512);
        let size = if options.show_apparent_size {
            stat.stx_size
        } else {
            allocated
        };

        let device = device_of(&stat);
        // Only where the filesystem this directory sits on was found to support sharing at all —
        // not merely where it is the scan root's. Probing means *opening* the file, so an NFS, SMB
        // or FUSE mount reached part-way through a scan must not be probed: those are excluded by
        // magic number rather than by device, which also lets btrfs subvolumes and a second XFS
        // volume be probed, and they are exactly where the sharing lives.
        let shared_extent = if job.reflinks
            && device == job.device
            && kind == FileType::RegularFile
            && allocated >= reflink::PROBE_ABOVE_BYTES
        {
            reflink::shared_identity(dir.as_fd(), name, device).unwrap_or(0)
        } else {
            0
        };

        if is_dir && descend {
            let child = job.path.join(OsStr::from_bytes(name.to_bytes()));
            // A different device means this entry is a mount point, and the only point at which
            // the walk has a decision to make. Everything below it is on one filesystem, already
            // judged, so the `statfs` costs one call per mount rather than one per directory.
            let crossing = device != job.device;
            let (allowed, reflinks) = if crossing {
                let same_filesystem = device == scan_device;
                let kind = filesystem::classify(&child);
                (
                    (!options.one_file_system || same_filesystem) && !kind.pseudo,
                    kind.reflinks,
                )
            } else {
                (true, job.reflinks)
            };
            if allowed {
                children.push(Job {
                    path: Arc::from(child.as_path()),
                    depth: job.depth + 1,
                    device,
                    reflinks,
                });
            }
        }

        // Straight from the `getdents64` buffer into the packed one: no allocation per entry.
        directory.push(
            OsStr::from_bytes(name.to_bytes()),
            EntryMeta {
                size,
                inode: stat.stx_ino,
                links: u64::from(stat.stx_nlink),
                is_dir,
                shared_extent,
            },
        );
    }

    directory.shrink();
    (directory, children)
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
    /// Extents asked for in one go. Enough to describe an unfragmented copy several times over; a
    /// file needing more than this is left counted in full rather than half-understood.
    const MAX_EXTENTS: usize = 64;
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
    pub fn shared_identity(dir: BorrowedFd<'_>, name: &CStr, device: u64) -> Option<u64> {
        let file = openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()?;

        let mut request = Request {
            start: 0,
            length: u64::MAX,
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

        // FNV-1a over the device and then the extent map. The device has to be in there: a
        // physical block offset means nothing without the filesystem it indexes, and the walk now
        // probes every sharing filesystem it meets rather than only the scan root's. Two unrelated
        // files on two volumes sharing an offset and a size is otherwise an easy collision, and it
        // would merge them — the same undercount that keying on one extent used to cause.
        let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fold = |value: u64| {
            identity = (identity ^ value).wrapping_mul(0x0000_0100_0000_01b3);
        };
        fold(device);

        let mut saw_last = false;
        for extent in &request.extents[..mapped] {
            if extent.flags & FIEMAP_EXTENT_SHARED == 0 {
                return None;
            }
            fold(extent.physical);
            fold(extent.length);
            saw_last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
        }
        if !saw_last {
            return None;
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

    /// Magic numbers of filesystems that can share extents between files.
    const REFLINK: &[u32] = &[
        0x5846_5342, // xfs
        0x9123_683e, // btrfs
    ];

    /// What the walk needs to know about a filesystem, from one `statfs`.
    #[derive(Clone, Copy, Debug)]
    pub struct Kind {
        /// Reports no meaningful disk usage; not worth descending into.
        pub pseudo: bool,
        /// Can share extents between files, so reflinks are worth looking for.
        pub reflinks: bool,
    }

    /// Classify the filesystem `path` sits on.
    ///
    /// Asked once per mount point crossed, never per entry. A filesystem that cannot be asked is
    /// treated as ordinary and non-sharing, which descends into it but does not open its files.
    pub fn classify(path: &Path) -> Kind {
        ::rustix::fs::statfs(path).map_or(
            Kind {
                pseudo: false,
                reflinks: false,
            },
            |fs| {
                let magic = magic_of(&fs);
                Kind {
                    pseudo: PSEUDO.contains(&magic),
                    reflinks: REFLINK.contains(&magic),
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

    let shared = Arc::new(Shared {
        jobs: Mutex::new(vec![Job {
            path: Arc::clone(&root),
            depth: 0,
            device: scan_device,
            reflinks: filesystem::classify(&root).reflinks,
        }]),
        ready: Condvar::new(),
        queued: AtomicUsize::new(1),
        pending: AtomicUsize::new(1),
        stop: AtomicBool::new(false),
    });

    // Bounded so a slow consumer bounds the walk's memory rather than letting it run ahead of the
    // tree by the whole filesystem.
    let (sender, batches): (SyncSender<Vec<DirEntries>>, Receiver<Vec<DirEntries>>) =
        sync_channel(64);

    let threads = threads.max(1);
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

    // Every job on `local` is counted in `pending`, so any path out of this function has to hand
    // the whole stack back or the walk never finishes.
    macro_rules! abandon {
        () => {{
            shared.retire(1 + local.len());
            return;
        }};
    }

    while let Some(job) = local.pop().or_else(|| shared.steal()) {
        let (directory, children) = read_directory(&job, &options, scan_device);

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
        local.extend(children);
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
