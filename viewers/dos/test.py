#!/usr/bin/env python3
"""Run DISKONAU.EXE in DOSBox-X on fixtures and check what it shows.

    python3 viewers/dos/test.py          # everything (runs `make dos` first)
    python3 viewers/dos/test.py -v       # and print every screen
    python3 viewers/dos/test.py layout   # only the checks whose name contains "layout"

Each check runs the program with `-k KEYS -s FILE`: the keys are typed, then the screen (80x25
character and attribute pairs) and the tiles are written and it quits. All the runs of a pass go
in one batch file, so DOSBox-X starts once. The layout checks lay out random folders and compare
the tiles with libdiskonaut's TreeMap, through the dos-tiles tool beside this file.

Needs dosbox-x, cargo, and ImageMagick (`magick`) for the picture previews.
"""

import os
import random
import shutil
import struct
import subprocess
import sys

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
WORK = os.path.join(REPO, "target", "dos", "t")  # fixtures and screens, C:\TARGET\DOS\T
DOS_WORK = "C:\\TARGET\\DOS\\T"
VERBOSE = "-v" in sys.argv
ONLY = [a for a in sys.argv[1:] if not a.startswith("-")]

LOW = " ☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼"
MAXTILES = 160
PV_COLS, PV_ROWS = 25, 7            # the preview's body, in cells
PV_PIXELS = PV_COLS * PV_ROWS * 2   # block pixels: two to a cell
PREVIEWED = 3                       # PV_PICTURE


class Screen:
    def __init__(self, data):
        self.chars = [[data[(y * 80 + x) * 2] for x in range(80)] for y in range(25)]
        self.attrs = [[data[(y * 80 + x) * 2 + 1] for x in range(80)] for y in range(25)]
        self.rows = ["".join(LOW[c] if c < 32 else bytes([c]).decode("cp437") for c in row)
                     for row in self.chars]
        rest = data[4000:]
        count, small_x, small_y, has_small = struct.unpack("<HHHB", rest[:7])
        self.small = (small_x, small_y) if has_small else None
        self.tiles = [struct.unpack("<HHHHHH", rest[8 + 16 * i:8 + 16 * i + 12])
                      for i in range(count)]
        rest = rest[8 + 16 * MAXTILES:]
        self.palette = [tuple(rest[i * 3:i * 3 + 3]) for i in range(16)]
        self.preview_state = rest[48]
        self.blocks_wide, self.blocks_high = struct.unpack("<HH", rest[49:53])
        pixels = rest[53:53 + PV_PIXELS * 4]
        self.block_rgb = [tuple(pixels[i * 4:i * 4 + 4]) for i in range(PV_PIXELS)]
        self.block_colour = list(rest[53 + PV_PIXELS * 4:53 + PV_PIXELS * 5])
        self.text = "\n".join(self.rows)

    def __contains__(self, text):
        return text in self.text

    def row(self, y):
        return self.rows[y]


def dosbox(commands, *, lfn=True, limit=300, extra=()):
    """Run the commands, one a line, from a batch file in DOSBox-X with the repository as C:."""
    batch = os.path.join(WORK, "RUN.BAT")
    with open(batch, "w", newline="\r\n") as f:
        f.write("\n".join(commands) + "\n")
    settings = ["dos ver=7.1", "dos lfn=true"] if lfn else ["dos ver=5.0", "dos lfn=false"]
    # a 286 with no coprocessor, the least it runs on; the normal core is the one that faults on
    # an instruction the machine does not have
    settings += ["cpu cputype=286", "cpu fpu=false", "cpu core=normal"]
    args = ["dosbox-x", "-silent", "-fastlaunch", "-nogui", "-nomenu", "-defaultconf",
            "-time-limit", str(limit), "-set", "cpu cycles=max"]
    for setting in list(settings) + list(extra):
        args += ["-set", setting]
    args += ["-c", f"mount c {REPO}", "-c", "c:", "-c", DOS_WORK + "\\RUN.BAT", "-c", "exit"]
    subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def screens(runs, **options):
    """runs: [(args, keys)] → [Screen or None], all in one DOSBox-X session."""
    commands = []
    for index, (args, keys) in enumerate(runs):
        out = f"{DOS_WORK}\\S{index:03}.BIN"
        path = os.path.join(WORK, f"S{index:03}.BIN")
        if os.path.exists(path):
            os.remove(path)
        script = f" -k {keys}" if keys else ""
        commands.append(f"TARGET\\DOS\\DISKONAU.EXE {args}{script} -s {out}")
    dosbox(commands, **options)
    result = []
    for index in range(len(runs)):
        path = os.path.join(WORK, f"S{index:03}.BIN")
        result.append(Screen(open(path, "rb").read()) if os.path.exists(path) else None)
    return result


