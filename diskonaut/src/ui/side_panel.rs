//! The panel left of the treemap: the current folder's details, then everything in it, largest
//! first.
//!
//! It has no border, so every column and row goes to text; the treemap's own left edge is the
//! divider. [`screen_areas`] decides where it goes and [`entry_at`] which entry a cell shows, and
//! both the renderer and the mouse use them, so a click lands on the row that was drawn.

use ::std::ffi::OsString;
use ::std::ops::Range;
use ::std::path::Path;

use ::ratatui::buffer::Buffer;
use ::ratatui::layout::{Constraint, Direction, Layout, Rect};
use ::ratatui::style::{Color, Modifier, Style};
use ::ratatui::widgets::Widget;
use ::unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use libdiskonaut::format::{DisplayCount, DisplaySize};
use libdiskonaut::tiles::{FileMetadata, FileType, Tile};

use crate::preview::Preview;

/// Terminals narrower than this give the whole width to the treemap: a third of anything less
/// is too narrow for a name and a size, and what it took would cramp the treemap.
pub const SIDE_PANEL_MIN_WIDTH: u16 = 80;

/// Rows above the list: path, size, contents, disk usage.
pub const HEADER_ROWS: u16 = 4;

/// Cell size assumed when the terminal does not report one, in pixels: the common 1:2.
pub const DEFAULT_CELL_PIXELS: (u16, u16) = (8, 16);

/// The preview is a 16:9 picture, whatever the cells' own shape.
const PREVIEW_ASPECT: (u32, u32) = (16, 9);

/// Rows the list keeps however tall the preview would like to be.
const MIN_LIST_ROWS: u16 = 3;

/// Where each part of the screen goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenAreas {
    pub title: Rect,
    /// The list beside the treemap — its header and entries — when the terminal is wide enough
    /// for one.
    pub side_panel: Option<Rect>,
    /// Below the list: a caption row, then a 16:9 area for the preview.
    pub preview: Option<Rect>,
    /// The treemap, borders included.
    pub grid: Rect,
    pub bottom: Rect,
}

/// Split the screen: a title line, the panel and the treemap side by side, and two bottom lines.
/// The panel takes a third of the width, the treemap the rest; the bottom of the panel is the
/// preview, sized for a 16:9 picture with cells of `cell_pixels`, and never more than half.
pub fn screen_areas(full_screen: Rect, cell_pixels: (u16, u16)) -> ScreenAreas {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(0)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(full_screen);
    let middle = chunks[1];
    let (side_panel, treemap) = if full_screen.width >= SIDE_PANEL_MIN_WIDTH {
        let side_width = middle.width / 3;
        (
            Some(Rect {
                width: side_width,
                ..middle
            }),
            Rect {
                x: middle.x + side_width,
                width: middle.width - side_width,
                ..middle
            },
        )
    } else {
        (None, middle)
    };
    // The treemap draws its right and bottom borders one cell inside its area.
    let grid = Rect {
        width: treemap.width.saturating_sub(1),
        height: treemap.height.saturating_sub(1),
        ..treemap
    };
    let (side_panel, preview) = match side_panel.map(|panel| split_preview(panel, cell_pixels)) {
        Some((list, preview)) => (Some(list), preview),
        None => (None, None),
    };
    ScreenAreas {
        title: chunks[0],
        side_panel,
        preview,
        grid,
        bottom: chunks[2],
    }
}

/// Take the preview off the bottom of `panel`: a caption row and a picture area of the panel's
/// width (less its gutter) and 16:9 in pixels. None when that would leave the list too little.
fn split_preview(panel: Rect, cell_pixels: (u16, u16)) -> (Rect, Option<Rect>) {
    let (cell_width, cell_height) = (
        u32::from(cell_pixels.0.max(1)),
        u32::from(cell_pixels.1.max(1)),
    );
    let picture_width = u32::from(panel.width.saturating_sub(1)) * cell_width;
    let picture_rows = (picture_width * PREVIEW_ASPECT.1 / PREVIEW_ASPECT.0)
        .div_ceil(cell_height)
        .min(u32::from(u16::MAX)) as u16;
    let wanted = (picture_rows + 1).min(panel.height / 2);
    if wanted < 3 || panel.height - wanted < HEADER_ROWS + MIN_LIST_ROWS {
        return (panel, None);
    }
    let list = Rect {
        height: panel.height - wanted,
        ..panel
    };
    let preview = Rect {
        y: panel.y + list.height,
        height: wanted,
        ..panel
    };
    (list, Some(preview))
}

