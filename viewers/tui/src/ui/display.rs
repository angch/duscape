use ::ratatui::Frame;
use ::ratatui::Terminal;
use ::ratatui::backend::Backend;
use ::ratatui::layout::Rect;
use ::std::ffi::OsString;
use ::std::path::PathBuf;
use ::std::time::Duration;

use libduscape::FileTree;
use libduscape::tiles::{Area, Board, FileMetadata};

use crate::UiMode;
use crate::config::Keybinds;
use crate::preview::Preview;
use crate::state::UiEffects;
use crate::ui::grid::RectangleGrid;
use crate::ui::modals::{ConfirmBox, ErrorBox, MessageBox, WarningBox};
use crate::ui::side_panel::{
    DEFAULT_CELL_PIXELS, FolderDetails, PreviewPanel, ScreenAreas, SidePanel, screen_areas,
};
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
    /// Small files the second pass has still to probe, while it runs.
    pub refining: Option<usize>,
}

/// What the app knows about the side panel that the tree does not: which panel has the keyboard,
/// which entry the list highlights, and the entry in hand for the status line.
pub struct PanelState {
    pub list_focused: bool,
    pub highlighted: Option<usize>,
    pub selected: Option<FileMetadata>,
    /// Names of the entries in a multi-selection.
    pub marked: Vec<OsString>,
    /// What the preview below the list shows, and its caption.
    pub preview: Preview,
    pub caption: String,
}

/// Everything the frame needs from the app beyond the tree and the board.
pub struct ViewStatus {
    pub title: TitleStatus,
    pub panel: PanelState,
}

/// What a mode shows of the chrome every mode has — the title, the treemap and the bottom
/// line — beyond what the tree says: which of the title's and the bottom line's extras are on.
struct Chrome {
    /// The scan is running (or was, when the exit was asked for): its progress indicator in
    /// the title, its last path in the bottom line.
    scanning: bool,
    /// The space freed by the last delete flashes in the title.
    flashes_space: bool,
    /// What was copied flashes in the title.
    flashes_clipboard: bool,
    shows_zoom: bool,
}

impl Chrome {
    fn of(ui_mode: &UiMode) -> Self {
        Chrome {
            scanning: matches!(
                ui_mode,
                UiMode::Loading | UiMode::WarningMessage(_) | UiMode::Exiting { app_loaded: false }
            ),
            flashes_space: matches!(
                ui_mode,
                UiMode::Normal | UiMode::ErrorMessage(_) | UiMode::Exiting { app_loaded: true }
            ),
            flashes_clipboard: matches!(ui_mode, UiMode::Loading | UiMode::Normal),
            shows_zoom: !matches!(ui_mode, UiMode::WarningMessage(_)),
        }
    }
}

