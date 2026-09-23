# Filesystem fixtures

What diskonaut reports depends on the filesystem under it: hard links, copy-on-write clones,
snapshots, compression, sparse files and mount layouts all change what "size" means, and a temp
directory on ext4 exercises none of it. These fixtures make each filesystem on a loopback image,
fill it with those quirks, and check diskonaut against what the volume really holds.

```bash
make test-fs                        # everything
make test-fs FS="btrfs snapshots"   # just these
fixtures/fs/run.sh --no-build xfs   # reuse the last build
```

Making filesystems needs root. As a member of the `docker` group, `run.sh` runs the root half in a
`--privileged` container of a local image, `diskonaut-fs-fixtures`, which `build-image.sh` makes
from this machine's own tools (plus `xfsprogs`, `f2fs-tools`, `nfs-kernel-server`, `nfs-common`
and `rclone`, fetched with `apt-get download` if the host lacks them) — no registry, and only the
work directory (`target/fs-fixtures`) is shared with it. As root (CI, under `sudo`) it runs directly on the host. Either way the scans and tests
run as an ordinary user, as the app would. Delete the image to rebuild it after installing tools.

## What runs

For each of **ext4, XFS, btrfs, f2fs, tmpfs, FAT32, exFAT, NTFS** (`ntfs3`):

- a generic tree: sizes either side of a block, 1500 tiny files, ten levels of nesting, a 64 MiB
  sparse file with 1 MiB written, a file with three hard links across two folders, a symlink, and
  names with spaces, non-ASCII and a newline — whatever the filesystem accepts
- diskonaut's total against an **inode oracle**: blocks (or, with `-a`, lengths) of every
  non-directory, counted once per `(device, inode)` by `find` — independent of diskonaut's code
- on the POSIX ones, the whole `libdiskonaut` and `diskonaut-angch` test suites with `TMPDIR` on
  that filesystem, so every test that makes a temp tree makes it there; `DISKONAUT_TEST_REFLINK_DIR`
  is set on XFS and btrfs and `DISKONAUT_TEST_BTRFS_DIR` on btrfs, which enables their tests
- on XFS and btrfs, **copy-on-write**: whole reflink clones held once, a clone rewritten in part
  counted in full (by design), and a clone below the probe threshold

Scenarios:

- **`snapshots`** (btrfs): a subvolume, three read-only snapshots, a file rewritten after them —
  once with large files, once with small; the oracle is btrfs's own *data used*, since `st_dev`
  differs per snapshot and no inode oracle can see the sharing. `-x` must see only the top level.
- **`compression`**: btrfs with `compress-force=zstd` holding text, random data, a file half of
  each, a preallocated file and a sparse one, scanned as the user and **as root**; btrfs without
  compression but a `chattr +c` folder, as root; f2fs with lz4. The oracle is btrfs's data used.
- **`compressed-snapshots`**: compressed files of 128 extents each (more than one FIEMAP page),
  two snapshots, a reflink copy and a rewrite: as root, btrfs's data used exactly; as a user, each
  distinct file once at its uncompressed size.
- **`mounts`**: an ext4 root with XFS nested in it, `proc` (must be skipped), `tmpfs` (counted, as
  `du` counts it), checked with and without `-x`; then a bind mount of a folder inside the scan.
- **`network`**: a loopback NFSv4 server (kernel `nfsd`, exporting a tmpfs) and an `rclone mount
  :memory:` (`fuse.rclone`, remote by subtype, no network needed), both mounted inside an ext4
  root. Neither may be walked into; an NFS mount named as the root must still scan. The container
  runs its own `fusermount3` from `/usr/local/bin`, since Ubuntu's AppArmor profile for it attaches
  by path and refuses mounts in a container.

## Reading the results

Each check is one line:

| Result | Meaning |
| ------ | ------- |
| `PASS` | within tolerance (almost always exact) |
| `FAIL` | a regression; the exit status is non-zero |
| `KNOWN` | a documented limitation or an open bug, with its size, so a change in it still shows |
| `SKIP` | this machine cannot make that filesystem or do that thing |

Scans run as the test user unless a check says "as root", which runs diskonaut as root the way
`sudo diskonaut` would. Totals are read from `--benchmark --bench-stage refined`: the scan and then the second pass over
small files, which is what the app shows once its title stops saying "refining".

The one KNOWN line as of 2026-09-23:

- **btrfs compression, as a user**: `stat` reports the uncompressed blocks, and only
  `BTRFS_IOC_TREE_SEARCH_V2`, which needs `CAP_SYS_ADMIN`, says what the extents occupy. As root
  diskonaut reads them, and those checks pass exactly.

Fixed since the fixtures found them, and now ordinary checks: btrfs snapshots counted once per
snapshot (the device was part of the extent identity), small files and clones under 64 KiB never
checked for sharing (now the second pass), and bind mounts counted twice.

## Adding to it

A scan or accounting change should come with a fixture here if its behaviour depends on the
filesystem or the mount table. Add the quirk to `make_dataset` if every filesystem should have it,
or a scenario function to `inside.sh` if it needs a layout, and give it an oracle that does not
share code with diskonaut: `find`, the filesystem's own tools, or sizes the scenario constructed.
A new limitation goes in as `KNOWN` with its reason, not as a looser tolerance.
