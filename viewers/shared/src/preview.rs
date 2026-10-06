//! Reading the file in hand for the preview, on a thread of its own. What a file is comes from
//! `libduscape::preview`; a picture's bytes are handed over whole, for the window to decode
//! with the system's own image support.

use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc::{Sender, channel};
use ::std::time::Duration;

use libduscape::preview::{Contents, describe_picture, read};

/// A request waits this long before it is read, so that holding an arrow key down through a
/// folder reads only the file it stops on.
const DEBOUNCE: Duration = Duration::from_millis(60);
/// Pictures larger than this are described, not shown.
const MAX_PICTURE: u64 = 64 * 1024 * 1024;
/// Picture formats the system decodes that `libduscape::preview` does not recognise; it calls
/// them binary files, and they are handed over as pictures by their extension instead: macOS
/// decodes them, the Linux window says it cannot. The rest (GIF, TIFF, BMP, WebP…) are known by
/// their bytes now.
const OTHER_PICTURES: [&str; 3] = ["heic", "heif", "avif"];

/// What was read.
pub enum Loaded {
    Info(String),
    Text(Vec<String>),
    /// A binary file: its description (`binary file · 1.7M`, then where its blocks are) and its
    /// first bytes as a hex dump (`libduscape::preview::hex_dump`).
    Binary {
        info: Vec<String>,
        dump: Vec<String>,
    },
    Picture {
        bytes: Vec<u8>,
        caption: String,
    },
}

/// The thread that reads files for the preview. A newer request supersedes any still waiting.
pub struct Previewer {
    requests: Sender<(u64, PathBuf)>,
}

impl Previewer {
    /// Start the thread. `deliver` gets each request's number and what was read, on that thread.
    pub fn spawn(deliver: impl Fn(u64, Loaded) + Send + 'static) -> Self {
        let (requests, inbox) = channel::<(u64, PathBuf)>();
        let _ = ::std::thread::Builder::new()
            .name("previewer".to_string())
            .spawn(move || {
                while let Ok(mut request) = inbox.recv() {
                    ::std::thread::sleep(DEBOUNCE);
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                    deliver(request.0, load(&request.1));
                }
            });
        Previewer { requests }
    }

    pub fn request(&self, generation: u64, path: PathBuf) {
        let _ = self.requests.send((generation, path));
    }
}

pub fn load(path: &Path) -> Loaded {
    match read(path) {
        Contents::Info(info) => Loaded::Info(info),
        Contents::Text(lines) => Loaded::Text(lines),
        // A binary file is described and dumped; one the system can decode is shown instead.
        Contents::Binary { info, dump } => match other_picture(path) {
            Some(format) => picture(path, format!("{format} image")),
            None => Loaded::Binary { info, dump },
        },
        Contents::Picture { kind, .. } => picture(path, describe_picture(path, kind)),
    }
}

fn other_picture(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    OTHER_PICTURES
        .contains(&extension.as_str())
        .then(|| extension.to_ascii_uppercase())
}

fn picture(path: &Path, caption: String) -> Loaded {
    // The file, or an archive's entry unpacked in memory, here on the previewer's thread.
    match libduscape::preview::picture_bytes(path, MAX_PICTURE) {
        Ok(bytes) => Loaded::Picture { bytes, caption },
        Err(why) => Loaded::Info(format!("{caption}, {why}")),
    }
}

/// A binary file's description as one paragraph, its lines joined with ` · `, for a panel that
/// wraps it to its width ([`wrap`]): one line a fact took seven lines of a panel a few hundred
/// points wide, the hex dump under them ten rows.
#[must_use]
pub fn paragraph(info: &[String]) -> String {
    info.join(" · ")
}

/// `text` word-wrapped into lines no wider than `width` by `measure` (a pen's width in the
/// panel's units): greedy, at spaces; a word wider than the line stands alone, for the pen to
/// cut with its ellipsis.
pub fn wrap(text: &str, width: f64, measure: impl Fn(&str) -> f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        if line.is_empty() {
            line.push_str(word);
            continue;
        }
        let longer = format!("{line} {word}");
        if measure(&longer) <= width {
            line = longer;
        } else {
            lines.push(::std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_wraps_at_spaces_to_the_width() {
        let by_chars = |text: &str| text.chars().count() as f64;
        assert_eq!(
            wrap(
                "binary file · 13.3M · 2 extents, fragmented",
                20.0,
                by_chars
            ),
            ["binary file · 13.3M", "· 2 extents,", "fragmented"]
        );
        assert_eq!(wrap("", 20.0, by_chars), Vec::<String>::new());
        assert_eq!(
            wrap("a word-longer-than-the-line b", 8.0, by_chars),
            ["a", "word-longer-than-the-line", "b"],
            "a word too wide stands alone, for the pen to cut"
        );
        assert_eq!(
            paragraph(&["binary file · 1M".to_string(), "1 extent".to_string()]),
            "binary file · 1M · 1 extent"
        );
    }

    #[test]
    fn text_is_read_as_lines() {
        let dir =
            ::std::env::temp_dir().join(format!("duscape-viewer-preview-{}", ::std::process::id()));
        ::std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        ::std::fs::write(&file, "one\ntwo\n").unwrap();
        let loaded = load(&file);
        ::std::fs::remove_dir_all(&dir).unwrap();
        match loaded {
            Loaded::Text(lines) => assert_eq!(lines[..2], ["one", "two"]),
            _ => panic!("a text file should load as text"),
        }
    }

    #[test]
    fn a_binary_file_is_described_and_dumped() {
        let dir =
            ::std::env::temp_dir().join(format!("duscape-viewer-binary-{}", ::std::process::id()));
        ::std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob.bin");
        ::std::fs::write(&file, [0u8, 1, 2, 0xff, 0, 0x41]).unwrap();
        let loaded = load(&file);
        ::std::fs::remove_dir_all(&dir).unwrap();
        match loaded {
            Loaded::Binary { info, dump } => {
                assert!(info[0].starts_with("binary file"), "{info:?}");
                assert_eq!(dump.len(), 1);
                assert!(dump[0].starts_with("00 01 02 FF 00 41"), "{dump:?}");
            }
            _ => panic!("a binary file should load as one"),
        }
    }

    #[test]
    fn pictures_the_system_decodes_are_recognised_by_extension() {
        assert_eq!(
            other_picture(Path::new("a/b.HEIC")).as_deref(),
            Some("HEIC")
        );
        assert_eq!(other_picture(Path::new("a/b.bin")), None);
        assert_eq!(other_picture(Path::new("a/heic")), None);
    }
}
