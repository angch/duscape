//! Looking inside a file for a viewer's preview: what it is, its first lines if it is text, and
//! its pixels if it is a picture `image` decodes (`Cargo.toml` says which: PNG, JPEG, WebP, GIF,
//! BMP, ICO, TIFF, QOI, HDR, PNM, DDS).
//!
//! Only what any viewer needs is here, and [`Reader`], the thread that reads files as the
//! selection moves and decodes a picture only once it has rested on one. How a picture is scaled
//! and drawn — kitty graphics, sixels or half blocks in a terminal, a bitmap in a window — is the
//! viewer's business, which it hands the reader as a closure.

use ::std::fs::File;
use ::std::io::Read;
use ::std::path::Path;
use ::std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use ::std::thread;
use ::std::time::Duration;

/// How long the selection must rest on a picture before it is decoded.
pub const IMAGE_DEBOUNCE: Duration = Duration::from_millis(100);

/// How much of a file is read to tell what it is, and to show as text.
pub const HEAD_BYTES: u64 = 64 * 1024;

/// Lines of text kept for the preview; more than any panel shows.
pub const MAX_LINES: usize = 200;

/// Pictures larger than this on disk are described rather than decoded.
pub const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Width a tab is expanded to.
const TAB_WIDTH: usize = 4;

/// macOS's flag for a file whose contents are in iCloud, not on disk (`<sys/stat.h>`'s
/// `SF_DATALESS`). Reading one starts a download, so it is described instead.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Empty,
    /// A picture `image` decodes, by its magic bytes; an animated one (GIF, WebP) shows its
    /// first frame.
    Picture(image::ImageFormat),
    Text,
    Binary,
}

impl Kind {
    /// The picture format's name, for descriptions: `PNG`, `JPEG`, `WebP`.
    #[must_use]
    pub fn format_name(self) -> &'static str {
        use image::ImageFormat;
        match self {
            Kind::Picture(format) => match format {
                ImageFormat::Png => "PNG",
                ImageFormat::Jpeg => "JPEG",
                ImageFormat::WebP => "WebP",
                ImageFormat::Gif => "GIF",
                ImageFormat::Bmp => "BMP",
                ImageFormat::Ico => "ICO",
                ImageFormat::Tiff => "TIFF",
                ImageFormat::Qoi => "QOI",
                ImageFormat::Hdr => "HDR",
                ImageFormat::Pnm => "PNM",
                ImageFormat::Dds => "DDS",
                _ => "picture",
            },
            _ => "picture",
        }
    }
}

/// What a file is, as far as a preview goes, from its first [`HEAD_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contents {
    /// A line saying what it is instead of showing it: `empty file`, `binary file`, or why it
    /// could not be read.
    Info(String),
    /// Its first lines, safe to draw.
    Text(Vec<String>),
    /// A binary file: what can be said about it — `binary file · 1.7M`, then where its blocks
    /// are (see [`crate::placement`]) — and its first bytes as a hex dump ([`hex_dump`]), for a
    /// viewer that shows one.
    Binary {
        info: Vec<String>,
        dump: Vec<String>,
    },
    /// A picture, not yet decoded, and the file's size.
    Picture { kind: Kind, size: u64 },
}

/// Read enough of `path` to say what it is. Only regular files are read, and on macOS not one
/// whose contents are only in iCloud.
#[must_use]
pub fn read(path: &Path) -> Contents {
    match head_of(path) {
        Err(reason) => Contents::Info(reason),
        Ok((head, size, in_archive)) => match sniff(&head) {
            Kind::Empty => Contents::Info("empty file".to_string()),
            Kind::Binary => Contents::Binary {
                // An archive's entry has no blocks of its own to say where they are: what it is
                // in, and how, instead.
                info: match in_archive {
                    Some(note) => vec![
                        format!("binary file · {}", crate::format::DisplaySize(size as f64)),
                        note,
                    ],
                    None => describe_binary(path, size),
                },
                dump: hex_dump(&head, HEX_LINES),
            },
            Kind::Text => Contents::Text(text_lines(&head, MAX_LINES)),
            kind @ Kind::Picture(_) => Contents::Picture { kind, size },
        },
    }
}

