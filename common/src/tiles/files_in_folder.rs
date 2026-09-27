use ::std::ffi::OsString;

use ::std::ffi::OsStr;

use crate::model::{FileOrFolder, Folder, SizeKind};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FileType {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    pub name: OsString,
    pub size: u128,
    pub descendants: Option<u64>,
    pub percentage: f64, // 1.0 is 100% (0.5 is 50%, etc.)
    pub file_type: FileType,
}

fn calculate_percentage(size: u128, total_size: u128, total_files_in_parent: usize) -> f64 {
    if size == 0 && total_size == 0 {
        // if all files in the folder are of size 0, we'll want to display them all as
        // the same size
        1.0 / total_files_in_parent as f64
    } else {
        size as f64 / total_size as f64
    }
}

/// The `limit` largest entries of `folder` by the size of `kind`, largest first, with each one's
/// share of the whole folder — what [`files_in_folder`] gives for them, without building and
/// sorting the rest: for a layout with room for `limit` tiles at most, in a folder of tens of
/// thousands of entries, relaid out on every batch of a scan.
#[must_use]
pub fn largest_in_folder(folder: &Folder, kind: SizeKind, limit: usize) -> Vec<FileMetadata> {
    largest_in_folder_from(folder, kind, limit, 0.0)
}

/// [`largest_in_folder`], less the entries whose share of the folder is under `least_share`:
/// for a layout where those can never get a tile, so that the dust of a folder of tens of
/// thousands of small files is not named and ranked on every relayout.
#[must_use]
pub fn largest_in_folder_from(
    folder: &Folder,
    kind: SizeKind,
    limit: usize,
    least_share: f64,
) -> Vec<FileMetadata> {
    let mut ranking = Ranking::new(folder, kind, least_share);
    let mut files = ranking.head(limit, least_share);
    for (index, file) in files.iter_mut().enumerate() {
        file.name = ranking.name(index).to_os_string();
    }
    files
}

/// An entry's share of its folder, its name borrowed: for a picture of many entries, where
/// copying each name would cost more than the rest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Share<'a> {
    pub name: &'a OsStr,
    pub percentage: f64,
    pub file_type: FileType,
}

type Ranked<'a> = (u128, &'a OsStr, &'a FileOrFolder);

/// A folder's entries of a least share or more, ranked largest first and ties by name only as
/// far as they are asked for: the tiles' head, then as many more as a corner has pixels, from
/// one pass over the folder, where each asked the folder again.
pub struct Ranking<'a> {
    entries: Vec<Ranked<'a>>,
    total_size: u128,
    count: usize,
    /// How many of `entries`, from the first, are in their places: the rest are all smaller.
    sorted: usize,
}

impl<'a> Ranking<'a> {
    /// `folder`'s entries of `least_share` of it or more, ranked as far as none.
    #[must_use]
    pub fn new(folder: &'a Folder, kind: SizeKind, least_share: f64) -> Self {
        let entries_total: u128 = folder.contents.values().map(|entry| entry.size(kind)).sum();
        let total_size = folder.sizes.get(kind).max(entries_total);
        let least = least_of(least_share, total_size);
        let entries = folder
            .contents
            .iter()
            .map(|(name, entry)| (entry.size(kind), name, entry))
            .filter(|&(size, _, _)| size >= least)
            .collect();
        Ranking {
            entries,
            total_size,
            count: folder.contents.len(),
            sorted: 0,
        }
    }

