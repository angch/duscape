.PHONY: build run install test test-fs quality coverage setup-ubuntu setup-windows installer static static-aarch64 static-linux-gui static-linux-gui-aarch64 static-windows mac-universal mac-app pgo dos dos-tools dos-run

build:
	cargo build --workspace

run:
	cargo run --bin duscape

# The one `duscape` for this platform: the terminal viewer with the platform's window in it
# (`front.rs` picks), from the lock file the release is built from.
install:
	cargo install --locked --path viewers/tui --bin duscape

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

# What `make static` needs on Ubuntu (or Debian): musl-gcc for jemalloc, which is C, a native
# compiler and make for the build scripts, and the musl target in rustup. `make setup-ubuntu
# ZIG=1` also fetches zig and cargo-zigbuild, what `static-aarch64` and `static-windows` need.
# Rust itself is not installed here: https://rustup.rs
ZIG_VERSION := 0.13.0
setup-ubuntu:
	sudo apt-get update
	sudo apt-get install -y musl-tools build-essential curl
	rustup target add x86_64-unknown-linux-musl
	@if [ -n "$(ZIG)" ]; then \
		rustup target add aarch64-unknown-linux-musl x86_64-pc-windows-gnu; \
		command -v zig >/dev/null || { \
			curl -sSfL https://ziglang.org/download/$(ZIG_VERSION)/zig-linux-x86_64-$(ZIG_VERSION).tar.xz | sudo tar -xJ -C /opt \
			&& sudo ln -sf /opt/zig-linux-x86_64-$(ZIG_VERSION)/zig /usr/local/bin/zig; }; \
		command -v cargo-zigbuild >/dev/null || cargo install cargo-zigbuild; \
	fi
	@if [ -n "$(ZIG)" ]; then echo "ready: make static, static-aarch64, static-windows"; \
		else echo "ready: make static (ZIG=1 adds static-aarch64 and static-windows)"; fi

