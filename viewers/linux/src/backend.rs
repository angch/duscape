//! What the two windowing systems have in common, as the app sees them: a window that takes a
//! frame and gives back input. `x11` and `wayland` each implement [`Backend`] and turn their own
//! events into [`Input`]; the app never sees a protocol.

use crate::canvas::Canvas;

/// X keysyms for the keys that type no character. Both backends speak keysyms: X11 has them
/// natively, and Wayland's xkb keymaps name them (`xkb`). Everything Latin-1 is the character.
pub mod keys {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const KP_HOME: u32 = 0xff95;
    pub const KP_LEFT: u32 = 0xff96;
    pub const KP_UP: u32 = 0xff97;
    pub const KP_RIGHT: u32 = 0xff98;
    pub const KP_DOWN: u32 = 0xff99;
    pub const KP_PAGE_UP: u32 = 0xff9a;
    pub const KP_PAGE_DOWN: u32 = 0xff9b;
    pub const KP_END: u32 = 0xff9c;
    pub const KP_INSERT: u32 = 0xff9e;
    pub const KP_DELETE: u32 = 0xff9f;
    pub const KP_ADD: u32 = 0xffab;
    pub const KP_SUBTRACT: u32 = 0xffad;
    pub const KP_0: u32 = 0xffb0;
    pub const F5: u32 = 0xffc2;
    pub const DELETE: u32 = 0xffff;
    pub const ISO_LEFT_TAB: u32 = 0xfe20;
}

/// Modifiers held with a key or a click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    WheelUp,
    WheelDown,
    /// The mouse's back (thumb) button.
    Back,
    Other,
}

/// What a window reports. Coordinates and sizes are in points; the frame is `scale` pixels a
/// point.
#[derive(Debug)]
pub enum Input {
    /// The window's contents were lost, or the window first appeared: draw.
    Redraw,
    /// The window is now `width`×`height` points, at `scale` pixels a point.
    Resized {
        width: f64,
        height: f64,
        scale: f64,
    },
    /// Whether the windowing system draws the title bar. Off it, the app draws one.
    Decorated(bool),
    Key {
        keysym: u32,
        mods: Mods,
    },
    Button {
        button: Button,
        x: f64,
        y: f64,
        mods: Mods,
    },
    Motion {
        x: f64,
        y: f64,
    },
    Leave,
    Focus(bool),
    Close,
}

/// A window on one windowing system. Its events come on a thread of the backend's own, through
/// the `deliver` callback given to [`open`]; these are the requests the app makes of it.
pub trait Backend {
    /// The window's size in points and its scale, as last known.
    fn size(&self) -> (f64, f64, f64);
    /// Whether the windowing system draws the title bar, as last known.
    fn decorated(&self) -> bool;
    /// Put the whole canvas on the window.
    fn present(&mut self, canvas: &Canvas) -> Result<(), String>;
    fn set_title(&mut self, title: &str) -> Result<(), String>;
    /// Take the clipboard with `text`, this window answering pastes. Whether it could.
    fn copy(&mut self, text: &str) -> bool;
    /// The title bar the app draws was dragged, double-clicked, or its buttons pressed.
    fn begin_move(&mut self) {}
    fn toggle_maximize(&mut self) {}
    fn minimize(&mut self) {}
}

/// The keysym a key press means, from a key's unshifted and shifted keysyms. A key with one
/// keysym shifts by case, and Caps Lock only ever changes letters. `shifted == 0` means the key
/// has only one.
pub fn level_keysym(plain: u32, shifted: u32, shift: bool, lock: bool) -> u32 {
    // Only Latin-1 keysyms are characters; the rest (arrows, function keys) have no case.
    let latin1 = |keysym: u32| char::from_u32(keysym).filter(|_| keysym < 0x100);
    let (lower, upper) = if shifted == 0 {
        match latin1(plain) {
            Some(ch) if ch.is_alphabetic() => (
                ch.to_lowercase().next().map_or(plain, u32::from),
                ch.to_uppercase().next().map_or(plain, u32::from),
            ),
            _ => (plain, plain),
        }
    } else {
        (plain, shifted)
    };
    let is_letter = latin1(lower).is_some_and(char::is_alphabetic);
    if shift != (lock && is_letter) {
        upper
    } else {
        lower
    }
}

/// Open a window on the windowing system the environment names: `DUSCAPE_BACKEND` (`wayland`
/// or `x11`), else Wayland when `WAYLAND_DISPLAY` is set, else X11. If the one chosen by the
/// environment fails and the other is there, the other is tried.
pub fn open(
    title: &str,
    size: (f64, f64),
    min: (f64, f64),
    deliver: impl Fn(Input) + Send + Sync + Clone + 'static,
) -> Result<Box<dyn Backend>, String> {
    let forced = ::std::env::var("DUSCAPE_BACKEND").ok();
    let has = |name: &str| ::std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let order: Vec<&str> = match forced.as_deref() {
        Some("wayland") => vec!["wayland"],
        Some("x11") => vec!["x11"],
        Some(other) => return Err(format!("DUSCAPE_BACKEND={other}: use wayland or x11")),
        None if has("WAYLAND_DISPLAY") => vec!["wayland", "x11"],
        None => vec!["x11", "wayland"],
    };
    let mut errors = Vec::new();
    for name in order {
        let result: Result<Box<dyn Backend>, String> = match name {
            "wayland" => crate::wayland::Wayland::open(title, size, min, deliver.clone())
                .map(|backend| Box::new(backend) as Box<dyn Backend>),
            _ => crate::x11::X11::open(title, size, min, deliver.clone())
                .map(|backend| Box::new(backend) as Box<dyn Backend>),
        };
        match result {
            Ok(backend) => return Ok(backend),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_shift_by_case_and_caps_lock_only_touches_letters() {
        let a = u32::from('a');
        assert_eq!(level_keysym(a, 0, false, false), a);
        assert_eq!(level_keysym(a, 0, true, false), u32::from('A'));
        assert_eq!(level_keysym(a, 0, false, true), u32::from('A'));
        assert_eq!(level_keysym(a, 0, true, true), a);
        let (one, bang) = (u32::from('1'), u32::from('!'));
        assert_eq!(level_keysym(one, bang, false, false), one);
        assert_eq!(level_keysym(one, bang, true, false), bang);
        assert_eq!(level_keysym(one, bang, false, true), one);
        assert_eq!(level_keysym(keys::LEFT, 0, true, true), keys::LEFT);
    }
}
