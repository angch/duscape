use ::std::ffi::{OsStr, OsString};
use ::std::path::{Component, Path, PathBuf};

use crate::model::{
    BuildProfile, DirRef, FileOrFolder, FileToDelete, Folder, HardLinks, SizeKind, Sizes,
};
use ::std::sync::Arc;
use ::std::time::Instant;

use super::profile;

use crate::scan::{DirEntries, DirSummary, EntryMeta, NamedEntry, SharedBlocks};

/// How many leading path components `last` and `now` share, on their bytes: both are relative
/// paths in the form `components()` yields, so a common byte prefix that ends at a separator
/// (or at the end of one of them) is a common component prefix. Parsing components costs more
/// than the lookups it would save.
fn shared_components(last: &OsStr, now: &OsStr) -> usize {
    let (last, now) = (last.as_encoded_bytes(), now.as_encoded_bytes());
    let is_sep = |byte: u8| byte.is_ascii() && ::std::path::is_separator(byte as char);
    let mut common = last.iter().zip(now).take_while(|(a, b)| a == b).count();
    let at_boundary = |bytes: &[u8], at: usize| at == bytes.len() || is_sep(bytes[at]);
    while common > 0 && !(at_boundary(last, common) && at_boundary(now, common)) {
        common -= 1;
    }
    if common == 0 {
        return 0;
    }
    now[..common].iter().filter(|&&b| is_sep(b)).count() + 1
}

/// Shared-block sightings a tree has put off charging: one entry per directory that held any,
/// with the directory's ledger id, and each file's size on disk (which the ledger identifies
/// files by) and both of its sizes (which are taken back).
type Sightings = Vec<(DirRef, Vec<(SharedBlocks, u64, Sizes)>)>;

pub struct FileTree {
    pub current_folder_names: Vec<OsString>,
    /// Freed by deletes this session, for the title. Add to it with [`Self::note_freed`].
    pub space_freed: Sizes,
    /// Freed since this tree was scanned and the volume measured, for [`Self::outside_scan`]: a
    /// tree from a whole rescan starts at zero, though the session's total carries over.
    freed_since_scan: Sizes,
    /// Which size the totals below report. The tree holds both; this is only which is shown.
    pub shown: SizeKind,
    pub failed_to_read: u64,
    /// Bytes in use on the volume the scan covered, when it covered a whole volume in disk-usage
    /// mode and stayed on it, so that the two are comparable. See [`Self::outside_scan`].
    pub volume_used: Option<u128>,
    pub path_in_filesystem: PathBuf,
    base_folder: Folder,
    hard_links: HardLinks,
    /// Reused between calls: how much size to add at each depth from the base folder down.
    size_at_depth: Vec<Sizes>,
    /// Reused between calls: the way to the directory being added, by position at each level.
    positions: Vec<usize>,
    /// The directory added last, relative to the root: the next one usually shares most of its
    /// path, and `positions` is trusted that far.
    last_dir: PathBuf,
    /// When set, shared blocks are counted in full and noted here instead of being charged, so
    /// that several trees built in parallel can be merged and then reconciled once. `None` is the
    /// ordinary tree, which charges as it goes.
    deferred: Option<Sightings>,
    /// Where the build's time went, kept only when [`profile::enable`] was called first.
    profile: Option<Box<BuildProfile>>,
}

impl FileTree {
    pub fn new(mut base_folder: Folder, path_in_filesystem: PathBuf) -> Self {
        let path_in_filesystem = path_in_filesystem
            .canonicalize()
            .unwrap_or(path_in_filesystem);
        base_folder.dir = DirRef::ROOT;
        FileTree {
            base_folder,
            current_folder_names: Vec::new(),
            path_in_filesystem,
            space_freed: Sizes::ZERO,
            freed_since_scan: Sizes::ZERO,
            shown: SizeKind::Disk,
            failed_to_read: 0,
            volume_used: None,
            hard_links: HardLinks::default(),
            size_at_depth: Vec::new(),
            positions: Vec::new(),
            last_dir: PathBuf::new(),
            deferred: None,
            profile: profile::enabled().then(Box::default),
        }
    }

