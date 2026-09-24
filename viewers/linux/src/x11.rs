//! The X11 backend, on `x11rb` — pure Rust, no Xlib: one window, its events read on a thread of
//! their own and turned into [`Input`], the frame put up whole with `PutImage`, the core keyboard
//! mapping, and the clipboard when no tool owns it.

use ::std::borrow::Cow;
use ::std::sync::{Arc, Mutex};

use x11rb::connection::Connection;
use x11rb::image::{BitsPerPixel, ColorComponent, Image, ImageOrder, PixelLayout, ScanlinePad};
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask,
    Gcontext, PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    VisualClass, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::backend::{Backend, Button, Input, Mods, level_keysym};
use crate::canvas::Canvas;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        _NET_WM_NAME,
        UTF8_STRING,
        CLIPBOARD,
        TARGETS,
    }
}

/// Modifier bits in an event's `state`.
const SHIFT: u16 = 1;
const LOCK: u16 = 2;
const CONTROL: u16 = 4;

/// The core keyboard mapping: keysyms per keycode, columns 0 and 1 being without and with Shift.
struct Keymap {
    min_keycode: u8,
    per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn fetch(conn: &RustConnection) -> Result<Keymap, String> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|error| format!("keyboard mapping: {error}"))?
            .reply()
            .map_err(|error| format!("keyboard mapping: {error}"))?;
        Ok(Keymap {
            min_keycode: min,
            per_keycode: reply.keysyms_per_keycode,
            keysyms: reply.keysyms,
        })
    }

    /// The keysym a key press means, given the modifiers held.
    fn keysym(&self, keycode: u8, state: u16) -> Option<u32> {
        let per = usize::from(self.per_keycode);
        let start = usize::from(keycode.checked_sub(self.min_keycode)?) * per;
        let column = |index: usize| self.keysyms.get(start + index).copied().unwrap_or(0);
        let (plain, shifted) = (column(0), column(1));
        if plain == 0 {
            return None;
        }
        Some(level_keysym(
            plain,
            shifted,
            state & SHIFT != 0,
            state & LOCK != 0,
        ))
    }
}

/// What the event thread and the app both reach: the connection, and the clipboard's text.
struct Shared {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    /// The text this window holds on the clipboard, when no clipboard tool took it.
    clipboard: Mutex<Option<Vec<u8>>>,
}

pub struct X11 {
    shared: Arc<Shared>,
    gc: Gcontext,
    depth: u8,
    /// Where the window's visual keeps each colour in a pixel.
    layout: PixelLayout,
    scale: f64,
    /// The window's size in pixels, as last drawn.
    size: Arc<Mutex<(u16, u16)>>,
}

/// The layout `Canvas` pixels are in.
fn canvas_layout() -> PixelLayout {
    PixelLayout::new(
        ColorComponent::new(8, 16).expect("a valid component"),
        ColorComponent::new(8, 8).expect("a valid component"),
        ColorComponent::new(8, 0).expect("a valid component"),
    )
}

fn mods(state: u16) -> Mods {
    Mods {
        shift: state & SHIFT != 0,
        control: state & CONTROL != 0,
    }
}

