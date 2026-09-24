# Features, and where they live

diskonaut is seven packages. Two are libraries every viewer shares, one is what the desktop
viewers share, and four are viewers.

```
common/            libdiskonaut      what a viewer shows and does, with no user interface
scanners/          diskonaut-scan    reading the disk: one walker per platform, and what drives them
viewers/tui/       diskonaut-angch   the terminal viewer (ratatui) — the primary one
viewers/windows/   diskonaut-windows     a native Windows window (Win32 and GDI)
viewers/macos/     diskonaut-mac     a native macOS window (AppKit, through objc2)
viewers/linux/     diskonaut-linux   a Wayland or X11 window for Linux and FreeBSD, no toolkit (wayland-client, x11rb, fontdue)
viewers/shared/    diskonaut-viewer  what the desktop viewers share: the window's state and layout,
                                     the first scan with its live outline, the preview reader
viewers/dos/       (FASM, not Cargo) the treemap for MS-DOS in 16-bit assembly, ported from the
                                     Rust code rather than built from it
```

Dependencies run one way: `diskonaut-scan` depends on `libdiskonaut`, each viewer on both, and the
macOS and Linux viewers on `diskonaut-viewer` as well, which holds everything about the window
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
| `preview` | `read`: what a file is from its first 64 KiB — empty, binary, text (its first lines, made safe to draw), or a PNG or JPEG. `describe_picture` from the header alone, and `decode_picture` within limits (40,000 pixels a side, 1 GiB). Only regular files are read, and on macOS never one that is only in iCloud. |
| `clipboard` | `copy`: text on the native clipboard — `pbcopy`, Win32, `wl-copy`/`xclip`/`xsel` — saying whether it worked, so a viewer with another way (a terminal's OSC 52) can fall back. |
| `format` | Sizes (`DisplaySize`: B to TB), counts, truncating names to a width, and quoting a path for the shell the user is in (`quote_path_for_shell`, used for copied paths). |
| `metafiles` | NTFS's metadata file names (`$MFT`, `$LogFile`, `$Extend`…), for the walker that sizes them and for `delete`, which refuses them. |
| `os` | The platform's answers: whether the user is an administrator, a volume's used space (so a whole-volume scan can say how much it could not see), link counts and on-disk sizes. |

## `diskonaut-scan` — the scanners

| Module | What it does |
| --- | --- |
| `linux` | `getdents64` and `statx` on its own thread pool. Probes shared extents (FIEMAP) for reflinks from 64 KiB up, reads btrfs compressed sizes when root, and refuses pseudo filesystems (`/proc`, `/sys`…), network filesystems (NFS, SMB, sshfs…) and bind-mount duplicates at their mount points. |
| `macos` | `getattrlistbulk(2)`, choosing the size attribute per device because FAT misreports it. |
| `windows` | One handle per directory, entries in bulk (`FileIdExtdDirectoryInfo`). Hard links deduplicated by file id where they are made (or everywhere, elevated). Elevated, NTFS's metadata files are sized from their MFT records (`ntfs`) and shown at the volume's root. |
| fallback | `dua-core`, on every other platform. |
| `scan_directories` | Picks the walker. Every walker yields the same thing: one `DirEntries` per directory. |
| `parallel` | `build_tree`: the tree built on several threads as the walk runs, with an optional callback per directory for a live view. |
| `refine` | The second pass: small files the walk left for later, probed after the tree is on screen, the folder in view first. It finds nothing to do off Linux. |
| `rescan` | `Rescanner`: a folder (or everything) scanned again on a thread of its own, the old figures kept on screen until it is done, and a rescan another one covers never started. `Refiner`: the second pass, run the same way. |

## What each viewer offers

| | terminal (`diskonaut-angch`) | Windows (`diskonaut-windows`) | macOS (`diskonaut-mac`) | Linux (`diskonaut-linux`) |
| --- | --- | --- | --- | --- |
| Scan with the native walker | yes | yes | yes | yes |
| Live treemap while scanning | yes (`Outline`) | progress count in the title | yes (`Outline`) | yes (`Outline`) |
| Treemap | yes, in cells | yes, GDI | yes, AppKit | yes, software-drawn; native Wayland or X11 |
| List of entries beside it | yes | — | yes, with the entry's details under it (⌃⌘S hides) | yes, with the entry's details under it (`s` hides) |
| Move by arrow keys, select by click | yes | yes; hovering names a tile in the status bar | yes; hovering names an entry in the status bar | yes; hovering names an entry in the status bar |
| Enter a folder, go up | Enter or double-click / Esc | Enter or click / Backspace, right-click or ◄ Up | Return, ⌘↓ or double-click / Esc, ⌫, ⌘↑ or a breadcrumb | Enter or double-click / Esc, Backspace or a breadcrumb |
| Delete | yes, one or every marked entry | yes, one | to the Trash (⌘⌫) or immediately (⌥⌘⌫), every marked entry | to the Trash (`d`, Delete) or immediately (`D`, Shift+Delete), every marked entry |
| Refuse NTFS metadata | yes | yes | yes | yes |
| Mark several entries | yes (Shift+arrows, Ctrl+click) | — | yes (⇧ arrows or ⇧-click, ⌘-click, ⌘A) | yes (Shift+arrows or Shift+click, Ctrl+click, Ctrl+A) |
| Copy a path, shell-quoted | yes (right-click; double for absolute) | — | yes (⌘C; ⌥⌘C plain, like Finder) | yes (Ctrl+C, right-click); the window holds the selection itself when no clipboard tool is installed |
| Preview text and pictures | yes (kitty graphics, sixels or half blocks) | — | yes, in the side panel (any format macOS decodes), and Quick Look (Space) | yes, in the side panel (PNG, JPEG) |
| Show in Finder, open with the default app | — | — | yes (⌥⌘R, ⌘↓ on a file) | — |
| Context menu | — | — | yes (right-click or Control-click) | — |
| Scan a folder dropped on the window | — | — | yes | — |
| Zoom | yes | — | yes (⌘+ / ⌘- / ⌘0) | yes (`+` / `-` / `0`) |
| Disk usage / apparent size toggle | yes | — | yes (`a`, View menu) | yes (`a`) |
| Rescan a folder / everything | yes (`r` / `R`) | — | yes (⌘R / ⇧⌘R, or `r` / `R`) | yes (`r` / `R`, F5) |
| Second pass for small shared files | yes (Linux) | not needed (Windows) | not needed (macOS) | not yet (the tree is the walk's) |
| Volume used vs. what the scan found | yes, in the title | — | yes, in the status bar | yes, in the status bar |
| Configurable keys | yes (`config.toml`) | — | — | — |
| Benchmark harness | yes (`--benchmark`) | — | — | — |

The MS-DOS viewer (`viewers/dos/`, 16-bit assembly for a 286 with no coprocessor) shares no
code, so it is not in the table. It has the scan (DOS
find calls, long names where there are), the live treemap, arrow-key and mouse selection, Enter/Esc,
deleting one entry, zoom, the disk usage / apparent size toggle, rescans (`r` / `R`), the list
beside the treemap, and previews of text, PNG and JPEG in half blocks; see its README.

A gap in a GUI column is a missing viewer feature, not a missing library one: everything in it
apart from drawing and input is already in the two libraries.
The macOS and Linux columns agree wherever the shared state decides: the same keys move the same
entry, and a change to `diskonaut-viewer` reaches both.

## What stays in a viewer

Anything about the screen or the input: layout, colours, fonts, key bindings, how a picture is
encoded for display (kitty graphics, sixels and half blocks are the terminal's own; the terminal
also detects which it has), the terminal's OSC 52 clipboard fallback, and when to decode a
picture (the terminal waits until the selection has rested on it for 100 ms).
