//! The window's controller: one loop over messages from the X server, the scan, rescans and the
//! previewer, each turned into a call on the shared `Viewer` and, when something changed, one
//! frame drawn and put up.

use ::std::io::Cursor;
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::mpsc::{Receiver, Sender, channel};
use ::std::time::{Duration, Instant};

use crate::backend::{self, Backend, Button, Input, Mods, keys};
use crate::canvas::{Canvas, Rgba};
use crate::draw::{self, TitleButton};
use crate::font::Fonts;
use crate::trash;
use duscape_scan::rescan::{Outcome, Rescanner};
use duscape_viewer::menu::{Action, Entry, Platform};
use duscape_viewer::preview::{Loaded, Previewer};
use duscape_viewer::scan;
use duscape_viewer::state::{Direction, Hit, Jump, Preview, Rect, Viewer, drop_later};
use libduscape::model::SizeKind;
use libduscape::{DirSummary, DisplayCount, DisplaySize, FileToDelete, FileTree, ScanOptions};

/// The window's starting size and its minimum, in points.
const WINDOW_SIZE: (f64, f64) = (1180.0, 760.0);
const MIN_SIZE: (f64, f64) = (520.0, 340.0);
/// Two clicks this close together, in time and place, are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_SLOP: f64 = 4.0;
/// Pictures are shrunk to this many pixels a side once decoded, so that drawing one is cheap.
const PICTURE_SIDE: u32 = 1024;
/// Lines of a wheel notch.
const WHEEL_ROWS: isize = 3;
/// The title bar the app draws where the windowing system draws none, in points.
pub const TITLE_BAR: f64 = 32.0;

/// What comes to the loop.
pub enum Msg {
    Input(Input),
    Batch(u64, Vec<DirSummary>),
    Done(u64, Option<Box<FileTree>>),
    Rescanned(u64, u64, Outcome),
    Preview(u64, Preview, Option<Rgba>),
    /// Time to draw again: a status message has run its course.
    Tick,
    /// Write the window to `DUSCAPE_SNAPSHOT` and quit.
    Snapshot,
}

/// What is over the window, if anything.
enum Dialog {
    None,
    /// A question before removing files: answered by the buttons, Enter/`y`, or Esc/`n`.
    Confirm {
        files: Vec<FileToDelete>,
        permanently: bool,
        title: String,
        detail: String,
    },
    Notice {
        title: String,
        detail: String,
    },
}

/// The context menu, while it is open: where it was opened, what it offers (from
/// `Viewer::context_menu`), and the item under the pointer or the keyboard.
struct Popup {
    at: (f64, f64),
    entries: Vec<Entry>,
    hover: Option<usize>,
}

/// What the context menu offers here beyond every viewer's items.
const PLATFORM: Platform = Platform {
    reveal: "Show in File Manager",
    quick_look: false,
    pathname: false,
    trash: true,
};

/// The key that does what a menu item does, shown beside it.
fn key_hint(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Open => "Enter",
        Action::CopyPath => "Ctrl+C",
        Action::CopyFullPath => "Ctrl+Shift+C",
        Action::Rescan => "r",
        Action::RescanAll => "R",
        Action::Trash => "d",
        Action::Delete => "D",
        _ => return None,
    })
}

pub struct App {
    backend: Box<dyn Backend>,
    canvas: Canvas,
    fonts: Fonts,
    viewer: Viewer,
    picture: Option<Rgba>,
    dialog: Dialog,
    /// Where the last frame drew the breadcrumbs, the dialog's buttons and the title bar's, for
    /// clicks.
    crumbs: Vec<(Rect, usize)>,
    buttons: draw::Buttons,
    title_buttons: Vec<(Rect, TitleButton)>,
    popup: Option<Popup>,
    /// Where the last frame drew the menu's items, and each one's index in its entries.
    popup_rows: Vec<(Rect, usize)>,
    options: ScanOptions,
    running: Arc<AtomicBool>,
    scans: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    previewer: Previewer,
    last_click: Option<(Instant, f64, f64)>,
    focused: bool,
    dirty: bool,
    /// Outline batches taken into the tree and not yet laid out: done once for all that
    /// arrived together, since a relayout (the nesting with it) costs more than a batch.
    outline_behind: bool,
    quit: bool,
    title: String,
    /// When a `Tick` is already on its way.
    tick_due: Option<Instant>,
    snapshot: Option<PathBuf>,
}

