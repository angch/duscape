//! Running as administrator: elevated, the scan of a whole volume reads its master file table
//! instead of walking it, counts every hard link and sizes NTFS's own files, and folders the
//! user cannot read open. So when the scan root is a volume and the process is not elevated,
//! the viewer asks — the UAC prompt — and starts itself again elevated, the same arguments plus
//! `--no-elevate` so the new process never asks in turn; declined, it scans as it is.
//!
//! The decision and the command line are plain and tested everywhere; only [`relaunch`] calls
//! the shell.

use ::std::ffi::OsString;
use ::std::path::{Component, Path, Prefix};

/// The flag that stops the asking, which the elevated process is started with.
pub const NO_ELEVATE: &str = "--no-elevate";

/// Whether to ask: `root` is a volume root, the process is not elevated, and nobody opted out.
/// Whether the volume is a local disk is [`is_local_disk`]'s to say.
#[must_use]
pub fn wanted(root: &Path, opted_out: bool, is_admin: bool) -> bool {
    // A drive and a root and nothing else: `C:\`, `\\?\D:\`, `/`. A relative path — `.`, or
    // one with a `..` in it — is not one, whatever it resolves to; the caller resolves it first.
    // Nor is a share (`\\server\share\`): elevation reads a local volume's table, and gains
    // nothing on another machine's files.
    let mut components = root.components().peekable();
    let is_volume_root = components.peek().is_some()
        && components.all(|component| match component {
            Component::Prefix(prefix) => {
                matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            }
            Component::RootDir => true,
            _ => false,
        });
    is_volume_root && !opted_out && !is_admin
}

/// Whether `root` is on a local disk — fixed or removable — rather than a share, a CD or a
/// RAM disk: where reading the volume's table elevated can pay. Anywhere but Windows, yes.
#[must_use]
pub fn is_local_disk(root: &Path) -> bool {
    #[cfg(windows)]
    {
        use ::std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

        // `GetDriveTypeW`'s answers for a local disk (`winbase.h`).
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;

        let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: the path is NUL-terminated and alive for the call.
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        true
    }
}

/// The arguments the elevated process is started with: [`NO_ELEVATE`], then this process's
/// (`args`, without the program's name), then the folder if it was chosen in the dialog rather
/// than given (`picked`), so the new process does not ask for it again.
#[must_use]
pub fn relaunch_args(args: impl Iterator<Item = OsString>, picked: Option<&Path>) -> Vec<OsString> {
    let mut out = vec![OsString::from(NO_ELEVATE)];
    out.extend(args.skip(1));
    if let Some(folder) = picked {
        out.push(folder.as_os_str().to_owned());
    }
    out
}

/// `args` as one command line, quoted the way `CommandLineToArgvW` reads it back: an argument
/// with a space, a tab or a quote is wrapped in quotes, a quote inside escaped with a backslash,
/// and backslashes before a quote (or the closing quote) doubled.
#[must_use]
pub fn command_line(args: &[OsString]) -> String {
    let mut line = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        let arg = arg.to_string_lossy();
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            line.push_str(&arg);
            continue;
        }
        line.push('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    line.extend(::std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                _ => {
                    line.extend(::std::iter::repeat_n('\\', backslashes));
                    line.push(c);
                    backslashes = 0;
                }
            }
        }
        line.extend(::std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }
    line
}

/// Why the elevated process did not start.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The user said no at the prompt.
    Declined,
    /// The shell would not start it: the Win32 error.
    Failed(u32),
}

