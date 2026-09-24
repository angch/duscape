# TODO

Work that needs a machine this repository's CI does not have. Each item says how to check it and
what should happen; tick it off (or fix what it finds) and say so in the commit.

## macOS viewer (`diskonaut-mac`): untried by hand

`viewers/macos/` was written and checked without screen or input access. The platform-independent
state (`src/state.rs`) has unit tests, and the drawing was checked with
`DISKONAUT_MAC_SNAPSHOT=out.png` (see `viewers/macos/README.md`), which renders the view after a
scan. **Nothing below has been done with a real mouse and keyboard.** Run
`cargo run -p diskonaut-mac -- <folder>` from a terminal and go through it. The code for each is in
`src/mac/view.rs` unless said otherwise.

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

- [ ] **Arrows in the list** move by row and keep the highlighted row visible (it scrolls). In the
      treemap they move by direction; ← off the treemap's left edge goes to the list, → from the
      list goes to the treemap; Tab switches panel.
- [ ] **Page Up/Down, Home, End** jump through the list.
- [ ] **Return / keypad Enter** opens a folder; **Esc and ⌫** go up with the folder just left in
      hand.
- [ ] **Plain letters:** `a` toggles apparent sizes, `+`/`=`, `-`, `0` zoom, `r`/`R` rescan, `d`
      and Forward Delete ask to move to the Trash.
- [ ] **Keys with ⌘ go to the menu, not the view** (`key()` returns false for ⌘/⌃, so an unhandled
      one beeps).

### Mouse

- [ ] **Click** selects in either panel; **double-click** opens a folder, and on a file opens Quick
      Look.
- [ ] **⌘-click** toggles a mark (the entry already in hand is marked as well); **⇧-click** marks
      the range from the anchor; a plain click clears the marks.
- [ ] **Hover** highlights the tile or row and names it in the status bar; leaving the view clears
      that (`NSTrackingArea` with `InVisibleRect`; `mouseExited:`).
- [ ] **Scrolling** over the list, with a trackpad (smooth, by points) and a mouse wheel (about
      three rows a notch). Over the treemap it does nothing.
- [ ] **Breadcrumbs:** clicking one goes up to it. With a deep path the middle ones collapse to
      "…" and the current folder stays visible (`draw.rs`, `path_bar`).
- [ ] **Clicking "small files"** says in the status bar that those entries are in the list.
- [ ] **Right-click and Control-click** an entry: the context menu appears, with the entry in hand
      (marks kept if it is one of them); each item works, and "Move to Trash" is greyed out while
      a scan runs (`menuForEvent:`, `validateMenuItem:`).
- [ ] **Clicking into an inactive window** acts at once (`acceptsFirstMouse:`).

### Menus

- [ ] **Each item works**, and is greyed out when it cannot: Open, Quick Look, Trash and Delete
      with nothing in hand; Enclosing Folder at the root; rescans during the first scan.
- [ ] **"Show Apparent Sizes" shows a check mark** when on, and "Hide Sidebar" toggles its title.
- [ ] **Shortcuts:** ⌘↑ / ⌘↓ (`U+F700`/`U+F701`), ⌘⌫ / ⌥⌘⌫ (`U+0008`), and whether ⌘+ fires on a
      US keyboard without Shift (the key equivalent is `+`; if only ⇧⌘= works, add `=`). Also
      ⇧⌘R (key equivalent `R`), ⌃⌘S, ⌃⌘F full screen, ⌘M, ⌘H, ⌘Q.
- [ ] **About diskonaut** shows the standard panel.

### Acting on entries

- [ ] **Move to the Trash** (⌘⌫): the confirmation names the entry (or counts several and lists up
      to five), and after it the entry is in the Finder's Trash and gone from the list, with the
      next entry in hand and "Moved … to the Trash" in the status bar. "Freed" doesn't change.
- [ ] **Delete immediately** (⌥⌘⌫): a critical-style alert saying it can't be undone; afterwards
      the space shows as freed.
- [ ] **A symlink:** trashing or deleting one removes the link, never its target.
- [ ] **A failure** (a file owned by root, or on a read-only volume): the others still go, an
      alert names the first failure and counts the rest, and the failed entries stay in the list.
- [ ] **Deleting while a rescan of the same folder runs:** the rescan starts again, and the
      deleted entry does not come back (`restart_rescans_under`).
- [ ] **Copy Path** (⌘C) puts a shell-quoted path on the pasteboard (paste it into a terminal:
      names with spaces, quotes, `$` and non-ASCII characters must survive). **Copy as Pathname**
      (⌥⌘C) is plain, one per line. With several marked, all of them are copied.
- [ ] **Show in Finder** (⌥⌘R) opens Finder with the entries selected, or the current folder when
      nothing is in hand.
- [ ] **Open** (⌘↓) on a file opens it in its default app.
- [ ] **Mark All** (⌘A) marks every entry, and the status bar shows the count and total.

### Previews and Quick Look

- [ ] **Text preview:** holding ↓ through a folder of text files shows only the one it stops on
      (60 ms debounce), with no flicker of stale previews (`preview_generation`).
- [ ] **Picture formats:** a JPEG with an EXIF rotation shows upright; HEIC, GIF, TIFF and WebP
      preview (`OTHER_PICTURES` in `src/preview.rs`); an AVIF may not, depending on the macOS
      version, and should then say it cannot be decoded.
- [ ] **A large but allowed picture** (say a 12,000×9,000 JPEG): the main thread may stall when it
      is first drawn, since `NSImage` decodes lazily on the main thread. Time it; if it's noticeable,
      decode a thumbnail off the main thread (`CGImageSourceCreateThumbnailAtIndex`) instead.
- [ ] **An iCloud-only file** (dataless) is described, not downloaded.
- [ ] **Quick Look:** Space opens the panel on the entry in hand, and again closes it. With it open,
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

## Windows viewer (`diskonaut-gui`): tiles are probably the wrong shape

The treemap (`common/src/tiles/treemap.rs`, `HEIGHT_WIDTH_RATIO = 2.5`) lays tiles out in cells it
takes to be 2.5 times **taller** than wide, like a terminal cell. `viewers/windows/src/main.rs` uses
`CELL_W = 10`, `CELL_H = 4`: cells 2.5 times *wider* than tall. The macOS viewer made the same
mistake first and drew every folder as a long flat band; swapping the constants
(`viewers/macos/src/state.rs`: `CELL_W = 2.4`, `CELL_H = 6.0`) fixed it.

- [ ] On Windows, scan a folder with a dozen similar-sized subfolders and check the tiles' shape.
      If they are flat bands, set `CELL_W = 4` and `CELL_H = 10` (and fix the comment above them,
      which says the opposite), check the tiles look square, and that the minimum tile (8×3 cells,
      now 32×30 px) still fits a label or reads as a tile.