def write(path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"x" * size)


def fixtures():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    fix = os.path.join(WORK, "fix")
    for name, size in [("big.bin", 300000), ("medium.dat", 120000), ("src/main.rs", 40000),
                       ("src/lib.rs", 25000), ("src/deep/er/x.txt", 90000),
                       ("docs/features.md", 15000), ("docs/scan-performance.md", 60000),
                       ("readme.txt", 2000), ("tiny1", 10), ("tiny2", 20), ("tiny3", 30)]:
        write(os.path.join(fix, name), size)
    for i in range(30):
        write(os.path.join(fix, "src", "deep", f"f{i:02}.o"), 1000 * (i + 1))
    os.makedirs(os.path.join(fix, "empty"))
    for copy in ("del", "del2", "del3", "part"):
        shutil.copytree(fix, os.path.join(WORK, copy))
    # a read-only file deleted with the rest; a folder that refuses to let go of what is in it
    os.chmod(os.path.join(WORK, "del", "src", "lib.rs"), 0o444)
    os.chmod(os.path.join(WORK, "part", "src", "deep", "er"), 0o555)
    # a name longer than the list's column
    write(os.path.join(WORK, "names", "IMG_20240101_1200.jpg"), 9000)
    write(os.path.join(WORK, "names", "b.txt"), 10)
    # a long name outside ASCII that CP437 has (DOSBox-X hides one it has not, so the short-name
    # path for those cannot be tried here)
    write(os.path.join(WORK, "wide", "café", "inside.txt"), 5000)
    write(os.path.join(WORK, "wide", "plain.txt"), 3000)


# pictures, each alone in a folder so that it is the list's first entry: (folder, file, how)
PICTURE = ('-size 400x225 radial-gradient:white-darkgreen -fill red -draw "rectangle 20,20 150,120"'
           ' -fill blue -draw "circle 300,110 300,60" -fill yellow -draw "rectangle 180,160 390,215"')
PICTURES = [
    ("rgb", "a.png", PICTURE + " PNG24:{out}"),
    ("palette", "a.png", PICTURE + " -colors 60 PNG8:{out}"),
    ("interlaced", "a.png", PICTURE + " -interlace PNG PNG24:{out}"),
    ("deep", "a.png", PICTURE + " -depth 16 PNG48:{out}"),
    ("gray8", "a.png", PICTURE + " -colorspace gray -depth 8 PNG:{out}"),
    ("gray4", "a.png", PICTURE + " -colorspace gray -depth 4 PNG:{out}"),
    ("gray2", "a.png", PICTURE + " -colorspace gray -depth 2 PNG:{out}"),
    ("gray1", "a.png", PICTURE + " -colorspace gray -monochrome PNG:{out}"),
    ("alpha", "a.png", "-size 300x160 gradient:red-blue ( -size 300x160 gradient:white-black"
                       " -rotate 90 -resize 300x160! ) -alpha off -compose copy_opacity"
                       " -composite PNG32:{out}"),
    ("tiny", "a.png", "-size 12x7 xc:orange -fill navy -draw \"rectangle 0,0 5,6\" PNG24:{out}"),
    ("tall", "a.png", "-size 90x700 gradient:cyan-maroon PNG24:{out}"),
    ("baseline", "a.jpg", PICTURE + " -quality 90 -sampling-factor 2x2 JPEG:{out}"),
    ("full", "a.jpg", PICTURE + " -quality 90 -sampling-factor 1x1 JPEG:{out}"),
    ("grayjpeg", "a.jpg", PICTURE + " -colorspace gray -quality 90 JPEG:{out}"),
    ("progressive", "a.jpg", PICTURE + " -quality 90 -interlace JPEG JPEG:{out}"),
    ("restarts", "a.jpg", PICTURE + " -quality 90 -define jpeg:restart-interval=3 JPEG:{out}"),
]


