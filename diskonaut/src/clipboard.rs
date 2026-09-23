//! Putting text on the system clipboard from a terminal program.
//!
//! The native clipboard is tried first: `pbcopy` on macOS, the Win32 clipboard on Windows, and
//! `wl-copy`, `xclip` or `xsel` on a Linux or BSD desktop. Where there is none — typically a
//! session over SSH, where the clipboard that matters is on the machine the terminal runs on — the
//! text is sent to the terminal itself as an OSC 52 sequence, which iTerm2, kitty, WezTerm,
//! Windows Terminal, foot, Alacritty and tmux (with `set-clipboard on`) put on their own
//! clipboard. A terminal that does not support OSC 52 ignores it, as it ignores any unknown OSC.

use ::std::io::{self, Write};
use ::std::process::{Command, Stdio};
use ::std::time::{Duration, Instant};

/// Somewhere to put copied text. The app holds one; tests give it a recorder.
pub trait Clipboard: Send {
    fn copy(&mut self, text: &str);
}

/// The clipboard of the machine the user sits at, as far as it can be reached.
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&mut self, text: &str) {
        if !copy_native(text) {
            let _ = copy_osc52(&mut io::stdout(), text);
        }
    }
}

/// Send `text` to the terminal's clipboard: `ESC ] 52 ; c ; <base64> BEL`.
fn copy_osc52(out: &mut impl Write, text: &str) -> io::Result<()> {
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

/// Standard base64 with padding, which is what OSC 52 expects.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0u32, |group, (index, &byte)| {
                group | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(group >> (18 - 6 * index)) as usize & 0x3f] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

/// Feed `text` to a clipboard command's standard input. `false` if it is not installed, will not
/// start, or fails.
///
/// The X11 and Wayland tools keep a process behind to serve the selection and return once they
/// have it, so the wait is short; it is bounded anyway, and a command still running after it is
/// assumed to have taken the text.
#[cfg_attr(windows, allow(dead_code))]
fn pipe_to(program: &str, args: &[&str], text: &str) -> bool {
    let child = Command::new(program)
        .args(args)
        // `pbcopy` decodes its input by the locale, and turns UTF-8 into mojibake under `C`
        // (`é` pastes as `√©`). `LC_ALL` outranks `LC_CTYPE`, so it has to go too.
        .env_remove("LC_ALL")
        .env("LC_CTYPE", "UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return written && status.success(),
            Ok(None) if Instant::now() < deadline => ::std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => return written,
            Err(_) => return false,
        }
    }
}

#[cfg(target_os = "macos")]
fn copy_native(text: &str) -> bool {
    pipe_to("pbcopy", &[], text)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn copy_native(text: &str) -> bool {
    let has = |variable| ::std::env::var_os(variable).is_some_and(|value| !value.is_empty());
    (has("WAYLAND_DISPLAY") && pipe_to("wl-copy", &[], text))
        || (has("DISPLAY")
            && (pipe_to("xclip", &["-selection", "clipboard"], text)
                || pipe_to("xsel", &["--clipboard", "--input"], text)))
}

#[cfg(windows)]
fn copy_native(text: &str) -> bool {
    windows_clipboard::set_text(text)
}

#[cfg(not(any(unix, windows)))]
fn copy_native(_text: &str) -> bool {
    false
}

#[cfg(windows)]
mod windows_clipboard {
    use ::std::ptr::null_mut;
    use ::std::thread::sleep;
    use ::std::time::Duration;

    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    /// Put `text` on the clipboard as UTF-16. The clipboard is shared and briefly held by whoever
    /// last touched it, so opening it is retried for a moment.
    pub fn set_text(text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        // SAFETY: every handle is checked before use; the allocation is handed to the clipboard
        // on success and freed here on failure; the clipboard is closed on every path that opened
        // it.
        unsafe {
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(null_mut()) != 0 {
                    opened = true;
                    break;
                }
                sleep(Duration::from_millis(10));
            }
            if !opened {
                return false;
            }
            let copied = (|| {
                if EmptyClipboard() == 0 {
                    return false;
                }
                let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * size_of::<u16>());
                if memory.is_null() {
                    return false;
                }
                let target = GlobalLock(memory).cast::<u16>();
                if target.is_null() {
                    GlobalFree(memory);
                    return false;
                }
                target.copy_from_nonoverlapping(wide.as_ptr(), wide.len());
                GlobalUnlock(memory);
                if SetClipboardData(u32::from(CF_UNICODETEXT), memory).is_null() {
                    GlobalFree(memory);
                    return false;
                }
                true
            })();
            CloseClipboard();
            copied
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{base64, copy_osc52};

    #[test]
    fn base64_pads_every_remainder() {
        let vectors = [
            ("", ""),
            ("a", "YQ=="),
            ("ab", "YWI="),
            ("abc", "YWJj"),
            ("abcd", "YWJjZA=="),
            ("abcde", "YWJjZGU="),
            ("abcdef", "YWJjZGVm"),
        ];
        for (raw, encoded) in vectors {
            assert_eq!(base64(raw.as_bytes()), encoded, "{raw:?}");
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn osc52_frames_the_text() {
        let mut out = Vec::new();
        copy_osc52(&mut out, "'a b'").expect("write");
        assert_eq!(out, b"\x1b]52;c;J2EgYic=\x07");
    }
}
