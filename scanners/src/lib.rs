//! The scanners: one native directory walker per platform, and what drives them.
//!
//! * `linux` — `getdents64` and `statx` on its own thread pool, with the FIEMAP reflink probe and
//!   btrfs compression; pseudo, network and bind-mounted filesystems are refused at their mount
//!   points.
//! * `macos` — `getattrlistbulk(2)`.
//! * `windows` — one handle per directory, entries in bulk from
//!   `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`; NTFS metadata files when elevated
//!   ([`ntfs`]).
//! * a `dua-core` fallback everywhere else.
//!
//! Every walker yields the same thing, one [`DirEntries`] per directory ([`scan_directories`]),
//! and [`parallel::build_tree`] turns that into a [`FileTree`] on several threads. [`refine`] is
//! the second pass over the small files the walk left for later, and [`rescan`] runs a rescan or
//! a second pass on a thread of its own, for a viewer to show when it is done. The types a walk
//! produces — [`ScanOptions`], [`EntryMeta`], [`DirEntries`], [`Outline`] — are
//! `libduscape`'s, re-exported here, since the model consumes them.

use ::std::num::NonZero;
use ::std::path::Path;

use ::dua_core::{Options, Order, walk};

use libduscape::model::{FileTree, Folder};
pub use libduscape::scan::*;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub mod ext4;

#[cfg(windows)]
pub mod windows;

/// NTFS read from its master file table, elevated. The parsing and the tree run everywhere, so
/// their tests do; only reading the volume is Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod mft;
/// NTFS record parsing. Compiled everywhere so that its tests run everywhere; only the Windows
/// walker uses it.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod ntfs;

pub mod refine;
pub mod rescan;

/// Building the tree on several threads at once.
///
/// Each builder owns a private tree and is sent every directory whose path hashes to it, so no
/// memory is shared while the scan runs. Afterwards the trees are merged and the shared blocks
/// every builder deferred are charged once over the result. See `docs/scan-performance.md` for
/// the measurements behind the two constants.
pub mod parallel {
    use ::std::path::Path;
    use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use ::std::thread;
    use ::std::time::{Duration, Instant};

    use super::{DirEntries, ScanOptions, scan_directories};
    use libduscape::model::{FileTree, Folder};

    /// Builders to run.
    ///
    /// On Linux the tree build is the bottleneck, and four builders hide it entirely behind a
    /// 24-thread walk on a 32-core machine; the curve is flat from there to eight. On Windows and
    /// macOS the walk is the bottleneck — a handle per directory on one, synchronous metadata reads
    /// on the other, both well under a tenth of what one builder can take — so a single builder
    /// keeps pace, and one shard skips the deferral, merge, replay, and per-directory shard hash a
    /// parallel build needs (see [`build_tree`]), matching the plain pipeline instead of paying for
    /// parallelism that a walk-bound volume cannot use. See `docs/scan-performance.md`.
    #[cfg(not(any(windows, target_os = "macos")))]
    pub const SHARDS: usize = 4;
    #[cfg(any(windows, target_os = "macos"))]
    pub const SHARDS: usize = 1;

    /// Path components that decide a directory's builder.
    ///
    /// Hashing the first few components rather than the whole path is what keeps the merge
    /// cheap: every directory under one prefix lands in one builder, so builders overlap only at
    /// folders shallower than this and everything deeper moves into the merged tree as a whole
    /// subtree. Hashing whole paths made the merge cost more than it saved — 74,688 folders to
    /// reconcile on a 4.2M-entry volume against 355 at this depth. Deeper than this the busiest
    /// builder gets unluckier again, because the largest subtrees stay indivisible.
    pub const SHARD_DEPTH: usize = 5;