def picture_fixtures():
    for folder, name, how in PICTURES:
        out = os.path.join(WORK, "pv", folder, name)
        os.makedirs(os.path.dirname(out))
        command = "magick " + how.replace("{out}", "'" + out + "'")
        command = command.replace("( ", "\\( ").replace(" )", " \\)")
        subprocess.run(command, shell=True, check=True)
    text = os.path.join(WORK, "pv", "text", "notes.txt")
    os.makedirs(os.path.dirname(text))
    with open(text, "wb") as f:
        f.write("a\tb\r\n\x1b[2Jcleared\nhéllo wörld ½\n".encode() + b"x" * 60 + b"\nlast")
    tiny = os.path.join(WORK, "pv", "tinyjpeg", "a.jpg")
    os.makedirs(os.path.dirname(tiny))
    subprocess.run(["magick", "-size", "5x4", "xc:orange", "-quality", "95",
                    "-sampling-factor", "2x2", "JPEG:" + tiny], check=True)
    for folder, data in [("binary", b"ELF\x00\x01\x02"), ("empty", b""),
                         ("head64k", b"x" * 65536), ("binary64k", b"x" * 65535 + b"\x00"),
                         ("broken", open(os.path.join(WORK, "pv", "rgb", "a.png"), "rb")
                          .read()[:900])]:
        os.makedirs(os.path.join(WORK, "pv", folder))
        with open(os.path.join(WORK, "pv", folder, "a.png" if folder == "broken" else "f"),
                  "wb") as f:
            f.write(data)


def reference_blocks(path):
    """The preview's block pixels as the terminal viewer makes them: the picture fitted into the
    preview (never scaled up), averaged into cells half a cell tall; from ImageMagick's decode."""
    size = subprocess.run(["magick", "identify", "-format", "%w %h", path + "[0]"],
                          capture_output=True, text=True, check=True).stdout.split()
    width, height = int(size[0]), int(size[1])
    raw = subprocess.run(["magick", path + "[0]", "-alpha", "on", "-depth", "8", "rgba:-"],
                         capture_output=True, check=True).stdout
    fit_w, fit_h = width, height
    if width > PV_COLS * 8 or height > PV_ROWS * 16:
        ratio = min(PV_COLS * 8 / width, PV_ROWS * 16 / height)
        fit_w = max(1, int(width * ratio + 0.5))
        fit_h = max(1, int(height * ratio + 0.5))
    cols = min(PV_COLS, (fit_w + 7) // 8)
    rows = min(PV_ROWS * 2, (fit_h + 7) // 8)
    sums = [[0, 0, 0, 0, 0] for _ in range(cols * rows)]
    for y in range(height):
        base = (y * rows // height) * cols
        line = raw[y * width * 4:(y + 1) * width * 4]
        for x in range(width):
            cell = sums[base + x * cols // width]
            for channel in range(4):
                cell[channel] += line[x * 4 + channel]
            cell[4] += 1
    return (width, height), cols, rows, [tuple(c // n[4] for c in n[:4]) for n in sums]


def weighted(a, b):
    return 2 * (a[0] - b[0]) ** 2 + 4 * (a[1] - b[1]) ** 2 + 3 * (a[2] - b[2]) ** 2


def cleanup():
    part = os.path.join(WORK, "part", "src", "deep", "er")
    if os.path.exists(part):
        os.chmod(part, 0o755)


CHECKS = []


def check(function):
    CHECKS.append(function)
    return function


def expect(condition, what):
    if not condition:
        raise AssertionError(what)


@check
def the_scan_and_the_treemap():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "")])
    expect("Total (apparent): 1.1M (46 files), freed: 0" in s.row(0), s.row(0))
    expect("src/ (+35 descendants)" in s, "the largest folder, with its descendants")
    expect("big.bin" in s and "293.0K (27%)" in s, "a file with its size and share")
    expect(s.small is not None, "the small files corner")
    return [s]


@check
def entering_and_leaving():
    into, back = screens([(f"-a {DOS_WORK}\\FIX", "lE"), (f"-a {DOS_WORK}\\FIX", "lEX")])
    expect(into.row(0).rstrip().endswith("fix\\src (605.5K, 35 files)"), into.row(0))
    expect(back.row(0).rstrip().endswith("C:\\target\\dos\\t\\fix"), back.row(0))
    return [into, back]


@check
def zoom():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "++")])
    expect("zoom 2" in s.row(23), s.row(23))
    board = "\n".join(row[BOARD[0]:] for row in s.rows[1:23])
    expect("medium.dat" in board and "big.bin" not in board, "the two largest are zoomed past")
    return [s]


