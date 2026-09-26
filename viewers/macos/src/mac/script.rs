//! Driving the window from a script, for testing it without a person or any permission.
//!
//! `DUSCAPE_MAC_SCRIPT=steps.txt duscape-mac FOLDER` runs the steps in `steps.txt` once the
//! scan has finished, one line each (`#` starts a comment):
//!
//! ```text
//! key down                 a key: a name or a character, with modifiers: cmd+opt+backspace
//! click 400 300            a click at a point in the view (top-left origin, points)
//! click 400 300 2          a double click
//! click 400 300 1 cmd      a click with modifiers (cmd, shift, opt, ctrl)
//! rclick 400 300           a right click, which opens the context menu
//! menu Copy Path           choose from the open context menu (which reads events itself while
//!                          it is open, so keys do not reach it); `menu cancel` closes it
//! wait 500                 milliseconds (each step already waits `STEP` after it)
//! snap out.png             the view, drawn to a PNG
//! state out.txt            what the viewer holds, as `name: value` lines, to compare
//! clipboard out.txt        the pasteboard's text
//! quit
//! ```
//!
//! Key names: `left` `right` `up` `down` `return` `enter` `esc` `backspace` (⌫) `delete` (⌦)
//! `tab` `space` `home` `end` `pageup` `pagedown` `plus`; any other single character is itself
//! (`a`, `R`, `0`). Modifiers: `cmd` `shift` `opt` `ctrl`, joined with `+` (so ⌘+ is `cmd+plus`).
//! A script runs once, after the first scan; a later scan in the same run does not start it again.
//!
//! The events are made here and posted to the application's own queue, so they take the path a
//! real key or click takes: the menus' key equivalents, the first responder, hit testing, and the
//! event loops of alerts and context menus, which read that queue while they are open. Nothing
//! is posted to the system, so no Accessibility permission is needed — only a logged-in session,
//! for the window server.

use ::std::path::{Path, PathBuf};
use ::std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString,
};
use objc2_foundation::{NSPoint, NSString};

use super::view::{DiskView, on_main};

/// How long each step is given before the next: long enough for a redraw, a preview, or an
/// alert to open.
const STEP: Duration = Duration::from_millis(250);

enum Step {
    Key(String),
    Click {
        x: f64,
        y: f64,
        count: isize,
        flags: NSEventModifierFlags,
        right: bool,
    },
    Menu(Option<String>),
    Wait(Duration),
    Snap(PathBuf),
    State(PathBuf),
    Clipboard(PathBuf),
    Quit,
}

fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, line) in script.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let bad = || format!("line {}: cannot read `{line}`", number + 1);
        let number_at = |index: usize| -> Result<f64, String> {
            words
                .get(index)
                .and_then(|w| w.parse().ok())
                .ok_or_else(bad)
        };
        let step = match words[0] {
            "key" if words.len() == 2 => Step::Key(words[1].to_string()),
            word @ ("click" | "rclick") => Step::Click {
                x: number_at(1)?,
                y: number_at(2)?,
                count: words
                    .get(3)
                    .map_or(Ok(1), |w| w.parse().map_err(|_| bad()))?,
                flags: words
                    .get(4)
                    .map_or(Ok(NSEventModifierFlags::empty()), |w| {
                        modifiers(w).ok_or_else(bad)
                    })?,
                right: word == "rclick",
            },
            "menu" if words.len() >= 2 => match line["menu".len()..].trim() {
                "cancel" => Step::Menu(None),
                title => Step::Menu(Some(title.to_string())),
            },
            "wait" => Step::Wait(Duration::from_millis(number_at(1)? as u64)),
            "snap" if words.len() == 2 => Step::Snap(words[1].into()),
            "state" if words.len() == 2 => Step::State(words[1].into()),
            "clipboard" if words.len() == 2 => Step::Clipboard(words[1].into()),
            "quit" => Step::Quit,
            _ => return Err(bad()),
        };
        steps.push(step);
    }
    Ok(steps)
}

