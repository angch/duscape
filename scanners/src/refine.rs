//! The second pass: small files that may share their extents with another file.
//!
//! The walk probes a file for shared extents only from `PROBE_ABOVE_BYTES` (64 KiB) up, because a
//! probe is an `openat` and an `ioctl` and most files are small. But small files are most of what
//! a btrfs snapshot holds: on `/usr` 96% of files and 13.6% of bytes are under 64 KiB, so a
//! snapshot of `/` scanned that way is overstated by roughly an eighth of itself.
//!
//! So the walk notes them instead, in [`SmallFiles`], and they are probed after the tree is built
//! and on screen — without holding it up — by [`refine`], whose findings [`FileTree::apply_found`]
//! folds in. That is the same reconciliation the parallel build already relies on: the files were
//! counted in full, and charging each to the ledger now says which ancestors had counted the same
//! blocks already and should give them back. The ledger's answers do not depend on order, so the
//! files can be probed in whatever order is most useful — the folder the user is looking at first.
//!
//! [`FileTree::apply_found`]: libdiskonaut::FileTree::apply_found

use ::std::collections::BTreeMap;
use ::std::ffi::OsStr;
use ::std::ops::Bound;
use ::std::path::{Path, PathBuf};
use ::std::sync::{Arc, Mutex};

use crate::DirEntries;
pub use libdiskonaut::scan::{Found, FoundFile};

/// Files smaller than this are left out of the second pass too: a file this small is usually
/// stored inline in the filesystem's metadata (btrfs), where FIEMAP reports no shared extent to
/// find.
pub const REFINE_ABOVE_BYTES: u64 = 4096;

/// Small files noted during the walk for the second pass, by directory.
///
/// Kept in path order, so that everything under one folder is one contiguous range and can be
/// taken first when the user is looking at it.
#[derive(Debug, Default)]
pub struct SmallFiles {
    dirs: BTreeMap<Arc<Path>, Pending>,
    files: usize,
}

/// One directory's small files: names end to end, where each ends, and what each was counted as.
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct Pending {
    extent_space: u64,
    names: Vec<u8>,
    ends: Vec<u32>,
    sizes: Vec<libdiskonaut::model::Sizes>,
}

impl Pending {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn iter(&self) -> impl Iterator<Item = (&OsStr, libdiskonaut::model::Sizes)> {
        let mut start = 0usize;
        self.ends.iter().zip(&self.sizes).map(move |(&end, &size)| {
            let end = end as usize;
            // SAFETY: the bytes came from `OsStr::as_encoded_bytes()`, split where they were joined.
            let name = unsafe { OsStr::from_encoded_bytes_unchecked(&self.names[start..end]) };
            start = end;
            (name, size)
        })
    }
}

impl SmallFiles {
    /// Note the entries of `directory` the walk left for the second pass.
    pub fn note(&mut self, directory: &DirEntries) {
        if directory.later.is_empty() {
            return;
        }
        let mut pending = Pending {
            extent_space: directory.extent_space,
            ..Pending::default()
        };
        for &index in &directory.later {
            let entry = &directory.entries()[index as usize];
            pending
                .names
                .extend_from_slice(directory.name(entry).as_encoded_bytes());
            pending
                .ends
                .push(u32::try_from(pending.names.len()).unwrap_or(u32::MAX));
            pending.sizes.push(libdiskonaut::model::Sizes::of(
                entry.meta.size,
                entry.meta.apparent,
            ));
        }
        self.files += pending.sizes.len();
        self.dirs.insert(Arc::clone(&directory.path), pending);
    }

    /// Files still to probe.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    /// Take the next directory: the first under `focus` if there is one, otherwise the first.
    fn take(&mut self, focus: Option<&Path>) -> Option<(Arc<Path>, Pending)> {
        let under_focus = focus.and_then(|focus| {
            self.dirs
                .range::<Path, _>((Bound::Included(focus), Bound::Unbounded))
                .next()
                .filter(|(dir, _)| dir.starts_with(focus))
                .map(|(dir, _)| Arc::clone(dir))
        });
        let key = under_focus.or_else(|| self.dirs.keys().next().cloned())?;
        let pending = self.dirs.remove(&key)?;
        self.files -= pending.sizes.len();
        Some((key, pending))
    }
}

