# TODO

Work that needs a machine this repository's CI does not have. Each item says how to check it and
what should happen; tick it off (or fix what it finds) and say so in the commit.

## Shared viewer (`duscape-viewer`): needs a macOS or Linux machine

- [ ] **One preview thread.** `viewers/shared/src/preview.rs` (`Previewer`, 60 ms debounce on
      every file, pictures handed over as bytes for the system decoder) and
      `libduscape::preview::Reader` (latest request only, 100 ms debounce on pictures alone,
      the picture prepared by a closure — the terminal and Windows viewers) do the same job twice.
      Port `Previewer` onto `Reader` with a `picture` closure that reads the bytes, keeping the
      by-extension formats (`OTHER_PICTURES`) the system decodes, then delete its loop. Check on a
      real window that HEIC and GIF still preview and that holding ↓ through a folder of photos
      decodes none until the key is released.

## macOS viewer (`duscape-mac`): untried by hand

`viewers/macos/` was written and checked without screen or input access. The platform-independent
state (`src/state.rs`) has unit tests, and the drawing was checked with
`DUSCAPE_MAC_SNAPSHOT=out.png` (see `viewers/macos/README.md`), which renders the view after a
scan. **Nothing below has been done with a real mouse and keyboard.** Run
`cargo run -p duscape-mac -- <folder>` from a terminal and go through it. The code for each is in
`src/mac/view.rs` unless said otherwise.

**Automated since:** `viewers/macos/tests/smoke.sh` drives the real app with synthetic keys and
clicks (`DUSCAPE_MAC_SCRIPT`, `src/mac/script.rs`) and checks its state after each. It needs a
logged-in session but no permissions. Items marked *(smoke)* pass there. Synthetic events are the
app's own, so a hand check is still worth doing once, mainly for what the keyboard really sends.
To automate another item, add steps and `expect` lines to `smoke.sh`.

### Found by the smoke test

- [ ] **Menu commands are disabled while Quick Look has the keyboard.** The view's commands (⌘C,
      ⌘⌫, ⌃⌘S, rescans…) go along the key window's responder chain, and the Quick Look panel's
      chain does not include the view. Finder allows ⌘⌫ with Quick Look open. Targeting those menu
      items at the view would fix it but would break ⌘C in the open panel's text fields; route
      them from the application delegate instead (it ends every chain), forwarding to the view.
- [x] ⌘⌫ and ⌥⌘⌫ never fired: their key equivalent was `U+0008`, and the ⌫ key types `U+007F`.
      Now `U+007F`, and ⌥⌘⌫ opens its alert in the smoke test. *Confirm with a real keyboard.*

### Start-up and the window

- [ ] **No argument:** the open panel appears at launch (`applicationDidFinishLaunching` →
      `scanFolder:`). Cancel it: the window stays, showing "Drop a folder here…". ⌘O opens the panel
      again.
- [ ] **A file or a missing path as the argument:** an alert says it is not a folder, and the
      window stays empty.
- [ ] **Unknown flag** (`--foo`): prints an error and exits with status 2 before any window opens.
- [ ] **Activation:** started from a terminal, the app comes to the front with its menu bar and a
      Dock icon, and the view takes keys straight away (no click needed first).
- [ ] **Window size is remembered** between runs (`setFrameAutosaveName`), and the minimum size
      (520×340) holds.
- [ ] **Closing the window quits** the app (`applicationShouldTerminateAfterLastWindowClosed`).
- [ ] **Title bar:** the folder name as title, its size and "on disk"/"apparent size" as subtitle,
      and a folder proxy icon; ⌘-clicking the title shows the path.

### The scan

- [ ] **Live treemap:** on a large folder (`~`, or `/`), folders appear and grow while the status
      bar counts entries; files appear when the scan finishes. The snapshots only ever saw the
      finished state.
- [ ] **Entering a folder mid-scan** and staying there: when the scan finishes, the window is still
      in that folder with the same entry in hand (`Viewer::finish_scan`, `adopt_navigation_from`).
- [ ] **Starting another scan mid-scan** (⌘O or drop): the old scan stops (CPU drops), and none of
      its results turn up in the new window (`scan_id` checks).
