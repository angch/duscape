# diskonaut-linux

A window on diskonaut-angch for Linux and FreeBSD: the same walker (`diskonaut-scan`) and model,
treemap, delete and preview reading (`libdiskonaut`) as the terminal viewer, and the same window
state as the macOS viewer (`diskonaut-viewer`, in `viewers/shared/`), drawn with **no toolkit**,
natively on **Wayland** or on **X11**. `docs/features.md` compares it with the other viewers.

```sh
cargo run -p diskonaut-linux --release -- ~/Downloads
cargo run -p diskonaut-linux --release -- -a ~       # apparent sizes to begin with
cargo run -p diskonaut-linux --release               # the current folder
DISKONAUT_BACKEND=x11 cargo run -p diskonaut-linux    # X11 even under Wayland (else the reverse)
make static-linux-gui                                # a static binary, ~1.7 MB, for any Linux
```

## Why no toolkit

The question was GTK, Qt, or something minimal. Both toolkits need their development packages to
build and their shared libraries at run time, which rules out the fully static binaries this fork
ships and ties the binary to a distribution's versions; their Rust bindings (`gtk4-rs`, `cxx-qt`)
each bring a few hundred crates for a window whose whole job is to draw one picture and take keys
and clicks. So the viewer speaks the display protocols itself: Wayland through
[`wayland-client`](https://github.com/Smithay/wayland-rs)'s pure-Rust implementation (no
libwayland — a `wl_shm` buffer, `xdg-shell`, `xdg-decoration`, the seat, the data device) and X11
through [`x11rb`](https://github.com/psychon/x11rb) (no Xlib — `PutImage`), behind one small
`Backend` trait. It paints the frame in software and rasterises text with
[`fontdue`](https://github.com/mooman219/fontdue) from the system's fonts. Nothing is linked from
the system, the binary is static, and it runs on any compositor or X server. The price is a fixed
dark theme instead of the desktop's, no native file chooser or drag and drop, and — on a compositor
that draws no title bars, like GNOME — a title bar of the viewer's own, plain but with move,
maximise, minimise and close.

Keyboard input on Wayland is the one place a toolkit would have brought a C library (xkbcommon):
instead `src/xkb.rs` reads the keymap the compositor sends, enough of it to know which keysym each
key gives with and without Shift, and falls back to a US layout for a key it cannot read. Key
repeat is the client's job on Wayland, and is done here.

## The window

- **Breadcrumbs** across the top: click one to go back up to it.
- **The list** on the left: the folder's entries, largest first, each with its share of the
  folder as a bar. Under it, the entry in hand: its size, item count and share, and a preview of
  a file — its first lines of text, or the picture (PNG, JPEG).
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
| Open a folder | Enter or double-click |
| Go up | Esc, Backspace, or a breadcrumb |
| Mark several | Shift+↑/↓ or Shift+click (a range), Ctrl+click (one), Ctrl+A (all) |
| Move to the Trash | `d` or Delete — asks first |
| Delete immediately | `D` or Shift+Delete — asks first |
| Copy path | Ctrl+C or right-click (quoted for the shell) |
| Zoom (hide the largest entries) | `+` / `-` / `0` |
| Disk usage / apparent size | `a` |
| Rescan folder / everything | `r` / `R` (or F5) |
| Hide the side panel | `s` |
| Quit | `q`, Ctrl+Q, or close the window |
| Window (own title bar) | drag to move, double-click to maximise; its buttons minimise, maximise, close |

Marked entries are what the Trash, delete and copy act on; with none marked, the entry in hand is.
A dialog answers to its buttons, Enter/`y` and Esc/`n`.

## Worth knowing

- **The Trash** is the freedesktop one: `gio trash` when it is installed (it knows every desktop's
  rules), else `~/.local/share/Trash` for files on the home filesystem and `.Trash-<uid>` at the
  top of any other, each with its `.trashinfo`. The Trash frees nothing until it is emptied, so it
  does not add to "freed".
- **Wayland or X11** is decided by `DISKONAUT_BACKEND` (`wayland`/`x11`), else by whether
  `WAYLAND_DISPLAY` is set; if the first choice cannot connect, the other is tried.
- **The clipboard** goes through `wl-copy`, `xclip` or `xsel` if one is installed. Otherwise the
  window holds the selection itself — offered to the compositor with the last click or key's
  serial on Wayland, owned as `CLIPBOARD` on X11 — for as long as the window is open.
- **HiDPI:** everything is laid out in points. On Wayland the scale is the compositor's
  (`preferred_buffer_scale`, else the output's), and the buffer is drawn at it. On X11 it is
  `Xft.dpi` over 96 (or `GDK_SCALE`, or `DISKONAUT_SCALE=2`), to a quarter.
- **Fonts** come from `fc-match sans-serif` (and `:bold`, `monospace`); without fontconfig, the
  usual DejaVu/Liberation/Noto files. `DISKONAUT_FONT`, `DISKONAUT_FONT_BOLD` and
  `DISKONAUT_FONT_MONO` name files to use instead.
- **Pictures** are decoded on the previewer's thread and shrunk to 1024 pixels a side once, so
  drawing them costs little; the limits are 40,000 pixels a side and 512 MiB decoded.

## Code

| File | |
| --- | --- |
| `../shared/src/state.rs` | `Viewer` (`diskonaut-viewer`): what the window shows and how it answers input, shared with the macOS viewer and tested on every platform |
| `src/app.rs` | The loop over one channel — X events, scan batches, the finished tree, rescans, previews, ticks — each a call on the `Viewer`, then one frame; keys, mouse, the confirm dialog, copy, Trash and delete |
| `src/backend.rs` | The `Backend` trait and `Input`: what either windowing system gives and takes, in points; which one to open |
| `src/wayland.rs` | Native Wayland on `wayland-client`: `wl_shm` buffers, `xdg-shell`, decorations, seat (keyboard with repeat, pointer with cursor and wheel), outputs and scale, the clipboard; events dispatched on a thread |
| `src/xkb.rs` | The compositor's xkb keymap read into keysyms, with a US fallback |
| `src/x11.rs` | `X11` on `x11rb`: window, properties, events on a thread, `PutImage`, the keyboard mapping, the clipboard |
| `src/canvas.rs` | The software framebuffer in points: fills, gradients, strokes, rounded rectangles, pictures |
| `src/font.rs` | Finding the system's fonts, `fontdue` glyphs cached, text drawn aligned and cut with "…" |
| `src/draw.rs` | Painting the frame by `state::Layout`, and the dialog |
| `src/trash.rs` | The freedesktop Trash |

To look at the drawing without a screen:

```sh
DISKONAUT_SNAPSHOT=out.png cargo run -p diskonaut-linux --release -- FOLDER
```

writes the window to `out.png` half a second after the scan finishes, and quits. Without a desktop:

- **X11:** an `Xvfb` will do — `x11rb` connects over TCP (`Xvfb :99 -listen tcp -ac`,
  `DISPLAY=localhost:99.0`), since it does not use abstract sockets; `xdotool` then drives keys and
  clicks, and `import -window root` grabs the screen.
- **Wayland:** a headless weston — `weston --backend=headless-backend.so --renderer=pixman
  --shell=kiosk-shell.so --socket=test --debug` (it runs from an unpacked .deb with
  `WESTON_MODULE_MAP` naming the modules) — then `WAYLAND_DISPLAY=test diskonaut-linux` and
  `weston-screenshooter` for the screen. It has no input devices to drive; the keymap reading is
  covered by `xkb`'s tests against a real `xkbcomp` dump.

That is how this viewer was checked; [`TODO.md`](../../TODO.md) lists what still wants a real
desktop.