    /// A tree that counts shared blocks in full and remembers where it saw them, for building in
    /// parallel. Call [`Self::replay_deferred`] on the merged result to make the sizes right.
    pub fn deferring_shared_blocks(base_folder: Folder, path_in_filesystem: PathBuf) -> Self {
        let mut tree = Self::new(base_folder, path_in_filesystem);
        tree.deferred = Some(Vec::new());
        tree
    }

    /// Fold another tree of the same scan root into this one.
    ///
    /// The other tree's folders add to this one's, and its deferred sightings — if either side
    /// has any — are carried over so that one `replay_deferred` on the result settles everything.
    pub fn merge_from(&mut self, other: FileTree) {
        // The other tree's folders get ids in this tree's ledger as they come in; `remap` says
        // what each of their old ids became, so their sightings can follow.
        let mut remap = vec![DirRef::NONE; other.hard_links.directories()];
        self.base_folder.merge_into(
            other.base_folder,
            DirRef::NONE,
            &mut self.hard_links,
            &mut remap,
        );
        self.failed_to_read += other.failed_to_read;
        if let Some(theirs) = other.deferred {
            let mine = self.deferred.get_or_insert_with(Vec::new);
            mine.reserve(theirs.len());
            for (dir, shared) in theirs {
                let dir = remap.get(dir.0 as usize).copied().unwrap_or(DirRef::NONE);
                debug_assert_ne!(
                    dir,
                    DirRef::NONE,
                    "a sighting in a folder the merge never saw"
                );
                if dir != DirRef::NONE {
                    mine.push((dir, shared));
                }
            }
        }
        if let (Some(mine), Some(theirs)) = (&mut self.profile, &other.profile) {
            mine.merge(theirs);
        }
    }

    /// The build's profile, with the process-wide counters folded in, if profiling was on when
    /// this tree was made. Take it once the tree is finished: merged, replayed, refined.
    pub fn take_profile(&mut self) -> Option<BuildProfile> {
        let mut profile = self.profile.take()?;
        profile.collect_globals();
        Some(*profile)
    }