    /// How long each phase took, for the benchmark.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Timings {
        /// The walk, with the builders keeping pace.
        pub built: Duration,
        pub merged: Duration,
        pub replayed: Duration,
    }

    /// Which builder a directory belongs to.
    pub fn shard_of(root: &Path, path: &Path, depth: usize, shards: usize) -> usize {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let depth = if depth == 0 { usize::MAX } else { depth };
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for component in relative.components().take(depth) {
            for byte in component.as_os_str().as_encoded_bytes() {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            // A separator, so that `ab/c` and `a/bc` do not hash alike.
            hash = (hash ^ u64::from(b'/')).wrapping_mul(0x0000_0100_0000_01b3);
        }
        (hash % shards as u64) as usize
    }

    /// Walk `root` and build its tree on `shards` threads.
    ///
    /// `progress` sees every directory as it is dispatched, before any builder has it, and may
    /// return `false` to stop the scan — in which case nothing is merged and `None` comes back.
    /// Otherwise the merged, reconciled tree, the count of unreadable entries, and the small
    /// files left for the second pass ([`super::refine`]).
    pub fn build_tree(
        root: &Path,
        options: ScanOptions,
        shards: usize,
        depth: usize,
        mut progress: impl FnMut(&DirEntries) -> bool,
    ) -> Option<(FileTree, u64, Timings, super::refine::SmallFiles)> {
        let shards = shards.max(1);
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let start = Instant::now();

        // A single builder sees the whole tree, so no hard link can span shards and it can charge
        // shared blocks inline exactly as the serial walk does. Deferring would only add a serial
        // merge and replay after the walk — on a walk-bound volume, pure overhead with no build
        // parallelism to pay for it.
        let deferring = shards > 1;

        let mut senders = Vec::with_capacity(shards);
        let mut builders = Vec::with_capacity(shards);
        for index in 0..shards {
            let (sender, receiver): (SyncSender<Vec<DirEntries>>, Receiver<Vec<DirEntries>>) =
                sync_channel(64);
            senders.push(sender);
            let root = root.clone();
            let builder = thread::Builder::new()
                .name(format!("tree_builder_{index}"))
                .spawn(move || {
                    let mut tree = if deferring {
                        FileTree::deferring_shared_blocks(Folder::new(&root), root)
                    } else {
                        FileTree::new(Folder::new(&root), root)
                    };
                    while let Ok(batch) = receiver.recv() {
                        for directory in batch {
                            tree.add_dir_entries(directory);
                        }
                    }
                    tree
                })
                .expect("spawn a tree builder");
            builders.push(builder);
        }

        let mut outboxes: Vec<Vec<DirEntries>> = (0..shards).map(|_| Vec::new()).collect();
        let mut queued = 0usize;
        let mut failed = 0u64;
        let mut stopped = false;
        let mut small = super::refine::SmallFiles::default();
        for directory in scan_directories(&root, options) {
            if !progress(&directory) {
                stopped = true;
                break;
            }
            failed += directory.failed;
            small.note(&directory);
            // One shard means one builder: skip hashing the path to choose it — the hash runs per
            // directory and always lands on zero, so on a walk-bound volume it is pure overhead.
            let shard = if shards == 1 {
                0
            } else {
                shard_of(&root, &directory.path, depth, shards)
            };
            outboxes[shard].push(directory);
            queued += 1;
            if queued >= 256 {
                queued = 0;
                for (outbox, sender) in outboxes.iter_mut().zip(&senders) {
                    if !outbox.is_empty() {
                        let _ = sender.send(::std::mem::take(outbox));
                    }
                }
            }
        }
        if !stopped {
            for (outbox, sender) in outboxes.into_iter().zip(&senders) {
                if !outbox.is_empty() {
                    let _ = sender.send(outbox);
                }
            }
        }
        drop(senders);

        let mut trees: Vec<FileTree> = builders
            .into_iter()
            .map(|builder| builder.join().expect("a tree builder panicked"))
            .collect();
        if stopped {
            return None;
        }
        let built = Instant::now();

        let mut tree = trees.pop().expect("at least one builder");
        for other in trees {
            tree.merge_from(other);
        }
        let merged = Instant::now();

        tree.replay_deferred();
        tree.volume_used = super::comparable_volume_used(&root, options);
        tree.shown = options.shown();
        let replayed = Instant::now();

        Some((
            tree,
            failed,
            Timings {
                built: built - start,
                merged: merged - built,
                replayed: replayed - merged,
            },
            small,
        ))
    }
}

