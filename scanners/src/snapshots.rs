//! A volume's local snapshots read for what they hold that the live volume does not — the
//! idle pass after every other (`rescan::Rescans::start_idle`), which fills the snapshots'
//! folder `libduscape::snapshots` describes.
//!
//! Each snapshot is mounted read-only in a private folder (root only: unprivileged the mount
//! is refused), read, and unmounted. Its files are set against the live volume by *file id*:
//! APFS keeps a file's id in every snapshot of it, and never gives an id to another file
//! (ids only grow), so a snapshot's file whose id is nowhere on the live volume is one deleted
//! since, its blocks held by the snapshot alone. One whose id is still live — where it was, or
//! moved or renamed anywhere — shares its blocks with the live file, except those written over
//! in place since: a file rewritten in place (a virtual machine's disk, a database) keeps its
//! id, and the snapshot keeps the blocks it had. Those are counted by where they are on the
//! disk ([`extents`]): the snapshot file's physical extents less the live file's, less what
//! another snapshot already counted. The live volume is asked by id through volfs
//! (`/.vol/<device>/<id>`, no privilege needed).
//!
//! Which folders to read ([`Scope`]) is the volume's change log's to say ([`log`]): a file
//! deleted, renamed away or written since the snapshot is an event on its folder, so only the
//! folders named since can hold anything the snapshot alone keeps — and a write leaves a
//! folder's time as it was, so without the log a file written in place cannot be found. Where
//! the log does not reach back to the snapshot, or lost events under the volume's root since,
//! the whole snapshot is read, every folder listed (one whose time is its live counterpart's
//! holds the same entries, and is not set against it), and what was written in place is not
//! counted.
//!
//! What this cannot see (`docs/sizes.md`, "Local snapshots"): a deleted file that was a clone
//! of a live one is counted in full, though its blocks are the live one's; a deleted file kept
//! by several snapshots at different sizes is counted once at each size, since the ledger takes
//! two sizes for two files.
//!
//! The walk and the tree are platform-independent ([`gone_from_live`], [`build`]) and tested
//! with plain folders standing in for a snapshot and its volume; only listing the snapshots,
//! mounting one, the log, the extents and the volfs lookups are macOS's.

use ::std::collections::{HashMap, HashSet};
use ::std::ffi::{OsStr, OsString};
use ::std::io;
use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc::sync_channel;
use ::std::sync::{Arc, Condvar, Mutex};
use ::std::time::{Duration, Instant, SystemTime};

use libduscape::model::{FileTree, Folder};
use libduscape::scan::{DirEntries, EntryMeta, LINKS_UNKNOWN};
use libduscape::snapshots::FOLDER;

pub mod extents;
pub mod log;
#[cfg(target_os = "macos")]
mod macos;

/// How the walk reads both sides: a folder listed as the walk lists one (each entry's `inode`
/// its file id), and the live volume asked by id.
pub struct Lookups<'a> {
    /// A folder's entries, of the snapshot or of the live volume.
    pub list: &'a (dyn Fn(&Path) -> io::Result<DirEntries> + Sync),
    /// The live folder of this id, where it is now, if it is still a folder there.
    pub live_folder: &'a (dyn Fn(u64) -> Option<PathBuf> + Sync),
    /// Whether anything of this id is on the live volume.
    pub live: &'a (dyn Fn(u64) -> bool + Sync),
    /// Whether the live volume has this id at this path from its root: a folder where it was.
    pub same_place: &'a (dyn Fn(&Path, u64) -> bool + Sync),
    /// For a file of the snapshot (its path) whose id is still live: the bytes the snapshot
    /// alone holds of it, written over in the live file since. Zero when it is the same.
    pub written_over: &'a (dyn Fn(&Path, u64) -> u64 + Sync),
}

/// What stays the same through a pass: the live volume's root and how it is asked, and how
/// many threads read while `keep_going` says so.
pub struct Pass<'a> {
    pub live_root: &'a Path,
    pub lookups: &'a Lookups<'a>,
    pub threads: usize,
    pub keep_going: &'a (dyn Fn() -> bool + Sync),
}

