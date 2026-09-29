//! What a scanner hands the model: the options a walk runs under, and each directory's entries
//! as it is read. The walkers themselves are in `duscape-scan`; this is the protocol between
//! them and [`crate::FileTree`], which is why it lives here and not with them.

use ::std::ffi::{OsStr, OsString};
use ::std::path::{Path, PathBuf};
use ::std::sync::atomic::{AtomicU64, Ordering};
use ::std::sync::{Arc, Mutex};
use ::std::time::{Duration, Instant};

/// A path from the scan root as `strip_prefix` gives it, less a leading separator.
///
/// The tree's root normally has its separator ([`crate::os::scan_root`]), so what is under
/// it strips to names alone; a root without one — a share's as `canonicalize` names it,
/// handed in by a caller that did not go through [`crate::os::canonical_root`] — leaves `\sub`
/// for its `sub`, whose first component is a root, not a name. Cheap: a path with no root is
/// returned as it is.
#[must_use]
pub fn below_root(relative: &Path) -> &Path {
    if relative.has_root() {
        relative
            .strip_prefix(::std::path::MAIN_SEPARATOR_STR)
            .unwrap_or(relative)
    } else {
        relative
    }
}

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
    /// Do not cross filesystem boundaries (like `du -x`). On btrfs the boundary is the
    /// filesystem, not the subvolume: every subvolume has a device of its own, and `du -x` stops
    /// at each — on a Synology NAS, at every share — where this keeps to the volume's.
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
    /// Walk into read-only btrfs snapshots inside the scan (Linux). Off, each is left empty, as a
    /// mount `-x` leaves is: a snapshot is a whole earlier copy of what the scan already walks —
    /// Synology keeps one per share per hour under `#snapshot` — so walking a few dozen of them
    /// walks the volume a few dozen times, and what they hold of their own (the blocks since
    /// rewritten) is a small part of it. Named as the scan root, a snapshot is scanned.
    ///
    /// Over the network a share shows its snapshots as a plain folder (Synology's `#snapshot`,
    /// QNAP's `@Recently-Snapshot`, ZFS's `.zfs`, NetApp's `.snapshot`): every walker leaves
    /// those empty by name, and this walks them ([`crate::nas::left_out`]).
    pub snapshots: bool,
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
            snapshots: false,
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
    /// Entries left for the second pass (`duscape_scan::refine`): small files that may share extents but were
    /// not worth probing during the walk. Indices into `entries`.
    /// Filled in by the walker that decided to leave them; nothing else writes it.
    pub later: Vec<u32>,
    /// What those entries' extent offsets index; see `linux::Job::extent_space`.
    pub extent_space: u64,
    /// Why what [`Self::failed`] counts failed, for `--issues`; see [`Self::fail`].
    pub issues: Issues,
}

/// A failure a scan counted, kept to be shown (`duscape --issues`): where, what was being done
/// there, and what the system said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: PathBuf,
    /// `open`, `list`, `stat`…
    pub action: &'static str,
    pub error: String,
}

/// The failures of a directory, or of a whole scan: every one counted by its kind (what was
/// being done and the error), a few kept whole as examples. Bounded whatever fails: a scan in
/// which every entry fails — a kernel without a call the walker uses — keeps a few hundred
/// examples and a line per kind, not one per entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issues {
    pub examples: Vec<Issue>,
    /// How many of each kind: `(action, error)`.
    pub kinds: ::std::collections::BTreeMap<(&'static str, String), u64>,
    /// How many in folders of each name — the name of the folder holding what failed: on a
    /// Synology NAS, a million-odd in folders called `@eaDir`, which no list of examples shows.
    pub folders: ::std::collections::BTreeMap<String, u64>,
}

