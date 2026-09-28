use ::std::collections::HashMap;

use crate::model::files::hash::FastBuildHasher;
use crate::tiles::{Area, FileMetadata, RectFloat, Tile};

const HEIGHT_WIDTH_RATIO: f64 = 2.5;
pub(crate) const MINIMUM_HEIGHT: u16 = 3;
pub(crate) const MINIMUM_WIDTH: u16 = 8;

/// The least the "small files" placeholder may occupy, so that hidden entries always leave a
/// visible trace: a border plus at least one row of one or two cells inside it.
const SMALL_FILES_MINIMUM_HEIGHT: u16 = 3;
const SMALL_FILES_MINIMUM_WIDTH: u16 = 4;

/// The cells a treemap is laid out in: their shape, and the least a tile may be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// How much taller than wide a cell is: 2.5 for a terminal's, 1.0 for a screen's pixels.
    pub ratio: f64,
    /// The least a tile measures, in cells; an entry smaller goes to the "small files" corner.
    pub min_width: u16,
    pub min_height: u16,
    /// The least that corner measures, so hidden entries always leave a trace.
    pub small_files_width: u16,
    pub small_files_height: u16,
}

impl Grid {
    /// A terminal's character cells: a tile holds a line of text in a border, 8×3.
    pub const TERMINAL: Grid = Grid {
        ratio: HEIGHT_WIDTH_RATIO,
        min_width: MINIMUM_WIDTH,
        min_height: MINIMUM_HEIGHT,
        small_files_width: SMALL_FILES_MINIMUM_WIDTH,
        small_files_height: SMALL_FILES_MINIMUM_HEIGHT,
    };

    /// Square cells of one device pixel, where a tile is worth drawing once it is `min` pixels
    /// either way: what a window lays out in, so it shows every entry its screen can.
    #[must_use]
    pub fn pixels(min: u16) -> Grid {
        let min = min.max(1);
        Grid {
            ratio: 1.0,
            min_width: min,
            min_height: min,
            small_files_width: min,
            small_files_height: min,
        }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Grid::TERMINAL
    }
}

/// How a layout cut its area into rows, for the next layout of the same folder to keep: each
/// row's entries by key, in order, and which way the row ran. See
/// [`TreeMap::populate_steady`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    rows: Vec<(bool, Vec<u64>)>,
}

impl Plan {
    /// How many entries it placed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.iter().map(|(_, keys)| keys.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A steady layout's shapes are kept while every tile is at least this square: squarify
/// makes them better than 1:3 and a steady one drifts, so past 1:5 it lays out afresh.
const STEADY_WORST_RATIO: f64 = 0.2;
/// Nor while entries the plan did not have take more than this share of the area.
const STEADY_NEW_SHARE: f64 = 0.25;

/// What became of each steady layout's plan, for `tests/layout_stability.rs` to report: kept,
/// dropped for new entries, for an entry too small for a tile, for a shape. Relaxed counters,
/// a few a relayout.
pub static STEADY_OUTCOMES: [::std::sync::atomic::AtomicUsize; 4] =
    [const { ::std::sync::atomic::AtomicUsize::new(0) }; 4];
fn note(outcome: usize) {
    STEADY_OUTCOMES[outcome].fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
}

/// What came of following a plan.
enum Followed {
    Yes,
    No,
    /// Not with these entries in it: too small now for a tile.
    Without(Vec<u64>),
}

pub struct TreeMap {
    pub tiles: Vec<Tile>,
    /// Top-left corner of the "small files" placeholder, which extends to the bottom-right of
    /// the board. `None` when every entry got a tile of its own.
    pub unrenderable_tile_coordinates: Option<(u16, u16)>,
    /// The children that got no tile, by their index in what was laid out, in order.
    pub hidden: Vec<usize>,
    /// Which child each tile is, by the same index.
    tile_entries: Vec<usize>,
    /// The rows laid out, each's direction and the children in it given a tile.
    rows: Vec<(bool, Vec<usize>)>,
    bounds: Area,
    empty_space: RectFloat,
    total_size: f64,
    grid: Grid,
}
impl TreeMap {
    pub fn new(bounds: &Area) -> Self {
        Self::with_grid(bounds, Grid::TERMINAL)
    }
    /// A treemap over `bounds` in `grid`'s cells.
    pub fn with_grid(bounds: &Area, grid: Grid) -> Self {
        let empty_space = RectFloat::new(bounds);
        TreeMap {
            tiles: vec![],
            unrenderable_tile_coordinates: None,
            hidden: Vec::new(),
            tile_entries: Vec::new(),
            rows: Vec::new(),
            total_size: (empty_space.height * empty_space.width),
            bounds: *bounds,
            empty_space,
            grid,
        }
    }
    pub fn populate_tiles(&mut self, children: Vec<&FileMetadata>) {
        self.squarify(&children);
        self.finish_corner();
    }

