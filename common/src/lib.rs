//! What every duscape viewer shares, with no user interface: the disk-usage model
//! ([`FileTree`]), the squarified treemap and its selection ([`Board`]), the protocol a scanner
//! delivers ([`scan`]), deleting from disk ([`delete`]), reading a file for a preview
//! ([`preview`]), the native clipboard ([`clipboard`]), handing an entry to the desktop
//! ([`launch`]) and formatting.
//!
//! The walkers are in `duscape-scan`, which depends on this crate. `docs/features.md` has the
//! whole map, and which viewer offers what.

pub mod clipboard;
pub mod delete;
pub mod error;
pub mod format;
pub mod launch;
pub mod metafiles;
pub mod model;
pub mod os;
pub mod placement;
pub mod preview;
pub mod scan;
pub mod tiles;

pub use error::DuscapeError;
pub use format::{DisplayCount, DisplaySize, DisplaySizeRounded, truncate_end, truncate_middle};
pub use model::{File, FileOrFolder, FileToDelete, FileTree, Folder};
pub use scan::{
    DirEntries, DirSummary, EntryMeta, Issue, Issues, NamedEntry, Outline, ScanItem, ScanOptions,
    SharedBlocks,
};
pub use tiles::{Area, Board, FileMetadata, FileType, RectFloat, Tile, TreeMap, files_in_folder};