/// What to show for a file that cannot be shown: what it is and its size, and where on which
/// disk its blocks are, on the platforms that can say.
#[must_use]
pub fn describe_binary(path: &Path, size: u64) -> Vec<String> {
    let mut lines = vec![format!(
        "binary file · {}",
        crate::format::DisplaySize(size as f64)
    )];
    lines.extend(crate::placement::describe(path));
    lines
}

/// Lines of a hex dump, at most.
pub const HEX_LINES: usize = 256;
/// Bytes on one line of it.
pub const HEX_BYTES_PER_LINE: usize = 16;

/// `bytes` as a hex dump, [`HEX_BYTES_PER_LINE`] a line: the bytes in hex, spaced, a dash after
/// the eighth, then the same as characters, `.` for one that is not printable ASCII —
/// `00 01 02 03 04 05 06 07-08 09 0A 0B 0C 0D 0E 0F ................`. A short last line is
/// padded so the characters line up.
#[must_use]
pub fn hex_dump(bytes: &[u8], max_lines: usize) -> Vec<String> {
    bytes
        .chunks(HEX_BYTES_PER_LINE)
        .take(max_lines)
        .map(|chunk| {
            let mut line = String::with_capacity(HEX_BYTES_PER_LINE * 4 + 1);
            for slot in 0..HEX_BYTES_PER_LINE {
                if slot > 0 {
                    // The dash only between bytes: a short last line is padded with spaces.
                    let dash = slot == HEX_BYTES_PER_LINE / 2 && slot < chunk.len();
                    line.push(if dash { '-' } else { ' ' });
                }
                match chunk.get(slot) {
                    Some(byte) => line.push_str(&format!("{byte:02X}")),
                    None => line.push_str("  "),
                }
            }
            line.push(' ');
            line.extend(chunk.iter().map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    byte as char
                } else {
                    '.'
                }
            }));
            line
        })
        .collect()
}

/// Tell what a file is from its first bytes: pictures by their magic numbers (`image`'s, for
/// the formats it is built to read, and the header after them where the magic is too short to
/// trust), text by the absence of what text does not contain.
#[must_use]
pub fn sniff(head: &[u8]) -> Kind {
    if head.is_empty() {
        return Kind::Empty;
    }
    if let Ok(format) = image::guess_format(head)
        && format.reading_enabled()
        && plausible(format, head)
    {
        return Kind::Picture(format);
    }
    if head.contains(&0) {
        return Kind::Binary;
    }
    // Text has tabs, line ends and the odd form feed or escape; much else below space is a
    // sign of something that is not meant to be read.
    let control = head
        .iter()
        .filter(|&&byte| byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
        .count();
    if control * 100 > head.len() {
        Kind::Binary
    } else {
        Kind::Text
    }
}

/// Whether `head`, whose magic bytes say `format`, has the header to go with them, for the
/// formats whose magic is a couple of letters a text file could start with: `BM` (BMP), `P1`…
/// `P7` (PNM), `qoif` (QOI). The rest — eight bytes of PNG, `RIFF····WEBP` — are trusted.
fn plausible(format: image::ImageFormat, head: &[u8]) -> bool {
    use image::ImageFormat;
    match format {
        // The file header, then the bitmap header, whose first field is its own size: one of
        // the few that exist (OS/2's 12 to Windows' 124).
        ImageFormat::Bmp => head.get(14..18).is_some_and(|size| {
            let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]);
            matches!(size, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124)
        }),
        ImageFormat::Pnm => pnm_header(head),
        // Width and height, then 3 or 4 channels and a colour space of 0 or 1.
        ImageFormat::Qoi => {
            head.get(12)
                .is_some_and(|channels| matches!(channels, 3 | 4))
                && head.get(13).is_some_and(|space| matches!(space, 0 | 1))
        }
        _ => true,
    }
}

