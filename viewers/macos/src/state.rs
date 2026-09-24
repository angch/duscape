//! What the window shows and how it answers input, with no AppKit in it: the tree and its
//! board, where each panel sits, the entry in hand, marks, navigation, zoom, rescans, and what a
//! delete changes. The AppKit side (`mac`) turns events into calls here and draws what is here.
//!
//! Kept free of AppKit so that its tests run on the Linux CI like the rest of the workspace.

use ::std::ffi::{OsStr, OsString};
use ::std::mem::ManuallyDrop;
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::time::{Duration, Instant};

use diskonaut_scan::rescan::{Outcome, Rescanner};
use libdiskonaut::model::SizeKind;
use libdiskonaut::tiles::{Area, Board, FileMetadata, FileType};
use libdiskonaut::{
    DirSummary, DisplayCount, DisplaySize, FileOrFolder, FileToDelete, FileTree, Folder,
};

/// A layout cell, in points. The treemap lays tiles out in cells 2.5 times taller than wide (its
/// `HEIGHT_WIDTH_RATIO`: a terminal's cell), so cells this shape come out as square-looking
/// tiles, and its 8×3-cell minimum tile becomes about 19×18 points.
pub const CELL_W: f64 = 2.4;
pub const CELL_H: f64 = 6.0;
/// The breadcrumb bar across the top, and the status bar across the bottom.
pub const PATH_BAR: f64 = 30.0;
pub const STATUS_BAR: f64 = 24.0;
/// One row of the list.
pub const ROW: f64 = 22.0;
/// The side panel is shown only in a window at least this wide, so the treemap keeps room.
const SIDEBAR_MIN_WIDTH: f64 = 640.0;
/// Below this height the side panel is all list, with no room for the entry's details.
const INFO_MIN_HEIGHT: f64 = 320.0;
/// How long a message (copied, deleted) holds the status bar.
pub const MESSAGE_TIME: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn inset(&self, dx: f64, dy: f64) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }
}

/// Where each part of the window is, in points from the top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    pub bounds: Rect,
    pub path_bar: Rect,
    /// The list of the folder's entries, when the window is wide enough for a side panel.
    pub list: Option<Rect>,
    /// Under the list: the entry in hand, and its preview.
    pub info: Option<Rect>,
    pub treemap: Rect,
    pub status: Rect,
    /// The treemap's size in layout cells.
    pub cols: u16,
    pub rows: u16,
}

impl Layout {
    pub fn new(width: f64, height: f64, sidebar: bool) -> Self {
        let width = width.max(0.0);
        let height = height.max(0.0);
        let body = Rect::new(
            0.0,
            PATH_BAR,
            width,
            (height - PATH_BAR - STATUS_BAR).max(0.0),
        );
        let (list, info, treemap) = if sidebar && width >= SIDEBAR_MIN_WIDTH {
            let panel = (width / 3.0).clamp(240.0, 420.0);
            let info_h = if body.h >= INFO_MIN_HEIGHT {
                (body.h * 0.4).round()
            } else {
                0.0
            };
            let list = Rect::new(0.0, body.y, panel, body.h - info_h);
            let info = (info_h > 0.0).then(|| Rect::new(0.0, list.bottom(), panel, info_h));
            // One point for the line between the panel and the treemap.
            let treemap = Rect::new(panel + 1.0, body.y, width - panel - 1.0, body.h);
            (Some(list), info, treemap)
        } else {
            (None, None, body)
        };
        let cells = |points: f64, cell: f64| (points / cell).floor().clamp(1.0, 4096.0) as u16;
        Layout {
            bounds: Rect::new(0.0, 0.0, width, height),
            path_bar: Rect::new(0.0, 0.0, width, PATH_BAR),
            list,
            info,
            treemap,
            status: Rect::new(0.0, (height - STATUS_BAR).max(0.0), width, STATUS_BAR),
            cols: cells(treemap.w, CELL_W),
            rows: cells(treemap.h, CELL_H),
        }
    }

    /// A span of layout cells in points. The cells are stretched to fill the treemap exactly, so
    /// the tiles meet its edges whatever the window's size.
    pub fn cells_to_rect(&self, x: u16, y: u16, width: u16, height: u16) -> Rect {
        let sx = self.treemap.w / f64::from(self.cols);
        let sy = self.treemap.h / f64::from(self.rows);
        Rect::new(
            self.treemap.x + f64::from(x) * sx,
            self.treemap.y + f64::from(y) * sy,
            f64::from(width) * sx,
            f64::from(height) * sy,
        )
    }

