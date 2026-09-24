use ::std::ffi::OsString;
use ::std::path::PathBuf;

use crate::model::{FileOrFolder, FileTree, Sizes};
use crate::tiles::{FileMetadata, FileType};

#[derive(Clone)]
pub struct FileToDelete {
    pub path_in_filesystem: PathBuf,
    pub path_to_file: Vec<OsString>,
    pub file_type: FileType,
    pub num_descendants: Option<u64>,
    /// The size shown, of whichever kind the view shows, for the confirmation dialog.
    pub size: u128,
    /// Both of its sizes, for what deleting it frees.
    pub sizes: Sizes,
}

impl FileToDelete {
    /// `entry`, an entry of `tree`'s current folder as a viewer lists it, ready to confirm and
    /// delete.
    #[must_use]
    pub fn in_current_folder(tree: &FileTree, entry: FileMetadata) -> Self {
        let sizes = tree
            .item_in_current_folder(&entry.name)
            .map(FileOrFolder::sizes)
            .unwrap_or_default();
        let mut path_to_file = tree.current_folder_names.clone();
        path_to_file.push(entry.name);
        FileToDelete {
            path_in_filesystem: tree.path_in_filesystem.clone(),
            path_to_file,
            file_type: entry.file_type,
            num_descendants: entry.descendants,
            size: entry.size,
            sizes,
        }
    }
    pub fn full_path(&self) -> PathBuf {
        let mut full_path = self.path_in_filesystem.clone();
        for component in &self.path_to_file {
            full_path.push(component);
        }
        full_path
    }
}
