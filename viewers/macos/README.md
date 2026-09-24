# diskonaut-mac

A native macOS window on diskonaut-angch: the same walker (`diskonaut-scan`) and model, treemap,
delete and preview reading (`libdiskonaut`) as the terminal viewer, drawn with AppKit through
[`objc2`](https://github.com/madsmtm/objc2). `docs/features.md` compares it with the other viewers.

```sh
cargo run -p diskonaut-mac --release -- ~/Downloads
cargo run -p diskonaut-mac --release -- -a ~       # apparent sizes to begin with
cargo run -p diskonaut-mac --release               # asks for a folder; or drop one on the window
```

The binary is unbundled: run from a terminal, it takes the terminal's permissions (Full Disk
Access, if the terminal has it). macOS asks separately for Desktop, Documents and Downloads the
first time a scan reaches them.

## The window

- **Breadcrumbs** across the top: click one to go back up to it.
- **The list** on the left: the folder's entries, largest first, each with its share of the
  folder as a bar. Under it, the entry in hand: its size, item count and share, and a preview of
  a file — its first lines of text, or the picture (any format macOS decodes).
- **The treemap** on the right: folders in blues, files coloured by extension, so files of a kind
  look alike from folder to folder. Entries too small for a tile are folded into "small files",
  and are all in the list.
- **The status bar**: what the pointer is on (or what was just done), and on the right what the
  scan found: entries, time, unreadable folders, space freed, rescans under way.

The treemap fills in while the scan runs, from an outline of the folders; files appear when it
finishes. Deleting waits until then.

## Keys and mouse

| | |
| --- | --- |
| Move | arrows (in the list ↑↓, in the treemap by direction); Tab switches panel |
| Jump through the list | Page Up / Page Down / Home / End |
| Open a folder | Return, ⌘↓ or double-click |
| Go up | Esc, ⌫, ⌘↑, or a breadcrumb |
| Mark several | ⇧↑/⇧↓ or ⇧-click (a range), ⌘-click (one), ⌘A (all) |
| Quick Look | Space, ⌘Y, or double-click a file |
| Move to the Trash | ⌘⌫ (or `d`, Forward Delete) — asks first |
| Delete immediately | ⌥⌘⌫ — asks first |
| Copy path | ⌘C (quoted for the shell); ⌥⌘C (plain, one per line, like Finder) |
| Show in Finder | ⌥⌘R |
| Open with the default app | ⌘↓ on a file |
| Zoom (hide the largest entries) | ⌘+ / ⌘- / ⌘0, or `+` `-` `0` |
| Disk usage / apparent size | `a`, or View ▸ Show Apparent Sizes |
| Rescan folder / everything | ⌘R / ⇧⌘R, or `r` / `R` |
| Hide the side panel | ⌃⌘S |
| Scan another folder | ⌘O, or drop it on the window |
| Context menu | right-click or Control-click an entry |

Marked entries are what the Trash, delete, copy, Quick Look and Show in Finder act on; with none
marked, the entry in hand is.

## Worth knowing

- **The Trash frees nothing** until it is emptied, so moving to the Trash does not add to "freed".
  The entry leaves the folder it was in; if the scan covers the Trash itself (scanning `~`), it
  shows up there only after a rescan.
- **Pictures** are read on a thread of their own and decoded by the system when drawn. Files over
  64 MiB, or with more pixels than 1 GiB would hold, are described instead.

## Code

| File | |
| --- | --- |
| `src/state.rs` | `Viewer`: what the window shows and how it answers input, with no AppKit. Layout in points, the entry in hand kept by name, marks, navigation, zoom, deletes, rescans. Its tests run on every platform |
| `src/scan.rs` | The first scan on its own thread, with the live outline |
| `src/preview.rs` | The preview reader: the latest request wins |
| `src/mac/view.rs` | The one `NSView`: events, menu commands, dialogs, the Trash, the pasteboard, Finder, Quick Look, drag and drop |
| `src/mac/draw.rs` | Painting the view, by `state::Layout` |
| `src/mac/mod.rs` | The application, its delegate, menus and window |

Other threads come back to the main thread through `view::on_main` (the main dispatch queue). The
`Viewer` lives in a `RefCell`; a modal (an alert, the open panel) runs the event loop inside the
call that opens it, so no borrow is held across one.

## Testing

`tests/smoke.sh` builds the app, scans a fixture, and drives it with a script of keys, clicks and
menu choices, checking what it holds after each step — navigation, marks, the clipboard, the
context menu, the side panel, a delete through its confirmation alert, Quick Look. It needs a
logged-in session and no permissions; the window comes to the front for the ten seconds or so it
runs.

```sh
viewers/macos/tests/smoke.sh
```

The script language is in `src/mac/script.rs`: `DISKONAUT_MAC_SCRIPT=steps.txt diskonaut-mac
FOLDER` runs `steps.txt` once the scan is done. Its events are made by the app and posted to its
own queue, so they take a real key's or click's path — menu key equivalents, the first responder,
hit testing, the event loops of alerts and menus — without the Accessibility permission that
posting to the system (`CGEventPost`, AppleScript's System Events, XCUITest) would need. A
`state FILE` step writes what the viewer holds as `name: value` lines, which the test compares.

To look at the drawing without screen access:

```sh
DISKONAUT_MAC_SNAPSHOT=out.png cargo run -p diskonaut-mac -- FOLDER
```

writes the window to `out.png` half a second after the scan finishes, and quits.

What has not yet been tried by hand, with how to check each, is in [`TODO.md`](../../TODO.md).
