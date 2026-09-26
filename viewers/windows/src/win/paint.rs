//! Drawing the window with GDI, to an off-screen bitmap blitted once, so nothing flickers. Every
//! rectangle comes from the viewer's [`Layout`] in points and is scaled to pixels here.
//!
//! The colours follow the terminal viewer's: marks are black on yellow, the entry in hand black
//! on grey, and the panel with the keyboard has an accent outline. Tiles take the colours the
//! other desktop viewers give them (`tile_color`), so a file's kind looks the same everywhere.

use ::std::cell::Cell;
use ::std::ffi::OsStr;
use ::std::ptr::null_mut;

use diskonaut_viewer::state::{
    EXPANDER, Focus, LIST_PAD, Layout, Preview, ROW, ROW_INDENT, Rect, describe, tile_color,
};
use libdiskonaut::DisplaySize;
use libdiskonaut::format::without_verbatim_prefix;
use libdiskonaut::tiles::FileType;
use libdiskonaut::tiles::{Row, Tile};

use windows_sys::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FF_DONTCARE, FF_MODERN,
    FIXED_PITCH, FW_NORMAL, FW_SEMIBOLD, FillRect, FrameRect, GetTextExtentPoint32W, HDC, HFONT,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetDIBitsToDevice,
    SetTextColor, TRANSPARENT, VARIABLE_PITCH,
};

use super::{Window, client_rect, preview_parts};
use crate::preview::PREVIEW_BACKGROUND;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

const BACKGROUND: COLORREF = rgb(24, 24, 24);
const BAR: COLORREF = rgb(40, 40, 40);
const PANEL: COLORREF = rgb(
    PREVIEW_BACKGROUND[0],
    PREVIEW_BACKGROUND[1],
    PREVIEW_BACKGROUND[2],
);
const TEXT: COLORREF = rgb(220, 220, 220);
const DIM: COLORREF = rgb(160, 160, 160);
const INK: COLORREF = rgb(0, 0, 0);
const CURSOR: COLORREF = rgb(200, 200, 200);
const MARK: COLORREF = rgb(235, 200, 40);
const ACCENT: COLORREF = rgb(90, 150, 230);
const BORDER: COLORREF = rgb(18, 18, 18);
const CAPTION: COLORREF = rgb(120, 200, 230);

/// A line of text, in points.
const LINE: f64 = 18.0;

/// The fonts the window draws with, made once at its DPI.
pub struct Fonts {
    ui: HFONT,
    bold: HFONT,
    mono: HFONT,
    /// The fonts' height in pixels.
    height: i32,
    /// The monospace font sized down for a hex dump to fit the preview's width: its height in
    /// pixels and the font, made when first needed and remade when the width changes.
    hex: Cell<(i32, HFONT)>,
}

/// The least a hex dump's font is shrunk to, in pixels.
const HEX_MIN_HEIGHT: i32 = 6;

impl Fonts {
    pub fn new(scale: f64) -> Self {
        let height = (15.0 * scale).round() as i32;
        Fonts {
            ui: make_font(
                height,
                FW_NORMAL,
                u32::from(VARIABLE_PITCH) | u32::from(FF_DONTCARE),
                "Segoe UI",
            ),
            bold: make_font(
                height,
                FW_SEMIBOLD,
                u32::from(VARIABLE_PITCH) | u32::from(FF_DONTCARE),
                "Segoe UI",
            ),
            mono: make_font(
                height,
                FW_NORMAL,
                u32::from(FIXED_PITCH) | u32::from(FF_MODERN),
                "Consolas",
            ),
            height,
            hex: Cell::new((0, null_mut())),
        }
    }