    /// Charge every shared block this tree deferred, and take back what was over-counted.
    ///
    /// During a deferred build each shared entry was added in full to every ancestor. Charging it
    /// now says which of those ancestors — from the root down to some depth — had already counted
    /// the same blocks through another path; those give the size back. The ledger's answers are
    /// order-independent, so replaying in shard order rather than arrival order lands on the same
    /// per-folder sizes an ordinary tree would have. Afterwards the tree is an ordinary tree.
    pub fn replay_deferred(&mut self) {
        let Some(sightings) = self.deferred.take() else {
            return;
        };
        let started = self.profile.as_ref().map(|_| Instant::now());
        let (mut replayed, mut taken_back, mut directories) = (0u64, 0u64, 0u64);
        // What each folder, by ledger id, has to give back: added up here, taken back from the
        // tree in one pass at the end rather than by walking down to every folder for every
        // sighting.
        let mut amounts = vec![Sizes::ZERO; self.hard_links.directories()];
        for (dir, shared) in sightings {
            directories += 1;
            let depth = self.hard_links.depth_of(dir);
            for (blocks, disk, sizes) in shared {
                replayed += 1;
                if let Some(charged) = self.hard_links.charge_in(blocks, disk, dir) {
                    taken_back += 1;
                    // The root and the ancestors down to depth `charged` had already counted
                    // these blocks through another path.
                    amounts[DirRef::ROOT.0 as usize] += sizes;
                    for ancestor in &self.hard_links.ancestors(dir)[..charged.min(depth)] {
                        amounts[ancestor.0 as usize] += sizes;
                    }
                }
            }
        }
        let charged = started.map(|started| started.elapsed());
        self.base_folder.take_back(&amounts);
        if let (Some(profile), Some(started), Some(charged)) = (&mut self.profile, started, charged)
        {
            profile.replay += started.elapsed();
            profile.replay_charge += charged;
            profile.replay_take_back += started.elapsed().saturating_sub(charged);
            profile.replay_sightings += replayed;
            profile.replay_taken_back += taken_back;
            profile.replay_directories += directories;
        }
    }
    /// Fold in what the second pass found (the second pass, `diskonaut_scan::refine`): small files counted in full
    /// by the walk whose blocks turn out to be held elsewhere in the tree too. Returns how many
    /// were taken back from some folder — zero when nothing on screen can have changed, as for
    /// the first sighting of any set of blocks, which stays counted where it is.
    ///
    /// A file that is gone from the tree, or has changed size, since it was noted — deleted in the
    /// meantime, or rescanned — is left alone, so each finding is applied to the file it is about
    /// or not at all. Each must be applied once: a second time would take a file's size back
    /// twice.
    pub fn apply_found(&mut self, found: &[crate::scan::Found]) -> usize {
        let mut charged_files = 0;
        for directory in found {
            let Ok(relative) = directory.dir.strip_prefix(&self.path_in_filesystem) else {
                continue;
            };
            let names: Vec<OsString> = relative
                .components()
                .map(|component| component.as_os_str().to_os_string())
                .collect();
            let folder = if names.is_empty() {
                Some(&self.base_folder)
            } else {
                match self.base_folder.path(names.clone()) {
                    Some(FileOrFolder::Folder(folder)) => Some(&**folder),
                    _ => None,
                }
            };
            let Some(folder) = folder else {
                continue;
            };
            let present: Vec<&crate::scan::FoundFile> = directory
                .files
                .iter()
                .filter(|found| {
                    matches!(folder.contents.get(&found.name), Some(FileOrFolder::File(file)) if file.sizes() == found.sizes)
                })
                .collect();
            if present.is_empty() {
                continue;
            }
            let depth = names.len();
            // The folder exists (just found), so this makes nothing: it only gives the folders
            // on the way their ids, where they have none yet.
            let mut positions = ::std::mem::take(&mut self.positions);
            let dir_ref = self.base_folder.resolve_path(
                names.iter().map(OsString::as_os_str),
                &mut self.hard_links,
                &mut positions,
            );
            self.positions = positions;
            for found in present {
                let disk = u64::try_from(found.sizes.disk).unwrap_or(u64::MAX);
                if let Some(charged) =
                    self.hard_links
                        .charge_in(SharedBlocks::Extent(found.identity), disk, dir_ref)
                {
                    charged_files += 1;
                    self.base_folder.subtract_along(
                        names.iter().map(OsString::as_os_str),
                        charged.min(depth),
                        found.sizes,
                    );
                }
            }
        }
        charged_files
    }
    /// The whole tree's size, of the kind [`Self::shown`].
    pub fn get_total_size(&self) -> u128 {
        self.base_folder.sizes.get(self.shown)
    }
    /// The whole tree's sizes, of both kinds.
    pub fn total_sizes(&self) -> Sizes {
        self.base_folder.sizes
    }
    pub fn get_total_descendants(&self) -> u64 {
        self.base_folder.num_descendants
    }
    pub fn get_current_folder(&self) -> &Folder {
        if self.current_folder_names.is_empty() {
            &self.base_folder
        } else if let Some(FileOrFolder::Folder(current_folder)) =
            self.base_folder.path(self.current_folder_names.clone())
        {
            current_folder
        } else {
            // here we have something in current_folder_names but the last
            // one is somehow not a folder... this is a corrupted state
            unreachable!("couldn't find current folder size")
        }
    }
    pub fn get_current_folder_size(&self) -> u128 {
        self.get_current_folder().sizes.get(self.shown)
    }
    pub fn get_current_path(&self) -> PathBuf {
        let mut full_path = PathBuf::from(&self.path_in_filesystem);
        for folder in &self.current_folder_names {
            full_path.push(folder)
        }
        full_path
    }
    pub fn item_in_current_folder(&self, item_name: &OsStr) -> Option<&FileOrFolder> {
        let current_folder = &self.get_current_folder();
        current_folder.path(vec![item_name.to_os_string()])
    }
    pub fn enter_folder(&mut self, folder_name: &OsStr) {
        self.current_folder_names.push(folder_name.to_os_string());
    }
    pub fn leave_folder(&mut self) -> bool {
        // true => succeeded, false => at base folder
        self.current_folder_names.pop().is_some()
    }
    /// Count `sizes` as freed by a delete.
    pub fn note_freed(&mut self, sizes: Sizes) {
        self.space_freed += sizes;
        self.freed_since_scan += sizes;
    }
    pub fn delete_file(&mut self, file_to_delete: &FileToDelete) {
        let path_to_delete = &file_to_delete.path_to_file;
        self.base_folder.delete_path(path_to_delete);
    }
    /// Space in use on the volume that the scan did not find: folders it could not read, and what
    /// no directory lists — filesystem metadata, shadow copies, snapshots.
    ///
    /// `None` unless [`Self::volume_used`] was recorded, and zero when the scan found as much as
    /// the volume reports. Deleting a file moves its size into `space_freed`, so the figure holds
    /// still as files are deleted rather than growing by what was freed.
    ///
    /// Freed means freed since this tree's scan: a tree from a whole rescan measured the volume
    /// after those deletes, and does not hold the files either.
    ///
    /// Only while sizes on disk are shown: the volume's figure is blocks, and lengths are not
    /// comparable with it.
    pub fn outside_scan(&self) -> Option<u128> {
        if self.shown != SizeKind::Disk {
            return None;
        }
        let found = self.base_folder.sizes.disk + self.freed_since_scan.disk;
        self.volume_used.map(|used| used.saturating_sub(found))
    }
    /// How many distinct files the scan has seen under more than one name.
    pub fn hard_linked_files(&self) -> usize {
        self.hard_links.tracked()
    }
    /// How many distinct reflinked files the scan has seen, counted once per set of shared blocks.
    pub fn reflinked_files(&self) -> usize {
        self.hard_links.tracked_reflinks()
    }
    /// Add a directory's outline — its subfolders and its files' total — without its files.
    ///
    /// This is the live view while the real tree is built on other threads: cheap enough to run
    /// on the rendering thread for every directory as it is scanned, and enough to show every
    /// folder with a running size. Files appear when the finished tree takes this one's place.
    /// Shared blocks are counted in full here, so sizes can run a little high until then.
    pub fn add_summary(&mut self, summary: DirSummary) {
        let DirSummary {
            dirs,
            files_size,
            files_apparent,
            entries,
        } = summary;
        let (dir_path, names, dir_entries) = dirs.into_parts();
        let Ok(relative) = dir_path.strip_prefix(&self.path_in_filesystem) else {
            return;
        };
        let depth = relative.components().count();
        let file_count = entries.saturating_sub(dir_entries.len() as u64);
        self.size_at_depth.clear();
        self.size_at_depth
            .resize(depth + 1, Sizes::of(files_size, files_apparent));
        self.base_folder.add_dir_entries(
            relative.components().map(Component::as_os_str),
            names,
            dir_entries,
            &self.size_at_depth,
            file_count,
        );
    }

