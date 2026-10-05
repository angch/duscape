//! A delete under way, on a thread of its own: a folder of thousands of files (a package cache)
//! takes seconds to remove, and done on the window's thread it froze the window with no word
//! of how far it had got. The thread counts what it removes in a [`Tally`]; a window draws the
//! box this lays out once [`SHOW_AFTER`] has passed, with a Cancel button, and hands the
//! outcome to [`crate::state::Viewer::delete_done`] when the thread reports.

use ::std::path::Path;
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicUsize, Ordering};
use ::std::time::{Duration, Instant};

use libduscape::delete::Tally;
use libduscape::{DisplayCount, FileToDelete};

use crate::state::Rect;

/// How long a delete runs before its box is shown: one that ends sooner is over before the box
/// would be read, and a box flashing up for every small delete would be noise.
pub const SHOW_AFTER: Duration = Duration::from_millis(100);
/// How often a window draws the box again while it is up, to show the count moving.
pub const REDRAW_EVERY: Duration = Duration::from_millis(100);

/// How one entry asked for went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Removed,
    /// Not removed: `error` says why, `None` when it was stopped (or never begun) by Cancel.
    /// `partly` when some of what was in it went, so the tree's folder is no longer what is on
    /// disk.
    Failed {
        error: Option<String>,
        partly: bool,
    },
}

/// What to tell the user when a delete could not remove everything: a title, and the detail
/// under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub title: String,
    pub detail: String,
}

impl Failure {
    /// Both in one text, for a message box with no title of its own.
    #[must_use]
    pub fn message(&self) -> String {
        format!("{}\n\n{}", self.title, self.detail)
    }
}

/// Removes for good, through [`libduscape::delete::remove_counting`].
pub fn for_good(file: &FileToDelete, tally: &Tally) -> Result<(), String> {
    libduscape::delete::remove_counting(file, tally).map_err(|error| error.to_string())
}

/// Moves an entry whole somewhere else (a Trash): `trash` called on its path, and all its
/// entries counted at once.
pub fn moving(
    file: &FileToDelete,
    tally: &Tally,
    trash: fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    trash(&file.full_path())?;
    tally.add(entries_in(file));
    Ok(())
}

/// The entries in `file` as the tree counted them: itself, and everything under it.
fn entries_in(file: &FileToDelete) -> u64 {
    file.num_descendants.unwrap_or(0) + 1
}

/// What the deleting thread shares with the window.
#[derive(Default)]
struct Shared {
    tally: Tally,
    /// The index of the entry being removed.
    on: AtomicUsize,
}

/// A delete under way.
pub struct Deletion {
    /// Which delete this is, so a report from one that was replaced is known.
    pub id: u64,
    pub files: Vec<FileToDelete>,
    /// Whether the space comes back (deleted for good) or only moves (to a Trash).
    pub freed: bool,
    started: Instant,
    shared: Arc<Shared>,
    /// The entries in all, as the tree counted them.
    total: u64,
}

