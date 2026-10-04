#!/bin/bash
# libduscape's and duscape-scan's tests on the architectures Debian builds that CI does not:
# 32-bit (i686, armv7: static musl) and big-endian (s390x: glibc), under QEMU's user-mode
# emulators. Needs no root: zig (the cross linker, through cargo-zigbuild), QEMU and s390x's
# libc are unpacked under $TOOLS. Run from the repository's root, on Linux (WSL will do).
#
#   docs/probes/cross-arch-tests.sh
#
# 2026-10-04: all pass on i686 and armv7; on s390x all but one, a test that starts zsh where
# none is installed — under QEMU, glibc's posix_spawn cannot report a program that failed to
# start, so the test's "is zsh there" guard sees a start that did not happen. Not a byte-order
# matter; on real s390x the guard works. docs/packaging.md, "Getting into Debian itself".
set -u
TOOLS=${TOOLS:-$HOME/tools}
ZIG13=$TOOLS/zig-linux-x86_64-0.13.0
# zig 0.13's glibc stubs define lgammal twice for s390x; 0.17 links it.
ZIG17=$TOOLS/zig-x86_64-linux-0.17.0
export PATH="$ZIG13:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/duscape-cross}
mkdir -p "$TOOLS" "$CARGO_TARGET_DIR"

fetch_zig() { # name tarball sha256
    [ -x "$TOOLS/$1/zig" ] && return
    (cd "$TOOLS" && curl -sSfLO "https://ziglang.org/download/$2" \
        && echo "$3  $(basename "$2")" | sha256sum -c - && tar -xJf "$(basename "$2")")
}
fetch_zig zig-linux-x86_64-0.13.0 0.13.0/zig-linux-x86_64-0.13.0.tar.xz \
    d45312e61ebcc48032b77bc4cf7fd6915c11fa16e4aad116b66c9468211230ea
fetch_zig zig-x86_64-linux-0.17.0 0.17.0/zig-x86_64-linux-0.17.0.tar.xz \
    1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
command -v cargo-zigbuild >/dev/null || cargo install cargo-zigbuild --locked
rustup target add i686-unknown-linux-musl armv7-unknown-linux-musleabihf \
    s390x-unknown-linux-gnu >/dev/null 2>&1

# QEMU's static user-mode emulators, from the distribution's package, unpacked.
if [ ! -x "$TOOLS/qemu/usr/bin/qemu-s390x-static" ]; then
    mkdir -p "$TOOLS/qemu" && (cd "$TOOLS/qemu" && apt-get download qemu-user-static \
        && for deb in *.deb; do dpkg-deb -x "$deb" .; done)
fi
# s390x's libc and libgcc, at the versions this machine runs, as the emulator's sysroot;
# merged /usr, so /lib is a link to usr/lib.
ROOT=$TOOLS/s390x-root
if [ ! -e "$ROOT/lib/ld64.so.1" ]; then
    mkdir -p "$TOOLS/s390x-debs" "$ROOT"
    (cd "$TOOLS/s390x-debs" \
        && curl -sSfLO "http://ports.ubuntu.com/ubuntu-ports/pool/main/g/glibc/libc6_$(dpkg-query -W -f='${Version}' libc6)_s390x.deb" \
        && curl -sSfLO "http://ports.ubuntu.com/ubuntu-ports/pool/main/g/$(dpkg-query -W -f='${source:Package}' libgcc-s1)/libgcc-s1_$(dpkg-query -W -f='${Version}' libgcc-s1)_s390x.deb" \
        && for deb in *.deb; do dpkg-deb -x "$deb" "$ROOT"; done)
    [ -e "$ROOT/lib" ] || ln -s usr/lib "$ROOT/lib"
fi

run() { # target emulator [emulator arguments]
    local target=$1 qemu=$2; shift 2
    echo "=== $target"
    if ! cargo zigbuild --tests -p libduscape -p duscape-scan --target "$target" \
        >"$CARGO_TARGET_DIR/$target.log" 2>&1; then
        echo "build failed: $CARGO_TARGET_DIR/$target.log"; return
    fi
    for crate in libduscape duscape_scan; do
        bin=$(ls -t "$CARGO_TARGET_DIR/$target/debug/deps/$crate"-* \
            | grep -v '\.d$' | grep -v '\.[a-z]*$' | head -1)
        [ -x "$bin" ] || continue
        out=$("$TOOLS/qemu/usr/bin/$qemu" "$@" "$bin" --test-threads=4 2>&1)
        echo "$(basename "$bin"): $(echo "$out" | grep -E '^test result' | tail -1)"
        echo "$out" | grep -E '^test .* FAILED' | head -10
    done
}
run i686-unknown-linux-musl qemu-i386-static
run armv7-unknown-linux-musleabihf qemu-arm-static
PATH="$ZIG17:$PATH" run s390x-unknown-linux-gnu qemu-s390x-static -L "$ROOT"
