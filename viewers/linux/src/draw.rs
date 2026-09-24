//! Painting the window into the canvas: the breadcrumb bar, the list and the entry's details, the
//! treemap, the status bar, and a dialog over it all. Everything is laid out by
//! `state::Layout`; this only paints it, in the same places as the macOS viewer's `draw`.
//!
//! Chrome is dark, like the treemap's background, and the same everywhere: there is no desktop
//! theme to follow without a toolkit. Tiles use `state::tile_color`.

use ::std::ffi::OsStr;
use ::std::path::Path;

use crate::canvas::{Canvas, Color, Rgba};
use crate::font::{Align, Cut, Face, Fonts, Pen};
use diskonaut_viewer::state::{Focus, Preview, ROW, Rect, Viewer, tile_color};
use libdiskonaut::tiles::FileType;
use libdiskonaut::{DisplayCount, DisplaySize};

pub const WINDOW: Color = (0.13, 0.13, 0.14);
const SEPARATOR: Color = (0.30, 0.30, 0.32);
const LABEL: Color = (0.93, 0.93, 0.94);
const SECONDARY: Color = (0.68, 0.68, 0.72);
const ACCENT: Color = (0.24, 0.50, 0.87);
const ACCENT_QUIET: Color = (0.36, 0.36, 0.40);
const TEXT_BACKGROUND: Color = (0.09, 0.09, 0.10);
const TREEMAP: Color = (0.11, 0.11, 0.12);
const SMALL_FILES: Color = (0.30, 0.30, 0.32);
const MARK: Color = (1.0, 0.84, 0.04);
const WHITE: Color = (1.0, 1.0, 1.0);
const BLACK: Color = (0.0, 0.0, 0.0);

fn lighter((r, g, b): Color, by: f64) -> Color {
    (r + (1.0 - r) * by, g + (1.0 - g) * by, b + (1.0 - b) * by)
}

fn pen(face: &Face, size: f64, color: Color, align: Align, cut: Cut) -> Pen<'_> {
    Pen {
        face,
        size,
        color,
        alpha: 1.0,
        align,
        cut,
    }
}

/// The pens a frame uses.
struct Pens<'a> {
    label: Pen<'a>,
    secondary: Pen<'a>,
    secondary_right: Pen<'a>,
    strong: Pen<'a>,
    tile_name: Pen<'a>,
    tile_size: Pen<'a>,
    row: Pen<'a>,
    row_right: Pen<'a>,
    row_selected: Pen<'a>,
    row_selected_right: Pen<'a>,
    mono: Pen<'a>,
    center: Pen<'a>,
}

