use ::ratatui::buffer::Buffer;
use ::ratatui::layout::Rect;
use ::ratatui::style::{Color, Style};
use ::ratatui::widgets::Widget;
use ::std::ffi::OsString;

use crate::ui::grid::{draw_rect_on_grid, draw_tile_text_on_grid};
use libdiskonaut::tiles::Tile;

fn draw_small_files_rect_on_grid(buf: &mut Buffer, rect: Rect) {
    for x in rect.x + 1..(rect.x + rect.width) {
        for y in rect.y + 1..(rect.y + rect.height) {
            let cell = &mut buf[(x, y)];
            cell.set_symbol("x");
            cell.set_style(Style::default().bg(Color::White).fg(Color::Black));
        }
    }
    draw_rect_on_grid(buf, (rect.x, rect.y), (rect.width, rect.height));
}

fn draw_empty_folder(buf: &mut Buffer, area: Rect) {
    for x in area.x + 1..area.x + area.width {
        for y in area.y + 1..area.y + area.height {
            let cell = &mut buf[(x, y)];
            cell.set_symbol("█");
            cell.set_style(Style::default().bg(Color::White).fg(Color::Black));
        }
    }
    let empty_folder_line = "Folder is empty";
    let text_length = empty_folder_line.len();
    let text_style = Style::default();
    let text_start_position =
        ((area.width - text_length as u16) as f64 / 2.0).ceil() as u16 + area.x;
    buf.set_string(
        text_start_position,
        (area.height / 2) + area.y - 1,
        empty_folder_line,
        text_style,
    );
    draw_rect_on_grid(buf, (area.x, area.y), (area.width, area.height));
}

/// Whether every cell `draw_rect_on_grid` will touch for this tile is inside the buffer.
///
/// The borders are drawn *on* `x + width` and `y + height`, so those are the last cells used and
/// have to be addressable, not just the interior.
fn fits(buf: &Buffer, tile: &Tile) -> bool {
    let area = buf.area();
    u32::from(tile.x) + u32::from(tile.width) < u32::from(area.x) + u32::from(area.width)
        && u32::from(tile.y) + u32::from(tile.height) < u32::from(area.y) + u32::from(area.height)
}

#[derive(Clone)]
pub struct RectangleGrid<'a> {
    rectangles: &'a [Tile],
    small_files_coordinates: Option<(u16, u16)>,
    selected_rect_index: Option<usize>,
    /// Names of the entries in a multi-selection, drawn marked.
    marked: &'a [OsString],
}

impl<'a> RectangleGrid<'a> {
    pub fn new(
        rectangles: &'a [Tile],
        small_files_coordinates: Option<(u16, u16)>,
        selected_rect_index: Option<usize>,
    ) -> Self {
        RectangleGrid {
            rectangles,
            small_files_coordinates,
            selected_rect_index,
            marked: &[],
        }
    }
    pub fn marked(mut self, marked: &'a [OsString]) -> Self {
        self.marked = marked;
        self
    }
}

impl<'a> Widget for RectangleGrid<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.rectangles.is_empty() {
            draw_empty_folder(buf, area);
        } else {
            for (index, tile) in self.rectangles.iter().enumerate() {
                // Everything below indexes the buffer directly, so a tile reaching past the edge
                // takes the whole app down rather than drawing wrong. The layout is float
                // arithmetic over sizes that do not have to add up — shared blocks make a folder
                // smaller than its contents — so treat "this tile does not fit" as a thing that
                // can happen and skip it, not as an invariant worth crashing over.
                if !fits(buf, tile) {
                    continue;
                }
                let selected = if let Some(selected_rect_index) = self.selected_rect_index {
                    index == selected_rect_index
                } else {
                    false
                };
                let marked = self.marked.contains(&tile.name);
                draw_tile_text_on_grid(buf, tile, selected, marked);
                draw_rect_on_grid(buf, (tile.x, tile.y), (tile.width, tile.height));
            }
        }
        if let Some(coords) = self.small_files_coordinates {
            let (x, y) = coords;
            let width = (area.x + area.width) - x;
            let height = (area.y + area.height) - y;
            let small_files_rect = Rect {
                x,
                y,
                width,
                height,
            };
            draw_small_files_rect_on_grid(buf, small_files_rect);
        }
    }
}
