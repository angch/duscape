//! What the panel below the list shows for the entry in hand: the first lines of a text file, or
//! a picture of a PNG or JPEG, scaled down and drawn with the kitty graphics protocol.
//!
//! Files are read on a thread of their own, so a slow disk never holds up the interface, and a
//! picture is decoded only once the selection has rested on it for [`IMAGE_DEBOUNCE`]: holding
//! an arrow key down through a folder of photos decodes none of them.

use ::std::fs::File;
use ::std::io::{self, Read, Write};
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;
use ::std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use ::std::thread;
use ::std::time::Duration;

use ::ratatui::layout::Rect;

use crate::clipboard::base64;

/// How long the selection must rest on a picture before it is decoded.
pub const IMAGE_DEBOUNCE: Duration = Duration::from_millis(100);

/// How much of a file is read to tell what it is, and to show as text.
const HEAD_BYTES: u64 = 64 * 1024;

/// Lines of text kept for the preview; more than any panel shows.
const MAX_LINES: usize = 200;

/// Pictures larger than this on disk are described rather than decoded.
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Width a tab is expanded to.
const TAB_WIDTH: usize = 4;

/// macOS's flag for a file whose contents are in iCloud, not on disk (`<sys/stat.h>`'s
/// `SF_DATALESS`). Reading one starts a download, so it is described instead.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

/// A picture ready for the terminal: scaled to fit the preview and encoded as PNG.
#[derive(Debug, PartialEq, Eq)]
pub struct PreparedImage {
    /// Which request it answers, so a stale picture is never shown and a new one is sent anew.
    pub generation: u64,
    pub png: Vec<u8>,
    /// Cells it covers once drawn: its scaled size over the cell size, never more than it was
    /// scaled for.
    pub columns: u16,
    pub rows: u16,
    /// What it is, for the caption: `PNG 1920×1080`.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Nothing to preview: a folder, several marked entries, or nothing selected.
    None,
    /// Asked for and not yet read.
    Loading,
    Text(Vec<String>),
    Image(Arc<PreparedImage>),
    /// A line saying what it is instead: `binary file`, `PNG image · 1920×1080`, an error.
    Info(String),
}

/// One file to preview, for a preview area of `cells`, whose cells are `cell_pixels` in size.
#[derive(Debug, Clone)]
pub struct Request {
    pub generation: u64,
    pub path: PathBuf,
    pub cells: (u16, u16),
    pub cell_pixels: (u16, u16),
    /// Whether the terminal can show pictures; without it they are only described.
    pub graphics: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Empty,
    Png,
    Jpeg,
    Text,
    Binary,
}

/// Tell what a file is from its first bytes: pictures by their magic numbers, text by the
/// absence of what text does not contain.
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
/// sequences above all) made visible as `?` rather than sent to the terminal, and anything not
/// UTF-8 shown lossily. A character cut short at the end of `head` is dropped.
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

/// Read the start of `path`, if it is a file whose contents can be read without side effects.
fn read_head(path: &Path) -> Result<(Vec<u8>, u64), String> {
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

fn format_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Png => "PNG",
        _ => "JPEG",
    }
}

/// Describe a picture without decoding it: its format and size, from its header.
fn describe_image(path: &Path, kind: Kind) -> Preview {
    match image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())
        .and_then(|reader| reader.into_dimensions().map_err(|error| error.to_string()))
    {
        Ok((width, height)) => {
            Preview::Info(format!("{} image · {width}×{height}", format_name(kind)))
        }
        Err(error) => Preview::Info(format!("{} image, unreadable: {error}", format_name(kind))),
    }
}

/// Decode a picture, scale it to the preview area and encode it for the terminal.
fn prepare_image(request: &Request, kind: Kind, size: u64) -> Preview {
    if !request.graphics || size > MAX_IMAGE_BYTES {
        return describe_image(&request.path, kind);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(40_000);
    limits.max_image_height = Some(40_000);
    limits.max_alloc = Some(1024 * 1024 * 1024);
    let decoded = image::ImageReader::open(&request.path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())
        .and_then(|mut reader| {
            reader.limits(limits);
            reader.decode().map_err(|error| error.to_string())
        });
    let picture = match decoded {
        Ok(picture) => picture,
        Err(error) => {
            return Preview::Info(format!("{} image, unreadable: {error}", format_name(kind)));
        }
    };
    let description = format!(
        "{} {}×{}",
        format_name(kind),
        picture.width(),
        picture.height()
    );
    let (cell_width, cell_height) = (
        u32::from(request.cell_pixels.0.max(1)),
        u32::from(request.cell_pixels.1.max(1)),
    );
    let (columns, rows) = (
        u32::from(request.cells.0.max(1)),
        u32::from(request.cells.1.max(1)),
    );
    // Never scaled up: a small picture is shown at its own size.
    let scaled = picture.thumbnail(columns * cell_width, rows * cell_height);
    let mut png = Vec::new();
    if let Err(error) = scaled.write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png) {
        return Preview::Info(format!("could not prepare the preview: {error}"));
    }
    Preview::Image(Arc::new(PreparedImage {
        generation: request.generation,
        png,
        columns: scaled.width().div_ceil(cell_width).clamp(1, columns) as u16,
        rows: scaled.height().div_ceil(cell_height).clamp(1, rows) as u16,
        description,
    }))
}

