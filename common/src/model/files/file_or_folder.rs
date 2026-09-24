use ::std::collections::VecDeque;
use ::std::ffi::{OsStr, OsString};
use ::std::path::{Path, PathBuf};

use crate::scan::{EntryMeta, NamedEntry};

use super::{DirRef, HardLinks};

/// What a folder holds, by name.
///
/// A `Folder` is boxed so that the far more numerous files do not each pay for a folder's size:
/// the map slot for a file is a name and a size rather than a name and an entire folder.
pub type ContentsMap = super::Contents;

#[derive(Debug, Clone)]
pub enum FileOrFolder {
    Folder(Box<Folder>),
    File(File),
}

impl FileOrFolder {
    /// Both of its sizes.
    pub fn sizes(&self) -> Sizes {
        match self {
            FileOrFolder::Folder(folder) => folder.sizes,
            FileOrFolder::File(file) => file.sizes(),
        }
    }
    /// The size of the kind asked for.
    pub fn size(&self, kind: SizeKind) -> u128 {
        self.sizes().get(kind)
    }
}

/// Which of an entry's two sizes is meant: the space it takes on disk (blocks allocated), or its
/// length (`du --apparent-size`, `-a`). The tree keeps both, so the view can switch between them
/// without scanning again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SizeKind {
    #[default]
    Disk,
    Apparent,
}

impl SizeKind {
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            SizeKind::Disk => SizeKind::Apparent,
            SizeKind::Apparent => SizeKind::Disk,
        }
    }
}

/// An amount of both kinds, as a folder's total or a change to one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sizes {
    pub disk: u128,
    pub apparent: u128,
}

impl Sizes {
    pub const ZERO: Sizes = Sizes {
        disk: 0,
        apparent: 0,
    };
    #[must_use]
    pub fn new(disk: u128, apparent: u128) -> Self {
        Self { disk, apparent }
    }
    #[must_use]
    pub fn of(disk: u64, apparent: u64) -> Self {
        Self::new(u128::from(disk), u128::from(apparent))
    }
    #[must_use]
    pub fn get(self, kind: SizeKind) -> u128 {
        match kind {
            SizeKind::Disk => self.disk,
            SizeKind::Apparent => self.apparent,
        }
    }
    #[must_use]
    pub fn saturating_sub(self, other: Sizes) -> Sizes {
        Sizes::new(
            self.disk.saturating_sub(other.disk),
            self.apparent.saturating_sub(other.apparent),
        )
    }
    pub fn is_zero(self) -> bool {
        self == Sizes::ZERO
    }
}

impl ::std::ops::Add for Sizes {
    type Output = Sizes;
    fn add(self, other: Sizes) -> Sizes {
        Sizes::new(self.disk + other.disk, self.apparent + other.apparent)
    }
}

impl ::std::ops::AddAssign for Sizes {
    fn add_assign(&mut self, other: Sizes) {
        *self = *self + other;
    }
}

impl ::std::iter::Sum for Sizes {
    fn sum<I: Iterator<Item = Sizes>>(iter: I) -> Sizes {
        iter.fold(Sizes::ZERO, |sum, sizes| sum + sizes)
    }
}

/// A file, as the tree holds it: its size on disk, and its length as a difference from that.
///
/// This is the single most repeated value in the model — one per file, millions of them — and it
/// decides the size of `FileOrFolder`, which is what every slot in every folder costs: 16 bytes,
/// tested in `model::tests`. A second `u64` would make it 24. But a file's two sizes are nearly
/// always within a block of each other, so the length is kept as a signed 32-bit difference from
/// the size on disk, in bytes, which fits beside the `u64` in the space the enum's tag leaves.
///
/// The few files whose sizes differ by 2 GiB or more — a huge sparse file (`/var/log/lastlog`),
/// a large file that compresses well — keep both sizes in a box of their own instead, so both are
/// exact for every file.
#[derive(Debug, Clone)]
pub struct File(FileSizes);

#[derive(Debug, Clone)]
enum FileSizes {
    Near { disk: u64, apparent_minus_disk: i32 },
    Far(Box<(u64, u64)>),
}

impl File {
    #[must_use]
    pub fn new(disk: u64, apparent: u64) -> Self {
        let difference = i128::from(apparent) - i128::from(disk);
        File(match i32::try_from(difference) {
            Ok(apparent_minus_disk) => FileSizes::Near {
                disk,
                apparent_minus_disk,
            },
            Err(_) => FileSizes::Far(Box::new((disk, apparent))),
        })
    }
    /// A file whose two sizes agree, as in tests that do not care which is shown.
    #[must_use]
    pub fn sized(size: u64) -> Self {
        Self::new(size, size)
    }
    #[must_use]
    pub fn disk(&self) -> u64 {
        match &self.0 {
            FileSizes::Near { disk, .. } => *disk,
            FileSizes::Far(both) => both.0,
        }
    }
    #[must_use]
    pub fn apparent(&self) -> u64 {
        match &self.0 {
            FileSizes::Near {
                disk,
                apparent_minus_disk,
            } => disk.saturating_add_signed(i64::from(*apparent_minus_disk)),
            FileSizes::Far(both) => both.1,
        }
    }
    #[must_use]
    pub fn sizes(&self) -> Sizes {
        Sizes::of(self.disk(), self.apparent())
    }
}