impl<'a> Pens<'a> {
    fn new(fonts: &'a Fonts) -> Pens<'a> {
        let (left, right) = (Align::Left, Align::Right);
        let (tail, middle) = (Cut::Tail, Cut::Middle);
        Pens {
            label: pen(&fonts.sans, 12.5, LABEL, left, middle),
            secondary: pen(&fonts.sans, 11.5, SECONDARY, left, tail),
            secondary_right: pen(&fonts.sans, 11.5, SECONDARY, right, tail),
            strong: pen(&fonts.bold, 13.0, LABEL, left, middle),
            tile_name: pen(&fonts.bold, 11.0, WHITE, left, middle),
            tile_size: Pen {
                alpha: 0.8,
                ..pen(&fonts.sans, 10.5, WHITE, left, tail)
            },
            row: pen(&fonts.sans, 12.5, LABEL, left, middle),
            row_right: pen(&fonts.sans, 11.5, SECONDARY, right, tail),
            row_selected: pen(&fonts.sans, 12.5, WHITE, left, middle),
            row_selected_right: pen(&fonts.sans, 11.5, WHITE, right, tail),
            mono: pen(&fonts.mono, 10.5, LABEL, left, tail),
            center: pen(&fonts.sans, 13.0, SECONDARY, Align::Center, tail),
        }
    }
}

/// Draw the whole window. Returns the breadcrumbs, for clicks: each one's rectangle and the
/// depth it goes up to.
pub fn frame(
    canvas: &mut Canvas,
    fonts: &Fonts,
    viewer: &Viewer,
    picture: Option<&Rgba>,
    focused: bool,
) -> Vec<(Rect, usize)> {
    canvas.clear(WINDOW);
    let pens = Pens::new(fonts);
    treemap(canvas, viewer, &pens);
    if let Some(list) = viewer.layout.list {
        let line = Rect::new(list.right(), list.y, 1.0, viewer.layout.treemap.h);
        canvas.fill(line, SEPARATOR, 1.0);
        rows(canvas, viewer, list, focused, &pens);
    }
    if let Some(info) = viewer.layout.info {
        details(canvas, viewer, info, picture, &pens);
    }
    status(canvas, viewer, &pens);
    path_bar(canvas, viewer, &pens)
}

fn treemap(canvas: &mut Canvas, viewer: &Viewer, pens: &Pens) {
    let layout = &viewer.layout;
    let area = layout.treemap;
    canvas.fill(area, TREEMAP, 1.0);
    if viewer.board.tiles.is_empty() {
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "Empty folder"
        };
        let middle = Rect::new(area.x, area.y + area.h / 2.0 - 10.0, area.w, 20.0);
        pens.center.draw(canvas, words, middle);
        return;
    }
    let hover = viewer.hover.as_deref();
    for (index, tile) in viewer.board.tiles.iter().enumerate() {
        let rect = layout
            .cells_to_rect(tile.x, tile.y, tile.width, tile.height)
            .inset(0.5, 0.5);
        // Tiles run in listing order after the zoomed-away entries, so this is its row's index,
        // and a folder's tile and its swatch in the list get the same blue.
        let color = tile_color(&tile.name, tile.file_type, index + viewer.board.zoom_level);
        canvas.gradient(rect, lighter(color, 0.18), color);
        if hover == Some(tile.name.as_os_str()) {
            canvas.fill(rect, WHITE, 0.14);
        }
        if viewer.is_marked(&tile.name) {
            canvas.fill(rect, MARK, 0.30);
            canvas.stroke(rect, MARK, 0.9, 1.5);
        }
        if rect.w >= 36.0 && rect.h >= 15.0 {
            let name = tile.name.to_string_lossy();
            let top = if rect.h >= 30.0 {
                rect.y + (rect.h / 2.0 - 15.0).clamp(1.0, 4.0)
            } else {
                rect.y + (rect.h - 15.0) / 2.0
            };
            pens.tile_name.draw(
                canvas,
                &name,
                Rect::new(rect.x + 4.0, top, rect.w - 8.0, 15.0),
            );
            if rect.h >= 30.0 {
                let size = DisplaySize(tile.size as f64).to_string();
                pens.tile_size.draw(
                    canvas,
                    &size,
                    Rect::new(rect.x + 4.0, top + 14.0, rect.w - 8.0, 14.0),
                );
            }
        }
    }
    // Entries too small for a tile of their own, as one marked corner.
    if let Some((sx, sy)) = viewer.board.unrenderable_tile_coordinates {
        let rect = layout
            .cells_to_rect(
                sx,
                sy,
                layout.cols - sx.min(layout.cols),
                layout.rows - sy.min(layout.rows),
            )
            .inset(0.5, 0.5);
        canvas.fill(rect, SMALL_FILES, 1.0);
        if rect.w >= 50.0 && rect.h >= 15.0 {
            pens.tile_size.draw(
                canvas,
                "small files",
                Rect::new(
                    rect.x + 4.0,
                    rect.y + (rect.h - 14.0) / 2.0,
                    rect.w - 8.0,
                    14.0,
                ),
            );
        }
    }
    if let Some(tile) = viewer.board.currently_selected() {
        let rect = layout.cells_to_rect(tile.x, tile.y, tile.width, tile.height);
        canvas.stroke(rect, BLACK, 0.6, 1.0);
        let alpha = if viewer.focus == Focus::Treemap {
            1.0
        } else {
            0.7
        };
        canvas.stroke(rect.inset(1.0, 1.0), MARK, alpha, 2.5);
    }
}

