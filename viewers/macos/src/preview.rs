//! Reading the file in hand for the preview, on a thread of its own. What a file is comes from
//! `libdiskonaut::preview`; a picture's bytes are handed over whole, for the window to decode
//! with the system's own image support.

use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc::{Sender, channel};
use ::std::time::Duration;

use libdiskonaut::preview::{Contents, describe_picture, read};

/// A request waits this long before it is read, so that holding an arrow key down through a
/// folder reads only the file it stops on.
const DEBOUNCE: Duration = Duration::from_millis(60);
/// Pictures larger than this are described, not shown.
const MAX_PICTURE: u64 = 64 * 1024 * 1024;
/// Picture formats the system decodes that `libdiskonaut::preview` does not recognise; it calls
/// them binary files, and they are handed over as pictures by their extension instead.
const OTHER_PICTURES: [&str; 8] = ["heic", "heif", "gif", "tif", "tiff", "bmp", "webp", "avif"];

/// What was read.
pub enum Loaded {
    Info(String),
    Text(Vec<String>),
    Picture { bytes: Vec<u8>, caption: String },
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
        Contents::Info(info) => match other_picture(path) {
            Some(format) if info == "binary file" => picture(path, format!("{format} image")),
            _ => Loaded::Info(info),
        },
        Contents::Text(lines) => Loaded::Text(lines),
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
    let size = path.metadata().map(|meta| meta.len()).unwrap_or(0);
    if size > MAX_PICTURE {
        return Loaded::Info(format!("{caption}, too large to preview"));
    }
    match ::std::fs::read(path) {
        Ok(bytes) => Loaded::Picture { bytes, caption },
        Err(error) => Loaded::Info(format!("{caption}, unreadable: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_read_as_lines() {
        let dir =
            ::std::env::temp_dir().join(format!("diskonaut-mac-preview-{}", ::std::process::id()));
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
    fn pictures_the_system_decodes_are_recognised_by_extension() {
        assert_eq!(
            other_picture(Path::new("a/b.HEIC")).as_deref(),
            Some("HEIC")
        );
        assert_eq!(other_picture(Path::new("a/b.bin")), None);
        assert_eq!(other_picture(Path::new("a/heic")), None);
    }
}