    /// The layout cell under a point in the treemap.
    pub fn cell_at(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        if !self.treemap.contains(x, y) {
            return None;
        }
        let sx = self.treemap.w / f64::from(self.cols);
        let sy = self.treemap.h / f64::from(self.rows);
        Some((
            ((x - self.treemap.x) / sx) as u16,
            ((y - self.treemap.y) / sy) as u16,
        ))
    }

    /// How many whole rows the list shows.
    pub fn list_rows(&self) -> usize {
        self.list.map_or(0, |list| (list.h / ROW).floor() as usize)
    }
}

/// Which panel the arrow keys drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Treemap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A jump through the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    PageUp,
    PageDown,
    Home,
    End,
}

/// What is under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// A row of the list: its index in the listing.
    Row(usize),
    /// A tile: its entry's name.
    Tile(OsString),
    /// The corner the entries too small for a tile of their own are folded into.
    SmallFiles,
    Nothing,
}

/// Modifier keys held during a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// ⌘: add or take away one entry from the marks.
    pub toggle: bool,
    /// ⇧: mark every entry from the anchor to this one.
    pub range: bool,
}

/// What the preview shows for the entry in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    /// Nothing asked for: a folder, or nothing in hand.
    None,
    Loading,
    /// A line instead of the contents: empty, binary, unreadable.
    Info(String),
    Text(Vec<String>),
    /// A picture; the image itself is the AppKit side's, which decodes it. This is its caption.
    Picture(String),
}

/// A rescan under way: its id, the folder's path from the scan root, and what stops it.
struct Rescan {
    id: u64,
    relative: Vec<OsString>,
    cancel: Arc<AtomicBool>,
}

impl Rescan {
    /// Whether this rescan's folder is `relative` or holds it, so it will bring back `relative`.
    fn covers(&self, relative: &[OsString]) -> bool {
        relative.starts_with(&self.relative)
    }
}

pub struct Viewer {
    /// The outline while the first scan runs; the finished tree after. Never dropped here: a
    /// tree's destructor walks every folder in it, so an old tree goes to [`drop_later`].
    pub tree: ManuallyDrop<FileTree>,
    pub board: Board,
    pub layout: Layout,
    pub sidebar: bool,
    pub focus: Focus,
    /// The entry in hand, by name, so that it stays in hand when the tiles are laid out again —
    /// a resize, a zoom, or new sizes arriving during a scan. Both panels show it.
    pub selected: Option<OsString>,
    /// Marked entries, in the order marked. When there are any, they are what a delete, a copy
    /// or Show in Finder acts on; otherwise the entry in hand is.
    pub marked: Vec<OsString>,
    /// Where a ⇧ range starts.
    anchor: Option<OsString>,
    /// The listing's first row on screen.
    pub list_top: usize,
    pub hover: Option<OsString>,
    /// The zoom level of each folder above this one, to restore on the way back up.
    zooms: Vec<usize>,
    pub scanning: bool,
    /// Counts scans started in this window, so that a scan's findings arriving after another
    /// scan has replaced it are dropped.
    pub scan_id: u64,
    pub entries_scanned: u64,
    pub last_read: Option<PathBuf>,
    scan_started: Instant,
    pub scan_took: Option<Duration>,
    /// A message for the status bar, and when it was posted.
    message: Option<(String, Instant)>,
    rescanner: Option<Rescanner>,
    rescans: Vec<Rescan>,
    next_rescan_id: u64,
    /// The file the preview was last asked for, and the request's number: an answer to an older
    /// one is dropped.
    preview_for: Option<PathBuf>,
    pub preview_generation: u64,
    pub preview: Preview,
}

impl Viewer {
    /// A window on `root`, about to be scanned: the tree is empty until outlines arrive.
    pub fn new(root: &Path, kind: SizeKind, scan_id: u64) -> Self {
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        tree.shown = kind;
        let mut board = Board::new(tree.get_current_folder());
        board.show(kind);
        Viewer {
            tree: ManuallyDrop::new(tree),
            board,
            layout: Layout::default(),
            sidebar: true,
            focus: Focus::List,
            selected: None,
            marked: Vec::new(),
            anchor: None,
            list_top: 0,
            hover: None,
            zooms: Vec::new(),
            scanning: true,
            scan_id,
            entries_scanned: 0,
            last_read: None,
            scan_started: Instant::now(),
            scan_took: None,
            message: None,
            rescanner: None,
            rescans: Vec::new(),
            next_rescan_id: 0,
            preview_for: None,
            preview_generation: 0,
            preview: Preview::None,
        }
    }

