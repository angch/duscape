//! The treemap nested: a folder's tile holds its own entries' tiles, laid out by
//! the same squarify inside it under the folder's label, and theirs inside those, as deep as
//! there is room. The top-level tiles are the board's, so selection, zoom and the "small files"
//! corner are what they were; the nesting is drawn inside them, and a nested tile can be
//! pointed at.

use ::std::collections::{HashMap, VecDeque};
use ::std::ffi::{OsStr, OsString};
use ::std::hash::BuildHasher;
use ::std::time::Instant;

use super::{Area, FileType, Grid, Plan, Ranking, Share, Tile, TreeMap, scatter};
use crate::model::files::hash::FastBuildHasher;
use crate::model::{FileOrFolder, Folder, SizeKind};

/// A tile inside a folder's tile.
#[derive(Debug, Clone)]
pub struct NestedTile {
    /// The nested tile of the folder it is in, by its index in the nesting; `None` for a
    /// top-level folder's own entries. [`nested_path`] follows these up.
    pub parent: Option<usize>,
    /// The board's tile it is inside, by its index in the tiles the nesting was made from.
    pub top: usize,
    /// Levels inside the top-level tile: 1 for a top-level folder's own entries.
    pub depth: usize,
    /// Its place and what it is, in the board's cells.
    pub tile: Tile,
    /// For a folder whose entries were laid out inside it: what they cover.
    pub inside: Option<Inside>,
}

/// What a folder tile's entries cover: all of `area` but `corner`, which is its "small files"
/// corner or the room its first pass left. A painter need fill only the rest of the tile — its
/// label band, its margins and the corner — since the entries' tiles are drawn over the
/// inside: filled whole, each level painted its parent's area again, a dozen times over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inside {
    pub area: Area,
    pub corner: Area,
}

impl Inside {
    /// The parts of `tile` its entries leave showing: above, below, left and right of the
    /// inside, then the corner. Some may be empty.
    #[must_use]
    pub fn around(&self, tile: &Area) -> [Area; 5] {
        let inside = self.area;
        let right = inside.x + inside.width;
        let bottom = inside.y + inside.height;
        [
            Area {
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: inside.y.saturating_sub(tile.y),
            },
            Area {
                x: tile.x,
                y: bottom,
                width: tile.width,
                height: (tile.y + tile.height).saturating_sub(bottom),
            },
            Area {
                x: tile.x,
                y: inside.y,
                width: inside.x.saturating_sub(tile.x),
                height: inside.height,
            },
            Area {
                x: right,
                y: inside.y,
                width: (tile.x + tile.width).saturating_sub(right),
                height: inside.height,
            },
            self.corner,
        ]
    }
}

/// A nesting: the tiles, what each board tile's entries cover (by its index, as
/// [`NestedTile::inside`] for the nested ones), and whether it is complete — `false` when a
/// first pass's deadline or `room_cap` cut it.
#[derive(Debug, Default)]
pub struct Nested {
    pub tiles: Vec<NestedTile>,
    pub tops: Vec<Option<Inside>>,
    pub complete: bool,
    /// How each folder's inside was cut, by the folder's path ([`path_key`]): for the next
    /// layout of the same folders with new sizes to keep ([`nest_steady`]).
    pub plans: Plans,
}

/// Each nested folder's [`Plan`], by [`path_key`].
pub type Plans = HashMap<u64, Plan, FastBuildHasher>;

/// A key for the folder or file `name` in the folder keyed `parent` (0 for the listed folder):
/// the same entry has the same key from layout to layout.
#[must_use]
pub fn path_key(hasher: &FastBuildHasher, parent: u64, name: &OsStr) -> u64 {
    hasher.hash_one((parent, name))
}

