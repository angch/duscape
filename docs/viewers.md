# The other viewers

The terminal viewer is the primary one. The same walker and model also drive three native
windows and an MS-DOS port. `features.md` lists what each offers.

## macOS GUI (experimental)

`diskonaut-mac` is a native macOS window on the same walker and model, drawn with AppKit (through
`objc2`). A list of the folder's entries sits beside the treemap, with the entry in hand previewed
under it; the treemap fills in live while the scan runs.

```sh
cargo run -p diskonaut-mac --release -- ~   # or run with no argument, or drop a folder on the window
```

- **Mac conventions:** ⌘⌫ moves to the Trash (⌥⌘⌫ deletes immediately), Space is Quick Look,
  ⌘↑/⌘↓ go up and in, ⌥⌘R shows in Finder, ⌘C copies the path, right-click for a context menu.
- **Marks:** ⇧-arrows, ⇧-click and ⌘-click mark several entries; Trash, copy and Finder act on all.
- **The list is a tree and the treemap nested**, as on Windows: → opens a folder in place, ←
  closes it, or click its expander; a folder's tile holds its entries' tiles down to the files.
- **Also:** breadcrumbs, zoom (⌘+/⌘-/⌘0, a pinch or a mouse's wheel over the treemap), apparent
  sizes (`a`), rescans (⌘R/⇧⌘R), a binary file previewed as a hex dump, light and dark mode.

## Linux GUI (experimental)

`diskonaut-linux` is a window on the same walker and model for Linux and FreeBSD, drawn with no
toolkit at all: the frame is painted in software and put on the screen by one of two backends,
native Wayland (`wayland-client`, `xdg-shell`, a `wl_shm` buffer) or X11 (`x11rb`), both pure
Rust, with text from the system's fonts (`fontconfig`'s sans-serif, rasterised by `fontdue`).
Nothing is linked from the system — not libwayland, not Xlib — so it builds static
(`--target x86_64-unknown-linux-musl`, about **1.7 MB**) and runs on any compositor or X server.
It shares its state — the layout, what is in hand, marks, navigation, rescans — with the macOS
viewer (`viewers/shared/`).

```sh
cargo run -p diskonaut-linux --release -- ~   # or run with no argument for the current folder
```

- **The same window as the Mac's:** breadcrumbs, the list beside the treemap with the entry in hand
  previewed under it (text, PNG and JPEG, a binary file as a hex dump under where its blocks
  are), live while the scan runs; a status bar. The list is a tree (→ opens a folder in place,
  ← closes it, or its expander) and the treemap is nested, as on Windows.
- **Keys:** arrows, Enter/Esc, Tab, Page Up/Down, Home/End; `d` or Delete moves to the Trash
  (freedesktop, through `gio trash` when it is installed), `D` or Shift+Delete deletes at once,
  each after asking; `a` apparent sizes, `+`/`-`/`0` zoom, `r`/`R` rescan, `s` hides the list,
  Ctrl+C copies the path (Ctrl+Shift+C the full path), Ctrl+A marks everything, `q` quits.
- **Mouse:** click, double-click to open, Ctrl+click and Shift+click to mark, right-click for the
  context menu (the same as the Mac's and Windows's: open, show in the file manager, copy, rescan,
  trash or delete), the wheel scrolls the list and zooms the treemap, breadcrumbs or the back button to
  go up.
- **Wayland or X11:** Wayland when `WAYLAND_DISPLAY` is set, else X11; `DISKONAUT_BACKEND=x11`
  or `wayland` picks. On Wayland the compositor is asked for a title bar (`xdg-decoration`); where
  it draws none (GNOME) the window draws its own, with move, maximise and close.
- **HiDPI:** on Wayland the compositor's scale; on X11 `Xft.dpi` (or `GDK_SCALE`, or
  `DISKONAUT_SCALE`).

Why not GTK or Qt: both need their development packages to build and their libraries to run, which
rules out the static binaries this fork ships, and their Rust bindings bring hundreds of crates for
a window that draws one picture. See [`viewers/linux/README.md`](../viewers/linux/README.md).

## Windows GUI (experimental)

`diskonaut-windows` is the terminal viewer in a native window: the same walker (`diskonaut-scan`), and
the same tree, treemap, deletion, previews and rescans (`libdiskonaut`), with the list and the
treemap side by side — the list as a tree whose folders open in place, and the treemap nested,
each folder's tile holding its entries' tiles. What the window shows and does is `diskonaut-viewer`, the state the macOS
and Linux windows share, so the three behave alike. It is built on `windows-sys` and GDI rather than a GUI framework, so the
release binary is about **840 KB**, most of it the PNG and JPEG decoders for the preview.
[`features.md`](features.md) compares the viewers feature by feature.

```sh
cargo run -p diskonaut-windows --release -- C:\   # or run with no argument for a folder picker
```

It takes the terminal viewer's scan flags: `-a`, `--max-depth`, `--threads`,
`--hard-link-threshold`. Given a whole volume unelevated it asks to run as administrator and
starts itself again elevated (`--no-elevate` scans as it is): elevated, the volume is read from
its master file table and every folder opens.

- **Live:** the treemap and list fill in as the scan runs, as in the terminal.
- **The list is a tree**, as WizTree's: → on a folder opens it in place, its entries indented
  under it with each one's share of its parent as a bar, to any depth; → again goes down into
  it, ← goes back up to the folder and then closes it; the expander (`▸`) before a folder's name
  does the same with the mouse. A nested entry is previewed, copied, entered (Enter goes down
  through the folders above it) or deleted where it is; the treemap highlights its top-level
  folder.
- **Navigation:** arrows move through the list or the treemap (Tab switches); Enter or a double
  click opens a folder; Esc, Backspace, a breadcrumb or the mouse's back button goes up.
- **Marks:** Ctrl+click, Shift+↑↓ or Shift+click marks several entries, Ctrl+A all of them, and
  each copies their paths.
- **Copy:** Ctrl+C copies the path of what is marked or in hand, relative to where the viewer was
  started and quoted for PowerShell; Ctrl+Shift+C the full path.
- **Delete:** Del or `d` deletes what is marked, or the entry in hand, after saying what and how
  much; NTFS's own metadata files are refused.
- **Preview:** a file in hand is shown under the list — text in a monospace font, a PNG or JPEG
  as a picture.
- **Right-click** opens a menu: open, copy, rescan, delete.
- **Keys:** `a` switches disk usage and apparent size, `+` / `-` / `0` zoom (or the wheel over
  the treemap), `r` / `R` (or F5 / Shift+F5) rescan the folder or everything, `s` hides the panel.
- **Details:** the status bar says what is in hand or under the pointer and how the scan went —
  entries, unreadable ones, how much of the volume it could not see, what was freed, rescans under
  way; the window is DPI-aware and double-buffered.

## MS-DOS

`viewers/dos/` is the treemap for MS-DOS: a 16-bit real-mode program in assembly (FASM), about
**29 KB**, that runs in DOSBox-X or on a 286 with no coprocessor (the treemap's doubles are
computed in software, bit for bit as Rust's f64). It is not built from the Rust code but
ported from it — the squarify layout, navigation, zoom, tile text, the side panel and size formats
— and its tiles match `libdiskonaut`'s on every folder it was compared on. It scans with long file
names where DOS has them, draws the treemap live during the scan, deletes and rescans, and
previews text, PNG and JPEG files beside the treemap, pictures in half blocks with six of text
mode's 16 colours set to the picture's own.

```sh
make dos-run    # needs dosbox-x; fetches FASM, assembles, opens this repository as C:
```

See [`viewers/dos/README.md`](../viewers/dos/README.md).

