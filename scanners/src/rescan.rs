//! Scanning part of the tree again, on a thread of its own, while the old figures stay on screen.

use ::std::ffi::OsString;
use ::std::path::{Path, PathBuf};
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::{Arc, Mutex};
use ::std::thread;
use ::std::time::{Duration, Instant};

use crate::parallel;
use crate::refine::{Found, SmallFiles, refine};
use libdiskonaut::{DisplayCount, FileToDelete, FileTree, Folder, ScanOptions};

/// What a rescan found.
pub enum Outcome {
    /// The folder's tree, with its unreadable entries counted, how long the scan took, and — for
    /// a whole-tree rescan — the small files left for a second pass. A folder's rescan has had
    /// its second pass already.
    Scanned(Box<FileTree>, Duration, SmallFiles),
    /// The folder is no longer there, or is no longer a folder.
    Gone,
    /// The scan does not go into this folder — a pseudo or network filesystem, another
    /// filesystem under `-x`, a bind mount of a folder it reaches anyway, or past `--max-depth` —
    /// so neither does its rescan.
    NotWalked,
}

/// Starts rescans, and hands each one's outcome to `done` with the id it was started under.
pub struct Rescanner {
    options: ScanOptions,
    /// Cleared when the program ends, which stops any rescan still walking.
    running: Arc<AtomicBool>,
    done: Arc<dyn Fn(u64, Outcome) + Send + Sync>,
}

impl Rescanner {
    pub fn new(
        options: ScanOptions,
        running: Arc<AtomicBool>,
        done: impl Fn(u64, Outcome) + Send + Sync + 'static,
    ) -> Self {
        Self {
            options,
            running,
            done: Arc::new(done),
        }
    }

    /// Scan `path` in the background. Setting `cancel` stops the walk, and then nothing is
    /// reported: whoever cancelled it has stopped waiting.
    ///
    /// With `refine_here`, the second pass runs here too, on the new tree before it is handed
    /// over: a folder's tree is small, and has a ledger of its own that the whole tree's second
    /// pass knows nothing of. A whole-tree rescan leaves it to the caller, to run while the new
    /// tree is already on screen.
    ///
    /// `path` is `depth` folders below the scan root `root`, and is walked under the same rules as
    /// if the whole scan had reached it: see [`Outcome::NotWalked`].
    pub fn spawn(
        &self,
        id: u64,
        root: PathBuf,
        path: PathBuf,
        depth: usize,
        cancel: Arc<AtomicBool>,
        refine_here: bool,
    ) {
        let mut options = self.options;
        // A rescan is of a folder the user is looking at: small, and wanted current, so it
        // goes through the kernel even where the first scan read the device.
        options.read_device = false;
        let running = Arc::clone(&self.running);
        let done = Arc::clone(&self.done);
        let _ = thread::Builder::new()
            .name(format!("rescan_{id}"))
            .spawn(move || {
                if !path.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
                    done(id, Outcome::Gone);
                    return;
                }
                // Depth counts from the scan root: a folder the scan reads to depth `max` below
                // there is read to `max - depth` below itself, and one at `max` not at all.
                if let Some(max) = options.max_depth {
                    if depth >= max {
                        done(id, Outcome::NotWalked);
                        return;
                    }
                    options.max_depth = Some(max - depth);
                }
                if depth > 0 && !crate::walk_would_enter(&root, &path, options) {
                    done(id, Outcome::NotWalked);
                    return;
                }
                let start = Instant::now();
                let built = parallel::build_tree(
                    &path,
                    options,
                    parallel::SHARDS,
                    parallel::SHARD_DEPTH,
                    |_| running.load(Ordering::Acquire) && !cancel.load(Ordering::Acquire),
                );
                if let Some((mut tree, failed, _, mut small)) = built {
                    tree.failed_to_read = failed;
                    if refine_here {
                        let keep_going =
                            || running.load(Ordering::Acquire) && !cancel.load(Ordering::Acquire);
                        refine(
                            ::std::mem::take(&mut small),
                            crate::thread_count(options),
                            &Mutex::new(None),
                            &keep_going,
                            |found, _| {
                                tree.apply_found(&found);
                                true
                            },
                        );
                        if !keep_going() {
                            return;
                        }
                    }
                    done(id, Outcome::Scanned(Box::new(tree), start.elapsed(), small));
                }
            });
    }
}

