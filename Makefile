.PHONY: build run install test test-fs quality coverage static static-aarch64 static-linux-gui static-linux-gui-aarch64 static-windows mac-universal mac-app pgo dos dos-tools dos-run

build:
	cargo build --workspace

run:
	cargo run --bin duscape

install:
	cargo install --path viewers/tui

test:
	cargo test --workspace

# The source's measurements (AGENTS.md, "Quality"): lines, tests and `unsafe` per crate, every
# `unsafe` block's SAFETY comment accounted for, the register of functions over clippy's size
# limits (each `#[allow(clippy::too_many_lines)]` with its reason), and clippy with those limits
# on every target this machine has.
quality:
	@echo "== per crate: lines of Rust, #[test], unsafe blocks, and those with no SAFETY comment above"; \
	for c in common scanners viewers/shared viewers/tui viewers/windows viewers/macos viewers/linux; do \
	  lines=$$(find $$c/src -name '*.rs' | xargs cat | wc -l); \
	  tests=$$(grep -ra '#\[test\]' $$c/src --include='*.rs' | wc -l); \
	  blocks=$$(grep -ra 'unsafe {' $$c/src --include='*.rs' | wc -l); \
	  bare=$$(find $$c/src -name '*.rs' -exec awk '/unsafe \{/ { if (p1 !~ /SAFETY/ && p2 !~ /SAFETY/ && p3 !~ /SAFETY/) n++ } { p3=p2; p2=p1; p1=$$0 } END { print n+0 }' {} \; | awk '{ s+=$$1 } END { print s+0 }'); \
	  printf '%-16s %7d lines %5d tests %4d unsafe %4d without SAFETY\n' $$c $$lines $$tests $$blocks $$bare; \
	done; \
	echo "== quality debt: functions over clippy's limits (clippy.toml), with their reasons"; \
	grep -ran 'allow(clippy::too_many_lines)\|allow(clippy::cognitive_complexity)' --include='*.rs' common scanners viewers \
	  | sed 's/: *#\[allow(clippy::[a-z_]*)\] *\/\/ */: /'; \
	echo "== clippy, with the limits, on every target this machine has"; \
	for t in x86_64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-apple-darwin; do \
	  if rustup target list --installed | grep -q "^$$t$$"; then \
	    if cargo clippy --workspace --all-targets --target $$t -- -D warnings >/dev/null 2>&1; then echo "$$t: clean"; else echo "$$t: NOT clean"; fi; \
	  else echo "$$t: not installed"; fi; \
	done

# Test coverage, line and function, per file and in all (`rustup component add
# llvm-tools-preview; cargo install cargo-llvm-cov`). The viewers that are stubs on this
# platform show as uncovered; read the row for the crate you changed.
coverage:
	cargo llvm-cov --workspace --summary-only

# The tests and totals on real filesystems (ext4, XFS, btrfs, f2fs, tmpfs, FAT, exFAT, NTFS) and
# mount layouts, on loopback images. Needs root or the docker group; see fixtures/fs/README.md.
# `make test-fs FS="btrfs snapshots"` runs just those.
test-fs:
	fixtures/fs/run.sh $(FS)

# Fully static x86_64 binary for any Linux (the release artifact): the terminal viewer and the
# window in one (`duscape --gui`, and the default from a desktop). Needs musl-gcc
# (`musl-tools`) for jemalloc; see .github/workflows/deploy.yml.
static:
	CC_x86_64_unknown_linux_musl=musl-gcc cargo build -p duscape --release --target x86_64-unknown-linux-musl

# The same for aarch64, cross-built with cargo-zigbuild (needs zig). jemalloc's page size is fixed
# at build time; 64K pages (2^16) also run on 4K and 16K kernels.
static-aarch64:
	JEMALLOC_SYS_WITH_LG_PAGE=16 cargo zigbuild -p duscape --release --target aarch64-unknown-linux-musl

# A profile-guided build of the terminal viewer: instrument, scan PGO_TRAIN (this directory by
# default; a big real tree trains it better) through every benchmark stage, then rebuild with the
# profile. Needs `rustup component add llvm-tools-preview`. Worth 2–3% of the wall clock and 6–8%
# of the CPU on top of the release profile, measured in docs/scan-performance.md — not enough to
# put in the release pipeline, which would have to train on every build; here for whoever wants
# it locally. The result is target/pgo/release/duscape.
PGO_TRAIN ?= .
PGO_DIR := $(CURDIR)/target/pgo
PROFDATA := $(shell find $(HOME)/.rustup/toolchains -name llvm-profdata -type f 2>/dev/null | head -1)
pgo:
	@test -n "$(PROFDATA)" || { echo "llvm-profdata not found: rustup component add llvm-tools-preview" >&2; exit 1; }
	rm -rf $(PGO_DIR)/data
	RUSTFLAGS="-Cprofile-generate=$(PGO_DIR)/data" cargo build -p duscape --release --target-dir $(PGO_DIR)/gen
	$(PGO_DIR)/gen/release/duscape --benchmark --bench-stage all $(PGO_TRAIN) >/dev/null
	$(PGO_DIR)/gen/release/duscape --benchmark --bench-stage sharded --bench-repeat 2 $(PGO_TRAIN) >/dev/null
	$(PROFDATA) merge -o $(PGO_DIR)/merged.profdata $(PGO_DIR)/data
	RUSTFLAGS="-Cprofile-use=$(PGO_DIR)/merged.profdata" cargo build -p duscape --release --target-dir $(PGO_DIR)
	@echo "built $(PGO_DIR)/release/duscape"