/// Whether a walk of `scan_root` goes into `folder`, a folder inside it: what a rescan of that
/// folder alone has to ask first, since a walk starting there applies no rule to its own root.
/// On Linux it asks what the walk would at a mount point; elsewhere every folder is entered.
#[must_use]
pub fn walk_would_enter(scan_root: &Path, folder: &Path, options: ScanOptions) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::walk_would_enter(scan_root, folder, options)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (scan_root, folder, options);
        true
    }
}

/// Walk `root`, yielding the contents of one directory at a time.
///
/// On macOS this uses `macos`, which asks the kernel only for the attributes disk usage needs.
/// On Linux it uses [`linux`], which owns its own thread pool because `dua-core`'s stops scaling
/// well before the kernel does. On Windows it uses `windows`, which reads a directory's sizes
/// in bulk rather than opening every file. Elsewhere it groups the `dua-core` walk, which reports a
/// directory's entries consecutively.
pub fn scan_directories(root: &Path, options: ScanOptions) -> impl Iterator<Item = DirEntries> {
    #[cfg(target_os = "macos")]
    {
        macos::walk_macos(
            root,
            thread_count(options),
            options.max_depth,
            options.one_file_system,
        )
    }
    #[cfg(target_os = "linux")]
    {
        // From the device where that is allowed and asked for, else through the kernel.
        let device = if options.read_device {
            ext4::walk_ext4(root, options)
        } else {
            None
        };
        match device {
            Some(walk) => LinuxScan::Device(walk),
            None => LinuxScan::Kernel(linux::walk_linux(root, thread_count(options), options)),
        }
    }
    #[cfg(windows)]
    {
        // From the volume's master file table where the process may open the volume
        // (elevated, NTFS) and asks for it, else through the filesystem.
        let device = if options.read_device {
            mft::walk_mft(root, thread_count(options), options)
        } else {
            None
        };
        match device {
            Some(walk) => WindowsScan::Table(walk),
            None => {
                WindowsScan::Kernel(windows::walk_windows(root, thread_count(options), options))
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        fallback::group_by_directory(root, options)
    }
}

/// The Windows walk: from the volume's table, or through the filesystem.
#[cfg(windows)]
enum WindowsScan {
    Table(mft::MftWalk),
    Kernel(windows::WindowsWalk),
}

#[cfg(windows)]
impl Iterator for WindowsScan {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        match self {
            WindowsScan::Table(walk) => walk.next(),
            WindowsScan::Kernel(walk) => walk.next(),
        }
    }
}

/// The Linux walk: from the device, or through the kernel.
#[cfg(target_os = "linux")]
enum LinuxScan {
    Device(ext4::Ext4Walk),
    Kernel(linux::LinuxWalk),
}

#[cfg(target_os = "linux")]
impl Iterator for LinuxScan {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        match self {
            LinuxScan::Device(walk) => walk.next(),
            LinuxScan::Kernel(walk) => walk.next(),
        }
    }
}

/// Compiled on every platform, though only selected on platforms that are neither macOS nor
/// Linux, so that it cannot rot unnoticed. The tests call it directly everywhere.
#[cfg_attr(
    any(target_os = "macos", target_os = "linux", windows),
    allow(dead_code)
)]
mod fallback {
    use super::{
        DirEntries, EntryMeta, Options, Order, ScanOptions, descend_predicate, dua_thread_count,
        entry_identity, entry_size, walk,
    };
    use ::std::path::Path;
    use ::std::sync::Arc;

