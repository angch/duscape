# Features, and where they live

diskonaut is seven packages. Two are libraries every viewer shares, one is what the desktop
viewers share, and four are viewers.

```
common/            libdiskonaut      what a viewer shows and does, with no user interface
scanners/          diskonaut-scan    reading the disk: one walker per platform, and what drives them
viewers/tui/       diskonaut-angch   the terminal viewer (ratatui) — the primary one
viewers/windows/   diskonaut-windows a native Windows window (Win32 and GDI)
viewers/macos/     diskonaut-mac     a native macOS window (AppKit, through objc2)
viewers/linux/     diskonaut-linux   a Wayland or X11 window for Linux and FreeBSD, no toolkit (wayland-client, x11rb, fontdue)
viewers/shared/    diskonaut-viewer  what the desktop viewers share: the window's state and layout,
                                     the first scan with its live outline, the preview reader
viewers/dos/       (FASM, not Cargo) the treemap for MS-DOS in 16-bit assembly, ported from the
                                     Rust code rather than built from it
```

Dependencies run one way: `diskonaut-scan` depends on `libdiskonaut`, each viewer on both, and the
desktop viewers (Windows, macOS, Linux) on `diskonaut-viewer` as well, which holds everything about the window
that is not drawing or input (it depends on `diskonaut-scan` for rescans, so it cannot live in
`common`).
Nothing in `common` or `scanners` knows about a terminal or a window. A feature that is not about
drawing or input belongs in one of them, so that a second viewer gets it by calling it rather than
by copying it.

## `libdiskonaut` — the common package