    fn finish_corner(&mut self) {
        if let Some((x, y)) = self.unrenderable_tile_coordinates {
            // the unrenderable files area should always be a rectangle
            // so if due to rounding errors some renderable tile is in
            // this area, we'd better remove it
            let tiles = ::std::mem::take(&mut self.tiles);
            let entries = ::std::mem::take(&mut self.tile_entries);
            for (tile, entry) in tiles.into_iter().zip(entries) {
                if tile.x < x || tile.y < y {
                    self.tiles.push(tile);
                    self.tile_entries.push(entry);
                } else {
                    self.hidden.push(entry);
                }
            }
            self.hidden.sort_unstable();
        }
    }
    /// [`Self::populate_tiles`], keeping the rows of `plan`, the last layout of the same folder
    /// in the same area, where they still make good tiles: as the sizes change the tiles grow
    /// and shrink in place rather than being cut again, so a folder overtaking another does not
    /// send the tiles after it across the board. Entries the plan did not have are laid out
    /// after its rows, in what they leave. The plan is dropped, and the children squarified
    /// afresh, when a tile would be worse than [`STEADY_WORST_RATIO`], a planned entry would
    /// get no tile, or the new entries take more than [`STEADY_NEW_SHARE`]. `key` names child
    /// `i` for the plan, stably across layouts (its name's hash). Returns this layout's plan.
    pub fn populate_steady(
        &mut self,
        children: Vec<&FileMetadata>,
        key: &mut dyn FnMut(usize) -> u64,
        plan: Option<&Plan>,
    ) -> Plan {
        let fresh = TreeMap::with_grid(&self.bounds, self.grid);
        if let Some(plan) = plan.filter(|plan| !plan.is_empty()) {
            let mut plan = plan.clone();
            // A planned entry grown too small for a tile is taken out of its row and laid out
            // with the new ones, and the plan tried again: a few times, then afresh.
            for _ in 0..3 {
                match self.follow(&children, key, &plan) {
                    Followed::Yes => return self.plan(key),
                    Followed::No => break,
                    Followed::Without(keys) => {
                        for (_, row) in &mut plan.rows {
                            row.retain(|key| !keys.contains(key));
                        }
                        plan.rows.retain(|(_, row)| !row.is_empty());
                        *self = TreeMap::with_grid(&self.bounds, self.grid);
                    }
                }
            }
        }
        *self = fresh;
        self.populate_tiles(children);
        self.plan(key)
    }

