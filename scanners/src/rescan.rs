//! Scanning part of the tree again, on a thread of its own, while the old figures stay on screen.

use ::std::path::PathBuf;
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::{Arc, Mutex};
use ::std::thread;
use ::std::time::{Duration, Instant};

use crate::parallel;
use crate::refine::{Found, SmallFiles, refine};
use libdiskonaut::{FileTree, ScanOptions};

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
