use ::ratatui::backend::Backend;
use ::ratatui::layout::Rect;
use ::std::ffi::{OsStr, OsString};
use ::std::mem::ManuallyDrop;
use ::std::path::PathBuf;
use ::std::sync::mpsc::{Receiver, SyncSender};
use ::std::time::{Duration, Instant};

use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};

use libduscape::format::{DisplayCount, copied_path};
use libduscape::model::SizeKind;
use libduscape::tiles::Board;
use libduscape::{DirSummary, FileOrFolder, FileToDelete, FileTree, Folder};
use ratatui::crossterm::event::MouseButton;

use crate::Event;
use crate::clipboard::{Clipboard, SystemClipboard};
use crate::config::Keybinds;
use crate::messages::{Instruction, handle_instructions};
use crate::preview::{
    Graphics, NoGraphics, Pictures, Placement, Preview, Previewer, Request, placement_in,
};
use crate::state::UiEffects;
use crate::ui::side_panel::{self, picture_area};
use crate::ui::{Display, Strip};
use duscape_scan::refine::{Found, SmallFiles};
use duscape_scan::rescan::{Outcome, Refiner, Rescanner, Rescans};

/// Which panel the arrow keys, Enter, Esc and delete act on: the list beside the treemap, or the
/// treemap itself. It follows the last click, Tab, and Left off the treemap's left edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    Treemap,
    #[default]
    List,
}

/// A jump through the list, from the keys that move by more than a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListJump {
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone)]
pub enum UiMode {
    Loading,
    Normal,
    ScreenTooSmall,
    /// Asking to delete these: the marked entries, or the one in hand.
    DeleteFiles(Vec<FileToDelete>),
    ErrorMessage(String),
    Exiting {
        app_loaded: bool,
    },
    #[allow(dead_code)]
    WarningMessage(FileToDelete),
}

pub struct App<B>
where
    B: Backend,
{
    pub is_running: bool,
    pub loaded: bool,
    pub ui_mode: UiMode,
    board: Board,
    file_tree: ManuallyDrop<FileTree>,
    display: Display<B>,
    event_sender: SyncSender<Event>,
    ui_effects: UiEffects,
    pub keybinds: Keybinds,
    /// Sizes are shown as the logical length rather than on-disk usage. Passed to the title so the
    /// reader knows which they see; the scan itself decides the figures.
    show_apparent_size: bool,
    /// When the app was created — a stand-in for the scan's start, which begins moments later — and
    /// how long the scan took once it completed. The title shows the elapsed time when done.
    scan_start: Instant,
    scan_duration: Option<Duration>,
    /// The entry last clicked — on its tile or its row in the side panel — with which button and
    /// when, so that a second click on it soon after counts as a double click.
    last_click: Option<(MouseButton, OsString, Instant)>,
    /// Which panel the keyboard is on. Only [`Focus::List`] while the side panel is showing.
    focus: Focus,
    /// The list's highlighted entry, by name: the list re-sorts as a scan adds to it, and a
    /// position would then point at another entry. Kept by name for the same reason as clicks.
    /// `None` is the top of the list, which is highlighted until the cursor is moved.
    list_cursor: Option<OsString>,
    /// A multi-selection, by name, in the order its entries were added: made with Ctrl+click and
    /// Shift+Up/Down, and copied to the clipboard as it changes.
    marked: Vec<OsString>,
    /// Whether the entry in hand was chosen — by a plain click, arrow or jump — rather than put
    /// there by the app (the top row of a folder just entered, the neighbour of what was just
    /// deleted). Only a chosen entry is swept into a selection begun with Ctrl+click, so a
    /// selection, and a deletion made from it, never takes in something nobody picked.
    cursor_chosen: bool,
    /// A Shift+Up/Down range in progress: where it started, and what was marked before it.
    /// Reversing direction shrinks the range back towards the anchor, leaving the rest alone.
    mark_range: Option<(OsString, Vec<OsString>)>,
    /// The thread that reads files for the preview, once started; without one nothing is read.
    previewer: Option<Previewer>,
    /// How pictures are shown, and what draws them when the terminal does.
    pictures: Pictures,
    graphics: Box<dyn Graphics>,
    /// What the preview was last asked for — the file, the picture area and the cell size — so
    /// it is asked again only when one of them changes.
    preview_key: Option<PreviewKey>,
    /// Counts requests, so an answer to one the selection has since moved past is dropped.
    preview_generation: u64,
    preview: Preview,
    /// The screen size the last frame was drawn at: a new one clears the screen, and with it any
    /// picture the terminal drew.
    screen_size: Option<Rect>,
    /// Where copied paths go: the system clipboard, or a recorder in tests.
    clipboard: Box<dyn Clipboard>,
    /// The directory duscape was started from, resolved, which copied relative paths start
    /// from. `None` when it cannot be known (it was deleted); relative copies are absolute then.
    working_dir: Option<PathBuf>,
    /// What starts rescans; without one, `r` and `R` do nothing.
    rescanner: Option<Rescanner>,
    /// Rescans under way, oldest first.
    rescans: Rescans,
    /// The last key or mouse press. The help line rests at least [`crate::ui::REST`] from it.
    last_input: Instant,
    /// Set while the help line slides, for the ticker thread to tick at frame rate.
    ticker_fast: Arc<AtomicBool>,
    /// What runs the second pass over small files; without one there is none.
    refiner: Option<Refiner>,
    /// The second pass under way, if one is: its generation (answers from an older one are
    /// dropped), what stops it, and how many files it has left.
    refine_generation: u64,
    refine_cancel: Option<Arc<AtomicBool>>,
    pub(crate) refining: Option<usize>,
    /// The folder the user is in, for the second pass to take first.
    refine_focus: Arc<::std::sync::Mutex<Option<PathBuf>>>,
    /// Folders rescanned since the second pass began: they had a second pass of their own, and
    /// the whole tree's findings in them are about files that have been replaced.
    refine_skip: Vec<PathBuf>,
}

