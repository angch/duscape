# Probes

The small programs and scripts behind the measurements in `../scan-performance.md`. Each
answers one question about a platform, a filesystem or a kernel call, outside the app, so the
answer can be had in minutes and reproduced on another machine. None is built by Cargo; the
build line is in each file's header. The sections of `scan-performance.md` that quote a probe
name it, and the date there is the date the question was asked.

## The standard measurement

| probe | what it does |
| --- | --- |
| `bench-matrix.sh` | The roadmap's yardstick on Linux and macOS: the machine, filesystem and disk, then warm, cold and root timings of duscape against `diskus` (and WizTreeMac) on the trees given, with `hyperfine`, and the build profile. Writes `../benchmarks/<host>-<date>.md`; commit it. |
| `bench-matrix.ps1` | The same on Windows, in the same file format: duscape against `diskus` and WizTree's export mode, warm and cold, elevated where it can. |
| `drop-cache.ps1` | Empties Windows's file cache for a cold run (the system cache's working set trimmed, the standby list purged), what `drop_caches` does on Linux. Elevated. Used by `bench-matrix.ps1`. |
| `bench-diskus.sh` | Duscape against `diskus` alone, warm and cold, with `hyperfine`, for a quick before-and-after on one machine. |
| `gdb-sample/` | A poor man's sampler where `perf` is not allowed: runs the target so `gdb` may attach, stops it every few milliseconds and counts stacks. |

## Linux

| probe | question |
| --- | --- |
| `statbench.c` | One thread, `getdents64` recursion, varying only how each entry's metadata is fetched (`statx` masks, `fstatat`, `d_type` alone…): which per-entry strategy is cheapest? |
| `mtwalk.c` | A deliberately naive in-process parallel walker (one shared queue, one mutex, `getdents64` + `fstatat`): what does one process achieve across N threads, against `jwalk`'s collapse past eight? |
| `dirblock_prefetch.py` | Can a cold ext4 walk read fewer, larger blocks by finding each directory's block with `FS_IOC_FIEMAP` and reading the run ahead through the device? The idea behind `linux/dirblocks.rs`. |
| `bulkstat_probe.c` | Is `XFS_IOC_BULKSTAT` usable unprivileged? (No: `EPERM`.) |
| `fsmap_scan.c`, `fsmap_dump.c` | Every extent on an XFS filesystem through `XFS_IOC_GETFSMAP`, summed per owning inode: sizes without a `stat` per file; the dump is the raw records, for the redaction check. |
| `btrfs_compression_check.sh` | Does `stat`'s block count already reflect btrfs compression? Answered by the fixtures: no; as root duscape reads the extent items instead. |

## macOS

What APFS offers instead of a device read; the write-up is "macOS: what is left" (2026-09-29).

| probe | question |
| --- | --- |
| `bulkwalk.c` | The walker's `getattrlistbulk` loop with every knob on an environment variable — `THREADS`, `ATTRS` (full, no sizes, names alone), `OPEN=evtonly`, `NOFSTAT`, `BUF`, `ORDER` (depth first, children by file id, smallest id in the queue) — and several roots at once, so threads can be set against processes. Prints wall, system CPU per entry and the allocated total, which should match the app's. |
| `searchfs_probe.c` | The whole volume's catalog through `searchfs(2)`, no directory opened: how fast, how much kernel CPU, does a file-id range seek or scan, do two searches at once share the disk? (Serial, I/O-bound, no and no.) |
| `fsevents_probe.c` | The volume's persisted FSEvents log: how far back it reaches and how fast it replays, `BACK=N` ids from now. What roadmap step 9 — a saved tree brought up to date — rests on. |