    /// The monospace font at which `sample` fits in `width` points — the ordinary one if it
    /// does, else one shrunk to fit, down to `HEX_MIN_HEIGHT` — and the line height to draw
    /// it at.
    fn fitting(&self, canvas: &Canvas, sample: &str, width: f64) -> (HFONT, f64) {
        let full = canvas.width(sample, self.mono);
        if full <= width || full <= 0.0 {
            return (self.mono, LINE);
        }
        let height = ((f64::from(self.height) * width / full).floor() as i32).max(HEX_MIN_HEIGHT);
        let (had, font) = self.hex.get();
        let font = if had == height && !font.is_null() {
            font
        } else {
            if !font.is_null() {
                // SAFETY: made by `make_font`, and not selected into any DC between frames.
                unsafe { DeleteObject(font as _) };
            }
            let font = make_font(
                height,
                FW_NORMAL,
                u32::from(FIXED_PITCH) | u32::from(FF_MODERN),
                "Consolas",
            );
            self.hex.set((height, font));
            font
        };
        (font, LINE * f64::from(height) / f64::from(self.height))
    }
}

/// A GDI font `height` pixels tall.
fn make_font(height: i32, weight: u32, pitch: u32, face: &str) -> HFONT {
    let face = super::wide(face);
    // SAFETY: the face name is NUL-terminated and alive for the call.
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            u32::from(DEFAULT_CHARSET),
            u32::from(OUT_DEFAULT_PRECIS),
            u32::from(CLIP_DEFAULT_PRECIS),
            u32::from(CLEARTYPE_QUALITY),
            pitch,
            face.as_ptr(),
        )
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: each font was made by `CreateFontW` and is deleted once, deselected by now.
        unsafe {
            DeleteObject(self.ui as _);
            DeleteObject(self.bold as _);
            DeleteObject(self.mono as _);
            let (_, hex) = self.hex.get();
            if !hex.is_null() {
                DeleteObject(hex as _);
            }
        }
    }
}

/// A device context to draw into, taking rectangles in points.
struct Canvas {
    dc: HDC,
    scale: f64,
}

impl Canvas {
    fn px(&self, points: f64) -> i32 {
        (points * self.scale).round() as i32
    }

    /// Each edge rounded on its own, so tiles that meet in points meet in pixels.
    fn rect(&self, rect: Rect) -> RECT {
        RECT {
            left: self.px(rect.x),
            top: self.px(rect.y),
            right: self.px(rect.right()),
            bottom: self.px(rect.bottom()),
        }
    }

    fn fill(&self, rect: Rect, color: COLORREF) {
        // SAFETY: the brush is made, used and deleted here.
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(self.dc, &self.rect(rect), brush);
            DeleteObject(brush as _);
        }
    }

    /// A frame `thickness` pixels wide, inside `rect`.
    fn frame(&self, rect: Rect, color: COLORREF, thickness: i32) {
        let mut area = self.rect(rect);
        // SAFETY: the brush is made, used and deleted here.
        unsafe {
            let brush = CreateSolidBrush(color);
            for _ in 0..thickness.max(1) {
                FrameRect(self.dc, &area, brush);
                area.left += 1;
                area.top += 1;
                area.right -= 1;
                area.bottom -= 1;
            }
            DeleteObject(brush as _);
        }
    }

    /// `text` in `rect`, one line, cut short with an ellipsis if it does not fit.
    fn text(&self, rect: Rect, text: &str, color: COLORREF, font: HFONT, right: bool) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() || rect.w <= 0.0 {
            return;
        }
        let mut area = self.rect(rect);
        let align = if right { DT_RIGHT } else { DT_LEFT };
        // SAFETY: `wide` and `area` are alive for the call; the old font is put back.
        unsafe {
            let old = SelectObject(self.dc, font as _);
            SetTextColor(self.dc, color);
            DrawTextW(
                self.dc,
                wide.as_ptr(),
                i32::try_from(wide.len()).unwrap_or(i32::MAX),
                &mut area,
                align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(self.dc, old);
        }
    }

    /// How wide `text` is in `font`, in points.
    fn width(&self, text: &str, font: HFONT) -> f64 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return 0.0;
        }
        let mut size = SIZE { cx: 0, cy: 0 };
        // SAFETY: `wide` and `size` are alive for the call; the old font is put back.
        unsafe {
            let old = SelectObject(self.dc, font as _);
            GetTextExtentPoint32W(
                self.dc,
                wide.as_ptr(),
                i32::try_from(wide.len()).unwrap_or(i32::MAX),
                &mut size,
            );
            SelectObject(self.dc, old);
        }
        f64::from(size.cx) / self.scale
    }
}

