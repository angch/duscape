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

use ::std::collections::HashMap;
use ::std::ffi::{OsStr, OsString};
use ::std::mem::ManuallyDrop;
use ::std::path::{Path, PathBuf};
use ::std::time::{Duration, Instant};

use duscape_scan::Focus as ScanFocus;
use duscape_scan::rescan::{Outcome, Rescanner, Rescans};
use libduscape::delete::Tally;
use libduscape::format::copied_path;
use libduscape::model::SizeKind;
use libduscape::model::files::hash::FastBuildHasher;
use libduscape::tiles::{
    Area, Board, Expansion, FileMetadata, FileType, FreeSpace, Grid, Inside, NestedTile, Nesting,
    Plans, Row, Speck, Tile, nest_steady, nest_with,
};
use libduscape::{
    DirSummary, DisplayCount, DisplaySize, FileOrFolder, FileToDelete, FileTree, Folder,
};

use crate::deleting::{Deletion, Ended, Failure};

/// A layout cell, in points. The treemap lays tiles out in cells 2.5 times taller than wide (its
/// `HEIGHT_WIDTH_RATIO`: a terminal's cell), so cells this shape come out as square-looking
/// tiles, and its 8×3-cell minimum tile becomes about 19×18 points.
pub const CELL_W: f64 = 2.4;
pub const CELL_H: f64 = 6.0;
/// Told the screen's pixels per point ([`Viewer::set_pixel_scale`]), the treemap is laid out in
/// square cells of one pixel instead, and an entry gets a tile of its own once it would be
/// this many pixels either way: a frame with colour inside. So the nesting goes on down to
/// whatever the screen can show, however big the window or fine its pixels.
pub const MIN_TILE_PIXELS: u16 = 4;
/// Most cells the treemap has either way; past that a cell is more than a pixel.
const MAX_CELLS: f64 = 4096.0;
/// The band at the top of a folder's tile for its label, and the margin its entries are kept
/// in from its sides and bottom, in points.
pub const TILE_LABEL: f64 = 18.0;
pub const TILE_MARGIN: f64 = 2.0;
/// What a relayout may take and still leave a 60 Hz frame room to paint: past it, the deeper
/// nesting and the specks of the "small files" corners wait for a second pass
/// ([`Viewer::defer_to_second_pass`]).
pub const LAYOUT_BUDGET: Duration = Duration::from_millis(10);
/// How long input must have stopped before a viewer runs that second pass
/// ([`Viewer::finish_second_pass`]), and paints in full what a first paint left out: long
/// enough that a drag of the window's edge is not held up.
pub const IDLE: Duration = Duration::from_millis(60);
/// How long the pointer rests on a treemap tile before the details panel shows that entry in
/// the entry in hand's place, and how long after it leaves before the entry in hand is shown
/// again ([`Viewer::peek_due`], [`Viewer::peek_tick`]).
pub const PEEK: Duration = Duration::from_millis(100);
/// Entries a folder lays out at most in a first pass: the largest, so it is the smallest that
/// wait. A Windows component store (27k entries) took 10–16 ms alone laid out whole.
pub const FIRST_PASS_ROOM: usize = 1000;
/// The breadcrumb bar across the top, and the status bar across the bottom.
pub const PATH_BAR: f64 = 30.0;
/// The button at the path bar's left that opens the chooser (what else to scan): a square.
pub const CHOOSER_BUTTON: f64 = PATH_BAR;
/// The width of the free-space toggle at the path bar's right ([`Layout::free_toggle`]).
pub const FREE_TOGGLE: f64 = 120.0;
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
    /// At the path bar's left: the button that opens the chooser over the scan.
    pub chooser_button: Rect,
    /// The breadcrumbs, from the button to the right edge.
    pub path_bar: Rect,
    /// At the path bar's right end, over it: the toggle for the volume's free space
    /// ([`Viewer::free_toggle`]), drawn there only while the root of a volume is shown — a
    /// painter then keeps the breadcrumbs and the size out from under it.
    pub free_toggle: Rect,
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
        Self::with_cells(width, height, sidebar, top, None)
    }

    /// A layout whose treemap is in square cells of one pixel at `pixel_scale` pixels per
    /// point (fewer, if the treemap would be more than `MAX_CELLS` of them), or in the
    /// terminal-shaped `CELL_W`×`CELL_H` cells without one.
    pub fn with_cells(
        width: f64,
        height: f64,
        sidebar: bool,
        top: f64,
        pixel_scale: Option<f64>,
    ) -> Self {
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
        let cells = |points: f64, cell: f64| (points / cell).floor().clamp(1.0, MAX_CELLS) as u16;
        let (cell_w, cell_h) = match pixel_scale {
            Some(scale) => {
                let cell = (1.0 / scale)
                    .max(treemap.w / MAX_CELLS)
                    .max(treemap.h / MAX_CELLS);
                (cell, cell)
            }
            None => (CELL_W, CELL_H),
        };
        Layout {
            bounds: Rect::new(0.0, 0.0, width, height),
            chooser_button: Rect::new(0.0, top, CHOOSER_BUTTON.min(width), PATH_BAR),
            path_bar: Rect::new(
                CHOOSER_BUTTON.min(width),
                top,
                (width - CHOOSER_BUTTON).max(0.0),
                PATH_BAR,
            ),
            free_toggle: Rect::new(
                (width - FREE_TOGGLE).max(CHOOSER_BUTTON.min(width)),
                top,
                FREE_TOGGLE.min((width - CHOOSER_BUTTON).max(0.0)),
                PATH_BAR,
            ),
            list,
            info,
            treemap,
            status: Rect::new(0.0, (height - STATUS_BAR).max(0.0), width, STATUS_BAR),
            cols: cells(treemap.w, cell_w),
            rows: cells(treemap.h, cell_h),
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

    /// The parts of `tile` to fill, in points within `clip` (the tile, or it inset): the whole
    /// of it, or where entries were laid out in it (`inside`) only around what they cover,
    /// since they are drawn over the rest — filled whole, each level painted its parent's
    /// area again.
    pub fn fill_parts(
        &self,
        tile: &Tile,
        inside: Option<&Inside>,
        clip: Rect,
    ) -> impl Iterator<Item = Rect> + '_ {
        let parts = inside.map(|inside| inside.around(&tile.area()));
        let whole = parts.is_none().then_some(clip);
        parts
            .into_iter()
            .flatten()
            .filter(|part| part.width > 0 && part.height > 0)
            .map(move |part| {
                let part = self.cells_to_rect(part.x, part.y, part.width, part.height);
                let (x, y) = (part.x.max(clip.x), part.y.max(clip.y));
                let (right, bottom) = (
                    part.right().min(clip.right()),
                    part.bottom().min(clip.bottom()),
                );
                Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
            })
            .chain(whole)
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

/// A speck's colour, by the rule a tile of its entry would have ([`entry_color`]); a file's
/// extension's worked out once for each, not once a speck.
#[derive(Default)]
struct SpeckColors {
    by_extension: HashMap<OsString, (f64, f64, f64), FastBuildHasher>,
    /// The last file speck's extension, its depth and colour: a corner's files come in runs of
    /// a kind, and one compared costs less than one hashed and looked up.
    last: Option<(OsString, usize, (f64, f64, f64))>,
}

impl SpeckColors {
    fn dust(&mut self, speck: &Speck) -> Dust {
        let entry = speck.entry;
        let color = if entry.file_type == FileType::Folder {
            entry_color(entry.name, entry.file_type, speck.depth)
        } else {
            self.file(entry.name, speck.depth)
        };
        let area = speck.area;
        let framed = area.width >= 2 * MIN_TILE_PIXELS && area.height >= 2 * MIN_TILE_PIXELS;
        Dust {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
            color: if framed {
                color
            } else {
                darker(color, SPECK_SHADE)
            },
            framed,
        }
    }

    /// A file speck's colour, `depth` in.
    fn file(&mut self, name: &OsStr, depth: usize) -> (f64, f64, f64) {
        // None is not an empty extension: `Makefile` is grey, `notes.` hashes its "" as a
        // kind; keyed alike, whichever came first coloured both.
        let Some(extension) = Path::new(name).extension() else {
            return entry_color(name, FileType::File, depth);
        };
        if let Some((last, last_depth, color)) = &self.last
            && *last_depth == depth
            && last == extension
        {
            return *color;
        }
        let base = match self.by_extension.get(extension) {
            Some(&color) => color,
            None => {
                let color = tile_color(name, FileType::File);
                self.by_extension.insert(extension.to_os_string(), color);
                color
            }
        };
        let color = darker(base, depth_shade(depth));
        match &mut self.last {
            Some((last, last_depth, last_color)) => {
                last.clear();
                last.push(extension);
                *last_depth = depth;
                *last_color = color;
            }
            None => self.last = Some((extension.to_os_string(), depth, color)),
        }
        color
    }
}

/// One entry of a "small files" corner, in the board's cells, and the colour of its kind;
/// where it has no frame, [`SPECK_SHADE`] darker and with the least tiles' grid drawn over it
/// ([`Dust::grid`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dust {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub color: (f64, f64, f64),
    /// Room for a frame and colour inside it, as a tile has (twice the least tile either way):
    /// a painter frames it, as it frames a tile. Smaller, a framed speck would read as a
    /// hollow box, so the grid of the least tiles goes over it instead.
    pub framed: bool,
}

/// How much darker an unframed speck is than its tile's colour, under its grid
/// ([`Dust::grid`]): the grid leaves colour on 9/16 of a corner, and the fields of small tiles
/// beside one measured 0.45 of theirs on Windows (2026-10-05: luminance 56 and 62 beside 64 and
/// 80 in the corner, undarkened, on `WinSxS\Manifests` and a folder of 30,000 small files of six
/// kinds), each tile drawing its own frame so two lie between neighbours. 9/16 × 0.8 = 0.45.
pub const SPECK_SHADE: f64 = 0.8;

impl Dust {
    /// The lines of the least tiles' grid inside this speck, in the board's cells: one cell
    /// wide, on every column and row that is a multiple of [`MIN_TILE_PIXELS`]. A painter
    /// draws them in the tiles' frame colour over an unframed speck, so a corner of specks
    /// reads as a field of the least tiles in their colours — which is what it lies beside —
    /// not as a block of flat colour. Plain, the specks were twice as bright on average as the
    /// framed tiles around them (`C:\Windows\WinSxS\Manifests`, 2026-10-05: the tiles beside
    /// the corner 52% of their colour); darkened to the same average, they read darker still,
    /// the eye going by a tile's colour and not by its frame. Aligned to the board, not to
    /// each speck, so the lines run on across a corner whatever its specks' sizes.
    pub fn grid(&self) -> impl Iterator<Item = (u16, u16, u16, u16)> + use<> {
        // An iterator, not a list: a painter asks it of every speck on every paint, tens of
        // thousands, and most have no line or one.
        let pitch = MIN_TILE_PIXELS;
        let Dust {
            x,
            y,
            width,
            height,
            framed,
            ..
        } = *self;
        let first = move |start: u16| start.div_ceil(pitch) * pitch;
        let (columns, rows) = if framed {
            (0..0, 0..0)
        } else {
            (
                first(x) / pitch..(x + width).div_ceil(pitch),
                first(y) / pitch..(y + height).div_ceil(pitch),
            )
        };
        columns
            .map(move |column| (column * pitch, y, 1, height))
            .chain(rows.map(move |row| (x, row * pitch, width, 1)))
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
    /// A binary file: what can be said about it (`binary file · 1.7M`, then where its blocks
    /// are) and its first bytes as a hex dump (`libduscape::preview::hex_dump`) — sixteen
    /// bytes a line, the characters beside them.
    Hex {
        info: Vec<String>,
        dump: Vec<String>,
    },
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
    /// Whether the volume's free space is shown as a tile beside its root's entries, where the
    /// root of a volume is what is shown (on by default; `toggle_free_space`).
    pub free_space: bool,
    /// The volume's free and used bytes, where the scan root is a volume's root, as of the
    /// last relayout: `None` elsewhere, and the toggle is not offered.
    free_bytes: Option<(u64, u64)>,
    /// How the free and used bytes are asked for: the OS's, or a test's.
    volume_free: fn(&Path) -> Option<(u64, u64)>,
    /// The scan root is on another machine: its volume is the server's, so the used space the
    /// scan has not found is the server's other shares, and no "not seen by the scan" strip is
    /// shown — on a NAS of several shares it took nearly all the board. Asked once, of the root.
    network_root: bool,
    /// Points at the top left to the viewer for a title bar of its own; see [`Layout::with_top`].
    pub top_inset: f64,
    /// Pixels per point, when the viewer has said: the treemap is then in pixel cells.
    pixel_scale: Option<f64>,
    /// How the folder tiles hold their entries, in the board's cells.
    nesting: Nesting,
    /// The panel the keyboard was last given to; [`Viewer::focus`] is the one it drives.
    focus: Focus,
    /// The entry in hand, by name, so that it stays in hand when the tiles are laid out again —
    /// a resize, a zoom, or new sizes arriving during a scan. Both panels show it.
    pub selected: Option<OsString>,
    /// Whether the entry in hand was picked by the user (an arrow, a jump, a click), rather than
    /// placed by the viewer (the top of a folder just entered, the folder just left, what took a
    /// deleted entry's place). Only a picked one is taken into a selection begun with a modified
    /// click, so nothing unchosen is ever deleted.
    pub chosen: bool,
    /// Marked entries, in the order marked, each as its row's path from the listed folder —
    /// `[name]` for one of the folder's own entries, deeper for a row of a folder opened in
    /// place, so a range or a Ctrl+click among a folder's rows marks those rows and not the
    /// folder. When there are any, they are what a delete, a copy or Show in Finder acts on;
    /// otherwise the entry in hand is.
    pub marked: Marks,
    /// How many outermost marks there are and what they weigh ([`Viewer::marked_count`],
    /// [`Viewer::marked_size`]), for the marks' and the rows' generations: the status bar asks
    /// on every paint, and worked out afresh it was a pass over the rows each time.
    marks_weighed: ::std::cell::Cell<Option<(u64, u64, usize, u128)>>,
    /// Counts the rows rebuilt, so what is kept from them knows when it is stale.
    rows_generation: u64,
    /// Where a ⇧ range starts: a row's path.
    anchor: Option<Vec<OsString>>,
    /// The marks there were when the run of ⇧ moves began: the range is added to them, so
    /// reversing the run shrinks it back towards its start rather than taking earlier marks out.
    mark_run: Option<Vec<Vec<OsString>>>,
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
    /// What each board tile's nested entries cover, by the tile's index; see
    /// [`Viewer::board_inside`].
    board_insides: Vec<Option<Inside>>,
    /// How the nesting's folders were cut, for a relayout with new sizes to keep; emptied by
    /// any other.
    plans: Plans,
    /// The folder the plans were made in: a delete or a rescan can leave the view in another.
    plans_for: Vec<OsString>,
    /// Whether new sizes keep the layout's rows ([`Viewer::set_steady`]), and whether the
    /// relayout under way is one that does: the finished tree's follows the outline's plans
    /// though the scan is over.
    steady: bool,
    following: bool,
    /// Whether tiles slide to a new layout of new sizes ([`Viewer::set_animation`]), and the
    /// slide under way.
    animation: bool,
    tween: Option<tween::Tween>,
    /// The "small files" corner filled in, in pixel cells; see [`Viewer::dust`].
    dust: Vec<Dust>,
    /// Whether what would make a relayout slow may wait for a second pass; see
    /// [`Viewer::defer_to_second_pass`].
    second_pass: bool,
    /// The second pass is owed: the first stopped at its deadline, or left the specks out.
    second_pass_owed: bool,
    /// What the last complete layout with the specks took — from the relayout's start where
    /// there was one — and over how many cells.
    dust_cost: Option<(Duration, u32)>,
    /// Counts the nestings laid out, so a viewer can tell a paint of a new layout from another
    /// of the same one ([`Viewer::layout_generation`]).
    layout_generation: u64,
    /// The nested tile under the pointer.
    pub hover_nested: Option<usize>,
    /// The treemap entry under the pointer, as its path from the listed folder, and since when:
    /// after [`PEEK`] the details panel shows it.
    hover_target: Option<(Vec<OsString>, Instant)>,
    /// The entry the details panel shows in the entry in hand's place while the pointer rests
    /// on its tile, and when the pointer left it — [`PEEK`] later the panel goes back.
    peek: Option<(Vec<OsString>, FileMetadata)>,
    peek_left: Option<Instant>,
    /// The zoom level of each folder above this one, to restore on the way back up.
    zooms: Vec<usize>,
    /// The folder shown, for the scan under way: the walk reads toward it first, and the
    /// outline sends what is under it whole.
    scan_focus: ScanFocus,
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
    /// The delete under way, if one is: while it runs a window takes no input but its box's
    /// Cancel ([`Viewer::deleting`]).
    deleting: Option<Deletion>,
    /// Counts deletes, so the report of one that was replaced (by another scan's viewer) is
    /// known.
    deletions: u64,
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
            free_space: true,
            free_bytes: None,
            volume_free: volume_space,
            network_root: duscape_scan::is_network(root),
            top_inset: 0.0,
            pixel_scale: None,
            nesting: Nesting::default(),
            focus: Focus::List,
            selected: None,
            chosen: false,
            marked: Marks::default(),
            marks_weighed: ::std::cell::Cell::new(None),
            rows_generation: 0,
            anchor: None,
            mark_run: None,
            clipboard: None,
            // Resolved like a scan root is, so that `..` counts real directories on both sides.
            // Not the filesystem's root, which is where a window started from the desktop (the
            // Finder, the Dock) finds itself: paths relative to it are no use to paste.
            working_dir: ::std::env::current_dir()
                .and_then(|dir| dir.canonicalize())
                .ok()
                .filter(|dir| dir.parent().is_some()),
            list_top: 0,
            hover: None,
            tree_view: false,
            expansion: Expansion::default(),
            rows: Vec::new(),
            cursor: None,
            hover_row: None,
            nested: Vec::new(),
            board_insides: Vec::new(),
            plans: Plans::default(),
            plans_for: Vec::new(),
            steady: true,
            following: false,
            animation: false,
            tween: None,
            dust: Vec::new(),
            second_pass: false,
            second_pass_owed: false,
            dust_cost: None,
            layout_generation: 0,
            hover_nested: None,
            hover_target: None,
            peek: None,
            peek_left: None,
            zooms: Vec::new(),
            scan_focus: ScanFocus::new(),
            scanning: true,
            scan_id,
            entries_scanned: 0,
            last_read: None,
            scan_started: Instant::now(),
            scan_took: None,
            message: None,
            rescanner: None,
            rescans: Rescans::default(),
            deleting: None,
            deletions: 0,
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
        self.rebuild_nested(Instant::now());
        self.sync_board();
    }

    /// Allow rescans, which `rescanner` runs.
    pub fn enable_rescans(&mut self, rescanner: Rescanner) {
        self.rescanner = Some(rescanner);
    }

    /// Copy paths through `clipboard` — [`libduscape::clipboard::copy`], or a record in a
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

    /// The folder shown, as the scan follows it: for [`crate::scan::spawn`].
    #[must_use]
    pub fn scan_focus(&self) -> ScanFocus {
        self.scan_focus.clone()
    }

    // ---------------------------------------------------------------- layout

    /// Lay the treemap out in the screen's pixels, `scale` of them to a point: every entry
    /// that would be `MIN_TILE_PIXELS` either way gets a tile, and the nesting goes as deep
    /// as those reach, with the label band and margins kept at their size in points. Taken at
    /// the next [`Viewer::resize`], which a viewer calls with it: the window's size in points
    /// changes with the scale.
    pub fn set_pixel_scale(&mut self, scale: f64) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if self.pixel_scale == Some(scale) {
            return;
        }
        self.pixel_scale = Some(scale);
        // The board is laid out again in the new cells: not with a slide's tiles in it.
        self.end_tween();
        self.plans.clear();
        let grid = Grid::pixels(MIN_TILE_PIXELS);
        self.board.set_grid(grid);
        self.nesting = Nesting {
            label_rows: (TILE_LABEL * scale).ceil() as u16,
            margin: ((TILE_MARGIN * scale).round() as u16).max(1),
            grid,
            dust: true,
            ..Nesting::default()
        };
    }

    /// Whether a tile may be labelled: a file always, where its label fits; a folder when it
    /// has its label band above its entries — a shorter one's entries are nested right under
    /// its margin, over where the label would be.
    #[must_use]
    pub fn labelled(&self, tile: &Tile) -> bool {
        !self.tree_view || tile.file_type != FileType::Folder || self.nesting.labelled(tile)
    }

    pub fn resize(&mut self, width: f64, height: f64) {
        self.end_tween();
        let started = Instant::now();
        self.layout = Layout::with_cells(
            width,
            height,
            self.sidebar,
            self.top_inset,
            self.pixel_scale,
        );
        self.board.change_area(&Area {
            x: 0,
            y: 0,
            width: self.layout.cols,
            height: self.layout.rows,
        });
        self.plans.clear();
        self.rebuild_nested(started);
        self.sync_board();
        self.scroll_to_selected();
    }

    /// Show or hide the volume's free space beside its root's entries (on by default), where
    /// the root of a volume is shown: the entries' tiles shrink to leave it its share, so the
    /// treemap shows the whole volume in proportion.
    pub fn toggle_free_space(&mut self) {
        self.free_space = !self.free_space;
        self.relayout(false);
    }

    /// The free-space toggle to draw at [`Layout::free_toggle`] — its words and whether it is
    /// on — while the root of a volume is what is shown; `None` elsewhere, where there is no
    /// free space to show and the path bar runs to the edge.
    #[must_use]
    pub fn free_toggle(&self) -> Option<(&'static str, bool)> {
        (self.tree.current_folder_names.is_empty() && self.free_bytes.is_some())
            .then_some(("Free space", self.free_space))
    }

    /// The free space's tile for the board, if one is to be shown now: the root of a volume,
    /// the toggle on, and some space free. Its share is free over the volume's size, which the
    /// volume knows before the scan starts, so the tile has its place and its area from the
    /// first frame on; the used space the scan has not found yet is the unscanned share beside
    /// it, which the entries grow into as the scan goes. Where the scan finds more than the
    /// volume says is used (shared or compressed blocks counted in full), the entries have the
    /// used share and there is none unscanned.
    fn free_space_tile(&self) -> Option<FreeSpace> {
        if !self.free_space || !self.tree.current_folder_names.is_empty() {
            return None;
        }
        let (bytes, used) = self.free_bytes.filter(|&(bytes, _)| bytes > 0)?;
        let total = bytes as f64 + used as f64;
        // On disk whatever is shown: the volume counts blocks, and lengths set against them
        // would call a folder of sparse files mostly unscanned, or a compressed one overfull.
        let found = self.tree.get_current_folder().sizes.get(SizeKind::Disk);
        let unscanned = if self.network_root {
            0
        } else {
            used.saturating_sub(u64::try_from(found).unwrap_or(u64::MAX))
        };
        Some(FreeSpace {
            bytes,
            share: bytes as f64 / total,
            unscanned,
            unscanned_share: unscanned as f64 / total,
            scanned: !self.scanning,
            purgeable: self.purgeable_unseen().min(unscanned),
        })
    }

    /// What may be the volume's snapshots and purgeable space among the unseen, once the scan
    /// is over and while the snapshots are unread: once read they are in the tree, and the
    /// system's figure would count them twice.
    fn purgeable_unseen(&self) -> u64 {
        let unread = self.tree.snapshots.is_none_or(|noted| !noted.read);
        self.tree
            .purgeable
            .filter(|_| !self.scanning && unread)
            .unwrap_or(0)
    }

    /// Where the volume's free and used bytes come from, in place of the OS's answer: for
    /// tests, whose roots are no volume's.
    pub fn set_volume_free_source(&mut self, source: fn(&Path) -> Option<(u64, u64)>) {
        self.volume_free = source;
    }

    /// Whether the scan root is to be taken for another machine's, in place of asking: for
    /// tests, whose roots are local.
    pub fn set_network_root(&mut self, network: bool) {
        self.network_root = network;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar = !self.sidebar;
        let bounds = self.layout.bounds;
        self.resize(bounds.w, bounds.h);
    }

    /// Lay the folder's entries out again, after the tree or the size shown changed.
    fn refresh(&mut self) {
        self.plans.clear();
        self.relayout(false);
    }

    /// [`Viewer::refresh`] for new sizes in the same view — the outline, the finished tree, a
    /// delete, a rescan: the tiles keep their rows where they still fit, and grow and shrink in
    /// place rather than being cut again.
    fn refresh_steady(&mut self) {
        if self.steady {
            self.relayout(true);
        } else {
            self.refresh();
        }
    }

    /// Whether new sizes in the same view keep the layout's rows (on by default): off, every
    /// outline batch squarifies afresh, as before — for comparing the two.
    pub fn set_steady(&mut self, on: bool) {
        self.steady = on;
    }

    fn relayout(&mut self, steady: bool) {
        // Wherever the user has gone, the walk goes there next.
        self.scan_focus.set(Some(self.tree.get_current_path()));
        // The volume's free space beside the root's entries, where the root of a volume is
        // what is shown and the toggle is on: asked for afresh, since a delete frees some.
        self.free_bytes = (self.volume_free)(self.root());
        self.board.set_free_space(self.free_space_tile());
        let from = if steady { self.tween_from() } else { None };
        self.end_tween();
        self.following = steady;
        let started = Instant::now();
        // What the specks cost is not known for what is shown now: tiles first, specks after.
        self.dust_cost = None;
        if steady {
            self.board
                .change_files_steady(self.tree.get_current_folder());
        } else {
            self.board.change_files(self.tree.get_current_folder());
        }
        self.rebuild_rows();
        self.rebuild_nested(started);
        self.sync_board();
        self.clamp_list_top();
        self.following = false;
        self.tween_to(from);
        self.want_visible_archives();
    }

    /// Tell the archive pass which unread archives are on screen, the biggest first: a tile big
    /// enough to show what is in one (three of the least tiles either way), on the board or
    /// nested, then a row of the list on screen. Only those are read; an archive whose contents
    /// would change nothing visible waits until a zoom, a folder or a scroll shows it. Cheap
    /// when nothing is unread, and a tile's path is made only for an archive's name.
    fn want_visible_archives(&self) {
        if !self.rescans.has_unread_archives() {
            return;
        }
        let least = 3 * MIN_TILE_PIXELS;
        let here: PathBuf = self.tree.current_folder_names.iter().collect();
        let archive = |tile: &Tile| {
            tile.file_type == FileType::File
                && tile.width >= least
                && tile.height >= least
                && libduscape::archive::is_archive_name(tile.name.as_encoded_bytes())
        };
        let mut wanted: Vec<(u32, PathBuf)> = Vec::new();
        let mut want = |area: u32, path: PathBuf| {
            if self.rescans.is_unread_archive(&path) {
                wanted.push((area, path));
            }
        };
        for tile in self.board.tiles.iter().filter(|tile| archive(tile)) {
            want(
                u32::from(tile.width) * u32::from(tile.height),
                here.join(&tile.name),
            );
        }
        for (index, nested) in self.nested.iter().enumerate() {
            if archive(&nested.tile) {
                let path = self.nested_path(index);
                want(
                    u32::from(nested.tile.width) * u32::from(nested.tile.height),
                    path.iter().fold(here.clone(), |path, name| path.join(name)),
                );
            }
        }
        let shown = self.layout.list_rows();
        for row in self.rows.iter().skip(self.list_top).take(shown) {
            if row.entry.file_type == FileType::File
                && libduscape::archive::is_archive_name(row.entry.name.as_encoded_bytes())
            {
                want(
                    0,
                    row.path
                        .iter()
                        .fold(here.clone(), |path, name| path.join(name)),
                );
            }
        }
        wanted.sort_by_key(|(area, _)| ::std::cmp::Reverse(*area));
        let mut seen = ::std::collections::HashSet::new();
        self.rescans.want_archives(
            wanted
                .into_iter()
                .map(|(_, path)| path)
                .filter(|path| seen.insert(path.clone())),
        );
    }

    /// The tiles inside the folder tiles, when the tree view is on; none otherwise. `started`
    /// is when the relayout began: the first pass's deadline counts from it.
    fn rebuild_nested(&mut self, started: Instant) {
        let pixels = self.pixel_scale.is_some();
        if self.second_pass && pixels {
            // The first pass: the nesting until the relayout's deadline, level by level, and
            // the specks only if the last complete layout with them, scaled to this one's
            // cells, would keep inside the budget. What it leaves out is the second pass's,
            // once input has stopped.
            let cells = self.cells();
            let dust = self.dust_cost.is_some_and(|(cost, then)| {
                cost.mul_f64(f64::from(cells) / f64::from(then.max(1))) <= LAYOUT_BUDGET
            });
            let complete = self.lay_nesting(dust, Some(started + LAYOUT_BUDGET), started);
            self.second_pass_owed = !dust || !complete;
        } else {
            self.lay_nesting(pixels, None, started);
            self.second_pass_owed = false;
        }
        // The tiles moved: what was under the pointer is not known until it moves again.
        self.hover_nested = None;
    }

    /// Let what would make a relayout overrun [`LAYOUT_BUDGET`] wait for a second pass: the
    /// nesting stops at the deadline, a level at a time so it is the deepest levels that
    /// wait, and the specks of the "small files" corners wait unless the last layout with them
    /// was quick (or none has been timed, for what is shown now). The viewer then calls
    /// [`Viewer::finish_second_pass`] once input has stopped for [`IDLE`], while
    /// [`Viewer::second_pass_owed`] says so: first paint and a drag stay fast, and the rest
    /// follows. Off, the nesting is laid out whole at once, the specks with it.
    pub fn defer_to_second_pass(&mut self, on: bool) {
        self.second_pass = on;
    }

    /// Whether the second pass is owed: the first stopped at its deadline, or left the specks.
    #[must_use]
    pub fn second_pass_owed(&self) -> bool {
        self.second_pass_owed
    }

    /// The second pass: the nesting again, whole, with the specks, timed for the next
    /// relayout's choice. The tiles a first pass laid out come out as they were.
    pub fn finish_second_pass(&mut self) {
        if std::mem::take(&mut self.second_pass_owed) {
            self.end_tween();
            self.lay_nesting(true, None, Instant::now());
            // Deeper tiles, maybe archives big enough to show what is in them.
            self.want_visible_archives();
        }
    }

    /// Which nesting is on show: it changes with every relayout and second pass, so a viewer
    /// that paints a first time in a hurry knows when it has painted this one in full.
    #[must_use]
    pub fn layout_generation(&self) -> u64 {
        self.layout_generation
    }

    /// The treemap's cells: what a layout's time is in proportion to.
    fn cells(&self) -> u32 {
        u32::from(self.layout.cols) * u32::from(self.layout.rows)
    }

    /// The nesting, and with `dust` the corners' specks, the time since `started` noted.
    /// Returns whether it is complete: not cut at `deadline`.
    fn lay_nesting(&mut self, dust: bool, deadline: Option<Instant>, started: Instant) -> bool {
        self.layout_generation += 1;
        // The specks have half the relayout's time: the top-level folders' entries come
        // whatever the time, so the tiles still to lay out when the specks stop need the rest.
        let dust_deadline = deadline.map(|deadline| deadline - LAYOUT_BUDGET / 2);
        let nesting = Nesting {
            dust,
            deadline,
            dust_deadline,
            // A first pass lays out no more of a folder than a frame has time for.
            room_cap: if deadline.is_some() {
                FIRST_PASS_ROOM
            } else {
                usize::MAX
            },
            ..self.nesting
        };
        let mut colors = SpeckColors::default();
        // The last layout's, for its room: a relayout makes about as many.
        let mut specks = ::std::mem::take(&mut self.dust);
        specks.clear();
        let mut complete = true;
        self.nested = if self.tree_view {
            // Plans are kept only where the next layout may be a steady one: a relayout keying
            // every tile costs a quarter again.
            let folder = self.tree.get_current_folder();
            let board = &self.board.tiles;
            let mut speck = |speck: Speck| specks.push(colors.dust(&speck));
            if self.plans_for != self.tree.current_folder_names {
                self.plans.clear();
                self.plans_for.clone_from(&self.tree.current_folder_names);
            }
            let keep = self.steady && (self.scanning || self.animation || self.following);
            let nested = if keep {
                let shown = self.tree.shown;
                nest_steady(
                    folder,
                    board,
                    shown,
                    &nesting,
                    &mut speck,
                    Some(&self.plans),
                )
            } else {
                nest_with(folder, board, self.tree.shown, &nesting, &mut speck)
            };
            complete = nested.complete;
            self.plans = nested.plans;
            self.board_insides = nested.tops;
            nested.tiles
        } else {
            self.board_insides.clear();
            Vec::new()
        };
        self.dust = specks;
        if dust && dust_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Past the deadline the board's corner waits for the second pass, as the folders'.
            complete = false;
        } else if dust {
            // The board's own corner, in pixel cells only, where a speck is a pixel or more.
            self.board
                .scatter_corner(&mut |speck| self.dust.push(colors.dust(&speck)));
            // A cut layout's time is not the whole one's.
            if complete {
                self.dust_cost = Some((started.elapsed(), self.cells()));
            }
        }
        complete
    }

    /// The "small files" corners filled in — the board's, and each nested folder's — every
    /// entry too small for a tile laid out again inside its corner down to a pixel each and
    /// coloured as its tile would be. A picture only — they have no names and are no targets:
    /// a click on the board's corner is [`Hit::SmallFiles`], on a folder's, the folder's.
    /// Empty unless the viewer lays out in pixels.
    #[must_use]
    pub fn dust(&self) -> &[Dust] {
        &self.dust
    }

    /// The colour of the board's tile at `index`, for every painter alike: by its name, as its
    /// swatch in the list is, so the two agree and neither changes as the listing re-sorts.
    #[must_use]
    pub fn board_color(&self, index: usize) -> (f64, f64, f64) {
        // The free space is no entry: a dark, neutral tile, so the used space stands out; the
        // unscanned space likewise, a shade apart.
        if self.board.free_tile() == Some(index) {
            return FREE_SPACE_COLOR;
        }
        if self.board.unscanned_tile() == Some(index) {
            return UNSCANNED_COLOR;
        }
        if self.board.purgeable_tile() == Some(index) {
            return PURGEABLE_COLOR;
        }
        let tile = &self.board.tiles[index];
        entry_color(&tile.name, tile.file_type, 0)
    }

    /// The colour of the nested tile at `index`: a step darker a level in ([`depth_shade`]).
    #[must_use]
    pub fn nested_color(&self, index: usize) -> (f64, f64, f64) {
        let nested = &self.nested[index];
        entry_color(&nested.tile.name, nested.tile.file_type, nested.depth)
    }

    /// What the nested entries of the board's tile at `index` cover, if any were laid out in it:
    /// a painter fills the tile around it ([`Inside::around`]), the entries being drawn over
    /// the rest.
    #[must_use]
    pub fn board_inside(&self, index: usize) -> Option<&Inside> {
        self.board_insides.get(index)?.as_ref()
    }

    /// The tree view's treemap: the tiles inside the board's folder tiles, parents first.
    #[must_use]
    pub fn nested(&self) -> &[NestedTile] {
        &self.nested
    }

    /// The names from the folder shown down to the nested tile at `index`.
    #[must_use]
    pub fn nested_path(&self, index: usize) -> Vec<OsString> {
        libduscape::tiles::nested_path(&self.nested, &self.board.tiles, index)
    }

    /// The nested tile of the row in hand, if it has one.
    #[must_use]
    pub fn cursor_nested(&self) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        let tiles = &self.board.tiles;
        (0..self.nested.len())
            .find(|&index| libduscape::tiles::nested_path_is(&self.nested, tiles, index, cursor))
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
        self.open_above(&path);
        self.select_row(path, chosen);
    }

    /// Open the folders above `path` in the tree; a top-level path, or one whose folders are
    /// open already, changes nothing, so the rows are rebuilt only when something opened.
    fn open_above(&mut self, path: &[OsString]) {
        let mut opened = false;
        for depth in 1..path.len() {
            opened |= self.expansion.open(&path[..depth]);
        }
        if opened {
            self.rebuild_rows();
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows = self
            .expansion
            .rows(self.tree.get_current_folder(), self.tree.shown);
        self.rows_generation += 1;
        self.retain_shown_marks();
    }

    /// Keep the marks to rows on show: one under a folder just closed goes with its row, as a
    /// tree view deselects what it hides — a delete never takes what the user cannot see is
    /// marked. A top-level mark stays while its entry is listed (the treemap shows it).
    fn retain_shown_marks(&mut self) {
        if !self.marked.has_deep() {
            return;
        }
        let shown: ::std::collections::HashSet<&[OsString]> =
            self.rows.iter().map(|row| row.path.as_slice()).collect();
        self.marked
            .retain(|path| path.len() == 1 || shown.contains(path));
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
        if !self.marked.is_empty() {
            // A set of the names, not `listed` a mark: after Ctrl+A that was marks × entries.
            let names: ::std::collections::HashSet<&OsString> =
                listing.iter().map(|entry| &entry.name).collect();
            self.marked
                .retain(|path| path.first().is_some_and(|name| names.contains(name)));
        }
        if self.hover.as_ref().is_some_and(|name| !listed(name)) {
            self.hover = None;
        }
        self.retain_shown_marks();
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
        // Rows scrolled into view may be archives to read.
        self.want_visible_archives();
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
        self.refresh_steady();
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
        self.refresh_steady();
        if self.selected.is_none() {
            self.select_first();
        }
        // A tree read from the saved scan as it was is brought up to date behind itself.
        if self.tree.from_saved_scan
            && let Some(rescanner) = &self.rescanner
        {
            self.rescans.start_catch_up(rescanner, &self.tree);
        }
        self.start_idle();
    }

    /// What is owed once nothing else reads the disk (`Rescans::start_idle`): the fill of what
    /// a saved scan trimmed, the folder in view first, or the volume's local snapshots — whose
    /// folder, when it is added, is laid out at once.
    fn start_idle(&mut self) {
        let Some(rescanner) = &self.rescanner else {
            return;
        };
        if self
            .rescans
            .start_idle(rescanner, &mut self.tree, self.scan_focus.clone())
        {
            self.refresh_steady();
        }
        // The archive pass may have just started: what is on screen is what it reads first.
        self.want_visible_archives();
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
        self.cursor = self.selected.clone().map(|name| vec![name]);
        self.anchor = self.cursor.clone();
        self.mark_run = None;
        self.sync_board();
        self.scroll_to_selected();
    }

    /// Put a row in hand by its path; its top-level entry is what the treemap selects.
    fn select_row(&mut self, path: Vec<OsString>, chosen: bool) {
        self.drop_peek();
        self.selected = path.first().cloned();
        self.chosen = chosen && self.selected.is_some();
        self.anchor = Some(path.clone());
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

    /// Whether the folder's own entry `name` is marked (a board tile, a top-level row).
    pub fn is_marked(&self, name: &OsStr) -> bool {
        self.marked.contains_name(name)
    }

    /// Whether the row at `path` (from the listed folder) is marked.
    #[must_use]
    pub fn is_marked_row(&self, path: &[OsString]) -> bool {
        self.marked.contains(path)
    }

    /// Which nested tiles are marked rows', by index: for a painter, once a frame. Empty when
    /// no mark is below the folder's own entries (`get` gives `None`: none marked). One pass
    /// in the nesting's order, parents before children, a tile's path made only where it is on
    /// the way to a mark — a path a tile was half the nesting's time, and asking each tile of
    /// each mark (`nested_path_is`) would be tiles × marks after a Ctrl+A.
    #[must_use]
    pub fn marked_nested(&self) -> Vec<bool> {
        use ::std::collections::HashSet;
        // Asked every frame: no pass over the marks when none is nested (after Ctrl+A, 87k).
        if !self.marked.has_deep() {
            return Vec::new();
        }
        let deep: Vec<&[OsString]> = self
            .marked
            .iter()
            .filter(|path| path.len() > 1)
            .map(Vec::as_slice)
            .collect();
        if deep.is_empty() {
            return Vec::new();
        }
        let ways: HashSet<&[OsString]> = deep
            .iter()
            .flat_map(|path| (1..=path.len()).map(move |len| &path[..len]))
            .collect();
        let marks: HashSet<&[OsString]> = deep.iter().copied().collect();
        let mut paths: Vec<Option<Vec<OsString>>> = vec![None; self.nested.len()];
        let mut marked = vec![false; self.nested.len()];
        for (index, nested) in self.nested.iter().enumerate() {
            let mut path = match nested.parent {
                Some(parent) => match &paths[parent] {
                    Some(path) => path.clone(),
                    None => continue,
                },
                None => {
                    let top = &self.board.tiles[nested.top].name;
                    if !ways.contains(::std::slice::from_ref(top)) {
                        continue;
                    }
                    vec![top.clone()]
                }
            };
            path.push(nested.tile.name.clone());
            if !ways.contains(path.as_slice()) {
                continue;
            }
            marked[index] = marks.contains(path.as_slice());
            paths[index] = Some(path);
        }
        marked
    }

    /// How many entries the marks are — a folder marked with rows inside it once — and what
    /// they weigh, as shown: kept until the marks or the rows change. Every mark is a row on
    /// show (`retain_shown_marks`), so one pass over the rows finds them all.
    fn marks_weighed(&self) -> (usize, u128) {
        let key = (self.marked.generation(), self.rows_generation);
        if let Some((marks, rows, count, size)) = self.marks_weighed.get()
            && (marks, rows) == key
        {
            return (count, size);
        }
        let (mut count, mut size) = (0, 0u128);
        if !self.marked.is_empty() {
            for row in &self.rows {
                if self.marked.is_outermost(&row.path) {
                    count += 1;
                    size += row.entry.size;
                }
            }
        }
        self.marks_weighed.set(Some((key.0, key.1, count, size)));
        (count, size)
    }

    /// How many entries a delete or a copy of the marks acts on: a folder marked with rows
    /// inside it counts once, as it goes once.
    #[must_use]
    pub fn marked_count(&self) -> usize {
        self.marks_weighed().0
    }

    /// What the marks weigh, as shown: a mark inside another marked folder counted once, in it.
    #[must_use]
    pub fn marked_size(&self) -> u128 {
        self.marks_weighed().1
    }

    /// Move the entry in hand. In the list, ↑ and ↓ go by row (with `extend`, marking the range
    /// from the anchor) and → crosses to the treemap. In the treemap the tile beside it in that
    /// direction is taken; ← off its left edge crosses to the list.
    pub fn arrow(&mut self, direction: Direction, extend: bool) {
        match (self.focus(), direction) {
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
            // A ⇧ range runs over the rows as they are on show, a folder's opened in place
            // with the rest: the marks are the rows'.
            let anchor = self.anchor.clone().or_else(|| self.cursor.clone());
            self.selected = path.first().cloned();
            self.chosen = true;
            self.anchor = anchor.clone();
            self.mark_range(anchor.as_deref(), index);
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

    /// Mark every row from `anchor` (a row's path) to the row `to`, swept in that order, on
    /// top of the marks there were when the ⇧ run began — so reversing the run shrinks the
    /// range back towards the anchor and no earlier mark is lost. Then copy them. An anchor no
    /// longer on show (its folder closed) starts the range at `to`.
    fn mark_range(&mut self, anchor: Option<&[OsString]>, to: usize) {
        let base: Vec<Vec<OsString>> = self
            .mark_run
            .get_or_insert_with(|| self.marked.as_slice().to_vec())
            .clone();
        let rows = &self.rows;
        let from = anchor
            .and_then(|anchor| rows.iter().position(|row| row.path.as_slice() == anchor))
            .unwrap_or(to);
        let swept: Vec<Vec<OsString>> = if from <= to {
            (from..=to).map(|row| rows[row].path.clone()).collect()
        } else {
            (to..=from)
                .rev()
                .map(|row| rows[row].path.clone())
                .collect()
        };
        // Each once, by the set: `contains` on the list was quadratic in a range to the end.
        self.marked.replace(base.into_iter().chain(swept));
        // Marks from the run's start that have since gone (a delete, a rescan, a folder
        // closed) stay gone.
        self.sync_board();
        self.copy_marked();
    }

    /// The panel the keyboard drives: the one it was last given to, but always the treemap
    /// while the layout has no room for the list — and the list again when it has.
    #[must_use]
    pub fn focus(&self) -> Focus {
        if self.layout.list.is_some() {
            self.focus
        } else {
            Focus::Treemap
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus() {
            Focus::List => Focus::Treemap,
            Focus::Treemap => Focus::List,
        };
    }

    /// Mark every entry in the folder.
    pub fn mark_all(&mut self) {
        let all: Vec<Vec<OsString>> = self
            .board
            .listing()
            .iter()
            .map(|entry| vec![entry.name.clone()])
            .collect();
        self.marked.replace(all);
        self.mark_run = None;
        self.copy_marked();
    }

    // ---------------------------------------------------------------- copying paths

    /// Copy the path of every marked entry, or else of the one in hand: relative to the working
    /// directory unless `absolute` (or there is no relative path), quoted for the shell and
    /// separated by spaces, ready to paste after a command. Says what was copied. `false` if
    /// there is no clipboard, or nothing to copy.
    pub fn copy_paths(&mut self, absolute: bool) -> bool {
        let Some((text, label)) = self.copied_paths(absolute) else {
            return false;
        };
        self.copy_text(&text, &label)
    }

    /// What [`Viewer::copy_paths`] would copy, and the words to say it with, for a viewer that
    /// puts it on the clipboard itself: the marked entries' paths, or the row in hand's —
    /// nested in the tree or not — quoted for the shell, relative to the working directory or
    /// `absolute`. `None` with nothing to copy.
    #[must_use]
    pub fn copied_paths(&self, absolute: bool) -> Option<(String, String)> {
        let full = self.target_paths();
        let paths: Vec<(_, String)> = full
            .iter()
            .map(|path| copied_path(path, self.working_dir.as_deref(), absolute))
            .collect();
        let label = match paths.as_slice() {
            [] => return None,
            [(kind, _)] if self.marked.is_empty() => format!("Copied {} path:", kind.name()),
            [_] => "Copied 1 path:".to_string(),
            many => format!("Copied {} paths:", DisplayCount(many.len() as u64)),
        };
        let text = paths
            .iter()
            .map(|(_, path)| path.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Some((text, label))
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
            // The free and unscanned space's tiles are no entries: nothing to take in hand or
            // preview.
            if !self.board.selectable(index) {
                return Hit::Nothing;
            }
            return Hit::Tile(self.board.tiles[index].name.clone());
        }
        match self.board.corner() {
            Some(corner)
                if (corner.x..corner.x + corner.width).contains(&col)
                    && (corner.y..corner.y + corner.height).contains(&row) =>
            {
                Hit::SmallFiles
            }
            _ => Hit::Nothing,
        }
    }

    /// A press of the main button: take the entry under the pointer in hand, with ⌘ (Ctrl)
    /// toggling its mark and ⇧ marking the range to it from the anchor. Returns the entry's name.
    pub fn click(&mut self, x: f64, y: f64, mods: Mods) -> Option<OsString> {
        let (focus, path) = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => (Focus::List, self.rows[index].path.clone()),
            Hit::Tile(name) => (Focus::Treemap, vec![name]),
            // A tile inside a folder's: a target like the others — the marks are its top-level
            // folder's — and the folders above it open in the tree so its row is in hand.
            Hit::Nested(index) => (Focus::Treemap, self.nested_path(index)),
            Hit::SmallFiles | Hit::Nothing => return None,
        };
        let name = path[0].clone();
        self.focus = focus;
        if mods.range {
            // Over the rows on show: a nested tile's folders open first, so its row is one.
            let anchor = self.anchor.clone().or_else(|| self.cursor.clone());
            self.open_above(&path);
            let to = self.rows.iter().position(|row| row.path == path)?;
            self.mark_range(anchor.as_deref(), to);
            self.selected = Some(name.clone());
            self.chosen = true;
            self.anchor = anchor;
            self.cursor = Some(path);
            self.sync_board();
            self.scroll_to_selected();
        } else if mods.toggle {
            // Starting a selection takes in the row already in hand, as a file manager does —
            // but only one the user picked, never one the viewer placed there.
            if self.marked.is_empty()
                && self.chosen
                && let Some(cursor) = self.cursor.clone()
                && cursor != path
            {
                self.marked.push(cursor);
            }
            if !self.marked.remove(&path) {
                self.marked.push(path.clone());
            }
            // Placed, not picked: a click that marks does not choose what a later one adds.
            self.reveal(path, false);
            self.copy_marked();
        } else {
            self.clear_marks();
            self.reveal(path, true);
        }
        Some(name)
    }

    /// Take the entry under the pointer in hand for a context menu, keeping the marks if it is
    /// one of them. Returns whether there is an entry there.
    pub fn context_click(&mut self, x: f64, y: f64) -> bool {
        let path = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => self.rows[index].path.clone(),
            Hit::Tile(name) => vec![name],
            Hit::Nested(index) => self.nested_path(index),
            Hit::SmallFiles | Hit::Nothing => return false,
        };
        if !self.is_marked_row(&path) {
            self.clear_marks();
        }
        self.reveal(path, true);
        true
    }

    /// The pointer moved. Returns whether what it is over changed. A treemap tile it comes to
    /// rest on is what the details panel shows after [`PEEK`] (a viewer wakes for
    /// [`Viewer::peek_tick`] when [`Viewer::peek_due`] says).
    pub fn hover_at(&mut self, x: f64, y: f64) -> bool {
        let (hover, hover_row, hover_nested, target) = match self.hit(x, y) {
            Hit::Row(index) | Hit::Expander(index) => (
                Some(self.rows[index].path[0].clone()),
                Some(index),
                None,
                None,
            ),
            Hit::Tile(name) => (Some(name.clone()), None, None, Some(vec![name])),
            Hit::Nested(index) => (None, None, Some(index), Some(self.nested_path(index))),
            Hit::SmallFiles | Hit::Nothing => (None, None, None, None),
        };
        let changed =
            hover != self.hover || hover_row != self.hover_row || hover_nested != self.hover_nested;
        self.hover = hover;
        self.hover_row = hover_row;
        self.hover_nested = hover_nested;
        match target {
            Some(path) => {
                if self
                    .hover_target
                    .as_ref()
                    .is_none_or(|(there, _)| *there != path)
                {
                    self.hover_target = Some((path, Instant::now()));
                }
                self.peek_left = None;
            }
            None => {
                self.hover_target = None;
                if self.peek.is_some() && self.peek_left.is_none() {
                    self.peek_left = Some(Instant::now());
                }
            }
        }
        changed
    }

    /// How long until [`Viewer::peek_tick`] has something to do: the pointer's rest on a tile
    /// reaching [`PEEK`], or its absence from one; none when neither is pending.
    #[must_use]
    pub fn peek_due(&self) -> Option<Duration> {
        let since = match (&self.hover_target, &self.peek_left) {
            (Some((path, since)), _) => {
                if self.peek.as_ref().is_some_and(|(shown, _)| shown == path) {
                    return None;
                }
                *since
            }
            (None, Some(left)) if self.peek.is_some() => *left,
            _ => return None,
        };
        Some(PEEK.saturating_sub(since.elapsed()))
    }

    /// The time [`Viewer::peek_due`] named has come: the details panel takes the tile the
    /// pointer rests on, or goes back to the entry in hand. Returns whether what it shows
    /// changed (then the viewer asks for the preview again, as after any change).
    pub fn peek_tick(&mut self) -> bool {
        if let Some((path, since)) = &self.hover_target {
            if since.elapsed() < PEEK || self.peek.as_ref().is_some_and(|(shown, _)| shown == path)
            {
                return false;
            }
            let path = path.clone();
            match self.entry_at(&path) {
                Some(entry) => {
                    self.peek = Some((path, entry));
                    self.peek_left = None;
                    return true;
                }
                // Gone (a delete, a rescan): nothing to show, and nothing to wait for.
                None => self.hover_target = None,
            }
        }
        if self.peek.is_some() && self.peek_left.is_some_and(|left| left.elapsed() >= PEEK) {
            self.drop_peek();
            return true;
        }
        false
    }

    /// The details panel back on the entry in hand at once.
    fn drop_peek(&mut self) {
        self.peek = None;
        self.peek_left = None;
        self.hover_target = None;
    }

    /// The entry the details panel is about: the one the pointer rests on, else the row in
    /// hand, with its path from the listed folder.
    #[must_use]
    pub fn shown(&self) -> Option<(&[OsString], &FileMetadata)> {
        self.peek
            .as_ref()
            .map(|(path, entry)| (path.as_slice(), entry))
            .or_else(|| {
                self.cursor_entry()
                    .map(|row| (row.path.as_slice(), &row.entry))
            })
    }

    /// [`Viewer::shown`]'s entry.
    #[must_use]
    pub fn shown_entry(&self) -> Option<&FileMetadata> {
        self.shown().map(|(_, entry)| entry)
    }

    /// The entry at `path` from the listed folder, as the list would give it.
    fn entry_at(&self, path: &[OsString]) -> Option<FileMetadata> {
        let (last, parents) = path.split_last()?;
        let mut folder = self.tree.get_current_folder();
        for name in parents {
            match folder.contents.get(name)? {
                FileOrFolder::Folder(inside) => folder = inside,
                FileOrFolder::File(_) => return None,
            }
        }
        libduscape::tiles::files_in_folder(folder, 0, self.tree.shown)
            .into_iter()
            .find(|entry| &entry.name == last)
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

    /// Before the board is laid out again for a zoom: a slide under way is ended, or it would
    /// put the new tiles where the old ones were going, and the nesting's rows forgotten, since
    /// every folder's tile changes shape.
    fn before_zoom(&mut self) {
        self.end_tween();
        self.plans.clear();
    }

    pub fn zoom_in(&mut self) {
        self.before_zoom();
        let started = Instant::now();
        self.board.zoom_in(self.tree.get_current_folder());
        self.rebuild_nested(started);
        self.sync_board();
    }

    pub fn zoom_out(&mut self) {
        self.before_zoom();
        let started = Instant::now();
        self.board.zoom_out(self.tree.get_current_folder());
        self.rebuild_nested(started);
        self.sync_board();
    }

    pub fn reset_zoom(&mut self) {
        self.before_zoom();
        let started = Instant::now();
        self.board.reset_zoom(self.tree.get_current_folder());
        self.rebuild_nested(started);
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

    /// The rows acted on, each as its path from the listed folder: every mark — less those
    /// inside another marked folder, which goes with it — or else the row in hand.
    pub fn target_rows(&self) -> Vec<Vec<OsString>> {
        if self.marked.is_empty() {
            return match (&self.cursor, &self.selected) {
                (Some(cursor), _) => vec![cursor.clone()],
                (None, Some(name)) => vec![vec![name.clone()]],
                (None, None) => Vec::new(),
            };
        }
        self.marked.outermost().cloned().collect()
    }

    pub fn target_paths(&self) -> Vec<PathBuf> {
        self.target_rows()
            .iter()
            .map(|path| self.row_path(path))
            .collect()
    }

    /// What a delete would act on. Nothing while the first scan runs: the tree on screen is an
    /// outline, and the finished tree that replaces it would still hold what was deleted.
    pub fn targets(&self) -> Vec<FileToDelete> {
        if self.scanning {
            return Vec::new();
        }
        let rows = self.target_rows();
        // Each target's entry from its row, by a map of the rows: a search of the rows a
        // target was rows × marks after Ctrl+A.
        let entries: HashMap<&[OsString], &FileMetadata> = if rows.len() > 1 {
            self.rows
                .iter()
                .map(|row| (row.path.as_slice(), &row.entry))
                .collect()
        } else {
            HashMap::new()
        };
        rows.iter()
            .filter_map(|path| {
                let entry = match rows.len() {
                    1 => self
                        .rows
                        .iter()
                        .find(|row| &row.path == path)
                        .map(|row| &row.entry)
                        .or_else(|| self.entry_named(&path[0]).filter(|_| path.len() == 1))?,
                    _ => *entries.get(path.as_slice())?,
                };
                Some(FileToDelete::in_current_tree(&self.tree, path, entry))
            })
            .collect()
    }

    /// The question to ask before deleting `files` for good.
    #[must_use]
    pub fn delete_prompt(files: &[FileToDelete]) -> String {
        let size: u128 = files.iter().map(|file| file.size).sum();
        match files {
            [one] => format!(
                "Delete {}?\n\n{} will be permanently removed from disk.",
                libduscape::format::shown_path(&one.full_path()),
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

    /// Why `files` may not be deleted, if one of them is NTFS's own metadata or in a local
    /// snapshot — to say before asking, as well as before touching anything.
    #[must_use]
    pub fn refusal(files: &[FileToDelete]) -> Option<String> {
        libduscape::delete::refused(files)
    }

    /// Start removing `files` — for good when `freed`, else by `remove` to a Trash — on a thread
    /// of its own, going on past any that fail; `done` is called there with the delete's id and
    /// how each went, for the viewer to post back to its own thread and hand to
    /// [`Viewer::delete_done`]. Refused (NTFS's own metadata, a snapshot) before anything is
    /// touched, as when a delete is already under way or no thread can be started.
    pub fn start_delete(
        &mut self,
        files: Vec<FileToDelete>,
        freed: bool,
        remove: impl Fn(&FileToDelete, &Tally) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(u64, Vec<Ended>) + Send + 'static,
    ) -> Result<(), String> {
        if let Some(refusal) = Self::refusal(&files) {
            return Err(refusal);
        }
        if self.deleting.is_some() {
            return Err("A delete is under way already".to_string());
        }
        self.deletions += 1;
        let deletion = Deletion::spawn(self.deletions, files, freed, remove, done)
            .map_err(|error| format!("Could not start deleting: {error}"))?;
        self.deleting = Some(deletion);
        Ok(())
    }

    /// The delete under way, for a window to draw once it is [`Deletion::shown`] and to cancel.
    /// While there is one the window takes no other input: the tree still holds what is going.
    #[must_use]
    pub fn deleting(&self) -> Option<&Deletion> {
        self.deleting.as_ref()
    }

    /// The delete numbered `id` has ended, each of its entries as `ended` says: what left the
    /// disk comes off the tree, a folder that went only in part is rescanned (the tree's copy is
    /// no longer what is on disk), and the status bar says what was freed. Returns what to tell
    /// the user if anything failed — not what Cancel stopped. A report from another delete than
    /// the one under way is dropped.
    pub fn delete_done(&mut self, id: u64, ended: &[Ended]) -> Option<Failure> {
        if self
            .deleting
            .as_ref()
            .is_none_or(|deletion| deletion.id != id)
        {
            return None;
        }
        let deletion = self.deleting.take()?;
        let files = &deletion.files;
        let mut removed = Vec::new();
        let mut failures = Vec::new();
        let mut partly = Vec::new();
        for (file, ended) in files.iter().zip(ended) {
            match ended {
                Ended::Removed => removed.push(file.clone()),
                Ended::Failed {
                    error,
                    partly: some,
                } => {
                    if *some {
                        partly.push(file.path_to_file.clone());
                    }
                    if let Some(error) = error {
                        failures.push((file, error.clone()));
                    }
                }
            }
        }
        self.removed(&removed, deletion.freed);
        for relative in partly {
            self.start_rescan(relative);
        }
        if !removed.is_empty() {
            let size: u128 = removed.iter().map(|file| file.size).sum();
            let items = match removed.len() {
                1 => "1 item".to_string(),
                n => format!("{} items", DisplayCount(n as u64)),
            };
            self.say(if deletion.freed {
                format!("Deleted {items}, freeing {}", DisplaySize(size as f64))
            } else {
                format!("Moved {items} ({}) to the Trash", DisplaySize(size as f64))
            });
        }
        let (file, error) = failures.first()?;
        let more = match failures.len() {
            1 => String::new(),
            n => format!(
                "\n\n{} more could not be removed either.",
                DisplayCount(n as u64 - 1)
            ),
        };
        let done = match removed.len() {
            0 => String::new(),
            n => format!(
                "\n\n{} of {} were removed.",
                DisplayCount(n as u64),
                DisplayCount(files.len() as u64)
            ),
        };
        Some(Failure {
            title: format!(
                "Could not remove {}",
                libduscape::format::shown_path(&file.full_path())
            ),
            detail: format!("{error}{more}{done}"),
        })
    }

    /// Cancel the delete under way, after the entry it is on; what it removed comes off the
    /// tree when it reports.
    pub fn cancel_delete(&self) {
        if let Some(deletion) = &self.deleting {
            deletion.cancel();
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
        self.refresh_steady();
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
    /// Rescan the folder in hand — the row's, nested or not — or the folder holding the file
    /// in hand: the folder shown for a top-level file, a nested file's own.
    pub fn rescan_selected(&mut self) {
        let mut relative = self.tree.current_folder_names.clone();
        if let Some(row) = self.cursor_entry() {
            let keep = if row.entry.file_type == FileType::Folder {
                row.path.len()
            } else {
                row.path.len() - 1
            };
            relative.extend(row.path[..keep].iter().cloned());
        } else if let Some(entry) = self.selected_entry()
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
        // A fill's batch changes no size and moves nothing: only the folder shown, if it was
        // among those filled, is laid out again, and the preview is left as it is.
        if let Some((filled, left)) = finished.filled {
            let here = self.tree.get_current_path();
            if filled.contains(&here) {
                self.refresh_steady();
            }
            // The last batch is the fill's end, and the snapshots' folder may come next.
            if left.is_none() {
                self.start_idle();
            }
            return;
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
            self.refresh_steady();
            if self.selected.is_none() {
                self.select_first();
            }
        }
        // The tree is current now: what the saved scan trimmed is put back behind it, or the
        // volume's snapshots read, if nothing else is under way.
        self.start_idle();
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
            .shown()
            .filter(|(_, entry)| entry.file_type == FileType::File)
            .map(|(path, _)| (self.row_path(path), pixels));
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
            .unwrap_or_else(|| libduscape::format::shown_path(&path))
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
            _ if !self.marked.is_empty() && self.hover.is_none() && self.hover_nested.is_none() => {
                format!(
                    "{} marked, {}",
                    DisplayCount(self.marked_count() as u64),
                    DisplaySize(self.marked_size() as f64)
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
            words.push(format!("scanning, {}", entries(self.entries_scanned)));
        } else if let Some(took) = self.scan_took {
            words.push(format!(
                "{} in {:.1}s",
                entries(self.entries_scanned),
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
        // The volume's local snapshots hold some of what no walk finds (the treemap's "Not
        // seen by the scan"), and only root can read them.
        if let Some(noted) = self
            .tree
            .snapshots
            .filter(|noted| noted.count > 0 && !noted.read)
            && !duscape_scan::snapshots::can_read()
        {
            words.push(format!(
                "{} local snapshots, read as root",
                DisplayCount(noted.count as u64)
            ));
        }
        if let Some(over) = self.tree.counted_beyond_volume() {
            words.push(format!(
                "{} more than the volume holds: blocks shared or compressed, counted in full",
                DisplaySize(over as f64)
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
pub fn describe_tile(tile: &libduscape::tiles::Tile) -> String {
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
        (FileType::Folder, Some(count)) => {
            format!("folder, {}", libduscape::format::items(count))
        }
        (FileType::Folder, None) => "folder".to_string(),
        (FileType::File, _) => "file".to_string(),
    };
    // What a system keeps the folder for; a file of the name is just a file.
    let nas = (entry.file_type == FileType::Folder)
        .then(|| libduscape::nas::describe(&entry.name))
        .flatten()
        .map(|words| format!(" · {words}"))
        .unwrap_or_default();
    format!(
        "{} — {} ({:.1}%) · {kind}{nas}",
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

/// A colour `by` of the way to white.
#[must_use]
pub fn lighter((r, g, b): (f64, f64, f64), by: f64) -> (f64, f64, f64) {
    (r + (1.0 - r) * by, g + (1.0 - g) * by, b + (1.0 - b) * by)
}

/// `shade` of a colour: 1.0 as it is, less for darker.
#[must_use]
pub fn darker((r, g, b): (f64, f64, f64), shade: f64) -> (f64, f64, f64) {
    (r * shade, g * shade, b * shade)
}

/// The free space's tile: dark and neutral beside the entries' colours, so what is used stands
/// out and what is free reads as room.
pub const FREE_SPACE_COLOR: (f64, f64, f64) = (0.30, 0.34, 0.32);
/// The used space the scan has not found (yet): darker than the free space, and grey.
pub const UNSCANNED_COLOR: (f64, f64, f64) = (0.22, 0.22, 0.24);
/// The unseen space that may be snapshots and purgeable: between the two, a little warmer, as
/// space the system could give back.
pub const PURGEABLE_COLOR: (f64, f64, f64) = (0.30, 0.27, 0.22);

/// How much darker an entry `depth` levels into the nesting is than on the board: a step a
/// level, to four, so the nesting reads as depth.
#[must_use]
pub fn depth_shade(depth: usize) -> f64 {
    1.0 - 0.12 * depth.min(4) as f64
}

/// The colour of an entry's tile — or its speck — `depth` levels into the nesting (0 the
/// board's): [`tile_color`], [`depth_shade`] darker.
#[must_use]
pub fn entry_color(name: &OsStr, file_type: FileType, depth: usize) -> (f64, f64, f64) {
    darker(tile_color(name, file_type), depth_shade(depth))
}

/// A tile's colour, as sRGB components: folders in blues, files by their extension — so files
/// of a kind share a colour, from one listing to the next — in muted tones that white text
/// reads on. Only the name decides it, never the entry's place: a scan re-sorts the listing
/// with every batch, and a colour by rank changed under the pointer as it did.
pub fn tile_color(name: &OsStr, file_type: FileType) -> (f64, f64, f64) {
    if file_type == FileType::Folder {
        return folder_color(fnv(name.as_encoded_bytes()));
    }
    let extension = Path::new(name)
        .extension()
        .map(|extension| extension.to_ascii_lowercase());
    let Some(extension) = extension else {
        return (0.45, 0.45, 0.47);
    };
    let hash = fnv(extension.as_encoded_bytes());
    // Hues clear of the folders' blues and the violets beside them (180°–280°), so a file never
    // passes for a folder.
    let hue = (hash % 260) as f64;
    let hue = if hue >= 180.0 { hue + 100.0 } else { hue };
    hsl(hue, 0.38, 0.46)
}

/// One of the folders' four blues, by `seed`: a folder's is its name's hash.
#[must_use]
pub fn folder_color(seed: u64) -> (f64, f64, f64) {
    const FOLDERS: [(f64, f64, f64); 4] = [
        (0.22, 0.42, 0.68),
        (0.25, 0.48, 0.76),
        (0.18, 0.36, 0.58),
        (0.29, 0.53, 0.80),
    ];
    FOLDERS[(seed % FOLDERS.len() as u64) as usize]
}

/// FNV-1a: stable across runs and platforms, unlike the std hasher.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
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

mod marks;
#[cfg(test)]
mod tests;
mod tween;
pub use marks::Marks;
pub use tween::TWEEN;

/// `count` entries, as words: `1 entry`, `12,140 entries`.
fn entries(count: u64) -> String {
    match count {
        1 => "1 entry".to_string(),
        count => format!("{} entries", DisplayCount(count)),
    }
}

/// The volume whose root is `path`: its free bytes and its used, or `None` when `path` is no
/// volume's root. Both are asked, the used for the share of it the scan has not found yet.
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    Some((
        libduscape::os::volume_free(path)?,
        libduscape::os::volume_used(path)?,
    ))
}
