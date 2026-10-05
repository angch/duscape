# How sizes are counted

What a folder's size means, what the scan enters, and why the total can be less than the disk's
used space. Moved here from the README; `scan-performance.md` has the measurements behind it.

## Hard links and folder sizes

A folder's size is the space held under it: each distinct file counted once, however many names
point at it. If `a/a`, `a/b` and `b/a` are all links to the same 1 KiB file, then `a` is 1 KiB,
`b` is 1 KiB, and the root holding both is 1 KiB — deleting either folder alone frees nothing, and
deleting both frees 1 KiB.

This differs from `du`, which deduplicates in traversal order and so charges whichever directory it
reached first, reporting nothing for the others.

Two things follow that are worth knowing:

- **Sizes do not add up where hard links are involved.** A folder can be smaller than the sum of
  the tiles inside it — each file tile shows that file's own size, while the folder shows the space
  it holds.
- **Deleting one link frees nothing** until the last link is gone, so the "space freed" figure is
  optimistic in that case. A rescan restores the true picture.

`scan-performance.md` covers the details and the reasoning.

## Clones on macOS

An APFS clone — what `cp -c`, the Finder's Duplicate, pnpm's store and uv's cache make — is a
second file sharing the first one's blocks. Like a hard link it is counted once: files that are
pure clones of each other share a *clone id*, and duscape keys their blocks by it, so a folder
holding three clones of a 100 MB file is 100 MB. Once a clone is written to, it gets an id of its
own and is counted in full beside the original, though most of its blocks may still be shared:
the same rule as reflinks on Linux, which overstates rather than understates. On one Mac the rule
took 14.4 GiB off the data volume (pnpm stores, Python caches, Telegram's copies of downloads,
Xcode's app installs) and 12.9 GiB off `/System/Volumes/Preboot`.

What it cannot see is blocks shared with *part* of another file. The system's cryptexes on
Preboot are unpacked from a disk image (`os.dmg`) by sharing ranges of it: the unpacked files'
compressed data lies inside the image's own extents (checked with `F_LOG2PHYS_EXT`), and no
per-file identity says so. Preboot reads 14.8 GiB on a volume using 8.4.

## Disk images

A disk image mounted inside the scan, from an image file the scan also counts, would be counted
twice: once as the file, once as the volume's contents. On macOS the volume is left empty, and
`--issues` says which file it was counted as. The iOS simulator's runtime is one: 21.6 GiB
mounted at `/Library/Developer/CoreSimulator/Volumes/…` from a `.dmg` under
`/System/Library/AssetsV2`. An image the scan does not reach — on a share, or outside the folder
scanned — is the only way to its files, and its volume is walked.

## Filesystems and mount points

By default the scan crosses mount points, like `du`. Pass `-x` / `--one-file-system` to keep it on
the filesystem the scan started on. On btrfs that is the whole filesystem, all its subvolumes
included: each has a device number of its own, so `du -x`, which goes by device, stops at every
subvolume — on a Synology NAS, at every share — where duscape keeps to the filesystem (by its
UUID, which every subvolume of it shares) and leaves out only other filesystems.

One thing is skipped either way: a mount point that leads back to the filesystem the scan started
on, because those files are already being counted by another path. On macOS that is
`/System/Volumes/Data`, which is both a mount point and grafted into `/` through firmlinks — follow
both and almost every file on the machine is counted twice.

On Windows the scan never follows a junction, a symbolic link or a volume mounted in a folder, so
it stays on the volume it started on with or without `-x`. OneDrive placeholders and other
reparse points that stand for real files are scanned as usual.

## Hard links on Windows

A file with several names (a hard link) is counted once per folder, however many of its names that
folder holds. Linux and macOS report each file's link count for free; Windows directory listings do
not, and asking costs a file open per file. So on Windows duscape tracks files by their NTFS/ReFS
file id instead, which costs memory rather than time — about 100 bytes a file.

By default only the places hard links are normally made are tracked: the Windows directory, Edge,
Docker and Git installs, and package stores (`node_modules`, pnpm, uv, `.venv`, `site-packages`).
A link made by hand elsewhere is counted once per name, which overstates rather than understates.
Run as administrator, duscape tracks every file: it then reaches other users' profiles and
system folders the list was not drawn from, and on one `C:\` those held 3.1 GiB of links the list
missed. To track everywhere unelevated, pass `--hard-link-threshold BYTES` — every file at least that large is
tracked, wherever it is:

```sh
duscape --hard-link-threshold 1 C:\       # exact: every non-empty file
duscape --hard-link-threshold 1048576 D:\ # only files of 1 MiB and more
```

On one 2M-entry `C:\`, the default found all but 240 MB of 17 GB of double-counted links, for 90 MB
of memory; tracking every file was exact, for 200 MB and about a second more.

## Why the total can be less than the disk's used space

A scan adds up the files it can reach. The volume's used space, which Explorer and WizTree report,
also holds what no directory lists: the filesystem's own metadata (NTFS's master file table),
shadow copies and snapshots, and every folder the scan was refused. So when a scan covers a whole
volume, the title shows the volume's used space and the part of it the scan did not find:

```
Total: 315.8G (2058004 files), freed: 0, disk used: 424.4G, 108.6G outside the scan
```

On macOS the volume's own use is asked of APFS (`ATTR_VOL_SPACEUSED`), since every volume of a
container reports the container's blocks, except at `/`: its scan reaches the data volume and the
other volumes mounted under it, so the container's use is what it can find. What `/` does not find
on a typical Mac, measured on 2026-10-01 (814.6 GiB in use, 715 found unprivileged):

- Time Machine's local snapshots and other purgeable space — what the system says it could free
  (40 GiB there). Blocks only a snapshot holds are in use and in no folder.
- Folders the scan is refused: as root, the ones behind Full Disk Access (`~/.Trash`, Mail,
  Messages, `~/Library/Group Containers`, Spotlight's index); unprivileged, root's too (16.6 GiB
  more of them). `duscape --issues` shows where, by folder.
- The Recovery volume, which is not mounted, and APFS's own metadata.

### Local snapshots

At a volume's root on macOS (`/`, whose data volume's, or another APFS volume's mount point) the
treemap holds a folder no directory lists: `(local snapshots)`, one folder in it per snapshot of
the volume (Time Machine's `com.apple.TimeMachine.<date>.local`, and any other's), the last of
the passes, once the scan, its catch-up and its fill are done. Nothing in it can be deleted
from duscape: a snapshot's space is freed by deleting the snapshot (`tmutil
deletelocalsnapshots`).

Unprivileged the snapshots are only named — listing them needs no privilege, reading inside one
does (its mount is refused) — and the status line says so. The window splits the unseen space:
"Snapshots and purgeable, at most" is what the system says it could free (available for
important use less free, 77.2 GB on the Mac this was written on), beside the free space, and
"Not seen by the scan" the rest. *At most*, because the system's figure counts purgeable files
(caches, iCloud copies kept locally), which the scan has found, as well as the snapshots, which
it has not; and the rest is mostly the folders the scan could not read (`duscape --issues`
says where).

As root (`sudo duscape /`; the terminal needs Full Disk Access, which root does not bypass) each
snapshot is mounted read-only out of the Finder's sight and read, and its folder holds what it
alone keeps:

- Files gone from the disk: a file whose id is nowhere on the live volume (APFS keeps a file's
  id in its snapshots, and never gives it to another file), so one moved or renamed since is not
  counted. A file several snapshots keep is counted once above them, in each one's folder all
  the same.
- Files written over in place since — a virtual machine's disk, a database, `Docker.raw` —
  which keep their id: the blocks the snapshot keeps of the old contents, found by where they
  are on the disk (the snapshot file's physical extents less the live file's, less what another
  snapshot already counted). Likely the bulk of what Time Machine's snapshots hold on a machine
  with a busy virtual machine — `Docker.raw` there went from 60.8 to 43.5 GB of blocks in an
  hour — though not yet measured as root: `--issues` says how much each snapshot holds of each
  kind.

A *dataless* snapshot (`tmutil listlocalsnapshots` marks it so) is named and not read: macOS
has trimmed it to its metadata, so it still lists every file it had at its size and holds none
of their blocks. Read, the one on the Mac this was written on listed 267.6 GB of files gone from
the disk, on a volume whose snapshots could hold 142 GB at most.

Which folders are read is the volume's change log's to say: the folders it names since the
snapshot was made, found by the log's own files (root only), streamed on `/` and taken back onto
the data volume through its firmlinks. Where the log does not reach back to a snapshot, or lost
events under the volume since, the snapshot is read whole, and what was written in place is
not seen (a write leaves a folder's time as it was).

What the folders cannot show, so some of a snapshot's space stays unseen:

- A snapshot read whole (the log does not reach it): files written over in place.
- A deleted file that was an APFS clone of one still live is counted in full, though its blocks
  are the live one's.
- A deleted file that changed size between two snapshots is counted once at each size: the
  ledger takes two sizes for two files.
- A live file deleted after the pass ran is held by the snapshots from then on, and shows in
  their folders only after the next scan.

`sudo duscape --issues /` reads every snapshot and says, for each, whether it mounted (and
`mount_apfs`'s error if not), whether the log narrowed it, how many folders were read, and how
much it holds in files gone and in blocks written over.

It appears for a drive root on Windows, and on Unix for a mount point scanned with `-x` (without
it, the scan may cross into other filesystems and the two stop being comparable). It is not shown
with `--apparent-size`, since file lengths cannot be set against blocks in use.

On Windows, run duscape as administrator to see nearly everything. Elevated, it turns on the
backup privilege, as WizTree does, which lets it read `System Volume Information`, other users'
profiles and `WindowsApps` whatever their permissions say. It grants reading only: deleting still
needs ordinary permission. On one `C:\`, unelevated, 192 folders were unreadable and 108.6 GiB of
the 424.4 GiB in use was outside the scan; elevated, none were.

Elevated, a whole-volume scan reads the master file table rather than walking the directories
(see `scan-performance.md`, "The master file table"), which reaches every folder, so nothing is
outside the scan but the filesystem's own reserved space.

Elevated, a scan of a whole NTFS volume also shows the filesystem's own files at its root, under
their real names: `$MFT` (the master file table, 2.7 GiB on that `C:\`), `$LogFile`, `$Bitmap`,
`$Secure`, and `$Extend` with the change journal and the rest in it. No folder lists them, so they
are read from their master file table records. They are there to account for the space; the app
will not offer to delete them. What stays outside the scan after that is small: folders' own
indexes, and files whose directory entries lag behind their size while they are being written.