impl Issues {
    /// Examples a scan keeps.
    pub const KEPT: usize = 200;
    /// Examples a directory keeps: what a scan keeps is spread over many.
    pub const KEPT_IN_A_DIRECTORY: usize = 8;
    /// Kinds kept apart; the rest are counted together as [`Self::OTHER`]. An error that names
    /// its path would otherwise make a kind of every failure.
    pub const KINDS: usize = 64;
    /// What the kinds past [`Self::KINDS`] are counted as.
    pub const OTHER: &'static str = "other errors";

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Every failure counted.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.kinds.values().sum()
    }

    /// One failure, kept as an example while there are fewer than `kept`.
    pub fn add(&mut self, issue: Issue, kept: usize) {
        self.count((issue.action, issue.error.clone()), 1);
        let folder = issue.path.parent().and_then(Path::file_name).map_or_else(
            || "/".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.count_folder(folder, 1);
        if self.examples.len() < kept {
            self.examples.push(issue);
        }
    }

    /// `count` more of `kind`, or of the action's [`Self::OTHER`] once there are
    /// [`Self::KINDS`] kinds.
    fn count(&mut self, kind: (&'static str, String), count: u64) {
        let kind = if self.kinds.len() < Self::KINDS || self.kinds.contains_key(&kind) {
            kind
        } else {
            (kind.0, Self::OTHER.to_string())
        };
        *self.kinds.entry(kind).or_default() += count;
    }

    /// `count` more in folders named `name`, or in [`Self::OTHER`] folders once there are
    /// [`Self::KINDS`] names.
    fn count_folder(&mut self, name: String, count: u64) {
        let name = if self.folders.len() < Self::KINDS || self.folders.contains_key(&name) {
            name
        } else {
            Self::OTHER.to_string()
        };
        *self.folders.entry(name).or_default() += count;
    }

    /// Another's failures added to these.
    pub fn merge(&mut self, other: Issues) {
        for (kind, count) in other.kinds {
            self.count(kind, count);
        }
        for (name, count) in other.folders {
            self.count_folder(name, count);
        }
        let room = Self::KEPT.saturating_sub(self.examples.len());
        self.examples.extend(other.examples.into_iter().take(room));
    }

    /// What `duscape --issues` prints: every kind with its count, most first, then the examples.
    #[must_use]
    pub fn report(&self) -> String {
        use ::std::fmt::Write;
        let mut out = String::new();
        if self.is_empty() {
            out.push_str("Nothing failed to read, and nothing was left out.\n");
            return out;
        }
        let total = self.total();
        let noun = if total == 1 { "issue" } else { "issues" };
        let _ = writeln!(out, "{total} {noun}, by kind:");
        let mut kinds: Vec<_> = self.kinds.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for ((action, error), count) in kinds {
            let _ = writeln!(out, "  {count:>10}  {action}: {error}");
        }
        // Where they gather, by the name of the folder holding them: the ten commonest.
        let mut folders: Vec<_> = self.folders.iter().collect();
        folders.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let _ = writeln!(out, "By the name of the folder they are in:");
        for (name, count) in folders.iter().take(10) {
            let _ = writeln!(out, "  {count:>10}  {name}");
        }
        if folders.len() > 10 {
            let rest: u64 = folders[10..].iter().map(|(_, count)| **count).sum();
            let _ = writeln!(out, "  {rest:>10}  in {} other names", folders.len() - 10);
        }
        let _ = writeln!(
            out,
            "Where{}:",
            if total > self.examples.len() as u64 {
                format!(
                    ", the first {} (at most {} a folder)",
                    self.examples.len(),
                    Self::KEPT_IN_A_DIRECTORY
                )
            } else {
                String::new()
            }
        );
        for issue in &self.examples {
            let _ = writeln!(
                out,
                "  {}: {}: {}",
                issue.path.display(),
                issue.action,
                issue.error
            );
        }
        out
    }
}

impl DirEntries {
    /// Something the walk did not do here, and why — a folder not walked, not a read that failed:
    /// kept for `--issues` as a failure is, but not counted in [`Self::failed`].
    /// Whether the folder `name`, an entry of this directory that the walk `would_enter`, is
    /// left out by name instead — a share's snapshots, unless `snapshots`
    /// ([`crate::nas::left_out`]) — and noted here (kind `left out`) for `--issues`. One rule
    /// for every walker. A folder the depth cap already keeps out is not noted, since
    /// `--snapshots` would not walk it either.
    pub fn leave_out(&mut self, name: &OsStr, would_enter: bool, snapshots: bool) -> bool {
        if !would_enter {
            return false;
        }
        match crate::nas::left_out(name, snapshots) {
            Some(known) => {
                self.note("left out", Some(name), crate::nas::left_out_note(known));
                true
            }
            None => false,
        }
    }

    pub fn note(
        &mut self,
        action: &'static str,
        name: Option<&OsStr>,
        why: impl ::std::fmt::Display,
    ) {
        let path = match name {
            Some(name) => self.path.join(name),
            None => self.path.to_path_buf(),
        };
        self.issues.add(
            Issue {
                path,
                action,
                error: why.to_string(),
            },
            Issues::KEPT_IN_A_DIRECTORY,
        );
    }

    /// Count a failure here — of the directory itself (`name` is `None`) or of one of its
    /// entries — and keep why, for `--issues`.
    pub fn fail(
        &mut self,
        action: &'static str,
        name: Option<&OsStr>,
        error: impl ::std::fmt::Display,
    ) {
        self.failed += 1;
        let path = match name {
            Some(name) => self.path.join(name),
            None => self.path.to_path_buf(),
        };
        self.issues.add(
            Issue {
                path,
                action,
                error: error.to_string(),
            },
            Issues::KEPT_IN_A_DIRECTORY,
        );
    }

    #[must_use]
    pub fn new(path: Arc<Path>) -> Self {
        Self {
            path,
            names: Vec::new(),
            entries: Vec::new(),
            failed: 0,
            later: Vec::new(),
            extent_space: 0,
            issues: Issues::default(),
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
            issues: Issues::default(),
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

    /// The failures' reasons, taken out, leaving none.
    pub fn take_issues(&mut self) -> Issues {
        ::std::mem::take(&mut self.issues)
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

    /// Every entry, the files too: for the folder the user is in, so that what it holds shows
    /// as soon as the walk reaches it rather than when the scan ends. The files' sizes are
    /// summed as in [`DirSummary::of`]; the tree adds them once, through those sums.
    #[must_use]
    pub fn whole(directory: &DirEntries) -> Self {
        let mut summary = Self::of(directory);
        let mut all = DirEntries::new(Arc::clone(&directory.path));
        for (name, meta) in directory.iter() {
            all.push(name, *meta);
        }
        all.failed = directory.failed;
        summary.dirs = all;
        summary
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
///
/// The folder the user has gone into ([`Focus`], `following`) is the exception: what is under
/// it is sent whole to the same depth again, counted from it, since that is what is on screen;
/// the folder itself comes with its files, if the walk reads it after the user got there; and a
/// batch holding something under it goes at once — [`Outline::FOCUS_FLUSH`] after the last —
/// rather than when it fills.
pub struct Outline {
    root: PathBuf,
    depth: usize,
    batch_size: usize,
    batch: Vec<DirSummary>,
    batched_entries: usize,
    /// Frontier folder (relative to the root) → rolled-up (file bytes, entries, failed).
    /// Per frontier folder: files on disk, their lengths, entries, unreadable entries.
    rolled: ::std::collections::HashMap<PathBuf, (u64, u64, u64, u64)>,
    /// The folder the user is in, if any: exempt from the depth cap.
    focus: FocusWatch,
    /// When the last batch went, for the early one under the focus.
    last_batch: Instant,
}

impl Outline {
    /// How deep the outline goes. Six levels below the root is past where anyone navigates in the
    /// first second of a scan, and on the volume measured it turns 385k summaries into about 15k.
    pub const DEFAULT_DEPTH: usize = 6;
    /// How soon after the last batch one holding a directory under the focus is sent, full or
    /// not: the folder on screen fills in as it is read, at no more than ten batches a second.
    pub const FOCUS_FLUSH: Duration = Duration::from_millis(100);

    /// `root` is named as the scan names it ([`crate::os::scan_root`]): a share's root given
    /// bare would leave the focus, which the tree names with the separator, never equal to it,
    /// and the whole scan "under the focus".
    #[must_use]
    pub fn new(root: PathBuf, depth: usize, batch_size: usize) -> Self {
        let root = crate::os::scan_root(root);
        let focus = Focus::default().watch_under(&root);
        Self {
            root,
            depth,
            batch_size,
            batch: Vec::with_capacity(128),
            batched_entries: 0,
            rolled: ::std::collections::HashMap::new(),
            focus,
            last_batch: Instant::now(),
        }
    }

    /// Follow `focus`, the folder the user is in, as described above.
    #[must_use]
    pub fn following(mut self, focus: &Focus) -> Self {
        self.focus = focus.watch_under(&self.root);
        self
    }

    /// Take one scanned directory in. Returns a batch of summaries when one is ready to send.
    pub fn add(&mut self, directory: &DirEntries) -> Option<Vec<DirSummary>> {
        self.batched_entries += directory.len().max(1);
        let relative = below_root(
            directory
                .path
                .strip_prefix(&self.root)
                .unwrap_or(&directory.path),
        );
        self.focus.refresh();
        // Under the folder the user is in, the cap is that folder's depth and the usual depth
        // again, so its own entries and those below show; the folder itself with its files.
        let focus = self
            .focus
            .current()
            .and_then(|focus| focus.strip_prefix(&self.root).ok())
            .map(below_root)
            .filter(|focus| relative.starts_with(focus));
        let (cap, at_focus) = match focus {
            Some(focus) => (self.depth + focus.components().count(), relative == focus),
            None => (self.depth, false),
        };
        if relative.components().count() <= cap {
            self.batch.push(if at_focus {
                DirSummary::whole(directory)
            } else {
                DirSummary::of(directory)
            });
        } else {
            let frontier: PathBuf = relative.components().take(cap + 1).collect();
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
        let due = self.batched_entries >= self.batch_size
            || (focus.is_some() && self.last_batch.elapsed() >= Self::FOCUS_FLUSH);
        due.then(|| self.take_batch())
    }

    /// Whatever is left, once the scan is over.
    #[must_use]
    pub fn finish(mut self) -> Vec<DirSummary> {
        self.take_batch()
    }

    fn take_batch(&mut self) -> Vec<DirSummary> {
        self.batched_entries = 0;
        self.last_batch = Instant::now();
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

/// Where the user is looking while a scan runs: the folder shown, set by the viewer as they
/// move and read by whatever is still working — the walkers, which take the directories on the
/// way down to it and under it before any other; the [`Outline`], which sends what is under it
/// whole where it would roll deeper folders up, and the folder's own files; and the second pass.
/// One is shared by cloning (an `Arc` inside). `None`, or the scan root, is nowhere in
/// particular. A reader keeps a [`FocusWatch`], which tells a move by one atomic load.
#[derive(Clone, Debug, Default)]
pub struct Focus {
    inner: Arc<FocusInner>,
}

#[derive(Debug, Default)]
struct FocusInner {
    path: Mutex<Option<PathBuf>>,
    /// Counts the changes, so a watcher can tell one without the lock.
    generation: AtomicU64,
}

impl Focus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The folder shown now. Set to what it already is, nothing moves.
    pub fn set(&self, path: Option<PathBuf>) {
        let mut current = self.inner.path.lock().unwrap_or_else(|e| e.into_inner());
        if *current != path {
            *current = path;
            self.inner.generation.fetch_add(1, Ordering::Release);
        }
    }

    #[must_use]
    pub fn get(&self) -> Option<PathBuf> {
        self.inner
            .path
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How many times the focus has moved.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// A reader's view of the focus for a scan of `root`: a focus at the root itself, or off
    /// it, is nowhere in particular.
    #[must_use]
    pub fn watch_under(&self, root: &Path) -> FocusWatch {
        let mut watch = FocusWatch {
            focus: self.clone(),
            root: root.to_path_buf(),
            seen: u64::MAX,
            current: None,
        };
        watch.refresh();
        watch
    }
}

/// One reader's view of a [`Focus`]: the folder as of the last [`FocusWatch::refresh`], which
/// costs an atomic load until the focus has moved.
#[derive(Debug)]
pub struct FocusWatch {
    focus: Focus,
    root: PathBuf,
    seen: u64,
    current: Option<PathBuf>,
}

impl FocusWatch {
    /// Bring the view up to date. Whether the focus moved since last asked.
    pub fn refresh(&mut self) -> bool {
        let generation = self.focus.generation();
        if generation == self.seen {
            return false;
        }
        self.seen = generation;
        self.current = self
            .focus
            .get()
            .filter(|path| path != &self.root && path.starts_with(&self.root));
        true
    }

    /// The folder in focus, as of the last refresh.
    #[must_use]
    pub fn current(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    /// Whether `path` is toward the folder in focus: under it, or on the way down to it.
    #[must_use]
    pub fn toward(&self, path: &Path) -> bool {
        self.current
            .as_deref()
            .is_some_and(|focus| path.starts_with(focus) || focus.starts_with(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An outline of a share given its root bare, as `canonicalize` names it, with the focus
    /// on the root as the tree names it: the root is nowhere in particular, so it is outlined
    /// as any root is, its files summed and not sent.
    #[cfg(windows)]
    #[test]
    fn an_outline_names_its_root_as_the_scan_does() {
        let bare = PathBuf::from(r"\\?\UNC\server\share");
        let focus = Focus::new();
        focus.set(Some(PathBuf::from(r"\\?\UNC\server\share\")));
        let mut outline = Outline::new(bare.clone(), 6, usize::MAX).following(&focus);
        let mut root = DirEntries::new(Arc::from(bare.as_path()));
        root.push(
            OsStr::new("sub"),
            EntryMeta {
                is_dir: true,
                ..EntryMeta::default()
            },
        );
        root.push(
            OsStr::new("top.txt"),
            EntryMeta {
                size: 10,
                apparent: 10,
                links: 1,
                ..EntryMeta::default()
            },
        );
        assert!(outline.add(&root).is_none());
        let batch = outline.finish();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].files_size, 10);
        assert_eq!(
            batch[0].dirs.len(),
            1,
            "the folder only: the root's files are not sent"
        );
    }

    /// A path from the root as `strip_prefix` gives it, less a leading separator: `\sub`,
    /// what is left under a share root named without its own, is `sub`.
    /// The focus is what its setter last said, and a watcher tells a move without the lock —
    /// a move to the scan root, or off it, counting as nowhere.
    #[test]
    fn a_focus_is_watched_by_generation_and_read_under_the_root() {
        let root = Path::new("/scan");
        let focus = Focus::new();
        let mut watch = focus.watch_under(root);
        assert!(!watch.refresh(), "nothing has moved");
        assert_eq!(watch.current(), None);

        focus.set(Some(root.join("a").join("b")));
        focus.set(Some(root.join("a").join("b")));
        assert_eq!(focus.generation(), 1, "the same folder again is no move");
        assert!(watch.refresh());
        assert_eq!(watch.current(), Some(root.join("a").join("b").as_path()));
        assert!(!watch.refresh());
        assert!(watch.toward(&root.join("a")), "on the way down to it");
        assert!(
            watch.toward(&root.join("a").join("b").join("c")),
            "under it"
        );
        assert!(!watch.toward(&root.join("a").join("x")));
        assert!(!watch.toward(&root.join("other")));

        focus.set(Some(root.to_path_buf()));
        assert!(watch.refresh());
        assert_eq!(watch.current(), None, "the root is nowhere in particular");
        focus.set(Some(PathBuf::from("/elsewhere")));
        assert!(watch.refresh());
        assert_eq!(watch.current(), None, "off the scan is nowhere");
        assert!(!watch.toward(root));
    }

    #[test]
    fn below_root_takes_off_a_leading_separator_and_nothing_else() {
        assert_eq!(below_root(Path::new("sub")), Path::new("sub"));
        assert_eq!(below_root(Path::new("")), Path::new(""));
        let rooted = format!("{}sub", ::std::path::MAIN_SEPARATOR);
        assert_eq!(below_root(Path::new(&rooted)), Path::new("sub"));
        let deeper = format!("{0}sub{0}inner", ::std::path::MAIN_SEPARATOR);
        assert_eq!(
            below_root(Path::new(&deeper)),
            Path::new(&deeper[1..]),
            "the components after the separator are kept"
        );
    }

    #[test]
    fn every_failure_is_counted_and_a_few_are_kept_whole() {
        let mut directory = DirEntries::new(Arc::from(Path::new("/volume1/share")));
        for index in 0..20 {
            let name = format!("file{index}");
            directory.fail(
                "stat",
                Some(OsStr::new(&name)),
                "Function not implemented (os error 38)",
            );
        }
        directory.fail("list", None, "Input/output error (os error 5)");
        assert_eq!(directory.failed, 21);
        assert_eq!(directory.issues.total(), 21);
        assert_eq!(directory.issues.examples.len(), Issues::KEPT_IN_A_DIRECTORY);
        assert_eq!(
            directory.issues.examples[0].path,
            Path::new("/volume1/share").join("file0")
        );

        // A whole scan of such directories keeps its examples bounded, and every count.
        let mut scan = Issues::default();
        for _ in 0..100 {
            let mut again = directory.issues.clone();
            again.examples.truncate(Issues::KEPT_IN_A_DIRECTORY);
            scan.merge(again);
        }
        assert_eq!(scan.total(), 2100);
        assert_eq!(scan.examples.len(), Issues::KEPT);
        let report = scan.report();
        assert!(report.starts_with("2100 issues, by kind:"), "{report}");
        // The most frequent kind first.
        let stat = report
            .find("stat: Function not implemented")
            .expect("the stat kind");
        let list = report
            .find("list: Input/output error")
            .expect("the list kind");
        assert!(stat < list, "{report}");
        assert!(
            report.contains("Where, the first 200 (at most 8 a folder):"),
            "{report}"
        );
    }

    #[test]
    fn failures_are_gathered_by_the_name_of_their_folder() {
        let mut issues = Issues::default();
        for share in ["homes", "photos", "backup"] {
            let mut directory = DirEntries::new(Arc::from(
                Path::new("/volume1").join(share).join("@eaDir").as_path(),
            ));
            for index in 0..100 {
                let name = format!("f{index}@SynoEAStream");
                directory.fail(
                    "stat",
                    Some(OsStr::new(&name)),
                    "Permission denied (os error 13)",
                );
            }
            issues.merge(directory.take_issues());
        }
        let mut locked = DirEntries::new(Arc::from(Path::new("/volume1/@apphome/git")));
        locked.fail("open", None, "Permission denied (os error 13)");
        issues.merge(locked.take_issues());
        assert_eq!(issues.folders["@eaDir"], 300);
        // A folder that cannot be opened is counted where it is: in @apphome.
        assert_eq!(issues.folders["@apphome"], 1);
        let report = issues.report();
        let folders = report
            .split("By the name of the folder they are in:\n")
            .nth(1)
            .expect("the folders section");
        assert!(folders.starts_with("         300  @eaDir\n"), "{report}");
    }

    #[test]
    fn a_note_is_kept_but_not_counted_as_a_failure() {
        let mut directory = DirEntries::new(Arc::from(Path::new("/volume1")));
        directory.note(
            "loop",
            Some(OsStr::new("@docker")),
            "a mount of /volume1 inside itself",
        );
        assert_eq!(directory.failed, 0);
        assert_eq!(directory.issues.total(), 1);
        assert_eq!(
            directory.issues.examples[0].path,
            Path::new("/volume1/@docker")
        );
    }

    #[test]
    fn errors_that_name_their_path_do_not_make_a_kind_each() {
        let mut issues = Issues::default();
        for index in 0..1000 {
            issues.add(
                Issue {
                    path: PathBuf::from(format!("/d/{index}")),
                    action: "walk",
                    error: format!("IO error for operation on /d/{index}: Permission denied"),
                },
                Issues::KEPT,
            );
        }
        assert_eq!(issues.total(), 1000);
        assert!(
            issues.kinds.len() <= Issues::KINDS + 1,
            "{}",
            issues.kinds.len()
        );
        assert_eq!(
            issues.kinds[&("walk", Issues::OTHER.to_string())],
            1000 - 64
        );
    }

    #[test]
    fn a_scan_with_no_failures_says_so() {
        assert_eq!(
            Issues::default().report(),
            "Nothing failed to read, and nothing was left out.\n"
        );
        let mut one = Issues::default();
        one.add(
            Issue {
                path: PathBuf::from("/x"),
                action: "open",
                error: "Permission denied (os error 13)".to_string(),
            },
            Issues::KEPT,
        );
        assert!(
            one.report().starts_with("1 issue, by kind:"),
            "{}",
            one.report()
        );
        assert!(
            one.report()
                .contains("Where:\n  /x: open: Permission denied"),
            "{}",
            one.report()
        );
    }
}
