//! Which viewer `duscape` is: the terminal viewer, or the platform's window (`duscape-linux`,
//! `duscape-windows`, `duscape-mac`, linked into the same binary by the `gui` feature).
//!
//! In order: `--tui` or `--gui` says; `--benchmark`, `--help` and `--version` are the terminal's;
//! a program named for the window (`duscape-gui`, or a viewer's own name, as a link) is the
//! window; then the platform's guess. On Linux and macOS a terminal on stdin or stdout means the
//! terminal viewer — a pipe or a file on one of them is still a terminal session, so a
//! benchmark's output can be redirected — and neither means a launcher started it: the window,
//! where there is a display to put it on. On Windows, no console means Explorer started it —
//! `windows.manifest`'s `detached` policy keeps it from making one, from Windows 11 24H2 on —
//! and so does a console of the program's own, which older Windows makes: that one is let go
//! before the window opens. A console shared with a shell means the terminal viewer, and the
//! terminal viewer with none (a shortcut to `--tui`) is given one.

use ::std::ffi::{OsStr, OsString};
use ::std::path::Path;

/// The flags that choose, which each viewer's command line also accepts and ignores.
pub const GUI: &str = "--gui";
pub const TUI: &str = "--tui";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    Terminal,
    Window,
}

/// What the platform says about how the program was started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Started {
    /// A terminal on stdin, or on stdout. On Windows, a console shared with whatever started the
    /// program (a shell), redirected or not: there the console decides, not the handles.
    pub terminal: bool,
    /// Somewhere to put a window: Linux's `WAYLAND_DISPLAY` or `DISPLAY`; always on macOS and
    /// Windows.
    pub display: bool,
    /// Windows: the console is the program's own, made for it when Explorer started it (or there
    /// is none). Never on other platforms.
    pub own_console: bool,
}

/// Which viewer, for `args` (with the program's name first) started as `started` says.
#[must_use]
pub fn choose(args: &[OsString], started: Started, window_built: bool) -> Front {
    if !window_built {
        return Front::Terminal;
    }
    // Flags only up to `--`: after it, a folder may be named `--gui`.
    let flags = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str());
    for flag in flags {
        match flag {
            TUI => return Front::Terminal,
            GUI => return Front::Window,
            "--benchmark" | "-h" | "--help" | "-V" | "--version" => return Front::Terminal,
            _ => {}
        }
    }
    if args.first().is_some_and(|name| named_for_the_window(name)) {
        return Front::Window;
    }
    if started.own_console || (!started.terminal && started.display) {
        Front::Window
    } else {
        Front::Terminal
    }
}

/// `duscape-gui`, `duscape-linux`, `duscape-windows.exe`, `/usr/bin/duscape-mac`: a link
/// by one of those names, or a copy, is the window.
fn named_for_the_window(program: &OsStr) -> bool {
    let Some(stem) = Path::new(program).file_stem().and_then(OsStr::to_str) else {
        return false;
    };
    ["-gui", "-linux", "-windows", "-mac"]
        .iter()
        .any(|suffix| stem.to_ascii_lowercase().ends_with(suffix))
}

/// `args` without the process serial number (`-psn_0_12345`) Launch Services once added when
/// Finder opened an app: no flag of ours, and a mistake to the command line's parser.
#[must_use]
pub fn without_launch_services(args: Vec<OsString>) -> Vec<OsString> {
    args.into_iter()
        .filter(|arg| !arg.to_str().is_some_and(|arg| arg.starts_with("-psn_")))
        .collect()
}

/// How this process was started.
#[must_use]
pub fn started() -> Started {
    use ::std::io::IsTerminal;
    let own_console = own_console();
    let terminal = if cfg!(windows) {
        !own_console
    } else {
        ::std::io::stdin().is_terminal() || ::std::io::stdout().is_terminal()
    };
    let display = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|name| ::std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    } else {
        true
    };
    Started {
        terminal,
        display,
        own_console,
    }
}

/// Windows: whether this process is alone on its console — Explorer made the console for it —
/// or has none.
#[cfg(windows)]
fn own_console() -> bool {
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    // SAFETY: no arguments.
    if unsafe { GetConsoleWindow() }.is_null() {
        return true;
    }
    let mut processes = [0u32; 2];
    // SAFETY: the buffer holds as many ids as it says; the count of processes comes back.
    let attached = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 2) };
    attached <= 1
}