    /// Lay `children` out by `plan`'s rows; `false` when it would not do (see
    /// [`Self::populate_steady`]), leaving `self` to be thrown away.
    fn follow(
        &mut self,
        children: &[&FileMetadata],
        key: &mut dyn FnMut(usize) -> u64,
        plan: &Plan,
    ) -> Followed {
        // Where each planned key sits: its row, its place in the row.
        let mut wanted: HashMap<u64, (usize, usize), FastBuildHasher> =
            HashMap::with_capacity_and_hasher(plan.len(), FastBuildHasher::default());
        for (row, (_, keys)) in plan.rows.iter().enumerate() {
            for (place, &key) in keys.iter().enumerate() {
                wanted.insert(key, (row, place));
            }
        }
        // The children are largest first and the planned ones had tiles, so they come early:
        // once every one is found the rest are new, and need no key.
        let mut rows: Vec<Vec<(usize, usize)>> = vec![Vec::new(); plan.rows.len()];
        let mut found = 0;
        let mut rest = Vec::new();
        let mut rest_share = 0.0;
        for (index, child) in children.iter().enumerate() {
            let at = if found < wanted.len() {
                wanted.get(&key(index)).copied()
            } else {
                None
            };
            match at {
                Some((row, place)) => {
                    rows[row].push((place, index));
                    found += 1;
                }
                None => {
                    rest.push(index);
                    rest_share += child.percentage;
                }
            }
        }
        if rest_share > STEADY_NEW_SHARE {
            note(1);
            return Followed::No;
        }
        for (row, (horizontal, _)) in rows.iter_mut().zip(&plan.rows) {
            if row.is_empty() {
                continue;
            }
            row.sort_unstable();
            let entries: Vec<usize> = row.iter().map(|&(_, index)| index).collect();
            let files: Vec<&FileMetadata> = entries.iter().map(|&index| children[index]).collect();
            self.lay_row(&entries, &files, Some(*horizontal));
        }
        if !self.hidden.is_empty() {
            // A planned entry too small now for a tile: the corner would open mid-board.
            note(2);
            return Followed::Without(self.hidden.iter().map(|&entry| key(entry)).collect());
        }
        if !self.steady_shapes() {
            note(3);
            return Followed::No;
        }
        note(0);
        if !rest.is_empty() {
            let files: Vec<&FileMetadata> = rest.iter().map(|&index| children[index]).collect();
            let (first_new, hidden, rows) = (self.tiles.len(), self.hidden.len(), self.rows.len());
            self.squarify(&files);
            // `squarify` numbered them in `files`: back to `children`'s indices.
            for entry in &mut self.tile_entries[first_new..] {
                *entry = rest[*entry];
            }
            for entry in &mut self.hidden[hidden..] {
                *entry = rest[*entry];
            }
            for (_, row) in &mut self.rows[rows..] {
                for entry in row {
                    *entry = rest[*entry];
                }
            }
        }
        self.finish_corner();
        Followed::Yes
    }

    /// Whether every tile is at least [`STEADY_WORST_RATIO`] square, in the grid's shape.
    fn steady_shapes(&self) -> bool {
        self.tiles.iter().all(|tile| {
            let width = f64::from(tile.width);
            let height = f64::from(tile.height) * self.grid.ratio;
            width.min(height) >= width.max(height) * STEADY_WORST_RATIO
        })
    }

    /// This layout as a plan: its rows, by `key`.
    fn plan(&self, key: &mut dyn FnMut(usize) -> u64) -> Plan {
        Plan {
            rows: self
                .rows
                .iter()
                .filter(|(_, row)| !row.is_empty())
                .map(|(horizontal, row)| {
                    (*horizontal, row.iter().map(|&entry| key(entry)).collect())
                })
                .collect(),
        }
    }

    /// Which child each of `tiles` is, by its index in what was laid out.
    #[must_use]
    pub fn tile_entries(&self) -> &[usize] {
        &self.tile_entries
    }
    /// What the children left of the area, in cells: nothing when their shares add up to the
    /// whole, their siblings' room when some were not given to be laid out.
    #[must_use]
    pub fn leftover(&self) -> Area {
        self.empty_space.round()
    }
    /// Lay out `row`, the children from `first` on.
    fn layoutrow(&mut self, first: usize, row: &[&FileMetadata]) {
        let entries: Vec<usize> = (first..first + row.len()).collect();
        self.lay_row(&entries, row, None);
    }

