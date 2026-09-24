# Scan performance

Notes from making a whole-disk scan fast enough to be worth waiting for, on macOS first and then on
Linux. Everything below was measured with the `--benchmark` harness that ships in the binary, so the
numbers are reproducible rather than remembered.

Read in order, the file is a record of being wrong in useful ways: the macOS findings at the top
set expectations for Linux that the Linux measurements then contradicted. Where a later section
supersedes an earlier one there is a note saying so — the earlier claim is kept rather than edited
away, because what looked true and why is the useful part.

## Test machine

| | |
| --- | --- |
| Hardware | Apple M4 Pro, 14 cores |
| OS | macOS 26.6.2 (arm64) |
| Filesystem | APFS, 926 GiB volume, 884 GiB used, ~11M inodes |
| Layout | sealed system volume at `/` (`/dev/disk3s1s1`), data volume at `/System/Volumes/Data` (`/dev/disk3s5`) |

All runs were warm-cache: the same scan repeated back-to-back gave times within a few percent, so
these measure CPU and kernel work, not disk reads. A cold-cache scan of a mechanical disk would be
dominated by seeks and none of this would matter much.

## Results

Whole disk, `/`, both stages from one run of the same binary:

| | time | entries | unreadable | reported size |
| --- | --- | --- | --- | --- |
| before (`dua-tree`) | 59.9s | 20,300,480 | 986 | 1.4 TiB |
| after (`tree`) | 36.5s | 10,409,705 | 479 | 716.5 GiB |
| after, with `-x` | 36.4s | 10,389,061 | 473 | 688.8 GiB |

The entry count is the more interesting column. The volume has ~11M inodes, so the old scan was
visiting nearly everything twice, and the 1.4 TiB it reported was about 1.6x the 884 GiB actually
in use. It was not just slow, it was wrong.

Run-to-run variance is a few percent, and a whole-disk scan moves a little between runs as the
machine writes files; quieter runs of the new path landed at 35–37s. Treat the 1.5x as the claim,
not the individual seconds.

## How to reproduce

```sh
cargo build --release
./target/release/diskonaut --benchmark /                       # all stages
./target/release/diskonaut --benchmark --bench-stage sharded /  # just the app's real path
./target/release/diskonaut --benchmark --max-depth 4 /          # partial scan, fast iteration
./target/release/diskonaut --benchmark --threads 6 --bench-repeat 3 /
```

The stages nest, so subtracting one from the next attributes cost to a layer:

| Stage | What it measures |
| --- | --- |
| `dua-walk` | the general-purpose `dua-core` walk alone, entries discarded |
| `dua-tree` | that walk feeding the folder tree |
| `walk` | the walk diskonaut uses now, alone |
| `tree` | that walk feeding the folder tree |
| `tree-only` | the folder tree alone — entries are collected first, untimed, then fed to the model |
| `pipeline` | scan and one tree builder on separate threads, over a channel |
| `sharded` | scan feeding several tree builders, merged and replayed — the app's real path |

`dua-*` against the others is a like-for-like walker comparison on the same tree. `walk` against
`tree` is the cost of the data model. `tree` against `pipeline` is the cost of the channel, and
`pipeline` against `sharded` the cost of parallelising the build — which is a saving where the build
is the bottleneck (Linux) and a small loss where the walk is (Windows and macOS, one shard).
`tree-only` is the model's cost with the walk taken out of the measurement entirely — use it rather
than the `walk`/`tree` subtraction, which on Linux conflates the model with the walk stalling
behind a busy consumer.

Useful alongside it: `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) to see the
user/system split. That split is what turned out to matter most.

## Findings

### 1. The walk is everything; the data model is free

The first measurement worth taking. On a 3.07M-entry single-volume tree, warm, every stage comes
out within a few percent of every other:

```
dua-walk      7.127s   3073489 entries   431232 entries/s
dua-tree      7.975s   3073489 entries   385380 entries/s
walk          8.188s   3073488 entries   375377 entries/s
tree          8.363s   3073488 entries   367492 entries/s
pipeline      8.441s   3073488 entries   364106 entries/s
```

Building the folder tree, allocating a `PathBuf` per entry, and shipping everything across a
channel together cost a couple of percent against the traversal. On the whole disk the tree build
does not register at all — `walk` 37.99s against `tree` 38.02s over 10.4M entries, a difference
smaller than the run-to-run variance.

This is worth internalising before optimising anything here. The obvious-looking targets in the
data model — the per-entry `sync_channel(1)`, the `O(depth²)` allocation in the recursive insert —
are real inefficiencies and were worth fixing, but fixing them alone would have changed the
whole-disk time by a couple of percent. **Measure the walker first.**

### 2. `/` was traversed twice, and that was most of the win

macOS presents the data volume in two places at once: mounted at `/System/Volumes/Data`, and
grafted into `/` through *firmlinks* (`/Users`, `/Applications`, `/Library`, … are firmlinks whose
targets live on the data volume). A walk that follows both arrives at every user file twice.

Hence 20.4M entries against ~11M inodes, and 1.4 TiB against 884 GiB used.

Detecting this is fiddly because **`/` and `/System/Volumes/Data` report the same `st_dev`**
(16777233 on this machine). The usual `du -x` trick of comparing device numbers does not see the
boundary at all:

```
$ stat -f "%d %N" / /System /System/Volumes/Data /Users
16777233 /
16777233 /System
16777233 /System/Volumes/Data
16777233 /Users
```

What does work is comparing inodes across the mount. `getattrlistbulk` enumerates without crossing
mount points, so a directory that something is mounted over is *listed* with the inode of the
directory it covers; `open`ing it does cross, so `fstat` on the descriptor returns the mounted
volume's root inode instead. When those two disagree, something is mounted there:

```rust
if read.inode != job.listed_inode && !job.firmlink {
    continue;   // skip: this volume gets scanned on its own terms, if at all
}
```

Firmlinks are the deliberate exception — they are the only route to what they point at, so they
stay followed. `SF_FIRMLINK` (`0x00800000`) in the entry's `ATTR_CMN_FLAGS` identifies them.

The skip is deliberately narrow: it applies only where the mount leads back to the filesystem the
scan started on, which is what makes it a *second* route to files already being counted. Genuinely
separate filesystems are crossed by default, like `du`, and `-x` / `--one-file-system` declines to
cross those too. On this machine the difference is the auxiliary APFS volumes (`VM`, `Preboot`,
`Update`, `xarts`, `iSCPreboot`, `Hardware`), worth 27.7 GiB and about 20,000 entries.

An earlier version of this fix skipped *every* mount point, which also stops the double count but
takes `-x` behaviour away from anyone who wanted the default. Worth noting because it looks
equivalent from the `/` benchmark alone; the difference only shows on a machine with other volumes
mounted, and scanning a directory that contains nothing but mount points then reports nothing at
all.

One known limitation: because the discriminator is "same filesystem as the scan root", scanning
`/System/Volumes` directly will not descend into `Data`, even though nothing else in that scan
reaches it. Scanning `/` or `/System/Volumes/Data` behaves as expected.

### 3. More threads is slower

Counterintuitive and the most portable finding. On a 14-core machine, whole-disk scan time by
worker count:

| threads | time |
| --- | --- |
| 4 | 42.5s |
| 6 | **35.3s** |
| 8 | 36.8s |
| 10 | 41.1s |
| 12 | (worse) |
| 14 (all cores) | (worse) |

The reason shows in the user/system split. At 8 threads a whole-disk scan spends **185s of system
time against 7s of user time**. The work is essentially all in the kernel, and past a handful of
threads the extra workers spend their time contending on filesystem locks rather than reading
anything. Single-threaded, the same scan costs ~5µs of kernel time per entry; at 14 threads it
costs ~28µs per entry.

For comparison, `find ~/project | wc -l` over a 3M-entry tree used ~3 cores and 17.7s of system
time, and finished in 6.3s — less kernel time than our 14-thread run and faster wall-clock.

`thread_count()` therefore caps workers at `min(cores, 8)`.

> **Now `min(cores, 6)` on macOS** — re-measured with a profiler and `iostat` in "macOS,
> re-benchmarked" below, which also says what the kernel is waiting on.

> **The explanation here does not hold on Linux — see "Linux XFS" below.** On a 32-core Linux box
> the same cliff appears on both XFS and ext4, but 16 *independent* walker processes scale to 13.3M
> entries/s against the same tree, so the kernel and the filesystem are not the limit; jwalk's
> thread model is. The cap is still worth keeping; the reason given for it is not the reason.

**This cap is a macOS/APFS measurement and should be re-derived on Linux**, where the contention profile of ext4/xfs/btrfs is different.

### 4. The lean attribute set was not, by itself, the win

Worth recording because it is the finding that contradicts the obvious story.

`dua-core` reproduces the Apple FTS contract: it requests the full `stat` attribute set for every
entry (`ATTR_CMN_CRTIME`, `MODTIME`, `CHGTIME`, `ACCTIME`, `OWNERID`, `GRPID`, `ACCESSMASK`,
`FLAGS`, `FILEID`, plus `ATTR_FILE_LINKCOUNT`, `ALLOCSIZE`, `IOBLOCKSIZE`, `DEVTYPE`,
`DATALENGTH`), and because bulk enumeration does not synthesize `stat` fields for directories, it
follows up with a **path-based `lstat` on every directory**. The replacement asks for a name, an
object type, flags, an inode, and one size field, and never leaves the bulk call.

That sounded like it should be the whole story. It is not. On a single-volume subtree with no
duplication the lean walker is **not faster — in the numbers above it is marginally slower**
(`walk` 8.19s against `dua-walk` 7.13s on the same tree). The saved syscall per directory and the
smaller attribute set are real but small: directories are a minority of entries, and the kernel's
cost per entry is dominated by fetching the inode record at all, which neither walker avoids.

**The whole-disk win is structural: not visiting 10M redundant entries.** The lean walker's value
is that it is ours, so the mount logic could live in it — not that it shaves microseconds. If a
future change makes `dua-core` skip duplicate volumes, most of this file's macOS-specific code
could be deleted and little speed would be lost.

### 5. A parser bug that hid inside a plausible number

Recorded as a cautionary tale, because the wrong version looked *better*.

`ATTR_FILE_*` attributes do not apply to directories, and — unlike invalid attributes under
`FSOPT_PACK_INVAL_ATTRS` — they are **omitted from a directory's record entirely rather than
zero-filled**. The first parser read the size field unconditionally, so on a directory record it
ran past the end of the record. For a directory whose name was ≥8 bytes it silently read name
bytes as a size; for a shorter name the cursor ran out and the entry was dropped — taking that
directory's entire subtree with it.

The resulting scan of `/` reported 7.7M entries and 768 GiB. That is *closer to the truth* than
the 20.4M/1.4 TiB it replaced, and it was tempting to read it as the mount fix working. It was
not: the mount fix was not working at all, and two bugs were partly cancelling.

What caught it was the unrelated-looking 233,390 "unreadable" count against `dua-core`'s 1,028.
The fix is to consult the returned-attributes bitmap before reading the field:

```rust
let size = if returned_file & size_attribute != 0 { cursor.u64()? } else { 0 };
```

Lesson: a total that moves toward the expected value is not evidence the intended change worked.
Check the entry count and the error count too.

### 6. Secondary fixes, worth doing but individually small

- Entries reached the UI thread one per message through a `sync_channel(1)`. Millions of
  round-trips; now batched at 4096 entries.
- `std::fs::Metadata` (~100 bytes) travelled with every entry. Replaced with `EntryMeta` — one
  `u64` and a `bool`.
- The tree insert recursed from the root per entry, rebuilding a `PathBuf` suffix at every level
  (`O(depth²)` allocations per file). Now entries arrive grouped by directory and
  `Folder::add_dir_entries` resolves the parent once per directory, then inserts the whole batch.
- `FileTree::add_entry` recomputed `path_in_filesystem.components().count()` for every entry.

### 7. What the audit caught afterwards

The performance work was reviewed after the fact, and the review found more than the performance
work did. Recorded because the same traps are waiting on Linux.

**Dropping the walk early did not stop it.** `MacosWalk::drop` drained the channel to release
workers blocked on a full send — but draining guarantees every send *succeeds*, so the workers
happily walked the entire remaining tree while the consumer waited to join them. Quitting
diskonaut partway through a scan of `/` took **32.2s**; with a `stop` flag checked in the queue's
`pop()`, it takes **0.1s**. `dua-core` had a stop flag and this walker did not, which is exactly
the kind of thing you lose when replacing a mature component.

The measurement is easy to repeat and worth repeating on any walker: start a scan of something
large, quit a few seconds in, and time how long the process takes to exit.

**The Linux fallback grouping dropped an entry at every directory boundary.** `group_by_directory`
declared its accumulator *inside* the `iter::from_fn` closure, so it was reset on every call
rather than held across them; the freshly started group — which already held the first entry of
the new directory — was discarded on return. The first entry of every directory after the first
was lost, along with the entire final group. On a Linux scan that is missing files and understated
sizes scaling with directory count.

It survived review, compilation and clippy because on macOS it is `cfg`-selected away and never
runs. There are now tests (`scan::tests::fallback_grouping_reports_every_entry_exactly_once` and
friends) that call it directly on every platform, and assert every entry appears exactly once.
**Type-checking dead code is not testing it.**

**The `dua-core` walk yields the scan root itself first**, with the root's *parent* as its parent
path. Grouping that entry produced a batch addressed outside the tree, which
`FileTree::add_dir_entries` folded into the base folder because it located a relative path by
skipping a component count. The result was a phantom empty folder named after the scan root,
inside the scan root. Both halves are fixed: depth-0 entries are skipped, and `add_dir_entries`
now uses `strip_prefix` and ignores anything outside the tree.

**`--max-depth` meant different depths in the two walkers**, so the benchmark was not comparing
like with like under that flag. They now agree exactly:

```
max-depth=2:  dua-walk 804 entries / 40.7 MiB      walk 803 entries / 40.7 MiB
max-depth=3:  dua-walk 10110 entries / 193.4 MiB   walk 10109 entries / 193.4 MiB
```

The remaining single entry is dua's own root entry, which the model ignores either way.

**`--max-depth`, `--threads` and `--single-thread` did nothing outside `--benchmark`** — the app's
scan path hardcoded its options. Now wired through.

**The record parser trusted a layout it had not checked.** It verified `ATTR_CMN_RETURNED_ATTRS`
but then read the error, name, type, flags and inode fields unconditionally. On a filesystem that
does not vend one of them — plausible for SMB, NFS, FUSE or exFAT, none of which could be tested
here — every following field shifts, and a garbage inode makes the mount check fire and silently
discard an entire subtree. The parser now requires the full set and falls back to `readdir` +
`lstat` for that directory when it is not there. On APFS the guard never fires: entry counts and
totals are unchanged.

### 8. Still open

Two audit findings were deliberately left alone, both pre-existing:

Both have since been dealt with: `delete_path` now subtracts the folder itself along with its
contents, and hard links are counted once per folder (see above).

## What a folder's size means, and hard links

A folder's size answers **"how much space is held under here"**: every distinct file beneath it,
counted once. Hard links make that different from "the sum of the entries", because one file can
be reached by several names.

The rule is per folder. A file counts once in any folder that can reach it, and once in any folder
above that — but never twice in the same folder. With `a/a`, `a/b` and `b/a` all links to the same
1 KiB file:

```
root   1 KiB     one file, however many names point at it
├── a  1 KiB     a/a and a/b are the same blocks
└── b  1 KiB     b/a is those same blocks, held here too
```

Deleting `a` on its own frees nothing, and neither does deleting `b`; deleting both frees 1 KiB.
That is a real property of hard links, not an artefact of the accounting.

`du` answers a different question. It deduplicates across the whole run in traversal order, so the
first directory it happens to visit is charged and the rest show nothing:

```
$ du -sk a b .
4    a
0    b        <- not "b holds nothing", just "b was visited second"
4    .
```

Which directory gets the 4 KiB depends on the order of the walk. diskonaut's answer does not: the
tests cover both orderings, and `HardLinks::charge` is deliberately order-independent.

### The gotchas

**Sizes are not additive.** A folder's size can be less than the sum of its children's sizes, and
the file tiles inside a folder can add up to more than the folder they sit in. In the example
above `a` is 1 KiB while the two files inside it each show 1 KiB. Both numbers are correct answers
to different questions — the tile shows how big that file is, the folder shows how much space it
holds — but they will not reconcile by addition wherever hard links are involved.

**Deleting one link frees nothing.** Space comes back only when the last link goes. diskonaut
subtracts the file's full size from its ancestors on delete, so after removing one of several
links the "space freed" figure and the folder sizes are optimistic until the last one is gone. The
subtraction saturates at zero so the tree cannot go negative, and a rescan always restores the
truth.

**Identity is the inode number plus the size.** Inode numbers are unique only within a filesystem,
and a scan of `/` on macOS spans a volume group whose volumes number their inodes independently.
Two entries claiming one inode but different sizes are therefore treated as different files. Two
genuinely different same-size files that collide on an inode number across volumes would be
merged; that needs both to be hard-linked as well, which makes it unlikely rather than impossible.

**Only files with more than one link are tracked.** Everything else takes the plain additive path,
which is what keeps the cost invisible: a whole-disk scan here found about 20,000 distinct
hard-linked files out of 10.4M entries, so the ledger is negligible and the measured scan time did
not move. It did move the total, by about 16 GiB — that much of the disk was being counted twice.

The walk supplies the two fields this needs, `EntryMeta::inode` and `EntryMeta::links`. Anything
that does not fill them in gets the old additive behaviour rather than a wrong answer.

## What is macOS-specific

| Piece | Portable? |
| --- | --- |
| Benchmark harness, stages, flags | yes, already builds and runs everywhere |
| "Measure the walker before the model" | yes |
| "More threads can be slower" | yes as a phenomenon; the number 8 is not |
| Batching, `EntryMeta`, per-directory tree insert | yes, already in shared code |
| `getattrlistbulk` walker (`scan/macos.rs`) | no, `#[cfg(target_os = "macos")]` |
| Firmlink handling | no, macOS has no counterpart elsewhere |
| Inode-vs-listed-inode mount detection | the *technique* ports; on Linux `st_dev` is simpler and sufficient |

Non-macOS builds use `fallback::group_by_directory` in `scanners/src/lib.rs` (then `libdiskonaut/src/scan/mod.rs`), which groups
the `dua-core` walk into per-directory batches so the rest of the pipeline is identical. It is
compiled on every platform (`#[cfg_attr(target_os = "macos", allow(dead_code))]`) and the tests in
`scan/tests.rs` call it directly everywhere, so it is exercised on macOS even though it is never
selected there. It still runs for real only on Linux — **run the test suite first and trust it
less than the numbers.**

## Repeating this on Linux

### Start here

1. `cargo build --release && cargo test` — the fallback path is exercised by tests but has never
   run against a real filesystem at scale. Start there.
2. Establish the baseline and confirm where the time goes:
   ```sh
   /usr/bin/time -v ./target/release/diskonaut --benchmark --bench-stage all /
   ```
   Note the user/system split. If system time dwarfs user time as it does on macOS, the work is in
   the kernel and the data model is not the problem.
3. Sweep the worker count before optimising anything — it may be the largest single lever and
   costs nothing to find:
   ```sh
   for t in 1 2 4 6 8 12 16 24 32; do
     ./target/release/diskonaut --benchmark --bench-stage pipeline --threads $t / | tail -1 |
       sed "s/^/threads=$t /"
   done
   ```
4. Sanity-check the total against `df` and the entry count against `df -i`. A scan that reports
   more entries than the filesystem has inodes is traversing something twice.
5. Time quitting mid-scan. Start a scan of `/`, press `q` then `y` a few seconds in, and check the
   process exits immediately rather than finishing the walk first — see finding 7.

### The structural difference to expect

This is the part that does not carry over, and it is the important one.

`getattrlistbulk(2)` returns **names and sizes together** in one call per directory's worth of entries.
Linux has no such syscall. `getdents64(2)` returns names, inode numbers and a type hint (`d_type`),
but **no size** — so a size still costs a `statx`/`fstatat` per file. The per-entry syscall that
macOS avoids is unavoidable in the portable Linux path.

That reframes the problem. On macOS the question was "what is the walker doing that it needn't
be"; on Linux it will be "how do we make several million `statx` calls cheaply, or avoid them".

Candidates, roughly in order of expected value. The first is portable; the rest trade portability
or privilege for speed, which is how WizTree gets its numbers on Windows (it reads the NTFS MFT
directly rather than asking about files one at a time).

- **Batch the `statx` calls with `io_uring`.** `IORING_OP_STATX` lets thousands of stats be
  submitted with a single syscall, which directly attacks the per-entry syscall cost. Probably the
  best portable-in-practice win; needs a reasonably modern kernel and a dependency such as
  `io-uring` or `tokio-uring`.
- **`statx(AT_STATX_DONT_SYNC)`** with a minimal `mask` (`STATX_TYPE | STATX_BLOCKS`, or
  `STATX_SIZE` for apparent size). Cheap to try, avoids revalidation on network filesystems, and
  asking for less may let some filesystems do less.
