//! The side panel as a tree — WizTree's tree view: the listed folder's entries, largest first,
//! and under each folder opened *in place* its own entries, indented, to any depth, rather than
//! one folder at a time. Nothing about drawing is here: which folders are open, and the rows
//! that makes. A viewer draws the rows with their depth and an expander, and keeps the cursor
//! by a row's path, which survives the list re-sorting under it during a scan.

use ::std::collections::HashSet;
use ::std::ffi::OsString;

use super::{FileMetadata, FileType, files_in_folder};
use crate::model::{FileOrFolder, Folder, SizeKind};

/// One row of the tree: an entry, and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// From the listed folder down to this entry: `[name]` for a top-level row.
    pub path: Vec<OsString>,
    /// Levels below the listed folder; the indentation.
    pub depth: usize,
    /// The entry as the flat listing gives it: its `percentage` is its share of its own parent,
    /// WizTree's "% of parent".
    pub entry: FileMetadata,
    /// A folder whose entries follow, indented.
    pub open: bool,
}

/// Which folders are open in place, by their path from the listed folder.
#[derive(Debug, Default, Clone)]
pub struct Expansion {
    open: HashSet<Vec<OsString>>,
}

impl Expansion {
    #[must_use]
    pub fn is_open(&self, path: &[OsString]) -> bool {
        self.open.contains(path)
    }

    /// Open `path` if closed, close it if open. Returns whether it is open now.
    pub fn toggle(&mut self, path: &[OsString]) -> bool {
        if self.is_open(path) {
            self.close(path);
            false
        } else {
            self.open(path);
            true
        }
    }

    pub fn open(&mut self, path: &[OsString]) {
        self.open.insert(path.to_vec());
    }

    /// Close `path`, and everything opened below it: opened again, it opens shallow.
    pub fn close(&mut self, path: &[OsString]) {
        self.open.retain(|opened| !opened.starts_with(path));
    }

    /// Close everything: the listed folder changed.
    pub fn clear(&mut self) {
        self.open.clear();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// The rows of `folder`: its entries largest first by the size of `kind`, each open folder
    /// followed by its own rows, indented. With nothing open, exactly the flat listing.
    #[must_use]
    pub fn rows(&self, folder: &Folder, kind: SizeKind) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(folder, &mut Vec::new(), 0, kind, &mut rows);
        rows
    }

    fn push_rows(
        &self,
        folder: &Folder,
        prefix: &mut Vec<OsString>,
        depth: usize,
        kind: SizeKind,
        out: &mut Vec<Row>,
    ) {
        for entry in files_in_folder(folder, 0, kind) {
            prefix.push(entry.name.clone());
            let open = entry.file_type == FileType::Folder && self.is_open(prefix);
            let child = if open {
                folder.contents.get(&entry.name)
            } else {
                None
            };
            out.push(Row {
                path: prefix.clone(),
                depth,
                entry,
                open,
            });
            if let Some(FileOrFolder::Folder(child)) = child {
                self.push_rows(child, prefix, depth + 1, kind, out);
            }
            prefix.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;
    use ::std::path::Path;

    use super::Expansion;
    use crate::model::SizeKind;
    use crate::scan::EntryMeta;
    use crate::{FileTree, Folder};

    fn meta(size: u64, is_dir: bool) -> EntryMeta {
        EntryMeta {
            size,
            apparent: size,
            inode: 0,
            links: 1,
            is_dir,
            shared_extent: 0,
        }
    }

    /// `big/` (a 600, b 300, `sub/` (c 50)), `medium.txt` 400.
    fn tree() -> FileTree {
        let root = Path::new("/r");
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        for (path, size, is_dir) in [
            ("big", 0, true),
            ("big/a", 600, false),
            ("big/b", 300, false),
            ("big/sub", 0, true),
            ("big/sub/c", 50, false),
            ("medium.txt", 400, false),
        ] {
            tree.add_entry(meta(size, is_dir), &root.join(path));
        }
        tree
    }

    fn names(rows: &[super::Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                format!(
                    "{}{}{}",
                    "  ".repeat(row.depth),
                    row.entry.name.to_string_lossy(),
                    if row.open { "/" } else { "" }
                )
            })
            .collect()
    }

    fn path(names: &[&str]) -> Vec<OsString> {
        names.iter().map(OsString::from).collect()
    }

    #[test]
    fn closed_it_is_the_flat_listing() {
        let tree = tree();
        let rows = Expansion::default().rows(tree.get_current_folder(), SizeKind::Disk);
        assert_eq!(names(&rows), ["big", "medium.txt"]);
        assert_eq!(rows[0].path, path(&["big"]));
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn an_open_folder_lists_its_entries_under_it_largest_first() {
        let tree = tree();
        let mut open = Expansion::default();
        assert!(open.toggle(&path(&["big"])));
        let rows = open.rows(tree.get_current_folder(), SizeKind::Disk);
        assert_eq!(names(&rows), ["big/", "  a", "  b", "  sub", "medium.txt"]);
        assert_eq!(rows[1].path, path(&["big", "a"]));
        assert_eq!(rows[1].depth, 1);
        // Its share is of its parent, as WizTree's "% of parent".
        assert!((rows[1].entry.percentage - 600.0 / 950.0).abs() < 1e-9);
        assert!((rows[0].entry.percentage - 950.0 / 1350.0).abs() < 1e-9);

        open.open(&path(&["big", "sub"]));
        let rows = open.rows(tree.get_current_folder(), SizeKind::Disk);
        assert_eq!(
            names(&rows),
            ["big/", "  a", "  b", "  sub/", "    c", "medium.txt"]
        );
    }

    #[test]
    fn closing_a_folder_closes_what_was_open_below_it() {
        let tree = tree();
        let mut open = Expansion::default();
        open.open(&path(&["big"]));
        open.open(&path(&["big", "sub"]));
        assert!(!open.toggle(&path(&["big"])));
        assert!(open.is_empty(), "sub is closed with it");
        open.open(&path(&["big"]));
        let rows = open.rows(tree.get_current_folder(), SizeKind::Disk);
        assert_eq!(names(&rows), ["big/", "  a", "  b", "  sub", "medium.txt"]);
        // A file is never open, whatever the set says.
        open.open(&path(&["medium.txt"]));
        let rows = open.rows(tree.get_current_folder(), SizeKind::Disk);
        assert!(!rows.last().unwrap().open);
        open.clear();
        assert!(open.is_empty());
    }
}
