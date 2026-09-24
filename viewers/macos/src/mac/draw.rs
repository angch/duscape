//! Drawing the window's one view: the breadcrumb bar, the list and the entry's details, the
//! treemap, and the status bar. Everything is laid out by `state::Layout`; this only paints it.
//!
//! Chrome uses the system's semantic colours, so it follows light and dark mode and the accent
//! colour. Tiles use `state::tile_color`, the same in either mode.

use ::std::ffi::OsStr;
use ::std::path::Path;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSGradient, NSImage, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStringDrawing, NSTextAlignment,
};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use diskonaut_viewer::state::{Focus, Preview, ROW, Rect, Viewer, tile_color};
use libdiskonaut::tiles::FileType;
use libdiskonaut::{DisplayCount, DisplaySize};

pub fn ns_rect(rect: Rect) -> NSRect {
    NSRect::new(NSPoint::new(rect.x, rect.y), NSSize::new(rect.w, rect.h))
}

fn srgb((r, g, b): (f64, f64, f64), alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha)
}

fn lighter((r, g, b): (f64, f64, f64), by: f64) -> (f64, f64, f64) {
    (r + (1.0 - r) * by, g + (1.0 - g) * by, b + (1.0 - b) * by)
}

fn fill(rect: Rect, color: &NSColor) {
    color.setFill();
    // `NSBezierPath` composites source-over, so translucent colours blend; `NSRectFill` copies.
    NSBezierPath::fillRect(ns_rect(rect));
}

fn stroke(rect: Rect, color: &NSColor, width: f64) {
    color.setStroke();
    let path = NSBezierPath::bezierPathWithRect(ns_rect(rect.inset(width / 2.0, width / 2.0)));
    path.setLineWidth(width);
    path.stroke();
}

fn rounded(rect: Rect, radius: f64, color: &NSColor) {
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(rect), radius, radius).fill();
}

/// Text attributes: a font, a colour, an alignment and where to truncate.
struct Pen(Retained<NSDictionary<NSAttributedStringKey, AnyObject>>);

impl Pen {
    fn new(font: &NSFont, color: &NSColor, align: NSTextAlignment, cut: NSLineBreakMode) -> Pen {
        let style = NSMutableParagraphStyle::new();
        style.setAlignment(align);
        style.setLineBreakMode(cut);
        // SAFETY: the keys are AppKit's own attribute names, each with a value of its type.
        let keys = unsafe {
            [
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
            ]
        };
        let values: [&AnyObject; 3] = [font, color, &style];
        Pen(NSDictionary::from_slices(&keys, &values))
    }

    fn draw(&self, text: &str, rect: Rect) {
        if text.is_empty() || rect.w <= 0.0 {
            return;
        }
        // SAFETY: the dictionary holds text attributes of the right types (see `new`).
        unsafe { NSString::from_str(text).drawInRect_withAttributes(ns_rect(rect), Some(&self.0)) };
    }

    fn width(&self, text: &str) -> f64 {
        // SAFETY: as for `draw`.
        unsafe { NSString::from_str(text).sizeWithAttributes(Some(&self.0)) }.width
    }
}

fn system(size: f64, weight: f64) -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(size, weight)
}

/// The pens a frame uses, made once per frame.
struct Pens {
    label: Pen,
    secondary: Pen,
    secondary_right: Pen,
    strong: Pen,
    tile_name: Pen,
    tile_size: Pen,
    row: Pen,
    row_right: Pen,
    row_selected: Pen,
    row_selected_right: Pen,
    mono: Pen,
    center: Pen,
}

