//! Tiles sliding to their new places after a relayout with new sizes (a scan's outline, the
//! finished tree, a delete, a rescan) rather than jumping there: the layout is made once, and
//! each frame of [`TWEEN`] shows the tiles part way from where they were, keyed by path. A
//! viewer that turns it on ([`Viewer::set_animation`]) calls [`Viewer::animate`] every frame
//! while [`Viewer::animating`] says so; until then nothing changes.

use ::std::collections::HashMap;
use ::std::time::{Duration, Instant};

use libduscape::model::files::hash::FastBuildHasher;
use libduscape::tiles::{Area, Inside, Tile, path_key};

use super::Viewer;

/// How long tiles take to slide to their new places.
pub const TWEEN: Duration = Duration::from_millis(200);

/// A slide under way: where each tile was, and where the layout put it.
pub(super) struct Tween {
    start: Instant,
    /// Where each tile on show was, by its [`path_key`]s.
    from: HashMap<u64, Area, FastBuildHasher>,
    board: Vec<Area>,
    nested: Vec<Area>,
    /// The folders' insides, taken off while the tiles move: a folder is painted whole, since
    /// its entries do not cover their inside until they are there.
    board_insides: Vec<Option<Inside>>,
    insides: Vec<Option<Inside>>,
}

fn area(tile: &Tile) -> Area {
    Area {
        x: tile.x,
        y: tile.y,
        width: tile.width,
        height: tile.height,
    }
}

fn place(tile: &mut Tile, area: Area) {
    tile.x = area.x;
    tile.y = area.y;
    tile.width = area.width;
    tile.height = area.height;
}

/// `from` to `to` by `t`, 0 to 1.
fn between(from: Area, to: Area, t: f64) -> Area {
    let at = |a: u16, b: u16| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u16;
    Area {
        x: at(from.x, to.x),
        y: at(from.y, to.y),
        width: at(from.width, to.width),
        height: at(from.height, to.height),
    }
}

impl Viewer {
    /// Slide the tiles to a new layout of new sizes rather than jump there. Off by default: a
    /// viewer that turns it on draws a frame on [`Viewer::animate`] while
    /// [`Viewer::animating`].
    pub fn set_animation(&mut self, on: bool) {
        self.animation = on;
        if !on {
            self.end_tween();
        }
    }

    /// Whether tiles are on their way to their places: the viewer calls
    /// [`Viewer::animate`] once a frame until it is not.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.tween.is_some()
    }

    /// Put the tiles where they are at `now` in the slide; returns whether it goes on.
    pub fn animate(&mut self, now: Instant) -> bool {
        let Some(tween) = &self.tween else {
            return false;
        };
        let t = now.saturating_duration_since(tween.start).as_secs_f64() / TWEEN.as_secs_f64();
        if t >= 1.0 {
            self.end_tween();
            return false;
        }
        // Ease out: quick away, settling at the end.
        let t = 1.0 - (1.0 - t).powi(3);
        let keys = self.tile_keys();
        let (board, nested) = keys.split_at(self.board.tiles.len());
        let tween = self.tween.as_ref().expect("checked above");
        let grow = |to: Area| Area {
            x: to.x + to.width / 2,
            y: to.y + to.height / 2,
            width: 0,
            height: 0,
        };
        for ((tile, key), &to) in self.board.tiles.iter_mut().zip(board).zip(&tween.board) {
            let from = tween.from.get(key).copied().unwrap_or_else(|| grow(to));
            place(tile, between(from, to, t));
        }
        for ((tile, key), &to) in self.nested.iter_mut().zip(nested).zip(&tween.nested) {
            let from = tween.from.get(key).copied().unwrap_or_else(|| grow(to));
            place(&mut tile.tile, between(from, to, t));
        }
        true
    }

    /// Before a relayout that is to slide: where every tile is now, part way or not.
    pub(super) fn tween_from(&mut self) -> Option<HashMap<u64, Area, FastBuildHasher>> {
        if !self.animation {
            return None;
        }
        let keys = self.tile_keys();
        let areas = self
            .board
            .tiles
            .iter()
            .map(area)
            .chain(self.nested.iter().map(|tile| area(&tile.tile)));
        Some(keys.into_iter().zip(areas).collect())
    }

    /// After it: slide from `from` to the layout just made.
    pub(super) fn tween_to(&mut self, from: Option<HashMap<u64, Area, FastBuildHasher>>) {
        let Some(from) = from else {
            return;
        };
        self.tween = None;
        let insides = self
            .nested
            .iter_mut()
            .map(|tile| tile.inside.take())
            .collect();
        self.tween = Some(Tween {
            start: Instant::now(),
            from,
            board: self.board.tiles.iter().map(area).collect(),
            nested: self.nested.iter().map(|tile| area(&tile.tile)).collect(),
            board_insides: ::std::mem::take(&mut self.board_insides),
            insides,
        });
        self.board_insides = vec![None; self.board.tiles.len()];
        self.animate(Instant::now());
    }

    /// Every tile in its place, and the slide over: at its end, and before any relayout that
    /// does not slide.
    pub(super) fn end_tween(&mut self) {
        let Some(tween) = self.tween.take() else {
            return;
        };
        for (tile, to) in self.board.tiles.iter_mut().zip(tween.board) {
            place(tile, to);
        }
        for ((tile, to), inside) in self.nested.iter_mut().zip(tween.nested).zip(tween.insides) {
            place(&mut tile.tile, to);
            tile.inside = inside;
        }
        self.board_insides = tween.board_insides;
    }

    /// Each tile's [`path_key`]: the board's, then the nesting's, in order.
    fn tile_keys(&self) -> Vec<u64> {
        let hasher = FastBuildHasher::default();
        let mut keys: Vec<u64> = self
            .board
            .tiles
            .iter()
            .map(|tile| path_key(&hasher, 0, &tile.name))
            .collect();
        let board = keys.len();
        for tile in &self.nested {
            let parent = match tile.parent {
                Some(parent) => keys[board + parent],
                None => keys[tile.top],
            };
            keys.push(path_key(&hasher, parent, &tile.tile.name));
        }
        keys
    }
}
