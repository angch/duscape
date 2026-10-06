//! The idle pass over archives: each one's index read ([`libduscape::archive::read_index`]) so
//! the viewer can show it as a folder of its entries (`FileTree::expand_archive`).
//!
//! Only what the viewer asks for is read ([`Wanted`]): the archives on screen with a tile big
//! enough to show what is in them, or a row in the list. An archive that would change nothing
//! visible — off screen, or a tile too small to nest anything in — waits until a zoom, a
//! folder change or a scroll brings it into view. A package cache of a hundred thousand jars
//! costs a read an archive the user can see, not a hundred thousand reads up front.

use ::std::collections::HashSet;
use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc;
use ::std::sync::{Condvar, Mutex};
use ::std::thread;
use ::std::time::{Duration, Instant};

use libduscape::archive::Member;

/// An archive read: its path from the scan root, and its entries, or why it could not be read
/// (it is no zip, or not readable), when it stays a file.
pub type Read = (PathBuf, Result<Vec<Member>, String>);

/// What the viewer wants read, most wanted first, and what is being read.
#[derive(Default)]
pub struct Wanted {
    state: Mutex<WantedState>,
    changed: Condvar,
}

#[derive(Default)]
struct WantedState {
    queue: Vec<PathBuf>,
    /// Taken by a worker and not yet [`Wanted::done`]: asked for again meanwhile (the viewer
    /// asks at every relayout), it is not read twice. Kept only until it is reported — kept for
    /// good, an archive a folder's rescan brought back as a file was never read again.
    reading: HashSet<PathBuf>,
}

impl Wanted {
    /// The archives to read now, in the order to read them; what was asked before and not yet
    /// taken is dropped — it is no longer on screen.
    pub fn set(&self, paths: Vec<PathBuf>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner);
        state.queue = paths;
        self.changed.notify_all();
    }

    /// `path` has been read and reported: asked for again, say after a rescan brought it back
    /// as a file, it is read again.
    pub fn done(&self, path: &Path) {
        self.state
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner)
            .reading
            .remove(path);
    }

    /// Wake the workers waiting for something to read, to see that they are to stop.
    pub fn wake(&self) {
        self.changed.notify_all();
    }

    /// The next archive to read, waiting up to `wait` for one to be asked for.
    fn next(&self, wait: Duration) -> Option<PathBuf> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner);
        loop {
            while !state.queue.is_empty() {
                let path = state.queue.remove(0);
                if state.reading.insert(path.clone()) {
                    return Some(path);
                }
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(::std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.queue.is_empty() {
                return None;
            }
        }
    }

    /// Whether nothing is asked for and not yet taken.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner)
            .queue
            .is_empty()
    }
}

/// How long the pass gathers what it read before reporting it: one relayout for a burst of
/// archives, not one each, and soon enough to be on screen in the next frame or two.
const GATHER: Duration = Duration::from_millis(30);

/// Read what `wanted` asks for, archives under `root`, on `threads` threads, until
/// `keep_going` says to stop, and hand each gathering to `report` (which says whether to go on).
pub fn run(
    root: &Path,
    wanted: &Wanted,
    threads: usize,
    keep_going: &(dyn Fn() -> bool + Sync),
    report: &mut dyn FnMut(Vec<Read>) -> bool,
) {
    let (sender, results) = mpsc::channel::<Read>();
    // The workers' own stop, for when `report` says to: `keep_going` alone would keep them
    // waiting for more.
    let stop = ::std::sync::atomic::AtomicBool::new(false);
    let going = || keep_going() && !stop.load(::std::sync::atomic::Ordering::Acquire);
    thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            let sender = sender.clone();
            let going = &going;
            scope.spawn(move || {
                while going() {
                    // Woken when asked for more and when stopped (`Wanted::wake`); the timeout
                    // only a backstop, so an idle pass costs nothing.
                    let Some(relative) = wanted.next(Duration::from_secs(1)) else {
                        continue;
                    };
                    let read = libduscape::archive::read_index(&root.join(&relative))
                        .map(|index| index.members)
                        .map_err(|error| error.to_string());
                    if sender.send((relative, read)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        let mut gathered: Vec<Read> = Vec::new();
        let mut since: Option<Instant> = None;
        loop {
            // Nothing gathered, it waits for a read (or for the workers to end, stopped);
            // something gathered, for no longer than the rest of the gathering.
            let received = if gathered.is_empty() {
                results
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            } else {
                results.recv_timeout(GATHER)
            };
            match received {
                Ok(read) => {
                    since.get_or_insert_with(Instant::now);
                    gathered.push(read);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let due = since.is_some_and(|since| since.elapsed() >= GATHER) || gathered.len() >= 256;
            if !gathered.is_empty() && (due || wanted.is_idle()) {
                since = None;
                if !report(::std::mem::take(&mut gathered)) {
                    break;
                }
            }
            if !keep_going() {
                break;
            }
        }
        stop.store(true, ::std::sync::atomic::Ordering::Release);
        wanted.wake();
    });
}
