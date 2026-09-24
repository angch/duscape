# Scan roadmap: what is left, and how to work through it

What was measured in September 2026 is in `scan-performance.md`. This is the plan for what
comes next, written so the steps can be run one at a time, on whichever machine is at hand,
and compared. Every step is an experiment with a **gate**; a step that fails its gate is written
up in `scan-performance.md` as a negative result and not merged.

## The rules every step follows

1. **Measure before and after with the same harness.** `docs/probes/bench-matrix.sh` runs the
   standard set (below) and writes `docs/benchmarks/<host>-<date>.md` with the machine, kernel,
   filesystem, disk and build recorded next to the numbers, so a row from one machine can be
   read against a row from another. A step's write-up quotes rows, not memories.
2. **Totals are the correctness check.** `--bench-stage sharded`'s totals must equal
   `pipeline`'s on every tree used, and `make test-fs` must pass. A walker that reads the disk
   differently must agree with the existing walker on the fixtures to the byte.
3. **Three trees**, chosen once per machine and kept: a *project* tree (a few hundred thousand
   entries, many small directories), a *home*-sized tree (millions of entries), and a
   *hard-link-heavy* tree (a package cache, snapshots). Name them in the benchmark file.
4. **Warm, cold, and as root where it matters.** Cold needs the caches dropped before every
   run; root is where the device-reading steps live. A step says which of the three regimes it
   expects to move, and is judged on that one.

## The matrix to cover

| dimension | values | why it matters |
| --- | --- | --- |
| platform | Linux glibc, Linux musl (the release), macOS, Windows | different walkers, allocators, kernels |
| filesystem | ext4, XFS, btrfs, f2fs, tmpfs, NTFS (Windows), APFS | on-disk layout, bulk APIs, reflinks |
| machine | VM on virtio (this box), bare-metal NVMe, SATA SSD, spinning disk, 32+ cores | syscall cost, IOPS floor, seek cost, parallelism |
| privilege | user, root / administrator | device reads, dm tables, bulkstat, MFT |
| cache | warm, cold | CPU-bound against I/O-bound |

Results so far come from two cells, both ext4 on Linux: an 8-core VM on a virtio SSD with the
host's cache under it (`docs/benchmarks/angch-noble-*.md`), and a 16-core bare-metal laptop on
NVMe (`badwolf-20260925-full.md`). Bare metal walks about 2.5M entries/s warm against the VM's
1.5M, and diskonaut and `diskus` finish within a few percent of each other on both, so warm, the
walk is the kernel's cost on either. Cold, the NVMe is 2.5–3x the warm time where the VM was
3–4x, and `diskus` leads by 10–25% there: the difference in how the two issue reads in flight
shows once the disk is the floor. Cold on a spinning disk has not been seen at all, and it is
where the inode-order and prefetch work should show most.

## The steps, in order of expected payoff

### 0. Baseline capture — done here, repeat on each machine

`docs/probes/bench-matrix.sh TREE...` on the three trees; commit the file it writes under
`docs/benchmarks/`. Gate: none; this is the yardstick. Do it first on every new machine, and
again after any step that merges.

### 1. ext4 metadata from the device, as root — a spike (done here: 7x warm, 10x cold)

*Linux, ext4, root, warm and cold.* Warm, the scan is bound by the kernel's per-entry work in
`statx` and `getdents64` (7 s of system time for 2.24M entries here), and the tree build is a
tenth of it. The same information is 92 MiB of inode tables plus the directory blocks, readable
sequentially from the block device, as WizTree reads NTFS's MFT. The spike reads the superblock,
group descriptors and inode bitmaps, then every used inode table block in order, and sums
`i_blocks`, timing it and comparing the sum against a `sharded` scan of the mount. No names, no
tree yet: it measures the floor.
- Gate: the sum agrees (within the journal's staleness) and the read is at least 3x faster
  than `walk` on two machines, one of them not a VM.
- Risk: consistency on a live filesystem — delayed allocation and an uncheckpointed journal
  mean recent writes are not on the device yet. Accept seconds of staleness, and say so in the
  title, as WizTree does.
- Result on this machine (`scan-performance.md`, "Roadmap step 1"): 0.20s warm and 0.44s cold
  for 2.14M inodes against 1.77s and 4.50s for the walk; the sum within 1 MB of `df`. Half the
  gate; the second machine is still to come.

### 2. The ext4 device walker (done here: cold 1.7x the kernel walk as root, 2.2x unprivileged; warm 7–10%)

*Linux, ext4, root.* On the spike's numbers: directory blocks parsed for names (linear and
htree leaves both hold `ext4_dir_entry_2`), extent trees followed for directories larger than
one block, inline data honoured, the tree built from `(parent inode, name, size)` without paths.
Behind a flag first (`--device-read`), then on by default as root where the mount is ext4 and
the device opens, falling back per mount otherwise. Must interoperate with the second pass and
the hard-link ledger unchanged.
- Gate: totals identical to the walker on every ext4 fixture and on the three trees; 3x on
  `walk` warm; cold at least 2x.
- Work: an ext4 on-disk reader (`scanners/src/ext4.rs`), a few hundred lines, tested against
  images made by the fixtures.
