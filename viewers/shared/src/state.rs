//! What the window shows and how it answers input, with no toolkit in it: the tree and its
//! board, where each panel sits, the entry in hand, marks, navigation, zoom, rescans, and what a
//! delete changes. Each desktop viewer (Win32 on Windows, AppKit on macOS, Wayland or X11 on
//! Linux) turns its events into calls here and draws what is here.
//!
//! It follows the terminal viewer's rules (`docs/features.md`, and "Key Patterns" in
//! `AGENTS.md`): the entry in hand is one *name*, which the list highlights and the treemap
//! selects; a plain move, jump, click or folder change clears the marks; a Shift run keeps the
//! marks it started from and shrinks when reversed; Ctrl+click takes in the entry already in
//! hand only if the user picked it (`chosen`), so nothing unchosen is ever deleted; every change
//! to the marks copies their paths, when the viewer has given it a clipboard; a rescan covered by
//! one under way is not started, and a delete inside a folder being rescanned restarts it.
//!
//! Kept free of any toolkit so that its tests run on the Linux CI like the rest of the workspace.

use ::std::ffi::{OsStr, OsString};
use ::std::mem::ManuallyDrop;
use ::std::path::{Path, PathBuf};
use ::std::time::{Duration, Instant};

use diskonaut_scan::rescan::{Outcome, Rescanner, Rescans};
use libdiskonaut::format::copied_path;
use libdiskonaut::model::SizeKind;
use libdiskonaut::tiles::{
    Area, Board, Expansion, FileMetadata, FileType, NestedTile, Nesting, Row,
};
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
/// The list's left padding, a level of indentation in the tree view, and the expander's column.
pub const LIST_PAD: f64 = 6.0;
pub const ROW_INDENT: f64 = 14.0;
pub const EXPANDER: f64 = 16.0;
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
        Self::with_top(width, height, sidebar, 0.0)
    }

    /// A layout that leaves the top `top` points to the viewer — a title bar it draws itself,
    /// where the windowing system draws none (Wayland without server-side decorations).
    pub fn with_top(width: f64, height: f64, sidebar: bool, top: f64) -> Self {
        let width = width.max(0.0);
        let height = height.max(0.0);
        let top = top.clamp(0.0, height);
        let body = Rect::new(
            0.0,
            top + PATH_BAR,
            width,
            (height - top - PATH_BAR - STATUS_BAR).max(0.0),
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
            path_bar: Rect::new(0.0, top, width, PATH_BAR),
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
    /// A folder row's expander, in the tree view: its index in the rows.
    Expander(usize),
    /// A tile inside a folder's tile, in the tree view: its index in [`Viewer::nested`].
    Nested(usize),
    Nothing,
}

/// Modifier keys held during a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    /// ⌘ (Ctrl on Linux): add or take away one entry from the marks.
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
    /// A picture; the image itself is the viewer's, which decodes it. This is its caption.
    Picture(String),
}

/// Where copied paths go. A viewer that copies through the toolkit itself leaves it unset.
type Clipboard = Box<dyn FnMut(&str) -> bool + Send>;

