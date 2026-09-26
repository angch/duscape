//! The treemap nested: a folder's tile holds its own entries' tiles, laid out by
//! the same squarify inside it under the folder's label, and theirs inside those, as deep as
//! there is room. The top-level tiles are the board's, so selection, zoom and the "small files"
//! corner are what they were; the nesting is drawn inside them, and a nested tile can be
//! pointed at.

use ::std::ffi::OsString;

use super::{Area, FileType, Tile, TreeMap, largest_in_folder};
use crate::model::{FileOrFolder, Folder, SizeKind};

/// A tile inside a folder's tile.
#[derive(Debug, Clone)]
pub struct NestedTile {
    /// From the listed folder down to this entry.
    pub path: Vec<OsString>,
    /// Levels inside the top-level tile: 1 for a top-level folder's own entries.
    pub depth: usize,
    /// Its place and what it is, in the board's cells.
    pub tile: Tile,
}

/// How far in the nesting goes. What stops it is room: a folder's entries are laid out inside
/// its tile only while the inside is big enough for two of the squarify's minimum tiles either
/// way, so the nesting reaches the files wherever there is room to show them, and the tiles
/// there number at most what the treemap's area holds at the minimum size. The two caps
/// are guards well beyond that, not the working limit.
#[derive(Debug, Clone, Copy)]
pub struct Nesting {
    /// Levels inside a top-level tile, at most.
    pub max_depth: usize,
    /// Cells left at the top of a folder's tile for its label.
    pub label_rows: u16,
    /// Cells left at a folder's tile's sides and bottom, so its border shows around its entries.
    pub margin: u16,
    /// Tiles in all, at most.
    pub max_tiles: usize,
}

impl Default for Nesting {
    fn default() -> Self {
        Nesting {
            max_depth: 64,
            label_rows: 3,
            margin: 1,
            max_tiles: 100_000,
        }
    }
}

/// The least a folder's inside must measure, in cells, for its entries to be laid out in it:
/// two of the squarify's minimum tiles side by side, and two on top of each other. This is
/// what ends the nesting.
const NEST_MIN_WIDTH: u16 = 2 * MIN_TILE_WIDTH;
const NEST_MIN_HEIGHT: u16 = 2 * MIN_TILE_HEIGHT;
/// The squarify's minimum tile (`treemap::MINIMUM_WIDTH`/`HEIGHT`), in cells.
const MIN_TILE_WIDTH: u16 = 8;
const MIN_TILE_HEIGHT: u16 = 3;

/// The tiles inside `tiles` — the board's, for `folder` — down to `nesting`'s limits, parents
/// before their children.
#[must_use]
pub fn nest(folder: &Folder, tiles: &[Tile], kind: SizeKind, nesting: &Nesting) -> Vec<NestedTile> {
    let mut out = Vec::new();
    for tile in tiles {
        if tile.file_type != FileType::Folder {
            continue;
        }
        if let Some(FileOrFolder::Folder(child)) = folder.contents.get(&tile.name) {
            nest_into(
                child,
                tile,
                vec![tile.name.clone()],
                1,
                kind,
                nesting,
                &mut out,
            );
        }
    }
    out
}

fn nest_into(
    folder: &Folder,
    tile: &Tile,
    path: Vec<OsString>,
    depth: usize,
    kind: SizeKind,
    nesting: &Nesting,
    out: &mut Vec<NestedTile>,
) {
    if depth > nesting.max_depth || out.len() >= nesting.max_tiles {
        return;
    }
    let width = tile.width.saturating_sub(2 * nesting.margin);
    let height = tile
        .height
        .saturating_sub(nesting.label_rows + nesting.margin);
    if width < NEST_MIN_WIDTH || height < NEST_MIN_HEIGHT {
        return;
    }
    let inside = Area {
        x: tile.x + nesting.margin,
        y: tile.y + nesting.label_rows,
        width,
        height,
    };
    // Only as many entries as the inside has room for at the minimum tile size, plus one so
    // the squarify still sees what follows: a folder of fifty thousand entries is not listed
    // and sorted whole for the dozen that get a tile.
    let room = (usize::from(width) / usize::from(MIN_TILE_WIDTH) + 1)
        * (usize::from(height) / usize::from(MIN_TILE_HEIGHT) + 1)
        + 1;
    let files = largest_in_folder(folder, kind, room);
    let mut map = TreeMap::new(&inside);
    map.populate_tiles(files.iter().collect());
    let first = out.len();
    for child in map.tiles {
        let mut child_path = path.clone();
        child_path.push(child.name.clone());
        out.push(NestedTile {
            path: child_path,
            depth,
            tile: child,
        });
    }
    let last = out.len();
    for index in first..last {
        if out[index].tile.file_type != FileType::Folder {
            continue;
        }
        let name = out[index].tile.name.clone();
        let Some(FileOrFolder::Folder(child)) = folder.contents.get(&name) else {
            continue;
        };
        let tile = out[index].tile.clone();
        let path = out[index].path.clone();
        nest_into(child, &tile, path, depth + 1, kind, nesting, out);
    }
}