/// The rescans a viewer has under way, and what their outcomes do to its tree.
///
/// Each is known by its folder's path from the scan root. One whose folder holds another's brings
/// that folder back too: so a rescan already covered by one under way is not started, and starting
/// one stops those it covers. A delete inside a folder being rescanned restarts that rescan, since
/// one that listed the file before it went would put it back, counted as present and as freed.
#[derive(Default)]
pub struct Rescans {
    under_way: Vec<Rescan>,
    next_id: u64,
}

/// A rescan under way: its id, the folder's path from the scan root, and what stops it.
struct Rescan {
    id: u64,
    relative: Vec<OsString>,
    cancel: Arc<AtomicBool>,
}

/// What a finished rescan did to the tree.
pub struct Finished {
    /// The folder rescanned, from the scan root; empty for everything.
    pub relative: Vec<OsString>,
    /// Whether the tree changed: a folder grafted in, or taken out because it has gone.
    pub changed: bool,
    /// For a rescan of everything: how long it took, and the small files left for the second
    /// pass, which the viewer runs over the new tree if it runs one at all.
    pub whole: Option<(Duration, SmallFiles)>,
    /// A folder's rescan that was grafted in: its path, which the whole tree's second pass, if
    /// one is under way, must now skip — the folder had a second pass of its own.
    pub refined_folder: Option<PathBuf>,
    /// The folder the rescanned one replaced. Dropping it walks everything in it, so the viewer
    /// decides: leak it (the terminal viewer, which quits soon enough), or drop it on a thread of
    /// its own (a window, which may live for hours of rescans).
    pub old: Option<Folder>,
}

impl Rescans {
    /// Start rescanning the folder at `relative` from the root of `tree`, with `rescanner`,
    /// unless one under way already covers it. Returns whether one was started.
    pub fn start(
        &mut self,
        rescanner: &Rescanner,
        tree: &FileTree,
        relative: Vec<OsString>,
    ) -> bool {
        if self
            .under_way
            .iter()
            .any(|rescan| relative.starts_with(&rescan.relative))
        {
            return false;
        }
        self.under_way.retain(|rescan| {
            let covered = rescan.relative.starts_with(&relative);
            if covered {
                rescan.cancel.store(true, Ordering::Release);
            }
            !covered
        });
        self.next_id += 1;
        let id = self.next_id;
        let cancel = Arc::new(AtomicBool::new(false));
        let path = relative
            .iter()
            .fold(tree.path_in_filesystem.clone(), |path, name| {
                path.join(name)
            });
        rescanner.spawn(
            id,
            tree.path_in_filesystem.clone(),
            path,
            relative.len(),
            Arc::clone(&cancel),
            !relative.is_empty(),
        );
        self.under_way.push(Rescan {
            id,
            relative,
            cancel,
        });
        true
    }

    /// Start again every rescan whose folder holds one of `deleted`. Returns whether any was.
    pub fn restart_under(
        &mut self,
        rescanner: &Rescanner,
        tree: &FileTree,
        deleted: &[FileToDelete],
    ) -> bool {
        let mut restart = Vec::new();
        self.under_way.retain(|rescan| {
            let holds = deleted
                .iter()
                .any(|file| file.path_to_file.starts_with(&rescan.relative));
            if holds {
                rescan.cancel.store(true, Ordering::Release);
                restart.push(rescan.relative.clone());
            }
            !holds
        });
        let restarted = !restart.is_empty();
        for relative in restart {
            self.start(rescanner, tree, relative);
        }
        restarted
    }

    /// Stop every rescan under way, as the viewer moves on to another folder; their outcomes are
    /// dropped when they report.
    pub fn cancel_all(&mut self) {
        for rescan in self.under_way.drain(..) {
            rescan.cancel.store(true, Ordering::Release);
        }
    }