/// A snapshot as the volume lists it: its name, when it was made, and whether macOS has
/// trimmed it to its metadata (`tmutil listlocalsnapshots` says "dataless"): such a snapshot
/// still lists every file at its size, and holds none of their blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: OsString,
    pub made: SystemTime,
    pub dataless: bool,
}

/// Which folders of a snapshot to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// None: the snapshot is dataless, and holds no blocks of its own. Read, it listed 267.6 GB
    /// of files gone from the disk on a volume whose snapshots could hold 142 GB at most
    /// (2026-10-05).
    Dataless,
    /// All of it, and why: the log does not reach back to the snapshot, or lost events.
    Whole(String),
    /// Those the log named since the snapshot (`since`, the log's id then), relative to the
    /// volume's root: `listed` each read one level, and `walked` whole, where the log lost
    /// events under them.
    Changed {
        since: u64,
        /// How many of the log's events came after it, for `--issues`.
        events: usize,
        listed: Vec<PathBuf>,
        walked: Vec<PathBuf>,
    },
}

/// What [`gone_from_live`] went through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Walked {
    /// Folders of the snapshot listed.
    pub folders: u64,
    /// Of those, the ones set against the live volume (the rest were the same by time).
    pub compared: u64,
    /// Files found in the snapshot alone, and their size on disk.
    pub kept: u64,
    pub kept_bytes: u64,
    /// Files still live but written over in place since, and the bytes the snapshot alone
    /// holds of them.
    pub written: u64,
    pub written_bytes: u64,
    /// Folders that could not be listed.
    pub failed: u64,
    /// Of the files of the folders compared, those found live by id: none at all would say the
    /// snapshot's ids are not the live volume's.
    pub live: u64,
    /// Of the files kept as gone, those that are APFS clones, and their size: a clone's blocks
    /// may be a live twin's, and then the snapshot does not hold them alone.
    pub cloned: u64,
    pub cloned_bytes: u64,
}

impl Walked {
    fn add(&mut self, other: &Walked) {
        self.folders += other.folders;
        self.compared += other.compared;
        self.kept += other.kept;
        self.kept_bytes += other.kept_bytes;
        self.written += other.written;
        self.written_bytes += other.written_bytes;
        self.failed += other.failed;
        self.live += other.live;
        self.cloned += other.cloned;
        self.cloned_bytes += other.cloned_bytes;
    }
}

/// A folder of the snapshot to look at: where it is mounted, where it is from the volume's
/// root, its live counterpart's path if known (the root's, which is asked by path: a volume's
/// root does not answer to its id the way its folders do), and whether its every subfolder is
/// to be looked at too (`whole`) or only those moved or gone since.
struct Job {
    path: PathBuf,
    relative: PathBuf,
    live: Option<PathBuf>,
    id: u64,
    whole: bool,
}

/// Folders still to look at, and how many workers are looking at one; empty and none means done.
struct Queue {
    state: Mutex<(Vec<Job>, usize)>,
    wake: Condvar,
}

impl Queue {
    fn pop(&self) -> Option<Job> {
        let mut state = self.state.lock().ok()?;
        loop {
            if let Some(job) = state.0.pop() {
                state.1 += 1;
                return Some(job);
            }
            if state.1 == 0 {
                return None;
            }
            state = self.wake.wait(state).ok()?;
        }
    }

    fn finish(&self, children: Vec<Job>, stop: bool) {
        if let Ok(mut state) = self.state.lock() {
            if stop {
                state.0.clear();
            } else {
                state.0.extend(children);
            }
            state.1 -= 1;
        }
        self.wake.notify_all();
    }
}

/// What the walk shares between its workers: the rules, and the folders already looked at.
struct Walk<'a> {
    into: &'a Path,
    lookups: &'a Lookups<'a>,
    /// The folders the log named: compared whatever their time, files written over counted.
    named: HashSet<PathBuf>,
    /// Folders looked at already, by path from the root, and whether their every subfolder
    /// was queued: start points and the subtrees walked from them meet, and a folder added to
    /// the tree twice would place its entries twice.
    seen: Mutex<HashMap<PathBuf, bool>>,
}

