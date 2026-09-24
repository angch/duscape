# AGENTS.md — Diskonaut Agentic Development Guide

## Project Overview

**Diskonaut** is an interactive terminal disk space navigator (TUI) written in Rust. It visualizes
disk usage via a squarify treemap, supports live scanning, and allows deleting large files in-place.

**Workspace layout** (Rust 2024 edition, version 0.1.0; the `diskonaut-angch` fork — see README):
```
diskonaut/
├── libdiskonaut/     # Core library: model, scan, treemap, formatting, os
├── diskonaut/        # TUI binary: CLI, UI, app state, input, config
├── diskonaut-gui/    # Windows GUI binary: windows-sys + GDI treemap, reuses libdiskonaut
├── example/config.toml
└── Cargo.toml        # Workspace root
```

---

## Essential Commands

```bash
cargo build --workspace          # Build everything
cargo test --workspace           # Run all tests
cargo clippy --workspace --all-targets -- -D warnings   # Lint
cargo fmt --all                  # Format code
cargo fmt --all -- --check       # Format check (CI)
```

**Static release binaries** (what `deploy.yml` ships; see "Releases" below):
```bash
make static           # x86_64-unknown-linux-musl, needs musl-gcc (apt install musl-tools)
make static-aarch64   # aarch64-unknown-linux-musl, needs zig + cargo-zigbuild
```