    /// Lay out `row`, children `entries`, along the empty space's shorter side, or across it
    /// if `horizontal` says so (a plan's row, kept the way it ran).
    fn lay_row(&mut self, entries: &[usize], row: &[&FileMetadata], horizontal: Option<bool>) {
        let row_total = row.iter().fold(0.0, |acc, file_metadata| {
            let size = file_metadata.percentage * self.total_size;
            acc + size
        });
        let should_render_horizontally = horizontal
            .unwrap_or(self.empty_space.width <= self.empty_space.height * self.grid.ratio);
        let mut laid = Vec::new();
        let mut progress_in_row = if should_render_horizontally {
            self.empty_space.x
        } else {
            self.empty_space.y
        };
        let mut length_of_row_second_side = 0.0;
        for (&entry, file_metadata) in entries.iter().zip(row) {
            let size = file_metadata.percentage * self.total_size;
            // A row of nothing but empty entries (a share of 0 each) is laid out as nothing, where
            // it is: 0 / 0 made its place NaN, which rounds to the board's corner, and the "small
            // files" corner grew from there over every tile.
            let share = if row_total > 0.0 {
                size / row_total
            } else {
                0.0
            };
            let tile_length_first_side = if should_render_horizontally {
                share * self.empty_space.width
            } else {
                share * self.empty_space.height
            };

            // we take the highest of length_of_row_second_side and length_candidate so the row will always
            // have the same width, even if it means fudging the calculation a little
            let length_candidate = if tile_length_first_side > 0.0 {
                size / tile_length_first_side
            } else {
                0.0
            };
            let tile_length_second_side = if length_of_row_second_side > length_candidate {
                length_of_row_second_side
            } else {
                length_candidate
            };

            let rect = if should_render_horizontally {
                RectFloat {
                    x: progress_in_row,
                    y: self.empty_space.y,
                    width: tile_length_first_side,
                    height: tile_length_second_side,
                }
            } else {
                RectFloat {
                    x: self.empty_space.x,
                    y: progress_in_row,
                    width: tile_length_second_side,
                    height: tile_length_first_side,
                }
            };
            progress_in_row += tile_length_first_side;

            // Named only if it gets a tile: most of a big folder's entries go to the corner.
            let area = rect.round();
            if area.height < self.grid.min_height || area.width < self.grid.min_width {
                self.add_unrenderable_tile(area.x, area.y);
                self.hidden.push(entry);
            } else {
                self.tiles.push(Tile::at(&area, file_metadata));
                self.tile_entries.push(entry);
                laid.push(entry);
            }

            if tile_length_second_side > length_of_row_second_side {
                length_of_row_second_side = tile_length_second_side;
            }
        }

        if !laid.is_empty() {
            self.rows.push((should_render_horizontally, laid));
        }
        if should_render_horizontally {
            self.empty_space.height -= length_of_row_second_side;
            self.empty_space.y += length_of_row_second_side;
        } else {
            self.empty_space.width -= length_of_row_second_side;
            self.empty_space.x += length_of_row_second_side;
        }
    }
    /// Record that the tile at `x`, `y` was too small to draw, growing the "small files"
    /// placeholder to cover it.
    ///
    /// A tile that rounds to zero cells still stands for real entries. When everything but one
    /// huge entry falls below the minimum tile size, all of the hidden tiles round to nothing, and
    /// dropping them would leave the board with no hint that anything else exists. So the
    /// placeholder is kept, and pulled in from the board's edge far enough to be visible.
    fn add_unrenderable_tile(&mut self, x: u16, y: u16) {
        let right = self.bounds.x + self.bounds.width;
        let bottom = self.bounds.y + self.bounds.height;
        let x = x
            .min(right.saturating_sub(self.grid.small_files_width))
            .max(self.bounds.x);
        let y = y
            .min(bottom.saturating_sub(self.grid.small_files_height))
            .max(self.bounds.y);
        self.unrenderable_tile_coordinates = match self.unrenderable_tile_coordinates {
            Some((current_x, current_y)) => Some((x.min(current_x), y.min(current_y))),
            None => Some((x, y)),
        };
    }