/// How far in the nesting goes. What stops it is room: a folder's entries are laid out inside
/// its tile only while the inside is big enough for two of the grid's minimum tiles either
/// way, so the nesting reaches the files wherever there is room to show them — in a window's
/// pixel grid, down to tiles a few pixels wide — and the tiles there number at most what the
/// treemap's area holds at the minimum size. The two caps are guards beyond that, not the
/// working limit.
///
/// A folder tile too short for its label and its entries under it still holds them, inside
/// its margin alone: [`Nesting::labelled`] says which folders have the label rows, so that a
/// painter labels only those.
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
    /// The cells, and the least tile: the board's.
    pub grid: Grid,
    /// Fill each folder's own "small files" corner with its entries' specks, as the board's
    /// is filled ([`scatter`]), handing each to [`nest_with`]'s `speck`: in pixel cells, where
    /// a speck is a pixel. A folder with a corner ranks its entries once ([`Ranking`]): the
    /// tiles' head, then as many more as the corner has pixels.
    pub dust: bool,
    /// When to stop, for a first pass: past it, no folder deeper than the top-level ones'
    /// entries is laid out, and [`nest_with`] says the nesting is not complete. The nesting
    /// goes a level at a time across every folder, so what a deadline cuts is the deepest
    /// levels everywhere, not the last folders whole.
    pub deadline: Option<Instant>,
    /// When the corners' specks stop, for a first pass: past it the rest wait for the second
    /// pass, and [`nest_with`] says the nesting is not complete. Earlier than `deadline`, since
    /// the top-level folders' entries are laid out whatever the time, and after the specks.
    pub dust_deadline: Option<Instant>,
    /// Entries a folder lays out at most, for a first pass: a folder of tens of thousands (a
    /// Windows component store) otherwise takes a frame's time by itself. Where it cuts,
    /// [`nest_with`] says the nesting is not complete.
    pub room_cap: usize,
}

impl Default for Nesting {
    fn default() -> Self {
        Nesting {
            max_depth: 64,
            label_rows: 3,
            margin: 1,
            max_tiles: 100_000,
            grid: Grid::TERMINAL,
            dust: false,
            deadline: None,
            dust_deadline: None,
            room_cap: usize::MAX,
        }
    }
}

impl Nesting {
    /// Whether a folder's tile has its label rows above its entries: room for them, the
    /// margin and two minimum tiles under them. A shorter one's entries start under the margin.
    #[must_use]
    pub fn labelled(&self, tile: &Tile) -> bool {
        self.has_label_rows(tile.height)
    }

    fn has_label_rows(&self, height: u16) -> bool {
        height >= self.label_rows + self.margin + 2 * self.grid.min_height
    }

    /// Where a folder's entries go in its tile, if it has room for two of the least tile
    /// either way — what ends the nesting.
    fn inside(&self, tile: &Area) -> Option<Area> {
        let width = tile.width.saturating_sub(2 * self.margin);
        let top = if self.has_label_rows(tile.height) {
            self.label_rows
        } else {
            self.margin
        };
        let height = tile.height.saturating_sub(top + self.margin);
        (width >= 2 * self.grid.min_width && height >= 2 * self.grid.min_height).then(|| Area {
            x: tile.x + self.margin,
            y: tile.y + top,
            width,
            height,
        })
    }
}

/// The tiles inside `tiles` — the board's, for `folder` — down to `nesting`'s limits, parents
/// before their children.
#[must_use]
pub fn nest(folder: &Folder, tiles: &[Tile], kind: SizeKind, nesting: &Nesting) -> Vec<NestedTile> {
    nest_with(folder, tiles, kind, nesting, &mut |_| {}).tiles
}

/// One entry of a "small files" corner, laid out in it as a speck ([`scatter`]).
pub struct Speck<'a> {
    /// Where, in the board's cells.
    pub area: Area,
    pub entry: &'a Share<'a>,
    /// Its place among its folder's entries, largest first: what a tile's colour goes by.
    pub rank: usize,
    /// How deep its folder's entries are: 0 the board's own, 1 a board tile's, and so on.
    pub depth: usize,
}

/// [`nest`], handing `speck` each speck of the folders' "small files" corners when
/// `nesting.dust` is on.
pub fn nest_with(
    folder: &Folder,
    tiles: &[Tile],
    kind: SizeKind,
    nesting: &Nesting,
    speck: &mut dyn FnMut(Speck),
) -> Nested {
    nest_layout(folder, tiles, kind, nesting, speck, None, false)
}

