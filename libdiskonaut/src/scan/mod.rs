//! Parallel directory traversal (`dua-core`).

use ::std::ffi::OsStr;
use ::std::num::NonZero;
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;

use ::dua_core::{Options, Order, walk};

use crate::model::{FileTree, Folder};

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(windows)]
pub mod windows;

/// NTFS record parsing. Compiled everywhere so that its tests run everywhere; only the Windows
/// walker uses it.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod ntfs;

pub mod refine;

/// Options controlling filesystem traversal.
#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    /// Use multiple threads for the walk.
    pub parallel: bool,
    /// Override the worker count. `None` uses one thread per core (or one if `parallel` is off).
    pub threads: Option<usize>,
    /// Show logical file length rather than blocks allocated on disk, to begin with. The tree
    /// holds both either way.
    pub show_apparent_size: bool,
    /// Stop descending below this depth (the root is depth 0). `None` means no limit.
    pub max_depth: Option<usize>,
    /// Do not cross filesystem boundaries (like `du -x`).
    ///
    /// Whatever this is set to, the scan never crosses into a pseudo filesystem (`/proc`, `/sys`)
    /// or a network one (NFS, SMB, sshfs and other remote FUSE filesystems), since neither holds
    /// this disk's space; either can still be scanned by naming it as the root. Nor does it walk a
    /// second route to files it already reaches: on Linux a bind mount of a folder inside the scan
    /// (or a second mount of a filesystem in it), on macOS the starting volume through another
    /// mount point. On macOS a network share is recognised only once its root is opened and
    /// listed, so a server that has gone away can still hold the scan up there.
    pub one_file_system: bool,
    /// Which files Windows tracks by id in case they are hard links, since its directory listings
    /// carry no link count (see `windows::links`).
    ///
    /// `None` tracks every non-empty file, but only in the directories where hard links are
    /// normally made — or everywhere, when the process is elevated and so reaches folders that list
    /// was not drawn from. `Some(bytes)` tracks every file at least that large, wherever it is.
    /// Ignored elsewhere: Unix walks get the link count for free.
    pub hard_link_threshold: Option<u64>,
}

impl ScanOptions {
    /// Which size a tree built with these options shows first.
    #[must_use]
    pub fn shown(&self) -> crate::model::SizeKind {
        if self.show_apparent_size {
            crate::model::SizeKind::Apparent
        } else {
            crate::model::SizeKind::Disk
        }
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            parallel: true,
            threads: None,
            show_apparent_size: false,
            max_depth: None,
            one_file_system: false,
            hard_link_threshold: None,
        }
    }
}

/// The few metadata fields the disk-usage model actually needs, extracted during the walk.
///
/// Keeping this small matters: one of these is produced for every file on the volume and handed
/// across a channel to the tree builder, so the platform `Metadata` (which is an order of
/// magnitude larger) never travels with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryMeta {
    /// Space taken on disk: blocks allocated, in bytes.
    pub size: u64,
    /// Length in bytes, as `du --apparent-size` counts it. The tree keeps both, so the view can
    /// switch between them; see `model::File` for how it keeps them in the space of one.
    pub apparent: u64,
    /// Filesystem identity, used to charge a hard-linked file to a folder only once.
    pub inode: u64,
    /// Directory entries pointing at this file; `1` for an ordinary file, [`LINKS_UNKNOWN`] when
    /// the walk could not cheaply tell.
    pub links: u64,
    pub is_dir: bool,
    /// An identity for this file's blocks when every one of them is shared with another file
    /// through copy-on-write (XFS or btrfs reflink). `0` when they are not, or were not looked
    /// for. Two files sharing only part of themselves get `0` each and are counted in full.
    ///
    /// Reflinked files are the hard-link problem wearing a different hat: one set of blocks
    /// reachable by several paths. `links` does not see them — every copy is its own inode with
    /// `nlink == 1` — so without this the same blocks are counted once per copy.
    pub shared_extent: u64,
}

/// [`EntryMeta::links`] for a file that may have other names, when learning how many would cost
/// more than tracking it does.
///
/// Such a file goes through the hard-link ledger keyed on its `inode`, like a file known to have
/// several names. The ledger charges a file's first name along its whole path, as it would an
/// ordinary file, so one that turns out to have only one name is counted exactly as though its
/// count had been known. Windows lists no link count, so its walker uses this (see
/// `windows::links`).
pub const LINKS_UNKNOWN: u64 = u64::MAX;

