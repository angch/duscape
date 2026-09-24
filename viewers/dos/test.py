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
    for copy in ("del", "part"):
        shutil.copytree(fix, os.path.join(WORK, copy))
    # a read-only file deleted with the rest; a folder that refuses to let go of what is in it
    os.chmod(os.path.join(WORK, "del", "src", "lib.rs"), 0o444)
    os.chmod(os.path.join(WORK, "part", "src", "deep", "er"), 0o555)
    # a long name outside ASCII that CP437 has (DOSBox-X hides one it has not, so the short-name
    # path for those cannot be tried here)
    write(os.path.join(WORK, "wide", "café", "inside.txt"), 5000)
    write(os.path.join(WORK, "wide", "plain.txt"), 3000)


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
    expect("medium.dat" in s and "big.bin" not in s, "the two largest are zoomed past")
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
def old_machines_are_refused():
    out = []
    for setting, message in [("cpu cputype=286", "needs a 386"),
                             ("cpu fpu=false", "needs a math coprocessor")]:
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
    shots = screens([(f"-a {DOS_WORK}\\LAYOUT\\C{case:02}", "") for case in range(len(cases))])
    area = BOARD
    for case, (sizes, shot) in enumerate(zip(cases, shots)):
        expect(shot is not None, f"C{case:02} ran")
        order = sorted(sizes.items(), key=lambda kv: (-kv[1], kv[0]))
        want, small = rust_tiles(area, [size for _, size in order])
        got = [(idx, x, y, w, h) for (x, y, w, h, _, idx) in shot.tiles]
        expect(got == want, f"C{case:02}: {got} != {want}")
        expect(shot.small == small, f"C{case:02}: small {shot.small} != {small}")
    return []


# the treemap's area: x, y, width, height
BOARD = (0, 1, 79, 21)


def main():
    subprocess.run(["make", "-C", REPO, "dos"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["cargo", "build", "-q", "--manifest-path",
                    os.path.join(REPO, "viewers", "dos", "tiles", "Cargo.toml")],
                   env={**os.environ, "CARGO_TARGET_DIR": os.path.join(REPO, "target", "dos",
                                                                       "tiles")}, check=True)
    fixtures()
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