    /// Allow rescans, which `rescanner` runs.
    pub fn enable_rescans(&mut self, rescanner: Rescanner) {
        self.rescanner = Some(rescanner);
    }

    pub fn root(&self) -> &Path {
        &self.tree.path_in_filesystem
    }

    // ---------------------------------------------------------------- layout

    pub fn resize(&mut self, width: f64, height: f64) {
        self.layout = Layout::new(width, height, self.sidebar);
        self.board.change_area(&Area {
            x: 0,
            y: 0,
            width: self.layout.cols,
            height: self.layout.rows,
        });
        if self.layout.list.is_none() {
            self.focus = Focus::Treemap;
        }
        self.sync_board();
        self.scroll_to_selected();
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar = !self.sidebar;
        let bounds = self.layout.bounds;
        self.resize(bounds.w, bounds.h);
    }

    /// Lay the folder's entries out again, after the tree or the size shown changed.
    fn refresh(&mut self) {
        self.board.change_files(self.tree.get_current_folder());
        self.sync_board();
        self.clamp_list_top();
    }

    /// Keep the entry in hand and the marks to entries that exist, and put the board's
    /// selection on the tile of the entry in hand, wherever the layout put it.
    fn sync_board(&mut self) {
        let listing = self.board.listing();
        let listed = |name: &OsString| listing.iter().any(|entry| &entry.name == name);
        if self.selected.as_ref().is_some_and(|name| !listed(name)) {
            self.selected = None;
        }
        self.marked.retain(|name| listed(name));
        if self.hover.as_ref().is_some_and(|name| !listed(name)) {
            self.hover = None;
        }
        match self.tile_index(self.selected.as_deref()) {
            Some(index) => self.board.set_selected_index(&index),
            None => self.board.reset_selected_index(),
        }
    }

    fn tile_index(&self, name: Option<&OsStr>) -> Option<usize> {
        let name = name?;
        self.board.tiles.iter().position(|tile| tile.name == name)
    }

    fn listing_index(&self, name: Option<&OsStr>) -> Option<usize> {
        let name = name?;
        self.board
            .listing()
            .iter()
            .position(|entry| entry.name == name)
    }

    pub fn selected_listing_index(&self) -> Option<usize> {
        self.listing_index(self.selected.as_deref())
    }

    fn clamp_list_top(&mut self) {
        let rows = self.layout.list_rows().max(1);
        let most = self.board.listing().len().saturating_sub(rows);
        self.list_top = self.list_top.min(most);
    }

    fn scroll_to_selected(&mut self) {
        if let Some(index) = self.selected_listing_index() {
            let rows = self.layout.list_rows().max(1);
            if index < self.list_top {
                self.list_top = index;
            } else if index >= self.list_top + rows {
                self.list_top = index + 1 - rows;
            }
        }
        self.clamp_list_top();
    }

    /// Scroll the list by `rows`, without moving what is in hand.
    pub fn scroll_list(&mut self, rows: isize) {
        self.list_top = self.list_top.saturating_add_signed(rows);
        self.clamp_list_top();
    }

    // ---------------------------------------------------------------- the scan

    /// Folder outlines from the running scan: the live view.
    pub fn add_summaries(&mut self, summaries: Vec<DirSummary>) {
        for summary in summaries {
            self.entries_scanned += summary.entries;
            self.tree.failed_to_read += summary.dirs.failed;
            self.last_read = Some(summary.dirs.path.to_path_buf());
            self.tree.add_summary(summary);
        }
        self.refresh();
    }

    /// The finished tree takes the outline's place, keeping the user where they are.
    pub fn finish_scan(&mut self, mut tree: FileTree) {
        tree.adopt_navigation_from(&self.tree);
        self.entries_scanned = tree.get_total_descendants();
        let outline = ::std::mem::replace(&mut self.tree, ManuallyDrop::new(tree));
        drop_later(ManuallyDrop::into_inner(outline));
        self.scanning = false;
        self.scan_took = Some(self.scan_started.elapsed());
        self.last_read = None;
        if self.tree.current_folder_names.len() < self.zooms.len() {
            self.zooms.truncate(self.tree.current_folder_names.len());
            self.board.reset_zoom_index();
        }
        self.refresh();
        if self.selected.is_none() {
            self.select_first();
        }
    }

