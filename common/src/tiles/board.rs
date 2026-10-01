use ::std::hash::BuildHasher;

use crate::model::files::hash::FastBuildHasher;
use crate::model::{Folder, SizeKind};
use crate::tiles::Area;
use crate::tiles::files_in_folder::FileType;
use crate::tiles::{
    FileMetadata, Grid, Plan, Share, Speck, Tile, TreeMap, files_in_folder, scatter,
};

/// The free space of a volume, shown as a tile beside its root's entries: how much, and its
/// share of the board — free over the volume's size. Beside it, the space the volume says is
/// used and the scan has not found (yet): how much, and its share likewise. The entries have
/// what is left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeSpace {
    pub bytes: u64,
    pub share: f64,
    pub unscanned: u64,
    pub unscanned_share: f64,
    /// Whether the scan is over: the unscanned space is then what no walk could see, not what
    /// is still to be walked, and is named so ([`UNSEEN_NAME`]).
    pub scanned: bool,
}

/// The name of the free space's tile. A real entry so named at a volume's root would share it;
/// the board tells the two apart by index ([`Board::free_tile`]), the listing never holds the
/// free space, and a painter labels the tile by its name as it labels a file's.
pub const FREE_SPACE_NAME: &str = "Free space";
/// The name of the tile of the used space the scan has not found ([`Board::unscanned_tile`]):
/// what is still to be walked, while the scan goes.
pub const UNSCANNED_NAME: &str = "Unscanned";
/// [`UNSCANNED_NAME`] once the scan is over: what is left is what no walk could see.
pub const UNSEEN_NAME: &str = "Not seen by the scan";

pub struct Board {
    pub tiles: Vec<Tile>,
    pub unrenderable_tile_coordinates: Option<(u16, u16)>,
    /// The entries that got no tile, by their index in `files`.
    hidden: Vec<usize>,
    /// The volume's free space to show beside the entries, unzoomed; see [`Self::set_free_space`].
    free: Option<FreeSpace>,
    /// The free space's tile in `tiles`, when it got one.
    free_index: Option<usize>,
    /// The unscanned space's tile in `tiles`, when it got one.
    unscanned_index: Option<usize>,
    /// Where the entries are laid out: the area, less the free and unscanned space's strip.
    entries_area: Area,
    pub selected_index: Option<usize>, // None means nothing is selected
    pub previous_indices_and_zoom_level: Vec<(Option<usize>, usize)>, // Stack of previous stats
    pub zoom_level: usize,
    area: Area,
    files: Vec<FileMetadata>,
    /// Every entry of the folder, largest first, whatever the zoom: the list beside the board.
    listing: Vec<FileMetadata>,
    /// Which size the tiles are drawn by; see [`Self::show`].
    kind: SizeKind,
    /// The cells the area is in; see [`Self::set_grid`].
    grid: Grid,
    /// Counts the layouts: what a viewer keeps derived from the tiles (a nesting) is stale
    /// when it has moved on.
    generation: u64,
    /// How the last layout cut the area, for [`Self::change_files_steady`] to keep.
    plan: Plan,
}