/// Whether `head` goes on as a PNM header after its `P1`…`P7`: whitespace, any `#` comment
/// lines, then the width — a number — or for `P7` its `WIDTH` line. "P3 is a phase" is text.
fn pnm_header(head: &[u8]) -> bool {
    if !head.get(2).is_some_and(u8::is_ascii_whitespace) {
        return false;
    }
    let mut rest = &head[2..];
    loop {
        rest = rest.trim_ascii_start();
        match rest.first() {
            Some(b'#') => match rest.iter().position(|&byte| byte == b'\n') {
                Some(end) => rest = &rest[end..],
                None => return false,
            },
            Some(byte) if byte.is_ascii_digit() => return head[1] != b'7',
            Some(_) => return head[1] == b'7' && rest.starts_with(b"WIDTH"),
            None => return false,
        }
    }
}

/// The first lines of `head`, ready to draw: tabs expanded, control characters (escape
/// sequences above all) made visible as `?` rather than sent to a terminal, and anything not
/// UTF-8 shown lossily. A character cut short at the end of `head` is dropped.
#[must_use]
pub fn text_lines(head: &[u8], max_lines: usize) -> Vec<String> {
    let text = match ::std::str::from_utf8(head) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() => {
            String::from_utf8_lossy(&head[..error.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(head).into_owned(),
    };
    text.lines()
        .take(max_lines)
        .map(|line| {
            let mut shown = String::new();
            for c in line.chars() {
                match c {
                    '\t' => {
                        let pad = TAB_WIDTH - shown.chars().count() % TAB_WIDTH;
                        shown.extend(::std::iter::repeat_n(' ', pad));
                    }
                    c if c.is_control() => shown.push('?'),
                    c => shown.push(c),
                }
            }
            shown
        })
        .collect()
}

/// Read the start of `path`, if it is a file whose contents can be read without side effects,
/// and its length.
pub fn read_head(path: &Path) -> Result<(Vec<u8>, u64), String> {
    head_of(path).map(|(head, size, _)| (head, size))
}

/// [`read_head`], and for an entry of an archive (a path inside one, [`crate::archive`]) its
/// start unpacked in memory, its unpacked length, and a line saying what it is in and how.
fn head_of(path: &Path) -> Result<(Vec<u8>, u64, Option<String>), String> {
    let metadata = match ::std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            let Some((archive, inner)) = crate::archive::member_of(path) else {
                return Err(error.to_string());
            };
            let (head, member) = crate::archive::read_member(&archive, &inner, HEAD_BYTES)
                .map_err(|error| {
                    format!("in {}: {error}", crate::archive::display_name(&archive))
                })?;
            let note = format!(
                "in {}, {} to {}",
                crate::archive::display_name(&archive),
                member.how(),
                crate::format::DisplaySize(member.compressed as f64)
            );
            return Ok((head, member.uncompressed, Some(note)));
        }
    };
    // A FIFO or a device would block, or never end; only regular files are read.
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        use ::std::os::macos::fs::MetadataExt;
        if metadata.st_flags() & SF_DATALESS != 0 {
            return Err("in iCloud, not downloaded".to_string());
        }
    }
    let mut head = Vec::new();
    File::open(path)
        .and_then(|file| file.take(HEAD_BYTES).read_to_end(&mut head))
        .map_err(|error| error.to_string())?;
    Ok((head, metadata.len(), None))
}

/// The largest entry of an archive unpacked whole to show it as a picture: it is unpacked in
/// memory, on the previewer's thread, and a picture's file is rarely larger.
pub const MAX_MEMBER_PICTURE: u64 = 64 * 1024 * 1024;

/// A picture's bytes, read or unpacked: anything `image` reads from.
trait Source: ::std::io::BufRead + ::std::io::Seek {}
impl<T: ::std::io::BufRead + ::std::io::Seek> Source for T {}

/// A reader of the picture at `path`: the file, or an archive's entry unpacked in memory.
fn picture_reader(path: &Path) -> Result<image::ImageReader<Box<dyn Source>>, String> {
    let source: Box<dyn Source> = match picture_member(path) {
        Some(bytes) => Box::new(::std::io::Cursor::new(bytes?)),
        None => Box::new(::std::io::BufReader::new(
            File::open(path).map_err(|error| error.to_string())?,
        )),
    };
    image::ImageReader::new(source)
        .with_guessed_format()
        .map_err(|error| error.to_string())
}