    /// Collect the `dua-core` walk into per-directory groups.
    ///
    /// `Order::Completion` reports a directory's entries together, so accumulating entries that
    /// share a parent recovers whole directories without buffering the whole walk. A directory
    /// whose entries arrive in several chunks simply yields several groups, which the tree builder
    /// handles: each group carries only its own entries' sizes and counts.
    pub fn group_by_directory(
        root: &Path,
        options: ScanOptions,
    ) -> impl Iterator<Item = DirEntries> {
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root: Arc<Path> = Arc::from(root_canon.as_path());
        let descend = descend_predicate(&root, options);
        let mut walk = walk(
            &root,
            dua_thread_count(options),
            Order::Completion,
            Options::default(),
            descend,
        );

        // Held across calls: the group being accumulated is only yielded once an entry for a
        // different directory shows up, or the walk ends.
        let mut open: Option<DirEntries> = None;

        std::iter::from_fn(move || {
            loop {
                let Some(entry) = walk.next() else {
                    return open.take();
                };
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        // No entry, so nothing identifies which directory this belongs to.
                        match &mut open {
                            Some(open) => open.failed += 1,
                            None => {
                                let mut lost = DirEntries::new(Arc::clone(&root));
                                lost.failed = 1;
                                return Some(lost);
                            }
                        }
                        continue;
                    }
                };
                let ::dua_core::Entry {
                    depth,
                    file_name,
                    file_type,
                    metadata,
                    parent_path,
                    ..
                } = entry;
                if depth == 0 {
                    // The walk root itself, reported with the root's *parent* as its parent path.
                    // It is not an entry inside the tree being scanned.
                    continue;
                }
                let named = metadata.and_then(Result::ok).map(|metadata| {
                    let (inode, links) = entry_identity(
                        Some(&parent_path.join(&file_name)),
                        file_type.is_dir(),
                        &metadata,
                    );
                    EntryMeta {
                        size: entry_size(&metadata, false),
                        apparent: entry_size(&metadata, true),
                        inode,
                        links,
                        is_dir: file_type.is_dir(),
                        shared_extent: 0,
                    }
                });

                match &mut open {
                    Some(open)
                        if Arc::ptr_eq(&open.path, &parent_path) || open.path == parent_path =>
                    {
                        match named {
                            Some(meta) => open.push(&file_name, meta),
                            None => open.failed += 1,
                        }
                    }
                    // A different directory: start its group, and hand back the finished one.
                    // The new group already holds this entry, so nothing is lost by returning.
                    _ => {
                        let mut group = DirEntries::with_capacity(parent_path, 32, 32 * 32);
                        match named {
                            Some(meta) => group.push(&file_name, meta),
                            None => group.failed = 1,
                        }
                        let finished = open.replace(group);
                        if let Some(finished) = finished {
                            return Some(finished);
                        }
                    }
                }
            }
        })
    }
}

/// Workers for the walker the app actually uses: `--threads`, else [`default_scan_threads`].
pub fn thread_count(options: ScanOptions) -> usize {
    if let Some(threads) = options.threads {
        return threads.max(1);
    }
    if options.parallel {
        default_scan_threads()
    } else {
        1
    }
}

/// Workers for the `dua-core` walk, wherever it is still reached.
///
/// It needs its own number. The cap is a property of a walker, not of a machine: raising the
/// native walker's cap to 24 and letting `dua-core` inherit it made the `dua-*` benchmark stages —
/// and `scan_folder`, which is public — 38% slower on this box, by running the one walker that
/// collapses past eight workers at 24 of them.
fn dua_thread_count(options: ScanOptions) -> usize {
    if let Some(threads) = options.threads {
        return threads.max(1);
    }
    if options.parallel {
        cores().min(MAX_DUA_THREADS)
    } else {
        1
    }
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, NonZero::get)
}

