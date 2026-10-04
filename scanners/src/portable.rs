//! A portable directory walk on `std::fs` alone: the walker of every platform with no native one
//! (the BSDs), and the `--benchmark` baseline (the `portable-*` stages, through
//! [`scan_folder`]). Compiled and tested on every platform so that it cannot rot unnoticed.
//!
//! It replaced `dua-core` (2026-10-04), of which only this much was used: Debian does not
//! package that crate, and a dependency that is not in Debian must be patched out of a Debian
//! build even when it is optional or for another platform, since Cargo resolves those too
//! (`docs/packaging.md`). The walk is the simple shape — a shared queue of directories, a few
//! workers, each directory listed whole and sent as one [`DirEntries`] — since nothing that
//! depends on its speed runs it: Linux, macOS and Windows scan with their own walkers.
//!
//! What `std` gives per platform, and so what this walk asks: on Unix, the listing's `lstat`
//! has it all (`st_blocks` for the size on disk, the inode, the link count); on Windows the
//! listing has neither an allocation size nor a file id, so each file is opened once for its
//! index, link count and allocation (`os::file_identity`), as the walk before it opened each
//! for the link count — the same sizes as the native walker's, and as `dua-core`'s.

use ::std::collections::VecDeque;
use ::std::fs::Metadata;
use ::std::path::Path;
use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use ::std::sync::{Arc, Condvar, Mutex};

use crate::{DirEntries, EntryMeta, ScanItem, ScanOptions, cores};

/// Workers for the portable walk: `--threads`, else the cores up to eight (the cap the walk
/// before it measured, past which it collapsed; this one is not measured past it either).
fn thread_count(options: ScanOptions) -> usize {
    if let Some(threads) = options.threads {
        return threads.max(1);
    }
    if options.parallel {
        cores().min(MAX_THREADS)
    } else {
        1
    }
}

const MAX_THREADS: usize = 8;

/// A directory waiting to be listed, and how deep it is below the root (the root's entries
/// are at depth 1).
struct Job {
    path: Arc<Path>,
    depth: usize,
}

/// The queue the workers share: directories waiting, and how many are being listed, so that
/// the walk ends when both are none.
#[derive(Default)]
struct Queue {
    waiting: VecDeque<Job>,
    listing: usize,
}

/// The rules a directory's entries are walked by, fixed for the walk.
#[derive(Clone)]
struct Rules {
    max_depth: Option<usize>,
    snapshots: bool,
    /// The root's device, under `-x`.
    device: Option<u64>,
}

impl Rules {
    fn new(root: &Path, options: ScanOptions) -> Self {
        Rules {
            max_depth: options.max_depth,
            snapshots: options.snapshots,
            device: options
                .one_file_system
                .then(|| libduscape::os::volume_id(root).unwrap_or_default()),
        }
    }

    /// Whether to go down into the directory `name`, an entry at `depth`. The scan's root is
    /// scanned whatever it is named; below it, a share's snapshot folders are listed, not
    /// entered; `-x` stops at another device.
    fn descend(
        &self,
        path: &Path,
        name: &::std::ffi::OsStr,
        depth: usize,
        meta: &Metadata,
    ) -> bool {
        if !self.max_depth.is_none_or(|max| depth < max) {
            return false;
        }
        if libduscape::nas::left_out(name, self.snapshots).is_some() {
            return false;
        }
        self.device.is_none_or(|root| device(path, meta) == root)
    }
}

/// Walk `root` on `std::fs`, a directory's entries at a time.
pub fn walk_directories(
    root: &Path,
    options: ScanOptions,
) -> impl Iterator<Item = DirEntries> + use<> {
    let root = libduscape::os::canonical_root(root);
    let rules = Rules::new(&root, options);
    let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
    queue
        .0
        .lock()
        .expect("a fresh queue")
        .waiting
        .push_back(Job {
            path: Arc::from(root.as_path()),
            depth: 0,
        });
    // Bounded, so a walk ahead of its reader waits rather than holding the tree in memory.
    let (sender, receiver): (SyncSender<DirEntries>, Receiver<DirEntries>) = sync_channel(64);
    for index in 0..thread_count(options) {
        let (queue, sender, rules) = (Arc::clone(&queue), sender.clone(), rules.clone());
        ::std::thread::Builder::new()
            .name(format!("portable_walk_{index}"))
            .spawn(move || work(&queue, &sender, &rules))
            .expect("spawning a walker thread");
    }
    receiver.into_iter()
}