/// The list beside the treemap and, when there is room, the preview under it.
fn render_side_panel(
    f: &mut Frame,
    areas: &ScreenAreas,
    ui_mode: &UiMode,
    file_tree: &FileTree,
    board: &Board,
    panel_state: &PanelState,
    current: &FolderInfo,
) {
    let Some(panel) = areas.side_panel else {
        return;
    };
    let at_root = file_tree.current_folder_names.is_empty();
    let scanned = !matches!(
        ui_mode,
        UiMode::Loading | UiMode::Exiting { app_loaded: false }
    );
    let details = FolderDetails {
        path: current.path,
        size: current.size,
        descendants: current.num_descendants,
        scan_total: (!at_root).then_some(file_tree.get_total_size()),
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
    if let Some(preview) = areas.preview {
        f.render_widget(
            PreviewPanel::new(&panel_state.caption, &panel_state.preview),
            preview,
        );
    }
}

pub struct Display<B>
where
    B: Backend,
{
    terminal: Terminal<B>,
    /// The terminal's cell size in pixels, from the last frame; it sizes the 16:9 preview.
    cell_pixels: (u16, u16),
}

impl<B> Display<B>
where
    B: Backend,
{
    pub fn new(terminal_backend: B) -> Self {
        let mut terminal = Terminal::new(terminal_backend).expect("failed to create terminal");
        terminal.clear().expect("failed to clear terminal");
        terminal.hide_cursor().expect("failed to hide cursor");
        Display {
            terminal,
            cell_pixels: DEFAULT_CELL_PIXELS,
        }
    }
    /// The cell size in pixels, as the system reports it, else as the terminal said when asked,
    /// else the usual 1:2.
    fn measure_cell_pixels(&mut self) -> (u16, u16) {
        match self.terminal.backend_mut().window_size() {
            Ok(size)
                if size.pixels.width > 0
                    && size.pixels.height > 0
                    && size.columns_rows.width > 0
                    && size.columns_rows.height > 0 =>
            {
                (
                    (size.pixels.width / size.columns_rows.width).max(1),
                    (size.pixels.height / size.columns_rows.height).max(1),
                )
            }
            _ => crate::preview::queried_cell_pixels().unwrap_or(DEFAULT_CELL_PIXELS),
        }
    }
    pub fn cell_pixels(&self) -> (u16, u16) {
        self.cell_pixels
    }
    /// Measure the cell size now, so what is worked out before the next frame uses the real one.
    pub fn refresh_cell_pixels(&mut self) {
        self.cell_pixels = self.measure_cell_pixels();
    }
    /// Where everything goes on the screen as it now is.
    pub fn areas(&self) -> ScreenAreas {
        screen_areas(self.size(), self.cell_pixels)
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
                    refining,
                },
            panel: panel_state,
        } = status;
        let clipboard_flash = ui_effects.clipboard_flash_at(::std::time::Instant::now());
        self.cell_pixels = self.measure_cell_pixels();
        let cell_pixels = self.cell_pixels;
        self.terminal
            .draw(|f| {
                let full_screen = f.area();
                let areas = screen_areas(full_screen, cell_pixels);
                board.change_area(&Area {
                    x: areas.grid.x,
                    y: areas.grid.y,
                    width: areas.grid.width,
                    height: areas.grid.height,
                });
                if matches!(ui_mode, UiMode::ScreenTooSmall) {
                    f.render_widget(TermTooSmall::new(), full_screen);
                    return;
                }
                let current_path = file_tree.get_current_path();
                let current = FolderInfo {
                    path: &current_path,
                    size: file_tree.get_current_folder_size(),
                    num_descendants: file_tree.get_current_folder().num_descendants,
                };
                let base = FolderInfo {
                    path: &file_tree.path_in_filesystem,
                    size: file_tree.get_total_size(),
                    num_descendants: file_tree.get_total_descendants(),
                };
                let chrome = Chrome::of(ui_mode);
                render_side_panel(f, &areas, ui_mode, file_tree, board, &panel_state, &current);
                let mut title =
                    TitleLine::new(base, current, file_tree.space_freed.get(file_tree.shown))
                        .apparent_size(apparent_size)
                        .scan_duration(scan_duration)
                        .refining(refining)
                        .path_error(ui_effects.current_path_is_red)
                        .read_errors(file_tree.failed_to_read)
                        .outside_scan(file_tree.outside_scan());
                if chrome.scanning {
                    title = title
                        .progress_indicator(ui_effects.loading_progress_indicator)
                        .show_loading();
                }
                if chrome.flashes_space {
                    title = title.flash_space(ui_effects.flash_space_freed);
                }
                if chrome.flashes_clipboard {
                    title = title.clipboard_flash(clipboard_flash);
                }
                if chrome.shows_zoom {
                    title = title.zoom_level(board.zoom_level);
                }
                f.render_widget(title, areas.title);
                f.render_widget(
                    RectangleGrid::new(
                        &board.tiles,
                        board.unrenderable_tile_coordinates,
                        board.selected_index,
                    )
                    .marked(&panel_state.marked),
                    areas.grid,
                );
                let mut bottom = BottomLine::new(keybinds, ui_effects)
                    .currently_selected(panel_state.selected.as_ref())
                    .switch_panel_hint(areas.side_panel.is_some())
                    .hide_small_files_legend(board.unrenderable_tile_coordinates.is_none());
                if chrome.scanning {
                    bottom = bottom
                        .last_read_path(ui_effects.last_read_path.as_ref())
                        .scanning();
                }
                f.render_widget(bottom, areas.bottom);
                // The modal over it all, where the mode has one.
                match ui_mode {
                    UiMode::DeleteFiles(files) => f.render_widget(
                        MessageBox::new(files, ui_effects.deletion_in_progress),
                        full_screen,
                    ),
                    UiMode::ErrorMessage(message) => {
                        f.render_widget(ErrorBox::new(message), full_screen);
                    }
                    UiMode::Exiting { .. } => f.render_widget(ConfirmBox::new(), full_screen),
                    UiMode::WarningMessage(_) => f.render_widget(WarningBox::new(), full_screen),
                    UiMode::Loading | UiMode::Normal | UiMode::ScreenTooSmall => {}
                }
            })
            .expect("failed to draw");
    }
    pub fn clear(&mut self) {
        self.terminal.clear().expect("failed to clear terminal");
        self.terminal.show_cursor().expect("failed to show cursor");
    }
}

#[cfg(test)]
impl Display<::ratatui::backend::TestBackend> {
    /// The last frame drawn, as text, one line per row.
    pub fn screen_text(&self) -> Vec<String> {
        let buffer = self.terminal.backend().buffer();
        let area = buffer.area;
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }
}
