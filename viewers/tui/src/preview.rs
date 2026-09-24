//! What the panel below the list shows for the entry in hand: the first lines of a text file, or
//! a picture of a PNG or JPEG, scaled down and drawn with the kitty graphics protocol — or, in a
//! terminal without it, as sixels, or failing those in cells as half blocks, two pixels to a
//! cell.
//!
//! Files are read on a thread of their own, so a slow disk never holds up the interface, and a
//! picture is decoded only once the selection has rested on it for
//! [`libdiskonaut::preview::IMAGE_DEBOUNCE`]: holding
//! an arrow key down through a folder of photos decodes none of them.

use ::std::io::{self, Write};
use ::std::path::{Path, PathBuf};
use ::std::sync::{Arc, OnceLock};
#[cfg(unix)]
use ::std::time::Duration;

use ::ratatui::layout::Rect;
use ::ratatui::style::Color;

use libdiskonaut::preview::{
    Kind, MAX_IMAGE_BYTES, Reader, Ready, Wanted, decode_picture, describe_picture,
};

use crate::clipboard::base64;

/// A picture ready for the terminal: scaled to fit the preview and encoded for it.
#[derive(Debug, PartialEq, Eq)]
pub struct PreparedImage {
    /// Which request it answers, so a stale picture is never shown and a new one is sent anew.
    pub generation: u64,
    /// A PNG for kitty graphics; for sixels, the whole sequence, ready to write.
    pub data: Vec<u8>,
    /// Cells it covers once drawn: its scaled size over the cell size, never more than it was
    /// scaled for.
    pub columns: u16,
    pub rows: u16,
    /// What it is, for the caption: `PNG 1920×1080`.
    pub description: String,
}

/// A picture drawn in cells: each cell is `▀`, its foreground the upper pixel and its
/// background the lower, so a cell holds two pixels one above the other.
#[derive(Debug, PartialEq, Eq)]
pub struct BlockImage {
    pub columns: u16,
    pub rows: u16,
    /// `columns` by `rows * 2` pixels, row by row; `None` where the picture is transparent.
    pub pixels: Vec<Option<Color>>,
    /// What it is, for the caption: `PNG 1920×1080`.
    pub description: String,
}

impl BlockImage {
    /// The pixel at `column` and half-row `half_row`, `None` where transparent or outside.
    pub fn pixel(&self, column: u16, half_row: u16) -> Option<Color> {
        if column >= self.columns || half_row >= self.rows * 2 {
            return None;
        }
        self.pixels[usize::from(half_row) * usize::from(self.columns) + usize::from(column)]
    }
}

/// How pictures are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pictures {
    /// Through the kitty graphics protocol, at full resolution.
    Kitty,
    /// As sixels, at full resolution in up to 256 colours.
    Sixel,
    /// In cells, as half blocks: in 24-bit colour, or else the xterm 256-colour palette.
    Blocks { true_color: bool },
    /// Not at all: described in words.
    Described,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Nothing to preview: a folder, several marked entries, or nothing selected.
    None,
    /// Asked for and not yet read.
    Loading,
    Text(Vec<String>),
    Image(Arc<PreparedImage>),
    Blocks(Arc<BlockImage>),
    /// A line saying what it is instead: `binary file`, `PNG image · 1920×1080`, an error.
    Info(String),
}

impl Preview {
    /// What the picture is, for the caption, when it is one.
    pub fn picture_description(&self) -> Option<&str> {
        match self {
            Preview::Image(image) => Some(&image.description),
            Preview::Blocks(image) => Some(&image.description),
            _ => None,
        }
    }
}

/// One file to preview, for a preview area of `cells`, whose cells are `cell_pixels` in size.
#[derive(Debug, Clone)]
pub struct Request {
    pub generation: u64,
    pub path: PathBuf,
    pub cells: (u16, u16),
    pub cell_pixels: (u16, u16),
    /// How pictures are shown; `Described` means they are not decoded at all.
    pub pictures: Pictures,
}

