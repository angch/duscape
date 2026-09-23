use ::ratatui::Terminal;
use ::ratatui::backend::Backend;
use ::ratatui::layout::Rect;
use ::std::ffi::OsString;
use ::std::path::PathBuf;
use ::std::time::Duration;

use libdiskonaut::FileTree;
use libdiskonaut::tiles::{Area, Board, FileMetadata};

use crate::UiMode;
use crate::config::Keybinds;
use crate::state::UiEffects;
use crate::ui::grid::RectangleGrid;
use crate::ui::modals::{ConfirmBox, ErrorBox, MessageBox, WarningBox};
use crate::ui::side_panel::{FolderDetails, SidePanel, screen_areas};
use crate::ui::title::TitleLine;
use crate::ui::{BottomLine, TermTooSmall};

pub struct FolderInfo<'a> {
    pub path: &'a PathBuf,
    pub size: u128,
    pub num_descendants: u64,
}

/// Title metadata that comes from the app rather than the tree: which size the figures are, and how
/// long the scan took once it finished.
pub struct TitleStatus {
    pub apparent_size: bool,
    pub scan_duration: Option<Duration>,
}

/// What the app knows about the side panel that the tree does not: which panel has the keyboard,
/// which entry the list highlights, and the entry in hand for the status line.
pub struct PanelState {
    pub list_focused: bool,
    pub highlighted: Option<usize>,
    pub selected: Option<FileMetadata>,
    /// Names of the entries in a multi-selection.
    pub marked: Vec<OsString>,
}

/// Everything the frame needs from the app beyond the tree and the board.
pub struct ViewStatus {
    pub title: TitleStatus,
    pub panel: PanelState,
}

pub struct Display<B>
where
    B: Backend,
{
    terminal: Terminal<B>,
}

