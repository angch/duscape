//! The snapshot pass on plain folders: a "snapshot" whose files shared with the live volume are
//! hard links to the live ones (one id, as APFS keeps it), and a lookup by id over the live
//! folder standing in for volfs.

use ::std::ffi::OsString;
use ::std::path::PathBuf;
use ::std::time::{Duration, SystemTime};

use super::{Listed, Mounted, parse_listing};

#[test]
fn mounted_is_undone_when_dropped() {
    let undone = ::std::sync::Arc::new(::std::sync::atomic::AtomicBool::new(false));
    let flag = ::std::sync::Arc::clone(&undone);
    drop(Mounted::new(PathBuf::from("/nowhere"), move || {
        flag.store(true, ::std::sync::atomic::Ordering::Relaxed);
    }));
    assert!(undone.load(::std::sync::atomic::Ordering::Relaxed));
}

/// Records as `fs_snapshot_list` lays them out on macOS 26: length, five `u32`s of attributes
/// returned, the name's reference (offset from itself, length with the NUL), the creation time
/// (seconds, nanoseconds), the flags, the name, padding.
#[test]
fn snapshot_names_times_and_dataless_are_parsed_from_the_listing() {
    let record = |name: &str, seconds: i64, flags: u32| {
        let mut bytes = Vec::new();
        let padded = (4 + 20 + 8 + 16 + 4 + name.len() + 1).next_multiple_of(8);
        bytes.extend((padded as u32).to_ne_bytes());
        bytes.extend(
            [0x8004_0201u32, 0, 0, 0, 0]
                .iter()
                .flat_map(|word| word.to_ne_bytes()),
        );
        bytes.extend(28i32.to_ne_bytes());
        bytes.extend((name.len() as u32 + 1).to_ne_bytes());
        bytes.extend(seconds.to_ne_bytes());
        bytes.extend(5i64.to_ne_bytes());
        bytes.extend(flags.to_ne_bytes());
        bytes.extend(name.as_bytes());
        bytes.resize(padded, 0);
        bytes
    };
    let tm = "com.apple.TimeMachine.2026-09-17-151335.local";
    let mut buffer = record(tm, 1_789_629_218, 0x20);
    buffer.extend(record("b", 7, 0));
    let at = |seconds: u64| SystemTime::UNIX_EPOCH + Duration::new(seconds, 5);
    assert_eq!(
        parse_listing(&buffer, 2),
        [
            Listed {
                name: OsString::from(tm),
                made: at(1_789_629_218),
                dataless: true,
            },
            Listed {
                name: OsString::from("b"),
                made: at(7),
                dataless: false,
            },
        ]
    );
    // A count beyond the records, or a record cut short, stops at what is whole.
    assert_eq!(parse_listing(&buffer, 3).len(), 2);
    assert!(parse_listing(&buffer[..40], 2).is_empty());
    // An offset leaving the record, either way, ends the parse.
    for offset in [-1000i32, 1000] {
        let mut bad = record(tm, 1, 0);
        bad[24..28].copy_from_slice(&offset.to_ne_bytes());
        assert!(parse_listing(&bad, 1).is_empty(), "{offset}");
    }
}

/// On folders: the fixtures need file ids, which the `std` lister gives on unix alone (Windows
/// lists none: `fill::identity`), and the pass is macOS's.
#[cfg(unix)]
mod on_folders {
    use ::std::collections::{HashMap, HashSet};
    use ::std::ffi::{OsStr, OsString};
    use ::std::fs;
    use ::std::path::{Path, PathBuf};

    use libduscape::model::FileTree;
    use libduscape::snapshots::FOLDER;

    use super::super::{Lookups, Mounted, Pass, Scope, Walked, build, gone_from_live};

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("create");
        fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    fn link(live: &Path, snapshot: &Path) {
        fs::create_dir_all(snapshot.parent().expect("a parent")).expect("create");
        fs::hard_link(live, snapshot).expect("link");
    }