    /// Carry over where `other` had navigated to, if that folder exists here; back to the root
    /// if it does not.
    pub fn adopt_navigation_from(&mut self, other: &FileTree) {
        let names = other.current_folder_names.clone();
        let exists = names.is_empty()
            || matches!(
                self.base_folder.path(names.clone()),
                Some(FileOrFolder::Folder(_))
            );
        self.current_folder_names = if exists { names } else { Vec::new() };
        self.space_freed = other.space_freed;
        self.shown = other.shown;
    }

    /// Put a fresh scan of the folder at `relative` — its path from the scan root — in place of
    /// what the tree held for it, and correct every ancestor's size and count by the difference.
    /// Returns what the tree held there, for the caller to dispose of — dropping a large folder
    /// is a walk of everything in it — or `None`, changing nothing, when there is no folder at
    /// `relative` any more.
    ///
    /// An empty `relative` is the whole tree: `rescanned` takes this one's place, keeping where
    /// the user is (when that folder still exists) and what they freed.
    ///
    /// Shared blocks are reconciled only within `rescanned`: a file hard-linked both inside and
    /// outside the folder was charged once to the common ancestors before, and after a graft is
    /// charged to them through both, until the whole tree is scanned again.
    ///
    /// Where the user is navigated to may no longer exist; see [`Self::current_folder_exists`].
    pub fn graft(&mut self, relative: &[OsString], rescanned: FileTree) -> Option<Folder> {
        if relative.is_empty() {
            let mut rescanned = rescanned;
            rescanned.adopt_navigation_from(self);
            return Some(::std::mem::replace(self, rescanned).base_folder);
        }
        let FileTree {
            base_folder: mut new_folder,
            hard_links: new_ledger,
            ..
        } = rescanned;
        let (old_size, old_descendants) = match self.base_folder.path(relative.to_vec())? {
            FileOrFolder::Folder(folder) => (folder.sizes, folder.num_descendants),
            FileOrFolder::File(_) => return None,
        };
        let (new_size, new_descendants) = (new_folder.sizes, new_folder.num_descendants);
        let resize = |folder: &mut Folder| {
            folder.sizes = folder.sizes.saturating_sub(old_size) + new_size;
            folder.num_descendants =
                folder.num_descendants.saturating_sub(old_descendants) + new_descendants;
        };
        let ledger = &mut self.hard_links;
        let mut folder = &mut self.base_folder;
        resize(folder);
        let mut parent = folder.dir;
        let (last, parents) = relative.split_last().expect("not empty");
        for name in parents {
            folder = match folder.contents.get_mut(name) {
                Some(FileOrFolder::Folder(next)) => next,
                _ => unreachable!("the path was just found"),
            };
            resize(folder);
            if folder.dir == DirRef::NONE {
                folder.dir = ledger.child(parent);
            }
            parent = folder.dir;
        }
        // The grafted folders' ids were the rescan's own; they get this tree's, under the parent.
        let mut remap = vec![DirRef::NONE; new_ledger.directories()];
        new_folder.renumber(parent, ledger, &mut remap);
        match folder.contents.get_mut(last) {
            Some(FileOrFolder::Folder(target)) => {
                Some(::std::mem::replace(&mut **target, new_folder))
            }
            _ => unreachable!("the path was just found"),
        }
    }