impl Pens {
    fn new() -> Pens {
        let (tail, middle) = (
            NSLineBreakMode::ByTruncatingTail,
            NSLineBreakMode::ByTruncatingMiddle,
        );
        let (left, right) = (NSTextAlignment::Left, NSTextAlignment::Right);
        // SAFETY: reading AppKit's font weight constants.
        let (regular, medium, semibold) = unsafe {
            (
                NSFontWeightRegular,
                NSFontWeightMedium,
                NSFontWeightSemibold,
            )
        };
        let body = system(12.0, regular);
        let small = system(11.0, regular);
        let white = NSColor::whiteColor();
        let selected_text = NSColor::alternateSelectedControlTextColor();
        Pens {
            label: Pen::new(&body, &NSColor::labelColor(), left, middle),
            secondary: Pen::new(&small, &NSColor::secondaryLabelColor(), left, tail),
            secondary_right: Pen::new(&small, &NSColor::secondaryLabelColor(), right, tail),
            strong: Pen::new(
                &system(13.0, semibold),
                &NSColor::labelColor(),
                left,
                middle,
            ),
            tile_name: Pen::new(&system(11.0, medium), &white, left, middle),
            tile_size: Pen::new(
                &system(10.0, regular),
                &white.colorWithAlphaComponent(0.8),
                left,
                tail,
            ),
            row: Pen::new(&body, &NSColor::labelColor(), left, middle),
            row_right: Pen::new(&small, &NSColor::secondaryLabelColor(), right, tail),
            row_selected: Pen::new(&body, &selected_text, left, middle),
            row_selected_right: Pen::new(&small, &selected_text, right, tail),
            mono: Pen::new(
                &NSFont::monospacedSystemFontOfSize_weight(10.5, regular),
                &NSColor::labelColor(),
                left,
                tail,
            ),
            center: Pen::new(
                &system(13.0, regular),
                &NSColor::secondaryLabelColor(),
                NSTextAlignment::Center,
                tail,
            ),
        }
    }
}

/// Everything the view shows, and where the breadcrumbs went, for clicks: each one's rectangle
/// and the depth it goes up to.
pub struct Frame<'a> {
    pub viewer: Option<&'a Viewer>,
    pub image: Option<&'a NSImage>,
    pub key_window: bool,
    pub bounds: Rect,
}

pub fn draw(frame: &Frame) -> Vec<(Rect, usize)> {
    fill(frame.bounds, &NSColor::windowBackgroundColor());
    let pens = Pens::new();
    let Some(viewer) = frame.viewer else {
        let middle = Rect::new(0.0, frame.bounds.h / 2.0 - 10.0, frame.bounds.w, 20.0);
        pens.center.draw(
            "Drop a folder here, or choose File ▸ Scan Folder… (⌘O)",
            middle,
        );
        return Vec::new();
    };
    treemap(viewer, &pens);
    if let Some(list) = viewer.layout.list {
        let line = Rect::new(list.right(), list.y, 1.0, viewer.layout.treemap.h);
        fill(line, &NSColor::separatorColor());
        rows(viewer, list, frame.key_window, &pens);
    }
    if let Some(info) = viewer.layout.info {
        details(viewer, info, frame.image, &pens);
    }
    status(viewer, &pens);
    path_bar(viewer, &pens)
}

