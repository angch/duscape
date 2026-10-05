//! Drawing the window with GDI, to an off-screen bitmap blitted once, so nothing flickers. Every
//! rectangle comes from the viewer's [`Layout`] in points and is scaled to pixels here.
//!
//! The colours follow the terminal viewer's: marks are black on yellow, the entry in hand black
//! on grey, and the panel with the keyboard has an accent outline. Tiles take the colours the
//! other desktop viewers give them (`tile_color`), so a file's kind looks the same everywhere.

use ::std::cell::Cell;
use ::std::mem::{size_of, zeroed};
use ::std::ptr::{null, null_mut};

use duscape_viewer::chooser::Chooser;
use duscape_viewer::deleting::{Deletion, DeletionLayout};
use duscape_viewer::passes::LabelBudget;
use duscape_viewer::state::{
    EXPANDER, Focus, LIST_PAD, Layout, Preview, ROW, ROW_INDENT, Rect, TILE_LABEL, describe,
};
use libduscape::DisplaySize;
use libduscape::format::without_verbatim_prefix;
use libduscape::tiles::FileType;
use libduscape::tiles::{Inside, Row, Tile};

use windows_sys::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, ETO_CLIPPED, EndPaint, ExtTextOutW, FF_DONTCARE, FF_MODERN,
    FIXED_PITCH, FW_NORMAL, FW_SEMIBOLD, GdiFlush, GetCurrentObject, GetDC, GetTextExtentExPointW,
    GetTextExtentPoint32W, GetTextMetricsW, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, OBJ_FONT,
    PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetDIBitsToDevice, SetTextAlign,
    SetTextColor, TA_LEFT, TA_RIGHT, TA_TOP, TEXTMETRICW, TRANSPARENT, VARIABLE_PITCH,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SystemParametersInfoW,
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

/// The fonts the window draws with, made once at its DPI: the system's message font — the one
/// Explorer lists files in, at the size and weight Settings give it (text size included) — its
/// semibold, and a monospace face at the same height.
pub struct Fonts {
    ui: HFONT,
    bold: HFONT,
    mono: HFONT,
    /// The treemap's labels, and a folder's at the top level: the system font a little
    /// smaller, and smaller still if its whole height would not fit in `LABEL_LINE`.
    label: HFONT,
    label_bold: HFONT,
    /// The system font the others are made from.
    base: LOGFONTW,
    /// The fonts' height in pixels.
    height: i32,
    /// The monospace font sized down for a hex dump to fit the preview's width: its height in
    /// pixels and the font, made when first needed and remade when the width changes.
    hex: Cell<(i32, HFONT)>,
}

/// The least a hex dump's font is shrunk to, in pixels.
const HEX_MIN_HEIGHT: i32 = 6;

/// A tile's label line: from `LABEL_TOP` below the tile's top to where the nesting starts its
/// entries (the viewer's `TILE_LABEL` band), less the pixel of their frame.
const LABEL_TOP: f64 = 1.0;
const LABEL_LINE: f64 = TILE_LABEL - LABEL_TOP;

/// How much smaller than the system font a label is.
const LABEL_SHRINK: f64 = 0.9;

/// The monospace face, for text previews and hex dumps; the system names none.
const MONO: &str = "Consolas";

impl Fonts {
    pub fn new(scale: f64) -> Self {
        let base = message_font().unwrap_or_else(|| fallback_font(scale));
        let height = base.lfHeight.abs();
        let fits = ((LABEL_LINE - 1.0) * scale).floor() as i32;
        let mut label_height = ((f64::from(height) * LABEL_SHRINK).round() as i32).max(1);
        while label_height > HEX_MIN_HEIGHT && line_height(&base, label_height) > fits {
            label_height -= 1;
        }
        let bold = base.lfWeight.max(FW_SEMIBOLD as i32);
        Fonts {
            ui: make_font(&base, height, base.lfWeight, None),
            bold: make_font(&base, height, bold, None),
            mono: make_font(&base, height, base.lfWeight, Some(MONO)),
            label: make_font(&base, label_height, base.lfWeight, None),
            label_bold: make_font(&base, label_height, bold, None),
            base,
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
            let font = make_font(&self.base, height, self.base.lfWeight, Some(MONO));
            self.hex.set((height, font));
            font
        };
        (font, LINE * f64::from(height) / f64::from(self.height))
    }
}