- [ ] **`/`:** a whole-disk scan completes, doesn't count the firmlinked data volume twice, and the
      status bar's "not found by the scan" figure is plausible.
- [ ] **Privacy prompts:** the first scan through Desktop, Documents or Downloads asks for access
      (the prompt names the terminal, since the binary is unbundled). Refusing it shows those
      folders as unreadable in the status bar rather than hanging.

### Keyboard

- [ ] *(smoke: ↓, Home, End)* **Arrows in the list** move by row and keep the highlighted row visible (it scrolls). In the
      treemap they move by direction; ← off the treemap's left edge goes to the list, → from the
      list goes to the treemap; Tab switches panel.
- [ ] **Page Up/Down, Home, End** jump through the list.
- [ ] *(smoke: Return, Esc)* **Return / keypad Enter** opens a folder; **Esc and ⌫** go up with the folder just left in
      hand.
- [ ] *(smoke: `a`)* **Plain letters:** `a` toggles apparent sizes, `+`/`=`, `-`, `0` zoom, `r`/`R` rescan, `d`
      and Forward Delete ask to move to the Trash.
- [ ] **Keys with ⌘ go to the menu, not the view** (`key()` returns false for ⌘/⌃, so an unhandled
      one beeps).

### Mouse

- [ ] *(smoke: click in the list)* **Click** selects in either panel; **double-click** opens a folder, and on a file opens Quick
      Look.
- [ ] *(smoke: ⌘-click)* **⌘-click** toggles a mark (the entry already in hand is marked as well, if the
      user picked it rather than the app placing it there); **⇧-click** marks
      the range from the anchor; a plain click clears the marks.
- [ ] **Hover** highlights the tile or row and names it in the status bar; leaving the view clears
      that (`NSTrackingArea` with `InVisibleRect`; `mouseExited:`).
- [ ] **Scrolling** over the list, with a trackpad (smooth, by points) and a mouse wheel (about
      three rows a notch). Over the treemap it does nothing.
- [ ] **Breadcrumbs:** clicking one goes up to it. With a deep path the middle ones collapse to
      "…" and the current folder stays visible (`draw.rs`, `path_bar`).
- [ ] **Clicking "small files"** says in the status bar that those entries are in the list.
- [ ] *(smoke: right-click, Copy as Pathname, cancel)* **Right-click and Control-click** an entry: the context menu appears, with the entry in hand
      (marks kept if it is one of them); each item works, and "Move to Trash" is greyed out while
      a scan runs (`menuForEvent:`, `validateMenuItem:`).
- [ ] **Clicking into an inactive window** acts at once (`acceptsFirstMouse:`).

### Menus

- [ ] **Each item works**, and is greyed out when it cannot: Open, Quick Look, Trash and Delete
      with nothing in hand; Enclosing Folder at the root; rescans during the first scan.
- [ ] **"Show Apparent Sizes" shows a check mark** when on, and "Hide Sidebar" toggles its title.
- [ ] **Shortcuts:** ⌘↑ / ⌘↓ (`U+F700`/`U+F701`), ⌘⌫ / ⌥⌘⌫ (`U+007F`; see above), and whether ⌘+ fires on a
      US keyboard without Shift (the key equivalent is `+`; if only ⇧⌘= works, add `=`). Also
      ⇧⌘R (key equivalent `R`), ⌃⌘S, ⌃⌘F full screen, ⌘M, ⌘H, ⌘Q.
- [ ] **About duscape** shows the standard panel.

### Acting on entries

- [ ] **Move to the Trash** (⌘⌫): the confirmation names the entry (or counts several and lists up
      to five), and after it the entry is in the Finder's Trash and gone from the list, with the
      next entry in hand and "Moved … to the Trash" in the status bar. "Freed" doesn't change.
- [ ] *(smoke: confirmed with Return; the file leaves the disk and the list)* **Delete immediately** (⌥⌘⌫): a critical-style alert saying it can't be undone; afterwards
      the space shows as freed.
- [ ] **A symlink:** trashing or deleting one removes the link, never its target.
- [ ] **A failure** (a file owned by root, or on a read-only volume): the others still go, an
      alert names the first failure and counts the rest, and the failed entries stay in the list.
- [ ] **Deleting while a rescan of the same folder runs:** the rescan starts again, and the
      deleted entry does not come back (`restart_rescans_under`).