/// A directory in the tree.
///
/// It does not store its own name. The name is the key it is filed under in its parent, so a copy
/// here would be a second one — and nothing ever read it. On a whole-volume scan that was 385k
/// `OsString`s allocated to be written once and never looked at.
#[derive(Debug, Clone)]
pub struct Folder {
    pub contents: ContentsMap,
    pub sizes: Sizes,
    pub num_descendants: u64,
    /// This folder in the tree's hard-link ledger, given when a directory's entries are first
    /// resolved to it, or [`DirRef::NONE`] until then. Links found in it are charged under this
    /// id, so a folder never has to be named to the ledger by path.
    pub dir: DirRef,
}
impl Default for Folder {
    fn default() -> Self {
        Self {
            contents: ContentsMap::default(),
            sizes: Sizes::ZERO,
            num_descendants: 0,
            dir: DirRef::NONE,
        }
    }
}
impl Folder {
    /// The scan root's folder. The path is not kept: a folder's name lives in its parent, and the
    /// root's is held by [`crate::FileTree`].
    pub fn new(_path: &Path) -> Self {
        Self::default()
    }

    /// Insert an entry addressed by its path components relative to this folder.
    pub fn add_entry(
        &mut self,
        meta: EntryMeta,
        relative_path: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) {
        let mut components = relative_path.into_iter().peekable();
        let mut folder = self;
        while let Some(name) = components.next() {
            let name = name.as_ref();
            folder.sizes += Sizes::of(meta.size, meta.apparent);
            folder.num_descendants += 1;
            if components.peek().is_some() {
                folder = folder.contents.folder_or_insert(name);
            } else if meta.is_dir {
                // A directory can already exist here if one of its children was reported first.
                folder
                    .contents
                    .insert_if_absent(name, || FileOrFolder::Folder(Box::default()));
            } else {
                folder.contents.insert(
                    name,
                    FileOrFolder::File(File::new(meta.size, meta.apparent)),
                );
            }
        }
    }