- Result on this machine (`scan-performance.md`, "Roadmap step 2"): the cold gate met, the
  warm one not — 2.5 GiB of directory blocks out of the page cache costs what the kernel's
  `statx` threads cost on eight cores. On by default as root; `--no-device-read` opts out.
- Result on bare-metal NVMe (`badwolf-20260925-full.md`, 16 cores, kernel 7.0): totals
  identical, but the cold gate is not met and warm it is a loss — cold 1.06–1.4x the kernel
  walk as root (1.09x on 673k entries), warm 0.7–0.8x (343 ms against 281). The inode survey
  alone is 97 ms at 4.2 GiB/s, so the device is not the cost; the generation-by-generation
  parse serialises what the kernel's `statx` threads run on sixteen cores at a third of the
  VM's per-call price. Whether it should stay on by default there is open: as it stands the
  default costs a root user on such a machine 25–40% warm for a 10% cold gain.

### 3. XFS bulkstat, as root (needs an XFS machine; not run here)

*Linux, XFS, root.* `XFS_IOC_BULKSTAT` returns every inode's stat in bulk, without paths, and
is refused unprivileged (measured, `scan-performance.md`). As root it removes the `statx` call
per entry; `getdents64` still supplies names, joined by inode number.
- Gate: totals identical on the XFS fixtures; `walk` at least 1.5x warm.

### 4. Windows: the MFT, as administrator (needs a Windows machine; not run here)

*Windows, NTFS, elevated.* The floor there is a fixed kernel and filter-driver cost per
directory handle, 3x worse on the system volume (`scan-performance.md`, 2026-09-24). Reading the
MFT with `FSCTL_GET_NTFS_VOLUME_DATA` and the raw volume bypasses it. `ntfs.rs` already parses
file records for the metadata files; this extends it to the whole table, with the parent
reference in each record's `$FILE_NAME` attribute giving the tree.
- Gate: totals identical to the handle walker on a data volume and on `C:\`; at least 3x.
- Needs a Windows machine with an admin session; CI cannot run it.

### 5. Workers that adapt (tried here: negative — 3–4% slower cold, no gain warm; not merged)

*All platforms.* The profile showed four builders starved to 982 ns an entry under 24 walkers
on 8 cores, while cold needs those 24 for queue depth. A walker pool that grows while its
workers block in the kernel and shrinks while they are CPU-bound would take the best of both,
and remove the per-machine constant (`default_scan_threads`).
- Gate: warm not slower than today on the 8-core and a 32-core box; cold not slower than
  today's 24 workers.
- Measure: time in `statx`/`getdents64` per worker (a sampled `Instant` is enough).
- Result (`scan-performance.md`, "Roadmap step 5"): the cold ramp costs more than the warm
  side saves, because warm was never builder-bound on the wall clock. Worth another look only
  on a machine with many more cores than the disk can feed.

### 6. Inode-table readahead, as root, cold (largely overtaken by step 2 on ext4)

*Linux, ext4, root, cold.* After the directory-block prefetch, the remaining 10k cold reads are
inode blocks at 11 KiB each. With the device open, the superblock and group descriptors say
where every inode is; a directory's children, already sorted by inode, can be advised in one
range per run before their `statx` calls. Falls out of step 1's superblock code.
- Gate: cold reads down by half on `project`; warm unchanged.
- Since step 2, a root scan of ext4 reads the device and does not make these reads at all; this
  step now matters for the kernel walk as root on other filesystems, and for `--no-device-read`.

### 7. The model's cache misses (done here: 5–6% of the build, under the 10% gate; kept, small)

*All platforms, warm.* From `--bench-profile`: `LinkedFile` is ~80 bytes, so 735k ledger
lookups miss cache — box the charged set, keep the first few directories inline. Resolving a
directory costs ~15 name comparisons per level, and consecutive directories share parents — a
cache of the last path's positions skips most of them.
- Gate: `tree-only` at least 10% faster on the hard-link-heavy tree; totals identical.
- Result (`scan-performance.md`, "Roadmap step 7"): 5–6%. The remaining cost is the pointer
  chase down the folder chain and the ledger's hash misses, which want a different layout,
  not fewer operations.

### 8. Whole-queue inode order (needs a spinning disk; not run here)

*Linux, cold, spinning disks above all.* Serving the walk's queue smallest inode first halved
the directory-block runs again over what is shipped, but sorting the local stacks cost 6% warm.
A heap on the shared queue alone may get most of it for less. Only worth judging on a spinning
disk, where the run length is the seek.
- Gate: cold on an HDD at least 10% faster; warm within 1%.

## Not worth revisiting

Measured dead, with the numbers in `scan-performance.md`: `statx` masks, `AT_STATX_DONT_SYNC`,
skipping stats by `d_type`, io_uring `statx` (warm and cold), per-crate `opt-level` under LTO,
thin LTO, mimalloc, PGO in the release pipeline.

## Running a step on another machine

```sh
git clone … && cd diskonaut && cargo build --release -p diskonaut-angch
cargo install hyperfine diskus              # the comparison
docs/probes/bench-matrix.sh ~/src ~ ~/.cache   # or whichever three trees
```

The script says what it could not do (no root for a cache drop, no `diskus`) in the file it
writes. Commit that file; then do the step; then run the script again and commit that too.