/// The preview thread's end of the conversation: send it files, and it answers each through
/// the callback it was started with, tagged with the request's generation.
pub struct Previewer {
    requests: Sender<Request>,
}

impl Previewer {
    pub fn spawn(on_ready: impl Fn(u64, Preview) + Send + 'static) -> Self {
        let (requests, incoming) = channel();
        let _ = thread::Builder::new()
            .name("previewer".to_string())
            .spawn(move || run(&incoming, &on_ready));
        Previewer { requests }
    }
    pub fn request(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

fn run(incoming: &Receiver<Request>, on_ready: &impl Fn(u64, Preview)) {
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
        let preview = match read_head(&request.path) {
            Err(reason) => Preview::Info(reason),
            Ok((head, size)) => match sniff(&head) {
                Kind::Empty => Preview::Info("empty file".to_string()),
                Kind::Binary => Preview::Info("binary file".to_string()),
                Kind::Text => Preview::Text(text_lines(&head, MAX_LINES)),
                kind @ (Kind::Png | Kind::Jpeg) => {
                    // Decode only once the selection has stayed here; a newer request means it
                    // moved on, and this picture is never needed.
                    match incoming.recv_timeout(IMAGE_DEBOUNCE) {
                        Ok(newer) => {
                            next = Some(newer);
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => prepare_image(&request, kind, size),
                    }
                }
            },
        };
        on_ready(request.generation, preview);
    }
}

/// Whether the terminal speaks the kitty graphics protocol, judged from the environment: kitty,
/// Ghostty and WezTerm do. Inside tmux the sequences would need a passthrough tmux does not
/// reliably give, so pictures are described there instead. `DISKONAUT_GRAPHICS=kitty` or `none`
/// overrides the guess.
pub fn kitty_supported() -> bool {
    let var = |name| ::std::env::var(name).unwrap_or_default();
    match var("DISKONAUT_GRAPHICS").as_str() {
        "kitty" => return true,
        "none" => return false,
        _ => {}
    }
    if !var("TMUX").is_empty() {
        return false;
    }
    var("TERM") == "xterm-kitty"
        || !var("KITTY_WINDOW_ID").is_empty()
        || !var("GHOSTTY_RESOURCES_DIR").is_empty()
        || matches!(var("TERM_PROGRAM").as_str(), "WezTerm" | "ghostty")
}

/// The id the preview's picture goes by in the terminal, so that it and only it is replaced.
const KITTY_IMAGE_ID: u32 = 0xD15C;

/// Base64 characters per chunk: the protocol's limit.
const KITTY_CHUNK: usize = 4096;

/// Where a picture goes: its top-left cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub image: Arc<PreparedImage>,
    pub column: u16,
    pub row: u16,
}

/// Something that can put the preview's picture on screen, or take it off. The app holds one;
/// tests give it a recorder.
pub trait Graphics: Send {
    fn show(&mut self, placement: Option<Placement>);
}

/// A terminal without pictures.
pub struct NoGraphics;

impl Graphics for NoGraphics {
    fn show(&mut self, _placement: Option<Placement>) {}
}

/// Pictures through the kitty graphics protocol, written straight to the terminal after each
/// frame. Only a change is sent: the same picture in the same place is left alone.
#[derive(Default)]
pub struct KittyGraphics {
    shown: Option<(u64, u16, u16)>,
}

impl Graphics for KittyGraphics {
    fn show(&mut self, placement: Option<Placement>) {
        let wanted = placement
            .as_ref()
            .map(|placement| (placement.image.generation, placement.column, placement.row));
        if wanted == self.shown {
            return;
        }
        let mut out = io::stdout().lock();
        if self.shown.is_some() {
            let _ = kitty_delete(&mut out);
        }
        if let Some(placement) = &placement {
            let _ = kitty_place(&mut out, placement);
        }
        let _ = out.flush();
        self.shown = wanted;
    }
}

/// Transmit and place a picture at its cell. `q=2` keeps the terminal from answering, which
/// would arrive on stdin as keypresses; `C=1` leaves the cursor where it was; `z=-1` puts the
/// picture under text, so a dialog drawn over the panel covers it.
pub fn kitty_place(out: &mut impl Write, placement: &Placement) -> io::Result<()> {
    let image = &placement.image;
    let data = base64(&image.png);
    // Save the cursor, go to the picture's cell, and put the cursor back after.
    write!(
        out,
        "\x1b7\x1b[{};{}H",
        placement.row + 1,
        placement.column + 1
    )?;
    let chunks: Vec<&[u8]> = data.as_bytes().chunks(KITTY_CHUNK).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let chunk = ::std::str::from_utf8(chunk).unwrap_or_default();
        if index == 0 {
            write!(
                out,
                "\x1b_Ga=T,f=100,t=d,i={KITTY_IMAGE_ID},p=1,c={},r={},C=1,z=-1,q=2,m={more};{chunk}\x1b\\",
                image.columns, image.rows
            )?;
        } else {
            write!(out, "\x1b_Gm={more},q=2;{chunk}\x1b\\")?;
        }
    }
    write!(out, "\x1b8")
}

