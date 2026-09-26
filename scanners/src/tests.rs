use ::std::fs::File;
use ::std::io::Write;
use ::std::path::PathBuf;

use super::{EntryMeta, ScanItem, ScanOptions, scan_directories, scan_folder, scan_into_tree};

fn temp_scan_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("diskonaut_scan_test_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    // Canonicalized because the app always scans a canonical path: `Opts::resolve_folder`
    // resolves the folder before it reaches `FileTree`, which canonicalizes its own root too. On
    // macOS `temp_dir()` is `/var/...`, a symlink to `/private/var/...`, so an uncanonicalized
    // path here fails `strip_prefix` against that root and every entry is silently dropped.
    dir.canonicalize().expect("canonicalize temp dir")
}

#[test]
fn scan_folder_finds_files() {
    let dir = temp_scan_dir("finds_files");
    let file_path = dir.join("a.txt");
    let mut file = File::create(&file_path).expect("create file");
    file.write_all(b"hello").expect("write file");
    drop(file);

    let options = ScanOptions {
        parallel: false,
        show_apparent_size: false,
        ..ScanOptions::default()
    };
    let entries: Vec<_> = scan_folder(&dir, options)
        .filter_map(|item| match item {
            ScanItem::Entry { path, .. } => Some(path),
            ScanItem::ReadError => None,
        })
        .collect();

    assert!(
        entries.iter().any(|p| p == &file_path),
        "expected walk to include {file_path:?}, got {entries:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_into_tree_aggregates_sizes() {
    let dir = temp_scan_dir("aggregates");
    let file_path = dir.join("data.bin");
    let mut file = File::create(&file_path).expect("create file");
    file.write_all(&[0u8; 1024]).expect("write file");
    drop(file);

    let options = ScanOptions {
        parallel: false,
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(&dir, options);

    assert_eq!(failed, 0);
    assert!(tree.get_total_size() >= 1024);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_directories_groups_entries_by_directory() {
    let dir = temp_scan_dir("grouped");
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).expect("create subdir");
    File::create(dir.join("top.txt"))
        .expect("create file")
        .write_all(b"top")
        .expect("write file");
    File::create(sub.join("inner.txt"))
        .expect("create file")
        .write_all(b"inner")
        .expect("write file");

    let options = ScanOptions {
        parallel: false,
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let mut named: Vec<_> = scan_directories(&dir, options)
        .flat_map(|directory| {
            let parent = directory.path.to_path_buf();
            let names: Vec<_> = directory
                .iter()
                .map(|(name, _)| parent.join(name))
                .collect();
            names.into_iter()
        })
        .collect();
    named.sort();

    assert_eq!(
        named,
        vec![sub.clone(), dir.join("top.txt"), sub.join("inner.txt")]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_into_tree_counts_nested_entries() {
    let dir = temp_scan_dir("nested");
    let sub = dir.join("a").join("b");
    std::fs::create_dir_all(&sub).expect("create nested dirs");
    File::create(sub.join("deep.bin"))
        .expect("create file")
        .write_all(&[7u8; 2048])
        .expect("write file");

    let options = ScanOptions {
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(&dir, options);

    assert_eq!(failed, 0);
    assert_eq!(tree.get_total_size(), 2048);
    // "a", "a/b" and "a/b/deep.bin"
    assert_eq!(tree.get_total_descendants(), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Build a small fixture tree and return the full set of paths it should produce.
fn fixture_tree(name: &str) -> (PathBuf, std::collections::BTreeSet<PathBuf>) {
    let dir = temp_scan_dir(name);
    let a = dir.join("a");
    let b = dir.join("b");
    let nested = a.join("nested");
    std::fs::create_dir_all(&nested).expect("mkdir a/nested");
    std::fs::create_dir_all(&b).expect("mkdir b");
    let mut expected = std::collections::BTreeSet::new();
    expected.insert(a.clone());
    expected.insert(b.clone());
    expected.insert(nested.clone());
    for (parent, file) in [
        (&dir, "top.txt"),
        (&a, "one.txt"),
        (&a, "two.txt"),
        (&nested, "deep.txt"),
        (&b, "three.txt"),
    ] {
        let path = parent.join(file);
        File::create(&path)
            .expect("create file")
            .write_all(b"0123456789")
            .expect("write file");
        expected.insert(path);
    }
    (dir, expected)
}

fn collect_paths(
    dir: &std::path::Path,
    options: ScanOptions,
) -> std::collections::BTreeSet<PathBuf> {
    scan_directories(dir, options)
        .flat_map(|directory| {
            let parent = directory.path.to_path_buf();
            let names: Vec<_> = directory
                .iter()
                .map(|(name, _)| parent.join(name))
                .collect();
            names.into_iter()
        })
        .collect()
}

#[test]
fn scan_directories_reports_every_entry_exactly_once() {
    let (dir, expected) = fixture_tree("every_entry");
    let mut seen = Vec::new();
    for directory in scan_directories(&dir, ScanOptions::default()) {
        let parent = directory.path.to_path_buf();
        seen.extend(directory.iter().map(|(name, _)| parent.join(name)));
    }
    let unique: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
    assert_eq!(unique, expected, "wrong set of entries");
    assert_eq!(
        seen.len(),
        unique.len(),
        "an entry was reported twice: {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `dua-core` grouping is only reached off macOS, so exercise it directly everywhere.
#[test]
fn fallback_grouping_reports_every_entry_exactly_once() {
    let (dir, expected) = fixture_tree("fallback_every_entry");
    let mut seen = Vec::new();
    for directory in super::fallback::group_by_directory(&dir, ScanOptions::default()) {
        let parent = directory.path.to_path_buf();
        seen.extend(directory.iter().map(|(name, _)| parent.join(name)));
    }
    let unique: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
    assert_eq!(unique, expected, "wrong set of entries");
    assert_eq!(
        seen.len(),
        unique.len(),
        "an entry was reported twice: {seen:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Both walkers must agree, so that the benchmark compares like with like.
#[test]
fn both_walkers_agree_on_the_same_tree() {
    let (dir, expected) = fixture_tree("walkers_agree");
    let options = ScanOptions {
        show_apparent_size: true,
        ..ScanOptions::default()
    };

    let native = collect_paths(&dir, options);
    let fallback: std::collections::BTreeSet<PathBuf> =
        super::fallback::group_by_directory(&dir, options)
            .flat_map(|directory| {
                let parent = directory.path.to_path_buf();
                let names: Vec<_> = directory
                    .iter()
                    .map(|(name, _)| parent.join(name))
                    .collect();
                names.into_iter()
            })
            .collect();

    assert_eq!(native, expected);
    assert_eq!(fallback, expected);

    let (tree, failed) = scan_into_tree(&dir, options);
    assert_eq!(failed, 0);
    assert_eq!(tree.get_total_size(), 50, "five ten-byte files");
    assert_eq!(tree.get_total_descendants(), expected.len() as u64);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A scan must never attribute anything to a directory outside the tree it was asked about.
#[test]
fn entries_outside_the_scan_root_are_ignored() {
    let dir = temp_scan_dir("outside_root");
    let mut tree = libdiskonaut::FileTree::new(libdiskonaut::Folder::new(&dir), dir.clone());
    let mut outside = crate::DirEntries::new(std::sync::Arc::from(std::path::Path::new(
        "/somewhere/else",
    )));
    outside.push(
        std::ffi::OsStr::new("intruder"),
        EntryMeta {
            size: 4096,
            links: 1,
            is_dir: false,
            ..EntryMeta::default()
        },
    );
    tree.add_dir_entries(outside);
    assert_eq!(tree.get_total_size(), 0);
    assert_eq!(tree.get_total_descendants(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Windows tracks hard links only in the places they are normally made unless told otherwise,
/// and a temporary directory is not one of them. Ignored on Unix, which always knows.
const TRACK_EVERY_LINK: Option<u64> = Some(1);

/// The case that motivates charging a hard-linked file per folder rather than per entry:
/// `a/a`, `a/b` and `b/a` are all the same 1 KiB file, so `a` holds 1 KiB, `b` holds 1 KiB, and
/// the root that contains both still holds only 1 KiB.
#[test]
fn hard_links_count_once_per_folder() {
    let dir = temp_scan_dir("hard_links");
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");

    let original = a.join("a");
    File::create(&original)
        .expect("create file")
        .write_all(&[7u8; 1024])
        .expect("write file");
    std::fs::hard_link(&original, a.join("b")).expect("hard link a/b");
    std::fs::hard_link(&original, b.join("a")).expect("hard link b/a");

    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: TRACK_EVERY_LINK,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(&dir, options);
    assert_eq!(failed, 0);
    assert_eq!(
        tree.hard_linked_files(),
        1,
        "one distinct file, three links"
    );

    let folder_size = |name: &str| match tree
        .get_current_folder()
        .path(vec![std::ffi::OsString::from(name)])
        .unwrap_or_else(|| panic!("{name} should exist"))
    {
        libdiskonaut::FileOrFolder::Folder(folder) => folder.sizes.get(tree.shown),
        libdiskonaut::FileOrFolder::File(_) => panic!("{name} should be a folder"),
    };

    assert_eq!(
        folder_size("a"),
        1024,
        "a/a and a/b are the same 1 KiB file"
    );
    assert_eq!(folder_size("b"), 1024, "b/a is that same file again");
    assert_eq!(
        tree.get_total_size(),
        1024,
        "the root holds one 1 KiB file however many names point at it"
    );
    // Every link is still a directory entry in its own right.
    assert_eq!(tree.get_total_descendants(), 5, "a, b, a/a, a/b, b/a");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Nested folders must each count the file once, and a folder holding two links to it still
/// reports one copy.
#[test]
fn hard_links_count_once_per_folder_when_nested() {
    let dir = temp_scan_dir("hard_links_nested");
    let deep = dir.join("one").join("two");
    std::fs::create_dir_all(&deep).expect("mkdir one/two");
    let other = dir.join("other");
    std::fs::create_dir_all(&other).expect("mkdir other");

    let original = deep.join("file");
    File::create(&original)
        .expect("create file")
        .write_all(&[1u8; 2048])
        .expect("write file");
    std::fs::hard_link(&original, deep.join("file-again")).expect("link in the same folder");
    std::fs::hard_link(&original, other.join("file")).expect("link in a sibling");

    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: TRACK_EVERY_LINK,
        ..ScanOptions::default()
    };
    let (tree, _) = scan_into_tree(&dir, options);

    let size_of = |path: &[&str]| {
        let names: Vec<std::ffi::OsString> = path
            .iter()
            .map(|part| std::ffi::OsString::from(*part))
            .collect();
        match tree
            .get_current_folder()
            .path(names)
            .unwrap_or_else(|| panic!("{path:?} should exist"))
        {
            libdiskonaut::FileOrFolder::Folder(folder) => folder.sizes.get(tree.shown),
            libdiskonaut::FileOrFolder::File(file) => file.sizes().get(tree.shown),
        }
    };

    assert_eq!(size_of(&["one", "two"]), 2048, "two links, one file");
    assert_eq!(size_of(&["one"]), 2048);
    assert_eq!(size_of(&["other"]), 2048, "reachable here too, on its own");
    assert_eq!(tree.get_total_size(), 2048, "still one file overall");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file with one link must not go through the hard-link ledger at all.
#[test]
fn ordinary_files_are_not_tracked_as_hard_links() {
    let (dir, _) = fixture_tree("no_hard_links");
    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: TRACK_EVERY_LINK,
        ..ScanOptions::default()
    };
    let (tree, _) = scan_into_tree(&dir, options);
    assert_eq!(tree.hard_linked_files(), 0);
    assert_eq!(tree.get_total_size(), 50);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Deleting links to a hard-linked file must not drive a folder's size below zero, since that
/// folder was only ever charged once for all of them.
#[test]
fn deleting_every_link_in_a_folder_does_not_underflow() {
    let dir = temp_scan_dir("hard_links_delete");
    let original = dir.join("one");
    File::create(&original)
        .expect("create file")
        .write_all(&[3u8; 512])
        .expect("write file");
    std::fs::hard_link(&original, dir.join("two")).expect("hard link");

    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: TRACK_EVERY_LINK,
        ..ScanOptions::default()
    };
    let (mut tree, _) = scan_into_tree(&dir, options);
    assert_eq!(tree.get_total_size(), 512, "two names, one file");

    for name in ["one", "two"] {
        tree.delete_file(&libdiskonaut::FileToDelete {
            path_in_filesystem: dir.clone(),
            path_to_file: vec![std::ffi::OsString::from(name)],
            file_type: libdiskonaut::tiles::FileType::File,
            num_descendants: None,
            size: 512,
            sizes: libdiskonaut::model::Sizes::ZERO,
        });
    }
    assert_eq!(tree.get_total_size(), 0);
    assert_eq!(tree.get_total_descendants(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_into_tree_follows_symlinked_root_directory() {
    let dir = temp_scan_dir("symlink_root_target");
    let file = dir.join("file.txt");
    File::create(&file)
        .expect("create file")
        .write_all(b"symlink root test")
        .expect("write file");
    let link = std::env::temp_dir().join("diskonaut_scan_test_symlink_root_link");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir(&link);
    let res = {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, &link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&dir, &link)
        }
    };
    if let Err(e) = res {
        // Windows grants symlink creation only in Developer Mode or elevated
        // (ERROR_PRIVILEGE_NOT_HELD, 1314): not this machine's to test, then.
        if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1314) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        panic!("create symlink: {e}");
    }

    let options = ScanOptions {
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(&link, options);
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(failed, 0);
    assert_eq!(tree.get_total_descendants(), 1);
    assert_eq!(tree.get_total_size(), 17);
}

#[test]
fn one_file_system_scans_same_device() {
    let (dir, expected) = fixture_tree("one_fs_fixture");
    let options = ScanOptions {
        one_file_system: true,
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(&dir, options);
    assert_eq!(failed, 0);
    assert_eq!(tree.get_total_descendants(), expected.len() as u64);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The native Linux walker: the properties the rest of the pipeline relies on.
///
/// These exercise the walker actually selected on Linux. The `fallback` tests above cover the
/// `dua-core` grouping, which Linux no longer uses but other platforms still do.
#[cfg(target_os = "linux")]
mod linux_walker {
    use super::{ScanOptions, temp_scan_dir};
    use crate::linux::walk_linux;
    use ::std::collections::HashMap;
    use ::std::fs::{File, create_dir_all};
    use ::std::io::Write;
    use ::std::path::{Path, PathBuf};

    /// `root/a/deep/x.bin`, `root/a/y.bin`, `root/b/z.bin`, each 1024 bytes.
    fn tree(name: &str) -> PathBuf {
        let root = temp_scan_dir(name);
        create_dir_all(root.join("a/deep")).expect("mkdir");
        create_dir_all(root.join("b")).expect("mkdir");
        for path in ["a/deep/x.bin", "a/y.bin", "b/z.bin"] {
            let mut file = File::create(root.join(path)).expect("create");
            file.write_all(&[7u8; 1024]).expect("write");
        }
        root
    }

    fn walk(root: &Path, threads: usize, options: ScanOptions) -> Vec<crate::DirEntries> {
        walk_linux(root, threads, options).collect()
    }

    #[test]
    fn reports_every_entry_exactly_once() {
        let root = tree("linux_once");
        for threads in [1, 2, 8] {
            let groups = walk(&root, threads, ScanOptions::default());
            let mut seen: HashMap<PathBuf, usize> = HashMap::new();
            for group in &groups {
                for (name, _) in group.iter() {
                    *seen.entry(group.path.join(name)).or_default() += 1;
                }
            }
            assert_eq!(
                seen.len(),
                6,
                "3 files + 3 dirs (a, b, a/deep), at {threads} threads"
            );
            assert!(
                seen.values().all(|&count| count == 1),
                "every entry exactly once at {threads} threads, got {seen:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One group per directory is what lets the tree builder resolve each parent once. The
    /// `dua-core` grouping could not promise this — it emitted about two groups per directory.
    #[test]
    fn emits_one_group_per_directory() {
        let root = tree("linux_groups");
        let groups = walk(&root, 8, ScanOptions::default());
        let mut paths: Vec<_> = groups.iter().map(|group| group.path.clone()).collect();
        paths.sort();
        let unique = {
            let mut unique = paths.clone();
            unique.dedup();
            unique
        };
        assert_eq!(
            paths, unique,
            "a directory was reported in more than one group"
        );
        assert_eq!(paths.len(), 4, "root, a, a/deep, b");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Both sizes, whatever is shown: the length as the apparent size, blocks as the size.
    #[test]
    fn the_walk_reports_length_and_blocks_both() {
        let root = tree("linux_apparent");
        let files: Vec<_> = walk(&root, 4, ScanOptions::default())
            .iter()
            .flat_map(|group| group.entries().to_vec())
            .filter(|entry| !entry.meta.is_dir)
            .collect();
        let apparent: u64 = files.iter().map(|entry| entry.meta.apparent).sum();
        assert_eq!(apparent, 3 * 1024);
        assert!(
            files.iter().all(|entry| entry.meta.size % 512 == 0),
            "whole blocks"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn max_depth_stops_descending() {
        let root = tree("linux_depth");
        // Depth 1 is the root's own entries; nothing below them is read.
        let groups = walk(
            &root,
            4,
            ScanOptions {
                max_depth: Some(1),
                ..ScanOptions::default()
            },
        );
        assert_eq!(groups.len(), 1, "only the root directory is read");
        assert_eq!(groups[0].entries().len(), 2, "a and b");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinks_are_not_followed() {
        let root = tree("linux_symlink");
        std::os::unix::fs::symlink(root.join("a"), root.join("loop")).expect("symlink");
        let groups = walk(&root, 4, ScanOptions::default());
        assert_eq!(groups.len(), 4, "the symlink is not descended into");
        let linked = groups
            .iter()
            .flat_map(crate::DirEntries::iter)
            .find(|(name, _)| *name == "loop")
            .expect("the symlink is still reported as an entry");
        assert!(
            !linked.1.is_dir,
            "a symlink to a directory is not a directory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `/proc` and `/sys` are kernel interfaces, not disks. The walk crosses into them only if it
    /// cannot tell them apart from a filesystem holding data.
    #[test]
    fn pseudo_filesystems_are_recognised() {
        use crate::linux::filesystem::classify;
        assert!(
            classify(Path::new("/proc")).pseudo,
            "/proc holds no disk usage"
        );
        assert!(
            classify(Path::new("/sys")).pseudo,
            "/sys holds no disk usage"
        );
        assert!(!classify(Path::new("/")).pseudo, "the root filesystem does");
        assert!(
            !classify(Path::new("/tmp")).pseudo,
            "tmpfs holds real files and is counted, as du counts it"
        );
        // Reflink capability is a property of the filesystem, not of the scan root: this is what
        // lets a second XFS volume, or a btrfs subvolume, be probed at all.
        assert!(!classify(Path::new("/proc")).reflinks);
        // As in the `reflink` tests below: only a machine that names an XFS or btrfs directory can
        // check the positive case.
        if let Some(dir) = std::env::var_os("DISKONAUT_TEST_REFLINK_DIR") {
            assert!(
                classify(Path::new(&dir)).reflinks,
                "DISKONAUT_TEST_REFLINK_DIR is on a filesystem that shares extents"
            );
        }
    }

    /// FUSE serves local and remote filesystems alike; the mount table's subtype tells them apart,
    /// and its superblock options say whether btrfs compresses.
    #[test]
    fn the_mount_table_names_types_subtypes_and_options() {
        use crate::linux::mounts::parse;
        let table = parse(
            "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
97 22 0:52 / /home/u/remote rw,nosuid,nodev,relatime shared:50 - fuse.sshfs u@host:/srv rw,user_id=1000
98 22 0:53 / /media/u/usb rw,relatime - fuseblk /dev/sdb1 rw,user_id=0
99 22 0:54 / /mnt/cloud rw,relatime shared:51 master:3 - fuse.rclone remote: rw
40 22 0:45 /@home /home rw,relatime shared:3 - btrfs /dev/nvme0n1p3 rw,compress=zstd:3,ssd,subvol=/@home
",
        );
        let by_id = |id: u64| table.iter().find(|mount| mount.id == id).unwrap();
        assert_eq!(by_id(97).fuse_subtype(), Some("sshfs"));
        assert_eq!(
            by_id(99).fuse_subtype(),
            Some("rclone"),
            "optional fields before the -"
        );
        assert_eq!(
            by_id(98).fuse_subtype(),
            None,
            "ntfs-3g and friends name no subtype"
        );
        assert_eq!(by_id(22).fuse_subtype(), None, "not FUSE");
        assert_eq!(by_id(40).fstype, "btrfs");
        assert!(by_id(40).options.contains("compress=zstd:3"));
        assert_eq!(by_id(40).root, Path::new("/@home"));
    }

    /// Local filesystems are walked into; the machine's own root is not a network mount.
    #[test]
    fn local_filesystems_are_not_network_mounts() {
        use crate::linux::filesystem::classify;
        for path in ["/", "/proc", "/tmp"] {
            assert!(!classify(Path::new(path)).network, "{path}");
        }
        if let Some(dir) = std::env::var_os("DISKONAUT_TEST_NETWORK_DIR") {
            assert!(
                classify(Path::new(&dir)).network,
                "DISKONAUT_TEST_NETWORK_DIR is on a network filesystem"
            );
        }
    }

    /// A bind mount of a folder the scan reaches anyway is recognised from the mount table; one
    /// whose source is outside the scan, or hidden, is not.
    #[test]
    fn a_bind_mount_of_a_folder_inside_the_scan_is_reached_elsewhere() {
        use crate::linux::mounts::{parse, reached_elsewhere};
        let table = parse(
            "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
30 22 259:2 /home/u/data /srv/data rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
31 22 8:17 / /mnt/usb rw,relatime - vfat /dev/sdb1 rw
32 22 8:17 / /media/usb\\040again rw,relatime - vfat /dev/sdb1 rw
33 22 259:2 /opt/secret /srv/opt rw - ext4 /dev/nvme0n1p2 rw
",
        );
        assert_eq!(
            table[3].point,
            Path::new("/media/usb again"),
            "octal escapes"
        );
        let everything = |_: &Path| true;
        // The bind mount at /srv/data shows /home/u/data, which a scan of / reaches.
        assert!(reached_elsewhere(
            &table,
            30,
            Path::new("/srv/data"),
            Path::new("/"),
            everything
        ));
        // Scanning only /srv, the source is outside the scan: the bind mount is all there is.
        assert!(!reached_elsewhere(
            &table,
            30,
            Path::new("/srv/data"),
            Path::new("/srv"),
            everything
        ));
        // The second mount of one filesystem is the duplicate, never the first.
        assert!(reached_elsewhere(
            &table,
            32,
            Path::new("/media/usb again"),
            Path::new("/"),
            everything
        ));
        assert!(!reached_elsewhere(
            &table,
            31,
            Path::new("/mnt/usb"),
            Path::new("/"),
            everything
        ));
        // A source that is not the same directory any more (hidden under another mount) is not.
        assert!(!reached_elsewhere(
            &table,
            33,
            Path::new("/srv/opt"),
            Path::new("/"),
            |_| false
        ));
        // The root filesystem itself is nobody's duplicate.
        assert!(!reached_elsewhere(
            &table,
            22,
            Path::new("/"),
            Path::new("/"),
            everything
        ));
    }

    /// btrfs file-extent items, as `BTRFS_IOC_TREE_SEARCH_V2` returns them: what each occupies.
    #[test]
    fn btrfs_extent_items_are_summed_as_stored() {
        use crate::linux::btrfs_extents::parse;
        // header: transid, objectid, offset, type, len; then the item.
        fn item(offset: u64, extent: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            for value in [1u64, 257, offset] {
                out.extend_from_slice(&value.to_ne_bytes());
            }
            out.extend_from_slice(&108u32.to_ne_bytes());
            out.extend_from_slice(&(extent.len() as u32).to_ne_bytes());
            out.extend_from_slice(extent);
            out
        }
        // generation, ram_bytes, compression, encryption, other_encoding(2), type
        fn head(ram: u64, compression: u8, kind: u8) -> Vec<u8> {
            let mut out = 7u64.to_ne_bytes().to_vec();
            out.extend_from_slice(&ram.to_ne_bytes());
            out.extend_from_slice(&[compression, 0, 0, 0, kind]);
            out
        }
        fn regular(ram: u64, compression: u8, bytenr: u64, disk: u64, num: u64) -> Vec<u8> {
            let mut out = head(ram, compression, 1);
            for value in [bytenr, disk, 0, num] {
                out.extend_from_slice(&value.to_ne_bytes());
            }
            out
        }
        let mut buffer = Vec::new();
        // 128 KiB compressed into 4 KiB, wholly referred to.
        buffer.extend(item(0, &regular(131_072, 3, 1 << 20, 4096, 131_072)));
        // The same, of which this file refers to half.
        buffer.extend(item(131_072, &regular(131_072, 3, 2 << 20, 8192, 65_536)));
        // Uncompressed: what the file refers to.
        buffer.extend(item(
            196_608,
            &regular(524_288, 0, 3 << 20, 524_288, 100_000),
        ));
        // A hole.
        buffer.extend(item(296_608, &regular(4096, 0, 0, 0, 4096)));
        // Inline: the bytes stored in the item.
        let mut inline = head(1500, 3, 0);
        inline.extend_from_slice(&[9u8; 700]);
        buffer.extend(item(300_704, &inline));

        let parsed = parse(&buffer, 5).expect("well formed");
        assert_eq!(parsed.bytes, 4096 + 4096 + 100_000 + 700);
        assert_eq!(parsed.last, Some(300_704));
        assert_eq!(parsed.used, buffer.len());
        assert!(
            parse(&buffer[..40], 1).is_none(),
            "a truncated item is refused"
        );
    }

    /// A rescan of one folder asks the walk's rules of it first: `/proc` under `/` is not entered,
    /// so it is not rescanned either; an ordinary folder is.
    #[test]
    fn a_rescan_follows_the_walks_rules_at_its_own_root() {
        use crate::walk_would_enter;
        let options = ScanOptions::default();
        assert!(!walk_would_enter(
            Path::new("/"),
            Path::new("/proc"),
            options
        ));
        let root = tree("rescan_rules");
        assert!(walk_would_enter(&root, &root.join("a"), options));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Scanning a pseudo-filesystem asked for by name still works — the skip applies to crossing
    /// into one part-way through a scan, not to the root the user chose.
    #[test]
    fn a_pseudo_filesystem_can_still_be_scanned_directly() {
        let groups = walk(Path::new("/proc"), 4, ScanOptions::default());
        assert!(
            groups.len() > 1,
            "asking for /proc by name should still walk it, got {} groups",
            groups.len()
        );
    }

    /// Dropping the walk part-way must stop the workers, not let them finish the tree while the
    /// consumer waits to join them. Without a stop flag this took 32 seconds on a whole disk.
    #[test]
    fn dropping_the_walk_early_does_not_hang() {
        let root = tree("linux_early_drop");
        let mut walk = walk_linux(&root, 8, ScanOptions::default());
        let _first = walk.next().expect("at least one directory");
        drop(walk); // Joins the workers; hangs here if the stop flag is not honoured.
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// End-to-end reflink accounting, on a filesystem that can actually share extents.
///
/// `std::env::temp_dir()` is usually ext4, which cannot, so this skips itself there rather than
/// passing vacuously. Point `DISKONAUT_TEST_REFLINK_DIR` at a directory on XFS or btrfs to run it
/// for real — which is the only way this is tested at all.
#[cfg(target_os = "linux")]
mod reflink {
    use super::{ScanOptions, scan_into_tree};
    use ::std::fs::File;
    use ::std::io::Write;
    use ::std::os::fd::AsRawFd;
    use ::std::path::PathBuf;

    /// `_IOW(0x94, 9, int)`: make this file share the source file's extents.
    const FICLONE: libc::Ioctl = 0x4004_9409;

    fn clone_file(from: &File, to: &File) -> bool {
        // SAFETY: both descriptors are open for the duration of the call.
        unsafe { libc::ioctl(to.as_raw_fd(), FICLONE, from.as_raw_fd()) == 0 }
    }

    /// Sharing the *opening* extent is not sharing the file. A reflink copy whose middle has been
    /// overwritten is a different file that happens to start in the same place, and merging the
    /// two would report half the space actually held.
    /// A scratch tree with `a/` and `b/`, wherever the caller pointed us.
    fn fixture(name: &str) -> Option<PathBuf> {
        let base = ::std::env::var_os("DISKONAUT_TEST_REFLINK_DIR")
            .map_or_else(::std::env::temp_dir, PathBuf::from);
        let root = base.join(format!("diskonaut_scan_test_{name}"));
        let _ = ::std::fs::remove_dir_all(&root);
        ::std::fs::create_dir_all(root.join("a")).ok()?;
        ::std::fs::create_dir_all(root.join("b")).ok()?;
        Some(root)
    }

    #[test]
    fn a_partly_shared_copy_is_counted_in_full() {
        let Some(root) = fixture("reflink_partial") else {
            return;
        };
        let payload = vec![9u8; 256 * 1024];
        let source_path = root.join("a/payload.bin");
        ::std::fs::write(&source_path, &payload).expect("write source");

        let source = File::open(&source_path).expect("open source");
        let copy_path = root.join("b/payload.bin");
        let copy = File::create(&copy_path).expect("create copy");
        if !clone_file(&source, &copy) {
            let _ = ::std::fs::remove_dir_all(&root);
            eprintln!("skipped: cannot reflink");
            return;
        }
        drop(copy);
        drop(source);

        // Rewrite the middle, breaking the share for everything but the first extent.
        let mut copy = ::std::fs::OpenOptions::new()
            .write(true)
            .open(&copy_path)
            .expect("reopen copy");
        ::std::io::Seek::seek(&mut copy, ::std::io::SeekFrom::Start(100 * 1024)).expect("seek");
        copy.write_all(&vec![1u8; 56 * 1024]).expect("overwrite");
        copy.sync_all().expect("sync");
        drop(copy);

        let options = ScanOptions {
            show_apparent_size: true,
            ..ScanOptions::default()
        };
        let (tree, _) = scan_into_tree(&root, options);
        let total = tree.get_total_size();
        let reflinked = tree.reflinked_files();
        let _ = ::std::fs::remove_dir_all(&root);

        assert_eq!(
            total,
            2 * 256 * 1024,
            "both files are held in full, so both are counted"
        );
        // The copy is never merged with anything. XFS splits the source's extent where the copy
        // was rewritten, so neither file is wholly shared; btrfs keeps the source's extent whole
        // and still referenced by the copy's unchanged ends, so the source alone is, and is
        // counted as reflinked with nothing to merge with.
        assert!(
            reflinked <= 1,
            "a partly shared file is not a copy of anything, got {reflinked}"
        );
    }

    #[test]
    fn a_reflinked_copy_is_counted_once() {
        let base = ::std::env::var_os("DISKONAUT_TEST_REFLINK_DIR")
            .map_or_else(::std::env::temp_dir, PathBuf::from);
        let root = base.join("diskonaut_scan_test_reflink");
        let _ = ::std::fs::remove_dir_all(&root);
        ::std::fs::create_dir_all(root.join("a")).expect("mkdir a");
        ::std::fs::create_dir_all(root.join("b")).expect("mkdir b");

        // Large enough to clear the walker's probe threshold and to occupy whole blocks.
        let payload = vec![9u8; 256 * 1024];
        let source_path = root.join("a/payload.bin");
        let mut source = File::create(&source_path).expect("create source");
        source.write_all(&payload).expect("write");
        source.sync_all().expect("sync");
        drop(source);

        let source = File::open(&source_path).expect("open source");
        let copy = File::create(root.join("b/payload.bin")).expect("create copy");
        if !clone_file(&source, &copy) {
            let _ = ::std::fs::remove_dir_all(&root);
            eprintln!("skipped: {} cannot reflink", base.display());
            return;
        }
        drop(copy);
        drop(source);

        let (tree, failed) = scan_into_tree(&root, ScanOptions::default());
        let total = tree.get_total_size();
        let reflinked = tree.reflinked_files();
        let _ = ::std::fs::remove_dir_all(&root);

        assert_eq!(failed, 0);
        assert_eq!(reflinked, 1, "the shared extent should be recognised once");
        assert!(
            total < 2 * 256 * 1024,
            "two names for one set of blocks must not count twice, got {total}"
        );
        assert!(
            total >= 256 * 1024,
            "the blocks are still held once, got {total}"
        );
    }
}

/// btrfs snapshots, on a real btrfs volume.
///
/// Every subvolume and snapshot has its own `st_dev`, so anything keyed on the device sees a
/// snapshot's files as unrelated to the live ones they share every extent with — which is how a
/// scan of a volume with snapshots used to count the same data once per snapshot. Point
/// `DISKONAUT_TEST_BTRFS_DIR` at a directory on btrfs where this user can make subvolumes, with
/// `btrfs` on the `PATH`; `docs/probes/btrfs/run.sh` sets one up in a container.
#[cfg(target_os = "linux")]
mod btrfs {
    use super::{ScanOptions, scan_into_tree};
    use crate::linux::filesystem::classify;
    use ::std::path::{Path, PathBuf};
    use ::std::process::Command;

    fn btrfs(args: &[&str], path: &Path) -> bool {
        Command::new("btrfs")
            .args(args)
            .arg(path)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// A fresh `root` holding a subvolume `live`, or `None` when this cannot be done here.
    fn fixture(name: &str) -> Option<PathBuf> {
        let base = PathBuf::from(::std::env::var_os("DISKONAUT_TEST_BTRFS_DIR")?);
        let root = base.join(format!("diskonaut_btrfs_test_{name}"));
        remove(&root);
        ::std::fs::create_dir_all(&root).ok()?;
        assert!(
            btrfs(&["-q", "subvolume", "create"], &root.join("live")),
            "DISKONAUT_TEST_BTRFS_DIR is set, so making a subvolume there has to work"
        );
        Some(root)
    }

    /// Snapshots are subvolumes, which `remove_dir_all` cannot take away on its own.
    fn remove(root: &Path) {
        if let Ok(entries) = ::std::fs::read_dir(root) {
            for entry in entries.flatten() {
                btrfs(&["-q", "subvolume", "delete"], &entry.path());
            }
        }
        let _ = ::std::fs::remove_dir_all(root);
    }

    fn snapshot(root: &Path, name: &str) {
        assert!(
            Command::new("btrfs")
                .args(["-q", "subvolume", "snapshot", "-r"])
                .arg(root.join("live"))
                .arg(root.join(name))
                .status()
                .is_ok_and(|status| status.success()),
            "snapshot {name}"
        );
    }

    fn write(path: &Path, bytes: usize, seed: u8) {
        // Not all one byte, so that compression, if the volume has it on, does not make the test
        // about compression.
        let data: Vec<u8> = (0..bytes)
            .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed) ^ (index >> 8) as u8)
            .collect();
        ::std::fs::write(path, data).expect("write");
    }

    #[test]
    fn every_subvolume_and_snapshot_names_the_same_extent_space() {
        let Some(root) = fixture("extent_space") else {
            return;
        };
        snapshot(&root, "snap");
        let spaces: Vec<Option<u64>> = [root.clone(), root.join("live"), root.join("snap")]
            .iter()
            .map(|path| classify(path).extent_space)
            .collect();
        remove(&root);
        assert!(spaces[0].is_some(), "btrfs names its filesystem");
        assert!(
            spaces.iter().all(|space| *space == spaces[0]),
            "one filesystem, one address space, whatever st_dev says: {spaces:?}"
        );
    }

    #[test]
    fn a_snapshot_of_a_large_file_is_counted_once() {
        let Some(root) = fixture("snapshot_large") else {
            return;
        };
        let size = 1024 * 1024;
        write(&root.join("live/big"), size, 1);
        snapshot(&root, "snap1");
        snapshot(&root, "snap2");
        // Changed after the snapshots: now two different files, both held in full.
        write(&root.join("live/changed"), size, 2);
        snapshot(&root, "snap3");
        write(&root.join("live/changed"), size, 3);

        let (tree, failed) = scan_into_tree(&root, ScanOptions::default());
        let total = tree.get_total_size();
        remove(&root);

        assert_eq!(failed, 0);
        // `big` once, and `changed` twice: the live copy and the one in snap3.
        let held = 3 * size as u128;
        assert!(
            total >= held && total < held + 64 * 1024,
            "expected about {held}, got {total}: a snapshot counted again is {} more",
            size
        );
    }
}

/// The parallel build is what the app uses; it has to agree with the single-threaded tree on a
/// real directory, not only on synthetic groups.
#[test]
fn parallel_build_matches_the_single_threaded_tree() {
    let (dir, expected) = fixture_tree("parallel_fixture");
    let options = ScanOptions {
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (single, single_failed) = scan_into_tree(&dir, options);
    let directories = expected.iter().filter(|path| path.is_dir()).count() + 1;

    let mut seen = 0usize;
    let (parallel, failed, _, _) = crate::parallel::build_tree(&dir, options, 3, 1, |_| {
        seen += 1;
        true
    })
    .expect("nothing asked the scan to stop");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(failed, single_failed);
    assert_eq!(parallel.get_total_size(), single.get_total_size());
    assert_eq!(
        parallel.get_total_descendants(),
        single.get_total_descendants()
    );
    assert_eq!(parallel.get_total_descendants(), expected.len() as u64);
    assert_eq!(
        seen, directories,
        "progress sees every directory exactly once, the root included"
    );
}

/// A single shard skips deferral and charges shared blocks inline, so it must land on exactly the
/// single-threaded tree even when the fixture holds a hard link — the case deferral exists for.
#[test]
fn single_shard_build_matches_the_single_threaded_tree() {
    let dir = temp_scan_dir("single_shard_build");
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    let original = a.join("file");
    File::create(&original)
        .expect("create file")
        .write_all(&[0u8; 4096])
        .expect("write file");
    std::fs::hard_link(&original, b.join("link")).expect("hard link b/link");

    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: TRACK_EVERY_LINK,
        ..ScanOptions::default()
    };
    let (single, single_failed) = scan_into_tree(&dir, options);
    let (sharded, failed, _, _) = crate::parallel::build_tree(&dir, options, 1, 1, |_| true)
        .expect("nothing asked the scan to stop");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(failed, single_failed);
    assert_eq!(sharded.get_total_size(), single.get_total_size());
    assert_eq!(
        sharded.get_total_descendants(),
        single.get_total_descendants()
    );
    assert_eq!(
        sharded.hard_linked_files(),
        single.hard_linked_files(),
        "the hard link is charged once, inline, without a replay"
    );
}

#[test]
fn parallel_build_stops_when_progress_says_so() {
    let (dir, _) = fixture_tree("parallel_stop");
    let result = crate::parallel::build_tree(&dir, ScanOptions::default(), 2, 1, |_| false);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.is_none(), "a stopped scan yields no tree");
}

/// Two names for one file in `dir`, of `bytes` bytes, and the scan of `root` with default options.
#[cfg(windows)]
fn scan_with_one_hard_link(
    root: &std::path::Path,
    dir: &std::path::Path,
    bytes: usize,
) -> libdiskonaut::FileTree {
    std::fs::create_dir_all(dir).expect("mkdir");
    File::create(dir.join("one"))
        .expect("create file")
        .write_all(&vec![5u8; bytes])
        .expect("write file");
    std::fs::hard_link(dir.join("one"), dir.join("two")).expect("hard link");
    let options = ScanOptions {
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, failed) = scan_into_tree(root, options);
    assert_eq!(failed, 0);
    tree
}

/// By default Windows tracks hard links only where they are normally made, so one made by hand
/// elsewhere is counted once per name: an overstatement, the documented price of not tracking
/// every file.
#[cfg(windows)]
#[test]
fn windows_counts_an_untracked_hard_link_per_name() {
    let root = temp_scan_dir("windows_untracked_link");
    let tree = scan_with_one_hard_link(&root, &root, 4096);
    assert_eq!(tree.get_total_size(), 2 * 4096);
    assert_eq!(tree.hard_linked_files(), 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// Below `node_modules`, where pnpm links packages, a hard link is counted once by default.
#[cfg(windows)]
#[test]
fn windows_counts_a_hard_link_in_a_hot_spot_once() {
    let root = temp_scan_dir("windows_hot_spot_link");
    let tree = scan_with_one_hard_link(&root, &root.join("Node_Modules").join("pkg"), 4096);
    assert_eq!(tree.get_total_size(), 4096, "two names, one file");
    assert_eq!(tree.hard_linked_files(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// Scanning from inside a hot spot tracks too, though no directory entered on the way down was
/// named like one.
#[cfg(windows)]
#[test]
fn windows_tracks_below_a_hot_spot_scan_root() {
    let outer = temp_scan_dir("windows_hot_spot_root");
    let root = outer.join("node_modules");
    let tree = scan_with_one_hard_link(&root, &root.join("pkg"), 4096);
    assert_eq!(tree.get_total_size(), 4096);
    let _ = std::fs::remove_dir_all(&outer);
}

/// Files below the threshold are not tracked, so a link among them is counted per name.
#[cfg(windows)]
#[test]
fn windows_threshold_skips_smaller_files() {
    let root = temp_scan_dir("windows_threshold");
    std::fs::create_dir_all(&root).expect("mkdir");
    for (name, bytes) in [("small", 4096usize), ("large", 64 * 1024)] {
        File::create(root.join(name))
            .expect("create file")
            .write_all(&vec![5u8; bytes])
            .expect("write file");
        std::fs::hard_link(root.join(name), root.join(format!("{name}-again"))).expect("link");
    }
    let options = ScanOptions {
        show_apparent_size: true,
        hard_link_threshold: Some(32 * 1024),
        ..ScanOptions::default()
    };
    let (tree, _) = scan_into_tree(&root, options);
    assert_eq!(tree.get_total_size(), 2 * 4096 + 64 * 1024);
    assert_eq!(tree.hard_linked_files(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// Scan `root` twice — once for on-disk size (the default), once for the logical length — and
/// return `(on_disk, apparent, logical_length_of `path`)`. The tree's own root is canonical, so
/// `path` is looked up only for its length.
#[cfg(any(windows, target_os = "linux"))]
fn on_disk_and_apparent(root: &std::path::Path, path: &std::path::Path) -> (u128, u128, u128) {
    let logical = u128::from(std::fs::metadata(path).expect("stat the test file").len());
    let (on_disk, _) = scan_into_tree(root, ScanOptions::default());
    let (apparent, _) = scan_into_tree(
        root,
        ScanOptions {
            show_apparent_size: true,
            ..ScanOptions::default()
        },
    );
    (on_disk.get_total_size(), apparent.get_total_size(), logical)
}

/// A sparse file is charged its allocated blocks, not its length: `set_len` grows the file to 16
/// MiB of hole with four bytes at the tail, and the default (on-disk) scan must see only the
/// handful of blocks behind it while `-a` still reports the full length.
#[cfg(target_os = "linux")]
#[test]
fn linux_sparse_file_is_sized_by_its_blocks() {
    use std::io::{Seek, SeekFrom};
    let root = temp_scan_dir("linux_sparse");
    let path = root.join("sparse.bin");
    let mut file = File::create(&path).expect("create sparse file");
    file.set_len(16 * 1024 * 1024).expect("grow to a hole");
    file.seek(SeekFrom::End(-4)).expect("seek to tail");
    file.write_all(&[1, 2, 3, 4]).expect("write the tail");
    file.sync_all().expect("flush");
    drop(file);

    let (on_disk, apparent, logical) = on_disk_and_apparent(&root, &path);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(apparent, logical, "apparent size is the logical length");
    assert!(
        on_disk < logical / 2,
        "on-disk {on_disk} should be far below the {logical}-byte length of a sparse file"
    );
}

/// FSCTL controls used only by the Windows disk-size tests: mark a file compressed or sparse in
/// place, so the filesystem stores less than the logical length behind it.
#[cfg(windows)]
mod disk_size_fsctl {
    use ::core::ffi::c_void;
    use ::std::fs::File;
    use ::std::os::windows::io::AsRawHandle;

    const FSCTL_SET_COMPRESSION: u32 = 0x0009_C040;
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    const COMPRESSION_FORMAT_DEFAULT: u16 = 1;

    // Pointer types match the crate's other `DeviceIoControl` declaration (`os::windows`) so the
    // two do not clash as extern declarations of the same symbol.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            handle: *mut c_void,
            control_code: u32,
            in_buffer: *const u8,
            in_size: u32,
            out_buffer: *mut u8,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut u8,
        ) -> i32;
    }

    fn control(file: &File, code: u32, input: &[u8]) {
        let mut returned = 0u32;
        // SAFETY: `file` is open for writing for the duration of the call, `input` outlives it,
        // and no output buffer is requested, so the null output pointer with size zero is correct.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                code,
                input.as_ptr(),
                input.len() as u32,
                ::core::ptr::null_mut(),
                0,
                &mut returned,
                ::core::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "DeviceIoControl {code:#010x} failed");
    }

    pub fn set_compressed(file: &File) {
        control(
            file,
            FSCTL_SET_COMPRESSION,
            &COMPRESSION_FORMAT_DEFAULT.to_le_bytes(),
        );
    }

    pub fn set_sparse(file: &File) {
        control(file, FSCTL_SET_SPARSE, &[]);
    }
}

/// NTFS compression: the default scan reports the compressed size on disk, well under the logical
/// length, while `-a` reports the length. Confirmed against `GetCompressedFileSize` in the API
/// probe that motivated this — `AllocationSize`, which the walker reads, already equals it.
#[cfg(windows)]
#[test]
fn windows_compressed_file_is_sized_by_its_allocation() {
    use std::fs::OpenOptions;
    let root = temp_scan_dir("windows_compressed");
    let path = root.join("compressible.bin");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("create file");
    disk_size_fsctl::set_compressed(&file);
    // Half a MiB of one repeating pattern compresses to a small fraction of its length.
    let block = b"ABCDEFGH".repeat(64 * 1024);
    (&file).write_all(&block).expect("write compressible data");
    file.sync_all().expect("flush");
    drop(file);

    let (on_disk, apparent, logical) = on_disk_and_apparent(&root, &path);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(apparent, logical, "apparent size is the logical length");
    assert!(
        on_disk < apparent / 2,
        "on-disk {on_disk} should be well under the {apparent}-byte logical size of compressed data"
    );
}

/// A sparse file on NTFS is charged its allocated clusters, not its 16 MiB length.
#[cfg(windows)]
#[test]
fn windows_sparse_file_is_sized_by_its_allocation() {
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom};
    let root = temp_scan_dir("windows_sparse");
    let path = root.join("sparse.bin");
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("create file");
    disk_size_fsctl::set_sparse(&file);
    file.set_len(16 * 1024 * 1024).expect("grow to a hole");
    file.seek(SeekFrom::End(-4)).expect("seek to tail");
    file.write_all(&[1, 2, 3, 4]).expect("write the tail");
    file.sync_all().expect("flush");
    drop(file);

    let (on_disk, apparent, logical) = on_disk_and_apparent(&root, &path);
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(apparent, logical, "apparent size is the logical length");
    assert!(
        on_disk < 256 * 1024,
        "on-disk {on_disk} should be a handful of clusters, not the 16 MiB length"
    );
}

/// A folder's scan is not set against its volume's usage, and a tree told its volume's usage
/// reports the difference, which deleting a file does not change.
#[test]
fn outside_scan_is_recorded_for_volume_roots_and_holds_across_deletes() {
    let dir = temp_scan_dir("outside_scan");
    File::create(dir.join("file"))
        .expect("create file")
        .write_all(&[1u8; 8192])
        .expect("write file");
    let (mut tree, _) = scan_into_tree(&dir, ScanOptions::default());
    assert_eq!(tree.volume_used, None, "a temporary folder is not a volume");
    assert_eq!(tree.outside_scan(), None);

    let total = tree.get_total_size();
    tree.volume_used = Some(total + 500);
    assert_eq!(tree.outside_scan(), Some(500));
    tree.delete_file(&libdiskonaut::FileToDelete {
        path_in_filesystem: dir.clone(),
        path_to_file: vec![std::ffi::OsString::from("file")],
        file_type: libdiskonaut::tiles::FileType::File,
        num_descendants: None,
        size: total,
        sizes: libdiskonaut::model::Sizes::ZERO,
    });
    tree.note_freed(libdiskonaut::model::Sizes::new(total, total));
    assert_eq!(
        tree.outside_scan(),
        Some(500),
        "freed space is not outside the scan"
    );

    tree.volume_used = Some(0);
    assert_eq!(tree.outside_scan(), Some(0), "never negative");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Moved from the model's tests when the walkers left `libdiskonaut`: it needs a real scan.
#[test]
fn a_scanned_tree_shows_either_size_without_scanning_again() {
    use crate::{ScanOptions, scan_into_tree};
    use ::std::fs;
    use libdiskonaut::model::SizeKind;
    let dir = std::env::temp_dir().join("diskonaut_model_test_both_sizes");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // Sparse where the filesystem allows it: long, with a little written.
    let sparse = fs::File::create(dir.join("sparse")).unwrap();
    libdiskonaut::os::set_sparse(&sparse);
    sparse.set_len(8 << 20).unwrap();
    drop(sparse);
    fs::write(dir.join("small"), [1u8; 100]).unwrap();

    let (mut tree, _) = scan_into_tree(&dir, ScanOptions::default());
    let disk = tree.get_total_size();
    tree.shown = SizeKind::Apparent;
    let apparent = tree.get_total_size();
    let _ = fs::remove_dir_all(&dir);

    assert_eq!(apparent, (8 << 20) + 100);
    assert_eq!(tree.total_sizes().disk, disk);
    // NTFS keeps a small file's data in its record and reports what it takes there (to the
    // 8 bytes), not blocks.
    let granule = if cfg!(windows) { 8 } else { 512 };
    assert_eq!(disk % granule, 0, "whole blocks");
    assert!(disk < apparent, "the hole takes no space on disk");
}