    // ---------------------------------------------------------------- what is in hand

    pub fn selected_entry(&self) -> Option<&FileMetadata> {
        let index = self.selected_listing_index()?;
        self.board.listing().get(index)
    }

    pub fn entry_named(&self, name: &OsStr) -> Option<&FileMetadata> {
        self.board.listing().iter().find(|entry| entry.name == name)
    }

    pub fn path_of(&self, name: &OsStr) -> PathBuf {
        self.tree.get_current_path().join(name)
    }

    fn select(&mut self, name: Option<OsString>) {
        self.selected = name;
        self.anchor = self.selected.clone();
        self.sync_board();
        self.scroll_to_selected();
    }

    fn select_first(&mut self) {
        let first = self.board.listing().first().map(|entry| entry.name.clone());
        self.select(first);
    }

    fn select_listing(&mut self, index: usize) {
        let name = self
            .board
            .listing()
            .get(index)
            .map(|entry| entry.name.clone());
        if name.is_some() {
            self.select(name);
        }
    }

    pub fn is_marked(&self, name: &OsStr) -> bool {
        self.marked.iter().any(|marked| marked == name)
    }

    /// Move the entry in hand. In the list, ↑ and ↓ go by row (with `extend`, marking the range
    /// from the anchor) and → crosses to the treemap. In the treemap the tile beside it in that
    /// direction is taken; ← off its left edge crosses to the list.
    pub fn arrow(&mut self, direction: Direction, extend: bool) {
        match (self.focus, direction) {
            (Focus::List, Direction::Up | Direction::Down) => {
                let delta = if direction == Direction::Up { -1 } else { 1 };
                let next = match self.selected_listing_index() {
                    Some(index) => index.saturating_add_signed(delta),
                    None => 0,
                };
                self.move_list_to(next, extend);
            }
            (Focus::List, Direction::Right) => self.focus = Focus::Treemap,
            (Focus::List, Direction::Left) => {}
            (Focus::Treemap, _) => {
                self.marked.clear();
                let before = self.board.get_selected_index();
                match direction {
                    Direction::Left => self.board.move_selected_left(),
                    Direction::Right => self.board.move_selected_right(),
                    Direction::Up => self.board.move_selected_up(),
                    Direction::Down => self.board.move_selected_down(),
                }
                match self.board.currently_selected() {
                    Some(tile) => {
                        let name = tile.name.clone();
                        self.select(Some(name));
                    }
                    // Off the edge: stay on the tile, or cross to the list beside it.
                    None => {
                        if let Some(index) = before {
                            self.board.set_selected_index(&index);
                        }
                        if direction == Direction::Left && self.layout.list.is_some() {
                            self.focus = Focus::List;
                        }
                    }
                }
            }
        }
    }

    /// Jump through the list, which takes the keyboard.
    pub fn jump(&mut self, jump: Jump, extend: bool) {
        if self.layout.list.is_some() {
            self.focus = Focus::List;
        }
        let page = self.layout.list_rows().saturating_sub(1).max(1);
        let current = self.selected_listing_index().unwrap_or(0);
        let last = self.board.listing().len().saturating_sub(1);
        let next = match jump {
            Jump::PageUp => current.saturating_sub(page),
            Jump::PageDown => current.saturating_add(page),
            Jump::Home => 0,
            Jump::End => last,
        };
        self.move_list_to(next, extend);
    }

    fn move_list_to(&mut self, index: usize, extend: bool) {
        let len = self.board.listing().len();
        if len == 0 {
            return;
        }
        let index = index.min(len - 1);
        if extend {
            let anchor = self.anchor.clone().or_else(|| self.selected.clone());
            self.selected = self.board.listing().get(index).map(|e| e.name.clone());
            self.anchor = anchor.clone();
            self.mark_range(anchor.as_deref(), index);
            self.sync_board();
            self.scroll_to_selected();
        } else {
            self.marked.clear();
            self.select_listing(index);
        }
    }

