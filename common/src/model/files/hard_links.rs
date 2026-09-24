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
pub struct DirRef(pub(crate) u32);

impl DirRef {
    /// No directory: a folder the ledger has not been told about yet.
    pub const NONE: DirRef = DirRef(u32::MAX);
    /// The scan root.
    pub const ROOT: DirRef = DirRef(0);
}

struct Dir {
    parent: DirRef,
    depth: u32,
    /// Where this directory's ancestors start in `HardLinks::chains`: `depth` of them, from the
    /// root's child down to this directory itself. The root has none.
    chain: u32,
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
    /// Every interned directory's ancestors, end to end; see `Dir::chain`.
    chains: Vec<DirRef>,
    /// Normalised relative directory path to its interned id.
    ids: FastMap<Box<OsStr>, DirRef>,
    /// Reused between calls to hold the normalised form of the path being interned.
    scratch: PathBuf,
}

const ROOT: DirRef = DirRef::ROOT;

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
                chain: 0,
            }],
            chains: Vec::new(),
            ids,
            scratch: PathBuf::new(),
        }
    }
}

/// Depth of the deepest folder that contains both directories: how many leading components
/// their paths share.
///
/// Each directory's ancestors are kept as an array, so this is the length of the two arrays'
/// common prefix — a few adjacent words compared, and for two directories that part company
/// near the root (two snapshots of the same tree, say) one or two. Walking up from each by
/// parent pointers until they met cost about 18 dependent loads a comparison, and with a file
/// linked from several folders compared against each of them, 126 loads a sighting: half of a
/// tree build with 734k sightings went there.
fn shared_depth(dirs: &[Dir], chains: &[DirRef], left: DirRef, right: DirRef) -> u32 {
    let chain = |dir: DirRef| {
        let dir = &dirs[dir.0 as usize];
        &chains[dir.chain as usize..dir.chain as usize + dir.depth as usize]
    };
    let shared = chain(left)
        .iter()
        .zip(chain(right))
        .take_while(|(a, b)| a == b)
        .count();
    super::profile::count(&super::profile::ANCESTOR_STEPS, shared as u64 + 1);
    shared as u32
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
        super::profile::count(&super::profile::ANCESTOR_STEPS, 1);
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

    /// How deep `directory` is below the scan root, which is depth 0.
    #[must_use]
    pub fn depth_of(&self, directory: DirRef) -> usize {
        self.dirs[directory.0 as usize].depth as usize
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
        let id = self.child(parent);
        self.ids.insert(Box::from(key), id);
        id
    }

    /// A new directory directly under `parent`, which is how the tree names its folders to the
    /// ledger: no path, no hashing — the folder keeps the id it is given and hands it back with
    /// every link found in it. ([`Self::directory`] is the same by path, for callers that have
    /// only that.)
    pub fn child(&mut self, parent: DirRef) -> DirRef {
        debug_assert_ne!(parent, DirRef::NONE, "a child of no directory");
        let id = DirRef(u32::try_from(self.dirs.len()).expect("fewer than 2^32 directories"));
        let above = &self.dirs[parent.0 as usize];
        let depth = above.depth + 1;
        let chain = u32::try_from(self.chains.len()).expect("ancestor chains fit in 4 Gi entries");
        let parent_chain = above.chain as usize..above.chain as usize + above.depth as usize;
        self.chains.extend_from_within(parent_chain);
        self.chains.push(id);
        self.dirs.push(Dir {
            parent,
            depth,
            chain,
        });
        id
    }

    /// `directory`'s ancestors below the root, from the root's child down to `directory` itself:
    /// the one at index `i` is at depth `i + 1`.
    #[must_use]
    pub fn ancestors(&self, directory: DirRef) -> &[DirRef] {
        let dir = &self.dirs[directory.0 as usize];
        &self.chains[dir.chain as usize..dir.chain as usize + dir.depth as usize]
    }

    /// How many directories the ledger knows: one more than the largest id it has given out.
    #[must_use]
    pub fn directories(&self) -> usize {
        self.dirs.len()
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
            chains,
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
                super::profile::count(
                    &super::profile::LINK_COMPARISONS,
                    seen.directories.len() as u64,
                );
                for &existing in &seen.directories {
                    if existing == directory {
                        // This folder already holds a link, so it and everything above it have
                        // been charged.
                        return Some(dirs[directory.0 as usize].depth as usize);
                    }
                    deepest = deepest.max(shared_depth(dirs, chains, existing, directory));
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