/// Decode a picture, scale it to the preview area and encode it for the terminal.
fn prepare_image(request: &Request, kind: Kind, size: u64) -> Preview {
    if request.pictures == Pictures::Described || size > MAX_IMAGE_BYTES {
        return Preview::Info(describe_picture(&request.path, kind));
    }
    let (picture, description) = match decode_picture(&request.path, kind) {
        Ok(decoded) => (decoded.image, decoded.description),
        Err(error) => return Preview::Info(error),
    };
    let (cell_width, cell_height) = (
        u32::from(request.cell_pixels.0.max(1)),
        u32::from(request.cell_pixels.1.max(1)),
    );
    let (columns, rows) = (
        u32::from(request.cells.0.max(1)),
        u32::from(request.cells.1.max(1)),
    );
    // Sixels come in bands six pixels tall, and a band cut short would spill into the row below.
    let height = match request.pictures {
        Pictures::Sixel => (rows * cell_height / 6 * 6).max(6),
        _ => rows * cell_height,
    };
    // Never scaled up: a small picture is shown at its own size.
    let scaled = libdiskonaut::preview::fit(picture, columns * cell_width, height);
    if let Pictures::Blocks { true_color } = request.pictures {
        return Preview::Blocks(Arc::new(block_image(
            &scaled,
            (cell_width, cell_height),
            (columns, rows),
            true_color,
            description,
        )));
    }
    let data = if request.pictures == Pictures::Sixel {
        sixel(&scaled.to_rgba8())
    } else {
        let mut png = Vec::new();
        if let Err(error) = scaled.write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        {
            return Preview::Info(format!("could not prepare the preview: {error}"));
        }
        png
    };
    Preview::Image(Arc::new(PreparedImage {
        generation: request.generation,
        data,
        columns: scaled.width().div_ceil(cell_width).clamp(1, columns) as u16,
        rows: scaled.height().div_ceil(cell_height).clamp(1, rows) as u16,
        description,
    }))
}

/// Resample a picture already scaled to fit to half-block pixels, which are a cell wide and half
/// a cell tall, and colour them for the terminal.
fn block_image(
    scaled: &image::DynamicImage,
    (cell_width, cell_height): (u32, u32),
    (columns, rows): (u32, u32),
    true_color: bool,
    description: String,
) -> BlockImage {
    let width = scaled.width().div_ceil(cell_width).clamp(1, columns);
    let half_rows = (scaled.height() * 2)
        .div_ceil(cell_height)
        .clamp(1, rows * 2);
    let small = scaled.thumbnail_exact(width, half_rows).to_rgba8();
    let rows = half_rows.div_ceil(2);
    let mut pixels = vec![None; (width * rows * 2) as usize];
    for (x, y, pixel) in small.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        if a >= 128 {
            pixels[(y * width + x) as usize] = Some(if true_color {
                Color::Rgb(r, g, b)
            } else {
                Color::Indexed(xterm_256(r, g, b))
            });
        }
    }
    BlockImage {
        columns: width as u16,
        rows: rows as u16,
        pixels,
        description,
    }
}

/// The nearest colour in the xterm 256-colour palette: its 6×6×6 cube or its 24 greys. The 16
/// system colours are left out, since each terminal picks its own.
fn xterm_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |value: u8| {
        (0..6)
            .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(value)).abs())
            .unwrap_or(0)
    };
    let distance = |(r2, g2, b2): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, r2) + d(g, g2) + d(b, b2)
    };
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = distance((LEVELS[ri], LEVELS[gi], LEVELS[bi]));
    let average = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    // Greys run 8, 18, … 238.
    let grey = (average.saturating_sub(3) / 10).min(23) as u8;
    let level = 8 + 10 * grey;
    if distance((level, level, level)) < cube_distance {
        232 + grey
    } else {
        cube as u8
    }
}

/// Most colours a sixel picture is given: the registers every sixel terminal of note has.
const SIXEL_COLOURS: usize = 256;