/// The colour the shared viewers give a tile, as GDI takes it.
/// A tile's colour, `shade` (1.0 as given, less for darker) of what every desktop viewer gives it.
fn tile_colorref(name: &OsStr, file_type: FileType, index: usize, shade: f64) -> COLORREF {
    let (r, g, b) = tile_color(name, file_type, index);
    let byte = |value: f64| ((value * shade).clamp(0.0, 1.0) * 255.0).round() as u8;
    rgb(byte(r), byte(g), byte(b))
}

/// Draw the whole window. Returns the breadcrumbs, in points, for clicks: each one's rectangle
/// and the depth it goes up to.
pub fn paint(window: &Window, hwnd: HWND) -> Vec<(Rect, usize)> {
    // SAFETY: BeginPaint/EndPaint bracket the paint; the off-screen DC and bitmap are made and
    // freed here, with what was selected into them put back first.
    unsafe {
        let mut ps: PAINTSTRUCT = ::std::mem::zeroed();
        let screen = BeginPaint(hwnd, &mut ps);
        let client = client_rect(hwnd);
        let (width, height) = (
            (client.right - client.left).max(1),
            (client.bottom - client.top).max(1),
        );
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let old_bitmap = SelectObject(dc, bitmap as _);
        SetBkMode(dc, TRANSPARENT as i32);
        let canvas = Canvas {
            dc,
            scale: window.scale,
        };
        let viewer = &window.viewer;
        let layout = &viewer.layout;

        canvas.fill(layout.bounds, BACKGROUND);
        draw_treemap(&canvas, window, layout);
        if let Some(list) = layout.list {
            draw_list(&canvas, window, list);
        }
        if let Some(info) = layout.info {
            draw_preview(&canvas, window, info);
        }
        let crumbs = draw_path_bar(&canvas, window, layout.path_bar);
        draw_status(&canvas, window, layout.status);

        BitBlt(screen, 0, 0, width, height, dc, 0, 0, SRCCOPY);
        SelectObject(dc, old_bitmap);
        DeleteObject(bitmap as _);
        DeleteDC(dc);
        EndPaint(hwnd, &ps);
        crumbs
    }
}

fn draw_treemap(canvas: &Canvas, window: &Window, layout: &Layout) {
    let viewer = &window.viewer;
    let board = &viewer.board;
    let fonts = &window.fonts;
    let pad = 4.0;
    let hover = viewer.hover.as_deref();
    for (index, tile) in board.tiles.iter().enumerate() {
        let rect = layout.cells_to_rect(tile.x, tile.y, tile.width, tile.height);
        let marked = viewer.is_marked(&tile.name);
        canvas.fill(
            rect,
            if marked {
                MARK
            } else {
                tile_colorref(&tile.name, tile.file_type, index + board.zoom_level, 1.0)
            },
        );
        canvas.frame(rect, BORDER, 1);
        if rect.w > 40.0 && rect.h > LINE {
            let ink = if marked { INK } else { rgb(240, 240, 240) };
            draw_tile_label(canvas, fonts, rect, pad, pad / 2.0, tile, ink, fonts.bold);
        }
        // (`hover` is never a name while a nested tile is hovered: `hover_at` sees to that.)
        if hover == Some(tile.name.as_os_str()) && board.get_selected_index() != Some(index) {
            canvas.frame(rect, rgb(170, 170, 170), 1);
        }
    }
    draw_nested(canvas, window, layout);
    // The "small files" corner: from where the board says it starts to the treemap's far corner.
    if let Some((sx, sy)) = board.unrenderable_tile_coordinates {
        let corner = layout.cells_to_rect(
            sx,
            sy,
            layout.cols.saturating_sub(sx),
            layout.rows.saturating_sub(sy),
        );
        canvas.fill(corner, rgb(60, 60, 60));
        canvas.frame(corner, BORDER, 1);
        // The label only where it fits: a sliver of a corner is still drawn, as the terminal
        // viewer keeps its `x`, but half a line of text would read as a glitch.
        if corner.h >= LINE + pad / 2.0 {
            let line = Rect::new(corner.x + pad, corner.y, corner.w - 2.0 * pad, LINE);
            canvas.text(line, "x  small files", DIM, fonts.ui, false);
        }
    }
    if let Some(tile) = board.currently_selected() {
        canvas.frame(
            layout.cells_to_rect(tile.x, tile.y, tile.width, tile.height),
            rgb(255, 255, 255),
            canvas.px(2.0),
        );
    }
    // The row in hand, when it is a tile inside a folder's.
    if let Some(index) = viewer.cursor_nested() {
        let t = &viewer.nested()[index].tile;
        canvas.frame(
            layout.cells_to_rect(t.x, t.y, t.width, t.height),
            rgb(255, 255, 255),
            canvas.px(2.0),
        );
    }
    if board.tiles.is_empty() {
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "This folder is empty"
        };
        let line = Rect::new(
            layout.treemap.x + pad * 4.0,
            layout.treemap.y + pad * 4.0,
            layout.treemap.w - pad * 4.0,
            LINE,
        );
        canvas.text(line, words, DIM, fonts.ui, false);
    }
    if viewer.focus == Focus::Treemap && layout.list.is_some() {
        canvas.frame(layout.treemap, ACCENT, 1);
    }
}