    fn id(path: &Path) -> u64 {
        use ::std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path).expect("there").ino()
    }

    /// Every id under `root`, folders' to their paths and all of them as a set.
    fn ids(root: &Path, folders: &mut HashMap<u64, PathBuf>, all: &mut HashSet<u64>) {
        for entry in fs::read_dir(root).expect("list") {
            let path = entry.expect("entry").path();
            all.insert(id(&path));
            if path.is_dir() {
                folders.insert(id(&path), path.clone());
                ids(&path, folders, all);
            }
        }
    }

    /// The fixture: `live`, and two snapshots of it from before files went.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = ::std::env::temp_dir().join(format!("duscape_snapshots_{name}"));
            let _ = fs::remove_dir_all(&dir);
            let (live, first, second) = (dir.join("live"), dir.join("first"), dir.join("second"));
            write(&live.join("kept.txt"), 100);
            write(&live.join("docs/a.txt"), 100);
            write(&live.join("docs/moved here.txt"), 100);
            write(&live.join("renamed to/r.txt"), 100);

            // The first snapshot: what is still live is the live file; the rest it alone holds.
            link(&live.join("kept.txt"), &first.join("kept.txt"));
            write(&first.join("gone.txt"), 1000);
            link(&live.join("docs/a.txt"), &first.join("docs/a.txt"));
            write(&first.join("docs/deleted.bin"), 2000);
            // A folder deleted since, whose one file was moved out of it first.
            link(&live.join("docs/moved here.txt"), &first.join("old/m.txt"));
            // A folder renamed since, one of whose files was deleted.
            link(
                &live.join("renamed to/r.txt"),
                &first.join("renamed from/r.txt"),
            );
            write(&first.join("renamed from/x.txt"), 3000);

            // The second keeps `gone.txt` too — the same file, so one id — and one of its own.
            link(&first.join("gone.txt"), &second.join("gone.txt"));
            link(&live.join("kept.txt"), &second.join("kept.txt"));
            write(&second.join("other.txt"), 4000);
            Self { dir }
        }

        fn live(&self) -> PathBuf {
            self.dir.join("live")
        }

        /// Which live folder each snapshot folder is, as APFS would say by their one id: here by
        /// name, but for the renamed one.
        fn counterparts(&self) -> HashMap<u64, PathBuf> {
            let mut map = HashMap::new();
            for snapshot in ["first", "second"] {
                let root = self.dir.join(snapshot);
                let (mut folders, mut all) = (HashMap::new(), HashSet::new());
                ids(&root, &mut folders, &mut all);
                for (folder_id, path) in folders {
                    let relative = path.strip_prefix(&root).expect("under it");
                    let relative = if relative == Path::new("renamed from") {
                        Path::new("renamed to")
                    } else {
                        relative
                    };
                    let live = self.live().join(relative);
                    if live.is_dir() {
                        map.insert(folder_id, live);
                    }
                }
            }
            map
        }

        fn live_ids(&self) -> HashSet<u64> {
            let (mut folders, mut all) = (HashMap::new(), HashSet::new());
            ids(&self.live(), &mut folders, &mut all);
            all
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// What the tests vary: which folders to read, and what a file written over holds.
    struct Run<'a> {
        scope: Scope,
        written_over: &'a (dyn Fn(&Path, u64) -> u64 + Sync),
        keep_going: bool,
    }

    impl Default for Run<'_> {
        fn default() -> Self {
            Self {
                scope: Scope::Whole("the test".to_string()),
                written_over: &|_, _| 0,
                keep_going: true,
            }
        }
    }

    /// The fixture's lookups, for `run` with the pass they make.
    fn with_pass<R>(fixture: &Fixture, run: &Run<'_>, then: impl FnOnce(&Pass<'_>) -> R) -> R {
        let counterparts = fixture.counterparts();
        let live_ids = fixture.live_ids();
        let live_root = fixture.live();
        let list = |path: &Path| crate::fill::list_for_tests(path);
        let live_folder = |id: u64| counterparts.get(&id).cloned();
        let live = |id: u64| live_ids.contains(&id);
        let same_place =
            |relative: &Path, id: u64| counterparts.get(&id) == Some(&live_root.join(relative));
        let lookups = Lookups {
            list: &list,
            live_folder: &live_folder,
            live: &live,
            same_place: &same_place,
            written_over: run.written_over,
        };
        let keep_going = run.keep_going;
        let keep_going = move || keep_going;
        then(&Pass {
            live_root: &live_root,
            lookups: &lookups,
            threads: 3,
            keep_going: &keep_going,
        })
    }

    /// `build` over the fixture, every snapshot "mounted" where it is, or not at all for
    /// `refused`.
    fn read(fixture: &Fixture, refused: Option<&str>, run: &Run<'_>) -> Option<FileTree> {
        let dir = fixture.dir.clone();
        let mount = |name: &OsStr| {
            if Some(name.to_str().expect("a name")) == refused {
                Err("refused".to_string())
            } else {
                Ok(Mounted::new(dir.join(name), || {}))
            }
        };
        let snapshots = [
            (OsString::from("first"), run.scope.clone()),
            (OsString::from("second"), run.scope.clone()),
        ];
        with_pass(fixture, run, |pass| {
            build(&fixture.dir.join("scan"), &snapshots, &mount, pass).map(|(tree, _)| tree)
        })
    }

    /// The first snapshot alone read with `run`, its findings and the folders reported.
    fn read_first(fixture: &Fixture, run: &Run<'_>) -> (Walked, Vec<PathBuf>) {
        let mut kept = Vec::new();
        let walked = with_pass(fixture, run, |pass| {
            gone_from_live(
                &fixture.dir.join("first"),
                Path::new("/into"),
                &run.scope,
                pass,
                |dir| kept.push(dir.path.to_path_buf()),
            )
        });
        kept.sort();
        (walked, kept)
    }

    fn apparent_at(tree: &mut FileTree, path: &[&str]) -> u128 {
        tree.current_folder_names.clear();
        for name in path {
            tree.enter_folder(OsStr::new(name));
        }
        tree.get_current_folder().sizes.apparent
    }

    fn changed(listed: &[&str], walked: &[&str]) -> Scope {
        Scope::Changed {
            since: 1,
            events: 0,
            listed: listed.iter().map(PathBuf::from).collect(),
            walked: walked.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn a_snapshot_keeps_what_is_gone_from_the_live_volume_and_no_more() {
        let fixture = Fixture::new("kept");
        let mut tree = read(&fixture, None, &Run::default()).expect("not stopped");
        assert_eq!(
            tree.path_in_filesystem,
            fixture.dir.join("scan").join(FOLDER)
        );
        // Deleted, deleted with its folder kept, and deleted from a folder renamed since; not what
        // is live, moved out of a folder deleted since, or renamed with its folder.
        assert_eq!(apparent_at(&mut tree, &["first"]), 1000 + 2000 + 3000);
        assert_eq!(apparent_at(&mut tree, &["first", "docs"]), 2000);
        assert_eq!(apparent_at(&mut tree, &["first", "renamed from"]), 3000);
        assert_eq!(apparent_at(&mut tree, &["second"]), 1000 + 4000);
        // `gone.txt` is in both, and counted once above them.
        assert_eq!(apparent_at(&mut tree, &[]), 1000 + 2000 + 3000 + 4000);
        tree.current_folder_names = vec!["first".into()];
        for shared in ["kept.txt", "old"] {
            assert!(
                tree.item_in_current_folder(OsStr::new(shared)).is_none(),
                "{shared} is live, or holds only what is"
            );
        }
        assert_eq!(tree.failed_to_read, 0);
    }

    #[test]
    fn a_snapshot_that_cannot_be_mounted_is_an_empty_folder_and_counted() {
        let fixture = Fixture::new("refused");
        let mut tree = read(&fixture, Some("first"), &Run::default()).expect("not stopped");
        assert_eq!(apparent_at(&mut tree, &["first"]), 0);
        tree.current_folder_names.clear();
        assert!(tree.item_in_current_folder(OsStr::new("first")).is_some());
        assert_eq!(apparent_at(&mut tree, &[]), 1000 + 4000);
        assert_eq!(tree.failed_to_read, 1);
    }

    /// Read whole, a folder whose time is its live counterpart's has the same entries, so it is
    /// not set against it: shown here by a file only the snapshot has in such a folder — which
    /// a real snapshot cannot have, its folder's time having moved when the file went.
    #[test]
    fn a_folder_the_same_by_time_is_not_compared() {
        let fixture = Fixture::new("same");
        let (live, first) = (fixture.live(), fixture.dir.join("first"));
        write(&first.join("docs/unseen.bin"), 5000);
        let time = fs::metadata(live.join("docs"))
            .and_then(|meta| meta.modified())
            .expect("a time");
        fs::File::open(first.join("docs"))
            .and_then(|folder| folder.set_modified(time))
            .expect("set the time");
        let (walked, kept) = read_first(&fixture, &Run::default());
        assert_eq!(walked.folders, 4, "the root, docs, old, renamed from");
        assert_eq!(walked.compared, 3, "not docs");
        assert_eq!(walked.kept, 2, "gone.txt and x.txt");
        assert_eq!(
            kept,
            [PathBuf::from("/into"), PathBuf::from("/into/renamed from")]
        );
    }

    /// Narrowed by the log: only the folders it named are read, and of their subfolders only
    /// those moved or gone since — whose subtrees the log named elsewhere or not at all.
    #[test]
    fn only_what_the_log_named_is_read() {
        let fixture = Fixture::new("narrowed");
        let docs = Run {
            scope: changed(&["docs"], &[]),
            ..Run::default()
        };
        let (walked, kept) = read_first(&fixture, &docs);
        assert_eq!((walked.folders, walked.kept), (1, 1), "deleted.bin alone");
        assert_eq!(kept, [PathBuf::from("/into/docs")]);

        // The root named: `old` is gone and `renamed from` moved, so both are walked; `docs` is
        // where it was and the log did not name it, so it is not.
        let root = Run {
            scope: changed(&[""], &[]),
            ..Run::default()
        };
        let (walked, kept) = read_first(&fixture, &root);
        assert_eq!(walked.folders, 3, "the root, old, renamed from");
        assert_eq!(walked.kept, 2, "gone.txt and x.txt");
        assert_eq!(
            kept,
            [PathBuf::from("/into"), PathBuf::from("/into/renamed from")]
        );
    }

    /// A folder reached from two start points is read once, and one named that is not in the
    /// snapshot (made since) is no failure.
    #[test]
    fn a_folder_is_read_once_and_one_made_since_is_nothing() {
        let fixture = Fixture::new("once");
        let run = Run {
            scope: changed(&["", "renamed from", "made since"], &["renamed from"]),
            ..Run::default()
        };
        let (walked, _) = read_first(&fixture, &run);
        assert_eq!(walked.folders, 3, "the root, old, renamed from — once");
        assert_eq!(walked.kept, 2);
        assert_eq!(walked.failed, 0);
        let mut tree = read(&fixture, None, &run).expect("not stopped");
        assert_eq!(apparent_at(&mut tree, &["first"]), 1000 + 3000);
    }

    /// A file still live but written over since, in a folder the log named, is sized to what
    /// the snapshot alone holds of it; read whole, it cannot be told, and is not asked.
    #[test]
    fn a_file_written_over_counts_what_the_snapshot_alone_holds() {
        let fixture = Fixture::new("written");
        let a = fixture.dir.join("first/docs/a.txt");
        let held = |path: &Path, _: u64| if path == a { 700 } else { 0 };
        let run = Run {
            scope: changed(&["docs"], &[]),
            written_over: &held,
            ..Run::default()
        };
        let (walked, _) = read_first(&fixture, &run);
        assert_eq!((walked.written, walked.written_bytes), (1, 700));
        let mut tree = read(&fixture, None, &run).expect("not stopped");
        assert_eq!(apparent_at(&mut tree, &["first", "docs"]), 2000 + 700);

        let whole = Run {
            written_over: &held,
            ..Run::default()
        };
        assert_eq!(read_first(&fixture, &whole).0.written, 0);
    }

    /// A dataless snapshot is not mounted nor read: it holds no blocks of its own, though it
    /// lists every file it had. Its folder is there, empty, and nothing failed.
    #[test]
    fn a_dataless_snapshot_is_named_and_not_read() {
        let fixture = Fixture::new("dataless");
        let dir = fixture.dir.clone();
        let mounted = ::std::sync::atomic::AtomicUsize::new(0);
        let mount = |name: &OsStr| {
            mounted.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
            Ok(Mounted::new(dir.join(name), || {}))
        };
        let snapshots = [
            (OsString::from("first"), Scope::Dataless),
            (
                OsString::from("second"),
                Scope::Whole("the test".to_string()),
            ),
        ];
        let (mut tree, readings) = with_pass(&fixture, &Run::default(), |pass| {
            build(&fixture.dir.join("scan"), &snapshots, &mount, pass)
        })
        .expect("not stopped");
        assert_eq!(mounted.load(::std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(apparent_at(&mut tree, &["first"]), 0);
        assert_eq!(apparent_at(&mut tree, &[]), 1000 + 4000);
        assert_eq!(tree.failed_to_read, 0);
        assert!(
            readings[0].describe().contains("dataless"),
            "{}",
            readings[0].describe()
        );
    }

    /// A folder the log named, reached again by a walk whole of a folder above it (where the
    /// log lost events), has its every subfolder read, though as named it took only those
    /// moved or gone — whichever of the two came first.
    #[test]
    fn a_named_folder_inside_one_walked_whole_is_read_to_the_bottom() {
        let fixture = Fixture::new("deeper");
        let (live, first) = (fixture.live(), fixture.dir.join("first"));
        write(&live.join("docs/sub/here.txt"), 100);
        link(
            &live.join("docs/sub/here.txt"),
            &first.join("docs/sub/here.txt"),
        );
        write(&first.join("docs/sub/lost.bin"), 600);
        for threads in [1, 3] {
            let run = Run {
                scope: changed(&["docs"], &[""]),
                ..Run::default()
            };
            let (walked, kept) = with_pass(&fixture, &run, |pass| {
                let pass = Pass { threads, ..*pass };
                let mut kept = Vec::new();
                let walked = gone_from_live(&first, Path::new("/into"), &run.scope, &pass, |dir| {
                    kept.push(dir.path.to_path_buf());
                });
                (walked, kept)
            });
            assert!(
                kept.contains(&PathBuf::from("/into/docs/sub")),
                "{threads} threads: {kept:?}"
            );
            assert_eq!(
                walked.folders, 5,
                "the root, docs, sub, old, renamed from — once each"
            );
        }
    }

    #[test]
    fn a_stopped_pass_gives_nothing() {
        let fixture = Fixture::new("stopped");
        let run = Run {
            keep_going: false,
            ..Run::default()
        };
        assert!(read(&fixture, None, &run).is_none());
    }
}

/// This Mac's data volume: its snapshots listed, unprivileged. `--ignored`, as CI has none.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs a Mac with local snapshots"]
fn this_mac_s_snapshots_are_listed() {
    let volume = super::volume_for(::std::path::Path::new("/")).expect("the data volume");
    assert_eq!(volume, ::std::path::Path::new("/System/Volumes/Data"));
    let names = super::list(&volume).expect("listed");
    eprintln!("{} snapshots: {names:?}", names.len());
    assert!(
        super::volume_for(::std::path::Path::new("/Users")).is_none(),
        "a folder, no volume"
    );
}

/// The pass's floor, unprivileged: a folder of the live volume standing in for a snapshot of
/// itself, so every folder is listed and found the same by time, nothing compared.
/// `DUSCAPE_SNAPSHOT_FLOOR=<folder>` (default: the home folder).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "walks a real folder; --nocapture for the time"]
fn the_pass_over_an_unchanged_folder_costs_its_listing() {
    let folder = ::std::env::var_os("DUSCAPE_SNAPSHOT_FLOOR")
        .or_else(|| ::std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("a folder");
    let options = libduscape::scan::ScanOptions::default();
    let threads = crate::thread_count(options);
    let started = ::std::time::Instant::now();
    let walked = super::with_lookups(
        ::std::path::Path::new("/System/Volumes/Data"),
        options,
        |lookups| {
            let pass = super::Pass {
                live_root: &folder,
                lookups,
                threads,
                keep_going: &|| true,
            };
            let whole = super::Scope::Whole("the floor".to_string());
            super::gone_from_live(
                &folder,
                ::std::path::Path::new("/into"),
                &whole,
                &pass,
                |_| {},
            )
        },
    )
    .expect("the volume");
    eprintln!(
        "{}: {walked:?} in {:.2}s on {threads} threads",
        folder.display(),
        started.elapsed().as_secs_f64()
    );
    assert_eq!(
        walked.kept, 0,
        "the live folder keeps nothing the live folder lacks"
    );
}

/// The change log of the last million ids, streamed on `/` and taken onto the data volume:
/// how many events land on it, and none dropped. `--ignored --nocapture`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "replays this Mac's change log"]
fn the_log_on_root_is_taken_onto_the_data_volume() {
    use ::std::path::Path;
    let volume = Path::new("/System/Volumes/Data");
    let firmlinks = super::log::parse_firmlinks(
        &::std::fs::read_to_string("/usr/share/firmlinks").expect("the firmlinks"),
    );
    let now = crate::fsevents::current_event_id().expect("an id");
    let started = ::std::time::Instant::now();
    let crate::fsevents::Events::Events(events) = crate::fsevents::events(
        Path::new("/"),
        now.saturating_sub(1_000_000),
        ::std::time::Duration::from_secs(60),
    ) else {
        panic!("the log replays");
    };
    let on: Vec<_> = events
        .iter()
        .filter_map(|event| super::log::on_volume(&event.path, event.whole, volume, &firmlinks))
        .collect();
    let dropped = events.iter().filter(|event| event.whole).count();
    eprintln!(
        "{} events in {:.1}s, {} on the data volume, {} whole; e.g. {:?}",
        events.len(),
        started.elapsed().as_secs_f64(),
        on.len(),
        dropped,
        on.iter().take(3).collect::<Vec<_>>()
    );
    assert!(events.iter().all(|event| event.id <= now + 1_000_000));
    for relative in on.iter().take(200) {
        assert!(relative.is_relative(), "{relative:?}");
    }
    let times = super::list_with_times(volume).expect("listed");
    eprintln!("{times:?}");
}

/// A real file's physical extents: as many bytes as it has blocks, and a file set against
/// itself holds nothing alone. `DUSCAPE_EXTENTS_FILE=<file>` (default: the test binary).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "reads a real file's extents; --nocapture for the time"]
fn a_file_s_extents_are_its_blocks() {
    use ::std::os::unix::fs::MetadataExt;
    let file = ::std::env::var_os("DUSCAPE_EXTENTS_FILE").map_or_else(
        || ::std::env::current_exe().expect("this binary"),
        PathBuf::from,
    );
    let started = ::std::time::Instant::now();
    let extents = super::extents::physical(&file).expect("extents");
    let mapped: u64 = extents.iter().map(|(_, length)| length).sum();
    let blocks = ::std::fs::metadata(&file).expect("there").blocks() * 512;
    eprintln!(
        "{}: {} extents, {mapped} bytes mapped, {blocks} in blocks, in {:.3}s",
        file.display(),
        extents.len(),
        started.elapsed().as_secs_f64()
    );
    assert!(
        mapped <= blocks && blocks - mapped < 64 * 1024,
        "{mapped} against {blocks}"
    );
    let live = super::extents::Ranges::of(&extents);
    let mut counted = super::extents::Ranges::default();
    assert_eq!(super::extents::held_alone(&extents, &live, &mut counted), 0);
}
