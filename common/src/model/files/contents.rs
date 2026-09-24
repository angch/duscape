//! What a folder holds, with the names packed together.
//!
//! An `OsString` per entry costs 24 bytes wherever it is stored plus a heap block of its own. On a
//! 4.2M entry volume the mean name is 32 bytes, so the names alone were about 305 MB of a 654 MB
//! tree, in 4.2M separate allocations.
//!
//! Here a folder keeps one buffer holding all its entries' names end to end, and an entry refers
//! to its name by offset and length — two `u32`s instead of an `OsString`. The scan already hands
//! over a directory at a time with its names packed the same way, so in the common case where a
//! folder is new the buffer is **moved in rather than copied**, and no name is allocated anywhere
//! between the `getdents64` buffer and the tree.
//!
//! Lookup is a linear scan of that buffer. For the median directory — 3 entries, mean 11 — those
//! names are adjacent in cache and the comparison beats hashing one. The tail is what needs care:
//! resolving a directory's parent looks it up among its siblings, so a folder with tens of
//! thousands of subdirectories would be quadratic on scans alone. Above [`INDEX_ABOVE`] entries a
//! folder builds a name index and lookups go through it; the threshold is past the 99th percentile
//! of real directory sizes, so almost no folder ever allocates one.

use ::std::ffi::OsStr;

use super::hash::FastMap;
use super::{FileOrFolder, Folder};

/// Entries above which a folder builds a name index instead of scanning.
const INDEX_ABOVE: usize = 128;

/// One entry: where its name is in the folder's buffer, and what it is.
#[derive(Debug, Clone)]
struct Entry {
    offset: u32,
    len: u32,
    node: FileOrFolder,
}

/// A folder's entries, by name.
#[derive(Debug, Clone, Default)]
pub struct Contents {
    /// Every entry's name, concatenated in insertion order.
    names: Vec<u8>,
    entries: Vec<Entry>,
    /// Name to position in `entries`, built only once a folder is big enough to want one.
    index: Option<Box<FastMap<Box<[u8]>, u32>>>,
}

impl Contents {
    fn bytes_of(&self, entry: &Entry) -> &[u8] {
        let start = entry.offset as usize;
        &self.names[start..start + entry.len as usize]
    }

    fn position(&self, name: &OsStr) -> Option<usize> {
        let wanted = name.as_encoded_bytes();
        if let Some(index) = &self.index {
            return index.get(wanted).map(|&position| position as usize);
        }
        self.entries
            .iter()
            .position(|entry| self.bytes_of(entry) == wanted)
    }

    /// Build the index if this folder has grown past the point where scanning is cheaper.
    fn index_if_large(&mut self) {
        if self.index.is_some() || self.entries.len() <= INDEX_ABOVE {
            return;
        }
        let mut index: FastMap<Box<[u8]>, u32> = FastMap::default();
        index.reserve(self.entries.len());
        for (position, entry) in self.entries.iter().enumerate() {
            let start = entry.offset as usize;
            let name = &self.names[start..start + entry.len as usize];
            index.insert(Box::from(name), position as u32);
        }
        self.index = Some(Box::new(index));
    }

    /// Adopt a directory's packed names, returning the offset its entries must be shifted by.
    ///
    /// A folder is usually empty when its own contents arrive — the only entries it can already
    /// hold are subdirectories created while resolving a path through it — so the buffer is taken
    /// whole and nothing is copied.
    pub fn absorb_names(&mut self, names: Vec<u8>) -> u32 {
        if self.names.is_empty() {
            self.names = names;
            return 0;
        }
        let shift = u32::try_from(self.names.len()).expect("a folder's names fit in 4 GiB");
        self.names.extend_from_slice(&names);
        shift
    }

    /// Record an entry whose name is already in this folder's buffer at `offset..offset + len`.
    ///
    /// `absent_only` skips the entry if that name is already here, which is what a directory needs:
    /// it may have been created already while resolving a path through it.
    pub fn place(&mut self, offset: u32, len: u32, node: FileOrFolder, absent_only: bool) {
        if absent_only {
            let start = offset as usize;
            // SAFETY: The slice came from `OsStr::as_encoded_bytes()`.
            let name = unsafe {
                OsStr::from_encoded_bytes_unchecked(&self.names[start..start + len as usize])
            };
            if self.position(name).is_some() {
                return;
            }
        }
        if let Some(index) = &mut self.index {
            let start = offset as usize;
            let name: Box<[u8]> = Box::from(&self.names[start..start + len as usize]);
            index.insert(name, self.entries.len() as u32);
        }
        self.entries.push(Entry { offset, len, node });
        self.index_if_large();
    }

    /// The bytes at `offset..offset + len` in this folder's buffer.
    #[must_use]
    pub fn names_at(&self, offset: u32, len: u32) -> &[u8] {
        let start = offset as usize;
        &self.names[start..start + len as usize]
    }