- **Trust `d_type` from `getdents64`** to decide what to descend into, so only non-directories need
  a stat, and skip stats entirely for entry kinds that cannot hold data. Note `d_type` is
  `DT_UNKNOWN` on some filesystems, so a fallback is required.
- **Filesystem-specific bulk paths**, worth measuring as an upper bound even if not shipped:
  - XFS has `XFS_IOC_BULKSTAT`, which returns inode stat records in bulk — the nearest thing Linux
    has to reading the metadata table directly.
  - btrfs has `BTRFS_IOC_TREE_SEARCH_V2`, which can read directory and inode items straight out of
    the filesystem trees.
  - ext4 has no supported userspace bulk-metadata API. Raw inode-table reads (the `e2image`
    approach) need root and a quiescent filesystem, and are not appropriate for a live tool.

  All of these need `CAP_SYS_ADMIN` or root, and each covers one filesystem, so they belong behind
  a capability check with the generic path as the fallback — if they are pursued at all. Verify the
  details against current man pages; they are cited here from background knowledge, not tested.

> **Tested since, and mostly wrong — see "Linux XFS" below.** io_uring `statx` is 3.8x *slower*
> than a plain `statx`, not the best win; the minimal mask, `AT_STATX_DONT_SYNC` and `d_type`-based
> stat elision are all worth nothing measurable; `XFS_IOC_BULKSTAT` is indeed `EPERM` unprivileged
> *and* returns no paths, so it cannot replace a walker; and `XFS_IOC_GETFSMAP` is the exception to
> "all of these need root" — it is callable by any user, but redacts every owner, which makes it
> useless for this purpose by a different route.

### Correctness requirements on Linux

The firmlink problem is macOS-only, but duplicated and runaway traversal have Linux analogues, and
they matter just as much as speed:

- **Pseudo-filesystems.** `/proc`, `/sys`, `/dev`, `/run` must not be walked as if they held data.
  `/proc` in particular is effectively unbounded.
- **Mount points.** Unlike macOS, `st_dev` genuinely differs across a Linux mount, so comparing a
  directory's `st_dev` with its parent's is sufficient — the inode comparison used on macOS is not
  needed. `du -x` semantics: do not cross, except for the scan root itself.
- **Bind mounts** make the same subtree reachable at two paths with the same `st_dev`, which the
  device check will not catch. `/proc/self/mountinfo` enumerates them.
- **btrfs subvolumes** report differing `st_dev` values within one filesystem, so a naive
  device check will *under*-count by refusing to descend. Check against `mountinfo`.
- **Hard links** are handled by `HardLinks` in the shared model, keyed on the inode number and
  size reported by the walk, so a Linux walker gets the behaviour for free as long as it fills in
  `EntryMeta::inode` and `EntryMeta::links`. `statx` supplies both (`stx_ino`, `stx_nlink`), and
  `getdents64` alone does not — another reason a size-bearing stat is unavoidable there.
- **Symlinks** are not followed, and should stay that way.

### Where to put the code

`libdiskonaut::scan::scan_directories()` is the seam. It returns `impl Iterator<Item = DirEntries>`
and picks an implementation by `cfg`. A Linux walker slots in beside `macos`, yields the same
`DirEntries { path, entries, failed }`, and everything downstream — batching, tree building, the
UI — is unchanged. Add a `linux-*` benchmark stage next to the `dua-*` ones so the old and new
walkers can be compared on the same tree in one run, which is what made the macOS work tractable.

## Linux ext4 performance & allocator pressure

> **Stale as of 2026-09-22.** The `/data` this section measured was ext4 with ~2.24M entries. The
> path now holds an XFS filesystem with ~4.23M entries on different hardware, so none of the
> numbers below are comparable with the XFS section that follows. The *changes* described here are
> still in the code and still correct; only the measurements are of a filesystem that no longer
> exists at that path.

On Linux (ext4, ~2.24M entries, ~170k hard-linked files on `/data`), `dua-tree` initially outperformed
diskonaut's tree building because of allocator pressure in the model and hard link accounting.

### Results on `/data` (warm cache)

| Stage | Time | Entries | Throughput | Reported Size | Hard Links |
| --- | --- | --- | --- | --- | --- |
| `dua-walk` | 2.23s | 2,236,001 | 1,000,975 entries/s | 288.6 GiB | (none) |
| `dua-tree` | 9.05s | 2,236,001 | 247,168 entries/s | 221.2 GiB | 170,057 |
| `walk` | 2.50s | 2,236,000 | 893,250 entries/s | 288.6 GiB | (none) |
| `tree` | 6.58s | 2,236,000 | 339,893 entries/s | 221.2 GiB | 170,057 |
| `pipeline` | 5.49s | 2,236,000 | 407,173 entries/s | 221.2 GiB | 170,057 |

`pipeline` completes in **5.49s** (~407k entries/s), beating `dua-tree` (9.05s) by ~39% by overlapping
the parallel walk with concurrent tree building across an MPSC channel.

### Key optimizations

1. **Eliminated 2.2M heap allocations in tree construction**: `FileTree::add_dir_entries` and
   `Folder::add_dir_entries` accept `Vec<NamedEntry>` by value. Instead of allocating a cloned `OsString`
   for every file and then dropping the original in the caller, `entry.name` moves directly into
   `Folder.contents`.
2. **Removed redundant `File.name` field**: `File` previously stored an unused `name: OsString` that was already
   the key in `Folder.contents`. Removing it eliminated 2.2M string allocations and shrunk the struct.
3. **Optimized `HardLinks`**:
   - Replaced `Vec<Vec<OsString>>` path storage with `Vec<(PathBuf, usize)>`, cutting allocations down to 1 per path.
   - Introduced a fast non-cryptographic `U64Hasher` (`SplitMix64`) for the 170k-inode map.
   - Added `charge_with_depth` to reuse the caller's directory depth instead of repeatedly parsing path components.
4. **Pointer equality in `group_by_directory`**: Replaced string equality with `Arc::ptr_eq(&open.path, &parent_path)`.
5. **Pre-allocated channel batches**: Sized batch vectors and enlarged sync channel buffer to prevent worker stalls.
6. **Symlink root canonicalization**: Ensured symlinked scan roots evaluate correctly under `dua-core` walker.

### Second pass: the tree build was the bottleneck after all

The first Linux pass left `pipeline` at 5.2s against a 2.4s `walk`: unlike macOS, the single
tree-building thread was taking twice as long as the eight-thread walk it was consuming. With
`perf` unavailable (`perf_event_paranoid=4`), timers around the three phases of
`FileTree::add_relative_dir_entries` attributed the ~4.3s of consumer time on `/data`:

| phase | time | what it is |
| --- | --- | --- |
| path prep | 0.09s | `strip_prefix`, component count |
| hard-link charging | **3.0s** | `HardLinks::charge` for 726k links to 170k inodes |
| tree insert | 1.2s | resolving the parent folder and inserting the entries |

Hard-link charging dominated because `HardLinks` compared every new link against every folder
already holding a link to the same inode, and each comparison parsed both paths component by
component (`Path == Path` does that too, not a byte compare). With 13k inodes linked from 20 or more
folders that is 5.1M path parses of ~15 components each.

Three changes, measured old binary against new on the same tree back to back:

| | before | after |
| --- | --- | --- |
| `tree` | 6.6–6.9s | 3.0–3.1s |
| `pipeline` | 5.0–5.5s | 2.1–2.8s |
| hard-link charging | 3.0s | 0.43s |
| tree insert | 1.2s | 0.75s |
| peak RSS (`pipeline`) | ~870 MB | ~430 MB |

Entries, total size and hard-linked count are identical between the two binaries, and a
randomised test (`model::tests::hard_links::interned_ledger_matches_component_wise_reference`)
checks the new ledger against the old algorithm on 20,000 charges.

1. **Directories are interned in `HardLinks`.** Each directory that holds a hard link is resolved
   once to a `DirRef` (an index into a `parent`/`depth` table), and a link records that id rather
   than a `PathBuf`. "How many leading components do these two folders share" becomes a
   lowest-common-ancestor walk over integers, and "is this the same folder" an integer compare.
   Interning is lazy — a directory with no hard links never touches the ledger — so the map of
   paths to ids holds only the directories that need it.
2. **`FileOrFolder::Folder` is boxed.** A `Folder` is 96 bytes; a `File` is 16. Every one of the
   2.2M files was paying for the larger variant in its map slot (120 bytes with the key), so the
   folder maps were ~2.5x bigger than they needed to be and inserts moved that much more memory.
   The slot is now 48 bytes. This is where the RSS halved, and part of the insert speed-up.
3. **A word-at-a-time hasher replaces `SipHash`** for the folder maps and the inode map
   (`model/files/hash.rs`). Resolving a directory's parent chain costs a lookup per component, so
   with 670k groups several components deep the tree build hashes several million names on top of
   the 2.2M inserts. The hasher is seeded once per process: its step is invertible, and a scan of
   `/` reads directories other users can write to, so an unseeded state would let them choose names
   that all collide in one folder's map.

A review of the change caught that the ledger first interned directories by their raw spelling,
so `a/b` and `a/b/` were two folders where the old component-wise compare saw one; paths are now
normalised through `components()` before lookup, and the randomised test generates the odd
spellings too. The review also pointed out that the ledger's directory table duplicates what the
`Folder` tree already resolves for the same batch. Storing a `DirRef` on each `Folder` would remove
the path-keyed map and its per-directory allocation; left for a later pass, since only directories
holding hard links are interned and it did not register in the timings.

With these, `pipeline` sits at or just above `walk` — the consumer is hidden behind the walk
again and further work on the model will not show in the app until the walker gets faster.

Two things noticed and left alone, for whoever picks the walker up next:

- The `dua-core` grouping yields **~670k directory groups for ~307k directories**: a directory's
  entries arrive in more than one chunk (`ENTRY_CHUNK_SIZE = 4`, several workers), so its parent
  is resolved and its hard-linked entries de-duplicated about twice as often as necessary. Harmless
  for correctness, worth ~0.3s of the remaining consumer time. A Linux walker that emits one group
  per directory would remove it.
- Re-sweeping worker counts on this machine (8 cores, ext4, warm cache): 2 threads 5.1s, 4 threads
  3.0s, 6 through 16 threads all within 2.2–2.9s of each other, with run-to-run noise of ±0.3s.
  The cap of 8 is not wrong here, and there is no better number to replace it with.

## FAT: vfat, msdos and exFAT

### The bug: FAT32 on macOS scanned as entirely empty

`msdosfs` **sets the `ATTR_FILE_ALLOCSIZE` bit in a record's returned-attributes bitmap and then
packs the value as zero.** The bitmap says the attribute is there, so the `returned_file &
size_attribute != 0` check in `parse_record` passes and reads a legitimate-looking `0`. Every file
on a FAT12/16/32 volume therefore had size zero, the treemap was blank, and nothing said so.
`--apparent-size` was the only working mode, since it asks for `ATTR_FILE_DATALENGTH` instead.

Measured on a 200 MB FAT32 image, before the fix:

```
FAT32   dua-walk  1683 entries  19.1 MiB     exFAT   walk  44.0 KiB   correct
        walk      1682 entries   0.0 B       APFS    walk  correct
        pipeline  1682 entries   0.0 B
```

A probe requesting all three file attributes at once shows it directly:

```
FAT32 : returned_file=0x205  linkcount=1  allocsize=0      datalength=5120
exFAT : returned_file=0x205  linkcount=1  allocsize=8192   datalength=5120
APFS  : returned_file=0x205  linkcount=1  allocsize=4096   datalength=353
```

The `REQUIRED_COMMON` guard added in finding 5 does not catch this: it covers *common* attributes
only, and only checks the first record of the first batch. The comment there guessed exFAT as the
filesystem at risk; exFAT is in fact fine, and FAT32 is the one that broke. **A filesystem can
misreport an attribute it claims to return — the bitmap is not a guarantee of the value.**

There is no allocated size to recover on such a volume. `msdosfs` reports `f_bsize` as 512 rather
than the cluster size, and `st_blocks` as `ceil(size / 512)`, so neither `statfs` nor `lstat` knows
the real allocation either — rounding the data length up to the cluster size is not available as a
fix. The data length is the closest honest answer.

`SizeAttribute` in `scan/macos.rs` now picks the attribute per filesystem, identified by one
`fstatfs` per *device* (cached, not per directory: a scan can span a FAT stick and an APFS disk, so
a single answer for the whole walk would be wrong, but probing every directory would tax every
filesystem to catch a rare one).

### Testing it

`scan::macos::tests::a_fat32_volume_does_not_scan_as_empty` creates a FAT32 image with `hdiutil`,
mounts it, scans it and asserts a non-zero total. It is `#[ignore]`d because it mounts a disk image:

```
cargo test -p libdiskonaut --lib -- --ignored fat32
```

Nothing synthetic reproduces this. A hand-built record either carries the attribute or does not,
and neither case is the one that broke; only the real driver claims an attribute and then zeroes
it. Confirmed to fail (`FAT32 volume scanned as 0 bytes`) with the fix reverted.

The test reads its mount point back from `hdiutil attach` output rather than deriving it from the
volume name: **a FAT label longer than eleven characters is silently replaced with `NO NAME`**, so
a name-derived path is wrong for long labels.

### For Linux — unverified, to check when someone has a Linux box

None of the following was tested; it is reasoning from the drivers, recorded so it can be confirmed
or knocked down rather than rediscovered. Test with a FAT stick and a loopback `mkfs.vfat` image.

- **Sizes should already be right, and should differ from macOS.** Linux's `fat_fill_inode` sets
  `i_blocks` from the size rounded up to the cluster size, so `st_blocks` is a true allocated size
  there, unlike macOS. The consequence: **the same stick totals differently on Linux and macOS**,
  and dramatically so on a 32 KB-cluster FAT32 full of small files. Confirm this rather than
  letting someone chase it as a bug.
- **Hard-link accounting is inert, correctly.** FAT has no hard links; `nlink` is always 1, and
  Linux `vfat` reports 1 for directories too. `HardLinks` never fires. No cost, no risk.
- **Synthetic inodes are harmless only by accident.** `vfat` derives `st_ino` from directory-entry
  position and they are not stable across remounts. This is safe today *only* because `links > 1`
  is never true, so the inode never reaches the dedup map. Anything that later keys a map on inode
  unconditionally will break here first.
- **The thread count is the open performance question.** `MAX_SCAN_THREADS = 8` is tuned for APFS
  on NVMe. FAT serialises FAT-chain traversal and usually lives on slow removable media, so 1–2
  workers may well beat 8. This was **not** measured: a disk image backed by NVMe does not model a
  real stick's seek cost, and the test tree scans in 13 ms either way. Needs a real USB stick and a
  `--threads 1/2/4/8` sweep before the constant is touched.
- **exFAT on Linux** is a separate driver (`exfat`, not `vfat`) and, like macOS's, is expected to
  be fine. Worth one confirming run, not more.

## Linux XFS: where the time actually goes (2026-09-22)

The exercise the "Repeating this on Linux" section set up — repeat the macOS walker work on
Linux, and look for an XFS bulk-metadata path — run against a real XFS volume. The short version
is that **the XFS-specific ideas are all dead ends without root, and the scan's remaining cost is
the walker's thread model**, which is not an XFS matter at all. The tree build is 0.85s of a 2.4s
scan, and the walk is the rest. Both halves are measured below.

It also turned up a correctness bug that has nothing to do with speed: XFS reflink sharing is
present on this volume and the model over-counts it (section 6).

### Test machine

| | |
| --- | --- |
| Hardware | 32 cores, 38 GiB RAM |
| OS | Linux 6.8.0-124-generic (Ubuntu 24.04) |
| Filesystem | XFS on `/dev/bcache0`, 8 TiB volume, 915 GiB used |
| `xfs_info` | `agcount=8`, `crc=1`, `finobt=1`, `sparse=1`, `rmapbt=1`, `reflink=1`, `bigtime=1`, `ftype=1`, `inode64`, `bsize=4096` |
| Nested mount | `/data/home/angch/project/myalamat-db` — a subtree mount of a second XFS (`/dev/sdd`) |
| Privilege | ordinary user, uid 1000, no `sudo` |

The tree: **4,228,429 entries**, 12 unreadable, 21,791 distinct hard-linked files, 806.8 GiB
reported (745.9 GiB with `-x`). `df -i` reports 4,217,342 inodes in use, and the ~11k excess is
the extra names of the hard-linked files — so nothing is being traversed twice. The 61 GiB
difference `-x` makes is the nested mount, which holds two multi-gigabyte tarballs and five other
entries; that is why excluding it moves the total by 61 GiB while moving the entry count by 5.

Everything below is warm-cache. "Warm" is not a guess here: `/proc/diskstats` shows **zero sectors
read from `bcache0` during a full scan**, so the whole 4.2M-inode working set is resident and every
number is CPU and kernel time, not I/O. A cold scan of this volume was not measured and would be a
different problem.

### Baseline

Default settings (8 threads), two runs of every stage:

```
dua-walk      4.999s / 4.202s    4228416 entries    896.0 GiB
dua-tree     10.388s /10.814s    4228417 entries    806.8 GiB   21791 hard-linked
walk          3.028s / 3.024s    4228424 entries    896.0 GiB
tree          7.026s / 7.929s    4228424 entries    806.8 GiB   21791 hard-linked
pipeline      2.932s / 3.116s    4228424 entries    806.8 GiB   21791 hard-linked
```

`/usr/bin/time -v` on `pipeline`: **5.32s user against 9.52s system**, 745 MB peak RSS, 483,793
voluntary context switches. System time dominates, as on macOS, but only by 1.8x rather than 26x.

### 1. Thread count: 6 is better than 8, and past 8 it falls off a cliff

`pipeline`, `/data`, three runs each:

| threads | 1 | 2 | 4 | 5 | 6 | 7 | 8 | 10 | 12 | 16 | 24 | 32 | 48 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| time | 11.40s | 5.25s | 2.92s | 2.50s | 2.48s | **2.26s** | 2.53s | 4.18s | 5.75s | 6.53s | 6.85s | 6.81s | 6.96s |

`MAX_SCAN_THREADS = 8` lands on the shoulder of the cliff rather than at the optimum, and 10
threads is already 85% slower than 7. On a 32-core machine the default (`min(cores, 8)`) is
therefore doing real work — without the cap this scan would run at 6.8s instead of 2.5s.

5 through 8 are within noise of each other (2.26–2.65s across runs), so this does not justify
retuning the constant to a precise value. It does justify not raising it.

### 2. The cliff is not XFS, and it is not the kernel — it is the walker's thread model

This is the finding that matters, and it contradicts finding #3 above.

**Control on another filesystem.** The same sweep on `/` (ext4 on `/dev/sda2`, 303,805 entries,
`-x`), `walk` stage:

| threads | 2 | 4 | 6 | 8 | 12 | 16 | 24 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| time | 0.403s | 0.230s | **0.167s** | 0.191s | 0.389s | 0.453s | 0.474s |

Identical shape: optimum at 6, cliff after 8, ~2.7x worse by 16 threads. Whatever this is, it is
not an XFS property.

**Does the kernel scale?** Run N *independent* single-threaded walkers over the same tree at once
(`docs/probes/statbench.c`, mode 1) and measure aggregate throughput:

| concurrent processes | 1 | 4 | 8 | 16 |
| --- | --- | --- | --- | --- |
| wall clock | 4.38s | 4.67s | 4.84s | 5.09s |
| aggregate | 965k/s | 3.62M/s | 6.99M/s | **13.3M entries/s** |

Sixteen processes hammering the *same* inodes get 13.8x the throughput of one, for a 16%
wall-clock penalty. XFS, the dcache and the VFS scale essentially linearly here. The premise of
finding #3 — "past a handful of workers the extra workers spend their time contending on
filesystem locks" — is simply not true on this machine.

**So it is the walker.** `docs/probes/mtwalk.c` is a deliberately naive in-process parallel walker:
one global mutex, a LIFO queue of directory fds, N pthreads, `getdents64` + `fstatat` per entry, no
work stealing. 90 lines. It reports the same 4,228,423 entries, the same 896.0 GiB and the same 12
failures as diskonaut.

**Read the table for its shape, not its multiple.** `mtwalk` is doing a strictly smaller job than
the Rust walker: it never allocates a name (it passes `d_name` straight to `fstatat` and keeps
subdirectory *file descriptors*, not paths), it builds no `NamedEntry`, no `Vec`, no `Arc<Path>`
group, and it hands nothing downstream. This document's own ext4 section records that removing 2.2M
`OsString` allocations was worth measuring, so 4.2M of them are not free. It also holds an open fd
for every discovered-but-unvisited directory — it needs `ulimit -n 65536` — which a shipped walker
cannot do; opening lazily costs an extra `openat` per descent. So 0.387s is a floor no real walker
will reach.

| threads | 1 | 4 | 6 | 8 | 12 | 16 | 24 | 32 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| mtwalk | 4.408s | 1.185s | 0.831s | 0.633s | 0.510s | 0.465s | **0.387s** | 0.395s |
| diskonaut `walk` | — | 2.896s | 2.216s | 2.558s | 6.340s | 6.647s | — | — |