**Run the binary:**
```bash
cargo run --bin diskonaut -- [FOLDER]
cargo run --bin diskonaut -- -a  # apparent size mode
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
| `refine_N` | The second pass (`rescan::Refiner` → `scan::refine::refine`, a few `refine_*` workers): FIEMAP on the small files the walk noted (`SmallFiles`, 4–64 KiB, `nlink == 1`, XFS/btrfs), directories under `App::refine_focus` (the folder shown) first → `Instruction::Refined(generation, Found, left)`; `FileTree::apply_found` charges them to the ledger. A new whole tree cancels it; folders rescanned meanwhile are skipped (`refine_skip`), since a folder rescan refines its own tree before grafting |
| `rescan_N` | One per `r`/`R` (`rescan::Rescanner`). A folder the whole scan would not enter (`scan::walk_would_enter`: pseudo, network, `-x`, bind duplicate) or past `--max-depth` (counted from the scan root) is not rescanned (`Outcome::NotWalked`); a delete inside a folder being rescanned restarts that rescan. `parallel::build_tree` on the folder → `Instruction::Rescanned(id, Outcome)`. `App::rescan_done` grafts it (`FileTree::graft`, ancestors corrected by the difference) and leaks the old folder like `finish_scan` does. A rescan that another under way covers is not started; one the new rescan covers is cancelled and its result dropped |
| `previewer` | Reads the file in hand for the preview: first 64 KB as text, or a PNG/JPEG decoded and scaled after a 100 ms debounce (a newer request supersedes it) → `Instruction::PreviewReady(generation, _)`; answers to an older generation are dropped |
| **main** | App state mutations + ratatui rendering. During the scan it renders from the *outline*; on `ScanComplete` it swaps in the finished tree, keeping the current folder |

**Synchronization**: `Arc<AtomicBool>` for `running`/`loaded` flags; bounded sync channels (capacity 1–100).

### Crate Responsibilities

**`libdiskonaut`** — pure logic, no TUI:
- `model/files/file_tree.rs` — `FileTree`: hierarchical navigation, deletion tracking;
  `deferring_shared_blocks` / `merge_from` / `replay_deferred` for the parallel build;
  `add_summary` for the outline
- `scan/mod.rs` — `scan_directories()`: per-directory batches, the seam every walker plugs into;
  `parallel::build_tree()`: the app's tree build — shard by path prefix, merge, replay;
  `Outline`/`DirSummary`: the depth-capped live view
- `scan/macos.rs` — macOS walker on `getattrlistbulk(2)` (see `docs/scan-performance.md`)
- `scan/linux.rs` — Linux walker on `getdents64`/`statx`, own thread pool; also the `FS_IOC_FIEMAP`
  reflink probe. `dua-core` is only the fallback for other platforms and the benchmark baseline
- `scan/ntfs.rs` — NTFS file-record parser: sizes `$MFT` and the other metadata files the Windows
  walker adds at a volume root when elevated (records fetched with `FSCTL_GET_NTFS_FILE_RECORD`).
  Platform-independent so its tests run on Linux CI
- `scan/windows.rs` — Windows walker: one handle per directory, entries read in bulk with
  `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`. No listing carries a link count, so
  files in hard-link hot spots (or all files ≥ `--hard-link-threshold`) are sent with
  `LINKS_UNKNOWN` and the ledger dedupes them by file id — memory instead of a file open each
- `model/files/hard_links.rs` — charges shared blocks to each folder once, over interned directory
  ids; two ledgers, one keyed on inode (hard links) and one on physical extent (reflinks)
- `model/files/hash.rs` — the fast hasher behind the folder and inode maps
- `tiles/treemap.rs` — squarify algorithm (`HEIGHT_WIDTH_RATIO = 2.5`)
- `tiles/board.rs` — `Board`: tile selection, zoom stack, navigation
- `format/display_size.rs` — byte → human-readable (B/KB/MB/GB/TB)
- `os/unix.rs`, `os/windows.rs` — `is_user_admin()`, `size_on_disk_fast()`, `volume_id()`, `link_count()`

**`diskonaut`** — TUI application:
- `main.rs` — entry point, thread spawning, channel setup
- `app/mod.rs` — `App` state machine, `UiMode` enum, render dispatch
- `input/controls.rs` — per-mode keypress handlers
- `messages/instruction.rs` — `Instruction` dispatch to `App` methods
- `ui/display.rs` — ratatui rendering orchestration
- `ui/side_panel.rs` — the list left of the treemap; `screen_areas` splits the screen (a third to
  the panel when ≥ 80 columns) and `entry_at` maps a cell to a row — used by both the renderer and
  the mouse, so they cannot disagree
- `config/mod.rs` — TOML config (`~/.config/diskonaut/config.toml`)
- `preview.rs` — the preview thread, file sniffing, and kitty/sixel graphics output (`Graphics`:
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
- `clipboard.rs` — native clipboard (`pbcopy`, Win32, `wl-copy`/`xclip`/`xsel`), OSC 52 fallback;
  paths are quoted by `libdiskonaut::format::quote_path_for_shell` before they get there
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
- **musl**: the release is built for musl, and `libc` types differ there. `ioctl`'s request is
  `c_ulong` on glibc but `c_int` on musl, so request constants are `libc::Ioctl`. CI tests
  `x86_64-unknown-linux-musl` on every push (`test-musl`).

---

## Adding Features — Agent Guidance

### Adding a new keybind
1. Add field to `KeybindConfig` in `diskonaut/src/config/mod.rs`
2. Add parsing in `diskonaut/src/config/keybind.rs`
3. Add to `Keybinds` struct and wire in `input/controls.rs`
4. Update `example/config.toml`

### Adding a new UI mode
1. Add variant to `UiMode` in `app/mod.rs`
2. Add `handle_keypress_<mode>()` in `input/controls.rs`
3. Add render arm in `ui/display.rs`
4. Wire `Instruction` variants in `messages/instruction.rs`

### Adding a scan option
1. Add field to `ScanOptions` in `libdiskonaut/src/scan/mod.rs`
2. Thread it through **every** walker: `scan/macos.rs` (macOS), `scan/linux.rs`, `scan/windows.rs`,
   and the `fallback` module in `scan/mod.rs` (everywhere else). The fallback is `cfg`-selected away on macOS, so it is only
   ever run by its tests here — do not assume compiling it means it works.
3. Expose via CLI in `diskonaut/src/cli/mod.rs` and config if persistent
4. Add a `--benchmark` stage if it changes how the walk performs

### Filesystem fixtures — the standard for scan and accounting changes
`make test-fs` (root or the `docker` group) makes ext4, XFS, btrfs, f2fs, tmpfs, FAT32, exFAT and
NTFS on loopback images, fills them with hard links, sparse files, reflinks, snapshots and
compression, builds mount layouts (nested, `proc`, `tmpfs`, bind mounts, loopback NFS,
`fuse.rclone`), runs both test suites on
each with `TMPDIR` there, and checks totals against oracles that share no code with diskonaut.
See `fixtures/fs/README.md`. Any change to the walkers, `EntryMeta`, the hard-link/reflink ledger
or mount handling must pass it, and a behaviour that depends on the filesystem or the mount table
gets a fixture there. CI runs it (`fs-fixtures.yml`). Remember: every btrfs subvolume and snapshot
has its own `st_dev` and `f_fsid` — never key anything cross-file on the device alone there.

### Touching filesystem attributes
Test against FAT as well as APFS — it is the filesystem that misreports. `docs/scan-performance.md`
has the FAT section, and the volume test is:

```bash
cargo test -p libdiskonaut --lib -- --ignored fat32
```

### Changing the scan
Read `docs/scan-performance.md` first. It records what was measured, what turned out not to
matter, and how to reproduce the numbers with `--benchmark`. The short version: on Linux the walk
is the floor (~0.40s for 4.2M entries, at the kernel's `statx` cost) and the tree build is hidden
behind it on `parallel::SHARDS` threads. On macOS and Windows the walk is the whole scan — macOS
waits on 4 KiB metadata reads and is fastest at six workers — so `SHARDS` is 1 there and any
serial work after the walk shows directly in the scan time. `--bench-stage sharded` is the app's
path; `pipeline` is the single-threaded build it replaced. Anything you change must keep `sharded`'s totals identical
to `pipeline`'s — that comparison is the correctness check, not just the speed one. On Windows the
floor is a fixed kernel and filter-driver cost per directory handle (3× on the system volume);
opening by file id, closing off the walker threads and skipping the last listing call were all
measured and none helped — read the 2026-09-24 section before trying them again.

### Modifying treemap layout
- Core algorithm: `libdiskonaut/src/tiles/treemap.rs`
- Tile rendering: `diskonaut/src/ui/grid/`
- Adjust `HEIGHT_WIDTH_RATIO`, `MINIMUM_HEIGHT`, `MINIMUM_WIDTH` constants
- Entries below the minimum tile size are never dropped: they fold into the "small files" `x`
  marker, whose corner is clamped by `SMALL_FILES_MINIMUM_WIDTH/HEIGHT` so it stays visible even
  when the hidden entries round to zero cells

### Releases
A `v*` tag runs `deploy.yml`. It builds `diskonaut-angch-<tag>-<target>.tar.gz` for
`x86_64-unknown-linux-musl` (`musl-gcc`) and `aarch64-unknown-linux-musl` (`cargo zigbuild`,
zig 0.13.0). The binaries are fully static, so they have no glibc floor and run on Alpine and
busybox. One job then publishes both tarballs: matrix jobs that each create the release race.
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

## CI Checks (must pass)

- `cargo test --workspace`
- `cargo test -p libdiskonaut -p diskonaut-angch --target x86_64-unknown-linux-musl`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo deny check`
- typos check
- `fixtures/fs/run.sh` (`fs-fixtures.yml`)

---

## File Size Reference

| File | Purpose |
|------|---------|
| `diskonaut/src/main.rs` | ~225 lines — thread/channel setup |
| `diskonaut/src/app/mod.rs` | ~300+ lines — core state machine |
| `libdiskonaut/src/tiles/board.rs` | ~200+ lines — tile nav/zoom |
| `libdiskonaut/src/tiles/treemap.rs` | ~150+ lines — squarify |
| `libdiskonaut/src/model/files/file_tree.rs` | ~150 lines — folder tree, hard-link accounting |
| `libdiskonaut/src/scan/macos.rs` | ~800 lines — macOS `getattrlistbulk` walker |
| `diskonaut/src/bench/mod.rs` | ~230 lines — `--benchmark` harness |
