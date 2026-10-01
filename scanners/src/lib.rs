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

pub mod cache;
pub mod fill;
pub mod focus;
#[cfg(target_os = "macos")]
pub mod fsevents;
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

    use super::{Audience, DirEntries, ScanOptions, scan_directories};
    use libduscape::model::{FileTree, Folder};
    use libduscape::scan::Focus;

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

    /// Whether `directory` came from a saved scan read as it was: its root says so, and the
    /// tree built from it owes a catch-up.
    fn owes_catch_up(directory: &DirEntries) -> bool {
        directory
            .issues
            .kinds
            .keys()
            .any(|(kind, _)| *kind == super::cache::NOTE_OWED)
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
        focus: &Focus,
        mut progress: impl FnMut(&DirEntries) -> bool,
    ) -> Option<(FileTree, u64, Timings, super::refine::SmallFiles)> {
        let shards = shards.max(1);
        let root = libduscape::os::canonical_root(root);
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
        let mut from_saved_scan = false;
        for directory in scan_directories(&root, options, focus) {
            if !progress(&directory) {
                stopped = true;
                break;
            }
            // A running total is the live view's alone (`progress` has had it); the listing
            // for the tree comes later.
            if matches!(directory.audience, Audience::View { .. }) {
                continue;
            }
            from_saved_scan |= owes_catch_up(&directory);
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
        tree.from_saved_scan = from_saved_scan;
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

/// What `duscape --issues` says about where a scan of `root` runs, before the scan: which walker
/// reads it, and on Linux the kernel, whether it has `statx`, the filesystem and who is asking —
/// what a report of an unreadable scan needs first.
#[must_use]
pub fn environment(root: &Path, options: ScanOptions) -> Vec<(&'static str, String)> {
    let walker = ("walker", walker_words(root, options));
    #[cfg(target_os = "linux")]
    return ::std::iter::once(walker)
        .chain(linux::environment(root))
        .chain(::std::iter::once((
            "snapshots",
            if options.snapshots {
                "walked (--snapshots)".to_string()
            } else {
                "read-only btrfs snapshots, and a share's snapshot folders by name, are left empty (--snapshots walks them)".to_string()
            },
        )))
        .collect();
    #[cfg(not(target_os = "linux"))]
    vec![walker]
}

/// Which walker `scan_directories` picks for `root`, in words.
fn walker_words(root: &Path, options: ScanOptions) -> String {
    #[cfg(target_os = "linux")]
    {
        if options.read_device && ext4::would_read_device(root) {
            "ext4, read from the block device (root)".to_string()
        } else {
            "the kernel walk: getdents64 and statx".to_string()
        }
    }
    #[cfg(windows)]
    {
        match (options.read_device, mft::would_read_device(root)) {
            (true, Ok(())) => "NTFS, read from the master file table (elevated)".to_string(),
            (true, Err(why)) => {
                format!("bulk directory listing (not the master file table: {why})")
            }
            (false, _) => "bulk directory listing (--no-device-read)".to_string(),
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Whether a saved scan is there is answered from its header, not by reading it whole.
        let key = cache::Key::new(root, options);
        let saved = match options.cache {
            Cache::Off => None,
            Cache::Saved | Cache::CatchUp => {
                cache::directory().map(|dir| (cache::peek(&dir, &key), dir))
            }
        };
        match saved {
            Some((Some(stamp), dir)) => format!(
                "getattrlistbulk, or the scan saved {} shown at once and brought up to date by FSEvents ({})",
                cache::age_words_ago(stamp.saved_at),
                dir.join(key.file_name()).display(),
            ),
            Some((None, _)) => "getattrlistbulk, saved for next time (FSEvents)".to_string(),
            None => "getattrlistbulk, walked afresh (no saved scan used or made)".to_string(),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (root, options);
        "dua-core, the portable walk (there is no native walker here)".to_string()
    }
}

/// Whether a walk of `scan_root` goes into `folder`, a folder inside it: what a rescan of that
/// folder alone has to ask first, since a walk starting there applies no rule to its own root.
/// On Linux it asks what the walk would at a mount point; elsewhere every folder is entered.
#[must_use]
pub fn walk_would_enter(scan_root: &Path, folder: &Path, options: ScanOptions) -> bool {
    // A share's snapshots are left empty by name on every platform; the scan's own root is
    // scanned whatever it is named.
    if folder != scan_root
        && folder
            .file_name()
            .is_some_and(|name| libduscape::nas::left_out(name, options.snapshots).is_some())
    {
        return false;
    }
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
pub fn scan_directories(
    root: &Path,
    options: ScanOptions,
    focus: &Focus,
) -> impl Iterator<Item = DirEntries> {
    #[cfg(target_os = "macos")]
    {
        macos_scan(root, options, focus)
    }
    #[cfg(target_os = "linux")]
    {
        // From the device where that is allowed and asked for, else through the kernel.
        let device = if options.read_device {
            ext4::walk_ext4(root, options, focus)
        } else {
            None
        };
        match device {
            Some(walk) => LinuxScan::Device(walk),
            None => LinuxScan::Kernel(linux::walk_linux(
                root,
                thread_count(options),
                options,
                focus,
            )),
        }
    }
    #[cfg(windows)]
    {
        // From the volume's master file table where the process may open the volume
        // (elevated, NTFS) and asks for it, else through the filesystem. The table's listing
        // comes once it is read whole, so the focus has nothing to steer there; the running
        // totals it hands the view meanwhile come in the table's order.
        let threads = thread_count(options);
        let device = if options.read_device {
            mft::walk_mft(root, threads, options)
        } else {
            None
        };
        match device {
            Some(walk) => WindowsScan::Table {
                walk,
                root: root.to_path_buf(),
                threads,
                options,
                focus,
            },
            None => WindowsScan::Kernel(windows::walk_windows(root, threads, options, focus)),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        // `dua-core` walks in its own order; the focus is not passed on.
        let _ = focus;
        fallback::group_by_directory(root, options)
    }
}

/// The macOS walk, by [`Cache`]: `Off`, `getattrlistbulk`, nothing saved; `Saved`, the saved
/// scan streamed as it is where its stamp still holds (else the walk, saved); `CatchUp`, the
/// saved scan brought up to date by the change log (else the walk), saved. The log's id is
/// taken before a walk starts, so a change made while it runs is replayed next time.
#[cfg(target_os = "macos")]
fn macos_scan<'a>(
    root: &Path,
    options: ScanOptions,
    focus: &'a Focus,
) -> cache::Recorder<MacosScan<'a>> {
    let walk = move |path: &Path, depth: usize| -> Box<dyn Iterator<Item = DirEntries> + '_> {
        Box::new(macos::walk_macos(
            path,
            thread_count(options),
            options.max_depth.map(|max| max.saturating_sub(depth)),
            options.one_file_system,
            options.snapshots,
            focus,
        ))
    };
    let dir = match options.cache {
        Cache::Off => None,
        Cache::Saved | Cache::CatchUp => cache::directory(),
    };
    let key = cache::Key::new(root, options);
    let stamp = dir.as_ref().and_then(|_| macos_stamp(root));
    let (Some(dir), Some(now)) = (dir, stamp) else {
        return cache::Recorder::plain(MacosScan::Walk(walk(root, 0)));
    };
    if options.cache == Cache::Saved {
        // As it was, no recording: the file is already that, and the catch-up will record.
        if let Some(saved) = macos_saved(&dir, &key, &now) {
            return cache::Recorder::plain(MacosScan::Saved(Box::new(saved)));
        }
    }
    let cached = match options.cache {
        Cache::CatchUp => macos_cached(&dir, &key, &now, options, walk),
        _ => None,
    };
    let inner = match cached {
        Some(cached) => MacosScan::Cached(Box::new(cached)),
        None => MacosScan::Walk(walk(root, 0)),
    };
    cache::Recorder::new(inner, &dir, key, now)
}

/// What a scan starting now is stamped with; `None` where the volume keeps no log, and then
/// nothing is saved either.
#[cfg(target_os = "macos")]
fn macos_stamp(root: &Path) -> Option<cache::Stamp> {
    use std::os::unix::fs::MetadataExt;
    let device = std::fs::metadata(root).ok()?.dev();
    Some(cache::Stamp {
        event_id: fsevents::current_event_id()?,
        device,
        log_uuid: fsevents::log_uuid(device)?,
        system: fsevents::system_version(),
        saved_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_secs()),
    })
}

/// Whether the log since `then` can still say what changed: the same device, the same log,
/// the same system.
#[cfg(target_os = "macos")]
fn stamp_holds(then: &cache::Stamp, now: &cache::Stamp) -> bool {
    then.device == now.device && then.log_uuid == now.log_uuid && then.system == now.system
}

/// The saved scan for `key` as it is, streamed, if its stamp holds.
#[cfg(target_os = "macos")]
fn macos_saved(dir: &Path, key: &cache::Key, now: &cache::Stamp) -> Option<cache::SavedStream> {
    let then = cache::peek(dir, key)?;
    if !stamp_holds(&then, now) {
        return None;
    }
    let note = format!(
        "read as saved {}; being brought up to date",
        cache::age_words_ago(then.saved_at)
    );
    cache::SavedStream::open(dir, key, note).ok()
}

/// The saved scan for `key` brought up to date, if there is one and the log since it can be
/// trusted. The file is inflated and checked while the log replays on a thread of its own;
/// the replay is given half the time the walk would take, at the rate measured here
/// (`docs/scan-performance.md`, "macOS: what is left").
#[cfg(target_os = "macos")]
fn macos_cached<'a>(
    dir: &Path,
    key: &cache::Key,
    now: &cache::Stamp,
    options: ScanOptions,
    walk: impl FnMut(&Path, usize) -> Box<dyn Iterator<Item = DirEntries> + 'a> + 'a,
) -> Option<cache::CachedScan<'a>> {
    let timing = std::env::var_os("DUSCAPE_CACHE_TIMES").is_some();
    let started = std::time::Instant::now();
    let then = cache::peek(dir, key)?;
    if !stamp_holds(&then, now) {
        return None;
    }
    const WALK_ENTRIES_PER_SECOND: u64 = 300_000;
    let (saved, replay) = std::thread::scope(|scope| {
        let replay = scope.spawn(|| {
            // The saved entry count is in the footer; the file's size stands in for it here.
            let bytes = std::fs::metadata(dir.join(key.file_name())).map_or(0, |m| m.len());
            let give_up = std::time::Duration::from_millis(
                (bytes / 3 * 1000 / WALK_ENTRIES_PER_SECOND / 2).clamp(1000, 60_000),
            );
            fsevents::replay(&key.root, then.event_id, give_up)
        });
        let saved = cache::Saved::open(dir, key);
        if timing {
            eprintln!(
                "cache: file read, inflated and checked in {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
        (saved, replay.join().ok())
    });
    let saved = saved.ok()?;
    let fsevents::Replay::Changes(changes) = replay? else {
        return None;
    };
    if timing {
        eprintln!(
            "cache: log replayed by {:.3}s: {} events, {} folders to list, {} to walk",
            started.elapsed().as_secs_f64(),
            changes.events,
            changes.listed.len(),
            changes.walked.len()
        );
    }
    if cache::too_stale(&changes, &saved.header) {
        return None;
    }
    let list = move |path: &Path, depth: usize| {
        macos::list_one(path, depth, options.max_depth, options.snapshots)
    };
    let note = cache::note(&saved.header, &changes, changes.listed.len() as u64);
    let listing = std::time::Instant::now();
    let scan = cache::CachedScan::new(
        saved,
        &key.root,
        &changes,
        &list,
        thread_count(options),
        Box::new(walk),
        note,
    );
    if timing {
        eprintln!(
            "cache: {} folders listed again in {:.3}s; first directory after {:.3}s in all",
            scan.relisted(),
            listing.elapsed().as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
    }
    Some(scan)
}

/// The macOS walk: from the saved scan as it is, from it brought up to date, or the kernel.
#[cfg(target_os = "macos")]
enum MacosScan<'a> {
    Saved(Box<cache::SavedStream>),
    Cached(Box<cache::CachedScan<'a>>),
    Walk(Box<dyn Iterator<Item = DirEntries> + 'a>),
}

#[cfg(target_os = "macos")]
impl Iterator for MacosScan<'_> {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        match self {
            MacosScan::Saved(stream) => stream.next(),
            MacosScan::Cached(scan) => scan.next(),
            MacosScan::Walk(walk) => walk.next(),
        }
    }
}

/// The Windows walk: from the volume's table, or through the filesystem. The table's walk is
/// read on a thread of its own and can fail part way (an I/O error reading the table), having
/// handed the view running totals but the tree nothing: then the kernel walk takes over from
/// the start, as it would have had the table declined up front. What the view has by then is
/// counted again as the walk lists it, until the finished tree takes the view's place.
#[cfg(windows)]
enum WindowsScan<'a> {
    Table {
        walk: mft::MftWalk,
        root: ::std::path::PathBuf,
        threads: usize,
        options: ScanOptions,
        focus: &'a Focus,
    },
    Kernel(windows::WindowsWalk),
}

