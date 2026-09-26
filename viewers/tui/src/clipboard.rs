//! Putting text on the system clipboard from a terminal program.
//!
//! The native clipboard is tried first ([`libduscape::clipboard::copy`]: `pbcopy` on macOS, the
//! Win32 clipboard on Windows, and `wl-copy`, `xclip` or `xsel` on a Linux or BSD desktop). Where
//! there is none — typically a
//! session over SSH, where the clipboard that matters is on the machine the terminal runs on — the
//! text is sent to the terminal itself as an OSC 52 sequence, which iTerm2, kitty, WezTerm,
//! Windows Terminal, foot, Alacritty and tmux (with `set-clipboard on`) put on their own
//! clipboard. A terminal that does not support OSC 52 ignores it, as it ignores any unknown OSC.

use ::std::io::{self, Write};

pub use libduscape::clipboard::base64;

/// Somewhere to put copied text. The app holds one; tests give it a recorder.
pub trait Clipboard: Send {
    fn copy(&mut self, text: &str);
}

/// The clipboard of the machine the user sits at, as far as it can be reached.
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&mut self, text: &str) {
        if !libduscape::clipboard::copy(text) {
            let _ = copy_osc52(&mut io::stdout(), text);
        }
    }
}

/// Send `text` to the terminal's clipboard: `ESC ] 52 ; c ; <base64> BEL`.
fn copy_osc52(out: &mut impl Write, text: &str) -> io::Result<()> {
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::copy_osc52;

    #[test]
    fn osc52_frames_the_text() {
        let mut out = Vec::new();
        copy_osc52(&mut out, "'a b'").expect("write");
        assert_eq!(out, b"\x1b]52;c;J2EgYic=\x07");
    }
}
