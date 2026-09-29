//! The fill pass: the folders a saved scan trimmed ([`crate::cache::KEEP_FROM`]), listed
//! again so their smaller files go into the tree one by one, the folder the user is in first.
//!
//! The shape of the second pass (`refine`): a queue taken from under the focus when there is
//! anything there, else from the front, on a few threads, results handed on in batches. Each
//! listing is a cold read (`docs/scan-performance.md`, "macOS: what is left"), so on a disk of
//! a million folders this runs for about as long as a walk would, behind a tree that is
//! already whole in every size — only the tiles of the small files are missing until it gets
//! to them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use libduscape::scan::{DirEntries, ScanOptions};

use crate::focus::Focus;

/// How many listings go in one report, and how long a report waits for more at most.
const BATCH: usize = 256;
const BATCH_WAIT: Duration = Duration::from_millis(100);

/// Folders still to list, taken from under the focus first.
struct Pending {
    folders: BTreeSet<PathBuf>,
}

impl Pending {
    fn take(&mut self, focus: Option<&Path>) -> Option<PathBuf> {
        let under_focus = focus.and_then(|focus| {
            self.folders
                .range::<Path, _>((std::ops::Bound::Included(focus), std::ops::Bound::Unbounded))
                .next()
                .filter(|folder| folder.starts_with(focus))
                .cloned()
        });
        let next = under_focus.or_else(|| self.folders.iter().next().cloned())?;
        self.folders.remove(&next);
        Some(next)
    }
}

/// List every folder in `folders` (absolute paths under `root`, the scan root, whose depth
/// from it the listing needs for the depth cap: `options.max_depth` and the path) on
/// `threads` threads, those under `focus` first, while `keep_going` says so; `report` gets
/// each batch with how many folders are left, `None` with the last. Returning `false` from
/// `report` stops the pass.
pub fn fill(
    root: &Path,
    folders: Vec<PathBuf>,
    options: ScanOptions,
    threads: usize,
    focus: &Focus,
    keep_going: &(dyn Fn() -> bool + Sync),
    mut report: impl FnMut(Vec<(PathBuf, DirEntries)>, Option<usize>) -> bool,
) {
    let total = folders.len();
    let pending = Mutex::new(Pending {
        folders: folders.into_iter().collect(),
    });
    let (sender, receiver) = std::sync::mpsc::sync_channel::<(PathBuf, DirEntries)>(BATCH * 4);
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, 32) {
            let sender = sender.clone();
            let pending = &pending;
            scope.spawn(move || {
                while keep_going() {
                    let focus = focus.get();
                    let Some(folder) = pending
                        .lock()
                        .ok()
                        .and_then(|mut pending| pending.take(focus.as_deref()))
                    else {
                        break;
                    };
                    let depth = folder
                        .strip_prefix(root)
                        .map_or(0, |relative| relative.components().count());
                    if let Ok(listing) = list(&folder, depth, options)
                        && sender.send((folder, listing)).is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut batch = Vec::with_capacity(BATCH);
        let mut done = 0usize;
        let mut since = Instant::now();
        loop {
            match receiver.recv_timeout(BATCH_WAIT) {
                Ok(listed) => {
                    batch.push(listed);
                    done += 1;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if batch.len() >= BATCH || (!batch.is_empty() && since.elapsed() >= BATCH_WAIT) {
                let left = total.saturating_sub(done);
                if !report(std::mem::take(&mut batch), Some(left)) {
                    return;
                }
                since = Instant::now();
            }
        }
        if keep_going() {
            report(batch, None);
        }
    });
}

/// One folder listed as the walk would list it.
fn list(folder: &Path, depth: usize, options: ScanOptions) -> std::io::Result<DirEntries> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::list_one(folder, depth, options.max_depth, options.snapshots)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No saved scan is made elsewhere yet; a fill lists through `std::fs`, for the tests.
        let _ = (depth, options);
        list_std(folder)
    }
}

/// A folder listed through `std::fs`, in the walkers' shape, for the tests.
#[cfg(test)]
pub fn list_for_tests(folder: &Path) -> std::io::Result<DirEntries> {
    list_std(folder)
}

#[cfg_attr(target_os = "macos", cfg(test))]
fn list_std(folder: &Path) -> std::io::Result<DirEntries> {
    {
        let mut directory = DirEntries::new(std::sync::Arc::from(folder));
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let is_dir = meta.is_dir();
            let (inode, links) = identity(&meta, &entry.path());
            directory.push(
                &entry.file_name(),
                libduscape::scan::EntryMeta {
                    size: if is_dir {
                        0
                    } else {
                        libduscape::os::size_on_disk_fast(&meta)
                    },
                    apparent: if is_dir { 0 } else { meta.len() },
                    inode,
                    links,
                    is_dir,
                    shared_extent: 0,
                },
            );
        }
        Ok(directory)
    }
}

#[cfg(all(any(not(target_os = "macos"), test), unix))]
fn identity(meta: &std::fs::Metadata, _path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.ino(), meta.nlink())
}

#[cfg(not(unix))]
fn identity(_meta: &std::fs::Metadata, path: &Path) -> (u64, u64) {
    (0, libduscape::os::link_count(path))
}