/// The scan's workers on this machine, past which more stop paying for themselves. The numbers
/// come from different walkers and do not transfer to each other.
///
/// On Linux, three workers a core, at most 32. Warm, the walk is CPU-bound and one a core would
/// do: on a 32-core box XFS and ext4 are flat from twelve workers up and give up only a few
/// percent by thirty-two. Cold, every directory costs the worker that opens it two synchronous
/// disk reads (its inode, then its blocks) before its entries can be looked at, so the disk sees
/// as many reads in flight as there are workers blocked in them, and at one a core an SSD that
/// serves seventy thousand reads a second is handed four at a time: 8 workers took 1.01s over
/// 375k entries on 8 cores, 16 took 0.79s and 24 took 0.74s, the device's floor for those 34k
/// reads. Warm, 24 cost 9% on a tree that scans in 0.2s and nothing on one of 2.2M entries. See
/// `docs/scan-performance.md`, "Cold cache".
#[cfg(target_os = "linux")]
fn default_scan_threads() -> usize {
    (cores() * 3).clamp(1, 32)
}
/// On Windows the walk is bound by a fixed cost per directory handle in the kernel and its filter
/// drivers (Defender among them): on a system volume about 55µs of thread time per directory,
/// three times what the same walk pays on a data volume. The kernel side stops scaling at a
/// number of workers that grows with the machine, not at a fixed count. On a 12-thread machine 8
/// workers beat 4 and 16. On a 32-thread one, `C:\` (436k directories) took 6.18s at 8 and 5.56s
/// at 12, interleaved over four rounds, then 6.9s at 16 and 7.3s at 32. Two thirds of the cores,
/// capped at 12, gives 8 on the first and 12 on the second.
#[cfg(windows)]
fn default_scan_threads() -> usize {
    (cores() * 2 / 3).clamp(1, 12)
}
/// On macOS the `getattrlistbulk` walk really does contend: a whole-disk scan is fastest at six
/// workers on a 14-core machine. Eight is 3% slower and burns 60% more kernel time, and it gets
/// steadily worse beyond that. The walk is bound by synchronous 4 KiB metadata reads, and past six
/// the extra queue depth is spent contending in the kernel rather than reading.
#[cfg(target_os = "macos")]
fn default_scan_threads() -> usize {
    cores().min(6)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn default_scan_threads() -> usize {
    cores().min(8)
}

/// Worker cap for the `dua-core` walk, which collapses past eight on every machine measured.
const MAX_DUA_THREADS: usize = 8;

/// Walk `root` and yield each filesystem entry (or a read error marker).
pub fn scan_folder(root: impl AsRef<Path>, options: ScanOptions) -> impl Iterator<Item = ScanItem> {
    let root = root
        .as_ref()
        .canonicalize()
        .unwrap_or_else(|_| root.as_ref().to_path_buf());
    let threads = dua_thread_count(options);
    let descend = descend_predicate(&root, options);

    walk(
        &root,
        threads,
        Order::Completion,
        Options::default(),
        descend,
    )
    .map(move |entry| match entry {
        Ok(entry) => {
            let path = entry.path();
            match entry.metadata {
                Some(Ok(metadata)) => {
                    let (inode, links) =
                        entry_identity(Some(&path), entry.file_type.is_dir(), &metadata);
                    ScanItem::Entry {
                        path,
                        meta: EntryMeta {
                            size: entry_size(&metadata, false),
                            apparent: entry_size(&metadata, true),
                            inode,
                            links,
                            is_dir: entry.file_type.is_dir(),
                            shared_extent: 0,
                        },
                    }
                }
                Some(Err(_)) | None => ScanItem::ReadError,
            }
        }
        Err(_) => ScanItem::ReadError,
    })
}

/// Whether the walk should descend into a directory entry.
///
/// Unlike the native macOS walker, `dua-core` decides this from the entry as its parent listed it,
/// which is enough for a device comparison: on platforms using this path a mount point really does
/// report the mounted filesystem's device.
fn descend_predicate(
    root: &Path,
    options: ScanOptions,
) -> impl Fn(&::dua_core::Entry) -> bool + Send + Sync + 'static {
    let max_depth = options.max_depth;
    let root_device = options
        .one_file_system
        .then(|| libduscape::os::volume_id(root).unwrap_or_default());
    move |entry| {
        if !max_depth.is_none_or(|max| entry.depth < max) {
            return false;
        }
        match (root_device, &entry.metadata) {
            (Some(root_device), Some(Ok(metadata))) => entry_device(metadata) == root_device,
            _ => true,
        }
    }
}