impl X11 {
    /// Open the display and a window of `size` points, at least `min` points.
    pub fn open(
        title: &str,
        size: (f64, f64),
        min: (f64, f64),
        deliver: impl Fn(Input) + Send + 'static,
    ) -> Result<X11, String> {
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|error| format!("cannot open the display: {error}"))?;
        let screen = &conn.setup().roots[screen_num];
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|depth| {
                depth
                    .visuals
                    .iter()
                    .map(move |visual| (depth.depth, visual))
            })
            .find(|(_, visual)| visual.visual_id == screen.root_visual)
            .ok_or("the screen's visual is missing from its depths")?;
        let (depth, visual) = (visual.0, *visual.1);
        if visual.class != VisualClass::TRUE_COLOR && visual.class != VisualClass::DIRECT_COLOR {
            return Err(format!("a {:?} visual is not supported", visual.class));
        }
        let layout = PixelLayout::from_visual_type(visual)
            .map_err(|error| format!("the screen's pixel format: {error}"))?;
        let atoms = Atoms::new(&conn)
            .map_err(|error| format!("atoms: {error}"))?
            .reply()
            .map_err(|error| format!("atoms: {error}"))?;
        let scale = scale_factor(&conn);

        let window = conn.generate_id().map_err(|error| error.to_string())?;
        let (w, h) = (
            (size.0 * scale).round() as u16,
            (size.1 * scale).round() as u16,
        );
        let (root, root_visual) = (screen.root, screen.root_visual);
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            root_visual,
            &CreateWindowAux::new()
                .background_pixel(layout.encode((0x2222, 0x2222, 0x2424)))
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::STRUCTURE_NOTIFY
                        | EventMask::KEY_PRESS
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION
                        | EventMask::LEAVE_WINDOW
                        | EventMask::FOCUS_CHANGE,
                ),
        )
        .map_err(|error| error.to_string())?;
        let mut hints = WmSizeHints::new();
        hints.min_size = Some(((min.0 * scale) as i32, (min.1 * scale) as i32));
        hints
            .set_normal_hints(&conn, window)
            .map_err(|error| error.to_string())?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms.WM_PROTOCOLS,
            AtomEnum::ATOM,
            &[atoms.WM_DELETE_WINDOW],
        )
        .map_err(|error| error.to_string())?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"diskonaut-linux\0diskonaut-linux\0",
        )
        .map_err(|error| error.to_string())?;
        let gc = conn.generate_id().map_err(|error| error.to_string())?;
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0))
            .map_err(|error| error.to_string())?;
        let keymap = Keymap::fetch(&conn)?;
        let shared = Arc::new(Shared {
            conn,
            window,
            atoms,
            clipboard: Mutex::new(None),
        });
        let backend = X11 {
            shared,
            gc,
            depth,
            layout,
            scale,
            size: Arc::new(Mutex::new((w, h))),
        };
        backend.set_title_inner(title)?;
        backend
            .shared
            .conn
            .map_window(window)
            .map_err(|error| error.to_string())?;
        backend.flush()?;
        backend.spawn_events(keymap, deliver);
        Ok(backend)
    }

    fn flush(&self) -> Result<(), String> {
        self.shared.conn.flush().map_err(|error| error.to_string())
    }

    fn set_title_inner(&self, title: &str) -> Result<(), String> {
        let shared = &self.shared;
        shared
            .conn
            .change_property8(
                PropMode::REPLACE,
                shared.window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        shared
            .conn
            .change_property8(
                PropMode::REPLACE,
                shared.window,
                shared.atoms._NET_WM_NAME,
                shared.atoms.UTF8_STRING,
                title.as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        self.flush()
    }

    /// Read events on a thread of their own, turning them into `Input`. The keyboard mapping
    /// lives here, as does answering paste requests, so the app thread never waits on the server.
    fn spawn_events(&self, mut keymap: Keymap, deliver: impl Fn(Input) + Send + 'static) {
        let shared = Arc::clone(&self.shared);
        let size = Arc::clone(&self.size);
        let scale = self.scale;
        let _ = ::std::thread::Builder::new()
            .name("x11_events".to_string())
            .spawn(move || {
                let conn = &shared.conn;
                while let Ok(event) = conn.wait_for_event() {
                    match event {
                        Event::Expose(expose) if expose.count == 0 => deliver(Input::Redraw),
                        Event::ConfigureNotify(configure) => {
                            let now = (configure.width, configure.height);
                            let mut known = size.lock().unwrap_or_else(|e| e.into_inner());
                            if now != *known && now.0 > 0 && now.1 > 0 {
                                *known = now;
                                drop(known);
                                deliver(Input::Resized {
                                    width: f64::from(now.0) / scale,
                                    height: f64::from(now.1) / scale,
                                    scale,
                                });
                            }
                        }
                        Event::KeyPress(key) => {
                            let state = u16::from(key.state);
                            if let Some(keysym) = keymap.keysym(key.detail, state) {
                                deliver(Input::Key {
                                    keysym,
                                    mods: mods(state),
                                });
                            }
                        }
                        Event::ButtonPress(press) => {
                            let button = match press.detail {
                                1 => Button::Left,
                                3 => Button::Right,
                                4 => Button::WheelUp,
                                5 => Button::WheelDown,
                                _ => Button::Other,
                            };
                            deliver(Input::Button {
                                button,
                                x: f64::from(press.event_x) / scale,
                                y: f64::from(press.event_y) / scale,
                                mods: mods(u16::from(press.state)),
                            });
                        }
                        Event::MotionNotify(motion) => deliver(Input::Motion {
                            x: f64::from(motion.event_x) / scale,
                            y: f64::from(motion.event_y) / scale,
                        }),
                        Event::LeaveNotify(_) => deliver(Input::Leave),
                        Event::FocusIn(_) => deliver(Input::Focus(true)),
                        Event::FocusOut(_) => deliver(Input::Focus(false)),
                        Event::ClientMessage(message) => {
                            if is_close(&shared, &message) {
                                deliver(Input::Close);
                            }
                        }
                        Event::SelectionRequest(request) => selection_request(&shared, &request),
                        Event::SelectionClear(_) => {
                            *shared.clipboard.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        }
                        Event::MappingNotify(_) => {
                            if let Ok(fresh) = Keymap::fetch(conn) {
                                keymap = fresh;
                            }
                        }
                        Event::Error(error) => eprintln!("diskonaut-linux: X error: {error:?}"),
                        _ => {}
                    }
                }
                // The server is gone: the window with it.
                deliver(Input::Close);
            });
    }
}

impl Backend for X11 {
    fn size(&self) -> (f64, f64, f64) {
        let (w, h) = *self.size.lock().unwrap_or_else(|e| e.into_inner());
        (
            f64::from(w) / self.scale,
            f64::from(h) / self.scale,
            self.scale,
        )
    }

    fn decorated(&self) -> bool {
        // The window manager's job; a bare X server without one leaves the window bare too.
        true
    }

    /// Put the whole canvas on the window.
    fn present(&mut self, canvas: &Canvas) -> Result<(), String> {
        let (width, height) = (canvas.width as u16, canvas.height as u16);
        if width == 0 || height == 0 {
            return Ok(());
        }
        let own = canvas_layout();
        let pixels: Vec<u32> = if own == self.layout {
            canvas.pixels.clone()
        } else {
            // An unusual visual (10-bit, or an odd channel order): each pixel re-encoded.
            canvas
                .pixels
                .iter()
                .map(|&pixel| self.layout.encode(own.decode(pixel)))
                .collect()
        };
        let mut bytes = Vec::with_capacity(pixels.len() * 4);
        for pixel in pixels {
            bytes.extend_from_slice(&pixel.to_le_bytes());
        }
        let image = Image::new(
            width,
            height,
            ScanlinePad::Pad32,
            self.depth,
            BitsPerPixel::B32,
            ImageOrder::LsbFirst,
            Cow::Owned(bytes),
        )
        .map_err(|error| error.to_string())?;
        image
            .put(&self.shared.conn, self.shared.window, self.gc, 0, 0)
            .map_err(|error| error.to_string())?;
        self.flush()
    }

    fn set_title(&mut self, title: &str) -> Result<(), String> {
        self.set_title_inner(title)
    }

    /// Take the clipboard with `text`, answering paste requests from the event thread. Whether
    /// the server made this window the owner.
    fn copy(&mut self, text: &str) -> bool {
        let shared = &self.shared;
        *shared.clipboard.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(text.as_bytes().to_vec());
        let owned = shared
            .conn
            .set_selection_owner(shared.window, shared.atoms.CLIPBOARD, x11rb::CURRENT_TIME)
            .is_ok()
            && shared
                .conn
                .get_selection_owner(shared.atoms.CLIPBOARD)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.owner == shared.window);
        if !owned {
            *shared.clipboard.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        owned
    }
}

/// Whether a client message is the window manager closing the window.
fn is_close(shared: &Shared, event: &ClientMessageEvent) -> bool {
    event.type_ == shared.atoms.WM_PROTOCOLS
        && event.data.as_data32()[0] == shared.atoms.WM_DELETE_WINDOW
}

/// Another client wants the clipboard's contents.
fn selection_request(shared: &Shared, request: &SelectionRequestEvent) {
    let conn = &shared.conn;
    let property = if request.property == x11rb::NONE {
        request.target
    } else {
        request.property
    };
    let text = shared
        .clipboard
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let utf8 = shared.atoms.UTF8_STRING;
    let string = u32::from(AtomEnum::STRING);
    let given = if request.target == shared.atoms.TARGETS {
        conn.change_property32(
            PropMode::REPLACE,
            request.requestor,
            property,
            AtomEnum::ATOM,
            &[shared.atoms.TARGETS, utf8, string],
        )
        .is_ok()
    } else if request.target == utf8 || request.target == string {
        conn.change_property8(
            PropMode::REPLACE,
            request.requestor,
            property,
            request.target,
            &text,
        )
        .is_ok()
    } else {
        false
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: if given { property } else { x11rb::NONE },
    };
    let _ = conn.send_event(false, request.requestor, EventMask::NO_EVENT, notify);
    let _ = conn.flush();
}

/// Pixels per point: `DISKONAUT_SCALE`, else `GDK_SCALE`, else `Xft.dpi` over 96, to a quarter.
fn scale_factor(conn: &RustConnection) -> f64 {
    let from_env = |name: &str| {
        ::std::env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
    };
    let scale = from_env("DISKONAUT_SCALE")
        .or_else(|| from_env("GDK_SCALE"))
        .or_else(|| {
            x11rb::resource_manager::new_from_default(conn)
                .ok()
                .and_then(|db| db.get_value::<f64>("Xft.dpi", "Xft.Dpi").ok().flatten())
                .map(|dpi| dpi / 96.0)
        })
        .unwrap_or(1.0);
    ((scale * 4.0).round() / 4.0).clamp(1.0, 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::keys;

    #[test]
    fn the_core_mapping_is_read_by_column() {
        // Two keycodes from 8: `a`/`A` given as one keysym, `1`/`!` as two, and Left as one.
        let map = Keymap {
            min_keycode: 8,
            per_keycode: 2,
            keysyms: vec![
                u32::from('a'),
                0,
                u32::from('1'),
                u32::from('!'),
                keys::LEFT,
                0,
            ],
        };
        assert_eq!(map.keysym(8, 0), Some(u32::from('a')));
        assert_eq!(map.keysym(8, SHIFT), Some(u32::from('A')));
        assert_eq!(map.keysym(9, SHIFT), Some(u32::from('!')));
        assert_eq!(map.keysym(10, SHIFT | LOCK), Some(keys::LEFT));
        assert_eq!(map.keysym(7, 0), None);
        assert_eq!(map.keysym(11, 0), None);
    }
}