/// Which entries a list of `rows` rows shows, keeping `selected` in view. When they do not all
/// fit, the last row says how many are left out, and the window is centred on the selection.
pub fn list_window(entries: usize, selected: Option<usize>, rows: usize) -> Range<usize> {
    if entries <= rows {
        return 0..entries;
    }
    let shown = rows.saturating_sub(1);
    let start = selected
        .map_or(0, |selected| selected.saturating_sub(shown / 2))
        .min(entries - shown);
    start..start + shown
}

/// The index in `listing` of the entry drawn at a screen cell, if the cell is on one.
pub fn entry_at(
    listing: &[FileMetadata],
    selected: Option<usize>,
    panel: Rect,
    column: u16,
    row: u16,
) -> Option<usize> {
    let list_top = panel.y + HEADER_ROWS;
    if !(panel.x..panel.x + panel.width).contains(&column)
        || !(list_top..panel.y + panel.height).contains(&row)
    {
        return None;
    }
    let rows = usize::from(panel.height.saturating_sub(HEADER_ROWS));
    let window = list_window(listing.len(), selected, rows);
    let index = window.start + usize::from(row - list_top);
    window.contains(&index).then_some(index)
}

/// What the header says about the folder on screen.
pub struct FolderDetails<'a> {
    pub path: &'a Path,
    pub size: u128,
    pub descendants: u64,
    /// The whole scan's size, to give the folder's share of it; `None` at the scan root.
    pub scan_total: Option<u128>,
    /// The volume's used space and how much of it the scan did not reach, at a volume root.
    pub disk: Option<(u128, u128)>,
}

pub struct SidePanel<'a> {
    folder: FolderDetails<'a>,
    listing: &'a [FileMetadata],
    selected: Option<usize>,
    /// The treemap's tiles: entries without one are drawn dimmed.
    tiles: &'a [Tile],
    /// Whether the keyboard is on the list: its highlight is then solid, and otherwise only
    /// marks the treemap's selection.
    focused: bool,
    /// Names of the entries in a multi-selection.
    marked: &'a [OsString],
}