impl App {
    pub fn new(root: &Path, options: ScanOptions) -> Result<App, String> {
        let fonts = Fonts::system()?;
        let (tx, rx) = channel();
        let events = tx.clone();
        let backend = backend::open("duscape", WINDOW_SIZE, MIN_SIZE, move |input| {
            let _ = events.send(Msg::Input(input));
        })?;
        let (width, height, scale) = backend.size();
        let canvas = Canvas::new(
            (width * scale).round() as usize,
            (height * scale).round() as usize,
            scale,
        );
        let deliver = tx.clone();
        let previewer = Previewer::spawn(move |generation, loaded| {
            let (preview, picture) = decode(loaded);
            let _ = deliver.send(Msg::Preview(generation, preview, picture));
        });
        let kind = if options.show_apparent_size {
            SizeKind::Apparent
        } else {
            SizeKind::Disk
        };
        let mut viewer = Viewer::new(root, kind, 0);
        if !backend.decorated() {
            viewer.top_inset = TITLE_BAR;
        }
        viewer.resize(width, height);
        let mut app = App {
            viewer,
            backend,
            canvas,
            fonts,
            picture: None,
            dialog: Dialog::None,
            crumbs: Vec::new(),
            buttons: Vec::new(),
            title_buttons: Vec::new(),
            popup: None,
            popup_rows: Vec::new(),
            options,
            running: Arc::new(AtomicBool::new(false)),
            scans: 0,
            tx,
            rx,
            previewer,
            last_click: None,
            focused: true,
            dirty: true,
            outline_behind: false,
            quit: false,
            title: String::new(),
            tick_due: None,
            snapshot: ::std::env::var_os("DUSCAPE_SNAPSHOT").map(PathBuf::from),
        };
        app.start_scan(root.to_path_buf());
        Ok(app)
    }

    /// The loop. Returns when the window is closed.
    pub fn run(mut self) -> Result<(), String> {
        self.render()?;
        while let Ok(msg) = self.rx.recv() {
            self.handle(msg);
            // Whatever else has arrived meanwhile, before drawing once for all of it.
            while let Ok(msg) = self.rx.try_recv() {
                self.handle(msg);
            }
            if ::std::mem::take(&mut self.outline_behind) {
                self.viewer.catch_up();
                self.changed();
            }
            if self.quit {
                break;
            }
            if self.dirty {
                self.render()?;
            }
        }
        self.running.store(false, Ordering::Release);
        Ok(())
    }

    fn render(&mut self) -> Result<(), String> {
        self.dirty = false;
        self.crumbs = draw::frame(
            &mut self.canvas,
            &self.fonts,
            &self.viewer,
            self.picture.as_ref(),
            self.focused,
        );
        let bounds = self.viewer.layout.bounds;
        self.popup_rows = match &self.popup {
            Some(popup) => {
                let entries: Vec<_> = popup
                    .entries
                    .iter()
                    .map(|entry| {
                        let hint = match entry {
                            Entry::Item { action, .. } => key_hint(*action),
                            Entry::Separator => None,
                        };
                        (entry.clone(), hint)
                    })
                    .collect();
                draw::menu(
                    &mut self.canvas,
                    &self.fonts,
                    bounds,
                    popup.at,
                    &entries,
                    popup.hover,
                )
            }
            None => Vec::new(),
        };
        self.buttons = match &self.dialog {
            Dialog::None => Vec::new(),
            Dialog::Confirm {
                permanently,
                title,
                detail,
                ..
            } => draw::dialog(
                &mut self.canvas,
                &self.fonts,
                bounds,
                title,
                detail,
                Some(if *permanently {
                    "Delete"
                } else {
                    "Move to Trash"
                }),
                *permanently,
            ),
            Dialog::Notice { title, detail } => draw::dialog(
                &mut self.canvas,
                &self.fonts,
                bounds,
                title,
                detail,
                None,
                false,
            ),
        };
        let title = format!(
            "{} — {} — duscape",
            self.viewer.title(),
            self.viewer.subtitle()
        );
        self.title_buttons = if self.viewer.top_inset > 0.0 {
            draw::title_bar(
                &mut self.canvas,
                &self.fonts,
                Rect::new(0.0, 0.0, bounds.w, self.viewer.top_inset),
                &title,
                self.focused,
            )
        } else {
            Vec::new()
        };
        self.backend.present(&self.canvas)?;
        if title != self.title {
            self.backend.set_title(&title)?;
            self.title = title;
        }
        if let Some(left) = self.viewer.message_left() {
            self.tick_in(left + Duration::from_millis(30));
        }
        Ok(())
    }