# What Windows development needs, beside Rust (https://rustup.rs, with the MSVC build tools it
# asks for) and Git for Windows (whose bash runs these recipes): `.\make setup-windows`.
# zig and NSIS go to target/tools, each zip checked against the hash pinned here, as the DOS
# tools go to target/dos: nothing installed for the whole machine, no administrator asked, and
# the versions CI uses (zig as deploy.yml pins it; NSIS 3.10, whose zip SourceForge lists by
# MD5 e3e2803a13ead75e4471a51069d04c20, the SHA-256 below taken from that file). Then the
# cargo tools: cargo-zigbuild (the release's GNU build, `make installer`), typos and cargo-deny
# (CI's checks), cargo-llvm-cov (`make coverage`).
TOOLS := target/tools
ZIG_WINDOWS := zig-windows-x86_64-$(ZIG_VERSION)
ZIG_WINDOWS_SHA256 := d859994725ef9402381e557c60bb57497215682e355204d754ee3df75ee3c158
NSIS_VERSION := 3.10
NSIS_SHA256 := fcdce3229717a2a148e7cda0ab5bdb667f39d8fb33ede1da8dabc336bd5ad110
setup-windows:
	rustup target add x86_64-pc-windows-gnu
	rustup component add llvm-tools-preview
	mkdir -p $(TOOLS)
	@test -x $(TOOLS)/$(ZIG_WINDOWS)/zig.exe || { 		curl -sSfL https://ziglang.org/download/$(ZIG_VERSION)/$(ZIG_WINDOWS).zip -o $(TOOLS)/zig.zip 		&& echo "$(ZIG_WINDOWS_SHA256)  $(TOOLS)/zig.zip" | sha256sum -c - 		&& unzip -q $(TOOLS)/zig.zip -d $(TOOLS) && rm $(TOOLS)/zig.zip; }
	@test -x $(TOOLS)/nsis-$(NSIS_VERSION)/makensis.exe || { 		curl -sSfL "https://downloads.sourceforge.net/project/nsis/NSIS%203/$(NSIS_VERSION)/nsis-$(NSIS_VERSION).zip" -o $(TOOLS)/nsis.zip 		&& echo "$(NSIS_SHA256)  $(TOOLS)/nsis.zip" | sha256sum -c - 		&& unzip -q $(TOOLS)/nsis.zip -d $(TOOLS) && rm $(TOOLS)/nsis.zip; }
	@command -v cargo-zigbuild >/dev/null || cargo install cargo-zigbuild --locked
	@command -v typos >/dev/null || cargo install typos-cli --locked
	@command -v cargo-deny >/dev/null || cargo install cargo-deny --locked
	@command -v cargo-llvm-cov >/dev/null || cargo install cargo-llvm-cov --locked
	@echo "ready: make installer, static-windows, coverage; zig and NSIS in $(TOOLS)"

# The Windows installer, target/installer/duscape-<version>-x86_64-setup.exe: the release's
# exes (the GNU build by zig, as deploy.yml makes them) in installer/duscape.nsi. zig and
# makensis from target/tools when `setup-windows` put them there, else from the PATH (Linux:
# zig as `setup-ubuntu ZIG=1` installs it, `apt install nsis`).
installer:
	PATH="$$PWD/$(TOOLS)/$(ZIG_WINDOWS):$$PWD/$(TOOLS)/nsis-$(NSIS_VERSION):$$PATH" cargo zigbuild -p duscape -p duscape-windows --release --locked --target x86_64-pc-windows-gnu
	mkdir -p target/installer
	PATH="$$PWD/$(TOOLS)/nsis-$(NSIS_VERSION):$$PATH" makensis -NOCD -V2 -DVERSION=$(VERSION) -DBIN=target/x86_64-pc-windows-gnu/release -DSETUP=target/installer/duscape-$(VERSION)-x86_64-setup.exe installer/duscape.nsi
	@echo "built target/installer/duscape-$(VERSION)-x86_64-setup.exe"

# Fully static binary: on Linux, musl with jemalloc (needs musl-gcc); on Windows, MSVC with
# crt-static (configured in .cargo/config.toml, needing only DLLs that come with Windows); on
# macOS, which has no musl-gcc, the Linux binary cross-built with cargo-zigbuild (needs zig).
# jemalloc's configure archives with the first `ar` on the PATH, and on macOS that is Apple's,
# which skips ELF objects: the archive came out empty and the link failed on `_rjem_malloc`.
ZIG_AR := $(shell if [ "$$(uname -s 2>/dev/null)" = Darwin ]; then echo "AR='zig ar'"; fi)
STATIC_CMD := $(shell if [ "$$OS" = "Windows_NT" ] || uname -s 2>/dev/null | grep -qE "MINGW|MSYS|CYGWIN"; then echo "cargo build -p duscape --release"; elif [ "$$(uname -s)" = Darwin ]; then echo "$(ZIG_AR) cargo zigbuild -p duscape --release --target x86_64-unknown-linux-musl"; else echo "CC_x86_64_unknown_linux_musl=musl-gcc cargo build -p duscape --release --target x86_64-unknown-linux-musl"; fi)
static:
	$(STATIC_CMD)

# The same for aarch64, cross-built with cargo-zigbuild (needs zig). jemalloc's page size is fixed
# at build time; 64K pages (2^16) also run on 4K and 16K kernels.
static-aarch64:
	$(ZIG_AR) JEMALLOC_SYS_WITH_LG_PAGE=16 cargo zigbuild -p duscape --release --target aarch64-unknown-linux-musl

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

# Windows: native on Windows (crt-static via .cargo/config.toml), or cross-built with
# cargo-zigbuild against the Universal C Runtime on other platforms.
STATIC_WINDOWS_CMD := $(shell if [ "$$OS" = "Windows_NT" ] || uname -s 2>/dev/null | grep -qE "MINGW|MSYS|CYGWIN"; then echo "cargo build -p duscape --release"; else echo "cargo zigbuild -p duscape --release --target x86_64-pc-windows-gnu"; fi)
static-windows:
	$(STATIC_WINDOWS_CMD)

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
	mkdir -p $(MAC_APP)/Contents/MacOS $(MAC_APP)/Contents/Resources
	cp target/universal/duscape $(MAC_APP)/Contents/MacOS/duscape
	cp viewers/macos/duscape.icns $(MAC_APP)/Contents/Resources/duscape.icns
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
