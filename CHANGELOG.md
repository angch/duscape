# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Forked as **`diskonaut-angch`** and restarted versioning at `0.1.0`. This fork diverged
  substantially from upstream diskonaut `0.13.0` (native per-platform walkers, parallel tree build,
  Windows support) and now versions independently; `repository` and `homepage` point at the fork.
  The binary is still named `diskonaut`, so the command and docs are unchanged. Entries below this
  line predate the rename.

### Added

- `d` deletes every marked entry, after one prompt that counts them, totals their size and names
  as many as fit. A failure part-way does not stop the rest; the message says what failed.
- Several entries can be marked: `Ctrl`+click in either panel toggles one, and `Shift`+`↑`/`↓`
  in the list marks a run. Each change copies the marked paths to the clipboard, shell-quoted
  and space-separated in the order picked, and flashes them in the title.
- The list has the keyboard when diskonaut starts, on its largest entry.
- Selections are easier to read: the cursor is black on light gray rather than magenta on gray,
  and entries listed without a tile are light gray rather than dark gray.
- The keyboard drives one panel at a time: the one last clicked, switched with `Tab` (new
  `switch-panel` keybind), or entered with `←` off the treemap's left edge and left with `→`.
  In the list, `↑`/`↓` walk every entry in size order — including those with no tile —
  `PgUp`/`PgDn`/`Home`/`End` jump, and `Enter`, `Esc` and `d` act on the highlighted row.
- A list beside the treemap, in the left third of any terminal 80 columns or wider: the folder's
  path, size, file count, share of the scan and contents, then every entry largest first with a
  bar, size and percentage. Borderless, so it gets every line and column. The selected tile's row
  is highlighted and kept in view; entries too small for a tile are listed dimmed. Rows take the
  same clicks as tiles.
- Right-click a tile to copy its path, relative to the directory diskonaut was run from, to the
  clipboard — in `/home/user/foo`, `diskonaut ../bar/` with `baz` selected copies `../bar/baz` —
  and double right-click to copy the absolute path. Paths are quoted for pasting into a shell (PowerShell on
  Windows), with control characters, invisible bidi characters and non-UTF-8 bytes escaped, and a
  leading `-` made `./-`. The title shows what was copied for two seconds. Copies go to the
  native clipboard (`pbcopy`, the Windows clipboard, `wl-copy`/`xclip`/`xsel`), or to the
  terminal by OSC 52 where there is none, as over SSH.
- Mouse support in the terminal UI: click a tile to select it, double-click a folder to open it
  (two clicks on the same tile within 500 ms). Works while the scan is still running. The mouse
  is captured while diskonaut runs, so the terminal's own text selection needs `Shift` (or
  `Option` in iTerm2 and Terminal.app); it is released on every exit, including a panic.
- Fully static Linux releases for x86_64 and aarch64,
  `diskonaut-angch-<version>-<arch>-unknown-linux-musl.tar.gz`, that run on any Linux of their
  architecture whatever its glibc (or none: Alpine, busybox). They replace the dynamic glibc
  tarball, which needed glibc 2.39 and `libgcc_s`. The musl build uses jemalloc, because
  musl's own allocator made the scan 7x slower; see `docs/scan-performance.md`. `make static`
  builds it locally (needs `musl-tools`); `make static-aarch64` cross-builds with `cargo-zigbuild`.