impl Deletion {
    /// Start removing `files` with `remove` on a thread of its own; `done` is called there with
    /// the id and how each went, in order, once all are done or Cancel has stopped it. An error
    /// if no thread could be started, when nothing has been touched.
    pub fn spawn(
        id: u64,
        files: Vec<FileToDelete>,
        freed: bool,
        remove: impl Fn(&FileToDelete, &Tally) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(u64, Vec<Ended>) + Send + 'static,
    ) -> ::std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let total = files.iter().map(entries_in).sum();
        let work = files.clone();
        let theirs = Arc::clone(&shared);
        ::std::thread::Builder::new()
            .name("deleter".to_string())
            .spawn(move || {
                let ended = work
                    .iter()
                    .enumerate()
                    .map(|(index, file)| {
                        theirs.on.store(index, Ordering::Relaxed);
                        let tally = &theirs.tally;
                        if tally.stopped() {
                            return Ended::Failed {
                                error: None,
                                partly: false,
                            };
                        }
                        let before = tally.removed();
                        // A panic is this entry's failure, not the thread's end: `done` must be
                        // called, or the window would wait on the delete, taking no input, for
                        // good. Where panics unwind — debug builds, tests: the release is built
                        // with `panic = "abort"` (Cargo.toml), where a panic ends the process, so
                        // a remove must not panic in the first place.
                        let removed =
                            ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                                remove(file, tally)
                            }))
                            .unwrap_or_else(|_| Err("the delete failed unexpectedly".to_string()));
                        match removed {
                            Ok(()) => Ended::Removed,
                            Err(error) => Ended::Failed {
                                error: (!tally.stopped()).then_some(error),
                                partly: tally.removed() > before,
                            },
                        }
                    })
                    .collect();
                done(id, ended);
            })?;
        Ok(Self {
            id,
            files,
            freed,
            started: Instant::now(),
            shared,
            total,
        })
    }

    /// Whether its box is to be drawn: it has run [`SHOW_AFTER`].
    #[must_use]
    pub fn shown(&self) -> bool {
        self.started.elapsed() >= SHOW_AFTER
    }

    /// How long until the box is to be drawn, if it is not yet.
    #[must_use]
    pub fn shown_in(&self) -> Option<Duration> {
        SHOW_AFTER.checked_sub(self.started.elapsed())
    }

    /// Entries removed, and in all — the latter as the scan counted them, so never less than
    /// the former (files written since the scan push it past what was counted).
    #[must_use]
    pub fn progress(&self) -> (u64, u64) {
        let removed = self.shared.tally.removed();
        (removed, self.total.max(removed))
    }

    /// The share done, from 0 to 1.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        let (removed, total) = self.progress();
        if total == 0 {
            0.0
        } else {
            removed as f64 / total as f64
        }
    }

    /// Stop after the entry being removed.
    pub fn cancel(&self) {
        self.shared.tally.stop();
    }

    #[must_use]
    pub fn cancelling(&self) -> bool {
        self.shared.tally.stopped()
    }

    /// The box's words: its title, the name of the entry being removed, and the count.
    #[must_use]
    pub fn words(&self) -> (String, String, String) {
        let verb = if self.freed { "Deleting" } else { "Moving" };
        let title = if self.cancelling() {
            "Stopping…".to_string()
        } else if self.freed {
            match self.files.len() {
                1 => "Deleting…".to_string(),
                n => format!("{verb} {} entries…", DisplayCount(n as u64)),
            }
        } else {
            match self.files.len() {
                1 => "Moving to the Trash…".to_string(),
                n => format!("{verb} {} entries to the Trash…", DisplayCount(n as u64)),
            }
        };
        let on = self.shared.on.load(Ordering::Relaxed);
        // Its name, not its path: a path cut short to fit loses the name, at its end.
        let name = self
            .files
            .get(on)
            .and_then(|file| file.path_to_file.last())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (removed, total) = self.progress();
        let count = format!(
            "{} of {} removed",
            DisplayCount(removed),
            DisplayCount(total)
        );
        (title, name, count)
    }
}

/// The delete box laid out in a window's bounds, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeletionLayout {
    pub panel: Rect,
    pub title: Rect,
    /// The name of the entry being removed.
    pub path: Rect,
    pub bar: Rect,
    /// The count, under the bar.
    pub count: Rect,
    pub cancel: Rect,
}

/// The box's width at most, its height, a line's height and the padding inside, in points.
pub const WIDTH: f64 = 440.0;
pub const HEIGHT: f64 = 150.0;
pub const LINE: f64 = 20.0;
pub const PAD: f64 = 16.0;
pub const BUTTON: (f64, f64) = (88.0, 28.0);

impl DeletionLayout {
    /// Centred in `bounds`, narrower when they are.
    #[must_use]
    pub fn new(bounds: Rect) -> Self {
        let w = WIDTH.min((bounds.w - 32.0).max(160.0));
        let panel = Rect::new(
            bounds.x + ((bounds.w - w) / 2.0).max(0.0),
            bounds.y + ((bounds.h - HEIGHT) / 2.0).max(0.0),
            w,
            HEIGHT,
        );
        let inner = panel.inset(PAD, PAD);
        let title = Rect::new(inner.x, inner.y, inner.w, LINE);
        let path = Rect::new(inner.x, title.bottom(), inner.w, LINE);
        let bar = Rect::new(inner.x, path.bottom() + 8.0, inner.w, 6.0);
        let count = Rect::new(inner.x, bar.bottom() + 6.0, inner.w, LINE);
        let cancel = Rect::new(
            inner.right() - BUTTON.0,
            inner.bottom() - BUTTON.1,
            BUTTON.0,
            BUTTON.1,
        );
        Self {
            panel,
            title,
            path,
            bar,
            count,
            cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_box_fits_in_a_narrow_window_and_its_parts_in_it() {
        for width in [200.0, 1200.0] {
            let bounds = Rect::new(0.0, 0.0, width, 600.0);
            let layout = DeletionLayout::new(bounds);
            assert!(layout.panel.x >= 0.0 && layout.panel.right() <= width);
            for part in [
                layout.title,
                layout.path,
                layout.bar,
                layout.count,
                layout.cancel,
            ] {
                assert!(part.x >= layout.panel.x && part.right() <= layout.panel.right());
                assert!(part.y >= layout.panel.y && part.bottom() <= layout.panel.bottom());
            }
            assert!(
                layout.count.bottom() <= layout.cancel.y,
                "the count clears the button"
            );
        }
    }
}
