//! Preparing a picture for GDI: decoded and scaled to the pixels it will take, as 32-bit BGRA
//! rows blended over the panel's colour, the way `SetDIBitsToDevice` takes them. No Win32 in it,
//! so it is tested on every platform. Reading the file, and only ever the latest request, is
//! `libdiskonaut::preview::Reader`'s.

use ::std::path::{Path, PathBuf};

use libdiskonaut::preview::{Kind, MAX_IMAGE_BYTES, Ready, Wanted};

/// The panel background, which a picture's transparent pixels are blended over.
pub const PREVIEW_BACKGROUND: [u8; 3] = [32, 32, 32];

/// A decoded picture, ready to draw: 32-bit BGRA rows, top to bottom, already blended over the
/// panel's background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// What it is, for the caption: `PNG 1920×1080`.
    pub description: String,
}

/// One file to preview, and the most pixels a picture of it may take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    pub generation: u64,
    pub path: PathBuf,
    pub max_pixels: (u32, u32),
}

impl Wanted for PreviewRequest {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Decode a picture and scale it to fit `request` — never up — for the preview thread.
pub fn prepare_picture(request: &PreviewRequest, kind: Kind, size: u64) -> Ready<Picture> {
    if size > MAX_IMAGE_BYTES {
        return Ready::Info(libdiskonaut::preview::describe_picture(&request.path, kind));
    }
    let decoded = match libdiskonaut::preview::decode_picture(&request.path, kind) {
        Ok(decoded) => decoded,
        Err(error) => return Ready::Info(error),
    };
    let (width, height) = (request.max_pixels.0.max(1), request.max_pixels.1.max(1));
    let scaled = libdiskonaut::preview::fit(decoded.image, width, height).to_rgba8();
    let [back_r, back_g, back_b] = PREVIEW_BACKGROUND.map(u32::from);
    let mut bgra = Vec::with_capacity(scaled.as_raw().len());
    for pixel in scaled.pixels() {
        let [r, g, b, a] = pixel.0.map(u32::from);
        let blend = |value: u32, back: u32| ((value * a + back * (255 - a) + 127) / 255) as u8;
        bgra.extend([blend(b, back_b), blend(g, back_g), blend(r, back_r), 255]);
    }
    Ready::Picture(Picture {
        width: scaled.width(),
        height: scaled.height(),
        bgra,
        description: decoded.description,
    })
}

#[cfg(test)]
mod tests {
    use ::std::fs;

    use libdiskonaut::preview::{Kind, Ready};

    use super::{PREVIEW_BACKGROUND, PreviewRequest, prepare_picture};

    /// A picture is scaled to fit, never up, and its transparent pixels take the panel's colour.
    #[test]
    fn a_picture_is_scaled_and_blended_over_the_panel() {
        let dir = ::std::env::temp_dir().join(format!(
            "diskonaut_windows_preview_{}",
            ::std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create");
        let path = dir.join("half.png");
        let mut picture = image::RgbaImage::from_pixel(400, 100, image::Rgba([255, 0, 0, 255]));
        for x in 200..400 {
            for y in 0..100 {
                picture.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
            }
        }
        picture.save(&path).expect("write picture");
        let request = PreviewRequest {
            generation: 1,
            path,
            max_pixels: (200, 200),
        };
        let size = fs::metadata(&request.path).expect("metadata").len();
        let Ready::Picture(scaled) = prepare_picture(&request, Kind::Png, size) else {
            panic!("expected a picture");
        };
        assert_eq!((scaled.width, scaled.height), (200, 50));
        assert_eq!(scaled.bgra.len(), 200 * 50 * 4);
        assert_eq!(&scaled.bgra[..4], &[0, 0, 255, 255], "red, as BGRA");
        let [r, g, b] = PREVIEW_BACKGROUND;
        let last = scaled.bgra.len() - 4;
        assert_eq!(
            &scaled.bgra[last..],
            &[b, g, r, 255],
            "the background, as BGRA"
        );
        assert_eq!(scaled.description, "PNG 400×100");

        let small = PreviewRequest {
            max_pixels: (4000, 4000),
            ..request
        };
        let Ready::Picture(unscaled) = prepare_picture(&small, Kind::Png, size) else {
            panic!("expected a picture");
        };
        assert_eq!(
            (unscaled.width, unscaled.height),
            (400, 100),
            "never scaled up"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