    /// Take the entry at `relative` out of the tree, as a delete would, when it has gone from
    /// disk. Returns whether there was one to take out.
    pub fn remove_path(&mut self, relative: &[OsString]) -> bool {
        if relative.is_empty() || self.base_folder.path(relative.to_vec()).is_none() {
            return false;
        }
        self.base_folder.delete_path(relative);
        true
    }

    /// Whether the folder the user is in is still in the tree, which a graft or a removal can
    /// take away from under them.
    pub fn current_folder_exists(&self) -> bool {
        self.current_folder_names.is_empty()
            || matches!(
                self.base_folder.path(self.current_folder_names.clone()),
                Some(FileOrFolder::Folder(_))
            )
    }

    /// Add every entry of one directory at once.
    ///
    /// Resolving `dir_path` is O(depth), and doing it once for the whole directory rather than
    /// once per entry is what keeps tree building off the critical path of a fast walk.
    pub fn add_dir_entries(&mut self, directory: DirEntries) {
        let (dir_path, names, entries) = directory.into_parts();
        // A directory from outside the scanned tree has no place in it. Silently folding such a
        // path into the base folder, as skipping a component count would, invents entries.
        let Ok(relative) = dir_path.strip_prefix(&self.path_in_filesystem) else {
            return;
        };
        self.add_relative_dir_entries(relative, names, entries);
    }
    pub fn add_entry(&mut self, meta: EntryMeta, entry_full_path: &Path) {
        let Ok(relative) = entry_full_path.strip_prefix(&self.path_in_filesystem) else {
            return;
        };
        let (Some(name), Some(parent)) = (relative.file_name(), relative.parent()) else {
            // The scan root itself, which is not an entry inside the tree.
            return;
        };
        let mut single = DirEntries::new(Arc::from(parent));
        single.push(name, meta);
        let (_, names, entries) = single.into_parts();
        self.add_relative_dir_entries(parent, names, entries);
    }
    /// Add one directory's entries, given that directory's path relative to the scan root.
    ///
    /// A hard-linked file is charged only to the folders that have not already counted it, which
    /// is what makes each folder's size the space actually held beneath it rather than the sum of
    /// its entries. See [`HardLinks`].
    fn add_relative_dir_entries(
        &mut self,
        relative_dir: &Path,
        names: Vec<u8>,
        entries: Vec<NamedEntry>,
    ) {
        let depth = relative_dir.components().count();
        let Self {
            base_folder,
            hard_links,
            size_at_depth,
            positions,
            last_dir,
            deferred,
            profile,
            ..
        } = self;
        let started = profile.as_ref().map(|_| Instant::now());

        // The folder first, which also gives it (and any folder on the way) its ledger id; the
        // sizes follow the same way by position once they are known. As far as this directory's
        // path runs with the last one's, the positions found then are checked rather than the
        // names looked up.
        let shared = shared_components(last_dir.as_os_str(), relative_dir.as_os_str());
        let this_dir = base_folder.resolve_path_from(
            relative_dir.components().map(Component::as_os_str),
            hard_links,
            positions,
            shared,
        );
        last_dir.clear();
        last_dir.push(relative_dir);
        let resolved = started.map(|_| Instant::now());

        size_at_depth.clear();
        size_at_depth.resize(depth + 1, Sizes::ZERO);
        let mut normal_size = Sizes::ZERO;
        let mut noted: Option<Vec<(SharedBlocks, u64, Sizes)>> = None;
        let mut sightings = 0u64;
        for entry in &entries {
            if entry.meta.is_dir {
                continue;
            }
            // Both kinds go the same way: which folders a shared file is charged to depends on
            // where else its blocks are, not on how they are measured.
            let size = Sizes::of(entry.meta.size, entry.meta.apparent);
            match entry.meta.shared_blocks() {
                Some(shared) if deferred.is_some() => {
                    sightings += 1;
                    // Counted in full for now like any other file; the sighting is kept so that
                    // `replay_deferred` can take back whatever turns out to be counted twice.
                    normal_size += size;
                    noted
                        .get_or_insert_with(Vec::new)
                        .push((shared, entry.meta.size, size));
                }
                Some(shared) => {
                    sightings += 1;
                    let charged_down_to = hard_links.charge_in(shared, entry.meta.size, this_dir);
                    let first_uncharged = charged_down_to.map_or(0, |charged| charged + 1);
                    for folder_size in &mut size_at_depth[first_uncharged.min(depth + 1)..] {
                        *folder_size += size;
                    }
                }
                None => normal_size += size,
            }
        }
        if let (Some(deferred), Some(noted)) = (deferred.as_mut(), noted) {
            deferred.push((this_dir, noted));
        }
        if !normal_size.is_zero() {
            for folder_size in &mut size_at_depth[..] {
                *folder_size += normal_size;
            }
        }
        let ledgered = started.map(|_| Instant::now());

        let count = entries.len() as u64;
        let folder = base_folder.descend(positions, size_at_depth, count);
        folder.place_entries(names, entries);

        if let (Some(profile), Some(started), Some(resolved), Some(ledgered)) =
            (profile, started, resolved, ledgered)
        {
            profile.directories += 1;
            profile.entries += count;
            profile.sightings += sightings;
            profile.resolve_steps += depth as u64;
            profile.resolve += resolved.duration_since(started);
            if sightings == 0 {
                profile.sizes += ledgered.duration_since(resolved);
            } else {
                profile.ledger_directories += 1;
                profile.ledger += ledgered.duration_since(resolved);
            }
            profile.place += ledgered.elapsed();
        }
    }
}