    /// Append an entry, copying its name in. For the paths that hold a name and not a buffer.
    fn push(&mut self, name: &OsStr, node: FileOrFolder) {
        let bytes = name.as_encoded_bytes();
        let offset = u32::try_from(self.names.len()).expect("a folder's names fit in 4 GiB");
        let len = u32::try_from(bytes.len()).expect("a name fits in 4 GiB");
        self.names.extend_from_slice(bytes);
        self.place(offset, len, node, false);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn contains_key(&self, name: &OsStr) -> bool {
        self.position(name).is_some()
    }

    #[must_use]
    pub fn get(&self, name: &OsStr) -> Option<&FileOrFolder> {
        self.position(name)
            .map(|position| &self.entries[position].node)
    }

    pub fn get_mut(&mut self, name: &OsStr) -> Option<&mut FileOrFolder> {
        self.position(name)
            .map(|position| &mut self.entries[position].node)
    }

    /// Reserve room for `entries` more entries.
    pub fn reserve(&mut self, entries: usize) {
        self.entries.reserve(entries);
    }

    pub fn insert(&mut self, name: &OsStr, node: FileOrFolder) -> Option<FileOrFolder> {
        if let Some(position) = self.position(name) {
            return Some(::std::mem::replace(&mut self.entries[position].node, node));
        }
        self.push(name, node);
        None
    }

    /// The subfolder called `name`, created empty if there is none.
    ///
    /// One scan where an `insert_if_absent` followed by a `get_mut` took two. This is the lookup
    /// on the path walk that resolves every directory's parent — once per level, for every
    /// directory the scan reports — so it is the hottest thing the model does.
    pub fn folder_or_insert(&mut self, name: &OsStr) -> &mut Folder {
        let position = match self.position(name) {
            Some(position) => position,
            None => {
                self.push(name, FileOrFolder::Folder(Box::default()));
                self.entries.len() - 1
            }
        };
        match &mut self.entries[position].node {
            FileOrFolder::Folder(folder) => folder,
            FileOrFolder::File(_) => unreachable!("got a file in the middle of a path"),
        }
    }

    /// Insert only if nothing is there yet.
    pub fn insert_if_absent(&mut self, name: &OsStr, node: impl FnOnce() -> FileOrFolder) {
        if self.position(name).is_none() {
            self.push(name, node());
        }
    }

    pub fn remove(&mut self, name: &OsStr) -> Option<FileOrFolder> {
        let position = self.position(name)?;
        let removed = self.entries.swap_remove(position);
        // The name's bytes stay in the buffer. Reclaiming them would mean shifting every later
        // entry's offset, and removal happens one file at a time from the UI, so the garbage is
        // bounded by what a person deletes by hand.
        if self.index.is_some() {
            self.index = None;
            self.index_if_large();
        }
        Some(removed.node)
    }

    /// Take everything `other` holds into this folder.
    ///
    /// A folder that is still empty just takes `other`'s buffers whole, which is the common case
    /// when several partial trees are merged: a directory's own entries all arrive in one group,
    /// so they land in one shard and every other shard sees that folder empty or not at all.
    /// Otherwise `other`'s names are appended and each entry re-placed, and a folder entry that
    /// already exists here is merged into the existing one rather than duplicated — that is the
    /// shared-ancestor case, where two shards both created `home/` on the way to different things
    /// beneath it. Files cannot collide, for the reason above.
    pub fn merge_from(&mut self, other: Contents) {
        if self.entries.is_empty() {
            *self = other;
            return;
        }
        let Contents { names, entries, .. } = other;
        let shift = self.absorb_names(names);
        for entry in entries {
            let offset = entry.offset + shift;
            let len = entry.len;
            match entry.node {
                FileOrFolder::Folder(incoming) => {
                    let start = offset as usize;
                    // SAFETY: The slice came from `OsStr::as_encoded_bytes()`.
                    let name = unsafe {
                        OsStr::from_encoded_bytes_unchecked(
                            &self.names[start..start + len as usize],
                        )
                    };
                    match self.position(name) {
                        Some(position) => match &mut self.entries[position].node {
                            FileOrFolder::Folder(existing) => existing.merge_from(*incoming),
                            FileOrFolder::File(_) => {
                                unreachable!("a file and a folder cannot share one name")
                            }
                        },
                        None => self.place(offset, len, FileOrFolder::Folder(incoming), false),
                    }
                }
                file @ FileOrFolder::File(_) => self.place(offset, len, file, false),
            }
        }
    }

    pub fn values(&self) -> impl Iterator<Item = &FileOrFolder> {
        self.entries.iter().map(|entry| &entry.node)
    }

    /// Every entry, as a name and what it holds.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &FileOrFolder)> {
        self.entries.iter().map(|entry| {
            // SAFETY: The slice came from `OsStr::as_encoded_bytes()`.
            let name = unsafe { OsStr::from_encoded_bytes_unchecked(self.bytes_of(entry)) };
            (name, &entry.node)
        })
    }
}

