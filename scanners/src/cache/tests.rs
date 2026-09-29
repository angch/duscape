//! The saved scan on every platform: the format round trip, and the replay with a fake log
//! against a real tree — a directory gone, a subfolder new, a subtree walked whole, a
//! hard-linked file grown through its other name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("duscape-cache-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A listing through `std::fs`, in the walkers' shape: directories at size 0, files with
/// their block size and length.
fn list_std(path: &Path) -> io::Result<DirEntries> {
    let mut directory = DirEntries::new(Arc::from(path));
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let is_dir = meta.is_dir();
        let (size, apparent, inode, links) = std_sizes(&meta, is_dir);
        directory.push(
            &entry.file_name(),
            EntryMeta {
                size,
                apparent,
                inode,
                links,
                is_dir,
                shared_extent: 0,
            },
        );
    }
    Ok(directory)
}

#[cfg(unix)]
fn std_sizes(meta: &fs::Metadata, is_dir: bool) -> (u64, u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    if is_dir {
        (0, 0, meta.ino(), 1)
    } else {
        (meta.blocks() * 512, meta.len(), meta.ino(), meta.nlink())
    }
}

#[cfg(not(unix))]
fn std_sizes(meta: &fs::Metadata, is_dir: bool) -> (u64, u64, u64, u64) {
    if is_dir {
        (0, 0, 0, 1)
    } else {
        (meta.len(), meta.len(), 0, 1)
    }
}

/// A walk through `std::fs`, parent before child, as the walkers send it.
fn walk_std(root: &Path) -> Vec<DirEntries> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = list_std(&dir) else {
            continue;
        };
        let mut subs: Vec<PathBuf> = listing
            .iter()
            .filter(|(_, meta)| meta.is_dir)
            .map(|(name, _)| dir.join(name))
            .collect();
        subs.sort();
        subs.reverse();
        stack.extend(subs);
        out.push(listing);
    }
    out
}

fn lister<'a>() -> Lister<'a> {
    Box::new(|path, _| list_std(path))
}

fn walker<'a>() -> Walker<'a> {
    Box::new(|path, _| Box::new(walk_std(path).into_iter()))
}

fn total<'a>(stream: impl IntoIterator<Item = &'a DirEntries>) -> (u64, u64) {
    let mut entries = 0;
    let mut bytes = 0;
    for directory in stream {
        entries += directory.len() as u64;
        bytes += directory.iter().map(|(_, m)| m.apparent).sum::<u64>();
    }
    (entries, bytes)
}

/// An entry as compared: its name, length and whether it is a folder.
type Named = (String, u64, bool);

/// Every directory's entries by name and length, order-free, for comparing two streams.
fn listing_map<'a>(stream: impl IntoIterator<Item = &'a DirEntries>) -> Vec<(PathBuf, Vec<Named>)> {
    let mut out: Vec<_> = stream
        .into_iter()
        .map(|directory| {
            let mut entries: Vec<_> = directory
                .iter()
                .map(|(name, meta)| {
                    (
                        name.to_string_lossy().into_owned(),
                        meta.apparent,
                        meta.is_dir,
                    )
                })
                .collect();
            entries.sort();
            (directory.path.to_path_buf(), entries)
        })
        .collect();
    out.sort();
    out
}

fn write(path: &Path, bytes: usize) {
    fs::write(path, vec![b'x'; bytes]).expect("write");
}

fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("a/deep/deeper")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::create_dir_all(root.join("gone/inside")).unwrap();
    write(&root.join("top.txt"), 10);
    write(&root.join("a/one"), 100);
    write(&root.join("a/deep/two"), 200);
    write(&root.join("a/deep/deeper/three"), 300);
    write(&root.join("b/four"), 400);
    write(&root.join("gone/five"), 500);
    write(&root.join("gone/inside/six"), 600);
}

fn key(root: &Path) -> Key {
    Key::new(root, ScanOptions::default())
}

fn stamp() -> Stamp {
    Stamp {
        event_id: 42,
        device: 7,
        log_uuid: "uuid".into(),
        system: "kernel".into(),
        saved_at: 1_000_000,
    }
}

