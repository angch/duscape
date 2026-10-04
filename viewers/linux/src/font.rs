//! Text: the system's fonts, found through `fc-match` (or well-known paths, or `DUSCAPE_FONT`),
//! rasterised by `ab_glyph` a glyph at a time and cached. Text is drawn into a rectangle in
//! points, centred vertically, aligned and truncated with "…" like the AppKit viewer's pens.
//!
//! `ab_glyph` since 2026-10-04, `fontdue` before: Debian packages `ab_glyph` (and the
//! `ttf-parser` and rasteriser under it) and not `fontdue`, and this is all of a font this
//! window uses — a glyph's coverage and metrics at a size, a line's ascent and descent.

use ::std::cell::RefCell;
use ::std::collections::HashMap;
use ::std::path::{Path, PathBuf};
use ::std::process::Command;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};

use crate::canvas::{Canvas, Color, pack};
use duscape_viewer::state::Rect;

/// Where a text that does not fit is cut.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    Tail,
    Middle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// A glyph rasterised at one size: its ink as coverage, a row at a time, placed from the pen's
/// position on the baseline (`left`, `top`: `top` negative above it), and how far it moves the
/// pen.
struct Glyph {
    left: i64,
    top: i64,
    width: usize,
    height: usize,
    advance: f32,
    coverage: Vec<u8>,
}

/// One font file, with its rasterised glyphs kept by character and pixel size.
pub struct Face {
    font: FontVec,
    /// The font's line height over its em: `ab_glyph` scales by the line height, where a size
    /// in pixels (the toolkits', `fontdue`'s) is the em's.
    em_to_height: f32,
    cache: RefCell<HashMap<(char, u32), Glyph>>,
}

