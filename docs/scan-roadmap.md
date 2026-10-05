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
1.5M, and duscape and `diskus` finish within a few percent of each other on both, so warm, the
walk is the kernel's cost on either. Cold, the NVMe is 2.5–3x the warm time where the VM was
3–4x, and `diskus` leads by 10–25% there: the difference in how the two issue reads in flight
shows once the disk is the floor. Cold on a spinning disk has not been seen at all, and it is
where the inode-order and prefetch work should show most.

One spinning disk has been seen, briefly (`scan-performance.md`, "A spinning RAID, twelve
minutes in": a 7.3 TB hardware RAID, 11.1M inodes, kernel 5.4, no root): about 2,200 random
4 KiB reads a second and a walk of over half an hour cold, which is where the device read and
the saved scan would pay most, and neither has been measured there.
On macOS (`handles-20260925.md`: M4 Pro, APFS on the internal SSD) the walk is about 300–350k
entries/s warm, a seventh of Linux's, and `diskus` takes 1.6x as long and WizTreeMac 5.4x (0.45 s
of which is its start-up). Root changes nothing: macOS has no device read. `purge` leaves APFS's
metadata cached, so cold is within 10% of warm there and a true cold cell still needs a reboot.

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
- Open: a spinning disk. On the RAID above the kernel walk is bound by 2,200 random reads a
  second; the device read's ordered sweep of the used inode-table blocks should be minutes
  against half an hour. Needs the device readable there (root, or the `disk` group).
- Result on bare-metal NVMe (`badwolf-20260925-full.md`, 16 cores, kernel 7.0): totals
  identical, but the cold gate is not met and warm it is a loss — cold 1.06–1.4x the kernel
  walk as root (1.09x on 673k entries), warm 0.7–0.8x (343 ms against 281). The inode survey
  alone is 97 ms at 4.2 GiB/s, so the device is not the cost; the generation-by-generation
  parse serialises what the kernel's `statx` threads run on sixteen cores at a third of the
  VM's per-call price. Whether it should stay on by default there is open: as it stands the
  default costs a root user on such a machine 25–40% warm for a 10% cold gain.

### 2b. NTFS's master file table (done: Windows, elevated, whole volume — 1.7x the walk warm, 2.6x cold, 1.8x WizTree)

*Windows, NTFS, elevated.* The Windows shape of step 2, and what WizTree does: the `$MFT` read
sequentially along its runs, every record parsed for its names and their directories, the
unnamed stream's sizes and the directory and reparse flags, the tree handed on breadth-first
from the scan root's record as one `DirEntries` per directory (`scanners/src/mft.rs`). The
volume is flushed first, so the table is current.
- Gate: entries and hard links identical to the walk's on a quiet tree; faster than the walk
  wherever it is chosen over it.