/// Probe every noted file on `threads` threads, the directories under `focus` (as it is at each
/// moment) first, handing findings to `deliver` in batches. Stops early when `keep_going` says to
/// or `deliver` returns `false`. Returns how many files were left unprobed.
pub fn refine(
    small: SmallFiles,
    threads: usize,
    focus: &Mutex<Option<PathBuf>>,
    keep_going: &(dyn Fn() -> bool + Sync),
    mut deliver: impl FnMut(Vec<Found>, usize) -> bool,
) -> usize {
    use ::std::sync::mpsc::{RecvTimeoutError, channel};
    use ::std::time::{Duration, Instant};

    let queue = Mutex::new(small);
    let (sender, receiver) = channel::<Option<Found>>();
    let stopped = ::std::sync::atomic::AtomicBool::new(false);
    let still_going = || keep_going() && !stopped.load(::std::sync::atomic::Ordering::Relaxed);
    ::std::thread::scope(|scope| {
        for index in 0..threads.max(1) {
            let sender = sender.clone();
            let queue = &queue;
            let still_going = &still_going;
            ::std::thread::Builder::new()
                .name(format!("refine_{index}"))
                .spawn_scoped(scope, move || {
                    while still_going() {
                        let next = {
                            let focus = focus.lock().ok().and_then(|focus| focus.clone());
                            queue
                                .lock()
                                .ok()
                                .and_then(|mut queue| queue.take(focus.as_deref()))
                        };
                        let Some((dir, pending)) = next else {
                            break;
                        };
                        let found = probe(dir, &pending);
                        if sender.send(found).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawn a refine worker");
        }
        drop(sender);

        // Batched, so the rendering thread is asked to redraw a few times a second rather than
        // once per directory.
        let mut batch = Vec::new();
        let mut since = Instant::now();
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(found)) => batch.push(found),
                Ok(None) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if since.elapsed() >= Duration::from_millis(200) || batch.len() >= 1024 {
                let left = queue.lock().map_or(0, |queue| queue.len());
                if !deliver(::std::mem::take(&mut batch), left) {
                    stopped.store(true, ::std::sync::atomic::Ordering::Relaxed);
                }
                since = Instant::now();
            }
        }
        if !stopped.load(::std::sync::atomic::Ordering::Relaxed) {
            let left = queue.lock().map_or(0, |queue| queue.len());
            deliver(batch, left);
        }
    });
    queue.into_inner().map_or(0, |queue| queue.len())
}

/// Probe one directory's small files. `None` when none of them is wholly shared.
#[cfg(target_os = "linux")]
fn probe(dir: Arc<Path>, pending: &Pending) -> Option<Found> {
    use ::rustix::fs::{Mode, OFlags, openat};
    use ::std::ffi::CString;
    use ::std::os::fd::AsFd;

    let fd = openat(
        ::rustix::fs::CWD,
        dir.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let files: Vec<FoundFile> = pending
        .iter()
        .filter_map(|(name, sizes)| {
            let cname = CString::new(name.as_encoded_bytes()).ok()?;
            let identity = super::linux::shared_identity(fd.as_fd(), &cname, pending.extent_space)?;
            Some(FoundFile {
                name: name.to_os_string(),
                sizes,
                identity,
            })
        })
        .collect();
    (!files.is_empty()).then_some(Found { dir, files })
}

/// Only the Linux walk notes small files, so there is never anything to probe elsewhere.
#[cfg(not(target_os = "linux"))]
fn probe(_dir: Arc<Path>, _pending: &Pending) -> Option<Found> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirEntries, EntryMeta};

    fn noted(paths: &[&str]) -> SmallFiles {
        let mut small = SmallFiles::default();
        for path in paths {
            let mut directory = DirEntries::new(Arc::from(Path::new(path)));
            directory.push(
                OsStr::new("f"),
                EntryMeta {
                    size: 8192,
                    ..EntryMeta::default()
                },
            );
            directory.later.push(0);
            small.note(&directory);
        }
        small
    }

    #[test]
    fn the_folder_in_view_and_everything_under_it_goes_first() {
        let mut small = noted(&["/a", "/home", "/home/u/x", "/home2", "/z", "/home/u"]);
        assert_eq!(small.len(), 6);
        let focus = Some(Path::new("/home/u"));
        let order: Vec<PathBuf> = ::std::iter::from_fn(|| small.take(focus))
            .map(|(dir, _)| dir.to_path_buf())
            .collect();
        let order: Vec<&str> = order.iter().map(|path| path.to_str().unwrap()).collect();
        assert_eq!(
            order,
            ["/home/u", "/home/u/x", "/a", "/home", "/home2", "/z"]
        );
        assert!(small.is_empty());
    }
}
