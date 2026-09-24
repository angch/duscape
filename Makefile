.PHONY: build run install test test-fs static static-aarch64 static-linux-gui dos dos-tools dos-run

build:
	cargo build --workspace

run:
	cargo run --bin diskonaut

install:
	cargo install --path viewers/tui

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

# The Linux GUI viewer, fully static: pure Rust down to the X11 protocol, so it needs no musl-gcc
# and no system library, and runs under XWayland as well as on any X server.
static-linux-gui:
	cargo build -p diskonaut-linux --release --target x86_64-unknown-linux-musl

# diskonaut for MS-DOS (viewers/dos), in 16-bit assembly. FASM assembles it inside DOSBox-X, under
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

# target/dos/DISKONAU.EXE, the 8.3 name FASM writes, and diskonaut.exe for DOSes with long names
dos: dos-tools
	rm -f $(DOS)/DISKONAU.EXE $(DOS)/FASM.TXT
	$(DOSBOX) -c "TARGET\DOS\CWSDPMI -p" \
		-c "TARGET\DOS\FASM VIEWERS\DOS\DISKONAU.ASM TARGET\DOS\DISKONAU.EXE > TARGET\DOS\FASM.TXT" \
		-c exit > /dev/null 2>&1
	@cat $(DOS)/FASM.TXT
	test -f $(DOS)/DISKONAU.EXE
	cp $(DOS)/DISKONAU.EXE $(DOS)/diskonaut.exe

# DOSBox-X with this repository as C:, diskonaut scanning it
dos-run: dos
	dosbox-x -conf viewers/dos/dosbox-x.conf
