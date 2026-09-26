//! The native Wayland backend, on `wayland-client`'s pure-Rust protocol implementation — no
//! libwayland, no toolkit. A `wl_shm` buffer carries the frame; `xdg_shell` makes it a window;
//! `xdg-decoration` asks the compositor for a title bar, and where it draws none (GNOME) the
//! app draws its own (`Input::Decorated(false)`); the keyboard's xkb keymap is read by `xkb`.
//! The compositor's events are dispatched on a thread of their own and turned into [`Input`];
//! the app thread sends requests (a frame, a title, the clipboard) straight to the connection,
//! which is made for that.

use ::std::io::Write;
use ::std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use ::std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use ::std::sync::{Arc, Mutex};
use ::std::time::Duration;

use wayland_client::backend::ObjectId;
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::WlDataOffer,
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard},
    wl_output::{self, WlOutput},
    wl_pointer::{self, Axis, ButtonState, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::{self, Capability, WlSeat},
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::{self, WlSurface},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_cursor::CursorTheme;
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, Mode, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::backend::{Backend, Button, Input, Mods};
use crate::canvas::Canvas;
use crate::xkb::Xkb;

/// Linux input codes for the mouse buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// The thumb button, and the one some mice report for it instead.
const BTN_SIDE: u32 = 0x113;
const BTN_BACK: u32 = 0x116;
/// A wheel notch, in the axis's surface units, when the compositor sends no discrete steps.
const WHEEL_NOTCH: f64 = 10.0;

type Deliver = Arc<dyn Fn(Input) + Send + Sync>;

/// What the dispatch thread and the app both reach.
struct Shared {
    conn: Connection,
    qh: QueueHandle<State>,
    surface: WlSurface,
    toplevel: XdgToplevel,
    seat: Option<WlSeat>,
    data_device: Option<WlDataDevice>,
    data_device_manager: Option<WlDataDeviceManager>,
    /// The last input event's serial: what a selection or a move must quote.
    serial: AtomicU32,
    /// The window's size in points, and its scale.
    size: Mutex<(f64, f64)>,
    scale: AtomicI32,
    decorated: AtomicBool,
    maximized: AtomicBool,
    /// The clipboard offer this window holds, if any.
    source: Mutex<Option<WlDataSource>>,
}

impl Shared {
    fn deliver_size(&self, deliver: &Deliver) {
        let (width, height) = *self.size.lock().unwrap_or_else(|e| e.into_inner());
        deliver(Input::Resized {
            width,
            height,
            scale: f64::from(self.scale.load(Ordering::Acquire)),
        });
    }
}

/// The dispatch thread's state: the globals, the seat's devices, and what the pointer and the
/// keyboard are up to.
struct State {
    deliver: Deliver,
    shared: Option<Arc<Shared>>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    decoration_manager: Option<ZxdgDecorationManagerV1>,
    data_device_manager: Option<WlDataDeviceManager>,
    /// Every output, with its scale; and the ones the surface is on.
    outputs: Vec<(WlOutput, i32)>,
    on_outputs: Vec<ObjectId>,
    /// `wl_surface.preferred_buffer_scale`, which beats guessing from the outputs.
    preferred_scale: Option<i32>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    xkb: Xkb,
    shift: bool,
    control: bool,
    lock: bool,
    /// Key repeat: rate (per second) and delay (ms), and a count that a release or another press
    /// bumps, so the repeating thread knows to stop.
    repeat: (i32, i32),
    held: Arc<AtomicU64>,
    pointer_at: (f64, f64),
    /// Scrolling: the axis value since the last notch, and whether this frame gave steps.
    axis: f64,
    discrete_seen: bool,
    /// The size the last `xdg_toplevel.configure` asked for, until its `xdg_surface.configure`.
    pending: Option<(i32, i32)>,
    configured: bool,
    cursor: Option<(CursorTheme, WlSurface)>,
}

impl State {
    fn shared(&self) -> &Arc<Shared> {
        self.shared
            .as_ref()
            .expect("the window exists before events reach it")
    }

    fn mods(&self) -> Mods {
        Mods {
            shift: self.shift,
            control: self.control,
        }
    }

    /// The scale the window should draw at: what the compositor prefers, else the largest of the
    /// outputs it is on, else the largest anywhere.
    fn scale(&self) -> i32 {
        if let Some(scale) = self.preferred_scale {
            return scale.max(1);
        }
        let on = self
            .outputs
            .iter()
            .filter(|(output, _)| self.on_outputs.contains(&output.id()))
            .map(|(_, scale)| *scale)
            .max();
        on.or_else(|| self.outputs.iter().map(|(_, scale)| *scale).max())
            .unwrap_or(1)
            .max(1)
    }

    fn rescale(&mut self) {
        let scale = self.scale();
        let shared = self.shared();
        if shared.scale.swap(scale, Ordering::AcqRel) != scale && self.configured {
            shared.deliver_size(&self.deliver);
        }
    }

    /// Show the theme's arrow over the window; a compositor shows nothing otherwise.
    fn set_cursor(&mut self, pointer: &WlPointer, serial: u32) {
        if self.cursor.is_none()
            && let (Some(shm), Some(compositor), Some(shared)) =
                (&self.shm, &self.compositor, &self.shared)
            && let Ok(theme) = CursorTheme::load(&shared.conn, shm.clone(), 24)
        {
            let surface = compositor.create_surface(&shared.qh, ());
            self.cursor = Some((theme, surface));
        }
        let Some((theme, surface)) = &mut self.cursor else {
            return;
        };
        let name = ["default", "left_ptr"]
            .into_iter()
            .find(|name| theme.get_cursor(name).is_some());
        let Some(cursor) = name.and_then(|name| theme.get_cursor(name)) else {
            return;
        };
        let image = &cursor[0];
        let (hx, hy) = image.hotspot();
        let (w, h) = image.dimensions();
        pointer.set_cursor(serial, Some(surface), hx as i32, hy as i32);
        surface.attach(Some(image), 0, 0);
        surface.damage(0, 0, w as i32, h as i32);
        surface.commit();
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut State,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor =
                    Some(registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ()));
            }
            "wl_shm" => state.shm = Some(registry.bind::<WlShm, _, _>(name, 1, qh, ())),
            "xdg_wm_base" => {
                state.wm_base =
                    Some(registry.bind::<XdgWmBase, _, _>(name, version.min(5), qh, ()));
            }
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ()));
            }
            "wl_output" => {
                let output = registry.bind::<WlOutput, _, _>(name, version.min(4), qh, ());
                state.outputs.push((output, 1));
            }
            "zxdg_decoration_manager_v1" => {
                state.decoration_manager =
                    Some(registry.bind::<ZxdgDecorationManagerV1, _, _>(name, 1, qh, ()));
            }
            "wl_data_device_manager" => {
                state.data_device_manager =
                    Some(registry.bind::<WlDataDeviceManager, _, _>(name, version.min(3), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut State,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut State,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let xdg_surface::Event::Configure { serial } = event else {
            return;
        };
        xdg_surface.ack_configure(serial);
        let first = !state.configured;
        state.configured = true;
        let shared = Arc::clone(state.shared());
        let mut changed = false;
        if let Some((w, h)) = state.pending.take()
            && w > 0
            && h > 0
        {
            let mut size = shared.size.lock().unwrap_or_else(|e| e.into_inner());
            let now = (f64::from(w), f64::from(h));
            changed = *size != now;
            *size = now;
        }
        if changed || first {
            shared.scale.store(state.scale(), Ordering::Release);
            shared.deliver_size(&state.deliver);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut State,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state.pending = Some((width, height));
                let maximized = states
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| u32::from_ne_bytes(*bytes))
                    .any(|value| value == xdg_toplevel::State::Maximized as u32);
                state.shared().maximized.store(maximized, Ordering::Release);
            }
            xdg_toplevel::Event::Close => (state.deliver)(Input::Close),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            let decorated = mode == WEnum::Value(Mode::ServerSide);
            let shared = state.shared();
            if shared.decorated.swap(decorated, Ordering::AcqRel) != decorated {
                (state.deliver)(Input::Decorated(decorated));
            }
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(
        state: &mut State,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => state.on_outputs.push(output.id()),
            wl_surface::Event::Leave { output } => state.on_outputs.retain(|id| *id != output.id()),
            wl_surface::Event::PreferredBufferScale { factor } => {
                state.preferred_scale = Some(factor);
            }
            _ => return,
        }
        state.rescale();
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut State,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_output::Event::Scale { factor } = event {
            for (known, scale) in &mut state.outputs {
                if known.id() == output.id() {
                    *scale = factor;
                }
            }
            if state.shared.is_some() {
                state.rescale();
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut State,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(Capability::Keyboard) && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
        if capabilities.contains(Capability::Pointer) && state.pointer.is_none() {
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if format == WEnum::Value(KeymapFormat::XkbV1) {
                    state.xkb = read_keymap(&fd, size as usize)
                        .map(|text| Xkb::parse(&text))
                        .unwrap_or_else(Xkb::empty);
                }
            }
            wl_keyboard::Event::Enter { .. } => (state.deliver)(Input::Focus(true)),
            wl_keyboard::Event::Leave { .. } => {
                state.held.fetch_add(1, Ordering::AcqRel);
                (state.deliver)(Input::Focus(false));
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                ..
            } => {
                // xkb's real modifiers come in a fixed order: Shift, Lock, Control, Mod1…
                let mods = mods_depressed | mods_latched | mods_locked;
                state.shift = mods & 1 != 0;
                state.lock = mods & 2 != 0;
                state.control = mods & 4 != 0;
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => state.repeat = (rate, delay),
            wl_keyboard::Event::Key {
                serial,
                key,
                state: key_state,
                ..
            } => {
                state.shared().serial.store(serial, Ordering::Release);
                let generation = state.held.fetch_add(1, Ordering::AcqRel) + 1;
                if key_state != WEnum::Value(KeyState::Pressed) {
                    return;
                }
                let Some(keysym) = state.xkb.keysym(key, state.shift, state.lock) else {
                    return;
                };
                let mods = state.mods();
                (state.deliver)(Input::Key { keysym, mods });
                // Repeat while held: the compositor leaves that to the client.
                let (rate, delay) = state.repeat;
                if rate <= 0 {
                    return;
                }
                let held = Arc::clone(&state.held);
                let deliver = Arc::clone(&state.deliver);
                let _ = ::std::thread::Builder::new()
                    .name("key_repeat".to_string())
                    .spawn(move || {
                        ::std::thread::sleep(Duration::from_millis(delay.max(0) as u64));
                        let interval = Duration::from_millis((1000 / rate.max(1)).max(10) as u64);
                        while held.load(Ordering::Acquire) == generation {
                            deliver(Input::Key { keysym, mods });
                            ::std::thread::sleep(interval);
                        }
                    });
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut State,
        pointer: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                state.shared().serial.store(serial, Ordering::Release);
                state.pointer_at = (surface_x, surface_y);
                state.set_cursor(pointer, serial);
                (state.deliver)(Input::Motion {
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Leave { .. } => (state.deliver)(Input::Leave),
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_at = (surface_x, surface_y);
                (state.deliver)(Input::Motion {
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: button_state,
                ..
            } => {
                state.shared().serial.store(serial, Ordering::Release);
                if button_state != WEnum::Value(ButtonState::Pressed) {
                    return;
                }
                let button = match button {
                    BTN_LEFT => Button::Left,
                    BTN_RIGHT => Button::Right,
                    BTN_SIDE | BTN_BACK => Button::Back,
                    _ => Button::Other,
                };
                let (x, y) = state.pointer_at;
                let mods = state.mods();
                (state.deliver)(Input::Button { button, x, y, mods });
            }
            wl_pointer::Event::AxisDiscrete {
                axis: WEnum::Value(Axis::VerticalScroll),
                discrete,
            } => {
                state.discrete_seen = true;
                state.wheel(discrete);
            }
            wl_pointer::Event::AxisValue120 {
                axis: WEnum::Value(Axis::VerticalScroll),
                value120,
            } => {
                state.discrete_seen = true;
                state.wheel(value120 / 120);
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(Axis::VerticalScroll),
                value,
                ..
            } => {
                if state.discrete_seen {
                    return;
                }
                // A touchpad: smooth values, a notch's worth at a time.
                state.axis += value;
                let notches = (state.axis / WHEEL_NOTCH).trunc();
                if notches != 0.0 {
                    state.axis -= notches * WHEEL_NOTCH;
                    state.wheel(notches as i32);
                }
            }
            wl_pointer::Event::Frame => state.discrete_seen = false,
            _ => {}
        }
    }
}

impl State {
    /// `steps` wheel notches, down when positive.
    fn wheel(&self, steps: i32) {
        let button = if steps > 0 {
            Button::WheelDown
        } else {
            Button::WheelUp
        };
        let (x, y) = self.pointer_at;
        let mods = self.mods();
        for _ in 0..steps.unsigned_abs().min(20) {
            (self.deliver)(Input::Button { button, x, y, mods });
        }
    }
}

impl Dispatch<WlBuffer, Arc<AtomicBool>> for State {
    fn event(
        _: &mut State,
        _: &WlBuffer,
        event: wl_buffer::Event,
        busy: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Release);
        }
    }
}

impl Dispatch<WlDataSource, Arc<Vec<u8>>> for State {
    fn event(
        _: &mut State,
        source: &WlDataSource,
        event: wl_data_source::Event,
        text: &Arc<Vec<u8>>,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let mut file = ::std::fs::File::from(fd);
                let _ = file.write_all(text);
            }
            wl_data_source::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        _: &mut State,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // Offers from other clients: this window never pastes.
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ZxdgDecorationManagerV1);
delegate_noop!(State: WlDataDeviceManager);
delegate_noop!(State: ignore WlDataOffer);

/// The keymap the compositor sent, as text: a memory file to map, never to read.
fn read_keymap(fd: &OwnedFd, size: usize) -> Option<String> {
    if size == 0 {
        return None;
    }
    // SAFETY: mapping a file the compositor gave us read-only and privately; the pointer is
    // checked, used only within `size`, and unmapped before returning.
    unsafe {
        let map = libc::mmap(
            ::std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd.as_raw_fd(),
            0,
        );
        if map == libc::MAP_FAILED {
            return None;
        }
        let bytes = ::std::slice::from_raw_parts(map as *const u8, size);
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(size);
        let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
        libc::munmap(map, size);
        Some(text)
    }
}

/// A `wl_shm` pool of buffers of one size, in a memory file mapped here: two to begin with, and
/// up to `MAX_BUFFERS` when the compositor holds them longer than frames come.
struct Pool {
    fd: OwnedFd,
    map: *mut u8,
    len: usize,
    pool: WlShmPool,
    buffers: Vec<(WlBuffer, Arc<AtomicBool>)>,
    width: u32,
    height: u32,
}

const MAX_BUFFERS: usize = 4;

// SAFETY: the mapping is only ever touched from the app thread, which owns the `Pool`.
unsafe impl Send for Pool {}

impl Pool {
    fn new(shm: &WlShm, qh: &QueueHandle<State>, width: u32, height: u32) -> Result<Pool, String> {
        let one = Self::one(width, height);
        let len = one * 2;
        // SAFETY: memfd_create with a static name; the result is checked.
        let raw = unsafe { libc::memfd_create(c"duscape-frame".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err("memfd_create failed".to_string());
        }
        // SAFETY: `raw` is a fresh descriptor this process owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: a plain ftruncate on our own file.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } < 0 {
            return Err("could not size the frame's memory".to_string());
        }
        // SAFETY: mapping our own file, shared so the compositor sees what is written.
        let map = unsafe {
            libc::mmap(
                ::std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err("could not map the frame's memory".to_string());
        }
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let mut this = Pool {
            buffers: Vec::new(),
            fd,
            map: map as *mut u8,
            len,
            pool,
            width,
            height,
        };
        this.add_buffer(qh);
        this.add_buffer(qh);
        Ok(this)
    }

    /// One buffer's bytes.
    fn one(width: u32, height: u32) -> usize {
        width as usize * 4 * height as usize
    }

    /// A buffer at the end of the pool.
    fn add_buffer(&mut self, qh: &QueueHandle<State>) {
        let one = Self::one(self.width, self.height);
        let busy = Arc::new(AtomicBool::new(false));
        let buffer = self.pool.create_buffer(
            (self.buffers.len() * one) as i32,
            self.width as i32,
            self.height as i32,
            (self.width * 4) as i32,
            Format::Xrgb8888,
            qh,
            Arc::clone(&busy),
        );
        self.buffers.push((buffer, busy));
    }

    /// Grow the file, the mapping and the pool by one buffer. Whether it worked.
    fn grow(&mut self, qh: &QueueHandle<State>) -> bool {
        let len = self.len + Self::one(self.width, self.height);
        // SAFETY: growing our own file, then mapping it afresh; the old mapping is released
        // first and never used again.
        unsafe {
            if libc::ftruncate(self.fd.as_raw_fd(), len as libc::off_t) < 0 {
                return false;
            }
            let map = libc::mmap(
                ::std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                0,
            );
            if map == libc::MAP_FAILED {
                return false;
            }
            libc::munmap(self.map as *mut libc::c_void, self.len);
            self.map = map as *mut u8;
        }
        self.len = len;
        self.pool.resize(len as i32);
        self.add_buffer(qh);
        true
    }

    /// Copy the canvas into a buffer the compositor has released and return it. When it still
    /// holds every one, another is added (up to `MAX_BUFFERS`); past that the oldest is reused,
    /// which a compositor that slow will not notice.
    fn fill(&mut self, canvas: &Canvas, qh: &QueueHandle<State>) -> &WlBuffer {
        let free = self
            .buffers
            .iter()
            .position(|(_, busy)| !busy.load(Ordering::Acquire));
        let index = match free {
            Some(index) => index,
            None if self.buffers.len() < MAX_BUFFERS && self.grow(qh) => self.buffers.len() - 1,
            None => 0,
        };
        let one = Self::one(self.width, self.height);
        // SAFETY: the mapping is `len` bytes and buffer `index` is `one` bytes within it, the
        // same size as the canvas whose pixels are copied.
        unsafe {
            let dest = self.map.add(index * one) as *mut u32;
            ::std::ptr::copy_nonoverlapping(canvas.pixels.as_ptr(), dest, canvas.pixels.len());
        }
        self.buffers[index].1.store(true, Ordering::Release);
        &self.buffers[index].0
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        for (buffer, _) in &self.buffers {
            buffer.destroy();
        }
        self.pool.destroy();
        // SAFETY: unmapping what `new` mapped, once.
        unsafe {
            libc::munmap(self.map as *mut libc::c_void, self.len);
        }
    }
}

pub struct Wayland {
    shared: Arc<Shared>,
    shm: WlShm,
    pool: Option<Pool>,
}

impl Wayland {
    /// Connect to the compositor `WAYLAND_DISPLAY` names and open a window of `size` points, at
    /// least `min` points.
    pub fn open(
        title: &str,
        size: (f64, f64),
        min: (f64, f64),
        deliver: impl Fn(Input) + Send + Sync + 'static,
    ) -> Result<Wayland, String> {
        let conn = Connection::connect_to_env()
            .map_err(|error| format!("cannot connect to the compositor: {error}"))?;
        let display = conn.display();
        let mut queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = State {
            deliver: Arc::new(deliver),
            shared: None,
            compositor: None,
            shm: None,
            wm_base: None,
            seat: None,
            decoration_manager: None,
            data_device_manager: None,
            outputs: Vec::new(),
            on_outputs: Vec::new(),
            preferred_scale: None,
            keyboard: None,
            pointer: None,
            xkb: Xkb::empty(),
            shift: false,
            control: false,
            lock: false,
            repeat: (25, 400),
            held: Arc::new(AtomicU64::new(0)),
            pointer_at: (0.0, 0.0),
            axis: 0.0,
            discrete_seen: false,
            pending: None,
            configured: false,
            cursor: None,
        };
        let _registry = display.get_registry(&qh, ());
        // Once for the globals, once more for what they announce (seat capabilities, scales).
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|error| format!("compositor: {error}"))?;
        }
        let compositor = state
            .compositor
            .clone()
            .ok_or("the compositor has no wl_compositor")?;
        let shm = state.shm.clone().ok_or("the compositor has no wl_shm")?;
        let wm_base = state
            .wm_base
            .clone()
            .ok_or("the compositor has no xdg_wm_base: it cannot show windows")?;

        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.to_string());
        toplevel.set_app_id("duscape-linux".to_string());
        toplevel.set_min_size(min.0 as i32, min.1 as i32);
        if let Some(manager) = &state.decoration_manager {
            let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
            decoration.set_mode(Mode::ServerSide);
        }
        let seat = state.seat.clone();
        let data_device = match (&state.data_device_manager, &seat) {
            (Some(manager), Some(seat)) => Some(manager.get_data_device(seat, &qh, ())),
            _ => None,
        };
        let shared = Arc::new(Shared {
            conn: conn.clone(),
            qh: qh.clone(),
            surface: surface.clone(),
            toplevel,
            seat,
            data_device,
            data_device_manager: state.data_device_manager.clone(),
            serial: AtomicU32::new(0),
            size: Mutex::new(size),
            scale: AtomicI32::new(1),
            // Until the compositor says, assume it draws no title bar: better a second one
            // than none.
            decorated: AtomicBool::new(false),
            maximized: AtomicBool::new(false),
            source: Mutex::new(None),
        });
        state.shared = Some(Arc::clone(&shared));
        surface.commit();
        conn.flush().map_err(|error| error.to_string())?;
        // The first configure says how big the window is; nothing is drawn before it.
        while !state.configured {
            queue
                .blocking_dispatch(&mut state)
                .map_err(|error| format!("compositor: {error}"))?;
        }
        let deliver = Arc::clone(&state.deliver);
        let _ = ::std::thread::Builder::new()
            .name("wayland_events".to_string())
            .spawn(move || {
                while queue.blocking_dispatch(&mut state).is_ok() {}
                deliver(Input::Close);
            });
        Ok(Wayland {
            shared,
            shm,
            pool: None,
        })
    }
}

impl Backend for Wayland {
    fn size(&self) -> (f64, f64, f64) {
        let (w, h) = *self.shared.size.lock().unwrap_or_else(|e| e.into_inner());
        (w, h, f64::from(self.shared.scale.load(Ordering::Acquire)))
    }

    fn decorated(&self) -> bool {
        self.shared.decorated.load(Ordering::Acquire)
    }

    fn present(&mut self, canvas: &Canvas) -> Result<(), String> {
        let (width, height) = (canvas.width as u32, canvas.height as u32);
        if width == 0 || height == 0 {
            return Ok(());
        }
        if self
            .pool
            .as_ref()
            .is_none_or(|pool| (pool.width, pool.height) != (width, height))
        {
            self.pool = Some(Pool::new(&self.shm, &self.shared.qh, width, height)?);
        }
        let pool = self.pool.as_mut().expect("just made");
        let buffer = pool.fill(canvas, &self.shared.qh);
        let surface = &self.shared.surface;
        surface.set_buffer_scale(self.shared.scale.load(Ordering::Acquire));
        surface.attach(Some(buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        self.shared.conn.flush().map_err(|error| error.to_string())
    }

    fn set_title(&mut self, title: &str) -> Result<(), String> {
        self.shared.toplevel.set_title(title.to_string());
        self.shared.conn.flush().map_err(|error| error.to_string())
    }

    /// Offer `text` as the selection, quoting the last input's serial: a compositor grants the
    /// clipboard only to a client the user just acted in.
    fn copy(&mut self, text: &str) -> bool {
        let (Some(manager), Some(device)) =
            (&self.shared.data_device_manager, &self.shared.data_device)
        else {
            return false;
        };
        let serial = self.shared.serial.load(Ordering::Acquire);
        if serial == 0 {
            return false;
        }
        let source =
            manager.create_data_source(&self.shared.qh, Arc::new(text.as_bytes().to_vec()));
        for mime in [
            "text/plain;charset=utf-8",
            "text/plain",
            "UTF8_STRING",
            "TEXT",
        ] {
            source.offer(mime.to_string());
        }
        device.set_selection(Some(&source), serial);
        let mut held = self.shared.source.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = held.replace(source) {
            old.destroy();
        }
        self.shared.conn.flush().is_ok()
    }

    fn begin_move(&mut self) {
        if let Some(seat) = &self.shared.seat {
            self.shared
                .toplevel
                ._move(seat, self.shared.serial.load(Ordering::Acquire));
            let _ = self.shared.conn.flush();
        }
    }

    fn toggle_maximize(&mut self) {
        if self.shared.maximized.load(Ordering::Acquire) {
            self.shared.toplevel.unset_maximized();
        } else {
            self.shared.toplevel.set_maximized();
        }
        let _ = self.shared.conn.flush();
    }

    fn minimize(&mut self) {
        self.shared.toplevel.set_minimized();
        let _ = self.shared.conn.flush();
    }
}