What the table does establish is the **scaling shape**, and allocation cannot explain a *collapse*:
`mtwalk` improves monotonically to 24 threads while `dua-core` peaks at 6 and is 3x worse by 16.
Together with the 16-process result above — the kernel delivering 13.3M entries/s — that is enough
to say the limiter is jwalk's thread model, not XFS, not the kernel, and not the syscalls.

A real replacement therefore lands somewhere between 0.39s and 2.2s at its best thread count, and
where in that range is unmeasured. It does not need to be near the bottom of it: section 5 measures
the tree build at 0.85s, so anything under ~0.9s already makes the model the binding constraint.

On Linux every benchmark stage goes through `dua-core` — `macos` is macOS-only, and
`fallback::group_by_directory` groups the same `dua-core` walk — so `dua-walk` and `walk` measure
the same underlying jwalk traversal. The collapse past 8 threads lives in there, not in XFS.

### 3. The per-entry stat is 78% of the walk, and nothing portable makes it cheaper

`docs/probes/statbench.c` — one process, one thread, `getdents64` recursion, varying only what it
asks per entry. Two passes over `/data`:

| strategy | time | ns/entry |
| --- | --- | --- |
| `getdents64` only (names + `d_type`) | 0.95s / 0.93s | 220 |
| `fstatat(AT_SYMLINK_NOFOLLOW)` | 4.30s / 4.50s | 1017–1063 |
| `statx`, minimal mask (`TYPE\|MODE\|INO\|NLINK\|BLOCKS`) | 4.47s / 4.46s | 1055 |
| `statx` + `AT_STATX_DONT_SYNC` | 4.56s / 4.28s | 1012–1079 |
| `fstatat`, skipping the 385k directories | 4.55s / 4.16s | 984–1075 |
| `io_uring` batched `statx`, queue depth 512 | 16.91s | 3998 |
| `io_uring` batched `statx` + `DONT_SYNC` | 16.71s | 3952 |

GNU `find` as an outside check: `find /data -printf '.'` 1.67s, `find /data -printf '%s.'` 5.23s.

Three of the four candidates the previous section proposed are now measured, and all three are
worth nothing:

- **A minimal `statx` mask does nothing.** XFS populates the whole in-core inode either way; asking
  for less does not let it do less.
- **`AT_STATX_DONT_SYNC` does nothing.** There is nothing to revalidate on a local filesystem.
- **Trusting `d_type` to skip stats does nothing measurable.** It removes 9% of the calls (385k of
  4.23M) and the result is inside run-to-run noise. It is also not applicable as stated: diskonaut
  counts a directory's own blocks, so it needs the directory's size too.
- **`io_uring` batched `statx` is 3.8x *slower*.** This was ranked "probably the best
  portable-in-practice win" and it is the worst option measured. `IORING_OP_STATX` is a blocking
  opcode: the ring hands each request to an `io-wq` kernel worker, and for a warm-cache metadata
  lookup that handoff costs several times the syscall it replaces. io_uring wins on operations that
  actually block; a cached `statx` does not block.

`ftype=1` is set on this volume, so `d_type` is always populated — `getdents64` returned
**zero** `DT_UNKNOWN` entries across 4.23M.

The honest reading: at ~1µs per entry, warm, the stat is real VFS and XFS work, not syscall entry
overhead. Batching the syscall cannot help because the syscall is not the cost. What *does* help is
doing those microseconds on more cores at once — which is finding 2, not an XFS question.

### 4. The XFS bulk-metadata ioctls, tested

Both were probed directly rather than taken from the man pages.

**`XFS_IOC_BULKSTAT` — `EPERM`.** `docs/probes/bulkstat_probe.c` opens `/data` and issues the v5
ioctl as uid 1000:

```
XFS_IOC_FSGEOMETRY: ok
XFS_IOC_BULKSTAT as uid 1000: FAILED errno=1 (Operation not permitted)
```

`FSGEOMETRY` succeeding on the same descriptor rules out the fd or the struct layout being at
fault. Bulkstat is `CAP_SYS_ADMIN`-gated on 6.8, as the previous section guessed.

Worth stating plainly, because the previous section calls bulkstat "the nearest thing Linux has to
reading the metadata table directly" without the caveat: **bulkstat returns inode records, not
paths.** No name, no parent. It cannot replace a walker for a treemap — the only shape that works
is `getdents64` for the namespace (names, `d_ino`, `d_type`) plus bulkstat as a bulk inode→size
oracle joined on inode number. That is a much larger change than dropping a walker into
`scan_directories()`, it needs a separate ioctl per filesystem (so the nested `myalamat-db` mount
needs its own), and per section 3 its entire ceiling is the ~3.4s of stat time.

**`XFS_IOC_GETFSMAP` — succeeds, and is useless.** This one is the surprise, in both directions.
It is *not* `CAP_SYS_ADMIN`-gated: as uid 1000 it walked the whole 8 TiB device in **25 ioctl calls,
99,822 records, 0.002 seconds**. The reverse-mapping btree (`rmapbt=1` here) really does answer
"what owns this block" at memory speed.

But every record comes back with `FMR_OF_SPECIAL_OWNER` set and an owner of `FMR_OWN_UNKNOWN` (2)
or `FMR_OWN_FREE` (1). Not one inode number in 99,822 records:

```
dev=64256 phys=0         len=634880   owner=2 flags=0x10
dev=64256 phys=634880    len=4096     owner=1 flags=0x10
dev=64256 phys=638976    len=38641664 owner=2 flags=0x10
...
LAST flag seen
batches=25 records=99822  last physical end=8796092989440 (8192.0 GiB)
```

The record count corroborates this: **99,822 records for a volume holding 4.2M inodes.** If owners
were real, adjacent extents belonging to different files could not be merged and the count would be
in the millions; collapsing every owner to `OWN_UNKNOWN` is exactly what lets them coalesce. The
mechanism is therefore inferred from the ioctl's behaviour, not read from the kernel source, but
both the owner values and the record count point the same way: ownership is redacted for
unprivileged callers, presumably so that any user cannot map out the sizes and layout of every
other user's files. An unprivileged caller learns which blocks are free and which are not, and
nothing about whose they are. As a size oracle it is worthless; as a "how
fragmented / how full is this volume" answer it is instant.

Under root it should return real inode numbers, which would make the `getdents64` + fsmap join in
section 4 viable and is the one number this exercise could not obtain. `docs/probes/fsmap_scan.c`
already accumulates blocks per owning inode and prints the distinct-inode count; running it as root
is the remaining measurement.

**ext4** has no supported userspace bulk-metadata API, as previously noted, so a bulk path would
cover XFS only — behind a capability check, with the generic walker as the fallback, for a ceiling
that section 3 shows is smaller than the walker win.

### 5. What to do

In order of measured value. **Items 1 and 2 were done on the same day — see "The native Linux
walker" below for what they cost and what they bought.**

