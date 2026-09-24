#!/usr/bin/env bash
# Run diskonaut's tests and totals against real filesystems and mount layouts.
#
#   fixtures/fs/run.sh [--build-only|--no-build] [filesystem-or-scenario ...]
#
# With no names, everything inside.sh knows. --build-only and --no-build split the cargo build
# from the run, so CI can build as itself and run under sudo without cargo running as root.
#
# Making filesystems needs root. Run as root (CI does, under sudo), or as a member of the docker
# group, which runs inside.sh in a --privileged container of the image build-image.sh makes. Only
# the work directory is shared with the container. See fixtures/fs/README.md.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=${WORK:-$repo/target/fs-fixtures}
target=x86_64-unknown-linux-gnu
mkdir -p "$work/bin"
build=1 run=1
case ${1:-} in
  --build-only) run=0; shift ;;
  --no-build) build=0; shift ;;
esac

if [ $build = 1 ]; then
  # Static, so the binaries run in the container's minimal rootfs whatever its libc.
  export RUSTFLAGS="-C target-feature=+crt-static"
  export CARGO_TARGET_DIR=$repo/target/static
  (cd "$repo" && cargo build -q --release --target "$target" --bin diskonaut)
  cp "$CARGO_TARGET_DIR/$target/release/diskonaut" "$work/bin/diskonaut"
  for package in libdiskonaut diskonaut-scan diskonaut-angch; do
    executable=$(cd "$repo" && cargo test -q -p "$package" --lib --target "$target" --no-run \
      --message-format=json | grep -oE '"executable":"[^"]+"' | cut -d'"' -f4 | tail -1)
    [ -n "$executable" ] || { echo "no test binary for $package" >&2; exit 1; }
    cp "$executable" "$work/bin/tests-$package"
  done
fi
cp "$repo/fixtures/fs/inside.sh" "$work/inside.sh"
[ $run = 1 ] || exit 0

if [ "$(id -u)" = 0 ]; then
  # The tests run as an ordinary user, as the app does: whoever called sudo, or 1000.
  W=$work TEST_UID=${SUDO_UID:-1000} bash "$work/inside.sh" "$@"
elif id -nG | grep -qw docker; then
  image=${IMAGE:-diskonaut-fs-fixtures}
  docker image inspect "$image" >/dev/null 2>&1 || IMAGE=$image "$repo/fixtures/fs/build-image.sh"
  docker run --rm --privileged -v "$work:/work" "$image" bash /work/inside.sh "$@"
else
  echo "needs root or the docker group: sudo $0 $*" >&2
  exit 2
fi