/// Whether a folder is looked at, by what was seen of it before.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    /// Not yet: all of it.
    First,
    /// Seen as a folder the log named, whose subfolders were taken only where moved or gone,
    /// and now reached by a walk whole (the log lost events under it): its subfolders all.
    Deeper,
    /// Seen as much already: nothing.
    Again,
}

impl Walk<'_> {
    fn visit(&self, job: &Job) -> Visit {
        let Ok(mut seen) = self.seen.lock() else {
            return Visit::Again;
        };
        match seen.get(&job.relative) {
            None => {
                seen.insert(job.relative.clone(), job.whole);
                Visit::First
            }
            Some(false) if job.whole => {
                seen.insert(job.relative.clone(), true);
                Visit::Deeper
            }
            Some(_) => Visit::Again,
        }
    }

    /// [`Self::look_at`] as `visit` says, a panic in it taken as a folder that could not be
    /// read: a worker that died would leave the others waiting for it, the snapshot mounted.
    fn look_at_once(&self, job: &Job) -> (Walked, Option<DirEntries>, Vec<Job>) {
        let visit = self.visit(job);
        if visit == Visit::Again {
            return (Walked::default(), None, Vec::new());
        }
        let looked = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
            self.look_at(job, visit == Visit::Deeper)
        }));
        looked.unwrap_or_else(|_| {
            let failed = Walked {
                failed: 1,
                ..Walked::default()
            };
            (failed, None, Vec::new())
        })
    }
}

/// The first folders to look at for `scope`: the named ones last, so they are taken first and
/// each is looked at as named before a walk reaches it.
fn start(snapshot: &Path, live_root: &Path, scope: &Scope) -> Vec<Job> {
    let job = |relative: &Path, whole: bool| {
        let path = snapshot.join(relative);
        // A folder made since the snapshot is not in it: nothing to look at.
        let id = file_id(&path)?;
        Some(Job {
            live: relative
                .as_os_str()
                .is_empty()
                .then(|| live_root.to_path_buf()),
            path,
            relative: relative.to_path_buf(),
            id,
            whole,
        })
    };
    match scope {
        Scope::Dataless => Vec::new(),
        Scope::Whole(_) => job(Path::new(""), true).into_iter().collect(),
        Scope::Changed { listed, walked, .. } => walked
            .iter()
            .filter_map(|relative| job(relative, true))
            .chain(listed.iter().filter_map(|relative| job(relative, false)))
            .collect(),
    }
}

/// Read the snapshot mounted at `snapshot` where `scope` says, and hand `report` a
/// [`DirEntries`] for each folder holding what the snapshot alone keeps, at its place under
/// `into`: files gone from the live volume, each with [`LINKS_UNKNOWN`] so the tree's ledger
/// counts one kept by several snapshots once above them, and files written over since, each
/// sized to the bytes the snapshot alone holds of it.
pub fn gone_from_live(
    snapshot: &Path,
    into: &Path,
    scope: &Scope,
    pass: &Pass<'_>,
    mut report: impl FnMut(DirEntries),
) -> Walked {
    let Pass {
        live_root,
        lookups,
        threads,
        keep_going,
    } = *pass;
    let walk = Walk {
        into,
        lookups,
        named: match scope {
            Scope::Whole(_) | Scope::Dataless => HashSet::new(),
            Scope::Changed { listed, .. } => listed.iter().cloned().collect(),
        },
        seen: Mutex::new(HashMap::new()),
    };
    let queue = Queue {
        state: Mutex::new((start(snapshot, live_root, scope), 0)),
        wake: Condvar::new(),
    };
    let walked = Mutex::new(Walked::default());
    let (sender, receiver) = sync_channel::<DirEntries>(threads.max(1) * 4);
    ::std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, 32) {
            let sender = sender.clone();
            let (queue, walked, walk) = (&queue, &walked, &walk);
            scope.spawn(move || {
                while let Some(job) = queue.pop() {
                    let (looked, kept, children) = walk.look_at_once(&job);
                    if let Ok(mut walked) = walked.lock() {
                        walked.add(&looked);
                    }
                    let sent = kept.is_none_or(|kept| sender.send(kept).is_ok());
                    queue.finish(children, !sent || !keep_going());
                }
            });
        }
        drop(sender);
        for kept in receiver {
            report(kept);
        }
    });
    walked.into_inner().unwrap_or_default()
}