- Result (`scan-performance.md`, "The master file table"; `benchmarks/tiamat-20260925-mft.md`):
  on a 2.46M-entry `C:\`, 3.7 s against the walk's 6.1 s warm and 3.0 s against 9.1 s cold,
  with 0.6 s of system CPU against 46 s; WizTree's export, reading the same table, 7.1 s and
  6.7 s. Entries and hard-linked counts came out
  identical on `C:\Windows` and `C:\Program Files`, and on the volume the table sees the
  folders the walk is refused. Sizes differ by 0.0015–0.017% on quiet trees: the directory
  listings' lazily updated size copies, which the walk reads and the table does not.
- Used only for a volume root, and only where a sample of the table says the volume's
  directories are small (`TABLE_UP_TO`, 8 entries a directory): the table costs every record
  on the volume (about 1.3 µs each) and the walk a handle per directory (about 12 µs on a system
  volume with its filter drivers), so a subtree (`C:\Program Files`: 2.3 s against 0.6 s) or a
  data volume of large files (`D:\`, 16 entries a directory: 0.63 s against 0.15 s) walks
  faster.
- Open: memory (about 600 MB for 2.5M records, against WizTree's 180 MB); the cold read, which
  the synchronous 32 MiB reads make 0.5 GB/s where the device does 3; and whether the sample
  (2.2 entries a directory on a volume whose true figure is 5.4) is the right predictor on other
  machines.

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

### 9. A saved tree, brought up to date by the volume's change journal (done on macOS: `~` in 4.8–6.1 s from 27.8 s; Windows to come)

*macOS, every scan after the first; Windows likewise.* APFS offers nothing to read but the
VFS (`scan-performance.md`, "macOS: what is left": the device is FileVault ciphertext,
`searchfs` is serial and I/O-bound, and the kernel's contention caps the walk at eight
workers and about 2.9 s a million entries), so a first scan stays at that floor. But the
volume keeps a change log that needs no privilege to replay: FSEvents holds this machine's
whole history and replays a day's million events in 1.7 s as ten thousand changed
directories. Save the finished tree with the event id at the end of a scan (under
`~/Library/Caches` or the config directory, per scan root, versioned); on the next start load
it, replay the log since its id, list the reported directories again (one bulk call each,
recursive where the event says so), re-charge the ledger for what was touched, and show the
tree. Show the saved tree at once and refresh in place, as the outline is shown now; a log
that has wrapped, been dropped or pruned, or names the root, means a whole rescan, said so in
the status line. Windows has the USN journal for the same, elevated; Linux has no persistent journal and
does not need one.
- Gate: on `~` here (8M entries, 27 s), a second scan a day later current to the byte against
  a fresh walk and on screen in under 10 s; the cache loads at no less than 5M entries/s; a
  saved tree older than the log's reach is rescanned whole and says why. `make test-fs` and
  every viewer unchanged.
- Result (`scan-performance.md`, "Roadmap step 9, done", then "Shown at once, caught up
  behind, filled in after"): `~` (8.1M entries) shown in 0.5 s and current in 2.6 against
  28 s fresh, `/` (11.4M) in 0.9 and 3.5 against 42, `~/project` (3.2M) in 0.17 and 0.7
  against 9.5 with totals identical to the byte; the file 3 bytes an entry (23 MB for `~`,
  32 for `/`); the first scan pays nothing measurable to save. What the step said it would
  not handle it does not: a new subfolder that is a mount point is walked as a root; `R`
  does not refresh the file, only the catch-up does. Windows (the USN journal, elevated) is
  the next cell; Linux the one after, on the spinning RAID above: the Saved stream and the
  recorder alone, a catch-up that always walks (no change log a user can read), no fill,
  since a cold walk there is half an hour and the file a second or two.
- The saved scans' size, what is left to do (2026-10-01): the sweep removes what can never be
  read again and keeps the rest within 256 MiB (`cache/sweep.rs`), but a scan of `/`, of `~` and
  of `~/project` are three files where one would do — each a folder's whole listing, the
  smaller ones inside the largest. Serving a folder from an ancestor's saved scan (its subtree
  of the stream, the ancestor's stamp, the log replayed under the folder only) would make them
  one; a feature of its own, since the catch-up and the fill would then write into the
  ancestor's file. Smaller levers, unmeasured: `KEEP_FROM` (1 MiB) higher keeps fewer files one
  by one at the fill's cost; deflate's best level instead of the default.
- Risk: a size changed with no directory event (an `mmap` writer that has not closed, unmeasured)
  is stale until its folder is rescanned; the cache's own size (about 25 bytes an entry
  compactly, 200 MB for `~`) and its staleness after a volume is moved between machines
  (key it on the volume UUID and the event id together). And three cases a relist of the
  reported directories alone gets wrong, each wanting model work: a folder *renamed* fires
  events on the old and the new parent with no `MustScanSubDirs`, so a relist sees one gone
  and one new — either the node is moved by inode, or the moved subtree is walked again
  whole; a hard-linked file written through *another* name fires its event in that other
  directory, so this one's copy of the size goes stale unless every link of the inode is
  updated after a relist (the ledger keys on inode, so the hook exists); and a relist must
  take `ENOENT` as the directory itself gone. Listing a directory again while keeping the
  subfolders under it is a new operation beside `FileTree::graft`, which replaces a subtree
  whole. On Windows the USN journal is read from the volume handle, elevated — the window
  elevates for a volume scan already — where FSEvents needs no privilege.

### 10. A volume's local snapshots, read as root (done on macOS, unrun as root: 2026-10-05)

*macOS.* Time Machine's hourly local snapshots hold tens of gigabytes no walk of the live
files finds, shown as "Not seen by the scan". Listing them needs no privilege
(`fs_snapshot_list`); mounting one does (`fs_snapshot_mount` and `mount_apfs -s` both `EPERM`
unprivileged, measured). So the volume root gets a `(local snapshots)` folder with a folder per
snapshot as the last idle pass (`Rescans::start_idle`), and as root each is mounted, walked and
set against the live volume by file id through volfs (`/.vol/<device>/<id>`, unprivileged),
the files gone from the live volume kept (`snapshots.rs`, `docs/sizes.md` "Local snapshots").
- Result: a pass over a folder the same as live costs its walk — every folder listed, one volfs
  lookup and two times each: `~` (1.12M folders, 8.2M entries) 30.3 s against the walk's
  29.1, warm, six threads. Four snapshots are four walks behind everything else. The root path
  (mount, walk, unmount, the graft) has not been run: no root on the machine it was written on.
- Narrowed by FSEvents (done, 2026-10-05, the root path unrun): the folders the log names since
  each snapshot are read, one level, with their subfolders moved or gone since walked whole;
  nothing else is listed. Measured unprivileged: the last million ids replay in 1.6–4.2 s as
  9.5k events, every one on the data volume once taken through the firmlinks; ten million in
  10–12 s. `FSEventsGetLastEventIdForDeviceBeforeTime` returns 0 for any time and is no use: the
  id before a snapshot comes from `.fseventsd`'s file names and times. A stream on the data
  volume saw its paths as `/Users/…`, outside itself: the stream is on `/`. A replay of ten
  million ids dropped 91–288 *live* events with `MustScanSubDirs` on `/`; events after the
  replay's start are left out (`fsevents::events`), which the catch-up gains too.
- Written over in place (done): in a folder the log named, a file still live whose size or time
  differs is counted by its blocks — `F_LOG2PHYS_EXT` extents (`Docker.raw`, 43.5 GB in 106k
  runs, 0.38 s), less the live file's, less what another snapshot counted. A root run on the
  machine that wrote it still showed 128 GB unseen before this; `Docker.raw` alone went from
  60.8 to 43.5 GB of blocks in an hour.
- First root run (2026-10-05, `sudo duscape --issues /`): every snapshot refused by
  `mount_apfs` with `Operation not permitted`, as unprivileged — most likely the terminal
  without Full Disk Access, which root does not bypass. The mount now tries the volume's mount
  point after its device node and keeps both errors, and `--issues` says whether the process
  has Full Disk Access (it can read a TCC database).
- Second and third root runs, with Full Disk Access: every snapshot mounted. The log narrowed
  the three it reaches to about 97k folders each, 16–17 s apiece, against 1.49M folders and
  57 s for the one it does not. But "held by the snapshots alone" came to 334.8 GB where at
  most 142 GB is unseen: the oldest snapshot, 266.6 GB of it, is *dataless* — trimmed by
  macOS to its metadata, listing files it no longer holds (`ATTR_CMN_FLAGS` 0x20 in
  `fs_snapshot_list`, as `tmutil` marks it). Such a snapshot is now named and not read. The
  other three's figures — about 68 GB more files gone than the dataless one listed, 13.5 GB of
  old blocks written over (`Docker.raw` and the like) — fit under the bound.
- Rerun with dataless snapshots skipped: the three read hold 128.3 GiB alone, in 56.6 s, against
  128.6 GiB unseen (888.2 GB used, 698.6 GiB found). Close enough to be a little high, since
  the 86 folders System Integrity Protection keeps from root and APFS's own metadata are unseen
  too: 21 GiB of the files gone are clones, whose blocks may be a live twin's. Next: a clone's
  blocks set against its family's live members by extents, as a file written over is.

## Not worth revisiting

Measured dead, with the numbers in `scan-performance.md`: `statx` masks, `AT_STATX_DONT_SYNC`,
skipping stats by `d_type`, io_uring `statx` (warm and cold), per-crate `opt-level` under LTO,
thin LTO, mimalloc, PGO in the release pipeline. On macOS (2026-09-29): `searchfs`, Spotlight,
fewer attributes in the bulk request, `O_EVTONLY`, bigger buffers, inode-ordered descent, more
than eight workers, and workers as processes rather than threads.

## Running a step on another machine

```sh
git clone … && cd duscape && cargo build --release -p duscape
cargo install hyperfine diskus              # the comparison
docs/probes/bench-matrix.sh ~/src ~ ~/.cache   # or whichever three trees
```

The script says what it could not do (no root for a cache drop, no `diskus`) in the file it
writes. Commit that file; then do the step; then run the script again and commit that too.
