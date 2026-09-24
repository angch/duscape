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

Other flags: `--max-depth N` stops the descent (a partial scan), `--threads N` sets the worker
count, `--bench-repeat N` repeats each stage, `--single-thread` forces one worker,
`--bench-shards N` and `--bench-shard-depth N` vary the parallel build. Whatever is changed,
`sharded`'s totals must stay identical to `pipeline`'s: that comparison is the correctness check.

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

`scan-performance.md` has every measurement, the reasoning, and notes for repeating the exercise
on another platform.