@check
def delete_asks_and_deletes():
    ask, done = screens([(f"-a {DOS_WORK}\\DEL", "ld"), (f"-a {DOS_WORK}\\DEL", "ldy")])
    expect("Delete this folder and everything in it?" in ask, "the question")
    expect("605.5K (35 files)" in ask, "what it would delete")
    expect("freed: 605.5K" in done.row(0), done.row(0))
    expect(not os.path.exists(os.path.join(WORK, "del", "src")), "src is gone from the disk")
    left = [name for name in os.listdir(os.path.join(WORK, "del")) if "DBLOCALFILE" in name]
    expect(not left, f"and DOSBox-X left no attribute files: {left}")
    expect("src/" not in done, "and from the treemap")
    return [ask, done]


@check
def delete_needs_a_pick():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "lEd")])  # entered: nothing picked in there
    expect("Delete" not in s, "no question for an entry nobody chose")
    return [s]


@check
def a_partial_delete_is_scanned_again():
    s, = screens([(f"-a {DOS_WORK}\\PART", "ldy")])
    expect("Could not delete all of it." in s, "the failure is shown")
    remaining = os.path.join(WORK, "part", "src", "deep", "er", "x.txt")
    expect(os.path.exists(remaining), "what could not go is still there")
    expect("freed: 517.6K" in s.row(0), "freed is what went: " + s.row(0))
    return [s]


@check
def rescan_keeps_the_totals():
    before, after = screens([(f"-a {DOS_WORK}\\FIX", ""), (f"-a {DOS_WORK}\\FIX", "R")])
    expect(before.row(0) == after.row(0), after.row(0))
    expect([t[:4] for t in before.tiles] == [t[:4] for t in after.tiles], "the same layout")
    return [before, after]


@check
def names_outside_ascii():
    s, = screens([(f"-a {DOS_WORK}\\WIDE", "")])
    expect("(3 files)" in s.row(0), "the folder is walked: " + s.row(0))
    expect("café/" in s, "and shown in CP437")
    return [s]


@check
def eight_three_names():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "lE")], lfn=False)
    expect("(46 files)" in s.row(0), s.row(0))
    expect("\\FIX\\SRC" in s.row(0), s.row(0))
    return [s]


@check
def an_8086_is_refused():
    out = []
    for setting, message in [("cpu cputype=8086", "needs an 80186")]:
        log = os.path.join(WORK, "OUT.TXT")
        if os.path.exists(log):
            os.remove(log)
        dosbox([f"TARGET\\DOS\\DISKONAU.EXE > {DOS_WORK}\\OUT.TXT"],
               extra=(setting, "cpu core=normal"), limit=60)
        text = open(log, encoding="cp437").read() if os.path.exists(log) else ""
        expect(message in text, f"{setting}: {text!r}")
    return out


