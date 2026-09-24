# diskonaut

**diskonaut** is an interactive terminal tool for exploring disk usage. Pick a directory, watch a live treemap fill in as files are scanned, drill into folders, and delete what you no longer need—without leaving the terminal.

## About this fork

This fork exists to **explore further performance optimizations for everyday disk-usage scanning across Linux, macOS, and Windows**. Upstream diskonaut is Unix-native; here each platform gets a native, parallel directory walker (including Windows, which is new), and the scan and tree-build pipeline is reworked for speed — warm, and since September 2026 cold too, where the first scan after boot is bound by the disk rather than the CPU. `docs/scan-performance.md` records the measurements and the reasoning.

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
- **Native walkers** — Linux, macOS, Windows and BSD each get their own parallel directory walk
- **Stays put on request** — `-x` keeps the scan on one filesystem, like `du -x`

## Other viewers

The same walker and model also drive native windows for [macOS](viewers/macos/README.md)
(`cargo run -p diskonaut-mac --release`), [Linux](viewers/linux/README.md) — Wayland or X11,
no toolkit, builds static (`cargo run -p diskonaut-linux --release`) — and Windows
(`cargo run -p diskonaut-windows --release`), and an [MS-DOS port](viewers/dos/README.md) in
16-bit assembly (`make dos-run`). All are experimental. [`docs/viewers.md`](docs/viewers.md)
describes each, and [`docs/features.md`](docs/features.md) lists what every viewer offers.

## Requirements

- Linux, macOS or Windows
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

## Sizes, hard links and mount points

A folder's size is the space held under it: each distinct file counted once, however many names
point at it, so sizes do not add up where hard links are involved, and deleting one link frees
nothing until the last is gone. By default the scan crosses mount points, like `du`; `-x` keeps it
on one filesystem. On a whole volume the title shows the disk's used space and how much of it the
scan did not reach. [`docs/sizes.md`](docs/sizes.md) explains all of this, including what Windows
does about hard links and why running as administrator there shows more.

## Benchmarking the scan

`--benchmark` scans headlessly and prints timings for each stage of the scan instead of starting
the UI; `docs/probes/bench-diskus.sh` compares it with `diskus`, warm and cold. See
[`docs/benchmarking.md`](docs/benchmarking.md) for the stages and flags, and
[`docs/scan-performance.md`](docs/scan-performance.md) for what was measured and why, and
[`docs/scan-roadmap.md`](docs/scan-roadmap.md) for what is next.

## Configuration

Optional TOML config (see [example/config.toml](example/config.toml)):

- Default path: `~/.config/diskonaut/config.toml`
- Override path: `diskonaut -c /path/to/config.toml`

## The list beside the treemap

On a terminal 80 columns or wider, the left third of the screen lists the current folder and every
entry in it, largest first, with a bar, size and share; below it, a preview of the file in hand
(text, or a PNG or JPEG drawn with kitty graphics, sixels or half blocks). `Tab` moves the
keyboard between the list and the treemap. `Ctrl`+click or `Shift`+arrows mark several entries
and copy their paths, quoted, to the clipboard; `d` deletes what is marked. Narrower terminals
give the whole width to the treemap. [`docs/terminal.md`](docs/terminal.md) has the details:
marks, the clipboard, previews and rescans.

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

Deletion always asks for `y` / `n` confirmation. `a` switches between disk usage and apparent
size without rescanning; `r`/`R` rescan in the background while the old figures stay up.
Right-click copies a path relative to the shell you started in, quoted for pasting; where no
clipboard is available, as over SSH, it goes to the terminal (OSC 52). More in
[`docs/terminal.md`](docs/terminal.md).
