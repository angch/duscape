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

## Filesystems and mount points

By default the scan crosses mount points, like `du`. Pass `-x` / `--one-file-system` to keep it on
the filesystem the scan started on.

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

