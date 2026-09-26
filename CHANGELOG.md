# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The Windows viewer does what the terminal viewer does: the list beside the treemap (Tab
  switches, `s` hides it), the live treemap while scanning, breadcrumbs, marks (Ctrl+click,
  Shift+↑↓, Shift+click, Ctrl+A) that copy their paths, copying paths (Ctrl+C, Ctrl+Shift+C),
  deleting several entries at once, a preview of text and pictures, zoom, the disk-usage/apparent-
  size switch, rescans (`r`/`R`, F5), a right-click menu, double-click to open, the volume and
  freed-space summary, and the terminal viewer's scan flags. It is built on `diskonaut-viewer`,
  the state the macOS and Linux windows share, so the three behave alike; only GDI and Win32
  input are its own.
- `diskonaut-viewer` follows the terminal viewer's rules, so every desktop viewer does: Ctrl+click
  (⌘-click) takes in the entry already in hand only if the user picked it, never one the viewer
  placed, so nothing unchosen is deleted; a Shift run adds its range to the marks there were and
  shrinks when reversed; every change to the marks copies their paths once the viewer has given it
  a clipboard (`set_clipboard`, `copy_paths`); `delete` removes entries from disk going on past
  failures and names the first, with `delete_prompt` the question to ask first; its rescans are
  `diskonaut_scan::rescan::Rescans`'s, as the terminal viewer's are; a status message shows over
  the marks' summary while it lasts; the status bar says the zoom; `wanted_preview_sized` asks for
  a preview again when the pixels it will take change.
- The Windows viewer's list is a tree, as WizTree's: → opens the folder in hand in place, its
  entries indented under it with each one's share of its parent, to any depth; → again goes down
  into it, ← goes up to the folder and then closes it, the expander before a folder's name does
  the same with the mouse. A nested entry is previewed, copied, entered or deleted where it is,
  and the treemap follows its top-level folder. The rows come from `libdiskonaut::tiles::
  tree_rows` and the behaviour is the shared viewer's (`tree_view`), so the macOS and Linux
  windows can have it by drawing the rows' depth.
- The Windows viewer previews a binary file as a hex dump: sixteen bytes a line, spaced, a dash
  after the eighth, the characters beside (`.` for one not printable), the monospace font shrunk
  until a whole line fits the panel. `libdiskonaut::preview::hex_dump` makes the lines, so any
  viewer can show them.
- The Windows viewer asks to run as administrator (the UAC prompt) when the folder is a whole
  volume, and starts itself again elevated with the same arguments: elevated, the volume is
  read from its master file table, every hard link is counted, NTFS's own files are sized and
  every folder opens. Declined, it scans as it is; `--no-elevate` never asks.
- The Windows viewer's treemap is nested: a folder's tile holds its entries' tiles, laid out
  under its label, and theirs in turn, as deep as there is room (each level a shade darker,
  labelled where it fits — the name at the left, the size at the right, as every tile is
  now). Hovering a nested tile names it in the status bar; clicking it
  opens the folders above it in the list and puts its row in hand, so the two panels show the
  same thing. The layout is `libdiskonaut::tiles::nest`, the hit-testing the shared viewer's.
- `make` on Windows without make: `.\make <target>` (`make.cmd`, `make.ps1`) reads the
  Makefile and runs its recipes in Git's bash — targets, prerequisites, variables, `$(shell)`,
  continuations, `VAR=value` — so `make quality`, `make test` and the rest are one recipe
  everywhere.
- Quality measurements: `cargo clippy` now holds every crate to `clippy.toml`'s limits (a
  function of at most 100 lines and cognitive complexity 25), on every target; a function over
  them is split or carries `#[allow(clippy::too_many_lines)]` with its reason, the debt register
  `make quality` prints along with lines, tests, `unsafe` blocks and `SAFETY` comments per
  crate. `make coverage` measures test coverage with `cargo llvm-cov`. The terminal viewer's
  frame and thread start-up were split by that measure; every `unsafe` block has its SAFETY
  comment. The test suite passes on Windows: paths, quoting, sparse files and symlink fixtures
  are platform-correct rather than Unix-shaped (`os::set_sparse` makes a hole on NTFS).