/// Take the preview's picture off the screen and out of the terminal's memory.
pub fn kitty_delete(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={KITTY_IMAGE_ID},q=2\x1b\\")
}

/// Where the picture sits in the preview area: centred across it, at its top.
pub fn placement_in(area: Rect, image: &Arc<PreparedImage>) -> Placement {
    Placement {
        image: Arc::clone(image),
        column: area.x + area.width.saturating_sub(image.columns) / 2,
        row: area.y,
    }
}

#[cfg(test)]
mod tests {
    use ::std::fs;
    use ::std::path::PathBuf;
    use ::std::sync::mpsc::channel;
    use ::std::sync::{Arc, Mutex};
    use ::std::time::{Duration, Instant};

    use super::{
        IMAGE_DEBOUNCE, Kind, Placement, PreparedImage, Preview, Previewer, Request, kitty_delete,
        kitty_place, sniff, text_lines,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = ::std::env::temp_dir().join(format!("diskonaut_preview_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn picture(path: &::std::path::Path, width: u32, height: u32, format: image::ImageFormat) {
        let pixels = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        pixels
            .save_with_format(path, format)
            .expect("write picture");
    }

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

    fn prepared(png_len: usize) -> Arc<PreparedImage> {
        Arc::new(PreparedImage {
            generation: 7,
            png: vec![0xAB; png_len],
            columns: 20,
            rows: 10,
            description: "PNG 1×1".to_string(),
        })
    }

    /// The sequences a kitty terminal needs: quiet (`q=2`), cursor left alone (`C=1`), under
    /// text (`z=-1`), sized in cells, and chunked at 4096 characters with `m=1` on all but the
    /// last.
    #[test]
    fn kitty_sequences_are_quiet_chunked_and_placed() {
        let mut out = Vec::new();
        let placement = Placement {
            image: prepared(5000),
            column: 3,
            row: 4,
        };
        kitty_place(&mut out, &placement).expect("write");
        let text = String::from_utf8(out).expect("ascii");
        assert!(
            text.starts_with("\x1b7\x1b[5;4H"),
            "saves the cursor, goes to row 5 col 4"
        );
        assert!(text.ends_with("\x1b8"), "puts the cursor back");
        let first = text
            .find("\x1b_Ga=T,f=100,t=d,")
            .expect("transmit and place");
        let header = &text[first..text[first..].find(';').map(|at| first + at).expect(";")];
        for key in ["i=53596", "c=20", "r=10", "C=1", "z=-1", "q=2", "m=1"] {
            assert!(header.contains(key), "{key} in {header}");
        }
        // 5000 bytes are 6668 base64 characters: two chunks.
        assert_eq!(text.matches("\x1b_G").count(), 2);
        assert!(
            text.contains("\x1b_Gm=0,q=2;"),
            "the last chunk says it is the last"
        );
        for chunk in text.split("\x1b_G").skip(1) {
            let data = chunk.split(';').nth(1).unwrap_or_default();
            let data = data.split("\x1b\\").next().unwrap_or_default();
            assert!(data.len() <= 4096, "chunk of {}", data.len());
        }

        let mut out = Vec::new();
        kitty_delete(&mut out).expect("write");
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=53596,q=2\x1b\\");
    }

    fn request(generation: u64, path: PathBuf, graphics: bool) -> Request {
        Request {
            generation,
            path,
            cells: (39, 11),
            cell_pixels: (8, 16),
            graphics,
        }
    }

    /// Collects what the previewer answers.
    fn previewer() -> (
        Previewer,
        ::std::sync::mpsc::Receiver<(u64, Preview, Instant)>,
    ) {
        let (sender, answers) = channel();
        let sender = Mutex::new(sender);
        let previewer = Previewer::spawn(move |generation, preview| {
            let _ = sender
                .lock()
                .expect("sender")
                .send((generation, preview, Instant::now()));
        });
        (previewer, answers)
    }

    #[test]
    fn text_is_previewed_at_once() {
        let dir = temp_dir("text");
        fs::write(dir.join("notes.txt"), "first\nsecond\n").expect("write");
        let (previewer, answers) = previewer();
        let asked = Instant::now();
        previewer.request(request(1, dir.join("notes.txt"), true));
        let (generation, preview, at) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answer");
        assert_eq!(generation, 1);
        assert_eq!(
            preview,
            Preview::Text(vec!["first".into(), "second".into()])
        );
        assert!(at - asked < IMAGE_DEBOUNCE, "no debounce for text");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A picture is decoded only after the debounce, scaled to fit its area in pixels, and
    /// sized in cells to match.
    #[test]
    fn a_picture_is_scaled_to_the_preview_after_the_debounce() {
        let dir = temp_dir("picture");
        for (name, format) in [
            ("wide.png", image::ImageFormat::Png),
            ("wide.jpg", image::ImageFormat::Jpeg),
        ] {
            picture(&dir.join(name), 1600, 400, format);
            let (previewer, answers) = previewer();
            let asked = Instant::now();
            previewer.request(request(3, dir.join(name), true));
            let (generation, preview, at) = answers
                .recv_timeout(Duration::from_secs(10))
                .expect("answer");
            assert_eq!(generation, 3);
            assert!(
                at - asked >= IMAGE_DEBOUNCE,
                "{name}: waited for the debounce"
            );
            let Preview::Image(image) = preview else {
                panic!("{name}: expected a picture, got {preview:?}");
            };
            // 39×11 cells of 8×16 is 312×176px; 1600×400 fits as 312×78, 39×5 cells.
            assert_eq!((image.columns, image.rows), (39, 5), "{name}");
            let decoded = image::load_from_memory(&image.png).expect("a PNG");
            assert_eq!((decoded.width(), decoded.height()), (312, 78), "{name}");
            assert!(
                image.description.ends_with("1600×400"),
                "{}",
                image.description
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Moving on within the debounce means the picture is never decoded: only the latest file
    /// is answered.
    #[test]
    fn moving_on_within_the_debounce_skips_the_picture() {
        let dir = temp_dir("debounce");
        picture(&dir.join("a.png"), 64, 64, image::ImageFormat::Png);
        fs::write(dir.join("b.txt"), "b").expect("write");
        let (previewer, answers) = previewer();
        previewer.request(request(1, dir.join("a.png"), true));
        ::std::thread::sleep(IMAGE_DEBOUNCE / 4);
        previewer.request(request(2, dir.join("b.txt"), true));
        let (generation, preview, _) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answer");
        assert_eq!((generation, preview), (2, Preview::Text(vec!["b".into()])));
        assert!(
            answers.recv_timeout(IMAGE_DEBOUNCE * 3).is_err(),
            "the picture was never answered"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Without graphics a picture is described from its header instead.
    #[test]
    fn without_graphics_a_picture_is_described() {
        let dir = temp_dir("described");
        picture(&dir.join("shot.png"), 1920, 1080, image::ImageFormat::Png);
        let (previewer, answers) = previewer();
        previewer.request(request(1, dir.join("shot.png"), false));
        let (_, preview, _) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answer");
        assert_eq!(preview, Preview::Info("PNG image · 1920×1080".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        let dir = temp_dir("unreadable");
        fs::write(dir.join("empty"), "").expect("write");
        fs::write(dir.join("bin"), [0u8, 1, 2, 3]).expect("write");
        fs::write(dir.join("fake.png"), b"\x89PNG\r\n\x1a\nnot really").expect("write");
        let (previewer, answers) = previewer();
        let ask = |generation, name: &str| {
            previewer.request(request(generation, dir.join(name), true));
            answers
                .recv_timeout(Duration::from_secs(5))
                .expect("answer")
                .1
        };
        assert_eq!(ask(1, "empty"), Preview::Info("empty file".into()));
        assert_eq!(ask(2, "bin"), Preview::Info("binary file".into()));
        assert_eq!(ask(3, "."), Preview::Info("not a regular file".into()));
        let Preview::Info(broken) = ask(4, "fake.png") else {
            panic!("expected a description");
        };
        assert!(broken.starts_with("PNG image, unreadable"), "{broken}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A FIFO would block a reader forever; it is never opened.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_read() {
        let dir = temp_dir("fifo");
        let fifo = dir.join("pipe");
        let made = ::std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            return;
        }
        let (previewer, answers) = previewer();
        previewer.request(request(1, fifo, true));
        let (_, preview, _) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answered");
        assert_eq!(preview, Preview::Info("not a regular file".into()));
        let _ = fs::remove_dir_all(&dir);
    }
}