# Windows, cross-built with cargo-zigbuild against the Universal C Runtime: `duscape.exe`, the
# terminal viewer and the window in one, needing only DLLs that come with Windows 10 and later.
# (Built on Windows with MSVC, `.cargo/config.toml` links the C runtime in the same way.)
static-windows:
	cargo zigbuild -p duscape --release --target x86_64-pc-windows-gnu

# macOS, on a Mac: `duscape` for both architectures in one file (`target/universal/duscape`),
# then Duscape.app around it, for Finder — a bare binary opened from Finder runs in Terminal.
# Nothing on macOS links fully static (libSystem is always shared); this links only the system's
# own libraries and frameworks. Signed ad hoc, as the linker signs each architecture.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
MAC_APP := target/universal/Duscape.app
mac-universal:
	cargo build -p duscape --release --target aarch64-apple-darwin
	cargo build -p duscape --release --target x86_64-apple-darwin
	mkdir -p target/universal
	lipo -create -output target/universal/duscape target/aarch64-apple-darwin/release/duscape target/x86_64-apple-darwin/release/duscape

mac-app: mac-universal
	mkdir -p $(MAC_APP)/Contents/MacOS
	cp target/universal/duscape $(MAC_APP)/Contents/MacOS/duscape
	sed 's/@VERSION@/$(VERSION)/g' viewers/macos/Info.plist > $(MAC_APP)/Contents/Info.plist
	codesign --force --sign - $(MAC_APP)

# The Linux GUI viewer alone, fully static: pure Rust down to the X11 protocol, so it needs no musl-gcc
# and no system library, and runs under XWayland as well as on any X server.
static-linux-gui:
	cargo build -p duscape-linux --release --target x86_64-unknown-linux-musl

# The same for aarch64, cross-built with cargo-zigbuild; with no C in it, no page size to fix.
static-linux-gui-aarch64:
	cargo zigbuild -p duscape-linux --release --target aarch64-unknown-linux-musl

# duscape for MS-DOS (viewers/dos), in 16-bit assembly. FASM assembles it inside DOSBox-X, under
# the CWSDPMI DPMI host; both are fetched once into target/dos and checked against these hashes.
DOS := target/dos
DOSBOX := dosbox-x -silent -fastlaunch -nogui -nomenu -defaultconf -time-limit 120 \
	-set "dos ver=7.1" -set "dos lfn=true" -set "cpu cycles=max" -c "mount c $(CURDIR)" -c c:
FASM_ZIP := https://flatassembler.net/fasm17335.zip
FASM_SHA256 := 2482299be364d411dba944524ecf87479a0105a997062743684e0531c0742e15
CWSDPMI_ZIP := https://www.delorie.com/pub/djgpp/current/v2misc/csdpmi7b.zip
CWSDPMI_SHA256 := deacda0488e1cdd7c4a9f32fab45662b34c0ed6b2d7d4d13bc07041b62004a8c

dos-tools: $(DOS)/FASM.EXE $(DOS)/CWSDPMI.EXE

$(DOS)/FASM.EXE:
	mkdir -p $(DOS)
	curl -sfL $(FASM_ZIP) -o $(DOS)/fasm.zip
	echo "$(FASM_SHA256)  $(DOS)/fasm.zip" | shasum -a 256 -c -
	unzip -ojq $(DOS)/fasm.zip FASM.EXE -d $(DOS)

$(DOS)/CWSDPMI.EXE:
	mkdir -p $(DOS)
	curl -sfL $(CWSDPMI_ZIP) -o $(DOS)/csdpmi.zip
	echo "$(CWSDPMI_SHA256)  $(DOS)/csdpmi.zip" | shasum -a 256 -c -
	unzip -ojq $(DOS)/csdpmi.zip bin/CWSDPMI.EXE -d $(DOS)

# target/dos/DUSCAPE.EXE: the name fits 8.3, so DOS with or without long names runs it by it
dos: dos-tools
	rm -f $(DOS)/DUSCAPE.EXE $(DOS)/FASM.TXT
	$(DOSBOX) -c "TARGET\DOS\CWSDPMI -p" \
		-c "TARGET\DOS\FASM VIEWERS\DOS\DUSCAPE.ASM TARGET\DOS\DUSCAPE.EXE > TARGET\DOS\FASM.TXT" \
		-c exit > /dev/null 2>&1
	@cat $(DOS)/FASM.TXT
	test -f $(DOS)/DUSCAPE.EXE

# DOSBox-X with this repository as C:, duscape scanning it
dos-run: dos
	dosbox-x -conf viewers/dos/dosbox-x.conf
