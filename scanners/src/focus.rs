//! Steering a walk toward the folder the user is in ([`Focus`]): the directories on the way
//! down to it and under it are read before any other, so that what is on screen fills in first.
//! The walkers keep their stacks — a worker's own, and the shared queue — and these put the
//! jobs toward the focus where they are popped next. With no focus nothing is touched, and
//! finding that out is one atomic load.

use ::std::path::Path;

pub use libduscape::scan::{Focus, FocusWatch};

/// Arrange a stack popped from its end so that the jobs toward the focus come first: the whole
/// stack when the focus has moved since last asked, else only the `pushed` jobs just put on it.
/// The order among the jobs moved, and among the rest, is kept.
// The macOS walker keeps one shared queue, partitioned as it fills (`macos::QueueState`).
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn arrange<J>(
    watch: &mut FocusWatch,
    stack: &mut Vec<J>,
    pushed: usize,
    path: impl Fn(&J) -> &Path,
) {
    let moved = watch.refresh();
    if watch.current().is_none() {
        return;
    }
    let from = if moved { 0 } else { stack.len() - pushed };
    let tail = &stack[from..];
    // The common case with a focus: this directory's children lie elsewhere, nothing moves.
    if tail.len() < 2 || !tail.iter().any(|job| watch.toward(path(job))) {
        return;
    }
    let (toward, rest): (Vec<J>, Vec<J>) =
        stack.drain(from..).partition(|job| watch.toward(path(job)));
    stack.extend(rest);
    stack.extend(toward);
}

/// The job toward the focus nearest the end of a shared queue, taken out — for a steal, which
/// otherwise pops the end. `None` with no focus, or nothing toward it.
// The macOS walker keeps one shared queue, partitioned as it fills (`macos::QueueState`).
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn take_toward<J>(
    watch: &mut FocusWatch,
    queue: &mut Vec<J>,
    path: impl Fn(&J) -> &Path,
) -> Option<J> {
    watch.refresh();
    watch.current()?;
    let index = queue.iter().rposition(|job| watch.toward(path(job)))?;
    Some(queue.remove(index))
}

#[cfg(test)]
mod tests {
    use ::std::path::{Path, PathBuf};

    use super::{Focus, arrange, take_toward};

    fn jobs(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| Path::new("/r").join(name))
            .collect()
    }

    fn names(jobs: &[PathBuf]) -> Vec<String> {
        jobs.iter()
            .map(|job| {
                job.strip_prefix("/r")
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    /// The jobs toward the focus go to the end of the stack, popped first: the folder itself,
    /// what is under it, and the folders on the way down to it. A move re-sorts the whole
    /// stack; after it, only what was just pushed.
    #[test]
    fn the_jobs_toward_the_focus_are_popped_first() {
        let focus = Focus::new();
        let mut watch = focus.watch_under(Path::new("/r"));
        let mut stack = jobs(&["a", "b", "c"]);
        arrange(&mut watch, &mut stack, 3, |job| job.as_path());
        assert_eq!(names(&stack), ["a", "b", "c"], "no focus: untouched");

        focus.set(Some(Path::new("/r").join("b").join("x")));
        arrange(&mut watch, &mut stack, 0, |job| job.as_path());
        assert_eq!(names(&stack), ["a", "c", "b"], "b is on the way down");

        // b read: its children pushed, x last.
        stack.pop();
        stack.extend(jobs(&["b/x", "b/y"]));
        arrange(&mut watch, &mut stack, 2, |job| job.as_path());
        assert_eq!(names(&stack), ["a", "c", "b/y", "b/x"]);
        stack.pop();
        stack.extend(jobs(&["b/x/1", "b/x/2"]));
        arrange(&mut watch, &mut stack, 2, |job| job.as_path());
        assert_eq!(
            names(&stack),
            ["a", "c", "b/y", "b/x/1", "b/x/2"],
            "all under: kept as pushed"
        );

        // The user goes elsewhere: the whole stack again.
        focus.set(Some(Path::new("/r").join("c")));
        arrange(&mut watch, &mut stack, 0, |job| job.as_path());
        assert_eq!(names(&stack), ["a", "b/y", "b/x/1", "b/x/2", "c"]);
        focus.set(None);
        stack.extend(jobs(&["c/1"]));
        arrange(&mut watch, &mut stack, 1, |job| job.as_path());
        assert_eq!(names(&stack), ["a", "b/y", "b/x/1", "b/x/2", "c", "c/1"]);
    }

    #[test]
    fn a_steal_takes_the_job_toward_the_focus_nearest_the_end() {
        let focus = Focus::new();
        let mut watch = focus.watch_under(Path::new("/r"));
        let mut queue = jobs(&["a", "b/x", "c", "b", "d"]);
        assert!(take_toward(&mut watch, &mut queue, |job| job.as_path()).is_none());
        focus.set(Some(Path::new("/r").join("b").join("x")));
        let taken = take_toward(&mut watch, &mut queue, |job| job.as_path()).unwrap();
        assert_eq!(names(&[taken]), ["b"]);
        let taken = take_toward(&mut watch, &mut queue, |job| job.as_path()).unwrap();
        assert_eq!(names(&[taken]), ["b/x"]);
        assert!(take_toward(&mut watch, &mut queue, |job| job.as_path()).is_none());
        assert_eq!(names(&queue), ["a", "c", "d"]);
    }
}
