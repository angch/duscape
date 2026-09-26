//! The Windows window alone, a GUI-subsystem executable with no console; `duscape` starts it
//! too (`--gui`, or when started from Explorer), from a console-subsystem executable that can
//! also be the terminal viewer.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    duscape_windows::run();
}