1. ~~**Fix the reflink over-count**~~ (section 6). It is a wrong number, not a slow one. *Done.*
2. ~~**Replace the `dua-core`/jwalk Linux walker**~~ *Done.* with a directory-parallel walker built on
   `getdents64` + `fstatat`, feeding the existing `DirEntries` seam. Unlike every other candidate
   here it needs no privilege, no new dependency and no filesystem-specific code — it would help
   ext4 and btrfs too. It also removes the "~670k groups for ~307k directories" waste noted earlier,
   since a walker that owns its own enumeration emits one group per directory.

   The size of the prize is now measured rather than guessed. The new `tree-only` stage collects
   every directory first, untimed, then times only the model:

   ```
   tree-only     0.822s / 0.853s / 0.865s   4228430 entries   806.8 GiB   21791 hard-linked
   ```

   The tree build is **0.85s** — 4.97M entries/s, and identical totals to every other stage, so it
   is doing the whole job. (Superseded twice over: fed by the native walker's one group per
   directory instead of `dua-core`'s ~2.2 groups, the same stage reads 0.37–0.51s, and the last
   section of this document shows that figure itself is ~1.5x too low — the model really costs
   about 0.7s. Take the lower
   figure as the model's cost and this one as the model's cost *plus* the grouping waste. The stage
   is also distorted — see the following section's known gaps.) Against `pipeline` at 2.37–2.52s and `walk` at 2.22–2.32s, the split is
   roughly 2.2s of walk hiding 0.85s of model. A walker landing anywhere under ~0.9s makes the tree
   build the binding constraint, so the realistic end-to-end target is **~2.4s → ~1.0s, about
   2.5x** — not the 5.6x the walk stage alone suggests.
3. **Leave `MAX_SCAN_THREADS` at 8.** It is on the shoulder rather than the peak, but 5–8 are within
   noise and the cap is what keeps a 32-core machine off the 6.8s cliff. Revisit only after the
   walker is replaced, since the cliff is the walker's and a new walker will have a different curve
   — `mtwalk` was still improving at 24 threads.
4. **Do not spend further effort on the model.** At 0.85s for 4.2M entries it is no longer the
   bottleneck and will not become one until the walker is roughly 2.5x faster.
5. **Do not pursue io_uring, `statx` masks, `DONT_SYNC`, or `d_type`-based stat elision.** All four
   are measured at zero or negative value above.
6. **Do not pursue bulkstat or GETFSMAP** for the scan. Both are root-only in the form that would
   help, both are XFS-only, and both are capped by a stat cost smaller than the walker win.

### 6. Reflink sharing is real here, and the model over-counts it

`reflink=1` is enabled on this volume, and unlike `reflink=1` on an idle filesystem, it is **in
use**. Checking 200 files over 50 MB with `filefrag -v` (unprivileged, no root needed — the claim
that this needed a root run was wrong):

```
  block size 4096
  shared 18.20 GiB of 61.69 GiB across the sample (29.5%)
```

The sharers are `uv`'s package cache — `~/.cache/uv/archive-v0/**` holds reflinked copies of large
CUDA, cuDNN, Torch and Playwright binaries, and `uv` reflinks from there into each project's
virtualenv:

```
4    shared  /data/home/angch/.cache/uv/archive-v0/isUXovQuEaGC7fkl5QdqH/nvidia/cu13/lib/libcusolver.so.12
3    shared  /data/home/angch/.cache/uv/archive-v0/X0ppwh-44oKKYjNcdAZiz/nvidia/nccl/lib/libnccl.so.2
1    shared  /data/home/angch/.cache/uv/archive-v0/svekiP7w12JJm5lx4U0Fv/torch/lib/libtorch_cuda.so
```

Copy-on-write shared extents are allocated once but **each sharing file reports the full
`st_blocks`, and `nlink` stays 1** — so `HardLinks`, which keys on inode and link count, cannot see
them. Every reflinked copy inside a scan is charged in full. `cp --reflink`, `uv`, container image
stores and snapshot tooling all produce this, on btrfs as well as XFS, so it is not an exotic case.

That sample is not random — it is the first 200 large files `find` returned, heavily weighted to
the uv cache — so **29.5% is not a whole-volume figure** and should not be extrapolated. What it
does establish is that the effect is present and large where it occurs, which is enough to call the
number wrong.

It does not show up in the whole-volume total: the scan reports 745.9 GiB against `df`'s 915.3 GiB
for this device, so it is *under* `df` overall. Eleven unreadable directories (two container
Postgres/MySQL `pgdata` trees, `drwx------` under other uids) sit inside that gap and more than
offset the over-count. The user-visible damage is local — point diskonaut at `~/.cache/uv` or a
virtualenv and the answer is inflated, and "delete this to free 15 GB" is not true.

Unprivileged `GETFSMAP` reported zero `FMR_OF_SHARED` records, which given the redaction in section
4 is now confirmed to be an artefact of the redaction rather than evidence of absence — a useful
check on that inference.

The fix has the same shape as `HardLinks`: dedupe on physical extent rather than on inode, which
means FIEMAP (`FS_IOC_FIEMAP`) per file and charging each shared extent to a folder once. That is a
per-file ioctl on top of the per-file stat, so it would want to be opt-in, or restricted to files
whose `st_blocks` suggests sharing is plausible. Scoping it is a separate exercise; recording it
here because by this document's own standard (finding #5) a wrong number outranks a slow one.

### Reproducing

```sh
cargo build --release
./target/release/diskonaut --benchmark --bench-stage all --bench-repeat 2 /data
./target/release/diskonaut --benchmark --bench-stage tree-only --bench-repeat 3 /data
for t in 1 2 4 6 8 12 16 24 32; do
  ./target/release/diskonaut --benchmark --bench-stage pipeline --threads $t /data | tail -1 |
    sed "s/^/threads=$t /"
done

cd docs/probes
gcc -O2 -o statbench statbench.c            # modes 0..6, see the table in section 3
gcc -O2 -pthread -o mtwalk mtwalk.c         # ./mtwalk /data <threads>
gcc -O2 -o bulkstat_probe bulkstat_probe.c  # XFS_IOC_BULKSTAT permission check
gcc -O2 -o fsmap_scan fsmap_scan.c          # XFS_IOC_GETFSMAP per-inode block totals
gcc -O2 -o fsmap_dump fsmap_dump.c          # raw GETFSMAP records, for the redaction check
```

The reflink check in section 6 needs no root and no probe:

```sh
find /data/home/angch -xdev -type f -size +50M | head -200 |
  while read -r f; do filefrag -v "$f"; done |
  gawk 'match($0, /blocks of ([0-9]+) bytes/, m) { bs=m[1]+0; next }
        match($0, /^[ \t]*[0-9]+:[ \t]*[0-9]+\.\.[ \t]*[0-9]+:[ \t]*[0-9]+\.\.[ \t]*[0-9]+:[ \t]*([0-9]+):/, m) {
          tot += m[1]; if ($0 ~ /shared/) sh += m[1] }
        END { printf "shared %.2f GiB of %.2f GiB\n", sh*bs/2^30, tot*bs/2^30 }'
```

`statbench` and `mtwalk` are Linux-only and are not part of the build; they exist so the numbers
above can be re-derived rather than believed.

## The native Linux walker, and reflink accounting (2026-09-22)

Acting on the two findings above: `dua-core` is no longer used on Linux, and copy-on-write shared
extents are now counted once. Same machine, same volume, same `--benchmark` harness as the section
above.

### Results

Whole of `/data`, 4.23M entries, warm, at each walker's own best thread count:

| | before | after | |
| --- | --- | --- | --- |
| `walk` (traversal alone) | 2.32s | **0.49s** | 4.7x |
| `pipeline` (what the app waits for), default settings | 2.93s | **0.72s** | 4.1x |
| `pipeline`, best thread count either way | 2.26s | 0.69s | 3.3x |
| quitting mid-scan | — | **1.3ms** | against a 420ms full scan |
| reported total | 807.2 GiB | **785.3 GiB** | 21.9 GiB was counted twice |

The entry count, unreadable count and hard-linked count are identical between the two walkers on
every run, and `--max-depth` agrees exactly at every depth (the one-entry difference is `dua-core`
reporting the scan root itself, which the model ignores either way).

A caution when reading `--bench-stage all` now: every stage uses `thread_count()`, so the `dua-*`
stages run at the new default of 24 workers, which is far past where `dua-core` collapses. They
report 7s rather than the 2.3s they manage at six. **Compare `dua-walk --threads 6` against the
native walker, not the two lines of one `all` run.**

### What the walker does

`scanners/src/linux.rs` (then `libdiskonaut/src/scan/linux.rs`). `getdents64` for names, `statx` for sizes, which is the same pair
of syscalls `dua-core` ends up making — section 3 above measured that no portable change to *what*
is asked per entry is worth anything. The whole difference is the thread model:

- **One shared queue of directories, N workers, and a local stack per worker.** A worker keeps the
  subdirectories it discovers to itself and publishes half of them only when the shared queue looks
  thin enough that someone might be about to go idle. Most directories are therefore claimed with
  no lock at all; the shared lock is taken a few thousand times rather than 385,000.
- **Batched handoff.** Directories go to the consumer in batches of ~4096 entries rather than one
  message each, which takes the channel out of the profile.
- **One `DirEntries` per directory.** The `dua-core` grouping emitted roughly two groups per
  directory (~670k for ~307k), so every parent was resolved and every hard link de-duplicated about
  twice. This is most of why the model got faster without being touched.
- **A stop flag, checked in the worker loop.** Dropping the walk part-way sets it and drains the
  channel. Both halves are needed, and this is the trap finding #7 above records: draining alone
  makes every send *succeed*, so the workers cheerfully finish the whole filesystem while the
  consumer waits to join them. Measured: dropping after 50 directories returns in **1.3ms**, against
  419ms for the full walk.

For reference, `docs/probes/mtwalk.c` — the throwaway C walker used above to prove the ceiling —
does the same traversal in 0.387s while allocating no names and emitting nothing. The real walker
lands at 0.49s while allocating 4.2M `OsString`s, building per-directory `Vec`s and `Arc<Path>`s,
and shipping it all to another thread. That is about 80% of a walker that does none of the work,
which is a reasonable place to stop.

### The thread cap moved from 8 to 24

The old cap was a `dua-core` property, not a kernel one. With the native walker the curve is flat
rather than cliffed, on both filesystems (`pipeline`, three runs each, best of):

| threads | 8 | 12 | 16 | 20 | 24 | 32 |
| --- | --- | --- | --- | --- | --- | --- |
| XFS, `/data` | 0.727s | 0.780s | 0.706s | 0.722s | **0.664s** | 0.665s |
| ext4, `/` `-x` | 0.068s | 0.060s | 0.058s | — | **0.054s** | 0.065s |

`MAX_SCAN_THREADS` is now 24 on Linux and stays 8 elsewhere — the macOS number is a real
`getattrlistbulk`/APFS contention measurement and does not transfer. Being wrong about this in
either direction is cheap now: everything from 12 to 32 is within about 10%.

### Reflink accounting

Section 6 above found that `uv`'s package cache reflinks large binaries, that XFS reports the full
`st_blocks` for every copy, and that `nlink` stays 1 so `HardLinks` could not see it.

The fix reuses the hard-link rule rather than inventing a second one. `EntryMeta` gained
`shared_extent`, and `HardLinks` gained a second ledger keyed on physical extent instead of inode —
kept separate because an inode number and a block offset are unrelated numbers that would otherwise
collide in one map. A file's identity is its first shared extent if it has one, else its inode if
it is hard-linked, else nothing; extent identity wins because every hard link to a file reports the
same first extent, so one file still gets one ledger entry.

The result is the same semantics hard links already had: the same blocks count once in any folder
that reaches them, and once in every folder above. A controlled pair of 200 MiB reflinks:

```
both together   200.0 MiB      (du -s says 400M)
a/ alone        200.0 MiB
b/ alone        200.0 MiB
```

**Finding it costs an `openat` and a `FS_IOC_FIEMAP` per file**, because there is no bulk answer
available unprivileged — `GETFSMAP` redacts owners and `BULKSTAT` is refused. Two guards keep that
affordable:

- **Only on filesystems that can share extents at all**, decided once by `statfs` magic (XFS,
  btrfs). On ext4 the probe never runs, and the ext4 scan time is unchanged at 0.058s.
- **Only on regular files of at least 64 KiB of allocated blocks**, which is 3.8% of the files on
  this volume (144k of 3.8M). Reflinks of small files exist and are missed; they cost the same two
  syscalls to find and are worth a rounding error.

Measured cost on the full scan: about **0.1s of 0.72s**. What it buys:

| | reported before | after | |
| --- | --- | --- | --- |
| `/data` | 807.2 GiB | 785.3 GiB | 10,804 distinct reflinked files |
| `~/.cache/uv` | 15.1 GiB | 12.0 GiB | 4,784 reflinked; `du -sh` still says 16G |

The identity is the file's **whole extent map**, folded into one number, and only when every
extent is shared. The first extent alone is not enough, and the first version of this got it
backwards — see the review finding below.

APFS clones are the same phenomenon and are *not* handled: `getattrlistbulk` does not report
sharing and macOS has no cheap per-file equivalent of FIEMAP. `scan/macos.rs` sets `shared_extent: 0`
and says so.

### Testing

The `dua-core` grouping is still compiled and still tested — it is what platforms other than macOS
and Linux use — but it is no longer what Linux runs, so the new path needed its own tests
(`scan::tests::linux_walker`): every entry exactly once at 1, 2 and 8 threads; one group per
directory; apparent size; `--max-depth`; symlinks not followed; and dropping the walk early without
hanging.

`scan::tests::reflink::a_reflinked_copy_is_counted_once` is the end-to-end one. It calls `FICLONE`
directly and **skips itself when the filesystem cannot clone** — which `std::env::temp_dir()`
usually cannot, since `/tmp` is typically ext4. Point it at a real one to actually run it:

```sh
DISKONAUT_TEST_REFLINK_DIR=/data cargo test --workspace reflink -- --nocapture
```

Without that variable it prints `skipped: /tmp cannot reflink` and passes, which is worth knowing
before trusting a green run — this is the same trap as "type-checking dead code is not testing it"
in finding #7.

### Filesystem-aware recursion, and the bug it caught

Scanning `/` without `-x` was the check that found the worst defect in the new walker, and it was
not the one being looked for.

**A persistent `getdents64` error was an infinite loop.** The read loop counted a failed directory
read and asked again:

```rust
let Ok(entry) = entry else { failed += 1; continue; };   // wrong
```

A `getdents64` error is a property of the descriptor, not of one entry, so it is still there on the
next call. `/proc/<pid>/net` for a process that has since become a zombie returns `EINVAL` *every
time*, and the walker spun on it forever — one worker pinned at 100% while the other 23 slept on
the condvar. `dua-core` finished `/proc` in ~1.0s on all five attempts; the new walker hung on all
five. Found by `strace`, which showed the same call repeating at 65µs intervals:

```
getdents64(3, 0x7e93093eeb90, 65536) = -1 EINVAL (Invalid argument)   × forever
```

It now `break`s, which is what `std`'s `ReadDir` does. Worth recording as the general lesson:
**`continue` on an error is only safe when the error belongs to the item, not to the iterator.**

**Pseudo-filesystems are no longer crossed into.** `/proc` and `/sys` are kernel interfaces wearing
a directory shape: walking them costs about a million `statx` calls to total zero bytes, `/proc`
grows a subtree per process and per thread while the scan runs, and — as above — parts of it fail
permanently when the process they describe dies mid-walk. This was listed under "Correctness
requirements on Linux" above and had never been done.

The check is by `statfs` magic (`filesystem::is_pseudo`), and is deliberately narrow in two ways:

- **Only at mount points.** A directory whose `st_dev` differs from its parent's is a mount; the
  device is already in the `statx` the walk makes anyway, so the `statfs` costs one call per mount
  crossed rather than one per directory.
- **Never to the scan root.** `diskonaut /proc` still walks `/proc`, because that was asked for.
  The skip only applies to wandering into one part-way through a scan of something else.

`tmpfs` is deliberately *not* on the list: `/tmp` and `/dev/shm` hold real files that really occupy
memory, and `du` counts them. `devtmpfs` reports the same magic, so `/dev` is walked too; it is
small and bounded, unlike the rest.

Measured:

| | before | after |
| --- | --- | --- |
| `diskonaut /proc` | hung, 5 runs of 5 | **0.24s**, 894k entries, ~6,840 unreadable |
| `--bench-stage pipeline /` (no `-x`) | did not finish in 600s | **1.98s**, 9.6M entries |

Scanning `/proc` by name is now about four times faster than `dua-core` managed, which is a side
effect of the thread model rather than the point.

### This change made a pre-existing wrong number reachable

Read this as a caveat on the work above, not as a footnote. `/` completing in two seconds makes it
something a user might actually do, and it reports **1.8 TiB on a machine holding about 1 TiB**.
Before this change a scan of `/` never finished, so nobody ever saw the wrong number. Making a
broken path fast enough to reach is a real cost of the speed work, even though the bug underneath
is older than it. The cause is visible in `df`: `/dev/bcache0` is mounted at both
`/data` and `/home`, so a walk of `/` traverses that filesystem twice and counts every file on it
twice — 9.6M entries against 4.2M inodes.

This is the Linux form of the macOS firmlink problem in finding #2, and it is listed under
"Correctness requirements on Linux" above as the bind-mount case. It is pre-existing — `dua-core`
double-counted identically, on the runs that finished — and it is not fixed here.

The narrow fix available without new machinery would be to refuse a mount leading to a device the
walk has already entered by another path, which is the rule the macOS walker uses. It was
considered and rejected: with work-stealing workers, *which* of `/data` and `/home` wins the race
would vary between runs, so the treemap would move around at random. Doing it properly means
reading `/proc/self/mountinfo` once at the start and picking a canonical path per device, which
also covers the same-`st_dev` bind-mount case the device check cannot see at all. That is the way
in, and it is a separate piece of work.

Until then, **`-x` is the flag that gives a trustworthy whole-machine number.**

### What the audit of this pass caught

Reviewed after the fact, as the macOS work was (finding #7). Two defects, both in the new walker,
both invisible to the benchmark that motivated it.

**Empty directories never flushed the outbox.** Workers batch directories to the consumer until
4096 *entries* have accumulated. A directory with no entries added nothing, so a worker walking a
wide tree of empty directories would hold every one of them until it ran out of work entirely,
while the consumer sat idle waiting for a batch that could not fill. It never deadlocked — the
final flush always happens — but it converts the pipeline back into two serial phases and holds the
whole run's results in memory, in exactly the tree shape where that is worst. Counting
`entries.len().max(1)` fixes it, which is the convention the app's own batching already used.

**A signal mid-`getdents64` looked like a dead directory.** The `EINVAL` fix above breaks out of a
directory on any readdir error, which is right for errors that describe the descriptor — but
`EINTR` describes neither the descriptor nor the entry. It matters here rather than in theory: the
TUI handles `SIGWINCH`, so **resizing the terminal during a scan** could have silently truncated
whichever directory a worker happened to be reading, along with its entire subtree, and reported it
as one unreadable entry. `EINTR` is now retried and everything else still breaks.

Neither would have shown up in the numbers. The first makes the benchmark look *better* on a tree
of empty directories (no channel traffic), and the second needs a signal that no headless run
sends.

### The reflink fix crashed the renderer, and the bug was older than it

Reported from real use, a few minutes after the work above was declared done:

```
panicked at ratatui-core-0.1.2/src/buffer/buffer.rs:251:
index outside of buffer: the area is Rect { x: 0, y: 0, width: 170, height: 48 }
but index is (134, 80)
  4: diskonaut::ui::grid::draw_next_symbol::draw_next_symbol
  5: diskonaut::ui::grid::draw_rect::draw_rect_on_grid
```

Row 80 of a 48-row buffer: not an off-by-one, a tile laid out far outside the board.

The cause is the "sizes are not additive" property this document has described from the start,
finally meeting code that assumed otherwise. `files_in_folder` computed each entry's share as
`entry.size / folder.size`, which is only ≤ 1 when the entries add up to the folder. Shared blocks
mean they do not: four reflinked copies of one 1 MB file sit in a folder holding 1 MB, and the
shares come out at **4.0**. The squarify layout then places tiles well off the screen, and the UI
indexes the terminal buffer directly, so it panicked instead of drawing wrong.

Hard links could always have done this, and the gotcha above says so in as many words. What
changed is the odds: the reflink work added 10,804 newly-deduplicated files on this volume, and
`~/.cache/uv` went from "entries sum to the folder" to 15.1 GiB of entries in a 12.0 GiB folder.
A latent bug became a crash anyone scanning a `uv` cache would hit.

Two changes, because the second would have made the first a cosmetic glitch:

1. **`files_in_folder` divides by the larger of the folder and the sum of its entries.** A tile is
   a share of the space its siblings take between them, which fills the board exactly and is the
   only reading that stays self-consistent once blocks are shared.
2. **`RectangleGrid::render` skips a tile that does not fit the buffer.** The layout is float
   arithmetic over sizes that need not add up; "this tile does not fit" is a thing that can happen,
   not an invariant worth crashing over.

`tiles::tests::entries_larger_than_the_folder_holding_them_stay_on_the_board` builds the
four-copies-in-a-one-copy-folder case, asserts the shares stay within 1.0, and asserts every tile
lands on the board. It fails on the old code with `percentages must not exceed the board, got 4`.

The lesson is the one finding #5 already records, from the other direction: **a number that moves
toward the truth is not the whole story.** The reflink work produced a total that agreed with the
filesystem, and every test and benchmark passed, because nothing downstream of the total was being
checked. Hunting the crash by resizing the terminal and fuzzing the layout found nothing; writing
down the invariant the fix had quietly broken found it in one test.

### What the review caught, including a claim that was simply false

Reviewed after the commits were written. Five findings, all real; two are worth repeating.

**Keying on the first extent could halve a total, not overstate it.** The code above documented
itself as erring high: "a file that shares only part of itself keeps being counted in full". That
was wrong, and the ledger's size guard does not save it, because it only fires when the two sizes
*differ*. Two equal-sized files sharing nothing but their opening extent were merged, and one of
them counted as nothing:

```sh
cp --reflink=always a/img.bin b/img.bin        # 1 MiB each
dd if=/dev/urandom of=b/img.bin bs=1k seek=100 count=100 conv=notrunc
```

`filefrag` shows `b` with three extents — shared, *not* shared, shared — and diskonaut reported
**1.0 MiB for 2.0 MiB of files.** The identity is now the whole extent map (up to 64 extents,
FNV-folded), accepted only when every extent is shared and the `LAST` flag proves the map is
complete. Anything else is counted in full, which is what the comment always claimed.

The lesson is the one this file keeps relearning: the guard that was supposed to make this safe
(`seen.size != size`) was written for hard links, where two different files cannot share an inode
number *and* a size. Reused for extents, the same line stopped meaning what it said.

**`statx` is not `lstat`, and the difference automounts a network.** The walk asked for
`AT_SYMLINK_NOFOLLOW` and nothing else. `stat`, `lstat` and `fstatat` all behave as though
`AT_NO_AUTOMOUNT` were set; **bare `statx` does not**, and `man 2 statx` names this exact case —
"can be used in tools that scan directories to prevent mass-automounting of a directory of
automount points". The walker it replaced went through `std`'s `lstat` and was implicitly safe.

So merely *looking at* an autofs placeholder mounted it. A directory of NFS home maps would have
been mounted wholesale and one dead server would have hung the scan — and this fires at stat time,
before the descent decision, so the care taken over autofs in `filesystem` did not cover it at all.
That reasoning is only sound now the flag is set.

The other three, more briefly:

- **The thread cap was per-platform when it wanted to be per-walker.** `thread_count()` also feeds
  `scan_folder` (public) and the `dua-*` benchmark stages, so raising Linux to 24 ran the one
  walker that collapses past eight at 24 of them — 38% slower, and it quietly re-tuned the very
  baseline this document compares against. There are now two caps.
- **`i128::from(f_type)` sign-extends on 32-bit.** `__fsword_t` is signed and 32 bits wide on i686
  and armv7, so every magic with the top bit set — btrfs, selinuxfs, bpf, hugetlbfs — would never
  have matched there, silently turning the whole reflink feature off. Truncating to `u32` is
  lossless and correct on both.
- **`Drop` set the stop flag outside the lock**, which is exactly what `retire` takes the lock to
  avoid. It was rescued by the retire-to-zero path, but only by a multi-step argument; it now holds
  the lock, and the argument is not needed.

### Known gaps in this pass

- **`tree-only` is distorted, not merely noisy, and should be read as an upper bound.** It ranges
  from 0.37s to 0.87s across runs, and in one run reported 0.871s while `tree` — which contains it —
  reported 0.727s. A stage cannot cost more than the stage that contains it, so this is systematic:
  the untimed `collect()` of 4.2M `DirEntries` leaves the allocator and page cache in a state the
  real pipeline never sees, and the timed build then runs in it. The conclusion it was used for
  still holds (walk 0.49s, pipeline 0.72s), but the number itself should not be quoted as the
  model's cost without that caveat.
- **`dua-core` is still a dependency** (bumped 3 → 4.1.0; the `walk` API and the comparison are
  unchanged — 4.x only wraps `Entry::metadata` in an `Option` for its new `skip_metadata`). It backs
  the `dua-*` benchmark stages, which are how the comparison above is reproduced, and
  `fallback::group_by_directory` for platforms that are neither macOS nor Linux. Dropping it would
  mean giving up the baseline.
- **The reflink threshold is a guess, not a measurement.** 64 KiB was chosen because it leaves 3.8%
  of files to probe on this volume. Nobody has measured how many shared bytes live below it.
- **A scan of `/` double-counts filesystems mounted in two places**, as above. `-x` avoids it.
- **The macOS build is unverified.** `scan/macos.rs` needed one field adding to two `EntryMeta`
  literals and nothing here can compile it — the module is `cfg`'d out on Linux, which is exactly
  the trap finding #7 records. It needs a build on a Mac before release.
- **`cargo deny check` was not run**; `cargo-deny` is not installed here. `Cargo.lock` is unchanged,
  but rustix's feature set is (`std` and `fs` added), so the licence and advisory gates are unproven.

## Is there a filesystem-specific way to go faster? (2026-09-22, after the walker)

Asked again once the native walker had landed, which is the right time to ask: the answer changed,
because the bottleneck did.

### The walk is no longer the bottleneck

Interleaved runs at the default thread count, `/data`:

```
walk     0.434  0.436  0.413  0.434  0.415  0.448  0.418  0.416
tree     0.682  0.691  0.681  0.701  0.690  0.751  0.710  0.682
pipeline 0.738  0.732  0.692  0.734  0.706  0.721  0.698  0.735
```

`pipeline` is not `max(walk, model)` with the model hidden — it sits at `tree`. Sweeping threads
shows why, and the shape is unambiguous:

| threads | 2 | 4 | 6 | 12 | 24 |
| --- | --- | --- | --- | --- | --- |
| `walk` | 2.563s | 1.328s | 0.931s | 0.548s | 0.434s |
| `pipeline` | 2.805s | 1.458s | 1.034s | 0.923s | 0.725s |

Up to six workers `pipeline` tracks `walk` within ~0.1s: the model is hidden behind the traversal,
as it was designed to be. Past twelve, `pipeline` stops following `walk` down and flattens at
~0.72s while `walk` keeps falling to 0.43s. That floor is the model.

**So `pipeline ~= max(walk, ~0.70s)`.** The tree build costs about 0.7s for 4.23M entries (~166ns
each), the walk costs 0.43s, and the walk is already entirely hidden behind it. (The 0.43s is the
`walk` *stage*; the last section of this document takes it apart and finds the walker itself nearer
0.40s, the rest being the harness. It does not change the conclusion here.)

This also gives a better number for the model than `tree-only` does. That stage reports 0.42-0.48s
and is the distorted one — its untimed `collect()` leaves the allocator and page cache in a state
the real pipeline never sees, so it under-reports by roughly 1.5x. `tree`, where a single consuming
thread drives the walk, lands at 0.69s and agrees with the `pipeline` floor. **Prefer `tree` over
`tree-only` when asking what the model costs.**

### Which caps every filesystem-specific idea at about 3%

Every bulk-metadata mechanism — XFS `BULKSTAT`, `GETFSMAP`, a hypothetical ext4 equivalent —
attacks the per-file `statx` and nothing else. Single-threaded that was measured at 0.93s for
`getdents64` alone against 4.4s with a stat per entry, so the stat is ~78% of traversal. Scaled to
the current walk, that is roughly **0.34s of statx and 0.09s of getdents**.

Make the stat *free* — the ceiling no real API reaches — and the walk goes 0.43s to 0.09s, while
the scan goes:

```
max(0.43, 0.70) = 0.72s   ->   max(0.09, 0.70) = 0.70s
```

**About 3%.** That is the entire prize, before any of it is shown to be reachable. It is not
reachable: bulkstat is `EPERM` unprivileged and returns inodes without names, so it cannot replace
a walk at all — only serve as a size oracle joined on inode, which is a large architectural change
for a fraction of that 3% — and `GETFSMAP` is callable but redacts every owner.

The other candidates were measured earlier in this document and are all zero or negative: a minimal
`statx` mask, `AT_STATX_DONT_SYNC`, skipping stats on directories, and `io_uring` batching (3.8x
*slower*).

### ext4 specifically

No supported userspace bulk-metadata API, as before. `EXT4_IOC_PRECACHE_EXTENTS` is per-file extent
caching, not metadata enumeration. Reading the inode table directly (the `e2image` approach) needs
root and a quiescent filesystem and is not appropriate for a live tool. Nothing has changed here.

### The one filesystem-aware lever left, and why it is not tested

**Stat in inode order, not readdir order.** Both ext4 (with `dir_index`) and XFS return directory
entries in name-hash order, which is uncorrelated with inode number, so the inode table is touched
at random. Sorting a directory's entries by `d_ino` — which `getdents64` already returns, for free —
makes that access sequential. It is the classic trick, and the only genuinely filesystem-shaped
optimisation this exercise has not tried.

It is untested here because **it can only pay on a cold cache, and every measurement in this
document is warm** — `/proc/diskstats` shows zero sectors read from `bcache0` during a full scan.
Warm, sorting changes no syscalls and no lookups, and in-core inodes are slab-allocated rather than
laid out by inode number, so there is nothing for the ordering to exploit. Testing it honestly
needs `/proc/sys/vm/drop_caches`, which is mode `0200` and root-owned.

Worth doing if cold scans ever matter — a first scan after boot, or a volume much larger than RAM.
It would not move any number in this document.

### An allocator experiment that failed usefully

The model at 0.7s is 4.2M `OsString`s, per-directory `Vec`s and `Arc<Path>`s allocated across 24
worker threads and freed on one consumer thread — the pattern glibc's malloc is supposed to handle
worst. Swapping in mimalloc as the global allocator tested that in a few minutes:

| | glibc | mimalloc |
| --- | --- | --- |
| `walk` | 0.43s | 0.51-0.69s |
| `tree` | 0.69s | 1.49-2.30s |
| `pipeline` | 0.72s | 1.56-2.05s |

Two to three times *worse* across the board, so the hypothesis is dead and the dependency was
reverted. Recorded because the negative result is the useful part: the model's cost is not glibc
arena contention, and whatever it is will not be fixed by changing allocator.

### The answer

No. Not on this machine, not warm, not without root — and the ceiling if all three were solved is
about 3% of the scan.

The remaining time is in the data model, which is not a filesystem question. If this is picked up
again the targets are the ~166ns per entry the tree build costs and the 4.2M string allocations
underneath it — an arena, or interned names — not another ioctl.

Two measurements would close the file, neither takeable from here:

- `sudo ./docs/probes/fsmap_scan /data` — confirms the ownership redaction is a privilege check and
  prices the inode-to-size oracle at its true ceiling.
- A cold scan after `echo 3 > /proc/sys/vm/drop_caches`, with and without `d_ino` ordering, which is
  the only regime where the answer above might be different.


## btrfs and ZFS (2026-09-22)

Same question as the section above, and the same answer, for the same reason: the scan is bound by
the tree build at ~0.70s, the walk is 0.43s and already hidden, so anything that attacks the
per-file `statx` is capped at about 3%. That arithmetic does not care which filesystem it is.

Two things are nonetheless worth writing down, and one of them was a bug.

### The bug: reflinks were invisible on every filesystem but the scan root's

The probe was gated on `device == scan_device` — added, correctly, to stop a scan that wanders onto
an NFS or FUSE mount from opening files there. But "is this the filesystem I started on" is the
wrong question. The right one is "can this filesystem share extents", and the two differ exactly
where it matters most:

- **btrfs gives every subvolume and snapshot its own `st_dev`.** A btrfs root is one filesystem
  presenting many device numbers, and subvolumes are where the sharing lives — snapshots share
  everything by construction. Reflink accounting would have been off for all of it.
- A second XFS volume mounted inside the scan is in the same position.

Confirmed on the nested XFS mount on this machine (`/dev/sdd` under `/data`, `reflink=1`), by
counting detections with and without a deliberately reflinked 150 MiB pair inside it:

```
before   with the pair 9305 reflinked   without it 9305   <- never seen
after    with the pair 9306 reflinked   without it 9305   <- seen
```

The gate is now per filesystem, by `statfs` magic, decided once when the walk crosses into a mount
and carried on the job — the same `statfs` that already decides whether a mount is a pseudo
filesystem, so it costs nothing extra. NFS, SMB and FUSE are still never probed, now because they
are not XFS or btrfs rather than because they are not the scan root.

**Lifting that gate needed a second change.** With one filesystem in play, physical block offsets
were unambiguous. Across several they are not: two unrelated files on two volumes sharing an offset
and a size is an easy collision, and the ledger would merge them — the same undercount that keying
on a single extent used to cause. The device is now folded into the extent identity before the
extent map is.

### btrfs `-x` reports almost nothing, and that is not a bug

Because subvolumes carry distinct `st_dev`, `--one-file-system` on a btrfs root refuses to descend
into any of them. That looks wrong, and the temptation is to compare `f_fsid` instead.

Don't: **`du -x` compares `st_dev` too and stops in exactly the same places.** The documented
contract is "like `du -x`", so this is consistent, and switching to `f_fsid` would make it
inconsistent with the thing it claims to match. On a btrfs root, use the default rather than `-x`.

### What is actually different about btrfs, for performance

`BTRFS_IOC_TREE_SEARCH_V2` reads the filesystem B-trees directly, and unlike XFS's `BULKSTAT` it
returns **names as well as inode items** — `DIR_INDEX` entries carry the name, `INODE_ITEM` the
size and link count. It is therefore the only bulk-metadata interface in this whole investigation
that is architecturally sufficient to replace *both* `getdents64` and `statx`, rather than serving
as a size oracle that still needs a walk for the namespace.

It is still capped at the ~3% above, and it is **believed** to require `CAP_SYS_ADMIN` like the
other tree-search ioctls. That belief is *not verified here* — btrfs is compiled into this kernel
but there is no btrfs volume to test against and mounting one needs root, so unlike every XFS claim
in this document it rests on reading rather than on a probe. Treat it accordingly.

Also worth dismissing, because it is the obvious thing to wonder: **btrfs qgroups** answer "how
much does each subvolume hold" instantly, and `zfs list -o space` does the same for datasets. Both
are far coarser than a treemap needs — they stop at the subvolume or dataset, not at the directory
— so neither substitutes for a walk.

### Measured on btrfs (2026-09-23): snapshots were counted once each

With a loopback btrfs now available (`fixtures/fs`, in a privileged container), the report that
btrfs double counted was reproduced at once: a subvolume of 43.1 MiB with two snapshots scanned as
129.4 MiB. The extent identity folded in the device so that equal offsets on two volumes would not
merge — but a snapshot *is* another device over the same address space, so its files never matched
the live ones they share every extent with. The identity now folds in the filesystem's UUID on
btrfs, from `BTRFS_IOC_FS_INFO`, which any user may call and which agrees across the top level,
subvolumes and snapshots (`f_fsid` differs per subvolume like `st_dev`; `FS_IOC_GETFSUUID` is not
implemented by btrfs). XFS keeps the device. After: 49.4 MiB, and the large-file snapshot fixture
is byte-exact against btrfs's *data used*.

What remains, measured on the same volume:

- **Small files.** Nothing under `PROBE_ABOVE_BYTES` (64 KiB) is probed, so each snapshot counts
  its small files again. FIEMAP answers them correctly (a 4 KiB file in a snapshot is `SHARED` at
  the live copy's offset), and a build probing everything from 4 KiB matched btrfs exactly on 40k
  files with a snapshot, but took 0.159 s against 0.018 s — about 2 µs a file, which on a 4M-file
  btrfs root would be seconds. On `/usr` here 96% of files and 13.6% of bytes are under 64 KiB, so
  a snapshot of `/` is overstated by roughly an eighth of itself. **Resolved** by a second pass:
  the walk notes these files instead of probing them, and they are probed once the tree is on
  screen, so the walk's time is unchanged (`scan::refine`). The fixtures' small-file snapshot case
  now matches btrfs's data figure exactly.
- **Inline files** (up to ~2 KiB) live in metadata: FIEMAP says `DATA_INLINE` at offset 0 and
  never `SHARED`, and `st_blocks` claims 4 KiB. They cannot be matched this way at all.
- **Compression is invisible to `stat`**: 64 MiB of text under `compress-force=zstd` holds 2 MiB
  of data and `stx_blocks` says 64 MiB. That settles `probes/btrfs_compression_check.sh`: no.
  Only `BTRFS_IOC_TREE_SEARCH_V2` sees compressed extent sizes, and it returns `EPERM` to a user
  (checked). **As root it is now used**: one search per file on the directory's descriptor, no
  open. Measured on 40k files of 4–60 KiB: 0.014 s as a user, 0.052 s as root on a `compress`
  mount — about 1 µs a file — and 0.014 s as root on an uncompressed one, where only files
  `statx` marks compressed are searched. The fixtures' compressed volume (text, random, mixed,
  preallocated, sparse) comes out at btrfs's data used to the byte.
- **Long extent maps**: the FIEMAP probe read 64 extents and gave up on more. A compressed file has
  an extent per 128 KiB, so every compressed file over 8 MiB was counted once per snapshot; it
  now pages through the map.
- A reflink clone rewritten in part leaves the *source* wholly `SHARED` on btrfs — the original
  extent stays whole, referenced by the clone's unchanged ends — where XFS splits it. Totals are
  unaffected; the reflinked count can be one higher.

### ZFS

Nothing here is measured: OpenZFS is not installed on this machine, so the following is reasoning,
not a result.

- **No bulk-metadata ioctl** exposed for this purpose. `zdb` reads pool internals but is a
  debugging tool that wants the pool quiescent.
- **FIEMAP is not implemented**, so the reflink probe is correctly skipped — by construction, since
  the magic list names only XFS and btrfs. Block cloning arrived in OpenZFS 2.2 and would be
  invisible to this code, which is the honest limitation.
- **Each dataset is its own `st_dev`**, so `-x` stops at dataset boundaries. Unlike btrfs
  subvolumes this matches most people's expectations, and `du -x` again does the same.
- **`.zfs/snapshot` is the hazard to know about.** It is hidden from readdir unless `snapdir=visible`,
  but where it is visible, every snapshot appears as a subtree and each one *automounts on access* —
  a scan would mount every snapshot and count the whole filesystem once per snapshot. The
  `AT_NO_AUTOMOUNT` fix above stops merely stating them from triggering that; descending still
  would. If ZFS support is ever taken seriously, `.zfs` belongs in the same category as `/proc`.


## The memory model (2026-09-22)

With the walk at 0.43s and the tree build at ~0.70s, the model is what the scan waits for. This is
what came of attacking it. One change shipped; three did not, and the three are the more useful
half of the record.

### The budget, first

`pipeline ~= max(walk, model)`, and the walk is 0.43s. So a model faster than ~0.43s buys nothing
end to end:

| model | 0.70s | 0.55s | 0.43s | 0.20s |
| --- | --- | --- | --- | --- |
| pipeline | 0.73s | ~0.58s | ~0.46s | ~0.46s |

There is about 0.3s worth taking and no more. **Memory is the axis with no such ceiling** — 824 MB
for 4.2M entries is ~193 bytes each, and a 10M-entry volume would want 2 GB.

### What worked: a file's size is a `u64`

`File::size` was a `u128`. It is the most repeated field in the model and, being the larger variant,
it set the size of `FileOrFolder` — which is what every slot in every folder costs. 24 bytes became
16.

| | before | after |
| --- | --- | --- |
| peak RSS | 824 MB | **654 MB** (−21%) |
| `tree` | 0.73s | **0.68s** |

A `u64` counts to 16 EiB. The range was never doing anything.

### What did not work, and why

**A `Vec` per folder instead of a hash map.** The reasoning was good and the measurement refused
it. Directories are tiny — median 3 entries, mean 11, p99 120 — so hashing a name to place three
items looked like waste, and a `hashbrown` table rounds its allocation up however few things go in.
Measured: `tree` 0.73s → 0.75–0.79s and RSS 824 MB → **881 MB**, worse on both. Removing the
duplicate check on insert recovered the time to level but no better. The conclusion is worth
keeping: **with a word-at-a-time hasher and SIMD probing, hashbrown is already at the speed of a
linear scan over three names**, and exact-sized `Vec`s lose the memory argument once a folder is
filled incrementally and doubles.

**Packing every folder's names into one buffer.** The measured shape said this should be the big
one: the mean name is 32 bytes, so an `OsString` costs ~24 bytes inline plus a ~48-byte heap block,
about 305 MB of the tree in 4.2M scattered allocations. Replacing that with one buffer per folder
and `(u32 offset, u32 len)` per entry predicted ~430 MB.

It delivered `tree` 0.68s → **0.85s** and RSS 654 MB → **642 MB**: 25% slower to save 2%.

The diagnosis is the useful part, and it is not that the idea is wrong — it is that **interning
inside the model cannot move the number the model does not set.** Peak RSS is reached while the
scan is in flight, and the walk allocates an `OsString` per entry regardless; packing them a second
time in the consumer *adds* a copy while the originals are still live in the channel. The only
place the allocation can be removed is where it is made.

**mimalloc.** Testing whether the model's cost is glibc arena contention — 24 threads allocating,
one thread freeing, the pattern glibc handles worst. Two to three times worse on every stage.

### What would actually work

Carry the packed names in `DirEntries` rather than rebuilding them in the model: the walker already
reads a directory's names out of one `getdents64` buffer, so it can copy them into a single
`Vec<u8>` and hand it over with offsets instead of allocating an `OsString` each. The model then
*moves* that buffer into the folder — no per-entry allocation anywhere, and no copy at all in the
common case where the folder is new.

That is the version worth doing, and it was not done here because it changes `NamedEntry`, which
means restructuring `scan/macos.rs` — macOS-only, `cfg`'d out on Linux, and therefore uncompilable
from this machine. Reshaping a type blind is exactly the trap finding #7 records; adding one field
blind was already at the edge of reasonable.

Measurements for whoever picks it up, so none of it has to be re-derived:

| | |
| --- | --- |
| entries | 4,232,419 |
| mean name length | 32.1 bytes |
| total name content | 129.6 MB |
| directory fan-out | median 3, mean 11.2, p90 13, p99 120, max 43,009 |
| tree after the `u64` change | ~0.68s, 654 MB (~155 bytes/entry) |


## Can the walk go below 0.43s? (2026-09-22)

It is already below it. The 0.43s this document has been quoting for `walk` was partly the
benchmark harness, and taking it apart is the whole answer.

### What the walk is made of

Each row adds one thing to the row above, `/data`, default threads:

| | walk |
| --- | --- |
| traversal alone — entries discarded without being freed, no reflink probe | **0.336–0.356s** |
| plus the reflink probe | 0.391–0.410s |
| plus the harness freeing 4.2M names on the consuming thread | 0.427–0.471s |

So the walker is **~0.40s**, not 0.43s, and without reflink accounting it is ~0.35s. For scale,
`docs/probes/mtwalk.c` — a C walker that allocates nothing and emits nothing — needs 0.387s for the
same tree. **The traversal is at the floor and slightly under it.**

The last row is a measurement artifact of the same family as `tree-only`: the `walk` stage frees
every name on one thread, which the app never does — the tree takes ownership and keeps them. It
was tempting to `mem::forget` them and re-publish a lower number. Measured, that is a bad trade:
five runs each gave 0.427/0.471/0.428/0.438/0.464 dropping against 0.400/0.429/0.404/0.418/0.431
leaking, so it removes about **0.02s — less than the 0.044s spread between runs of the same
binary** — and costs 434 MB of leaked memory, which would make the stage's RSS meaningless. The
harness is left alone and the artifact is written down here instead.

**Treat anything under ~0.04s in this section as noise.** Several of the effects below are near
that line.

### Where the remaining kernel work goes

The walk is 89% kernel: 0.98s user against 8.10s system. At ~22 effective cores out of 24 threads
the parallel efficiency is already high, and more threads make it worse. Syscall counts, from
`strace -c` on a subtree of 27,884 entries in 3,539 directories:

| syscall | calls | share of time |
| --- | --- | --- |
| `statx` | 27,888 | 55% |
| `openat` | 6,200 | 14% |
| `getdents64` | 7,078 | 14% |
| `close` | 6,200 | 12% |
| `ioctl` (FIEMAP) | 2,656 | 5% |

One `statx` per entry, and it is 55% of the time. That is the irreducible part: the size has to come
from somewhere and no unprivileged bulk interface will give it (see the two sections above).

Two things in that table are *not* one-per-entry and were looked at:

- **`getdents64` runs exactly twice per directory** — once returning entries, once returning zero to
  prove the end. 385k extra syscalls, perhaps 0.6s of kernel CPU. It can be avoided by treating a
  short return as end-of-directory, and it is **deliberately not avoided**: the kernel fills the
  buffer greedily on local filesystems but does not promise to, and a filesystem that returns short
  for its own reasons would have its directories silently truncated. That is the same shape as the
  `EINVAL` loop above — silent loss, found only by someone noticing a missing subtree.
- **`openat`/`close` are 1.75 per directory, not 1**, because the reflink probe opens files too.

### The reflink probe, and why the threshold stays at 64 KiB

The probe is the only part of the walk that is discretionary. It costs about **0.05s of wall and
1.67s of CPU** — cheap in wall terms precisely because it parallelises well.

Raising the size threshold is the obvious lever, and the curve says don't:

| threshold | walk | reported total | distinct reflinked files found |
| --- | --- | --- | --- |
| **64 KiB** (current) | 0.466s | **785.4 GiB** | **10,805** |
| 256 KiB | 0.407s | 786.6 GiB | 3,050 |
| 1 MiB | 0.396s | 788.1 GiB | 885 |
| 4 MiB | 0.385s | 789.9 GiB | 304 |

Going to 1 MiB buys 0.07s and loses 9,920 shared files — 2.7 GiB counted twice that should not be.
By this document's own standard a wrong number outranks a slow one, so 64 KiB stays. The table is
recorded as the justification for the status quo, not as a tuning opportunity.

### The one idea left, and why it was not built

The probe costs 1.67s of CPU for 0.05s of wall, while the tree build that follows runs
**single-threaded for 0.68s with more than twenty cores idle**. Moving the probe off the walk's
critical path and into that idle time is the only structural change left that could lower the
traversal figure.

It cannot be done naively. The model needs `shared_extent` at the moment it charges an entry to a
folder; probing afterwards means folder sizes that are wrong until the second pass lands and then
change underneath the user. "The number moves after you have read it" is exactly what breaks the
"delete this to free 15 GB" promise these sizes exist to make. Doing it properly means the model
charging provisionally and reconciling, which is a larger change than the 0.05s justifies.

### And none of it matters yet

`pipeline = max(walk, model)` = `max(0.40, 0.68)` = **0.68s**. Every improvement to the walk is
invisible until the model comes down, and the model's remaining win needs the packed-names change
to `DirEntries` that the previous section declines to make blind against macOS `scan/macos.rs`.

The walk was asked to get under 0.43s. It is at 0.40s with reflink accounting and 0.35s without,
and the next useful work is not here.


## Packed names, end to end (2026-09-22)

The change the previous section named as "what would actually work", now measured rather than
predicted. `DirEntries` carries one buffer holding all of a directory's names, entries refer to
theirs by offset and length, and the tree **takes that buffer** instead of copying names out of it.
No name is allocated between the `getdents64` buffer and the folder that ends up holding it.

| | before | after |
| --- | --- | --- |
| `walk` | 0.43–0.47s | **0.385–0.421s** |
| `walk` peak RSS | 134 MB | **74 MB** |
| `tree` | 0.67–0.69s | **0.623–0.669s** |
| `pipeline` | 0.70–0.81s | **0.649–0.678s** |
| `pipeline` peak RSS | 654–670 MB | **617 MB** |

Entry counts, totals, hard-link and reflink counts are unchanged.

So: real, and smaller than predicted. The prediction was ~430 MB and it landed at 617 MB. Two
things account for the gap, and both were measured rather than guessed.

**The buffers arrived half empty.** They are filled by pushing, so they double as they go, and the
tree then keeps that slack for the life of the scan. Counting what was handed over: 125.3 MB of
names in 190.3 MB of capacity — **52% overshoot**. One `shrink_to_fit` per directory before the
buffer is sent brought capacity to exactly 125.4 MB and peak RSS down by ~30 MB. Worth knowing in
general: a `Vec` that is grown by pushing and then *retained* pays its growth slack forever.

The move itself works as intended — of 385,497 directories handed to the tree, **384,991 had their
buffer moved and 506 copied**. The copies are folders whose own group arrived after one of their
children's, so a placeholder was already sitting in the buffer.

**The rest is the allocator, and it is not worth buying back.** With names allocated on 24 worker
threads and never freed — the tree keeps them — each thread's glibc arena grows and none of it is
reused. Capping the arenas proves it and prices it:

| | peak RSS | `pipeline` |
| --- | --- | --- |
| default | 625 MB | 0.667–0.697s |
| `MALLOC_ARENA_MAX=4` | 586 MB | 1.25–1.42s |
| `MALLOC_ARENA_MAX=1` | 538 MB | 2.67–2.90s |

88 MB for **four times the wall clock**. Left alone.

### What this cost elsewhere

`NamedEntry` no longer carries a name, which is a breaking change to the library's scan API and
touches every walker. The `dua-core` fallback and the macOS `getattrlistbulk` walker both collect
entries before they know they have a whole directory, so they keep owned names and pack them where
a `DirEntries` is built — one copy per entry on those paths, and none on Linux.

**The macOS walker is changed but unverified.** `scan/macos.rs` is `cfg`'d out on Linux and there is
no Mac here, so it has been checked by reading and by `rustfmt` parsing it, and that is all. It
needs a build and a run on macOS before release. This is the trap finding #7 records, entered
deliberately and with the user's agreement rather than by accident.


## Squeezing the model, with memory no longer sacred (2026-09-22)

Asked for more speed, memory allowed to give. Two changes landed and three were refused by
measurement, which by now is the expected ratio.

### Fewer reads, or more sequential ones?

The 34k reads split, single-threaded and in inode order (`docs/probes` scripts, `/proc/diskstats`):

| what is read | reads | MiB | how |
| --- | --- | --- | --- |
| the directories' inodes | 8.9k | 96 | `lstat` of each; ext4 inode readahead makes them 11 KiB |
| the directories' blocks | +25.5k | +100 | `getdents64`: one 4 KiB read per directory, no readahead, no merging |
| the files' inodes | +1.1k | +38 | `statx`, in inode order: readahead does nearly all of it |

So 72% of the reads are directory blocks, one synchronous 4 KiB read each, and nothing an
unprivileged process does changes their number: `posix_fadvise(SEQUENTIAL)` on the directory fd
does nothing (its readahead is on the inode's own mapping, which ext4 directories do not use), and
at 24 workers the block layer merges no more than at one, since the reads that could merge are
never in flight together.

Their **order**, though, is ours to choose, and it matters more than expected: `FS_IOC_FIEMAP`
works on a directory fd, so `docs/probes/dirblock_prefetch.py --dry` can put every directory's
block on the disk and score a walk order by the contiguous runs those blocks form.

| order of directories | runs (31k dirs) | jumps back | seek distance | runs (310k dirs) |
| --- | --- | --- | --- | --- |
| depth first, readdir order (before) | 24.9k | 12.5k | 411 TB | 164k |
| depth first, each directory's children smallest inode first (now) | 6.0k | 1.6k | 60 TB | 64k |
| whole queue smallest inode first | 3.6k | 0.9k | 33 TB | 20k |

ext4 puts a directory's blocks in its inode's block group, so inode order is disk order. The
walker stacked a directory's children in inode order and popped the *largest* first; reversing
that costs nothing and turned 25k runs into 6k. Single-threaded and cold that is 7.3s → 6.2s
(same reads: the device answers a forward pattern in 0.09 ms instead of 0.13 ms), and the whole
queue in inode order 6.0s — but that needs sorting the queue, 6% warm on 2.2M entries, and at 24
workers neither is measurable on this SSD (0.73–0.77s throughout), which is at its IOPS floor
whatever the order. A spinning disk, where the order is the seek, is where the 7x shorter
distance would show. Kept: the free one.

**Reading fewer blocks is possible, with root — and the walker now does it.** In this order the
blocks come in runs of 5 to 9, and one `posix_fadvise(WILLNEED)` on the *block device* from the
first block of a run fetches the run in one read — the buffer cache `getdents64` looks in is the
device's page cache, so the siblings' blocks are then already in core. Where the filesystem is
ext2/3/4 and the device opens for reading (root, or the `disk` group; found through
`/sys/dev/block/<maj>:<min>/uevent` and checked against `st_rdev`), the walker asks FIEMAP where
each directory's first block is as it opens it and advises a 32 KiB window from there, unless
the last window covers it (`linux::dirblocks`; one window per worker, since workers are in
different subtrees). Anywhere else the walk is exactly the unprivileged one.

Measured as root, cold, 24 workers, the block layer through `/proc/diskstats`:

| | reads | avg read | MiB | `project` | `home` |
| --- | --- | --- | --- | --- | --- |
| unprivileged | 35.5k | 6.8 KiB | 236 | 0.745s | 5.25s |
| root, blocks read ahead | 12.1k | 23 KiB | 274 | 0.70s | 4.21s |
| `diskus` (24 threads) | 35.4k | 6.8 KiB | 235 | 0.77s | 4.93s |

The reads fell by the predicted 3x, from one 4 KiB read per directory to one 23 KiB read per run,
and diskonaut as root is now ahead of `diskus` cold on both trees, 9% and 15%. But the clock
moved less than the reads: 6% on `project`, 20% on `home`. The reason is in the CPU columns.
A cold scan of `project` burns 4.2–4.7s of *system* time against 1.0s warm, whatever the walker,
and diskonaut's 24 workers spend it on 8 cores: 6.7 cores busy for the 0.7s. Cold, the kernel's
work per entry — instantiating 385k inodes and dentries from the buffers, the page cache and
buffer heads for every block, the completion of every request through virtio — is four times
the warm walk, and on this box that is the floor once the reads are in flight. Fewer requests
did trim it (the 23k requests saved were 0.4s of system time, about 17 µs each, and `home` saved
4s of 28), which is the gain seen. A machine with more cores per disk, or a slower disk where the
round trip rather than the completion is the cost, has more to gain from the same change.

Warm, as root, the FIEMAP and the advice cost 5% on `project` (233 → 245 ms), 3% on `home`;
the totals are unchanged, forced onto a regular file (`DISKONAUT_DIRBLOCKS_DEVICE=<file>`) or
as root; the fixtures, which run as root on loop-mounted ext4 and so take the real path, agree
with their oracles. `docs/probes/dirblock_prefetch.py` is the same idea single-threaded
(`--window 0` to switch it off, `--dry` for the runs without root): there, one thread, the
reads do *not* fall (35.6k either way), because a single thread's `WILLNEED` is answered in the
order it was asked and the thread is already waiting on that very block — the prefetch only
pays when other workers open the siblings meanwhile.

**Further, with root: the inode tables too.** The remaining 10k reads are inode blocks, already
11 KiB each thanks to ext4's readahead. With the device open, the superblock and group
descriptors give where every inode is, and a directory's children — known in sorted inode order
before their stats — could be advised in one range per inode-table run. Perhaps 10k reads to 3k;
not attempted.

### Where it stands

| | |
| --- | --- |
| `walk` | 0.385–0.421s |
| model (`tree`) | **0.622–0.651s** |
| `pipeline` | **0.626–0.650s** |
| peak RSS | 593 MB |

`pipeline = max(walk, model)`, so the model is still what the scan waits for, and **it only has to
reach 0.40s** — the walk's floor — for any further work on it to stop mattering. That is a 1.6x
target, not an open-ended one.

### What landed

- **`shrink_to_fit` on each directory's buffers turned out to be a speed win too**, not just the
  30 MB it was added for: `tree` 0.664–0.707s with it disabled against 0.623–0.669s with it. Smaller
  buffers, better locality. Both dimensions agree here, which is rare.
- **A folder no longer stores its own name.** It was already the key in its parent, nothing ever
  read the copy, and it cost 385k `OsString`s and 24 bytes per folder: `pipeline` 0.654–0.686s to
  0.626–0.650s, RSS 617 MB to 593 MB.

### What was refused

**An index of subdirectory positions, so resolving a path scans only folders.** The reasoning was
sound — a directory holds a median of 3 entries and usually one is a subdirectory, so scanning all
of them to find a child looks wasteful. Measured: `tree` 0.633–0.670s to **0.671–0.716s**, worse.
Following `subdirs[i]` into `entries[position]` hops through memory where the plain scan walks it
in order; at these sizes the contiguous read wins, and the index just adds a third vector to
maintain.

That is the third time this session a lookup structure has lost to a linear scan over the same
data. The pattern is worth stating outright: **for a container of about a dozen adjacent items,
any index that adds an indirection is likely to lose.** The per-folder hash map, the `Vec` of
`(OsString, node)` pairs, and now the subdirectory index all failed the same way.

**Re-tuning the walker's thread count now that the model binds.** Flat from 12 to 32 workers
(0.644–0.691s); 24 stays.

### The instrumented breakdown, for whoever goes further

Timers around the phases, on `/data` (the timers themselves inflate the total, so read the ratios):

| phase | time |
| --- | --- |
| resolve the parent folder | 0.128s |
| place the entries | 0.314s |
| of which: directories (385k) | 0.060s |
| of which: files (3.85M) | 0.244s |

Placing a file is a push onto a `Vec` and costs ~25–40ns. That is not computation; it is the cost
of writing ~100 MB of tree into memory that was just allocated, one cache line at a time, on one
thread. Nothing in the model is algorithmically wrong — it is bandwidth and latency bound.

### The one lever left, and its price

**Parallelise the placement.** Each directory group targets a distinct folder, and placing entries
touches only that folder, so within one batch the targets are disjoint and could be filled by
several threads. The resolve pass would stay sequential — it creates ancestors and updates their
sizes — and the parallel pass would follow it per batch.

It needs only ~1.6x to reach the walk's floor, and 4 threads on disjoint folders should clear that
easily: `pipeline` 0.65s to ~0.40s, about 38%.

The price is that Rust cannot prove the folders are disjoint. It means raw pointers to tree nodes
handed across threads, in a tree the main thread is also rendering from live. That is a different
risk class from everything above — not a wrong number, a potential data race — and it is the
reason it is written down here rather than built.

> **Superseded, the same day.** It was built — but not this way. The design that shipped shares no
> memory at all: each thread owns a private tree, the live view is an outline the rendering thread
> builds for itself, and the trees are merged once at the end. See the next section.


## The tree build goes parallel — private trees, a live outline, one merge (2026-09-22)

The proposal, from outside the work: *let the live renderer do extra work while parallel workers
fill up their own folders, then a final dedup after it is done.* That is a better design than the
one the previous section declined, because it removes the reason for declining it. Workers that
own private trees share nothing, so there is nothing to race on; the "final dedup" is where
shared-block accounting goes; and the renderer builds itself a cheap outline to show meanwhile.

### Result

`/data`, 4.23M entries, the app's path (`--bench-stage sharded`) against the single-threaded build
it replaces (`pipeline`), interleaved:

| | `pipeline` | `sharded` |
| --- | --- | --- |
| scan, as the app waits for it | 0.646–0.677s | **0.432–0.467s** |
| of which walk + build | — | 0.417s |
| of which merge | — | **0.002s** |
| of which replay (the dedup) | — | 0.013s |
| peak RSS, benchmark | 594 MB | **558 MB** |

About **30% faster**, and it lands exactly on the walk floor: `walk+build` is the walk alone. Peak
memory went *down*, because a directory's name buffer is moved into whichever shard's tree owns it
rather than copied, and four smaller trees fragment the allocator less than one large one.

That is the build path in isolation. **As the app shows it — from launch to the `Total:` line under
`tmux`, five runs each — the previous app took 0.66–0.68s and this one takes 0.46–0.47s: the same
30%.** It did not start out that way. The live view cost the app a third of that gain at first, and
the section on it below is the record of two wrong turns on the way from 0.55s to 0.47s.

Totals are identical to the single-threaded build to the entry, the hard link and the reflink.

### Why it is correct

Everything rests on one property this document has relied on since the hard-link work:
**`HardLinks::charge` is order-independent.** A deferred tree counts every shared entry in full
and notes `(blocks, size, directory)`; the replay charges each note through one ledger and, where
the ledger says depths `0..=d` had already counted these blocks through another path, subtracts
the size from exactly those folders. Inline charging adds to depths `d+1..`; deferred-then-replay
adds to all and takes back `0..=d`. Same folders, same numbers — in *whatever order* the shards
happen to replay.

Tested three ways, because the property is load-bearing:

- `model::tests::sharded::sharded_build_matches_inline_build_folder_by_folder`: a random tree
  thick with hard links and reflinks, built inline and built as 1, 2, 3, 4 and 8 shards, merged
  and replayed; **every folder's size and descendant count compared**, over five seeds.
- `a_stub_parent_merges_into_the_real_one`: the placeholder case — a child's group arrives in one
  shard before its parent's arrives in another.
- `scan::tests::parallel_build_matches_the_single_threaded_tree`: the real walker on a real
  fixture, against `scan_into_tree`.

### The first version was slower than what it replaced

Worth its own heading, because the fix is the transferable part.

Sharding by a hash of the **whole** directory path balances perfectly and gave 0.701s — worse than
0.646s — with the merge at **0.242s** and scaling linearly with shard count (0.107s at 2, 0.405s at
8). The build phase was already at the walk floor; the merge ate the entire gain.

The reason: with whole-path hashing, every ancestor of every directory exists as a stub in every
shard that holds anything beneath it. A simulation over the real directory listing counted
**74,688 folders needing reconciliation at K=4**, across 274,496 shard-visits — and each visit is a
cold-cache pointer chase on one core, after the walk, into memory that four other cores wrote.

Sharding by the first **D components** of the path confines overlap to folders shallower than D;
everything deeper moves into the merged tree as a whole subtree, in O(1). The same simulation,
choosing D:

| D | prefixes | largest prefix | folders to reconcile | busiest shard, K=4 | K=8 |
| --- | --- | --- | --- | --- | --- |
| 3 | 64 | 61.7% | 3 | 73.6% | 65.2% |
| 4 | 355 | 13.3% | 64 | 39.0% | 26.0% |
| **5** | 2,479 | 10.8% | **355** | **29.5%** | **18.1%** |
| 6 | 11,146 | 10.7% | 2,479 | 33.9% | 28.0% |

D=3 is nearly free to merge and useless to parallelise: `home/angch/project` alone is 62% of the
volume, and a prefix is indivisible. D=6 gets *unluckier* than D=5 because the ~10% subtrees are
still indivisible and now hash into fewer, larger lumps. D=5 is the knee, and the bench agreed:

| | walk+build | merge | total |
| --- | --- | --- | --- |
| whole path, K=4 | 0.444s | 0.242s | 0.701s |
| D=5, K=4 | 0.423s | **0.002s** | **0.437s** |
| D=5, K=8 | 0.450s | 0.002s | 0.467s |

The whole D∈{4,5,6} × K∈{4,6,8} grid lands within 0.437–0.480s, so the choice is not fragile.
`SHARD_DEPTH = 5` and `SHARDS = 4` are the constants here, with this table as their reason — on
Linux, where the fast walker makes the build the bottleneck. (This once said "Linux and macOS";
macOS turned out to be walk-bound and uses one shard — see "macOS, re-benchmarked" below.) Windows is walk-bound and
uses one shard instead, which needs no merge or replay at all; see "The shard count, and why
Windows uses one" below.

### The live view

Loading mode allows full navigation — move, zoom, enter, go up — so the view during the scan
cannot be a stub at some depth without sending people into empty folders. Instead the dispatching
thread, which sees every directory anyway, sends the renderer a **`DirSummary`**: the directory's
subfolder entries and what its files add up to. The renderer applies it with
`FileTree::add_summary`, which walks the full path adding size and count at every level and places
only the subfolder entries.

**The first version of that cost 0.52–0.55s on the rendering thread — the whole scan.** The
estimate had been ~0.13s, from the "resolve-parent" phase measured in the single-threaded build,
and it was wrong by four times. Instrumented under `tmux`:

```
dispatcher: built=0.521-0.545s  (0.417s in the benchmark)  blocked-on-main=0.207-0.238s
main:       385,519 summaries applied in 0.520-0.554s;  6 loading renders in 0.001s
```

The rendering thread was saturated applying the outline, so the bounded instruction channel
filled, the dispatcher spent ~0.22s blocked sending into it, and — because the dispatcher is the
walk's single consumer — the walk itself ran 0.1s slower than in the benchmark. App: 0.55–0.62s.
Rendering, for the record, was free.

**Wrong turn one: make each summary cheaper.** The path walk did an `insert_if_absent` and then a
`get_mut` at every level — two scans of the same folder — and then walked the path again to add
the file count. `Contents::folder_or_insert` does the find-or-create in one scan and the count
rides along. Worth having: the four real builders resolve parents through the same call, and the
single-threaded `pipeline` benchmark fell from 0.646–0.677s to 0.616–0.619s. But measured again,
the outline had only gone from 0.53s to **0.47s — still the whole scan**, dispatcher still blocked
0.15–0.17s, app 0.52–0.55s. A 12% cut in a cost that needed to fall ten times.

**Wrong turn two: index the wide folders.** Every path walks through `home/angch` (61 entries) and
`project` (39), scanned linearly below `INDEX_ABOVE = 128`; indexing them looked like the answer.
Lowering the threshold made everything *worse* — 16: 0.671s, 32: 0.637s, 64: 0.622s against
0.617s at 128 — and the constant went back. That is the **fourth** time this session a lookup
structure has lost to a linear scan over the same data, now including a 61-entry folder on the
hottest path there is. The rule stands without exceptions so far.

**What worked: do less, not the same thing faster.** The view can only show folders near the top
of the tree, yet the outline was O(directories). `Outline` now stops at `DEFAULT_DEPTH = 6`:
directories above it go through as they are, and everything deeper is rolled up — bytes, entry
count, read failures — into the *frontier* folder just below the cap that contains it. Every
folder the view can show keeps exact running totals; what lies beneath the frontier is unknown
until the finished tree arrives. `model::tests::outline::a_capped_outline_keeps_the_totals_it_shows`
asserts the roll-up conserves size and count folder by folder above the frontier.

```
dispatcher: built=0.465-0.474s  blocked-on-main=0.008-0.011s
main:       ~44,000 summaries applied in 0.039-0.047s
```

385k summaries became 44k, the rendering thread went from saturated to ~9% busy, the back-pressure
vanished, and the walk runs at benchmark speed inside the app. **App: 0.46–0.47s.**

On completion the finished tree is swapped in (`App::finish_scan`) and the user stays where they
were: `adopt_navigation_from` carries the current folder over, which is always valid because
every outline folder came from a real directory group. The outline is leaked, deliberately, the way
the main tree already was.

Observed under `tmux` on `/data`:

```
t=0.20s   Scanning: 662.9G (623867 files)     home/ (+623865 descendants)   630.9G (95%)
t=3.3s    Total: 785.4G (4232570 files), freed: 0 (failed to read 12 files) | /data
```

Bytes lead entries in the live counter — 85% of the bytes at 15% of the entries — because a
handful of 90 GB files dominate this volume and the directory-parallel walk reaches them early.
That is the filesystem, not a bug. Quitting mid-scan takes 0.32s by the same crude `tmux` clock
that gave the previous app 0.51s.

### What it costs

Like for like, the running app on `/data`, `/usr/bin/time` under `tmux`:

| | previous app | this app |
| --- | --- | --- |
| launch to `Total:` | 0.66–0.68s | **0.46–0.47s** |
| peak RSS | 603 MB | **581 MB** |

Speed was the stated priority, and it turned out not to cost memory after all: the build path
alone peaks at 558 MB, the capped outline adds about 23 MB on top, and the result is still below
the previous app. The uncapped outline had cost 662 MB — the depth cap is where the other 80 MB
went.

Four consequences to know about rather than fix:

- **Below the frontier — seven levels down — folders are empty until the scan completes.** Enter
  one mid-scan and it says so. Their sizes are correct in the folder above; their contents arrive
  with the finished tree. On a half-second scan nobody notices; on a minutes-long cold scan of a
  slow disk it is the trade that bought the speed.
- **The live total overshoots and then drops.** The outline counts shared blocks in full; the
  finished tree charges them once. On this volume the live figure passes 814 GB on its way to a
  final 785.4 GB, and the correction lands at the swap. Same cause, same swap.
- **"(N files)" during the scan counts what has been outlined so far**, and the treemap of a
  folder that holds only files is blank until completion. Both are honest — the scan is not done —
  but they look different from the previous app, which showed files as they arrived.
- **A builder panic takes the scan down.** `build_tree` joins its builders and propagates a panic;
  nothing in a builder should panic, and a `FileTree` that panics on one thread would have
  panicked on the rendering thread before, so this is not a regression — but the failure now
  surfaces from `hd_scanner` rather than `main`.


## Windows: a native walker, and hard links by file id (2026-09-23)

The first Windows build ran the `dua-core` walk, and a 609k-entry `D:\` took 34s. WizTree does it
in 1.3s by reading the NTFS master file table, which needs administrator rights. This section is
about getting close to that without them.

### Test machine

Windows 11 25H2 (build 26200), 12 logical cores, NTFS. `D:\`: 609k entries, 1.6 TiB, no hard links.
`C:\`: 2.06M entries, 314 GiB, with 155k hard-linked files. Warm cache throughout. WizTree's
figures, for comparison: 1.3s for `D:\`, 10.72s for `C:\`.

### Results

All times are `--bench-stage sharded`, the app's path.

| | `D:\` | `C:\` | `C:\` total vs exact | `C:\` peak memory |
| --- | ---: | ---: | ---: | ---: |
| `dua-core` walk, link count per file | 40.0s | — | — | — |
| native walker, link count probed ≥ 64 KiB | 2.8s | — | — | — |
| native walker, no hard-link handling | 0.42s | 7.6s | +17.5 GB | 282 MB |
| native walker, hot spots tracked by id (default) | 0.42s | 7.9s | ≈ +240 MB | 396 MB |
| native walker, every file tracked by id | 0.53s | 8.2s | exact | 508 MB |

`sharded` and `pipeline` agree to the byte on `D:\`. On `C:\` they differ by a few KB between runs,
as do two runs of the same stage: something is always writing to the system volume.

### What the walker does

`scan/windows.rs` has the Linux walker's thread model: one queue behind a mutex, work stealing,
batches to the consumer. Each directory is one `CreateFileW` and a few calls to
`GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`. Each call fills a 64 KiB buffer with
entries carrying end-of-file, allocation size, attributes, reparse tag and 128-bit file id. The
`dua-core` walk went through `std::fs::DirEntry::metadata`, which has neither allocation size nor
file id, so both cost opening the file.

Filesystems that do not answer the id class (FAT, exFAT, some network shares) fall back to
`FileFullDirectoryInfo`, with no ids. Junctions, symbolic links and folder mount points (reparse
tags `MOUNT_POINT` and `SYMLINK`) are not followed. Every other reparse point is descended into,
because OneDrive placeholders and dedup files are real data carrying a tag.

Eight workers is the cap, as before: on `D:\`, 4 took 3.9s, 8 took 2.8s, 16 took 4.0s and 24 took
4.2s (timed while link probing was still on). Since 2026-09-24 the cap is two thirds of the cores,
at most 12, which is still 8 on this machine; see *Where `C:\`'s six seconds go* below.

### Hard links: what did not work

No Windows directory listing carries a link count. The first version asked for it the only way
Windows offers, by opening the file (`FILE_STANDARD_INFO.NumberOfLinks`), for NTFS files of 64 KiB
and more. On `D:\` that was 200k files and 2.4s of the 2.8s scan: about 100µs of thread time per
file. Opening a file is expensive on Windows, probably because antivirus filter drivers sit on
that path.

`GetFileInformationByName` (Windows 11 24H2) returns the link count from a path without opening
a handle, and measured no faster (2.96–3.08s against 2.72–2.85s). It was removed.

Probing only in known locations helped but did not get under WizTree. On `C:\`, probing every
non-empty file there took 19.1s; files over 4 KiB, 11.4s; files of 64 KiB and more, 8.9s at 1 GB
over.

### Hard links: what did

The ledger (`model/files/hard_links.rs`) charges a file's first name along its whole path, exactly
as it charges an ordinary file, and only a later name with the same identity is reduced. So a
file sent as "may be shared" that turns out to have one name gets exactly the sizes it would have
had anyway. The link count is not needed: the file id the listing already returns is enough.
`EntryMeta::links = LINKS_UNKNOWN` (`u64::MAX`) marks such files, and it already satisfies the
`links > 1` test the model uses, so the model's charging did not change.

The price is one ledger entry per tracked file (about 100 bytes) and a longer single-threaded
replay after the parallel build: 0.24–0.32s on `C:\` for 542k tracked files. That replay is why
`sharded` is about 0.3s behind `pipeline` on Windows, where the walk is the floor and one builder
keeps up with it.

Only NTFS and ReFS volumes are tracked. A third-party filesystem driver that reported one id for
every file would otherwise have its equal-sized files merged, which is an undercount.

`LinkedFile` gained a `linked` flag, so the hard-linked count only includes files seen under a
second name. Without it, every tracked file would be reported as hard-linked.

### Where the hot spots came from

The default tracks files only in `%SystemRoot%`, below directories named `node_modules`,
`.pnpm-store`, `pnpm`, `uv`, `.venv` and `site-packages`, and in a few fixed installation paths.
The list came from measurement, not guesswork: every file on `C:\` was asked for its link count and
each hard-linked path was logged. Of the 155,547 hard-linked files (28.5 GiB), `%SystemRoot%` plus
the directory names covered 146,593 (21.9 GiB). Most of the rest was Microsoft software linking
between its own versions: Edge, WebView2 and Copilot each link into `EdgeCore` under
`Program Files (x86)\Microsoft` (4.7 GiB). Docker's CLI plugins (1.3 GiB), Git's `git-core`,
Reference Assemblies and Defender's definitions made up most of what was left. Those are matched
as whole paths, since `Microsoft` as a name would take in all of `AppData`. After that, about
240 MB of links on that machine go untracked: Cargo `target` directories, `AppData\Local\Packages`,
the Dart pub cache. Tracking those would cost more memory than they are worth.

### The key-release bug

Windows consoles report a key release as its own event; Unix terminals report presses only. Every
handler acted on both, so `q` opened the quit prompt on the way down and answered it on the way up.
`TerminalEvents` now drops releases before anything sees them.

### Why `C:\` came to 315 GiB against WizTree's 424 GB

WizTree's 424.4 "GB" is binary: it matches `C:`'s used-space counter, 455,733,346,304 B =
424.43 GiB, to the byte. That counter covers every allocated cluster, so the gap is space an
unelevated walk cannot reach, not space counted wrongly. A diagnostic pass, since reverted, compared
what the directory listings said with what each file said:

- **Stale directory sizes: 0.51 GiB.** NTFS keeps a copy of each file's size in its directory
  entry and updates it lazily, so files open for writing (logs, SQLite write-ahead logs) can show
  less than they hold. Across 2M entries the difference came to 2,885 files and 0.51 GiB.
- **`hiberfil.sys` (15.7 GiB), `pagefile.sys` (4.75 GiB), `swapfile.sys`:** cannot be opened
  even to query, but their listed sizes are current and already counted.
- **192 unreadable directories.** Among them `System Volume Information` (shadow copies),
  `Program Files\WindowsApps`, other users' profiles, parts of `ProgramData\Microsoft`,
  `Windows\Temp`, `Prefetch`, `Recovery`. None of them can be sized without elevation.
- **NTFS metadata files** (`$MFT`, `$LogFile`, `$UsnJrnl`, `$Secure`): never listed in any
  directory, so no walk counts them. Only reading the master file table does.
- **Hard links** do not explain any of it: the used-space counter counts each cluster once, as the
  deduplicated total does.

Two changes came of it. Elevated, the walker now enables `SeBackupPrivilege`. An administrator's
token holds it but disabled, and until it is enabled `FILE_FLAG_BACKUP_SEMANTICS`, which the walker
already passed, does not bypass ACLs. `System Volume Information` refuses administrators without it.
And a whole-volume scan now reports the volume's used space and the part of it the scan did not
find, in the title bar and in the benchmark header, so the gap is on screen rather than something
to reverse-engineer.

Run elevated, on the same `C:\` (`--bench-stage sharded`):

| | entries | unreadable | total | volume used | outside the scan | time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| unelevated | 2,058,004 | 192 | 315.8 GiB | 424.4 GiB | 108.6 GiB | 8.9s |
| elevated | 2,456,499 | 0 | 422.9 GiB | 424.6 GiB | 1.7 GiB | 8.8s |

The elevated scan is faster than WizTree's 10.72s on the same volume while walking 400k more
entries than the unelevated one.

### Where the last 1.7 GiB went

The 1.7 GiB left looked like NTFS's metadata files, but `$MFT` alone turned out to be 2.68 GiB,
more than the whole gap. So something was also being counted twice. A second temporary diagnostic
run, elevated, compared every file's directory entry with the file itself, opened each metadata
file it could, and asked `FSCTL_GET_NTFS_VOLUME_DATA` and `fsutil` for the rest. Tracking every
hard link exactly (`--hard-link-threshold 1`) widens the gap to 4.15 GiB, and that is explained to
within 0.3 GiB:

| | GiB | how it was measured |
| --- | ---: | --- |
| `$MFT` | 2.679 | opened with the backup privilege; matches `fsutil`'s *Mft Valid Data Length* |
| stale directory entries | 1.170 | directory entry against the file's own allocation. 0.47 GiB of it is one Delivery Optimization download in progress |
| directory indexes | 0.682 | `FILE_STANDARD_INFO` on 452k directory handles; the walk counts directories as 0 |
| other metadata: `$LogFile`, `$Bitmap`, `$Secure`, `$UsnJrnl`… | ≈ 0.15 | estimated: `$Bitmap` is clusters/8, the journal's maximum is 32 MB |
| reserved clusters | 0.034 | `fsutil`: 8,910 |
| `$UpCase`, `$AttrDef`, `$MFTMirr`, `$Deleted`, alternate streams | 0.002 | |
| compressed and sparse files | −0.266 | `GetCompressedFileSizeW` below the allocation counted |
| **explained** | **4.45** | |
| **gap, with exact hard links** | **4.15** | the remainder is drift over the minutes between runs |

The double counting was hard links: 3.09 GiB of them, in 10,041 files (79,288 hard-linked files
with every file tracked, against 69,247 by default). An elevated scan reaches places the hot-spot
list, built from an unelevated run, does not cover. Nothing else was counted twice. Reserved storage
holds nothing back, since both reserves are over their guarantee, and CompactOS is not in use
(alternate streams total 2 MB).

The shadow copies, `hiberfil.sys` and `pagefile.sys` are all counted correctly once the scan can
read `System Volume Information`.

### NTFS's metadata files in the tree

Elevated, a whole-volume scan now shows the metadata files under their own names at the volume's
root: `$MFT`, `$LogFile`, `$Bitmap`, `$Secure` and the rest, and a `$Extend` folder holding
`$UsnJrnl`, `$ObjId`, `$Quota`, `$Reparse` and `$RmMetadata`. Several of them refuse to open by name
even with the backup privilege (`$LogFile`, `$Bitmap`, `$Boot`, `$BadClus`), and `$Secure` is not
found by name at all. But every one has an MFT record, and `FSCTL_GET_NTFS_FILE_RECORD` on the
volume handle returns it to an administrator. `scan/ntfs.rs` counts the clusters a record's
non-resident attributes occupy, from their run lists: every piece of every attribute, holes
skipped, with attribute lists followed into extension records.

The first version read each attribute's allocated-size field instead, trusting the sparse flag to
mark the exceptions. It reported 883.1 GiB for a 424.6 GiB volume. `$BadClus:$Bad` is one hole the
size of the volume, it claims all of it as allocated, and it carries no sparse flag. Run lists say
which ranges have clusters behind them, so they get holes, sparse streams and compression right
whatever the flags say. As a guard against any other misreading, a metadata file larger than the
volume is dropped rather than counted.

Resident attributes are left out, since they sit inside `$MFT`'s own clusters. The parser is
platform-independent and tested on hand-built records, `$BadClus`'s shape among them.

`$Extend`'s children have no fixed record numbers, so `$Extend` is listed, which works elevated,
and each child is sized from the reference its entry carries. The names are reserved at a volume's
root, so they cannot collide with anything the walk finds, and the app refuses to offer them for
deletion. Unelevated, the volume cannot be opened for reading and nothing is added.

What this does not cover: directory indexes (0.68 GiB here) and stale directory entries (1.17 GiB)
are still outside the scan. Counting either would cost a call per directory or per file.

The result, elevated, against 424.7 GiB in use:

| | total | hard-linked | vs used | time |
| --- | ---: | ---: | ---: | ---: |
| hot spots tracked | 426.4 GiB | 69,247 | +1.7 GiB | 8.5s |
| every file tracked (`--hard-link-threshold 1`) | 423.3 GiB | 79,288 | −1.4 GiB | 8.8s |

The metadata files added 2.84 GiB to both runs (3,049,046,320 B with every file tracked), in line
with `$MFT`'s 2.68 GiB plus the smaller ones. It was the hard links that pushed the
default past the volume. The hot-spot list was drawn from an unelevated scan, and elevated the
walk reaches other users' profiles, `WindowsApps` and `System Volume Information`, where 10,041
more hard-linked files live. So an elevated scan now tracks every file unless told otherwise, for
0.3s and 0.5s more replay here. The 1.4 GiB left is directory indexes and stale entries, less what
compression saves: what the accounting above predicted.

### Known gaps on Windows

- CI builds and tests on Linux only. The Windows walker has been run on one machine. The Linux and
  macOS builds were checked with `cargo clippy --target` from Windows, not built or run there.
- Folder mount points are never followed, so a volume mounted in a folder is not scanned even
  without `-x`, unlike on Unix.
- The id-class fallback is decided once per scan: if one NTFS directory answered
  `FileIdExtdDirectoryInfo` with `ERROR_INVALID_PARAMETER`, the rest of the scan would run without
  ids, and so without hard-link tracking.
- ReFS 128-bit ids are folded into 64 bits by xor and rotation. That is a bijection on NTFS, where
  the high half is zero, and collision-free on ReFS while the low half stays under 2^32.
- The `dua-*` benchmark stages on Windows still open every file for its link count, so they
  overstate what `dua-core` itself costs there.

### The shard count, and why Windows uses one (2026-09-23)

`parallel::build_tree` shards the tree build across `SHARDS` threads, defers every shared-block
sighting, then merges the partial trees and replays the deferrals to settle hard links that might
span shards. That is the right trade when the walk is faster than one builder — Linux and macOS,
where four builders hide behind a 24-thread walk. Windows is the opposite: a handle per directory
makes the walk the bottleneck, so one builder already keeps pace and the extra shards only add a
serial tail after the walk.

Measured on `D:\` (3.0 TiB, 414,583 entries, warm), the `walk+build` phase is flat across one, two,
and four shards at ~0.14s — the build is entirely hidden either way — while `merge` grows ~0.5 ms
per shard and `replay` is a shard-independent ~7–9 ms. That serial tail was the visible part of the
gap between the `sharded` stage (~0.155s) and `pipeline` (~0.145s).

The fix came in three parts, and the third only surfaced after the first two. `build_tree` now
builds a **non-deferring** tree when `shards == 1`: a lone builder sees the whole tree, so no hard
link can cross a shard and it charges inline exactly as the single-threaded `scan_into_tree` does —
no deferral, no merge, no replay. And `SHARDS` is now `1` on Windows (`4` elsewhere). With the tail
gone the phase line read `merge 0.000s  replay 0.000s`, yet one-shard `sharded` was still ~10–15 ms
behind `pipeline` in `walk+build` alone. The remaining cost was `shard_of`: an FNV hash over each
directory's path components, run per directory on the walker thread — the bottleneck thread — only
to land on shard zero every time. The loop now skips it when `shards == 1`. After that the two
stages interleave within noise (sharded and pipeline each win about half the rounds, medians ~6 ms
apart, inside a ±10 ms spread), and the only thing `sharded` still does that `pipeline` does not is
run the live-outline progress callback — real app work, not overhead.

`single_shard_build_matches_the_single_threaded_tree` pins the one-shard path to the reference tree
over a fixture that holds a hard link; skipping the hash is behaviour-preserving because `hash % 1`
is always zero.

The app's real load path is `parallel::build_tree` (the `sharded` stage), not `pipeline`, despite
an earlier benchmark comment that said otherwise — now corrected.

### Where `C:\`'s six seconds go, and what did not help (2026-09-24)

A second machine: Windows 11, 32 logical cores, Defender real-time protection on, unelevated.
`C:\` is 2.34M entries in 436k directories, 1.2 TiB; `D:\` is 415k entries in 25.5k directories.
`walk` and `sharded` take the same time here, so the tree build and the hard-link ledger are
already hidden behind the walk, and nothing after the walk is worth moving to a second pass the way
the Linux small-file probe was (`refine`). The walk does no per-file work at all; its cost is per
directory. A temporary timer around each call, single-threaded, per directory:

| | `C:\` | `D:\` |
| --- | ---: | ---: |
| `CreateFileW` | 34µs | 12µs |
| two listing calls (the second only says there are no more) | 16µs | 8µs |
| parsing the entries | 3µs | 4µs |
| `CloseHandle` | 12µs | 5.5µs |

Same code, same filesystem, and the system volume costs three times as much per handle: the filter
drivers attached to it (Defender among them) run on every open and close. Charged to each folder
at depth three, the cost was a uniform 45–60µs a directory everywhere, `Users` and `WinSxS` alike,
so there is no subtree to special-case. And it does not scale: at 12 workers the same work took 72
thread-seconds against 29 on one, and throughput levels off at about 75–80k directories a second.

What was tried, interleaved against an unchanged build because two runs of the same binary differ
by up to half a second:

- **Opening directories by file id** (`OpenFileById` with the id from the parent's listing), to
  skip resolving the whole path from the root: 18.2 against 14.9 thread-seconds of opens
  single-threaded. Slower. Path lookup was never the cost.
- **Not closing handles on the walker threads**: with handles leaked outright, as an upper bound,
  open times rose to take the place of the closes and the walk took as long.
- **Skipping the listing call that only says there are no more entries.** NTFS fills the buffer
  while entries remain, and on all 461,996 directories of both volumes, no listing that left room
  for the longest possible entry had more to give. Half of all calls, and about 4µs each, but at
  12 workers the walk was *slower* without them in six rounds of six (5.98s against 5.80s), and
  faster only at 8 (5.90s against 6.18s). Not kept: it is a gain only below the best thread count.
- **More workers.** The cap of 8 was set on a 12-thread machine, where 8 beat 4 and 16. On this
  one, over four interleaved rounds, 8 took 6.18s and 12 took 5.56s; 16 took 6.9s and 32 took
  7.3s. So the cap is now two thirds of the cores, at most 12: 8 there, 12 here. The app's path
  (`sharded`, no `--threads`) went from 6.26s to 5.87s over five interleaved rounds, with identical
  entry counts. `D:\` gained too, 0.225s to 0.180s over six rounds, so the data volume, where
  opens are three times cheaper, does not want fewer workers than the system volume.

Unelevated, that is the floor: every directory has to be opened to be listed, and on a system
volume the open is the filter drivers' price, not the walker's. What would go below it is not
opening directories at all — reading the master file table, which only an administrator can do.

## A static Linux release: musl, and the allocator it needs (2026-09-23)

The release binary is `x86_64-unknown-linux-musl`, fully static, so it runs on any x86_64 Linux
regardless of glibc version (checked on CentOS 7's glibc 2.17, Alpine, and a bare busybox image;
the dynamic glibc build fails on all three). Nothing in the tree compiles C, so a plain musl build
needs no C toolchain. The only source change was `ioctl`'s request type, which is `c_ulong` on
glibc and `c_int` on musl (`libc::Ioctl`).

A plain musl build is 7x slower, though. Measured on the 4.2M-entry home directory (Ryzen 9 9950X,
32 threads, warm cache), `--bench-stage sharded`, with totals identical in every row:

| build | sharded | user | sys |
| --- | --- | --- | --- |
| glibc, dynamic | 0.46s | 3.4s | 17s |
| glibc, `+crt-static` | 0.47s | 3.4s | 17s |
| musl, its own malloc | 3.4s | 11.5s | 72s |
| musl + mimalloc | 1.0-1.7s | 4.0s | 35s |
| glibc + mimalloc | 1.0-1.1s | 3.6s | 38s |
| musl + jemalloc | 0.45-0.50s | 3.7s | 17s |

- **Static linking costs nothing.** Static glibc matches dynamic glibc.
- **musl's allocator stops the walk scaling.** Syscall counts are identical, because rustix issues
  them raw on both. With musl's malloc, `--threads` 1/4/12/24 gives 6.1/3.1/3.5/3.5s against
  glibc's 5.2/1.4/0.61/0.45s: 18% slower on one thread, and no gain past four.
- **The allocator is the entire gap, and it shows up as kernel time.** Everything else in musl
  costs nothing: with jemalloc it matches glibc. glibc plus mimalloc is as slow as musl plus
  mimalloc. This agrees with [the earlier mimalloc
  experiment](#an-allocator-experiment-that-failed-usefully). Tuning mimalloc (`PURGE_DELAY=-1`,
  eager commit, large pages) changed nothing. jemalloc matches glibc on time and RSS.

So musl builds use jemalloc (`tikv-jemallocator`, 64-bit only, the same choice ripgrep makes), and
glibc builds keep the system allocator. jemalloc is C, so building the release needs `musl-gcc`
(`musl-tools`; `make static`). With zig as the C compiler instead, cc-rs's `--target=` has to be
filtered out, and debug builds need `-fno-sanitize=undefined`: zig's default UBSan traps inside
jemalloc and the tests die with SIGILL.

aarch64 is cross-built with `cargo zigbuild` (which handles both of those itself). jemalloc fixes
its page size at build time, and aarch64 kernels run 4K, 16K (Asahi) or 64K pages (some RHEL). A
4K build aborts on the larger two, so the release sets `JEMALLOC_SYS_WITH_LG_PAGE=16`. The binary
and all tests were run under `qemu-aarch64`, which uses the host's 4K pages. The 64K setting has
not been run on a 16K or 64K kernel.

## macOS, re-benchmarked: the walk is I/O-bound, and the ledger was quadratic (2026-09-23)

The prompt was a whole-disk scan of `/` that had crept from ~36s to ~37s, and the expectation that
the sharded build ought to be buying something on macOS. It was not, and could not: on this machine
the tree build is not the bottleneck. What it *was* buying was a serial tail, which a hard-link
ledger bug had grown to over a second.

### Result

`/`, the app's path (`--bench-stage sharded`, default flags), old and new binaries interleaved,
three rounds:

| | old: 4 shards, 8 workers | new: 1 shard, 6 workers |
| --- | --- | --- |
| scan, as the app waits for it | 41.6 / 41.8 / 42.0s | **39.5 / 40.3 / 39.5s** |
| of which replay | 1.16–1.18s | — |
| system time | 211s | **131–138s** |
| user time | 9.5s | 7.1s |

About **2s (5%) faster, with 37% less kernel CPU**. Totals agree between the binaries to within
what the machine wrote between paired runs; on two sealed, read-only volumes, where nothing
changes, `pipeline` and `sharded` from both binaries agree to the byte (below).

The 36s→37s drift was the disk, not the code: the scan now visits 11.2M entries against the 10.4M
recorded at the top of this file, at the same ~285–290k entries/s.

### The walk is the whole scan

```
walk         36.750s   10524788 entries      5.84 user   183.87 sys
pipeline     36.597s   10524801 entries      7.19 user   184.33 sys
sharded      37.601s   10525101 entries      7.80 user   187.73 sys   (merge 0.009s, replay 0.301s)
```

`pipeline` — one builder — is the walk to within noise, so there is nothing for more builders to
hide behind. One builder keeps pace with ~6.5M entries/s on Linux; the macOS walk peaks around
0.35M/s. This is the Windows situation from the previous section, for a different reason.

What the walk is doing, from `xctrace` (Time Profiler, user-space stacks only — kernel frames need
root) on a 12-worker scan: **62% of samples in `open`, 33% in `getattrlistbulk`**, everything else
under 1%. And from `iostat` during a "warm" scan of `/`:

```
    KB/t  tps  MB/s
    4.25 36412 151.19
    4.27 32228 134.39
    4.01 37338 146.33
```

The scan is **not warm**. It reads 30–40k 4 KiB metadata blocks a second from the SSD for its
whole length; APFS's metadata for 11M entries does not stay cached between runs. A single worker
on a smaller tree runs at 12.4s wall-clock on 4.7s of CPU — mostly blocked. So the walk is bound
by synchronous 4 KiB reads at a queue depth of about the worker count, and every worker past six
buys queue depth at the price of kernel contention:

| workers | `walk` | system time |
| --- | --- | --- |
| 4 | 42.5s | 78s |
| **6** | **35.3s** | **112s** |
| 8 | 36.4s | 183s |
| 10 | 40.5s | 286s |
| 12 | 45.1s | 401s |

Interleaved on the app's path, six beat eight in every round (39.1–39.8s against 40.5–41.7s).
`MAX_SCAN_THREADS` is now 6 on macOS; Windows keeps 8 (two thirds of the cores, at most 12,
since 2026-09-24), Linux 24. It is a measurement on one
14-core M4 Pro and an NVMe SSD, not a law — a slower disk or a different core count may move it.

The vnode cache is small next to the tree: `kern.maxvnodes` is 263,168 and one scan of `/` creates
and recycles **1.7M vnodes**, one per directory opened. That looked like the contention, but a
tree that fits the cache (`/Applications`, 124k directories, second run: 4 new vnodes) shows the
same cliff — 3.05s at six workers, 3.30s at twelve with three times the system time. Raising
`kern.maxvnodes` (root, system-wide) might still help repeat scans of `/`; unmeasured.

### What did not work: opening directories relative to their parent

Every directory is opened by full path, so every worker resolves `/System/Volumes/Data/Users/…`
from the root again for every directory, which looked like a way to contend on a few vnodes. A
prototype kept each directory open until its subdirectories were opened with `openat(parent, name)`
(bounded by an fd budget). No change: 37.0s at six workers against 35.3s, and system time
identical at every worker count. Path lookup is cheap next to what `open` does on a directory whose
inode is not cached — read it. Reverted.

### The ledger was quadratic in a file's link count

With the walk as the floor, the only thing `sharded` added was its tail, and the replay had grown
from 0.30s to **1.15–1.19s** over the course of the day. Instrumented:

```
replay: 105,164 directories, 218,462 sightings, 164,214 subtractions
        ledger 0.972s   interning 0.058s   subtraction 0.099s
        54,231 files, three of them in 10,572 / 5,799 / 4,079 folders
```

`HardLinks::charge_in` answered "the deepest folder already charged" by comparing the new link's
folder with *every folder already holding a link to that file*, an `O(depth)` walk each — so a file
linked from N folders cost O(N² · depth) to build. Those three files were ~97% of the sum of
squares. They are real: the sealed system volume dedupes identical `_CodeSignature/CodeResources`
files into one inode with 5,799 links, and an iOS simulator runtime mounted under
`/Library/Developer/CoreSimulator/Volumes/` added ~33k more hard-linked files — which is also why
the hard-linked count went from 21k to 54k.

The fix answers the same question another way. A folder is already charged exactly when it is an
ancestor of some earlier link, so the answer is the first charged folder found walking up from the
new one. Past `INDEXED_AT` (32) folders a file keeps that set — every link folder and all of its
ancestors — and a charge walks up inserting until it finds one already there: O(depth), and O(1)
amortised for the insertions. Below the threshold the list stays, as it is smaller and no slower.

| | replay, old | replay, new |
| --- | --- | --- |
| `/` | 1.16s | 0.18s |
| simulator runtime volume (728k entries, 32,809 hard-linked) | 0.777s | 0.108s |
| `/System/Library` (443k entries) | 0.217s | 0.012s |

On both sealed volumes old and new, `pipeline` and `sharded`, report the same total to the byte
(23,201,353,728 B and 28,655,095,808 B). The inline build pays the same cost, hidden behind the
walk; `sharded` paid it after. `a_file_linked_from_many_folders_matches_the_reference` checks the
set against the component-wise reference well past the switch.

### One shard on macOS

With the ledger fixed the tail is ~0.2s on `/`, below the run-to-run noise there, so the choice was
made on the simulator volume, which is sealed and repeats to ±0.1s:

| | `walk+build` | replay | total |
| --- | --- | --- | --- |
| 1 shard, four rounds | 4.16–4.32s | — | 4.16–4.32s |
| 4 shards, four rounds | 4.21–4.32s | 0.109–0.111s | 4.32–4.43s |

One shard won every round, by the replay it skips. `SHARDS` is now 1 on macOS as on Windows. Four
builders stay on Linux, where the build really is the bottleneck.

### Reproduce

```sh
./target/release/diskonaut --benchmark --bench-stage sharded --bench-shards 1 --threads 6 /
./target/release/diskonaut --benchmark --bench-stage sharded --bench-shards 4 --threads 8 /
iostat -d -w 5 disk0                                   # alongside: is the "warm" scan reading?
xcrun xctrace record --template 'Time Profiler' --launch -- \
  ./target/release/diskonaut --benchmark --bench-stage walk --threads 12 /Users
sysctl kern.maxvnodes vfs.vnstats.num_newvnode_calls   # before and after, for vnode churn
```

Spotlight (`mds_stores`) indexing new files moves whole-disk numbers by 2–3s; check `top` before
trusting a round.

## Known gaps

> The two Linux sections above that end "the model is the bottleneck" are superseded: the model is
> built on four threads and hidden behind the walk. On Linux the scan is now bound by the walk,
> which is bound by one `statx` per entry in the kernel.

- The reported total for `/` is ~706 GiB against 884 GiB used. The difference is APFS snapshots,
  purgeable space, and the deliberately skipped auxiliary volumes. Whether that is the right
  definition of "the disk" is a product decision, not a settled one.
- ~470 entries under `/` are unreadable without Full Disk Access. Granting it to the terminal will
  change the total.
- Peak RSS for a whole-disk scan is ~3.5 GB, roughly 340 bytes per entry. `Folder` stores a
  `HashMap<OsString, FileOrFolder>` per directory and every `File` pays the size of the larger
  `Folder` variant. An arena or an interned-name representation would cut this substantially, and
  the allocator pressure may be costing time as well — unmeasured.


## Cold cache (2026-09-24)

Every number above this section was taken warm. This one was not: `/proc/sys/vm/drop_caches`
before every run, `hyperfine` over `diskus` 0.9.0 and `--benchmark --bench-stage sharded`, on an
8-core VM with a virtio SSD (`docs/probes/bench-diskus.sh` does exactly this, warm and cold; the
cache drop needs one sudoers line, `NOPASSWD: /usr/bin/tee /proc/sys/vm/drop_caches`). Two ext4
trees: `project`, 375k entries in 29.7k directories, and `home`, 2.2M entries with 171k hard links.

### What was found

Warm, `diskus` and diskonaut were within noise on the small tree and 15% apart on the big one:
that gap is the tree build, which `diskus` does not do. Cold, `diskus` was **1.6x faster on
both**, 0.74s to 1.22s and 5.0s to 8.1s, and the block layer says why. Every run reads the same
34k requests and the same 228 MiB — the access pattern was identical — but `diskus` kept 7.5 reads
in flight and diskonaut 3.3. `diskus` gives rayon three threads a core and stats every entry of a
directory as its own task; diskonaut gave the walk one worker a core, and one worker stats one
directory's entries one after another.

The 34k reads are the floor, not waste: a names-only walk (`statbench` mode 0) already issues
33k of them, since opening a directory reads its inode synchronously and `getdents64` its blocks,
one round trip each, and ext4's inode readahead (32 blocks, 512 inodes) then brings the files'
inodes in almost for nothing — the `statx` pass adds 1k reads and 40 MiB. Bytes are near the
minimum too: 92 MiB of inode table for 375k inodes at 256 bytes plus one 4 KiB block per
directory is about 210 MiB. So nothing in userspace can make the walk *read less*; what it can do
is keep more of those reads in flight, and every directory is a dependency chain (inode, then
blocks, then children's inodes), so in-flight depth is the number of directories being worked on
at once, which is the number of workers blocked in the kernel.

The device serves about 70k random 4 KiB reads a second from queue depth 8 up (`O_DIRECT`
`preadv` from threads, the same file). Every configuration below that reached 24 or more workers
sits at 47–49k reads a second with the same 34k reads — the floor for this pattern on this device,
with merging — and `diskus` is at the same floor.

### What was tried

Cold, `project`, 3–4 runs each (the two-phase read is what the walker does now):

| walker | 8 workers | 16 | 24 | 48 |
| --- | --- | --- | --- | --- |
| before | 1.22s | | 0.92s | |
| stats in inode order | 1.12s | | | |
| big directories' stats shared over helpers | 1.00s | | | |
| both | 1.01s | 0.79s | 0.74s | 0.70s |
| `diskus` | 1.17s (`-j 8`) | | 0.74s | 0.75s |

- **Inode order.** `getdents64` returns hash order; sorting a directory by `d_ino` before the
  stats made the inode table reads forward, 8% on this SSD. It changes nothing warm (the listing
  is copied into an arena and sorted, a few percent of user time on 2.2M entries, inside noise on
  wall time) and a spinning disk has a great deal more to gain from a seek order than an SSD.
- **Sharing a big directory's stats.** `@mui/icons-material`, 43k entries in one directory,
  took 0.40s alone, one worker, while the others had run out of work; `diskus` took 0.15s.
  A directory of 2048 entries or more now has its stats split over helper threads, 1024 each, up
  to the walk's worker count. 18% cold on `project`; nothing on `home`, which has no such
  directory, and nothing warm, where the calls are microseconds.
- **Workers.** The lever that is most of it. On `home` the two changes above did nothing at 8
  workers (8.19s to 8.14s) and 24 workers took it to 5.3s, 48 to 5.1s, against `diskus` at 5.0s.
  The Linux default is now three a core, at most 32, where it was one a core, at most 24. Warm,
  that cost 9% on `project` (198 → 216 ms; the walk shares eight cores with four tree builders)
  and nothing measurable on `home` (1.87s at 8, 1.79s at 16, 1.84s at 24).
- **`io_uring` `statx`, cold.** The earlier verdict (3.8x slower, warm) was worth rechecking
  where a blocking opcode has something to hide. On the 43k-entry directory it does: 0.16s to
  0.37s serial, the same as threads give. Over the whole tree it does not — 6.5s to 6.7s, single
  threaded — because 91% of directories have fewer than sixteen entries and a per-directory batch
  is nothing to overlap. Threads across directories are what overlap; still not worth a ring.

### Where it stands

Against `diskus` 0.9.0 (24 threads), 3 runs each, after the tree build work of the same day
(next section) as well; diskonaut's `sharded` stage, which builds the whole navigable tree:

| | warm | cold | cold, as root |
| --- | --- | --- | --- |
| `project`, 375k entries: `diskus` | 226 ms | 756 ms | |
| `project`: diskonaut | 223 ms | 763 ms | 685 ms |
| `home`, 2.24M entries, 173k hard-linked: `diskus` | 1.45 s | 4.93 s | |
| `home`: diskonaut | 1.50 s | 4.97 s | 3.87 s |

Level with `diskus` warm and cold, on both trees, while building the tree it does not; as
root, with the directories' blocks read ahead through the device, 9% and 21% ahead of it. The
measured regime is one ext4 SSD in a VM; on NVMe the floor is higher and the workers matter
more, and on a spinning disk the inode order matters more. Neither was available to measure.

## The tree build (2026-09-24)

Warm, on the 8-core VM, the walk is 1.1s for 2.24M entries and the build alone 0.92s, and the
app's `sharded` stage came to 1.75–1.81s: the walk and the builders share eight cores, and after
the walk a serial replay of 0.44s. This tree has 173k hard-linked files under `~/.cache` (uv,
pnpm), so it is the ledger's worst case, which is what made the replay visible.

### Instrumentation

`--benchmark --bench-profile` now prints where the builders' time went, phase by phase, for any
stage that builds a tree: resolving each directory's folder, the pass over its entries, the
ledger, placing the entries, and the replay's two halves, with counts (folders stepped through,
name comparisons, sightings, ancestor steps). `libdiskonaut::model::files::profile` holds it;
off, it costs one predictable branch per counted event. The numbers below are its output.

`perf` is unavailable on this box (`perf_event_paranoid` is 4) and the release binary is
stripped, so for a sampling profile there is `--profile profiling` (release with symbols) and
`docs/probes/gdb-sample/`, a poor man's sampler: it runs the target with `PR_SET_PTRACER` set so
`gdb` may attach from outside despite Yama, stops it every few milliseconds and counts stacks.
Two hundred samples were enough to rank the hot spots and agree with the profile.

### What was found, in order of size

**The release profile was `opt-level = "z"`.** Set for the viewers' binary sizes and never
benchmarked for the scan. `"s"` makes the build 30% faster (0.92 → 0.66s) and the app's stage
13% (1.86 → 1.60s), for 2% more binary (1.77 → 1.81 MB); `2` and `3` are no faster than `"s"`
and 25–30% larger. Setting `opt-level = 3` on the two library crates alone did nothing: with
`lto = true` the final codegen takes the top-level profile's level. The release is `"s"` now.

**Half of the build was the ledger, and most of that was naming directories to it.** The build
profile of the single-threaded stage:

| phase | before | after |
| --- | --- | --- |
| resolve the folder (10.5 deep, ~150 name compares) | 0.12s | 0.15s |
| the pass over the entries, directories without links | 0.015s | 0.015s |
| the same with the ledger, 155k directories, 735k sightings | 0.30s | 0.19s |
| place the entries | 0.16s | 0.18s |
| **build** | **0.70s** | **0.63s** |
| replay after a sharded build | 0.44s | 0.14s |

- The ledger interned each directory by *path*: normalise into a `PathBuf`, hash it, and for a
  new one hash every ancestor's path above and box two copies of the key — 0.7 µs a directory,
  0.11s of the build, and the replay did all of it again for the merged tree, 0.13s serial.
  Now a folder carries its ledger id (`Folder::dir`, given when its path is first resolved, as
  `HardLinks::child(parent)`: a push, no hashing), the deferred sightings hold ids instead of
  paths, the merge renumbers the folders it moves in (`Folder::renumber`, with a remap the
  sightings follow), and a graft does the same for the rescanned subtree.
- The replay took sizes back by walking the tree down to each folder, by name, for every
  sighting. It now adds up what each folder owes in a `Vec` by id and takes it back in one pass
  (`Folder::take_back`): 0.012s for 558k sightings.
- `shared_depth` walked parent pointers up from both directories until they met — about 18
  dependent loads a comparison, 126 a sighting. Each interned directory now keeps its ancestor
  chain (`HardLinks::ancestors`), so it is the common prefix of two short arrays: 28 steps a
  sighting. Worth 14% of the ledger; the interning above was the rest.

**What is left**, per the profile: the ledger's remaining 0.19s is 735k hash lookups into a
map of 173k `LinkedFile`s of ~80 bytes each, a cache miss or two apiece; shrinking the entry
(box the charged set, keep the first few directories inline) would help. Resolving is ~15 name
comparisons per level — the walk emits siblings together, so a cache of the last path's
positions would skip most of them. Both are second-order now. The first-order fact on this box
is that the whole warm scan is CPU-bound at eight cores: the walk's 7s of system time (3 µs an
entry, in a VM) is what the clock measures, and the build is 0.6s of the ~10s of CPU.

### Where it stands

Warm, against the previous commit, 5 runs each:

| | before | after |
| --- | --- | --- |
| `~/.cache`, 1.05M entries, 169k hard-linked: build alone | 0.60s | 0.36s |
| the same, `sharded` (the app) | 1.12s | 0.82s |
| `/data/angch`, 2.24M entries: build alone | 0.92s | 0.63s |
| the same, `sharded` | 1.80s | 1.45s |

Totals are identical between `tree`, `pipeline` and `sharded` on both trees, the model's
folder-by-folder sharded test passes, and so do the filesystem fixtures.


## PGO, and thin against fat LTO (2026-09-24)

The release profile has `lto = true` (fat) and, since this morning, `opt-level = "s"`. Two
things were left to try on the compiler side: profile-guided optimisation on top, and whether
thin LTO would do. The profile was trained on the instrumented binary running every benchmark
stage over `project` (375k entries) and `~/.cache` (1.05M, 169k hard-linked), then merged with
`llvm-profdata`; `make pgo` does the same. Warm, `sharded`, 5 runs each:

| build | binary | `~/.cache` | `/data/angch` (2.24M) | build alone (`~/.cache`) | walk alone |
| --- | --- | --- | --- | --- | --- |
| `s` + fat LTO (the release) | 1.84 MB | 834 ms | 1.477 s | 0.376 s | 1.087 s |
| `s` + fat LTO + PGO | 1.93 MB | 824 ms | 1.440 s | 0.352 s | 1.055 s |
| `3` + fat LTO + PGO | 2.36 MB | 814 ms | 1.453 s | 0.366 s | 1.065 s |
| `s` + thin LTO | 2.20 MB | 849 ms | 1.505 s | 0.413 s | |

- **PGO is worth 2–3% of the wall clock, and 6–8% of the user CPU** (2.52 → 2.33s on the big
  tree): the tree build gets 6%, the walk 3%. Real, and not enough. The release is built in CI
  for two musl targets; a profile has to come from a training run of that very build, on some
  tree, on every release, or be committed and go stale with the next change to the model. For
  a few percent that is not worth the pipeline. `make pgo` is there for a local build.
- **`opt-level = 3` with PGO is no better than `"s"` with it**, and 22% larger — the same
  answer as without the profile.
- **Thin LTO is 2–3% slower than fat and 20% larger.** Fat stays.

The CPU that remains is where it was: the kernel's `statx` work in the walk, and the model's
cache misses in the ledger and the folder lookups (see "The tree build").


## Roadmap step 1: ext4 metadata from the device (2026-09-24)

The spike of `scan-roadmap.md` step 1, `--benchmark --bench-stage ext4-raw`, as root: read the
superblock, the group descriptor table and the used part of every group's inode table from the
block device in sequential reads, and sum every live inode's `i_blocks`. No names, no tree —
the floor a device-reading walker could reach. On `/data` (503 GiB ext4, 4096 groups, 334 with
inodes, 256-byte inodes; 2.14M live inodes, 2.75M names):

| | warm | cold | total |
| --- | --- | --- | --- |
| `ext4-raw`: 600 MiB read, 334 reads | 0.20s (0.37s the first time) | 0.44s | 463.3 GiB |
| `walk`, as root | 1.43s | | |
| `sharded`, as root | 1.77s | 4.50s | 462.9 GiB |
| `df` used | | | 463.3 GiB |

The sum agrees with `df` to within 1 MB and is 0.08% above the scan's, which is what no
directory reaches (open-but-unlinked files, and what is under mount points). So the floor is
**7x the walk warm and 10x cold**, from reading 600 MiB sequentially instead of asking the
kernel 2.75M times. The gate asks for the same on a second machine that is not a VM before
the walker is built on it; the syscall share is inflated here, so the ratio will be smaller
there, but it has a long way to fall.

What a walker adds on top of the survey: the directory blocks, one 4 KiB block per directory
at least — 370k directories here, ~1.5 GiB — read in physical order in batches and parsed for
names (`ext4_dir_entry_2`, the same in htree leaves; index blocks and checksum tails have
inode 0 and are skipped), so cold they are a sequential sweep rather than 370k dependent
reads, and warm a copy out of the device's page cache, which is the buffer cache the kernel
reads them from too. Mount points inside the tree are handed to the ordinary walker.


## Roadmap step 2: the ext4 device walker (2026-09-24)

As root on ext4, the Linux scan now reads the filesystem from its block device
(`scanners/src/ext4.rs`) instead of asking the kernel for every entry: `--no-device-read` gets
the old walk back, rescans always use it, a filesystem the reader cannot follow (META_BG, a
device that will not open) makes it decline before it has said anything and the kernel walk
takes over, and a directory of a shape it does not read (an inline directory spilling into an
xattr, a triply indirect one) is handed to the kernel walker whole, like a mount point. Every
directory still leaves as one `DirEntries`, so the tree, the ledger and the viewers see no
difference; the fixtures, which run the binary as root on loop-mounted ext4, agree with their
oracles to the byte, mounts inside the tree included (those go to the kernel walker).

### How it reads

A generation at a time, from the scan root's directory: the frontier's directory blocks are
gathered (extent trees and the classic indirect map both followed), sorted by device address,
merged into runs (blocks within eight of each other in one read, up to 8 MiB), every run
advised with `WILLNEED` **before** any is read — so the device has the whole sorted list in its
queue — then read and parsed for names on eight threads. The inodes those names point at are
fetched the same way, in one sweep per generation, only the ones not seen yet. Then the
directory batches are built on eight threads too. Nothing is read up front: a subtree scan
pays for its subtree.

Three things were measured on the way there, each a factor of two:

- The first version read every inode table up front (600 MiB, 0.35s whatever the subtree) and
  swept directory blocks one synchronous `pread` at a time. Warm it was level with the kernel
  walk and **cold it was 2x slower**: queue depth one against the kernel walk's 24 workers.
  Advising every run first turned the sweep into what it should be.
- The inode sweep found each run's index with a linear search: quadratic in runs per
  generation, thousands of them.
- Building the batches ran on the reader thread alone, 0.7s of the 1.7s; done in parallel it is
  0.03s a generation.

`--bench-profile` prints each generation: directories, blocks, runs, bytes and seconds for the
directory sweep and the inode fetch, and the emit.

### Where it stands

`docs/benchmarks/angch-noble-20260924-step2.md`, 3 runs each, `sharded`; the *kernel walk as
root* row is the same privilege without the device read, so the pair isolates it:

| tree | warm: device / kernel walk (root) | cold: device / kernel walk (root) | cold, unprivileged | cold, `diskus` |
| --- | --- | --- | --- | --- |
| `project`, 401k entries | 225 / 248 ms | **398** / 708 ms | 768 ms | 777 ms |
| `home`, 2.25M entries | 1.51 / 1.61 s | **2.25** / 3.88 s | 4.87 s | 4.92 s |
| `~/.cache`, 1.05M, 169k hard-linked | 827 / 905 ms | **1.18** / 1.96 s | 2.53 s | 2.57 s |

Cold, as root, the scan is now 1.7x the kernel walk with the same privilege and 2.2x what an
unprivileged scan or `diskus` gets; warm, 7–10% ahead. The gate asked for 3x warm on `walk`,
which the survey's floor promised and the walker does not reach: reading 2.5 GiB of directory
blocks and 1.1 GiB of inode blocks out of the page cache, parsing 2.25M names and building
the batches costs about what the kernel's 24 threads of `statx` cost on eight cores. Two things
would close the gap and are left for another day: reading only the blocks of directories that
are not already in the tree's way (the runs' merging reads twice the bytes of the blocks
wanted), and folding the batch build into the parse. Cold is where the change lives, and it
is a clean 2x there.


## Roadmap step 7: the model's cache misses (2026-09-24) — under its gate

Two changes the build profile pointed at, measured with `--bench-stage tree-only
--bench-profile`, two runs each:

| | `~/.cache` (1.05M entries, 169k hard-linked) | `home` (2.25M) |
| --- | --- | --- |
| ledger, before → after | 0.167s → 0.147s | 0.178s → 0.154s |
| resolve, before → after | 0.081s → 0.073s | 0.140s → 0.124s |
| name compares per directory | 121 → 16 | 182 → 18 |
| `tree-only` | 0.39s → 0.37s | 0.60s → 0.59s |

- **The ledger's entry is 40 bytes instead of 80**: the first three folders holding a link
  kept inline, the charged set boxed. Twelve percent of the ledger's time, from a denser map.
- **A directory's folder is resolved from the previous directory's positions**, as far as the
  two paths run together, each remembered position checked by one name comparison. Seven
  times fewer name comparisons — and only ten percent of the resolve time, which is therefore
  the walk down ten boxed folders, a cache miss each, not the comparisons. (A first version
  found the shared prefix by parsing `Path` components and was *slower*; the bytes are
  compared now.)

Five to six percent of the build on the hard-link-heavy tree, against a gate of ten. Kept,
being eighty lines that make the structures smaller and the lookups fewer, but recorded as
under the gate: what is left in `resolve` and `ledger` is pointer chasing and hash misses,
which a different layout (folders in an arena, the ledger keyed for locality) would address,
not fewer operations.


## Roadmap step 5: workers that adapt (2026-09-24) — a negative result

The idea: start the walk at one worker a core, which is right warm, and let a monitor raise the
count while the workers wait on the disk, which is what cold wants, instead of the fixed three
a core. Two signals were tried, twenty milliseconds apart each:

- **The process's CPU against its cores** (`getrusage`). Never fired cold on this VM: a cold
  walk keeps six of eight cores busy *inside the kernel* — the cold work is CPU, page cache
  and buffer heads and completions, with a quarter of the time waiting — so utilisation never
  fell under the threshold, the pool stayed at eight, and cold was 7–14% slower.
- **Each worker's own time on CPU** (`/proc/self/task/<tid>/schedstat`), which tells waiting
  from working: grow by doubling under 90% busy. Cold 3–4% slower than the fixed 24 (the
  ramp costs 40–60 ms of a 770 ms scan); warm 1–2% slower, within noise — and that is the
  finding. The gain the step was after, builders less starved with fewer walkers warm, did not
  appear: at 24 walkers warm the builders were slow per entry but the wall clock was already
  what the machine's cores allow.

So the fixed count stays: three a core, at most 32. Not merged; the code is in the session's
history if a machine with a different balance — many cores, a slow disk — wants to try it.