    /// Arrange a `Tick` in `after`, unless one is due sooner.
    fn tick_in(&mut self, after: Duration) {
        let due = Instant::now() + after;
        if self.tick_due.is_some_and(|already| already <= due) {
            return;
        }
        self.tick_due = Some(due);
        let tx = self.tx.clone();
        let _ = ::std::thread::Builder::new()
            .name("ticker".to_string())
            .spawn(move || {
                ::std::thread::sleep(after);
                let _ = tx.send(Msg::Tick);
            });
    }

    // ---------------------------------------------------------------- messages

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(input) => self.input(input),
            Msg::Batch(scan_id, summaries) => {
                if scan_id == self.viewer.scan_id {
                    self.viewer.absorb_summaries(summaries);
                    self.outline_behind = true;
                }
            }
            Msg::Done(scan_id, tree) => self.scan_done(scan_id, tree),
            Msg::Rescanned(scan_id, id, outcome) => {
                if scan_id == self.viewer.scan_id {
                    self.viewer.rescan_done(id, outcome);
                    self.changed();
                } else if let Outcome::Scanned(tree, ..) = outcome {
                    drop_later(tree);
                }
            }
            Msg::Preview(generation, preview, picture) => {
                if self.viewer.preview_ready(generation, preview) {
                    self.picture = picture;
                    self.dirty = true;
                }
            }
            Msg::Tick => {
                self.tick_due = None;
                self.dirty = true;
            }
            Msg::Snapshot => {
                if let Some(path) = self.snapshot.take() {
                    if let Err(error) = self.render().and_then(|()| self.write_snapshot(&path)) {
                        eprintln!("duscape-linux: snapshot: {error}");
                    }
                    self.quit = true;
                }
            }
        }
    }

    /// After the viewer changed: ask for the preview of what is now in hand, and draw.
    fn changed(&mut self) {
        if let Some((generation, path)) = self.viewer.wanted_preview() {
            self.picture = None;
            self.previewer.request(generation, path);
        } else if matches!(self.viewer.preview, Preview::None) {
            self.picture = None;
        }
        self.dirty = true;
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Redraw => self.dirty = true,
            Input::Resized {
                width,
                height,
                scale,
            } => {
                let (w, h) = (
                    (width * scale).round() as usize,
                    (height * scale).round() as usize,
                );
                if (w, h) != (self.canvas.width, self.canvas.height) || scale != self.canvas.scale {
                    self.canvas.scale = scale;
                    self.canvas.resize(w, h);
                }
                self.viewer.resize(width, height);
                self.dirty = true;
            }
            Input::Decorated(decorated) => {
                self.viewer.top_inset = if decorated { 0.0 } else { TITLE_BAR };
                let bounds = self.viewer.layout.bounds;
                self.viewer.resize(bounds.w, bounds.h);
                self.dirty = true;
            }
            Input::Key { keysym, mods } => self.key(keysym, mods),
            Input::Button { button, x, y, mods } => self.button(button, x, y, mods),
            Input::Motion { x, y } if self.popup.is_some() => self.popup_hover(x, y),
            Input::Motion { x, y } => {
                if matches!(self.dialog, Dialog::None) && self.viewer.hover_at(x, y) {
                    self.dirty = true;
                }
            }
            Input::Leave => {
                // Nothing is under a pointer that has gone: the row, the tile and the nested
                // tile it was over all let go.
                if self.viewer.hover_at(-1.0, -1.0) {
                    self.dirty = true;
                }
            }
            Input::Focus(focused) => {
                self.focused = focused;
                // A menu closes when its window loses the keyboard, as a toolkit's does.
                if !focused {
                    self.popup = None;
                }
                self.dirty = true;
            }
            Input::Close => self.quit = true,
        }
    }

    // ---------------------------------------------------------------- keys

    fn key(&mut self, keysym: u32, mods: Mods) {
        let (shift, control) = (mods.shift, mods.control);
        let ch = char::from_u32(keysym).filter(|_| keysym < 0x100);
        if self.popup.is_some() {
            return self.popup_key(keysym);
        }
        if !matches!(self.dialog, Dialog::None) {
            match (keysym, ch) {
                (keys::RETURN | keys::KP_ENTER, _) | (_, Some('y' | 'Y')) => self.answer(true),
                (keys::ESCAPE, _) | (_, Some('n' | 'N' | 'q')) => self.answer(false),
                _ => {}
            }
            return;
        }
        match keysym {
            keys::LEFT | keys::KP_LEFT => self.viewer.arrow(Direction::Left, shift),
            keys::RIGHT | keys::KP_RIGHT => self.viewer.arrow(Direction::Right, shift),
            keys::UP | keys::KP_UP => self.viewer.arrow(Direction::Up, shift),
            keys::DOWN | keys::KP_DOWN => self.viewer.arrow(Direction::Down, shift),
            keys::PAGE_UP | keys::KP_PAGE_UP => self.viewer.jump(Jump::PageUp, shift),
            keys::PAGE_DOWN | keys::KP_PAGE_DOWN => self.viewer.jump(Jump::PageDown, shift),
            keys::HOME | keys::KP_HOME => self.viewer.jump(Jump::Home, shift),
            keys::END | keys::KP_END => self.viewer.jump(Jump::End, shift),
            keys::RETURN | keys::KP_ENTER => {
                self.viewer.enter_selected();
            }
            keys::ESCAPE | keys::BACKSPACE => {
                self.viewer.go_up();
            }
            keys::TAB | keys::ISO_LEFT_TAB => self.viewer.toggle_focus(),
            keys::DELETE | keys::KP_DELETE => return self.remove(shift),
            keys::KP_ADD => self.viewer.zoom_in(),
            keys::KP_SUBTRACT => self.viewer.zoom_out(),
            keys::KP_0 => self.viewer.reset_zoom(),
            keys::F5 => self.viewer.rescan_all(),
            _ => match (control, ch) {
                (true, Some('c' | 'C')) => return self.copy_paths(shift),
                (true, Some('a' | 'A')) => self.viewer.mark_all(),
                (true, Some('q' | 'Q' | 'w' | 'W')) => self.quit = true,
                (true, Some('r')) => self.viewer.rescan_selected(),
                (true, Some('R')) => self.viewer.rescan_all(),
                (true, _) => return,
                (false, Some('a')) => self.viewer.toggle_size(),
                (false, Some('+' | '=')) => self.viewer.zoom_in(),
                (false, Some('-' | '_')) => self.viewer.zoom_out(),
                (false, Some('0')) => self.viewer.reset_zoom(),
                (false, Some('r')) => self.viewer.rescan_selected(),
                (false, Some('R')) => self.viewer.rescan_all(),
                (false, Some('s' | 'S')) => self.viewer.toggle_sidebar(),
                (false, Some('d')) => return self.remove(false),
                (false, Some('D')) => return self.remove(true),
                (false, Some('q')) => self.quit = true,
                _ => return,
            },
        }
        self.changed();
    }

    // ---------------------------------------------------------------- the mouse

    fn button(&mut self, button: Button, x: f64, y: f64, mods: Mods) {
        if self.popup.is_some() {
            return self.popup_click(button, x, y, mods);
        }
        if !matches!(self.dialog, Dialog::None) {
            if button == Button::Left
                && let Some((_, confirms)) = self
                    .buttons
                    .iter()
                    .find(|(rect, _)| rect.contains(x, y))
                    .copied()
            {
                self.answer(confirms);
            }
            return;
        }
        if y < self.viewer.top_inset {
            self.title_bar_click(button, x, y);
            return;
        }
        match button {
            Button::Left => self.left_click(x, y, mods),
            Button::Right => self.open_popup(x, y),
            Button::WheelUp | Button::WheelDown => self.wheel(button == Button::WheelUp, x, y),
            Button::Back if self.viewer.go_up() => self.changed(),
            Button::Back | Button::Other => {}
        }
    }

    fn left_click(&mut self, x: f64, y: f64, mods: Mods) {
        if let Some(&(_, depth)) = self.crumbs.iter().find(|(rect, _)| rect.contains(x, y)) {
            self.viewer.go_to_depth(depth);
            self.last_click = None;
            return self.changed();
        }
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(at, lx, ly)| {
            now.duration_since(at) < DOUBLE_CLICK
                && (lx - x).abs() <= DOUBLE_CLICK_SLOP
                && (ly - y).abs() <= DOUBLE_CLICK_SLOP
        });
        // A folder row's expander opens it in place; the second click of a double is not a
        // second toggle.
        if let Hit::Expander(index) = self.viewer.hit(x, y) {
            if double {
                self.last_click = None;
            } else {
                self.last_click = Some((now, x, y));
                self.viewer.toggle_row(index);
            }
            return self.changed();
        }
        let mods = duscape_viewer::state::Mods {
            toggle: mods.control,
            range: mods.shift,
        };
        if double && !mods.toggle && !mods.range {
            self.last_click = None;
            if self
                .viewer
                .click(x, y, duscape_viewer::state::Mods::default())
                .is_some()
            {
                self.viewer.enter_selected();
            }
        } else {
            self.last_click = Some((now, x, y));
            if self.viewer.click(x, y, mods).is_none()
                && matches!(self.viewer.hit(x, y), Hit::SmallFiles)
            {
                self.viewer
                    .say("The entries too small for a tile are all in the list");
            }
        }
        self.changed();
    }

    /// A wheel notch: over the list it scrolls, over the treemap it zooms.
    fn wheel(&mut self, up: bool, x: f64, y: f64) {
        let layout = self.viewer.layout;
        if layout.list.is_some_and(|list| list.contains(x, y)) {
            self.viewer
                .scroll_list(if up { -WHEEL_ROWS } else { WHEEL_ROWS });
            self.viewer.hover_at(x, y);
            self.dirty = true;
        } else if layout.treemap.contains(x, y) {
            if up {
                self.viewer.zoom_in();
            } else {
                self.viewer.zoom_out();
            }
            self.viewer.hover_at(x, y);
            self.changed();
        }
    }

    /// The title bar the app draws: its buttons, a drag to move, a double-click to maximise.
    fn title_bar_click(&mut self, button: Button, x: f64, y: f64) {
        if button != Button::Left {
            return;
        }
        if let Some(&(_, which)) = self
            .title_buttons
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
        {
            match which {
                TitleButton::Close => self.quit = true,
                TitleButton::Maximize => self.backend.toggle_maximize(),
                TitleButton::Minimize => self.backend.minimize(),
            }
            return;
        }
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(at, ..)| now.duration_since(at) < DOUBLE_CLICK);
        if double {
            self.last_click = None;
            self.backend.toggle_maximize();
        } else {
            self.last_click = Some((now, x, y));
            self.backend.begin_move();
        }
    }

    // ---------------------------------------------------------------- scanning

    fn start_scan(&mut self, root: PathBuf) {
        self.running.store(false, Ordering::Release);
        let running = Arc::new(AtomicBool::new(true));
        self.running = Arc::clone(&running);
        self.scans += 1;
        let scan_id = self.scans;
        let mut options = self.options;
        let (kind, sidebar) = (self.viewer.tree.shown, self.viewer.sidebar);
        options.show_apparent_size = kind == SizeKind::Apparent;
        self.viewer.cancel_rescans();
        let mut viewer = Viewer::new(&root, kind, scan_id);
        viewer.sidebar = sidebar;
        viewer.set_tree_view(true);
        let done = self.tx.clone();
        viewer.enable_rescans(Rescanner::new(
            options,
            Arc::clone(&running),
            move |id, outcome| {
                let _ = done.send(Msg::Rescanned(scan_id, id, outcome));
            },
        ));
        viewer.top_inset = self.viewer.top_inset;
        let bounds = self.viewer.layout.bounds;
        viewer.resize(bounds.w, bounds.h);
        let old = ::std::mem::replace(&mut self.viewer, viewer);
        drop_later(::std::mem::ManuallyDrop::into_inner(old.tree));
        self.picture = None;
        let (batch, done) = (self.tx.clone(), self.tx.clone());
        scan::spawn(
            root,
            options,
            running,
            move |summaries| {
                let _ = batch.send(Msg::Batch(scan_id, summaries));
            },
            move |tree| {
                let _ = done.send(Msg::Done(scan_id, tree.map(Box::new)));
            },
        );
        self.changed();
    }

    fn scan_done(&mut self, scan_id: u64, tree: Option<Box<FileTree>>) {
        let Some(tree) = tree else {
            return;
        };
        if scan_id != self.viewer.scan_id {
            drop_later(tree);
            return;
        }
        self.viewer.finish_scan(*tree);
        self.changed();
        if self.snapshot.is_some() {
            // Long enough for the preview of what is in hand to arrive.
            let tx = self.tx.clone();
            ::std::thread::spawn(move || {
                ::std::thread::sleep(Duration::from_millis(500));
                let _ = tx.send(Msg::Snapshot);
            });
        }
    }

    /// The canvas as a PNG at `path`: a way to look at the drawing without a screen.
    fn write_snapshot(&self, path: &Path) -> Result<(), String> {
        let (w, h) = (self.canvas.width as u32, self.canvas.height as u32);
        let image = image::RgbImage::from_fn(w, h, |x, y| {
            let pixel = self.canvas.pixels[y as usize * self.canvas.width + x as usize];
            image::Rgb([(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])
        });
        image.save(path).map_err(|error| error.to_string())
    }

    // ---------------------------------------------------------------- acting on entries

    /// Copy the marked entries' paths, or the one in hand's, quoted for the shell — relative to
    /// the working directory unless `absolute` (`Viewer::copied_paths`): through a clipboard
    /// tool if there is one, else this window holds the selection.
    fn copy_paths(&mut self, absolute: bool) {
        let Some((text, label)) = self.viewer.copied_paths(absolute) else {
            return;
        };
        let copied = libduscape::clipboard::copy(&text) || self.backend.copy(&text);
        self.viewer.say(if copied {
            format!("{label} {text}")
        } else {
            "Could not copy to the clipboard".to_string()
        });
        self.dirty = true;
    }

    /// Ask before moving the targets to the Trash, or deleting them for good.
    fn remove(&mut self, permanently: bool) {
        let files = self.viewer.targets();
        if files.is_empty() {
            if self.viewer.scanning {
                self.viewer
                    .say("Deleting waits until the scan has finished");
                self.dirty = true;
            }
            return;
        }
        if let Some(name) = libduscape::delete::refused(&files) {
            self.dialog = Dialog::Notice {
                title: "This cannot be deleted".to_string(),
                detail: format!("NTFS metadata belongs to the filesystem: {name}"),
            };
            self.dirty = true;
            return;
        }
        let (title, detail) = confirmation(&files, permanently);
        self.dialog = Dialog::Confirm {
            files,
            permanently,
            title,
            detail,
        };
        self.dirty = true;
    }

    /// The dialog was answered.
    fn answer(&mut self, confirmed: bool) {
        let dialog = ::std::mem::replace(&mut self.dialog, Dialog::None);
        self.dirty = true;
        let Dialog::Confirm {
            files, permanently, ..
        } = dialog
        else {
            return;
        };
        if !confirmed {
            return;
        }
        let mut removed = Vec::new();
        let mut failures = Vec::new();
        for file in files {
            let result = if permanently {
                libduscape::delete::remove(&file).map_err(|error| error.to_string())
            } else {
                trash::trash(&file.full_path())
            };
            match result {
                Ok(()) => removed.push(file),
                Err(error) => failures.push((file, error)),
            }
        }
        let size: u128 = removed.iter().map(|file| file.size).sum();
        self.viewer.removed(&removed, permanently);
        if !removed.is_empty() {
            let items = match removed.len() {
                1 => "1 item".to_string(),
                n => format!("{} items", DisplayCount(n as u64)),
            };
            self.viewer.say(if permanently {
                format!("Deleted {items}, freeing {}", DisplaySize(size as f64))
            } else {
                format!("Moved {items} ({}) to the Trash", DisplaySize(size as f64))
            });
        }
        if let Some((file, error)) = failures.first() {
            let more = match failures.len() {
                1 => String::new(),
                n => format!(
                    "\n\n{} more could not be removed either.",
                    DisplayCount(n as u64 - 1)
                ),
            };
            self.dialog = Dialog::Notice {
                title: format!("Could not remove {}", file.full_path().display()),
                detail: format!("{error}{more}"),
            };
        }
        self.changed();
    }
}