    /// Mark every entry from `anchor` to the listing's `to`, in place of the marks there were.
    fn mark_range(&mut self, anchor: Option<&OsStr>, to: usize) {
        let from = self.listing_index(anchor).unwrap_or(to);
        let (low, high) = (from.min(to), from.max(to));
        self.marked = self.board.listing()[low..=high]
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Treemap,
            Focus::Treemap if self.layout.list.is_some() => Focus::List,
            Focus::Treemap => Focus::Treemap,
        };
    }

    /// Mark every entry in the folder.
    pub fn mark_all(&mut self) {
        self.marked = self
            .board
            .listing()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
    }

    // ---------------------------------------------------------------- the mouse

    pub fn hit(&self, x: f64, y: f64) -> Hit {
        if let Some(list) = self.layout.list
            && list.contains(x, y)
        {
            let row = ((y - list.y) / ROW) as usize;
            let index = self.list_top + row;
            // Past the last whole row is a sliver where no row is drawn.
            return if row < self.layout.list_rows() && index < self.board.listing().len() {
                Hit::Row(index)
            } else {
                Hit::Nothing
            };
        }
        let Some((col, row)) = self.layout.cell_at(x, y) else {
            return Hit::Nothing;
        };
        if let Some(index) = self.board.tile_at(col, row) {
            return Hit::Tile(self.board.tiles[index].name.clone());
        }
        match self.board.unrenderable_tile_coordinates {
            Some((sx, sy)) if col >= sx && row >= sy => Hit::SmallFiles,
            _ => Hit::Nothing,
        }
    }

    /// A press of the main button: take the entry under the pointer in hand, with ⌘ toggling its
    /// mark and ⇧ marking the range to it from the anchor. Returns the entry's name.
    pub fn click(&mut self, x: f64, y: f64, mods: Mods) -> Option<OsString> {
        let (focus, name) = match self.hit(x, y) {
            Hit::Row(index) => (Focus::List, self.board.listing()[index].name.clone()),
            Hit::Tile(name) => (Focus::Treemap, name),
            Hit::SmallFiles | Hit::Nothing => return None,
        };
        self.focus = focus;
        if mods.range {
            let to = self.listing_index(Some(&name))?;
            let anchor = self.anchor.clone().or_else(|| self.selected.clone());
            self.mark_range(anchor.as_deref(), to);
            self.selected = Some(name.clone());
            self.anchor = anchor;
            self.sync_board();
        } else if mods.toggle {
            // The entry in hand joins the marks first, so ⌘-click after a plain click marks both.
            if self.marked.is_empty()
                && let Some(selected) = self.selected.clone()
                && selected != name
            {
                self.marked.push(selected);
            }
            match self.marked.iter().position(|marked| marked == &name) {
                Some(at) => {
                    self.marked.remove(at);
                }
                None => self.marked.push(name.clone()),
            }
            self.select(Some(name.clone()));
        } else {
            self.marked.clear();
            self.select(Some(name.clone()));
        }
        Some(name)
    }

    /// Take the entry under the pointer in hand for a context menu, keeping the marks if it is
    /// one of them. Returns whether there is an entry there.
    pub fn context_click(&mut self, x: f64, y: f64) -> bool {
        let name = match self.hit(x, y) {
            Hit::Row(index) => self.board.listing()[index].name.clone(),
            Hit::Tile(name) => name,
            Hit::SmallFiles | Hit::Nothing => return false,
        };
        if !self.is_marked(&name) {
            self.marked.clear();
        }
        self.select(Some(name));
        true
    }

    /// The pointer moved. Returns whether what it is over changed.
    pub fn hover_at(&mut self, x: f64, y: f64) -> bool {
        let hover = match self.hit(x, y) {
            Hit::Row(index) => Some(self.board.listing()[index].name.clone()),
            Hit::Tile(name) => Some(name),
            Hit::SmallFiles | Hit::Nothing => None,
        };
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    // ---------------------------------------------------------------- navigation

    /// Enter the entry in hand, if it is a folder. Returns whether it was.
    pub fn enter_selected(&mut self) -> bool {
        match self.selected_entry() {
            Some(entry) if entry.file_type == FileType::Folder => {
                let name = entry.name.clone();
                self.enter(&name)
            }
            _ => false,
        }
    }

    pub fn enter(&mut self, name: &OsStr) -> bool {
        if !matches!(
            self.tree.item_in_current_folder(name),
            Some(FileOrFolder::Folder(_))
        ) {
            return false;
        }
        self.zooms.push(self.board.zoom_level);
        self.tree.enter_folder(name);
        self.board.reset_zoom_index();
        self.marked.clear();
        self.hover = None;
        self.list_top = 0;
        self.refresh();
        self.select_first();
        true
    }

    /// Up to the parent folder, with the folder just left in hand. Returns whether there was one.
    pub fn go_up(&mut self) -> bool {
        let Some(left) = self.tree.current_folder_names.last().cloned() else {
            return false;
        };
        self.tree.leave_folder();
        self.board.set_zoom_index(self.zooms.pop().unwrap_or(0));
        self.marked.clear();
        self.hover = None;
        self.refresh();
        self.select(Some(left));
        true
    }

    /// Up to the folder `depth` levels below the root: a breadcrumb.
    pub fn go_to_depth(&mut self, depth: usize) {
        while self.tree.current_folder_names.len() > depth && self.go_up() {}
    }

    pub fn depth(&self) -> usize {
        self.tree.current_folder_names.len()
    }

    pub fn zoom_in(&mut self) {
        self.board.zoom_in(self.tree.get_current_folder());
        self.sync_board();
    }

    pub fn zoom_out(&mut self) {
        self.board.zoom_out(self.tree.get_current_folder());
        self.sync_board();
    }

    pub fn reset_zoom(&mut self) {
        self.board.reset_zoom(self.tree.get_current_folder());
        self.sync_board();
    }

    /// Switch between sizes on disk and apparent sizes. The tree holds both: nothing is scanned.
    pub fn toggle_size(&mut self) {
        let kind = self.tree.shown.other();
        self.tree.shown = kind;
        self.board.show(kind);
        self.refresh();
    }

    pub fn showing_apparent(&self) -> bool {
        self.tree.shown == SizeKind::Apparent
    }

    // ---------------------------------------------------------------- acting on entries

    /// The names acted on: every marked entry, or else the one in hand.
    pub fn target_names(&self) -> Vec<OsString> {
        if self.marked.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            self.marked.clone()
        }
    }

    pub fn target_paths(&self) -> Vec<PathBuf> {
        self.target_names()
            .iter()
            .map(|name| self.path_of(name))
            .collect()
    }

    /// What a delete would act on. Nothing while the first scan runs: the tree on screen is an
    /// outline, and the finished tree that replaces it would still hold what was deleted.
    pub fn targets(&self) -> Vec<FileToDelete> {
        if self.scanning {
            return Vec::new();
        }
        self.target_names()
            .iter()
            .filter_map(|name| self.entry_named(name))
            .map(|entry| FileToDelete::in_current_folder(&self.tree, entry.clone()))
            .collect()
    }

    /// Take entries that have left the disk off the tree. `freed` says whether their space came
    /// back (a delete), or only moved elsewhere (to the Trash). What took the first one's place
    /// in the list is put in hand.
    pub fn removed(&mut self, files: &[FileToDelete], freed: bool) {
        let at = self.selected_listing_index();
        for file in files {
            if self.tree.remove_path(&file.path_to_file) && freed {
                self.tree.note_freed(file.sizes);
            }
        }
        if !files.is_empty() {
            self.restart_rescans_under(files);
        }
        self.marked.clear();
        self.leave_vanished_folders();
        self.refresh();
        if self.selected.is_none()
            && let Some(at) = at
        {
            let last = self.board.listing().len().saturating_sub(1);
            self.select_listing(at.min(last));
        }
    }

    /// Out of a folder that is no longer in the tree, to the nearest one that is.
    fn leave_vanished_folders(&mut self) {
        while !self.tree.current_folder_exists() && self.tree.leave_folder() {
            self.zooms.pop();
            self.board.reset_zoom_index();
            self.selected = None;
            self.list_top = 0;
        }
    }

    // ---------------------------------------------------------------- rescans

    pub fn can_rescan(&self) -> bool {
        !self.scanning && self.rescanner.is_some()
    }

    /// Scan the selected folder again — or the folder shown, when a file or nothing is in hand.
    pub fn rescan_selected(&mut self) {
        let mut relative = self.tree.current_folder_names.clone();
        if let Some(entry) = self.selected_entry()
            && entry.file_type == FileType::Folder
        {
            relative.push(entry.name.clone());
        }
        self.start_rescan(relative);
    }

    pub fn rescan_all(&mut self) {
        self.start_rescan(Vec::new());
    }

    /// Rescan the folder at `relative`. Not while the first scan runs, nor when a rescan under
    /// way covers it; one that this covers is stopped, as this brings its folder back too.
    fn start_rescan(&mut self, relative: Vec<OsString>) {
        if self.scanning || self.rescans.iter().any(|rescan| rescan.covers(&relative)) {
            return;
        }
        let Some(rescanner) = &self.rescanner else {
            return;
        };
        self.rescans.retain(|rescan| {
            let covered = rescan.relative.starts_with(&relative);
            if covered {
                rescan.cancel.store(true, Ordering::Release);
            }
            !covered
        });
        self.next_rescan_id += 1;
        let id = self.next_rescan_id;
        let cancel = Arc::new(AtomicBool::new(false));
        let root = self.tree.path_in_filesystem.clone();
        let path = relative
            .iter()
            .fold(root.clone(), |path, name| path.join(name));
        rescanner.spawn(
            id,
            root,
            path,
            relative.len(),
            Arc::clone(&cancel),
            !relative.is_empty(),
        );
        self.rescans.push(Rescan {
            id,
            relative,
            cancel,
        });
    }

    /// Start again every rescan whose folder holds one of `files`: one that listed a file before
    /// it was deleted would otherwise put it back.
    fn restart_rescans_under(&mut self, files: &[FileToDelete]) {
        let mut restart = Vec::new();
        self.rescans.retain(|rescan| {
            let holds = files
                .iter()
                .any(|file| file.path_to_file.starts_with(&rescan.relative));
            if holds {
                rescan.cancel.store(true, Ordering::Release);
                restart.push(rescan.relative.clone());
            }
            !holds
        });
        for relative in restart {
            self.start_rescan(relative);
        }
    }

    /// Stop every rescan, as the window moves on to another folder.
    pub fn cancel_rescans(&mut self) {
        for rescan in self.rescans.drain(..) {
            rescan.cancel.store(true, Ordering::Release);
        }
    }

    /// A rescan has finished: put what it found in the tree. One stopped meanwhile is dropped.
    pub fn rescan_done(&mut self, id: u64, outcome: Outcome) {
        let Some(index) = self.rescans.iter().position(|rescan| rescan.id == id) else {
            return;
        };
        let rescan = self.rescans.remove(index);
        let navigated_to = self.tree.current_folder_names.clone();
        let changed = match outcome {
            Outcome::NotWalked => {
                self.say(
                    "That folder is not scanned (another filesystem, or past the depth limit)",
                );
                false
            }
            Outcome::Scanned(tree, duration, _small) => {
                let old = self.tree.graft(&rescan.relative, *tree);
                let grafted = old.is_some();
                if let Some(old) = old {
                    drop_later(old);
                }
                if rescan.relative.is_empty() {
                    self.scan_took = Some(duration);
                    self.entries_scanned = self.tree.get_total_descendants();
                }
                grafted
            }
            Outcome::Gone => self.tree.remove_path(&rescan.relative),
        };
        if changed {
            if self.tree.current_folder_names != navigated_to {
                self.zooms.clear();
                self.board.reset_zoom_index();
                self.marked.clear();
            }
            self.leave_vanished_folders();
            // The file in hand may have changed on disk too.
            self.preview_for = None;
            self.refresh();
            if self.selected.is_none() {
                self.select_first();
            }
        }
    }

    // ---------------------------------------------------------------- preview

    /// A new preview request, when the file in hand is not the one last asked about. A folder, or
    /// nothing, clears the preview.
    pub fn wanted_preview(&mut self) -> Option<(u64, PathBuf)> {
        let target = self
            .selected_entry()
            .filter(|entry| entry.file_type == FileType::File)
            .map(|entry| self.path_of(&entry.name));
        if target == self.preview_for {
            return None;
        }
        self.preview_for = target.clone();
        self.preview_generation += 1;
        match target {
            Some(path) => {
                self.preview = Preview::Loading;
                Some((self.preview_generation, path))
            }
            None => {
                self.preview = Preview::None;
                None
            }
        }
    }

    /// A preview has been read. Kept only if it answers the latest request.
    pub fn preview_ready(&mut self, generation: u64, preview: Preview) -> bool {
        let current = generation == self.preview_generation;
        if current {
            self.preview = preview;
        }
        current
    }

    // ---------------------------------------------------------------- words

    pub fn say(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), Instant::now()));
    }

    /// The window's title: the folder shown.
    pub fn title(&self) -> String {
        let path = self.tree.get_current_path();
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    /// Under the title: its size, and the whole scan's.
    pub fn subtitle(&self) -> String {
        let mut words = format!(
            "{} · {}",
            DisplaySize(self.tree.get_current_folder_size() as f64),
            if self.showing_apparent() {
                "apparent size"
            } else {
                "on disk"
            }
        );
        if self.depth() > 0 {
            words += &format!(
                " · {} scanned",
                DisplaySize(self.tree.get_total_size() as f64)
            );
        }
        words
    }

    /// The status bar: on the left what the pointer or the keyboard is on (or a message), on the
    /// right what the scan found.
    pub fn status(&self) -> (String, String) {
        let left = match &self.message {
            Some((message, at)) if at.elapsed() < MESSAGE_TIME => message.clone(),
            _ => {
                let name = self.hover.as_ref().or(self.selected.as_ref());
                match name.and_then(|name| self.entry_named(name)) {
                    Some(entry) => describe(entry),
                    None if self.scanning => "Scanning…".to_string(),
                    None => String::new(),
                }
            }
        };
        if !self.marked.is_empty() && self.hover.is_none() {
            let size: u128 = self
                .marked
                .iter()
                .filter_map(|name| self.entry_named(name))
                .map(|entry| entry.size)
                .sum();
            let left = format!(
                "{} marked, {}",
                DisplayCount(self.marked.len() as u64),
                DisplaySize(size as f64)
            );
            return (left, self.totals());
        }
        (left, self.totals())
    }

    fn totals(&self) -> String {
        let mut words = Vec::new();
        if self.scanning {
            words.push(format!(
                "scanning, {} entries",
                DisplayCount(self.entries_scanned)
            ));
        } else if let Some(took) = self.scan_took {
            words.push(format!(
                "{} entries in {:.1}s",
                DisplayCount(self.entries_scanned),
                took.as_secs_f64()
            ));
        }
        if self.tree.failed_to_read > 0 {
            words.push(format!(
                "{} unreadable",
                DisplayCount(self.tree.failed_to_read)
            ));
        }
        if let Some(outside) = self.tree.outside_scan().filter(|&bytes| bytes > 0) {
            words.push(format!(
                "{} not found by the scan",
                DisplaySize(outside as f64)
            ));
        }
        let freed = self.tree.space_freed.get(self.tree.shown);
        if freed > 0 {
            words.push(format!("{} freed", DisplaySize(freed as f64)));
        }
        match self.rescans.len() {
            0 => {}
            1 => words.push("rescanning 1 folder".to_string()),
            n => words.push(format!("rescanning {} folders", DisplayCount(n as u64))),
        }
        words.join(" · ")
    }
}

