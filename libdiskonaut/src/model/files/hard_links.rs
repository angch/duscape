use ::std::collections::hash_map::Entry;
use ::std::ffi::OsStr;
use ::std::path::{Path, PathBuf};

use super::hash::{FastMap, FastSet};
use crate::scan::SharedBlocks;

/// A directory known to the ledger, addressed by its position in `HardLinks::dirs`.
///
/// Directories are interned so that the per-link work is integer comparison and pointer chasing
/// rather than path parsing: with a few hundred thousand hard links, several of which live in
/// dozens of folders each, comparing paths component by component was most of the cost of
/// building the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DirRef(u32);

struct Dir {
    parent: DirRef,
    depth: u32,
}

/// Folders holding links to one file, past which [`LinkedFile`] keeps the folders they charge as
/// a set instead.
///
/// Charging a link against a list compares it with every folder already holding one, so a file
/// linked from N folders costs O(N² · depth) to build. A few files are linked from thousands of
/// folders — three on one whole-disk scan of macOS were linked from 20,000 folders between them —
/// and those alone took nearly a second. Below this, the list is smaller and no slower.
const INDEXED_AT: usize = 32;

/// A file reachable by more than one path, and where those paths were.
struct LinkedFile {
    size: u64,
    /// One entry per distinct folder holding a link, in the order they were found. Emptied when
    /// `charged` takes over.
    directories: Vec<DirRef>,
    /// Every folder already charged for this file — each folder holding a link, and all of its
    /// ancestors — once `directories` outgrows [`INDEXED_AT`].
    charged: Option<FastSet<DirRef>>,
    /// Whether a second name has been seen. A file whose link count was not known
    /// ([`crate::scan::LINKS_UNKNOWN`]) is tracked from its first name, and may have no other.
    linked: bool,
}

/// Charges each hard-linked file to any one folder at most once.
///
/// A folder's size answers "how much space is held under here", so the same blocks reached twice
/// within one folder must count once. Two links in sibling folders, though, each hold that space
/// on their own — while the parent they share still counts it once.
///
/// With `a/a`, `a/b` and `b/a` all links to the same 1 KiB file: `a` is 1 KiB, `b` is 1 KiB, and
/// the root containing both is 1 KiB.
pub struct HardLinks {
    files: FastMap<u64, LinkedFile>,
    /// The same ledger keyed on physical extent, for copy-on-write reflinks. Kept apart from
    /// `files` because an inode number and a block offset are unrelated numbers that would
    /// otherwise collide.
    reflinks: FastMap<u64, LinkedFile>,
    /// Interned directories; index 0 is the scan root (the empty relative path).
    dirs: Vec<Dir>,
    /// Normalised relative directory path to its interned id.
    ids: FastMap<Box<OsStr>, DirRef>,
    /// Reused between calls to hold the normalised form of the path being interned.
    scratch: PathBuf,
}

const ROOT: DirRef = DirRef(0);

impl Default for HardLinks {
    fn default() -> Self {
        let mut ids = FastMap::default();
        ids.insert(Box::from(OsStr::new("")), ROOT);
        Self {
            files: FastMap::default(),
            reflinks: FastMap::default(),
            dirs: vec![Dir {
                parent: ROOT,
                depth: 0,
            }],
            ids,
            scratch: PathBuf::new(),
        }
    }
}

/// Depth of the deepest folder that contains both directories: how many leading components
/// their paths share.
fn shared_depth(dirs: &[Dir], mut left: DirRef, mut right: DirRef) -> u32 {
    let at = |dir: DirRef| &dirs[dir.0 as usize];
    let (mut left_depth, mut right_depth) = (at(left).depth, at(right).depth);
    while left_depth > right_depth {
        left = at(left).parent;
        left_depth -= 1;
    }
    while right_depth > left_depth {
        right = at(right).parent;
        right_depth -= 1;
    }
    while left != right {
        left = at(left).parent;
        right = at(right).parent;
        left_depth -= 1;
    }
    left_depth
}

/// Mark `directory` and its ancestors as charged, returning the depth of the deepest of them that
/// already was.
///
/// That is the answer the list gives: a folder is already charged exactly when it is an ancestor
/// of some earlier link, so the deepest folder `directory` shares with any of them is the first
/// charged one found walking up. The root is charged by the first link, so the walk always stops.
fn charge_ancestors(dirs: &[Dir], charged: &mut FastSet<DirRef>, mut directory: DirRef) -> u32 {
    while charged.insert(directory) {
        directory = dirs[directory.0 as usize].parent;
    }
    dirs[directory.0 as usize].depth
}

