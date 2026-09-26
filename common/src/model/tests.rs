use ::std::ffi::OsString;
use ::std::fs;
use ::std::io::Write;
use ::std::path::PathBuf;

use crate::model::{FileOrFolder, FileTree, Folder};

#[test]
fn folder_add_file_updates_size() {
    let mut folder = Folder::default();
    folder.add_file(PathBuf::from("a.txt"), 100);
    folder.add_file(PathBuf::from("b.txt"), 250);
    assert_eq!(folder.sizes.disk, 350);
    assert_eq!(folder.num_descendants, 2);
}

#[test]
fn folder_nested_path() {
    let mut folder = Folder::default();
    folder.add_file(PathBuf::from("sub/file.txt"), 42);
    assert_eq!(folder.sizes.disk, 42);
    let sub = folder.path(vec!["sub".into()]).expect("subfolder exists");
    match sub {
        FileOrFolder::Folder(subfolder) => {
            assert_eq!(subfolder.sizes.disk, 42);
        }
        FileOrFolder::File(_) => panic!("expected folder"),
    }
}

#[test]
fn file_tree_delete_path() {
    let dir = std::env::temp_dir().join("duscape_model_test_delete");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    // Canonicalized because the app always scans a canonical path: `Opts::resolve_folder`
    // resolves the folder before it reaches `FileTree`, which canonicalizes its own root too. On
    // macOS `temp_dir()` is `/var/...`, a symlink to `/private/var/...`, so an uncanonicalized
    // path here fails `strip_prefix` against that root and every entry is silently dropped.
    let dir = dir.canonicalize().expect("canonicalize temp dir");
    let file_path = dir.join("gone.txt");
    let mut f = fs::File::create(&file_path).expect("create");
    f.write_all(b"x").expect("write");

    let metadata = fs::metadata(&file_path).expect("metadata");
    let mut tree = FileTree::new(Folder::new(&dir), dir.clone());
    tree.add_entry(
        crate::EntryMeta {
            size: metadata.len(),
            links: 1,
            is_dir: false,
            ..crate::EntryMeta::default()
        },
        &file_path,
    );
    assert_eq!(tree.get_total_descendants(), 1);

    let to_delete = crate::FileToDelete {
        path_in_filesystem: dir.clone(),
        path_to_file: vec!["gone.txt".into()],
        file_type: crate::tiles::FileType::File,
        num_descendants: None,
        size: metadata.len().into(),
        sizes: crate::model::Sizes::ZERO,
    };
    tree.delete_file(&to_delete);
    assert_eq!(tree.get_total_descendants(), 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn deleting_a_folder_removes_the_folder_itself_from_the_count() {
    // Built the way a scan builds it: directories arrive as entries in their own right.
    let mut root = Folder::default();
    root.add_file(PathBuf::from("keep.txt"), 10);
    root.add_folder(PathBuf::from("sub"));
    root.add_file(PathBuf::from("sub/one.txt"), 20);
    root.add_file(PathBuf::from("sub/two.txt"), 30);
    root.add_folder(PathBuf::from("sub/deeper"));
    root.add_file(PathBuf::from("sub/deeper/three.txt"), 40);
    // keep.txt, sub, sub/one.txt, sub/two.txt, sub/deeper, sub/deeper/three.txt
    assert_eq!(root.num_descendants, 6);
    assert_eq!(root.sizes.disk, 100);

    root.delete_path(&[OsString::from("sub")]);

    // Only keep.txt is left, and "sub" itself is gone along with its four descendants.
    assert_eq!(root.sizes.disk, 10);
    assert_eq!(
        root.num_descendants, 1,
        "the deleted folder itself must be subtracted too, not just its contents"
    );
}

#[test]
fn deleting_a_nested_folder_updates_every_ancestor() {
    let mut root = Folder::default();
    root.add_folder(PathBuf::from("a"));
    root.add_folder(PathBuf::from("a/b"));
    root.add_folder(PathBuf::from("a/b/c"));
    root.add_file(PathBuf::from("a/b/c/file.txt"), 64);
    root.add_file(PathBuf::from("a/other.txt"), 8);
    // a, a/b, a/b/c, a/b/c/file.txt, a/other.txt
    assert_eq!(root.num_descendants, 5);

    root.delete_path(&[OsString::from("a"), OsString::from("b")]);

    // a and a/other.txt remain; b, b/c and b/c/file.txt are gone.
    assert_eq!(root.num_descendants, 2);
    assert_eq!(root.sizes.disk, 8);
    let a = root.path(vec!["a".into()]).expect("a still exists");
    match a {
        FileOrFolder::Folder(a) => {
            assert_eq!(a.num_descendants, 1);
            assert_eq!(a.sizes.disk, 8);
        }
        FileOrFolder::File(_) => panic!("expected folder"),
    }
}

mod hard_links {
    use ::std::path::{Path, PathBuf};

    use crate::model::HardLinks;
    use crate::scan::SharedBlocks;

    /// The old ledger, kept as the specification: compare paths component by component.
    fn reference_charge(
        seen: &mut Vec<(u64, u64, Vec<PathBuf>)>,
        inode: u64,
        size: u64,
        directory: &Path,
    ) -> Option<usize> {
        let shared = |left: &Path, right: &Path| {
            left.components()
                .zip(right.components())
                .take_while(|(l, r)| l == r)
                .count()
        };
        match seen.iter_mut().find(|(i, _, _)| *i == inode) {
            Some((_, seen_size, _)) if *seen_size != size => None,
            Some((_, _, dirs)) => {
                if dirs.iter().any(|d| d == directory) {
                    return Some(directory.components().count());
                }
                let deepest = dirs.iter().map(|d| shared(d, directory)).max().unwrap_or(0);
                dirs.push(directory.to_path_buf());
                Some(deepest)
            }
            None => {
                seen.push((inode, size, vec![directory.to_path_buf()]));
                None
            }
        }
    }

    /// The interned ledger must answer exactly as the component-wise one does, in any order.
    #[test]
    fn interned_ledger_matches_component_wise_reference() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |bound: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % bound
        };
        let names = ["a", "b", "c"];
        let mut links = HardLinks::default();
        let mut reference = Vec::new();
        for _ in 0..20_000 {
            let depth = next(5) as usize;
            // Spelled the way a path might arrive, not only canonically: the old ledger compared
            // component-wise, and the new one must agree on `a/b/`, `a/./b` and `a//b` too.
            let mut dir = ::std::ffi::OsString::new();
            for level in 0..depth {
                if level > 0 {
                    dir.push(["/", "//", "/./"][next(3) as usize]);
                }
                dir.push(names[next(3) as usize]);
            }
            if depth > 0 && next(4) == 0 {
                dir.push("/");
            }
            let dir = PathBuf::from(dir);
            let inode = next(40);
            let size = 1024 * (1 + next(2));
            assert_eq!(
                links.charge(SharedBlocks::Inode(inode), size, &dir),
                reference_charge(&mut reference, inode, size, &dir),
                "inode {inode} size {size} in {}",
                dir.display()
            );
        }
        assert_eq!(links.tracked(), reference.len());
    }

    /// A file linked from hundreds of folders is charged from a set rather than a list; it must
    /// still answer as the reference does, before the switch and after it.
    #[test]
    fn a_file_linked_from_many_folders_matches_the_reference() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % bound
        };
        let mut links = HardLinks::default();
        let mut reference = Vec::new();
        for _ in 0..5_000 {
            let dir: PathBuf = (0..next(8))
                .map(|_| ["a", "b", "c", "d"][next(4) as usize])
                .collect();
            let inode = next(2);
            assert_eq!(
                links.charge(SharedBlocks::Inode(inode), 1024, &dir),
                reference_charge(&mut reference, inode, 1024, &dir),
                "inode {inode} in {}",
                dir.display()
            );
        }
        assert!(
            reference.iter().all(|(_, _, dirs)| dirs.len() > 100),
            "both files should be well past the switch to a set"
        );
    }

    /// A reflinked file is the hard-link rule again: the same blocks reached twice inside one
    /// folder count once, while siblings each hold them in full.
    #[test]
    fn reflinked_blocks_are_charged_once_per_folder() {
        let mut links = HardLinks::default();
        let blocks = SharedBlocks::Extent(0x4000_0000);
        // First sighting charges the whole path.
        assert_eq!(links.charge(blocks, 1024, Path::new("a")), None);
        // A copy in the same folder is already paid for, down to that folder's depth.
        assert_eq!(links.charge(blocks, 1024, Path::new("a")), Some(1));
        // A copy in a sibling: the root above them has it, `b` itself does not.
        assert_eq!(links.charge(blocks, 1024, Path::new("b")), Some(0));
        assert_eq!(links.tracked_reflinks(), 1);
        assert_eq!(links.tracked(), 0, "no hard links were charged");
    }

    /// An inode number and a physical block offset are unrelated numbers. Sharing one map would
    /// silently merge a hard-linked file with a reflinked one whose extent happened to match.
    #[test]
    fn inode_and_extent_identities_do_not_collide() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(42), 1024, Path::new("a")),
            None
        );
        assert_eq!(
            links.charge(SharedBlocks::Extent(42), 1024, Path::new("a")),
            None,
            "the same number under a different identity is a different file"
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(42), 1024, Path::new("b")),
            Some(0),
            "a second name for the inode finds only the inode's first"
        );
        assert_eq!(links.tracked(), 1);
        assert_eq!(links.tracked_reflinks(), 1);
    }

    /// Two files starting at one physical extent but differing in length share only part of
    /// themselves. Charging in full overstates; merging them would understate.
    #[test]
    fn partly_shared_files_are_charged_in_full() {
        let mut links = HardLinks::default();
        let blocks = SharedBlocks::Extent(0x8000);
        assert_eq!(links.charge(blocks, 4096, Path::new("a")), None);
        assert_eq!(links.charge(blocks, 8192, Path::new("b")), None);
    }

    /// `charge` returns the depth already accounted for, so folders below it still pay.
    #[test]
    fn spellings_of_one_directory_are_one_directory() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a/b")),
            None
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a/b/")),
            Some(2)
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a//b")),
            Some(2)
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a/./b")),
            Some(2)
        );
    }

    /// The first name is charged like an ordinary file, and is not yet a hard link: on Windows
    /// every file in a hot spot is tracked without knowing whether it has another name.
    #[test]
    fn first_sighting_charges_the_whole_path() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a")),
            None
        );
        assert_eq!(links.tracked(), 0, "one name so far");
        links.charge(SharedBlocks::Inode(1), 1024, Path::new("b"));
        assert_eq!(links.tracked(), 1);
    }

    #[test]
    fn a_second_link_in_the_same_folder_is_already_paid_for() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a")),
            None
        );
        // "a" is at depth 1 and already holds it, so nothing below depth 1 owes anything.
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a")),
            Some(1)
        );
    }

    #[test]
    fn a_link_in_a_sibling_folder_is_only_paid_for_by_shared_ancestors() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a")),
            None
        );
        // Only the root is shared, so "b" itself has not been charged yet.
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("b")),
            Some(0)
        );
    }

    /// The case a single running common-ancestor gets wrong: once two links have collapsed the
    /// common ancestor to the root, a third link beside one of them must still be recognised.
    #[test]
    fn a_third_link_beside_an_earlier_one_is_already_paid_for() {
        for order in [["b", "a", "a"], ["a", "b", "a"]] {
            let mut links = HardLinks::default();
            assert_eq!(
                links.charge(SharedBlocks::Inode(1), 1024, Path::new(order[0])),
                None
            );
            links.charge(SharedBlocks::Inode(1), 1024, Path::new(order[1]));
            let third = links.charge(SharedBlocks::Inode(1), 1024, Path::new(order[2]));
            assert_eq!(
                third,
                Some(1),
                "with order {order:?} the folder {:?} already holds this file",
                order[2]
            );
        }
    }

    #[test]
    fn deeper_folders_are_recognised_independently_of_shallower_ones() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 2048, Path::new("other")),
            None
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 2048, Path::new("one/two")),
            Some(0)
        );
        // "one/two" holds it already, so nothing below depth 2 owes anything.
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 2048, Path::new("one/two")),
            Some(2)
        );
        // A sibling of "two" shares only "one".
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 2048, Path::new("one/three")),
            Some(1)
        );
    }

    /// Inode numbers repeat across the volumes of a macOS volume group, so a differing size means
    /// a different file and must not be deduplicated.
    #[test]
    fn a_reused_inode_number_with_a_different_size_is_a_different_file() {
        let mut links = HardLinks::default();
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 1024, Path::new("a")),
            None
        );
        assert_eq!(
            links.charge(SharedBlocks::Inode(1), 4096, Path::new("b")),
            None
        );
    }
}

