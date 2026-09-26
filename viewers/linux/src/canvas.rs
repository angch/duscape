//! A software framebuffer in points: everything is drawn here, then handed to the X server whole.
//! There is no toolkit and no GPU; a frame is a few hundred thousand pixels, which a CPU fills in
//! well under a millisecond, and text is rasterised by `font`.
//!
//! Coordinates are the layout's points (`state::Layout`); `scale` turns them into pixels, so a
//! HiDPI screen gets the same layout drawn larger.

use duscape_viewer::state::Rect;

/// A colour as sRGB components in 0..=1, like `state::tile_color` gives.
pub type Color = (f64, f64, f64);

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Pixels per point.
    pub scale: f64,
    /// `0x00RRGGBB`, rows top to bottom.
    pub pixels: Vec<u32>,
}

/// A decoded picture, RGBA, for `blit`.
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

fn channel(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u32
}

pub fn pack((r, g, b): Color) -> u32 {
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

fn unpack(pixel: u32) -> (u32, u32, u32) {
    ((pixel >> 16) & 0xff, (pixel >> 8) & 0xff, pixel & 0xff)
}

/// `over` blended onto `under` by `alpha` (0..=256).
fn blend(under: u32, over: u32, alpha: u32) -> u32 {
    if alpha >= 256 {
        return over;
    }
    if alpha == 0 {
        return under;
    }
    let (ur, ug, ub) = unpack(under);
    let (or, og, ob) = unpack(over);
    let mix = |u: u32, o: u32| (u * (256 - alpha) + o * alpha) >> 8;
    (mix(ur, or) << 16) | (mix(ug, og) << 8) | mix(ub, ob)
}

impl Canvas {
    pub fn new(width: usize, height: usize, scale: f64) -> Self {
        Canvas {
            width,
            height,
            scale,
            pixels: vec![0; width * height],
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height];
    }

    /// The rectangle in pixels, clipped to the canvas: `(x0, y0, x1, y1)`, exclusive at the end.
    fn pixel_bounds(&self, rect: Rect) -> Option<(usize, usize, usize, usize)> {
        let x0 = (rect.x * self.scale).round().max(0.0) as usize;
        let y0 = (rect.y * self.scale).round().max(0.0) as usize;
        let x1 = ((rect.right() * self.scale).round().max(0.0) as usize).min(self.width);
        let y1 = ((rect.bottom() * self.scale).round().max(0.0) as usize).min(self.height);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(pack(color));
    }

    /// Blend a pixel by `alpha` in 0..=1.
    pub fn blend_pixel(&mut self, x: usize, y: usize, color: u32, alpha: f64) {
        if x < self.width && y < self.height {
            let at = y * self.width + x;
            self.pixels[at] = blend(self.pixels[at], color, (alpha * 256.0) as u32);
        }
    }

    pub fn fill(&mut self, rect: Rect, color: Color, alpha: f64) {
        let Some((x0, y0, x1, y1)) = self.pixel_bounds(rect) else {
            return;
        };
        let over = pack(color);
        let alpha = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
        for y in y0..y1 {
            let row = &mut self.pixels[y * self.width + x0..y * self.width + x1];
            if alpha >= 256 {
                row.fill(over);
            } else {
                for pixel in row {
                    *pixel = blend(*pixel, over, alpha);
                }
            }
        }
    }

    /// A rectangle shaded from `top` at its top edge to `bottom` at its bottom: a tile lit from
    /// above.
    pub fn gradient(&mut self, rect: Rect, top: Color, bottom: Color) {
        let Some((x0, y0, x1, y1)) = self.pixel_bounds(rect) else {
            return;
        };
        let rows = (y1 - y0).max(1) as f64;
        for y in y0..y1 {
            let t = (y - y0) as f64 / rows;
            let color = pack((
                top.0 + (bottom.0 - top.0) * t,
                top.1 + (bottom.1 - top.1) * t,
                top.2 + (bottom.2 - top.2) * t,
            ));
            self.pixels[y * self.width + x0..y * self.width + x1].fill(color);
        }
    }

    /// The outline of `rect`, `width` points thick, inside its edges.
    pub fn stroke(&mut self, rect: Rect, color: Color, alpha: f64, width: f64) {
        let width = (width * self.scale).round().max(1.0) / self.scale;
        if rect.w <= 2.0 * width || rect.h <= 2.0 * width {
            self.fill(rect, color, alpha);
            return;
        }
        self.fill(Rect::new(rect.x, rect.y, rect.w, width), color, alpha);
        self.fill(
            Rect::new(rect.x, rect.bottom() - width, rect.w, width),
            color,
            alpha,
        );
        self.fill(
            Rect::new(rect.x, rect.y + width, width, rect.h - 2.0 * width),
            color,
            alpha,
        );
        self.fill(
            Rect::new(
                rect.right() - width,
                rect.y + width,
                width,
                rect.h - 2.0 * width,
            ),
            color,
            alpha,
        );
    }

    /// A filled rectangle with rounded corners, anti-aliased at the corners.
    pub fn rounded(&mut self, rect: Rect, radius: f64, color: Color, alpha: f64) {
        let Some((x0, y0, x1, y1)) = self.pixel_bounds(rect) else {
            return;
        };
        let radius = (radius * self.scale)
            .min((x1 - x0) as f64 / 2.0)
            .min((y1 - y0) as f64 / 2.0);
        if radius < 1.0 {
            self.fill(rect, color, alpha);
            return;
        }
        let over = pack(color);
        let alpha_scaled = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
        let r = radius as usize;
        // Rows through the corners are done pixel by pixel; the rest are straight fills.
        for y in y0..y1 {
            let in_corner_rows = y < y0 + r || y + r >= y1;
            let row = y * self.width;
            if !in_corner_rows {
                for pixel in &mut self.pixels[row + x0..row + x1] {
                    *pixel = blend(*pixel, over, alpha_scaled);
                }
                continue;
            }
            let cy = if y < y0 + r {
                y0 as f64 + radius
            } else {
                y1 as f64 - radius
            };
            for x in x0..x1 {
                let cx = if x < x0 + r {
                    x0 as f64 + radius
                } else if x + r >= x1 {
                    x1 as f64 - radius
                } else {
                    self.pixels[row + x] = blend(self.pixels[row + x], over, alpha_scaled);
                    continue;
                };
                // Coverage from the distance of the pixel's centre to the corner's arc.
                let dx = x as f64 + 0.5 - cx;
                let dy = y as f64 + 0.5 - cy;
                let distance = (dx * dx + dy * dy).sqrt();
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let a = (alpha * coverage * 256.0) as u32;
                    self.pixels[row + x] = blend(self.pixels[row + x], over, a);
                }
            }
        }
    }

    /// A picture fitted into `room` (never enlarged), centred horizontally at its top, scaled by
    /// averaging the source pixels under each destination pixel. Returns where it went.
    pub fn blit(&mut self, picture: &Rgba, room: Rect) -> Option<Rect> {
        if picture.width == 0 || picture.height == 0 || room.w <= 0.0 || room.h <= 0.0 {
            return None;
        }
        let (pw, ph) = (picture.width as f64, picture.height as f64);
        // In pixels: the picture is never drawn larger than its own pixels.
        let room_px = (room.w * self.scale, room.h * self.scale);
        let fit = (room_px.0 / pw).min(room_px.1 / ph).min(1.0);
        let (w, h) = ((pw * fit).floor().max(1.0), (ph * fit).floor().max(1.0));
        let at = Rect::new(
            room.x + (room.w - w / self.scale) / 2.0,
            room.y,
            w / self.scale,
            h / self.scale,
        );
        let (x0, y0, x1, y1) = self.pixel_bounds(at)?;
        let (dw, dh) = ((x1 - x0) as f64, (y1 - y0) as f64);
        for y in y0..y1 {
            let sy0 = (((y - y0) as f64) / dh * ph) as usize;
            let sy1 = ((((y - y0 + 1) as f64) / dh * ph) as usize).clamp(sy0 + 1, picture.height);
            for x in x0..x1 {
                let sx0 = (((x - x0) as f64) / dw * pw) as usize;
                let sx1 =
                    ((((x - x0 + 1) as f64) / dw * pw) as usize).clamp(sx0 + 1, picture.width);
                let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for sy in sy0..sy1 {
                    let row = sy * picture.width * 4;
                    for sx in sx0..sx1 {
                        let p = &picture.data[row + sx * 4..row + sx * 4 + 4];
                        let alpha = u64::from(p[3]);
                        r += u64::from(p[0]) * alpha;
                        g += u64::from(p[1]) * alpha;
                        b += u64::from(p[2]) * alpha;
                        a += alpha;
                        n += 1;
                    }
                }
                if a == 0 {
                    continue;
                }
                let color = (((r / a) as u32) << 16) | (((g / a) as u32) << 8) | (b / a) as u32;
                let coverage = (a * 256 / (n * 255)) as u32;
                let at = y * self.width + x;
                self.pixels[at] = blend(self.pixels[at], color, coverage);
            }
        }
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_covers_exactly_its_pixels_and_blends_by_alpha() {
        let mut canvas = Canvas::new(10, 10, 1.0);
        canvas.clear((0.0, 0.0, 0.0));
        canvas.fill(Rect::new(2.0, 3.0, 4.0, 2.0), (1.0, 1.0, 1.0), 1.0);
        assert_eq!(canvas.pixels[3 * 10 + 2], 0xffffff);
        assert_eq!(canvas.pixels[3 * 10 + 5], 0xffffff);
        assert_eq!(canvas.pixels[3 * 10 + 6], 0);
        assert_eq!(canvas.pixels[5 * 10 + 2], 0);
        canvas.fill(Rect::new(0.0, 0.0, 1.0, 1.0), (1.0, 1.0, 1.0), 0.5);
        let (r, ..) = unpack(canvas.pixels[0]);
        assert!((120..=135).contains(&r), "half white, got {r}");
    }

    #[test]
    fn scale_turns_points_into_pixels() {
        let mut canvas = Canvas::new(20, 20, 2.0);
        canvas.fill(Rect::new(1.0, 1.0, 2.0, 2.0), (1.0, 0.0, 0.0), 1.0);
        assert_eq!(canvas.pixels[2 * 20 + 2], 0xff0000);
        assert_eq!(canvas.pixels[5 * 20 + 5], 0xff0000);
        assert_eq!(canvas.pixels[6 * 20 + 6], 0);
    }

    #[test]
    fn rounded_corners_are_left_out() {
        let mut canvas = Canvas::new(20, 20, 1.0);
        canvas.rounded(Rect::new(0.0, 0.0, 20.0, 20.0), 6.0, (1.0, 1.0, 1.0), 1.0);
        assert_eq!(canvas.pixels[0], 0, "the corner pixel is outside the arc");
        assert_eq!(canvas.pixels[10 * 20 + 10], 0xffffff);
        assert_eq!(
            canvas.pixels[10], 0xffffff,
            "the top edge's middle is filled"
        );
    }

    #[test]
    fn blit_fits_the_picture_and_averages_pixels() {
        let mut canvas = Canvas::new(10, 10, 1.0);
        // A 4×4 picture, left half red and right half blue, drawn into a 2-point-wide room.
        let mut data = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                data.extend_from_slice(if x < 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let picture = Rgba {
            width: 4,
            height: 4,
            data,
        };
        let at = canvas
            .blit(&picture, Rect::new(0.0, 0.0, 2.0, 10.0))
            .unwrap();
        assert_eq!((at.w, at.h), (2.0, 2.0), "fitted to the room's width");
        assert_eq!(canvas.pixels[0], 0xff0000);
        assert_eq!(canvas.pixels[1], 0x0000ff);
        assert_eq!(canvas.pixels[2], 0, "nothing past the picture");
    }
}