impl HardLinks {
    /// Intern `directory`, a path relative to the scan root, so that links found in it can be
    /// charged with [`Self::charge_in`]. Resolve once per directory, not once per entry.
    ///
    /// Paths are compared component-wise, as [`Path`] equality is: `a/b`, `a/b/`, `a//b` and
    /// `a/./b` are one directory.
    pub fn directory(&mut self, directory: &Path) -> DirRef {
        let mut normalised = ::std::mem::take(&mut self.scratch);
        normalised.clear();
        normalised.extend(directory.components());
        let id = self.intern_normalised(&normalised);
        self.scratch = normalised;
        id
    }

    /// [`Self::directory`] for a path already in the normalised form `components()` yields.
    fn intern_normalised(&mut self, directory: &Path) -> DirRef {
        let key = directory.as_os_str();
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let parent = match directory.parent() {
            Some(parent) => self.intern_normalised(parent),
            None => ROOT,
        };
        let id = DirRef(u32::try_from(self.dirs.len()).expect("fewer than 2^32 directories"));
        let depth = self.dirs[parent.0 as usize].depth + 1;
        self.dirs.push(Dir { parent, depth });
        self.ids.insert(Box::from(key), id);
        id
    }

    /// Record a link to `inode`, of `size` bytes, found in `directory` (relative to the scan root).
    ///
    /// Returns the depth of the deepest folder that has already been charged for this file, so
    /// that the caller charges only the folders below it. `None` means no folder has been charged
    /// yet and the whole path should be.
    ///
    /// The answer is the longest prefix `directory` shares with *any* folder already holding a
    /// link, not with all of them at once: a folder is already charged exactly when it is an
    /// ancestor of some earlier link, and collapsing the earlier links into a single common
    /// ancestor would forget the deeper folders among them.
    pub fn charge(&mut self, shared: SharedBlocks, size: u64, directory: &Path) -> Option<usize> {
        let directory = self.directory(directory);
        self.charge_in(shared, size, directory)
    }

    /// [`Self::charge`] for a directory already interned with [`Self::directory`].
    ///
    /// # Panics
    ///
    /// If `directory` came from a different `HardLinks`, whose ids mean nothing here.
    pub fn charge_in(
        &mut self,
        shared: SharedBlocks,
        size: u64,
        directory: DirRef,
    ) -> Option<usize> {
        let Self {
            files,
            reflinks,
            dirs,
            ..
        } = self;
        let (seen_before, key) = match shared {
            SharedBlocks::Inode(inode) => (files, inode),
            SharedBlocks::Extent(physical) => (reflinks, physical),
        };
        match seen_before.entry(key) {
            Entry::Occupied(mut seen) => {
                let seen = seen.get_mut();
                if seen.size != size {
                    // Inode numbers are unique only within a filesystem, and a scan of `/` on
                    // macOS covers a volume group whose volumes number their inodes separately.
                    // Two different sizes cannot be the same file, so charge this one in full.
                    //
                    // For a reflink the same guard means something slightly different: two files
                    // that begin with the same physical extent but differ in length are only
                    // partly the same blocks, and counting them as one would understate. Charging
                    // in full is the conservative half of that trade.
                    return None;
                }
                seen.linked = true;
                if let Some(charged) = &mut seen.charged {
                    return Some(charge_ancestors(dirs, charged, directory) as usize);
                }
                let mut deepest = 0;
                for &existing in &seen.directories {
                    if existing == directory {
                        // This folder already holds a link, so it and everything above it have
                        // been charged.
                        return Some(dirs[directory.0 as usize].depth as usize);
                    }
                    deepest = deepest.max(shared_depth(dirs, existing, directory));
                }
                seen.directories.push(directory);
                if seen.directories.len() > INDEXED_AT {
                    let mut charged = FastSet::default();
                    for existing in ::std::mem::take(&mut seen.directories) {
                        charge_ancestors(dirs, &mut charged, existing);
                    }
                    seen.charged = Some(charged);
                }
                Some(deepest as usize)
            }
            Entry::Vacant(slot) => {
                slot.insert(LinkedFile {
                    size,
                    directories: vec![directory],
                    charged: None,
                    linked: false,
                });
                None
            }
        }
    }

    /// Number of distinct files the scan has seen under more than one name, for reporting and
    /// tests. A file whose other names lie outside the scan is not counted.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.files.values().filter(|file| file.linked).count()
    }

    /// Number of distinct reflinked files the scan has seen, counted once however many copies
    /// share their blocks.
    #[must_use]
    pub fn tracked_reflinks(&self) -> usize {
        self.reflinks.len()
    }
}