fn draw_list(canvas: &Canvas, window: &Window, list: Rect) {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    canvas.fill(list, PANEL);
    let listing = viewer.rows();
    let cursor = viewer.cursor_row();
    let rows = viewer.layout.list_rows();
    for (shown, row) in listing.iter().enumerate().skip(viewer.list_top).take(rows) {
        let entry = &row.entry;
        let index = shown;
        let shown = shown - viewer.list_top;
        let rect = Rect::new(list.x, list.y + shown as f64 * ROW, list.w, ROW);
        // The tree: each level indented, a folder with its expander before its name.
        let indent = LIST_PAD + row.depth as f64 * ROW_INDENT;
        let marked = row.depth == 0 && viewer.is_marked(&entry.name);
        let in_hand = cursor == Some(index);
        let (background, ink) = if marked {
            (Some(MARK), INK)
        } else if in_hand {
            (Some(CURSOR), INK)
        } else {
            (None, TEXT)
        };
        match background {
            Some(color) => canvas.fill(rect, color),
            None => {
                // How much of its parent this entry is, as a bar behind its name — WizTree's
                // "% of parent" — from where its level starts.
                let share = entry.percentage.clamp(0.0, 1.0);
                let bar = Rect::new(
                    rect.x + indent,
                    rect.y + 1.0,
                    (rect.w - indent) * share,
                    rect.h - 2.0,
                );
                let color = if entry.file_type == FileType::Folder {
                    rgb(40, 58, 84)
                } else {
                    rgb(52, 52, 52)
                };
                canvas.fill(bar, color);
            }
        }
        if viewer.hover_row == Some(index) && background.is_none() {
            canvas.frame(rect, rgb(90, 90, 90), 1);
        }
        draw_row_words(canvas, fonts, row, rect, indent, ink);
    }
    if listing.is_empty() {
        let line = Rect::new(
            list.x + LIST_PAD,
            list.y + LIST_PAD,
            list.w - 2.0 * LIST_PAD,
            ROW,
        );
        let words = if viewer.scanning {
            "Scanning…"
        } else {
            "Empty"
        };
        canvas.text(line, words, DIM, fonts.ui, false);
    }
    if viewer.focus == Focus::List {
        canvas.frame(list, ACCENT, 1);
    }
}

