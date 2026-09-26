use ::std::ffi::OsString;

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