/// [`nest_with`], keeping each folder's rows from `plans`, the last nesting's, where they still
/// fit ([`TreeMap::populate_steady`]): for the same folders with new sizes, so the nested tiles
/// grow and shrink in place.
pub fn nest_steady(
    folder: &Folder,
    tiles: &[Tile],
    kind: SizeKind,
    nesting: &Nesting,
    speck: &mut dyn FnMut(Speck),
    plans: Option<&Plans>,
) -> Nested {
    nest_layout(folder, tiles, kind, nesting, speck, plans, true)
}

/// The nesting, keeping `plans`' rows, and noting its own when `record`: keying every tile
/// costs a quarter again on a relayout, so only a viewer that will lay out again steadily does.
fn nest_layout(
    folder: &Folder,
    tiles: &[Tile],
    kind: SizeKind,
    nesting: &Nesting,
    speck: &mut dyn FnMut(Speck),
    plans: Option<&Plans>,
    record: bool,
) -> Nested {
    // However deep it goes, the nesting holds at most as many tiles as the folder tiles' area
    // holds at the minimum size: the bound on a relayout's and a paint's work is the screen.
    let area: usize = tiles
        .iter()
        .map(|tile| usize::from(tile.width) * usize::from(tile.height))
        .sum();
    let least = usize::from(nesting.grid.min_width) * usize::from(nesting.grid.min_height);
    let nesting = Nesting {
        max_tiles: nesting.max_tiles.min(area / least.max(1) + 1),
        ..*nesting
    };
    let nesting = &nesting;
    let mut out = Nested {
        tiles: Vec::new(),
        tops: vec![None; tiles.len()],
        complete: true,
        plans: Plans::default(),
    };
    let hasher = FastBuildHasher::default();
    let steady = Steady {
        kind,
        hasher,
        plans,
        record,
    };
    // A level at a time, across every folder: parents before children still, and a deadline
    // cuts the deepest levels everywhere rather than the last folders whole.
    let mut queue: VecDeque<(&Folder, Place)> = VecDeque::new();
    for (top, tile) in tiles.iter().enumerate() {
        if tile.file_type != FileType::Folder {
            continue;
        }
        if let Some(FileOrFolder::Folder(child)) = folder.contents.get(&tile.name) {
            let place = Place {
                cells: tile.area(),
                parent: None,
                top,
                depth: 1,
                key: path_key(&hasher, 0, &tile.name),
            };
            queue.push_back((&**child, place));
        }
    }
    while let Some((folder, place)) = queue.pop_front() {
        // The top-level folders' own entries always: a first pass shows every folder's.
        if place.depth > 1
            && nesting
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            out.complete = false;
            return out;
        }
        let capped = nest_into(
            folder, &place, nesting, &steady, &mut out, &mut queue, speck,
        );
        out.complete &= !capped;
    }
    out
}

/// The names from the listed folder down to the nested tile at `index`: the board's tile it is
/// in (from `tiles`, what the nesting was made from), then each folder tile it is inside.
#[must_use]
pub fn nested_path(nested: &[NestedTile], tiles: &[Tile], index: usize) -> Vec<OsString> {
    let mut path = Vec::with_capacity(nested[index].depth + 1);
    let mut at = Some(index);
    while let Some(index) = at {
        path.push(nested[index].tile.name.clone());
        at = nested[index].parent;
    }
    path.push(tiles[nested[index].top].name.clone());
    path.reverse();
    path
}

/// Whether the nested tile at `index` is at `path`, as [`nested_path`] gives it, without
/// making the path.
#[must_use]
pub fn nested_path_is(
    nested: &[NestedTile],
    tiles: &[Tile],
    index: usize,
    path: &[OsString],
) -> bool {
    let tile = &nested[index];
    if path.len() != tile.depth + 1 || tiles[tile.top].name != path[0] {
        return false;
    }
    let mut at = Some(index);
    for name in path[1..].iter().rev() {
        let Some(index) = at else {
            return false;
        };
        if nested[index].tile.name != *name {
            return false;
        }
        at = nested[index].parent;
    }
    at.is_none()
}