impl Face {
    pub fn load(bytes: Vec<u8>) -> Result<Face, String> {
        let font = FontVec::try_from_vec(bytes)
            .map_err(|error| format!("could not read the font: {error}"))?;
        let em = font.units_per_em().unwrap_or(1000.0);
        let em_to_height = font.height_unscaled() / em;
        Ok(Face {
            font,
            em_to_height,
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// Sizes are kept to quarter pixels, so the cache stays small across scales.
    fn key(px: f32) -> u32 {
        (px * 4.0).round() as u32
    }

    /// `ab_glyph`'s scale for an em of `px` pixels.
    fn scale(&self, px: f32) -> PxScale {
        PxScale::from(px * self.em_to_height)
    }

    fn rasterize(&self, ch: char, px: f32) -> Glyph {
        let scale = self.scale(px);
        // A character the font lacks is its glyph 0, the "missing glyph" box, like any toolkit.
        let id = self.font.glyph_id(ch);
        let advance = self.font.as_scaled(scale).h_advance(id);
        let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
        let Some(outlined) = self.font.outline_glyph(glyph) else {
            // No outline: a space, or a glyph with nothing to draw.
            return Glyph {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
                advance,
                coverage: Vec::new(),
            };
        };
        let bounds = outlined.px_bounds();
        let width = (bounds.max.x - bounds.min.x) as usize;
        let height = (bounds.max.y - bounds.min.y) as usize;
        let mut coverage = vec![0u8; width * height];
        outlined.draw(|x, y, amount| {
            if let Some(cell) = coverage.get_mut(y as usize * width + x as usize) {
                *cell = (amount.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Glyph {
            left: bounds.min.x as i64,
            top: bounds.min.y as i64,
            width,
            height,
            advance,
            coverage,
        }
    }

    fn with_glyph<R>(&self, ch: char, px: f32, use_glyph: impl FnOnce(&Glyph) -> R) -> R {
        let key = (ch, Self::key(px));
        let mut cache = self.cache.borrow_mut();
        let glyph = cache
            .entry(key)
            .or_insert_with(|| self.rasterize(ch, f32::from(key.1 as u16) / 4.0));
        use_glyph(glyph)
    }

    fn advance(&self, ch: char, px: f32) -> f32 {
        self.with_glyph(ch, px, |glyph| glyph.advance)
    }

    /// The text's width in pixels.
    pub fn width_px(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|ch| self.advance(ch, px)).sum()
    }

    /// Ascent above and descent below the baseline, in pixels (the descent positive).
    fn line(&self, px: f32) -> (f32, f32) {
        let scaled = self.font.as_scaled(self.scale(px));
        (scaled.ascent(), -scaled.descent())
    }

    /// Draw `text` with its baseline at `(x, baseline)` in pixels, in `ink`: a packed colour
    /// and an alpha.
    fn draw_px(
        &self,
        canvas: &mut Canvas,
        text: &str,
        (x, baseline): (f32, f32),
        px: f32,
        (color, alpha): (u32, f64),
    ) {
        let mut pen_x = x;
        for ch in text.chars() {
            self.with_glyph(ch, px, |glyph| {
                let left = pen_x.round() as i64 + glyph.left;
                let top = baseline.round() as i64 + glyph.top;
                for row in 0..glyph.height {
                    let y = top + row as i64;
                    if y < 0 || y >= canvas.height as i64 {
                        continue;
                    }
                    for column in 0..glyph.width {
                        let x = left + column as i64;
                        if x < 0 || x >= canvas.width as i64 {
                            continue;
                        }
                        let coverage = glyph.coverage[row * glyph.width + column];
                        if coverage > 0 {
                            canvas.blend_pixel(
                                x as usize,
                                y as usize,
                                color,
                                alpha * f64::from(coverage) / 255.0,
                            );
                        }
                    }
                }
                pen_x += glyph.advance;
            });
        }
    }
}

/// The faces a frame draws with.
pub struct Fonts {
    pub sans: Face,
    pub bold: Face,
    pub mono: Face,
}

impl Fonts {
    /// The system's sans-serif, its bold, and its monospace, by fontconfig; without fontconfig,
    /// the usual files. `DUSCAPE_FONT`, `DUSCAPE_FONT_BOLD` and `DUSCAPE_FONT_MONO` name
    /// files to use instead.
    pub fn system() -> Result<Fonts, String> {
        let sans_path = find_font(
            "DUSCAPE_FONT",
            "sans-serif",
            &[
                "dejavu/DejaVuSans.ttf",
                "liberation/LiberationSans-Regular.ttf",
                "liberation2/LiberationSans-Regular.ttf",
                "noto/NotoSans-Regular.ttf",
                "freefont/FreeSans.ttf",
                "cantarell/Cantarell-Regular.otf",
            ],
        )
        .ok_or_else(|| {
            "no font found: install fontconfig or DejaVu, or set DUSCAPE_FONT=/path/to/font.ttf"
                .to_string()
        })?;
        let bold_path = find_font(
            "DUSCAPE_FONT_BOLD",
            "sans-serif:bold",
            &[
                "dejavu/DejaVuSans-Bold.ttf",
                "liberation/LiberationSans-Bold.ttf",
                "liberation2/LiberationSans-Bold.ttf",
                "noto/NotoSans-Bold.ttf",
                "freefont/FreeSansBold.ttf",
                "cantarell/Cantarell-Bold.otf",
            ],
        )
        .unwrap_or_else(|| sans_path.clone());
        let mono_path = find_font(
            "DUSCAPE_FONT_MONO",
            "monospace",
            &[
                "dejavu/DejaVuSansMono.ttf",
                "liberation/LiberationMono-Regular.ttf",
                "liberation2/LiberationMono-Regular.ttf",
                "noto/NotoSansMono-Regular.ttf",
                "freefont/FreeMono.ttf",
            ],
        )
        .unwrap_or_else(|| sans_path.clone());
        let load = |path: &Path| {
            ::std::fs::read(path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))
                .and_then(Face::load)
                .map_err(|error| format!("{}: {error}", path.display()))
        };
        Ok(Fonts {
            sans: load(&sans_path)?,
            bold: load(&bold_path)?,
            mono: load(&mono_path)?,
        })
    }
}

/// The directories fonts are installed in, for when there is no fontconfig to ask.
const FONT_DIRS: [&str; 5] = [
    "/usr/share/fonts/truetype",
    "/usr/share/fonts/opentype",
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/usr/X11R6/lib/X11/fonts/TTF",
];

fn find_font(env: &str, pattern: &str, well_known: &[&str]) -> Option<PathBuf> {
    if let Some(path) = ::std::env::var_os(env).map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    if let Some(path) = fc_match(pattern) {
        return Some(path);
    }
    let mut dirs: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = ::std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/share/fonts"));
        dirs.push(Path::new(&home).join(".fonts"));
    }
    for dir in &dirs {
        for name in well_known {
            let path = dir.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// fontconfig's answer for `pattern`, when it is a file `ab_glyph` can read (TrueType or
/// OpenType; not a bitmap or Type 1 font).
fn fc_match(pattern: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let extension = path
        .extension()
        .map(|extension| extension.to_ascii_lowercase())?;
    let readable = ["ttf", "otf", "ttc"]
        .iter()
        .any(|ok| extension == ::std::ffi::OsStr::new(ok));
    (readable && path.is_file()).then_some(path)
}

/// A way of drawing text: a face at a size in points, a colour, an alignment and a cut.
pub struct Pen<'a> {
    pub face: &'a Face,
    pub size: f64,
    pub color: Color,
    pub alpha: f64,
    pub align: Align,
    pub cut: Cut,
}

impl Pen<'_> {
    fn px(&self, canvas_scale: f64) -> f32 {
        (self.size * canvas_scale) as f32
    }

    /// The text's width in points.
    pub fn width(&self, canvas: &Canvas, text: &str) -> f64 {
        f64::from(self.face.width_px(text, self.px(canvas.scale))) / canvas.scale
    }

    /// Draw `text` in `rect`, centred vertically, aligned, and cut with "…" if it is too wide.
    pub fn draw(&self, canvas: &mut Canvas, text: &str, rect: Rect) {
        if text.is_empty() || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let px = self.px(canvas.scale);
        let room = (rect.w * canvas.scale) as f32;
        let text = self.fit(text, room, px);
        let width = self.face.width_px(&text, px);
        let x = match self.align {
            Align::Left => rect.x * canvas.scale,
            Align::Right => rect.right() * canvas.scale - f64::from(width),
            Align::Center => (rect.x + rect.w / 2.0) * canvas.scale - f64::from(width) / 2.0,
        };
        let (ascent, descent) = self.face.line(px);
        let baseline = (rect.y + rect.h / 2.0) * canvas.scale + f64::from(ascent - descent) / 2.0;
        self.face.draw_px(
            canvas,
            &text,
            (x as f32, baseline as f32),
            px,
            (pack(self.color), self.alpha),
        );
    }

    /// `text` cut to `room` pixels with an ellipsis, at the tail or in the middle. Linear in the
    /// text's length: a 64 KiB line of minified JSON in the preview must not stall a frame.
    fn fit(&self, text: &str, room: f32, px: f32) -> String {
        if self.face.width_px(text, px) <= room {
            return text.to_string();
        }
        let ellipsis = self.face.advance('…', px);
        let room = room - ellipsis;
        if room <= 0.0 {
            return String::new();
        }
        let chars: Vec<char> = text.chars().collect();
        // The longest prefix, and suffix, whose glyphs fit in `budget` pixels.
        let prefix = |budget: f32| {
            let mut width = 0.0;
            let mut count = 0;
            for &ch in &chars {
                width += self.face.advance(ch, px);
                if width > budget {
                    break;
                }
                count += 1;
            }
            count
        };
        let suffix = |budget: f32| {
            let mut width = 0.0;
            let mut count = 0;
            for &ch in chars.iter().rev() {
                width += self.face.advance(ch, px);
                if width > budget {
                    break;
                }
                count += 1;
            }
            count
        };
        match self.cut {
            Cut::Tail => {
                let head = prefix(room);
                chars[..head].iter().chain(['…'].iter()).collect()
            }
            Cut::Middle => {
                let tail = suffix(room / 2.0);
                let tail_width: f32 = chars[chars.len() - tail..]
                    .iter()
                    .map(|&ch| self.face.advance(ch, px))
                    .sum();
                // The head takes whatever the tail left.
                let head = prefix(room - tail_width).min(chars.len() - tail);
                chars[..head]
                    .iter()
                    .chain(['…'].iter())
                    .chain(chars[chars.len() - tail..].iter())
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> Option<Face> {
        let path = find_font("DUSCAPE_FONT", "sans-serif", &["dejavu/DejaVuSans.ttf"])?;
        Face::load(::std::fs::read(path).ok()?).ok()
    }

    #[test]
    fn text_is_cut_to_fit_with_an_ellipsis() {
        let Some(face) = face() else {
            eprintln!("no system font; skipping");
            return;
        };
        let pen = Pen {
            face: &face,
            size: 12.0,
            color: (1.0, 1.0, 1.0),
            alpha: 1.0,
            align: Align::Left,
            cut: Cut::Middle,
        };
        let name = "a_rather_long_file_name_that_will_not_fit.tar.gz";
        let full = face.width_px(name, 12.0);
        let cut = pen.fit(name, full / 2.0, 12.0);
        assert!(cut.contains('…'));
        assert!(cut.starts_with("a_rath"), "the head is kept: {cut}");
        assert!(cut.ends_with("tar.gz"), "the tail is kept: {cut}");
        assert!(face.width_px(&cut, 12.0) <= full / 2.0);
        let tail = Pen {
            cut: Cut::Tail,
            ..pen
        };
        let cut = tail.fit(name, full / 2.0, 12.0);
        assert!(cut.ends_with('…'), "{cut}");
    }

    #[test]
    fn drawing_puts_ink_inside_the_rectangle() {
        let Some(face) = face() else {
            return;
        };
        let mut canvas = Canvas::new(200, 40, 1.0);
        canvas.clear((0.0, 0.0, 0.0));
        let pen = Pen {
            face: &face,
            size: 14.0,
            color: (1.0, 1.0, 1.0),
            alpha: 1.0,
            align: Align::Center,
            cut: Cut::Tail,
        };
        pen.draw(&mut canvas, "Hello", Rect::new(50.0, 0.0, 100.0, 40.0));
        let lit = |x0: usize, x1: usize| {
            (0..40).any(|y| (x0..x1).any(|x| canvas.pixels[y * 200 + x] != 0))
        };
        assert!(lit(80, 120), "text in the middle");
        assert!(!lit(0, 50), "nothing left of the rectangle");
        assert!(!lit(150, 200), "nothing right of it");
    }
}