#[cfg(windows)]
impl Iterator for WindowsScan<'_> {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        loop {
            match self {
                WindowsScan::Table { walk, .. } => match walk.next() {
                    Some(Ok(directory)) => return Some(directory),
                    None => return None,
                    Some(Err(error)) => {
                        eprintln!(
                            "duscape: reading the volume's table failed, walking instead: {error}"
                        );
                        let WindowsScan::Table {
                            root,
                            threads,
                            options,
                            focus,
                            ..
                        } = self
                        else {
                            unreachable!("matched above");
                        };
                        let walk = windows::walk_windows(root.as_path(), *threads, *options, focus);
                        *self = WindowsScan::Kernel(walk);
                    }
                },
                WindowsScan::Kernel(walk) => return walk.next(),
            }
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
        let root_canon = libduscape::os::canonical_root(root);
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
                    Err(error) => {
                        // No entry, so nothing identifies which directory this belongs to.
                        match &mut open {
                            Some(open) => open.fail("walk", None, &error),
                            None => {
                                let mut lost = DirEntries::new(Arc::clone(&root));
                                lost.fail("walk", None, &error);
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
                let metadata = match metadata {
                    Some(Ok(metadata)) => Ok(metadata),
                    Some(Err(error)) => Err(error.to_string()),
                    None => Err("the walk gave no metadata".to_string()),
                };
                let named = metadata.map(|metadata| {
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
                            Ok(meta) => open.push(&file_name, meta),
                            Err(error) => open.fail("stat", Some(&file_name), error),
                        }
                    }
                    // A different directory: start its group, and hand back the finished one.
                    // The new group already holds this entry, so nothing is lost by returning.
                    _ => {
                        let mut group = DirEntries::with_capacity(parent_path, 32, 32 * 32);
                        match named {
                            Ok(meta) => group.push(&file_name, meta),
                            Err(error) => group.fail("stat", Some(&file_name), error),
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

/// Workers for the walker the app actually uses: `--threads`, else `default_scan_threads`.
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
    let root = libduscape::os::canonical_root(root.as_ref());
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
    let snapshots = options.snapshots;
    let root_device = options
        .one_file_system
        .then(|| libduscape::os::volume_id(root).unwrap_or_default());
    move |entry| {
        if !max_depth.is_none_or(|max| entry.depth < max) {
            return false;
        }
        // A share's snapshots, seen over the network: listed, not entered — the scan's own
        // root scanned whatever it is named, as every walker scans it.
        if entry.depth > 0 && libduscape::nas::left_out(&entry.file_name, snapshots).is_some() {
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
    let root_path = libduscape::os::canonical_root(root.as_ref());
    let mut tree = FileTree::new(Folder::new(&root_path), root_path.clone());
    let mut failed_to_read = 0u64;

    for directory in scan_directories(&root_path, options, &Focus::default()) {
        // A running total is for a live view, which this has none of.
        if matches!(directory.audience, Audience::View { .. }) {
            continue;
        }
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