/// A folder tile whose entries are to be nested: its cells, its own nested tile (`None` for a
/// board's tile), the board's tile it is in, and how deep its entries are.
struct Place {
    cells: Area,
    parent: Option<usize>,
    top: usize,
    depth: usize,
    /// The folder's [`path_key`].
    key: u64,
}

/// Which size the tiles go by, and what a steady nesting keys and keeps by.
struct Steady<'a> {
    kind: SizeKind,
    hasher: FastBuildHasher,
    plans: Option<&'a Plans>,
    record: bool,
}

/// Lay `folder`'s entries out in its tile at `place`, into `out`, and queue its folders'.
/// Returns whether `nesting.room_cap` left some of them out.
fn nest_into<'a>(
    folder: &'a Folder,
    place: &Place,
    nesting: &Nesting,
    steady: &Steady,
    out: &mut Nested,
    queue: &mut VecDeque<(&'a Folder, Place)>,
    speck: &mut dyn FnMut(Speck),
) -> bool {
    if place.depth > nesting.max_depth || out.tiles.len() >= nesting.max_tiles {
        return false;
    }
    let Some(inside) = nesting.inside(&place.cells) else {
        return false;
    };
    // Only as many entries as the inside has room for at the minimum tile size, plus one so
    // the squarify still sees what follows: a folder of fifty thousand entries is not listed
    // and sorted whole for the dozen that get a tile.
    let grid = nesting.grid;
    let room = (usize::from(inside.width) / usize::from(grid.min_width) + 1)
        * (usize::from(inside.height) / usize::from(grid.min_height) + 1)
        + 1;
    let capped = room > nesting.room_cap && folder.contents.len() > nesting.room_cap;
    let room = room.min(nesting.room_cap);
    // A tile is its entry's share of the inside, and rounding adds less than a cell to either
    // side, so an entry under (least − 1)² cells can never get one: it is left out.
    let least_cells =
        f64::from(grid.min_width.saturating_sub(1)) * f64::from(grid.min_height.saturating_sub(1));
    let inside_cells = f64::from(inside.width) * f64::from(inside.height);
    let least_tile = least_cells / inside_cells;
    // Past a first pass's deadline for them the specks wait too, as the deeper levels do: how
    // many a corner holds is not known until it is laid out, and grows far faster than the
    // window (a cache of 256 folders: 5k specks at 1600×1000 points, 119k at 2560×1400).
    let late = nesting
        .dust_deadline
        .is_some_and(|deadline| Instant::now() >= deadline);
    let specks = nesting.dust && !late;
    // With the corner's specks, down to a pixel: ranked once for the tiles and the corner.
    let least_speck = 1.0 / inside_cells;
    let mut ranking = Ranking::new(
        folder,
        steady.kind,
        if specks {
            least_speck.min(least_tile)
        } else {
            least_tile
        },
    );
    let files = ranking.head(room, least_tile);
    let mut map = TreeMap::with_grid(&inside, grid);
    if steady.record {
        let plan = map.populate_steady(
            files.iter().collect(),
            &mut |entry| path_key(&steady.hasher, place.key, ranking.name(entry)),
            steady.plans.and_then(|plans| plans.get(&place.key)),
        );
        out.plans.insert(place.key, plan);
    } else {
        map.populate_tiles(files.iter().collect());
    }
    // Only the entries given a tile are named: the rest are specks, or nothing.
    for index in 0..map.tiles.len() {
        let entry = map.tile_entries()[index];
        map.tiles[index].name = ranking.name(entry).to_os_string();
    }
    // The corner: from the first entry given no tile to the far corner, or where there was
    // none, the room left by the entries too small to be ranked, which lies there too.
    let corner = match map.unrenderable_tile_coordinates {
        Some((x, y)) => Area {
            x,
            y,
            width: (inside.x + inside.width).saturating_sub(x),
            height: (inside.y + inside.height).saturating_sub(y),
        },
        None => map.leftover(),
    };
    if nesting.dust && late && corner.width > 0 && corner.height > 0 {
        out.complete = false;
    } else if specks && corner.width > 0 && corner.height > 0 {
        scatter_folder_corner(&mut ranking, &files, &map, corner, place.depth, speck);
    }
    // What the entries cover, for the folder's own tile to be filled around it.
    if !map.tiles.is_empty() {
        let covered = Some(Inside {
            area: inside,
            corner,
        });
        match place.parent {
            Some(index) => out.tiles[index].inside = covered,
            None => out.tops[place.top] = covered,
        }
    }
    // The folders among them, gone into once this level is in `out`: parents first.
    for child in map.tiles {
        if child.file_type == FileType::Folder
            && let Some(FileOrFolder::Folder(entries)) = folder.contents.get(&child.name)
        {
            let inner = Place {
                cells: child.area(),
                parent: Some(out.tiles.len()),
                top: place.top,
                depth: place.depth + 1,
                key: path_key(&steady.hasher, place.key, &child.name),
            };
            queue.push_back((entries, inner));
        }
        out.tiles.push(NestedTile {
            parent: place.parent,
            top: place.top,
            depth: place.depth,
            tile: child,
            inside: None,
        });
    }
    capped
}