/// A tree built in parallel shards, merged, and reconciled must be indistinguishable from one
/// built inline — folder by folder, not just in total. This is the property the parallel build
/// rests on, and it is the ledger's order-independence made concrete: shards replay their shared
/// blocks in an order that has nothing to do with arrival order.
mod sharded {
    use ::std::collections::BTreeMap;
    use ::std::ffi::OsStr;
    use ::std::hash::{DefaultHasher, Hash, Hasher};
    use ::std::path::{Path, PathBuf};
    use ::std::sync::Arc;

    use crate::model::{FileOrFolder, FileTree, Folder};
    use crate::scan::{DirEntries, EntryMeta};

    fn folder_sizes(
        folder: &Folder,
        at: PathBuf,
        out: &mut BTreeMap<PathBuf, (crate::model::Sizes, u64)>,
    ) {
        out.insert(at.clone(), (folder.sizes, folder.num_descendants));
        for (name, node) in folder.contents.iter() {
            if let FileOrFolder::Folder(child) = node {
                folder_sizes(child, at.join(name), out);
            }
        }
    }

    /// One group per directory of a small random tree, in random order, thick with shared
    /// blocks: hard links (same inode, same size, many folders) and reflinks (same extent).
    fn synthetic_groups(root: &Path, seed: u64) -> Vec<DirEntries> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        // A tree of directories: a few names reused at every level so shards share ancestors.
        let mut dirs: Vec<PathBuf> = vec![PathBuf::new()];
        for depth in 1..=4 {
            let parents: Vec<PathBuf> = dirs
                .iter()
                .filter(|dir| dir.components().count() == depth - 1)
                .cloned()
                .collect();
            for parent in parents {
                let children = next(4);
                for _ in 0..children {
                    let name = ["d0", "d1", "d2", "d3", "d4", "d5"][next(6) as usize];
                    let child = parent.join(name);
                    if !dirs.contains(&child) {
                        dirs.push(child);
                    }
                }
            }
        }
        let mut groups: Vec<DirEntries> = dirs
            .iter()
            .map(|dir| {
                let mut group = DirEntries::new(Arc::from(root.join(dir).as_path()));
                for child in dirs
                    .iter()
                    .filter(|other| other.parent() == Some(dir.as_path()))
                {
                    let name = child.file_name().expect("child has a name");
                    group.push(
                        name,
                        EntryMeta {
                            is_dir: true,
                            ..EntryMeta::default()
                        },
                    );
                }
                let files = next(7);
                for i in 0..files {
                    let name = format!("f{i}");
                    let meta = match next(3) {
                        0 => EntryMeta {
                            size: 1 + next(5000),
                            inode: 1_000_000 + next(1_000_000),
                            links: 1,
                            ..EntryMeta::default()
                        },
                        1 => {
                            // A hard link: one of 12 inodes, each with a fixed size.
                            let inode = 1 + next(12);
                            EntryMeta {
                                size: 1024 * (1 + inode % 3),
                                inode,
                                links: 5,
                                ..EntryMeta::default()
                            }
                        }
                        _ => {
                            // A reflink: one of 8 extents, each with a fixed size.
                            let extent = 100 + next(8);
                            EntryMeta {
                                size: 4096 * (1 + extent % 4),
                                inode: 5_000_000 + next(1_000_000),
                                links: 1,
                                shared_extent: extent,
                                ..EntryMeta::default()
                            }
                        }
                    };
                    group.push(OsStr::new(&name), meta);
                }
                group
            })
            .collect();
        // Random arrival order, so children regularly arrive before their parents.
        for i in (1..groups.len()).rev() {
            let j = next(i as u64 + 1) as usize;
            groups.swap(i, j);
        }
        groups
    }

    fn shard_of(path: &Path, shards: usize) -> usize {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        (hasher.finish() % shards as u64) as usize
    }

    #[test]
    fn sharded_build_matches_inline_build_folder_by_folder() {
        let root = PathBuf::from("/synthetic/root");
        for seed in [1u64, 7, 42, 1234, 99_999] {
            let mut inline = FileTree::new(Folder::new(&root), root.clone());
            for group in synthetic_groups(&root, seed) {
                inline.add_dir_entries(group);
            }
            let mut expected = BTreeMap::new();
            folder_sizes(inline.get_current_folder(), PathBuf::new(), &mut expected);
            assert!(
                inline.hard_linked_files() > 0 && inline.reflinked_files() > 0,
                "seed {seed} must exercise both kinds of sharing to prove anything"
            );

            for shards in [1usize, 2, 3, 4, 8] {
                let mut trees: Vec<FileTree> = (0..shards)
                    .map(|_| FileTree::deferring_shared_blocks(Folder::new(&root), root.clone()))
                    .collect();
                for group in synthetic_groups(&root, seed) {
                    let shard = shard_of(&group.path, shards);
                    trees[shard].add_dir_entries(group);
                }
                let mut merged = trees.pop().expect("at least one shard");
                for tree in trees {
                    merged.merge_from(tree);
                }
                merged.replay_deferred();

                let mut actual = BTreeMap::new();
                folder_sizes(merged.get_current_folder(), PathBuf::new(), &mut actual);
                assert_eq!(
                    actual, expected,
                    "seed {seed}, {shards} shards: folder sizes or descendant counts differ"
                );
                assert_eq!(merged.hard_linked_files(), inline.hard_linked_files());
                assert_eq!(merged.reflinked_files(), inline.reflinked_files());
                assert_eq!(merged.get_total_size(), inline.get_total_size());
            }
        }
    }

    /// The placeholder case on its own: a child's group arrives before its parent's, in a
    /// different shard, so the parent exists twice at merge time — once real, once as a stub.
    #[test]
    fn a_stub_parent_merges_into_the_real_one() {
        let root = PathBuf::from("/synthetic/root");
        let file = |size: u64| EntryMeta {
            size,
            inode: size,
            links: 1,
            ..EntryMeta::default()
        };
        let mut shard_a = FileTree::deferring_shared_blocks(Folder::new(&root), root.clone());
        let mut shard_b = FileTree::deferring_shared_blocks(Folder::new(&root), root.clone());

        // Shard B gets `top/inner` and creates `top` as a stub on the way.
        let mut inner = DirEntries::new(Arc::from(root.join("top/inner").as_path()));
        inner.push(OsStr::new("deep.bin"), file(300));
        shard_b.add_dir_entries(inner);

        // Shard A gets `top` itself, with the real entry for `inner` and a file of its own.
        let mut top = DirEntries::new(Arc::from(root.join("top").as_path()));
        top.push(
            OsStr::new("inner"),
            EntryMeta {
                is_dir: true,
                ..EntryMeta::default()
            },
        );
        top.push(OsStr::new("shallow.bin"), file(200));
        shard_a.add_dir_entries(top);

        shard_a.merge_from(shard_b);
        shard_a.replay_deferred();

        let mut sizes = BTreeMap::new();
        folder_sizes(shard_a.get_current_folder(), PathBuf::new(), &mut sizes);
        assert_eq!(sizes[&PathBuf::from("")].0.disk, 500);
        assert_eq!(sizes[&PathBuf::from("top")].0.disk, 500);
        assert_eq!(sizes[&PathBuf::from("top/inner")].0.disk, 300);
        assert_eq!(
            sizes.len(),
            3,
            "no duplicate `top` or `inner` survived the merge"
        );
        assert_eq!(
            sizes[&PathBuf::from("top")].1,
            3,
            "inner, shallow.bin, deep.bin"
        );
    }
}

