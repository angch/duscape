use ::std::hash::BuildHasher;

use crate::model::files::hash::FastBuildHasher;
use crate::model::{Folder, SizeKind};
use crate::tiles::Area;
use crate::tiles::files_in_folder::FileType;
use crate::tiles::{
    FileMetadata, Grid, Plan, Share, Speck, Tile, TreeMap, files_in_folder, scatter,
};

/// The free space of a volume, shown as a tile beside its root's entries: how much, and its
/// share of the board — free over free and used — which the entries' shares make room for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeSpace {
    pub bytes: u64,
    pub share: f64,
}

/// The name of the free space's tile. A real entry so named at a volume's root would share it;
/// the board tells the two apart by index ([`Board::free_tile`]), the listing never holds the
/// free space, and a painter labels the tile by its name as it labels a file's.
pub const FREE_SPACE_NAME: &str = "Free space";

pub struct Board {
    pub tiles: Vec<Tile>,
    pub unrenderable_tile_coordinates: Option<(u16, u16)>,
    /// The entries that got no tile, by their index in `files`.
    hidden: Vec<usize>,
    /// The volume's free space to show beside the entries, unzoomed; see [`Self::set_free_space`].
    free: Option<FreeSpace>,
    /// The free space's tile in `tiles`, when it got one.
    free_index: Option<usize>,
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
    /// from the next [`Self::change_files`] on, unzoomed only: the entries' shares shrink to
    /// make its room, so the board shows the whole volume in proportion. `None` shows the
    /// entries alone, as the board does by default. Not an entry: the listing never holds it,
    /// the selection never lands on it, and a hit on it is nothing.
    pub fn set_free_space(&mut self, free: Option<FreeSpace>) {
        self.free = free.filter(|free| free.bytes > 0 && free.share > 0.0 && free.share < 1.0);
    }
    /// The free space's tile, when one is laid out: a painter colours and a viewer ignores it.
    #[must_use]
    pub fn free_tile(&self) -> Option<usize> {
        self.free_index
    }
    /// Whether the tile at `index` is an entry's, which the selection may land on.
    fn selectable(&self, index: usize) -> bool {
        self.free_index != Some(index)
    }
    /// The first entry's tile, when there is one: where the selection goes from nothing.
    fn select_first(&mut self) {
        if let Some(index) = (0..self.tiles.len()).find(|&index| self.selectable(index)) {
            self.set_selected_index(&index);
        }
    }
    /// The free space's entry among `files`, when it is to be shown, the others' shares scaled
    /// down to leave it its own.
    fn add_free_space(free: Option<FreeSpace>, files: &mut Vec<FileMetadata>) {
        let Some(free) = free else {
            return;
        };
        for file in files.iter_mut() {
            file.percentage *= 1.0 - free.share;
        }
        // In rank order: the files come largest first, and the layout counts on it — appended
        // after them, the largest entry of all got no tile.
        let at = files
            .iter()
            .position(|file| file.percentage < free.share)
            .unwrap_or(files.len());
        files.insert(
            at,
            FileMetadata {
                name: FREE_SPACE_NAME.into(),
                size: u128::from(free.bytes),
                descendants: None,
                percentage: free.share,
                file_type: FileType::File,
            },
        );
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
    /// The entries on the board at the zoom: the listing, with the free space when it is to
    /// be shown, or the zoom's fewer.
    fn refile(&mut self, folder: &Folder) {
        self.files = if self.zoom_level == 0 {
            self.listing.clone()
        } else {
            files_in_folder(folder, self.zoom_level, self.kind)
        };
        if self.zoom_level == 0 {
            Self::add_free_space(self.free, &mut self.files);
        }
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
        let Some((x, y)) = self.unrenderable_tile_coordinates else {
            return;
        };
        let corner = Area {
            x,
            y,
            width: (self.area.x + self.area.width).saturating_sub(x),
            height: (self.area.y + self.area.height).saturating_sub(y),
        };
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
                rank: self.zoom_level + self.tiles.len() + mote.entry,
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
        let mut tree_map = TreeMap::with_grid(&self.area, self.grid);
        let hasher = FastBuildHasher::default();
        let files = &self.files;
        self.plan = tree_map.populate_steady(
            files.iter().collect(),
            &mut |index| hasher.hash_one(&files[index].name),
            plan,
        );
        self.tiles = tree_map.tiles;
        self.unrenderable_tile_coordinates = tree_map.unrenderable_tile_coordinates;
        self.hidden = tree_map.hidden;
        // By name, since the layout ranks the files: the free space may be the largest.
        self.free_index = self.free.and_then(|_| {
            self.tiles
                .iter()
                .position(|tile| tile.name == FREE_SPACE_NAME)
        });
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
            .filter(|(_, tile)| tile.file_type == FileType::Folder)
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