fn treemap(viewer: &Viewer, pens: &Pens) {
    let layout = &viewer.layout;
    let area = layout.treemap;
    fill(area, &srgb((0.11, 0.11, 0.12), 1.0));
    if viewer.board.tiles.is_empty() {
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "Empty folder"
        };
        let middle = Rect::new(area.x, area.y + area.h / 2.0 - 10.0, area.w, 20.0);
        pens.center.draw(words, middle);
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
        if let Some(gradient) = NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &srgb(lighter(color, 0.18), 1.0),
            &srgb(color, 1.0),
        ) {
            // In a flipped view, 90° runs top to bottom: lit from above.
            gradient.drawInRect_angle(ns_rect(rect), 90.0);
        }
        if hover == Some(tile.name.as_os_str()) {
            fill(rect, &NSColor::whiteColor().colorWithAlphaComponent(0.14));
        }
        if viewer.is_marked(&tile.name) {
            fill(rect, &srgb((1.0, 0.84, 0.04), 0.30));
            stroke(rect, &srgb((1.0, 0.84, 0.04), 0.9), 1.5);
        }
        if rect.w >= 36.0 && rect.h >= 15.0 {
            let name = tile.name.to_string_lossy();
            let top = if rect.h >= 30.0 {
                rect.y + (rect.h / 2.0 - 15.0).clamp(1.0, 4.0)
            } else {
                rect.y + (rect.h - 15.0) / 2.0
            };
            pens.tile_name
                .draw(&name, Rect::new(rect.x + 4.0, top, rect.w - 8.0, 15.0));
            if rect.h >= 30.0 {
                let size = DisplaySize(tile.size as f64).to_string();
                pens.tile_size.draw(
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
        fill(rect, &srgb((0.30, 0.30, 0.32), 1.0));
        if rect.w >= 50.0 && rect.h >= 15.0 {
            pens.tile_size.draw(
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
        stroke(rect, &srgb((0.0, 0.0, 0.0), 0.6), 1.0);
        let alpha = if viewer.focus == Focus::Treemap {
            1.0
        } else {
            0.7
        };
        stroke(rect.inset(1.0, 1.0), &srgb((1.0, 0.84, 0.04), alpha), 2.5);
    }
}

fn rows(viewer: &Viewer, list: Rect, key_window: bool, pens: &Pens) {
    let listing = viewer.board.listing();
    if listing.is_empty() {
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "Empty folder"
        };
        pens.secondary.draw(
            words,
            Rect::new(list.x + 12.0, list.y + 10.0, list.w - 24.0, 16.0),
        );
        return;
    }
    let emphasised = key_window && viewer.focus == Focus::List;
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
            let color = if emphasised {
                NSColor::selectedContentBackgroundColor()
            } else {
                NSColor::unemphasizedSelectedContentBackgroundColor()
            };
            rounded(pill, 5.0, &color);
        } else if viewer.is_marked(&entry.name) {
            rounded(
                pill,
                5.0,
                &NSColor::controlAccentColor().colorWithAlphaComponent(0.25),
            );
        } else if hover == Some(entry.name.as_os_str()) {
            rounded(
                pill,
                5.0,
                &NSColor::labelColor().colorWithAlphaComponent(0.06),
            );
        }
        let color = tile_color(&entry.name, entry.file_type, index);
        // Its share of the folder, as a bar under the name.
        let bar_w = (pill.w - 30.0) * entry.percentage.clamp(0.0, 1.0);
        fill(
            Rect::new(pill.x + 22.0, pill.bottom() - 3.0, bar_w, 2.0),
            &srgb(color, 0.55),
        );
        let swatch = Rect::new(pill.x + 7.0, rect.y + (ROW - 10.0) / 2.0, 10.0, 10.0);
        if entry.file_type == FileType::Folder {
            rounded(swatch, 2.5, &srgb(color, 1.0));
        } else {
            rounded(swatch, 5.0, &srgb(color, 1.0));
        }
        let size = DisplaySize(entry.size as f64).to_string();
        let (name_pen, size_pen) = if is_selected && emphasised {
            (&pens.row_selected, &pens.row_selected_right)
        } else {
            (&pens.row, &pens.row_right)
        };
        let size_w = 70.0;
        let text_y = rect.y + (ROW - 16.0) / 2.0;
        name_pen.draw(
            &entry.name.to_string_lossy(),
            Rect::new(pill.x + 22.0, text_y, pill.w - 30.0 - size_w, 16.0),
        );
        size_pen.draw(
            &size,
            Rect::new(pill.right() - 8.0 - size_w, text_y + 1.0, size_w, 16.0),
        );
    }
}

