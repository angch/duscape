//! The application's icon, drawn by the treemap itself: a folder holding a few files, beside a
//! few more, squarified at whatever size the platform asks for and coloured as the window
//! colours tiles. A placeholder until there is a drawn one, but the app's own picture of a
//! disk, and sharp at every size since it is laid out at that size rather than scaled.

use ::std::ffi::OsStr;

use libduscape::tiles::{Area, FileMetadata, FileType, Grid, TreeMap};

use crate::state::{darker, folder_color, tile_color};

/// The entries drawn: (name, share, is a folder). The names only choose the colours.
const TOP: [(&str, f64, bool); 6] = [
    ("folder", 0.46, true),
    ("disk.iso", 0.19, false),
    ("server.log", 0.13, false),
    ("photo.png", 0.10, false),
    ("library.dll", 0.07, false),
    ("main.rs", 0.05, false),
];
/// What the folder holds, drawn inside it under a band of its own colour.
const INSIDE: [(&str, f64); 3] = [("song.mp3", 0.55), ("backup.gz", 0.28), ("util.c", 0.17)];

/// Straight (not premultiplied) RGBA, `size` × `size`, rows top to bottom.
#[must_use]
pub fn rgba(size: u32) -> Vec<u8> {
    let side = size.clamp(1, 1024) as usize;
    let mut pixels = vec![0u8; side * side * 4];
    let edge = side as u16;
    // A gap between tiles, and round corners on the whole: at 16 pixels a gap of one and no
    // rounding, so the tiles still read.
    let gap = (side / 24).max(1) as u16;
    let radius = if side >= 24 { side as f64 / 7.0 } else { 0.0 };
    let dark = (0.08, 0.08, 0.09);
    let mut canvas = Paint {
        pixels: &mut pixels,
        side,
    };
    canvas.fill(0, 0, edge, edge, dark);
    let tiles = layout(
        &TOP,
        Area {
            x: gap,
            y: gap,
            width: edge.saturating_sub(2 * gap),
            height: edge.saturating_sub(2 * gap),
        },
    );
    for (index, (tile, name, folder)) in tiles.iter().enumerate() {
        let kind = if *folder {
            FileType::Folder
        } else {
            FileType::File
        };
        // The folder's blue by its place, as the icon has always been drawn.
        let color = match kind {
            FileType::Folder => folder_color(index as u64),
            _ => tile_color(OsStr::new(name), kind),
        };
        // Each tile gives up its right and bottom edge to the gap.
        let (w, h) = (
            tile.width.saturating_sub(gap).max(1),
            tile.height.saturating_sub(gap).max(1),
        );
        canvas.fill(tile.x, tile.y, w, h, color);
        if *folder && w >= 6 * gap && h >= 6 * gap {
            // The folder's own entries under its band, as in the window's nesting.
            let band = (h / 5).max(gap);
            let inside = Area {
                x: tile.x + gap,
                y: tile.y + band,
                width: w.saturating_sub(gap),
                height: h.saturating_sub(band),
            };
            let inner = layout(&INSIDE.map(|(name, share)| (name, share, false)), inside);
            for (tile, name, _) in &inner {
                let color = darker(tile_color(OsStr::new(name), FileType::File), 0.85);
                canvas.fill(
                    tile.x,
                    tile.y,
                    tile.width.saturating_sub(gap).max(1),
                    tile.height.saturating_sub(gap).max(1),
                    color,
                );
            }
        }
    }
    if radius > 0.0 {
        round_corners(&mut pixels, side, radius);
    }
    pixels
}

/// `entries` squarified in `area`, in square cells, largest first.
fn layout(entries: &[(&'static str, f64, bool)], area: Area) -> Vec<(Area, &'static str, bool)> {
    let files: Vec<FileMetadata> = entries
        .iter()
        .map(|&(_, share, _)| FileMetadata {
            name: ::std::ffi::OsString::new(),
            size: 0,
            descendants: None,
            percentage: share,
            file_type: FileType::File,
        })
        .collect();
    let mut map = TreeMap::with_grid(&area, Grid::pixels(1));
    map.populate_tiles(files.iter().collect());
    map.tiles
        .iter()
        .zip(map.tile_entries())
        .map(|(tile, &entry)| {
            let (name, _, folder) = entries[entry];
            (tile.area(), name, folder)
        })
        .collect()
}

struct Paint<'a> {
    pixels: &'a mut [u8],
    side: usize,
}

impl Paint<'_> {
    fn fill(&mut self, x: u16, y: u16, width: u16, height: u16, (r, g, b): (f64, f64, f64)) {
        let byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        let pixel = [byte(r), byte(g), byte(b), 255];
        let (x0, y0) = (usize::from(x).min(self.side), usize::from(y).min(self.side));
        let x1 = (usize::from(x) + usize::from(width)).min(self.side);
        let y1 = (usize::from(y) + usize::from(height)).min(self.side);
        for row in y0..y1 {
            for column in x0..x1 {
                let at = (row * self.side + column) * 4;
                self.pixels[at..at + 4].copy_from_slice(&pixel);
            }
        }
    }
}

