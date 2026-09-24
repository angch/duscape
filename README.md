# diskonaut

**diskonaut** is an interactive terminal tool for exploring disk usage. Pick a directory, watch a live treemap fill in as files are scanned, drill into folders, and delete what you no longer need—without leaving the terminal.

## About this fork

This fork exists to **explore further performance optimizations for everyday disk-usage scanning across Linux, macOS, and Windows**. Upstream diskonaut is Unix-native; here each platform gets a native, parallel directory walker (including Windows, which is new), and the scan and tree-build pipeline is reworked for speed. `docs/scan-performance.md` records the measurements and the reasoning.

**Caveat:** these changes are largely **not yet battle-tested**. Treat the fork as experimental — sanity-check reported sizes against a tool you trust, and keep backups before deleting anything. The performance direction is the point; hardened, production-grade reliability is not there yet.

## Features

- **Live scanning** — the treemap updates while the walk is still running
- **Side-by-side list** — beside the treemap, the folder's size, file count and share of the scan,
  and every entry in it, largest first, with a bar, size and percentage
- **Treemap navigation** — proportional tiles for files and folders; zoom for dense directories;
  click a tile to select it, double-click a folder to open it, right-click to copy its path
- **In-session cleanup** — delete files or folders and track space freed in the title bar
- **Apparent or on-disk size** — default shows blocks allocated on disk; `-a` uses logical file size
- **Hard-link aware** — a file reached by several names counts once in each folder that holds it
- **Unix-native** — Linux, macOS, and BSD; built on `ratatui` and parallel directory walking
- **Stays put on request** — `-x` keeps the scan on one filesystem, like `du -x`

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
- **Also:** breadcrumbs, zoom (⌘+/⌘-/⌘0), apparent sizes (`a`), rescans (⌘R/⇧⌘R), light and dark mode.

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
  previewed under it (text, PNG and JPEG), live while the scan runs; a status bar.
- **Keys:** arrows, Enter/Esc, Tab, Page Up/Down, Home/End; `d` or Delete moves to the Trash
  (freedesktop, through `gio trash` when it is installed), `D` or Shift+Delete deletes at once,
  each after asking; `a` apparent sizes, `+`/`-`/`0` zoom, `r`/`R` rescan, `s` hides the list,
  Ctrl+C copies the path, Ctrl+A marks everything, `q` quits.
- **Mouse:** click, double-click to open, Ctrl+click and Shift+click to mark, right-click to copy
  the path, wheel over the list, breadcrumbs to go up.
- **Wayland or X11:** Wayland when `WAYLAND_DISPLAY` is set, else X11; `DISKONAUT_BACKEND=x11`
  or `wayland` picks. On Wayland the compositor is asked for a title bar (`xdg-decoration`); where
  it draws none (GNOME) the window draws its own, with move, maximise and close.
- **HiDPI:** on Wayland the compositor's scale; on X11 `Xft.dpi` (or `GDK_SCALE`, or
  `DISKONAUT_SCALE`).

Why not GTK or Qt: both need their development packages to build and their libraries to run, which
rules out the static binaries this fork ships, and their Rust bindings bring hundreds of crates for
a window that draws one picture. See [`viewers/linux/README.md`](viewers/linux/README.md).

## MS-DOS

`viewers/dos/` is the treemap for MS-DOS: a 16-bit real-mode program in assembly (FASM), about
**26 KB**, that runs in DOSBox-X or on a 386 with a 387. It is not built from the Rust code but
ported from it — the squarify layout, navigation, zoom, tile text, the side panel and size formats
— and its tiles match `libdiskonaut`'s on every folder it was compared on. It scans with long file
names where DOS has them, draws the treemap live during the scan, deletes and rescans, and
previews text, PNG and JPEG files beside the treemap, pictures in half blocks with six of text
mode's 16 colours set to the picture's own.

```sh
make dos-run    # needs dosbox-x; fetches FASM, assembles, opens this repository as C:
```