- Elevated on Windows, a whole-volume scan reads NTFS's master file table instead of walking
  the directories — the volume flushed, then one sequential read of the `$MFT`, parsed on a
  few threads, the tree handed on from it exactly as the walk would (the roadmap's step 2 for
  Windows, and what WizTree does). On a 2.46M-entry `C:\` that is 3.7 s against the walk's
  6.1 s warm and 3.0 s against 9.1 s cold, and against WizTree's 7.1 s; entries and hard links
  come out the same as the walk's, and the table sees the folders the walk is refused. Hard
  links come counted from the records, so only files with several names go through the ledger.
  It is used only for a volume root, and only where a sample of the table says the volume's
  directories are small enough for it to pay (the table costs every record on the volume, the
  walk a handle per directory: a subtree, or a data volume of big files, walks faster).
  `--no-device-read` walks instead, and a rescan (`r`) always does. The benchmark's header
  says which ran (`device read: yes (NTFS master file table, elevated)`), and the Windows
  matrix has a `kernel walk` row beside it when elevated.
- `docs/probes/bench-matrix.ps1`: the cross-machine benchmark matrix on Windows, in the same
  file format as the Linux script — diskonaut against `diskus` and WizTree's export mode, with
  WizTree's own figures beside the trees for the sizes cross-check. From an elevated shell it
  runs cold rows too, the file cache emptied before each by `drop-cache.ps1` (the system file
  cache trimmed and the standby list purged through the API, as RAMMap does).
- Shared by the viewers now, rather than written into the terminal one: rescan bookkeeping
  (`diskonaut_scan::rescan::Rescans`), how a copied path is made (`format::copied_path`) and the
  debounced preview thread (`preview::Reader`).
- The MS-DOS viewer, from a review of its 286 port: deleting or rescanning a folder whose path is
  too long for DOS is refused (the check was lost to a flag overwritten before it was tested),
  and a rescan no longer takes a folder out of the tree before knowing it can be read, so one
  DOS cannot open neither vanishes nor counts as freed; colour averages round to nearest; a 16-bit
  JPEG quantiser above 32767 is clamped; the disk line shows "0 not scanned" instead of nothing
  when the drive reports less than was found; invalid UTF-8 is read exactly as Rust's lossy
  decoding reads it; and PNG transparency (tRNS) works for gray and RGB as well as palettes.
- The MS-DOS viewer runs on a 286 (or an 80186) with no coprocessor: 16-bit registers only, no
  FS/GS, conditional jumps a 286 can take (`J286.INC`), and the treemap in IEEE doubles computed in
  software (`SOFTFP.ASM`), so it still lays out exactly as Rust's f64 code does. Checked by a lint
  for 386 instructions, 150,000 random soft-float operations against the host's doubles, and the
  whole test suite on an emulated 286 without an FPU. With it, fixes from a review: a tiny JPEG
  could hang the decoder for hours (a scan now ends at its end marker, and keys are read each
  row); JPEG Huffman tables are checked before use; sniff read only every other byte; long names
  lost their ends in the list; small JPEGs came out blank; the list followed the treemap's
  selection only when the list had the keyboard; the cursor went to the top after a delete; a
  file whose name DOS converted could not be previewed; the disk line and the share of the scan
  differed from the terminal viewer's; restart markers could swallow the next marker; 4-byte
  UTF-8 showed the wrong character; a 64 KiB head with no newline previewed blank.
- The MS-DOS viewer's side panel and previews: the folder's details and its entries beside the
  treemap, with the TUI's focus rules (the list has the keyboard, Tab and the arrows cross over,
  Page Up/Down, Home/End), and under them the entry in hand previewed — text in CP437, or a PNG
  or JPEG in half blocks, decoded in assembly (inflate, every PNG colour type and Adam7; JPEG
  from its DC coefficients, progressive too) with six of the 16 text colours set to the
  picture's own. `s` hides the panel. The tests compare every preview with ImageMagick's decode.
- diskonaut for MS-DOS (`viewers/dos/`): the treemap as a 16-bit real-mode program in FASM
  assembly, about 12 KB, for a 386 with a 387 or DOSBox-X. Ported from the Rust code — the
  squarify layout, rounding, navigation, zoom, tile text and size formats — with tiles identical to
  `libdiskonaut`'s on 100 random folders. It scans with DOS's find calls (long names where DOS has
  them), draws the treemap live during the scan, deletes files and whole folders, toggles apparent
  size, rescans a folder or everything (`r`/`R`), and takes the mouse. What does not fit in
  conventional memory is shown as one "(not in memory)" entry per folder, so totals stay right; a
  folder that will not delete completely is walked again. `make dos` assembles it inside DOSBox-X
  with FASM and CWSDPMI fetched into `target/dos/`; `make dos-run` opens this repository;
  `viewers/dos/test.py` checks it headless on fixtures.
- A Linux viewer, `diskonaut-linux` (`viewers/linux/`), with no toolkit: the frame is drawn in
  software and put on the screen natively on Wayland (`wayland-client`'s pure-Rust protocol:
  `wl_shm`, `xdg-shell`, `xdg-decoration`, the seat with the xkb keymap read by the viewer, the
  data device) or on X11 (`x11rb`), text comes from the system's fonts through `fontdue`, and
  nothing is linked from the system, so it builds static for musl (about 1.7 MB) and runs on any
  compositor or X server. Where the compositor draws no title bar, the window draws its own. The
  same window as the macOS viewer's —
  breadcrumbs, the list beside the treemap with the entry in hand previewed under it, live while
  scanning — with the Trash (freedesktop; `gio trash` when installed), immediate deletion, marks,
  copying paths (the window owns the clipboard itself when no tool is installed), zoom, apparent
  sizes, rescans, and HiDPI by `Xft.dpi`. `DISKONAUT_SNAPSHOT=out.png` writes the frame after the
  scan, for looking at the drawing without a screen.
- `diskonaut-viewer` (`viewers/shared/`): the desktop viewers' shared state — the window's layout
  in points, the entry in hand, marks, navigation, zoom, rescans, what a delete changes, the first
  scan with its live outline, and the preview reader — moved out of the macOS viewer so the Linux
  one behaves the same and both are tested once.
- A native macOS viewer, `diskonaut-mac` (`viewers/macos/`), on AppKit through `objc2`: the
  treemap with the list beside it, live while scanning; previews in the side panel and Quick
  Look; the Trash or immediate deletion of every marked entry; copying paths, Show in Finder, a
  context menu, breadcrumbs, zoom, apparent sizes, rescans, and folders dropped on the window.
  Its state is kept free of AppKit and tested on every platform, and `tests/smoke.sh` drives the
  real window with synthetic keys and clicks (`DISKONAUT_MAC_SCRIPT`) and checks what it holds.

### Fixed

- The Windows viewer answers while a volume is scanned elevated. The table read hands the tree
  on in a burst — dozens of outline batches a second — and the window laid the view out for
  each (15–25 ms: the listing sorted, the tree rows and the nested tiles rebuilt), so its
  thread was taken up until the scan ended and it painted, moved and closed nothing meanwhile.
  Batches now go into the tree as they come (`Viewer::absorb_summaries`) and the view is laid
  out once, 100 ms after the first of a burst (`Viewer::catch_up`).
- The workspace builds on Windows and macOS again: the Linux viewer's Wayland and X11 crates are
  Unix-only, so they are now dependencies of Linux and FreeBSD targets alone, and elsewhere
  `diskonaut-linux` is the stub it already was.
- macOS and Linux viewers: pictures the system decodes but `libdiskonaut` does not (HEIC, GIF,
  WebP, TIFF, BMP, AVIF) are shown again. Since a binary file's preview became a description of
  where its blocks are, `Previewer` was looking for the old `binary file` line and never found it.
- macOS viewer: ⌘⌫ (Move to Trash) and ⌥⌘⌫ (Delete Immediately) did nothing; their key
  equivalent was Backspace (`U+0008`), not the Delete character the ⌫ key types.
- Pictures in the preview are drawn as sixels in terminals without kitty graphics that have
  them, before falling back to half blocks. Sixel support is attribute 4 of the device
  attributes the terminal is already asked for (tmux answers for itself), with the cell size
  asked alongside (`CSI 16 t`) for when the system reports none; on Windows, where the terminal
  is not asked, Windows Terminal is taken to have them. `DISKONAUT_GRAPHICS=sixel` forces it.

### Changed

- The Windows viewer is `diskonaut-windows` (crate and binary), not `diskonaut-gui`: there are GUI
  viewers for three platforms now, and each is named for its own.
- The workspace is split by role: `common/` (`libdiskonaut`: model, treemap, scan protocol,
  deletion, preview reading, native clipboard), `scanners/` (the new `diskonaut-scan`: every
  walker, the parallel build, the second pass, rescans), and `viewers/tui/` (`diskonaut-angch`) and
  `viewers/windows/` (`diskonaut-windows`). Crate and binary names are unchanged; code that used
  `libdiskonaut::scan::parallel`, `scan_directories` or `scan_into_tree` now takes them from
  `diskonaut_scan`. `docs/features.md` describes every feature, where it lives, and which viewer
  offers it.
- The Windows walk uses two thirds of the cores, at most 12, instead of a fixed 8: `C:\` took
  5.9s instead of 6.3s on a 32-thread machine (`D:\` 0.18s instead of 0.23s), and a 12-thread
  one keeps its 8.
- Forked as **`diskonaut-angch`** and restarted versioning at `0.1.0`. This fork diverged
  substantially from upstream diskonaut `0.13.0` (native per-platform walkers, parallel tree build,
  Windows support) and now versions independently; `repository` and `homepage` point at the fork.
  The binary is still named `diskonaut`, so the command and docs are unchanged. Entries below this
  line predate the rename.

### Fixed

- Pictures smaller than the preview were scaled up to fill it, blurred, though the code said they
  were not: `DynamicImage::thumbnail` enlarges too. Both viewers now use `preview::fit`.
- The Windows GUI's Delete removed the entry from the treemap but never from disk, though its
  dialog said it would. It now deletes through the same code as the terminal viewer, and like it
  refuses NTFS's metadata files.
- Deleting a junction or directory symbolic link on Windows failed: it is not a directory to
  `symlink_metadata`, so it went to `remove_file`, which Windows refuses for a directory link. It
  is now removed with `remove_dir`, which takes the link and leaves its target alone.
- btrfs snapshots were counted once per snapshot. Every subvolume and snapshot has its own
  `st_dev`, which was folded into the identity of shared extents, so a snapshot's files never
  matched the live ones they share. On btrfs the identity now uses the filesystem's UUID
  (`BTRFS_IOC_FS_INFO`, no privileges needed).
- btrfs compression was invisible: `stat` reports the uncompressed size, so 64 MiB of text that
  holds 2 MiB read as 64 MiB. Run as root, the walk now reads each file's extent items
  (`BTRFS_IOC_TREE_SEARCH_V2`, on the directory, no file opened) and counts what they occupy on
  disk: compressed extents at their stored size (a partly referenced one pro rata), holes as
  nothing, inline data as its bytes. About a microsecond a file, so only on btrfs mounted with
  `compress`/`compress-force`, or for files marked compressed (`chattr +c`). As a user, sizes stay
  uncompressed: the search needs `CAP_SYS_ADMIN`.
- Compressed files over 8 MiB on btrfs, and any file of more than 64 extents on XFS or btrfs, were
  never recognised as shared, so each snapshot or reflink copy of one counted again: the FIEMAP
  probe read one page of 64 extents and gave up on longer maps. It now reads the whole map (to
  256Ki extents).
- Small files in btrfs snapshots, and small reflink copies on XFS and btrfs, were counted once per
  copy: the walk only checks files of 64 KiB and up for shared blocks, to stay fast. It now notes
  the smaller ones (4 KiB and up, not hard-linked) and checks them in a second pass after the
  treemap is up, the folder being looked at first; the title says "refining" while it runs and
  sizes settle as it goes. A folder rescan runs its own second pass before it is shown.
- A folder rescan (`r`) walks under the same rules as the whole scan: a folder the scan left
  empty — `/proc`, a network mount, another filesystem under `-x`, a bind mount it reaches anyway,
  a folder past `--max-depth` — is not rescanned into life. Deleting something inside a folder
  that is being rescanned starts the rescan again, so the deleted entry cannot come back. After
  `R`, "outside the scan" no longer counts space freed before the rescan a second time, and if
  the folder shown has gone, the marks and zoom history of it go too.
- A bind mount of a folder inside the scan was counted twice, `-x` or not: it has the same device
  as the folder, so nothing marked a boundary. At a mount root the walk now reads the mount table
  and leaves the mount empty when an earlier mount shows the same directory inside the scan (the
  same directory by device and inode, so a source since hidden under another mount is kept). A
  second mount of the same filesystem is handled the same way. Scanning a bind mount on its own is
  unaffected.

### Changed

- Scans no longer walk into network filesystems: NFS, SMB/CIFS, 9p, Ceph, AFS, Lustre, GPFS and
  others by `statfs` magic, and remote FUSE filesystems (sshfs, rclone, s3fs, gcsfuse, GVfs …) by
  their subtype in the mount table, so local FUSE filesystems such as ntfs-3g are still counted.
  Another machine's files are not where this disk's space went, and a slow or dead server no
  longer slows or hangs the scan. Named as the scan root, a network mount is still scanned. On
  macOS, a mount point without `MNT_LOCAL` is skipped the same way.

### Added

- `a` switches between size on disk and apparent size without scanning again (new `toggle-size`
  keybind); `-a` now only chooses which is shown first. The tree keeps both sizes: a file holds its
  size on disk and its length as a 32-bit difference from it, in the padding its 16-byte slot
  already had, so the tree is no larger per file; the rare file whose two sizes are 2 GiB or more
  apart (a huge sparse file) keeps both in a box of its own, so both are exact. Folders hold both
  totals. Every walker now reads both sizes; on macOS the bulk listing asks for the allocation
  and the data length together, the data length standing in for the allocation on FAT as before.
- `make test-fs` / `fixtures/fs/`: ext4, XFS, btrfs, f2fs, tmpfs, FAT32, exFAT and NTFS on loopback
  images, with hard links, sparse files, reflinks, snapshots, compression, mount layouts and
  network mounts (loopback NFS, `fuse.rclone`), checked
  against independent oracles and run in CI. Needs root or the docker group.
- `r` rescans the selected folder (the one shown when a file is selected) and `R` everything,
  in the background, with the old view browsable until the result is grafted in; ancestors'
  sizes and counts are corrected by the difference. New `rescan` and `rescan-all` keybinds. A
  character bound without Shift now also matches with it, since terminals report `R` with Shift.
- The help line moves on by itself: the key legend, then a tip (Ctrl+click, right-click copy,
  rescans…), then the legend again. Each rests at least five seconds, restarted by any key press,
  then slides to the next in 80 ms, eased, at 60 frames a second. A legend wider than the terminal
  is shown a page at a time, which replaces the abbreviated legend narrow terminals used to get.
- A preview below the list, 16:9 and at most half the panel: the first lines of a text file, or
  a PNG or JPEG (detected by magic bytes) drawn with the kitty graphics protocol after a 100 ms
  debounce, scaled to fit. Support is read from the environment, or else asked of the terminal
  (a graphics query then a device attributes request), so it is found over ssh too. Terminals
  without it, and tmux, get the picture in half blocks (`▀`, two pixels to a cell) in 24-bit
  colour when `COLORTERM` says so, else the xterm 256-colour palette.
  `DISKONAUT_GRAPHICS=kitty|blocks|none` overrides the detection; `none` describes pictures. Files are read on a thread of their
  own; only regular files are opened, and iCloud-only files on macOS are left undownloaded.
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
