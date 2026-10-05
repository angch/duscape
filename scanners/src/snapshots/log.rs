//! What the volume's change log says about a snapshot: which folders changed since it.
//!
//! Three facts, measured on macOS 26 (2026-10-05), decide the shape:
//! - The log has no answer for "the id at this time": `FSEventsGetLastEventIdForDeviceBeforeTime`
//!   gives 0 (the start of history) for an hour ago as for a week ago. The log's own files do:
//!   `.fseventsd` at the volume's root (root only, as reading a snapshot is) holds the log in
//!   files named by an event id in hex, each written before its modification time. Every event
//!   in a file finished before the snapshot was taken is older than the snapshot, so the
//!   greatest id named by such a file is an id to replay from ([`since_for`]) — whether a name
//!   is its file's first id or its last, the replay starts no later than the snapshot. No such
//!   file, and the log does not reach back to it.
//! - On `/` the log spells the data volume's paths through their firmlinks (`/Users/…`, not
//!   `/System/Volumes/Data/Users/…`): a stream on the data volume found nearly every event
//!   "outside" it. So the stream is on `/`, and each path is taken back onto the volume by the
//!   system's own table, `/usr/share/firmlinks` ([`on_volume`]).
//! - A replay of a long history drops live events with `MustScanSubDirs` on the root, which
//!   `fsevents::events` leaves out (they are after the replay began).

use ::std::collections::BTreeSet;
use ::std::ffi::OsString;
use ::std::path::{Component, Path, PathBuf};
use ::std::time::SystemTime;

use super::Scope;

/// A log file of `.fseventsd`: the id its name gives, and when it was last written.
pub type LogFile = (u64, SystemTime);

/// The log files among `entries` (a name and a modification time each): those named by
/// sixteen hex digits. The log keeps other files beside them (`fseventsd-uuid`).
pub fn log_files(entries: impl IntoIterator<Item = (OsString, SystemTime)>) -> Vec<LogFile> {
    entries
        .into_iter()
        .filter_map(|(name, time)| {
            let name = name.to_str()?;
            (name.len() == 16)
                .then(|| u64::from_str_radix(name, 16).ok())
                .flatten()
                .map(|id| (id, time))
        })
        .collect()
}

/// The id to replay from for a snapshot made at `created`: the greatest named by a log file
/// written before then. `None` when no file is that old: the log does not reach back to it.
#[must_use]
pub fn since_for(created: SystemTime, files: &[LogFile]) -> Option<u64> {
    files
        .iter()
        .filter(|(_, written)| *written < created)
        .map(|(id, _)| *id)
        .max()
}

/// `/usr/share/firmlinks`: each line a path on `/` and, after a tab, where it is on the data
/// volume, relative to its root.
#[must_use]
pub fn parse_firmlinks(text: &str) -> Vec<(PathBuf, PathBuf)> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(from, to)| (PathBuf::from(from.trim()), PathBuf::from(to.trim())))
        .filter(|(from, _)| from.is_absolute())
        .collect()
}

/// Where the log's `path` is on the volume mounted at `volume`, from its root: under the
/// volume's own path, or through the firmlink that names it (the longest, as
/// `/System/Library/Caches` is inside `/System`). `/` itself is the root's when the log lost
/// events there (`whole`: everything under it is to be read again), and otherwise only the
/// system volume's own listing. `None` for a path on no part of the volume: the sealed system
/// volume's, which no snapshot of the data volume holds.
#[must_use]
pub fn on_volume(
    path: &Path,
    whole: bool,
    volume: &Path,
    firmlinks: &[(PathBuf, PathBuf)],
) -> Option<PathBuf> {
    if path.components().all(|c| matches!(c, Component::RootDir)) {
        return whole.then(PathBuf::new);
    }
    if let Ok(relative) = path.strip_prefix(volume) {
        return Some(relative.components().collect());
    }
    firmlinks
        .iter()
        .filter(|(from, _)| path.starts_with(from))
        .max_by_key(|(from, _)| from.components().count())
        .and_then(|(from, to)| Some(to.join(path.strip_prefix(from).ok()?)))
}

/// An event taken onto the volume: where (from its root), its id, and whether the log lost
/// events under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub relative: PathBuf,
    pub id: u64,
    pub whole: bool,
}

