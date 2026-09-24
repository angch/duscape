# diskonaut for MS-DOS

The treemap viewer as a 16-bit real-mode DOS program, in FASM, about 29 KB assembled, for a 286
(or an 80186) with no coprocessor. It is not built from the Rust code; the Rust code is its
reference, and the layout is checked against it tile for tile.

| File | What it holds |
| --- | --- |
| [`DISKONAU.ASM`](DISKONAU.ASM) | start-up, the scan, the tree, the treemap, drawing, keys, delete, rescan |
| [`PANEL.ASM`](PANEL.ASM) | the side panel: the folder's details, its entries, the list's cursor and focus |
| [`PREVIEW.ASM`](PREVIEW.ASM) | the preview: text, what a file is, pictures in half blocks, the palette |
| [`PNG.ASM`](PNG.ASM), [`JPEG.ASM`](JPEG.ASM) | the two picture decoders |
| [`PVDATA.ASM`](PVDATA.ASM) | the data of the last three |
| [`SOFTFP.ASM`](SOFTFP.ASM), [`FPDATA.ASM`](FPDATA.ASM) | IEEE doubles in 16-bit code, and their working storage |
| [`J286.INC`](J286.INC) | conditional jumps a 286 can take |
| [`tests/`](tests/) | the soft float against the host's doubles, and a lint for 386 instructions |

```sh
make dos        # target/dos/DISKONAU.EXE (and diskonaut.exe, the same file)
make dos-run    # DOSBox-X with this repository as C:, diskonaut scanning it
```

`make dos` needs `dosbox-x` (`brew install dosbox-x`, or your distribution's package). It fetches
FASM 1.73 for DOS and the CWSDPMI DPMI host into `target/dos/` once, checks them against the
hashes in the `Makefile`, and assembles inside DOSBox-X. Inside DOS, `VIEWERS\DOS\BUILD.BAT` does
the same from the repository root.

```
DISKONAU [-a] [-k KEYS] [-s FILE] [FOLDER]
  -a       apparent sizes instead of space on disk
  -k KEYS  type these first; E X T P N H Z are Enter Esc Tab PgUp PgDn Home End
           (DOS keeps < and > for redirection)
  -s FILE  then write the screen to FILE (80x25 character/attribute pairs, then the tiles,
           the palette and the preview's block pixels)
```

Keys are the terminal viewer's. The list beside the treemap has the keyboard to begin with: up and
down (`k`, `j`) move its cursor, Page Up, Page Down, Home and End jump, right (`l`) or Tab goes to
the treemap, where the arrows move between tiles and left off its edge (or Tab) comes back. Enter
opens a folder (on the treemap with nothing selected, the largest), Esc or Backspace goes
up, `+` `-` `0` zoom, `a` switches between space on disk and apparent size, `d` or Delete deletes
(folders with everything in them, read-only files too, after asking; only an entry you picked,
never one the program placed, as after entering a folder or a delete), `r` scans the selected
folder again (or the one shown), `R` everything, `s` hides the side panel (as the Linux viewer's
does), `q` quits. With a mouse driver, a click selects a tile or a list row and gives its panel
the keyboard, and a second click on the same entry within half a second opens it. `q`, Esc or Ctrl-C during the
scan stops it.

## What it does

- **Scans** with DOS's own find calls: long file names (INT 21h `71xxh`) when DOS has them, as
  DOSBox-X does with `lfn=true`, else 8.3 names. The treemap of the folder is drawn live while
  the walk runs. Space on disk is the length rounded up to the drive's cluster size.
- **Lays out** with the squarify algorithm of `common/src/tiles/treemap.rs` in IEEE doubles
  computed in software, in Rust's order of operations, so every intermediate value is the f64
  the Rust code has: the same row decisions, the same `RectFloat::round`, the same "small files"
  corner.
  Navigation (`board.rs`, `tile.rs`), the tile text and colours (`draw_rect.rs`), and the size and
  count formats (`display_size.rs`, `display_count.rs`, `truncate.rs`) are the same too, drawn
  in CP437 box characters that join where tiles meet.
- **The side panel** is `side_panel.rs` on 80 columns: a third of the width, the folder's path,
  its size and share of the scan, its folders and files, the space the drive says is used when it
  is more than the scan found, then its entries largest first with their sizes (the cursor kept
  in the middle, "... N more" under them), each coloured by whether it has a tile. Under the list,
  the entry in hand, and its preview.
- **Previews** the file in hand as `common/src/preview.rs` reads it: the first lines of a text
  file (tabs to four, control characters as `?`, UTF-8 shown in CP437 where it has the
  character), "binary file", "empty file", or a PNG or JPEG drawn in half blocks, two pixels to a
  cell, fitted into 25 by 7 cells the way the image crate's `thumbnail` fits it. A picture waits
  until the keyboard has been still for 100 ms, and a key stops it.
  - PNG: every colour type and bit depth, Adam7, palette transparency. Inflate is Mark Adler's
    puff, a bit at a time; every eighth pixel of every eighth row a block pixel covers is
    averaged, which is plenty for 25 by 14.
  - JPEG: baseline, extended and progressive, grayscale or YCbCr, any sampling, restart markers.
    A block pixel covers many 8x8 blocks, and a block's mean is its DC coefficient alone, so
    there is no inverse DCT: the Huffman codes are decoded (the AC ones skipped) and the means
    averaged; a progressive file needs only its first scans.
  - Text mode has 16 colours. Ten are the interface's; the other six (green, magenta, brown, dark
    gray, bright red, bright magenta, which the interface never uses) are set in the VGA DAC to
    the colours the picture needs, picked farthest first and refined once, and each block pixel
    takes its nearest of the 16. They are put back when the picture goes.
  - In DOSBox-X at `cycles=max` a 4000x3000 JPEG takes about 2 s, a 1920x1080 PNG about 3.5 s;
    on a real 286, many times that, which is why a key stops it.