pub struct Viewer {
    /// The outline while the first scan runs; the finished tree after. Never dropped here: a
    /// tree's destructor walks every folder in it, so an old tree goes to [`drop_later`].
    pub tree: ManuallyDrop<FileTree>,
    pub board: Board,
    pub layout: Layout,
    pub sidebar: bool,
    /// Points at the top left to the viewer for a title bar of its own; see [`Layout::with_top`].
    pub top_inset: f64,
    pub focus: Focus,
    /// The entry in hand, by name, so that it stays in hand when the tiles are laid out again —
    /// a resize, a zoom, or new sizes arriving during a scan. Both panels show it.
    pub selected: Option<OsString>,
    /// Whether the entry in hand was picked by the user (an arrow, a jump, a click), rather than
    /// placed by the viewer (the top of a folder just entered, the folder just left, what took a
    /// deleted entry's place). Only a picked one is taken into a selection begun with a modified
    /// click, so nothing unchosen is ever deleted.
    pub chosen: bool,
    /// Marked entries, in the order marked. When there are any, they are what a delete, a copy
    /// or Show in Finder acts on; otherwise the entry in hand is.
    pub marked: Vec<OsString>,
    /// Where a ⇧ range starts.
    anchor: Option<OsString>,
    /// The marks there were when the run of ⇧ moves began: the range is added to them, so
    /// reversing the run shrinks it back towards its start rather than taking earlier marks out.
    mark_run: Option<Vec<OsString>>,
    /// Where copied paths go, if the viewer has said; see [`Viewer::set_clipboard`].
    clipboard: Option<Clipboard>,
    /// What copied paths are relative to: the working directory the viewer started in.
    working_dir: Option<PathBuf>,
    /// The listing's first row on screen.
    pub list_top: usize,
    pub hover: Option<OsString>,
    /// The list as a tree, WizTree's: folders open in place and their entries follow, indented.
    /// Only a viewer that draws rows with their depth and an expander turns it on; the others
    /// see the flat listing, since nothing opens.
    pub tree_view: bool,
    expansion: Expansion,
    rows: Vec<Row>,
    /// The row in hand, as its path from the listed folder — `[name]` for a top-level row. Its
    /// first name is `selected`, what the treemap and the marks know.
    cursor: Option<Vec<OsString>>,
    /// The row under the pointer.
    pub hover_row: Option<usize>,
    /// The treemap nested, in the tree view: the tiles inside the board's folder tiles.
    nested: Vec<NestedTile>,
    /// The nested tile under the pointer.
    pub hover_nested: Option<usize>,
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
    rescans: Rescans,
    /// The file the preview was last asked for (and the pixels it may take, when the viewer says
    /// — see [`Viewer::wanted_preview_sized`]), and the request's number: an answer to an older
    /// one is dropped.
    preview_for: Option<(PathBuf, Option<(u32, u32)>)>,
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
            top_inset: 0.0,
            focus: Focus::List,
            selected: None,
            chosen: false,
            marked: Vec::new(),
            anchor: None,
            mark_run: None,
            clipboard: None,
            // Resolved like a scan root is, so that `..` counts real directories on both sides.
            working_dir: ::std::env::current_dir()
                .and_then(|dir| dir.canonicalize())
                .ok(),
            list_top: 0,
            hover: None,
            tree_view: false,
            expansion: Expansion::default(),
            rows: Vec::new(),
            cursor: None,
            hover_row: None,
            nested: Vec::new(),
            hover_nested: None,
            zooms: Vec::new(),
            scanning: true,
            scan_id,
            entries_scanned: 0,
            last_read: None,
            scan_started: Instant::now(),
            scan_took: None,
            message: None,
            rescanner: None,
            rescans: Rescans::default(),
            preview_for: None,
            preview_generation: 0,
            preview: Preview::None,
        }
    }

    /// The list as a tree and the treemap nested, for a viewer that draws rows with their
    /// depth and tiles inside tiles.
    pub fn set_tree_view(&mut self, on: bool) {
        self.tree_view = on;
        if !on {
            self.expansion.clear();
            self.rebuild_rows();
        }
        self.rebuild_nested();
        self.sync_board();
    }

    /// Allow rescans, which `rescanner` runs.
    pub fn enable_rescans(&mut self, rescanner: Rescanner) {
        self.rescanner = Some(rescanner);
    }

    /// Copy paths through `clipboard` — [`libdiskonaut::clipboard::copy`], or a record in a
    /// test. Once set, every change to the marks copies their paths, as the terminal viewer
    /// does; a viewer that copies through its toolkit instead leaves this unset and asks for
    /// [`Viewer::target_paths`].
    pub fn set_clipboard(&mut self, clipboard: impl FnMut(&str) -> bool + Send + 'static) {
        self.clipboard = Some(Box::new(clipboard));
    }

    /// What copied paths are relative to; the working directory unless told otherwise.
    pub fn set_working_dir(&mut self, working_dir: Option<PathBuf>) {
        self.working_dir = working_dir;
    }

    pub fn root(&self) -> &Path {
        &self.tree.path_in_filesystem
    }

    // ---------------------------------------------------------------- layout

    pub fn resize(&mut self, width: f64, height: f64) {
        self.layout = Layout::with_top(width, height, self.sidebar, self.top_inset);
        self.board.change_area(&Area {
            x: 0,
            y: 0,
            width: self.layout.cols,
            height: self.layout.rows,
        });
        self.rebuild_nested();
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
        self.rebuild_rows();
        self.rebuild_nested();
        self.sync_board();
        self.clamp_list_top();
    }

    /// The tiles inside the folder tiles, when the tree view is on; none otherwise.
    fn rebuild_nested(&mut self) {
        self.nested = if self.tree_view {
            libdiskonaut::tiles::nest(
                self.tree.get_current_folder(),
                &self.board.tiles,
                self.tree.shown,
                &Nesting::default(),
            )
        } else {
            Vec::new()
        };
        if self.hover_nested.is_some_and(|i| i >= self.nested.len()) {
            self.hover_nested = None;
        }
    }

    /// The tree view's treemap: the tiles inside the board's folder tiles, parents first.
    #[must_use]
    pub fn nested(&self) -> &[NestedTile] {
        &self.nested
    }

    /// The nested tile of the row in hand, if it has one.
    #[must_use]
    pub fn cursor_nested(&self) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        self.nested.iter().position(|tile| &tile.path == cursor)
    }

    /// The deepest nested tile under a cell.
    fn nested_at(&self, column: u16, row: u16) -> Option<usize> {
        self.nested
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                (t.tile.x..t.tile.x.saturating_add(t.tile.width)).contains(&column)
                    && (t.tile.y..t.tile.y.saturating_add(t.tile.height)).contains(&row)
            })
            .max_by_key(|(_, t)| t.depth)
            .map(|(index, _)| index)
    }

    /// Open the folders above `path` in the tree, so its row is there, and put it in hand.
    fn reveal(&mut self, path: Vec<OsString>, chosen: bool) {
        for depth in 1..path.len() {
            self.expansion.open(&path[..depth]);
        }
        self.rebuild_rows();
        self.select_row(path, chosen);
    }

    fn rebuild_rows(&mut self) {
        self.rows = self
            .expansion
            .rows(self.tree.get_current_folder(), self.tree.shown);
    }

    /// Keep the entry in hand and the marks to entries that exist, and put the board's
    /// selection on the tile of the entry in hand, wherever the layout put it.
    fn sync_board(&mut self) {
        let listing = self.board.listing();
        let listed = |name: &OsString| listing.iter().any(|entry| &entry.name == name);
        if self.selected.as_ref().is_some_and(|name| !listed(name)) {
            self.selected = None;
            self.chosen = false;
        }
        self.marked.retain(|name| listed(name));
        if self.hover.as_ref().is_some_and(|name| !listed(name)) {
            self.hover = None;
        }
        // A row that has gone (a delete, a rescan, a folder closed over it) leaves the cursor on
        // the entry in hand's own row.
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| !self.rows.iter().any(|row| &row.path == cursor))
        {
            self.cursor = self.selected.clone().map(|name| vec![name]);
        }
        if self.hover_row.is_some_and(|row| row >= self.rows.len()) {
            self.hover_row = None;
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

    // ---------------------------------------------------------------- the rows

    /// What the list shows: the folder's entries, and under each folder open in place its
    /// own. With nothing open, the flat listing in its order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row in hand, by its index in [`Viewer::rows`].
    #[must_use]
    pub fn cursor_row(&self) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        self.rows.iter().position(|row| &row.path == cursor)
    }

    /// The row in hand.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Row> {
        self.cursor_row().map(|index| &self.rows[index])
    }

    /// Where a row's entry is on disk.
    #[must_use]
    pub fn row_path(&self, relative: &[OsString]) -> PathBuf {
        relative
            .iter()
            .fold(self.tree.get_current_path(), |path, name| path.join(name))
    }

    /// Open a folder's row in place, or close it. The row stays in hand.
    pub fn toggle_row(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if row.entry.file_type != FileType::Folder {
            return;
        }
        let path = row.path.clone();
        self.expansion.toggle(&path);
        self.rebuild_rows();
        self.select_row(path, true);
    }

    fn clamp_list_top(&mut self) {
        let rows = self.layout.list_rows().max(1);
        let most = self.rows.len().saturating_sub(rows);
        self.list_top = self.list_top.min(most);
    }

    fn scroll_to_selected(&mut self) {
        if let Some(index) = self.cursor_row() {
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
        self.absorb_summaries(summaries);
        self.catch_up();
    }

    /// A batch of the outline into the tree, and nothing else: what is shown is not touched
    /// until [`Viewer::catch_up`]. A window that gets dozens of batches a second lays the view
    /// out once per frame rather than once per batch — laid out per batch, the window's
    /// thread is fully taken up by the scan and answers nothing until it ends.
    pub fn absorb_summaries(&mut self, summaries: Vec<DirSummary>) {
        for summary in summaries {
            self.entries_scanned += summary.entries;
            self.tree.failed_to_read += summary.dirs.failed;
            self.last_read = Some(summary.dirs.path.to_path_buf());
            self.tree.add_summary(summary);
        }
    }

    /// Lay the view out for what the outline holds by now.
    pub fn catch_up(&mut self) {
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

    /// Put `name` in hand: `chosen` says whether the user picked it or the viewer placed it. A
    /// plain selection ends any ⇧ run and starts the next range here.
    fn select(&mut self, name: Option<OsString>, chosen: bool) {
        self.selected = name;
        self.chosen = chosen && self.selected.is_some();
        self.anchor = self.selected.clone();
        self.mark_run = None;
        self.cursor = self.selected.clone().map(|name| vec![name]);
        self.sync_board();
        self.scroll_to_selected();
    }

    /// Put a row in hand by its path; its top-level entry is what the treemap selects.
    fn select_row(&mut self, path: Vec<OsString>, chosen: bool) {
        self.selected = path.first().cloned();
        self.chosen = chosen && self.selected.is_some();
        self.anchor = self.selected.clone();
        self.mark_run = None;
        self.cursor = Some(path);
        self.sync_board();
        self.scroll_to_selected();
    }

    /// The top of the list comes into hand, placed rather than picked.
    fn select_first(&mut self) {
        let first = self.board.listing().first().map(|entry| entry.name.clone());
        self.select(first, false);
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
                let next = match self.cursor_row() {
                    Some(index) => index.saturating_add_signed(delta),
                    None => 0,
                };
                self.move_list_to(next, extend);
            }
            // In the tree view → opens the folder in hand in place, and once open goes down
            // into it; on a file it crosses to the treemap, as ever. ← closes the folder in
            // hand, or goes up to the row it is under.
            (Focus::List, Direction::Right) => match self.cursor_entry() {
                Some(row) if self.tree_view && row.entry.file_type == FileType::Folder => {
                    let index = self.cursor_row().unwrap_or(0);
                    if row.open {
                        self.move_list_to(index + 1, false);
                    } else {
                        self.toggle_row(index);
                    }
                }
                _ => self.focus = Focus::Treemap,
            },
            (Focus::List, Direction::Left) => {
                if let Some(row) = self.cursor_entry().filter(|_| self.tree_view) {
                    if row.open {
                        let index = self.cursor_row().unwrap_or(0);
                        self.toggle_row(index);
                    } else if row.depth > 0 {
                        let parent = row.path[..row.depth].to_vec();
                        self.clear_marks();
                        self.select_row(parent, true);
                    }
                }
            }
            (Focus::Treemap, _) => {
                self.clear_marks();
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
                        self.select(Some(name), true);
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
        let current = self.cursor_row().unwrap_or(0);
        let last = self.rows.len().saturating_sub(1);
        let next = match jump {
            Jump::PageUp => current.saturating_sub(page),
            Jump::PageDown => current.saturating_add(page),
            Jump::Home => 0,
            Jump::End => last,
        };
        self.move_list_to(next, extend);
    }

    fn move_list_to(&mut self, index: usize, extend: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let index = index.min(len - 1);
        let path = self.rows[index].path.clone();
        if extend {
            // A ⇧ range runs over the folder's own entries: the marks are theirs. A row under
            // an open folder takes the cursor, and its top-level folder is what is marked.
            let anchor = self.anchor.clone().or_else(|| self.selected.clone());
            self.selected = path.first().cloned();
            self.chosen = true;
            self.anchor = anchor.clone();
            if let Some(to) = self.listing_index(self.selected.as_deref()) {
                self.mark_range(anchor.as_deref(), to);
            }
            self.cursor = Some(path);
            self.sync_board();
            self.scroll_to_selected();
        } else {
            self.clear_marks();
            self.select_row(path, true);
        }
    }

    /// Take every mark off, and with them the ⇧ run they were part of — a later ⇧ move starts a
    /// range from nothing, and never brings back marks the user saw cleared.
    fn clear_marks(&mut self) {
        self.marked.clear();
        self.mark_run = None;
    }

    /// Mark every entry from `anchor` to the listing's `to`, swept in that order, on top of the
    /// marks there were when the ⇧ run began — so reversing the run shrinks the range back
    /// towards the anchor and no earlier mark is lost. Then copy them.
    fn mark_range(&mut self, anchor: Option<&OsStr>, to: usize) {
        let listing = self.board.listing();
        // Marks from the run's start that have since left the listing (a delete, a rescan) stay
        // gone.
        let base: Vec<OsString> = self
            .mark_run
            .get_or_insert_with(|| self.marked.clone())
            .iter()
            .filter(|name| listing.iter().any(|entry| &entry.name == *name))
            .cloned()
            .collect();
        let from = self.listing_index(anchor).unwrap_or(to);
        let swept: Vec<OsString> = if from <= to {
            (from..=to).map(|row| listing[row].name.clone()).collect()
        } else {
            (to..=from)
                .rev()
                .map(|row| listing[row].name.clone())
                .collect()
        };
        let mut marked = base;
        for name in swept {
            if !marked.contains(&name) {
                marked.push(name);
            }
        }
        self.marked = marked;
        self.copy_marked();
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
        self.mark_run = None;
        self.copy_marked();
    }

    // ---------------------------------------------------------------- copying paths

    /// Copy the path of every marked entry, or else of the one in hand: relative to the working
    /// directory unless `absolute` (or there is no relative path), quoted for the shell and
    /// separated by spaces, ready to paste after a command. Says what was copied. `false` if
    /// there is no clipboard, or nothing to copy.
    pub fn copy_paths(&mut self, absolute: bool) -> bool {
        let names = self.target_names();
        let paths: Vec<(_, String)> = names
            .iter()
            .map(|name| copied_path(&self.path_of(name), self.working_dir.as_deref(), absolute))
            .collect();
        let label = match paths.as_slice() {
            [] => return false,
            [(kind, _)] if self.marked.is_empty() => format!("Copied {} path:", kind.name()),
            [_] => "Copied 1 path:".to_string(),
            many => format!("Copied {} paths:", DisplayCount(many.len() as u64)),
        };
        let text = paths
            .iter()
            .map(|(_, path)| path.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        self.copy_text(&text, &label)
    }

    /// The marks changed: copy their paths, if there is a clipboard to copy to.
    fn copy_marked(&mut self) {
        if self.clipboard.is_some() && !self.marked.is_empty() {
            self.copy_paths(false);
        }
    }

    fn copy_text(&mut self, text: &str, label: &str) -> bool {
        let Some(clipboard) = self.clipboard.as_mut() else {
            return false;
        };
        if clipboard(text) {
            self.say(format!("{label} {text}"));
            true
        } else {
            self.say("Could not reach the clipboard");
            false
        }
    }

    // ---------------------------------------------------------------- the mouse

    pub fn hit(&self, x: f64, y: f64) -> Hit {
        if let Some(list) = self.layout.list
            && list.contains(x, y)
        {
            let row = ((y - list.y) / ROW) as usize;
            let index = self.list_top + row;
            // Past the last whole row is a sliver where no row is drawn.
            if row >= self.layout.list_rows() || index >= self.rows.len() {
                return Hit::Nothing;
            }
            // In the tree view a folder row's expander is its own target.
            let entry = &self.rows[index];
            let expander = list.x + LIST_PAD + entry.depth as f64 * ROW_INDENT;
            if self.tree_view
                && entry.entry.file_type == FileType::Folder
                && (expander..expander + EXPANDER).contains(&x)
            {
                return Hit::Expander(index);
            }
            return Hit::Row(index);
        }
        let Some((col, row)) = self.layout.cell_at(x, y) else {
            return Hit::Nothing;
        };
        if let Some(index) = self.nested_at(col, row) {
            return Hit::Nested(index);
        }
        if let Some(index) = self.board.tile_at(col, row) {
            return Hit::Tile(self.board.tiles[index].name.clone());
        }
        match self.board.unrenderable_tile_coordinates {
            Some((sx, sy)) if col >= sx && row >= sy => Hit::SmallFiles,
            _ => Hit::Nothing,
        }
    }

    /// A press of the main button: take the entry under the pointer in hand, with ⌘ (Ctrl)
    /// toggling its mark and ⇧ marking the range to it from the anchor. Returns the entry's name.
    pub fn click(&mut self, x: f64, y: f64, mods: Mods) -> Option<OsString> {
        let (focus, path) = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => (Focus::List, self.rows[index].path.clone()),
            Hit::Tile(name) => (Focus::Treemap, vec![name]),
            // A tile inside a folder's: the folders above it open in the tree, and it is the
            // row in hand.
            Hit::Nested(index) => {
                let path = self.nested[index].path.clone();
                self.focus = Focus::Treemap;
                self.clear_marks();
                self.reveal(path.clone(), true);
                return path.first().cloned();
            }
            Hit::SmallFiles | Hit::Nothing => return None,
        };
        let name = path[0].clone();
        self.focus = focus;
        if mods.range {
            let to = self.listing_index(Some(&name))?;
            let anchor = self.anchor.clone().or_else(|| self.selected.clone());
            self.mark_range(anchor.as_deref(), to);
            self.selected = Some(name.clone());
            self.chosen = true;
            self.anchor = anchor;
            self.cursor = Some(path);
            self.sync_board();
        } else if mods.toggle {
            // Starting a selection takes in the entry already in hand, as a file manager does —
            // but only one the user picked, never one the viewer placed there.
            if self.marked.is_empty()
                && self.chosen
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
            // Placed, not picked: a click that marks does not choose what a later one adds.
            self.select_row(path, false);
            self.copy_marked();
        } else {
            self.clear_marks();
            self.select_row(path, true);
        }
        Some(name)
    }

    /// Take the entry under the pointer in hand for a context menu, keeping the marks if it is
    /// one of them. Returns whether there is an entry there.
    pub fn context_click(&mut self, x: f64, y: f64) -> bool {
        let path = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => self.rows[index].path.clone(),
            Hit::Tile(name) => vec![name],
            Hit::Nested(index) => self.nested[index].path.clone(),
            Hit::SmallFiles | Hit::Nothing => return false,
        };
        if !self.is_marked(&path[0]) {
            self.clear_marks();
        }
        self.reveal(path, true);
        true
    }

    /// The pointer moved. Returns whether what it is over changed.
    pub fn hover_at(&mut self, x: f64, y: f64) -> bool {
        let (hover, hover_row, hover_nested) = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => {
                (Some(self.rows[index].path[0].clone()), Some(index), None)
            }
            Hit::Tile(name) => (Some(name), None, None),
            Hit::Nested(index) => (Some(self.nested[index].path[0].clone()), None, Some(index)),
            Hit::SmallFiles | Hit::Nothing => (None, None, None),
        };
        let changed =
            hover != self.hover || hover_row != self.hover_row || hover_nested != self.hover_nested;
        self.hover = hover;
        self.hover_row = hover_row;
        self.hover_nested = hover_nested;
        changed
    }

    // ---------------------------------------------------------------- navigation

    /// Enter the entry in hand, if it is a folder. Returns whether it was.
    pub fn enter_selected(&mut self) -> bool {
        let path = match self.cursor_entry() {
            Some(row) if row.entry.file_type == FileType::Folder => row.path.clone(),
            _ => return false,
        };
        // A row under an open folder: down through each folder above it.
        path.iter().all(|name| self.enter(name))
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
        self.clear_marks();
        self.expansion.clear();
        self.hover = None;
        self.hover_row = None;
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
        self.clear_marks();
        self.expansion.clear();
        self.hover = None;
        self.hover_row = None;
        self.refresh();
        self.select(Some(left), false);
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
        self.rebuild_nested();
        self.sync_board();
    }

    pub fn zoom_out(&mut self) {
        self.board.zoom_out(self.tree.get_current_folder());
        self.rebuild_nested();
        self.sync_board();
    }

    pub fn reset_zoom(&mut self) {
        self.board.reset_zoom(self.tree.get_current_folder());
        self.rebuild_nested();
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
        if self.marked.is_empty()
            && let Some(row) = self.cursor_entry()
        {
            return vec![self.row_path(&row.path)];
        }
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
        // Nothing marked: the row in hand, wherever in the tree it is.
        if self.marked.is_empty()
            && let Some(row) = self.cursor_entry()
        {
            return vec![FileToDelete::in_current_tree(
                &self.tree, &row.path, &row.entry,
            )];
        }
        self.target_names()
            .iter()
            .filter_map(|name| self.entry_named(name))
            .map(|entry| FileToDelete::in_current_folder(&self.tree, entry.clone()))
            .collect()
    }

    /// The question to ask before deleting `files` for good.
    #[must_use]
    pub fn delete_prompt(files: &[FileToDelete]) -> String {
        let size: u128 = files.iter().map(|file| file.size).sum();
        match files {
            [one] => format!(
                "Delete {}?\n\n{} will be permanently removed from disk.",
                one.full_path().display(),
                DisplaySize(size as f64)
            ),
            many => {
                const SHOWN: usize = 10;
                let mut names: Vec<String> = many
                    .iter()
                    .take(SHOWN)
                    .map(|file| {
                        format!("  {}  ({})", last_name(file), DisplaySize(file.size as f64))
                    })
                    .collect();
                if many.len() > SHOWN {
                    names.push(format!(
                        "  and {} more",
                        DisplayCount((many.len() - SHOWN) as u64)
                    ));
                }
                format!(
                    "Delete these {} entries?\n\n{}\n\n{} will be permanently removed from disk.",
                    DisplayCount(many.len() as u64),
                    names.join("\n"),
                    DisplaySize(size as f64)
                )
            }
        }
    }

    /// Why `files` may not be deleted, if one of them is NTFS's own metadata — to say before
    /// asking, as well as before touching anything.
    #[must_use]
    pub fn refusal(files: &[FileToDelete]) -> Option<String> {
        libdiskonaut::delete::refused(files).map(|name| {
            format!("NTFS metadata belongs to the filesystem and cannot be deleted: {name}")
        })
    }

    /// Delete `files` from disk for good, going on past any that fail: only what left the disk
    /// comes off the tree and counts as freed. NTFS's own metadata is refused before anything is
    /// touched. The error names what failed — the first failure, and how many more.
    pub fn delete(&mut self, files: &[FileToDelete]) -> Result<(), String> {
        if let Some(refusal) = Self::refusal(files) {
            return Err(refusal);
        }
        let mut deleted = Vec::new();
        let mut failures = Vec::new();
        for file in files {
            match libdiskonaut::delete::remove(file) {
                Ok(()) => deleted.push(file.clone()),
                Err(error) => failures.push((file, error)),
            }
        }
        self.removed(&deleted, true);
        match failures.as_slice() {
            [] => Ok(()),
            [(file, error)] if files.len() == 1 => Err(format!(
                "Could not delete {}: {error}",
                file.full_path().display()
            )),
            [(file, error), rest @ ..] => {
                let more = if rest.is_empty() {
                    String::new()
                } else {
                    format!(" (and {} more)", DisplayCount(rest.len() as u64))
                };
                Err(format!(
                    "Deleted {} of {}; {}: {error}{more}",
                    DisplayCount(deleted.len() as u64),
                    DisplayCount(files.len() as u64),
                    last_name(file)
                ))
            }
        }
    }

    /// Take entries that have left the disk off the tree. `freed` says whether their space came
    /// back (a delete), or only moved elsewhere (to the Trash). What took the first one's place
    /// in the list is put in hand, placed rather than picked. A rescan under way of a folder one
    /// of them was in is started again, or it would put the entry back.
    pub fn removed(&mut self, files: &[FileToDelete], freed: bool) {
        let at = self.cursor_row();
        let had = self.cursor.clone();
        for file in files {
            if self.tree.remove_path(&file.path_to_file) && freed {
                self.tree.note_freed(file.sizes);
            }
        }
        if !files.is_empty()
            && let Some(rescanner) = &self.rescanner
        {
            self.rescans.restart_under(rescanner, &self.tree, files);
        }
        self.clear_marks();
        self.leave_vanished_folders();
        self.refresh();
        // The row in hand went with them: the row now where it was — its neighbour, or with a
        // folder re-sorted by what it lost, whatever came down to there — is put in hand.
        let gone = had.is_some_and(|had| !self.rows.iter().any(|row| row.path == had));
        if gone
            && let Some(at) = at
            && !self.rows.is_empty()
        {
            let path = self.rows[at.min(self.rows.len() - 1)].path.clone();
            self.select_row(path, false);
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

    /// Rescan the folder at `relative`. Not while the first scan runs (it is reading all of it
    /// anyway), nor when a rescan under way covers it; one that this covers is stopped, as this
    /// brings its folder back too — [`Rescans`] keeps those rules for every viewer.
    fn start_rescan(&mut self, relative: Vec<OsString>) {
        if self.scanning {
            return;
        }
        if let Some(rescanner) = &self.rescanner {
            self.rescans.start(rescanner, &self.tree, relative);
        }
    }

    /// What is being rescanned, for a status line; `None` when nothing is.
    pub fn rescanning(&self) -> Option<String> {
        self.rescans.describe(&self.tree.path_in_filesystem)
    }

    /// Stop every rescan, as the window moves on to another folder.
    pub fn cancel_rescans(&mut self) {
        self.rescans.cancel_all();
    }

    /// A rescan has finished: put what it found in the tree. One stopped meanwhile is dropped.
    pub fn rescan_done(&mut self, id: u64, outcome: Outcome) {
        let not_walked = matches!(outcome, Outcome::NotWalked);
        let navigated_to = self.tree.current_folder_names.clone();
        let Some(finished) = self.rescans.finish(id, outcome, &mut self.tree) else {
            return;
        };
        if not_walked {
            self.say("That folder is not scanned (another filesystem, or past the depth limit)");
        }
        // The folder replaced is freed off the window's thread; a window lives long enough for
        // leaking it every rescan to add up.
        if let Some(old) = finished.old {
            drop_later(old);
        }
        if let Some((duration, _small)) = finished.whole {
            self.scan_took = Some(duration);
            self.entries_scanned = self.tree.get_total_descendants();
        }
        if finished.changed {
            if self.tree.current_folder_names != navigated_to {
                self.zooms.clear();
                self.board.reset_zoom_index();
                self.clear_marks();
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
        self.wanted_preview_sized(None)
    }

    /// [`Viewer::wanted_preview`] for a viewer that prepares the picture at the size it will be
    /// drawn: `pixels` is part of what was asked, so a resize asks again for the same file.
    pub fn wanted_preview_sized(&mut self, pixels: Option<(u32, u32)>) -> Option<(u64, PathBuf)> {
        let target = self
            .cursor_entry()
            .filter(|row| row.entry.file_type == FileType::File)
            .map(|row| (self.row_path(&row.path), pixels));
        if target == self.preview_for {
            return None;
        }
        self.preview_for = target.clone();
        self.preview_generation += 1;
        match target {
            Some((path, _)) => {
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

    /// How long the status bar's message has left, if one is showing: a viewer without a timer
    /// of its own arranges a redraw for then.
    pub fn message_left(&self) -> Option<Duration> {
        let (_, at) = self.message.as_ref()?;
        MESSAGE_TIME
            .checked_sub(at.elapsed())
            .filter(|left| !left.is_zero())
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

    /// The status bar: on the left a message while it lasts (what was copied, say), else the
    /// marks' count and size, else what the pointer or the keyboard is on; on the right what the
    /// scan found.
    pub fn status(&self) -> (String, String) {
        let left = match &self.message {
            Some((message, at)) if at.elapsed() < MESSAGE_TIME => message.clone(),
            _ if !self.marked.is_empty() && self.hover.is_none() => {
                let size: u128 = self
                    .marked
                    .iter()
                    .filter_map(|name| self.entry_named(name))
                    .map(|entry| entry.size)
                    .sum();
                format!(
                    "{} marked, {}",
                    DisplayCount(self.marked.len() as u64),
                    DisplaySize(size as f64)
                )
            }
            _ => {
                let nested = self.hover_nested.and_then(|index| self.nested.get(index));
                let entry = self
                    .hover_row
                    .and_then(|index| self.rows.get(index))
                    .map(|row| &row.entry)
                    .or_else(|| self.hover.as_ref().and_then(|name| self.entry_named(name)))
                    .or_else(|| self.cursor_entry().map(|row| &row.entry));
                match (nested, entry) {
                    (Some(nested), _) => describe_tile(&nested.tile),
                    (None, Some(entry)) => describe(entry),
                    (None, None) if self.scanning => "Scanning…".to_string(),
                    (None, None) => String::new(),
                }
            }
        };
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
        if self.board.zoom_level > 0 {
            words.push(format!("zoom ×{}", self.board.zoom_level));
        }
        match self.rescans.len() {
            0 => {}
            1 => words.push("rescanning 1 folder".to_string()),
            n => words.push(format!("rescanning {} folders", DisplayCount(n as u64))),
        }
        words.join(" · ")
    }
}

/// An entry's own name, from its path in the tree.
fn last_name(file: &FileToDelete) -> String {
    file.path_to_file
        .last()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// [`describe`] for a tile: the same line, from what a tile carries.
pub fn describe_tile(tile: &libdiskonaut::tiles::Tile) -> String {
    describe(&FileMetadata {
        name: tile.name.clone(),
        size: tile.size,
        descendants: tile.descendants,
        percentage: tile.percentage,
        file_type: tile.file_type,
    })
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
