//! What a scanner hands the model: the options a walk runs under, and each directory's entries
//! as it is read. The walkers themselves are in `diskonaut-scan`; this is the protocol between
//! them and [`crate::FileTree`], which is why it lives here and not with them.

use ::std::ffi::{OsStr, OsString};
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;

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
    /// Read the filesystem's metadata from its device where the process may — root on ext4 on
    /// Linux, elevated on NTFS on Windows (the master file table) — instead of asking the kernel
    /// for each entry: several times faster, at the price of not seeing the last seconds of
    /// writes. Off for rescans, whose folders are small and want to be current, and with
    /// `--no-device-read`.
    pub read_device: bool,
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
            read_device: true,
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
    /// Entries left for the second pass (`diskonaut_scan::refine`): small files that may share extents but were
    /// not worth probing during the walk. Indices into `entries`.
    /// Filled in by the walker that decided to leave them; nothing else writes it.
    pub later: Vec<u32>,
    /// What those entries' extent offsets index; see `linux::Job::extent_space`.
    pub extent_space: u64,
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

/// One step of a directory walk.
#[derive(Debug)]
pub enum ScanItem {
    Entry { meta: EntryMeta, path: PathBuf },
    ReadError,
}

/// One directory's findings: each small file whose every extent is shared.
#[derive(Debug, Clone)]
pub struct Found {
    pub dir: Arc<Path>,
    pub files: Vec<FoundFile>,
}

/// A small file whose every extent is shared: its name, the sizes it was counted as, and the
/// identity of its blocks.
#[derive(Debug, Clone)]
pub struct FoundFile {
    pub name: OsString,
    pub sizes: crate::model::Sizes,
    pub identity: u64,
}
