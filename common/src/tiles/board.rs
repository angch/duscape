use crate::model::{Folder, SizeKind};
use crate::tiles::Area;
use crate::tiles::files_in_folder::FileType;
use crate::tiles::{FileMetadata, Tile, TreeMap, files_in_folder};

pub struct Board {
    pub tiles: Vec<Tile>,
    pub unrenderable_tile_coordinates: Option<(u16, u16)>,
    pub selected_index: Option<usize>, // None means nothing is selected
    pub previous_indices_and_zoom_level: Vec<(Option<usize>, usize)>, // Stack of previous stats
    pub zoom_level: usize,
    area: Area,
    files: Vec<FileMetadata>,
    /// Every entry of the folder, largest first, whatever the zoom: the list beside the board.
    listing: Vec<FileMetadata>,
    /// Which size the tiles are drawn by; see [`Self::show`].
    kind: SizeKind,
}

impl Board {
    pub fn new(folder: &Folder) -> Self {
        Board {
            tiles: vec![],
            unrenderable_tile_coordinates: None,
            files: files_in_folder(folder, 0, SizeKind::Disk),
            listing: files_in_folder(folder, 0, SizeKind::Disk),
            kind: SizeKind::Disk,
            selected_index: None,
            previous_indices_and_zoom_level: vec![],
            zoom_level: 0,
            area: Area::default(),
        }
    }
    /// Draw tiles by the size of `kind` from the next [`Self::change_files`] on.
    pub fn show(&mut self, kind: SizeKind) {
        self.kind = kind;
    }
    pub fn change_files(&mut self, folder: &Folder) {
        self.listing = files_in_folder(folder, 0, self.kind);
        self.files = if self.zoom_level == 0 {
            self.listing.clone()
        } else {
            files_in_folder(folder, self.zoom_level, self.kind)
        };
        self.fill();
    }
    /// Every entry of the folder on the board, largest first, including those the zoom leaves
    /// off it and those too small for a tile of their own.
    pub fn listing(&self) -> &[FileMetadata] {
        &self.listing
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
    fn fill(&mut self) {
        let mut tree_map = TreeMap::new(&self.area);
        tree_map.populate_tiles(self.files.iter().collect());
        self.tiles = tree_map.tiles;
        self.unrenderable_tile_coordinates = tree_map.unrenderable_tile_coordinates;
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
                    .filter(|(_, c)| {
                        c.is_directly_right_of(currently_selected)
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
            None => self.set_selected_index(&0),
        }
    }
    pub fn move_selected_left(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        c.is_directly_left_of(currently_selected)
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
            None => self.set_selected_index(&0),
        }
    }
    pub fn move_selected_down(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        c.is_directly_below(currently_selected)
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
            None => self.set_selected_index(&0),
        }
    }
    pub fn move_selected_up(&mut self) {
        match self.currently_selected() {
            Some(currently_selected) => {
                let next_index = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        c.is_directly_above(currently_selected)
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
            None => self.set_selected_index(&0),
        }
    }
    pub fn zoom_in(&mut self, folder: &Folder) {
        if self.zoom_level < self.files.len() {
            self.zoom_level += 1;
            self.files = files_in_folder(folder, self.zoom_level, self.kind);
            self.fill();
        }
    }
    pub fn zoom_out(&mut self, folder: &Folder) {
        if self.zoom_level > 0 {
            self.zoom_level -= 1;
            self.files = files_in_folder(folder, self.zoom_level, self.kind);
            self.fill();
        }
    }
    pub fn reset_zoom(&mut self, folder: &Folder) {
        self.zoom_level = 0;
        self.files = files_in_folder(folder, self.zoom_level, self.kind);
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