def rust_tiles(area, sizes):
    tool = os.path.join(REPO, "target", "dos", "tiles", "debug", "dos-tiles")
    lines = subprocess.run([tool, *map(str, area), *map(str, sizes)], capture_output=True,
                           text=True, check=True).stdout.split("\n")
    tiles = [tuple(map(int, line.split())) for line in lines if line and line[0].isdigit()]
    small_line = [line for line in lines if line.startswith("small")][0].split()
    small = None if small_line[1] == "none" else (int(small_line[1]), int(small_line[2]))
    return tiles, small


@check
def layout_matches_the_rust_treemap():
    random.seed(7)
    cases = []
    for case in range(40):
        folder = os.path.join(WORK, "layout", f"C{case:02}")
        os.makedirs(folder)
        count = random.choice([1, 2, 3, 5, 8, 13, 20, 40, 80])
        kind = random.random()
        sizes = {}
        for i in range(count):
            if kind < 0.15:
                size = 0
            elif kind < 0.3:
                size = 4096
            else:
                size = int(random.paretovariate(0.7) * 1000)
            name = f"f{i:03}.bin"
            with open(os.path.join(folder, name), "wb") as f:
                f.truncate(size)
            sizes[name] = size
        cases.append(sizes)
    runs = [(f"-a {DOS_WORK}\\LAYOUT\\C{case:02}", keys)
            for keys in ("", "s") for case in range(len(cases))]
    shots = screens(runs)
    for case, ((_, keys), shot) in enumerate(zip(runs, shots)):
        area = BOARD_ALONE if keys else BOARD
        sizes = cases[case % len(cases)]
        expect(shot is not None, f"C{case:02} ran")
        order = sorted(sizes.items(), key=lambda kv: (-kv[1], kv[0]))
        want, small = rust_tiles(area, [size for _, size in order])
        got = [(idx, x, y, w, h) for (x, y, w, h, _, idx) in shot.tiles]
        expect(got == want, f"C{case:02}: {got} != {want}")
        expect(shot.small == small, f"C{case:02}: small {shot.small} != {small}")
    return []


@check
def the_side_panel():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "")])
    expect(s.row(1).startswith("C:\\target\\dos\\t\\fix"), "the folder: " + s.row(1))
    # at the top there is no share of the scan to give, as in the terminal viewer
    expect(s.row(2).startswith("1.1M · 46 files "), s.row(2))
    expect(s.row(3).startswith("3 folders, 6 files here"), s.row(3))
    expect(s.row(5)[:PV_COLS] == "src/               605.5K", s.row(5))
    expect(s.attrs[5][0] == 0x70, "the list's cursor, on its top row, has the keyboard")
    expect("SELECTED: src/" in s.row(23), "and the treemap shows its entry: " + s.row(23))
    return [s]


@check
def the_list_has_the_keyboard():
    down, tab, into, back = screens([
        (f"-a {DOS_WORK}\\FIX", "j"), (f"-a {DOS_WORK}\\FIX", "Tj"),
        (f"-a {DOS_WORK}\\FIX", "E"), (f"-a {DOS_WORK}\\FIX", "EX")])
    expect(down.row(6).startswith("big.bin") and down.attrs[6][0] == 0x70, down.row(6))
    expect("SELECTED: big.bin" in down.row(23), down.row(23))
    expect("SELECTED: big.bin" in tab.row(23), "Tab, j: down the treemap: " + tab.row(23))
    expect(tab.attrs[6][0] == 0x1F, "the list shows the treemap's entry, without the keyboard")
    expect(into.row(0).rstrip().endswith("fix\\src (605.5K, 35 files)"), into.row(0))
    expect(into.attrs[5][0] == 0x70, "entered: the list's top")
    expect(back.row(5).startswith("src/") and back.attrs[5][0] == 0x70,
           "back up: the cursor on the folder left")
    return [down, tab, into, back]