/// The system's message font, in pixels at the system DPI (the process is system-DPI aware);
/// `None` if Windows will not say.
fn message_font() -> Option<LOGFONTW> {
    // SAFETY: all-zero is a valid `NONCLIENTMETRICSW`; its size is set before the call, which
    // writes no more than that.
    unsafe {
        let mut metrics: NONCLIENTMETRICSW = zeroed();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let ok = SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&raw mut metrics).cast(),
            0,
        );
        (ok != 0 && metrics.lfMessageFont.lfHeight != 0).then_some(metrics.lfMessageFont)
    }
}

/// Segoe UI at 15 points, what the window drew with before it asked the system.
fn fallback_font(scale: f64) -> LOGFONTW {
    // SAFETY: all-zero is a valid `LOGFONTW`: default everything, an empty face name.
    let mut font: LOGFONTW = unsafe { zeroed() };
    font.lfHeight = -((15.0 * scale).round() as i32);
    font.lfWeight = FW_NORMAL as i32;
    font.lfCharSet = DEFAULT_CHARSET;
    font.lfQuality = CLEARTYPE_QUALITY;
    font.lfPitchAndFamily = VARIABLE_PITCH | FF_DONTCARE;
    set_face(&mut font, "Segoe UI");
    font
}

fn set_face(font: &mut LOGFONTW, face: &str) {
    let face = super::wide(face);
    let len = face.len().min(font.lfFaceName.len());
    font.lfFaceName = [0; 32];
    font.lfFaceName[..len].copy_from_slice(&face[..len]);
    font.lfFaceName[31] = 0;
}

/// How tall a line of `base` at `height` pixels is drawn, in pixels: ascent to descent, all
/// that `DrawTextW` paints; `height` itself if it cannot be measured.
fn line_height(base: &LOGFONTW, height: i32) -> i32 {
    let font = make_font(base, height, base.lfWeight.max(FW_SEMIBOLD as i32), None);
    // SAFETY: the screen DC is released and the font deselected and deleted before returning;
    // all-zero is a valid `TEXTMETRICW` for the call to fill.
    unsafe {
        let screen = GetDC(null_mut());
        let old = SelectObject(screen, font as _);
        let mut metrics: TEXTMETRICW = zeroed();
        let ok = GetTextMetricsW(screen, &mut metrics);
        SelectObject(screen, old);
        ReleaseDC(null_mut(), screen);
        DeleteObject(font as _);
        if ok != 0 { metrics.tmHeight } else { height }
    }
}

/// A GDI font like `base`, `height` pixels tall at `weight`, in `face` (monospace) if given.
fn make_font(base: &LOGFONTW, height: i32, weight: i32, face: Option<&str>) -> HFONT {
    let mut font = *base;
    font.lfHeight = -height;
    font.lfWidth = 0;
    font.lfWeight = weight;
    if let Some(face) = face {
        font.lfPitchAndFamily = FIXED_PITCH | FF_MODERN;
        set_face(&mut font, face);
    }
    // SAFETY: the face name is NUL-terminated within the struct, alive for the call.
    unsafe { CreateFontIndirectW(&font) }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: each font was made by `CreateFontW` and is deleted once, deselected by now.
        unsafe {
            DeleteObject(self.ui as _);
            DeleteObject(self.bold as _);
            DeleteObject(self.mono as _);
            DeleteObject(self.label as _);
            DeleteObject(self.label_bold as _);
            let (_, hex) = self.hex.get();
            if !hex.is_null() {
                DeleteObject(hex as _);
            }
        }
    }
}