fn modifiers(words: &str) -> Option<NSEventModifierFlags> {
    words
        .split('+')
        .try_fold(NSEventModifierFlags::empty(), |flags, word| {
            Some(
                flags
                    | match word {
                        "cmd" => NSEventModifierFlags::Command,
                        "shift" => NSEventModifierFlags::Shift,
                        "opt" => NSEventModifierFlags::Option,
                        "ctrl" => NSEventModifierFlags::Control,
                        _ => return None,
                    },
            )
        })
}

/// A key's hardware code and the characters it types, by name or as its own character.
fn key(name: &str) -> Option<(u16, String)> {
    let (code, characters) = match name {
        "left" => (123, "\u{f702}"),
        "right" => (124, "\u{f703}"),
        "down" => (125, "\u{f701}"),
        "up" => (126, "\u{f700}"),
        "return" => (36, "\r"),
        "enter" => (76, "\u{3}"),
        "esc" => (53, "\u{1b}"),
        "backspace" => (51, "\u{7f}"),
        "delete" => (117, "\u{f728}"),
        "tab" => (48, "\t"),
        "space" => (49, " "),
        "home" => (115, "\u{f729}"),
        "end" => (119, "\u{f72b}"),
        "pageup" => (116, "\u{f72c}"),
        "pagedown" => (121, "\u{f72d}"),
        "plus" => (24, "+"),
        _ => {
            let mut characters = name.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            // US-layout codes; the menus match on the characters, the view on these for the
            // keys above only.
            const CODES: &str = "asdfhgzxcv bqweryt123465=97-80]ou[ip lj'k;\\,/nm.";
            let lower = character.to_ascii_lowercase();
            let code = CODES.find(lower).unwrap_or(0) as u16;
            return Some((code, character.to_string()));
        }
    };
    Some((code, characters.to_string()))
}

/// Run the script at the path `DUSCAPE_MAC_SCRIPT` names, if it does, and quit when it ends.
pub fn run_from_environment() -> bool {
    let Some(path) = ::std::env::var_os("DUSCAPE_MAC_SCRIPT") else {
        return false;
    };
    // Once: a script that scans another folder must not start itself again when that finishes.
    static STARTED: ::std::sync::atomic::AtomicBool = ::std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, ::std::sync::atomic::Ordering::AcqRel) {
        return true;
    }
    let steps = match ::std::fs::read_to_string(&path)
        .map_err(|error| format!("{}: {error}", Path::new(&path).display()))
        .and_then(|script| parse(&script))
    {
        Ok(steps) => steps,
        Err(error) => {
            eprintln!("duscape-mac: script: {error}");
            ::std::process::exit(2);
        }
    };
    // Steps are posted from a thread of their own, never waited on: a key that opens an alert
    // does not return until the alert closes, and it is a later step that closes it.
    ::std::thread::spawn(move || {
        for step in steps {
            match step {
                Step::Wait(duration) => ::std::thread::sleep(duration),
                Step::Quit => break,
                step => {
                    make_active();
                    on_main(move |view| perform(view, step));
                }
            }
            ::std::thread::sleep(STEP);
        }
        DispatchQueue::main().exec_async(|| {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            let app = NSApplication::sharedApplication(mtm);
            // Close an alert the script left open, or `terminate:` waits for it.
            app.abortModal();
            app.terminate(None);
        });
    });
    true
}

/// Bring the application to the front and wait until it is, up to a second. An inactive
/// application gets keys but not its menus' key equivalents, so another app taking the focus
/// mid-script would fail every shortcut after it; activation is asynchronous. `state` records
/// whether it was active, to tell that apart from a real failure.
fn make_active() {
    for _ in 0..20 {
        let (tx, rx) = ::std::sync::mpsc::channel();
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
            let app = NSApplication::sharedApplication(mtm);
            let active = app.isActive();
            if !active {
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
            }
            let _ = tx.send(active);
        });
        if rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false) {
            return;
        }
        ::std::thread::sleep(Duration::from_millis(50));
    }
}