/// The live view during a parallel scan: every folder, with running sizes, and no files.
mod outline {
    use ::std::ffi::{OsStr, OsString};
    use ::std::path::{Path, PathBuf};
    use ::std::sync::Arc;

    use crate::model::{FileOrFolder, FileTree, Folder};
    use crate::scan::{DirEntries, DirSummary, EntryMeta};

    fn summary(root: &Path, relative: &str, subdirs: &[&str], files: &[u64]) -> DirSummary {
        let mut group = DirEntries::new(Arc::from(root.join(relative).as_path()));
        for name in subdirs {
            group.push(
                OsStr::new(name),
                EntryMeta {
                    is_dir: true,
                    ..EntryMeta::default()
                },
            );
        }
        for (index, size) in files.iter().enumerate() {
            group.push(
                OsStr::new(&format!("f{index}")),
                EntryMeta {
                    size: *size,
                    inode: 1 + index as u64,
                    links: 1,
                    ..EntryMeta::default()
                },
            );
        }
        DirSummary::of(&group)
    }

    fn folder<'a>(tree: &'a FileTree, path: &[&str]) -> &'a Folder {
        let names: Vec<OsString> = path.iter().map(OsString::from).collect();
        match tree.get_current_folder().path(names) {
            Some(FileOrFolder::Folder(folder)) => folder,
            _ => panic!("{path:?} should be a folder"),
        }
    }

    #[test]
    fn an_outline_has_every_folder_with_running_sizes_and_no_files() {
        let root = PathBuf::from("/synthetic/root");
        let mut tree = FileTree::new(Folder::new(&root), root.clone());
        tree.add_summary(summary(&root, "", &["a"], &[]));
        tree.add_summary(summary(&root, "a", &["b"], &[100]));
        tree.add_summary(summary(&root, "a/b", &[], &[50, 25]));

        assert_eq!(tree.get_total_size(), 175);
        let a = folder(&tree, &["a"]);
        assert_eq!(
            a.sizes.disk, 175,
            "a holds its own file and everything under b"
        );
        assert_eq!(
            a.contents.len(),
            1,
            "the subfolder is there; the file is not"
        );
        let b = folder(&tree, &["a", "b"]);
        assert_eq!(b.sizes.disk, 75);
        assert!(b.contents.is_empty(), "files are not part of the outline");

        // Descendants count files too, so the tile labels read right during the scan.
        assert_eq!(tree.get_total_descendants(), 5, "a, b and three files");
        assert_eq!(a.num_descendants, 4);
        assert_eq!(b.num_descendants, 2);
    }

    /// Rolling deep directories up into the frontier must lose nothing the view shows: the root's
    /// totals, and every folder above the frontier, agree with an outline built from everything.
    #[test]
    fn a_capped_outline_keeps_the_totals_it_shows() {
        use crate::scan::Outline;
        let root = PathBuf::from("/synthetic/root");
        // Directories at depths 0..=5, each with one subfolder and files of a known size.
        let chain = ["", "a", "a/b", "a/b/c", "a/b/c/d", "a/b/c/d/e"];
        let groups: Vec<_> = chain
            .iter()
            .enumerate()
            .map(|(depth, relative)| {
                let child = chain.get(depth + 1).map(|next| {
                    Path::new(next)
                        .file_name()
                        .expect("named")
                        .to_str()
                        .expect("utf8")
                });
                let subdirs: Vec<&str> = child.into_iter().collect();
                let files: Vec<u64> = vec![100 * (depth as u64 + 1)];
                (relative, subdirs, files)
            })
            .collect();

        let mut full = FileTree::new(Folder::new(&root), root.clone());
        let mut capped = FileTree::new(Folder::new(&root), root.clone());
        let mut outline = Outline::new(root.clone(), 2, usize::MAX);
        for (relative, subdirs, files) in &groups {
            let sizes: Vec<u64> = files.clone();
            let group = {
                let mut group = DirEntries::new(Arc::from(root.join(relative).as_path()));
                for name in subdirs {
                    group.push(
                        OsStr::new(name),
                        EntryMeta {
                            is_dir: true,
                            ..EntryMeta::default()
                        },
                    );
                }
                for (index, size) in sizes.iter().enumerate() {
                    group.push(
                        OsStr::new(&format!("f{index}")),
                        EntryMeta {
                            size: *size,
                            inode: 1 + index as u64,
                            links: 1,
                            ..EntryMeta::default()
                        },
                    );
                }
                group
            };
            full.add_summary(DirSummary::of(&group));
            assert!(outline.add(&group).is_none(), "no batch until asked");
        }
        let summaries = outline.finish();
        assert_eq!(
            summaries.len(),
            4,
            "depths 0, 1 and 2 as they are, plus one frontier folder at depth 3 for the rest"
        );
        for summary in summaries {
            capped.add_summary(summary);
        }

        assert_eq!(capped.get_total_size(), full.get_total_size());
        assert_eq!(capped.get_total_descendants(), full.get_total_descendants());
        for shown in ["a", "a/b", "a/b/c"] {
            let names: Vec<OsString> = shown.split('/').map(OsString::from).collect();
            let want = folder(&full, &shown.split('/').collect::<Vec<_>>());
            let got = match capped.get_current_folder().path(names) {
                Some(FileOrFolder::Folder(folder)) => folder,
                _ => panic!("{shown} should exist in the capped outline"),
            };
            assert_eq!(got.sizes, want.sizes, "{shown} size");
            assert_eq!(
                got.num_descendants, want.num_descendants,
                "{shown} descendants"
            );
        }
        let frontier = folder(&capped, &["a", "b", "c"]);
        assert!(
            frontier.contents.is_empty(),
            "below the frontier nothing is known until the scan completes"
        );
    }

    #[test]
    fn the_finished_tree_keeps_navigation_that_still_resolves() {
        let root = PathBuf::from("/synthetic/root");
        let mut outline = FileTree::new(Folder::new(&root), root.clone());
        outline.add_summary(summary(&root, "", &["a"], &[]));
        outline.add_summary(summary(&root, "a", &[], &[1]));
        outline.enter_folder(OsStr::new("a"));

        let mut full = FileTree::new(Folder::new(&root), root.clone());
        let mut group = DirEntries::new(Arc::from(root.as_path()));
        group.push(
            OsStr::new("a"),
            EntryMeta {
                is_dir: true,
                ..EntryMeta::default()
            },
        );
        full.add_dir_entries(group);
        full.adopt_navigation_from(&outline);
        assert_eq!(full.current_folder_names, vec![OsString::from("a")]);

        let mut unrelated = FileTree::new(Folder::new(&root), root.clone());
        unrelated.adopt_navigation_from(&outline);
        assert!(
            unrelated.current_folder_names.is_empty(),
            "a folder that does not exist here falls back to the root"
        );
    }
}

