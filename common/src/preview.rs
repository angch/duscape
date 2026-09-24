//! Looking inside a file for a viewer's preview: what it is, its first lines if it is text, and
//! its pixels if it is a PNG or JPEG.
//!
//! Only what any viewer needs is here. How a picture is scaled and drawn — kitty graphics, sixels
//! or half blocks in a terminal, a bitmap in a window — and when it is decoded (after the
//! selection has rested on it) is the viewer's business.

use ::std::fs::File;
use ::std::io::Read;
use ::std::path::Path;

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
    Png,
    Jpeg,
    Text,
    Binary,
}

impl Kind {
    /// The picture format's name, for descriptions: `PNG`, `JPEG`.
    #[must_use]
    pub fn format_name(self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            _ => "JPEG",
        }
    }
}

/// What a file is, as far as a preview goes, from its first [`HEAD_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contents {
    /// A line saying what it is instead of showing it: `empty file`, `binary file`, or why it
    /// could not be read.
    Info(String),
    /// Its first lines, safe to draw. For a binary file, what can be said about it instead:
    /// `binary file · 1.7M`, then where its blocks are (see [`crate::placement`]).
    Text(Vec<String>),
    /// A picture, not yet decoded, and the file's size.
    Picture { kind: Kind, size: u64 },
}

/// Read enough of `path` to say what it is. Only regular files are read, and on macOS not one
/// whose contents are only in iCloud.
#[must_use]
pub fn read(path: &Path) -> Contents {
    match read_head(path) {
        Err(reason) => Contents::Info(reason),
        Ok((head, size)) => match sniff(&head) {
            Kind::Empty => Contents::Info("empty file".to_string()),
            Kind::Binary => Contents::Text(describe_binary(path, size)),
            Kind::Text => Contents::Text(text_lines(&head, MAX_LINES)),
            kind @ (Kind::Png | Kind::Jpeg) => Contents::Picture { kind, size },
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

/// Tell what a file is from its first bytes: pictures by their magic numbers, text by the
/// absence of what text does not contain.
#[must_use]
pub fn sniff(head: &[u8]) -> Kind {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if head.is_empty() {
        return Kind::Empty;
    }
    if head.starts_with(PNG) {
        return Kind::Png;
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Kind::Jpeg;
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
    let metadata = ::std::fs::metadata(path).map_err(|error| error.to_string())?;
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
    Ok((head, metadata.len()))
}

/// Describe a picture without decoding it, from its header: `PNG image · 1920×1080`.
#[must_use]
pub fn describe_picture(path: &Path, kind: Kind) -> String {
    match image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())
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
    let image = image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::{Kind, sniff, text_lines};

    #[test]
    fn files_are_told_apart_by_their_first_bytes() {
        assert_eq!(sniff(b""), Kind::Empty);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Kind::Png);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Kind::Jpeg);
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