impl<'a> SidePanel<'a> {
    pub fn new(
        folder: FolderDetails<'a>,
        listing: &'a [FileMetadata],
        selected: Option<usize>,
        tiles: &'a [Tile],
    ) -> Self {
        SidePanel {
            folder,
            listing,
            selected,
            tiles,
            focused: false,
            marked: &[],
        }
    }
    pub fn marked(mut self, marked: &'a [OsString]) -> Self {
        self.marked = marked;
        self
    }
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

/// Characters a name may not carry onto the screen: a control character would reach the
/// terminal as a command, and the name is only being shown, not copied.
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The longest start of `chars` that fits in `width` columns.
fn take_width(chars: impl Iterator<Item = char>, width: usize) -> Vec<char> {
    let mut used = 0;
    chars
        .take_while(|c| {
            used += UnicodeWidthChar::width(*c).unwrap_or(0);
            used <= width
        })
        .collect()
}

/// `text` cut or padded to exactly `width` columns. What is cut is the middle, marked `…`, so
/// both the start of a name and its extension stay visible: `libsyn….rlib`.
///
/// Measured in columns, not bytes, so that a wide character never pushes the text past `width`
/// — which would make whatever holds the text cut it again.
fn fit(text: &str, width: usize) -> String {
    let mut fitted = if UnicodeWidthStr::width(text) <= width {
        text.to_string()
    } else if width <= 1 {
        take_width(text.chars(), width).into_iter().collect()
    } else {
        let tail_width = (width - 1) / 2;
        let head_width = width - 1 - tail_width;
        let mut tail = take_width(text.chars().rev(), tail_width);
        tail.reverse();
        let mut fitted: String = take_width(text.chars(), head_width).into_iter().collect();
        fitted.push('…');
        fitted.extend(tail);
        fitted
    };
    let used = UnicodeWidthStr::width(fitted.as_str());
    fitted.extend(::std::iter::repeat_n(' ', width.saturating_sub(used)));
    fitted
}

/// The first of `options` that fits in `width` columns, or the last one cut to fit.
fn first_that_fits(options: &[String], width: usize) -> String {
    options
        .iter()
        .find(|option| UnicodeWidthStr::width(option.as_str()) <= width)
        .cloned()
        .unwrap_or_else(|| fit(options.last().map_or("", String::as_str), width))
}

/// A bar `width` cells long, filled to `share` in eighths of a cell.
fn bar(share: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (share.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let mut bar: String = ::std::iter::repeat_n('█', eighths / 8).collect();
    if eighths / 8 < width {
        bar.push(PARTS[eighths % 8]);
    }
    fit(&bar, width)
}

impl Widget for SidePanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // One column is left blank beside the treemap's border.
        let width = usize::from(area.width.saturating_sub(1));
        if width == 0 || area.height == 0 {
            return;
        }
        let x = area.x;
        let line = |buf: &mut Buffer, row: u16, text: &str, style: Style| {
            if row < area.y + area.height {
                buf.set_stringn(x, row, fit(text, width), width, style);
            }
        };

        let heading = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let details = Style::default().fg(Color::Yellow);
        let folder = &self.folder;
        line(
            buf,
            area.y,
            &printable(&folder.path.to_string_lossy()),
            heading,
        );

        let size = DisplaySize(folder.size as f64);
        let files = DisplayCount(folder.descendants);
        let mut size_lines = vec![];
        if let Some(total) = folder.scan_total.filter(|&total| total > 0) {
            let share = folder.size as f64 / total as f64 * 100.0;
            size_lines.push(format!("{size} · {files} files · {share:.1}% of scan"));
            size_lines.push(format!("{size} · {share:.1}% of scan"));
        } else {
            size_lines.push(format!("{size} · {files} files"));
        }
        size_lines.push(format!("{size}"));
        line(
            buf,
            area.y + 1,
            &first_that_fits(&size_lines, width),
            details,
        );

        let folders = self
            .listing
            .iter()
            .filter(|entry| entry.file_type == FileType::Folder)
            .count() as u64;
        let loose = self.listing.len() as u64 - folders;
        line(
            buf,
            area.y + 2,
            &first_that_fits(
                &[
                    format!(
                        "{} folders, {} files here",
                        DisplayCount(folders),
                        DisplayCount(loose)
                    ),
                    format!(
                        "{} dirs, {} files",
                        DisplayCount(folders),
                        DisplayCount(loose)
                    ),
                ],
                width,
            ),
            details,
        );

        if let Some((used, outside)) = folder.disk {
            let used = DisplaySize(used as f64);
            let outside = DisplaySize(outside as f64);
            line(
                buf,
                area.y + 3,
                &first_that_fits(
                    &[
                        format!("disk used {used}, {outside} not scanned"),
                        format!("disk {used}, +{outside}"),
                    ],
                    width,
                ),
                details,
            );
        }

        let rows = usize::from(area.height.saturating_sub(HEADER_ROWS));
        if rows == 0 {
            return;
        }
        // Columns, dropped right to left as the panel narrows: the bar, then the percentage.
        let bar_width = if width >= 36 { 6 } else { 0 };
        let show_share = width >= 30;
        let size_width = 7;
        let share_width = if show_share { 7 } else { 0 };
        let name_width =
            width.saturating_sub(bar_width + usize::from(bar_width > 0) + size_width + share_width);

        let window = list_window(self.listing.len(), self.selected, rows);
        let mut row = area.y + HEADER_ROWS;
        for index in window.clone() {
            let entry = &self.listing[index];
            let mut name = printable(&entry.name.to_string_lossy());
            if entry.file_type == FileType::Folder {
                name.push('/');
            }
            let mut text = String::new();
            if bar_width > 0 {
                text.push_str(&bar(entry.percentage, bar_width));
                text.push(' ');
            }
            text.push_str(&fit(&name, name_width));
            text.push_str(&format!(
                "{:>size_width$}",
                DisplaySize(entry.size as f64).to_string()
            ));
            if show_share {
                text.push_str(&format!(" {:>5.1}%", entry.percentage * 100.0));
            }

            let on_board = self.tiles.iter().any(|tile| tile.name == entry.name);
            let marked = self.marked.contains(&entry.name);
            let cursor = Some(index) == self.selected;
            // No dark gray anywhere: on a black background it is close to unreadable.
            let style = if marked {
                let marked = Style::default().fg(Color::Black).bg(Color::Yellow);
                if cursor {
                    marked.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    marked
                }
            } else if cursor && self.focused {
                // Black on the light cursor bar: magenta on it is hard to read.
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::BOLD)
            } else if cursor {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else if !on_board {
                // Listed but without a tile: a quieter colour than a real tile's, still legible.
                Style::default().fg(Color::Gray)
            } else if entry.file_type == FileType::Folder {
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            line(buf, row, &text, style);
            row += 1;
        }
        let hidden = self.listing.len() - window.len();
        if hidden > 0 {
            line(
                buf,
                row,
                &format!("… {} more", DisplayCount(hidden as u64)),
                Style::default().fg(Color::Gray),
            );
        }
    }
}

/// `text` cut at the end to at most `width` columns: for preview lines, whose start matters.
fn cut_end(text: &str, width: usize) -> String {
    take_width(text.chars(), width).into_iter().collect()
}

/// The area below the list: a caption naming what is previewed, then the preview. A picture is
/// drawn over the blank area by the terminal itself, after the frame (see `preview::Graphics`).
pub struct PreviewPanel<'a> {
    caption: &'a str,
    preview: &'a Preview,
}