/// A picture as a sixel sequence: a palette of up to [`SIXEL_COLOURS`] fitted to it by median
/// cut, then bands of six rows, one run-length coded pass per colour in each. Pixels less than
/// half opaque are left undrawn (`P2=1`), so what is behind them shows through.
fn sixel(picture: &image::RgbaImage) -> Vec<u8> {
    let (width, height) = picture.dimensions();
    let opaque: Vec<(u32, [u8; 3])> = picture
        .pixels()
        .enumerate()
        .filter(|(_, pixel)| pixel.0[3] >= 128)
        .map(|(at, pixel)| (at as u32, [pixel.0[0], pixel.0[1], pixel.0[2]]))
        .collect();
    let (palette, indices) = median_cut(&opaque, SIXEL_COLOURS);
    // Each pixel's register, or none where it is transparent.
    let mut register = vec![None; (width * height) as usize];
    for (&(at, _), &index) in opaque.iter().zip(&indices) {
        register[at as usize] = Some(index);
    }

    // `P2=1` leaves transparent pixels alone; the raster attributes make pixels square.
    let mut out = format!("\x1bP0;1;0q\"1;1;{width};{height}").into_bytes();
    let percent = |value: u8| (u32::from(value) * 100 + 127) / 255;
    for (index, [r, g, b]) in palette.iter().enumerate() {
        out.extend(format!("#{index};2;{};{};{}", percent(*r), percent(*g), percent(*b)).bytes());
    }
    let bands = height.div_ceil(6);
    let mut passes: Vec<Option<Vec<u8>>> = vec![None; palette.len()];
    for band in 0..bands {
        passes.iter_mut().for_each(|pass| *pass = None);
        for y in band * 6..(band * 6 + 6).min(height) {
            for x in 0..width {
                if let Some(index) = register[(y * width + x) as usize] {
                    let bits =
                        passes[usize::from(index)].get_or_insert_with(|| vec![0; width as usize]);
                    bits[x as usize] |= 1 << (y - band * 6);
                }
            }
        }
        let mut first = true;
        for (index, bits) in passes.iter().enumerate() {
            let Some(bits) = bits else { continue };
            if !first {
                // Back to the start of the band for the next colour.
                out.push(b'$');
            }
            first = false;
            out.extend(format!("#{index}").bytes());
            sixel_runs(bits, &mut out);
        }
        // No new line after the last band: it could scroll the screen.
        if band + 1 < bands {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// One colour's pass over a band: each column's six bits as a character, runs of more than three
/// coded as `!count`, and the empty columns at the end left out.
fn sixel_runs(bits: &[u8], out: &mut Vec<u8>) {
    let end = bits.iter().rposition(|&b| b != 0).map_or(0, |at| at + 1);
    let mut at = 0;
    while at < end {
        let value = bits[at];
        let run = bits[at..end].iter().take_while(|&&b| b == value).count();
        let character = b'?' + value;
        if run > 3 {
            out.extend(format!("!{run}").bytes());
            out.push(character);
        } else {
            out.extend(::std::iter::repeat_n(character, run));
        }
        at += run;
    }
}

/// A palette of at most `colours` (no more than 256) for `pixels`, and each pixel's index in it.
/// The box of colours with the widest spread is split at its median along that channel until
/// there are enough boxes, and each box is coloured by its average.
fn median_cut(pixels: &[(u32, [u8; 3])], colours: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    // Each pixel's place in `pixels`, and its colour.
    let mut order: Vec<(usize, [u8; 3])> = pixels
        .iter()
        .enumerate()
        .map(|(item, &(_, colour))| (item, colour))
        .collect();
    let spread = |slice: &[(usize, [u8; 3])]| {
        (0..3)
            .map(|channel| {
                let (low, high) = slice.iter().fold((u8::MAX, 0), |(low, high), (_, c)| {
                    (low.min(c[channel]), high.max(c[channel]))
                });
                (high.saturating_sub(low), channel)
            })
            .max()
            .unwrap_or((0, 0))
    };
    // Where each box starts and ends in `order`, never empty, with its spread: measured once,
    // when the box is made, so each split costs only the box it splits.
    let mut boxes = if order.is_empty() {
        Vec::new()
    } else {
        vec![(0, order.len(), spread(&order))]
    };
    while boxes.len() < colours.min(256) {
        let widest = boxes
            .iter()
            .enumerate()
            .filter(|(_, (start, end, _))| end - start > 1)
            .max_by_key(|(_, (_, _, spread))| *spread);
        let Some((at, &(start, end, (range, channel)))) = widest else {
            break;
        };
        if range == 0 {
            break;
        }
        order[start..end].sort_unstable_by_key(|(_, c)| c[channel]);
        let middle = start + (end - start) / 2;
        boxes[at] = (start, middle, spread(&order[start..middle]));
        boxes.push((middle, end, spread(&order[middle..end])));
    }
    let mut indices = vec![0; pixels.len()];
    let palette = boxes
        .iter()
        .enumerate()
        .map(|(index, &(start, end, _))| {
            let mut sum = [0u64; 3];
            for &(item, colour) in &order[start..end] {
                for channel in 0..3 {
                    sum[channel] += u64::from(colour[channel]);
                }
                indices[item] = index as u8;
            }
            let count = (end - start) as u64;
            sum.map(|total| ((total + count / 2) / count) as u8)
        })
        .collect();
    (palette, indices)
}

/// The preview thread, as the terminal uses it: [`libdiskonaut::preview::Reader`] with pictures
/// prepared for the terminal by [`prepare_image`].
pub struct Previewer {
    reader: Reader<Request>,
}

impl Wanted for Request {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Previewer {
    pub fn spawn(on_ready: impl Fn(u64, Preview) + Send + 'static) -> Self {
        let reader = Reader::spawn(
            |request: &Request, kind, size| Ready::Picture(prepare_image(request, kind, size)),
            move |generation, ready| {
                on_ready(
                    generation,
                    match ready {
                        Ready::Info(info) => Preview::Info(info),
                        Ready::Text(lines) => Preview::Text(lines),
                        Ready::Picture(preview) => preview,
                    },
                );
            },
        );
        Previewer { reader }
    }
    pub fn request(&self, request: Request) {
        self.reader.request(request);
    }
}

/// How pictures are shown. `DISKONAUT_GRAPHICS` picks: `kitty`, `sixel`, `blocks` or `none`;
/// otherwise kitty graphics where the terminal has them, sixels where it has those instead, and
/// half blocks everywhere else.
pub fn pictures() -> Pictures {
    match ::std::env::var("DISKONAUT_GRAPHICS").as_deref() {
        Ok("none") => Pictures::Described,
        _ => match graphics_protocol() {
            Some(Protocol::Kitty) => Pictures::Kitty,
            Some(Protocol::Sixel) => Pictures::Sixel,
            None => Pictures::Blocks {
                true_color: true_color_supported(),
            },
        },
    }
}

/// Whether the terminal says it takes 24-bit colour. ssh does not forward `COLORTERM`, so a
/// remote session usually falls back to the 256-colour palette, which every terminal of note
/// has.
fn true_color_supported() -> bool {
    let var = |name| ::std::env::var(name).unwrap_or_default();
    matches!(var("COLORTERM").as_str(), "truecolor" | "24bit")
        || var("TERM").ends_with("-direct")
        || matches!(
            var("TERM").as_str(),
            "xterm-kitty" | "xterm-ghostty" | "wezterm"
        )
}

/// A way of drawing pictures in the terminal itself, rather than in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Sixel,
}

/// What was found out about the terminal: how it draws pictures, if it does, and its cell size in
/// pixels if it said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TerminalGraphics {
    protocol: Option<Protocol>,
    cell_pixels: Option<(u16, u16)>,
}

/// How the terminal draws pictures. Decided once, by [`detect`] on the first call, which must
/// come in raw mode and before anything else reads stdin.
pub fn graphics_protocol() -> Option<Protocol> {
    TERMINAL_GRAPHICS.get_or_init(detect).protocol
}

/// What [`graphics_protocol`] decided was kitty, without asking the terminal if nothing has yet:
/// on the way out a query would be answered into the shell.
pub fn kitty_known() -> bool {
    TERMINAL_GRAPHICS
        .get()
        .is_some_and(|terminal| terminal.protocol == Some(Protocol::Kitty))
}

/// The cell size in pixels the terminal gave when it was asked, for when the window size the
/// system reports has no pixels in it. `None` if it was not asked, or did not say.
pub fn queried_cell_pixels() -> Option<(u16, u16)> {
    TERMINAL_GRAPHICS
        .get()
        .and_then(|terminal| terminal.cell_pixels)
}

static TERMINAL_GRAPHICS: OnceLock<TerminalGraphics> = OnceLock::new();

/// Judged from the environment first: kitty, Ghostty and WezTerm say who they are. ssh forwards
/// none of that but `TERM`, and that is often `xterm-256color`, so otherwise the terminal is
/// asked, which also finds sixels: they are attribute 4 of its device attributes. Inside tmux
/// kitty's sequences would need a passthrough tmux does not reliably give, so only sixels are
/// asked about there — tmux answers for itself, and draws them when it was built to. Where the
/// terminal cannot be asked (Windows), Windows Terminal, foot, mlterm and iTerm2 are taken at
/// their word. `DISKONAUT_GRAPHICS` set to `kitty` or `sixel` says which without asking, and
/// `blocks` or `none` neither.
fn detect() -> TerminalGraphics {
    let var = |name| ::std::env::var(name).unwrap_or_default();
    let protocol = |protocol| TerminalGraphics {
        protocol,
        cell_pixels: None,
    };
    match var("DISKONAUT_GRAPHICS").as_str() {
        "kitty" => return protocol(Some(Protocol::Kitty)),
        "sixel" => return protocol(Some(Protocol::Sixel)),
        "blocks" | "none" => return protocol(None),
        _ => {}
    }
    let tmux = !var("TMUX").is_empty();
    let kitty = matches!(
        var("TERM").as_str(),
        "xterm-kitty" | "xterm-ghostty" | "wezterm"
    ) || !var("KITTY_WINDOW_ID").is_empty()
        || !var("GHOSTTY_RESOURCES_DIR").is_empty()
        || matches!(var("TERM_PROGRAM").as_str(), "WezTerm" | "ghostty");
    if kitty && !tmux {
        return protocol(Some(Protocol::Kitty));
    }
    let reply = query_terminal(!tmux);
    let term = var("TERM");
    let windows_terminal = !tmux && !var("WT_SESSION").is_empty();
    let sixel_by_name = !tmux
        && (windows_terminal
            || term == "foot"
            || term.starts_with("foot-")
            || term.starts_with("mlterm")
            || var("TERM_PROGRAM") == "iTerm.app");
    let reply = reply.unwrap_or_default();
    TerminalGraphics {
        protocol: if reply.kitty {
            Some(Protocol::Kitty)
        } else if reply.sixel || (sixel_by_name && !reply.answered) {
            Some(Protocol::Sixel)
        } else {
            None
        },
        // Windows Terminal lays sixels out on a cell of 10×20 pixels whatever its font, and the
        // console has no pixel size to report.
        cell_pixels: reply
            .cell_pixels
            .or((windows_terminal && !reply.answered).then_some(WINDOWS_TERMINAL_CELL)),
    }
}

/// The cell Windows Terminal draws sixels to: the VT340's, 10×20 pixels.
const WINDOWS_TERMINAL_CELL: (u16, u16) = (10, 20);

/// How long to wait for the terminal to answer [`query_terminal`]. Every terminal answers the
/// device attributes request, which ends the wait early; this only bounds one that does not, or
/// a slow link. An answer later than this reaches the input thread as stray keys.
#[cfg(unix)]
const QUERY_TIMEOUT: Duration = Duration::from_millis(1000);

/// Ask the terminal: a graphics query for a 1x1 picture (`a=q`, which stores nothing) if
/// `ask_kitty`, its cell size (`CSI 16 t`), then a primary device attributes request. A terminal
/// answers them in order, and every terminal answers the last, so it ends the wait; one without
/// kitty graphics or the cell size report just leaves those out. Keys typed while it waits are
/// dropped. `None` if the terminal could not be asked.
#[cfg(unix)]
fn query_terminal(ask_kitty: bool) -> Option<Reply> {
    use ::std::time::Instant;

    // SAFETY: isatty only inspects the descriptors.
    if unsafe { libc::isatty(0) != 1 || libc::isatty(1) != 1 } {
        return None;
    }
    let mut out = io::stdout();
    let kitty: &[u8] = if ask_kitty {
        b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"
    } else {
        b""
    };
    if out
        .write_all(kitty)
        .and_then(|()| out.write_all(b"\x1b[16t\x1b[c"))
        .and_then(|()| out.flush())
        .is_err()
    {
        return None;
    }
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut reply = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and a timeout under a second fits an int.
        let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis() as libc::c_int) };
        if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if ready <= 0 {
            break;
        }
        let mut buf = [0u8; 256];
        // SAFETY: reads into a buffer of the length given.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        reply.extend_from_slice(&buf[..n as usize]);
        if let Some(reply) = parse_reply(&reply) {
            return Some(reply);
        }
    }
    Some(Reply::default())
}

