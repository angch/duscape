"""Whether Debian packages what a Debian build of duscape needs: the direct dependencies of the
crates a glibc Linux build compiles, with their version requirements from Cargo.toml, against
the versions sources.debian.org lists in any suite. Debian builds against the requirements, not
our Cargo.lock, and supplies the indirect dependencies through its own packages of the direct
ones. Run from the repository's root before a Debian upload, or after adding a dependency:

    python3 docs/probes/debian-deps.py

2026-10-04, after fontdue -> ab_glyph, dua-core -> the portable walk, embed-manifest -> our own
resource writer, and x11rb's range opened to Debian's 0.13: every one is in Debian. A crate that
is optional, or for another platform, must be in Debian too, or patched out: Cargo resolves those
as well (docs/packaging.md, "Getting into Debian itself")."""
import json, re, subprocess, sys, time, urllib.request

meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                                 capture_output=True, text=True, check=True).stdout)
LINUX = ["duscape", "libduscape", "duscape-scan", "duscape-viewer", "duscape-linux"]

def applies(target):
    """Whether a [target.'cfg(..)'] table applies to x86_64 Linux with glibc."""
    if target is None:
        return True, ""
    t = target.replace(" ", "")
    if 'target_env="musl"' in t:
        return False, "musl only"
    if "windows" in t or 'target_os="macos"' in t:
        return False, target
    return True, target

def parse(v):
    m = re.match(r"(\d+)\.(\d+)(?:\.(\d+))?", v)
    return [int(m.group(1)), int(m.group(2)), int(m.group(3) or 0)] if m else None

def satisfies(req, v):
    """Cargo's default (caret) requirements, and the comparisons; enough for what this
    workspace writes."""
    b = parse(v)
    if b is None:
        return False
    for part in req.split(","):
        part = part.strip()
        op = re.match(r"(\^|>=|<=|<|>|=|~)?\s*(.*)", part)
        kind, ver = op.group(1) or "^", op.group(2)
        nums = [int(x) for x in ver.split(".") if x.isdigit()]
        a = nums + [0] * (3 - len(nums))
        if kind == ">=":
            if b < a: return False
        elif kind == "<":
            if b >= a: return False
        elif kind == "<=":
            if b > a: return False
        elif kind == ">":
            if b <= a: return False
        elif kind == "=":
            if b[:len(nums)] != nums: return False
        else:  # caret (and ~ treated as caret on minor: close enough here)
            if b < a: return False
            if a[0] != 0:
                if b[0] != a[0]: return False
            elif len(nums) >= 2 and a[1] != 0:
                if b[0] != 0 or b[1] != a[1]: return False
            elif len(nums) >= 3:
                if b[:3] != a[:3]: return False
            else:
                if b[0] != 0 or (len(nums) >= 2 and b[1] != a[1]): return False
    return True

def debian(name):
    url = f"https://sources.debian.org/api/src/rust-{name.replace('_', '-')}/"
    for _ in range(3):
        try:
            with urllib.request.urlopen(url, timeout=30) as r:
                d = json.load(r)
            return sorted({v["version"].split("-")[0].split("+")[0] for v in d.get("versions", [])},
                          key=lambda v: parse(v) or [0])
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return []
        except Exception:
            time.sleep(2)
    return None

seen = {}
for p in meta["packages"]:
    if p["name"] not in LINUX:
        continue
    for d in p["dependencies"]:
        if d.get("path") or d["name"] in LINUX:
            continue  # the workspace's own crates
        ok, why = applies(d.get("target"))
        key = (d["name"], d["req"])
        entry = seen.setdefault(key, {"used_by": set(), "kinds": set(), "skip": why if not ok else None})
        entry["used_by"].add(p["name"]); entry["kinds"].add(d["kind"] or "normal")
        if ok:
            entry["skip"] = None

for (name, req), e in sorted(seen.items()):
    if e["skip"]:
        print(f"  (not in a Debian build: {name} {req} — {e['skip']})")
        continue
    versions = debian(name)
    if versions is None:
        verdict = "lookup failed"
    elif not versions:
        verdict = "NOT IN DEBIAN"
    elif any(satisfies(req, v) for v in versions):
        verdict = "ok (" + ", ".join(v for v in versions if satisfies(req, v))[-40:] + ")"
    else:
        verdict = "NEEDS A NEWER VERSION THAN DEBIAN HAS (" + ", ".join(versions[-3:]) + ")"
    print(f"{name:20} {req:10} {'/'.join(sorted(e['kinds'])):12} {', '.join(sorted(e['used_by'])):45} {verdict}")
    time.sleep(0.1)