/// Clear what lies outside a square with corners of `radius`, anti-aliased by coverage.
fn round_corners(pixels: &mut [u8], side: usize, radius: f64) {
    let edge = side as f64;
    for row in 0..side {
        for column in 0..side {
            // How far the pixel's centre is into the corner's curve, if it is in a corner.
            let (px, py) = (column as f64 + 0.5, row as f64 + 0.5);
            let cx = px.clamp(radius, edge - radius);
            let cy = py.clamp(radius, edge - radius);
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            let at = (row * side + column) * 4 + 3;
            pixels[at] = (f64::from(pixels[at]) * coverage).round() as u8;
        }
    }
}

/// The icon as a PNG file: for a toolkit that takes an image file's bytes (AppKit's `NSImage`,
/// at start-up, so compressed quickly: 512 pixels in a few milliseconds).
#[must_use]
pub fn png(size: u32) -> Vec<u8> {
    encode(size, image::codecs::png::CompressionType::Fast)
}

/// The icon at `size` as a PNG, compressed by `compression`: [`png`]'s quickly, the Windows
/// icon file's ([`ico`]) as well as `image` can, since that file is checked in.
fn encode(size: u32, compression: image::codecs::png::CompressionType) -> Vec<u8> {
    use image::ImageEncoder;
    use image::codecs::png::{FilterType, PngEncoder};
    let side = size.clamp(1, 1024);
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, compression, FilterType::Adaptive)
        .write_image(&rgba(side), side, side, image::ExtendedColorType::Rgba8)
        .expect("a PNG of pixels in memory is always written");
    png
}

/// The sizes the Windows icon file holds: what Explorer and the taskbar pick from, at 100% to
/// 200% scaling, up to the 256 of a large view.
pub const ICO_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

/// A `.ico` file of `images`: (size, PNG bytes) each, in order. Windows (from Vista) takes PNG
/// images in an icon at every size.
#[must_use]
pub fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    // Reserved, type 1 (icon), count.
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in images {
        // A width or height of 256 is written as 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        // One plane, 32 bits a pixel.
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ICO_SIZES, encode, ico, png, rgba};

    /// `viewers/windows/duscape.ico`, which the Windows binaries carry as their own icon (their
    /// build scripts pack it as a resource), is this module's drawing: the test fails when the
    /// drawing changed and the file did not. `DUSCAPE_WRITE_ICON=1` writes it.
    #[test]
    fn the_icon_file_is_the_one_drawn() {
        let best = image::codecs::png::CompressionType::Best;
        let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
            .iter()
            .map(|&size| (size, encode(size, best)))
            .collect();
        let drawn = ico(&images);
        let path =
            ::std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../windows/duscape.ico");
        if ::std::env::var_os("DUSCAPE_WRITE_ICON").is_some() {
            ::std::fs::write(&path, &drawn).expect("writing the icon file");
        }
        let committed = ::std::fs::read(&path).unwrap_or_default();
        assert!(
            committed == drawn,
            "{} is not the icon drawn: DUSCAPE_WRITE_ICON=1 cargo test -p duscape-viewer              the_icon_file_is_the_one_drawn writes it",
            path.display()
        );
    }

    #[test]
    fn it_is_drawn_at_every_size_asked_with_clear_round_corners() {
        for size in [16, 20, 24, 32, 48, 64, 128, 256] {
            let pixels = rgba(size);
            assert_eq!(pixels.len(), (size * size * 4) as usize);
            let alpha = |x: u32, y: u32| pixels[((y * size + x) * 4 + 3) as usize];
            assert_eq!(
                alpha(size / 2, size / 2),
                255,
                "{size}: the middle is opaque"
            );
            if size >= 24 {
                assert_eq!(alpha(0, 0), 0, "{size}: the corner is rounded off");
            }
            // More than the background: tiles of several colours.
            let mut colors: Vec<&[u8]> = pixels.chunks(4).filter(|p| p[3] == 255).collect();
            colors.sort_unstable();
            colors.dedup();
            assert!(colors.len() >= 5, "{size}: {} colours", colors.len());
        }
    }

    #[test]
    fn the_png_decodes_to_the_pixels_drawn() {
        let file = png(32);
        assert_eq!(&file[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory_with_format(&file, image::ImageFormat::Png)
            .expect("the icon decodes")
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (32, 32));
        assert_eq!(decoded.into_raw(), rgba(32));
    }
}