#[cfg(not(unix))]
fn query_terminal(_ask_kitty: bool) -> Option<Reply> {
    None
}

/// What the terminal answered to [`query_terminal`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Reply {
    /// Whether it answered at all: the device attributes reply came.
    answered: bool,
    kitty: bool,
    sixel: bool,
    cell_pixels: Option<(u16, u16)>,
}

/// Read the answer to [`query_terminal`]: `None` until the device attributes reply
/// (`ESC [ ? … c`) has arrived. Kitty graphics if an `OK` graphics reply for id 31 came before
/// it — an error reply means the terminal parses the protocol but would not show our pictures;
/// sixels if 4 is among its attributes; the cell size from a `CSI 6 ; height ; width t` before
/// it.
#[cfg(any(unix, test))]
fn parse_reply(reply: &[u8]) -> Option<Reply> {
    let da = reply.windows(3).position(|w| w == b"\x1b[?")?;
    let length = reply[da + 3..].iter().position(|&b| b == b'c')?;
    let attributes = &reply[da + 3..da + 3 + length];
    let before = &reply[..da];
    let id = b"\x1b_Gi=31;OK";
    let number = |text: &[u8]| ::std::str::from_utf8(text).ok()?.parse::<u16>().ok();
    let cell_pixels = before
        .windows(4)
        .position(|w| w == b"\x1b[6;")
        .and_then(|at| {
            let rest = &before[at + 4..];
            let end = rest.iter().position(|&b| b == b't')?;
            let mut sizes = rest[..end].split(|&b| b == b';');
            let height = number(sizes.next()?)?;
            let width = number(sizes.next()?)?;
            (height > 0 && width > 0).then_some((width, height))
        });
    Some(Reply {
        answered: true,
        kitty: before.windows(id.len()).any(|w| w == id),
        sixel: attributes.split(|&b| b == b';').any(|a| a == b"4"),
        cell_pixels,
    })
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
    /// Called before each frame is drawn, with what [`Graphics::show`] will be given after it.
    fn prepare(&mut self, _placement: Option<&Placement>) {}
    /// Called after each frame is drawn.
    fn show(&mut self, placement: Option<Placement>);
    /// The screen was cleared, and whatever was on it is gone.
    fn cleared(&mut self) {}
    /// Whether the picture stays whole under text drawn over it, so a dialog can cover it and
    /// leave it as it was once gone. If not, the picture is taken away while a dialog is up.
    fn stays_under_text(&self) -> bool {
        true
    }
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
    fn cleared(&mut self) {
        self.shown = None;
    }
}