fn rows(canvas: &mut Canvas, viewer: &Viewer, list: Rect, focused: bool, pens: &Pens) {
    let listing = viewer.board.listing();
    if listing.is_empty() {
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "Empty folder"
        };
        pens.secondary.draw(
            canvas,
            words,
            Rect::new(list.x + 12.0, list.y + 10.0, list.w - 24.0, 16.0),
        );
        return;
    }
    let emphasised = focused && viewer.focus == Focus::List;
    let selected = viewer.selected.as_deref();
    let hover = viewer.hover.as_deref();
    let visible = listing
        .iter()
        .enumerate()
        .skip(viewer.list_top)
        .take(viewer.layout.list_rows());
    for (row, (index, entry)) in visible.enumerate() {
        let rect = Rect::new(list.x, list.y + row as f64 * ROW, list.w, ROW);
        let is_selected = selected == Some(entry.name.as_os_str());
        let pill = rect.inset(5.0, 1.0);
        if is_selected {
            let color = if emphasised { ACCENT } else { ACCENT_QUIET };
            canvas.rounded(pill, 5.0, color, 1.0);
        } else if viewer.is_marked(&entry.name) {
            canvas.rounded(pill, 5.0, ACCENT, 0.25);
        } else if hover == Some(entry.name.as_os_str()) {
            canvas.rounded(pill, 5.0, LABEL, 0.06);
        }
        let color = tile_color(&entry.name, entry.file_type, index);
        // Its share of the folder, as a bar under the name.
        let bar_w = (pill.w - 30.0) * entry.percentage.clamp(0.0, 1.0);
        canvas.fill(
            Rect::new(pill.x + 22.0, pill.bottom() - 3.0, bar_w, 2.0),
            color,
            0.55,
        );
        let swatch = Rect::new(pill.x + 7.0, rect.y + (ROW - 10.0) / 2.0, 10.0, 10.0);
        let radius = if entry.file_type == FileType::Folder {
            2.5
        } else {
            5.0
        };
        canvas.rounded(swatch, radius, color, 1.0);
        let size = DisplaySize(entry.size as f64).to_string();
        let (name_pen, size_pen) = if is_selected && emphasised {
            (&pens.row_selected, &pens.row_selected_right)
        } else {
            (&pens.row, &pens.row_right)
        };
        let size_w = 70.0;
        let text_y = rect.y + (ROW - 16.0) / 2.0;
        name_pen.draw(
            canvas,
            &entry.name.to_string_lossy(),
            Rect::new(pill.x + 22.0, text_y, pill.w - 30.0 - size_w, 16.0),
        );
        size_pen.draw(
            canvas,
            &size,
            Rect::new(pill.right() - 8.0 - size_w, text_y + 1.0, size_w, 16.0),
        );
    }
}