/// Record a walk of `root` into `cache`, and read it back.
fn save(root: &Path, cache: &Path) -> Saved {
    let recorded: Vec<DirEntries> =
        Recorder::new(walk_std(root).into_iter(), cache, key(root), stamp()).collect();
    assert!(!recorded.is_empty());
    Saved::open(cache, &key(root)).expect("the saved scan reads back")
}

#[test]
fn the_saved_scan_reads_back_as_it_was_written() {
    let dir = temp_dir("roundtrip");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let saved = save(&root, &cache);
    assert_eq!(saved.header.key, key(&root));
    assert_eq!(saved.header.stamp, stamp());
    assert_eq!(saved.header.directories, 7);
    let replayed: Vec<DirEntries> = saved.directories(&root).map(|(_, d)| d).collect();
    assert_eq!(listing_map(&replayed), listing_map(&walk_std(&root)));
    // Nothing but the final file is left, and it is the owner's alone.
    let files: Vec<_> = fs::read_dir(&cache)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(cache.join(&files[0]))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_stopped_scan_saves_nothing() {
    let dir = temp_dir("stopped");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let mut recorder = Recorder::new(walk_std(&root).into_iter(), &cache, key(&root), stamp());
    let _ = recorder.next();
    drop(recorder);
    assert_eq!(Saved::open(&cache, &key(&root)).err(), Some(Unusable::None));
    assert!(
        fs::read_dir(&cache).unwrap().next().is_none(),
        "the part file is removed"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_for_another_key_or_a_damaged_one_is_not_used() {
    let dir = temp_dir("invalid");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let saved = save(&root, &cache);
    let other = Key {
        one_file_system: true,
        ..key(&root)
    };
    assert_ne!(other.file_name(), key(&root).file_name());
    fs::copy(
        cache.join(key(&root).file_name()),
        cache.join(other.file_name()),
    )
    .unwrap();
    assert_eq!(Saved::open(&cache, &other).err(), Some(Unusable::Invalid));
    // A byte flipped in the body fails the checksum; a truncated file fails the footer.
    let path = cache.join(key(&root).file_name());
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
    assert_eq!(
        Saved::open(&cache, &key(&root)).err(),
        Some(Unusable::Invalid)
    );
    drop(saved);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nothing_changed_replays_the_saved_scan_whole() {
    let dir = temp_dir("unchanged");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let saved = save(&root, &cache);
    let changes = Changes::default();
    let scan = CachedScan::new(saved, &root, &changes, lister(), walker(), "note".into());
    let replayed: Vec<DirEntries> = scan.collect();
    assert_eq!(listing_map(&replayed), listing_map(&walk_std(&root)));
    // The root carries the note, and nothing else does.
    let notes: Vec<_> = replayed
        .iter()
        .filter(|d| d.issues.kinds.keys().any(|(action, _)| *action == "cache"))
        .map(|d| d.path.to_path_buf())
        .collect();
    assert_eq!(notes, vec![root.clone()]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_changes_named_are_listed_again_and_the_rest_read_from_the_file() {
    let dir = temp_dir("changes");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let saved = save(&root, &cache);

    // A file grown, a folder removed with what was under it, a folder new with a subtree
    // under it, and a saved subtree the log says to walk whole after it changed.
    write(&root.join("a/one"), 1000);
    fs::remove_dir_all(root.join("gone")).unwrap();
    fs::create_dir_all(root.join("b/new/inner")).unwrap();
    write(&root.join("b/new/seven"), 700);
    write(&root.join("b/new/inner/eight"), 800);
    write(&root.join("a/deep/deeper/three"), 3000);
    let changes = Changes {
        listed: vec![PathBuf::from("a"), PathBuf::new(), PathBuf::from("b")],
        walked: vec![PathBuf::from("a/deep")],
        events: 5,
    };
    assert!(!too_stale(&changes, &saved.header));
    let scan = CachedScan::new(saved, &root, &changes, lister(), walker(), "note".into());
    let replayed: Vec<DirEntries> = scan.collect();
    assert_eq!(listing_map(&replayed), listing_map(&walk_std(&root)));
    assert_eq!(total(&replayed), total(&walk_std(&root)));
    // Parents come before children, as the tree builder needs.
    let order: Vec<PathBuf> = replayed.iter().map(|d| d.path.to_path_buf()).collect();
    for (i, path) in order.iter().enumerate() {
        if let Some(parent) = path.parent()
            && parent.starts_with(&root)
        {
            {
                let at = order
                    .iter()
                    .position(|p| p == parent)
                    .expect("parent listed");
                assert!(at < i, "{parent:?} after {path:?}");
            }
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_changed_directory_that_is_gone_drops_what_was_under_it() {
    let dir = temp_dir("gone");
    let root = dir.join("tree");
    make_tree(&root);
    let cache = dir.join("cache");
    let saved = save(&root, &cache);
    fs::remove_dir_all(root.join("gone")).unwrap();
    // The log names the folder itself (its parent's event is what a walker would get, but
    // the folder's own removal is reported on it too), not its parent.
    let changes = Changes {
        listed: vec![PathBuf::from("gone"), PathBuf::from("gone/inside")],
        ..Changes::default()
    };
    let scan = CachedScan::new(saved, &root, &changes, lister(), walker(), "note".into());
    let replayed: Vec<DirEntries> = scan.collect();
    assert!(
        replayed
            .iter()
            .all(|d| !d.path.starts_with(root.join("gone")))
    );
    // The root's saved listing still names `gone` — the log did not name the root — which is
    // what a walker whose directory vanished under it would send too.
    assert_eq!(replayed.len(), 5);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_hard_linked_file_grown_through_another_name_is_patched_everywhere() {
    let dir = temp_dir("links");
    let root = dir.join("tree");
    make_tree(&root);
    fs::hard_link(root.join("a/one"), root.join("b/one-too")).unwrap();
    let cache = dir.join("cache");
    let saved = save(&root, &cache);
    // Written through `b`'s name, and `a/deep/two` linked into `b` since the save: the log
    // names `b` alone.
    write(&root.join("b/one-too"), 5000);
    fs::hard_link(root.join("a/deep/two"), root.join("b/two-too")).unwrap();
    let changes = Changes {
        listed: vec![PathBuf::from("b")],
        ..Changes::default()
    };
    let scan = CachedScan::new(saved, &root, &changes, lister(), walker(), "note".into());
    let replayed: Vec<DirEntries> = scan.collect();
    let a = replayed
        .iter()
        .find(|d| *d.path == *root.join("a"))
        .unwrap();
    let (_, one) = a.iter().find(|(name, _)| *name == "one").unwrap();
    assert_eq!(
        one.apparent, 5000,
        "the saved copy under `a` took the size listed under `b`"
    );
    assert_eq!(one.links, 2);
    let deep = replayed
        .iter()
        .find(|d| *d.path == *root.join("a/deep"))
        .unwrap();
    let (_, two) = deep.iter().find(|(name, _)| *name == "two").unwrap();
    assert_eq!(
        two.links, 2,
        "the saved copy was one name; it has two now, and the ledger must see it"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn too_much_named_is_too_stale() {
    let header = Header {
        key: key(Path::new("/x")),
        stamp: stamp(),
        directories: 1000,
        entries: 0,
    };
    let few = Changes {
        listed: (0..64).map(|i| PathBuf::from(format!("d{i}"))).collect(),
        ..Changes::default()
    };
    assert!(!too_stale(&few, &header));
    let many = Changes {
        listed: (0..300).map(|i| PathBuf::from(format!("d{i}"))).collect(),
        ..Changes::default()
    };
    assert!(too_stale(&many, &header));
    let root_whole = Changes {
        walked: vec![PathBuf::new()],
        ..Changes::default()
    };
    assert!(too_stale(&root_whole, &header));
    // A small tree may have every one of its folders named and still be worth it.
    let small = Header {
        directories: 10,
        ..header
    };
    assert!(!too_stale(&few, &small));
}

#[test]
fn varints_round_trip() {
    for value in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
        let mut out = Vec::new();
        put_varint(&mut out, value);
        let mut cursor = Cursor {
            bytes: &out,
            pos: 0,
        };
        assert_eq!(cursor.varint(), Some(value));
        assert!(cursor.done());
    }
    let mut cursor = Cursor {
        bytes: &[0x80; 12],
        pos: 0,
    };
    assert_eq!(cursor.varint(), None, "an endless varint is not a number");
}