- [ ] *(smoke: a plain path)* **Copy Path** (⌘C) puts a shell-quoted path on the pasteboard (paste it into a terminal:
      names with spaces, quotes, `$` and non-ASCII characters must survive). **Copy as Pathname**
      (⌥⌘C) is plain, one per line. With several marked, all of them are copied.
- [ ] **Show in Finder** (⌥⌘R) opens Finder with the entries selected, or the current folder when
      nothing is in hand.
- [ ] **Open** (⌘↓) on a file opens it in its default app.
- [ ] **Mark All** (⌘A) marks every entry, and the status bar shows the count and total.

### Previews and Quick Look

- [ ] *(smoke: a text file's lines)* **Text preview:** holding ↓ through a folder of text files shows only the one it stops on
      (60 ms debounce), with no flicker of stale previews (`preview_generation`).
- [ ] **Picture formats:** a JPEG with an EXIF rotation shows upright; HEIC, GIF, TIFF and WebP
      preview (`OTHER_PICTURES` in `src/preview.rs`); an AVIF may not, depending on the macOS
      version, and should then say it cannot be decoded.
- [ ] **A large but allowed picture** (say a 12,000×9,000 JPEG): the main thread may stall when it
      is first drawn, since `NSImage` decodes lazily on the main thread. Time it; if it's noticeable,
      decode a thumbnail off the main thread (`CGImageSourceCreateThumbnailAtIndex`) instead.
- [ ] **An iCloud-only file** (dataless) is described, not downloaded.
- [ ] *(smoke: Space opens it)* **Quick Look:** Space opens the panel on the entry in hand, and again closes it. With it open,
      arrows in the panel move the selection here and the panel follows (`previewPanel:handleEvent:`).
      With several marked, the panel pages through them.

### Drag and drop

- [ ] **Dropping a folder** from Finder onto the window scans it; a file is refused (no drop
      highlight, nothing happens).

### Appearance

- [ ] **Light mode:** all the snapshots were taken in dark mode. Check that the chrome (list,
      separators, text, selection) reads well in light mode, and that switching mode while the
      window is open redraws it. The treemap background is always dark (`draw.rs`, `treemap`);
      decide whether it should follow the mode.
- [ ] **Accent colour:** the list's selection uses it; try a non-blue accent.
- [ ] **Retina and non-Retina displays**, and dragging the window between them: text and tile
      borders stay crisp.
- [ ] **Resizing:** the side panel drops below 640 pt wide and the details pane below 320 pt tall;
      the entry in hand stays in hand throughout.

### Rescans

- [ ] **⌘R** on a folder after adding a big file to it from a terminal: when the rescan finishes,
      sizes update here and in every ancestor, and the status bar said "rescanning 1 folder"
      meanwhile. **⇧⌘R** rescans everything and updates the scan time.
- [ ] **Rescanning a folder that has been deleted** from a terminal: it disappears, and if the
      window was inside it, the window moves up to the nearest folder that still exists.

### Performance

- [ ] **A folder with 100,000+ entries** (e.g. a mail or cache folder): opening it, scrolling the
      list and moving through it stay responsive. `Board::change_files` sorts once per change,
      and several per-row lookups are linear in the listing or the marks.
- [ ] **Memory after several scans** in one session (⌘O repeatedly on a large folder): old trees
      are dropped on a background thread (`state::drop_later`), so memory should come back rather
      than grow with each scan.

### Packaging

- [ ] **An `.app` bundle** (not made yet): an `Info.plist` (bundle id, `LSMinimumSystemVersion`,
      `NSHighResolutionCapable`) and an icon, so it can be run from Finder and get privacy
      permissions of its own. Check that a bundled launch ignores Launch Services' `-psn_`
      argument (handled in `mac/mod.rs`, `options`).

## Windows viewer (`duscape-windows`): tiles are probably the wrong shape

The treemap (`common/src/tiles/treemap.rs`, `HEIGHT_WIDTH_RATIO = 2.5`) lays tiles out in cells it
takes to be 2.5 times **taller** than wide, like a terminal cell. `viewers/windows/src/main.rs` uses
`CELL_W = 10`, `CELL_H = 4`: cells 2.5 times *wider* than tall. The macOS viewer made the same
mistake first and drew every folder as a long flat band; swapping the constants
(`viewers/macos/src/state.rs`: `CELL_W = 2.4`, `CELL_H = 6.0`) fixed it.

- [ ] On Windows, scan a folder with a dozen similar-sized subfolders and check the tiles' shape.
      If they are flat bands, set `CELL_W = 4` and `CELL_H = 10` (and fix the comment above them,
      which says the opposite), check the tiles look square, and that the minimum tile (8×3 cells,
      now 32×30 px) still fits a label or reads as a tile.

## Linux viewer (`duscape-linux`): tried under Xvfb and a headless weston, not on a desktop

`viewers/linux/` was checked on an `Xvfb` with `xdotool` (see `viewers/linux/README.md`): the
scan, the list and treemap, arrows, Enter/Esc, Tab, Shift ranges, the Trash and delete dialogs,
click, right-click copy, double-click, `a`, resizing below the side panel's width, Ctrl+Q. The
Wayland backend was checked on a headless weston (kiosk shell, no input devices): connecting,
the first configure, the frame in a `wl_shm` buffer, the compositor's screenshot of it, and the
viewer's own title bar where the shell draws none. What neither can show:

- [ ] **Wayland input:** on GNOME, KDE or sway, keys (a US and a non-US layout, the keypad, Caps
      Lock, holding an arrow down for repeat), clicks, Ctrl/Shift-click, the wheel (a mouse's
      notches and a touchpad's smooth scrolling), hover, and the cursor showing as an arrow
      (`wayland.rs`; the keymap reading is `xkb.rs`).
- [ ] **Wayland decorations:** KDE and sway give a server title bar (`xdg-decoration`), and the
      viewer draws none; GNOME gives none, and the viewer's own appears — drag moves the window,
      double-click maximises, the three buttons work. Resizing from the edges is the
      compositor's; with the viewer's own bar there is no resize handle yet.
- [ ] **Wayland scale:** on a 2× output the buffer is drawn at 2× and text is crisp
      (`preferred_buffer_scale`, else `wl_output.scale`); dragging between a 1× and a 2× output
      switches.
- [ ] **Wayland clipboard:** right-click, then paste elsewhere; the offer carries the last input
      serial (`Shared::serial`), which a compositor may refuse if the click was long ago.
- [ ] **Backend choice:** under a Wayland session `WAYLAND_DISPLAY` picks Wayland; with
      `DUSCAPE_BACKEND=x11` it runs through XWayland instead; a session with neither says so.

- [ ] **Under a window manager:** the window opens at 1180×760, the minimum size (520×340) holds,
      the title bar shows the folder and its size, closing the window quits (`WM_DELETE_WINDOW`).
- [ ] **On Wayland (XWayland):** GNOME, KDE and sway start it; the scale follows the desktop
      (`Xft.dpi`, else set `DUSCAPE_SCALE=2`) and the text is not blurred.
- [ ] **HiDPI:** with `Xft.dpi: 192`, everything is twice the size and the mouse still lands on
      the right row and tile (`app.rs` divides by `Display::scale`).
- [ ] **Focus:** the highlighted row dims when another window takes focus and comes back after.
- [ ] **Hover** names the tile or row in the status bar; leaving the window clears it.
- [ ] **Clipboard:** right-click, then paste into a terminal — through `wl-copy`/`xclip`/`xsel`
      when installed, else from the window's own selection (which ends with the window).
- [ ] **Trash:** `d` on a small file, then look in the desktop's Trash and put it back; without
      `gio`, `~/.local/share/Trash/info/*.trashinfo` names where it came from. A file on another
      filesystem (a USB stick) goes to its `.Trash-<uid>`.
- [ ] **Keyboard layouts:** `+`/`-` on a non-US layout, the keypad's arrows and Enter, Caps Lock
      leaving `d` as `d` and `D` as `D` (`x11.rs`, `Keymap::keysym`).
- [ ] **A large scan (`/`, `~`):** folders appear and grow while the status bar counts; the window
      stays responsive (batches are drained before each frame, `App::run`).
- [ ] **Fonts:** a system without DejaVu (Alpine, a fresh Arch) finds one through `fc-match`, and
      says what to set when it finds none.