// ---------------------------------------------------------------- the context menu

impl App {
    /// A right-click: the entry under the pointer into hand (the marks kept if it is one of
    /// them), and the menu of what can be done with it.
    fn open_popup(&mut self, x: f64, y: f64) {
        if !self.viewer.context_click(x, y) {
            return;
        }
        let entries = self.viewer.context_menu(&PLATFORM);
        if !entries.is_empty() {
            self.popup = Some(Popup {
                at: (x, y),
                entries,
                hover: None,
            });
        }
        self.changed();
    }

    fn popup_hover(&mut self, x: f64, y: f64) {
        let under = self.popup_item_at(x, y);
        if let Some(popup) = &mut self.popup
            && popup.hover != under
        {
            popup.hover = under;
            self.dirty = true;
        }
    }

    /// The item that can be chosen under a point.
    fn popup_item_at(&self, x: f64, y: f64) -> Option<usize> {
        let popup = self.popup.as_ref()?;
        self.popup_rows
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|&(_, index)| index)
            .filter(|&index| popup.entries[index].chosen().is_some())
    }

    /// A click while the menu is open: an item is chosen; anywhere else closes the menu, and a
    /// right-click there opens it again on what is under it.
    fn popup_click(&mut self, button: Button, x: f64, y: f64, mods: Mods) {
        let on_menu = self.popup_rows.iter().any(|(rect, _)| rect.contains(x, y));
        if on_menu {
            if button == Button::Left
                && let Some(index) = self.popup_item_at(x, y)
            {
                self.choose(index);
            }
            return;
        }
        self.popup = None;
        self.dirty = true;
        match button {
            Button::Right => self.open_popup(x, y),
            Button::Left => self.button(button, x, y, mods),
            _ => {}
        }
    }

    /// ↑ and ↓ move through the items that can be chosen, Enter chooses, Esc closes.
    fn popup_key(&mut self, keysym: u32) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        let choosable: Vec<usize> = (0..popup.entries.len())
            .filter(|&index| popup.entries[index].chosen().is_some())
            .collect();
        let at = popup
            .hover
            .and_then(|hover| choosable.iter().position(|&index| index == hover));
        match keysym {
            keys::DOWN | keys::KP_DOWN => {
                let next = at.map_or(0, |at| (at + 1) % choosable.len().max(1));
                popup.hover = choosable.get(next).copied();
            }
            keys::UP | keys::KP_UP => {
                let last = choosable.len().saturating_sub(1);
                let next = at.map_or(last, |at| at.checked_sub(1).unwrap_or(last));
                popup.hover = choosable.get(next).copied();
            }
            keys::RETURN | keys::KP_ENTER => {
                if let Some(index) = popup.hover {
                    return self.choose(index);
                }
            }
            keys::ESCAPE => self.popup = None,
            _ => return,
        }
        self.dirty = true;
    }

    /// Close the menu and carry out its item `index`.
    fn choose(&mut self, index: usize) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        self.dirty = true;
        let Some(action) = popup.entries.get(index).and_then(Entry::chosen) else {
            return;
        };
        let failed = match action {
            Action::Open => self
                .viewer
                .open_in_hand()
                .and_then(|path| libduscape::launch::open(&path).err()),
            Action::Reveal => {
                let paths = self.viewer.target_paths();
                let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
                libduscape::launch::reveal(&paths).err()
            }
            Action::CopyPath | Action::CopyFullPath => {
                return self.copy_paths(action == Action::CopyFullPath);
            }
            Action::Rescan => {
                self.viewer.rescan_selected();
                None
            }
            Action::RescanAll => {
                self.viewer.rescan_all();
                None
            }
            Action::Trash | Action::Delete => return self.remove(action == Action::Delete),
            // Not offered here (`PLATFORM`).
            Action::QuickLook | Action::CopyPathname => None,
        };
        if let Some(error) = failed {
            self.viewer.say(error);
        }
        self.changed();
    }
}