fn tree_of(files: &[(&str, u128)]) -> FileTree {
    let mut folder = Folder::default();
    for (path, size) in files {
        folder.add_file(PathBuf::from(path), *size);
    }
    FileTree::new(folder, PathBuf::from("/nonexistent/graft_root"))
}

fn folder_at<'a>(tree: &'a FileTree, path: &[&str]) -> &'a Folder {
    if path.is_empty() {
        return tree.get_current_folder();
    }
    match tree
        .get_current_folder()
        .path(path.iter().map(OsString::from).collect())
    {
        Some(FileOrFolder::Folder(folder)) => folder,
        _ => panic!("no folder at {path:?}"),
    }
}

#[test]
fn graft_replaces_a_folder_and_corrects_every_ancestor() {
    let mut tree = tree_of(&[
        ("a/b/old1", 100),
        ("a/b/old2", 50),
        ("a/other", 7),
        ("top", 3),
    ]);
    // The folder as it now is on disk: one file gone, one grown, one new.
    let rescanned = tree_of(&[("old1", 400), ("new/deep", 10)]);
    let old = tree.graft(&["a".into(), "b".into()], rescanned);
    assert_eq!(old.map(|folder| folder.sizes.disk), Some(150));

    // `add_file` counts files alone as descendants, not the folders made on the way to them.
    let b = folder_at(&tree, &["a", "b"]);
    assert_eq!((b.sizes.disk, b.num_descendants), (410, 2));
    let a = folder_at(&tree, &["a"]);
    assert_eq!((a.sizes.disk, a.num_descendants), (417, 3));
    let root = folder_at(&tree, &[]);
    assert_eq!((root.sizes.disk, root.num_descendants), (420, 4));
}