/// The filesystem an entry lives on, however the platform's metadata spells it.
#[cfg(target_os = "macos")]
fn entry_device(metadata: &::dua_core::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn entry_device(metadata: &::dua_core::Metadata) -> u64 {
    use ::std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(windows)]
fn entry_device(metadata: &::dua_core::Metadata) -> u64 {
    metadata.hard_link_id().map(|(vol, _)| vol).unwrap_or(0)
}

/// Inode number and link count, however the platform's metadata spells them.
#[cfg(target_os = "macos")]
fn entry_identity(
    _path: Option<&Path>,
    _is_dir: bool,
    metadata: &::dua_core::Metadata,
) -> (u64, u64) {
    (metadata.ino(), metadata.nlink())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn entry_identity(
    _path: Option<&Path>,
    _is_dir: bool,
    metadata: &::dua_core::Metadata,
) -> (u64, u64) {
    use ::std::os::unix::fs::MetadataExt;
    (metadata.ino(), metadata.nlink())
}

#[cfg(windows)]
fn entry_identity(
    path: Option<&Path>,
    is_dir: bool,
    metadata: &::dua_core::Metadata,
) -> (u64, u64) {
    let id = metadata
        .hard_link_id()
        .map(|(_, file_id)| file_id)
        .unwrap_or(0);
    let links = if is_dir {
        1
    } else if let Some(path) = path {
        libduscape::os::link_count(path)
    } else {
        1
    };
    (id, links)
}

#[cfg(target_os = "macos")]
fn entry_size(metadata: &::dua_core::Metadata, apparent: bool) -> u64 {
    if apparent {
        metadata.len()
    } else {
        metadata.allocated_size()
    }
}

#[cfg(target_os = "windows")]
fn entry_size(metadata: &::dua_core::Metadata, apparent: bool) -> u64 {
    if apparent {
        metadata.len()
    } else {
        metadata.allocated_size()
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn entry_size(metadata: &::dua_core::Metadata, apparent: bool) -> u64 {
    if apparent {
        metadata.len()
    } else {
        libduscape::os::size_on_disk_fast(metadata)
    }
}

/// Walk `root` and populate a [`FileTree`]. Returns the tree and a count of read failures.
pub fn scan_into_tree(root: impl AsRef<Path>, options: ScanOptions) -> (FileTree, u64) {
    let root_path = root
        .as_ref()
        .canonicalize()
        .unwrap_or_else(|_| root.as_ref().to_path_buf());
    let mut tree = FileTree::new(Folder::new(&root_path), root_path.clone());
    let mut failed_to_read = 0u64;

    for directory in scan_directories(&root_path, options) {
        failed_to_read += directory.failed;
        tree.add_dir_entries(directory);
    }
    tree.volume_used = comparable_volume_used(&root_path, options);
    tree.shown = options.shown();

    (tree, failed_to_read)
}

/// The volume's used bytes, when a scan of `root` with `options` is comparable with them.
///
/// It is when `root` is a volume's root, sizes are blocks allocated rather than lengths, and the
/// scan stayed on that volume. Windows never leaves it, since mounted folders are not followed; on
/// Unix only `-x` promises that, and a scan of `/` that crossed into `/home` would otherwise be
/// set against the root filesystem's usage alone.
fn comparable_volume_used(root: &Path, options: ScanOptions) -> Option<u128> {
    // Blocks either way; `FileTree::outside_scan` hides it while lengths are shown.
    let stays = cfg!(windows) || options.one_file_system;
    if !stays {
        return None;
    }
    libduscape::os::volume_used(root).map(u128::from)
}

#[cfg(test)]
mod tests;