/// One line on an entry: name, size, share of its folder, and what it is.
pub fn describe(entry: &FileMetadata) -> String {
    let kind = match (entry.file_type, entry.descendants) {
        (FileType::Folder, Some(count)) => format!("folder, {} items", DisplayCount(count)),
        (FileType::Folder, None) => "folder".to_string(),
        (FileType::File, _) => "file".to_string(),
    };
    format!(
        "{} — {} ({:.1}%) · {kind}",
        entry.name.to_string_lossy(),
        DisplaySize(entry.size as f64),
        entry.percentage * 100.0
    )
}

/// Drop `value` on a thread of its own: dropping a tree walks every folder in it, which is no
/// work for the thread that draws the window.
pub fn drop_later<T: Send + 'static>(value: T) {
    let _ = ::std::thread::Builder::new()
        .name("dropper".to_string())
        .spawn(move || drop(value));
}

/// A tile's colour, as sRGB components: folders in blues, files by their extension — so files
/// of a kind share a colour, from one listing to the next — in muted tones that white text
/// reads on.
pub fn tile_color(name: &OsStr, file_type: FileType, index: usize) -> (f64, f64, f64) {
    if file_type == FileType::Folder {
        const FOLDERS: [(f64, f64, f64); 4] = [
            (0.22, 0.42, 0.68),
            (0.25, 0.48, 0.76),
            (0.18, 0.36, 0.58),
            (0.29, 0.53, 0.80),
        ];
        return FOLDERS[index % FOLDERS.len()];
    }
    let extension = Path::new(name)
        .extension()
        .map(|extension| extension.to_ascii_lowercase());
    let Some(extension) = extension else {
        return (0.45, 0.45, 0.47);
    };
    // FNV-1a: stable across runs and platforms, unlike the std hasher.
    let hash = extension
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    // Hues clear of the folders' blues and the violets beside them (180°–280°), so a file never
    // passes for a folder.
    let hue = (hash % 260) as f64;
    let hue = if hue >= 180.0 { hue + 100.0 } else { hue };
    hsl(hue, 0.38, 0.46)
}

fn hsl(hue: f64, saturation: f64, lightness: f64) -> (f64, f64, f64) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    (r + m, g + m, b + m)
}

#[cfg(test)]
mod tests;