/// A folder's corner filled in: the entries `map` ranked but gave no tile, then those too small
/// for one — as many more as the corner has pixels, of a pixel or more, from the same
/// `ranking` — each handed to `speck` where [`scatter`] lays it.
fn scatter_folder_corner(
    ranking: &mut Ranking,
    files: &[super::FileMetadata],
    map: &TreeMap,
    corner: Area,
    depth: usize,
    speck: &mut dyn FnMut(Speck),
) {
    let pixels = usize::from(corner.width) * usize::from(corner.height);
    let more = ranking.shares(files.len(), files.len() + pixels);
    let hidden: Vec<Share> = map
        .hidden
        .iter()
        .map(|&index| {
            let file = &files[index];
            Share {
                name: ranking.name(index),
                percentage: file.percentage,
                file_type: file.file_type,
            }
        })
        .chain(more)
        .collect();
    let shares: Vec<f64> = hidden.iter().map(|share| share.percentage).collect();
    for mote in scatter(&shares, &corner) {
        speck(Speck {
            area: mote.area,
            entry: &hidden[mote.entry],
            rank: map.tiles.len() + mote.entry,
            depth,
        });
    }
}

#[cfg(test)]
mod tests {
    use ::std::path::Path;

    use super::{Nesting, nest, nested_path, nested_path_is};
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
        let c = nested.iter().position(|t| t.tile.name == "c").unwrap();
        let path = nested_path(&nested, &board.tiles, c);
        assert_eq!(path, ["big", "sub", "c"]);
        assert!(nested_path_is(&nested, &board.tiles, c, &path));
        assert!(!nested_path_is(&nested, &board.tiles, c, &path[..2]));
        let sub = nested.iter().position(|t| t.tile.name == "sub").unwrap();
        assert_eq!(nested_path(&nested, &board.tiles, sub), ["big", "sub"]);
        assert!(!nested_path_is(&nested, &board.tiles, sub, &path));
        // Parents come before their children, for painting.
        let sub_at = nested.iter().position(|t| t.tile.name == "sub").unwrap();
        let c_at = nested.iter().position(|t| t.tile.name == "c").unwrap();
        assert!(sub_at < c_at);
    }

    #[test]
    fn the_largest_entries_are_the_head_of_the_whole_listing() {
        use crate::tiles::{files_in_folder, largest_in_folder};
        // Equal sizes rank by name, as the whole listing sorts them; a limit in the middle of
        // a run of equals must take the first of them by name.
        let root = Path::new("/r");
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        for (name, size, is_dir) in [
            ("x", 500, false),
            ("t3", 100, false),
            ("t1", 100, false),
            ("t4", 100, false),
            ("t2", 100, false),
            ("small", 10, false),
        ] {
            tree.add_entry(meta(size, is_dir), &root.join(name));
        }
        let folder = tree.get_current_folder();
        let whole = files_in_folder(folder, 0, SizeKind::Disk);
        let names: Vec<_> = whole
            .iter()
            .map(|f| f.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["x", "t1", "t2", "t3", "t4", "small"]);
        for limit in 0..=7 {
            let top = largest_in_folder(folder, SizeKind::Disk, limit);
            assert_eq!(top, whole[..limit.min(whole.len())], "limit {limit}");
        }
        // Ranked a piece at a time — the tiles' head, then a corner's more — it is the same
        // order as ranked whole.
        for head in 0..=6 {
            for more in 0..=6 {
                let mut ranking = crate::tiles::Ranking::new(folder, SizeKind::Disk, 0.0);
                let files = ranking.head(head, 0.0);
                assert_eq!(files.len(), head.min(whole.len()));
                let shares = ranking.shares(files.len(), files.len() + more);
                let got: Vec<_> = (0..files.len())
                    .map(|index| ranking.name(index))
                    .chain(shares.iter().map(|share| share.name))
                    .collect();
                let want: Vec<_> = whole
                    .iter()
                    .take(head + more)
                    .map(|f| f.name.as_os_str())
                    .collect();
                assert_eq!(got, want, "head {head}, then {more}");
            }
        }
    }

    #[test]
    fn past_the_deadline_the_corners_specks_wait_for_the_second_pass() {
        use super::nest_with;
        use crate::tiles::Grid;
        // `many/`: one large file and a thousand small ones, which only a corner can show.
        let root = Path::new("/r");
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        tree.add_entry(meta(0, true), &root.join("many"));
        tree.add_entry(meta(1_000_000, false), &root.join("many/large"));
        for index in 0..1000 {
            tree.add_entry(meta(10, false), &root.join(format!("many/small{index}")));
        }
        tree.add_entry(meta(1_000, false), &root.join("other"));
        let folder = tree.get_current_folder();
        let grid = Grid::pixels(4);
        let mut board = Board::new(folder);
        board.set_grid(grid);
        board.change_area(&Area {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        });
        board.change_files(folder);
        let nesting = Nesting {
            grid,
            label_rows: 0,
            margin: 1,
            dust: true,
            ..Nesting::default()
        };
        let mut specks = 0;
        let whole = nest_with(folder, &board.tiles, SizeKind::Disk, &nesting, &mut |_| {
            specks += 1;
        });
        assert!(whole.complete && specks > 0, "{specks} specks");
        let late = Nesting {
            dust_deadline: Some(::std::time::Instant::now()),
            ..nesting
        };
        let mut late_specks = 0;
        let cut = nest_with(folder, &board.tiles, SizeKind::Disk, &late, &mut |_| {
            late_specks += 1;
        });
        assert!(!cut.complete, "the specks are owed");
        assert_eq!(late_specks, 0);
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

    #[test]
    fn a_folder_too_short_for_its_label_holds_its_entries_under_its_margin() {
        use crate::tiles::{Grid, Tile};
        let tree = tree();
        let pixels = Nesting {
            label_rows: 18,
            margin: 2,
            grid: Grid::pixels(4),
            ..Nesting::default()
        };
        let folder = tree.get_current_folder();
        let big = |height: u16| Tile {
            x: 0,
            y: 0,
            width: 200,
            height,
            name: "big".into(),
            size: 1150,
            descendants: Some(4),
            percentage: 1.0,
            file_type: crate::tiles::FileType::Folder,
        };
        // Tall enough for the label band: the entries start under it.
        let tall = [big(120)];
        assert!(pixels.labelled(&tall[0]));
        let nested = nest(folder, &tall, SizeKind::Disk, &pixels);
        assert!(!nested.is_empty());
        let top = nested
            .iter()
            .filter(|t| t.depth == 1)
            .map(|t| t.tile.y)
            .min();
        assert_eq!(top, Some(18));
        // Too short for it: no label, the entries right under the margin.
        let short = [big(20)];
        assert!(!pixels.labelled(&short[0]));
        let nested = nest(folder, &short, SizeKind::Disk, &pixels);
        assert!(
            !nested.is_empty(),
            "a short folder still shows what is in it"
        );
        let top = nested
            .iter()
            .filter(|t| t.depth == 1)
            .map(|t| t.tile.y)
            .min();
        assert_eq!(top, Some(2));
        for t in &nested {
            assert!(t.tile.y + t.tile.height <= 20 - 2, "{:?}", t.tile);
        }
        // Too short for two of the least tile: nothing inside.
        assert!(nest(folder, &[big(11)], SizeKind::Disk, &pixels).is_empty());
    }

    #[test]
    fn a_deadline_cuts_the_deepest_levels_and_a_cap_the_largest_folders() {
        use super::nest_with;
        let tree = tree();
        let board = board(&tree, 200, 80);
        let folder = tree.get_current_folder();
        let whole = nest_with(
            folder,
            &board.tiles,
            SizeKind::Disk,
            &Nesting::default(),
            &mut |_| {},
        );
        let (complete, whole) = (whole.complete, whole.tiles);
        assert!(complete && whole.iter().any(|t| t.depth == 2));
        // Past its deadline: the top-level folders' own entries, and no deeper.
        let late = Nesting {
            deadline: Some(::std::time::Instant::now()),
            ..Nesting::default()
        };
        let cut = nest_with(folder, &board.tiles, SizeKind::Disk, &late, &mut |_| {});
        let (complete, cut) = (cut.complete, cut.tiles);
        assert!(!complete);
        assert!(
            !cut.is_empty() && cut.iter().all(|t| t.depth == 1),
            "{cut:?}"
        );
        // The same tiles as the whole nesting's first level: a level at a time, parents first.
        let first: Vec<_> = whole
            .iter()
            .filter(|t| t.depth == 1)
            .map(|t| &t.tile.name)
            .collect();
        let cut_names: Vec<_> = cut.iter().map(|t| &t.tile.name).collect();
        assert_eq!(first, cut_names);
        // A cap under a folder's entries leaves some out, and says so.
        let capped = Nesting {
            room_cap: 1,
            ..Nesting::default()
        };
        let few = nest_with(folder, &board.tiles, SizeKind::Disk, &capped, &mut |_| {});
        let (complete, few) = (few.complete, few.tiles);
        assert!(!complete);
        assert!(few.iter().filter(|t| t.depth == 1).count() <= 1, "{few:?}");
    }

    #[test]
    fn a_folders_inside_holds_its_entries_and_the_rest_of_it_is_what_to_fill() {
        use super::nest_with;
        let tree = tree();
        let board = board(&tree, 200, 80);
        let folder = tree.get_current_folder();
        let nested = nest_with(
            folder,
            &board.tiles,
            SizeKind::Disk,
            &Nesting::default(),
            &mut |_| {},
        );
        let big = board.tiles.iter().position(|t| t.name == "big").unwrap();
        let inside = nested.tops[big].expect("big's entries were laid out in it");
        let tile = &board.tiles[big];
        let area = |a: &Area| u32::from(a.width) * u32::from(a.height);
        // The parts to fill and the inside make up the tile, the corner being in the inside.
        let around: u32 = inside.around(&tile.area())[..4].iter().map(area).sum();
        assert_eq!(around + area(&inside.area), area(&tile.area()));
        // Its entries lie in the inside, clear of the corner.
        for t in nested.tiles.iter().filter(|t| t.depth == 1) {
            let t = &t.tile;
            assert!(t.x >= inside.area.x && t.x + t.width <= inside.area.x + inside.area.width);
            assert!(t.y >= inside.area.y && t.y + t.height <= inside.area.y + inside.area.height);
            let corner = inside.corner;
            let apart = t.x >= corner.x + corner.width
                || t.x + t.width <= corner.x
                || t.y >= corner.y + corner.height
                || t.y + t.height <= corner.y;
            assert!(apart || area(&corner) == 0, "{t:?} in {corner:?}");
        }
        // A folder with no entries laid out has none.
        let medium = board
            .tiles
            .iter()
            .position(|t| t.name == "medium.txt")
            .unwrap();
        assert_eq!(nested.tops[medium], None);
    }
}
