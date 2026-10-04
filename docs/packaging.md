# Packaging for Linux: the static binary first, and why

The Linux release is two fully static binaries, `duscape-<version>-x86_64-unknown-linux-musl`
and `…-aarch64-…`, in tarballs. That is the default way duscape is distributed, and the
packages discussed below — deb and rpm, Flatpak, Snap — are wrappers around it or trade-offs
against it. This file records what each would take and cost, so the decision can be made again
when the facts change (the AGENTS.md rule: every fence has its reason written down).

## The canonical builds are GitHub CI's

**The binaries `deploy.yml` builds for a tag, as published on that GitHub release, are the
canonical ones.** Every package holds those files byte for byte and never a build of its own:
the tarballs and the Windows zip, the deb and rpm (`packaging/linux/nfpm.yaml`, run in the job
that built the binary), the Windows installer (`installer/duscape.nsi`, by Ubuntu's makensis
in the Windows job, around the zip's two exes), and a Snap or Flatpak if one is ever made. The
release carries `SHA256SUMS` over every file, and a build-provenance attestation for each
(`gh attestation verify <file> -R angch/duscape`: this workflow, this commit).

Why: a duscape is then the same program however it was installed, and a measurement, a bug
report or a fix is about that program; and **a self-update** can fetch the release's file for
its platform, check it, and run it, whichever package it came from (below). What follows from
the rule:

- `make static`, `make installer` and the like are for development; what they build is not
  published.
- Packaging that rebuilds from source does not hold canonical binaries: Debian's and Fedora's
  own repositories, and Flathub as it prefers (below). Either they are left to others, or the
  package repackages the release's file.
- **macOS** is built by its own job on `macos-latest` (`build-macos`): the universal binary and
  `Duscape.app`, the tarball's binary taken from inside the signed bundle so the two are one
  file. Signed ad hoc and not notarised — notarising needs an Apple Developer ID (a paid
  account) and its credentials as CI secrets; until then Gatekeeper asks before a downloaded
  copy's first start, and a self-update on macOS would have to clear the quarantine attribute
  of what it fetched itself (the user asked for it; Gatekeeper does not know that).

### What a manual self-update needs (groundwork, not built)

The idea: duscape, when asked, downloads the latest release's file for its platform, keeps it
in a folder of its own, and the old binary execs the new one. What the release gives it and what
it would still have to decide:

- **Finding the file**: `https://api.github.com/repos/angch/duscape/releases/latest`, the asset
  named `duscape-<tag>-<target>.<ext>` for the target the running binary was built for (known at
  compile time). The names are the contract; changing them breaks every installed updater.
- **Checking it**: `SHA256SUMS` from the same release (over HTTPS, so as trustworthy as the
  download itself), and for more, the attestation — `gh attestation verify` needs `gh`; the
  Sigstore bundle can be checked without it, a dependency to weigh. Refuse anything that does
  not match.
- **Where it goes**: a per-user folder (`~/.local/share/duscape/bin/<version>/` on Linux,
  `%LOCALAPPDATA%\duscape\bin\<version>\` on Windows, `~/Library/Application Support/
  duscape/bin/<version>/` on macOS) — never over a package's own file: dpkg and rpm own
  `/usr/bin/duscape`, a Snap or Flatpak's is read-only, the installer's is in its uninstall
  list. The package stays as installed; the newer copy runs in front of it.
- **Handing over**: at start, if a newer verified copy is in that folder, exec it with the same
  arguments (Unix `execv`; Windows starts it and exits, passing the console on). The new one
  must not hand over again (an environment variable or a flag), and must be newer, not just
  different, so a downgrade by the package manager is not undone silently.
- **Manual means manual**: no request to GitHub unless the user asks (a menu item, a flag):
  a disk tool that phones home is not expected. Say what will be fetched, from where, and its
  size, before fetching.
- **Inside a sandbox**: a Snap's strict confinement does not run what it downloads; Flatpak
  lets an app run files from its own data folder. A sandboxed build either updates through its
  store or not at all.

## The static binary is the default

- **One file runs everywhere.** musl linked in, no glibc floor, no system library for the
  window (Wayland and X11 are spoken in pure Rust, fonts drawn by `ab_glyph`): old
  distributions, Alpine, busybox, a NAS. `deploy.yml` checks that it is static.
- **It is as fast as anything else measured.** The walk is the first of the performance goals,
  so the release's build settings were measured again for this decision (below).
- **It needs no root to install and no sandbox to run in**, and a sandbox is exactly what a disk
  usage tool fights (Flatpak and Snap, below): it reads the whole disk, the mount table, sysfs,
  and as root the block device.

The tarball holds `duscape`, `LICENSE`, `LICENSE-jemalloc`, and `share/` — the desktop files laid
out as they go under a prefix:

```bash
tar -xzf duscape-*-x86_64-unknown-linux-musl.tar.gz
install -Dm755 duscape ~/.local/bin/duscape     # on the PATH: the .desktop file runs `duscape`
cp -r share ~/.local/                           # the app menu entry, its icon, its metadata
```

### The build settings, measured (2026-10-04)

WSL2 (kernel 6.6, Ubuntu 24.04), 12 threads, the WSL disk's ext4 root: 791k entries, warm,
`--benchmark --bench-stage sharded /` (the app's scan: walk and tree build). The four builds
interleaved, 21 rounds, medians:

| Build | Wall | CPU (user + sys) | Size |
|---|---|---|---|
| **A** musl, jemalloc, `opt-level = "s"` (the release) | 0.655 s | 4.82 s | 3.3 MB |
| B glibc, its malloc, `"s"` | 0.740 s (+13%) | 4.65 s (−3.5%) | 2.9 MB |
| C musl, jemalloc, `opt-level = 3` | 0.638 s (−2.6%) | 4.66 s (−3.3%) | 4.3 MB |
| D musl, jemalloc, `"s"`, `target-cpu=x86-64-v3` | 0.678 s (+3.5%) | 4.91 s (+1.8%) | 3.3 MB |

Run to run, one build's wall time spans ±15% here (A: 0.55–0.91 s), and a first 7-round run
ordered them differently (glibc −5%, opt 3 −13%). Read: **no build is reliably faster than the
release.** glibc is not; `x86-64-v3` (AVX2) gains nothing, since the walk waits on the kernel and
the build on memory, and would refuse to start on older CPUs; `opt-level = 3` may be a few
percent of CPU for a binary 30% bigger — inside the noise here, and `docs/scan-performance.md`
found `"s"` as fast as `3` on bare metal. Before changing any of it, measure on bare metal with
`docs/probes/bench-matrix.sh`; WSL2's filesystem path is not a Linux disk's. What stands from
before: musl's own malloc made the scan 7x slower and mimalloc 2x, so musl without jemalloc is
never the release.

## Desktop integration (`packaging/linux/`)

Every package below needs these, and the tarball carries them:

- **`io.github.angch.duscape.desktop`** — the app menu entry. `Exec=duscape --gui %f`: `--gui`
  so the window opens whatever the launcher's stdin is (`front::choose`), `%f` a folder dropped on
  it. No `MimeType=inode/directory`: on some desktops the newest handler of a type becomes its
  default, and duscape would open every folder (a test keeps it out). `StartupNotify=false`: the
  window does not send the startup-complete message, and a launcher waiting for it shows a busy
  cursor for half a minute. Categories `System;Filesystem;`, where GNOME's Disk Usage Analyzer
  sits (`Utility` beside them is a second main category: listed twice).
- **`io.github.angch.duscape.metainfo.xml`** — AppStream, what GNOME Software, KDE Discover and
  Flathub show. Its newest `<release>` must be the workspace's version (tested), so a version bump
  adds a release there. It has no `<screenshots>`, which Flathub requires; they need images at a
  stable URL.
- **`icons/hicolor/<n>x<n>/apps/io.github.angch.duscape.png`**, 16 to 512 — the treemap icon,
  drawn by `duscape_viewer::icon` and tested against the drawing like the `.ico` and `.icns`
  (`DUSCAPE_WRITE_ICON=1` writes them).
- **The app ID, `io.github.angch.duscape`** (`duscape_viewer::APP_ID`), is the window's Wayland
  app ID and X11 class too. A Wayland compositor finds a window's icon and name only through the
  `.desktop` file of that ID — before it, the window had no icon on Wayland. Reverse-DNS on the
  GitHub account, as Flathub requires for an app with no domain of its own.

Checked with `desktop-file-validate` and `appstreamcli validate --pedantic` (AppStream 1.0.2):
clean. `viewers/shared/tests/packaging.rs` holds the files to the app ID and the version.

## deb and rpm: built with every release

`deploy.yml` makes them in each Linux job, from the binary just built (the deb's
`/usr/bin/duscape` is compared with the tarball's, and the x86_64 one installed and run). The
static binary needs nothing from the system, so one `.deb` and one `.rpm` an architecture
serve every Debian/Ubuntu and Fedora/RHEL/openSUSE release, with no `Depends`. The tool is
[nfpm](https://nfpm.goreleaser.com/): one YAML file, deb, rpm, apk and Arch packages from the
binaries `deploy.yml` already builds, on the same Linux runner.

- **Contents**: `/usr/bin/duscape`; the three `packaging/linux` parts under `/usr/share`;
  `/usr/share/doc/duscape/copyright` (Debian policy) with every notice the binary carries —
  see "Licences" below.
- **Recommends, not depends**: `xdg-utils` (open, show in the file manager), `wl-clipboard` or
  `xclip` (the terminal viewer's clipboard; the window owns X11's itself), `fontconfig`
  (`fc-match` finds the window's fonts; without it, well-known paths). Each degrades when
  missing.
- **Architectures**: `amd64`/`x86_64` and `arm64`/`aarch64`, from the two musl builds.
- **Where they go**: the GitHub release. An apt or dnf repository later (signed: a GPG key
  for `Release` and `repomd.xml`, kept as a CI secret), serving these same files. **Not into
  Debian or Fedora themselves**: both build from source, so theirs would not be the canonical
  binary — see "Getting into Debian itself" below; Fedora's case is much the same (its `rust-*`
  packages, its own builders).
- **Cost**: done — `packaging/linux/nfpm.yaml` (nfpm 2.47.0, pinned by hash), two CI steps,
  the copyright file made from `LICENSE` and jemalloc's. Tried locally: the deb's contents and
  modes are as listed, its binary identical to the build's; the rpm is built (no `rpm` tool
  was at hand to list it).

## Getting into Debian itself: what it would take

Our own deb (above) is the canonical binary in a package. A package *in Debian* — what
`apt install duscape` finds with no repository added, and Ubuntu's universe after it — is a
different thing, and these are the considerations, roughly in the order they would bite.

**It cannot be the canonical binary.**

- Debian builds every package from source on its own build machines; a prebuilt binary is not
  accepted. Debian's duscape would be Debian's build: glibc, dynamically linked, the system
  allocator (`tikv-jemallocator` is only on 64-bit musl), Debian's crate versions. The
  canonical rule cannot hold there, so it would be a second, distribution-maintained build —
  measured separately (glibc's malloc was within the noise on the walk, above, but the
  window's layout was not measured), and bug reports from it told apart (`--version` should
  say which build it is).
- **The self-update must be off in it.** Debian does not ship software that downloads and runs
  newer code of itself; updates come through apt. The updater, when built, belongs behind a
  cargo feature that a distribution build leaves out, and it should refuse to hand over from a
  binary under `/usr` anyway.

**Every crate must already be in Debian.**

- Debian packages Rust crates one by one (`librust-<crate>-dev`, maintained by the Debian Rust
  team in `debcargo-conf`), and an application builds against those, not against vendored
  sources. Each crate in our `Cargo.lock` that Debian lacks, or has at an incompatible
  version, must be packaged first, each through the NEW queue. `cargo debstatus` lists them
  all, and `docs/probes/debian-deps.py` checks ours: each direct dependency of a glibc Linux
  build against the versions sources.debian.org lists, by the requirement in `Cargo.toml`
  (not `Cargo.lock`: Debian builds against requirements, and brings the indirect ones with
  its packages of the direct). **2026-10-04: every one is in Debian.** Four were not, and
  were dealt with rather than left to a Debian patch — an optional dependency, or one for
  another platform, would still have needed one, since Cargo resolves those too:
  - `fontdue` (the Linux window's text) → `ab_glyph`, which Debian packages: the same
    sizes (by the em), a snapshot differing by anti-aliasing alone (3,695 pixels of
    896,800, by at most 21 levels in 255), paint times alike (2.6–5.9 ms against
    2.9–5.2 ms a frame on `/usr`).
  - `dua-core` (the BSDs' walk, and the benchmark's baseline) → `scanners/src/portable.rs`,
    a walk on `std::fs`: with `-x` on `/usr` the same 171,652 entries and bytes as the
    native walk, in 0.43 s against `dua-core`'s 1.55 s (native: 0.11 s).
  - `embed-manifest` (the MSVC build's manifest, a Windows-only build dependency that
    every platform resolved) → the manifest written as a resource by our own
    `resources.rs`, as the GNU build already did: one `RT_MANIFEST` in each exe, checked.
  - `x11rb` 0.14 (Debian has 0.13.2) → the requirement opened to `>=0.13.2, <0.15`: the
    window builds against 0.13.2 and ran on it (an X11 snapshot under WSLg). Our own
    builds lock 0.14.
  Run the probe after adding a dependency; the indirect ones Debian resolves itself, but
  whether our code builds against Debian's versions only a Debian build shows.
- Debian is often a version or two behind; duscape may need patches to build against older
  crates, or to wait for them.
- Ubuntu sometimes accepts vendored crates for an application; Debian generally does not. An
  Ubuntu-only package is the shorter road, if Debian is not the goal.

**The workspace and its other platforms.**

- The workspace has crates for Windows (`windows-sys`) and macOS (`objc2`, AppKit); the Linux
  build never compiles them, but their dependencies are in `Cargo.lock` and `Cargo.toml`.
  Debian patches such dependencies out (a `debian/patches` series); keeping the Linux
  binary's dependency set clean upstream (target-specific tables, as now) makes that small.
- The binary package would be `duscape` (the `viewers/tui` crate with its `gui` feature,
  which brings the Linux window crate); `libduscape` and `duscape-scan` would not be published
  as `librust-*` crates unless something else wanted them.

**Architectures: Debian builds on all of them.**

- amd64, arm64, armel, armhf, i386, ppc64el, riscv64, s390x and more; CI tests x86_64 (glibc
  and musl) alone. `docs/probes/cross-arch-tests.sh` runs `libduscape`'s and `duscape-scan`'s
  tests on i686 and armv7 (32-bit) and s390x (big-endian) under QEMU, with no root. On
  2026-10-04:
  - **i686 and armv7: all pass** (108 and 83). `FileOrFolder` is 16 bytes there too — the
    exact assertion in `model::tests` holds on 32-bit, contrary to a first guess here.
  - **s390x: one byte-order bug, fixed.** `scanners/src/linux/btrfs/extents.rs` read the btrfs
    extent items from `BTRFS_IOC_TREE_SEARCH_V2` in the CPU's byte order. Each item's header
    is the kernel's, in the CPU's order, but its body is btrfs's on-disk format, little-endian;
    on a big-endian machine every compressed size was byte-swapped (the test's 108,892 bytes
    read as 11.6 exabytes). The body is now read little-endian, and the test builds it so —
    run with the old parser on s390x it fails, so it guards the fix wherever it runs
    big-endian. The other on-disk parsers (`ext4.rs`, `ntfs.rs`, `mft.rs`) already read
    little-endian, and their tests pass on s390x.
  - **s390x: one failure that is the emulator's**: a test that starts `zsh` where none is
    installed. Under QEMU's user mode, glibc's `posix_spawn` cannot report a program that
    failed to start, so the test's guard believes zsh ran. On a real s390x it skips as it
    should.
- Not run at all: ppc64el, riscv64, armel; the Linux window crate and the terminal viewer on
  any of these (their tests need a display or a terminal less, but were not tried); anything
  needing root (the ext4 device reader, btrfs compressed sizes: their parsers are tested, the
  ioctls are not).
- To keep it so, CI would run the probe's s390x leg (nothing guards byte order on x86 alone);
  until then, run the probe before a Debian upload.

**Debian Policy and lintian.**

- **Licensing, in full**: `debian/copyright` in the machine-readable format, every file's
  copyright holders and licence — duscape's (`LICENSE`: diskonaut's author, the fork, and the
  contributors by name), and anything embedded. The crates are covered by their own packages,
  with `Static-Built-Using` recording what was linked in.
- **Generated files in the source**: `duscape.ico`, `duscape.icns`, the hicolor PNG files
  (drawn by `duscape_viewer::icon`, held to it by tests: fine, the source of each is in the tree,
  but a reviewer will ask), and `docs/explainer/explainer.js`, compiled from the
  `explainer.ts` beside it — rebuild it or exclude `docs/explainer` from the Debian tarball
  (`Files-Excluded`). The DOS port's tools (FASM, CWSDPMI) are fetched by `make dos`, not in
  the tree: good, and the DOS port need not be built.
- **No network during the build**: the build scripts download nothing on Linux (the Windows
  manifest and resources are Windows-only); the tests must not either, and any test that needs
  root, a particular filesystem, `/proc` details a build chroot lacks, or a display must skip
  rather than fail (the fixtures already need root and are not part of `cargo test`).
- **A man page**: lintian flags a binary without one. `clap_mangen` can generate it from the
  CLI definition at build time, and shell completions (`clap_complete`) likewise.
- **Desktop files**: the `.desktop` file, AppStream metadata and hicolor icons in
  `packaging/linux` are what Debian wants already, and validate clean.
- **Static linking**: Rust links its crates statically, which Debian accepts for Rust; the
  musl static release is not what Debian would build.

**Upstream practice Debian looks for.**

- Signed release tags or tarballs (`debian/upstream/signing-key.asc`) and a `debian/watch` on
  the GitHub releases; a stable version scheme (0.x is fine, but every tag a release).
- A changelog and a security contact; prompt releases for security fixes, since Debian's own
  fixes would otherwise be patches against an old version.
- The project's own caveat: the README calls duscape experimental and not battle-tested.
  Debian maintainers will read that; a stable release should come first.

**People and process.**

- A Debian maintainer: an ITP bug (Intent To Package), the package on salsa.debian.org, a
  Debian Developer to sponsor uploads (mentors.debian.net), most naturally inside the Debian
  Rust team, who also review the crate packages. Then the NEW queue for duscape and each new
  crate; then testing migration; and Debian's release freeze dates decide when it reaches a
  stable release. Ubuntu picks it up from Debian on its own.
- Name clash check: no `duscape` source package in Debian (checked 2026-10-04); upstream
  `diskonaut` is not packaged there either.

**Short version**: possible, but it is a project of its own — mostly packaging crates —
and its result is a distribution build beside the canonical one, with the self-update taken
out. Our own deb on the GitHub release (and an apt repository for it) gives Debian and Ubuntu
users the canonical binary now. The architecture check above was worth doing regardless: it
found a real big-endian bug, now fixed.

## Flatpak: possible, but the sandbox changes what is measured

- **The host's root is not `/`.** With `--filesystem=host`, home and most of the tree appear at
  their paths, but `/usr`, `/etc`, `/lib` and friends are the runtime's; the host's are under
  `/run/host/` (`--filesystem=host-os`). A scan of `/` measures the sandbox.
- **The mount table is the sandbox's.** `/proc/self/mountinfo` and `/proc/self/mounts` list the
  sandbox's mount namespace. They drive the crossing rules, bind-mount duplicates, `-x`, the
  loop check and the volume chooser (`os::volumes`) — all would need to learn the `/run/host`
  layout to be right.
- **No root.** The ext4 device read (`ext4.rs`), btrfs compressed sizes and the block read-ahead
  are root-only; the scan works without them, slower cold.
- **What works**: opening files (`xdg-open` is the portal's), the window's own clipboard,
  deleting (with write access). Showing in the file manager needs
  `--talk-name=org.freedesktop.FileManager1`; Trash via `gio` is in the runtime, or the fallback.
- **The terminal viewer** is `flatpak run io.github.angch.duscape --tui`: awkward, so a Flatpak
  is for the window.
- **Flathub's rules**: it prefers apps built from source offline (`flatpak-cargo-generator`
  vendors the crates), against the runtime's glibc. That is not the canonical binary. A manifest
  can instead take the release's static binary by URL and SHA-256 (a `file` source), which
  Flathub accepts for some apps and questions for open-source ones; ask before building it. The
  metadata needs screenshots; `--filesystem=host` must be justified (disk analysers on Flathub
  hold it).
- **Cost**: the manifest is small; teaching the scanner the sandbox's layout is the real work,
  and until then duscape inside answers differently from outside.

## Snap: classic confinement or nothing

- **Strict confinement hides what matters.** The `home` interface leaves out dot-folders —
  `~/.cache`, `~/.local`, `~/.cargo`, often the biggest in a home. Beyond home: `removable-media`
  (`/media`, `/mnt`), `system-files` (each path, Store approval), `mount-observe` (the mount
  table), `hardware-observe` (sysfs, for where a file's blocks are), `block-devices` (the ext4
  device read; super-privileged). A strict snap would be a different, smaller tool.
- **Classic confinement** runs it as the deb would, but the Snap Store grants it only on manual
  review, for tools that cannot work otherwise; a disk analyser has a case, not a certainty.
- **What it would add over the deb**: automatic updates for Ubuntu users who prefer the Store.
- **Cost**: `snapcraft.yaml` with the `dump` plugin on the release's tarball (by URL and
  SHA-256: the canonical binary, as the rule asks) is small; the review is the uncertainty.

## AppImage: not needed

An AppImage bundles libraries for a binary that needs them; the static binary needs none, and a
tarball with `share/` is the same thing without the runtime.

## Licences in a package

What the Linux binary carries, and what a package must say:

- **duscape**: MIT (`LICENSE`: diskonaut's author's line and the fork's).
- **jemalloc**: BSD-2-Clause, linked into the musl builds. Permissive, no copyleft: it does not
  clash with MIT, Apache-2.0, Zlib or Unicode-3.0 (or with GPL code, were any linked). Its one
  condition for a binary is its notice in the documentation that comes with it — shipped as
  `LICENSE-jemalloc` in the tarball, and the deb's `copyright` file would list it. (`cargo deny`
  does not see it: the crate around it, `tikv-jemalloc-sys`, is MIT/Apache-2.0, the C inside BSD.)
- **musl**: MIT, linked in by the musl target. Its notice belongs with the binary too, and is
  **not shipped yet**.
- **The Rust crates**: MIT, Apache-2.0, Unicode-3.0, Zlib (`deny.toml`'s allow list). MIT and
  Unicode ask for their notices with a binary, Apache-2.0 for its text and any NOTICE files.
  **Not shipped yet**: `cargo about generate` makes the one file, which the tarballs, the deb's
  `copyright`, the Windows zip and the installer should all carry.