/// Two clicks on one tile within this long are a double click. Terminals pass on presses without
/// the desktop's own double-click setting, so this is the common default.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// How long the title shows what was just copied to the clipboard.
const CLIPBOARD_FLASH: Duration = Duration::from_secs(2);

/// What a preview was asked for: the file, the picture area in cells, and the cell size in pixels.
type PreviewKey = (PathBuf, (u16, u16), (u16, u16));

impl<B> App<B>
where
    B: Backend,
{
    pub fn new(
        terminal_backend: B,
        path_in_filesystem: PathBuf,
        event_sender: SyncSender<Event>,
        keybinds: Keybinds,
        show_apparent_size: bool,
    ) -> Self {
        let display = Display::new(terminal_backend);
        let shown = if show_apparent_size {
            SizeKind::Apparent
        } else {
            SizeKind::Disk
        };
        let mut board = Board::new(&Folder::new(&path_in_filesystem));
        board.show(shown);
        let base_folder = Folder::new(&path_in_filesystem);
        let mut file_tree = ManuallyDrop::new(FileTree::new(base_folder, path_in_filesystem));
        file_tree.shown = shown;
        // we use ManuallyDrop here because otherwise the app takes forever to exit
        let ui_effects = UiEffects::new();
        App {
            is_running: true,
            loaded: false,
            board,
            file_tree,
            display,
            ui_mode: UiMode::Loading,
            event_sender,
            ui_effects,
            keybinds,
            show_apparent_size,
            scan_start: Instant::now(),
            scan_duration: None,
            last_click: None,
            focus: Focus::List,
            list_cursor: None,
            marked: Vec::new(),
            cursor_chosen: false,
            mark_range: None,
            previewer: None,
            pictures: Pictures::Described,
            graphics: Box::new(NoGraphics),
            preview_key: None,
            preview_generation: 0,
            screen_size: None,
            preview: Preview::None,
            clipboard: Box::new(SystemClipboard),
            // Resolved like the scan root, so that `..` counts real directories on both sides.
            working_dir: ::std::env::current_dir()
                .and_then(|dir| dir.canonicalize())
                .ok(),
            rescanner: None,
            rescans: Rescans::default(),
            last_input: Instant::now(),
            ticker_fast: Arc::new(AtomicBool::new(false)),
            refiner: None,
            refine_generation: 0,
            refine_cancel: None,
            refining: None,
            refine_focus: Arc::new(::std::sync::Mutex::new(None)),
            refine_skip: Vec::new(),
        }
    }
    /// Start previewing files: `previewer` reads them, and pictures are shown as `pictures`
    /// says — by `graphics` for [`Pictures::Kitty`] and [`Pictures::Sixel`], in the frame
    /// itself for blocks.
    pub fn enable_previews(
        &mut self,
        previewer: Previewer,
        pictures: Pictures,
        graphics: Box<dyn Graphics>,
    ) {
        self.previewer = Some(previewer);
        self.pictures = pictures;
        self.graphics = graphics;
    }
    /// Allow rescans, which `rescanner` runs.
    pub fn enable_rescans(&mut self, rescanner: Rescanner) {
        self.rescanner = Some(rescanner);
    }
    /// Set while the help line is sliding and wants ticks every frame, rather than a few a second.
    pub fn ticker_pace(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ticker_fast)
    }
    /// A key or mouse button was pressed: the help line stops where it is for a while.
    pub fn note_input(&mut self) {
        self.last_input = Instant::now();
    }
    /// Move the help line on: another frame of a slide, or the start of one once it has rested.
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }
    fn tick_at(&mut self, now: Instant) {
        if matches!(self.ui_mode, UiMode::ScreenTooSmall) {
            return;
        }
        let strip = Strip::new(&self.keybinds, !self.loaded, self.side_panel_visible());
        // The width the bottom line draws it in.
        let width = self.display.size().width.saturating_sub(2);
        let moved = strip.advance(&mut self.ui_effects.ticker, width, self.last_input, now);
        self.ticker_fast
            .store(self.ui_effects.ticker.sliding(), Ordering::Release);
        if moved {
            self.render();
        }
    }
    /// Scan the selected folder again — or the folder shown, when what is selected is a file or
    /// nothing — and put what is on disk now in place of what the tree says.
    pub fn rescan_selected(&mut self) {
        let mut relative = self.file_tree.current_folder_names.clone();
        if let Some(entry) = self.selected_entry()
            && entry.file_type == libduscape::tiles::FileType::Folder
        {
            relative.push(entry.name);
        }
        self.start_rescan(relative);
    }
    /// Scan everything again.
    pub fn rescan_all(&mut self) {
        self.start_rescan(Vec::new());
    }
    /// Start rescanning the folder at `relative`, from the scan root. Not while the first scan
    /// runs, which is reading all of it anyway; and not when a rescan under way already covers
    /// it. One that it covers is stopped, as this one brings that folder back as well.
    fn start_rescan(&mut self, relative: Vec<OsString>) {
        if !self.loaded {
            return;
        }
        let Some(rescanner) = &self.rescanner else {
            return;
        };
        if self.rescans.start(rescanner, &self.file_tree, relative) {
            self.describe_rescans();
            self.render();
        }
    }
    /// Start again every rescan whose folder holds one of `files`: one that listed a file before
    /// it was deleted would otherwise put it back, counted as present and as freed both.
    fn restart_rescans_under(&mut self, files: &[FileToDelete]) {
        let Some(rescanner) = &self.rescanner else {
            return;
        };
        if self
            .rescans
            .restart_under(rescanner, &self.file_tree, files)
        {
            self.describe_rescans();
            self.render();
        }
    }
    /// Say in the status line what is being rescanned.
    fn describe_rescans(&mut self) {
        self.ui_effects.rescanning = self.rescans.describe(&self.file_tree.path_in_filesystem);
    }
    /// Switch between sizes on disk and apparent sizes. The tree holds both, so nothing is scanned
    /// again: the board is laid out anew and the entry in hand stays in hand.
    pub fn toggle_size(&mut self) {
        let kind = self.file_tree.shown.other();
        self.file_tree.shown = kind;
        self.show_apparent_size = kind == SizeKind::Apparent;
        self.board.show(kind);
        let selected = self
            .board
            .currently_selected()
            .map(|tile| tile.name.clone());
        let current_folder = self.file_tree.get_current_folder();
        self.board.change_files(current_folder);
        if let Some(tile) =
            selected.and_then(|name| self.board.tiles.iter().position(|tile| tile.name == name))
        {
            self.board.set_selected_index(&tile);
        }
        self.render();
    }
    /// Allow the second pass, which `refiner` runs.
    pub fn enable_refining(&mut self, refiner: Refiner) {
        self.refiner = Some(refiner);
    }
    /// Start the second pass over the whole tree's small files, stopping any already under way:
    /// a new whole tree has just replaced the one it was refining.
    pub fn start_refining(&mut self, small: SmallFiles) {
        if let Some(cancel) = self.refine_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.refine_generation += 1;
        self.refine_skip.clear();
        self.refining = None;
        let Some(refiner) = &self.refiner else {
            return;
        };
        if small.is_empty() {
            return;
        }
        self.refining = Some(small.len());
        let cancel = Arc::new(AtomicBool::new(false));
        refiner.spawn(
            self.refine_generation,
            small,
            Arc::clone(&self.refine_focus),
            Arc::clone(&cancel),
        );
        self.refine_cancel = Some(cancel);
    }
    /// Findings of the second pass: fold them in and redraw, keeping the entry in hand.
    pub fn refined(&mut self, generation: u64, mut found: Vec<Found>, left: Option<usize>) {
        if generation != self.refine_generation {
            return;
        }
        self.refining = left;
        if left.is_none() {
            self.refine_cancel = None;
        }
        found.retain(|found| {
            !self
                .refine_skip
                .iter()
                .any(|skipped| found.dir.starts_with(skipped))
        });
        let here = self.file_tree.get_current_path();
        let on_screen = found.iter().any(|found| found.dir.starts_with(&here));
        // Findings elsewhere change only totals above the folder shown, which the title draws.
        if self.file_tree.apply_found(&found) > 0 && on_screen {
            let selected = self
                .board
                .currently_selected()
                .map(|tile| tile.name.clone());
            let current_folder = self.file_tree.get_current_folder();
            self.board.change_files(current_folder);
            if let Some(tile) =
                selected.and_then(|name| self.board.tiles.iter().position(|tile| tile.name == name))
            {
                self.board.set_selected_index(&tile);
            }
        }
        self.render();
    }
    /// A rescan has finished: put what it found in the tree. One stopped in the meantime is
    /// dropped.
    pub fn rescan_done(&mut self, id: u64, outcome: Outcome) {
        let selected = self
            .board
            .currently_selected()
            .map(|tile| tile.name.clone());
        let navigated_to = self.file_tree.current_folder_names.clone();
        let Some(finished) = self.rescans.finish(id, outcome, &mut self.file_tree) else {
            return;
        };
        // Leaked, for the reason `file_tree` is `ManuallyDrop`: dropping a folder walks
        // everything in it, here on the thread that draws the screen.
        ::std::mem::forget(finished.old);
        self.describe_rescans();
        if let Some((duration, small)) = finished.whole {
            self.scan_duration = Some(duration);
            self.start_refining(small);
        }
        if let Some(folder) = finished.refined_folder {
            self.refine_skip.push(folder);
        }
        let changed = finished.changed;
        if changed {
            // A whole rescan puts the user back at the root if their folder has gone; what was
            // chosen there, and the way back up from it, belong to the old place.
            if self.file_tree.current_folder_names != navigated_to {
                self.board.previous_indices_and_zoom_level.clear();
                self.board.reset_zoom_index();
                self.list_cursor = None;
                self.cursor_chosen = false;
                self.clear_marks();
            }
            // Out of a folder that is no longer there, to the nearest one that is.
            while !self.file_tree.current_folder_exists() && self.file_tree.leave_folder() {
                self.board.pop_previous_index_and_zoom_level();
                self.board.reset_zoom_index();
                self.list_cursor = None;
                self.cursor_chosen = false;
                self.clear_marks();
            }
            // The file in hand may have changed on disk too.
            self.preview_key = None;
            self.board.reset_selected_index();
            let current_folder = self.file_tree.get_current_folder();
            self.board.change_files(current_folder);
            // The same entry stays in hand, wherever the new sizes put its tile.
            if let Some(tile) =
                selected.and_then(|name| self.board.tiles.iter().position(|tile| tile.name == name))
            {
                self.board.set_selected_index(&tile);
            }
        }
        self.render();
    }
    /// A preview has been read. Kept only if it answers the latest request.
    pub fn preview_ready(&mut self, generation: u64, preview: Preview) {
        if generation == self.preview_generation {
            self.preview = preview;
            self.render();
        }
    }
    /// The file to preview: the entry in hand, when it is a file and it is the only one chosen.
    fn preview_target(&self) -> Option<PathBuf> {
        if self.marked.len() > 1 {
            return None;
        }
        let entry = self.selected_entry()?;
        if entry.file_type != libduscape::tiles::FileType::File {
            return None;
        }
        let mut path = self.file_tree.get_current_path();
        path.push(&entry.name);
        Some(path)
    }
    /// Ask for a preview if what should be shown has changed since it was last asked for.
    fn update_preview(&mut self) {
        let cell_pixels = self.display.cell_pixels();
        let key = self
            .display
            .areas()
            .preview
            .map(picture_area)
            .zip(self.preview_target())
            .map(|(area, path)| (path, (area.width, area.height), cell_pixels));
        if key == self.preview_key {
            return;
        }
        self.preview_key = key.clone();
        self.preview_generation += 1;
        self.preview = match (key, &self.previewer) {
            (Some((path, cells, cell_pixels)), Some(previewer)) => {
                previewer.request(Request {
                    generation: self.preview_generation,
                    path,
                    cells,
                    cell_pixels,
                    pictures: self.pictures,
                });
                Preview::Loading
            }
            _ => Preview::None,
        };
    }
    /// The line above the preview: what is in hand, and how big.
    fn preview_caption(&self) -> String {
        if self.marked.len() > 1 {
            let size: u128 = self
                .get_files_to_delete()
                .iter()
                .map(|file| file.size)
                .sum();
            return format!(
                "{} marked · {}",
                DisplayCount(self.marked.len() as u64),
                libduscape::format::DisplaySize(size as f64)
            );
        }
        let Some(entry) = self.selected_entry() else {
            return String::new();
        };
        let mut caption = entry.name.to_string_lossy().into_owned();
        if entry.file_type == libduscape::tiles::FileType::Folder {
            caption.push('/');
        }
        caption.push_str(&format!(
            " · {}",
            libduscape::format::DisplaySize(entry.size as f64)
        ));
        if let Some(description) = self.preview.picture_description() {
            caption.push_str(&format!(" · {description}"));
        }
        caption
    }
    /// Where the picture goes over the frame, if anywhere. A picture that text would destroy is
    /// taken away while a dialog is up.
    fn picture_placement(&self) -> Option<Placement> {
        let dialog = matches!(
            self.ui_mode,
            UiMode::DeleteFiles(_)
                | UiMode::ErrorMessage(_)
                | UiMode::Exiting { .. }
                | UiMode::WarningMessage(_)
        );
        match (&self.preview, self.display.areas().preview, &self.ui_mode) {
            (_, _, UiMode::ScreenTooSmall) => None,
            _ if dialog && !self.graphics.stays_under_text() => None,
            (Preview::Image(image), Some(area), _) => Some(placement_in(picture_area(area), image)),
            _ => None,
        }
    }
    /// Send copied paths somewhere other than the system clipboard, for tests.
    #[cfg(test)]
    pub(crate) fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = clipboard;
    }
    /// Pretend duscape was started from `working_dir`, for tests.
    #[cfg(test)]
    pub(crate) fn set_working_dir(&mut self, working_dir: Option<PathBuf>) {
        self.working_dir = working_dir;
    }
    pub fn start(&mut self, receiver: Receiver<Instruction>) {
        handle_instructions(self, receiver);
        self.display.clear();
    }
    pub fn render_and_update_board(&mut self) {
        // Wherever the user has gone, the second pass goes there next.
        let here = self.file_tree.get_current_path();
        if let Ok(mut focus) = self.refine_focus.lock()
            && focus.as_ref() != Some(&here)
        {
            *focus = Some(here);
        }
        let current_folder = self.file_tree.get_current_folder();
        self.board.change_files(current_folder);
        self.render();
    }
    pub fn increment_loading_progress_indicator(&mut self) {
        self.ui_effects.increment_loading_progress_indicator();
    }
    pub fn render(&mut self) {
        let full_screen_size = self.display.size();
        if full_screen_size.width < 50 || full_screen_size.height < 15 {
            self.ui_mode = UiMode::ScreenTooSmall;
        }
        // With the list in hand the treemap shows the list's entry, whatever changed under it —
        // a scan re-sorting the list, a folder entered, a relayout.
        if self.focus() == Focus::List {
            self.sync_board_to_list();
        }
        // Measured before the preview is asked for, so the very first request is sized for the
        // real cells and is not superseded by a second one as soon as the frame is drawn.
        self.display.refresh_cell_pixels();
        self.update_preview();
        // A new size clears the screen as the frame is drawn.
        if self.screen_size.replace(full_screen_size) != Some(full_screen_size) {
            self.graphics.cleared();
        }
        let placement = self.picture_placement();
        self.graphics.prepare(placement.as_ref());
        let panel_state = crate::ui::PanelState {
            list_focused: self.focus() == Focus::List,
            highlighted: self.highlighted_listing_index(),
            selected: self.selected_entry(),
            marked: self.marked.clone(),
            preview: self.preview.clone(),
            caption: self.preview_caption(),
        };
        self.display.render(
            &mut self.file_tree,
            &mut self.board,
            &self.ui_mode,
            &self.ui_effects,
            &self.keybinds,
            crate::ui::ViewStatus {
                title: crate::ui::TitleStatus {
                    apparent_size: self.show_apparent_size,
                    scan_duration: self.scan_duration,
                    refining: self.refining,
                },
                panel: panel_state,
            },
        );
        // Drawn over the frame just drawn, or taken away.
        self.graphics.show(placement);
    }
    pub fn flash_space_freed(&mut self) {
        self.ui_effects.flash_space_freed = true;
    }
    pub fn unflash_space_freed(&mut self) {
        self.ui_effects.flash_space_freed = false;
    }
    pub fn set_path_to_red(&mut self) {
        self.ui_effects.current_path_is_red = true;
    }
    pub fn reset_current_path_color(&mut self) {
        self.ui_effects.current_path_is_red = false;
    }
    pub fn start_ui(&mut self) {
        self.ui_mode = UiMode::Normal;
        self.loaded = true;
        self.render_and_update_board();
    }
    /// Add the outlines of several scanned directories to the live view.
    pub fn add_scanned_summaries(&mut self, summaries: Vec<DirSummary>) {
        let mut failed = 0;
        let mut last_path = None;
        for summary in summaries {
            failed += summary.dirs.failed;
            last_path = Some(Arc::clone(&summary.dirs.path));
            self.file_tree.add_summary(summary);
        }
        self.file_tree.failed_to_read += failed;
        if let Some(path) = last_path {
            self.ui_effects.last_read_path = Some(path.to_path_buf());
        }
    }
    /// Replace the live view's outline with the finished tree, keeping the user where they are.
    pub fn finish_scan(&mut self, mut tree: FileTree) {
        tree.adopt_navigation_from(&self.file_tree);
        // Dropping a `ManuallyDrop` runs no destructor, so the outline is leaked on purpose — for
        // the same reason the field is `ManuallyDrop` at all: nobody is waiting for its memory
        // back, and dropping a tree is a recursive walk of every folder in it.
        let _outline = std::mem::replace(&mut self.file_tree, ManuallyDrop::new(tree));
        self.scan_duration = Some(self.scan_start.elapsed());
    }
    pub fn reset_ui_mode(&mut self) {
        match self.ui_mode {
            UiMode::Loading | UiMode::Normal => {}
            _ => {
                self.ui_mode = {
                    if self.loaded {
                        UiMode::Normal
                    } else {
                        UiMode::Loading
                    }
                }
            }
        };
    }
    pub fn show_warning_modal(&mut self) {
        let files = self.get_files_to_delete();
        if self.refuse_metafile(&files) {
            return;
        }
        if let Some(file_to_delete) = files.into_iter().next() {
            self.ui_mode = UiMode::WarningMessage(file_to_delete);
            self.render();
        }
    }
    pub fn prompt_exit(&mut self) {
        self.ui_mode = UiMode::Exiting {
            app_loaded: self.loaded,
        };
        self.render();
    }
    pub fn exit(&mut self) {
        self.is_running = false;
        // here we do a blocking send rather than a try_send
        // because we want to make sure that if the receiver
        // is active, it received this event so that the app
        // would exit cleanly
        let _ = self.event_sender.send(Event::AppExit);
    }
    pub fn handle_enter(&mut self) {
        if self.focus() == Focus::List {
            if let Some(index) = self.list_position() {
                let name = self.board.listing()[index].name.clone();
                self.enter_named(&name);
            }
            return;
        }
        if !self.board.has_selected_index() {
            self.board.move_to_largest_folder();
        }
        self.enter_selected();
    }
    pub fn move_selected_right(&mut self) {
        self.cursor_chosen = true;
        match self.focus() {
            // Right from the list goes back to the treemap it sits beside.
            Focus::List => self.focus_treemap(),
            Focus::Treemap => self.board.move_selected_right(),
        }
        self.render();
    }
    pub fn move_selected_left(&mut self) {
        self.cursor_chosen = true;
        match self.focus() {
            Focus::List => {}
            Focus::Treemap => {
                // Left off the treemap's left edge would clear the selection; with the list
                // there, it moves into the list instead.
                let before = self.board.get_selected_index();
                self.board.move_selected_left();
                if let (Some(index), None) = (before, self.board.get_selected_index())
                    && self.side_panel_visible()
                {
                    self.board.set_selected_index(&index);
                    self.focus_list();
                }
            }
        }
        self.render();
    }
    pub fn move_selected_down(&mut self) {
        self.clear_marks();
        self.cursor_chosen = true;
        match self.focus() {
            Focus::List => self.move_list_cursor(1),
            Focus::Treemap => self.board.move_selected_down(),
        }
        self.render();
    }
    pub fn move_selected_up(&mut self) {
        self.clear_marks();
        self.cursor_chosen = true;
        match self.focus() {
            Focus::List => self.move_list_cursor(-1),
            Focus::Treemap => self.board.move_selected_up(),
        }
        self.render();
    }
    /// Page Up, Page Down, Home and End: through the list when it has the keyboard.
    pub fn jump_list(&mut self, jump: ListJump) {
        if self.focus() != Focus::List {
            return;
        }
        // A plain move, like the arrow keys: it starts over.
        self.clear_marks();
        self.cursor_chosen = true;
        let page = isize::try_from(self.list_rows().saturating_sub(1).max(1)).unwrap_or(1);
        match jump {
            ListJump::PageUp => self.move_list_cursor(-page),
            ListJump::PageDown => self.move_list_cursor(page),
            ListJump::Home => self.move_list_cursor(isize::MIN),
            ListJump::End => self.move_list_cursor(isize::MAX),
        }
        self.render();
    }
    /// Shift+Down (`delta` 1) or Shift+Up (-1) in the list: move the cursor and mark every entry
    /// from where the run of Shift moves began to where the cursor now is, then copy the marked
    /// paths. The treemap has no order to extend a range along, so there it does nothing.
    pub fn extend_selection(&mut self, delta: isize) {
        self.extend_selection_at(delta, Instant::now());
    }
    fn extend_selection_at(&mut self, delta: isize, now: Instant) {
        if self.focus() != Focus::List {
            return;
        }
        let Some(start) = self.list_position() else {
            return;
        };
        if self.mark_range.is_none() {
            let anchor = self.board.listing()[start].name.clone();
            self.mark_range = Some((anchor, self.marked.clone()));
        }
        let Some((anchor, base)) = self.mark_range.clone() else {
            return;
        };
        self.move_list_cursor(delta);
        let listing = self.board.listing();
        let (Some(from), Some(to)) = (
            listing.iter().position(|entry| entry.name == anchor),
            self.list_position(),
        ) else {
            return;
        };
        // From the anchor towards the cursor, so the copy lists them in the order swept.
        let range: Vec<OsString> = if from <= to {
            (from..=to)
                .map(|index| listing[index].name.clone())
                .collect()
        } else {
            (to..=from)
                .rev()
                .map(|index| listing[index].name.clone())
                .collect()
        };
        let mut marked = base;
        for name in range {
            if !marked.contains(&name) {
                marked.push(name);
            }
        }
        self.marked = marked;
        self.copy_marked(now);
        self.render();
    }
    /// Ctrl+click: add the entry under the pointer to the multi-selection, or take it out, and
    /// copy what is then marked. Starting a selection this way takes in the entry already in
    /// hand, if one was chosen, as a desktop file manager does.
    pub fn ctrl_click(&mut self, column: u16, row: u16) {
        self.ctrl_click_at(column, row, Instant::now());
    }
    fn ctrl_click_at(&mut self, column: u16, row: u16, now: Instant) {
        let Some((clicked_in, tile, name)) = self.target_at(column, row) else {
            return;
        };
        if self.marked.is_empty() && self.cursor_chosen {
            let chosen = match self.focus() {
                Focus::List => self.list_cursor.clone(),
                Focus::Treemap => self
                    .board
                    .currently_selected()
                    .map(|tile| tile.name.clone()),
            };
            if let Some(chosen) = chosen.filter(|chosen| *chosen != name) {
                self.marked.push(chosen);
            }
        }
        match self.marked.iter().position(|marked| *marked == name) {
            Some(index) => {
                self.marked.remove(index);
            }
            None => self.marked.push(name.clone()),
        }
        self.mark_range = None;
        self.last_click = None;
        // Toggling the last mark off leaves nothing chosen to start the next selection from.
        self.cursor_chosen = false;
        self.focus = clicked_in;
        if clicked_in == Focus::List {
            self.list_cursor = Some(name);
        }
        if let Some(index) = tile {
            self.board.set_selected_index(&index);
        }
        if !self.marked.is_empty() {
            self.copy_marked(now);
        }
        self.render();
    }
    fn clear_marks(&mut self) {
        self.marked.clear();
        self.mark_range = None;
    }
    /// Copy the marked entries' paths, quoted, separated by spaces — ready to paste after a
    /// command — and show them in the title.
    fn copy_marked(&mut self, now: Instant) {
        let paths: Vec<String> = self
            .marked
            .iter()
            .map(|name| self.shell_path(name, false).1)
            .collect();
        let count = paths.len();
        let noun = if count == 1 { "path" } else { "paths" };
        self.copy_text(
            &paths.join(" "),
            &format!("{} {noun}:", DisplayCount(count as u64)),
            now,
        );
    }
    /// Move the keyboard to the other panel.
    pub fn switch_focus(&mut self) {
        match self.focus() {
            Focus::List => self.focus_treemap(),
            Focus::Treemap if self.side_panel_visible() => self.focus_list(),
            Focus::Treemap => {}
        }
        self.render();
    }
    /// The panel the keyboard is on. The list can only have it while it is on screen: a
    /// terminal narrowed past the point where the panel shows puts the keyboard back on the
    /// treemap, where arrows can be seen to do something.
    pub fn focus(&self) -> Focus {
        if self.focus == Focus::List && self.side_panel_visible() {
            Focus::List
        } else {
            Focus::Treemap
        }
    }
    fn side_panel_visible(&self) -> bool {
        self.display.areas().side_panel.is_some()
    }
    /// Rows the side panel has for entries.
    fn list_rows(&self) -> usize {
        self.display.areas().side_panel.map_or(0, |panel| {
            usize::from(panel.height.saturating_sub(side_panel::HEADER_ROWS))
        })
    }
    fn focus_list(&mut self) {
        self.focus = Focus::List;
        if self.list_cursor_index().is_none() {
            // Start where the treemap's selection is, or at the top.
            if let Some(index) =
                self.board
                    .selected_listing_index()
                    .or((!self.board.listing().is_empty()).then_some(0))
            {
                self.set_list_cursor(index);
            }
        }
    }
    fn focus_treemap(&mut self) {
        self.focus = Focus::Treemap;
        // Carry the list's entry over, if it has a tile to select.
        if let Some(name) = &self.list_cursor
            && let Some(tile) = self.board.tiles.iter().position(|tile| &tile.name == name)
        {
            self.board.set_selected_index(&tile);
        }
    }
    /// Where the list's cursor is: the entry moved to, or the top of the list until then.
    fn list_position(&self) -> Option<usize> {
        self.list_cursor_index()
            .or((!self.board.listing().is_empty()).then_some(0))
    }
    /// Select the tile of the list's entry, or nothing if it has none.
    fn sync_board_to_list(&mut self) {
        let tile = self.list_position().and_then(|index| {
            let name = &self.board.listing()[index].name;
            self.board.tiles.iter().position(|tile| &tile.name == name)
        });
        match tile {
            Some(tile) => self.board.set_selected_index(&tile),
            None => self.board.reset_selected_index(),
        }
    }
    /// Where the list's highlighted entry now sits in the listing, if it has been moved.
    fn list_cursor_index(&self) -> Option<usize> {
        let name = self.list_cursor.as_ref()?;
        self.board
            .listing()
            .iter()
            .position(|entry| &entry.name == name)
    }
    /// The listing entry the side panel highlights: the list's own when it has the keyboard,
    /// the treemap's selection otherwise.
    pub(crate) fn highlighted_listing_index(&self) -> Option<usize> {
        match self.focus() {
            Focus::List => self.list_position(),
            Focus::Treemap => self.board.selected_listing_index(),
        }
    }
    /// Put the list's highlight on entry `index`, and select its tile to match — or nothing, if
    /// it has none, so the treemap does not claim some other entry is the one in hand.
    fn set_list_cursor(&mut self, index: usize) {
        let Some(entry) = self.board.listing().get(index) else {
            return;
        };
        let name = entry.name.clone();
        match self.board.tiles.iter().position(|tile| tile.name == name) {
            Some(tile) => self.board.set_selected_index(&tile),
            None => self.board.reset_selected_index(),
        }
        self.list_cursor = Some(name);
    }
    /// Move the list's highlight by `delta` rows, stopping at either end. With nothing
    /// highlighted, the first move lands on the first entry.
    fn move_list_cursor(&mut self, delta: isize) {
        let len = self.board.listing().len();
        if len == 0 {
            return;
        }
        let next = match self.list_position() {
            Some(current) => current.saturating_add_signed(delta).min(len - 1),
            None => 0,
        };
        self.set_list_cursor(next);
    }
    /// A mouse press at a screen cell, on the tile there. A press on no tile does nothing.
    ///
    /// - Left: select the tile; a second left click on it within [`DOUBLE_CLICK`] enters it, as
    ///   Enter would.
    /// - Right: select the tile and copy its path, relative to the directory duscape was started
    ///   from, to the clipboard; a second right click on it within [`DOUBLE_CLICK`] copies its
    ///   absolute path instead.
    pub fn click(&mut self, button: MouseButton, column: u16, row: u16) {
        self.click_at(button, column, row, Instant::now());
    }
    /// What is under a screen cell: a tile, or a row of the list beside the board — either names
    /// an entry of this folder — with the panel it is in and its tile, if it has one.
    fn target_at(&self, column: u16, row: u16) -> Option<(Focus, Option<usize>, OsString)> {
        match self.board.tile_at(column, row) {
            Some(index) => Some((
                Focus::Treemap,
                Some(index),
                self.board.tiles[index].name.clone(),
            )),
            None => self.side_panel_entry_at(column, row).map(|name| {
                let tile = self.board.tiles.iter().position(|tile| tile.name == name);
                (Focus::List, tile, name)
            }),
        }
    }
    fn click_at(&mut self, button: MouseButton, column: u16, row: u16, now: Instant) {
        let Some((clicked_in, tile, name)) = self.target_at(column, row) else {
            self.last_click = None;
            return;
        };
        // Keyed on the name, not the tile index: the board is laid out again when the finished
        // tree replaces the scan's outline, and the same index can then be a different entry.
        let double = self
            .last_click
            .take()
            .is_some_and(|(last_button, last_name, at)| {
                last_button == button
                    && last_name == name
                    && now.saturating_duration_since(at) <= DOUBLE_CLICK
            });
        // A plain click starts over, as in any file manager; a right click copies one path and
        // leaves a multi-selection alone.
        if button == MouseButton::Left {
            self.clear_marks();
        }
        match (button, double) {
            (MouseButton::Left, true) => self.enter_named(&name),
            (MouseButton::Left | MouseButton::Right, _) => {
                if !double {
                    self.last_click = Some((button, name.clone(), now));
                }
                // An entry the zoom leaves off the board, or too small for a tile, has nothing
                // to select; it can still be entered or copied.
                // The keyboard follows the mouse to whichever panel was clicked.
                self.focus = clicked_in;
                self.cursor_chosen = true;
                if clicked_in == Focus::List {
                    self.list_cursor = Some(name.clone());
                }
                match tile {
                    Some(index) => self.board.set_selected_index(&index),
                    None if clicked_in == Focus::List => self.board.reset_selected_index(),
                    None => {}
                }
                if button == MouseButton::Right {
                    self.copy_path(&name, double, now);
                }
                self.render();
            }
            (MouseButton::Middle, _) => {}
        }
    }
    /// The entry whose row in the side panel is at a screen cell, if the panel is showing.
    fn side_panel_entry_at(&self, column: u16, row: u16) -> Option<OsString> {
        let panel = self.display.areas().side_panel?;
        let listing = self.board.listing();
        let index = side_panel::entry_at(
            listing,
            self.highlighted_listing_index(),
            panel,
            column,
            row,
        )?;
        Some(listing[index].name.clone())
    }
    /// Copy the path of `name`, in the current folder, to the clipboard — relative to the working
    /// directory, or absolute — quoted for the platform's shell, and show what was copied in the
    /// title.
    ///
    /// With no relative path to be had (the working directory is unknown, or on another drive),
    /// a relative copy is absolute, and the title says so.
    fn copy_path(&mut self, name: &OsStr, absolute: bool, now: Instant) {
        let (kind, text) = self.shell_path(name, absolute);
        self.copy_text(&text, &format!("{kind} path:"), now);
    }
    /// The path of `name`, in the current folder, quoted for the platform's shell: relative to
    /// the working directory, or absolute. Says which it is, since with no relative path to be
    /// had (the working directory is unknown, or on another drive) it is absolute either way.
    fn shell_path(&self, name: &OsStr, absolute: bool) -> (&'static str, String) {
        let full: PathBuf = self
            .file_tree
            .current_folder_names
            .iter()
            .map(OsString::as_os_str)
            .chain(Some(name))
            .fold(self.file_tree.path_in_filesystem.clone(), |path, part| {
                path.join(part)
            });
        let (kind, text) = copied_path(&full, self.working_dir.as_deref(), absolute);
        (kind.name(), text)
    }
    /// Put `text` on the clipboard and flash it in the title after `label`.
    fn copy_text(&mut self, text: &str, label: &str, now: Instant) {
        self.clipboard.copy(text);
        self.ui_effects.clipboard_flash = Some((format!("{label} {text}"), now + CLIPBOARD_FLASH));
        let _ = self
            .event_sender
            .try_send(Event::ClipboardFlash(CLIPBOARD_FLASH));
    }
    pub fn enter_selected(&mut self) {
        if let Some(tile) = self.board.currently_selected() {
            let name = tile.name.clone();
            self.enter_named(&name);
        }
    }
    /// Enter the folder `name` in the current folder. A file is not entered, and leaves nothing
    /// for Esc to undo: the place to come back to is recorded only when a folder is entered.
    fn enter_named(&mut self, name: &OsStr) {
        if let Some(FileOrFolder::Folder(_)) = self.file_tree.item_in_current_folder(name) {
            self.clear_marks();
            self.board.record_current_index_and_zoom_level();
            self.file_tree.enter_folder(name);
            self.board.reset_zoom_index();
            self.board.reset_selected_index();
            self.list_cursor = None;
            self.cursor_chosen = false;
            self.render_and_update_board();
            // Arriving with the list in hand, start at its top, so the highlight is visible.
            if self.focus() == Focus::List && !self.board.listing().is_empty() {
                self.set_list_cursor(0);
                self.render();
            }
        }
    }
    pub fn go_up(&mut self) {
        self.clear_marks();
        self.cursor_chosen = false;
        let left = self.file_tree.current_folder_names.last().cloned();
        let succeeded = self.file_tree.leave_folder();
        if let Some((index, zoom_level)) = self.board.pop_previous_index_and_zoom_level() {
            if let Some(index) = index {
                self.board.set_selected_index(&index);
            }
            self.board.set_zoom_index(zoom_level);
        }
        self.render_and_update_board();
        // Back in the parent, the list's highlight is on the folder just left.
        if let Some(left) = left {
            self.list_cursor = None;
            if let Some(index) = self
                .board
                .listing()
                .iter()
                .position(|entry| entry.name == left)
                && self.focus() == Focus::List
            {
                self.set_list_cursor(index);
                self.render();
            }
        }
        if !succeeded {
            let _ = self.event_sender.try_send(Event::PathError);
        }
    }
    /// The entry in hand, whichever panel it is in: the list's highlighted entry — which may
    /// have no tile — or the treemap's selected tile.
    pub fn selected_entry(&self) -> Option<libduscape::tiles::FileMetadata> {
        match self.focus() {
            Focus::List => self
                .list_position()
                .map(|index| self.board.listing()[index].clone()),
            Focus::Treemap => {
                self.board
                    .currently_selected()
                    .map(|tile| libduscape::tiles::FileMetadata {
                        name: tile.name.clone(),
                        size: tile.size,
                        descendants: tile.descendants,
                        percentage: tile.percentage,
                        file_type: tile.file_type,
                    })
            }
        }
    }
    /// Describe the entry `entry` of the current folder for deletion.
    fn file_to_delete(&self, entry: libduscape::tiles::FileMetadata) -> FileToDelete {
        FileToDelete::in_current_folder(&self.file_tree, entry)
    }
    /// What `d` would delete: every marked entry, in the order marked, or else the one in hand.
    pub fn get_files_to_delete(&self) -> Vec<FileToDelete> {
        if self.marked.is_empty() {
            return self
                .selected_entry()
                .map(|entry| self.file_to_delete(entry))
                .into_iter()
                .collect();
        }
        let listing = self.board.listing();
        self.marked
            .iter()
            .filter_map(|name| listing.iter().find(|entry| &entry.name == name))
            .map(|entry| self.file_to_delete(entry.clone()))
            .collect()
    }
    fn refuse_metafile(&mut self, files: &[FileToDelete]) -> bool {
        // All or nothing: deleting the rest of a selection and quietly skipping one would leave
        // it unclear what happened.
        let Some(name) = libduscape::delete::refused(files) else {
            return false;
        };
        self.ui_mode = UiMode::ErrorMessage(format!(
            "NTFS metadata belongs to the filesystem and cannot be deleted: {name}"
        ));
        self.render();
        true
    }
    pub fn prompt_file_deletion(&mut self) {
        let files = self.get_files_to_delete();
        if files.is_empty() || self.refuse_metafile(&files) {
            return;
        }
        self.ui_mode = UiMode::DeleteFiles(files);
        self.render();
    }
    pub fn normal_mode(&mut self) {
        self.ui_mode = UiMode::Normal;
        self.render_and_update_board();
    }
    /// Delete `files`, going on past any that fail, then say which did. Only what was actually
    /// removed is counted as freed and taken off the board.
    pub fn delete_files(&mut self, files: &[FileToDelete]) {
        self.ui_effects.deletion_in_progress = true;
        self.render();
        self.ui_effects.deletion_in_progress = false;

        let at = self.list_cursor_index();
        let mut failures = Vec::new();
        for file in files {
            match libduscape::delete::remove(file) {
                Ok(()) => self.remove_file_from_ui(file),
                Err(error) => failures.push((file, error)),
            }
        }
        let deleted = files.len() - failures.len();
        if deleted > 0 {
            self.restart_rescans_under(files);
        }
        self.clear_marks();
        self.cursor_chosen = false;
        self.ui_mode = match failures.as_slice() {
            [] => UiMode::Normal,
            // One entry: the error alone, as it always was.
            [(_, error)] if files.len() == 1 => UiMode::ErrorMessage(format!("{error}")),
            [(file, error), rest @ ..] => {
                let name = file
                    .path_to_file
                    .last()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let more = if rest.is_empty() {
                    String::new()
                } else {
                    format!(" (and {} more)", DisplayCount(rest.len() as u64))
                };
                UiMode::ErrorMessage(format!(
                    "Deleted {} of {}; {name}: {error}{more}",
                    DisplayCount(deleted as u64),
                    DisplayCount(files.len() as u64)
                ))
            }
        };
        self.render_and_update_board();
        if deleted > 0 {
            // The list's highlight moves to what took the deleted entries' place.
            if let Some(at) = at
                && self.focus() == Focus::List
                && !self.board.listing().is_empty()
            {
                self.set_list_cursor(at.min(self.board.listing().len() - 1));
                self.render();
            }
            let _ = self.event_sender.try_send(Event::FileDeleted);
        }
    }
    pub fn zoom_in(&mut self) {
        let current_folder = self.file_tree.get_current_folder();
        self.board.zoom_in(current_folder);
        self.render();
    }
    pub fn zoom_out(&mut self) {
        let current_folder = self.file_tree.get_current_folder();
        self.board.zoom_out(current_folder);
        self.render();
    }
    pub fn reset_zoom(&mut self) {
        let current_folder = self.file_tree.get_current_folder();
        self.board.reset_zoom(current_folder);
        self.render();
    }
    fn remove_file_from_ui(&mut self, file_to_delete: &FileToDelete) {
        self.file_tree.note_freed(file_to_delete.sizes);
        self.file_tree.delete_file(file_to_delete);
        self.board.reset_selected_index();
    }
}

#[cfg(test)]
mod tests;