/// Why a file's blocks might already have been counted elsewhere.
///
/// Both cases mean the same thing to the model — these blocks can be reached by more than one
/// path, so a folder that reaches them twice must count them once — but they are identified
/// differently and must not be confused: an inode number and a physical block offset are
/// unrelated numbers that would otherwise collide in one map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedBlocks {
    /// Several names for one inode: a hard link.
    Inode(u64),
    /// Several inodes sharing one physical extent: a copy-on-write reflink.
    Extent(u64),
}

impl EntryMeta {
    /// Whether more than one directory entry points at this file, so that the same blocks can be
    /// reached by more than one path and must not be counted twice within a folder.
    #[must_use]
    pub fn is_hardlinked(&self) -> bool {
        !self.is_dir && self.links > 1
    }

    /// How this file's blocks are shared, if they are.
    ///
    /// A reflinked file may also be hard-linked. The extent identity is the stronger of the two —
    /// every hard link to a file reports the same first extent — so it is preferred, which keeps
    /// one file to one identity and one ledger entry.
    #[must_use]
    pub fn shared_blocks(&self) -> Option<SharedBlocks> {
        if self.is_dir {
            return None;
        }
        if self.shared_extent != 0 {
            return Some(SharedBlocks::Extent(self.shared_extent));
        }
        if self.links > 1 {
            return Some(SharedBlocks::Inode(self.inode));
        }
        None
    }
}

/// An entry named relative to the directory that contains it.
///
/// The name is not here: it lives in the owning [`DirEntries`]' packed buffer, and this records
/// where. An `OsString` per entry costs 24 bytes wherever it is stored plus a heap block of its
/// own, and a whole-volume scan makes millions of them only to hand them straight to the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedEntry {
    offset: u32,
    len: u32,
    pub meta: EntryMeta,
}

impl NamedEntry {
    /// Where this entry's name sits in the buffer that owns it.
    #[must_use]
    pub fn name_range(&self) -> ::std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// Every entry of one directory, and the number of its entries that could not be read.
///
/// Directory-at-a-time delivery is what lets the tree builder resolve a parent once per directory
/// instead of once per file. The names are concatenated into one buffer rather than allocated
/// individually, so a directory costs one allocation for all of its names and the tree can take
/// that buffer over without copying it.
#[derive(Debug)]
pub struct DirEntries {
    pub path: Arc<Path>,
    /// Every entry's name, end to end, in entry order.
    names: Vec<u8>,
    entries: Vec<NamedEntry>,
    pub failed: u64,
    /// Entries left for the second pass ([`refine`]): small files that may share extents but were
    /// not worth probing during the walk. Indices into `entries`.
    pub(crate) later: Vec<u32>,
    /// What those entries' extent offsets index; see `linux::Job::extent_space`.
    pub(crate) extent_space: u64,
}

impl DirEntries {
    #[must_use]
    pub fn new(path: Arc<Path>) -> Self {
        Self {
            path,
            names: Vec::new(),
            entries: Vec::new(),
            failed: 0,
            later: Vec::new(),
            extent_space: 0,
        }
    }

    /// A directory known to hold about `entries` entries and `name_bytes` of names between them.
    #[must_use]
    pub fn with_capacity(path: Arc<Path>, entries: usize, name_bytes: usize) -> Self {
        Self {
            path,
            names: Vec::with_capacity(name_bytes),
            entries: Vec::with_capacity(entries),
            failed: 0,
            later: Vec::new(),
            extent_space: 0,
        }
    }

    /// Append an entry, copying its name into the buffer.
    ///
    /// # Panics
    ///
    /// If one directory's names exceed 4 GiB, which no filesystem permits.
    pub fn push(&mut self, name: &OsStr, meta: EntryMeta) {
        let bytes = name.as_encoded_bytes();
        let offset = u32::try_from(self.names.len()).expect("a directory's names fit in 4 GiB");
        let len = u32::try_from(bytes.len()).expect("a name fits in 4 GiB");
        self.names.extend_from_slice(bytes);
        self.entries.push(NamedEntry { offset, len, meta });
    }

    /// The name of an entry belonging to this directory.
    #[must_use]
    pub fn name(&self, entry: &NamedEntry) -> &OsStr {
        // SAFETY: The slice came from `OsStr::as_encoded_bytes()`.
        unsafe { OsStr::from_encoded_bytes_unchecked(&self.names[entry.name_range()]) }
    }