fn details(viewer: &Viewer, info: Rect, image: Option<&NSImage>, pens: &Pens) {
    fill(
        Rect::new(info.x, info.y, info.w, 1.0),
        &NSColor::separatorColor(),
    );
    let inner = info.inset(12.0, 10.0);
    let Some(entry) = viewer.selected_entry() else {
        return;
    };
    pens.strong.draw(
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
    pens.secondary
        .draw(&summary, Rect::new(inner.x, inner.y + 20.0, inner.w, 15.0));
    let body = Rect::new(inner.x, inner.y + 42.0, inner.w, (inner.h - 42.0).max(0.0));
    if body.h < 20.0 {
        return;
    }
    match &viewer.preview {
        Preview::None => {
            if entry.file_type == FileType::Folder {
                pens.secondary.draw(
                    "Return or double-click to open · ⌘R to rescan",
                    Rect::new(body.x, body.y, body.w, 15.0),
                );
            }
        }
        Preview::Loading => pens
            .secondary
            .draw("…", Rect::new(body.x, body.y, body.w, 15.0)),
        Preview::Info(info) => pens
            .secondary
            .draw(info, Rect::new(body.x, body.y, body.w, 15.0)),
        Preview::Text(lines) => {
            rounded(body, 6.0, &NSColor::textBackgroundColor());
            let text = body.inset(8.0, 6.0);
            for (row, line) in lines.iter().enumerate() {
                let y = text.y + row as f64 * 13.0;
                if y + 13.0 > text.bottom() {
                    break;
                }
                pens.mono.draw(line, Rect::new(text.x, y, text.w, 14.0));
            }
        }
        Preview::Picture(caption) => {
            let room = Rect::new(body.x, body.y, body.w, (body.h - 20.0).max(0.0));
            if let Some(image) = image {
                let size = image.size();
                if size.width > 0.0 && size.height > 0.0 {
                    // Fitted, and never enlarged past its own size.
                    let scale = (room.w / size.width).min(room.h / size.height).min(1.0);
                    let (w, h) = (size.width * scale, size.height * scale);
                    let at = Rect::new(room.x + (room.w - w) / 2.0, room.y, w, h);
                    // SAFETY: no hints are passed, and the rest are plain values.
                    unsafe {
                        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                            ns_rect(at),
                            NSRect::ZERO,
                            objc2_app_kit::NSCompositingOperation::SourceOver,
                            1.0,
                            true,
                            None,
                        );
                    }
                }
            }
            pens.secondary.draw(
                caption,
                Rect::new(body.x, body.bottom() - 16.0, body.w, 15.0),
            );
        }
    }
}

fn status(viewer: &Viewer, pens: &Pens) {
    let bar = viewer.layout.status;
    fill(
        Rect::new(bar.x, bar.y, bar.w, 1.0),
        &NSColor::separatorColor(),
    );
    let (left, right) = viewer.status();
    let text_y = bar.y + (bar.h - 15.0) / 2.0 + 0.5;
    let right_w = pens.secondary_right.width(&right).min(bar.w * 0.6) + 2.0;
    pens.secondary_right.draw(
        &right,
        Rect::new(bar.right() - 12.0 - right_w, text_y, right_w, 15.0),
    );
    pens.secondary.draw(
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
fn path_bar(viewer: &Viewer, pens: &Pens) -> Vec<(Rect, usize)> {
    let bar = viewer.layout.path_bar;
    fill(
        Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0),
        &NSColor::separatorColor(),
    );
    let size = DisplaySize(viewer.tree.get_current_folder_size() as f64).to_string();
    let size_w = pens.secondary_right.width(&size) + 2.0;
    let text_y = bar.y + (bar.h - 16.0) / 2.0;
    pens.secondary_right.draw(
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
    let separator_w = pens.secondary.width(separator);
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
            pen.width(name) + 1.0
        })
        .collect();
    let room = (bar.w - size_w - 36.0).max(0.0);
    let total = |skip: usize| {
        let shown = widths[0] + widths[1 + skip..].iter().sum::<f64>();
        let crumbs = names.len() - skip;
        shown
            + separator_w * (crumbs - 1 + usize::from(skip > 0)) as f64
            + if skip > 0 {
                pens.label.width("…")
            } else {
                0.0
            }
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
                pens.secondary
                    .draw(separator, Rect::new(x, text_y + 1.0, separator_w, 15.0));
                x += separator_w;
                pens.label.draw("…", Rect::new(x, text_y, 20.0, 16.0));
                x += pens.label.width("…");
            }
            continue;
        }
        if depth > 0 {
            pens.secondary
                .draw(separator, Rect::new(x, text_y + 1.0, separator_w, 15.0));
            x += separator_w;
        }
        let w = widths[depth].min((bar.x + room + 12.0 - x).max(0.0));
        let rect = Rect::new(x, text_y, w, 16.0);
        if depth == last {
            pens.strong.draw(name, rect);
        } else {
            pens.label.draw(name, rect);
            crumbs.push((Rect::new(x - 3.0, bar.y, w + 6.0, bar.h), depth));
        }
        x += w;
    }
    crumbs
}

/// `/Users/me/Music` as `~/Music`.
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