impl<'a> PreviewPanel<'a> {
    pub fn new(caption: &'a str, preview: &'a Preview) -> Self {
        PreviewPanel { caption, preview }
    }
}

/// Where the picture goes in a preview area: below the caption, clear of the gutter.
pub fn picture_area(preview: Rect) -> Rect {
    Rect {
        y: preview.y + 1,
        height: preview.height.saturating_sub(1),
        width: preview.width.saturating_sub(1),
        ..preview
    }
}

impl Widget for PreviewPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = usize::from(area.width.saturating_sub(1));
        if width == 0 || area.height == 0 {
            return;
        }
        // Clear it: a picture shows only through cells with nothing in them.
        for y in area.y..area.y + area.height {
            buf.set_stringn(area.x, y, " ".repeat(width), width, Style::default());
        }
        let caption = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        buf.set_stringn(area.x, area.y, fit(self.caption, width), width, caption);
        let body = picture_area(area);
        let lines: Vec<String> = match self.preview {
            Preview::None | Preview::Image(_) => Vec::new(),
            Preview::Loading => vec!["…".to_string()],
            Preview::Info(info) => vec![printable(info)],
            Preview::Text(lines) => lines.clone(),
        };
        for (row, line) in (body.y..body.y + body.height).zip(&lines) {
            buf.set_stringn(area.x, row, cut_end(line, width), width, Style::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;
    use ::std::path::Path;

    use ::ratatui::buffer::Buffer;
    use ::ratatui::layout::Rect;
    use ::ratatui::widgets::Widget;
    use libdiskonaut::tiles::{FileMetadata, FileType};

    use super::{
        DEFAULT_CELL_PIXELS, FolderDetails, HEADER_ROWS, SIDE_PANEL_MIN_WIDTH, SidePanel, bar,
        entry_at, fit, list_window, screen_areas,
    };

    #[test]
    fn a_wide_screen_gives_a_third_to_the_panel() {
        let areas = screen_areas(Rect::new(0, 0, 120, 30), DEFAULT_CELL_PIXELS);
        assert_eq!(areas.title, Rect::new(0, 0, 120, 1));
        // 39 columns of 8px is 312px; 16:9 of that is 175px, 11 rows of 16px, plus a caption.
        assert_eq!(areas.side_panel, Some(Rect::new(0, 1, 40, 15)));
        assert_eq!(areas.preview, Some(Rect::new(0, 16, 40, 12)));
        assert_eq!(areas.grid, Rect::new(40, 1, 79, 26), "borders inside");
        assert_eq!(areas.bottom, Rect::new(0, 28, 120, 2));
    }

    /// The picture area is 16:9 in pixels, so squarer cells give it fewer rows; it never takes
    /// more than half the panel, and it gives way when the list would be left too little.
    #[test]
    fn the_preview_is_sixteen_by_nine_in_pixels() {
        let panel_height = |cells| {
            screen_areas(Rect::new(0, 0, 120, 30), cells)
                .preview
                .map(|preview| preview.height)
        };
        assert_eq!(panel_height((8, 16)), Some(12));
        assert_eq!(panel_height((10, 20)), Some(12), "same shape, same rows");
        assert_eq!(
            panel_height((16, 16)),
            Some(13),
            "square cells: capped at half of 27"
        );
        assert_eq!(panel_height((12, 16)), Some(13));
        assert_eq!(
            panel_height((4, 16)),
            Some(7),
            "tall thin cells: fewer rows"
        );
        let short = screen_areas(Rect::new(0, 0, 120, 12), DEFAULT_CELL_PIXELS);
        assert_eq!(short.preview, None, "too short to spare any");
        assert_eq!(
            short.side_panel.map(|panel| panel.height),
            Some(10),
            "all of it to the list"
        );
    }

    #[test]
    fn a_narrow_screen_gives_everything_to_the_treemap() {
        let areas = screen_areas(
            Rect::new(0, 0, SIDE_PANEL_MIN_WIDTH - 1, 24),
            DEFAULT_CELL_PIXELS,
        );
        assert_eq!(areas.side_panel, None);
        assert_eq!(areas.grid.x, 0);
        assert_eq!(areas.grid.width, SIDE_PANEL_MIN_WIDTH - 2);
    }

    #[test]
    fn the_list_window_keeps_the_selection_in_view() {
        assert_eq!(list_window(5, None, 10), 0..5, "everything fits");
        assert_eq!(list_window(5, Some(4), 5), 0..5, "exactly fits");
        // Too many: one row goes to "… N more", and the selection is centred.
        assert_eq!(list_window(100, None, 10), 0..9);
        assert_eq!(list_window(100, Some(3), 10), 0..9);
        assert_eq!(list_window(100, Some(50), 10), 46..55);
        assert_eq!(
            list_window(100, Some(99), 10),
            91..100,
            "clamped at the end"
        );
        assert_eq!(list_window(100, Some(0), 0), 0..0, "no rows at all");
    }

    fn listing(count: usize) -> Vec<FileMetadata> {
        (0..count)
            .map(|index| FileMetadata {
                name: OsString::from(format!("entry{index:02}")),
                size: 1000 * (count - index) as u128,
                descendants: (index % 2 == 0).then_some(3),
                percentage: 1.0 / count as f64,
                file_type: if index % 2 == 0 {
                    FileType::Folder
                } else {
                    FileType::File
                },
            })
            .collect()
    }

    #[test]
    fn a_cell_maps_to_the_entry_drawn_there() {
        let entries = listing(30);
        let panel = Rect::new(0, 1, 40, 14);
        let top = panel.y + HEADER_ROWS;
        assert_eq!(entry_at(&entries, None, panel, 5, panel.y), None, "header");
        assert_eq!(entry_at(&entries, None, panel, 5, top), Some(0));
        assert_eq!(entry_at(&entries, None, panel, 5, top + 3), Some(3));
        // Ten rows: nine entries and the "more" line, which is not an entry.
        assert_eq!(
            entry_at(&entries, None, panel, 5, top + 9),
            None,
            "more line"
        );
        assert_eq!(
            entry_at(&entries, None, panel, 40, top),
            None,
            "right of panel"
        );
        // Scrolled to keep entry 20 in view, the first row is a later entry.
        assert_eq!(entry_at(&entries, Some(20), panel, 5, top), Some(16));
    }

    fn rendered(panel: SidePanel<'_>, area: Rect) -> Vec<String> {
        let mut buf = Buffer::empty(area);
        panel.render(area, &mut buf);
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn the_panel_shows_the_folder_then_its_entries() {
        let entries = listing(30);
        let area = Rect::new(0, 0, 40, 14);
        let details = FolderDetails {
            path: Path::new("/data/photos"),
            size: 12 * 1024 * 1024,
            descendants: 12_345,
            scan_total: Some(48 * 1024 * 1024),
            disk: None,
        };
        let lines = rendered(SidePanel::new(details, &entries, Some(20), &[]), area);

        assert!(lines[0].starts_with("/data/photos"), "{lines:#?}");
        assert!(lines[1].contains("12.0M"), "{lines:#?}");
        assert!(lines[1].contains("25.0% of scan"), "{lines:#?}");
        assert!(lines[2].contains("15 folders, 15 files here"), "{lines:#?}");
        let list = &lines[usize::from(HEADER_ROWS)..];
        assert!(
            list[0].contains("entry16/"),
            "folders end in a slash: {lines:#?}"
        );
        assert!(
            list.iter().any(|line| line.contains("entry20/")),
            "selected in view"
        );
        assert!(
            list.iter().any(|line| line.contains("  3.3%")),
            "{lines:#?}"
        );
        assert!(list[9].starts_with("… 21 more"), "{lines:#?}");
    }

    /// The highlight is solid only while the list has the keyboard; otherwise it only marks the
    /// treemap's selection.
    #[test]
    fn the_highlight_shows_which_panel_has_the_keyboard() {
        use ::ratatui::style::Color;
        let entries = listing(3);
        let area = Rect::new(0, 0, 40, 8);
        let details = || FolderDetails {
            path: Path::new("/"),
            size: 1,
            descendants: 3,
            scan_total: None,
            disk: None,
        };
        let background = |focused: bool| {
            let mut buf = Buffer::empty(area);
            SidePanel::new(details(), &entries, Some(1), &[])
                .focused(focused)
                .render(area, &mut buf);
            buf[(10, HEADER_ROWS + 1)].bg
        };
        assert_eq!(background(true), Color::Gray);
        assert_ne!(background(false), Color::Gray);
    }

    /// Marked rows stand out in black on yellow and the cursor is black on light gray. Nothing is
    /// drawn in dark gray, close to unreadable on black, or magenta, hard to read on light gray.
    #[test]
    fn marked_rows_stand_out_and_nothing_is_dark_gray() {
        use ::ratatui::style::Color;
        let entries = listing(12);
        let area = Rect::new(0, 0, 40, 10);
        let details = FolderDetails {
            path: Path::new("/"),
            size: 1,
            descendants: 12,
            scan_total: None,
            disk: None,
        };
        let marked = [entries[2].name.clone()];
        let mut buf = Buffer::empty(area);
        SidePanel::new(details, &entries, Some(0), &[])
            .focused(true)
            .marked(&marked)
            .render(area, &mut buf);
        let cell = &buf[(10, HEADER_ROWS + 2)];
        assert_eq!((cell.fg, cell.bg), (Color::Black, Color::Yellow));
        let cursor = &buf[(10, HEADER_ROWS)];
        assert_eq!(
            (cursor.fg, cursor.bg),
            (Color::Black, Color::Gray),
            "cursor bar"
        );
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                assert_ne!(cell.fg, Color::DarkGray, "dark gray at ({x}, {y})");
                assert_ne!(cell.fg, Color::Magenta, "magenta at ({x}, {y})");
            }
        }
    }

    /// A name holding a control character must not put it on the screen.
    #[test]
    fn control_characters_in_names_are_not_drawn() {
        let mut entries = listing(1);
        entries[0].name = OsString::from("evil\x1b[2Jname");
        let area = Rect::new(0, 0, 40, 6);
        let details = FolderDetails {
            path: Path::new("/"),
            size: 1,
            descendants: 1,
            scan_total: None,
            disk: None,
        };
        let lines = rendered(SidePanel::new(details, &entries, None, &[]), area);
        let row = &lines[usize::from(HEADER_ROWS)];
        assert!(row.contains("evil?[2Jname"), "{row}");
        assert!(!row.contains('\x1b'), "{row:?}");
    }

    /// A cut name keeps its start and its end, and is exactly the width asked for, in columns —
    /// wide characters included.
    #[test]
    fn names_are_cut_in_the_middle_to_the_exact_width() {
        use ::unicode_width::UnicodeWidthStr;
        assert_eq!(fit("short", 8), "short   ");
        assert_eq!(fit("libsyn-2c1d0e9f.rlib", 12), "libsyn….rlib");
        assert_eq!(fit("abcdef", 1), "a");
        assert_eq!(fit("abcdef", 0), "");
        for width in 2..20 {
            let cut = fit("日本語のファイル名.txt", width);
            assert_eq!(UnicodeWidthStr::width(cut.as_str()), width, "{cut:?}");
        }
    }

    #[test]
    fn bars_fill_in_eighths() {
        assert_eq!(bar(0.0, 4), "    ");
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(0.5 + 1.0 / 32.0, 4), "██▏ ");
        assert_eq!(bar(2.0, 2), "██", "clamped");
    }
}