/// What was read for the preview, decoded here on the previewer's thread: a picture becomes
/// RGBA, shrunk to `PICTURE_SIDE`, so that the window only ever scales something small.
fn decode(loaded: Loaded) -> (Preview, Option<Rgba>) {
    match loaded {
        Loaded::Info(info) => (Preview::Info(info), None),
        Loaded::Text(lines) => (Preview::Text(lines), None),
        Loaded::Binary { info, dump } => (Preview::Hex { info, dump }, None),
        Loaded::Picture { bytes, caption } => match decode_picture(&bytes) {
            Ok(picture) => (Preview::Picture(caption), Some(picture)),
            Err(error) => (Preview::Info(format!("{caption}, {error}")), None),
        },
    }
}

fn decode_picture(bytes: &[u8]) -> Result<Rgba, String> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(40_000);
    limits.max_image_height = Some(40_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "which cannot be decoded here".to_string())?;
    let (w, h) = (decoded.width(), decoded.height());
    let shrink = f64::from(PICTURE_SIDE) / f64::from(w.max(h));
    let rgba = if shrink < 1.0 {
        let (nw, nh) = (
            ((f64::from(w) * shrink) as u32).max(1),
            ((f64::from(h) * shrink) as u32).max(1),
        );
        image::imageops::resize(
            &decoded.to_rgba8(),
            nw,
            nh,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded.to_rgba8()
    };
    Ok(Rgba {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        data: rgba.into_raw(),
    })
}

