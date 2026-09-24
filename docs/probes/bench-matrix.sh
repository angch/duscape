#!/usr/bin/env bash
# The standard measurement for docs/scan-roadmap.md: the machine, the filesystem and the disk,
# then warm, cold and root timings of diskonaut against diskus on the trees given, and the build
# profile. Writes docs/benchmarks/<host>-<date>.md; commit it.
#
#   docs/probes/bench-matrix.sh [--runs N] TREE...
#
# Cold runs need root to drop caches (a sudoers line for `/usr/bin/tee /proc/sys/vm/drop_caches`
# does), root runs need `sudo -n <this checkout>/target/release/diskonaut` to work; what cannot be
# done is said in the file rather than silently skipped. `diskus` and `hyperfine` come from
# `cargo install`.
set -euo pipefail

runs=3
trees=()
while [ $# -gt 0 ]; do
    case "$1" in
        --runs) runs=$2; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) trees+=("$1") ;;
    esac
    shift
done
[ ${#trees[@]} -gt 0 ] || { echo "usage: $0 [--runs N] TREE..." >&2; exit 2; }

here=$(cd "$(dirname "$0")/../.." && pwd)
bin=${DISKONAUT:-$here/target/release/diskonaut}
diskus=$(command -v diskus || echo "$HOME/.cargo/bin/diskus")
command -v hyperfine >/dev/null || { echo "missing: hyperfine (cargo install hyperfine)" >&2; exit 1; }
[ -x "$bin" ] || { echo "missing: $bin (cargo build --release -p diskonaut-angch)" >&2; exit 1; }
host=$(hostname -s)
out=$here/docs/benchmarks/$host-$(date +%Y%m%d).md
drop='sync; echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null'
can_drop=; if [ "$(id -u)" -eq 0 ]; then drop='sync; echo 3 > /proc/sys/vm/drop_caches'; can_drop=1;
elif echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null 2>&1; then can_drop=1; fi
can_root=; if [ "$(id -u)" -eq 0 ] || sudo -n "$bin" --version >/dev/null 2>&1; then can_root=1; fi
has_diskus=; [ -x "$diskus" ] && has_diskus=1

# --- the machine ---
cpu=$(grep -m1 "model name" /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ *//' || sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)
cores=$(nproc 2>/dev/null || sysctl -n hw.ncpu)
mem=$(free -g 2>/dev/null | awk '/^Mem:/{print $2 " GiB"}' || echo unknown)
virt=$(systemd-detect-virt 2>/dev/null || echo unknown)
distro=$(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || uname -s)
{
    echo "# $host — $(date +%Y-%m-%d)"
    echo
    echo "| | |"
    echo "| --- | --- |"
    echo "| machine | $cpu, $cores cores, $mem, virtualisation: $virt |"
    echo "| system | $distro, kernel $(uname -r) |"
    echo "| diskonaut | $(git -C "$here" rev-parse --short HEAD) ($(git -C "$here" status --porcelain | grep -q . && echo "with local changes" || echo clean)), release profile |"
    echo "| diskus | $([ -n "$has_diskus" ] && "$diskus" --version || echo "not installed") |"
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
    fs=$(findmnt -no FSTYPE -T "$tree" 2>/dev/null || echo unknown)
    src=$(findmnt -no SOURCE -T "$tree" 2>/dev/null || echo unknown)
    disk=$(lsblk -no PKNAME "$src" 2>/dev/null | head -1); [ -z "$disk" ] && disk=$(basename "$src")
    model=$(cat /sys/class/block/$disk/device/model 2>/dev/null | sed 's/ *$//' || true)
    rota=$(cat /sys/class/block/$disk/queue/rotational 2>/dev/null || echo "?")
    kind=$([ "$rota" = 1 ] && echo HDD || echo SSD)
    tran=$(lsblk -no TRAN /dev/$disk 2>/dev/null | head -1)
    line=$("$bin" --benchmark --bench-stage sharded "$tree" 2>/dev/null | grep "^sharded" || true)
    entries=$(echo "$line" | awk '{print $3}'); size=$(echo "$line" | grep -o '[0-9.]* [KMGT]iB' | head -1)
    hl=$(echo "$line" | grep -o '[0-9]* hard-linked' || true)
    echo "| \`$tree\` | $fs | $src | /dev/$disk ${model:+($model)} $kind${tran:+, $tran} | $entries${hl:+, $hl} | $size |" >> "$out"
done

bench() { # title dir hyperfine-args...
    local title=$1 dir=$2; shift 2
    local md; md=$(mktemp)
    local cmds=()
    [ -n "$has_diskus" ] && cmds+=(-n "diskus" "'$diskus' --directories excluded '$dir' >/dev/null 2>&1")
    cmds+=(-n "diskonaut sharded" "'$bin' --benchmark --bench-stage sharded '$dir' >/dev/null")
    cmds+=(-n "diskonaut refined" "'$bin' --benchmark --bench-stage refined '$dir' >/dev/null")
    if [ -n "$can_root" ] && [ "$(id -u)" -ne 0 ] && [ "$title" = cold ]; then
        cmds+=(-n "diskonaut sharded, as root" "sudo -n '$bin' --benchmark --bench-stage sharded '$dir' >/dev/null")
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