/// For a path inside an archive, the entry unpacked whole, up to [`MAX_MEMBER_PICTURE`];
/// `None` for a path on disk.
fn picture_member(path: &Path) -> Option<Result<Vec<u8>, String>> {
    let (archive, inner) = crate::archive::member_of(path)?;
    Some(
        crate::archive::read_member(&archive, &inner, MAX_MEMBER_PICTURE + 1)
            .map_err(|error| error.to_string())
            .and_then(|(bytes, member)| {
                if member.uncompressed > MAX_MEMBER_PICTURE {
                    Err("too large to unpack from an archive for a preview".to_string())
                } else {
                    Ok(bytes)
                }
            }),
    )
}

/// The bytes of the picture at `path`, at most `limit` of them: the file, or an archive's entry
/// unpacked. For a viewer that decodes the picture itself (macOS, Linux). The error says why.
pub fn picture_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    if let Some(bytes) = picture_member(path) {
        return bytes.and_then(|bytes| {
            if bytes.len() as u64 > limit {
                Err("too large to preview".to_string())
            } else {
                Ok(bytes)
            }
        });
    }
    let size = ::std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len();
    if size > limit {
        return Err("too large to preview".to_string());
    }
    ::std::fs::read(path).map_err(|error| format!("unreadable: {error}"))
}

/// Describe a picture without decoding it, from its header: `PNG image · 1920×1080`.
#[must_use]
pub fn describe_picture(path: &Path, kind: Kind) -> String {
    match picture_reader(path)
        .and_then(|reader| reader.into_dimensions().map_err(|error| error.to_string()))
    {
        Ok((width, height)) => format!("{} image · {width}×{height}", kind.format_name()),
        Err(error) => format!("{} image, unreadable: {error}", kind.format_name()),
    }
}

/// A decoded picture, and what it is for a caption: `PNG 1920×1080`.
pub struct Picture {
    pub image: image::DynamicImage,
    pub description: String,
}

/// Decode the picture at `path`, within limits that keep a hostile or enormous file from taking
/// the machine's memory: 40,000 pixels a side, 1 GiB decoded. The error says why, ready to show.
pub fn decode_picture(path: &Path, kind: Kind) -> Result<Picture, String> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(40_000);
    limits.max_image_height = Some(40_000);
    limits.max_alloc = Some(1024 * 1024 * 1024);
    let image = picture_reader(path)
        .and_then(|mut reader| {
            reader.limits(limits);
            reader.decode().map_err(|error| error.to_string())
        })
        .map_err(|error| format!("{} image, unreadable: {error}", kind.format_name()))?;
    let description = format!(
        "{} {}×{}",
        kind.format_name(),
        image.width(),
        image.height()
    );
    Ok(Picture { image, description })
}

/// `image` scaled down, keeping its shape, to fit within `width` by `height` pixels — or as it
/// is, if it already fits. `DynamicImage::thumbnail` alone would blow a small picture up to fill
/// the space, blurred.
#[must_use]
pub fn fit(image: image::DynamicImage, width: u32, height: u32) -> image::DynamicImage {
    let (width, height) = (width.max(1), height.max(1));
    if image.width() <= width && image.height() <= height {
        image
    } else {
        image.thumbnail(width, height)
    }
}

/// A file a viewer wants previewed: its path, a generation that says which request an answer is
/// for, and whatever else the viewer needs to prepare a picture of it (the area it goes in, say).
pub trait Wanted: Send + 'static {
    fn generation(&self) -> u64;
    fn path(&self) -> &Path;
}

/// What the reader answers: a line saying what the file is, its first lines, a binary file's
/// description and hex dump, or the picture the viewer prepared of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready<P> {
    Info(String),
    Text(Vec<String>),
    Binary {
        info: Vec<String>,
        dump: Vec<String>,
    },
    Picture(P),
}

/// The preview thread's end of the conversation: send it files, and it answers each through the
/// callback it was started with, tagged with the request's generation. Only the latest request
/// matters: anything queued behind it is dropped unread, and a picture is prepared only once no
/// newer request has come for [`IMAGE_DEBOUNCE`], so holding an arrow key down through a folder
/// of photos decodes none of them. Files are read on this thread, so a slow disk never holds up
/// the viewer.
pub struct Reader<R> {
    requests: Sender<R>,
}

