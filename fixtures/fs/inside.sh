#!/usr/bin/env bash
# The root half of fixtures/fs/run.sh: make each filesystem on a loopback image, exercise its
# quirks, and check duscape against what the volume really holds. Runs as root, in the
# container or under sudo; the scans and tests themselves run as TEST_UID, as the app would.
#
#   inside.sh [name ...]   filesystems: ext4 xfs btrfs f2fs tmpfs vfat exfat ntfs3
#                          scenarios:   mounts network snapshots compression compressed-snapshots
#
# Every check prints one line: PASS, FAIL, KNOWN (a documented limitation, with the size of the
# error so a regression in it still shows) or SKIP (the filesystem or its tools cannot do it).
# The exit status is non-zero when anything FAILs.
set -uo pipefail

W=${W:-/work}
U=${TEST_UID:-1000}
all="ext4 xfs btrfs f2fs tmpfs vfat exfat ntfs3 mounts network snapshots compression compressed-snapshots"
names=${*:-$all}
mnt=$W/mnt
failures=0
mkdir -p "$mnt"
chmod 755 "$W" "$W/bin"
chmod 755 "$W"/bin/*

as_user() { setpriv --reuid="$U" --regid="$U" --clear-groups env HOME=/tmp PATH="$PATH" "$@"; }
say() { printf '%-8s %-44s %s\n' "$1" "$2" "$3"; }
fail() { say FAIL "$1" "$2"; failures=$((failures + 1)); }

# ---------------------------------------------------------------------------------------------
# Filesystems

# Make filesystem $1 and mount it at $2 with any extra mount options $3. Returns 1 if this
# machine cannot (no mkfs, or no kernel support), which the caller reports as SKIP.
make_fs() {
  # Separate from the line above: `local` expands every word before it assigns any.
  local fs=$1 at=$2 extra=${3:-} opts=()
  # One image per mount point, so that nested mounts of one type do not share a file.
  local image=$W/img${at//\//_}.img
  mkdir -p "$W/img"
  mkdir -p "$at"
  if [ "$fs" = tmpfs ]; then
    mount -t tmpfs -o size=1g tmpfs "$at" || return 1
  else
    rm -f "$image"
    truncate -s 1G "$image"
    case $fs in
      ext4) mkfs.ext4 -q -F "$image" ;;
      xfs) mkfs.xfs -q -f -m reflink=1 "$image" ;;
      btrfs) mkfs.btrfs -q -f "$image" ;;
      f2fs) mkfs.f2fs -q -f -O extra_attr,compression "$image" ;;
      vfat) mkfs.vfat -F 32 "$image" ;;
      exfat) mkfs.exfat "$image" ;;
      ntfs3) mkfs.ntfs -Q -F "$image" ;;
      *) return 1 ;;
    esac >/dev/null 2>&1 || return 1
    case $fs in
      vfat | exfat | ntfs3) opts=(-o "uid=$U,gid=$U${extra:+,$extra}") ;;
      btrfs) opts=(-o "user_subvol_rm_allowed${extra:+,$extra}") ;;
      *) [ -n "$extra" ] && opts=(-o "$extra") ;;
    esac
    mount -t "$fs" -o loop "${opts[@]}" "$image" "$at" 2>/dev/null || return 1
  fi
  chown "$U:$U" "$at"
}

unmount() { umount -R "$1" 2>/dev/null || umount -l "$1" 2>/dev/null; }

# ---------------------------------------------------------------------------------------------
# What duscape says, and what is really there

# duscape's total for $1, in bytes, with any extra flags (-a, -x) after it: as the app shows it
# once the second pass over small files has finished. Scanned as the test user.
duscape_total() {
  local dir=$1
  shift
  as_user "$W/bin/duscape" "$@" --benchmark --bench-stage refined "$dir" 2>/dev/null |
    grep '^refined' | grep -oE '\([0-9]+ B\)' | tr -dc 0-9
}

# The same, scanned as root: what `sudo duscape` shows, which on btrfs can read compressed
# extent sizes.
duscape_total_as_root() {
  local dir=$1
  shift
  "$W/bin/duscape" "$@" --benchmark --bench-stage refined "$dir" 2>/dev/null |
    grep '^refined' | grep -oE '\([0-9]+ B\)' | tr -dc 0-9
}

# The independent answer for a tree without shared extents: every non-directory counted once
# per (device, inode), by blocks allocated or, with -a, by length. $2... are `find` pruning
# arguments. Directories are not counted, because duscape does not count them either.
inode_oracle() {
  local mode=$1 dir=$2 field='%b'
  shift 2
  [ "$mode" = apparent ] && field='%s'
  find "$dir" "$@" ! -type d -printf "%D %i $field\n" 2>/dev/null | sort -u |
    awk -v m="$mode" '{ s += (m == "apparent" ? $3 : $3 * 512) } END { printf "%d", s }'
}

# Bytes of data btrfs holds on the volume mounted at $1: what is left once sharing and
# compression are accounted for. Inline files live in metadata and are not in it.
btrfs_data_used() {
  btrfs filesystem usage -b "$1" 2>/dev/null | grep -oE '^Data,[^:]*: Size:[0-9]+, Used:[0-9]+' |
    grep -oE 'Used:[0-9]+' | tr -dc 0-9
}

# Compare: check NAME EXPECTED ACTUAL [TOLERANCE_BYTES [KNOWN_REASON]].
check() {
  local name=$1 expected=$2 actual=$3 tolerance=${4:-0} known=${5:-}
  if [ -z "$actual" ] || [ -z "$expected" ]; then
    fail "$name" "no figure (expected '$expected', got '$actual')"
    return
  fi
  local diff=$((actual - expected))
  local abs=${diff#-}
  local detail
  detail="expected $expected, duscape $actual ($( [ $diff -ge 0 ] && printf '+')$diff)"
  if [ "$abs" -le "$tolerance" ]; then
    say PASS "$name" "$detail"
  elif [ -n "$known" ]; then
    say KNOWN "$name" "$detail: $known"
  else
    fail "$name" "$detail"
  fi
}

# ---------------------------------------------------------------------------------------------
# Datasets

random_file() { head -c "$2" /dev/urandom >"$1"; } # path, bytes

# The generic tree every filesystem gets: plain sizes around the block edges, a directory of many
# tiny files, deep nesting, a sparse file, hard links within and across folders, a symlink and
# awkward names. What the filesystem cannot do (hard links on FAT) is noted and left out.
make_dataset() {
  local d=$1 quirks=""
  mkdir -p "$d/plain" "$d/many" "$d/hl/x" "$d/hl/y"
  for size in 0 1 4095 4096 4097 100000 1048576; do random_file "$d/plain/f$size" "$size"; done
  for i in $(seq 1500); do random_file "$d/many/f$i" $((1024 + i % 2048)); done
  local deep=$d/deep
  for level in $(seq 10); do deep=$deep/l$level; mkdir -p "$deep"; random_file "$deep/f" 5000; done
  # Sparse: 64 MiB long, 1 MiB written in the middle.
  if truncate -s 64M "$d/sparse.bin" 2>/dev/null &&
    dd if=/dev/urandom of="$d/sparse.bin" bs=1M count=1 seek=30 conv=notrunc status=none 2>/dev/null; then
    quirks+=" sparse"
  fi
  random_file "$d/hl/x/a" 300000
  if ln "$d/hl/x/a" "$d/hl/x/a2" 2>/dev/null && ln "$d/hl/x/a" "$d/hl/y/a3" 2>/dev/null; then
    quirks+=" hardlinks"
  fi
  ln -s plain/f4096 "$d/link" 2>/dev/null && quirks+=" symlinks"
  random_file "$d/with space" 7000
  random_file "$d/ünïcödé" 7000 2>/dev/null || true
  random_file "$d/new"$'\n'"line" 7000 2>/dev/null || true
  sync
  echo "$quirks"
}

# ---------------------------------------------------------------------------------------------
# Per filesystem: the test suites on it, then the generic tree against the inode oracle

run_suites() { # fs, dir for TMPDIR
  local fs=$1 tmp=$2/tmp extra=()
  mkdir -p "$tmp" && chown "$U:$U" "$tmp"
  case $fs in xfs | btrfs) extra+=(DUSCAPE_TEST_REFLINK_DIR="$tmp") ;; esac
  [ "$fs" = btrfs ] && extra+=(DUSCAPE_TEST_BTRFS_DIR="$tmp")
  for suite in libduscape duscape-scan duscape; do
    local out
    out=$(as_user env TMPDIR="$tmp" "${extra[@]}" "$W/bin/tests-$suite" --test-threads=4 2>&1)
    local summary
    summary=$(grep -E '^test result' <<<"$out" | tail -1)
    if grep -q 'test result: ok' <<<"$summary"; then
      say PASS "$fs: $suite tests" "$(grep -oE '[0-9]+ passed' <<<"$summary")"
    else
      fail "$fs: $suite tests" "$(grep -oE '[0-9]+ failed' <<<"$summary"): $(grep -E '^    [a-z_:]+$' <<<"$out" | tr -d ' ' | tr '\n' ' ')"
    fi
  done
}

filesystem() {
  local fs=$1 at=$mnt/$1
  if ! make_fs "$fs" "$at"; then
    say SKIP "$fs" "cannot make or mount it here"
    return
  fi
  local quirks
  quirks=$(as_user bash -c "$(declare -f random_file make_dataset); make_dataset '$at/data'")
  say INFO "$fs: dataset" "has:${quirks:- none of hardlinks, sparse, symlinks}"
  check "$fs: disk usage" "$(inode_oracle disk "$at/data")" "$(duscape_total "$at/data")"
  check "$fs: apparent size" "$(inode_oracle apparent "$at/data")" "$(duscape_total "$at/data" -a)"
  # As a kernel before 4.11 reads it, with fstatat standing in for statx.
  check "$fs: disk usage, without statx" \
    "$(inode_oracle disk "$at/data")" "$(DUSCAPE_NO_STATX=1 duscape_total "$at/data")"
  case $fs in vfat | exfat | ntfs3) ;; *) run_suites "$fs" "$at" ;; esac
  case $fs in xfs | btrfs) copy_on_write "$fs" "$at" ;; esac
  unmount "$at"
}

# Reflinks: one 8 MiB file, two whole clones (held once), a clone rewritten in part (counted in
# full, by design: see `reflink::shared_identity`) and a clone of a file under the probe threshold.
copy_on_write() {
  local fs=$1 at=$2 c=$2/cow
  as_user bash -c "
    mkdir -p '$c/a' '$c/b'
    head -c 8388608 /dev/urandom >'$c/a/orig'
    cp --reflink=always '$c/a/orig' '$c/a/clone1' && cp --reflink=always '$c/a/orig' '$c/b/clone2' &&
    cp --reflink=always '$c/a/orig' '$c/b/partial' &&
    dd if=/dev/urandom of='$c/b/partial' bs=1M count=1 seek=3 conv=notrunc status=none &&
    head -c 16384 /dev/urandom >'$c/a/small' && cp --reflink=always '$c/a/small' '$c/b/small_clone'
    sync" || { say SKIP "$fs: copy on write" "cannot reflink"; return; }
  local all clones small
  all=$(inode_oracle disk "$c")
  clones=$(( $(stat -c %b "$c/a/clone1") * 512 + $(stat -c %b "$c/b/clone2") * 512 ))
  small=$(( $(stat -c %b "$c/b/small_clone") * 512 ))
  # Whole clones once, the small clone once (found by the second pass), the rewritten clone in
  # full, by design.
  check "$fs: reflink clones held once, small ones too" "$((all - clones - small))" \
    "$(duscape_total "$c")"
}

# ---------------------------------------------------------------------------------------------
# btrfs snapshots: data held once however many snapshots see it

snapshots() {
  snapshot_case "large files" 4194304 4194304 ""
  # 4 KiB and up, so none is inline and all of it is in btrfs's data figure.
  snapshot_case "small files" 4096 61440 ""
}

# A subvolume of 40 files between $2 and $3 bytes, three read-only snapshots of it, then one file
# rewritten — its old blocks stay held by the snapshots, the new ones by the live copy. btrfs's
# own data figure is the oracle, since `st_dev` differs across snapshots and no inode oracle can
# tell they share.
snapshot_case() {
  local name=$1 low=$2 high=$3 known=$4 at=$mnt/snapshots
  make_fs btrfs "$at" || { say SKIP "snapshots: $name" "no btrfs"; return; }
  btrfs -q subvolume create "$at/live" && chown "$U:$U" "$at/live"
  local step=$(( (high - low) / 4096 / 40 + 1 ))
  as_user bash -c "$(declare -f random_file)
    for i in \$(seq 40); do random_file '$at/live/f'\$i \$(( $low + 4096 * (i * $step % ($high / 4096 - $low / 4096 + 1)) )); done"
  sync
  for n in 1 2 3; do btrfs -q subvolume snapshot -r "$at/live" "$at/snap$n"; done
  as_user bash -c "$(declare -f random_file); random_file '$at/live/f1' $high"
  sync
  check "snapshots: $name, 3 snapshots" "$(btrfs_data_used "$at")" "$(duscape_total "$at")" 65536 "$known"
  check "snapshots: $name, -x sees the top level only" 0 "$(duscape_total "$at" -x)" 0
  unmount "$at"
}

# ---------------------------------------------------------------------------------------------
# Compression: does the block count duscape reads reflect it?

compression() {
  local at=$mnt/compression
  if make_fs btrfs "$at" compress-force=zstd; then
    as_user bash -c "$(declare -f random_file)
      text() { yes 'duscape compresses well, line after line after line' | head -c \$1; }
      text 67108864 >'$at/text'
      random_file '$at/random' 8388608
      { head -c 8388608 /dev/urandom; text 8388608; } >'$at/mixed'
      fallocate -l 4M '$at/preallocated'
      truncate -s 32M '$at/sparse' && text 1048576 | dd of='$at/sparse' bs=1M seek=10 conv=notrunc status=none"
    sync
    local used
    used=$(btrfs_data_used "$at")
    # As a user, stx_blocks is the uncompressed size and there is nothing better to be had.
    check "btrfs zstd: as a user" "$used" "$(duscape_total "$at")" 65536 \
      "btrfs reports uncompressed blocks to stat; the compressed size needs root"
    check "btrfs zstd: as root, from the extent items" "$used" "$(duscape_total_as_root "$at")" 65536
    check "btrfs zstd: apparent size is the length" \
      "$(inode_oracle apparent "$at")" "$(duscape_total_as_root "$at" -a)"
    unmount "$at"
  else
    say SKIP "btrfs compression" "no btrfs"
  fi
  # Mounted without compression, one folder marked for it: only what `statx` says is compressed
  # is asked about, and that must still come out right.
  if make_fs btrfs "$at" && command -v chattr >/dev/null; then
    as_user bash -c "
      text() { yes 'duscape compresses well, line after line after line' | head -c \$1; }
      mkdir '$at/marked' && chattr +c '$at/marked' && text 33554432 >'$at/marked/text'
      text 8388608 >'$at/plain'"
    sync
    check "btrfs chattr +c: as root" "$(btrfs_data_used "$at")" "$(duscape_total_as_root "$at")" 65536
    unmount "$at"
  else
    say SKIP "btrfs chattr +c" "no btrfs or chattr"
  fi
  if make_fs f2fs "$at" compress_algorithm=lz4,compress_extension=txt; then
    as_user bash -c "yes 'duscape compresses well, line after line after line' | head -c 67108864 >'$at/text.txt'"
    sync
    # f2fs reserves a compressed file's blocks until they are released, so st_blocks is the
    # uncompressed size and that is what the volume has set aside: both agree with the oracle.
    check "f2fs lz4: 64 MiB of text" "$(inode_oracle disk "$at")" "$(duscape_total "$at")"
    unmount "$at"
  else
    say SKIP "f2fs compression" "cannot make f2fs with compression"
  fi
}

# Compressed data shared by snapshots and a reflink copy, scanned as root: each extent once, at
# its compressed size.
compressed_snapshots() {
  local at=$mnt/compressed-snapshots
  make_fs btrfs "$at" compress-force=zstd || { say SKIP "compressed snapshots" "no btrfs"; return; }
  btrfs -q subvolume create "$at/live" && chown "$U:$U" "$at/live"
  as_user bash -c "
    text() { yes \"duscape compresses \$2, line after line\" | head -c \$1; }
    for i in 1 2 3 4; do text 16777216 \$i >'$at/live/t'\$i; done
    for i in \$(seq 50); do text \$((4096 * (2 + i % 13))) small\$i >'$at/live/s'\$i; done"
  sync
  for n in 1 2; do btrfs -q subvolume snapshot -r "$at/live" "$at/snap$n"; done
  as_user bash -c "cp --reflink=always '$at/live/t2' '$at/live/t2.copy'
    yes 'rewritten after the snapshots' | head -c 16777216 >'$at/live/t1'"
  sync
  check "compressed snapshots: as root" "$(btrfs_data_used "$at")" \
    "$(duscape_total_as_root "$at")" 65536
  # As a user the sizes are uncompressed, but every distinct file must still count once: the live
  # files, less the reflink copy, plus the old t1 the snapshots keep. The files have 128 extents
  # each, more than one FIEMAP page.
  local blocks=$(( $(inode_oracle disk "$at/live") - $(stat -c %b "$at/live/t2.copy") * 512 \
    + $(stat -c %b "$at/snap1/t1") * 512 ))
  check "compressed snapshots: as a user, each file once" "$blocks" "$(duscape_total "$at")"
  unmount "$at"
}

# ---------------------------------------------------------------------------------------------
# Mount layouts: nested filesystems, pseudo filesystems, a bind mount, the same data twice

mounts() {
  local m=$mnt/mounts
  make_fs ext4 "$m" || { say SKIP mounts "no ext4"; return; }
  as_user bash -c "$(declare -f random_file make_dataset); make_dataset '$m/data' >/dev/null"
  mkdir -p "$m/nested" "$m/proc" "$m/tmp" "$m/bind"
  local nested_fs=xfs
  make_fs xfs "$m/nested" || { nested_fs=ext4; make_fs ext4 "$m/nested"; }
  as_user bash -c "$(declare -f random_file); random_file '$m/nested/file' 3000000"
  mount -t proc proc "$m/proc"
  mount -t tmpfs -o size=64m tmpfs "$m/tmp" && chown "$U:$U" "$m/tmp"
  as_user bash -c "$(declare -f random_file); random_file '$m/tmp/file' 2000000"
  sync

  local prune=(-path "$m/proc" -prune -o)
  check "mounts: $nested_fs + tmpfs nested, proc skipped" \
    "$(inode_oracle disk "$m" "${prune[@]}")" "$(duscape_total "$m")"
  check "mounts: -x stays on the root's filesystem" \
    "$(inode_oracle disk "$m" -xdev)" "$(duscape_total "$m" -x)"
  check "mounts: apparent size" \
    "$(inode_oracle apparent "$m" "${prune[@]}")" "$(duscape_total "$m" -a)"

  # The same directory a second time, on the same device: nothing in `st_dev` says the walk
  # crossed into a mount. `du` counts it once; so should duscape.
  mount --bind "$m/data" "$m/bind"
  check "mounts: a bind mount of data/ inside the scan" \
    "$(inode_oracle disk "$m" "${prune[@]}")" "$(duscape_total "$m")"
  check "mounts: the same, with -x" \
    "$(inode_oracle disk "$m" -xdev)" "$(duscape_total "$m" -x)"
  # Scanning the bind mount alone, its source is outside the scan: it is all there is.
  check "mounts: a bind mount scanned on its own" \
    "$(inode_oracle disk "$m/bind")" "$(duscape_total "$m/bind")"
  # As a kernel before 4.11 reads it (Synology's DSM runs 4.4): no statx, so no mount root or
  # mount id from the kernel, and the mount table has to say where the bind mount is.
  check "mounts: everything, as a kernel without statx" \
    "$(inode_oracle disk "$m" "${prune[@]}")" "$(DUSCAPE_NO_STATX=1 duscape_total "$m")"
  check "mounts: the same, with -x" \
    "$(inode_oracle disk "$m" -xdev)" "$(DUSCAPE_NO_STATX=1 duscape_total "$m" -x)"
  check "mounts: the bind mount on its own, without statx" \
    "$(inode_oracle disk "$m/bind")" "$(DUSCAPE_NO_STATX=1 duscape_total "$m/bind")"
  unmount "$m"
}

# ---------------------------------------------------------------------------------------------
# Network mounts: another machine's files are not this disk's space, and are not walked into

# A loopback NFSv4 server exporting a tmpfs. Prints nothing and returns 1 if it cannot start.
start_nfs() {
  mkdir -p /export /var/lib/nfs/v4recovery
  mount -t tmpfs -o size=64m tmpfs /export || return 1
  echo '/export *(rw,fsid=0,no_subtree_check,insecure,no_root_squash)' >/etc/exports
  touch /var/lib/nfs/etab /var/lib/nfs/rmtab
  mount -t nfsd nfsd /proc/fs/nfsd 2>/dev/null
  exportfs -ra >/dev/null 2>&1 || return 1
  rpc.mountd -N 3 --no-udp >/dev/null 2>&1 &
  rpc.nfsd -N 3 -U 4 >/dev/null 2>&1 || return 1
}

stop_nfs() {
  rpc.nfsd 0 >/dev/null 2>&1
  pkill rpc.mountd 2>/dev/null
  umount /export 2>/dev/null
}

network() {
  local m=$mnt/network
  make_fs ext4 "$m" || { say SKIP network "no ext4"; return; }
  as_user bash -c "$(declare -f random_file make_dataset); make_dataset '$m/data' >/dev/null"
  mkdir -p "$m/nfs" "$m/cloud"
  local mounted=()
  if start_nfs && head -c 5000000 /dev/urandom >/export/f &&
    timeout 30 mount -t nfs4 127.0.0.1:/ "$m/nfs" 2>/dev/null; then
    mounted+=(nfs)
  else
    say SKIP "network: nfs" "cannot start a loopback NFS server"
  fi
  # A FUSE filesystem that is remote by subtype (`fuse.rclone`); the memory backend needs no network.
  if command -v rclone >/dev/null; then
    (timeout 300 rclone mount :memory: "$m/cloud" --allow-other >/dev/null 2>&1 &)
    for _ in $(seq 50); do grep -q " $m/cloud " /proc/self/mountinfo && break; sleep 0.1; done
    if grep " $m/cloud " /proc/self/mountinfo | grep -q ' - fuse.rclone '; then
      head -c 3000000 /dev/urandom >"$m/cloud/f" && mounted+=(rclone)
    else
      say SKIP "network: fuse.rclone" "cannot mount rclone"
    fi
  fi
  sync
  if [ ${#mounted[@]} = 0 ]; then unmount "$m"; stop_nfs; return; fi
  say INFO "network: mounted" "${mounted[*]}"

  local prune=(-path "$m/nfs" -prune -o -path "$m/cloud" -prune -o)
  check "network: ${mounted[*]} inside the scan are not walked" \
    "$(inode_oracle disk "$m" "${prune[@]}")" "$(duscape_total "$m")"
  check "network: nor in apparent size" \
    "$(inode_oracle apparent "$m" "${prune[@]}")" "$(duscape_total "$m" -a)"
  if [[ " ${mounted[*]} " == *" nfs "* ]]; then
    check "network: an nfs mount named as the root is scanned" 5000000 "$(duscape_total "$m/nfs" -a)"
    local out
    out=$(as_user env DUSCAPE_TEST_NETWORK_DIR="$m/nfs" "$W/bin/tests-duscape-scan" \
      network_mounts 2>&1)
    if grep -q 'test result: ok. [1-9]' <<<"$out"; then
      say PASS "network: nfs is classified as network" "unit test on the mount"
    else
      fail "network: nfs is classified as network" "$(grep -E 'panicked|assert' <<<"$out" | head -2)"
    fi
  fi
  umount "$m/cloud" 2>/dev/null
  umount -l "$m/nfs" 2>/dev/null
  stop_nfs
  unmount "$m"
}

# ---------------------------------------------------------------------------------------------

say RESULT CHECK DETAIL
for name in $names; do
  case $name in
    mounts | network | snapshots | compression) "$name" ;;
    compressed-snapshots) compressed_snapshots ;;
    *) filesystem "$name" ;;
  esac
done
echo
[ "$failures" = 0 ] && echo "no failures" || echo "$failures failed"
[ "$failures" = 0 ]