fn perform(view: &DiskView, step: Step) {
    let mtm = view.mtm();
    let app = NSApplication::sharedApplication(mtm);
    match step {
        Step::Key(spec) => {
            let (flags, name) = match spec.rsplit_once('+') {
                Some((mods, name)) if !name.is_empty() => match modifiers(mods) {
                    Some(flags) => (flags, name),
                    None => return eprintln!("script: unknown modifiers in {spec}"),
                },
                _ => (NSEventModifierFlags::empty(), spec.as_str()),
            };
            let Some((code, characters)) = key(name) else {
                return eprintln!("script: unknown key {name}");
            };
            // The arrows and the keys above them are function keys, as the keyboard says.
            let mut flags = flags;
            if characters
                .chars()
                .next()
                .is_some_and(|c| ('\u{f700}'..='\u{f8ff}').contains(&c))
            {
                flags |= NSEventModifierFlags::Function;
                if (123..=126).contains(&code) {
                    flags |= NSEventModifierFlags::NumericPad;
                }
            }
            // To whichever window has the keyboard: an alert's, while one is open.
            let window = app.keyWindow().or_else(|| view.window());
            let number = window.map_or(0, |window| window.windowNumber());
            let characters = NSString::from_str(&characters);
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown,
                NSPoint::ZERO,
                flags,
                0.0,
                number,
                None,
                &characters,
                &characters,
                false,
                code,
            );
            if let Some(event) = event {
                app.postEvent_atStart(&event, false);
            }
        }
        Step::Click {
            x,
            y,
            count,
            flags,
            right,
        } => {
            let Some(window) = view.window() else {
                return;
            };
            let at = view.convertPoint_toView(NSPoint::new(x, y), None);
            let (down, up) = if right {
                (NSEventType::RightMouseDown, NSEventType::RightMouseUp)
            } else {
                (NSEventType::LeftMouseDown, NSEventType::LeftMouseUp)
            };
            for (kind, clicks) in (1..=count).flat_map(|n| [(down, n), (up, n)]) {
                let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    kind,
                    at,
                    flags,
                    0.0,
                    window.windowNumber(),
                    None,
                    0,
                    clicks,
                    1.0,
                );
                if let Some(event) = event {
                    app.postEvent_atStart(&event, false);
                }
            }
        }
        Step::Menu(title) => {
            if !view.choose_from_context_menu(title.as_deref()) {
                eprintln!("script: no context menu open, or no item {title:?} in it");
            }
        }
        Step::Snap(path) => view.snapshot(&path),
        Step::State(path) => {
            if let Err(error) = ::std::fs::write(&path, view.state_for_script()) {
                eprintln!("script: {}: {error}", path.display());
            }
        }
        Step::Clipboard(path) => {
            // SAFETY: reading AppKit's pasteboard type constant.
            let text = NSPasteboard::generalPasteboard()
                .stringForType(unsafe { NSPasteboardTypeString })
                .map(|text| text.to_string())
                .unwrap_or_default();
            if let Err(error) = ::std::fs::write(&path, text) {
                eprintln!("script: {}: {error}", path.display());
            }
        }
        Step::Wait(_) | Step::Quit => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_read_line_by_line() {
        let steps =
            parse("key down\n# a comment\n\nclick 10 20 2 cmd+shift\nwait 5\nquit").unwrap();
        assert_eq!(steps.len(), 4);
        assert!(matches!(&steps[0], Step::Key(key) if key == "down"));
        assert!(matches!(
            steps[1],
            Step::Click { count: 2, right: false, flags, .. }
                if flags == NSEventModifierFlags::Command | NSEventModifierFlags::Shift
        ));
        assert!(
            matches!(&parse("menu Copy Path").unwrap()[0], Step::Menu(Some(t)) if t == "Copy Path")
        );
        assert!(matches!(parse("menu cancel").unwrap()[0], Step::Menu(None)));
        assert!(parse("click ten 20").is_err());
        assert!(parse("dance").is_err());
    }

    #[test]
    fn keys_by_name_or_character() {
        assert_eq!(key("down"), Some((125, "\u{f701}".to_string())));
        assert_eq!(key("r"), Some((15, "r".to_string())));
        assert_eq!(key("R"), Some((15, "R".to_string())));
        assert_eq!(key("nonsense"), None);
    }
}