/// A device context to draw into, taking rectangles in points: a 32-bit DIB section, whose
/// rectangles are filled by writing its pixels here, and whose text and pictures GDI draws.
///
/// A nested treemap is tens of thousands of tiles, each painted over its parent's: FillRect
/// and FrameRect cost two microseconds or so a call, and at that a frame took 30 ms and more.
/// Written straight into the buffer they cost what the pixels do.
struct Canvas {
    dc: HDC,
    scale: f64,
    /// The DIB section's pixels, top-down, `0x00RRGGBB`, `width` to a row.
    pixels: *mut u32,
    width: i32,
    height: i32,
    /// GDI has drawn since the pixels were last written: its batch is flushed first.
    gdi: Cell<bool>,
    /// Which of the treemap's tiles this paint has time to label.
    labels: LabelBudget,
    /// The last font an ellipsis was measured in, and its width there.
    ellipsis: Cell<(HFONT, i32)>,
    /// The font selected into the DC, so it is selected only when it changes; the DC's own
    /// is put back when the paint ends.
    font: Cell<HFONT>,
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

    /// The width of "…" in `font`, selected: measured once a font, since every cut label asks.
    fn ellipsis_width(&self, font: HFONT) -> i32 {
        let (known, width) = self.ellipsis.get();
        if known == font {
            return width;
        }
        let mut dots = SIZE { cx: 0, cy: 0 };
        // SAFETY: `ELLIPSIS` and `dots` are alive for the call, the length is one.
        unsafe { GetTextExtentPoint32W(self.dc, &ELLIPSIS, 1, &mut dots) };
        self.ellipsis.set((font, dots.cx));
        dots.cx
    }

    /// Pixels `left..right` × `top..bottom` in `color`, cut to the buffer.
    fn span(&self, left: i32, top: i32, right: i32, bottom: i32, color: COLORREF) {
        let (left, right) = (left.max(0), right.min(self.width));
        let (top, bottom) = (top.max(0), bottom.min(self.height));
        if left >= right || top >= bottom {
            return;
        }
        if self.gdi.replace(false) {
            // SAFETY: no arguments; it only completes GDI's pending drawing.
            unsafe { GdiFlush() };
        }
        // A DIB's pixel is blue in the low byte, a COLORREF red.
        let pixel = ((color & 0xFF) << 16) | (color & 0xFF00) | ((color >> 16) & 0xFF);
        let (width, run) = (self.width as usize, (right - left) as usize);
        for y in top as usize..bottom as usize {
            // SAFETY: `left..right` and `top..bottom` are inside the `width`×`height` buffer
            // the DIB section gave, alive while the canvas is, and nothing else holds it now.
            let row = unsafe {
                ::std::slice::from_raw_parts_mut(self.pixels.add(y * width + left as usize), run)
            };
            row.fill(pixel);
        }
    }

    fn fill(&self, rect: Rect, color: COLORREF) {
        let area = self.rect(rect);
        self.span(area.left, area.top, area.right, area.bottom, color);
    }

    /// A frame `thickness` pixels wide, inside `rect`.
    fn frame(&self, rect: Rect, color: COLORREF, thickness: i32) {
        let mut area = self.rect(rect);
        for _ in 0..thickness.max(1) {
            if area.left >= area.right || area.top >= area.bottom {
                return;
            }
            let RECT {
                left,
                top,
                right,
                bottom,
            } = area;
            self.span(left, top, right, top + 1, color);
            self.span(left, bottom - 1, right, bottom, color);
            self.span(left, top + 1, left + 1, bottom - 1, color);
            self.span(right - 1, top + 1, right, bottom - 1, color);
            area.left += 1;
            area.top += 1;
            area.right -= 1;
            area.bottom -= 1;
        }
    }

