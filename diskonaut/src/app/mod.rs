use ::ratatui::backend::Backend;
use ::std::ffi::OsString;
use ::std::fs;
use ::std::mem::ManuallyDrop;
use ::std::path::PathBuf;
use ::std::sync::mpsc::{Receiver, SyncSender};
use ::std::time::{Duration, Instant};

use ::std::sync::Arc;

use libdiskonaut::tiles::Board;
use libdiskonaut::{DirSummary, FileOrFolder, FileToDelete, FileTree, Folder};

use crate::Event;
use crate::config::Keybinds;
use crate::messages::{Instruction, handle_instructions};
use crate::state::UiEffects;
use crate::ui::Display;

#[derive(Clone)]
pub enum UiMode {
    Loading,
    Normal,
    ScreenTooSmall,
    DeleteFile(FileToDelete),
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
    /// The tile last clicked — its index and name — and when, so that a second click on it soon
    /// after enters it. The name is kept because the board is laid out again when the finished
    /// tree replaces the scan's outline, and the same index can then be a different tile.
    last_click: Option<(usize, OsString, Instant)>,
}

/// Two clicks on one tile within this long are a double click. Terminals pass on presses without
/// the desktop's own double-click setting, so this is the common default.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

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
        let board = Board::new(&Folder::new(&path_in_filesystem));
        let base_folder = Folder::new(&path_in_filesystem);
        let file_tree = ManuallyDrop::new(FileTree::new(base_folder, path_in_filesystem));
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
        }
    }
    pub fn start(&mut self, receiver: Receiver<Instruction>) {
        handle_instructions(self, receiver);
        self.display.clear();
    }
    pub fn render_and_update_board(&mut self) {
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
        self.display.render(
            &mut self.file_tree,
            &mut self.board,
            &self.ui_mode,
            &self.ui_effects,
            &self.keybinds,
            crate::ui::TitleStatus {
                apparent_size: self.show_apparent_size,
                scan_duration: self.scan_duration,
            },
        );
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
        if let Some(file_to_delete) = self.get_file_to_delete() {
            if self.refuse_metafile(&file_to_delete) {
                return;
            }
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
        if !self.board.has_selected_index() {
            self.board.move_to_largest_folder();
        }
        self.enter_selected();
    }
    pub fn move_selected_right(&mut self) {
        self.board.move_selected_right();
        self.render();
    }
    pub fn move_selected_left(&mut self) {
        self.board.move_selected_left();
        self.render();
    }
    pub fn move_selected_down(&mut self) {
        self.board.move_selected_down();
        self.render();
    }
    pub fn move_selected_up(&mut self) {
        self.board.move_selected_up();
        self.render();
    }
    /// A left click at a screen cell: select the tile there, and on a second click of the same
    /// tile within [`DOUBLE_CLICK`], enter it as Enter would. A click on no tile does nothing.
    pub fn click(&mut self, column: u16, row: u16) {
        self.click_at(column, row, Instant::now());
    }
    fn click_at(&mut self, column: u16, row: u16, now: Instant) {
        let Some(index) = self.board.tile_at(column, row) else {
            self.last_click = None;
            return;
        };
        let name = self.board.tiles[index].name.clone();
        let double = self.last_click.take().is_some_and(|(last, last_name, at)| {
            last == index && last_name == name && now.saturating_duration_since(at) <= DOUBLE_CLICK
        }) && self.board.get_selected_index() == Some(index);
        if double {
            self.enter_selected();
        } else {
            self.last_click = Some((index, name, now));
            self.board.set_selected_index(&index);
            self.render();
        }
    }
    pub fn enter_selected(&mut self) {
        self.board.record_current_index_and_zoom_level();
        if let Some(tile) = &self.board.currently_selected() {
            let selected_name = &tile.name;
            if let Some(file_or_folder) = self.file_tree.item_in_current_folder(selected_name) {
                match file_or_folder {
                    FileOrFolder::Folder(_) => {
                        self.file_tree.enter_folder(selected_name);
                        self.board.reset_zoom_index();
                        self.board.reset_selected_index();
                        self.render_and_update_board();
                    }
                    FileOrFolder::File(_) => {} // do not enter if currently_selected is a file
                }
            };
        }
    }
    pub fn go_up(&mut self) {
        let succeeded = self.file_tree.leave_folder();
        if let Some((index, zoom_level)) = self.board.pop_previous_index_and_zoom_level() {
            if let Some(index) = index {
                self.board.set_selected_index(&index);
            }
            self.board.set_zoom_index(zoom_level);
        }
        self.render_and_update_board();
        if !succeeded {
            let _ = self.event_sender.try_send(Event::PathError);
        }
    }
    pub fn get_file_to_delete(&self) -> Option<FileToDelete> {
        let currently_selected = self.board.currently_selected()?;
        let mut path_to_file = self.file_tree.current_folder_names.clone();
        path_to_file.push(currently_selected.name.clone());
        let file_to_delete = FileToDelete {
            path_in_filesystem: self.file_tree.path_in_filesystem.clone(),
            path_to_file,
            file_type: currently_selected.file_type,
            num_descendants: currently_selected.descendants,
            size: currently_selected.size,
        };
        Some(file_to_delete)
    }
    /// NTFS's own files are shown so the space they hold is accounted for, not so they can be
    /// deleted. Say so rather than ask for a confirmation the filesystem would refuse anyway.
    fn refuse_metafile(&mut self, file_to_delete: &FileToDelete) -> bool {
        let refuse = libdiskonaut::scan::ntfs::is_metafile_path(
            &file_to_delete.path_in_filesystem,
            &file_to_delete.path_to_file,
        );
        if refuse {
            self.ui_mode = UiMode::ErrorMessage(
                "NTFS metadata belongs to the filesystem and cannot be deleted".to_string(),
            );
            self.render();
        }
        refuse
    }
    pub fn prompt_file_deletion(&mut self) {
        if let Some(file_to_delete) = self.get_file_to_delete() {
            if self.refuse_metafile(&file_to_delete) {
                return;
            }
            self.ui_mode = UiMode::DeleteFile(file_to_delete);
            self.render();
        }
    }
    pub fn normal_mode(&mut self) {
        self.ui_mode = UiMode::Normal;
        self.render_and_update_board();
    }
    pub fn delete_file(&mut self, file_to_delete: &FileToDelete) {
        self.ui_effects.deletion_in_progress = true;
        self.render();
        self.ui_effects.deletion_in_progress = false;

        let full_path = file_to_delete.full_path();
        match fs::metadata(&full_path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                let file_removed = if file_type.is_dir() {
                    fs::remove_dir_all(&full_path)
                } else {
                    fs::remove_file(&full_path)
                };
                match file_removed {
                    Ok(_) => {
                        self.remove_file_from_ui(file_to_delete);
                        self.ui_mode = UiMode::Normal;
                        self.render_and_update_board();
                        let _ = self.event_sender.try_send(Event::FileDeleted);
                    }
                    Err(msg) => {
                        self.ui_mode = UiMode::ErrorMessage(format!("{}", msg));
                        self.render();
                    }
                };
            }
            Err(msg) => {
                self.ui_mode = UiMode::ErrorMessage(format!("{}", msg));
                self.render();
            }
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
        self.file_tree.space_freed += file_to_delete.size;
        self.file_tree.delete_file(file_to_delete);
        self.board.reset_selected_index();
    }
}

#[cfg(test)]
mod tests;
