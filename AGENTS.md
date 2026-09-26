# AGENTS.md — Duscape Agentic Development Guide

## Project Overview

**Duscape** is an interactive terminal disk space navigator (TUI) written in Rust. It visualizes
disk usage via a squarify treemap, supports live scanning, and allows deleting large files in-place.

**Workspace layout** (Rust 2024 edition, version 0.2.0; duscape, a fork of diskonaut renamed after
0.2.0 so as not to clash with upstream's command — see README):
```
duscape/
├── common/            # libduscape: what every viewer shares — model, treemap, scan protocol,
│                      #   delete, preview reading, native clipboard, formatting, os
├── scanners/          # duscape-scan: the walkers (Linux, macOS, Windows, fallback), NTFS,
│                      #   the parallel build, the second pass, rescan/refine threads
├── viewers/
│   ├── tui/           # duscape: the ratatui viewer (primary) — CLI, UI, input, config;
│   │                  #   its `duscape` binary holds the platform's window too (`front.rs`)
│   ├── windows/       # duscape-windows: the Win32/GDI viewer
│   ├── macos/         # duscape-mac: the AppKit viewer (objc2)
│   ├── linux/         # duscape-linux: the Wayland/X11 viewer, no toolkit (wayland-client, x11rb, fontdue)
│   ├── shared/        # duscape-viewer: what the desktop viewers share — the window's
│   │                  #   state and layout (`Viewer`), the first scan with its outline, the previewer
│   └── dos/           # not a crate: the MS-DOS treemap in 16-bit FASM assembly, `make dos`
├── docs/              # features.md (every feature, per viewer), sizes.md (how sizes are counted),
│                      #   terminal.md, viewers.md, benchmarking.md, scan-performance.md (the measurements),
│                      #   probes/ (bench-matrix.sh and .ps1, drop-cache.ps1, bench-diskus.sh, the C/Python probes behind the measurements)
├── example/config.toml
└── Cargo.toml         # Workspace root
```
Dependencies run one way: `duscape-scan` → `libduscape`, each viewer → both, and the desktop
viewers (Windows, macOS, Linux) → `duscape-viewer` too. The terminal viewer → the platform's
window crate, by its `gui` feature (default): the window crates are libraries (`run`, and
`run_with` for a command line read elsewhere) with a thin binary of their own each, and
`duscape` is one program per platform. A feature that is not drawing or input goes in `common`
(or `scanners`, if it reads the disk), so the other viewers get it by calling it; what is about the
*window* but not about a toolkit (which entry is in hand, marks, the layout in points, what a
delete changes) goes in `viewers/shared`, so the three desktop viewers behave alike. The scan protocol types (`ScanOptions`, `EntryMeta`, `DirEntries`,
`Outline`, `Found`) are in `common` because the model consumes them; `duscape-scan` re-exports
them, so `duscape_scan::X` works for either kind.

---

## Essential Commands

```bash
cargo build --workspace          # Build everything
cargo test --workspace           # Run all tests
cargo clippy --workspace --all-targets -- -D warnings   # Lint
cargo fmt --all                  # Format code
cargo fmt --all -- --check       # Format check (CI)
```

On Windows without `make`: `.\make <target>` runs the same Makefile — `make.cmd` starts
`make.ps1`, which reads it and runs each recipe line in Git for Windows's bash (`MAKE_SHELL`
names another). It interprets the subset of make this file uses and stops on anything else, so
a recipe is written once. `.\make -n <target>` prints what it would run.

**Static release binaries** (what `deploy.yml` ships; see "Releases" below):
```bash
make static           # x86_64-unknown-linux-musl, needs musl-gcc (apt install musl-tools)
make static-aarch64   # aarch64-unknown-linux-musl, needs zig + cargo-zigbuild
```

**Run the binary:**
```bash
cargo run --bin duscape -- [FOLDER]
cargo run --bin duscape -- -a  # apparent size mode
```

---

## Architecture

### Thread Model