See [`viewers/dos/README.md`](viewers/dos/README.md).

## Windows GUI (experimental)

`diskonaut-windows` is a native Windows treemap window. The walker (`diskonaut-scan`) and the tree,
layout and deletion (`libdiskonaut`) are the same code the terminal app runs; the window is only
drawing and input. [`docs/features.md`](docs/features.md) lists what each viewer offers. It is built on
`windows-sys` and GDI rather than a GUI framework, so the release binary is about **250 KB**.

```sh
cargo run -p diskonaut-windows --release -- C:\   # or run with no argument for a folder picker
```

- **Fast:** the native parallel walker, scanning on a worker thread; the title bar shows entries/second.
- **Live progress:** the window opens immediately and reports entries as the scan runs.
- **Navigation:** left-click or Enter to open a folder, right-click / Backspace / the ◄ Up button to
  go back, arrow keys to move the selection.
- **Deletion:** Delete removes the selected file or folder from disk after a confirmation dialog,
  and refuses NTFS's own metadata files.
- **Details:** the bottom bar shows the hovered tile; a "small files" block stands in for entries too
  small to draw; the window is DPI-aware and double-buffered (no flicker).

Still experimental — no breadcrumb clicks, custom icon, or in-place rescan yet.

## Requirements

- Linux/MacOS
- A terminal with reasonable size (roughly 50×15 cells minimum for the main UI)
- [Rust](https://www.rust-lang.org/tools/install), unless you use a release binary

## Linux release binaries

Each [release](https://github.com/angch/diskonaut/releases) has fully static Linux binaries for
x86_64 and aarch64, `diskonaut-angch-<version>-<arch>-unknown-linux-musl.tar.gz`. They need no
particular glibc, or any glibc: they run on old distributions, Alpine and busybox alike. Unpack and
run; `diskonaut` and `diskonaut-angch` are the same program.

```bash
tar -xzf diskonaut-angch-*-x86_64-unknown-linux-musl.tar.gz && ./diskonaut-angch
```

To build them yourself: `make static` (needs `musl-tools`) or `make static-aarch64` (needs
`cargo-zigbuild`).

## Hard links and folder sizes

A folder's size is the space held under it: each distinct file counted once, however many names
point at it. If `a/a`, `a/b` and `b/a` are all links to the same 1 KiB file, then `a` is 1 KiB,
`b` is 1 KiB, and the root holding both is 1 KiB — deleting either folder alone frees nothing, and
deleting both frees 1 KiB.

This differs from `du`, which deduplicates in traversal order and so charges whichever directory it
reached first, reporting nothing for the others.

Two things follow that are worth knowing:

- **Sizes do not add up where hard links are involved.** A folder can be smaller than the sum of
  the tiles inside it — each file tile shows that file's own size, while the folder shows the space
  it holds.
- **Deleting one link frees nothing** until the last link is gone, so the "space freed" figure is
  optimistic in that case. A rescan restores the true picture.

`docs/scan-performance.md` covers the details and the reasoning.

## Filesystems and mount points

By default the scan crosses mount points, like `du`. Pass `-x` / `--one-file-system` to keep it on
the filesystem the scan started on.

One thing is skipped either way: a mount point that leads back to the filesystem the scan started
on, because those files are already being counted by another path. On macOS that is
`/System/Volumes/Data`, which is both a mount point and grafted into `/` through firmlinks — follow
both and almost every file on the machine is counted twice.

On Windows the scan never follows a junction, a symbolic link or a volume mounted in a folder, so
it stays on the volume it started on with or without `-x`. OneDrive placeholders and other
reparse points that stand for real files are scanned as usual.

## Hard links on Windows

A file with several names (a hard link) is counted once per folder, however many of its names that
folder holds. Linux and macOS report each file's link count for free; Windows directory listings do
not, and asking costs a file open per file. So on Windows diskonaut tracks files by their NTFS/ReFS
file id instead, which costs memory rather than time — about 100 bytes a file.

By default only the places hard links are normally made are tracked: the Windows directory, Edge,
Docker and Git installs, and package stores (`node_modules`, pnpm, uv, `.venv`, `site-packages`).
A link made by hand elsewhere is counted once per name, which overstates rather than understates.
Run as administrator, diskonaut tracks every file: it then reaches other users' profiles and
system folders the list was not drawn from, and on one `C:\` those held 3.1 GiB of links the list
missed. To track everywhere unelevated, pass `--hard-link-threshold BYTES` — every file at least that large is
tracked, wherever it is:

```sh
diskonaut --hard-link-threshold 1 C:\       # exact: every non-empty file
diskonaut --hard-link-threshold 1048576 D:\ # only files of 1 MiB and more
```

On one 2M-entry `C:\`, the default found all but 240 MB of 17 GB of double-counted links, for 90 MB
of memory; tracking every file was exact, for 200 MB and about a second more.

## Why the total can be less than the disk's used space

A scan adds up the files it can reach. The volume's used space, which Explorer and WizTree report,
also holds what no directory lists: the filesystem's own metadata (NTFS's master file table),
shadow copies and snapshots, and every folder the scan was refused. So when a scan covers a whole
volume, the title shows the volume's used space and the part of it the scan did not find:

```
Total: 315.8G (2058004 files), freed: 0, disk used: 424.4G, 108.6G outside the scan
```

It appears for a drive root on Windows, and on Unix for a mount point scanned with `-x` (without
it, the scan may cross into other filesystems and the two stop being comparable). It is not shown
with `--apparent-size`, since file lengths cannot be set against blocks in use.

On Windows, run diskonaut as administrator to see nearly everything. Elevated, it turns on the
backup privilege, as WizTree does, which lets it read `System Volume Information`, other users'
profiles and `WindowsApps` whatever their permissions say. It grants reading only: deleting still
needs ordinary permission. On one `C:\`, unelevated, 192 folders were unreadable and 108.6 GiB of
the 424.4 GiB in use was outside the scan; elevated, none were.

Elevated, a scan of a whole NTFS volume also shows the filesystem's own files at its root, under
their real names: `$MFT` (the master file table, 2.7 GiB on that `C:\`), `$LogFile`, `$Bitmap`,
`$Secure`, and `$Extend` with the change journal and the rest in it. No folder lists them, so they
are read from their master file table records. They are there to account for the space; the app
will not offer to delete them. What stays outside the scan after that is small: folders' own
indexes, and files whose directory entries lag behind their size while they are being written.

## Benchmarking the scan

`--benchmark` scans headlessly and prints timings instead of starting the UI, so scanning strategies
can be compared on a real tree:

```sh
diskonaut --benchmark /                    # every stage, whole disk
diskonaut --benchmark --bench-stage pipeline ~/src
diskonaut --benchmark --max-depth 4 /      # partial scan, for a quick iteration loop
diskonaut --benchmark --threads 6 --bench-repeat 3 /
```

| Stage      | What it measures                                                       |
| ---------- | ---------------------------------------------------------------------- |
| `dua-walk` | the general-purpose `dua-core` walk alone                              |
| `dua-tree` | that walk feeding the folder tree                                      |
| `walk`     | the walk diskonaut uses now, alone                                     |
| `tree`     | that walk feeding the folder tree                                      |
| `pipeline` | scan and tree build on separate threads, exactly as the app runs them  |

Other flags: `--max-depth N` stops the descent (a partial scan), `--threads N` sets the worker
count, `--bench-repeat N` repeats each stage, `--single-thread` forces one worker.

On macOS the scan uses `getattrlistbulk(2)` directly, requesting only the name, type, flags, inode,
and one size field per entry — the general-purpose walker asks for the whole `stat` set and pays an
extra path lookup per directory.

The scan does not enter mount points other than the scan root, the same restriction `du -x`
applies. On macOS this is what keeps a scan of `/` from counting almost every file twice, since the
data volume is both mounted at `/System/Volumes/Data` and grafted into `/` through firmlinks
(firmlinks are still followed — they are the only route to what they point at). One consequence
worth knowing: pointing diskonaut at a directory that contains nothing but mount points, such as
`/Volumes`, reports nothing. Scan the volume itself instead.

`docs/scan-performance.md` has the measurements, the reasoning, and notes for repeating the
exercise on another platform.

## Configuration

Optional TOML config (see [example/config.toml](example/config.toml)):

- Default path: `~/.config/diskonaut/config.toml`
- Override path: `diskonaut -c /path/to/config.toml`

## The list beside the treemap

On a terminal 80 columns or wider, the left third of the screen lists the current folder: its
path, size, file count and share of the whole scan, how many folders and files it holds, and at a
volume root the disk's used space. Below that is every entry, largest first, with a bar, its size
and its share of the folder. It has no border, so every line and column goes to names; long names
are cut in the middle, keeping the start and the extension.

The keyboard works one panel at a time — the list, to begin with, on its largest entry — and
after that on whichever you last clicked. `Tab` switches;
`←` off the treemap's left edge moves into the list, and `→` from the list moves back. In the
list, `↑`/`↓` (`k`/`j`) walk every entry in size order, tiles or not, `PgUp`/`PgDn`/`Home`/`End`
jump, and `Enter`, `Esc` and `d` act on the highlighted row. In the treemap the arrows move
between tiles as before. The panel with the keyboard has the solid highlight; the other still
marks the same entry, more quietly.

To pick several entries, `Ctrl`+click them — in the list or on the treemap; a second
`Ctrl`+click takes one out — or hold `Shift` and press `↑`/`↓` in the list to mark a run of
rows. Every change copies the marked paths to the clipboard, quoted and separated by spaces in
the order you picked them, ready to paste after a command: `cp 'my file' notes.txt ~/backup/`.
The title shows what was copied. Marked entries are black on yellow in both panels. A plain
click or arrow key, or changing folder, clears the marks.

`d` deletes the marked entries, or the entry in hand when nothing is marked. The prompt says how
many and how much — `Delete these 3 items (4.2G)?` — and names as many as fit. If one fails, the
rest are still deleted and the message says which did not and why; only what was really removed
is counted as freed. A marked NTFS metadata file refuses the whole deletion rather than being
skipped quietly.
Some terminals keep `Ctrl`+click for a context menu of their own; `Shift`+arrows work in all of
them.

The row for the selected tile is highlighted and kept in view. Entries without a tile of their
own — too small, and folded into the `x` corner, or left off by the zoom — are listed dimmed, and
are often most of a folder: in a build cache of 30,000 small files the list is the only way to
see them. Rows take the same clicks as tiles: select, double-click to open, right-click to copy.

Below the list is a preview of the file in hand, 16:9 in shape (worked out from the terminal's
cell size in pixels, where it reports one) and never more than half the panel. A text file shows
its first lines, with escape sequences and other control characters shown as `?` rather than
passed to the terminal. A PNG or JPEG — recognised by its first bytes, not its name — is drawn
as a picture in terminals that speak the kitty graphics protocol (kitty, Ghostty, WezTerm; known
from the environment, or by asking the terminal when it says nothing, as over ssh), once
the selection has rested on it for 100 ms, so moving quickly through a folder of photos decodes
none of them. A terminal without kitty graphics that has sixels (foot, xterm, mlterm, Windows
Terminal, iTerm2, Konsole, tmux built with them — found from the terminal's device attributes)
gets the picture as sixels, in up to 256 colours. Elsewhere it is drawn in the text itself as
half blocks (`▀`), two pixels to a cell: in 24-bit colour where `COLORTERM` says the terminal has
it, and in the 256-colour palette otherwise (ssh does not pass `COLORTERM` on).
`DISKONAUT_GRAPHICS=kitty`, `sixel`, `blocks` or `none` overrides the guess; `none` describes a picture instead of drawing it:
`PNG image · 1920×1080`. Only regular files are read, and on
macOS files that are only in iCloud are not, since reading one would download it.

Narrower terminals give the whole width to the treemap, as before.

## Keyboard and mouse

| Key                                | Action                                |
| ---------------------------------- | ------------------------------------- |
| `←` `→` `↑` `↓` or `h` `j` `k` `l` | Move selection                        |
| `Enter`                            | Open folder                           |
| `Esc`                              | Go to parent folder                   |
| `d`                                | Delete selected file or folder        |
| `+` / `-`                          | Zoom in / out                         |
| `0`                                | Reset zoom                            |
| `a`                                | Disk usage / apparent size            |
| `r`                                | Rescan the selected folder            |
| `R`                                | Rescan everything                     |
| `q` or `Ctrl+C`                    | Quit (confirm with `y` when prompted) |
| `Tab`                              | Move the keyboard to the list / map   |
| `PgUp` `PgDn` `Home` `End`         | Jump through the list                 |
| `Shift`+`↑` `↓`                    | Mark a run of rows, copy their paths  |
| `Ctrl`+click                       | Mark or unmark, copy the marked paths |
| Click                              | Select the tile under the pointer     |
| Double-click                       | Open that folder                      |
| Right-click                        | Copy its path, relative to your shell |
| Double right-click                 | Copy its absolute path                |

Deletion always asks for `y` / `n` confirmation.

`a` switches between the space files take on disk and their apparent size (their length, as
`du --apparent-size` counts it) at once, without scanning again: every file keeps both. `-a` or
`apparent-size = true` in the config only chooses which is shown first.

`r` scans the selected folder again (the folder shown, when a file is selected) and `R` the whole
tree, in the background: the old figures stay up, and can be browsed, until the new ones replace
them. A folder that has gone from disk is taken out of the view. Hard links between the rescanned
folder and the rest are counted on both sides until the next `R`.

On XFS and btrfs, small files are checked for blocks they share with other files (reflink copies,
btrfs snapshots) after the treemap is up, the folder you are looking at first; the title says
"refining" meanwhile, and sizes can go down a little as it finds them.

The help line at the bottom moves on by itself: the key legend, then a tip, then the legend again
and the next tip. Each rests at least five seconds, counted from your last key press, and then
slides to the next in under 100 ms. A legend wider than the terminal is shown a page at a time.

A double click is two clicks on the same tile within half a second.

Copied relative paths start from the directory you ran diskonaut in, so they paste straight into
the same shell: in `/home/user/foo`, `diskonaut ../bar/` with `baz` selected copies `../bar/baz`.
Both ends are resolved first, so a symlinked directory cannot send `..` somewhere else; where no
relative path exists (the directory was deleted, or on Windows the scan is on another drive) the
absolute path is copied, and the title says so. Paths are quoted so they paste into a
shell as exactly that path — `'my dir/it'\''s here'` — in PowerShell's quoting on Windows. Names
with a newline, an escape sequence, bytes that are not UTF-8, or an invisible right-to-left
override come out as `$'…'` escapes rather than raw, so a pasted path cannot run anything and
what the title shows is what was copied. A relative path starting with `-` gets a leading `./`,
so a command cannot take it for an option. The title shows what was copied for two seconds.

The copy goes to the system clipboard: `pbcopy` on macOS, the Windows clipboard, and `wl-copy`,
`xclip` or `xsel` on a Linux desktop. Where none is available, as over SSH, it is sent to the
terminal instead (OSC 52), which iTerm2, kitty, WezTerm, Windows Terminal, foot and Alacritty put
on the clipboard of the machine you are sitting at; in tmux that needs `set -g set-clipboard on`. While diskonaut runs it
captures the mouse, so the terminal's own click-and-drag text selection needs a modifier: `Shift`
in most terminals, `Option` (`⌥`) in iTerm2 and Terminal.app. Quitting releases the mouse.
