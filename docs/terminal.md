# The terminal viewer in detail

The list beside the treemap, marks and the clipboard, previews, rescans, and what the keys do
beyond the table in the README.

## The list beside the treemap

On a terminal 80 columns or wider, the left third of the screen lists the current folder: its
path, size, file count and share of the whole scan, how many folders and files it holds, and at a
volume root the disk's used space. Below that is every entry, largest first, with a bar, its size
and its share of the folder. It has no border, so every line and column goes to names; long names
are cut in the middle, keeping the start and the extension.

The keyboard works one panel at a time — the list, to begin with, on its largest entry — and
after that on whichever you last clicked. `Tab` switches;
`←` off the treemap's left edge moves into the list, and `→` from the list moves back. In the
list, `↑`/`↓` (`k`/`j`) walk every entry in size order, tiles or not, `PgUp`/`PgDn`/`Home`/`End`
jump, and `Enter`, `Esc` and `d` act on the highlighted row. In the treemap the arrows move
between tiles as before. The panel with the keyboard has the solid highlight; the other still
marks the same entry, more quietly.

To pick several entries, `Ctrl`+click them — in the list or on the treemap; a second
`Ctrl`+click takes one out — or hold `Shift` and press `↑`/`↓` in the list to mark a run of
rows. Every change copies the marked paths to the clipboard, quoted and separated by spaces in
the order you picked them, ready to paste after a command: `cp 'my file' notes.txt ~/backup/`.
The title shows what was copied. Marked entries are black on yellow in both panels. A plain
click or arrow key, or changing folder, clears the marks.

`d` deletes the marked entries, or the entry in hand when nothing is marked. The prompt says how
many and how much — `Delete these 3 items (4.2G)?` — and names as many as fit. If one fails, the
rest are still deleted and the message says which did not and why; only what was really removed
is counted as freed. A marked NTFS metadata file refuses the whole deletion rather than being
skipped quietly.
Some terminals keep `Ctrl`+click for a context menu of their own; `Shift`+arrows work in all of
them.

The row for the selected tile is highlighted and kept in view. Entries without a tile of their
own — too small, and folded into the `x` corner, or left off by the zoom — are listed dimmed, and
are often most of a folder: in a build cache of 30,000 small files the list is the only way to
see them. Rows take the same clicks as tiles: select, double-click to open, right-click to copy.

Below the list is a preview of the file in hand, 16:9 in shape (worked out from the terminal's
cell size in pixels, where it reports one) and never more than half the panel. A text file shows
its first lines, with escape sequences and other control characters shown as `?` rather than
passed to the terminal. A PNG or JPEG — recognised by its first bytes, not its name — is drawn
as a picture in terminals that speak the kitty graphics protocol (kitty, Ghostty, WezTerm; known
from the environment, or by asking the terminal when it says nothing, as over ssh), once
the selection has rested on it for 100 ms, so moving quickly through a folder of photos decodes
none of them. A terminal without kitty graphics that has sixels (foot, xterm, mlterm, Windows
Terminal, iTerm2, Konsole, tmux built with them — found from the terminal's device attributes)
gets the picture as sixels, in up to 256 colours. Elsewhere it is drawn in the text itself as
half blocks (`▀`), two pixels to a cell: in 24-bit colour where `COLORTERM` says the terminal has
it, and in the 256-colour palette otherwise (ssh does not pass `COLORTERM` on).
`DISKONAUT_GRAPHICS=kitty`, `sixel`, `blocks` or `none` overrides the guess; `none` describes a picture instead of drawing it:
`PNG image · 1920×1080`. Only regular files are read, and on
macOS files that are only in iCloud are not, since reading one would download it.

Narrower terminals give the whole width to the treemap, as before.

## Keys, in more detail

Deletion always asks for `y` / `n` confirmation.

`a` switches between the space files take on disk and their apparent size (their length, as
`du --apparent-size` counts it) at once, without scanning again: every file keeps both. `-a` or
`apparent-size = true` in the config only chooses which is shown first.

`r` scans the selected folder again (the folder shown, when a file is selected) and `R` the whole
tree, in the background: the old figures stay up, and can be browsed, until the new ones replace
them. A folder that has gone from disk is taken out of the view. Hard links between the rescanned
folder and the rest are counted on both sides until the next `R`.

On XFS and btrfs, small files are checked for blocks they share with other files (reflink copies,
btrfs snapshots) after the treemap is up, the folder you are looking at first; the title says
"refining" meanwhile, and sizes can go down a little as it finds them.

The help line at the bottom moves on by itself: the key legend, then a tip, then the legend again
and the next tip. Each rests at least five seconds, counted from your last key press, and then
slides to the next in under 100 ms. A legend wider than the terminal is shown a page at a time.

A double click is two clicks on the same tile within half a second.

Copied relative paths start from the directory you ran diskonaut in, so they paste straight into
the same shell: in `/home/user/foo`, `diskonaut ../bar/` with `baz` selected copies `../bar/baz`.
Both ends are resolved first, so a symlinked directory cannot send `..` somewhere else; where no
relative path exists (the directory was deleted, or on Windows the scan is on another drive) the
absolute path is copied, and the title says so. Paths are quoted so they paste into a
shell as exactly that path — `'my dir/it'\''s here'` — in PowerShell's quoting on Windows. Names
with a newline, an escape sequence, bytes that are not UTF-8, or an invisible right-to-left
override come out as `$'…'` escapes rather than raw, so a pasted path cannot run anything and
what the title shows is what was copied. A relative path starting with `-` gets a leading `./`,
so a command cannot take it for an option. The title shows what was copied for two seconds.

The copy goes to the system clipboard: `pbcopy` on macOS, the Windows clipboard, and `wl-copy`,
`xclip` or `xsel` on a Linux desktop. Where none is available, as over SSH, it is sent to the
terminal instead (OSC 52), which iTerm2, kitty, WezTerm, Windows Terminal, foot and Alacritty put
on the clipboard of the machine you are sitting at; in tmux that needs `set -g set-clipboard on`. While diskonaut runs it
captures the mouse, so the terminal's own click-and-drag text selection needs a modifier: `Shift`
in most terminals, `Option` (`⌥`) in iTerm2 and Terminal.app. Quitting releases the mouse.