    #[must_use]
    pub fn entries(&self) -> &[NamedEntry] {
        &self.entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry, as a name and its metadata.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &EntryMeta)> {
        self.entries.iter().map(|entry| {
            (
                // SAFETY: The slice came from `OsStr::as_encoded_bytes()`.
                unsafe { OsStr::from_encoded_bytes_unchecked(&self.names[entry.name_range()]) },
                &entry.meta,
            )
        })
    }

    /// Give back the slack both vectors grew while the directory was being read.
    ///
    /// They are filled by pushing, so they double as they go and end up around half again as large
    /// as they need to be — and the tree keeps them for the life of the scan, so that slack is
    /// permanent. One realloc per directory buys it back.
    pub fn shrink(&mut self) {
        self.names.shrink_to_fit();
        self.entries.shrink_to_fit();
    }

    /// Hand over the name buffer and the entries, so the tree can take the buffer rather than
    /// copy out of it.
    #[must_use]
    pub fn into_parts(self) -> (Arc<Path>, Vec<u8>, Vec<NamedEntry>) {
        (self.path, self.names, self.entries)
    }
}

/// What a live view needs to know about a directory while the full tree is still being built
/// elsewhere: which subdirectories it has, and what its files add up to.
///
/// Cheap to make from a [`DirEntries`] and cheap to apply, so the rendering thread can keep up
/// with the scan by outline alone and show every folder with a running size.
#[derive(Debug)]
pub struct DirSummary {
    /// The directory's subdirectory entries only.
    pub dirs: DirEntries,
    /// What the files directly inside add up to on disk, shared blocks counted in full.
    pub files_size: u64,
    /// The same files' lengths.
    pub files_apparent: u64,
    /// How many entries of any kind the directory holds.
    pub entries: u64,
}

impl DirSummary {
    #[must_use]
    pub fn of(directory: &DirEntries) -> Self {
        let mut dirs = DirEntries::new(Arc::clone(&directory.path));
        let mut files_size = 0u64;
        let mut files_apparent = 0u64;
        for (name, meta) in directory.iter() {
            if meta.is_dir {
                dirs.push(name, *meta);
            } else {
                files_size = files_size.saturating_add(meta.size);
                files_apparent = files_apparent.saturating_add(meta.apparent);
            }
        }
        dirs.failed = directory.failed;
        Self {
            dirs,
            files_size,
            files_apparent,
            entries: directory.len() as u64,
        }
    }
}

/// Turns the stream of scanned directories into the outline the live view is built from.
///
/// The view during a scan only ever shows folders near the top of the tree, but the first version
/// of the outline sent every directory to the rendering thread — 385k of them on a 4.2M-entry
/// volume — and applying them saturated that thread for the whole scan, which backed up the
/// dispatcher, which slowed the walk. So the outline stops at [`Outline::DEFAULT_DEPTH`]:
/// directories above it are sent as they are, and everything deeper is rolled up — size, entry
/// count and read failures — into the *frontier* folder just below the cap that contains it.
/// Folders the view can show keep exact running totals; what lies beneath the frontier is
/// unknown until the finished tree arrives.
pub struct Outline {
    root: PathBuf,
    depth: usize,
    batch_size: usize,
    batch: Vec<DirSummary>,
    batched_entries: usize,
    /// Frontier folder (relative to the root) → rolled-up (file bytes, entries, failed).
    /// Per frontier folder: files on disk, their lengths, entries, unreadable entries.
    rolled: ::std::collections::HashMap<PathBuf, (u64, u64, u64, u64)>,
}

impl Outline {
    /// How deep the outline goes. Six levels below the root is past where anyone navigates in the
    /// first second of a scan, and on the volume measured it turns 385k summaries into about 15k.
    pub const DEFAULT_DEPTH: usize = 6;

    #[must_use]
    pub fn new(root: PathBuf, depth: usize, batch_size: usize) -> Self {
        Self {
            root,
            depth,
            batch_size,
            batch: Vec::with_capacity(128),
            batched_entries: 0,
            rolled: ::std::collections::HashMap::new(),
        }
    }

