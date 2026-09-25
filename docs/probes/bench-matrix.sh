#!/usr/bin/env bash
# The standard measurement for docs/scan-roadmap.md: the machine, the filesystem and the disk,
# then warm, cold and root timings of duscape against diskus on the trees given, and the build
# profile. Writes docs/benchmarks/<host>-<date>.md; commit it.
#
#   docs/probes/bench-matrix.sh [--runs N] [--tag WORD] TREE...
#
# `--tag` names the file `<host>-<date>-<tag>.md`, for a second run on the same day (after a
# step, say) beside the baseline.
#
# Cold runs need root to drop caches (a sudoers line for `/usr/bin/tee /proc/sys/vm/drop_caches`
# does; on macOS, `/usr/sbin/purge`), root runs need `sudo -n <this checkout>/target/release/duscape`
# to work; what cannot be done is said in the file rather than silently skipped. `diskus` and
# `hyperfine` come from `cargo install`.
#
# On macOS WizTreeMac is timed too, in its export mode (`--export`, folders only, `--admin=0`),
# which scans, writes a CSV and exits. It refuses to unless it has Full Disk Access, which it
# checks for itself, so it is started through `open` (as its own responsible process; run from a
# shell it inherits the terminal's and fails the check, under `sudo` too). Its admin mode asks for
# a password in a dialog, so it has no root row. With folders and files both off it writes
# nothing and exits at once, so the timed export writes the folders.
set -euo pipefail

runs=3
tag=
trees=()
while [ $# -gt 0 ]; do
    case "$1" in
        --runs) runs=$2; shift ;;
        --tag) tag=-$2; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) trees+=("$1") ;;
    esac
    shift
