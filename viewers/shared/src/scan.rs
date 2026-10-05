//! The first scan, on a thread of its own: an outline of each directory as it goes past, for the
//! live view, then the finished tree.

use ::std::path::PathBuf;
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};

use duscape_scan::{Focus, parallel};
use libduscape::{DirSummary, FileTree, Outline, ScanOptions};

/// How many entries go into one batch of outlines sent to the window.
const BATCH: usize = 4096;
/// A frame at 60 fps: the outline sends what it has once this has passed, so the live view
/// moves at the window's frame rate. Waiting for a full batch instead, a Windows walk's 4096
/// entries took 120–300 ms each on `C:\Users` (2026-10-05), and the view moved at 3–8 fps.
pub const FRAME: ::std::time::Duration = ::std::time::Duration::from_millis(16);

/// Scan `root`. `batch` gets the outlines as they are ready and `done` the finished tree, both
/// on the scan's thread; `None` if the scan was stopped. Clearing `running` stops it. `focus`
/// is the folder the viewer shows ([`crate::state::Viewer::focus`]): the walk reads toward it
/// first, and the outline sends what is under it whole.
pub fn spawn(
    root: PathBuf,
    options: ScanOptions,
    running: Arc<AtomicBool>,
    focus: Focus,
    batch: impl Fn(Vec<DirSummary>) + Send + 'static,
    done: impl FnOnce(Option<FileTree>) + Send + 'static,
) {
    let _ = ::std::thread::Builder::new()
        .name("hd_scanner".to_string())
        .spawn(move || {
            let mut outline = Outline::new(root.clone(), Outline::DEFAULT_DEPTH, BATCH)
                .following(&focus)
                .flushing_every(FRAME);
            let built = parallel::build_tree(
                &root,
                options,
                parallel::SHARDS,
                parallel::SHARD_DEPTH,
                &focus,
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