    /// `text` in `rect`, one line, vertically centred, cut short with an ellipsis if it does
    /// not fit. Measured once and drawn with `ExtTextOutW`: `DrawTextW`, which did the same,
    /// laid the line out again for its ellipsis and cost half as much again a label.
    fn text(&self, rect: Rect, text: &str, color: COLORREF, font: HFONT, right: bool) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let area = self.rect(rect);
        let room = area.right - area.left;
        if wide.is_empty() || room <= 0 {
            return;
        }
        self.gdi.set(true);
        // SAFETY: `wide` and `area` are alive for the calls; the counts are their lengths.
        unsafe {
            self.select(font);
            SetTextColor(self.dc, color);
            let len = i32::try_from(wide.len()).unwrap_or(i32::MAX);
            let (mut fit, mut size) = (0, SIZE { cx: 0, cy: 0 });
            // Where each character that fits ends, so a cut needs no second measuring.
            let mut ends = vec![0i32; wide.len()];
            GetTextExtentExPointW(
                self.dc,
                wide.as_ptr(),
                len,
                room,
                &mut fit,
                ends.as_mut_ptr(),
                &mut size,
            );
            if fit < len {
                // What fits beside an ellipsis, then the ellipsis, not splitting a pair.
                let fit = usize::try_from(fit).unwrap_or(0).min(wide.len());
                let beside = room - self.ellipsis_width(font);
                let mut keep = ends[..fit].partition_point(|&end| end <= beside);
                if keep > 0 && (0xD800..0xDC00).contains(&wide[keep - 1]) {
                    keep -= 1;
                }
                wide.truncate(keep);
                wide.push(ELLIPSIS);
            }
            let (x, align) = if right {
                (area.right, TA_RIGHT | TA_TOP)
            } else {
                (area.left, TA_LEFT | TA_TOP)
            };
            SetTextAlign(self.dc, align);
            let y = area.top + (area.bottom - area.top - size.cy) / 2;
            ExtTextOutW(
                self.dc,
                x,
                y,
                ETO_CLIPPED,
                &area,
                wide.as_ptr(),
                u32::try_from(wide.len()).unwrap_or(u32::MAX),
                null(),
            );
        }
    }

    fn select(&self, font: HFONT) {
        if self.font.get() != font {
            // SAFETY: the font is alive for the paint (the window's `Fonts`); the DC's first
            // font is put back before the DC is deleted.
            unsafe { SelectObject(self.dc, font as _) };
            self.font.set(font);
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
            self.select(font);
            GetTextExtentPoint32W(
                self.dc,
                wide.as_ptr(),
                i32::try_from(wide.len()).unwrap_or(i32::MAX),
                &mut size,
            );
        }
        f64::from(size.cx) / self.scale
    }
}

/// "…", which a label cut short ends with.
const ELLIPSIS: u16 = 0x2026;

/// An sRGB colour as GDI takes it.
fn colorref((r, g, b): (f64, f64, f64)) -> COLORREF {
    let byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    rgb(byte(r), byte(g), byte(b))
}

/// The off-screen buffer frames are drawn in: a 32-bit DIB section in a memory DC, kept from
/// one paint to the next and made again only when the window's size changes. Made fresh each
/// paint, its 7 MB were allocated, faulted in page by page on the first fill, and unmapped
/// again: some 5 ms of every frame.
pub struct BackBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    /// What was selected into the DC before the bitmap, put back before it is deleted.
    old_bitmap: HGDIOBJ,
    /// The DC's own font, put back likewise.
    first_font: HGDIOBJ,
    pixels: *mut u32,
    width: i32,
    height: i32,
}

impl BackBuffer {
    /// One `width` × `height` for `screen`, or `None` if GDI will not make it.
    fn new(screen: HDC, width: i32, height: i32) -> Option<Self> {
        // SAFETY: the header describes a top-down 32-bit bitmap of the size asked; what is
        // made is deleted again if the rest cannot be.
        unsafe {
            let dc = CreateCompatibleDC(screen);
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = ::std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..::std::mem::zeroed()
            };
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old_bitmap = SelectObject(dc, bitmap as _);
            SetBkMode(dc, TRANSPARENT as i32);
            let first_font = GetCurrentObject(dc, OBJ_FONT as u32);
            Some(BackBuffer {
                dc,
                bitmap,
                old_bitmap,
                first_font,
                pixels: bits.cast(),
                width,
                height,
            })
        }
    }
}