/// Start this program again elevated with `args`, through the shell's `runas`. `Ok` means the
/// new process is running and this one should end.
#[cfg(windows)]
pub fn relaunch(args: &[OsString]) -> Result<(), Refused> {
    use ::std::os::windows::ffi::OsStrExt;
    use ::std::ptr::null;

    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = ::std::env::current_exe().map_err(|error| {
        Refused::Failed(
            error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok())
                .unwrap_or(0),
        )
    })?;
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let verb = wide("runas");
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let parameters = wide(&command_line(args));
    // The new process starts in this one's directory, so a folder given relative to it — the
    // arguments are passed on as given — names the same place.
    let directory: Vec<u16> = ::std::env::current_dir()
        .map(|dir| dir.as_os_str().encode_wide().chain(Some(0)).collect())
        .unwrap_or_else(|_| vec![0]);
    // SAFETY: every string is NUL-terminated and outlives the call; no window is needed.
    let started = unsafe {
        ShellExecuteW(
            null::<::std::ffi::c_void>() as _,
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            directory.as_ptr(),
            SW_SHOWNORMAL,
        )
    };
    // The shell's convention: a value over 32 is success, anything else an error code.
    if started as isize > 32 {
        return Ok(());
    }
    // SAFETY: reading the calling thread's last error.
    let error = unsafe { GetLastError() };
    if error == ERROR_CANCELLED {
        Err(Refused::Declined)
    } else {
        Err(Refused::Failed(error))
    }
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;
    use ::std::path::Path;

    use super::{NO_ELEVATE, command_line, relaunch_args, wanted};

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_volume_root_asks_unless_elevated_or_opted_out() {
        // A drive letter is a path prefix only on Windows; elsewhere `C:\` is a file's name.
        let roots: &[&str] = if cfg!(windows) {
            &[r"C:\", r"\\?\C:\", r"D:\", "/"]
        } else {
            &["/"]
        };
        for &root in roots {
            assert!(wanted(Path::new(root), false, false), "{root}");
            assert!(!wanted(Path::new(root), true, false), "{root} opted out");
            assert!(
                !wanted(Path::new(root), false, true),
                "{root} elevated already"
            );
        }
        // A relative path is never one, even `.` in a volume root: the caller resolves it.
        // (`C:\.` is `C:\` to `Path`, and a volume root.)
        for root in [
            r"C:\Users",
            r"\\?\C:\Windows\",
            "/home",
            "project",
            ".",
            "..",
            "",
            // A share is not a volume of this machine.
            r"\\server\share\",
            r"\\?\UNC\server\share\",
        ] {
            assert!(
                !wanted(Path::new(root), false, false),
                "{root} is a subtree"
            );
        }
    }

    /// The system drive is a local disk, given plainly or as the resolved root the viewer
    /// decides on (`\\?\C:\`), which `GetDriveTypeW` must take as well.
    #[cfg(windows)]
    #[test]
    fn the_system_drive_is_a_local_disk_by_either_spelling() {
        let drive = ::std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        for root in [format!("{drive}\\"), format!("\\\\?\\{drive}\\")] {
            assert!(super::is_local_disk(Path::new(&root)), "{root}");
        }
        // A letter no drive has: whichever one this machine leaves free (all 26 in use — no
        // absent letter to try, and nothing to assert).
        // SAFETY: no preconditions.
        let used = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        if let Some(free) = (b'A'..=b'Z').find(|letter| used & (1 << (letter - b'A')) == 0) {
            let root = format!("{}:\\", free as char);
            assert!(
                !super::is_local_disk(Path::new(&root)),
                "{root} is no drive"
            );
        }
    }

    #[test]
    fn the_elevated_process_gets_the_same_arguments_and_never_asks_again() {
        let mine = args(&["diskonaut-windows.exe", "-a", "--max-depth", "3"]);
        assert_eq!(
            relaunch_args(mine.clone().into_iter(), None),
            args(&[NO_ELEVATE, "-a", "--max-depth", "3"])
        );
        // Chosen in the dialog, the folder is passed on so it is not asked for again.
        assert_eq!(
            relaunch_args(mine.into_iter(), Some(Path::new(r"C:\"))),
            args(&[NO_ELEVATE, "-a", "--max-depth", "3", r"C:\"])
        );
    }

    #[test]
    fn the_command_line_is_quoted_as_the_shell_reads_it() {
        assert_eq!(command_line(&args(&["-a", r"C:\"])), r#"-a C:\"#);
        assert_eq!(
            command_line(&args(&["--no-elevate", r"C:\My Files\"])),
            r#"--no-elevate "C:\My Files\\""#
        );
        assert_eq!(command_line(&args(&[r#"say "hi""#])), r#""say \"hi\"""#);
        assert_eq!(
            command_line(&args(&[r#"back\\"quote"#])),
            r#""back\\\\\"quote""#
        );
        assert_eq!(command_line(&args(&[""])), r#""""#);
    }
}
