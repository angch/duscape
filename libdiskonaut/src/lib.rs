//! Core disk usage model, treemap layout, directory scanning, and helpers (no TUI).

pub mod error;
pub mod format;
pub mod model;
pub mod os;
pub mod scan;
pub mod tiles;

pub use error::DiskonautError;
pub use format::{DisplayCount, DisplaySize, DisplaySizeRounded, truncate_end, truncate_middle};
pub use model::{File, FileOrFolder, FileToDelete, FileTree, Folder};
pub use scan::{
    DirEntries, DirSummary, EntryMeta, NamedEntry, Outline, ScanItem, ScanOptions, SharedBlocks,
    scan_directories, scan_folder, scan_into_tree,
};
pub use tiles::{Area, Board, FileMetadata, FileType, RectFloat, Tile, TreeMap, files_in_folder};