impl<B> Display<B>
where
    B: Backend,
{
    pub fn new(terminal_backend: B) -> Self {
        let mut terminal = Terminal::new(terminal_backend).expect("failed to create terminal");
        terminal.clear().expect("failed to clear terminal");
        terminal.hide_cursor().expect("failed to hide cursor");
        Display { terminal }
    }
    pub fn size(&self) -> Rect {
        let size = self.terminal.size().expect("could not get terminal size");
        Rect::new(0, 0, size.width, size.height)
    }
    pub fn render(
        &mut self,
        file_tree: &mut FileTree,
        board: &mut Board,
        ui_mode: &UiMode,
        ui_effects: &UiEffects,
        keybinds: &Keybinds,
        status: ViewStatus,
    ) {
        let ViewStatus {
            title:
                TitleStatus {
                    apparent_size,
                    scan_duration,
                },
            panel: panel_state,
        } = status;
        let clipboard_flash = ui_effects.clipboard_flash_at(::std::time::Instant::now());
        self.terminal
            .draw(|f| {
                let full_screen = f.area();
                let current_path = file_tree.get_current_path();
                let current_path_size = file_tree.get_current_folder_size();
                let current_path_descendants = file_tree.get_current_folder().num_descendants;
                let base_path_size = file_tree.get_total_size();
                let base_path_descendants = file_tree.get_total_descendants();
                let current_path_info = FolderInfo {
                    path: &current_path,
                    size: current_path_size,
                    num_descendants: current_path_descendants,
                };
                let path_in_filesystem = &file_tree.path_in_filesystem;
                let base_path_info = FolderInfo {
                    path: path_in_filesystem,
                    size: base_path_size,
                    num_descendants: base_path_descendants,
                };
                let areas = screen_areas(full_screen);
                let chunks = [areas.title, areas.grid, areas.bottom];
                let grid_area = areas.grid;
                board.change_area(&Area {
                    x: grid_area.x,
                    y: grid_area.y,
                    width: grid_area.width,
                    height: grid_area.height,
                });
                if let (Some(panel), false) =
                    (areas.side_panel, matches!(ui_mode, UiMode::ScreenTooSmall))
                {
                    let at_root = file_tree.current_folder_names.is_empty();
                    let scanned = !matches!(ui_mode, UiMode::Loading)
                        && !matches!(ui_mode, UiMode::Exiting { app_loaded: false });
                    let details = FolderDetails {
                        path: &current_path,
                        size: current_path_size,
                        descendants: current_path_descendants,
                        scan_total: (!at_root).then_some(base_path_size),
                        disk: file_tree
                            .volume_used
                            .zip(file_tree.outside_scan())
                            .filter(|_| at_root && scanned),
                    };
                    f.render_widget(
                        SidePanel::new(
                            details,
                            board.listing(),
                            panel_state.highlighted,
                            &board.tiles,
                        )
                        .focused(panel_state.list_focused)
                        .marked(&panel_state.marked),
                        panel,
                    );
                }
                match ui_mode {
                    UiMode::Loading => {
                        f.render_widget(
                            TitleLine::new(
                                base_path_info,
                                current_path_info,
                                file_tree.space_freed,
                            )
                            .apparent_size(apparent_size)
                            .scan_duration(scan_duration)
                            .progress_indicator(ui_effects.loading_progress_indicator)
                            .path_error(ui_effects.current_path_is_red)
                            .read_errors(file_tree.failed_to_read)
                            .outside_scan(file_tree.outside_scan())
                            .zoom_level(board.zoom_level)
                            .clipboard_flash(clipboard_flash)
                            .show_loading(),
                            chunks[0],
                        );
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(
                            BottomLine::new(keybinds)
                                .currently_selected(panel_state.selected.as_ref())
                                .switch_panel_hint(areas.side_panel.is_some())
                                .last_read_path(ui_effects.last_read_path.as_ref())
                                .hide_delete()
                                .hide_small_files_legend(
                                    board.unrenderable_tile_coordinates.is_none(),
                                ),
                            chunks[2],
                        );
                    }
                    UiMode::Normal => {
                        f.render_widget(
                            TitleLine::new(
                                base_path_info,
                                current_path_info,
                                file_tree.space_freed,
                            )
                            .apparent_size(apparent_size)
                            .scan_duration(scan_duration)
                            .path_error(ui_effects.current_path_is_red)
                            .flash_space(ui_effects.flash_space_freed)
                            .clipboard_flash(clipboard_flash)
                            .zoom_level(board.zoom_level)
                            .read_errors(file_tree.failed_to_read)
                            .outside_scan(file_tree.outside_scan()),
                            chunks[0],
                        );
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(
                            BottomLine::new(keybinds)
                                .currently_selected(panel_state.selected.as_ref())
                                .switch_panel_hint(areas.side_panel.is_some())
                                .hide_small_files_legend(
                                    board.unrenderable_tile_coordinates.is_none(),
                                ),
                            chunks[2],
                        );
                    }
                    UiMode::ScreenTooSmall => {
                        f.render_widget(TermTooSmall::new(), full_screen);
                    }
                    UiMode::DeleteFiles(files) => {
                        f.render_widget(
                            TitleLine::new(
                                base_path_info,
                                current_path_info,
                                file_tree.space_freed,
                            )
                            .apparent_size(apparent_size)
                            .scan_duration(scan_duration)
                            .path_error(ui_effects.current_path_is_red)
                            .zoom_level(board.zoom_level)
                            .read_errors(file_tree.failed_to_read)
                            .outside_scan(file_tree.outside_scan()),
                            chunks[0],
                        );
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(
                            BottomLine::new(keybinds)
                                .currently_selected(panel_state.selected.as_ref())
                                .switch_panel_hint(areas.side_panel.is_some())
                                .hide_small_files_legend(
                                    board.unrenderable_tile_coordinates.is_none(),
                                ),
                            chunks[2],
                        );
                        f.render_widget(
                            MessageBox::new(files, ui_effects.deletion_in_progress),
                            full_screen,
                        );
                    }
                    UiMode::ErrorMessage(message) => {
                        f.render_widget(
                            TitleLine::new(
                                base_path_info,
                                current_path_info,
                                file_tree.space_freed,
                            )
                            .apparent_size(apparent_size)
                            .scan_duration(scan_duration)
                            .path_error(ui_effects.current_path_is_red)
                            .flash_space(ui_effects.flash_space_freed)
                            .zoom_level(board.zoom_level)
                            .read_errors(file_tree.failed_to_read)
                            .outside_scan(file_tree.outside_scan()),
                            chunks[0],
                        );
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(
                            BottomLine::new(keybinds)
                                .currently_selected(panel_state.selected.as_ref())
                                .switch_panel_hint(areas.side_panel.is_some())
                                .hide_small_files_legend(
                                    board.unrenderable_tile_coordinates.is_none(),
                                ),
                            chunks[2],
                        );
                        f.render_widget(ErrorBox::new(message), full_screen);
                    }
                    UiMode::Exiting { app_loaded } => {
                        if *app_loaded {
                            // render normal ui mode
                            f.render_widget(
                                TitleLine::new(
                                    base_path_info,
                                    current_path_info,
                                    file_tree.space_freed,
                                )
                                .apparent_size(apparent_size)
                                .scan_duration(scan_duration)
                                .path_error(ui_effects.current_path_is_red)
                                .flash_space(ui_effects.flash_space_freed)
                                .zoom_level(board.zoom_level)
                                .read_errors(file_tree.failed_to_read)
                                .outside_scan(file_tree.outside_scan()),
                                chunks[0],
                            );
                            f.render_widget(
                                BottomLine::new(keybinds)
                                    .currently_selected(panel_state.selected.as_ref())
                                    .switch_panel_hint(areas.side_panel.is_some())
                                    .hide_small_files_legend(
                                        board.unrenderable_tile_coordinates.is_none(),
                                    ),
                                chunks[2],
                            );
                        } else {
                            // render loading ui mode
                            f.render_widget(
                                TitleLine::new(
                                    base_path_info,
                                    current_path_info,
                                    file_tree.space_freed,
                                )
                                .apparent_size(apparent_size)
                                .scan_duration(scan_duration)
                                .progress_indicator(ui_effects.loading_progress_indicator)
                                .path_error(ui_effects.current_path_is_red)
                                .zoom_level(board.zoom_level)
                                .read_errors(file_tree.failed_to_read)
                                .outside_scan(file_tree.outside_scan())
                                .show_loading(),
                                chunks[0],
                            );
                            f.render_widget(
                                BottomLine::new(keybinds)
                                    .currently_selected(panel_state.selected.as_ref())
                                    .switch_panel_hint(areas.side_panel.is_some())
                                    .last_read_path(ui_effects.last_read_path.as_ref())
                                    .hide_delete()
                                    .hide_small_files_legend(
                                        board.unrenderable_tile_coordinates.is_none(),
                                    ),
                                chunks[2],
                            );
                        }
                        // render common widgets
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(ConfirmBox::new(), full_screen);
                    }
                    UiMode::WarningMessage(_) => {
                        f.render_widget(
                            TitleLine::new(
                                base_path_info,
                                current_path_info,
                                file_tree.space_freed,
                            )
                            .apparent_size(apparent_size)
                            .scan_duration(scan_duration)
                            .progress_indicator(ui_effects.loading_progress_indicator)
                            .path_error(ui_effects.current_path_is_red)
                            .read_errors(file_tree.failed_to_read)
                            .outside_scan(file_tree.outside_scan())
                            .show_loading(),
                            chunks[0],
                        );
                        f.render_widget(
                            RectangleGrid::new(
                                &board.tiles,
                                board.unrenderable_tile_coordinates,
                                board.selected_index,
                            )
                            .marked(&panel_state.marked),
                            grid_area,
                        );
                        f.render_widget(
                            BottomLine::new(keybinds)
                                .currently_selected(panel_state.selected.as_ref())
                                .switch_panel_hint(areas.side_panel.is_some())
                                .last_read_path(ui_effects.last_read_path.as_ref())
                                .hide_delete()
                                .hide_small_files_legend(
                                    board.unrenderable_tile_coordinates.is_none(),
                                ),
                            chunks[2],
                        );
                        f.render_widget(WarningBox::new(), full_screen);
                    }
                };
            })
            .expect("failed to draw");
    }
    pub fn clear(&mut self) {
        self.terminal.clear().expect("failed to clear terminal");
        self.terminal.show_cursor().expect("failed to show cursor");
    }
}