/// The tiles inside the folder tiles — the nesting — parents first, so each level paints
/// over its parent's body and under the parent's label; a level deeper is a shade darker, and
/// a label goes on whatever has the room for one.
fn draw_nested(canvas: &Canvas, window: &Window, layout: &Layout) {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    let pad = 3.0;
    for (index, nested) in viewer.nested().iter().enumerate() {
        let t = &nested.tile;
        let rect = layout.cells_to_rect(t.x, t.y, t.width, t.height);
        // Each level in, the colour is a step darker: the nesting reads as depth.
        let shade = 1.0 - 0.12 * nested.depth.min(4) as f64;
        canvas.fill(rect, tile_colorref(&t.name, t.file_type, index, shade));
        canvas.frame(rect, BORDER, 1);
        if rect.w > 30.0 && rect.h > LINE {
            draw_tile_label(
                canvas,
                fonts,
                rect,
                pad,
                1.0,
                t,
                rgb(235, 235, 235),
                fonts.ui,
            );
        }
        if viewer.hover_nested == Some(index) {
            canvas.frame(rect, rgb(200, 200, 200), 1);
        }
    }
}

/// A tile's label, in `ink`, `pad` in from the sides and `top` down from the top. A folder's is
/// one line, its name (with its `\`) at the left and its size at the right, since its entries
/// take the rest of the tile; a file's name has the whole top line, and its size goes at the
/// bottom right when the tile has a second line, since the name is the longer and the one to
/// read. Beside a name, a size is left out where the name would keep too little room.
#[allow(clippy::too_many_arguments)]
fn draw_tile_label(
    canvas: &Canvas,
    fonts: &Fonts,
    rect: Rect,
    pad: f64,
    top: f64,
    tile: &Tile,
    ink: COLORREF,
    font: HFONT,
) {
    /// The least the name may keep beside the size, in points.
    const NAME_ROOM: f64 = 24.0;
    let is_dir = tile.file_type == FileType::Folder;
    let name = tile.name.to_string_lossy();
    let label = if is_dir {
        format!("{name}\\")
    } else {
        name.into_owned()
    };
    let line = Rect::new(rect.x + pad, rect.y + top, rect.w - 2.0 * pad, LINE);
    let size = DisplaySize(tile.size as f64).to_string();
    let size_width = canvas.width(&size, fonts.ui);
    let beside = line.w - size_width - LIST_PAD >= NAME_ROOM;
    let below = !is_dir && rect.h >= top + 2.0 * LINE + pad;
    if below {
        canvas.text(line, &label, ink, font, false);
        let size_line = Rect::new(line.x, rect.bottom() - pad - LINE, line.w, LINE);
        canvas.text(size_line, &size, ink, fonts.ui, true);
    } else if beside {
        let name_rect = Rect::new(line.x, line.y, line.w - size_width - LIST_PAD, line.h);
        canvas.text(name_rect, &label, ink, font, false);
        let size_rect = Rect::new(line.right() - size_width, line.y, size_width, line.h);
        canvas.text(size_rect, &size, ink, fonts.ui, true);
    } else {
        canvas.text(line, &label, ink, font, false);
    }
}

/// A row's words: a folder's expander, the name, and the size on the right, in `ink`.
fn draw_row_words(
    canvas: &Canvas,
    fonts: &Fonts,
    row: &Row,
    rect: Rect,
    indent: f64,
    ink: COLORREF,
) {
    const SIZE_WIDTH: f64 = 80.0;
    let entry = &row.entry;
    let is_dir = entry.file_type == FileType::Folder;
    if is_dir {
        let glyph = if row.open { "\u{25BE}" } else { "\u{25B8}" };
        canvas.text(
            Rect::new(rect.x + indent, rect.y, EXPANDER, rect.h),
            glyph,
            ink,
            fonts.ui,
            false,
        );
    }
    let name = entry.name.to_string_lossy();
    let label = if is_dir {
        format!("{name}\\")
    } else {
        name.into_owned()
    };
    let pad = indent + EXPANDER;
    let name_rect = Rect::new(
        rect.x + pad,
        rect.y,
        (rect.w - SIZE_WIDTH - pad - LIST_PAD).max(0.0),
        rect.h,
    );
    let font = if is_dir { fonts.bold } else { fonts.ui };
    canvas.text(name_rect, &label, ink, font, false);
    let size_rect = Rect::new(
        rect.right() - SIZE_WIDTH - LIST_PAD,
        rect.y,
        SIZE_WIDTH,
        rect.h,
    );
    canvas.text(
        size_rect,
        &DisplaySize(entry.size as f64).to_string(),
        ink,
        fonts.ui,
        true,
    );
}