    /// The row's worst aspect ratio, `None` if one of it is not renderable; `sum` is its
    /// children's sizes added in order.
    fn worst_in_renderable_row(
        &self,
        row: &[&FileMetadata],
        sum: f64,
        length_of_row: f64,
        min_first_side: f64,
        min_second_side: f64,
    ) -> Option<f64> {
        // In a row every child has the same second side (the row's sum over its length) and a
        // first side in proportion to its size, so with the children largest first both what
        // renders and the worst ratio are decided at one end or the other: the rest need not be
        // looked at, where on a pixel grid a row of thousands made each step cost the row.
        let ends;
        let row = if row.len() > 2 {
            ends = [row[0], row[row.len() - 1]];
            &ends[..]
        } else {
            row
        };
        let mut worst_aspect_ratio = None;
        for val in row.iter() {
            let size = val.percentage * self.total_size;
            let first_side = (size / sum) * length_of_row;
            let second_side = size / first_side;
            if first_side >= min_first_side && second_side >= min_second_side {
                let val_aspect_ratio = if first_side < second_side {
                    first_side / second_side
                } else {
                    second_side / first_side
                };
                match worst_aspect_ratio {
                    Some(current_worst) => {
                        if val_aspect_ratio < current_worst {
                            worst_aspect_ratio = Some(val_aspect_ratio);
                        }
                    }
                    None => {
                        worst_aspect_ratio = Some(val_aspect_ratio);
                    }
                }
            } else {
                return None;
            }
        }
        worst_aspect_ratio
    }

    /// Squarify `children` into the empty space: a row grows one child at a time while that
    /// improves its worst aspect ratio, and is laid out when it would not.
    ///
    /// A loop over the children, not a recursion on the rest of them, so tens of thousands of
    /// entries (a window's pixel grid) cost neither the stack nor a copy of the list a step.
    /// Whether any child left is big enough for a tile is the largest left against the least
    /// tile, from `largest_after`, where it was a scan of every child left at every step.
    fn squarify(&mut self, children: &[&FileMetadata]) {
        let mut largest_after = vec![0.0f64; children.len() + 1];
        for (index, child) in children.iter().enumerate().rev() {
            largest_after[index] = largest_after[index + 1].max(child.percentage * self.total_size);
        }
        let ratio = self.grid.ratio;
        let min_width = f64::from(self.grid.min_width);
        let min_height = f64::from(self.grid.min_height);
        // The row is `children[row_start..next]`; its sizes added in order, as the layout of it
        // adds them.
        let mut row_start = 0;
        let mut row_sum = 0.0;
        let mut next = 0;
        // The row's worst ratio, when it is known: after a child is taken the row is the one
        // just measured with it, in the same empty space.
        let mut row_worst: Option<Option<f64>> = None;
        loop {
            let (length_of_row, min_first_side, min_second_side) =
                if self.empty_space.height * ratio < self.empty_space.width {
                    (
                        self.empty_space.height * ratio,
                        min_height * ratio,
                        min_width / ratio,
                    )
                } else {
                    (
                        self.empty_space.width / ratio,
                        min_width / ratio,
                        min_height * ratio,
                    )
                };

            let (row, rest) = (&children[row_start..next], &children[next..]);
            if rest.is_empty() {
                self.layoutrow(row_start, row);
                return;
            }
            if largest_after[next] < min_first_side * min_second_side {
                self.layoutrow(row_start, row);
                self.layoutrow(next, rest);
                return;
            }
            let current_row_worst_ratio = row_worst.unwrap_or_else(|| {
                self.worst_in_renderable_row(
                    row,
                    row_sum,
                    length_of_row,
                    min_first_side,
                    min_second_side,
                )
            });
            let with_child = row_sum + rest[0].percentage * self.total_size;
            let row_with_child_worst_ratio = self.worst_in_renderable_row(
                &children[row_start..=next],
                with_child,
                length_of_row,
                min_first_side,
                min_second_side,
            );

            let take_child = match (current_row_worst_ratio, row_with_child_worst_ratio) {
                // Renderable children are somewhere, but not the way the row is now nor with
                // the next child in it: add it and keep looking. At worst the renderable
                // children run out and the rest is laid out as one row (above).
                (None, None) => true,
                // Only with the child is the row renderable: add it, and look for a better
                // ratio still.
                (None, Some(_)) => true,
                // The row is renderable as it is and the child would spoil it: lay it out.
                (Some(_), None) => false,
                // Add the child while it improves the worst ratio; lay the row out when it
                // stops.
                (Some(current_ratio), Some(next_ratio)) => current_ratio < next_ratio,
            };
            if take_child {
                row_sum = with_child;
                next += 1;
                row_worst = Some(row_with_child_worst_ratio);
            } else {
                self.layoutrow(row_start, row);
                row_start = next;
                row_sum = 0.0;
                row_worst = None;
            }
        }
    }
}