fn details(canvas: &mut Canvas, viewer: &Viewer, info: Rect, picture: Option<&Rgba>, pens: &Pens) {
    canvas.fill(Rect::new(info.x, info.y, info.w, 1.0), SEPARATOR, 1.0);
    let inner = info.inset(12.0, 10.0);
    let Some(entry) = viewer.selected_entry() else {
        return;
    };
    pens.strong.draw(
        canvas,
        &entry.name.to_string_lossy(),
        Rect::new(inner.x, inner.y, inner.w, 18.0),
    );
    let summary = match (entry.file_type, entry.descendants) {
        (FileType::Folder, Some(count)) => format!(
            "Folder · {} items · {} · {:.1}%",
            DisplayCount(count),
            DisplaySize(entry.size as f64),
            entry.percentage * 100.0
        ),
        _ => format!(
            "{} · {:.1}%",
            DisplaySize(entry.size as f64),
            entry.percentage * 100.0
        ),
    };
    pens.secondary.draw(
        canvas,
        &summary,
        Rect::new(inner.x, inner.y + 20.0, inner.w, 15.0),
    );
    let body = Rect::new(inner.x, inner.y + 42.0, inner.w, (inner.h - 42.0).max(0.0));
    if body.h < 20.0 {
        return;
    }
    let line = Rect::new(body.x, body.y, body.w, 15.0);
    match &viewer.preview {
        Preview::None => {
            if entry.file_type == FileType::Folder {
                pens.secondary
                    .draw(canvas, "Enter or double-click to open · r to rescan", line);
            }
        }
        Preview::Loading => pens.secondary.draw(canvas, "…", line),
        Preview::Info(info) => pens.secondary.draw(canvas, info, line),
        Preview::Text(lines) => {
            canvas.rounded(body, 6.0, TEXT_BACKGROUND, 1.0);
            let text = body.inset(8.0, 6.0);
            for (row, line) in lines.iter().enumerate() {
                let y = text.y + row as f64 * 13.0;
                if y + 13.0 > text.bottom() {
                    break;
                }
                pens.mono
                    .draw(canvas, line, Rect::new(text.x, y, text.w, 14.0));
            }
        }
        Preview::Picture(caption) => {
            let room = Rect::new(body.x, body.y, body.w, (body.h - 20.0).max(0.0));
            if let Some(picture) = picture {
                canvas.blit(picture, room);
            }
            pens.secondary.draw(
                canvas,
                caption,
                Rect::new(body.x, body.bottom() - 16.0, body.w, 15.0),
            );
        }
    }
}

fn status(canvas: &mut Canvas, viewer: &Viewer, pens: &Pens) {
    let bar = viewer.layout.status;
    canvas.fill(Rect::new(bar.x, bar.y, bar.w, 1.0), SEPARATOR, 1.0);
    let (left, right) = viewer.status();
    let text_y = bar.y + (bar.h - 15.0) / 2.0 + 0.5;
    let right_w = pens.secondary_right.width(canvas, &right).min(bar.w * 0.6) + 2.0;
    pens.secondary_right.draw(
        canvas,
        &right,
        Rect::new(bar.right() - 12.0 - right_w, text_y, right_w, 15.0),
    );
    pens.secondary.draw(
        canvas,
        &left,
        Rect::new(
            bar.x + 12.0,
            text_y,
            (bar.w - right_w - 36.0).max(0.0),
            15.0,
        ),
    );
}

/// The breadcrumbs: the scan's root, then each folder down to the one shown. Every one but the
/// last is a way back up. When they do not fit, the ones nearest the root after it give way
/// to "…".
fn path_bar(canvas: &mut Canvas, viewer: &Viewer, pens: &Pens) -> Vec<(Rect, usize)> {
    let bar = viewer.layout.path_bar;
    canvas.fill(
        Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0),
        SEPARATOR,
        1.0,
    );
    let size = DisplaySize(viewer.tree.get_current_folder_size() as f64).to_string();
    let size_w = pens.secondary_right.width(canvas, &size) + 2.0;
    let text_y = bar.y + (bar.h - 16.0) / 2.0;
    pens.secondary_right.draw(
        canvas,
        &size,
        Rect::new(bar.right() - 12.0 - size_w, text_y + 1.0, size_w, 15.0),
    );

    let mut names: Vec<String> = vec![abbreviate_home(viewer.root())];
    names.extend(
        viewer
            .tree
            .current_folder_names
            .iter()
            .map(|name: &::std::ffi::OsString| OsStr::to_string_lossy(name).into_owned()),
    );
    let separator = "  ›  ";
    let separator_w = pens.secondary.width(canvas, separator);
    let ellipsis_w = pens.label.width(canvas, "…");
    let last = names.len() - 1;
    // The folder shown is drawn bold, and measured so.
    let widths: Vec<f64> = names
        .iter()
        .enumerate()
        .map(|(depth, name)| {
            let pen = if depth == last {
                &pens.strong
            } else {
                &pens.label
            };
            pen.width(canvas, name) + 1.0
        })
        .collect();
    let room = (bar.w - size_w - 36.0).max(0.0);
    let total = |skip: usize| {
        let shown = widths[0] + widths[1 + skip..].iter().sum::<f64>();
        let crumbs = names.len() - skip;
        shown
            + separator_w * (crumbs - 1 + usize::from(skip > 0)) as f64
            + if skip > 0 { ellipsis_w } else { 0.0 }
    };
    // Crumbs after the root hidden to fit, never the one shown.
    let mut skip = 0;
    while skip + 2 < names.len() && total(skip) > room {
        skip += 1;
    }
    let mut x = bar.x + 12.0;
    let mut crumbs = Vec::new();
    for (depth, name) in names.iter().enumerate() {
        if depth > 0 && depth <= skip {
            if depth == 1 {
                pens.secondary.draw(
                    canvas,
                    separator,
                    Rect::new(x, text_y + 1.0, separator_w, 15.0),
                );
                x += separator_w;
                pens.label
                    .draw(canvas, "…", Rect::new(x, text_y, 20.0, 16.0));
                x += ellipsis_w;
            }
            continue;
        }
        if depth > 0 {
            pens.secondary.draw(
                canvas,
                separator,
                Rect::new(x, text_y + 1.0, separator_w, 15.0),
            );
            x += separator_w;
        }
        let w = widths[depth].min((bar.x + room + 12.0 - x).max(0.0));
        let rect = Rect::new(x, text_y, w, 16.0);
        if depth == last {
            pens.strong.draw(canvas, name, rect);
        } else {
            pens.label.draw(canvas, name, rect);
            crumbs.push((Rect::new(x - 3.0, bar.y, w + 6.0, bar.h), depth));
        }
        x += w;
    }
    crumbs
}