done
[ ${#trees[@]} -gt 0 ] || { echo "usage: $0 [--runs N] TREE..." >&2; exit 2; }

here=$(cd "$(dirname "$0")/../.." && pwd)
bin=${DUSCAPE:-$here/target/release/duscape}
diskus=$(command -v diskus || echo "$HOME/.cargo/bin/diskus")
command -v hyperfine >/dev/null || { echo "missing: hyperfine (cargo install hyperfine)" >&2; exit 1; }
[ -x "$bin" ] || { echo "missing: $bin (cargo build --release -p duscape)" >&2; exit 1; }
host=$(hostname -s)
out=$here/docs/benchmarks/$host-$(date +%Y%m%d)$tag.md
macos=; [ "$(uname -s)" = Darwin ] && macos=1
can_drop=
if [ -n "$macos" ]; then
    drop='sync; sudo -n /usr/sbin/purge'
    if [ "$(id -u)" -eq 0 ]; then drop='sync; /usr/sbin/purge'; can_drop=1;
    elif sudo -n /usr/sbin/purge >/dev/null 2>&1; then can_drop=1; fi
else
    drop='sync; echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null'
    if [ "$(id -u)" -eq 0 ]; then drop='sync; echo 3 > /proc/sys/vm/drop_caches'; can_drop=1;
    elif echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null 2>&1; then can_drop=1; fi
fi
can_root=; if [ "$(id -u)" -eq 0 ] || sudo -n "$bin" --version >/dev/null 2>&1; then can_root=1; fi
has_diskus=; [ -x "$diskus" ] && has_diskus=1
wizapp=/Applications/WizTreeMac.app
wiztree="open -W -n -a $wizapp --args"
wizcsv=$(mktemp -d)/bench-matrix-wiztree.csv
has_wiztree=; wiztree_why="not installed"
if [ -n "$macos" ] && [ -d "$wizapp" ]; then
    # A trial export: without Full Disk Access it writes nothing (and `open` still exits 0).
    $wiztree "$here/docs" "--export=$wizcsv" --admin=0 --exportfolders=1 --exportfiles=0 >/dev/null 2>&1 || true
    if [ -s "$wizcsv" ]; then
        has_wiztree=1
        # What starting the app costs, apart from the scan: the export of an empty folder.
        empty=$(mktemp -d)
        hyperfine --runs 3 --export-json "$empty.json" "$wiztree '$empty' '--export=$wizcsv' --admin=0 --exportfolders=1 --exportfiles=0" >/dev/null 2>&1 || true
        wiz_launch=$(python3 -c 'import json,sys; print("%.2f s" % json.load(open(sys.argv[1]))["results"][0]["mean"])' "$empty.json" 2>/dev/null || echo "?")
        rm -rf "$empty" "$empty.json"
    else wiztree_why="installed but its export wrote nothing: grant it Full Disk Access"; fi
fi
wiz_figures=()

# --- the machine ---
if [ -n "$macos" ]; then
    cpu=$(sysctl -n machdep.cpu.brand_string)
    cores="$(sysctl -n hw.ncpu) ($(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null || echo ?) performance)"
    mem="$(( $(sysctl -n hw.memsize) / 1073741824 )) GiB"
    virt=$([ "$(sysctl -n kern.hv_vmm_present 2>/dev/null)" = 1 ] && echo "a virtual machine" || echo none)
    distro="macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion))"
else
    cpu=$(grep -m1 "model name" /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ *//' || echo unknown)
    cores=$(nproc)
    mem=$(free -g 2>/dev/null | awk '/^Mem:/{print $2 " GiB"}' || echo unknown)
    virt=$(systemd-detect-virt 2>/dev/null || true); [ -n "$virt" ] || virt=unknown  # exits 1 for "none"
    distro=$(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || uname -s)
fi
{
    echo "# $host — $(date +%Y-%m-%d)"
    echo
    echo "| | |"
    echo "| --- | --- |"
    echo "| machine | $cpu, $cores cores, $mem, virtualisation: $virt |"
    echo "| system | $distro, kernel $(uname -r) |"
    echo "| duscape | $(git -C "$here" rev-parse --short HEAD) ($(git -C "$here" status --porcelain -uno | grep -q . && echo "with local changes" || echo clean)), release profile |"
    echo "| diskus | $([ -n "$has_diskus" ] && "$diskus" --version || echo "not installed") |"
    [ -n "$macos" ] && echo "| WizTreeMac | $([ -n "$has_wiztree" ] && echo "$(defaults read /Applications/WizTreeMac.app/Contents/Info CFBundleShortVersionString 2>/dev/null), timed in export mode (folders only, through \`open\`; an empty folder's export takes $wiz_launch); no root row, its admin mode asks for a password" || echo "$wiztree_why") |"
    echo "| cold runs | $([ -n "$can_drop" ] && echo "yes (caches dropped before each)" || echo "no: cannot drop caches without root") |"
    echo "| root runs | $([ -n "$can_root" ] && echo "yes" || echo "no: sudo -n $bin not allowed") |"
    echo "| runs per cell | $runs |"
    echo
    echo "## Trees"
    echo
    echo "| tree | filesystem | device | disk | entries | size |"
    echo "| --- | --- | --- | --- | --- | --- |"
} > "$out"
for tree in "${trees[@]}"; do
    tree=$(realpath "$tree")
    if [ -n "$macos" ]; then
        # The volume, then the physical store under its APFS container: that is the disk.
        src=$(df -P "$tree" | awk 'NR==2{print $1}')
        field() { diskutil info "$1" 2>/dev/null | awk -v k="$2:" '{ sub(/^ */, "") } index($0, k) == 1 { sub(/^[^:]*: */, ""); print; exit }'; }
        fs=$(field "$src" "File System Personality"); [ -n "$fs" ] || fs=unknown
        store=$(field "$src" "APFS Physical Store"); [ -n "$store" ] || store=$(basename "$src")
        disk=$(field "$store" "Part of Whole"); [ -n "$disk" ] || disk=$store
        model=$(field "$disk" "Device / Media Name")
        kind=$([ "$(field "$disk" "Solid State")" = Yes ] && echo SSD || echo HDD)
        tran=$(field "$disk" "Protocol")
    else
        fs=$(findmnt -no FSTYPE -T "$tree" 2>/dev/null || echo unknown)
        src=$(findmnt -no SOURCE -T "$tree" 2>/dev/null || echo unknown)
        disk=$(lsblk -no PKNAME "$src" 2>/dev/null | head -1); [ -z "$disk" ] && disk=$(basename "$src")
        model=$(cat /sys/class/block/$disk/device/model 2>/dev/null | sed 's/ *$//' || true)
        rota=$(cat /sys/class/block/$disk/queue/rotational 2>/dev/null || echo "?")
        kind=$([ "$rota" = 1 ] && echo HDD || echo SSD)
        tran=$(lsblk -no TRAN /dev/$disk 2>/dev/null | head -1)
    fi
    line=$("$bin" --benchmark --bench-stage sharded "$tree" 2>/dev/null | grep "^sharded" || true)
    entries=$(echo "$line" | awk '{print $3}'); size=$(echo "$line" | grep -o '[0-9.]* [KMGT]iB' | head -1)
    hl=$(echo "$line" | grep -o '[0-9]* hard-linked' || true)
    echo "| \`$tree\` | $fs | $src | /dev/$disk ${model:+($model)} $kind${tran:+, $tran} | $entries${hl:+, $hl} | $size |" >> "$out"
    if [ -n "$has_wiztree" ]; then
        # WizTreeMac's own count of the same tree, for the sizes cross-check: its CSV's first
        # data row is the tree itself — Size, Allocated, ..., Files, Folders.
        rm -f "$wizcsv"
        $wiztree "$tree" "--export=$wizcsv" --admin=0 --exportfolders=1 --exportfiles=0 >/dev/null 2>&1 || true
        row=$(sed -n 3p "$wizcsv" 2>/dev/null | python3 -c 'import csv,sys; print("\t".join(next(csv.reader(sys.stdin), [])))' 2>/dev/null || true)
        if [ -n "$row" ]; then
            allocated=$(echo "$row" | cut -f3); files=$(echo "$row" | cut -f6); folders=$(echo "$row" | cut -f7)
            human=$(awk -v b="$allocated" 'BEGIN{ if (b>=2^30) printf "%.1f GiB", b/2^30; else if (b>=2^20) printf "%.1f MiB", b/2^20; else printf "%.1f KiB", b/2^10 }')
            wiz_figures+=("| \`$tree\` | $((files + folders)) | $human ($allocated B) |")
        fi
    fi
done
if [ ${#wiz_figures[@]} -gt 0 ]; then
    { echo; echo "WizTreeMac's figures for the same trees (files and folders, allocated):"; echo
      echo "| tree | entries | allocated |"; echo "| --- | --- | --- |"; printf '%s\n' "${wiz_figures[@]}"; } >> "$out"
fi

bench() { # title dir hyperfine-args...
    local title=$1 dir=$2; shift 2
    local md; md=$(mktemp)
    local cmds=()
    [ -n "$has_diskus" ] && cmds+=(-n "diskus" "'$diskus' --directories excluded '$dir' >/dev/null 2>&1")
    cmds+=(-n "duscape sharded" "'$bin' --benchmark --bench-stage sharded '$dir' >/dev/null")
    cmds+=(-n "duscape refined" "'$bin' --benchmark --bench-stage refined '$dir' >/dev/null")
    if [ -n "$can_root" ] && [ "$(id -u)" -ne 0 ]; then
        # As root the Linux scan reads ext4 from the device; the kernel walk as root is the
        # same privilege without that, so the two rows separate the device read from the rest.
        cmds+=(-n "duscape sharded, as root" "sudo -n '$bin' --benchmark --bench-stage sharded '$dir' >/dev/null")
        # macOS has no device read, so there the one root row is the whole comparison.
        [ -n "$macos" ] || cmds+=(-n "duscape sharded, as root, kernel walk" "sudo -n '$bin' --benchmark --bench-stage sharded --no-device-read '$dir' >/dev/null")
    fi
    if [ -n "$has_wiztree" ]; then
        cmds+=(-n "WizTreeMac export" "$wiztree '$dir' '--export=$wizcsv' --admin=0 --exportfolders=1 --exportfiles=0 >/dev/null 2>&1")
    fi
    hyperfine --runs "$runs" "$@" --export-markdown "$md" "${cmds[@]}" >/dev/null 2>&1 || true
    { echo "### $title: $dir"; echo; cat "$md"; echo; } >> "$out"
    rm -f "$md"
}

echo >> "$out"; echo "## Timings" >> "$out"; echo >> "$out"
for tree in "${trees[@]}"; do
    tree=$(realpath "$tree")
    echo "== warm $tree" >&2
    bench warm "$tree" --warmup 1
    if [ -n "$can_drop" ]; then
        echo "== cold $tree" >&2
        bench cold "$tree" --prepare "$drop"
    fi
done

echo "## Build profile (tree-only, warm)" >> "$out"; echo >> "$out"
for tree in "${trees[@]}"; do
    tree=$(realpath "$tree")
    { echo "### $tree"; echo; echo '```'; "$bin" --benchmark --bench-stage tree-only --bench-profile "$tree" 2>&1 | grep -E "^  |^tree-only" | grep -v "threads:"; echo '```'; echo; } >> "$out"
done
echo "wrote $out"