@check
def list_jumps():
    end, home, page = screens([(f"-a {DOS_WORK}\\FIX\\SRC\\DEEP", "Z"),
                               (f"-a {DOS_WORK}\\FIX\\SRC\\DEEP", "ZH"),
                               (f"-a {DOS_WORK}\\FIX\\SRC\\DEEP", "N")])
    expect("SELECTED: f00.o" in end.row(23), "End: the last entry " + end.row(23))
    expect("... 22 more" in home.text, "Home: the top again, the rest counted")
    expect("SELECTED: f21.o" in page.row(23), "Page Down: nine on " + page.row(23))
    return [end, home, page]


@check
def deleting_an_entry_without_a_tile():
    s, = screens([(f"-a {DOS_WORK}\\DEL2", "jjjjjjjdy")])
    expect(not os.path.exists(os.path.join(WORK, "del2", "tiny1")), "tiny1 is gone")
    expect("tiny1" not in s, "and from the list")
    return [s]


@check
def long_names_keep_their_ends():
    s, = screens([(f"-a {DOS_WORK}\\NAMES", "")])
    expect(s.row(5)[:PV_COLS] == "IMG_20240~1200.jpg   8.8K", s.row(5))
    return [s]


@check
def after_a_delete_the_neighbour():
    s, = screens([(f"-a {DOS_WORK}\\DEL3", "jdy")])     # big.bin, the second
    expect(not os.path.exists(os.path.join(WORK, "del3", "big.bin")), "big.bin is gone")
    expect(s.row(6).startswith("medium.dat") and s.attrs[6][0] == 0x70,
           "the cursor on the entry that took its place: " + s.row(6))
    expect("Delete" not in s, "and nothing asked again: it was placed, not picked")
    return [s]


@check
def a_tiny_jpeg_keeps_its_colour():
    s, = screens([(f"-a {DOS_WORK}\\PV\\TINYJPEG", "")])
    expect(s.preview_state == PREVIEWED, s.row(16))
    r, g, b, opaque = s.block_rgb[0]
    expect(opaque and r > 200 and 100 < g < 200 and b < 80, f"orange, not {r, g, b}")
    return [s]


@check
def the_panel_can_be_hidden():
    s, = screens([(f"-a {DOS_WORK}\\FIX", "s")])
    expect(s.row(1).startswith("┌"), "the treemap takes the whole width: " + s.row(1))
    return [s]


@check
def text_previews():
    s, = screens([(f"-a {DOS_WORK}\\PV\\TEXT", "")])
    body = [s.row(y)[:PV_COLS].rstrip() for y in range(16, 23)]
    expect(s.row(15).startswith("notes.txt · 99 "), "the caption: " + s.row(15))
    expect(body[:5] == ["a   b", "?[2Jcleared", "héllo wörld ½", "x" * PV_COLS, "last"],
           f"the lines: {body}")
    return [s]


@check
def previews_say_what_a_file_is():
    binary, empty, broken, head, late = screens([(f"-a {DOS_WORK}\\PV\\{f}", "")
                                                 for f in ("BINARY", "EMPTY", "BROKEN",
                                                           "HEAD64K", "BINARY64K")])
    expect(binary.row(16).startswith("binary file"), binary.row(16))
    expect(empty.row(16).startswith("empty file"), empty.row(16))
    expect(broken.row(16).startswith("PNG image, unreadable: t"), broken.row(16))
    expect(head.row(16).startswith("x" * PV_COLS), "a 64 KiB head, one line: " + head.row(16))
    expect(late.row(16).startswith("binary file"), "a NUL in the head's last byte: binary")
    return [binary, empty, broken, head, late]