impl Drop for BackBuffer {
    fn drop(&mut self) {
        // SAFETY: made by `new`; what was selected into the DC is put back before the DC and
        // the bitmap are deleted, once each.
        unsafe {
            SelectObject(self.dc, self.first_font);
            SelectObject(self.dc, self.old_bitmap);
            DeleteObject(self.bitmap as _);
            DeleteDC(self.dc);
        }
    }
}

/// Draw the whole window; `in_full` with every label, else stopping the treemap's labels at
/// [`LABEL_DEADLINE`](duscape_viewer::passes::LABEL_DEADLINE). Returns the breadcrumbs, in points, for clicks — each one's rectangle and
/// the depth it goes up to — and whether it was painted in full.
pub fn paint(
    window: &Window,
    back: &mut Option<BackBuffer>,
    hwnd: HWND,
    in_full: bool,
) -> (Vec<(Rect, usize)>, bool) {
    // SAFETY: BeginPaint/EndPaint bracket the paint; the off-screen buffer is the window's,
    // alive for the paint, and drawn into only here.
    unsafe {
        let mut ps: PAINTSTRUCT = ::std::mem::zeroed();
        let screen = BeginPaint(hwnd, &mut ps);
        let client = client_rect(hwnd);
        let (width, height) = (
            (client.right - client.left).max(1),
            (client.bottom - client.top).max(1),
        );
        if back
            .as_ref()
            .is_none_or(|back| (back.width, back.height) != (width, height))
        {
            // The old one goes first: two of them at once is twice the memory for a moment.
            *back = None;
            *back = BackBuffer::new(screen, width, height);
        }
        let Some(back) = back.as_ref() else {
            EndPaint(hwnd, &ps);
            return (Vec::new(), true);
        };
        let dc = back.dc;
        let canvas = Canvas {
            dc,
            scale: window.scale,
            pixels: back.pixels,
            width,
            height,
            gdi: Cell::new(false),
            // Whatever font the last paint left selected is one of the window's, alive still;
            // not knowing which, the first text selects its own.
            font: Cell::new(null_mut()),
            ellipsis: Cell::new((null_mut(), 0)),
            labels: LabelBudget::new(in_full),
        };
        let viewer = &window.viewer;
        let layout = &viewer.layout;

        canvas.fill(layout.bounds, BACKGROUND);
        if let Some(chooser) = &window.chooser {
            let rows = draw_chooser(&canvas, window, layout.bounds, chooser);
            GdiFlush();
            BitBlt(screen, 0, 0, width, height, dc, 0, 0, SRCCOPY);
            EndPaint(hwnd, &ps);
            return (rows, true);
        }
        draw_treemap(&canvas, window, layout);
        if let Some(list) = layout.list {
            draw_list(&canvas, window, list);
        }
        if let Some(info) = layout.info {
            draw_preview(&canvas, window, info);
        }
        // The free-space toggle at the bar's right, at a volume's root, and the bar short of it.
        let toggle = window.viewer.free_toggle();
        let mut bar = layout.path_bar;
        if toggle.is_some() {
            bar.w = (layout.free_toggle.x - bar.x).max(0.0);
        }
        let crumbs = draw_path_bar(&canvas, window, bar);
        if let Some((label, on)) = toggle {
            draw_free_toggle(&canvas, window, layout.free_toggle, label, on);
        }
        draw_chooser_button(&canvas, layout.chooser_button);
        draw_status(&canvas, window, layout.status);
        if let Some(deletion) = viewer.deleting().filter(|deletion| deletion.shown()) {
            draw_deletion(&canvas, window, layout.bounds, deletion);
        }

        GdiFlush();
        BitBlt(screen, 0, 0, width, height, dc, 0, 0, SRCCOPY);
        EndPaint(hwnd, &ps);
        (crumbs, canvas.labels.complete())
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
        let color = if marked {
            MARK
        } else {
            colorref(viewer.board_color(index))
        };
        fill_tile(canvas, layout, tile, viewer.board_inside(index), color);
        canvas.frame(rect, BORDER, 1);
        // A folder too short for its label band has its entries right under its margin.
        if rect.w > 40.0
            && rect.h > LINE
            && viewer.labelled(tile)
            && let Some(_label) = canvas.labels.allows()
        {
            let ink = if marked { INK } else { rgb(240, 240, 240) };
            draw_tile_label(canvas, fonts, rect, pad, tile, ink, fonts.label_bold);
        }
        // (`hover` is never a name while a nested tile is hovered: `hover_at` sees to that.)
        if hover == Some(tile.name.as_os_str()) && board.get_selected_index() != Some(index) {
            canvas.frame(rect, rgb(170, 170, 170), 1);
        }
    }
    draw_nested(canvas, window, layout);
    // The "small files" corner: from where the board says it starts to the entries' far
    // corner, short of the free space's strip.
    if let Some(corner) = board.corner() {
        let corner = layout.cells_to_rect(corner.x, corner.y, corner.width, corner.height);
        canvas.fill(corner, rgb(60, 60, 60));
        canvas.frame(corner, BORDER, 1);
        // The label only where it fits, and not over the specks: a sliver of a corner is still
        // drawn, as the terminal viewer keeps its `x`, but half a line of text would read as a
        // glitch.
        if viewer.dust().is_empty() && corner.h >= LINE + pad / 2.0 {
            let line = Rect::new(corner.x + pad, corner.y, corner.w - 2.0 * pad, LINE);
            canvas.text(line, "x  small files", DIM, fonts.ui, false);
        }
    }
    // The "small files" corners' entries — the board's, and each folder's — a speck each
    // down to a pixel, in their tiles' colours: framed where there is room for a frame and
    // colour inside it, else with the least tiles' grid over it (`Dust::grid`).
    for dust in viewer.dust() {
        let rect = layout.cells_to_rect(dust.x, dust.y, dust.width, dust.height);
        canvas.fill(rect, colorref(dust.color));
        if dust.framed {
            canvas.frame(rect, BORDER, 1);
        }
        for (x, y, width, height) in dust.grid() {
            canvas.fill(layout.cells_to_rect(x, y, width, height), BORDER);
        }
    }
    // The board's corner framed as a tile is, over the specks at its edges.
    if !viewer.dust().is_empty()
        && let Some(corner) = board.corner()
    {
        let corner = layout.cells_to_rect(corner.x, corner.y, corner.width, corner.height);
        canvas.frame(corner, BORDER, 1);
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
    if viewer.focus() == Focus::Treemap && layout.list.is_some() {
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
        let marked = viewer.is_marked_row(&row.path);
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
    if viewer.focus() == Focus::List {
        canvas.frame(list, ACCENT, 1);
    }
}

/// A tile in `color`: whole, or where entries were nested in it only around what they cover,
/// since they are drawn over the rest.
fn fill_tile(
    canvas: &Canvas,
    layout: &Layout,
    tile: &Tile,
    inside: Option<&Inside>,
    color: COLORREF,
) {
    let whole = layout.cells_to_rect(tile.x, tile.y, tile.width, tile.height);
    for part in layout.fill_parts(tile, inside, whole) {
        canvas.fill(part, color);
    }
}

/// The tiles inside the folder tiles — the nesting — parents first, so each level paints
/// over its parent's body and under the parent's label; a level deeper is a shade darker, and
/// a label goes on whatever has the room for one.
fn draw_nested(canvas: &Canvas, window: &Window, layout: &Layout) {
    let viewer = &window.viewer;
    let fonts = &window.fonts;
    let pad = 3.0;
    let marks = viewer.marked_nested();
    for (index, nested) in viewer.nested().iter().enumerate() {
        let t = &nested.tile;
        let rect = layout.cells_to_rect(t.x, t.y, t.width, t.height);
        let marked = marks.get(index).copied().unwrap_or(false);
        let color = if marked {
            MARK
        } else {
            colorref(viewer.nested_color(index))
        };
        fill_tile(canvas, layout, t, nested.inside.as_ref(), color);
        canvas.frame(rect, BORDER, 1);
        if rect.w > 30.0
            && rect.h > LINE
            && viewer.labelled(t)
            && let Some(_label) = canvas.labels.allows()
        {
            let ink = if marked { INK } else { rgb(235, 235, 235) };
            draw_tile_label(canvas, fonts, rect, pad, t, ink, fonts.label);
        }
        if viewer.hover_nested == Some(index) {
            canvas.frame(rect, rgb(200, 200, 200), 1);
        }
    }
}

/// A tile's label, in `ink`, `pad` in from the sides, in the `LABEL_LINE` at its top. A folder's is
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
    let line = Rect::new(
        rect.x + pad,
        rect.y + LABEL_TOP,
        rect.w - 2.0 * pad,
        LABEL_LINE,
    );
    let size = DisplaySize(tile.size as f64).to_string();
    let below = !is_dir && rect.h >= LABEL_TOP + 2.0 * LABEL_LINE + pad;
    if below {
        canvas.text(line, &label, ink, font, false);
        let size_line = Rect::new(line.x, rect.bottom() - pad - LABEL_LINE, line.w, LABEL_LINE);
        canvas.text(size_line, &size, ink, fonts.label, true);
        return;
    }
    // Measured only where it may go beside the name.
    let size_width = canvas.width(&size, fonts.label);
    if line.w - size_width - LIST_PAD >= NAME_ROOM {
        let name_rect = Rect::new(line.x, line.y, line.w - size_width - LIST_PAD, line.h);
        canvas.text(name_rect, &label, ink, font, false);
        let size_rect = Rect::new(line.right() - size_width, line.y, size_width, line.h);
        canvas.text(size_rect, &size, ink, fonts.label, true);
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
    // Explorer's weight for every name; the expander and the `\` say it is a folder.
    canvas.text(name_rect, &label, ink, fonts.ui, false);
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
        format!(
            "{} marked · {}",
            libduscape::DisplayCount(viewer.marked.len() as u64),
            DisplaySize(viewer.marked_size() as f64)
        )
    } else if let Some(entry) = viewer.shown_entry().or_else(|| viewer.selected_entry()) {
        let mut words = entry.name.to_string_lossy().into_owned();
        if entry.file_type == FileType::Folder {
            words.push('\\');
        }
        words.push_str(&format!(" · {}", DisplaySize(entry.size as f64)));
        match &viewer.preview {
            Preview::Picture(description) => words.push_str(&format!(" · {description}")),
            Preview::Hex { .. } => words.push_str(" · binary"),
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
        Preview::Hex { info, dump } => {
            // Its description first, one paragraph wrapped to the panel, then the dump.
            let words = duscape_viewer::preview::paragraph(info);
            let described =
                duscape_viewer::preview::wrap(&words, body.w, |line| canvas.width(line, fonts.ui));
            let mut top = body.y;
            for line in &described {
                if top + LINE > body.bottom() {
                    break;
                }
                canvas.text(
                    Rect::new(body.x, top, body.w, LINE),
                    line,
                    DIM,
                    fonts.ui,
                    false,
                );
                top += LINE;
            }
            if !described.is_empty() {
                top += LINE / 2.0;
            }
            let rest = Rect::new(body.x, top, body.w, (body.bottom() - top).max(0.0));
            draw_hex(canvas, fonts, rest, dump);
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
        canvas.gdi.set(true);
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
/// The button at the path bar's left that opens the chooser: a drive, drawn small.
fn draw_chooser_button(canvas: &Canvas, button: Rect) {
    canvas.fill(button, BAR);
    let (w, h) = (16.0, 10.0);
    let drive = Rect::new(
        button.x + (button.w - w) / 2.0,
        button.y + (button.h - h) / 2.0,
        w,
        h,
    );
    canvas.frame(drive, DIM, 1);
    let light = Rect::new(drive.right() - 5.0, drive.bottom() - 5.0, 2.0, 2.0);
    canvas.fill(light, DIM);
}

/// The toggle at the path bar's right while the root of a volume is shown: a box, ticked when
/// the free space is on the board, and its words.
fn draw_free_toggle(canvas: &Canvas, window: &Window, rect: Rect, label: &str, on: bool) {
    canvas.fill(rect, BAR);
    let side = 12.0;
    let r#box = Rect::new(rect.x + 10.0, rect.y + (rect.h - side) / 2.0, side, side);
    canvas.frame(r#box, DIM, 1);
    if on {
        canvas.fill(
            Rect::new(r#box.x + 3.0, r#box.y + 3.0, side - 6.0, side - 6.0),
            DIM,
        );
    }
    let words = Rect::new(
        r#box.right() + 8.0,
        rect.y,
        (rect.right() - r#box.right() - 16.0).max(0.0),
        rect.h,
    );
    canvas.text(words, label, DIM, window.fonts.ui, false);
}

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

/// A delete under way, over everything else: what it is on, how far it has got, and Cancel.
fn draw_deletion(canvas: &Canvas, window: &Window, bounds: Rect, deletion: &Deletion) {
    let fonts = &window.fonts;
    let layout = DeletionLayout::new(bounds);
    let (title, name, count) = deletion.words();
    canvas.fill(layout.panel, rgb(44, 44, 48));
    canvas.frame(layout.panel, ACCENT, 1);
    canvas.text(layout.title, &title, TEXT, fonts.bold, false);
    canvas.text(layout.path, &name, DIM, fonts.ui, false);
    canvas.fill(layout.bar, BAR);
    let done = Rect::new(
        layout.bar.x,
        layout.bar.y,
        layout.bar.w * deletion.fraction().clamp(0.0, 1.0),
        layout.bar.h,
    );
    canvas.fill(done, ACCENT);
    canvas.text(layout.count, &count, DIM, fonts.ui, false);
    let button = layout.cancel;
    let cancelling = deletion.cancelling();
    canvas.fill(button, rgb(60, 60, 64));
    canvas.frame(button, if cancelling { BORDER } else { DIM }, 1);
    let words = "Cancel";
    let width = canvas.width(words, fonts.ui);
    canvas.text(
        Rect::new(
            button.x + ((button.w - width) / 2.0).max(0.0),
            button.y,
            width.min(button.w),
            button.h,
        ),
        words,
        if cancelling { DIM } else { TEXT },
        fonts.ui,
        false,
    );
}

/// The window opened with no folder: the volumes and the home folder as rows, each with how
/// full it is, the one in hand framed and the one under the pointer lit. Returns the rows
/// with their indexes, for clicks, in the breadcrumbs' place.
fn draw_chooser(
    canvas: &Canvas,
    window: &Window,
    bounds: Rect,
    chooser: &Chooser,
) -> Vec<(Rect, usize)> {
    let fonts = &window.fonts;
    let layout = chooser.layout(bounds);
    canvas.text(layout.heading, chooser.heading(), TEXT, fonts.bold, false);
    for &(rect, index) in &layout.rows {
        let (title, detail, size, share) = chooser.words(index);
        let lit = chooser.hover == Some(index);
        canvas.fill(rect, if lit { rgb(52, 52, 56) } else { PANEL });
        if chooser.cursor == index {
            canvas.frame(rect, ACCENT, 2);
        }
        let inner = rect.inset(14.0, 8.0);
        let size_w = canvas.width(&size, fonts.ui) + 8.0;
        canvas.text(
            Rect::new(inner.x, inner.y, inner.w - size_w, LINE),
            &title,
            TEXT,
            fonts.bold,
            false,
        );
        canvas.text(
            Rect::new(inner.right() - size_w, inner.y, size_w, LINE),
            &size,
            DIM,
            fonts.ui,
            true,
        );
        canvas.text(
            Rect::new(inner.x, inner.y + LINE, inner.w, LINE),
            &detail,
            DIM,
            fonts.ui,
            false,
        );
        if let Some(share) = share {
            let bar = Rect::new(inner.x, inner.bottom() - 5.0, inner.w, 4.0);
            canvas.fill(bar, BAR);
            let used = Rect::new(bar.x, bar.y, bar.w * share, bar.h);
            let color = if share > 0.9 {
                rgb(215, 75, 65)
            } else {
                ACCENT
            };
            canvas.fill(used, color);
        }
    }
    layout.rows
}