impl Board {
    pub fn new(folder: &Folder) -> Self {
        Board {
            tiles: vec![],
            unrenderable_tile_coordinates: None,
            hidden: Vec::new(),
            free: None,
            free_index: None,
            unscanned_index: None,
            entries_area: Area::default(),
            files: files_in_folder(folder, 0, SizeKind::Disk),
            listing: files_in_folder(folder, 0, SizeKind::Disk),
            kind: SizeKind::Disk,
            selected_index: None,
            previous_indices_and_zoom_level: vec![],
            zoom_level: 0,
            area: Area::default(),
            grid: Grid::TERMINAL,
            generation: 0,
            plan: Plan::default(),
        }
    }
    /// Lay the tiles out in `grid`'s cells: a terminal's (the default), or a window's pixels.
    pub fn set_grid(&mut self, grid: Grid) {
        if self.grid != grid {
            self.grid = grid;
            self.fill();
        }
    }
    /// Draw tiles by the size of `kind` from the next [`Self::change_files`] on.
    pub fn show(&mut self, kind: SizeKind) {
        self.kind = kind;
    }
    /// Show `free`, a volume's free space, as a tile of its own beside the folder's entries
    /// from the next [`Self::change_files`] on, unzoomed only, so the board shows the whole
    /// volume in proportion: the free space at the bottom right, the unscanned space beside it,
    /// the entries in the rest. `None` shows the entries alone, as the board does by default.
    /// Neither is an entry: the listing never holds them, the selection never lands on them,
    /// and a hit on one is nothing.
    pub fn set_free_space(&mut self, free: Option<FreeSpace>) {
        self.free = free.filter(|free| {
            free.bytes > 0
                && free.share > 0.0
                && free.unscanned_share >= 0.0
                && free.share + free.unscanned_share <= 1.0
        });
    }
    /// The free space's tile, when one is laid out: a painter colours and a viewer ignores it.
    #[must_use]
    pub fn free_tile(&self) -> Option<usize> {
        self.free_index
    }
    /// The unscanned space's tile, when one is laid out: likewise no entry.
    #[must_use]
    pub fn unscanned_tile(&self) -> Option<usize> {
        self.unscanned_index
    }
    /// Whether the tile at `index` is an entry's, which the selection may land on.
    #[must_use]
    pub fn selectable(&self, index: usize) -> bool {
        self.free_index != Some(index) && self.unscanned_index != Some(index)
    }
    /// The "small files" corner, when entries got no tile: from where the treemap put it to
    /// the far corner of the entries' area — never over the free space's strip.
    #[must_use]
    pub fn corner(&self) -> Option<Area> {
        let (x, y) = self.unrenderable_tile_coordinates?;
        let area = self.entries_area;
        Some(Area {
            x,
            y,
            width: (area.x + area.width).saturating_sub(x),
            height: (area.y + area.height).saturating_sub(y),
        })
    }
    /// The first entry's tile, when there is one: where the selection goes from nothing.
    fn select_first(&mut self) {
        if let Some(index) = (0..self.tiles.len()).find(|&index| self.selectable(index)) {
            self.set_selected_index(&index);
        }
    }
    pub fn change_files(&mut self, folder: &Folder) {
        self.relist(folder);
        self.fill();
    }
    /// [`Self::change_files`] for the same folder with new sizes (a scan's outline, the
    /// finished tree, a rescan): the last layout's rows are kept where they still fit
    /// ([`TreeMap::populate_steady`]), so the tiles grow and shrink in place.
    pub fn change_files_steady(&mut self, folder: &Folder) {
        self.relist(folder);
        let plan = ::std::mem::take(&mut self.plan);
        self.lay_out(Some(&plan));
    }
    fn relist(&mut self, folder: &Folder) {
        self.listing = files_in_folder(folder, 0, self.kind);
        self.refile(folder);
    }
    /// The entries on the board at the zoom: the listing, or the zoom's fewer.
    fn refile(&mut self, folder: &Folder) {
        self.files = if self.zoom_level == 0 {
            self.listing.clone()
        } else {
            files_in_folder(folder, self.zoom_level, self.kind)
        };
    }
    /// Every entry of the folder on the board, largest first, including those the zoom leaves
    /// off it and those too small for a tile of their own.
    pub fn listing(&self) -> &[FileMetadata] {
        &self.listing
    }
    /// The entries on the board that got no tile — in the "small files" corner — largest
    /// first.
    #[must_use]
    pub fn hidden(&self) -> Vec<&FileMetadata> {
        self.hidden
            .iter()
            .map(|&index| &self.files[index])
            .collect()
    }
    /// The entries in the "small files" corner laid out in it as specks ([`scatter`]), each
    /// handed to `speck`: in a pixel grid, where a speck is a pixel or more.
    pub fn scatter_corner(&self, speck: &mut dyn FnMut(Speck)) {
        let Some(corner) = self.corner() else {
            return;
        };
        let entry_tiles = self.tiles.len()
            - usize::from(self.free_index.is_some())
            - usize::from(self.unscanned_index.is_some());
        let hidden: Vec<Share> = self
            .hidden
            .iter()
            .map(|&index| {
                let file = &self.files[index];
                Share {
                    name: &file.name,
                    percentage: file.percentage,
                    file_type: file.file_type,
                }
            })
            .collect();
        let shares: Vec<f64> = hidden.iter().map(|share| share.percentage).collect();
        for mote in scatter(&shares, &corner) {
            speck(Speck {
                area: mote.area,
                entry: &hidden[mote.entry],
                rank: self.zoom_level + entry_tiles + mote.entry,
                depth: 0,
            });
        }
    }
    /// Where the selected tile's entry sits in [`Self::listing`].
    pub fn selected_listing_index(&self) -> Option<usize> {
        let selected = &self.currently_selected()?.name;
        self.listing
            .iter()
            .position(|entry| &entry.name == selected)
    }
    pub fn change_area(&mut self, area: &Area) {
        if self.area != *area {
            self.area = *area;
            self.fill();
        }
    }
    /// Which layout the tiles are: it changes with every one.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
    fn fill(&mut self) {
        self.lay_out(None);
    }
    fn lay_out(&mut self, plan: Option<&Plan>) {
        self.generation += 1;
        let free = self.free.filter(|_| self.zoom_level == 0);
        let (entries, strip) = match free {
            Some(free) => self.cut_strip(&free),
            None => (self.area, None),
        };
        self.entries_area = entries;
        let mut tree_map = TreeMap::with_grid(&entries, self.grid);
        if entries.width > 0 && entries.height > 0 {
            let hasher = FastBuildHasher::default();
            let files = &self.files;
            self.plan = tree_map.populate_steady(
                files.iter().collect(),
                &mut |index| hasher.hash_one(&files[index].name),
                plan,
            );
        } else {
            self.plan = Plan::default();
        }
        self.tiles = tree_map.tiles;
        self.unrenderable_tile_coordinates = tree_map.unrenderable_tile_coordinates;
        self.hidden = tree_map.hidden;
        self.free_index = None;
        self.unscanned_index = None;
        // After the entries' tiles, so their indices are the treemap's.
        if let (Some(free), Some((unscanned, free_area))) = (free, strip) {
            if let Some(area) = unscanned {
                self.unscanned_index = Some(self.tiles.len());
                self.tiles.push(Tile::at(
                    &area,
                    &Self::space_entry(
                        if free.scanned {
                            UNSEEN_NAME
                        } else {
                            UNSCANNED_NAME
                        },
                        free.unscanned,
                        free.unscanned_share,
                    ),
                ));
            }
            self.free_index = Some(self.tiles.len());
            self.tiles.push(Tile::at(
                &free_area,
                &Self::space_entry(FREE_SPACE_NAME, free.bytes, free.share),
            ));
        }
    }
    /// The area cut for the free space: a strip along the far side of the longer way — the
    /// right of a wide board, the bottom of a tall one — as wide as the free and unscanned
    /// space's share, the free space at its far end (the bottom right) and the unscanned
    /// space before it. Cut first, not ranked among the entries, so the free space keeps its
    /// place and its area whatever the scan finds and whichever folder is largest: as the scan
    /// goes, the unscanned space shrinks and the entries grow into it. Returns the entries'
    /// area, and the strip's two parts (the unscanned one `None` when it rounds to nothing).
    fn cut_strip(&self, free: &FreeSpace) -> (Area, Option<(Option<Area>, Area)>) {
        let area = self.area;
        let tail = free.share + free.unscanned_share;
        // How far into the strip the free space starts, as a share of it.
        let unscanned_part = if tail > 0.0 {
            free.unscanned_share / tail
        } else {
            0.0
        };
        let wide = f64::from(area.width) >= f64::from(area.height) * self.grid.ratio;
        let cut = |length: u16, share: f64| -> u16 {
            ((f64::from(length) * share).round() as u16).clamp(1, length.max(1))
        };
        if wide {
            let strip = cut(area.width, tail).min(area.width);
            let x = area.x + area.width - strip;
            let gap = (f64::from(area.height) * unscanned_part).round() as u16;
            let gap = gap.min(area.height.saturating_sub(1));
            let entries = Area {
                width: area.width - strip,
                ..area
            };
            let unscanned = (gap > 0).then_some(Area {
                x,
                y: area.y,
                width: strip,
                height: gap,
            });
            let free = Area {
                x,
                y: area.y + gap,
                width: strip,
                height: area.height - gap,
            };
            (entries, Some((unscanned, free)))
        } else {
            let strip = cut(area.height, tail).min(area.height);
            let y = area.y + area.height - strip;
            let gap = (f64::from(area.width) * unscanned_part).round() as u16;
            let gap = gap.min(area.width.saturating_sub(1));
            let entries = Area {
                height: area.height - strip,
                ..area
            };
            let unscanned = (gap > 0).then_some(Area {
                x: area.x,
                y,
                width: gap,
                height: strip,
            });
            let free = Area {
                x: area.x + gap,
                y,
                width: area.width - gap,
                height: strip,
            };
            (entries, Some((unscanned, free)))
        }
    }
    /// The free or unscanned space as what a tile is made of.
    fn space_entry(name: &str, bytes: u64, share: f64) -> FileMetadata {
        FileMetadata {
            name: name.into(),
            size: u128::from(bytes),
            descendants: None,
            percentage: share,
            file_type: FileType::File,
        }
    }
    pub fn get_selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub fn set_selected_index(&mut self, next_index: &usize) {
        self.selected_index = Some(*next_index);
    }
    pub fn has_selected_index(&self) -> bool {
        self.selected_index.is_some()
    }
    pub fn reset_selected_index(&mut self) {
        self.selected_index = None;
    }
    pub fn currently_selected(&self) -> Option<&Tile> {
        match &self.selected_index {
            Some(selected_index) => self.tiles.get(*selected_index),
            None => None,
        }
    }
    pub fn pop_previous_index_and_zoom_level(&mut self) -> Option<(Option<usize>, usize)> {
        self.previous_indices_and_zoom_level.pop()
    }
    pub fn move_to_largest_folder(&mut self) {
        let next_index = self
            .tiles
            .iter()
            .enumerate()
            .filter(|&(index, tile)| tile.file_type == FileType::Folder && self.selectable(index))
            .map(|(index, _)| index)
            .next();

        if let Some(index) = next_index {
            self.set_selected_index(&index);
        }
    }
    pub fn move_selected_right(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|&(index, c)| {
                        self.selectable(index)
                            && c.is_directly_right_of(currently_selected)
                            && c.horizontally_overlaps_with(currently_selected)
                    })
                    // get the index of the tile with the most overlap with currently selected
                    .max_by_key(|(_, c)| c.get_horizontal_overlap_with(currently_selected))
                    .map(|(index, _)| index);
                match next_index {
                    Some(i) => self.set_selected_index(&i),
                    None => self.reset_selected_index(), // move off the edge of the screen resets selection
                }
            }
            None => self.select_first(),
        }
    }
    pub fn move_selected_left(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|&(index, c)| {
                        self.selectable(index)
                            && c.is_directly_left_of(currently_selected)
                            && c.horizontally_overlaps_with(currently_selected)
                    })
                    // get the index of the tile with the most overlap with currently selected
                    .max_by_key(|(_, c)| c.get_horizontal_overlap_with(currently_selected))
                    .map(|(index, _)| index);
                match next_index {
                    Some(i) => self.set_selected_index(&i),
                    None => self.reset_selected_index(), // move off the edge of the screen resets selection
                }
            }
            None => self.select_first(),
        }
    }
    pub fn move_selected_down(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|&(index, c)| {
                        self.selectable(index)
                            && c.is_directly_below(currently_selected)
                            && c.vertically_overlaps_with(currently_selected)
                    })
                    // get the index of the tile with the most overlap with currently selected
                    .max_by_key(|(_, c)| c.get_vertical_overlap_with(currently_selected))
                    .map(|(index, _)| index);
                match next_index {
                    Some(i) => self.set_selected_index(&i),
                    None => self.reset_selected_index(), // move off the edge of the screen resets selection
                }
            }
            None => self.select_first(),
        }
    }
    pub fn move_selected_up(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|&(index, c)| {
                        self.selectable(index)
                            && c.is_directly_above(currently_selected)
                            && c.vertically_overlaps_with(currently_selected)
                    })
                    // get the index of the tile with the most overlap with currently selected
                    .max_by_key(|(_, c)| c.get_vertical_overlap_with(currently_selected))
                    .map(|(index, _)| index);
                match next_index {
                    Some(i) => self.set_selected_index(&i),
                    None => self.reset_selected_index(), // move off the edge of the screen resets selection
                }
            }
            None => self.select_first(),
        }
    }
    pub fn zoom_in(&mut self, folder: &Folder) {
        if self.zoom_level < self.files.len() {
            self.zoom_level += 1;
            self.refile(folder);
            self.fill();
        }
    }
    pub fn zoom_out(&mut self, folder: &Folder) {
        if self.zoom_level > 0 {
            self.zoom_level -= 1;
            self.refile(folder);
            self.fill();
        }
    }
    pub fn reset_zoom(&mut self, folder: &Folder) {
        self.zoom_level = 0;
        self.refile(folder);
        self.fill();
    }
    pub fn reset_zoom_index(&mut self) {
        self.zoom_level = 0;
    }
    pub fn set_zoom_index(&mut self, index: usize) {
        self.zoom_level = index;
    }
    /// The tile drawn at a screen cell, for mouse selection.
    ///
    /// Tiles are laid out in screen coordinates and neighbours share a border line, so each tile
    /// owns its top and left borders and the half-open span to its right and bottom: every cell
    /// belongs to at most one tile. Cells outside every tile — past the last border, or in the
    /// "small files" corner — belong to none.
    pub fn tile_at(&self, column: u16, row: u16) -> Option<usize> {
        self.tiles.iter().position(|tile| {
            (tile.x..tile.x.saturating_add(tile.width)).contains(&column)
                && (tile.y..tile.y.saturating_add(tile.height)).contains(&row)
        })
    }
    pub fn record_current_index_and_zoom_level(&mut self) {
        self.previous_indices_and_zoom_level
            .push((self.get_selected_index(), self.zoom_level));
    }
}