#[cfg(not(windows))]
fn own_console() -> bool {
    false
}

/// Windows: the terminal viewer with no console to be in — `--tui` or `--help` from a shortcut,
/// started from Explorer, where `windows.manifest` says no console is made — gets one of its own,
/// as a console program always did before.
pub fn ensure_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
        // SAFETY: no arguments; a process with a console already is left as it is.
        unsafe {
            if GetConsoleWindow().is_null() {
                AllocConsole();
            }
        }
    }
}

/// Windows: before the window opens, let go of a console that is the program's own, so it
/// closes rather than sitting empty behind the window. One shared with a shell stays.
pub fn leave_own_console(started: Started) {
    #[cfg(windows)]
    if started.own_console {
        // SAFETY: no arguments; the console closes once no process is attached to it.
        unsafe { windows_sys::Win32::System::Console::FreeConsole() };
    }
    #[cfg(not(windows))]
    let _ = started;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    const TERMINAL: Started = Started {
        terminal: true,
        display: true,
        own_console: false,
    };
    const LAUNCHER: Started = Started {
        terminal: false,
        display: true,
        own_console: false,
    };

    #[test]
    fn in_a_terminal_it_is_the_terminal_viewer() {
        assert_eq!(
            choose(&args(&["duscape", "/"]), TERMINAL, true),
            Front::Terminal
        );
        // With no display either (a console, an ssh session without X).
        let bare = Started {
            display: false,
            ..LAUNCHER
        };
        assert_eq!(choose(&args(&["duscape"]), bare, true), Front::Terminal);
    }

    #[test]
    fn from_a_launcher_or_explorer_it_is_the_window() {
        assert_eq!(choose(&args(&["duscape"]), LAUNCHER, true), Front::Window);
        let explorer = Started {
            own_console: true,
            ..TERMINAL
        };
        assert_eq!(
            choose(&args(&["duscape.exe"]), explorer, true),
            Front::Window
        );
    }

    #[test]
    fn the_flags_decide_and_a_benchmark_is_the_terminals() {
        assert_eq!(
            choose(&args(&["duscape", "--gui", "/"]), TERMINAL, true),
            Front::Window
        );
        assert_eq!(
            choose(&args(&["duscape", "--tui"]), LAUNCHER, true),
            Front::Terminal
        );
        // Redirected, as the benchmark scripts run it.
        assert_eq!(
            choose(&args(&["duscape", "--benchmark", "/"]), LAUNCHER, true),
            Front::Terminal
        );
        assert_eq!(
            choose(&args(&["duscape", "--help"]), LAUNCHER, true),
            Front::Terminal
        );
        // After `--`, a folder's name.
        assert_eq!(
            choose(&args(&["duscape", "--", "--gui"]), TERMINAL, true),
            Front::Terminal
        );
    }

    #[test]
    fn named_for_the_window_it_is_the_window() {
        for name in [
            "duscape-gui",
            "/usr/local/bin/duscape-linux",
            r"C:\tools\duscape-windows.exe",
            "Duscape-Mac",
        ] {
            assert_eq!(
                choose(&args(&[name]), TERMINAL, true),
                Front::Window,
                "{name}"
            );
        }
        assert_eq!(choose(&args(&["duscape"]), TERMINAL, true), Front::Terminal);
    }

    #[test]
    fn on_windows_a_shared_console_is_a_terminal_redirected_or_not() {
        // What `started` reports on Windows for `duscape < nul > out.txt` from cmd: the
        // console is the shell's, so it is a terminal session whatever the handles are.
        let redirected_in_a_shell = Started {
            terminal: true,
            display: true,
            own_console: false,
        };
        assert_eq!(
            choose(&args(&["duscape.exe"]), redirected_in_a_shell, true),
            Front::Terminal
        );
    }

    #[test]
    fn launch_services_serial_numbers_are_dropped() {
        assert_eq!(
            without_launch_services(args(&["duscape", "-psn_0_1234", "-a"])),
            args(&["duscape", "-a"])
        );
    }

    #[test]
    fn built_without_the_window_it_is_always_the_terminal_viewer() {
        assert_eq!(
            choose(&args(&["duscape", "--gui"]), LAUNCHER, false),
            Front::Terminal
        );
    }
}