#[test]
fn graft_of_a_folder_no_longer_in_the_tree_changes_nothing() {
    let mut tree = tree_of(&[("a/file", 5)]);
    assert!(tree.graft(&["gone".into()], tree_of(&[("x", 1)])).is_none());
    assert!(
        tree.graft(&["a".into(), "file".into()], tree_of(&[("x", 1)]))
            .is_none()
    );
    assert_eq!(tree.get_total_size(), 5);
}

#[test]
fn graft_at_the_root_replaces_the_tree_but_keeps_where_the_user_is() {
    let mut tree = tree_of(&[("a/b/file", 5)]);
    tree.enter_folder("a".as_ref());
    tree.space_freed = crate::model::Sizes::new(9, 9);
    tree.graft(&[], tree_of(&[("a/c", 20)]));
    assert_eq!(tree.get_total_size(), 20);
    assert_eq!(tree.current_folder_names, vec![OsString::from("a")]);
    assert_eq!(tree.space_freed, crate::model::Sizes::new(9, 9));
}

#[test]
fn a_removed_path_leaves_the_current_folder_missing() {
    let mut tree = tree_of(&[("a/b/file", 5), ("c", 1)]);
    tree.enter_folder("a".as_ref());
    tree.enter_folder("b".as_ref());
    assert!(tree.remove_path(&["a".into()]));
    assert!(!tree.remove_path(&["a".into()]));
    assert_eq!(tree.get_total_size(), 1);
    assert!(!tree.current_folder_exists());
}