| Module | What it gives a viewer |
| --- | --- |
| `model` | `FileTree`: the scanned folders and files, both sizes of each (on disk, and apparent length), navigation (enter, leave, current folder), deletion that corrects every ancestor, a folder rescanned and grafted back in (`graft`), and the second pass's findings folded in (`apply_found`). Sizes count each set of shared blocks once — hard links and reflinks — so a folder can be smaller than the sum of its entries. |
| `tiles` | The squarified treemap (`TreeMap`) and `Board`: tiles for the current folder, the selection and moving it by direction, zoom, and `listing` — the folder's entries largest first, the same targets as the tiles, for a list beside them. Entries too small for a tile are folded into one "small files" marker, never dropped. |
| `scan` | The protocol between a walker and the model: `ScanOptions` (apparent size, one filesystem, depth, threads, hard-link tracking), `EntryMeta` and `DirEntries` (one directory's entries as read), `Outline` and `DirSummary` (the folder-only live view shown while a scan runs), and `Found` (what the second pass found). |
| `delete` | `remove`: a file, or a folder and everything in it, from disk — a link itself, never its target. `refused`: NTFS's own metadata files, which a whole-volume scan shows and no viewer may delete. |
| `preview` | `read`: what a file is from its first 64 KiB — empty, text (its first lines, made safe to draw), a PNG or JPEG, or binary, for which `describe_binary` gives its size and, on Linux, where its blocks are: `placement::describe` — extents, sparseness, shared blocks, and the physical disk (model, SSD or HDD) and offset, through a partition; the members of an LVM or md volume; a loop device's file. `describe_picture` from the header alone, `decode_picture` within limits (40,000 pixels a side, 1 GiB), and `fit`, which scales down and never up. `Reader`: the preview thread — the latest request only, a picture decoded once the selection has rested on it 100 ms, prepared by whatever the viewer passes it. Only regular files are read, and on macOS never one that is only in iCloud. |
| `clipboard` | `copy`: text on the native clipboard — `pbcopy`, Win32, `wl-copy`/`xclip`/`xsel` — saying whether it worked, so a viewer with another way (a terminal's OSC 52) can fall back. |
| `format` | Sizes (`DisplaySize`: B to TB), counts, truncating names to a width, quoting a path for the shell the user is in (`quote_path_for_shell`), and `copied_path`: a path as a viewer copies it — relative to the working directory or absolute, `./` before a leading `-`, quoted. |
| `metafiles` | NTFS's metadata file names (`$MFT`, `$LogFile`, `$Extend`…), for the walker that sizes them and for `delete`, which refuses them. |
| `os` | The platform's answers: whether the user is an administrator, a volume's used space (so a whole-volume scan can say how much it could not see), link counts and on-disk sizes. |

## `diskonaut-scan` — the scanners

| Module | What it does |
| --- | --- |
| `ext4` | As root on ext4, the scan read from the block device instead of asked of the kernel: directory blocks and inodes swept in device order a generation at a time, every run advised before any is read; a mount point inside, or a directory of a shape it does not read, goes to the kernel walker; a filesystem it cannot follow makes it decline up front. `--no-device-read` opts out; rescans never use it. Cold it is 1.7x the kernel walk, 2.2x an unprivileged scan. |
| `linux` | `getdents64` and `statx` on its own thread pool. Probes shared extents (FIEMAP) for reflinks from 64 KiB up, reads btrfs compressed sizes when root, and refuses pseudo filesystems (`/proc`, `/sys`…), network filesystems (NFS, SMB, sshfs…) and bind-mount duplicates at their mount points. |
| `macos` | `getattrlistbulk(2)`, choosing the size attribute per device because FAT misreports it. |
| `windows` | One handle per directory, entries in bulk (`FileIdExtdDirectoryInfo`). Hard links deduplicated by file id where they are made (or everywhere, elevated). Elevated, NTFS's metadata files are sized from their MFT records (`ntfs`) and shown at the volume's root. |
| `mft` | Elevated, a whole NTFS volume read from its master file table instead of walked: the `$MFT` in a few large sequential reads, parsed on several threads into every record's names, parent and sizes, the tree handed on as the walk would hand it. Used only where a sample of the table says the volume's directories are small enough for it to pay; `--no-device-read` walks instead. The parsing runs and is tested everywhere. |
| fallback | `dua-core`, on every other platform. |
| `scan_directories` | Picks the walker. Every walker yields the same thing: one `DirEntries` per directory. |
| `parallel` | `build_tree`: the tree built on several threads as the walk runs, with an optional callback per directory for a live view. |
| `refine` | The second pass: small files the walk left for later, probed after the tree is on screen, the folder in view first. It finds nothing to do off Linux. |
| `rescan` | `Rescanner`: a folder (or everything) scanned again on a thread of its own, the old figures kept on screen until it is done. `Rescans`: the ones under way — one another covers is not started, one the new one covers is stopped, a delete inside one restarts it — and `finish`, which grafts the result into the tree. `Refiner`: the second pass, run the same way. |

## What each viewer offers

| | terminal (`diskonaut-angch`) | Windows (`diskonaut-windows`) | macOS (`diskonaut-mac`) | Linux (`diskonaut-linux`) |
| --- | --- | --- | --- | --- |
| Scan with the native walker | yes | yes | yes | yes |
| Live treemap while scanning | yes (`Outline`) | yes (`Outline`) | yes (`Outline`) | yes (`Outline`) |
| Treemap | yes, in cells | yes, GDI, nested: a folder's tile holds its entries' tiles, and theirs in turn, down to the files wherever there is room; a file's size sits at its tile's bottom right; clicking a nested tile opens the tree to it, Ctrl+click marks its folder | yes, AppKit | yes, software-drawn; native Wayland or X11 |
| List of entries beside it | yes | yes, as a tree: folders open in place (→ / ←, or the expander), their entries indented under them with each one's share of its parent — WizTree's tree view; the entry's details under it (`s` hides) | yes, with the entry's details under it (⌃⌘S hides) | yes, with the entry's details under it (`s` hides) |
| Move by arrow keys, select by click | yes | yes, and PgUp / PgDn / Home / End; hovering names the entry in the status bar | yes; hovering names an entry in the status bar | yes; hovering names an entry in the status bar |
| Enter a folder, go up | Enter or double-click / Esc | Enter or double-click / Esc, Backspace, a breadcrumb or the mouse's back button | Return, ⌘↓ or double-click / Esc, ⌫, ⌘↑ or a breadcrumb | Enter or double-click / Esc, Backspace or a breadcrumb |
| Delete | yes, one or every marked entry | yes (Del or `d`), one or every marked entry | to the Trash (⌘⌫) or immediately (⌥⌘⌫), every marked entry | to the Trash (`d`, Delete) or immediately (`D`, Shift+Delete), every marked entry |
| Refuse NTFS metadata | yes | yes | yes | yes |
| Run as administrator | from an elevated terminal | asks (the UAC prompt) when the folder is a whole volume, and starts itself again elevated; `--no-elevate` scans as it is | run as root | run as root |
| Mark several entries | yes (Shift+arrows, Ctrl+click) | yes (Shift+arrows or Shift+click, Ctrl+click, Ctrl+A) | yes (⇧ arrows or ⇧-click, ⌘-click, ⌘A) | yes (Shift+arrows or Shift+click, Ctrl+click, Ctrl+A) |
| Copy a path, shell-quoted | yes (right-click; double for absolute) | yes (Ctrl+C; Ctrl+Shift+C absolute; marking copies; the context menu) | yes (⌘C; ⌥⌘C plain, like Finder) | yes (Ctrl+C, right-click); the window holds the selection itself when no clipboard tool is installed |
| Preview text and pictures | yes (kitty graphics, sixels or half blocks) | yes, under the list (a bitmap, full colour); a binary file as a hex dump, sixteen bytes a line with the characters beside, the font shrunk to fit | yes, in the side panel (any format macOS decodes), and Quick Look (Space) | yes, in the side panel (PNG, JPEG) |
| Show in Finder, open with the default app | — | — | yes (⌥⌘R, ⌘↓ on a file) | — |
| Context menu | — | yes (right-click: open, copy, rescan, delete) | yes (right-click or Control-click) | — |
| Scan a folder dropped on the window | — | — | yes | — |
| Zoom | yes | yes (`+` / `-` / `0`, the wheel over the treemap) | yes (⌘+ / ⌘- / ⌘0) | yes (`+` / `-` / `0`) |
| Disk usage / apparent size toggle | yes | yes (`a`) | yes (`a`, View menu) | yes (`a`) |
| Rescan a folder / everything | yes (`r` / `R`) | yes (`r` / `R`, F5 / Shift+F5) | yes (⌘R / ⇧⌘R, or `r` / `R`) | yes (`r` / `R`, F5) |
| Second pass for small shared files | yes (Linux) | not needed (Windows) | not needed (macOS) | not yet (the tree is the walk's) |
| Volume used vs. what the scan found | yes, in the title | yes, under the path, with freed space and unreadable entries | yes, in the status bar | yes, in the status bar |
| Configurable keys | yes (`config.toml`) | — | — | — |
| Benchmark harness | yes (`--benchmark`) | — | — | — |

The MS-DOS viewer (`viewers/dos/`, 16-bit assembly for a 286 with no coprocessor) shares no
code, so it is not in the table. It has the scan (DOS
find calls, long names where there are), the live treemap, arrow-key and mouse selection, Enter/Esc,
deleting one entry, zoom, the disk usage / apparent size toggle, rescans (`r` / `R`), the list
beside the treemap, and previews of text, PNG and JPEG in half blocks; see its README.

A gap in a GUI column is a missing viewer feature, not a missing library one: everything in it
apart from drawing and input is already in the two libraries.
The Windows, macOS and Linux columns agree wherever the shared state decides: the same keys move
the same entry, and a change to `diskonaut-viewer` reaches all three.

The rules behind these are the terminal viewer's, kept once in `viewers/shared/src/state.rs` (no
toolkit in it, tested on every platform) on the shared pieces `Rescans`, `copied_path`,
`preview::Reader` and `delete`: a plain move, jump, click or folder change clears the marks;
Ctrl+click (⌘-click) takes in the entry already in hand only if the user picked it, never one
the viewer placed, so nothing unchosen is ever deleted; a Shift run adds its range to the marks
it started from and shrinks when reversed; a rescan another one covers is not started, and a
delete inside a folder being rescanned restarts it.

What the Windows viewer still lacks is configurable keys: the terminal viewer's `config.toml`
names terminal keys, and nothing yet maps it to a window's.

## What stays in a viewer

Anything about the screen or the input: layout, colours, fonts, key bindings, how a picture is
encoded for display (kitty graphics, sixels and half blocks are the terminal's own; the terminal
also detects which it has), the terminal's OSC 52 clipboard fallback, and when to decode a
picture (the terminal waits until the selection has rested on it for 100 ms).