fn draw_preview(canvas: &Canvas, window: &Window, info: Rect) {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    let (caption, body) = preview_parts(info);
    canvas.fill(info, PANEL);
    canvas.fill(Rect::new(info.x, info.y, info.w, 1.0), BAR);

    // The caption: what is in hand and how big; how many are marked; what picture it is.
    let words = if viewer.marked.len() > 1 {
        let size: u128 = viewer
            .marked
            .iter()
            .filter_map(|name| viewer.entry_named(name))
            .map(|entry| entry.size)
            .sum();
        format!(
            "{} marked · {}",
            libdiskonaut::DisplayCount(viewer.marked.len() as u64),
            DisplaySize(size as f64)
        )
    } else if let Some(entry) = viewer
        .cursor_entry()
        .map(|row| &row.entry)
        .or_else(|| viewer.selected_entry())
    {
        let mut words = entry.name.to_string_lossy().into_owned();
        if entry.file_type == FileType::Folder {
            words.push('\\');
        }
        words.push_str(&format!(" · {}", DisplaySize(entry.size as f64)));
        match &viewer.preview {
            Preview::Picture(description) => words.push_str(&format!(" · {description}")),
            Preview::Hex(_) => words.push_str(" · binary"),
            _ => {}
        }
        words
    } else {
        String::new()
    };
    canvas.text(caption, &words, CAPTION, fonts.bold, false);

    let lines: Vec<&str> = match &viewer.preview {
        Preview::None => Vec::new(),
        Preview::Loading => vec!["…"],
        Preview::Info(info) => vec![info.as_str()],
        Preview::Text(text) => text.iter().map(String::as_str).collect(),
        Preview::Hex(dump) => {
            draw_hex(canvas, fonts, body, dump);
            Vec::new()
        }
        Preview::Picture(_) => {
            if let Some(picture) = &window.picture {
                draw_picture(canvas, picture, body);
            }
            Vec::new()
        }
    };
    let font = if matches!(viewer.preview, Preview::Text(_)) {
        fonts.mono
    } else {
        fonts.ui
    };
    for (index, line) in lines.iter().enumerate() {
        let top = body.y + index as f64 * LINE;
        if top + LINE > body.bottom() {
            break;
        }
        canvas.text(
            Rect::new(body.x, top, body.w, LINE),
            line,
            TEXT,
            font,
            false,
        );
    }
}

/// A hex dump in `body`: the monospace font shrunk until a whole line fits the width, so the
/// character column is never cut off, as many lines as fit.
fn draw_hex(canvas: &Canvas, fonts: &Fonts, body: Rect, dump: &[String]) {
    let Some(widest) = dump.iter().max_by_key(|line| line.len()) else {
        return;
    };
    let (font, line_height) = fonts.fitting(canvas, widest, body.w);
    for (index, line) in dump.iter().enumerate() {
        let top = body.y + index as f64 * line_height;
        if top + line_height > body.bottom() {
            break;
        }
        canvas.text(
            Rect::new(body.x, top, body.w, line_height),
            line,
            TEXT,
            font,
            false,
        );
    }
}

/// The picture, centred in `body`, at the pixels it was prepared for.
fn draw_picture(canvas: &Canvas, picture: &crate::preview::Picture, body: Rect) {
    let area = canvas.rect(body);
    let width = i32::try_from(picture.width)
        .unwrap_or(0)
        .min(area.right - area.left);
    let height = i32::try_from(picture.height)
        .unwrap_or(0)
        .min(area.bottom - area.top);
    if width <= 0 || height <= 0 {
        return;
    }
    let left = area.left + (area.right - area.left - width) / 2;
    // SAFETY: the header describes `bgra` exactly: top-down (negative height), 32 bits.
    unsafe {
        let mut info: BITMAPINFO = ::std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: picture.width as i32,
            biHeight: -(picture.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..::std::mem::zeroed()
        };
        SetDIBitsToDevice(
            canvas.dc,
            left,
            area.top,
            width as u32,
            height as u32,
            0,
            0,
            0,
            picture.height,
            picture.bgra.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
        );
    }
}