/// `/home/me/Music` as `~/Music`.
fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = ::std::env::var_os("HOME").map(::std::path::PathBuf::from)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

// ---------------------------------------------------------------- dialogs

/// A dialog's buttons, as drawn: each one's rectangle and whether it is the confirming one.
pub type Buttons = Vec<(Rect, bool)>;

/// A question or a notice over the window: `title`, `detail` (wrapped), and the `confirm`
/// button's label if there is a choice. Returns where the buttons went.
pub fn dialog(
    canvas: &mut Canvas,
    fonts: &Fonts,
    bounds: Rect,
    title: &str,
    detail: &str,
    confirm: Option<&str>,
    dangerous: bool,
) -> Buttons {
    canvas.fill(bounds, BLACK, 0.45);
    let pens = Pens::new(fonts);
    let width = (bounds.w - 40.0).clamp(200.0, 480.0);
    let inner_w = width - 40.0;
    let lines = wrap(canvas, &pens.label, detail, inner_w);
    let line_h = 17.0;
    let buttons_h = 32.0;
    let height = 20.0 + 22.0 + 10.0 + lines.len() as f64 * line_h + 16.0 + buttons_h + 20.0;
    let height = height.min(bounds.h - 20.0);
    let box_ = Rect::new(
        bounds.x + (bounds.w - width) / 2.0,
        bounds.y + (bounds.h - height) / 2.0,
        width,
        height,
    );
    canvas.rounded(box_, 10.0, (0.20, 0.20, 0.22), 1.0);
    canvas.stroke(box_, SEPARATOR, 1.0, 1.0);
    let mut y = box_.y + 20.0;
    let strong = pen(&fonts.bold, 14.0, LABEL, Align::Left, Cut::Middle);
    strong.draw(canvas, title, Rect::new(box_.x + 20.0, y, inner_w, 22.0));
    y += 32.0;
    for line in &lines {
        if y + line_h > box_.bottom() - buttons_h - 20.0 {
            break;
        }
        pens.label
            .draw(canvas, line, Rect::new(box_.x + 20.0, y, inner_w, line_h));
        y += line_h;
    }
    let mut buttons = Vec::new();
    let button_y = box_.bottom() - 20.0 - buttons_h;
    let mut right = box_.right() - 20.0;
    let labels: Vec<(&str, bool)> = match confirm {
        Some(verb) => vec![("Cancel", false), (verb, true)],
        None => vec![("OK", true)],
    };
    // Laid out from the right: the confirming button last, where the eye ends up.
    for (label, confirms) in labels.into_iter().rev() {
        let w = pens.label.width(canvas, label) + 32.0;
        let rect = Rect::new(right - w, button_y, w, buttons_h);
        let color = match (confirms, dangerous, confirm.is_some()) {
            (true, true, true) => (0.75, 0.22, 0.20),
            (true, _, _) => ACCENT,
            _ => (0.30, 0.30, 0.33),
        };
        canvas.rounded(rect, 6.0, color, 1.0);
        let text = pen(&fonts.sans, 12.5, WHITE, Align::Center, Cut::Tail);
        text.draw(canvas, label, rect);
        buttons.push((rect, confirms));
        right -= w + 10.0;
    }
    let hint = if confirm.is_some() {
        "Enter or y confirms · Esc or n cancels"
    } else {
        "Enter or Esc closes"
    };
    pens.secondary.draw(
        canvas,
        hint,
        Rect::new(
            box_.x + 20.0,
            button_y,
            (right - box_.x - 30.0).max(0.0),
            buttons_h,
        ),
    );
    buttons
}

