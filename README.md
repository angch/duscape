# duscape

**duscape** shows where your disk space went: pick a folder, watch a treemap of it fill in live as
it is scanned, drill into folders, and delete what you no longer need. It is one program that is
both a **terminal viewer** and a **native window** — Win32 on Windows, AppKit on macOS, Wayland or
X11 on Linux — on the same scanner, the same sizes and the same rules, and there is a port to
MS-DOS as well.

## About this fork

duscape is a fork of [diskonaut](https://github.com/imsnif/diskonaut). It was `diskonaut-angch`
until 0.2.0, and is renamed so that its command does not clash with upstream's `diskonaut`.

It began as a way to **explore further performance for everyday disk-usage scanning across Linux,
macOS and Windows**. Upstream diskonaut is a Unix terminal tool; here each of those platforms gets
a native, parallel directory walker (Windows support is new), and the scan and tree-build
pipeline is reworked for speed — warm, and cold too, where the first scan after boot is bound by
the disk rather than the CPU. `docs/scan-performance.md` records the measurements and the
reasoning. The native windows came after, sharing everything but their drawing.

**Caveat:** these changes are largely **not yet battle-tested**. Treat the fork as experimental — sanity-check reported sizes against a tool you trust, and keep backups before deleting anything. The performance direction is the point; hardened, production-grade reliability is not there yet.

## Features

- **Live scanning** — the treemap updates while the walk is still running
- **A list beside the treemap** — the folder's entries, largest first, each with its size and
  share; in the windows it is a tree, folders opening in place (as WizTree's), and the treemap
  is nested, each folder's tile holding its entries' tiles down to the files
- **Previews** — the file in hand's text or picture under the list (in a terminal: kitty
  graphics, sixels or half blocks); a binary file described, and in the windows shown as a hex
  dump
- **In-session cleanup** — delete one entry or everything marked, after a confirmation, and see
  the space freed; the macOS and Linux windows can move to the Trash instead
- **Marks and paths** — mark several entries and copy their paths, quoted for the shell; the
  windows have a right-click menu for opening, showing in the file manager, copying, rescanning
  and deleting
- **Rescans** — a folder or everything, in the background, the old figures kept up meanwhile
- **Apparent or on-disk size** — default shows blocks allocated on disk; `-a` (or `a` while
  running) uses logical file size
- **Hard-link aware** — a file reached by several names counts once in each folder that holds it
- **Native walkers** — Linux, macOS and Windows each get their own parallel directory walk; other
  platforms, the BSDs included, use `dua-core`'s portable one
- **Stays put on request** — `-x` keeps the scan on one filesystem, like `du -x`
- **Reads the disk itself** — as root on ext4, the metadata comes straight off the block device
  in ordered sweeps rather than one `stat` per file (a cold scan in half the time); elevated on
  Windows, a whole NTFS volume can be read from its master file table, as WizTree does, where a
  sample of the table says that is faster than walking

## One program: the terminal and the window

`duscape` is the terminal viewer when started in a terminal, and the platform's window when
started from a desktop — a launcher on Linux, Explorer on Windows, Finder through `Duscape.app`
on macOS (`make mac-app`). `--gui` and `--tui` choose, and so does the name: a link called
`duscape-gui` is the window. The window takes the same command line: the folder, `-a`, `-x`,
`--max-depth` and the other scan flags.

```bash
duscape ~          # in a terminal: the terminal viewer
duscape --gui ~    # the window
```

On Windows the one executable is a console program that asks, in its manifest, for no console
unless it inherits one: from Windows 11 24H2 a double-click opens the window alone; older Windows
shows a console for a moment first. Each window also builds alone (`cargo run -p duscape-mac
--release`, `-p duscape-linux`, `-p duscape-windows`), and `--no-default-features` builds the
terminal viewer without one. [`docs/viewers.md`](docs/viewers.md) describes each viewer,
including [the MS-DOS port](viewers/dos/README.md) (16-bit assembly for a 286, `make dos-run`),
and [`docs/features.md`](docs/features.md) lists what every viewer offers. All the windows, and the
DOS port, are experimental.

## Getting it

### Release binaries

Each [release](https://github.com/angch/duscape/releases) has one binary per platform that is
both the terminal viewer and the window:

- **Linux**, x86_64 and aarch64: `duscape-<version>-<arch>-unknown-linux-musl.tar.gz`, fully
  static. They need no particular glibc, or any glibc, and no system library for the window: they
  run on old distributions, Alpine and busybox alike, on Wayland or X11.
- **Windows**, x86_64: `duscape-<version>-x86_64-pc-windows-gnu.zip`, needing only DLLs that come
  with Windows 10 and later.

```bash
tar -xzf duscape-*-x86_64-unknown-linux-musl.tar.gz
./duscape ~
```

Releases up to 0.2.0 came out under the old name: `diskonaut-angch-<version>-…` tarballs for
Linux, the window as a separate `diskonaut-linux` beside the terminal viewer, and nothing for
Windows.

### From source

Needs [Rust](https://www.rust-lang.org/tools/install). Linux, macOS or Windows:

```bash
cargo run --release --bin duscape -- ~    # the terminal viewer, or the window with --gui
cargo install --path viewers/tui          # duscape on your PATH
```

The static release builds: `make static` (needs `musl-tools`), `make static-aarch64` and `make
static-windows` (need `cargo-zigbuild`); on a Mac, `make mac-app` builds `duscape` for both
architectures in one file and `Duscape.app` around it (macOS links its system libraries
dynamically, always; nothing else). On Windows, `.\make <target>` runs the Makefile's targets
without make installed (it reads the Makefile and uses Git's bash).

The terminal viewer wants a terminal of roughly 50×15 cells at least.

## Sizes, hard links and mount points

A folder's size is the space held under it: each distinct file counted once, however many names
point at it, so sizes do not add up where hard links are involved, and deleting one link frees
nothing until the last is gone. By default the scan crosses mount points, like `du`; `-x` keeps it
on one filesystem. On a whole volume duscape shows the disk's used space and how much of it the
scan did not reach (the terminal viewer in its title, the Windows window under the path, the
macOS and Linux windows in their status bar).
[`docs/sizes.md`](docs/sizes.md) explains all of this, including what Windows does about hard
links and why running as administrator there shows more (and, for a whole volume, reads the
master file table instead of walking, as WizTree does). The Windows window asks to run as
administrator when the folder is a whole volume; `--no-elevate` scans as it is.

## Benchmarking the scan

`--benchmark` scans headlessly and prints timings for each stage of the scan instead of starting
the UI; `docs/probes/bench-diskus.sh` compares it with `diskus`, warm and cold. See
[`docs/benchmarking.md`](docs/benchmarking.md) for the stages and flags, and
[`docs/scan-performance.md`](docs/scan-performance.md) for what was measured and why, and
[`docs/scan-roadmap.md`](docs/scan-roadmap.md) for what is next.

## When files fail to read

The terminal viewer's title counts what it could not read ("failed to read 12 files"). To see why,
run a scan with nothing drawn:

```bash
duscape --issues /volume1
```

It prints where it runs — the walker, and on Linux the kernel, whether it has `statx`, the
filesystem and who is asking — then every kind of failure with the system's error and a count,
and examples of where. That output is what to send with a problem report.

Old kernels are fine: before Linux 4.11 there is no `statx`, the call duscape sizes entries with
(Synology's DSM runs 4.4, for one), and every entry used to fail; it now asks `fstatat` there
instead, as it does where a container's seccomp filter refuses `statx`. What a kernel before 5.8
cannot say — which directories are mount points, so that a folder bind-mounted inside the scan is
not counted twice — comes from `/proc/self/mountinfo` instead. `DUSCAPE_NO_STATX=1` makes any
kernel read that way, should its `statx` ever be the trouble.

## Configuration

The terminal viewer reads an optional TOML config — its key bindings, and apparent sizes by
default (see [example/config.toml](example/config.toml)); the windows take only the command line.

- Default path: `~/.config/duscape/config.toml` (one at `~/.config/diskonaut/config.toml`, from
  before the rename, is still read while there is none at the new place)
- Override path: `duscape -c /path/to/config.toml`

## In the terminal

On a terminal 80 columns or wider, the left third of the screen lists the current folder and every
entry in it, largest first, with a bar, size and share; below it, a preview of the file in hand
(text, or a PNG or JPEG drawn with kitty graphics, sixels or half blocks). `Tab` moves the
keyboard between the list and the treemap. `Ctrl`+click or `Shift`+arrows mark several entries
and copy their paths, quoted, to the clipboard; `d` deletes what is marked. Narrower terminals
give the whole width to the treemap. [`docs/terminal.md`](docs/terminal.md) has the details:
marks, the clipboard, previews and rescans.

### Terminal keys and mouse

The defaults; the config file can rebind the keys.

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

Deletion always asks for `y` / `n` confirmation. `a` switches between disk usage and apparent
size without rescanning; `r`/`R` rescan in the background while the old figures stay up.
Right-click copies a path relative to the shell you started in, quoted for pasting; where no
clipboard is available, as over SSH, it goes to the terminal (OSC 52). More in
[`docs/terminal.md`](docs/terminal.md).

## In the windows

The three windows behave alike (their state is shared, `viewers/shared/`): the list is a tree,
the treemap nested, and a right-click opens a menu of what can be done with what is under the
pointer — open it, show it in the file manager, copy its path, rescan, move it to the Trash or
delete it. The keys differ where the platform has its own:

| Action | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Open a folder in place / close it | `→` / `←` or the expander | `→` / `←` or the expander | `→` / `←` or the expander |
| Go into a folder / up | `Enter`, double-click / `Esc`, `Backspace`, back button | `Enter`, double-click / `Esc`, `Backspace`, back button | `Return`, `⌘↓`, double-click / `Esc`, `⌫`, `⌘↑`, back button |
| Mark | `Ctrl`+click, `Shift`+click or arrows, `Ctrl+A` | `Ctrl`+click, `Shift`+click or arrows, `Ctrl+A` | `⌘`-click, `⇧`-click or arrows, `⌘A` |
| Copy the path / full path | `Ctrl+C` / `Ctrl+Shift+C` | `Ctrl+C` / `Ctrl+Shift+C` | `⌘C`; `⌥⌘C` plain, as Finder does |
| Delete | `Del` or `d` | to the Trash `d` / at once `D` | to the Trash `⌘⌫` / at once `⌥⌘⌫` |
| Zoom | `+` `-` `0`, the wheel over the treemap | `+` `-` `0`, the wheel over the treemap | `⌘+` `⌘-` `⌘0`, a pinch or a mouse's wheel |
| Rescan the folder / everything | `r` or `F5` / `R` or `Shift+F5` | `r` / `R` or `F5` | `⌘R` / `⇧⌘R`, or `r` / `R` |
| Disk usage / apparent size | `a` | `a` | `a` |
| Hide the list | `s` | `s` | `⌃⌘S` |

A delete always asks first. [`docs/features.md`](docs/features.md) has every feature, viewer by
viewer.