impl Walk<'_> {
    /// One folder of the snapshot: what it went through, what it keeps, and the folders inside
    /// it to look at next — only those, when it was compared already (`children_only`).
    fn look_at(&self, job: &Job, children_only: bool) -> (Walked, Option<DirEntries>, Vec<Job>) {
        let mut walked = Walked::default();
        let listing = match (self.lookups.list)(&job.path) {
            Ok(listing) => listing,
            // Gone since it was queued, or never there: nothing to count.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (walked, None, Vec::new());
            }
            Err(_) => {
                walked.failed = 1;
                return (walked, None, Vec::new());
            }
        };
        let children = self.children(job, &listing);
        if children_only {
            return (walked, None, children);
        }
        walked.folders = 1;
        let named = self.named.contains(&job.relative);
        let live = job
            .live
            .clone()
            .or_else(|| (self.lookups.live_folder)(job.id));
        if !named
            && let Some(live) = &live
            && modified(live).is_some_and(|time| Some(time) == modified(&job.path))
        {
            return (walked, None, children);
        }
        walked.compared = 1;
        let kept = self.compare(job, &listing, live.as_deref(), named, &mut walked);
        (walked, (!kept.is_empty()).then_some(kept), children)
    }

    /// The folders inside `job`'s to look at: every one in a folder walked whole; in a folder
    /// the log named, those moved or gone since, whose subtrees the log named at another path
    /// or not at all.
    fn children(&self, job: &Job, listing: &DirEntries) -> Vec<Job> {
        listing
            .iter()
            .filter(|(_, meta)| meta.is_dir)
            .filter(|(name, meta)| {
                job.whole || !(self.lookups.same_place)(&job.relative.join(name), meta.inode)
            })
            .map(|(name, meta)| Job {
                path: job.path.join(name),
                relative: job.relative.join(name),
                live: None,
                id: meta.inode,
                whole: true,
            })
            .collect()
    }

    /// `listing` set against the live volume: the files the snapshot alone keeps, and in a
    /// folder the log `named`, what of each live one was written over since.
    fn compare(
        &self,
        job: &Job,
        listing: &DirEntries,
        live: Option<&Path>,
        named: bool,
        walked: &mut Walked,
    ) -> DirEntries {
        // The live folder's own ids first, one listing for the lot; what is not there may have
        // moved anywhere, and is asked by id.
        let here: HashSet<u64> = live
            .and_then(|live| (self.lookups.list)(live).ok())
            .map(|live| live.iter().map(|(_, meta)| meta.inode).collect())
            .unwrap_or_default();
        let mut kept = DirEntries::new(Arc::from(self.into.join(&job.relative)));
        for (name, meta) in listing.iter() {
            if meta.is_dir {
                continue;
            }
            if here.contains(&meta.inode) || (self.lookups.live)(meta.inode) {
                walked.live += 1;
                let bytes = if named {
                    (self.lookups.written_over)(&job.path.join(name), meta.inode)
                } else {
                    0
                };
                if bytes > 0 {
                    walked.written += 1;
                    walked.written_bytes += bytes;
                    // Sized to what the snapshot alone holds, which the extents have kept
                    // apart from every other snapshot's: counted as it is, not by id.
                    kept.push(name, written_over_entry(bytes));
                }
                continue;
            }
            walked.kept += 1;
            walked.kept_bytes += meta.size;
            if meta.shared_extent != 0 {
                walked.cloned += 1;
                walked.cloned_bytes += meta.size;
            }
            kept.push(
                name,
                EntryMeta {
                    links: LINKS_UNKNOWN,
                    // A clone's identity is its mount's device and clone id, which differ from
                    // one snapshot's mount to the next: the file id is what the snapshots share.
                    shared_extent: 0,
                    ..*meta
                },
            );
        }
        kept
    }
}