@check
def picture_previews():
    runs = [(f"-a {DOS_WORK}\\PV\\{folder.upper()}", "") for folder, _, _ in PICTURES]
    shown = []
    for (folder, name, _), s in zip(PICTURES, screens(runs)):
        expect(s is not None, f"{folder} ran")
        shown.append(s)
        (width, height), cols, rows, want = reference_blocks(os.path.join(WORK, "pv", folder,
                                                                          name))
        expect(s.preview_state == PREVIEWED, f"{folder}: shown ({s.row(16).strip()})")
        kind = "PNG" if name.endswith("png") else "JPEG"
        expect(f"{kind} {width}x{height}" in s.row(15), f"{folder}: caption {s.row(15)}")
        expect((s.blocks_wide, s.blocks_high) == (cols, rows),
               f"{folder}: {s.blocks_wide}x{s.blocks_high} blocks, not {cols}x{rows}")
        error = worst = 0
        for got, ref in zip(s.block_rgb, want):
            if ref[3] < 100 or ref[3] > 156:    # clearly transparent or clearly not
                expect(bool(got[3]) == (ref[3] >= 128), f"{folder}: transparency")
            if got[3] and ref[3] >= 128:
                diff = max(abs(g - r) for g, r in zip(got[:3], ref[:3]))
                error += diff
                worst = max(worst, diff)
        mean = error / len(want)
        expect(mean < 6 and worst < 60, f"{folder}: colours off by {mean:.1f} on average, "
                                        f"{worst} at worst")
        used = cols * rows
        for got, colour in zip(s.block_rgb[:used], s.block_colour):   # each its nearest colour
            if got[3]:
                best = min(weighted(got, p) for p in s.palette)
                expect(weighted(got, s.palette[colour]) == best, f"{folder}: nearest colour")
    return shown


def hostile_pictures():
    """Damaged and hostile pictures, each alone in a folder: (folder, file name, bytes)."""
    random.seed(11)
    cases = []
    for source in ("rgb", "interlaced", "palette", "baseline", "progressive", "restarts"):
        name = "a.png" if os.path.exists(os.path.join(WORK, "pv", source, "a.png")) else "a.jpg"
        good = open(os.path.join(WORK, "pv", source, name), "rb").read()
        for cut in (10, 40, 100, len(good) // 3, len(good) - 5):
            cases.append((f"{source}-cut{cut}", name, good[:cut]))
        for flips in range(8):
            bad = bytearray(good)
            for _ in range(1 + flips * 3):
                at = random.randrange(8, len(bad))
                bad[at] = random.randrange(256)
            cases.append((f"{source}-flip{flips}", name, bytes(bad)))

    def png(width, height, depth, colour, interlace=0, chunks=(), data=None):
        def chunk(kind, body):
            import zlib
            return (struct.pack(">I", len(body)) + kind + body +
                    struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))
        import zlib
        head = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(
            ">IIBBBBB", width, height, depth, colour, 0, 0, interlace))
        body = b"".join(chunk(k, v) for k, v in chunks)
        if data is None:
            data = zlib.compress(b"\x00" * (height * (1 + width)))
        return head + body + chunk(b"IDAT", data) + chunk(b"IEND", b"")

    cases += [
        ("png-zero", "a.png", png(0, 0, 8, 0)),
        ("png-huge", "a.png", png(40000, 40000, 8, 2)),
        ("png-wide", "a.png", png(20000, 2, 16, 6)),
        ("png-one", "a.png", png(1, 1, 8, 0)),
        ("png-nopalette", "a.png", png(4, 4, 8, 3)),
        ("png-bigtrns", "a.png", png(4, 4, 8, 3, chunks=[(b"PLTE", b"\xff\x00\x00"),
                                                        (b"tRNS", b"\x80" * 300)])),
        ("png-baddepth", "a.png", png(4, 4, 3, 2)),
        ("png-emptyidat", "a.png", png(4, 4, 8, 0, data=b"")),
        ("png-garbageidat", "a.png", png(8, 8, 8, 2, data=bytes(range(256)) * 4)),
        ("png-interlace1px", "a.png", png(1, 1, 8, 2, interlace=1,
                                          data=__import__("zlib").compress(b"\x00\x01\x02\x03"))),
    ]
    soi, eoi = b"\xff\xd8", b"\xff\xd9"

    def seg(marker, body):
        return b"\xff" + bytes([marker]) + struct.pack(">H", len(body) + 2) + body

    sof = seg(0xC0, b"\x08" + struct.pack(">HH", 16, 16) + b"\x03" +
              b"\x01\x22\x00\x02\x11\x00\x03\x11\x00")
    cases += [
        ("jpeg-sosfirst", "a.jpg", soi + seg(0xDA, b"\x01\x01\x00\x00\x3f\x00") + eoi),
        ("jpeg-zerosize", "a.jpg", soi + seg(0xC0, b"\x08\x00\x00\x00\x00\x01\x01\x11\x00") + eoi),
        ("jpeg-nohuffman", "a.jpg", soi + sof + seg(0xDA, b"\x03\x01\x00\x02\x11\x03\x11"
                                                          b"\x00\x3f\x00") + b"\x12\x34" * 50 + eoi),
        ("jpeg-sampling3", "a.jpg", soi + seg(0xC0, b"\x08\x00\x10\x00\x10\x03\x01\x33\x00"
                                                   b"\x02\x11\x00\x03\x11\x00") + eoi),
        ("jpeg-sameids", "a.jpg", soi + seg(0xC0, b"\x08\x00\x10\x00\x10\x03\x01\x11\x00"
                                                 b"\x01\x11\x00\x01\x11\x00") + eoi),
        ("jpeg-bighuffman", "a.jpg", soi + seg(0xC4, b"\x00" + b"\xff" * 16 + b"\x00" * 40) + eoi),
        ("jpeg-restartnone", "a.jpg", soi + seg(0xDD, b"\x00\x01") + sof + eoi),
        ("jpeg-onlysoi", "a.jpg", soi),
        ("jpeg-markers", "a.jpg", soi + b"\xff" * 5000),
    ]
    return cases