#[test]
fn found_small_files_are_charged_once_and_only_to_the_files_they_name() {
    use crate::scan::Found;
    use ::std::sync::Arc;
    let mut tree = tree_of(&[("a/one", 8192), ("b/copy", 8192), ("b/other", 4096)]);
    let root = tree.path_in_filesystem.clone();
    let found = |dir: &str, name: &str, size: u64| Found {
        dir: Arc::from(root.join(dir).as_path()),
        files: vec![crate::scan::FoundFile {
            name: OsString::from(name),
            sizes: crate::model::Sizes::new(u128::from(size), u128::from(size)),
            identity: 42,
        }],
    };
    // The first sighting of the blocks keeps them; the second gives them back.
    assert_eq!(
        tree.apply_found(&[found("a", "one", 8192)]),
        0,
        "nothing to take back yet"
    );
    assert_eq!(tree.get_total_size(), 20480);
    assert_eq!(tree.apply_found(&[found("b", "copy", 8192)]), 1);
    assert_eq!(tree.get_total_size(), 12288);
    // Each folder counts the blocks once: both still hold them, only their common parent had
    // counted them twice.
    assert_eq!(folder_at(&tree, &["b"]).sizes.disk, 12288);
    assert_eq!(folder_at(&tree, &["a"]).sizes.disk, 8192);
    // Gone since, or changed size: left alone.
    assert_eq!(tree.apply_found(&[found("b", "deleted", 8192)]), 0);
    assert_eq!(tree.apply_found(&[found("b", "other", 9999)]), 0);
    assert_eq!(tree.apply_found(&[found("nowhere", "x", 1)]), 0);
    assert_eq!(tree.get_total_size(), 12288);
}

