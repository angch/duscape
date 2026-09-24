#!/usr/bin/env python3
"""Can a cold walk read fewer, larger blocks by prefetching directory blocks through the device?

A cold ext4 walk pays one 4 KiB read per directory for its data block, issued synchronously by
`getdents64` with no readahead across directories (72% of all reads on the tree this was written
against). In inode order those blocks fall into contiguous runs of several — allocated in the
inode's block group — so a `readahead(2)` on the *block device* over the run, once the first
directory of the run is open and `FS_IOC_FIEMAP` has said where it is, would bring the siblings'
blocks in with one larger read. The buffer cache and the device's page cache are the same pages,
so `getdents64` then finds them.

Reading the device needs root (or the `disk` group), which is the only reason the walker does not
do this itself. Run it both ways on a cold cache and compare the block layer's read count:

    sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'; sudo ./dirblock_prefetch.py DIR
    sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'; sudo ./dirblock_prefetch.py DIR --window 0

`--dry` needs no root: it walks warm, asks FIEMAP where every directory's first block is, and
reports how many contiguous runs the blocks form in the order this walk uses (smallest inode
first, depth first), which bounds what the prefetch can save.
"""
import argparse
import array
import fcntl
import os
import struct
import sys
import time

FS_IOC_FIEMAP = 0xC020660B
BLOCK = 4096


def first_block(fd):
    """Physical byte offset of the directory's first block, or None."""
    header = struct.pack("QQIIII", 0, 1 << 62, 0, 0, 1, 0)
    buf = array.array("B", header + bytes(56))
    try:
        fcntl.ioctl(fd, FS_IOC_FIEMAP, buf, True)
    except OSError:
        return None
    if struct.unpack_from("I", buf, 20)[0] == 0:
        return None
    return struct.unpack_from("Q", buf, 32 + 8)[0]


def device_of(path):
    dev = os.stat(path).st_dev
    for line in open("/proc/self/mountinfo"):
        f = line.split()
        if f[2] == f"{os.major(dev)}:{os.minor(dev)}":
            return f[f.index("-") + 2]
    sys.exit(f"no device for {path}")


def diskstats(dev):
    name = os.path.basename(os.path.realpath(dev))
    for line in open("/proc/diskstats"):
        f = line.split()
        if f[2] == name:
            return int(f[3]), int(f[5])
    return 0, 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("root")
    ap.add_argument("--window", type=int, default=32, help="KiB to read ahead from each directory's first block (0: none)")
    ap.add_argument("--dry", action="store_true", help="no prefetch, no device: count the runs")
    a = ap.parse_args()

    dev_fd = None
    if not a.dry and a.window:
        dev_fd = os.open(device_of(a.root), os.O_RDONLY)
    dev = device_of(a.root)
    reads0, sectors0 = diskstats(dev)
    t0 = time.time()

    stack = [a.root]
    dirs = entries = 0
    blocks = []
    prefetched_to = -1
    while stack:
        path = stack.pop()
        try:
            fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        except OSError:
            continue
        dirs += 1
        where = first_block(fd)
        if where is not None:
            blocks.append(where // BLOCK)
            # Prefetch forwards from here unless the last window already covers it.
            if dev_fd is not None and where >= prefetched_to:
                os.posix_fadvise(dev_fd, where, a.window * 1024, os.POSIX_FADV_WILLNEED)
                prefetched_to = where + a.window * 1024
        children = []
        with os.scandir(fd) as it:
            for d in it:
                entries += 1
                if d.is_dir(follow_symlinks=False):
                    children.append((d.inode(), d.name))
                elif not a.dry:
                    d.stat(follow_symlinks=False)
        os.close(fd)
        children.sort(reverse=True)  # popped from the end: smallest inode first
        stack.extend(os.path.join(path, name) for _, name in children)

    elapsed = time.time() - t0
    reads, sectors = diskstats(dev)
    runs = 1 + sum(1 for x, y in zip(blocks, blocks[1:]) if y != x + 1)
    print(f"{dirs} directories, {entries} entries, {elapsed:.2f}s")
    print(f"directory blocks: {len(blocks)} in {runs} contiguous runs ({len(blocks) / max(runs, 1):.1f} a run)")
    if not a.dry:
        print(f"device reads: {reads - reads0}, {(sectors - sectors0) / 2048:.0f} MiB, window {a.window} KiB")


if __name__ == "__main__":
    main()