@check
def damaged_pictures_do_not_hang():
    cases = hostile_pictures()
    for folder, name, data in cases:
        path = os.path.join(WORK, "hostile", folder, name)
        os.makedirs(os.path.dirname(path))
        with open(path, "wb") as f:
            f.write(data)
    shots = screens([(f"-a {DOS_WORK}\\HOSTILE\\{folder.upper()}", "") for folder, _, _ in cases],
                    limit=600)
    for (folder, _, _), shot in zip(cases, shots):
        expect(shot is not None, f"{folder}: no screen: it hung or crashed (and so did the rest)")
        # FF D8 alone is text, as preview::sniff has it
        allowed = (1,) if folder == "jpeg-onlysoi" else (2, PREVIEWED)
        expect(shot.preview_state in allowed, f"{folder}: state {shot.preview_state}")
    return []


# the treemap's area, x y width height: beside the side panel, and with it hidden
BOARD = (26, 1, 53, 21)
BOARD_ALONE = (0, 1, 79, 21)


def main():
    subprocess.run(["make", "-C", REPO, "dos"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["cargo", "build", "-q", "--manifest-path",
                    os.path.join(REPO, "viewers", "dos", "tiles", "Cargo.toml")],
                   env={**os.environ, "CARGO_TARGET_DIR": os.path.join(REPO, "target", "dos",
                                                                       "tiles")}, check=True)
    fixtures()
    picture_fixtures()
    failed = 0
    try:
        for function in CHECKS:
            if ONLY and not any(word in function.__name__ for word in ONLY):
                continue
            try:
                shown = function()
                print(f"ok    {function.__name__}")
            except AssertionError as error:
                failed += 1
                shown = []
                print(f"FAIL  {function.__name__}: {error}")
            for screen in shown if VERBOSE else []:
                if screen:
                    print(screen.text)
    finally:
        cleanup()
    print(f"{failed} failed" if failed else "all passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
