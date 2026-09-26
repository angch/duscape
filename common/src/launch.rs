//! Handing an entry to the desktop: [`open`] a file with its default app, and [`reveal`] entries
//! in the file manager with them selected — `open` and `open -R` on macOS, the shell's
//! `ShellExecuteW` and `explorer.exe /select` on Windows, `xdg-open` and the freedesktop
//! `org.freedesktop.FileManager1` interface on Linux and the BSDs.
//!
//! Neither waits for the program it starts: a window calls them on its own thread. What is
//! reported is whether the program could be started; what it then does is the desktop's.

use ::std::path::Path;
use ::std::process::{Child, Command, Stdio};

/// Open `path` as the desktop would on a double-click: a file with its default app.
pub fn open(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        shell_open(path)
    }
    #[cfg(not(windows))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let mut command = Command::new(program);
        command.arg(path);
        spawn(command, "open it")
    }
}

/// Show `paths` in the file manager, selected. Where the file manager cannot be asked to select
/// (on Linux, nothing answers for `FileManager1` on the session bus), the first one's folder is
/// opened instead.
pub fn reveal(paths: &[&Path]) -> Result<(), String> {
    let Some(first) = paths.first() else {
        return Ok(());
    };
    #[cfg(target_os = "macos")]
    {
        let _ = first;
        let mut command = Command::new("open");
        command.arg("-R").args(paths);
        spawn(command, "show it in Finder")
    }
    #[cfg(windows)]
    {
        use ::std::os::windows::process::CommandExt;
        // Explorer selects one entry, and reads `/select,"path"` itself rather than by the
        // usual rules: a Windows path cannot hold a quote, so this is always well formed.
        let mut command = Command::new("explorer.exe");
        command.raw_arg(format!("/select,\"{}\"", first.display()));
        spawn(command, "show it in Explorer")
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        // The bus call waits for its answer, and a file manager being started to give one can
        // take seconds: it is made on a thread of its own, so the window never waits for it.
        let uris: Vec<String> = paths.iter().map(|path| file_uri(path)).collect();
        let folder = first.parent().unwrap_or(first).to_path_buf();
        ::std::thread::Builder::new()
            .name("reveal".to_string())
            .spawn(move || {
                let shown = Command::new("dbus-send")
                    .args([
                        "--session",
                        "--print-reply",
                        "--reply-timeout=5000",
                        "--dest=org.freedesktop.FileManager1",
                        "--type=method_call",
                        "/org/freedesktop/FileManager1",
                        "org.freedesktop.FileManager1.ShowItems",
                    ])
                    .arg(format!("array:string:{}", uris.join(",")))
                    .arg("string:")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if !shown {
                    let _ = open(&folder);
                }
            })
            .map(drop)
            .map_err(|error| format!("Could not show it in the file manager: {error}"))
    }
}

/// Start `command` with no terminal attached, and wait for it on a thread of its own, so that
/// it is reaped when it ends rather than left a zombie until the window closes.
#[cfg_attr(windows, allow(dead_code))]
fn spawn(mut command: Command, what: &str) -> Result<(), String> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Could not {what}: {error}"))?;
    reap(child);
    Ok(())
}

fn reap(mut child: Child) {
    let _ = ::std::thread::Builder::new()
        .name("launched".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// The shell's own open, which reads the file's association as a double-click in Explorer
/// does. (`explorer.exe <file>` also opens it, but takes a comma in the path for a switch.)
#[cfg(windows)]
fn shell_open(path: &Path) -> Result<(), String> {
    use ::std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide =
        |text: &::std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
    let verb = wide("open".as_ref());
    let file = wide(path.as_os_str());
    // SAFETY: both strings are NUL-terminated and alive for the call; the rest may be null.
    let result = unsafe {
        ShellExecuteW(
            ::std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ::std::ptr::null(),
            ::std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Above 32 is success; at or below, an error code (`shellapi.h`).
    if result as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "Could not open it: no app for it, or it could not start (error {})",
            result as isize
        ))
    }
}

/// `path` as a `file://` URI, every byte outside the unreserved set and `/` percent-encoded. A
/// comma is encoded too, since `dbus-send` splits an array's items at commas.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_uri_encodes_what_is_not_plain() {
        assert_eq!(
            file_uri(Path::new("/home/me/My Music,old/ü.mp3")),
            "file:///home/me/My%20Music%2Cold/%C3%BC.mp3"
        );
    }

    #[test]
    fn nothing_to_reveal_is_no_error() {
        assert_eq!(reveal(&[]), Ok(()));
    }
}