    /// What is being rescanned, for a status line: a folder's path from the root, the root and
    /// `(everything)`, or how many folders. `None` when nothing is.
    #[must_use]
    pub fn describe(&self, root: &Path) -> Option<String> {
        match self.under_way.as_slice() {
            [] => None,
            [one] if one.relative.is_empty() => {
                Some(format!("{} (everything)", root.to_string_lossy()))
            }
            [one] => Some(
                one.relative
                    .iter()
                    .collect::<PathBuf>()
                    .to_string_lossy()
                    .into_owned(),
            ),
            many => Some(format!("{} folders", DisplayCount(many.len() as u64))),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.under_way.is_empty()
    }

    /// How many are under way.
    #[must_use]
    pub fn len(&self) -> usize {
        self.under_way.len()
    }

    /// Those under way, oldest first: each one's id and its folder's path from the scan root.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[OsString])> {
        self.under_way
            .iter()
            .map(|rescan| (rescan.id, rescan.relative.as_slice()))
    }

    /// The rescan `id` has reported `outcome`: put what it found in `tree`. `None` if it was
    /// stopped in the meantime, and its outcome is dropped.
    ///
    /// Where the user is navigated to may have gone with it; the viewer moves them out to the
    /// nearest folder that is still there (see [`FileTree::current_folder_exists`]).
    pub fn finish(&mut self, id: u64, outcome: Outcome, tree: &mut FileTree) -> Option<Finished> {
        let index = self.under_way.iter().position(|rescan| rescan.id == id)?;
        let rescan = self.under_way.remove(index);
        let mut finished = Finished {
            changed: false,
            whole: None,
            refined_folder: None,
            old: None,
            relative: Vec::new(),
        };
        match outcome {
            Outcome::NotWalked => {}
            Outcome::Scanned(rescanned, duration, small) => {
                finished.old = tree.graft(&rescan.relative, *rescanned);
                finished.changed = finished.old.is_some();
                if rescan.relative.is_empty() {
                    finished.whole = Some((duration, small));
                } else if finished.changed {
                    finished.refined_folder = Some(
                        rescan
                            .relative
                            .iter()
                            .fold(tree.path_in_filesystem.clone(), |path, name| {
                                path.join(name)
                            }),
                    );
                }
            }
            Outcome::Gone => finished.changed = tree.remove_path(&rescan.relative),
        }
        finished.relative = rescan.relative;
        Some(finished)
    }
}

/// Runs the second pass over a whole tree's small files, in the background, the folder the user
/// is in first, and hands each batch of findings to `deliver` with the generation it was started
/// under and how many files are left — `None` once it has finished.
pub struct Refiner {
    threads: usize,
    running: Arc<AtomicBool>,
    deliver: Arc<Deliver>,
}

/// Where a [`Refiner`] sends findings: generation, findings, files left (`None` when done).
type Deliver = dyn Fn(u64, Vec<Found>, Option<usize>) + Send + Sync;

impl Refiner {
    pub fn new(
        threads: usize,
        running: Arc<AtomicBool>,
        deliver: impl Fn(u64, Vec<Found>, Option<usize>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            threads,
            running,
            deliver: Arc::new(deliver),
        }
    }

    /// Probe `small` in the background. `focus` is read before each directory is chosen, so the
    /// folder the user moves to is taken next. Setting `cancel` stops it.
    pub fn spawn(
        &self,
        generation: u64,
        small: SmallFiles,
        focus: Arc<Mutex<Option<PathBuf>>>,
        cancel: Arc<AtomicBool>,
    ) {
        let threads = self.threads;
        let running = Arc::clone(&self.running);
        let deliver = Arc::clone(&self.deliver);
        let _ = thread::Builder::new()
            .name(format!("refine_{generation}"))
            .spawn(move || {
                let keep_going =
                    || running.load(Ordering::Acquire) && !cancel.load(Ordering::Acquire);
                refine(small, threads, &focus, &keep_going, |found, left| {
                    deliver(generation, found, Some(left));
                    true
                });
                if keep_going() {
                    deliver(generation, Vec::new(), None);
                }
            });
    }
}
