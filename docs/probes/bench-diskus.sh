#!/usr/bin/env bash
# Benchmark duscape's scan against diskus with hyperfine, warm and cold.
#
#   docs/probes/bench-diskus.sh [--warm|--cold|--both] [--runs N] DIR...
#
# Warm runs need nothing. Cold runs drop the kernel's page, dentry and inode
# caches before every timing (`echo 3 > /proc/sys/vm/drop_caches`), which needs
# root: run as root, or allow `/usr/bin/tee /proc/sys/vm/drop_caches` without
# a password in sudoers, or `sudo -v` first so the cached credential carries
# the prepare steps. Without it the cold set is skipped and said so.
#
# duscape runs `--benchmark`, headless, at two stages: `sharded` is the walk
# and tree build exactly as the app runs them, `refined` adds the second pass
# over small files that may share extents. diskus is run with
# `--directories excluded` so that its total matches duscape's, which never
# counts a directory's own blocks; the walk it does is the same either way.
#
# Results go to $OUT (default: bench-diskus-<host>-<date>.md) as a Markdown
# table per set, plus JSON beside it.
set -euo pipefail

mode=both
runs=5
dirs=()
while [ $# -gt 0 ]; do
    case "$1" in
        --warm) mode=warm ;;
        --cold) mode=cold ;;
        --both) mode=both ;;
        --runs) runs=$2; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) dirs+=("$1") ;;
    esac
    shift
done
[ ${#dirs[@]} -gt 0 ] || { echo "usage: $0 [--warm|--cold|--both] [--runs N] DIR..." >&2; exit 2; }

here=$(cd "$(dirname "$0")/../.." && pwd)
duscape=${DUSCAPE:-$here/target/release/duscape}
diskus=${DISKUS:-$(command -v diskus || echo "$HOME/.cargo/bin/diskus")}
for bin in hyperfine "$duscape" "$diskus"; do
    command -v "$bin" >/dev/null || { echo "missing: $bin" >&2; exit 1; }
done
out=${OUT:-bench-diskus-$(hostname -s)-$(date +%Y%m%d-%H%M).md}

# `tee` rather than a shell redirect, so one sudoers line suffices:
#   angch ALL=(root) NOPASSWD: /usr/bin/tee /proc/sys/vm/drop_caches
if [ "$(id -u)" -eq 0 ]; then
    prepare='sync; echo 3 > /proc/sys/vm/drop_caches'
elif echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null 2>&1; then
    prepare='sync; echo 3 | sudo -n /usr/bin/tee /proc/sys/vm/drop_caches >/dev/null'
else
    prepare=
fi

{
    echo "# duscape vs diskus"
    echo
    echo "- host: $(hostname -s), $(nproc) cpus, kernel $(uname -r)"
    echo "- duscape: $(git -C "$here" rev-parse --short HEAD 2>/dev/null || echo ?) ($duscape)"
    echo "- diskus: $("$diskus" --version)"
    echo "- hyperfine: $(hyperfine --version)"
    echo "- runs: $runs per command"
    echo
} > "$out"

bench() { # set dir hyperfine-args...
    local set=$1 dir=$2; shift 2
    local tag; tag=$(echo "$dir" | tr '/' '_' | sed 's/^_//')
    local fs; fs=$(findmnt -no FSTYPE -T "$dir")
    local json=${out%.md}-$set-$tag.json md=${out%.md}-$set-$tag.tmp
    echo "== $set: $dir ($fs)"
    hyperfine --runs "$runs" "$@" \
        --export-json "$json" --export-markdown "$md" \
        -n "diskus" "'$diskus' --directories excluded '$dir' >/dev/null 2>&1" \
        -n "duscape sharded" "'$duscape' --benchmark --bench-stage sharded '$dir' >/dev/null" \
        -n "duscape refined" "'$duscape' --benchmark --bench-stage refined '$dir' >/dev/null"
    {
        echo "## $set: $dir ($fs)"
        echo
        cat "$md"
        echo
    } >> "$out"
    rm -f "$md"
}

for dir in "${dirs[@]}"; do
    dir=$(realpath "$dir")
    if [ "$mode" != cold ]; then
        bench warm "$dir" --warmup 2
    fi
    if [ "$mode" != warm ]; then
        if [ -n "$prepare" ]; then
            bench cold "$dir" --prepare "$prepare"
        else
            echo "cold: skipped, dropping caches needs root (run as root, or sudo -v first)" >&2
            echo "## cold: $dir — skipped, no root to drop caches" >> "$out"
        fi
    fi
done
echo "wrote $out"
