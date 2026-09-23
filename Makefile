.PHONY: build run install test test-fs static static-aarch64

build:
	cargo build --workspace

run:
	cargo run --bin diskonaut

install:
	cargo install --path diskonaut

test:
	cargo test --workspace

# The tests and totals on real filesystems (ext4, XFS, btrfs, f2fs, tmpfs, FAT, exFAT, NTFS) and
# mount layouts, on loopback images. Needs root or the docker group; see fixtures/fs/README.md.
# `make test-fs FS="btrfs snapshots"` runs just those.
test-fs:
	fixtures/fs/run.sh $(FS)

# Fully static x86_64 binary for any Linux (the release artifact). Needs musl-gcc (`musl-tools`)
# for jemalloc; see .github/workflows/deploy.yml.
static:
	CC_x86_64_unknown_linux_musl=musl-gcc cargo build -p diskonaut-angch --release --target x86_64-unknown-linux-musl

# The same for aarch64, cross-built with cargo-zigbuild (needs zig). jemalloc's page size is fixed
# at build time; 64K pages (2^16) also run on 4K and 16K kernels.
static-aarch64:
	JEMALLOC_SYS_WITH_LG_PAGE=16 cargo zigbuild -p diskonaut-angch --release --target aarch64-unknown-linux-musl