    /// Take one scanned directory in. Returns a batch of summaries when one is ready to send.
    pub fn add(&mut self, directory: &DirEntries) -> Option<Vec<DirSummary>> {
        self.batched_entries += directory.len().max(1);
        let relative = directory
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&directory.path);
        if relative.components().count() <= self.depth {
            self.batch.push(DirSummary::of(directory));
        } else {
            let frontier: PathBuf = relative.components().take(self.depth + 1).collect();
            let (disk, apparent) = directory.iter().filter(|(_, meta)| !meta.is_dir).fold(
                (0u64, 0u64),
                |(disk, apparent), (_, meta)| {
                    (
                        disk.saturating_add(meta.size),
                        apparent.saturating_add(meta.apparent),
                    )
                },
            );
            let slot = self.rolled.entry(frontier).or_insert((0, 0, 0, 0));
            slot.0 = slot.0.saturating_add(disk);
            slot.1 = slot.1.saturating_add(apparent);
            slot.2 += directory.len() as u64;
            slot.3 += directory.failed;
        }
        (self.batched_entries >= self.batch_size).then(|| self.take_batch())
    }

    /// Whatever is left, once the scan is over.
    #[must_use]
    pub fn finish(mut self) -> Vec<DirSummary> {
        self.take_batch()
    }

    fn take_batch(&mut self) -> Vec<DirSummary> {
        self.batched_entries = 0;
        for (frontier, (files_size, files_apparent, entries, failed)) in self.rolled.drain() {
            let mut dirs = DirEntries::new(Arc::from(self.root.join(frontier).as_path()));
            dirs.failed = failed;
            self.batch.push(DirSummary {
                dirs,
                files_size,
                files_apparent,
                entries,
            });
        }
        ::std::mem::replace(&mut self.batch, Vec::with_capacity(128))
    }
}

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
    use crate::model::{FileTree, Folder};

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
        linux::walk_linux(root, thread_count(options), options)
    }
    #[cfg(windows)]
    {
        windows::walk_windows(root, thread_count(options), options)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        fallback::group_by_directory(root, options)
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

/// One step of a directory walk.
#[derive(Debug)]
pub enum ScanItem {
    Entry { meta: EntryMeta, path: PathBuf },
    ReadError,
}

/// Workers for the walker the app actually uses.
pub fn thread_count(options: ScanOptions) -> usize {
    capped(options, MAX_SCAN_THREADS)
}

/// Workers for the `dua-core` walk, wherever it is still reached.
///
/// It needs its own number. The cap below is a property of a walker, not of a machine: raising the
/// native walker's cap to 24 and letting `dua-core` inherit it made the `dua-*` benchmark stages —
/// and `scan_folder`, which is public — 38% slower on this box, by running the one walker that
/// collapses past eight workers at 24 of them.
fn dua_thread_count(options: ScanOptions) -> usize {
    capped(options, MAX_DUA_THREADS)
}

fn capped(options: ScanOptions, cap: usize) -> usize {
    if let Some(threads) = options.threads {
        return threads.max(1);
    }
    if options.parallel {
        std::thread::available_parallelism()
            .map_or(1, NonZero::get)
            .min(cap)
    } else {
        1
    }
}

/// Worker cap for the scan, past which more workers stop paying for themselves.
///
/// The two numbers come from different walkers and do not transfer to each other.
///
/// On macOS the `getattrlistbulk` walk really does contend: a whole-disk scan is fastest at six
/// workers on a 14-core machine. Eight is 3% slower and burns 60% more kernel time, and it gets
/// steadily worse beyond that. The walk is bound by synchronous 4 KiB metadata reads, and past six
/// the extra queue depth is spent contending in the kernel rather than reading.
///
/// On Linux the old cap of eight was a property of the `dua-core` walk, which collapsed past it —
/// not of the kernel, which serves sixteen concurrent walkers at near-linear throughput. With the
/// native walker the curve is flat from twelve workers up: on a 32-core box, XFS and ext4 both
/// bottom out around twenty-four and give up only a few percent by thirty-two.
#[cfg(target_os = "linux")]
const MAX_SCAN_THREADS: usize = 24;
#[cfg(target_os = "macos")]
const MAX_SCAN_THREADS: usize = 6;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const MAX_SCAN_THREADS: usize = 8;

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
        .then(|| crate::os::volume_id(root).unwrap_or_default());
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
        crate::os::link_count(path)
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
        crate::os::size_on_disk_fast(metadata)
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
    crate::os::volume_used(root).map(u128::from)
}

#[cfg(test)]
mod tests;
