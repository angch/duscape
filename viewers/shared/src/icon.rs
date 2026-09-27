//! The application's icon, drawn by the treemap itself: a folder holding a few files, beside a
//! few more, squarified at whatever size the platform asks for and coloured as the window
//! colours tiles. A placeholder until there is a drawn one, but the app's own picture of a
//! disk, and sharp at every size since it is laid out at that size rather than scaled.

use ::std::ffi::OsStr;

use libduscape::tiles::{Area, FileMetadata, FileType, Grid, TreeMap};

use crate::state::tile_color;

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
        &TOP.map(|(name, share, folder)| (name, share, folder)),
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
        let color = tile_color(OsStr::new(name), kind, index);
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
                let color = tile_color(OsStr::new(name), FileType::File, 0);
                let color = (color.0 * 0.85, color.1 * 0.85, color.2 * 0.85);
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
        .enumerate()
        .map(|(index, &(_, share, _))| FileMetadata {
            name: ::std::ffi::OsString::new(),
            size: index as u128,
            descendants: None,
            percentage: share,
            file_type: FileType::File,
        })
        .collect();
    let mut map = TreeMap::with_grid(&area, Grid::pixels(1));
    map.populate_tiles(files.iter().collect());
    map.tiles
        .iter()
        .map(|tile| {
            let (name, _, folder) = entries[tile.size as usize];
            let cells = Area {
                x: tile.x,
                y: tile.y,
                width: tile.width,
                height: tile.height,
            };
            (cells, name, folder)
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

/// The icon as a PNG file: for a toolkit that takes an image file's bytes (AppKit's `NSImage`).
/// Stored, not compressed — an icon is a few kilobytes either way.
#[must_use]
pub fn png(size: u32) -> Vec<u8> {
    let side = size.clamp(1, 1024);
    let pixels = rgba(side);
    // Each row starts with its filter type, 0: none.
    let row = side as usize * 4;
    let mut raw = Vec::with_capacity((row + 1) * side as usize);
    for line in pixels.chunks(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&side.to_be_bytes());
    header.extend_from_slice(&side.to_be_bytes());
    // 8 bits a channel, colour type 6 (RGBA), deflate, adaptive filtering, not interlaced.
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// `data` in a zlib stream of stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(0xFFFF).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        let last = u8::from(blocks.peek().is_none());
        let len = block.len() as u16;
        out.push(last);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{crc32, png, rgba};

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
    fn the_png_is_well_formed() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        let file = png(32);
        assert_eq!(&file[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&file[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(file[16..20].try_into().unwrap()), 32);
        assert_eq!(&file[file.len() - 8..file.len() - 4], b"IEND");
    }
}