/// A file written over since the snapshot, as its folder in the tree holds it: the bytes the
/// snapshot alone holds of it, both sizes alike (two far apart would box them, for nothing).
fn written_over_entry(bytes: u64) -> EntryMeta {
    EntryMeta {
        size: bytes,
        apparent: bytes,
        links: 1,
        ..EntryMeta::default()
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    ::std::fs::symlink_metadata(path).ok()?.modified().ok()
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<u64> {
    use ::std::os::unix::fs::MetadataExt;
    Some(::std::fs::symlink_metadata(path).ok()?.ino())
}

/// Elsewhere no file ids: no snapshot is read there.
#[cfg(not(unix))]
fn file_id(path: &Path) -> Option<u64> {
    ::std::fs::symlink_metadata(path).ok().map(|_| 0)
}

/// A snapshot made readable: its root, and what undoes that when dropped.
pub struct Mounted {
    pub root: PathBuf,
    undo: Option<Box<dyn FnOnce() + Send>>,
}

impl Mounted {
    /// `root`, undone by `undo` when dropped.
    pub fn new(root: PathBuf, undo: impl FnOnce() + Send + 'static) -> Self {
        Self {
            root,
            undo: Some(Box::new(undo)),
        }
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if let Some(undo) = self.undo.take() {
            undo();
        }
    }
}

/// How one snapshot was read, for `--issues`.
#[derive(Clone, Debug)]
pub struct Reading {
    pub name: OsString,
    /// Why it could not be mounted, if it could not.
    pub mount: Result<(), String>,
    pub scope: Scope,
    pub walked: Walked,
    pub took: Duration,
}

impl Reading {
    /// The reading in a line or two.
    #[must_use]
    pub fn describe(&self) -> String {
        let name = self.name.to_string_lossy();
        if let Err(why) = &self.mount {
            return format!("{name}: not mounted: {why}");
        }
        let scope = match &self.scope {
            Scope::Dataless => {
                return format!(
                    "{name}: dataless — macOS has kept only its metadata, so it holds no blocks \
                     of its own; not read"
                );
            }
            Scope::Whole(why) => format!("read whole ({why}; written over in place not seen)"),
            Scope::Changed {
                since,
                events,
                listed,
                walked,
            } => format!(
                "the change log since id {since}: {events} events, {} folders named, {} walked \
                 whole",
                listed.len(),
                walked.len()
            ),
        };
        let w = &self.walked;
        format!(
            "{name}: {scope}; {} folders listed, {} compared, {} unreadable; {} files still \
             live by id; {} files gone from the disk, {} ({} of them clones, {}); {} written \
             over since, {} of their old blocks; {:.1}s",
            w.folders,
            w.compared,
            w.failed,
            w.live,
            w.kept,
            libduscape::DisplaySize(w.kept_bytes as f64),
            w.cloned,
            libduscape::DisplaySize(w.cloned_bytes as f64),
            w.written,
            libduscape::DisplaySize(w.written_bytes as f64),
            self.took.as_secs_f64()
        )
    }
}

/// The snapshots' folder for the scan of `scan_root`: a folder per snapshot of `snapshots`
/// (each with the scope to read), each holding what [`gone_from_live`] found in it, made
/// readable one at a time by `mount`. One tree, so its ledger counts a file several snapshots
/// keep once in the folder above them; graft it at [`FOLDER`]. A snapshot that cannot be
/// mounted is an empty folder, counted in `failed_to_read`. `None` if stopped.
pub fn build(
    scan_root: &Path,
    snapshots: &[(OsString, Scope)],
    mount: &dyn Fn(&OsStr) -> Result<Mounted, String>,
    pass: &Pass<'_>,
) -> Option<(FileTree, Vec<Reading>)> {
    let keep_going = pass.keep_going;
    let container = scan_root.join(FOLDER);
    let mut tree = FileTree::new(Folder::new(&container), container.clone());
    // Every snapshot a folder, even one that keeps nothing of its own.
    let mut top = DirEntries::new(Arc::from(container.as_path()));
    for (name, _) in snapshots {
        let meta = EntryMeta {
            is_dir: true,
            ..EntryMeta::default()
        };
        top.push(name, meta);
    }
    tree.add_dir_entries(top);
    let mut readings = Vec::with_capacity(snapshots.len());
    for (name, scope) in snapshots {
        if !keep_going() {
            return None;
        }
        let started = Instant::now();
        let mut reading = Reading {
            name: name.clone(),
            mount: Ok(()),
            scope: scope.clone(),
            walked: Walked::default(),
            took: Duration::ZERO,
        };
        if *scope == Scope::Dataless {
            readings.push(reading);
            continue;
        }
        match mount(name) {
            Err(why) => {
                tree.failed_to_read += 1;
                reading.mount = Err(why);
            }
            Ok(mounted) => {
                reading.walked =
                    gone_from_live(&mounted.root, &container.join(name), scope, pass, |kept| {
                        tree.add_dir_entries(kept);
                    });
                tree.failed_to_read += reading.walked.failed;
            }
        }
        reading.took = started.elapsed();
        readings.push(reading);
    }
    keep_going().then_some((tree, readings))
}

#[cfg(target_os = "macos")]
pub use macos::{
    can_read, full_disk_access, is_snapshot_mount, list, list_with_times, read, volume_for,
    with_lookups,
};

/// Elsewhere there are no snapshots to show: APFS's are macOS's.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn volume_for(_root: &Path) -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn list(_volume: &Path) -> io::Result<Vec<OsString>> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn can_read() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn full_disk_access() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn read(
    _scan_root: &Path,
    _volume: &Path,
    _names: &[OsString],
    _options: libduscape::scan::ScanOptions,
    _keep_going: &(dyn Fn() -> bool + Sync),
) -> Option<(FileTree, Vec<Reading>)> {
    None
}

/// `ATTR_CMN_FLAGS`' bit on a dataless snapshot, as `fs_snapshot_list` gives it: set on the
/// one snapshot `tmutil` called dataless, clear on the three others (2026-10-05).
const SNAPSHOT_DATALESS: u32 = 0x20;

/// The snapshots in `count` records of an `fs_snapshot_list` buffer asked for `ATTR_CMN_NAME`,
/// `ATTR_CMN_CRTIME` and `ATTR_CMN_FLAGS` with `ATTR_CMN_RETURNED_ATTRS`: each record its
/// length, the attributes returned (five `u32`s), the name's reference (an offset from itself,
/// and a length with the NUL), the time (seconds and nanoseconds, eight bytes each), then the
/// flags. Laid out as measured on macOS 26 (2026-10-05); a record too short for it ends the
/// parse.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_listing(bytes: &[u8], count: usize) -> Vec<Listed> {
    const RETURNED: usize = 20;
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let long = |at: usize| -> Option<i64> {
        Some(i64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
    };
    let mut snapshots = Vec::with_capacity(count);
    let mut at = 0usize;
    for _ in 0..count {
        let Some(length) = word(at).map(|length| length as usize) else {
            break;
        };
        let reference = at + 4 + RETURNED;
        let (Some(offset), Some(name_length)) = (word(reference), word(reference + 4)) else {
            break;
        };
        let (Some(seconds), Some(nanos), Some(flags)) = (
            long(reference + 8),
            long(reference + 16),
            word(reference + 24),
        ) else {
            break;
        };
        // The offset is from the reference, and signed: anything that would leave the record
        // ends the parse rather than wrapping.
        let start = reference.checked_add_signed(offset as i32 as isize);
        let end = start.and_then(|start| start.checked_add(name_length as usize));
        let name = start
            .zip(end)
            .filter(|&(_, end)| length > 0 && end <= at + length)
            .and_then(|(start, end)| bytes.get(start..end.saturating_sub(1)));
        let Some(name) = name else {
            break;
        };
        snapshots.push(Listed {
            name: os_string(name),
            made: SystemTime::UNIX_EPOCH
                + Duration::new(
                    u64::try_from(seconds).unwrap_or(0),
                    u32::try_from(nanos).unwrap_or(0),
                ),
            dataless: flags & SNAPSHOT_DATALESS != 0,
        });
        at += length;
    }
    snapshots
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use ::std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(bytes).to_os_string()
}

#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests;