impl<'a> IntoIterator for &'a Contents {
    type Item = (&'a OsStr, &'a FileOrFolder);
    type IntoIter = Box<dyn Iterator<Item = (&'a OsStr, &'a FileOrFolder)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::File;
    use ::std::ffi::OsString;

    fn file(size: u64) -> FileOrFolder {
        FileOrFolder::File(File::sized(size))
    }

    /// The size a `FileOrFolder` reports, as a `u64` so the assertions read plainly.
    fn size(node: Option<&FileOrFolder>) -> Option<u64> {
        node.map(|node| {
            u64::try_from(node.size(crate::model::SizeKind::Disk)).expect("test sizes are small")
        })
    }

    #[test]
    fn finds_entries_by_name() {
        let mut contents = Contents::default();
        contents.insert(OsStr::new("alpha"), file(1));
        contents.insert(OsStr::new("beta"), file(2));
        assert_eq!(contents.len(), 2);
        assert_eq!(size(contents.get(OsStr::new("alpha"))), Some(1));
        assert_eq!(size(contents.get(OsStr::new("beta"))), Some(2));
        assert!(contents.get(OsStr::new("gamma")).is_none());
    }

    /// Names sit next to each other in one buffer, so a prefix must not be mistaken for a name.
    #[test]
    fn a_prefix_of_a_neighbour_is_not_a_match() {
        let mut contents = Contents::default();
        contents.insert(OsStr::new("read"), file(1));
        contents.insert(OsStr::new("me"), file(2));
        assert!(contents.get(OsStr::new("readme")).is_none());
        assert!(contents.get(OsStr::new("rea")).is_none());
        assert_eq!(size(contents.get(OsStr::new("read"))), Some(1));
        assert_eq!(size(contents.get(OsStr::new("me"))), Some(2));
    }

    #[test]
    fn replacing_an_entry_returns_the_old_one() {
        let mut contents = Contents::default();
        contents.insert(OsStr::new("a"), file(1));
        let old = contents.insert(OsStr::new("a"), file(9));
        assert_eq!(size(old.as_ref()), Some(1));
        assert_eq!(contents.len(), 1, "replacing does not add an entry");
        assert_eq!(size(contents.get(OsStr::new("a"))), Some(9));
    }

    /// An absorbed buffer is taken whole when the folder is empty and appended when it is not;
    /// either way the offsets an entry was given have to still name it.
    #[test]
    fn absorbing_a_buffer_keeps_every_name_addressable() {
        for preload in [false, true] {
            let mut contents = Contents::default();
            if preload {
                contents.insert(OsStr::new("already-here"), file(7));
            }
            let names = b"onetwothree".to_vec();
            let shift = contents.absorb_names(names);
            for (offset, len, value) in [(0u32, 3u32, 1u64), (3, 3, 2), (6, 5, 3)] {
                contents.place(offset + shift, len, file(value), false);
            }
            assert_eq!(size(contents.get(OsStr::new("one"))), Some(1));
            assert_eq!(size(contents.get(OsStr::new("two"))), Some(2));
            assert_eq!(size(contents.get(OsStr::new("three"))), Some(3));
            if preload {
                assert_eq!(size(contents.get(OsStr::new("already-here"))), Some(7));
            }
        }
    }

    /// Everything must behave the same either side of the threshold, since the lookup path changes.
    #[test]
    fn behaves_the_same_once_the_index_exists() {
        let mut contents = Contents::default();
        let count = INDEX_ABOVE * 2;
        for i in 0..count {
            contents.insert(OsStr::new(&format!("entry-{i}")), file(i as u64));
        }
        assert_eq!(contents.len(), count);
        for i in 0..count {
            assert_eq!(
                size(contents.get(OsStr::new(&format!("entry-{i}")))),
                Some(i as u64),
                "entry {i} after the index was built"
            );
        }
        assert!(contents.get(OsStr::new("entry-nope")).is_none());
    }

    #[test]
    fn removing_keeps_the_rest_findable() {
        let mut contents = Contents::default();
        for i in 0..INDEX_ABOVE * 2 {
            contents.insert(OsStr::new(&format!("e{i}")), file(i as u64));
        }
        assert_eq!(size(contents.remove(OsStr::new("e0")).as_ref()), Some(0));
        assert!(contents.get(OsStr::new("e0")).is_none());
        for i in 1..INDEX_ABOVE * 2 {
            assert_eq!(
                size(contents.get(OsStr::new(&format!("e{i}")))),
                Some(i as u64),
                "entry {i} survived the removal"
            );
        }
    }

    #[test]
    fn iteration_yields_every_name_once() {
        let mut contents = Contents::default();
        for name in ["one", "two", "three"] {
            contents.insert(OsStr::new(name), file(1));
        }
        let mut seen: Vec<_> = contents
            .iter()
            .map(|(name, _)| name.to_os_string())
            .collect();
        seen.sort();
        assert_eq!(seen, ["one", "three", "two"].map(OsString::from));
    }
}