/// What every entry of every folder costs: two sizes must not make it grow.
#[test]
fn a_tree_entry_holds_both_sizes_in_sixteen_bytes() {
    assert_eq!(::std::mem::size_of::<FileOrFolder>(), 16);
}

#[test]
fn both_sizes_are_exact_however_far_apart() {
    use crate::model::File;
    for (disk, apparent) in [
        (4096, 1),
        (0, 0),
        (4096, 4096),
        // Sparse: a 64 MiB file with 1 MiB written.
        (1 << 20, 64 << 20),
        // Further apart than an `i32` of bytes: lastlog, a compressed disk image.
        (0, 1_200_000_000_000),
        (5_000_000_000, 1),
        (u64::MAX, 0),
        (0, u64::MAX),
    ] {
        let file = File::new(disk, apparent);
        assert_eq!((file.disk(), file.apparent()), (disk, apparent));
    }
}

#[test]
fn a_whole_rescan_does_not_count_earlier_frees_against_the_volume_again() {
    use crate::model::Sizes;
    let mut tree = tree_of(&[("big", 1000), ("small", 10)]);
    tree.volume_used = Some(1010);
    tree.delete_file(&crate::FileToDelete {
        path_in_filesystem: tree.path_in_filesystem.clone(),
        path_to_file: vec![OsString::from("big")],
        file_type: crate::tiles::FileType::File,
        num_descendants: None,
        size: 1000,
        sizes: Sizes::new(1000, 1000),
    });
    tree.note_freed(Sizes::new(1000, 1000));
    assert_eq!(tree.outside_scan(), Some(0));

    // Scanned again: the volume is measured after the delete, and the file is not in the tree.
    let mut rescanned = tree_of(&[("small", 10)]);
    rescanned.volume_used = Some(10);
    tree.graft(&[], rescanned);
    assert_eq!(tree.outside_scan(), Some(0), "not -1000");
    assert_eq!(
        tree.space_freed,
        Sizes::new(1000, 1000),
        "the session's total carries over"
    );
}
