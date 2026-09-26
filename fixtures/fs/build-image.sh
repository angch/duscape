#!/usr/bin/env bash
# Build `duscape-fs-fixtures`, a minimal local docker image for run.sh: this machine's own
# util-linux, coreutils and mkfs tools, with the libraries they load, plus the tools of any
# package in PACKAGES that the host lacks, fetched with `apt-get download` (no root needed).
#
# Built from the host's binaries rather than pulled, so it needs no registry. Nothing of the host
# is mounted into the container: only these copies.
set -euo pipefail

image=${IMAGE:-duscape-fs-fixtures}
tools=(bash sh find mount umount losetup findmnt setpriv truncate cp mkdir stat du dd sync head tail
       cat chown chmod ls rm seq tee sed grep awk xargs cut sort tr wc touch mv ln sleep id env
       date basename dirname yes timeout pkill ps fallocate)
# Used when present; a filesystem whose mkfs is missing is skipped by inside.sh, not failed.
optional=(chattr lsattr mkfs.ext4 mkfs.btrfs btrfs mkfs.vfat mkfs.exfat mkfs.ntfs mkfs.xfs mkfs.f2fs)
# Fetched when the host does not have them. Their libraries still come from the host.
packages=(${PACKAGES:-xfsprogs f2fs-tools nfs-kernel-server nfs-common rclone})

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

copy() { # copy a file and every library it loads, keeping their paths
  local file=$1
  if [ -e "$root$file" ]; then return; fi
  mkdir -p "$root$(dirname "$file")"
  cp -L "$file" "$root$file"
  { ldd "$file" 2>/dev/null || true; } | { grep -oE "/[^ ]+" || true; } | while read -r lib; do
    [ -e "$root$lib" ] || { mkdir -p "$root$(dirname "$lib")"; cp -L "$lib" "$root$lib"; }
  done
}

add_tool() { # $1: path of the tool on the host
  local path=$1
  copy "$(readlink -f "$path")"
  # Multi-call binaries (uutils coreutils) dispatch on argv[0]: keep the name they were run as.
  if [ "$(readlink -f "$path")" != "$path" ]; then
    mkdir -p "$root$(dirname "$path")"
    ln -sf "$(readlink -f "$path")" "$root$path"
  fi
}

for tool in "${tools[@]}"; do
  path=$(command -v "$tool") || { echo "missing on this host: $tool" >&2; exit 1; }
  add_tool "$path"
done
for tool in "${optional[@]}"; do
  if path=$(command -v "$tool"); then add_tool "$path"; fi
done

# Packages the host lacks: unpacked beside the rootfs, and their binaries copied in with the
# host libraries they need (a library the host lacks too is taken from the package).
debs=$(mktemp -d)
trap 'rm -rf "$root" "$debs"' EXIT
for package in "${packages[@]}"; do
  (cd "$debs" && apt-get download -q "$package" >/dev/null 2>&1) || { echo "could not fetch $package" >&2; continue; }
done
for deb in "$debs"/*.deb; do
  if [ ! -e "$deb" ]; then continue; fi
  dpkg-deb -x "$deb" "$debs/x"
done
if [ -d "$debs/x" ]; then
  export LD_LIBRARY_PATH="$debs/x/usr/lib/x86_64-linux-gnu:$debs/x/lib/x86_64-linux-gnu"
  for bin in "$debs"/x/usr/sbin/* "$debs"/x/sbin/* "$debs"/x/usr/bin/*; do
    if [ ! -f "$bin" ] || [ ! -x "$bin" ]; then continue; fi
    case $bin in */bin/*) target=/usr/bin/$(basename "$bin") ;; *) target=/usr/sbin/$(basename "$bin") ;; esac
    if [ -e "$root$target" ]; then continue; fi
    mkdir -p "$root$(dirname "$target")"
    cp -L "$bin" "$root$target"
    { ldd "$bin" 2>/dev/null || true; } | { grep -oE "/[^ ]+" || true; } | while read -r lib; do
      case $lib in "$debs"/x/*) dest=${lib#"$debs"/x} ;; *) dest=$lib ;; esac
      [ -e "$root$dest" ] || { mkdir -p "$root$(dirname "$dest")"; cp -L "$lib" "$root$dest"; }
    done
  done
fi
mkdir -p "$root"/{tmp,dev,proc,sys,work,etc}
# fusermount, for FUSE mounts, as real files outside /usr/bin: Ubuntu's AppArmor profile for it
# attaches by path, and inside the container it refuses every mount ("Permission denied").
if fusermount=$(command -v fusermount3); then
  mkdir -p "$root/usr/local/bin"
  copy "$(readlink -f "$fusermount")"
  cp -L "$fusermount" "$root/usr/local/bin/fusermount3"
  cp -L "$fusermount" "$root/usr/local/bin/fusermount"
fi

# What the NFS tools look up: the `nfs` port, protocol numbers, and libtirpc's transports.
for file in /etc/services /etc/protocols /etc/netconfig; do
  if [ -e "$file" ]; then cp "$file" "$root$file"; fi
done
echo 'root:x:0:0::/:/bin/bash' >"$root/etc/passwd"
echo 'user:x:1000:1000::/tmp:/bin/bash' >>"$root/etc/passwd"
[ -e "$root/bin" ] || ln -s usr/bin "$root/bin"
[ -e "$root/sbin" ] || ln -s usr/sbin "$root/sbin"

tar -C "$root" -c . | docker import -c 'ENV PATH=/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin' - "$image" >/dev/null
echo "built $image with: $(cd "$root/usr/sbin" && ls mkfs.* 2>/dev/null | tr '\n' ' ')"