impl<R: Wanted> Reader<R> {
    /// Start the thread. `picture` prepares a picture for display, from the request, its
    /// [`Kind`] and the file's size — typically with [`decode_picture`], or [`describe_picture`]
    /// when the viewer shows none; it may answer with [`Ready::Info`] instead.
    pub fn spawn<P: Send + 'static>(
        picture: impl Fn(&R, Kind, u64) -> Ready<P> + Send + 'static,
        on_ready: impl Fn(u64, Ready<P>) + Send + 'static,
    ) -> Self {
        let (requests, incoming) = channel();
        let _ = thread::Builder::new()
            .name("previewer".to_string())
            .spawn(move || run(&incoming, &picture, &on_ready));
        Reader { requests }
    }

    pub fn request(&self, request: R) {
        let _ = self.requests.send(request);
    }
}

fn run<R: Wanted, P>(
    incoming: &Receiver<R>,
    picture: &impl Fn(&R, Kind, u64) -> Ready<P>,
    on_ready: &impl Fn(u64, Ready<P>),
) {
    let mut next = None;
    loop {
        let mut request = match next.take() {
            Some(request) => request,
            None => match incoming.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };
        // Only the latest request matters; anything queued behind it is already out of date.
        while let Ok(newer) = incoming.try_recv() {
            request = newer;
        }
        let ready = match read(request.path()) {
            Contents::Info(info) => Ready::Info(info),
            Contents::Text(lines) => Ready::Text(lines),
            Contents::Binary { info, dump } => Ready::Binary { info, dump },
            Contents::Picture { kind, size } => {
                // Decode only once the selection has stayed here; a newer request means it moved
                // on, and this picture is never needed.
                match incoming.recv_timeout(IMAGE_DEBOUNCE) {
                    Ok(newer) => {
                        next = Some(newer);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => picture(&request, kind, size),
                }
            }
        };
        on_ready(request.generation(), ready);
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, hex_dump, sniff, text_lines};
    use image::ImageFormat;

    /// Scaled down to fit, keeping its shape, and never up.
    #[test]
    fn a_picture_is_fitted_down_and_never_up() {
        let picture = image::DynamicImage::new_rgba8(400, 100);
        let fitted = super::fit(picture.clone(), 200, 200);
        assert_eq!((fitted.width(), fitted.height()), (200, 50));
        let fitted = super::fit(picture.clone(), 4000, 4000);
        assert_eq!((fitted.width(), fitted.height()), (400, 100));
        let fitted = super::fit(picture, 0, 0);
        assert_eq!(
            (fitted.width(), fitted.height()),
            (1, 1),
            "a degenerate area is one pixel"
        );
    }

    #[test]
    fn files_are_told_apart_by_their_first_bytes() {
        assert_eq!(sniff(b""), Kind::Empty);
        assert_eq!(
            sniff(b"\x89PNG\r\n\x1a\nrest"),
            Kind::Picture(ImageFormat::Png)
        );
        assert_eq!(
            sniff(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Kind::Picture(ImageFormat::Jpeg)
        );
        assert_eq!(sniff(b"GIF89a\x01\x00"), Kind::Picture(ImageFormat::Gif));
        assert_eq!(sniff(b"II*\x00\x08\x00"), Kind::Picture(ImageFormat::Tiff));
        // Short magic, checked against the header behind it: text that starts like a picture
        // is still text.
        assert_eq!(sniff(b"BMW service record, 2026\n"), Kind::Text);
        assert_eq!(sniff(b"P3 is the third phase\n"), Kind::Text);
        assert_eq!(
            sniff(b"P3\n2 1\n255\n0 0 0 255 255 255\n"),
            Kind::Picture(ImageFormat::Pnm)
        );
        assert_eq!(
            sniff(b"P6\n# made by hand\n2 1\n255\n"),
            Kind::Picture(ImageFormat::Pnm),
            "a comment before the width"
        );
        assert_eq!(
            sniff(b"P7\nWIDTH 2\nHEIGHT 1\n"),
            Kind::Picture(ImageFormat::Pnm)
        );
        assert_eq!(sniff(b"qoifish and chips\n"), Kind::Text);
        // Formats this build does not decode are no pictures: OpenEXR, AVIF.
        assert_eq!(sniff(&[0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0]), Kind::Binary);
        assert_eq!(sniff(b"hello\n\tworld\r\n"), Kind::Text);
        assert_eq!(sniff("héllo wörld".as_bytes()), Kind::Text);
        assert_eq!(
            sniff(b"ELF\x00\x01\x02"),
            Kind::Binary,
            "a NUL is never text"
        );
        assert_eq!(
            sniff(&[1, 2, 3, 4, 5, b'a']),
            Kind::Binary,
            "mostly control bytes"
        );
        // A PNG's magic without the rest is still a PNG; a lone 0xFF 0xD8 is not a JPEG.
        assert_eq!(sniff(&[0xFF, 0xD8]), Kind::Text);
        assert_eq!(
            sniff(b"RIFF\x24\x00\x00\x00WEBPVP8L"),
            Kind::Picture(ImageFormat::WebP)
        );
        assert_eq!(
            sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "),
            Kind::Binary,
            "RIFF of another form is no picture"
        );
    }

    /// A WebP file is a picture to preview: described from its header, decoded within the
    /// limits, captioned as WebP.
    #[test]
    fn a_webp_file_is_previewed_as_a_picture() {
        let dir = ::std::env::temp_dir().join("duscape_preview_webp_test");
        let _ = ::std::fs::remove_dir_all(&dir);
        ::std::fs::create_dir_all(&dir).expect("create");
        let path = dir.join("picture.webp");
        let mut pixels = image::RgbaImage::new(32, 16);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x * 8) as u8, (y * 16) as u8, 128, 255]);
        }
        pixels
            .save_with_format(&path, image::ImageFormat::WebP)
            .expect("written as WebP");
        let webp = Kind::Picture(ImageFormat::WebP);
        match super::read(&path) {
            super::Contents::Picture { kind, .. } => assert_eq!(kind, webp),
            other => panic!("not a picture: {other:?}"),
        }
        assert_eq!(super::describe_picture(&path, webp), "WebP image · 32×16");
        let picture = super::decode_picture(&path, webp).expect("decoded");
        assert_eq!((picture.image.width(), picture.image.height()), (32, 16));
        assert_eq!(picture.description, "WebP 32×16");
        let _ = ::std::fs::remove_dir_all(&dir);
    }

    /// An archive's entry is previewed as a file is: its text, its picture, its bytes —
    /// unpacked in memory — and a binary one says what archive it is in.
    #[test]
    fn an_archives_entry_is_previewed_from_inside_it() {
        let dir = ::std::env::temp_dir().join("duscape_preview_archive_test");
        let _ = ::std::fs::remove_dir_all(&dir);
        ::std::fs::create_dir_all(&dir).expect("create");
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(6, 4)
            .write_to(&mut ::std::io::Cursor::new(&mut png), ImageFormat::Png)
            .expect("png");
        let archive = dir.join("bundle.zip");
        ::std::fs::write(
            &archive,
            crate::archive::stored_zip(&[
                ("docs/readme.txt", b"hello from inside\nsecond line\n"),
                ("art/icon.png", &png),
                ("bin/blob", &[0, 1, 2, 3, 0, 9]),
            ]),
        )
        .expect("write");
        let inside = |path: &[&str]| {
            path.iter()
                .fold(archive.clone(), |path, name| path.join(name))
        };
        assert_eq!(
            super::read(&inside(&["docs", "readme.txt"])),
            super::Contents::Text(vec![
                "hello from inside".to_string(),
                "second line".to_string()
            ])
        );
        let icon = inside(&["art", "icon.png"]);
        let png_kind = Kind::Picture(ImageFormat::Png);
        assert_eq!(super::describe_picture(&icon, png_kind), "PNG image · 6×4");
        let picture = super::decode_picture(&icon, png_kind).expect("decoded");
        assert_eq!(picture.description, "PNG 6×4");
        assert_eq!(super::picture_bytes(&icon, 1 << 20).expect("bytes"), png);
        match super::read(&inside(&["bin", "blob"])) {
            super::Contents::Binary { info, .. } => {
                assert!(info[1].starts_with("in bundle.zip, stored"), "{info:?}");
            }
            other => panic!("not binary: {other:?}"),
        }
        assert!(matches!(
            super::read(&inside(&["missing.txt"])),
            super::Contents::Info(_)
        ));
        let _ = ::std::fs::remove_dir_all(&dir);
    }

    /// Every format this build reads is previewed: written by `image` where it can write the
    /// format, recognised by its bytes, decoded, and captioned by its name.
    #[test]
    fn every_format_built_in_is_previewed() {
        let dir = ::std::env::temp_dir().join("duscape_preview_formats_test");
        let _ = ::std::fs::remove_dir_all(&dir);
        ::std::fs::create_dir_all(&dir).expect("create");
        let mut pixels = image::RgbImage::new(24, 12);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 10) as u8, (y * 20) as u8, 90]);
        }
        let picture = image::DynamicImage::ImageRgb8(pixels);
        for (format, name) in [
            (ImageFormat::Png, "PNG"),
            (ImageFormat::Jpeg, "JPEG"),
            (ImageFormat::WebP, "WebP"),
            (ImageFormat::Gif, "GIF"),
            (ImageFormat::Bmp, "BMP"),
            (ImageFormat::Ico, "ICO"),
            (ImageFormat::Tiff, "TIFF"),
            (ImageFormat::Qoi, "QOI"),
            (ImageFormat::Hdr, "HDR"),
            (ImageFormat::Pnm, "PNM"),
        ] {
            let path = dir.join(format!("picture.{}", format.extensions_str()[0]));
            let written = match format {
                // Radiance HDR is written from floating-point pixels.
                ImageFormat::Hdr => image::DynamicImage::ImageRgb32F(picture.to_rgb32f())
                    .save_with_format(&path, format),
                // An icon's picture is RGBA: `image` writes RGB's but cannot read it back.
                ImageFormat::Ico => image::DynamicImage::ImageRgba8(picture.to_rgba8())
                    .save_with_format(&path, format),
                _ => picture.save_with_format(&path, format),
            };
            written.unwrap_or_else(|error| panic!("{name} written: {error}"));
            let kind = Kind::Picture(format);
            match super::read(&path) {
                super::Contents::Picture { kind: read, .. } => assert_eq!(read, kind, "{name}"),
                other => panic!("{name} not a picture: {other:?}"),
            }
            let decoded = super::decode_picture(&path, kind)
                .unwrap_or_else(|error| panic!("{name} decoded: {error}"));
            assert_eq!(
                decoded.description,
                format!("{name} 24×12"),
                "{name} captioned"
            );
        }
        let _ = ::std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hex_dump_is_sixteen_bytes_a_line_with_the_characters_beside() {
        let bytes: Vec<u8> = (0u8..=0x11).chain(*b"Hi!~").collect();
        let lines = hex_dump(&bytes, 10);
        assert_eq!(
            lines,
            vec![
                "00 01 02 03 04 05 06 07-08 09 0A 0B 0C 0D 0E 0F ................".to_string(),
                // A short line is padded (no dash in the padding) so the characters line up.
                format!("10 11 48 69 21 7E{} ..Hi!~", " ".repeat(30)),
            ]
        );
        assert_eq!(lines[0].len(), lines[1].len() + 10, "the columns line up");
        assert_eq!(hex_dump(&[0; 100], 2).len(), 2, "cut at max_lines");
        assert!(hex_dump(&[], 2).is_empty());
        // Space is shown as itself; DEL and anything above ASCII is not printable.
        let line = &hex_dump(b" \x7f\xff", 1)[0];
        assert_eq!(&line[line.len() - 3..], " ..");
    }

    #[test]
    fn text_is_made_safe_to_draw() {
        let lines = text_lines(b"a\tb\n\x1b[2Jcleared\r\nlast", 10);
        assert_eq!(lines, vec!["a   b", "?[2Jcleared", "last"]);
        assert_eq!(text_lines(b"1\n2\n3\n4", 2), vec!["1", "2"]);
        // A character cut in half by the end of what was read is dropped, not shown as junk.
        let cut = "ok é".as_bytes();
        assert_eq!(text_lines(&cut[..cut.len() - 1], 5), vec!["ok "]);
        assert_eq!(
            text_lines(b"na\xefve latin-1", 5),
            vec!["na\u{FFFD}ve latin-1"]
        );
    }
}