Six kinds of thread communicate via `mpsc` channels (bounded, except the previewer's inbox), plus
`parallel::SHARDS` tree builders that share nothing:

| Thread | Role |
|--------|------|
| `stdin_handler` | Reads crossterm events → `Instruction::Keypress` |
| `hd_scanner` | Drives the walk (its own worker pool). Sends each directory to one `tree_builder` by path prefix, and feeds an `Outline` that sends **main** a folder-only view → `Instruction::AddScannedSummaries` (batched, ~4096 entries). Directories deeper than `Outline::DEFAULT_DEPTH` are rolled up into the frontier folder above them rather than sent, so main does O(visible) work, not O(directories). When the walk ends: merges the builders' trees, replays deferred shared blocks → `Instruction::ScanComplete(tree)`, then `StartUi` |
| `tree_builder_N` | Owns a private `FileTree` in deferred-sharing mode and adds whatever `hd_scanner` sends it. Never touches another thread's memory |
| `event_executer` | Converts `Event` → `Instruction` (visual feedback). A clipboard flash gets a short-lived `clipboard_flash` thread that asks for a redraw when it expires; the flash carries its own deadline, so a lost redraw cannot leave it on screen |
| `loading_loop` | Toggles loading indicator while scanning |
| `ticker` | Sends `Instruction::Tick` with `try_send` (late ticks are dropped): every `ui::FRAME` (16 ms) while `App::ticker_pace` says the help line is sliding, else every `ui::IDLE_TICK` (250 ms), looking again one frame after each resting tick since a slide begins on one — twice per resting interval, not at frame rate. `App::tick` moves the help line (`ui::Ticker`/`Strip`) |
| `refine_N` | The second pass (`duscape_scan::rescan::Refiner` → `refine::refine`, a few `refine_*` workers): FIEMAP on the small files the walk noted (`SmallFiles`, 4–64 KiB, `nlink == 1`, XFS/btrfs), directories under `App::refine_focus` (the folder shown) first → `Instruction::Refined(generation, Found, left)`; `FileTree::apply_found` charges them to the ledger. A new whole tree cancels it; folders rescanned meanwhile are skipped (`refine_skip`), since a folder rescan refines its own tree before grafting |
| `rescan_N` | One per `r`/`R` (`duscape_scan::rescan::Rescanner`). A folder the whole scan would not enter (`walk_would_enter`: pseudo, network, `-x`, bind duplicate) or past `--max-depth` (counted from the scan root) is not rescanned (`Outcome::NotWalked`); a delete inside a folder being rescanned restarts that rescan. `parallel::build_tree` on the folder → `Instruction::Rescanned(id, Outcome)`. `App::rescan_done` grafts it (`FileTree::graft`, ancestors corrected by the difference) and leaks the old folder like `finish_scan` does. A rescan that another under way covers is not started; one the new rescan covers is cancelled and its result dropped |
| `previewer` | Reads the file in hand for the preview: first 64 KB as text, or a PNG/JPEG decoded and scaled after a 100 ms debounce (a newer request supersedes it) → `Instruction::PreviewReady(generation, _)`; answers to an older generation are dropped |
| **main** | App state mutations + ratatui rendering. During the scan it renders from the *outline*; on `ScanComplete` it swaps in the finished tree, keeping the current folder |

**Synchronization**: `Arc<AtomicBool>` for `running`/`loaded` flags; bounded sync channels (capacity 1–100).

### Crate Responsibilities

**`libduscape`** (`common/`) — what every viewer shares, no user interface:
- `model/files/file_tree.rs` — `FileTree`: hierarchical navigation, deletion tracking;
  `deferring_shared_blocks` / `merge_from` / `replay_deferred` for the parallel build;
  `add_summary` for the outline; `graft`, `apply_found`
- `model/file_to_delete.rs` — `FileToDelete::in_current_folder`, what both viewers confirm
- `scan/mod.rs` — the scan protocol: `ScanOptions`, `EntryMeta`, `DirEntries` (its `later` and
  `extent_space` are filled by the Linux walker for the second pass; a failure is counted with
  `DirEntries::fail(action, name, error)`, never `failed += 1`, so that `--issues` can say why:
  `Issues` counts every one by kind and by the name of the folder holding it (`folders`: a
  Synology NAS's million `@eaDir` failures in one line; 64 kinds and 64 names at most, the rest as `Issues::OTHER`, so an error
  that names its path cannot make one kind a failure) and keeps a few examples — 8 a directory,
  200 a scan — and
  `FileTree::add_dir_entries`/`merge_from` gather them into `FileTree::issues`), `Outline`/`DirSummary` (the
  depth-capped live view), `Found`/`FoundFile`
- `model/files/hard_links.rs` — charges shared blocks to each folder once, over interned directory
  ids; two ledgers, one keyed on inode (hard links) and one on physical extent (reflinks)
- `model/files/hash.rs` — the fast hasher behind the folder and inode maps
- `tiles/treemap.rs` — squarify algorithm (`HEIGHT_WIDTH_RATIO = 2.5`)
- `tiles/board.rs` — `Board`: tile selection, zoom stack, navigation
- `tiles/nested.rs` — `nest`: the treemap nested — the same squarify run inside each folder
  tile (under its label rows, within a margin) on the folder's entries, and theirs in turn,
  down to the files wherever there is room: what ends it is an inside too small for two
  minimum tiles either way, and the tiles in all are bounded by the folder tiles' area at the
  minimum size — the screen bounds a relayout's and a paint's work (`Nesting`'s depth and
  tile caps are guards beyond that). The minimum tile is `treemap::MINIMUM_WIDTH`/`HEIGHT`.
  Each folder is listed by `largest_in_folder`, only as many entries as its inside has room
  for, so a folder of fifty thousand entries is not sorted whole on every relayout; parents
  before children, so a painter draws in order
- `delete.rs` — `remove` (from disk, a link itself never its target) and `refused` (NTFS metadata)
- `metafiles.rs` — NTFS metadata names, for the Windows walker and for `delete`
- `preview.rs` — `read` (sniff the first 64 KiB: text lines, info, or a picture), `describe_picture`,
  `decode_picture` (bounded). Scaling and encoding for display are the viewer's. A binary file
  is `Contents::Binary`: `describe_binary` (its size, then `placement::describe`) and `hex_dump`
  (`HEX_LINES` of sixteen bytes, a dash after the eighth, the characters beside); the terminal
  shows the description, the desktop viewers the dump (`Preview::Hex`, drawn with the monospace
  font shrunk until a line fits — `Fonts::fitting` on Windows, `draw::hex` on macOS and Linux,
  which put the description above it)
- `placement.rs` — where a file's blocks are, for the preview of a file with nothing else to
  show (Linux): FIEMAP for its extents, sparseness and shared blocks, then sysfs for the disk —
  through a partition to the disk's model and SSD/HDD and the offset into it; the members of an
  LVM or md volume (the table that says which is root's); a loop device's file. The companion
  tool `whereisthis` follows every layer; this is the few lines that fit under a treemap
- `clipboard.rs` — native clipboard (`pbcopy`, Win32, `wl-copy`/`xclip`/`xsel`), `base64`
- `launch.rs` — `open` (the default app) and `reveal` (the file manager, selected): `open`/
  `open -R` on macOS; `ShellExecuteW` and `explorer.exe /select,"…"` (a raw argument: Explorer
  takes a comma in a plain one for a switch) on Windows; `xdg-open` and
  `org.freedesktop.FileManager1` over `dbus-send` elsewhere, the bus call on a thread of its own
  since a file manager being started to answer can take seconds. Nothing waits on the window's
  thread, and every child is reaped by a thread that waits for it
- `format/display_size.rs` — byte → human-readable (B/KB/MB/GB/TB)
- `os/unix.rs`, `os/windows.rs` — `is_user_admin()`, `size_on_disk_fast()`, `volume_id()`, `link_count()`

**`duscape-scan`** (`scanners/`) — reading the disk:
- `lib.rs` — `scan_directories()`: per-directory batches, the seam every walker plugs into;
  `parallel::build_tree()`: the app's tree build — shard by path prefix, merge, replay;
  `walk_would_enter`, `thread_count`, `scan_into_tree`, the `dua-core` `fallback`
- `macos.rs` — macOS walker on `getattrlistbulk(2)` (see `docs/scan-performance.md`)
- `ext4.rs` — as root on ext4, the walk read from the block device: directory blocks and inodes
  swept in device order a generation at a time, every run advised before any is read, parsed and
  batched on several threads; one `DirEntries` per directory like any walker. Declines up front
  (and the kernel walk runs) for a filesystem it cannot follow (META_BG, a device that will not
  open); a mount point inside, or a directory of a shape it does not read, goes to the kernel
  walker whole; rescans and `--no-device-read` never use it. `--bench-stage ext4-raw` is the inode
  survey alone. Last seconds of writes may be missing: it reads the device's page cache
- `linux.rs` — Linux walker on `getdents64`/`statx`, own thread pool; also the `FS_IOC_FIEMAP`
  reflink probe. `dua-core` is only the fallback for other platforms and the benchmark baseline.
  Every `statx` goes through its own `statx`, which on `ENOSYS` (a kernel before 4.11 — Synology
  DSM's 4.4) or a seccomp `EPERM` answers with `fstatat` in `statx`'s shape (`statx_from_stat`)
  and stays on it (`NO_STATX`): basic fields only, no attributes or mount id. Those the kernel
  gives only from 5.8 (`STATX_ATTR_MOUNT_ROOT`, `stx_mnt_id`), and before it `mount_of` takes
  both from the mount table (`Shared::mounted_at`, `mounts::points`: the last mount at a point
  shows), so bind-mount duplicates are still found; `mount_at` likewise. `DUSCAPE_NO_STATX=1`
  makes a scan read as a pre-4.11 kernel would — the fixtures run their totals and mount layouts
  that way too — and `strace -e inject=statx:error=ENOSYS` reproduces such a kernel from outside.
  A read-only btrfs snapshot inside the scan is left empty unless `ScanOptions::snapshots`
  (`--snapshots`): every subvolume has a device of its own, so one is always a crossing, onto
  btrfs (`filesystem::Kind::btrfs`), at a root numbered 256 — only there does
  `btrfs_subvolume::is_read_only` open it and ask `BTRFS_IOC_SUBVOL_GETFLAGS` (no privilege
  needed), never off btrfs, where opening an automount point would mount it; `walk_would_enter`
  says the same for a rescan and for the ext4 device reader's mount points, the scan's own root
  excepted. Synology keeps a share's snapshots under
  its `#snapshot`, one an hour, and walking them walked the share once each.
  `environment` is what `--issues` says about the machine
- `ntfs.rs` — NTFS file-record parser: sizes `$MFT` and the other metadata files the Windows
  walker adds at a volume root when elevated (records fetched with `FSCTL_GET_NTFS_FILE_RECORD`).
  Platform-independent so its tests run on Linux CI
- `mft.rs` — NTFS read from its master file table, elevated (`docs/scan-roadmap.md` step 2 for
  Windows, what WizTree does): the whole `$MFT` read in 32 MiB chunks along its runs and parsed on
  a few threads into `(parent, name, sizes)` per record — every `$FILE_NAME` (one per hard link,
  the DOS name dropped beside a Win32 one), the unnamed `$DATA`'s length and allocation (resident:
  0 on disk; compressed or sparse: the compressed size), directory and junction flags, extension
  records merged into their base — then handed on breadth-first from the scan root's record as
  one `DirEntries` per directory, exactly as the kernel walk would. Hard links arrive counted, so
  only files with several names go through the ledger; NTFS's own files are sized by their
  clusters as `ntfs.rs` sizes them. `scan_directories` picks it when `read_device` is on, the
  scan root is a volume root (a subtree is a fraction of the volume and the table costs all of
  it), the volume opens (`\.\C:`, elevated) and flushes, and a sample of the table says its
  directories are small enough for the table to pay (`TABLE_UP_TO` entries a directory: the walk
  costs a handle per directory, the table a record per file); else the kernel walk.
  `--no-device-read` opts out. The parser and the tree run and are tested on every platform;
  only `mft::volume` is Windows
- `windows.rs` — Windows walker: one handle per directory, entries read in bulk with
  `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`. No listing carries a link count, so
  files in hard-link hot spots (or all files ≥ `--hard-link-threshold`) are sent with
  `LINKS_UNKNOWN` and the ledger dedupes them by file id — memory instead of a file open each
- `refine.rs` — the second pass (`SmallFiles`, `refine`)
- `rescan.rs` — `Rescanner` and `Refiner`: rescans and the second pass on threads of their own,
  results through a callback, for any viewer

**`duscape-windows`** (`viewers/windows/`) — the Win32/GDI viewer, over `duscape-viewer`, at
feature parity with the TUI bar configurable keys (`docs/features.md`). Behaviour goes in the
shared `Viewer`, not in `win/`:
- `preview.rs` — `prepare_picture`: a picture decoded and scaled to the pixels it will take, as
  BGRA rows blended over the panel, for `libduscape::preview::Reader`; no Win32, tested everywhere
- `cli.rs` — the TUI's scan flags, and `--no-elevate`
- `elevate.rs` — a whole volume unelevated asks to run as administrator: `wanted` decides,
  `relaunch_args` and `command_line` (Win32 quoting, tested) make the new process's arguments
  — this process's plus `--no-elevate`, so it never asks in turn, plus the folder if it was
  picked in the dialog — and `relaunch` starts it through the shell's `runas`; declined
  (`Refused::Declined`) the scan goes on unelevated; failed, so does it, and the status bar says
  why. Only a local disk (`is_local_disk`: fixed or removable, by `GetDriveTypeW`) asks — a
  share, a CD or a RAM disk gains nothing from it
- `win/mod.rs` — the window: input → `Viewer` calls (points are pixels over the DPI scale), then
  `changed()` (a preview request at the drawn size — `wanted_preview_sized` — title, redraw).
  Messages the window does nothing with go to `DefWindowProcW` *outside* the re-entrancy guard
  (`HANDLED` is the list; an arm added to `dispatch` goes there too, or it never runs): the frame's own loops (an edge dragged, maximise, the system menu) run inside
  that call and send `WM_SIZE` and `WM_PAINT` re-entrantly, which the guard would drop.
  Threads post one boxed `AppMsg`; one arriving during a modal loop (message box, context menu)
  is queued FIFO in `PENDING` and handled when the handler returns — order matters, the outline's
  last batch comes before the finished tree. Outline batches are absorbed as they come and the
  view laid out once per burst, `OUTLINE_MS` after the first (`OUTLINE_TIMER`): the elevated
  scan of a volume sends dozens a second, and laid out per batch (15–25 ms each) the window
  answered nothing until the scan ended
- `win/paint.rs` — GDI, double-buffered, by `Viewer::layout`; returns the breadcrumbs for clicks.
  The list is drawn as the tree (`Viewer::rows`): each level indented `ROW_INDENT`, a folder's
  expander (`▸`/`▾`) in the `EXPANDER` column before its name, the "% of parent" bar from its
  level's indent. The
  treemap is drawn nested (`draw_nested`, after the top-level tiles and before the corner and
  the frames): each level a shade darker, a label where there is room, the hovered tile framed

**`duscape-viewer`** (`viewers/shared/`) — what the desktop viewers share, with no toolkit:
- `state.rs` — `Viewer`: everything the window shows and how it answers input — `Layout` (points;
  tiles in 2.4×6 pt cells, the treemap's 2.5 ratio), the entry in hand kept by *name* so a
  relayout cannot move it, marks, navigation, zoom, delete (`delete`, `delete_prompt`, and
  `removed` for a Trash), rescans (through `duscape_scan::rescan::Rescans`), the status bar's
  words; `absorb_summaries` takes outline batches in without a relayout and `catch_up` lays the
  view out for them (`add_summaries` is both). It keeps the TUI's rules from "Key Patterns": `chosen` says whether the entry in hand was
  picked or placed, and only a picked one seeds a Ctrl+click selection; a Shift run adds its range
  to the marks it started from (`mark_run`) and shrinks when reversed; every change to the marks
  copies their paths once the viewer has called `set_clipboard` (Windows does; macOS and Linux
  copy through their toolkits and read `target_paths`). Its tests run on every platform; a
  behaviour the windows should share goes here first. The list can be a *tree* (WizTree's
  view, `tree_view`): `libduscape::tiles::tree_rows` keeps which folders are open in place
  and makes the rows (each open folder's entries indented under it, largest first, `Row::path`
  from the listed folder); the cursor is a row's path, its first name being `selected`, so the
  treemap and the marks follow the row's top-level entry. → opens the folder in hand and then
  goes down into it, ← goes up to the folder and then closes it, a click on the expander
  (`Hit::Expander`) toggles; a nested row is what is previewed, copied, entered (down through
  the folders above it) or deleted. With it the treemap is nested (`nested`, rebuilt with the
  board): a tile inside a folder's (`Hit::Nested`) is a target like any other — a click opens
  the folders above it and puts its row in hand, Ctrl and Shift mark its top-level folder —
  and hovering one names it (`hover_nested`, cleared by every relayout since the tiles moved). Off by default (`set_tree_view`); all three
  desktop viewers turn it on for every scan, and a viewer that does not draw depth sees the flat
  listing and the flat tiles
- `menu.rs` — the context menu every desktop viewer opens on a right-click: `Viewer::
  context_menu(&Platform)` gives its `Entry`s (an `Action`, the words, whether it can be chosen)
  for what is targeted, the counts following the marks; `Platform` says what the viewer adds
  (the file manager's name, Quick Look, Copy as Pathname, a Trash). A viewer draws the entries
  (Win32 `TrackPopupMenu`, an `NSMenu`, the Linux viewer's `draw::menu`), adds its own key
  hints, and carries out the action chosen; an item offered on one window is offered on all.
  `open_in_hand` is Open: a folder goes in, a file's path comes back to be opened
- `scan.rs` — the first scan with the live `Outline`, results through callbacks on the scan's
  thread; `preview.rs` — the latest-wins preview reader (pictures are handed over as bytes for
  the viewer to decode: `NSImage` on macOS, the `image` crate on Linux; a binary file as its
  description and hex dump, `Loaded::Binary`)

**`duscape-linux`** (`viewers/linux/`) — the Wayland and X11 viewer, pure Rust, no C library
(not libwayland, not Xlib), so it builds static for musl and runs on any compositor or X server:
- `backend.rs` — the `Backend` trait (present a canvas, set the title, own the clipboard, move/
  maximise/minimise for the app's own title bar) and `Input`, what either windowing system
  reports, in points; `keys` has the non-character keysyms both speak; `level_keysym` picks the
  shifted level (Caps Lock only on letters); `open` chooses: `DUSCAPE_BACKEND`, else Wayland when
  `WAYLAND_DISPLAY` is set, else X11, trying the other if the first fails
- `wayland.rs` — `wayland-client`'s pure-Rust protocol: `wl_shm` double buffers in a memfd,
  `xdg_shell` (configure → `Input::Resized`, close), `xdg-decoration` asking for a server title bar
  and reporting `Input::Decorated(false)` where there is none, `wl_seat` keyboard (xkb keymap by
  `xkb`, repeat done here since the compositor leaves it to clients) and pointer (cursor from the
  theme via `wayland-cursor`; discrete or smooth wheel), `wl_output`/`preferred_buffer_scale` for
  the scale, `wl_data_device` offering the clipboard with the last input serial. Events are
  dispatched on a thread of their own; the app thread sends requests to the same connection
- `xkb.rs` — reads the compositor's xkb keymap text: `xkb_keycodes` names → codes (aliases too)
  and `xkb_symbols` first-group levels → keysyms, by name; keys it lacks fall back to a US
  layout by evdev code, keys it has but cannot name mean nothing. Tested against a keymap dumped
  by `xkbcomp` (`xkb_test_keymap.xkb`)
- `x11.rs` — `X11` on `x11rb`: the window, its properties and size hints, the events (read on a
  thread of their own into `Input`), the frame put up whole with `PutImage` (re-encoded only for
  an unusual visual), the core keyboard mapping, and the clipboard: when no `wl-copy`/`xclip`/
  `xsel` is installed the window owns `CLIPBOARD` itself and answers `SelectionRequest`. The
  scale (pixels per point) is `DUSCAPE_SCALE`, else `GDK_SCALE`, else `Xft.dpi`/96
- `canvas.rs` — the software framebuffer in points: fills with alpha, gradients, strokes,
  anti-aliased rounded rectangles, and `blit` (a picture fitted by box-filtering)
- `font.rs` — `Fonts::system` finds the sans, bold and mono faces through `fc-match` (else
  well-known paths, else `DUSCAPE_FONT*`); `Face` caches `fontdue` glyphs by character and
  quarter-pixel size; `Pen` draws into a rect, aligned, vertically centred, cut with "…"
- `draw.rs` — the frame, by `Layout`: the same panels as `mac/draw.rs`, in a fixed dark theme —
  the list as the tree (`rows`: each level indented `ROW_INDENT`, a folder's expander where
  `Viewer::hit` looks for it), the treemap nested (`nested`, after the top-level tiles and
  before the marks, the corner and the frames), a tile's label its name at the left and its
  size at the right (`tile_label`; a file's size at the bottom right when there is room);
  `dialog` paints the confirm/notice box and returns its buttons for clicks; `title_bar` the
  app's own title bar (`Viewer::top_inset`, `Layout::with_top`) where the compositor draws none
- `trash.rs` — freedesktop Trash: `gio trash` if present, else `~/.local/share/Trash` or
  `.Trash-<uid>` at the filesystem's top, with the `.trashinfo`
- `app.rs` — the loop over one `mpsc` channel (`Msg`: `Input` from the backend, scan batches,
  the finished tree, rescans, decoded previews, ticks, snapshot), draining what has piled up
  before one redraw — outline batches are absorbed as they come and laid out once for the lot
  (`outline_behind`); keys and mouse → `Viewer` calls like `mac/view.rs`'s (a click on an
  expander toggles before `click`, the wheel over the treemap zooms, the back button goes up); `Dialog` for asking
  before a removal; the title bar's buttons, drag and double-click → the backend.
  `DUSCAPE_SNAPSHOT=out.png` writes the frame after the scan and quits — how the drawing was
  checked here: X11 on an `Xvfb` (which `x11rb` reaches over TCP, `-listen tcp -ac`, since it
  does not do abstract sockets) with `xdotool` for keys and clicks; Wayland on a headless
  `weston --backend=headless-backend.so --shell=kiosk-shell.so`, unpacked from its .deb, with
  `weston-screenshooter` (it has no input to inject, so keys are covered by `xkb`'s tests)

**`duscape-mac`** (`viewers/macos/`) — the AppKit viewer, on `objc2`/`objc2-app-kit`, over
`duscape-viewer`:
- `mac/view.rs` — the one `NSView`: events, menu commands, dialogs, Trash, pasteboard, Finder,
  Quick Look, drag and drop. Other threads come back through `on_main` (the main dispatch queue).
  The `Viewer` is in a `RefCell`; never hold a borrow across a modal (`NSAlert::runModal`, the
  open panel), which runs the event loop inside the call. `DUSCAPE_MAC_SNAPSHOT=out.png` writes
  the view to a PNG after the scan and quits — how to look at the drawing without screen access
- `mac/script.rs` — `DUSCAPE_MAC_SCRIPT`: synthetic keys, clicks and menu choices posted to the
  app's own event queue, and `state` dumps to assert on. `tests/smoke.sh` runs one on a fixture;
  run it after changing the viewer (macOS, logged-in session, no permissions needed)
- `mac/draw.rs` — painting, by `Layout`: the same tree, nesting and labels as the Linux
  viewer's `draw.rs`; `mac/mod.rs` — the app, delegate, menus, window. In `mac/view.rs` a click
  on an expander toggles before `click`, a mouse's wheel (not a trackpad's) or a pinch over the
  treemap zooms, the back button goes up, and outline batches are laid out once per burst
  (`outline_behind`, a catch-up queued behind the batches waiting on the main queue)

**MS-DOS** (`viewers/dos/`) — FASM, real mode on a 286 (or 186) with no coprocessor, not part of
the Cargo workspace:
`DUSCAPE.ASM` (the program), `PANEL.ASM` (side panel), `PREVIEW.ASM` (text and half-block
pictures, the six adaptive DAC colours), `PNG.ASM` and `JPEG.ASM` (decoders; a JPEG block's mean
is its DC coefficient, so no IDCT), `PVDATA.ASM` (their data), `SOFTFP.ASM`/`FPDATA.ASM` (IEEE doubles in software, unpacked,
rounded after each operation), `J286.INC` (conditional jumps as short-or-inverted macros). No
32-bit register, FS/GS, 386 or x87 instruction, and no `dd`/`dq` variable (FASM makes 32-bit
code for those unasked): `python3 viewers/dos/tests/lint286.py` must say so, and test.py runs
everything on an emulated 286 without an FPU. 16-bit addressing has no `[si+di]`, and `[bp+…]`
is in SS. A port, not a binding: the squarify layout, `RectFloat::round`,
`Board` navigation, the tile text and the size formats are rewritten from `common/` and the TUI's
`grid/`, so a change to those should be carried over by hand. `make dos` fetches FASM and CWSDPMI
into `target/dos/` (hash-checked) and assembles inside DOSBox-X; `make dos-run` opens the repo as
C:. `-k KEYS -s FILE` types keys and writes the screen and tiles; `python3 viewers/dos/test.py`
runs the checks on fixtures that way, the layout ones against `TreeMap` through `viewers/dos/tiles`
(a crate outside the workspace). Run it after changing the DOS viewer. DOSBox-X loses long-name
search entries when two searches are open, so each folder is listed to the end before its
subfolders are walked (a pending stack in its own segment), and the delete queues its entries the
same way; keep it that way. Do not set attributes on host files needlessly: DOSBox-X stores them in
`.DBLOCALFILE_ATR_*` files beside them. Decoding runs with ES = the line segment and FS = the
window: a `rep stos`/`movs` into DS there must set ES first, and a table in the code segment is
read through `cs:`.

**`duscape`** (`viewers/tui/`) — the ratatui viewer:
- `issues.rs` — `--issues`: a scan with nothing drawn (`parallel::build_tree`, as the app scans),
  then `duscape_scan::environment` and `FileTree::issues`' report on stdout
- `front.rs` — which viewer this `duscape` is: `choose` (pure, tested) on the arguments and
  `Started` — `--tui`/`--gui`; `--benchmark`, `--issues`, `--help`, `--version` the terminal's; a name ending
  `-gui`/`-linux`/`-windows`/`-mac` the window; then a terminal on stdin *or* stdout is the
  terminal viewer (so a redirected benchmark stays one), neither with a display the window, and on
  Windows no console, or one the process has alone (`GetConsoleProcessList`), is Explorer's start:
  the window, after `FreeConsole`. `windows.manifest`, embedded by `build.rs` (`embed-manifest`,
  bins only, `gui` feature), sets `consoleAllocationPolicy` to `detached`, so from Windows 11 24H2
  Explorer makes no console at all and nothing flashes; older Windows ignores it and makes one,
  which is let go. The terminal viewer with no console (`--tui` from a shortcut) gets one
  (`ensure_console`, `AllocConsole`). `lib.rs`'s `run` dispatches: the window gets the same `Opt`, parsed once
  (`Opt::scan_options`, the folder, `--no-elevate`), not the config file. The executable is
  console subsystem, or the terminal viewer would have no stdin; `duscape-windows.exe` alone
  keeps `windows_subsystem = "windows"`. The window's elevated relaunch passes `--gui`
  (`elevate::relaunch_args`), since the program relaunched may be this one
- `main.rs` — entry point, thread spawning, channel setup
- `app/mod.rs` — `App` state machine, `UiMode` enum, render dispatch
- `input/controls.rs` — per-mode keypress handlers
- `messages/instruction.rs` — `Instruction` dispatch to `App` methods
- `ui/display.rs` — ratatui rendering orchestration
- `ui/side_panel.rs` — the list left of the treemap; `screen_areas` splits the screen (a third to
  the panel when ≥ 80 columns) and `entry_at` maps a cell to a row — used by both the renderer and
  the mouse, so they cannot disagree
- `config/mod.rs` — TOML config (`~/.config/duscape/config.toml`)
- `preview.rs` — the preview thread (debounce, then `libduscape::preview`), and kitty/sixel graphics output (`Graphics`:
  `KittyGraphics` writes after each frame, only on change; `q=2` so the terminal never answers
  on stdin, `z=-1` so dialogs cover it). `SixelGraphics` has nothing to delete a picture by and
  the frame never redraws cells it thinks blank, so `prepare` (before the frame) erases the old
  picture's cells when it changes; text over a sixel destroys it, so it is hidden while a dialog
  is up (`stays_under_text`), and `cleared` forgets it when a resize clears the screen. The
  encoder is a median-cut palette of 256 and the picture is cut to whole 6-pixel bands so none
  spills into the next row. `side_panel::screen_areas` sizes the preview 16:9 from the cell pixel
  size `Display` measures each frame (else the `CSI 16 t` answer, `queried_cell_pixels`).
  `graphics_protocol` decides once: env vars (kitty, Ghostty, WezTerm; tmux never kitty), else a
  graphics query + `CSI 16 t` + DA1 read straight off stdin — what finds it over ssh; sixels are
  DA1 attribute `4` (tmux answers for itself). Where nothing can be asked (Windows) Windows
  Terminal, foot, mlterm and iTerm2 are taken as sixel by name (WT with its fixed 10×20
  sixel cell, since the console reports no pixels). It must first run in raw mode
  before any thread reads stdin (`try_main`). `pictures()` picks `Pictures::Kitty`, `Sixel`,
  `Blocks` (the fallback: `▀` cells the previewer colours as `Preview::Blocks` and `side_panel`
  draws into the frame) or `Described`
- `clipboard.rs` — `libduscape::clipboard::copy`, else the terminal's OSC 52;
  paths are quoted by `libduscape::format::quote_path_for_shell` before they get there
- `cli/mod.rs` — clap CLI args

### UI State Machine (`UiMode`)

```rust
Loading                     // Scan in progress
Normal                      // Main treemap view
ScreenTooSmall              // Terminal < 50×15
DeleteFiles(Vec<FileToDelete>)  // Confirmation dialog: the marked entries, or the one in hand
ErrorMessage(String)        // Error display
Exiting { app_loaded: bool }
```

---

## Key Patterns

- **Multi-selection**: `App::marked` holds names in the order picked; `mark_range` is a Shift
  run's anchor and the marks it started from, so reversing shrinks the range. Every change calls
  `copy_marked`, which reuses right-click's `shell_path`. Plain moves, jumps, clicks and folder
  changes clear it. `cursor_chosen` says whether the entry in hand was picked (plain click, arrow,
  jump) or placed by the app (a folder's top row, a deleted entry's neighbour); only a picked one
  seeds a Ctrl+click selection, so `d` never deletes an entry nobody chose. `d` deletes every
  marked entry (`get_files_to_delete`), continuing past failures and naming the first.
- **Help line**: an endless strip, legend and tips alternating (`ui::bottom_line::Strip`). It
  rests `REST` (5 s, from arrival or the last key, whichever is later) at each stop — one per
  segment, or a page at a time for one wider than the screen — and slides to the next in `SLIDE`
  (80 ms, eased), so every move finishes within 100 ms. The position is kept *within a segment*
  (`Ticker`), so a legend that changes length (scan done) does not make what is on screen jump.
  Keys named in it come from `Keybinds`, never literals.
- **Colours**: no dark gray (unreadable on black) and no magenta on the light cursor bar; the
  cursor is black on gray, marks black on yellow. `side_panel` tests assert both.
- **Focus**: the list has it by default (`Focus::List`; `list_cursor: None` means its top row,
  and while the list has focus `render` syncs the treemap's selection to it). `App::focus` says
  which panel the keyboard drives; it follows the last click, Tab,
  and Left off the treemap's left edge, and is always `Treemap` while the panel is hidden. The
  list's cursor is kept by *name* (the listing re-sorts during a scan); moving it selects the
  entry's tile, or nothing if it has none. Enter, Esc and delete go through `selected_entry`, so
  an entry without a tile can still be acted on.
- **One listing, two views**: `Board::listing` is the folder's entries, largest first, unzoomed,
  sorted once per `change_files`; the side panel draws it every frame without re-sorting. Clicks
  are keyed by entry *name*, so a tile and a list row are the same target, and an entry with no
  tile can still be entered or copied.
- **Render-on-demand**: Render only when an `Instruction` arrives; no continuous loop.
- **Live treemap update**: while scanning, `Board` recomputes tiles from a folder-only *outline*
  (`Outline` → `FileTree::add_summary`) — every folder to `Outline::DEFAULT_DEPTH` with a running
  size, shared blocks counted in full, deeper directories rolled up into the frontier folder above
  them. Files, and folders below the frontier, appear when the finished tree replaces the outline
  (`App::finish_scan`). Keep the outline O(visible): the first version sent every directory and
  saturated the rendering thread, which back-pressured the dispatcher and slowed the walk.
- **Parallel build, no shared memory**: each builder owns a tree; correctness rests on
  `HardLinks::charge` being order-independent, so deferring every charge to one final replay gives
  the same per-folder sizes as charging inline. Tested folder-by-folder against the inline tree
  (`model::tests::sharded`). Shard by `SHARD_DEPTH` path components, not the whole path — see
  `docs/scan-performance.md` for why the merge otherwise costs more than the parallelism saves.
- **Two sizes everywhere**: `EntryMeta` carries `size` (on disk) and `apparent` (length); every
  walker fills both. `model::File` stores disk as `u64` plus apparent as an `i32` difference
  (`FileSizes::Far` boxes both when they are ≥ 2 GiB apart), keeping `FileOrFolder` at 16 bytes —
  asserted in `model::tests`; do not grow it. Folders hold `sizes: Sizes`. `FileTree::shown` and
  `Board::show` pick which is displayed; the ledger charges both kinds to the same ancestors and
  identifies files by the disk size. `ScanOptions::show_apparent_size` only sets the initial view.
- **Sizes are not additive**: a folder's size counts each distinct *set of blocks* once, so hard
  links and XFS/btrfs reflinks both make it smaller than the sum of its entries. See
  `docs/scan-performance.md`.
- **Zoom as filter**: Zoom level controls which nested folders are rendered.
- **Modal via enum**: `UiMode` variant change = modal open/close; no separate stack.
- **Config merging**: CLI `--apparent-size` ORs with config file setting.
- **Graceful degradation**: Read errors counted but scan continues. A failed *directory read* ends
  that directory rather than retrying it — the error belongs to the descriptor, not the entry.
- **Per-filesystem attributes**: a returned-attributes bitmap says an attribute is *present*, not
  that its value is real. `msdosfs` claims `ATTR_FILE_ALLOCSIZE` and packs zero, so the size
  attribute is chosen per device. See `docs/scan-performance.md`.
- **Pseudo-filesystems**: crossing a mount point into `/proc`, `/sys`, cgroup, debugfs and friends
  is refused (by `statfs` magic); naming one as the scan root still scans it.
- **Loops**: a mount of a directory inside itself, or of one above the scan inside it, is one of
  its own ancestors, and walked holds itself again (Synology's `/volume1/@docker`, a folder
  mounted onto itself, is not one: the `synology` fixture scenario lays out a real DSM mount table
  and passes with this check disabled). At every mount root or crossing — only a mount can loop, Linux having
  no directory hard links — `linux::loops_back` stats the ancestors up to `/` and compares device
  and inode; a match is not walked, and is noted (`DirEntries::note`, kind `loop`: in `--issues`,
  not in the failure count). `walk_would_enter` refuses such a folder to a rescan.
- **`-x` on btrfs**: the boundary is the filesystem, not the subvolume (`linux::same_filesystem`:
  the same device, or btrfs with the root's `extent_space`, the filesystem's UUID hash). Every
  subvolume has its own `st_dev`, so by device `-x` stopped at each — every Synology share.
  Deliberately unlike `du -x`.
- **Snapshots**: a read-only btrfs subvolume inside the scan is left empty by default, as a mount
  `-x` refuses; `--snapshots` walks it, the extent ledger counting what it shares once. Named as
  the scan root, it is scanned.
- **Bind mounts**: a mount root (`STATX_ATTR_MOUNT_ROOT`) is looked up in `/proc/self/mountinfo`
  by `stx_mnt_id` (`linux::mounts`); if an earlier mount of the same device shows the same
  directory at a path inside the scan — checked by device and inode — the mount is left empty.
- **btrfs compression (root only)**: `linux::btrfs_extents::on_disk` reads a file's
  `EXTENT_DATA` items with `BTRFS_IOC_TREE_SEARCH_V2` on the directory fd (tree 0 = its
  subvolume) and sums what they occupy; it replaces `stx_blocks` as the disk size. Gated by
  `filesystem::Compressed` (decided per mount: `Maybe` if the superblock options say `compress`,
  `Marked` for `STATX_ATTR_COMPRESSED` files only, `Never` if not btrfs or not permitted), since
  it costs ~1 µs a file. The FIEMAP identity pages through long extent maps (compressed files have
  one extent per 128 KiB).
- **Two passes**: the walk probes shared extents only from 64 KiB; smaller files are noted in
  `DirEntries::later` and probed after the tree is shown (see the `refine_N` thread). The
  benchmark's `refined` stage is walk + second pass, and is what the fixtures measure.
- **Network filesystems**: refused the same way, whatever `-x` says — NFS, SMB, 9p, Ceph, AFS… by
  magic (`linux::filesystem::NETWORK`), and FUSE by its subtype in `/proc/self/mountinfo`, found by
  `stx_mnt_id` (`NETWORK_FUSE`: sshfs, rclone, s3fs…), since FUSE also serves local filesystems.
  macOS skips a mount point without `MNT_LOCAL`. Another machine's files are not this disk's space.
- **ManuallyDrop on FileTree**: Avoids slow recursive drop on exit.
- **Folders know their ledger id**: `Folder::dir` is the folder in `HardLinks`, given when a
  directory's path is first resolved (`HardLinks::child(parent)`, no hashing) and `NONE` until
  then. Deferred sightings carry ids, not paths; a merge renumbers the folders it moves in
  (`Folder::renumber`, remapping the other tree's sightings) and so does a graft. Interning by
  path was 0.7 µs a directory and a third of the ledger's time — see "The tree build" in
  `docs/scan-performance.md`. `--benchmark --bench-profile` shows the build phase by phase.

---

## Default Keybinds

| Action | Key |
|--------|-----|
| Quit | `q` |
| Delete | `d` |
| Navigate | `h/j/k/l` or arrow keys |
| Enter folder | `Enter` |
| Go to parent | `Esc` |
| Select tile | left click |
| Enter folder | double-click (same tile, within 500 ms) |
| Switch list / treemap | `Tab` (`←` off the treemap's left edge, `→` from the list) |
| Jump through list | `PgUp` / `PgDn` / `Home` / `End` |
| Mark a range (copies) | `Shift`+`↑`/`↓` in the list |
| Mark / unmark (copies) | `Ctrl`+click, either panel |
| Copy relative path | right-click |
| Copy absolute path | double right-click |
| Zoom in/out | `+` / `-` |
| Disk usage / apparent size | `a` (no rescan) |
| Rescan selected folder / everything | `r` / `R` (not during the first scan) |
| Reset zoom | `0` |
| Confirm | `y` |
| Cancel | `n` |

---

## Code Conventions

- **Error handling**: `thiserror` derives; `?` propagation; distinct error enums per crate boundary.
- **Testing**: `#[cfg(test)] mod tests` in same file; temp dirs via helpers; setup → action → assert.
- **Concurrency**: Named threads; bounded channels; `park_timeout` (100ms) for polling.
- **Exports**: `pub use` re-exports in `mod.rs` files.
- **No async runtime**: Threads + channels only.
- **Cross-platform**: Linux, macOS, and Windows supported. Windows consoles report key releases
  as events; `TerminalEvents` drops them, so handlers only ever see presses. It drops mouse
  movement, drags, releases and scrolls too (`is_mouse_noise`): mouse capture reports every
  movement, and the warning modal closes on any event. CI runs on Linux only
  — check other targets with `cargo clippy --workspace --all-targets --target <triple>`.
  The BSDs have no native walker: they scan with the `dua-core` fallback. The Linux window is
  gated to Linux and FreeBSD and type-checks for `x86_64-unknown-freebsd`, but has never been
  run there; say so rather than claim BSD support.
- **musl**: the release is built for musl, and `libc` types differ there. `ioctl`'s request is
  `c_ulong` on glibc but `c_int` on musl, so request constants are `libc::Ioctl`. CI tests
  `x86_64-unknown-linux-musl` on every push (`test-musl`).

---

## Adding Features — Agent Guidance

### Adding a new keybind
1. Add field to `KeybindConfig` in `viewers/tui/src/config/mod.rs`
2. Add parsing in `viewers/tui/src/config/keybind.rs`
3. Add to `Keybinds` struct and wire in `input/controls.rs`
4. Update `example/config.toml`

### Adding a new UI mode
1. Add variant to `UiMode` in `app/mod.rs`
2. Add `handle_keypress_<mode>()` in `input/controls.rs`
3. Add render arm in `ui/display.rs`
4. Wire `Instruction` variants in `messages/instruction.rs`

### Adding a scan option
1. Add field to `ScanOptions` in `common/src/scan/mod.rs`
2. Thread it through **every** walker in `scanners/src/`: `macos.rs` (macOS), `linux.rs`, `windows.rs`,
   and the `fallback` module in `lib.rs` (everywhere else). The fallback is `cfg`-selected away on macOS, so it is only
   ever run by its tests here — do not assume compiling it means it works.
3. Expose via CLI in `viewers/tui/src/cli/mod.rs` and config if persistent
4. Add a `--benchmark` stage if it changes how the walk performs

### Filesystem fixtures — the standard for scan and accounting changes
`make test-fs` (root or the `docker` group) makes ext4, XFS, btrfs, f2fs, tmpfs, FAT32, exFAT and
NTFS on loopback images, fills them with hard links, sparse files, reflinks, snapshots and
compression, builds mount layouts (nested, `proc`, `tmpfs`, bind mounts, loopback NFS,
`fuse.rclone`), runs both test suites on
each with `TMPDIR` there, and checks totals against oracles that share no code with duscape.
See `fixtures/fs/README.md`. Any change to the walkers, `EntryMeta`, the hard-link/reflink ledger
or mount handling must pass it, and a behaviour that depends on the filesystem or the mount table
gets a fixture there. CI runs it (`fs-fixtures.yml`). Remember: every btrfs subvolume and snapshot
has its own `st_dev` and `f_fsid` — never key anything cross-file on the device alone there.

### Touching filesystem attributes
Test against FAT as well as APFS — it is the filesystem that misreports. `docs/scan-performance.md`
has the FAT section, and the volume test is:

```bash
cargo test -p libduscape --lib -- --ignored fat32
```

### Changing the scan
Read `docs/scan-performance.md` first, and `docs/scan-roadmap.md` for what is planned and the
rules each step follows: measure with `docs/probes/bench-matrix.sh` before and after (it writes
`docs/benchmarks/<host>-<date>.md`; commit it), totals identical, fixtures green. It records what was measured, what turned out not to
matter, and how to reproduce the numbers with `--benchmark`. The short version: on Linux the walk
is the floor (~0.40s for 4.2M entries, at the kernel's `statx` cost) and the tree build is hidden
behind it on `parallel::SHARDS` threads. On macOS and Windows the walk is the whole scan — macOS
waits on 4 KiB metadata reads and is fastest at six workers — so `SHARDS` is 1 there and any
serial work after the walk shows directly in the scan time. `--bench-stage sharded` is the app's
path; `pipeline` is the single-threaded build it replaced. Those are warm numbers. Cold (caches
dropped) the walk is bound by reads in flight, one directory's inode and blocks per blocked
worker, which is why Linux runs three workers a core, stats a directory in inode order, walks children
smallest inode first, and as root reads the directories' blocks ahead through the device
(`linux::dirblocks`, ext4 only — `DUSCAPE_DIRBLOCKS_DEVICE=<file>` forces the path for testing).
As root on ext4 the whole walk reads the device instead (`ext4.rs`, roadmap step 2: cold 1.7x
the kernel walk); `--no-device-read` compares;
`docs/probes/bench-diskus.sh` measures warm and cold against `diskus` — see "Cold cache" in the doc. Anything you change must keep `sharded`'s totals identical
to `pipeline`'s — that comparison is the correctness check, not just the speed one. On Windows the
floor is a fixed kernel and filter-driver cost per directory handle (3× on the system volume);
opening by file id, closing off the walker threads and skipping the last listing call were all
measured and none helped — read the 2026-09-24 section before trying them again.

### Modifying treemap layout
- Core algorithm: `common/src/tiles/treemap.rs`
- Tile rendering: `viewers/tui/src/ui/grid/` (terminal), `viewers/windows/src/win/paint.rs`,
  `viewers/macos/src/mac/draw.rs` (`treemap`)
- Adjust `HEIGHT_WIDTH_RATIO`, `MINIMUM_HEIGHT`, `MINIMUM_WIDTH` constants
- Entries below the minimum tile size are never dropped: they fold into the "small files" `x`
  marker, whose corner is clamped by `SMALL_FILES_MINIMUM_WIDTH/HEIGHT` so it stays visible even
  when the hidden entries round to zero cells

### Releases
A `v*` tag runs `deploy.yml`. It builds `duscape-<tag>-<target>.tar.gz` for
`x86_64-unknown-linux-musl` (`musl-gcc`) and `aarch64-unknown-linux-musl` (`cargo zigbuild`,
zig 0.13.0), and `duscape-<tag>-x86_64-pc-windows-gnu.zip` (`cargo zigbuild`, against the
Universal C Runtime: only DLLs Windows 10 carries), each `duscape` being the terminal viewer and
the window. The Linux binaries are fully static, so they have no glibc floor and run on Alpine
and busybox; the job checks it, and that `duscape.exe` is console subsystem. macOS is not in
it: linking AppKit needs a Mac, where `make mac-app` makes the universal binary and
`Duscape.app` (`viewers/macos/Info.plist`) — a bare binary opened from Finder runs in Terminal,
so Finder's way to the window is the bundle. One job then publishes every archive: matrix jobs
that each create the release race. The repository's Actions permission must allow actions from
outside it (`actions/checkout`…): set to local actions only, every run fails to start.
The version is the workspace's (`Cargo.toml`), with the path dependencies' `version` beside it.
- **`opt-level = "s"`**: the release profile is size-optimised for the viewers' binaries, but
  `"z"` cost the scan 13% and the tree build 30%; `"s"` is as fast as `3` at 2% more size. With
  `lto = true` a per-crate `opt-level` does nothing, the final codegen uses the top level's.
  `--profile profiling` is the release with symbols, for samplers. PGO (`make pgo`) is worth
  2–3% wall, 6–8% CPU, and is not in the release pipeline; thin LTO is slower than fat.
- **Allocator**: musl builds use jemalloc (`tikv-jemallocator`, 64-bit musl only). musl's own
  malloc made the scan 7x slower and mimalloc 2x. Do not swap it without rerunning the
  `--bench-stage sharded` comparison in `docs/scan-performance.md`. glibc builds use the system
  allocator.
- **aarch64 page size**: jemalloc fixes its page size at build time. aarch64 builds set
  `JEMALLOC_SYS_WITH_LG_PAGE=16`, so the binary also runs on 16K- and 64K-page kernels.
- **Licences**: jemalloc is BSD-2-Clause and linked in, so its `COPYING` ships in each tarball as
  `LICENSE-jemalloc`.
- **zig as the musl C compiler without zigbuild**: cc-rs passes a Rust `--target=` that zig
  rejects, and zig's debug-mode UBSan traps in jemalloc (tests die with SIGILL). Filter the flag
  and pass `-fno-sanitize=undefined`, or just use `cargo zigbuild`, which does both.

---

## Quality

What is measured, where the limit is, and what to do when a change crosses it. `make quality`
prints the live figures; `make coverage` the test coverage.

- **Function size and complexity.** `cargo clippy` warns — and CI, with `-D warnings`, fails —
  on a function over 100 lines or of cognitive complexity over 25 (`clippy.toml`; the lints are
  turned on for every crate by `[workspace.lints]`). Split the function, or when its length is
  the honest shape of the work (a widget drawn top to bottom, a listing decoded entry by entry)
  put `#[allow(clippy::too_many_lines)] // quality debt: <why>` on it. Those markers are the
  debt register, which `make quality` lists; do not add one without the reason, and take one
  off when you split the function. Measured on all three targets, since each viewer's code is
  compiled on its platform alone.
- **Tests.** `cargo test --workspace` passes on Linux (CI) and on Windows and macOS; a fixture
  that needs something the machine lacks (symlink creation on Windows without Developer Mode,
  a sparse file on a filesystem that makes none) skips with a comment saying so, never fails
  and never asserts less than it did. Tests are platform-correct, not platform-skipped: a path
  is built with `Path::join`, a quoted path with `quote_path_for_shell`, a sparse file with
  `os::set_sparse`.
- **`unsafe`.** Every `unsafe` block has a `// SAFETY:` comment above it saying what is
  upheld; `make quality` counts the blocks per crate and those without one, which is zero.
- **Coverage.** `make coverage` (`cargo llvm-cov`). The viewers that are stubs on the platform
  measuring show as uncovered; the row to read is the crate changed. Baseline, 2026-09-26, on
  Windows: 70.6% of lines and 74.4% of functions in all — `common` 88.7%, `viewers/tui` 76.2%,
  `viewers/shared` 74.0%, `scanners` 65.4% (its Linux walkers are not compiled there),
  `viewers/windows` 7.1% (GDI and Win32; what it decides is in `viewers/shared`, and tested).
- **Dependencies and spelling.** `cargo deny check` (licences) and `typos`, in CI.

## CI Checks (must pass)

- `cargo test --workspace`
- `cargo test -p libduscape -p duscape --target x86_64-unknown-linux-musl`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo deny check`
- typos check
- `fixtures/fs/run.sh` (`fs-fixtures.yml`)

---

## File Size Reference

Regenerated by hand from `wc -l` when this file is touched; `make quality` prints the live figures.

| File | Purpose |
|------|---------|
| `viewers/tui/src/lib.rs` | ~470 lines — which viewer (`front`), terminal setup, thread/channel setup |
| `viewers/tui/src/app/mod.rs` | ~1300 lines — the TUI's state machine |
| `viewers/tui/src/preview.rs` | ~1300 lines — preview thread, kitty/sixel/half-block output, detection |
| `viewers/windows/src/win/mod.rs` | ~950 lines — the Windows window: input → `Viewer`, threads, messages |
| `viewers/windows/src/win/paint.rs` | ~870 lines — GDI drawing by `Viewer::layout` |
| `viewers/shared/src/state.rs` | ~1670 lines — the desktop viewers' shared state, no toolkit |
| `viewers/macos/src/mac/view.rs` | ~1300 lines — the macOS viewer's view, events and commands |
| `viewers/linux/src/wayland.rs` | ~1000 lines — the Wayland backend: shm, xdg-shell, seat, clipboard |
| `viewers/linux/src/app.rs` | ~1030 lines — the Linux viewer's loop, keys, mouse, dialogs, context menu, title bar |
| `viewers/linux/src/draw.rs` | ~890 lines — the Linux viewer's painting: the tree, the nesting, the menu, the dialogs |
| `viewers/linux/src/x11.rs` | ~520 lines — the X11 backend: window, events, `PutImage`, keymap, clipboard |
| `viewers/linux/src/xkb.rs` | ~360 lines — the xkb keymap reader |
| `common/src/tiles/board.rs` | ~230 lines — tile nav/zoom |
| `common/src/tiles/treemap.rs` | ~270 lines — squarify |
| `common/src/model/files/file_tree.rs` | ~590 lines — folder tree, hard-link accounting, the build profile |
| `scanners/src/lib.rs` | ~770 lines — walker selection, parallel build, fallback, `environment` |
| `scanners/src/linux.rs` | ~1700 lines — Linux `getdents64`/`statx` walker, inode order, block prefetch, the `fstatat` fallback |
| `scanners/src/macos.rs` | ~830 lines — macOS `getattrlistbulk` walker |
| `scanners/src/windows.rs` | ~920 lines — Windows bulk-listing walker |
| `viewers/tui/src/bench/mod.rs` | ~470 lines — `--benchmark` harness |
| `viewers/dos/DUSCAPE.ASM` | ~4700 lines — the MS-DOS viewer, one instruction a line |
| `viewers/dos/SOFTFP.ASM` | ~810 lines — IEEE doubles on a 286 |
| `viewers/dos/PREVIEW.ASM` | ~1300 lines — its previews: text, blocks, the palette |
| `scanners/src/mft.rs` | ~980 lines — NTFS read from its master file table: the parser, the tree, and the volume read |
| `viewers/tui/src/ui/display.rs` | ~320 lines — the frame: what every mode shows, and each mode's modal |
