#!/usr/bin/env bash
#
# ANSWERED 2026-09-23 by fixtures/fs (the `compression` scenario): blocks ~= logical.
# stx_blocks reports the uncompressed size. diskonaut, run as root, now reads the extent items
# instead (BTRFS_IOC_TREE_SEARCH_V2); as a user it still reports the uncompressed size.
#
# Does `stat` (and therefore diskonaut's on-disk size, which reads stx_blocks)
# already reflect btrfs transparent compression? This cannot be answered on the
# development machine, which has no btrfs volume, so run it on a real one.
#
# Usage:  ./btrfs_compression_check.sh /path/on/a/btrfs/volume
#
# It writes one highly compressible file with compression forced on, then prints
# the logical length, the allocated blocks (what diskonaut uses), what du and
# compsize say, and a verdict. No root needed for the write; `compsize` is
# optional and only sharpens the picture.
#
# The question this settles:
#   * blocks << logical  -> stx_blocks already reflects compression.
#       diskonaut is correct on btrfs with NO code change.
#   * blocks ~= logical  -> stx_blocks reports the uncompressed size.
#       diskonaut would over-report, and the fix is BTRFS_IOC_TREE_SEARCH
#       (what compsize reads), NOT FIEMAP — FIEMAP's extent lengths are
#       logical and never expose the compressed size.

set -eu

target="${1:-}"
if [ -z "$target" ]; then
  echo "usage: $0 /path/on/a/btrfs/volume" >&2
  exit 2
fi
if [ ! -d "$target" ]; then
  echo "error: '$target' is not a directory" >&2
  exit 2
fi

fstype=$(stat -f -c %T "$target" 2>/dev/null || echo unknown)
if [ "$fstype" != "btrfs" ]; then
  echo "warning: '$target' is $fstype, not btrfs — the verdict only means something on btrfs" >&2
fi

work="$target/.diskonaut_btrfs_probe.$$"
cleanup() { rm -f "$work"; }
trap cleanup EXIT

# Force compression on the file itself, independent of the mount's compress= option.
: > "$work"
chattr +c "$work" 2>/dev/null || echo "note: chattr +c failed; relying on the mount's compress= option" >&2

# 8 MiB of one repeating byte: compresses to almost nothing.
yes "AAAAAAAAAAAAAAAA" | head -c $((8 * 1024 * 1024)) > "$work"
sync "$work" 2>/dev/null || sync

logical=$(stat -c %s "$work")
blocks512=$(stat -c %b "$work")
blocksize=$(stat -c %B "$work")
on_disk=$((blocks512 * blocksize))
du_bytes=$(du --block-size=1 "$work" | cut -f1)

printf '%-28s %s\n' "logical length (stx_size):" "$logical"
printf '%-28s %s\n' "allocated (stx_blocks*512):" "$on_disk   <- what diskonaut charges"
printf '%-28s %s\n' "du --block-size=1:" "$du_bytes"
if command -v compsize >/dev/null 2>&1; then
  echo "--- compsize (authoritative disk usage) ---"
  compsize "$work" || true
else
  echo "note: install 'compsize' for the authoritative on-disk figure" >&2
fi

echo
if [ "$on_disk" -lt $((logical / 2)) ]; then
  echo "VERDICT: stx_blocks reflects compression. diskonaut is correct on btrfs, no code change."
else
  echo "VERDICT: stx_blocks does NOT reflect compression (allocated ~= logical)."
  echo "         diskonaut would over-report compressed files. The fix is BTRFS_IOC_TREE_SEARCH"
  echo "         (compsize's approach), not FIEMAP."
fi