    /// Rank the first `to`, or as many as there are.
    fn rank_to(&mut self, to: usize) {
        // Largest first, ties by name: the order `files_in_folder` sorts into.
        let by_rank = |a: &Ranked<'_>, b: &Ranked<'_>| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1));
        let to = to.min(self.entries.len());
        if to <= self.sorted {
            return;
        }
        let rest = &mut self.entries[self.sorted..];
        let wanted = to - self.sorted;
        if wanted < rest.len() {
            rest.select_nth_unstable_by(wanted - 1, by_rank);
        }
        rest[..wanted].sort_unstable_by(by_rank);
        self.sorted = to;
    }

    fn percentage(&self, size: u128) -> f64 {
        calculate_percentage(size, self.total_size, self.count)
    }

    /// The `limit` largest of `least_share` of the folder or more (at least what the ranking
    /// was made with), for a layout: their names left empty, since only those given a tile need
    /// one ([`Ranking::name`], by the same index).
    pub fn head(&mut self, limit: usize, least_share: f64) -> Vec<FileMetadata> {
        self.rank_to(limit);
        let least = least_of(least_share, self.total_size);
        self.entries[..self.sorted.min(limit)]
            .iter()
            .take_while(|&&(size, _, _)| size >= least)
            .map(|&(size, _, entry)| {
                let (descendants, file_type) = match entry {
                    FileOrFolder::Folder(folder) => {
                        (Some(folder.num_descendants), FileType::Folder)
                    }
                    FileOrFolder::File(_) => (None, FileType::File),
                };
                FileMetadata {
                    size,
                    name: OsString::new(),
                    descendants,
                    percentage: self.percentage(size),
                    file_type,
                }
            })
            .collect()
    }

    /// The name of the entry ranked `index`th, once ranked that far.
    #[must_use]
    pub fn name(&self, index: usize) -> &'a OsStr {
        self.entries[index].1
    }

    /// The entries ranked `from` up to `to`, as [`Share`]s.
    pub fn shares(&mut self, from: usize, to: usize) -> Vec<Share<'a>> {
        self.rank_to(to);
        let to = to.min(self.sorted);
        self.entries[from.min(to)..to]
            .iter()
            .map(|&(size, name, entry)| Share {
                name,
                percentage: self.percentage(size),
                file_type: match entry {
                    FileOrFolder::Folder(_) => FileType::Folder,
                    FileOrFolder::File(_) => FileType::File,
                },
            })
            .collect()
    }
}

/// `least_share` of `total_size`, in bytes; nothing for an empty folder, whose entries all
/// share it equally, so none is dropped.
fn least_of(least_share: f64, total_size: u128) -> u128 {
    if total_size == 0 {
        0
    } else {
        (least_share * total_size as f64) as u128
    }
}

/// The entries of `folder`, largest first by the size of `kind`, less the `offset` largest (the
/// zoom).
pub fn files_in_folder(folder: &Folder, offset: usize, kind: SizeKind) -> Vec<FileMetadata> {
    let mut files = Vec::new();
    // A folder is never larger than the entries inside it, but it can be *smaller*: shared blocks
    // — hard links, or XFS/btrfs reflinks — reached twice under one folder are held once, so the
    // folder's size is the space it occupies while each entry still reports its own full size.
    //
    // Dividing by the folder's size would then give fractions summing to more than 1.0 (four
    // reflinked copies of one file give 4.0), and the layout would run off the board. The tiles
    // are shares of the space their siblings take between them, which is what fills the board
    // exactly and is the only reading that stays self-consistent when blocks are shared.
    let entries_total: u128 = folder.contents.values().map(|entry| entry.size(kind)).sum();
    let total_size = folder.sizes.get(kind).max(entries_total);
    for (name, file_or_folder) in &folder.contents {
        files.push({
            let size = file_or_folder.size(kind);
            let name = name.to_os_string();
            let (descendants, file_type) = match file_or_folder {
                FileOrFolder::Folder(folder) => (Some(folder.num_descendants), FileType::Folder),
                FileOrFolder::File(_file) => (None, FileType::File),
            };
            let percentage = calculate_percentage(size, total_size, folder.contents.len());
            FileMetadata {
                size,
                name,
                descendants,
                percentage,
                file_type,
            }
        });
    }
    files.sort_by(|a, b| {
        if a.percentage == b.percentage {
            a.name.partial_cmp(&b.name).expect("could not compare name")
        } else {
            b.percentage
                .partial_cmp(&a.percentage)
                .expect("could not compare percentage")
        }
    });
    if offset > 0 {
        let removed_items = files.drain(..offset);
        let number_of_files_without_removed_contents = folder.contents.len() - removed_items.len();
        let removed_size = removed_items.fold(0, |acc, file| acc + file.size);
        let size_without_removed_items = total_size - removed_size;
        for file in &mut files {
            file.percentage = calculate_percentage(
                file.size,
                size_without_removed_items,
                number_of_files_without_removed_contents,
            );
        }
    }
    files
}