- **Deletes** a file, or a folder and everything in it, and takes it off every folder above it;
  the title shows what was freed. If part of a folder will not go, the error is shown and the
  folder is walked again, so the treemap shows what is left and "freed" what went.
- **Rescans** a folder (`r`) or everything (`R`) into a new node grafted in place, the folders
  above it corrected by the difference.

## A 286 and no coprocessor

It uses nothing a 286 does not have, and nothing of the 286 an 80186 lacks, and on an 8086 or
8088 says so and stops (they shift by all of CL, the 186 by its low five bits):

- **16-bit registers only.** Sizes are 64 bits in four words, counts and sums 32 bits in two;
  a 32-bit decimal is two chained divides. Node and listing access that used FS and GS goes
  through ES, reloaded as needed, so string instructions still write through ES = DS.
- **No near conditional jumps.** The 386's 0F 8x forms do not exist on a 286, and FASM has no
  286 mode, so [`J286.INC`](J286.INC) redefines every conditional jump as a macro: short when the
  target is in reach, else the opposite condition over a near `jmp`.
- **No coprocessor.** [`SOFTFP.ASM`](SOFTFP.ASM) is IEEE 754 double precision: add, subtract,
  multiply, divide, compare, conversion from 64-bit integers, and the two roundings Rust uses
  (`f64::round`, and `{:.N}`'s half to even, exact for a small multiplier). Values stay unpacked
  (64-bit significand, exponent, sign, class) and each result is rounded to 53 bits, to nearest,
  ties to even, so it is bit for bit what an IEEE double gives, as long as nothing overflows,
  underflows or goes subnormal, which nothing here does. Sizes and percentages go through it
  where Rust uses f64 (`size as f64 / total as f64`, `DisplaySize`'s division by 2^30, which is
  an exact change of exponent); the JPEG colour conversion is in fixed point, which is only a
  picture's colour.
- A relayout of a folder of 46 entries takes about 0.12 s at 286 speed (DOSBox-X at
  `cycles=fixed 3000`, about a 12 MHz 286).

Ctrl-C and critical errors (a drive not ready) are its own while it runs, so neither leaves the
screen or a search behind.
Ctrl-C and critical errors (a drive not ready) are its own while it runs, so neither leaves the
screen or a search behind.

## Memory

Every entry is a node in conventional memory: 32 bytes, then its name. After the program, the
listing (64 KB) and the pending walks (32 KB, which inflate's window shares, since the two never
run at once), about 420 KB is left for them in DOSBox-X: roughly 6,000 entries with long names,
9,000 with 8.3 ones. The preview's 64 KB of picture lines comes from upper memory where DOS has
it, as DOSBox-X does. When it is full, the rest of a folder is
charged to one **(not in memory)** entry in it: totals stay right everywhere, but that part of
the tree cannot be opened. Scanning the repository with a Rust build in `target/` (70,000 entries)
shows that.

Folders deeper than 40 levels, or whose path would pass DOS's 250 characters, are counted but
not walked into: their size shows as 0, and the status line says "not scanned: too deep" when one
is selected.

## DOSBox-X and host folders

- Only one directory search is ever open. DOSBox-X loses entries of long-name searches when
  several are open at once, so each folder is listed to the end before its subfolders are walked.
- In a host folder with thousands of names that share a long prefix (`target/debug/deps`),
  DOSBox-X mixes up the short names it makes for them and reports some files with another file's
  size, and its own `DIR` shows the same. diskonaut shows what DOS reports.
- DOS knows nothing of hard links, so a Rust `target/` (hard-linked binaries) counts larger than
  `du` says.
- A long name with a character CP437 lacks (`łódź`, `日本語`) is not listed by DOSBox-X at all, so
  neither is anything in it. On a DOS that lists it with the long name converted lossily (the
  7xxxh calls set bit 0 of CX), the node keeps the short name too and every path uses that.
- DOSBox-X keeps a host file's DOS attributes in a `.DBLOCALFILE_ATR_<name>` file beside it, so
  attributes are only touched when a delete is refused and the entry is read-only, hidden or
  system; clearing them on every folder would leave such files behind in the folder's parent.

## Checking it

```sh
python3 viewers/dos/test.py            # builds, then every check; -v prints the screens
python3 viewers/dos/test.py layout     # the checks with "layout" in their name
python3 viewers/dos/tests/softfp.py    # SOFTFP.ASM against the host's doubles, 20,000 random ops
python3 viewers/dos/tests/lint286.py   # any instruction a 286 does not have
```

Every run is on an emulated 286 with no coprocessor (DOSBox-X `cputype=286`, `fpu=false`,
`core=normal`), which faults on any instruction the machine lacks, so a check that passes has
executed only 286 code. The lint catches what no check reaches: 32-bit registers and operands, FS
and GS, the 386's and the x87's instructions, and variables declared `dd` or `dq`, for which FASM
emits 32-bit instructions without being asked. `softfp.py` assembles `tests/TESTFP.ASM` and runs
it on the same 286: random additions, subtractions, products, quotients, comparisons, roundings
and conversions (integers, fractions, ties, infinities, NaNs, zeros of both signs), each
compared bit for bit with the host's IEEE result; 150,000 of them agreed.

`-s` writes the screen and the tiles and `-k` types the keys, so each check runs the program on a
fixture in `target/dos/t/` and reads what it drew. All of a pass's runs go in one batch file, so
DOSBox-X starts once. The checks cover the scan and the treemap, entering and leaving, zoom,
deleting (the question, a read-only file, a folder that will not let go of part of itself, no
delete for an entry nobody picked, the list's cursor on the neighbour after one), rescanning,
names outside ASCII, 8.3 names, and an 8086 refused; the side panel, the list's keys and focus,
jumps, deleting an entry that has no tile, a long name cut keeping its end, hiding the panel;
text previews (a 64 KiB head as one line) and what a file is (a NUL in a 64 KiB head's last
byte). A tiny JPEG keeps its colour, and 97 damaged or hostile pictures (cut short, bytes
flipped, or made up: zero or huge sizes, no palette, empty or garbage image data, a scan before
any frame, sampling factors of 3, repeated component ids, a DHT claiming 4080 symbols, restarts
without markers) must each end in a picture or a line saying why, never hang or fault. The picture check makes sixteen
pictures with ImageMagick (PNG: every colour type and depth, interlaced, transparent, tiny,
tall; JPEG 4:2:0, 4:4:4, grayscale, progressive, restart intervals), decodes each with
ImageMagick too, averages it into the same block pixels, and compares: the grid's size, the
caption, every block's colour (within a few levels on average), and that each is drawn in its
nearest of the 16 colours. The layout check lays out 40 random folders (sizes 0 to
Pareto-distributed, 1 to 80 entries), beside the panel and without it, and compares every tile and the small-files corner with
`libdiskonaut::tiles::TreeMap`, through [`tiles/`](tiles/), a tool outside the workspace.

The mouse is written against the INT 33h driver but not tried: DOSBox-X run headless has no
pointer to move.

Not ported: marks, the second pass, the clipboard, configurable keys, the help line's tips.