    /// Add every entry of one directory, given that directory's path relative to this folder.
    ///
    /// `size_at_depth[k]` is added to the folder `k` levels down, so that a hard-linked file can
    /// be charged to some ancestors and not others. For a tree without hard links every element
    /// is the same total. See [`crate::model::HardLinks`].
    ///
    /// `extra_descendants` are counted along the path as well as the entries themselves: the live
    /// outline places a directory's subfolders but not its files, and still wants the files in
    /// every ancestor's count.
    pub fn add_dir_entries<'a>(
        &mut self,
        dir_path: impl Iterator<Item = &'a OsStr>,
        names: Vec<u8>,
        entries: Vec<NamedEntry>,
        size_at_depth: &[Sizes],
        extra_descendants: u64,
    ) {
        let contained_count = entries.len() as u64 + extra_descendants;
        let (folder, _) = self.resolve_dir(dir_path, size_at_depth, contained_count);
        folder.place_entries(names, entries);
    }

    /// The first half of [`Self::add_dir_entries`]: find or make the folder at `dir_path`, adding
    /// `size_at_depth` and `contained_count` to it and to every folder on the way. Returns the
    /// folder and how many folders were stepped through (the path's depth).
    pub fn resolve_dir<'a>(
        &mut self,
        dir_path: impl Iterator<Item = &'a OsStr>,
        size_at_depth: &[Sizes],
        contained_count: u64,
    ) -> (&mut Self, u64) {
        let size_at = |depth: usize| size_at_depth.get(depth).copied().unwrap_or(Sizes::ZERO);
        let mut steps = 0;
        let mut folder = self;
        folder.sizes += size_at(0);
        folder.num_descendants += contained_count;
        for (depth, name) in dir_path.enumerate() {
            folder = folder.contents.folder_or_insert(name);
            folder.sizes += size_at(depth + 1);
            folder.num_descendants += contained_count;
            steps += 1;
        }
        (folder, steps)
    }

    /// Find or make the folder at `dir_path`, telling the ledger about every folder on the way
    /// that it does not know yet, and note where each step went in `positions` (emptied first)
    /// so that [`Self::descend`] can take the same way by index. Returns the folder's ledger id.
    /// This folder's own id must be set — the tree's root is [`DirRef::ROOT`].
    pub fn resolve_path<'a>(
        &mut self,
        dir_path: impl Iterator<Item = &'a OsStr>,
        ledger: &mut HardLinks,
        positions: &mut Vec<usize>,
    ) -> DirRef {
        positions.clear();
        let mut folder = self;
        let mut dir = folder.dir;
        for name in dir_path {
            let position = folder.contents.folder_position_or_insert(name);
            positions.push(position);
            folder = folder.contents.folder_at_mut(position);
            if folder.dir == DirRef::NONE {
                folder.dir = ledger.child(dir);
            }
            dir = folder.dir;
        }
        dir
    }

    /// Walk the way [`Self::resolve_path`] noted, adding `size_at_depth` and `contained_count`
    /// to this folder and each one on the way, and return the folder at its end.
    pub fn descend(
        &mut self,
        positions: &[usize],
        size_at_depth: &[Sizes],
        contained_count: u64,
    ) -> &mut Self {
        let size_at = |depth: usize| size_at_depth.get(depth).copied().unwrap_or(Sizes::ZERO);
        let mut folder = self;
        folder.sizes += size_at(0);
        folder.num_descendants += contained_count;
        for (depth, &position) in positions.iter().enumerate() {
            folder = folder.contents.folder_at_mut(position);
            folder.sizes += size_at(depth + 1);
            folder.num_descendants += contained_count;
        }
        folder
    }

    /// Give this folder and every folder below it ids in `ledger` under `parent`, recording in
    /// `remap` (indexed by old id) what each id it had before became. For a folder moved in
    /// whole from another tree, whose ids meant something only in that tree's ledger. A folder
    /// the other ledger never knew stays unknown.
    pub fn renumber(&mut self, parent: DirRef, ledger: &mut HardLinks, remap: &mut [DirRef]) {
        if self.dir != DirRef::NONE {
            // Under a folder the ledger does not know, nothing below can be known either.
            let new = if parent == DirRef::NONE {
                DirRef::NONE
            } else {
                ledger.child(parent)
            };
            if let Some(slot) = remap.get_mut(self.dir.0 as usize) {
                *slot = new;
            }
            self.dir = new;
        }
        let mine = self.dir;
        for folder in self.contents.folders_mut() {
            folder.renumber(mine, ledger, remap);
        }
    }

    /// Take `amounts[dir]` back from every folder with a ledger id, this one and below. The
    /// second half of deferred shared-block accounting, done in one pass once the replay has
    /// added up what each folder over-counted.
    pub fn take_back(&mut self, amounts: &[Sizes]) {
        if let Some(amount) = amounts.get(self.dir.0 as usize)
            && !amount.is_zero()
        {
            self.sizes = self.sizes.saturating_sub(*amount);
        }
        for folder in self.contents.folders_mut() {
            folder.take_back(amounts);
        }
    }

    /// The second half of [`Self::add_dir_entries`]: place a directory's packed entries in this
    /// folder.
    pub fn place_entries(&mut self, names: Vec<u8>, entries: Vec<NamedEntry>) {
        let folder = self;
        // The scan packed these names once; the folder takes that buffer rather than copying each
        // name out of it, so an entry's name is never allocated between the kernel and the tree.
        let shift = folder.contents.absorb_names(names);
        folder.contents.reserve(entries.len());
        for entry in entries {
            let range = entry.name_range();
            let offset = shift + u32::try_from(range.start).expect("names fit in 4 GiB");
            let len = u32::try_from(range.len()).expect("a name fits in 4 GiB");
            if entry.meta.is_dir {
                // The directory may already be here if its own contents were read first, which is
                // the only case that has to look before it writes.
                folder
                    .contents
                    .place(offset, len, FileOrFolder::Folder(Box::default()), true);
            } else {
                folder.contents.place(
                    offset,
                    len,
                    FileOrFolder::File(File::new(entry.meta.size, entry.meta.apparent)),
                    false,
                );
            }
        }
    }

    /// Fold another partial view of this same folder into this one.
    ///
    /// Sizes and counts add, because each side counted disjoint groups of entries; where both
    /// sides hold a folder of the same name, that folder is merged in turn.
    pub fn merge_from(&mut self, other: Folder) {
        let mut ledger = HardLinks::default();
        let mut remap = Vec::new();
        self.merge_into(other, DirRef::NONE, &mut ledger, &mut remap);
    }

    /// [`Self::merge_from`] with the ledger the merged tree charges to: `other`'s folders take
    /// this tree's ids — this folder's where both have one, new ones under it (or under
    /// `parent`, if this folder has none yet) where only `other` does — and `remap`, indexed by
    /// `other`'s old ids, says what each became, so that what `other` noted about them can be
    /// carried over.
    pub fn merge_into(
        &mut self,
        other: Folder,
        parent: DirRef,
        ledger: &mut HardLinks,
        remap: &mut [DirRef],
    ) {
        self.sizes += other.sizes;
        self.num_descendants += other.num_descendants;
        if other.dir != DirRef::NONE {
            if self.dir == DirRef::NONE && parent != DirRef::NONE {
                self.dir = ledger.child(parent);
            }
            if let Some(slot) = remap.get_mut(other.dir.0 as usize) {
                *slot = self.dir;
            }
        }
        self.contents
            .merge_from(other.contents, self.dir, ledger, remap);
    }

    /// Take `sizes` back from this folder and from the first `down_to` folders along `path`.
    ///
    /// This is the second half of deferred shared-block accounting: a file whose blocks had
    /// already been counted by some folder was nonetheless added in full to every ancestor during
    /// a parallel build, and the ancestors from the root down to the one that already held it
    /// have to give that back. Saturating for the same reason `delete_path` is.
    pub fn subtract_along<'a>(
        &mut self,
        mut path: impl Iterator<Item = &'a OsStr>,
        down_to: usize,
        sizes: Sizes,
    ) {
        let mut folder = self;
        folder.sizes = folder.sizes.saturating_sub(sizes);
        for _ in 0..down_to {
            let Some(name) = path.next() else {
                return;
            };
            folder = match folder.contents.get_mut(name) {
                Some(FileOrFolder::Folder(next)) => next,
                _ => return,
            };
            folder.sizes = folder.sizes.saturating_sub(sizes);
        }
    }

    pub fn add_folder(&mut self, path: PathBuf) {
        self.add_entry(
            EntryMeta {
                is_dir: true,
                ..EntryMeta::default()
            },
            path.components().map(|component| component.as_os_str()),
        );
    }
    pub fn add_file(&mut self, path: PathBuf, size: u128) {
        self.add_entry(
            EntryMeta {
                size: u64::try_from(size).unwrap_or(u64::MAX),
                apparent: u64::try_from(size).unwrap_or(u64::MAX),
                links: 1,
                is_dir: false,
                ..EntryMeta::default()
            },
            path.components().map(|component| component.as_os_str()),
        );
    }
    pub fn path(&self, mut folder_names: Vec<OsString>) -> Option<&FileOrFolder> {
        let next_folder_name = folder_names.remove(0);
        let next_in_path = &self.contents.get(&next_folder_name)?;
        if folder_names.is_empty() {
            Some(next_in_path)
        } else if let FileOrFolder::Folder(next_folder) = next_in_path {
            next_folder.path(folder_names)
        } else {
            Some(next_in_path)
        }
    }
    /// How much an item contributed to each of its ancestors' `num_descendants`.
    ///
    /// A folder counts its own contents plus itself: every ancestor was incremented once for the
    /// folder's entry and once for each entry inside it.
    fn delete_path_removed_descendants(item: &FileOrFolder) -> u64 {
        match item {
            FileOrFolder::Folder(folder) => folder.num_descendants + 1,
            FileOrFolder::File(_) => 1,
        }
    }

    pub fn delete_path(&mut self, folder_names: &[OsString]) {
        // TODO: there are some needless allocations here, this is not terrible since
        // the deletion itself takes an order of magnitude longer, but it can be nice
        // to reduce them
        let mut folders_to_traverse: VecDeque<OsString> = VecDeque::from(folder_names.to_owned());
        if folder_names.len() == 1 {
            let name = folder_names
                .last()
                .expect("could not find last item in path");
            let removed_size = &self
                .contents
                .get(name)
                .expect("could not find folder")
                .sizes();
            let removed_descendents = Self::delete_path_removed_descendants(
                self.contents.get(name).expect("could not find folder"),
            );
            // Saturating because a hard-linked file's size was charged to this folder only once
            // however many links it has here, so removing each link would otherwise underflow.
            self.sizes = self.sizes.saturating_sub(*removed_size);
            self.num_descendants = self.num_descendants.saturating_sub(removed_descendents);
            self.contents.remove(name);
        } else {
            let (removed_size, removed_descendents) = {
                let item_to_remove = self
                    .path(Vec::from(folders_to_traverse.clone()))
                    .expect("could not find item to delete");
                let removed_size = item_to_remove.sizes();
                (
                    removed_size,
                    Self::delete_path_removed_descendants(item_to_remove),
                )
            };
            let next_name = folders_to_traverse
                .pop_front()
                .expect("could not find next path folder");
            let next_item = &mut self
                .contents
                .get_mut(&next_name)
                .expect("could not find folder in path");
            match next_item {
                FileOrFolder::Folder(folder) => {
                    self.sizes = self.sizes.saturating_sub(removed_size);
                    self.num_descendants = self.num_descendants.saturating_sub(removed_descendents);
                    folder.delete_path(&Vec::from(folders_to_traverse));
                }
                FileOrFolder::File(_) => {
                    panic!("got a file in the middle of a path");
                }
            }
        }
    }
}
