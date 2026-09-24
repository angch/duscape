#!/usr/bin/env python3
"""SOFTFP.ASM against the host's IEEE doubles: tests/TESTFP.ASM runs random operations in
DOSBox-X on an emulated 286 without a coprocessor, and every result must be the same bits.

    python3 viewers/dos/tests/softfp.py [count]
"""
import math, os, random, struct, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
WORK = os.path.join(REPO, "target", "dos", "fp")
COUNT = int(sys.argv[1]) if len(sys.argv) > 1 else 20000


def dosbox(commands, cpu286=False):
    args = ["dosbox-x", "-silent", "-fastlaunch", "-nogui", "-nomenu", "-defaultconf",
            "-time-limit", "600", "-set", "cpu cycles=max"]
    if cpu286:
        args += ["-set", "cpu cputype=286", "-set", "cpu fpu=false", "-set", "cpu core=normal"]
    args += ["-c", f"mount c {REPO}", "-c", "c:"]
    for command in commands:
        args += ["-c", command]
    subprocess.run(args + ["-c", "exit"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def value(rng):
    kind = rng.random()
    if kind < 0.05:
        return rng.choice([0.0, -0.0, math.inf, -math.inf, math.nan, 0.5, 1.5, 2.5, 3.0])
    if kind < 0.35:        # sizes and coordinates: integers and simple fractions
        return float(rng.randrange(0, 1 << rng.choice([4, 8, 16, 32, 53]))) / rng.choice(
            [1, 2, 4, 8, 3, 10, 7, 1024])
    if kind < 0.5:         # ties for the roundings
        return rng.randrange(0, 70000) + 0.5
    return rng.uniform(-1, 1) * 2.0 ** rng.randrange(-40, 60)


def expected(op, k, a, b):
    try:
        if op == 0: return a + b
        if op == 1: return a - b
        if op == 2: return a * b
        if op == 3:
            if b == 0:
                return math.nan if a == 0 or math.isnan(a) else math.copysign(math.inf, a) * \
                    math.copysign(1, b)
            return a / b
    except (OverflowError, ZeroDivisionError):
        return None
    if op == 4:
        if math.isnan(a) or math.isnan(b): return 3
        return 0 if a < b else 1 if a == b else 2
    if op in (5, 6):
        x = a * k if op == 6 else a
        if math.isnan(a) or a <= 0: return 0
        if math.isinf(a): return 65535
        if op == 5:
            r = math.floor(a + 0.5) if a - math.floor(a) != 0.5 else math.floor(a) + 1
        else:
            from fractions import Fraction
            exact = Fraction(a) * k
            r = round(exact)   # Fraction rounds half to even
        return min(int(r), 65535)


def main():
    os.makedirs(WORK, exist_ok=True)
    rng = random.Random(5)
    records, wants = [], []
    while len(records) < COUNT:
        op = rng.randrange(8)
        k = rng.choice([1, 10, 100, 1000])
        if op == 7:
            n = rng.randrange(0, 1 << rng.choice([8, 32, 53, 60, 64]))
            records.append(struct.pack("<BHQ8x", op, k, n))
            wants.append(("f", float(n)))
            continue
        a, b = value(rng), value(rng)
        want = expected(op, k, a, b)
        if want is None:
            continue
        if isinstance(want, float) and want != 0 and not math.isinf(want) and not math.isnan(
                want) and abs(want) < 2.0 ** -1000:
            continue   # subnormal: not modelled
        records.append(struct.pack("<BHdd", op, k, a, b))
        wants.append(("f", want) if isinstance(want, float) else ("w", want))
    open(os.path.join(WORK, "FPIN.BIN"), "wb").write(b"".join(records))
    com = os.path.join(WORK, "TESTFP.COM")
    if os.path.exists(com):
        os.remove(com)
    dosbox(["TARGET\\DOS\\CWSDPMI -p",
            "TARGET\\DOS\\FASM VIEWERS\\DOS\\TESTS\\TESTFP.ASM TARGET\\DOS\\FP\\TESTFP.COM"
            " > TARGET\\DOS\\FP\\FASM.TXT"])
    if not os.path.exists(com):
        sys.exit(open(os.path.join(WORK, "FASM.TXT"), encoding="cp437").read())
    dosbox(["cd TARGET\\DOS\\FP", "TESTFP"], cpu286=True)
    out = open(os.path.join(WORK, "FPOUT.BIN"), "rb").read()
    at, bad = 0, 0
    for record, (kind, want) in zip(records, wants):
        if kind == "f":
            got = struct.unpack("<d", out[at:at + 8])[0] if at + 8 <= len(out) else None
            at += 8
            same = got is not None and (struct.pack("<d", got) == struct.pack("<d", want) or
                                        (math.isnan(got) and math.isnan(want)) or
                                        (got == 0 and want == 0))
        else:
            got = struct.unpack("<H", out[at:at + 2])[0] if at + 2 <= len(out) else None
            at += 2
            same = got == want
        if not same:
            bad += 1
            if bad <= 15:
                op, k = record[0], struct.unpack("<H", record[1:3])[0]
                a, b = struct.unpack("<dd", record[3:19])
                print(f"op {op} k {k}: {a!r} {b!r}: got {got!r}, want {want!r}")
    print(f"{len(records)} operations, {bad} wrong")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