/// One worker: take a directory, list it, queue its subdirectories, send it; until the queue is
/// empty and nobody is listing (nothing more can come), or the reader has gone.
fn work(queue: &(Mutex<Queue>, Condvar), sender: &SyncSender<DirEntries>, rules: &Rules) {
    let (lock, wake) = queue;
    loop {
        let job = {
            let mut state = lock.lock().expect("the queue's lock");
            loop {
                if let Some(job) = state.waiting.pop_back() {
                    state.listing += 1;
                    break job;
                }
                if state.listing == 0 {
                    wake.notify_all();
                    return;
                }
                state = wake.wait(state).expect("the queue's lock");
            }
        };
        let (entries, below) = list(&job, rules);
        let mut state = lock.lock().expect("the queue's lock");
        state.waiting.extend(below);
        state.listing -= 1;
        wake.notify_all();
        drop(state);
        if sender.send(entries).is_err() {
            return; // the reader stopped
        }
    }
}

/// List one directory: its entries, and the subdirectories to walk next.
fn list(job: &Job, rules: &Rules) -> (DirEntries, Vec<Job>) {
    let mut entries = DirEntries::with_capacity(Arc::clone(&job.path), 32, 32 * 32);
    let mut below = Vec::new();
    let reader = match ::std::fs::read_dir(&job.path) {
        Ok(reader) => reader,
        Err(error) => {
            entries.fail("read_dir", None, &error);
            return (entries, below);
        }
    };
    let depth = job.depth + 1;
    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // A failed listing step names no entry; the directory carries it.
                entries.fail("read_dir", None, &error);
                continue;
            }
        };
        let name = entry.file_name();
        // The entry itself, not what a link points to: `DirEntry::metadata` does not follow.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                entries.fail("stat", Some(&name), &error);
                continue;
            }
        };
        let path = entry.path();
        let is_dir = metadata.is_dir();
        let (inode, links, size) = identity(&path, is_dir, &metadata);
        entries.push(
            &name,
            EntryMeta {
                size,
                apparent: metadata.len(),
                inode,
                links,
                is_dir,
                shared_extent: 0,
            },
        );
        if is_dir && rules.descend(&path, &name, depth, &metadata) {
            below.push(Job {
                path: Arc::from(path.as_path()),
                depth,
            });
        }
    }
    (entries, below)
}

/// Walk `root` and yield each entry (or a read error marker), one by one: the walk's
/// directories flattened, for the benchmark's `portable-walk` stage.
pub fn scan_folder(root: impl AsRef<Path>, options: ScanOptions) -> impl Iterator<Item = ScanItem> {
    walk_directories(root.as_ref(), options).flat_map(|directory| {
        let failed = (0..directory.failed).map(|_| ScanItem::ReadError);
        let entries: Vec<ScanItem> = directory
            .iter()
            .map(|(name, meta)| ScanItem::Entry {
                path: directory.path.join(name),
                meta: *meta,
            })
            .collect();
        entries.into_iter().chain(failed)
    })
}

/// The filesystem an entry lives on.
#[cfg(unix)]
fn device(_path: &Path, metadata: &Metadata) -> u64 {
    use ::std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(windows)]
fn device(path: &Path, _metadata: &Metadata) -> u64 {
    libduscape::os::volume_id(path).unwrap_or(0)
}

/// Inode number, link count, and what the entry occupies on disk.
#[cfg(unix)]
fn identity(_path: &Path, _is_dir: bool, metadata: &Metadata) -> (u64, u64, u64) {
    use ::std::os::unix::fs::MetadataExt;
    (
        metadata.ino(),
        metadata.nlink(),
        libduscape::os::size_on_disk_fast(metadata),
    )
}

/// On Windows the listing has neither a file id nor an allocation size, so a file is opened
/// once for both, and its link count (`os::file_identity`); a directory takes its length.
#[cfg(windows)]
fn identity(path: &Path, is_dir: bool, metadata: &Metadata) -> (u64, u64, u64) {
    if is_dir {
        return (0, 1, metadata.len());
    }
    let file = libduscape::os::file_identity(path);
    (
        file.index,
        file.links,
        file.allocated.unwrap_or(metadata.len()),
    )
}
