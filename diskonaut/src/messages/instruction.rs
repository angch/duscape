use ::std::sync::mpsc::Receiver;

use ::ratatui::backend::Backend;
use ratatui::crossterm::event::Event as BackEvent;

use crate::input::{
    handle_keypress_delete_file_mode, handle_keypress_error_message, handle_keypress_exiting_mode,
    handle_keypress_loading_mode, handle_keypress_normal_mode, handle_keypress_screen_too_small,
    handle_keypress_warning_message,
};
use libdiskonaut::{DirSummary, FileTree};

use crate::{App, UiMode};

pub enum Instruction {
    SetPathToRed,
    ResetCurrentPathColor,
    FlashSpaceFreed,
    UnflashSpaceFreed,
    /// Outlines of scanned directories, for the live view while the tree is built elsewhere.
    AddScannedSummaries(Vec<DirSummary>),
    /// The finished tree, which replaces the live view's outline, and the small files left for
    /// the second pass.
    ScanComplete(Box<FileTree>, libdiskonaut::scan::refine::SmallFiles),
    StartUi,
    ToggleScanningVisualIndicator,
    RenderAndUpdateBoard,
    Render,
    ResetUiMode,
    Keypress(BackEvent),
    /// A preview has been read, for the request of this generation.
    PreviewReady(u64, crate::preview::Preview),
    /// The rescan started under this id has finished.
    Rescanned(u64, crate::rescan::Outcome),
    /// Second-pass findings for the refine of this generation, and how many files are left to
    /// probe; `None` once it has finished.
    Refined(u64, Vec<libdiskonaut::scan::refine::Found>, Option<usize>),
    /// Time for the help line to scroll, if nobody is pressing keys.
    Tick,
}

pub fn handle_instructions<B>(app: &mut App<B>, receiver: Receiver<Instruction>)
where
    B: Backend,
{
    loop {
        let instruction = receiver
            .recv()
            .expect("failed to receive instruction on channel");
        match instruction {
            Instruction::SetPathToRed => {
                app.set_path_to_red();
            }
            Instruction::ResetCurrentPathColor => {
                app.reset_current_path_color();
            }
            Instruction::FlashSpaceFreed => {
                app.flash_space_freed();
            }
            Instruction::UnflashSpaceFreed => {
                app.unflash_space_freed();
            }
            Instruction::AddScannedSummaries(summaries) => {
                app.add_scanned_summaries(summaries);
            }
            Instruction::ScanComplete(tree, small) => {
                app.finish_scan(*tree);
                app.start_refining(small);
            }
            Instruction::Refined(generation, found, left) => {
                app.refined(generation, found, left);
            }
            Instruction::StartUi => {
                app.start_ui();
            }
            Instruction::ToggleScanningVisualIndicator => {
                app.increment_loading_progress_indicator();
            }
            Instruction::RenderAndUpdateBoard => {
                app.render_and_update_board();
            }
            Instruction::Render => {
                app.render();
            }
            Instruction::ResetUiMode => {
                app.reset_ui_mode();
            }
            Instruction::PreviewReady(generation, preview) => {
                app.preview_ready(generation, preview);
            }
            Instruction::Rescanned(id, outcome) => {
                app.rescan_done(id, outcome);
            }
            Instruction::Tick => {
                app.tick();
            }
            Instruction::Keypress(evt) => {
                app.note_input();
                match &app.ui_mode {
                    UiMode::Loading => {
                        handle_keypress_loading_mode(evt, app);
                    }
                    UiMode::Normal => {
                        handle_keypress_normal_mode(evt, app);
                    }
                    UiMode::ScreenTooSmall => {
                        handle_keypress_screen_too_small(evt, app);
                    }
                    UiMode::DeleteFiles(files) => {
                        let files = files.clone();
                        handle_keypress_delete_file_mode(evt, app, files);
                    }
                    UiMode::ErrorMessage(_) => {
                        handle_keypress_error_message(evt, app);
                    }
                    UiMode::Exiting { app_loaded: _ } => {
                        handle_keypress_exiting_mode(evt, app);
                    }
                    UiMode::WarningMessage(_) => {
                        handle_keypress_warning_message(evt, app);
                    }
                }
                if !app.is_running {
                    break;
                }
            }
        }
    }
}
