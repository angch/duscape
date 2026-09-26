//! The first scan, on a thread of its own: an outline of each directory as it goes past, for the
//! live view, then the finished tree.

use ::std::path::PathBuf;
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};

use duscape_scan::parallel;
use libduscape::{DirSummary, FileTree, Outline, ScanOptions};

/// How many entries go into one batch of outlines sent to the window.
const BATCH: usize = 4096;

/// Scan `root`. `batch` gets the outlines as they are ready and `done` the finished tree, both
/// on the scan's thread; `None` if the scan was stopped. Clearing `running` stops it.
pub fn spawn(
    root: PathBuf,
    options: ScanOptions,
    running: Arc<AtomicBool>,
    batch: impl Fn(Vec<DirSummary>) + Send + 'static,
    done: impl FnOnce(Option<FileTree>) + Send + 'static,
) {
    let _ = ::std::thread::Builder::new()
        .name("hd_scanner".to_string())
        .spawn(move || {
            let mut outline = Outline::new(root.clone(), Outline::DEFAULT_DEPTH, BATCH);
            let built = parallel::build_tree(
                &root,
                options,
                parallel::SHARDS,
                parallel::SHARD_DEPTH,
                |directory| {
                    if !running.load(Ordering::Acquire) {
                        return false;
                    }
                    if let Some(summaries) = outline.add(directory) {
                        batch(summaries);
                    }
                    true
                },
            );
            if !running.load(Ordering::Acquire) {
                done(None);
                return;
            }
            let rest = outline.finish();
            if !rest.is_empty() {
                batch(rest);
            }
            done(built.map(|(mut tree, failed, _, _)| {
                tree.failed_to_read = failed;
                tree
            }));
        });
}