/// The question a removal asks, and its detail: what, how much, and where.
fn confirmation(files: &[FileToDelete], permanently: bool) -> (String, String) {
    let what = match files {
        [one] => format!(
            "“{}”",
            one.path_to_file
                .last()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        many => format!("{} items", DisplayCount(many.len() as u64)),
    };
    let title = if permanently {
        format!("Delete {what} immediately?")
    } else {
        format!("Move {what} to the Trash?")
    };
    let size: u128 = files.iter().map(|file| file.size).sum();
    let mut detail = match files {
        [one] => {
            let contents = match one.num_descendants {
                Some(count) if one.file_type == libduscape::FileType::Folder => {
                    format!(", a folder of {} items", DisplayCount(count))
                }
                _ => String::new(),
            };
            format!(
                "{}{contents}\n{}",
                DisplaySize(size as f64),
                one.full_path().display()
            )
        }
        many => {
            let names: Vec<String> = many
                .iter()
                .take(5)
                .filter_map(|file| file.path_to_file.last())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            let more = if many.len() > 5 {
                format!(" and {} more", DisplayCount(many.len() as u64 - 5))
            } else {
                String::new()
            };
            format!(
                "{} in all: {}{more}",
                DisplaySize(size as f64),
                names.join(", ")
            )
        }
    };
    if permanently {
        detail += "\n\nThis can’t be undone.";
    } else {
        detail += "\n\nThe Trash frees nothing until it is emptied.";
    }
    (title, detail)
}
