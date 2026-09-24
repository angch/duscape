# Benchmarking the scan

`--benchmark` scans headlessly and prints timings instead of starting the UI, so scanning strategies
can be compared on a real tree:

```sh
diskonaut --benchmark /                    # every stage, whole disk
diskonaut --benchmark --bench-stage sharded ~/src
diskonaut --benchmark --max-depth 4 /      # partial scan, for a quick iteration loop
diskonaut --benchmark --threads 6 --bench-repeat 3 /
```

| Stage       | What it measures                                                                  |
| ----------- | --------------------------------------------------------------------------------- |
| `dua-walk`  | the general-purpose `dua-core` walk alone                                         |
| `dua-tree`  | that walk feeding the folder tree                                                 |
| `walk`      | the walk diskonaut uses now, alone                                                |
| `tree`      | that walk feeding the folder tree                                                 |
| `tree-only` | the folder tree alone, from entries collected first                               |
| `pipeline`  | scan and tree build on separate threads, the single-threaded build `sharded` replaced |
| `sharded`   | the walk feeding several tree builders at once, merged at the end: what the app runs |
| `refined`   | `sharded`, then the second pass over small files that may share extents           |

| `ext4-raw`  | root, Linux, ext4: every live inode's size read straight from the device's inode tables, no names — the floor |

Other flags: `--max-depth N` stops the descent (a partial scan), `--threads N` sets the worker
count, `--bench-repeat N` repeats each stage, `--single-thread` forces one worker,
`--no-device-read` asks the kernel for every entry even as root on ext4 (the two rows the
harness prints as root differ by exactly that),
`--bench-shards N` and `--bench-shard-depth N` vary the parallel build. Whatever is changed,
`sharded`'s totals must stay identical to `pipeline`'s: that comparison is the correctness check.

## Inside the tree build

`--bench-profile` adds, after each stage that builds a tree, where the builders' time went:

```
  build profile: 311671 directories, 2240019 entries, 0.656s of builder time
    resolve 0.119s  10.5 folders/dir  152.2 name compares/dir  1108 indexes built
    place   0.168s  75 ns/entry
    sizes   0.015s  13 ns/entry over the 156222 directories with no shared blocks
    ledger  0.187s  155460 directories, 734679 sightings  6.9 link comparisons and 27.8 ancestor steps each
    replay  0.146s  charge 0.124s  take back 0.022s  155460 directories, 734679 sightings, 561706 taken back
```

`resolve` is finding each directory's folder down from the root, `sizes` the pass over its
entries, `ledger` the same pass where it charges shared blocks, `place` putting the entries in,
and `replay` the reconciliation after a parallel build. In `sharded` the builders run in
parallel and their times add up, so the total exceeds the wall clock; compare phases, not the
sum, and compare `tree-only` runs for the build on its own.

For a sampling profile, build with `--profile profiling` (release with symbols) and use
`docs/probes/gdb-sample/`, which works where `perf` is not allowed.

On macOS the scan uses `getattrlistbulk(2)` directly, requesting only the name, type, flags, inode,
and one size field per entry — the general-purpose walker asks for the whole `stat` set and pays an
extra path lookup per directory.

## Warm and cold, against `diskus`

Every number in `scan-performance.md` before September 2026 was taken with the filesystem's
metadata already in memory. A first scan after boot is not like that, and behaves differently: the
walk is bound by how many reads it keeps in flight, not by the CPU. `probes/bench-diskus.sh` runs
[`hyperfine`](https://github.com/sharkdp/hyperfine) over [`diskus`](https://github.com/sharkdp/diskus)
and diskonaut's `sharded` and `refined` stages, warm and then cold, dropping the kernel's caches
before every cold run:

```sh
cargo install hyperfine diskus
docs/probes/bench-diskus.sh --both --runs 5 ~/src /data
```

The cache drop needs root: run it as root, or allow `/usr/bin/tee /proc/sys/vm/drop_caches`
without a password in sudoers, or `sudo -v` first. Without it the cold set is skipped and said so.
As root, diskonaut also reads directories' blocks ahead through the block device (ext4), which is
worth measuring separately: `sudo docs/probes/bench-diskus.sh --cold DIR`. The results, and what
they led to, are under "Cold cache" in `scan-performance.md`.

## Across machines

`probes/bench-matrix.sh TREE...` is the standard measurement: it records the machine, kernel,
filesystem, disk and build, runs the warm, cold and root comparisons above on the trees given,
adds the build profile, and writes `docs/benchmarks/<host>-<date>.md`. Run it on a new machine
before anything else and commit the file; `scan-roadmap.md` is the plan those files feed.

On Windows, `probes/bench-matrix.ps1 TREE...` writes the same file: the machine from CIM, the
volume and the physical disk behind each tree, and the warm rows against `diskus` and
[WizTree](https://wiztreefree.com) — timed in its export mode (`/export`, folders only,
`/admin=0`), which scans, writes a CSV and exits, and whose figures for each tree are listed for
the sizes cross-check. From an elevated shell it also runs the cold rows, and every row is
elevated (diskonaut reading the volume's metadata files, WizTree reading the MFT); unelevated
there are neither, and the file says so. The cache is emptied by `probes/drop-cache.ps1`, the
`drop_caches` of Windows: every volume's write cache flushed, the system file cache's working
set trimmed (`SetSystemFileCacheSize`) and the modified and standby page lists purged
(`NtSetSystemInformation`, what RAMMap's "Empty" menu calls), so NTFS's MFT and directory
blocks are read from the disk again. It takes about fifteen seconds on a 128 GiB machine, which
hyperfine leaves out of the timing, and makes a 415k-entry NVMe volume scan 3.4x slower than
warm — the disk, not a reboot: what a kernel keeps outside the page lists stays.

`scan-performance.md` has every measurement, the reasoning, and notes for repeating the exercise
on another platform.