/// Each snapshot's [`Scope`]: those whose `since` the log reaches read where `events` (since
/// the earliest of them) named after it; the rest, or all when the replay failed (`events` an
/// error saying why), read whole. Lost events under the root since a snapshot read it whole
/// too.
pub fn scopes(
    snapshots: &[(OsString, Option<u64>)],
    events: Result<&[Named], &str>,
) -> Vec<(OsString, Scope)> {
    snapshots
        .iter()
        .map(|(name, since)| {
            let scope = match (since, events) {
                (None, _) => Scope::Whole("the change log does not reach back to it".to_string()),
                (Some(_), Err(why)) => Scope::Whole(why.to_string()),
                (Some(since), Ok(events)) => scope_since(*since, events),
            };
            (name.clone(), scope)
        })
        .collect()
}

fn scope_since(since: u64, events: &[Named]) -> Scope {
    let mut listed = BTreeSet::new();
    let mut walked = BTreeSet::new();
    let mut count = 0;
    for event in events.iter().filter(|event| event.id > since) {
        count += 1;
        if event.whole {
            walked.insert(event.relative.clone());
        } else {
            listed.insert(event.relative.clone());
        }
    }
    if walked.contains(Path::new("")) {
        return Scope::Whole("the change log lost events under the volume since it".to_string());
    }
    Scope::Changed {
        since,
        events: count,
        listed: listed.into_iter().collect(),
        walked: walked.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;
    use ::std::path::{Path, PathBuf};
    use ::std::time::{Duration, SystemTime};

    use super::{Named, Scope, log_files, on_volume, parse_firmlinks, scopes, since_for};

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn the_id_before_a_snapshot_is_the_greatest_of_the_files_written_before_it() {
        let files = log_files([
            (OsString::from("0000000000000100"), at(10)),
            (OsString::from("0000000000000200"), at(20)),
            (OsString::from("00000000000002ff"), at(30)),
            (OsString::from("fseventsd-uuid"), at(1)),
        ]);
        assert_eq!(files.len(), 3);
        assert_eq!(since_for(at(25), &files), Some(0x200));
        assert_eq!(since_for(at(31), &files), Some(0x2ff));
        assert_eq!(since_for(at(10), &files), None, "nothing written before it");
    }

    #[test]
    fn the_log_s_paths_are_taken_onto_the_data_volume() {
        let firmlinks = parse_firmlinks(
            "/Users\tUsers\n/Library\tLibrary\n/System/Library/Caches\tSystem/Library/Caches\n\
             nonsense\n",
        );
        assert_eq!(firmlinks.len(), 3);
        let data = Path::new("/System/Volumes/Data");
        let on = |path: &str, whole: bool| on_volume(Path::new(path), whole, data, &firmlinks);
        assert_eq!(
            on("/Users/a/Downloads/", false),
            Some("Users/a/Downloads".into())
        );
        assert_eq!(
            on("/System/Volumes/Data/.Spotlight-V100/", false),
            Some(".Spotlight-V100".into())
        );
        assert_eq!(
            on("/System/Library/Caches/x", false),
            Some("System/Library/Caches/x".into())
        );
        assert_eq!(
            on("/System/Library/Frameworks", false),
            None,
            "the sealed volume"
        );
        assert_eq!(on("/UsersX", false), None, "a name, not a prefix");
        assert_eq!(on("/", false), None);
        assert_eq!(on("/", true), Some(PathBuf::new()), "lost under the root");
        assert_eq!(on("/System/Volumes/Data", true), Some(PathBuf::new()));
    }

    #[test]
    fn each_snapshot_reads_what_the_log_named_after_it() {
        let named = |relative: &str, id: u64, whole: bool| Named {
            relative: relative.into(),
            id,
            whole,
        };
        let events = [
            named("Users/a", 5, false),
            named("Users/b", 15, false),
            named("Library/x", 25, true),
            named("Users/a", 30, false),
        ];
        let snapshots = [
            (OsString::from("old"), Some(1)),
            (OsString::from("new"), Some(20)),
            (OsString::from("older than the log"), None),
        ];
        let read = scopes(&snapshots, Ok(&events));
        assert_eq!(
            read[0].1,
            Scope::Changed {
                since: 1,
                events: 4,
                listed: vec!["Users/a".into(), "Users/b".into()],
                walked: vec!["Library/x".into()],
            }
        );
        assert_eq!(
            read[1].1,
            Scope::Changed {
                since: 20,
                events: 2,
                listed: vec!["Users/a".into()],
                walked: vec!["Library/x".into()],
            }
        );
        assert!(matches!(read[2].1, Scope::Whole(_)));
        assert!(
            scopes(&snapshots, Err("gave up"))
                .iter()
                .all(|(_, scope)| matches!(scope, Scope::Whole(_)))
        );
        let lost = [named("", 30, true)];
        assert!(matches!(
            scopes(&snapshots[..1], Ok(&lost))[0].1,
            Scope::Whole(_)
        ));
    }
}
