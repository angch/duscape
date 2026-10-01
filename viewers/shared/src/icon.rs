//! The application's icon: a six-tile mark matching the explainer's hero artwork, rendered at
//! whatever size a platform asks for so its edges stay sharp rather than being scaled.

const BACKGROUND: (f64, f64, f64) = (0.18, 0.23, 0.21);
const HERO_COLORS: [(f64, f64, f64); 6] = [
    (0.72, 0.89, 0.74),
    (0.91, 0.73, 0.34),
    (0.66, 0.84, 0.86),
    (0.95, 0.49, 0.36),
    (0.60, 0.71, 0.64),
    (0.84, 0.93, 0.39),
];

/// Straight (not premultiplied) RGBA, `size` × `size`, rows top to bottom.
#[must_use]
pub fn rgba(size: u32) -> Vec<u8> {
    let side = size.clamp(1, 1024) as usize;
    let mut pixels = vec![0u8; side * side * 4];
    let edge = side as u16;
    // Wider gaps from 32 px, so the tiles read apart on a dark backing; below it a pixel or
    // two is all a small icon has room for.
    let gap = if side >= 32 {
        (side / 24).max(3) as u16
    } else {
        (side / 32).max(1) as u16
    };
    let radius = if side >= 24 { side as f64 / 7.0 } else { 0.0 };
    let mut canvas = Paint {
        pixels: &mut pixels,
        side,
    };
    canvas.fill(0, 0, edge, edge, BACKGROUND);

    let margin = 2 * gap;
    let content_width = edge.saturating_sub(2 * margin);
    let content_height = content_width;
    let tile_width = content_width.saturating_sub(2 * gap);
    let tile_height = content_height.saturating_sub(2 * gap);
    let columns = [tile_width * 41 / 100, tile_width * 34 / 100];
    let columns = [columns[0], columns[1], tile_width - columns[0] - columns[1]];
    let rows = [tile_height * 26 / 100, tile_height * 46 / 100];
    let rows = [rows[0], rows[1], tile_height - rows[0] - rows[1]];
    let x = [
        margin,
        margin + columns[0] + gap,
        margin + columns[0] + gap + columns[1] + gap,
    ];
    let y = [
        margin,
        margin + rows[0] + gap,
        margin + rows[0] + gap + rows[1] + gap,
    ];

    canvas.fill(
        x[0],
        y[0],
        columns[0],
        rows[0] + gap + rows[1],
        HERO_COLORS[0],
    );
    canvas.fill(
        x[1],
        y[0],
        columns[1] + gap + columns[2],
        rows[0],
        HERO_COLORS[1],
    );
    canvas.fill(
        x[1],
        y[1],
        columns[1],
        rows[1] + gap + rows[2],
        HERO_COLORS[2],
    );
    canvas.fill(x[2], y[1], columns[2], rows[1], HERO_COLORS[3]);
    canvas.fill(x[0], y[2], columns[0], rows[2], HERO_COLORS[4]);
    canvas.fill(x[2], y[2], columns[2], rows[2], HERO_COLORS[5]);
    canvas.stroke(0, 0, edge, edge, gap);
    if radius > 0.0 {
        round_corners(&mut pixels, side, radius);
    }
    pixels
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

    /// A white frame of `thickness` just inside the rectangle: the icon's outline, which keeps
    /// its edge on a dark taskbar or Dock.
    fn stroke(&mut self, x: u16, y: u16, width: u16, height: u16, thickness: u16) {
        let thickness = thickness.min(width).min(height);
        let inner_height = height.saturating_sub(2 * thickness);
        let white = (1.0, 1.0, 1.0);
        self.fill(x, y, width, thickness, white);
        self.fill(x, y + height - thickness, width, thickness, white);
        self.fill(x, y + thickness, thickness, inner_height, white);
        self.fill(
            x + width - thickness,
            y + thickness,
            thickness,
            inner_height,
            white,
        );
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

/// The images the macOS icon file holds: (type, size in pixels) — each point size at 1x and
/// 2x, 16 to 512 points, what Finder, the Dock and Launchpad pick from.
pub const ICNS_TYPES: [(&[u8; 4], u32); 10] = [
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// A `.icns` file of `images`: (type, PNG bytes) each, as [`ICNS_TYPES`] names them. Every type
/// there holds a PNG (from OS X 10.7), each after its type and length, big-endian, the lengths
/// counting their own eight bytes.
#[must_use]
pub fn icns(images: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let length: usize = 8 + images.iter().map(|(_, png)| 8 + png.len()).sum::<usize>();
    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(length as u32).to_be_bytes());
    for (kind, png) in images {
        out.extend_from_slice(*kind);
        out.extend_from_slice(&((8 + png.len()) as u32).to_be_bytes());
        out.extend_from_slice(png);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{HERO_COLORS, ICNS_TYPES, ICO_SIZES, encode, icns, ico, png, rgba};

    /// `viewers/macos/duscape.icns`, which `Duscape.app` carries as its icon (`make mac-app`), is
    /// this module's drawing, as the Windows file is. `DUSCAPE_WRITE_ICON=1` writes it.
    #[test]
    fn the_icns_file_is_the_one_drawn() {
        let best = image::codecs::png::CompressionType::Best;
        let images: Vec<(&[u8; 4], Vec<u8>)> = ICNS_TYPES
            .iter()
            .map(|&(kind, size)| (kind, encode(size, best)))
            .collect();
        let drawn = icns(&images);
        let path = ::std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../macos/duscape.icns");
        if ::std::env::var_os("DUSCAPE_WRITE_ICON").is_some() {
            ::std::fs::write(&path, &drawn).expect("writing the icon file");
        }
        let committed = ::std::fs::read(&path).unwrap_or_default();
        assert!(
            committed == drawn,
            "{} is not the icon drawn: DUSCAPE_WRITE_ICON=1 cargo test -p duscape-viewer \
             the_icns_file_is_the_one_drawn writes it",
            path.display()
        );
        // The header's length is the file's, and every image's length leads to the next.
        assert_eq!(&drawn[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(drawn[4..8].try_into().unwrap()) as usize,
            drawn.len()
        );
        let mut at = 8;
        for (kind, _) in ICNS_TYPES {
            assert_eq!(&drawn[at..at + 4], kind);
            assert_eq!(&drawn[at + 8..at + 12], b"\x89PNG");
            at += u32::from_be_bytes(drawn[at + 4..at + 8].try_into().unwrap()) as usize;
        }
        assert_eq!(at, drawn.len());
    }

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
    fn the_hero_palette_is_present_in_the_icon() {
        let pixels = rgba(64);
        for (red, green, blue) in HERO_COLORS {
            let color = [
                (red * 255.0).round() as u8,
                (green * 255.0).round() as u8,
                (blue * 255.0).round() as u8,
                255,
            ];
            assert!(pixels.as_chunks::<4>().0.contains(&color));
        }
    }

    #[test]
    fn white_outline_frames_the_icon_without_outlining_its_tiles() {
        let size = 64usize;
        let pixels = rgba(size as u32);
        let pixel = |x: usize, y: usize| &pixels[(y * size + x) * 4..][..4];
        assert_eq!(pixel(32, 1), &[255, 255, 255, 255]);
        assert_eq!(pixel(32, 4), &[46, 59, 54, 255]);
        assert_eq!(pixel(12, 6), &[184, 227, 189, 255]);
        assert_eq!(pixel(25, 12), &[46, 59, 54, 255]);
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