- Windows support. A native walker reads a directory's sizes, allocation and file ids in bulk
  (`GetFileInformationByHandleEx`) instead of opening every file: a 609k-entry `D:\` scans in
  0.4s instead of 34s, a 2M-entry `C:\` in about 8s. Junctions, symbolic links and mounted folders
  are not followed.
- `--hard-link-threshold BYTES` (Windows): Windows lists no link count, so hard links are found by
  tracking files by id, which costs memory. By default only the places hard links are normally
  made are tracked — the Windows directory, Edge, Docker and Git installs, `node_modules`, pnpm and
  uv stores; run as administrator, every file is. The flag tracks every file of at least that
  size, everywhere; `1` is exact.
- `--benchmark` prints each stage's total in exact bytes, so stages can be compared to the byte.
- A scan of a whole volume shows the volume's used space and how much of it the scan did not find
  (unreadable folders, filesystem metadata, snapshots): `disk used: 424.4G, 108.6G outside the
  scan`. Shown for drive roots on Windows and for mount points scanned with `-x` on Unix, not in
  apparent-size mode. `--benchmark` prints the volume's used space in its header.
- Elevated on Windows, a whole-volume scan shows NTFS's own files at the root under their real
  names — `$MFT`, `$LogFile`, `$Bitmap`, `$Secure`, `$Extend\$UsnJrnl` and the rest — sized from
  their MFT records. On one `C:\` they held 2.9 GiB, `$MFT` alone 2.7 GiB. They cannot be deleted
  from the app.
- Elevated on Windows, the scan turns on the backup privilege, so it reads folders whose permissions
  refuse even administrators: `System Volume Information`, other users' profiles, `WindowsApps`.

- `--bench-stage tree-only` times the folder tree with the walk taken out of the measurement, so
  the model's cost can be read directly instead of inferred from `walk` against `tree`.

### Changed

- Counts in the UI are grouped by thousands: `11,341,063 files`, `failed to read 1,296 files`,
  `(+12,345 descendants)`, `Delete folder with 1,024 children?`, and the Windows GUI's entry
  counts and rate. Sizes are unchanged.
- macOS scans with one tree builder and at most six workers, down from four and eight. The macOS
  walk is bound by metadata reads, so extra builders only added work after it, and workers past
  six spent their time contending in the kernel. A whole-disk scan of `/` went from ~41.8s to
  ~39.7s with 37% less system CPU. See `docs/scan-performance.md`.
- Hard-link accounting no longer slows down on files linked from many folders. The ledger was
  quadratic in a file's link folders, and macOS has files linked from thousands of them (the
  system volume's shared `_CodeSignature/CodeResources`, iOS simulator runtimes): on `/` that
  cost 1.16s, now 0.18s. Sizes are unchanged.

- The benchmark's hard-linked count now counts files seen under more than one name within the
  scan. A file whose other names lie outside the scanned folder is no longer counted. Sizes are
  unchanged.

- The folder tree is built on four threads while the scan runs. Each owns a private tree for the
  directories sent to it, the trees are merged once at the end, and hard links and reflinks are
  charged in one pass over the result. On a 4.2M-entry XFS volume the build runs at the traversal
  floor — 0.45s against 0.62s in the benchmark — and the app, launch to first full view, goes from
  about 0.67s to 0.47s with peak memory down from 603 MB to 581 MB. During the scan the treemap
  shows every folder to six levels down with a running size and descendant count; deeper folders,
  and individual files everywhere, appear when the scan completes, and the folder being viewed is
  kept across that swap. The live total counts shared blocks in full and so reads a little high
  until then. `--bench-stage sharded` measures the build path, `pipeline` the single-threaded one
  it replaced.

- A directory's entry names travel and are stored as one packed buffer instead of an `OsString`
  each, and the folder tree takes that buffer rather than copying names out of it — so a name is
  never allocated between the kernel and the tree. Traversal peak memory halved (134 MB to 74 MB),
  a whole-volume scan went from 0.70-0.81s to 0.65-0.68s and its peak from 654-670 MB to 617 MB.
  `NamedEntry` no longer carries a name, which is a breaking change for anyone using the scan API
  directly. The macOS walker is updated but unverified — it needs a build on a Mac.

- Linux scans use a native walker (`getdents64` + `statx`) with its own worker pool instead of
  `dua-core`. A whole-volume scan of a 4.2M-entry XFS filesystem went from 2.9s to 0.7s in the
  app's load path, and the traversal alone from 2.3s to 0.5s. `dua-core`'s walk stopped scaling
  well before the kernel did — sixteen independent walker processes over the same tree reached
  13.3M entries/s while its own workers peaked at six. The scan worker cap on Linux rises from 8
  to 24 accordingly. See `docs/scan-performance.md`.

- Tree building on Linux is about twice as fast and uses half the memory: hard-link accounting
  works over interned directory ids instead of re-parsing paths, folders are boxed inside
  `FileOrFolder` so a file's map slot no longer pays for a folder, and the folder and inode maps use
  a fast non-cryptographic hasher. On a 2.2M-entry ext4 volume the app's load path went from ~5.2s
  to ~2.4s, and peak RSS from ~870 MB to ~430 MB. See `docs/scan-performance.md`.

### Known issues

- The scan is now bound by tree building rather than traversal: the walk is 0.43s of a 0.72s scan
  on a 4.2M-entry volume and is fully hidden behind the model. Filesystem-specific metadata APIs
  would buy about 3% even if they were free and available, which they are not.

- A scan of `/` double-counts any filesystem mounted in more than one place — on a machine with
  one filesystem at both `/data` and `/home` it reports 1.8 TiB against about 1 TiB held. The
  double count is not new, but `/` now finishes fast enough for anyone to see it. Use `-x` for a
  trustworthy whole-machine total.

### Fixed

- Pressing `d` on a file whose name is not UTF-8 crashed the app: the delete prompt assumed
  every path was valid UTF-8. Names are now shown lossily, with control characters replaced.
- Enter on a file no longer records a place for Esc to go back to. It opened nothing, so Esc then
  restored a stale selection in the parent folder.
- On Windows, pressing `q` opened the quit prompt and letting go of it answered the prompt, so it
  had to be held down. Windows consoles report key releases as events of their own; they are now
  dropped before any handler sees them.

- Directory recursion is filesystem-aware: `/proc`, `/sys`, cgroup, debugfs and the other
  pseudo-filesystems are no longer descended into when a scan crosses a mount point into one. They
  report no disk usage, `/proc` grows while the scan runs, and parts of it fail permanently when the
  process they describe exits. Scanning one by name still works — `diskonaut /proc` walks `/proc`.
  A scan of `/` went from not finishing in ten minutes to 2.0s. `tmpfs` is still counted, as `du`
  counts it.
- A directory whose `getdents64` failed part-way was read again forever rather than abandoned,
  which hung the scan. `/proc/<pid>/net` for an exited process returns `EINVAL` on every call and
  triggered it reliably. A signal arriving mid-read (`SIGWINCH`, i.e. resizing the terminal while
  a scan runs) is retried rather than treated as a dead directory, which would have dropped the
  rest of that directory and its subtree.
- The treemap no longer panics on a folder whose entries are bigger than the folder holding them.
  Shared blocks — hard links, and now reflinks — make a folder smaller than the sum of its
  contents, so entry shares could add up to more than the whole board (four reflinked copies of one
  file gave 4.0) and tiles were laid out off the screen, where the renderer indexes the terminal
  buffer directly and crashed. Shares are now taken against the larger of the folder and its
  contents, and a tile that does not fit is skipped rather than drawn.
- A file sharing only part of itself with another is counted in full rather than merged with it.
  Identity is the whole extent map, not the first extent, which could otherwise halve a total for
  two equal-sized files that shared nothing but their opening extent.
- Scanning no longer triggers automounts. The walk's `statx` lacked `AT_NO_AUTOMOUNT`, which
  `stat`/`lstat`/`fstatat` imply but `statx` does not, so merely stating an autofs placeholder
  mounted it — a directory of NFS home maps would have been mounted wholesale.
- Reflinks are now found on every filesystem that supports them, not only the one the scan started
  on. The probe was gated on the scan root's device, which meant it never fired inside a btrfs
  subvolume or snapshot — where sharing is the norm — nor on a second XFS volume mounted inside the
  scan. It is gated on the filesystem's type instead, so NFS, SMB and FUSE are still never opened.
- Sizes no longer count copy-on-write shared data twice. On XFS and btrfs a reflinked copy reports
  its full block usage with a link count of 1, so tools that de-duplicate on inode — including
  `du` — charge every copy in full. A scan of a `uv` package cache reported 15.1 GiB where 12.0 GiB
  is held, and a whole-volume scan overstated by 21.9 GiB. Shared extents are now charged to a
  folder once, the same rule hard links already followed. Files under 64 KiB, and filesystems that
  cannot share extents, are not probed.
- The treemap silently dropped every entry too small to draw when their combined tile rounded to
  zero cells, which happens whenever one entry (a `target/` or `.git/`, say) holds nearly all of a
  folder. The board then showed that single entry at 100% with no "small files" marker and no
  legend, as if the scan had missed the rest. The `x` marker is now always drawn for hidden
  entries, clamped to at least a few cells inside the board, and zoom (`+`) reveals them as before.
- The help line at the bottom of the screen still advertised `<BACKSPACE> - delete`, a key that
  has done nothing since keybinds became configurable with `d` as the default. The help line is now
  built from the configured keybinds, so it always names the keys that actually work. To keep
  Backspace, set `delete = "backspace"` in `~/.config/diskonaut/config.toml`.

## [0.13.0] - 2026-09-04

### Changed

- Directory walk: **`jwalk` → `dua-core`**.
- `ratatui`'s `crossterm` feature is used instead of a direct `crossterm` dependency.
- `README.md` trimmed down.
- Dependency bumps: `clap` 4.6.5 → 4.6.6, `thiserror` 2.0.18 → 2.0.20, `toml` 1.1.2+spec-1.1.0 → 1.1.4+spec-1.1.0, `ratatui` 0.30.0 → 0.30.2, `actions/checkout` 6 → 7, `codecov/codecov-action` 6 → 7.

### Fixed

- TUI not rendering on initial start, caused by a race between the stdin event-reader thread and the terminal's cursor-position query during startup.

### Added

- `docs/ARCHITECTURE.md`.

## [0.12.2] - 2026-05-28

### Added

- TOML config (`version = 1`, `[base]`, `[keybinds]`) with default `~/.config/diskonaut/config.toml` and `-c` / `--config` override; see `example/config.toml`.
- `libdiskonaut` uses the repository root `README.md` on [crates.io](https://crates.io/crates/libdiskonaut).

### Changed

- Delete keybind: `Backspace` → `d`.

### Removed

- `-x` / `--disable-delete-confirmation` CLI flag; deletions always require confirmation.

## [0.12.1] - 2026-05-28

### Changed

- Fixed `Deploy` job

## [0.12.0] - 2026-05-28

### Added

- Cargo workspace with **`libdiskonaut`** (scan, model, treemap, formatting) and **`diskonaut`** (CLI + TUI).
- Unit tests colocated per module (`tests.rs` siblings) in both crates.
- GitHub Actions CI: `fmt`, `typos`, `cargo deny`, `clippy`, `test`, and `doc` workflows.
- Block-usage sizing on Unix via `rustix` / `st_blocks` (replaces the `filesize` crate).
- CLI flags unchanged in spirit: `-a` / `--apparent-size`, `-x` / `--disable-delete-confirmation`, optional scan path argument.

### Changed

- Rust **2024** edition (workspace).
- TUI stack: **`tui` → `ratatui`** (with `crossterm` 0.29).
- CLI: **`structopt` → `clap` v4** (derive).
- Errors: **`failure` → `thiserror`** at crate boundaries.
- POSIX helpers: **`nix`, `filesize` → `rustix`** (e.g. admin / root indicator).
- Directory walk: **`jwalk` 0.8**.

### Removed

- **Windows** support (`winapi`, Windows-specific OS code, and Windows CI).
- **`insta`** snapshot / integration UI tests (replaced by focused unit tests; manual TUI smoke test for UI).
- Dependencies dropped as part of the migration: `failure`, `structopt`, `nix`, `filesize`, `tui`.