/// Transmit and place a picture at its cell. `q=2` keeps the terminal from answering, which
/// would arrive on stdin as keypresses; `C=1` leaves the cursor where it was; `z=-1` puts the
/// picture under text, so a dialog drawn over the panel covers it.
pub fn kitty_place(out: &mut impl Write, placement: &Placement) -> io::Result<()> {
    let image = &placement.image;
    let data = base64(&image.data);
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

/// Pictures as sixels, written straight to the terminal after each frame. A sixel picture is
/// pixels painted into cells, with nothing to delete it by, and the frame does not know it is
/// there: cells the frame leaves blank are never redrawn. So before a frame that changes or
/// removes it, its cells are erased, which the frame then draws over as it would anyway. Text
/// written over it destroys it (in foot, all of it), hence [`Graphics::stays_under_text`].
#[derive(Default)]
pub struct SixelGraphics {
    /// The picture on screen: its generation, and the cells it covers.
    shown: Option<(u64, Rect)>,
}

fn sixel_key(placement: Option<&Placement>) -> Option<(u64, Rect)> {
    placement.map(|placement| {
        let image = &placement.image;
        (
            image.generation,
            Rect::new(placement.column, placement.row, image.columns, image.rows),
        )
    })
}

impl Graphics for SixelGraphics {
    fn prepare(&mut self, placement: Option<&Placement>) {
        if let Some((_, cells)) = self.shown
            && sixel_key(placement) != self.shown
        {
            let mut out = io::stdout().lock();
            let _ = erase_cells(&mut out, cells).and_then(|()| out.flush());
            self.shown = None;
        }
    }
    fn show(&mut self, placement: Option<Placement>) {
        let wanted = sixel_key(placement.as_ref());
        if wanted == self.shown {
            return;
        }
        let mut out = io::stdout().lock();
        if let Some((_, cells)) = self.shown {
            let _ = erase_cells(&mut out, cells);
        }
        if let Some(placement) = &placement {
            let _ = sixel_place(&mut out, placement);
        }
        let _ = out.flush();
        self.shown = wanted;
    }
    fn cleared(&mut self) {
        self.shown = None;
    }
    fn stays_under_text(&self) -> bool {
        false
    }
}

/// Draw a sixel picture from its top-left cell, the cursor and its colours put back after.
pub fn sixel_place(out: &mut impl Write, placement: &Placement) -> io::Result<()> {
    write!(
        out,
        "\x1b7\x1b[{};{}H",
        placement.row + 1,
        placement.column + 1
    )?;
    out.write_all(&placement.image.data)?;
    write!(out, "\x1b8")
}

/// Blank `cells` in the default colours, the cursor and its colours put back after.
pub fn erase_cells(out: &mut impl Write, cells: Rect) -> io::Result<()> {
    write!(out, "\x1b7\x1b[0m")?;
    for row in cells.y..cells.y + cells.height {
        write!(
            out,
            "\x1b[{};{}H\x1b[{}X",
            row + 1,
            cells.x + 1,
            cells.width
        )?;
    }
    write!(out, "\x1b8")
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
    use libdiskonaut::preview::IMAGE_DEBOUNCE;

    use ::ratatui::layout::Rect;

    use super::{
        Pictures, Placement, PreparedImage, Preview, Previewer, Reply, Request, erase_cells,
        kitty_delete, kitty_place, median_cut, parse_reply, sixel, sixel_place, sixel_runs,
        xterm_256,
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

    fn prepared(png_len: usize) -> Arc<PreparedImage> {
        Arc::new(PreparedImage {
            generation: 7,
            data: vec![0xAB; png_len],
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

    /// The answer to the query counts only once the device attributes reply ends it.
    #[test]
    fn the_reply_waits_for_device_attributes() {
        let kitty = |reply: &[u8]| parse_reply(reply).map(|reply| reply.kitty);
        assert_eq!(kitty(b""), None);
        assert_eq!(kitty(b"\x1b_Gi=31;OK\x1b\\"), None);
        assert_eq!(kitty(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;"), None);
        assert_eq!(kitty(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"), Some(true));
        assert_eq!(kitty(b"\x1b[?1;2c"), Some(false));
        assert_eq!(
            kitty(b"\x1b_Gi=31;EINVAL:bad\x1b\\\x1b[?62;22c"),
            Some(false)
        );
    }

    /// Sixels are attribute 4 of the device attributes — the whole attribute, not a digit of
    /// one — and the cell size comes from the report before them.
    #[test]
    fn the_reply_says_sixels_and_cell_size() {
        assert_eq!(
            parse_reply(b"\x1b[6;20;10t\x1b[?62;4;22c"),
            Some(Reply {
                answered: true,
                kitty: false,
                sixel: true,
                cell_pixels: Some((10, 20)),
            })
        );
        let sixel = |reply: &[u8]| parse_reply(reply).map(|reply| reply.sixel);
        assert_eq!(sixel(b"\x1b[?4c"), Some(true));
        assert_eq!(sixel(b"\x1b[?1;2c"), Some(false));
        assert_eq!(
            sixel(b"\x1b[?62;14;42c"),
            Some(false),
            "14 and 42 are not 4"
        );
        let cell = |reply: &[u8]| parse_reply(reply).and_then(|reply| reply.cell_pixels);
        assert_eq!(cell(b"\x1b[?62;4c"), None, "no report");
        assert_eq!(cell(b"\x1b[6;0;0t\x1b[?62;4c"), None, "a report of nothing");
    }

    /// A sixel sequence: square pixels, transparent background, the palette, then bands of six
    /// rows with one pass per colour; runs coded, no new line after the last band.
    #[test]
    fn sixels_are_banded_and_run_length_coded() {
        // 8×7: red on top, a transparent row, then blue — two bands.
        let picture = image::RgbaImage::from_fn(8, 7, |_, y| match y {
            0..=2 => image::Rgba([255, 0, 0, 255]),
            3 => image::Rgba([0, 0, 0, 0]),
            _ => image::Rgba([0, 0, 255, 255]),
        });
        let text = String::from_utf8(sixel(&picture)).expect("ascii");
        assert!(text.starts_with("\x1bP0;1;0q\"1;1;8;7"), "{text:?}");
        assert!(text.ends_with("\x1b\\"), "{text:?}");
        let red = text.find(";2;100;0;0").expect("red in the palette");
        let blue = text.find(";2;0;0;100").expect("blue in the palette");
        let register = |at: usize| {
            text[..at]
                .rsplit('#')
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let (red, blue) = (register(red), register(blue));
        // Band one: red in rows 0–2 (bits 0b000111 → 'F'), blue in rows 4–5 (0b110000 → 'o').
        assert!(
            text.contains(&format!("#{red}!8F$#{blue}!8o-"))
                || text.contains(&format!("#{blue}!8o$#{red}!8F-")),
            "{text:?}"
        );
        // Band two: blue in its first row only, and nothing after it.
        assert!(text.ends_with(&format!("#{blue}!8@\x1b\\")), "{text:?}");
    }

    #[test]
    fn sixel_runs_code_long_runs_and_drop_trailing_blanks() {
        let mut out = Vec::new();
        sixel_runs(&[1, 1, 1, 2, 2, 2, 2, 2, 0, 0], &mut out);
        assert_eq!(out, b"@@@!5A");
    }

    /// Median cut keeps distinct colours apart while there are registers for them, and never
    /// gives more than it was asked for.
    #[test]
    fn median_cut_fits_the_palette_to_the_picture() {
        let pixels: Vec<(u32, [u8; 3])> = (0..4)
            .flat_map(|colour| {
                (0..10).map(move |_| [[255, 0, 0], [0, 255, 0], [0, 0, 255], [9, 9, 9]][colour])
            })
            .enumerate()
            .map(|(at, colour)| (at as u32, colour))
            .collect();
        let (palette, indices) = median_cut(&pixels, 256);
        assert_eq!(palette.len(), 4);
        for (&(_, colour), &index) in pixels.iter().zip(&indices) {
            assert_eq!(palette[usize::from(index)], colour);
        }
        let gradient: Vec<(u32, [u8; 3])> = (0..=255u8)
            .flat_map(|r| (0..4u8).map(move |g| [r, g * 60, 0]))
            .enumerate()
            .map(|(at, c)| (at as u32, c))
            .collect();
        let (palette, indices) = median_cut(&gradient, 16);
        assert_eq!(palette.len(), 16);
        assert!(indices.iter().all(|&index| usize::from(index) < 16));
        assert_eq!(median_cut(&[], 256), (Vec::new(), Vec::new()));
    }

    /// Placing keeps the cursor; erasing blanks each row of the cells in the default colours.
    #[test]
    fn sixel_sequences_are_placed_and_erased() {
        let mut out = Vec::new();
        let placement = Placement {
            image: prepared(3),
            column: 3,
            row: 4,
        };
        sixel_place(&mut out, &placement).expect("write");
        assert_eq!(out, b"\x1b7\x1b[5;4H\xAB\xAB\xAB\x1b8");
        let mut out = Vec::new();
        erase_cells(&mut out, Rect::new(3, 4, 20, 2)).expect("write");
        assert_eq!(
            String::from_utf8(out).expect("ascii"),
            "\x1b7\x1b[0m\x1b[5;4H\x1b[20X\x1b[6;4H\x1b[20X\x1b8"
        );
    }

    fn request(generation: u64, path: PathBuf, pictures: Pictures) -> Request {
        Request {
            generation,
            path,
            cells: (39, 11),
            cell_pixels: (8, 16),
            pictures,
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
        previewer.request(request(1, dir.join("notes.txt"), Pictures::Kitty));
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
            previewer.request(request(3, dir.join(name), Pictures::Kitty));
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
            let decoded = image::load_from_memory(&image.data).expect("a PNG");
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
        previewer.request(request(1, dir.join("a.png"), Pictures::Kitty));
        ::std::thread::sleep(IMAGE_DEBOUNCE / 4);
        previewer.request(request(2, dir.join("b.txt"), Pictures::Kitty));
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

    /// For sixels a picture is scaled to whole bands of six pixels, so none spills into the row
    /// below, and encoded as a sixel sequence.
    #[test]
    fn a_picture_for_sixels_is_cut_to_whole_bands() {
        let dir = temp_dir("sixel");
        picture(&dir.join("tall.png"), 400, 1600, image::ImageFormat::Png);
        let (previewer, answers) = previewer();
        previewer.request(request(1, dir.join("tall.png"), Pictures::Sixel));
        let (_, preview, _) = answers
            .recv_timeout(Duration::from_secs(10))
            .expect("answer");
        let Preview::Image(image) = preview else {
            panic!("expected a picture, got {preview:?}");
        };
        // 11 rows of 16px is 176px, cut to 174; a picture a quarter as wide as tall fits as
        // 43 or 44 by 174, six cells across.
        let text = String::from_utf8_lossy(&image.data);
        let header = text
            .strip_prefix("\x1bP0;1;0q\"1;1;")
            .and_then(|rest| rest.split('#').next())
            .expect("raster attributes");
        assert!(matches!(header, "43;174" | "44;174"), "{header}");
        assert_eq!((image.columns, image.rows), (6, 11));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Without graphics a picture is described from its header instead.
    #[test]
    fn without_graphics_a_picture_is_described() {
        let dir = temp_dir("described");
        picture(&dir.join("shot.png"), 1920, 1080, image::ImageFormat::Png);
        let (previewer, answers) = previewer();
        previewer.request(request(1, dir.join("shot.png"), Pictures::Described));
        let (_, preview, _) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answer");
        assert_eq!(preview, Preview::Info("PNG image · 1920×1080".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Without kitty graphics a picture is drawn in cells: scaled the same way, then two pixels
    /// to a cell, a transparent pixel left blank.
    #[test]
    fn without_kitty_a_picture_is_drawn_in_half_blocks() {
        use ::ratatui::style::Color;
        let dir = temp_dir("blocks");
        let mut wide = image::RgbaImage::from_pixel(1600, 400, image::Rgba([255, 0, 0, 255]));
        for x in 0..1600 {
            for y in 200..400 {
                wide.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
            }
        }
        wide.save(dir.join("wide.png")).expect("write picture");
        for (true_color, red) in [(true, Color::Rgb(255, 0, 0)), (false, Color::Indexed(196))] {
            let (previewer, answers) = previewer();
            previewer.request(request(
                1,
                dir.join("wide.png"),
                Pictures::Blocks { true_color },
            ));
            let (_, preview, _) = answers
                .recv_timeout(Duration::from_secs(10))
                .expect("answer");
            let Preview::Blocks(image) = preview else {
                panic!("expected blocks, got {preview:?}");
            };
            // Scaled to 312×78px as for kitty: 39 cells across, 78/8 → 10 half rows, 5 rows.
            assert_eq!((image.columns, image.rows), (39, 5));
            assert_eq!(image.pixels.len(), 39 * 10);
            assert_eq!(image.pixel(0, 0), Some(red), "the red top half");
            assert_eq!(image.pixel(38, 9), None, "the transparent bottom half");
            assert_eq!(image.pixel(39, 0), None, "outside");
            assert!(image.description.ends_with("1600×400"));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn colours_map_to_the_nearest_of_the_256() {
        assert_eq!(xterm_256(0, 0, 0), 16);
        assert_eq!(xterm_256(255, 255, 255), 231);
        assert_eq!(xterm_256(255, 0, 0), 196);
        assert_eq!(xterm_256(128, 128, 128), 244, "a grey from the grey ramp");
        assert_eq!(xterm_256(0, 95, 135), 24);
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        let dir = temp_dir("unreadable");
        fs::write(dir.join("empty"), "").expect("write");
        fs::write(dir.join("bin"), [0u8, 1, 2, 3]).expect("write");
        fs::write(dir.join("fake.png"), b"\x89PNG\r\n\x1a\nnot really").expect("write");
        let (previewer, answers) = previewer();
        let ask = |generation, name: &str| {
            previewer.request(request(generation, dir.join(name), Pictures::Kitty));
            answers
                .recv_timeout(Duration::from_secs(5))
                .expect("answer")
                .1
        };
        assert_eq!(ask(1, "empty"), Preview::Info("empty file".into()));
        let Preview::Text(described) = ask(2, "bin") else {
            panic!("expected a description");
        };
        assert_eq!(described[0], "binary file · 4");
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
        previewer.request(request(1, fifo, Pictures::Kitty));
        let (_, preview, _) = answers
            .recv_timeout(Duration::from_secs(5))
            .expect("answered");
        assert_eq!(preview, Preview::Info("not a regular file".into()));
        let _ = fs::remove_dir_all(&dir);
    }
}