/// The breadcrumbs: the scan's root, then each folder down to the one shown, which is drawn
/// bold; every one before it is a way back up. When they do not fit, the ones nearest the root
/// after it give way to "…". On the right, the folder's size and the whole scan's.
fn draw_path_bar(canvas: &Canvas, window: &Window, bar: Rect) -> Vec<(Rect, usize)> {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    canvas.fill(bar, BAR);
    let pad = 12.0;
    let subtitle = viewer.subtitle();
    let subtitle_w = canvas.width(&subtitle, fonts.ui) + 2.0;
    canvas.text(
        Rect::new(bar.right() - pad - subtitle_w, bar.y, subtitle_w, bar.h),
        &subtitle,
        DIM,
        fonts.ui,
        true,
    );

    let mut names: Vec<String> = vec![without_verbatim_prefix(&viewer.root().to_string_lossy())];
    names.extend(
        viewer
            .tree
            .current_folder_names
            .iter()
            .map(|name| name.to_string_lossy().into_owned()),
    );
    let separator = "  ›  ";
    let separator_w = canvas.width(separator, fonts.ui);
    let ellipsis_w = canvas.width("…", fonts.ui);
    let last = names.len() - 1;
    let widths: Vec<f64> = names
        .iter()
        .enumerate()
        .map(|(depth, name)| {
            let font = if depth == last { fonts.bold } else { fonts.ui };
            canvas.width(name, font) + 1.0
        })
        .collect();
    let room = (bar.w - subtitle_w - 3.0 * pad).max(0.0);
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
    let mut x = bar.x + pad;
    let mut crumbs = Vec::new();
    for (depth, name) in names.iter().enumerate() {
        if depth > 0 && depth <= skip {
            if depth == 1 {
                canvas.text(
                    Rect::new(x, bar.y, separator_w, bar.h),
                    separator,
                    DIM,
                    fonts.ui,
                    false,
                );
                x += separator_w;
                canvas.text(
                    Rect::new(x, bar.y, ellipsis_w + 2.0, bar.h),
                    "…",
                    TEXT,
                    fonts.ui,
                    false,
                );
                x += ellipsis_w;
            }
            continue;
        }
        if depth > 0 {
            canvas.text(
                Rect::new(x, bar.y, separator_w, bar.h),
                separator,
                DIM,
                fonts.ui,
                false,
            );
            x += separator_w;
        }
        let w = widths[depth].min((bar.x + pad + room - x).max(0.0));
        let rect = Rect::new(x, bar.y, w, bar.h);
        if depth == last {
            canvas.text(rect, name, TEXT, fonts.bold, false);
        } else {
            canvas.text(rect, name, TEXT, fonts.ui, false);
            crumbs.push((Rect::new(x - 3.0, bar.y, w + 6.0, bar.h), depth));
        }
        x += w;
    }
    crumbs
}

/// The status bar: what the pointer or the keyboard is on (or a message), and on the right what
/// the scan found. With nothing to say, the keys.
fn draw_status(canvas: &Canvas, window: &Window, status: Rect) {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    canvas.fill(status, BAR);
    let pad = 8.0;
    let (mut left, right) = viewer.status();
    if left.is_empty() {
        left = match viewer.hover.as_deref().and_then(|name| viewer.entry_named(name)) {
            Some(entry) => describe(entry),
            None => "→ open in place · ← close · Enter open · Esc up · Tab list/treemap · \
                     Ctrl+click mark · Shift+↑↓ range · Ctrl+C copy · right-click menu · Del delete · \
                     r/R rescan · a size · +/− zoom · s panel"
                .to_string(),
        };
    }
    let right_w = canvas.width(&right, fonts.ui).min(status.w / 2.0);
    canvas.text(
        Rect::new(status.right() - pad - right_w, status.y, right_w, status.h),
        &right,
        DIM,
        fonts.ui,
        true,
    );
    canvas.text(
        Rect::new(
            status.x + pad,
            status.y,
            (status.w - right_w - 3.0 * pad).max(0.0),
            status.h,
        ),
        &left,
        TEXT,
        fonts.ui,
        false,
    );
}