#[cfg(test)]
mod tests {
    use ::std::path::Path;

    use super::{Nesting, nest};
    use crate::model::SizeKind;
    use crate::scan::EntryMeta;
    use crate::tiles::{Area, Board};
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

    /// `big/` (a 600, b 300, `sub/` (c 250)), `medium.txt` 100.
    fn tree() -> FileTree {
        let root = Path::new("/r");
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        for (path, size, is_dir) in [
            ("big", 0, true),
            ("big/a", 600, false),
            ("big/b", 300, false),
            ("big/sub", 0, true),
            ("big/sub/c", 250, false),
            ("medium.txt", 100, false),
        ] {
            tree.add_entry(meta(size, is_dir), &root.join(path));
        }
        tree
    }

    fn board(tree: &FileTree, width: u16, height: u16) -> Board {
        let mut board = Board::new(tree.get_current_folder());
        board.change_area(&Area {
            x: 0,
            y: 0,
            width,
            height,
        });
        board
    }

    #[test]
    fn a_folder_tile_holds_its_entries_under_its_label_and_theirs_in_turn() {
        let tree = tree();
        let board = board(&tree, 200, 80);
        let big = board
            .tiles
            .iter()
            .find(|tile| tile.name == "big")
            .expect("a tile");
        let nesting = Nesting::default();
        let nested = nest(
            tree.get_current_folder(),
            &board.tiles,
            SizeKind::Disk,
            &nesting,
        );
        let names: Vec<(String, usize)> = nested
            .iter()
            .map(|t| (t.tile.name.to_string_lossy().into_owned(), t.depth))
            .collect();
        assert!(names.contains(&("a".to_string(), 1)), "{names:?}");
        assert!(names.contains(&("sub".to_string(), 1)), "{names:?}");
        assert!(names.contains(&("c".to_string(), 2)), "{names:?}");
        assert!(
            !names.iter().any(|(n, _)| n == "medium.txt"),
            "a file nests nothing"
        );
        for t in &nested {
            // Inside its top-level tile, under the label rows, within the margins.
            assert!(t.tile.x >= big.x + nesting.margin, "{:?}", t.tile);
            assert!(t.tile.y >= big.y + nesting.label_rows, "{:?}", t.tile);
            assert!(
                t.tile.x + t.tile.width <= big.x + big.width - nesting.margin,
                "{:?}",
                t.tile
            );
            assert!(
                t.tile.y + t.tile.height <= big.y + big.height,
                "{:?}",
                t.tile
            );
        }
        let c = nested.iter().find(|t| t.tile.name == "c").unwrap();
        assert_eq!(c.path, ["big", "sub", "c"]);
        // Parents come before their children, for painting.
        let sub_at = nested.iter().position(|t| t.tile.name == "sub").unwrap();
        let c_at = nested.iter().position(|t| t.tile.name == "c").unwrap();
        assert!(sub_at < c_at);
    }

    #[test]
    fn the_largest_entries_are_the_head_of_the_whole_listing() {
        use crate::tiles::{files_in_folder, largest_in_folder};
        let tree = tree();
        let folder = tree.get_current_folder();
        let whole = files_in_folder(folder, 0, SizeKind::Disk);
        for limit in [0, 1, 2, 5] {
            let top = largest_in_folder(folder, SizeKind::Disk, limit);
            assert_eq!(top, whole[..limit.min(whole.len())], "limit {limit}");
        }
    }

    #[test]
    fn too_small_a_tile_or_too_deep_nests_nothing() {
        let tree = tree();
        let tiny = board(&tree, 20, 6);
        assert!(
            nest(
                tree.get_current_folder(),
                &tiny.tiles,
                SizeKind::Disk,
                &Nesting::default()
            )
            .is_empty()
        );
        let board = board(&tree, 200, 80);
        let shallow = Nesting {
            max_depth: 1,
            ..Nesting::default()
        };
        let nested = nest(
            tree.get_current_folder(),
            &board.tiles,
            SizeKind::Disk,
            &shallow,
        );
        assert!(nested.iter().all(|t| t.depth == 1));
        let capped = Nesting {
            max_tiles: 0,
            ..Nesting::default()
        };
        assert!(
            nest(
                tree.get_current_folder(),
                &board.tiles,
                SizeKind::Disk,
                &capped
            )
            .is_empty()
        );
    }
}