/// `text` broken into lines no wider than `width` points, at spaces where it can be, and
/// anywhere in a word that is itself too wide (a long path).
fn wrap(canvas: &Canvas, pen: &Pen, text: &str, width: f64) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if pen.width(canvas, &candidate) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(::std::mem::take(&mut line));
            }
            // A word wider than the line, cut where it stops fitting.
            let mut piece = String::new();
            for ch in word.chars() {
                piece.push(ch);
                if pen.width(canvas, &piece) > width && piece.chars().count() > 1 {
                    piece.pop();
                    lines.push(::std::mem::take(&mut piece));
                    piece.push(ch);
                }
            }
            line = piece;
        }
        lines.push(line);
    }
    lines
}

// ---------------------------------------------------------------- the title bar

/// The buttons of the title bar the app draws itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleButton {
    Minimize,
    Maximize,
    Close,
}

/// A title bar across `bar`, for a compositor that draws none: the title in the middle, three
/// buttons on the right. Returns where the buttons went.
pub fn title_bar(
    canvas: &mut Canvas,
    fonts: &Fonts,
    bar: Rect,
    title: &str,
    focused: bool,
) -> Vec<(Rect, TitleButton)> {
    canvas.fill(bar, (0.18, 0.18, 0.20), 1.0);
    canvas.fill(
        Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0),
        SEPARATOR,
        1.0,
    );
    let mut buttons = Vec::new();
    let size = (bar.h - 10.0).max(12.0);
    let mut right = bar.right() - 8.0;
    for (which, glyph) in [
        (TitleButton::Close, "×"),
        (TitleButton::Maximize, "▢"),
        (TitleButton::Minimize, "–"),
    ] {
        let rect = Rect::new(right - size, bar.y + (bar.h - size) / 2.0, size, size);
        let color = if which == TitleButton::Close {
            (0.75, 0.25, 0.22)
        } else {
            (0.30, 0.30, 0.33)
        };
        canvas.rounded(rect, size / 2.0, color, if focused { 1.0 } else { 0.6 });
        let text = pen(&fonts.sans, 12.0, WHITE, Align::Center, Cut::Tail);
        text.draw(canvas, glyph, rect);
        buttons.push((rect, which));
        right -= size + 6.0;
    }
    let color = if focused { LABEL } else { SECONDARY };
    let title_pen = pen(&fonts.bold, 12.5, color, Align::Center, Cut::Middle);
    let room = (right - bar.x - 16.0).max(0.0);
    // Centred in the bar when it fits, else in what the buttons leave.
    let width = title_pen.width(canvas, title).min(room);
    let x = (bar.x + (bar.w - width) / 2.0)
        .min(right - 8.0 - width)
        .max(bar.x + 8.0);
    title_pen.draw(canvas, title, Rect::new(x, bar.y, width, bar.h));
    buttons
}
