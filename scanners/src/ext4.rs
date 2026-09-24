//! ext4 read from the device: the walk's answers taken from the inode tables and directory
//! blocks in large ordered reads, instead of asked of the kernel one entry at a time. Root, or
//! the `disk` group, since it opens the block device.
//!
//! `docs/scan-roadmap.md` steps 1 and 2. The survey (step 1) reads the superblock, the group
//! descriptors and the used part of every group's inode table, and sums every live inode's
//! size: the floor. The walk (step 2) keeps the inodes it read, then goes down from the scan
//! root's directory a generation at a time: the frontier's directory blocks are gathered, sorted
//! by where they are on the device and read in a few large sweeps, parsed for names
//! (`ext4_dir_entry_2`, which htree leaves use as well; index blocks and checksum tails carry
//! inode 0 and drop out), and every directory is handed on as one [`DirEntries`] exactly as the
//! kernel walk would hand it, so the tree, the ledger and the viewers see no difference. A
//! mount point inside the tree is another filesystem: it goes to the kernel walker.
//!
//! The layout is the kernel's `fs/ext4/ext4.h`. What is read is the device's page cache, which
//! is the buffer cache ext4 reads its own metadata through, so it is as current as the disk:
//! delayed allocation and an uncheckpointed journal mean the last seconds of writes are not in
//! it yet. A filesystem this cannot follow — META_BG, a device that will not open — makes it
//! decline before it has said anything, and the kernel walk takes over; a directory of a shape
//! it does not read (an inline directory spilling into an xattr, a triply indirect one) goes to
//! the kernel walker whole, like a mount point.

use ::std::ffi::OsStr;
use ::std::fs::File;
use ::std::os::unix::ffi::OsStrExt;
use ::std::os::unix::fs::{FileExt, MetadataExt};
use ::std::path::{Path, PathBuf};
use ::std::sync::Arc;
use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use ::std::thread::JoinHandle;
use ::std::time::{Duration, Instant};
use libdiskonaut::model::files::hash::FastMap;

use super::{DirEntries, EntryMeta, ScanOptions};

const SUPERBLOCK_OFFSET: u64 = 1024;
const SUPERBLOCK_LEN: usize = 1024;
const MAGIC: u16 = 0xEF53;

const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_64BIT: u32 = 0x80;
const RO_COMPAT_HUGE_FILE: u32 = 0x8;
const RO_COMPAT_GDT_CSUM: u32 = 0x10;
const RO_COMPAT_METADATA_CSUM: u32 = 0x400;

const BG_INODE_UNINIT: u16 = 0x1;

const INODE_HUGE_FILE_FL: u32 = 0x0004_0000;
const INODE_EXTENTS_FL: u32 = 0x0008_0000;
const INODE_INLINE_DATA_FL: u32 = 0x1000_0000;
const S_IFMT: u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;

const EXTENT_MAGIC: u16 = 0xF30A;
/// The root directory's inode number.
const ROOT_INO: u64 = 2;
/// One `pread` at most this long when sweeping directory blocks.
const SWEEP_READ: u64 = 8 << 20;
/// Entries per channel message to the tree builder, as the kernel walk batches.
const SEND_BATCH: usize = 4096;

fn le16(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}
fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().expect("4 bytes"))
}

/// The block device the filesystem holding `path` is on: `/dev/sdb`, by the file's `st_dev`.
pub fn device_for(path: &Path) -> Result<PathBuf, String> {
    let dev = ::std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .dev();
    #[allow(clippy::cast_possible_truncation)]
    let (major, minor) = (
        libc::major(dev as libc::dev_t),
        libc::minor(dev as libc::dev_t),
    );
    let link = ::std::fs::read_link(format!("/sys/dev/block/{major}:{minor}"))
        .map_err(|_| format!("{} is not on a block device this can name", path.display()))?;
    let name = link
        .file_name()
        .ok_or_else(|| "unnamed block device".to_string())?
        .to_string_lossy()
        .to_string();
    Ok(PathBuf::from(format!("/dev/{name}")))
}

/// An ext4 filesystem open on its device: the superblock's geometry and the group descriptors.
struct Fs {
    file: File,
    device: PathBuf,
    block_size: u32,
    inodes_per_group: u32,
    first_ino: u32,
    inode_size: u16,
    huge_file: bool,
    /// Per group: the inode table's first block and how many inodes of it are in use, or
    /// `None` for a group with no inodes yet.
    tables: Vec<Option<(u64, u32)>>,
    bytes_read: u64,
}

impl Fs {
    fn open(device: &Path) -> Result<Self, String> {
        let file =
            File::open(device).map_err(|e| format!("cannot open {}: {e}", device.display()))?;
        let mut sb = vec![0u8; SUPERBLOCK_LEN];
        file.read_exact_at(&mut sb, SUPERBLOCK_OFFSET)
            .map_err(|e| format!("superblock: {e}"))?;
        if le16(&sb, 56) != MAGIC {
            return Err(format!("{} is not ext2/3/4", device.display()));
        }
        let blocks_count = u64::from(le32(&sb, 4)) | (u64::from(le32(&sb, 0x150)) << 32);
        let first_data_block = le32(&sb, 20);
        let block_size = 1024u32 << le32(&sb, 24);
        let blocks_per_group = le32(&sb, 32);
        let inodes_per_group = le32(&sb, 40);
        let first_ino = le32(&sb, 84);
        let inode_size = le16(&sb, 88);
        let incompat = le32(&sb, 96);
        let ro_compat = le32(&sb, 100);
        if incompat & INCOMPAT_META_BG != 0 {
            return Err("META_BG filesystems are not followed".to_string());
        }
        let desc_size = if incompat & INCOMPAT_64BIT != 0 {
            le16(&sb, 254).max(64) as usize
        } else {
            32
        };
        // `bg_itable_unused` is only kept up to date with a group descriptor checksum.
        let unused_is_valid = ro_compat & (RO_COMPAT_GDT_CSUM | RO_COMPAT_METADATA_CSUM) != 0;
        if inodes_per_group == 0 || blocks_per_group == 0 || inode_size < 128 || block_size > 65536
        {
            return Err("superblock does not add up".to_string());
        }
        let groups = usize::try_from(
            blocks_count
                .saturating_sub(u64::from(first_data_block))
                .div_ceil(u64::from(blocks_per_group)),
        )
        .map_err(|_| "too many groups".to_string())?;

        let gdt_block = u64::from(first_data_block) + 1;
        let mut gdt = vec![0u8; groups * desc_size];
        file.read_exact_at(&mut gdt, gdt_block * u64::from(block_size))
            .map_err(|e| format!("group descriptors: {e}"))?;
        let mut tables = Vec::with_capacity(groups);
        for group in 0..groups {
            let desc = &gdt[group * desc_size..(group + 1) * desc_size];
            if le16(desc, 18) & BG_INODE_UNINIT != 0 {
                tables.push(None);
                continue;
            }
            let mut table = u64::from(le32(desc, 8));
            let mut unused = u32::from(le16(desc, 28));
            if desc_size >= 64 {
                table |= u64::from(le32(desc, 40)) << 32;
                unused |= u32::from(le16(desc, 50)) << 16;
            }
            let used = if unused_is_valid {
                inodes_per_group.saturating_sub(unused)
            } else {
                inodes_per_group
            };
            tables.push((used > 0).then_some((table, used)));
        }
        Ok(Fs {
            file,
            device: device.to_path_buf(),
            block_size,
            inodes_per_group,
            first_ino,
            inode_size,
            huge_file: ro_compat & RO_COMPAT_HUGE_FILE != 0,
            tables,
            bytes_read: SUPERBLOCK_LEN as u64 + gdt.len() as u64,
        })
    }

    fn read_at(&mut self, buf: &mut [u8], offset: u64) -> Result<(), String> {
        self.file.read_exact_at(buf, offset).map_err(|e| {
            format!(
                "read {} bytes at {offset} of {}: {e}",
                buf.len(),
                self.device.display()
            )
        })?;
        self.bytes_read += buf.len() as u64;
        Ok(())
    }

    fn read_block(&mut self, block: u64) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; self.block_size as usize];
        self.read_at(&mut buf, block * u64::from(self.block_size))?;
        Ok(buf)
    }
}

/// One live inode, as much of it as the tree wants.
#[derive(Clone, Copy, Debug, Default)]
struct Meta {
    mode: u16,
    links: u16,
    /// Blocks allocated, in bytes.
    on_disk: u64,
    /// `i_size`.
    size: u64,
}

/// What a directory inode says about where its blocks are: `i_block` and the flags that say
/// how to read it.
#[derive(Clone, Debug)]
struct DirInode {
    flags: u32,
    size: u64,
    block: [u8; 60],
}

/// One inode's bytes, parsed. `None` for one that is not live.
fn parse_inode(fs: &Fs, ino: u64, inode: &[u8]) -> Option<(Meta, Option<DirInode>)> {
    // Reserved inodes — the journal, the resize inode and the rest below `first_ino` — are in
    // no directory. The root directory is the exception.
    if ino != ROOT_INO && ino < u64::from(fs.first_ino) {
        return None;
    }
    let mode = le16(inode, 0);
    let links = le16(inode, 26);
    let dtime = le32(inode, 20);
    if mode == 0 || links == 0 || dtime != 0 {
        return None;
    }
    let flags = le32(inode, 32);
    let mut blocks = u64::from(le32(inode, 28)) | (u64::from(le16(inode, 116)) << 32);
    if fs.huge_file && flags & INODE_HUGE_FILE_FL != 0 {
        blocks *= u64::from(fs.block_size) / 512;
    }
    let size = u64::from(le32(inode, 4)) | (u64::from(le32(inode, 108)) << 32);
    let meta = Meta {
        mode,
        links,
        on_disk: blocks * 512,
        size,
    };
    let dir = (mode & S_IFMT == S_IFDIR).then(|| {
        let mut block = [0u8; 60];
        block.copy_from_slice(&inode[40..100]);
        DirInode { flags, size, block }
    });
    Some((meta, dir))
}

/// Ranges of the device, `(offset, length)`, from sorted block numbers: adjacent blocks in one
/// range, and blocks within `gap` blocks of each other too — a little read for nothing is
/// cheaper than another request — up to [`SWEEP_READ`] each.
fn runs_of(blocks: &[u64], block_size: u64, gap: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for &block in blocks {
        if let Some(last) = out.last_mut() {
            let end_block = (last.0 + last.1) / block_size;
            if block >= end_block
                && block - end_block <= gap
                && (block + 1) * block_size - last.0 <= SWEEP_READ
            {
                last.1 = (block + 1) * block_size - last.0;
                continue;
            }
            if block < end_block {
                continue; // a duplicate
            }
        }
        out.push((block * block_size, block_size));
    }
    out
}

/// Read every range, on `threads` threads, after telling the kernel about all of them, so that
/// the device has the whole sorted list in its queue rather than one request at a time. Each
/// range's bytes go to `each`, whose results come back in range order.
fn sweep<T: Send>(
    fs: &Fs,
    runs: &[(u64, u64)],
    threads: usize,
    each: impl Fn(u64, &[u8]) -> T + Sync,
) -> Result<Vec<T>, String> {
    use ::std::os::unix::io::AsRawFd;
    for &(offset, len) in runs {
        // SAFETY: an advisory call on an open descriptor with plain integer arguments.
        #[allow(clippy::cast_possible_wrap)]
        unsafe {
            libc::posix_fadvise(
                fs.file.as_raw_fd(),
                offset as libc::off_t,
                len as libc::off_t,
                libc::POSIX_FADV_WILLNEED,
            );
        }
    }
    let next = ::std::sync::atomic::AtomicUsize::new(0);
    let results: ::std::sync::Mutex<Vec<Option<T>>> =
        ::std::sync::Mutex::new((0..runs.len()).map(|_| None).collect());
    let failure: ::std::sync::Mutex<Option<String>> = ::std::sync::Mutex::new(None);
    let threads = threads.clamp(1, runs.len().max(1));
    ::std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buffer = Vec::new();
                loop {
                    let i = next.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
                    let Some(&(offset, len)) = runs.get(i) else {
                        break;
                    };
                    buffer.resize(len as usize, 0);
                    if let Err(e) = fs.file.read_exact_at(&mut buffer, offset) {
                        *failure.lock().expect("sweep") = Some(format!(
                            "read {len} bytes at {offset} of {}: {e}",
                            fs.device.display()
                        ));
                        break;
                    }
                    let result = each(offset, &buffer);
                    results.lock().expect("sweep")[i] = Some(result);
                }
            });
        }
    });
    if let Some(error) = failure.into_inner().expect("sweep") {
        return Err(error);
    }
    Ok(results
        .into_inner()
        .expect("sweep")
        .into_iter()
        .map(|r| r.expect("every run read"))
        .collect())
}

/// The inodes the walk has read so far.
#[derive(Default)]
struct Inodes {
    metas: FastMap<u64, Meta>,
    dirs: FastMap<u64, DirInode>,
}

impl Inodes {
    /// Read the inodes `wanted` names that are not here yet: their table blocks, gathered,
    /// sorted and swept.
    fn fetch(&mut self, fs: &mut Fs, wanted: &mut Vec<u64>, threads: usize) -> Result<(), String> {
        wanted.sort_unstable();
        wanted.dedup();
        wanted.retain(|ino| *ino != 0 && !self.metas.contains_key(ino));
        if wanted.is_empty() {
            return Ok(());
        }
        let block_size = u64::from(fs.block_size);
        let inode_size = u64::from(fs.inode_size);
        let per_group = u64::from(fs.inodes_per_group);
        // Each inode's byte offset on the device, if its group has a table.
        let offset_of = |ino: u64| -> Option<u64> {
            let index = ino - 1;
            let group = usize::try_from(index / per_group).ok()?;
            let (table, _used) = (*fs.tables.get(group)?)?;
            Some(table * block_size + (index % per_group) * inode_size)
        };
        let mut blocks: Vec<u64> = wanted
            .iter()
            .filter_map(|&ino| offset_of(ino).map(|o| o / block_size))
            .collect();
        blocks.sort_unstable();
        blocks.dedup();
        let runs = runs_of(&blocks, block_size, 4);
        let bytes: u64 = runs.iter().map(|r| r.1).sum();
        // Which inodes fall in each run, in order: both lists are sorted.
        let mut offsets: Vec<(u64, u64)> = wanted
            .iter()
            .filter_map(|&ino| offset_of(ino).map(|o| (o, ino)))
            .collect();
        offsets.sort_unstable();
        let mut at = 0usize;
        let mut per_run: Vec<Vec<(u64, u64)>> = Vec::with_capacity(runs.len());
        for &(start, len) in &runs {
            let mut mine = Vec::new();
            while at < offsets.len() && offsets[at].0 < start + len {
                if offsets[at].0 >= start {
                    mine.push(offsets[at]);
                }
                at += 1;
            }
            per_run.push(mine);
        }
        let fs_ref: &Fs = fs;
        let parsed = sweep(fs_ref, &runs, threads, |start, bytes| {
            let i = runs.partition_point(|r| r.0 < start);
            let mut out = Vec::with_capacity(per_run[i].len());
            for &(offset, ino) in &per_run[i] {
                let from = (offset - start) as usize;
                let to = from + fs_ref.inode_size as usize;
                if to <= bytes.len()
                    && let Some(parsed) = parse_inode(fs_ref, ino, &bytes[from..to])
                {
                    out.push((ino, parsed));
                }
            }
            out
        })?;
        fs.bytes_read += bytes;
        for (ino, (meta, dir)) in parsed.into_iter().flatten() {
            self.metas.insert(ino, meta);
            if let Some(dir) = dir {
                self.dirs.insert(ino, dir);
            }
        }
        Ok(())
    }
}

/// What the inode tables held (step 1's survey).
#[derive(Debug, Default)]
pub struct Survey {
    pub inodes: u64,
    pub files: u64,
    pub directories: u64,
    /// `i_blocks` summed, in bytes: what a disk-usage scan reports.
    pub bytes_on_disk: u64,
    /// `i_size` of the regular files summed: the apparent total.
    pub apparent: u64,
    pub groups: u32,
    pub groups_read: u32,
    pub bytes_read: u64,
    pub elapsed: Duration,
    pub device: PathBuf,
    pub block_size: u32,
    pub inode_size: u16,
}

/// Survey the ext4 filesystem holding `path`.
pub fn survey_for(path: &Path) -> Result<Survey, String> {
    survey(&device_for(path)?)
}

/// Read the inode tables of the ext4 filesystem on `device` and sum what they hold.
pub fn survey(device: &Path) -> Result<Survey, String> {
    let started = Instant::now();
    let mut fs = Fs::open(device)?;
    let block_size = u64::from(fs.block_size);
    let inode_size = u64::from(fs.inode_size);
    // Every group's used prefix, one range each, swept.
    let mut runs: Vec<(u64, u64, u64)> = Vec::new(); // (offset, len, first inode)
    for (group, table) in fs.tables.iter().enumerate() {
        if let Some((block, used)) = table {
            runs.push((
                block * block_size,
                u64::from(*used) * inode_size,
                group as u64 * u64::from(fs.inodes_per_group) + 1,
            ));
        }
    }
    let ranges: Vec<(u64, u64)> = runs.iter().map(|r| (r.0, r.1)).collect();
    let fs_ref: &Fs = &fs;
    let sums = sweep(fs_ref, &ranges, 8, |start, bytes| {
        let first = runs[runs.partition_point(|r| r.0 < start)].2;
        let mut sum = Survey::default();
        for (i, inode) in bytes.chunks_exact(inode_size as usize).enumerate() {
            if let Some((meta, dir)) = parse_inode(fs_ref, first + i as u64, inode) {
                sum.inodes += 1;
                sum.bytes_on_disk += meta.on_disk;
                if dir.is_some() {
                    sum.directories += 1;
                } else if meta.mode & S_IFMT == S_IFREG {
                    sum.files += 1;
                    sum.apparent += meta.size;
                }
            }
        }
        sum
    })?;
    let mut out = Survey {
        groups: fs.tables.len() as u32,
        groups_read: runs.len() as u32,
        device: fs.device.clone(),
        block_size: fs.block_size,
        inode_size: fs.inode_size,
        ..Survey::default()
    };
    for sum in sums {
        out.inodes += sum.inodes;
        out.files += sum.files;
        out.directories += sum.directories;
        out.bytes_on_disk += sum.bytes_on_disk;
        out.apparent += sum.apparent;
    }
    fs.bytes_read += ranges.iter().map(|r| r.1).sum::<u64>();
    out.bytes_read = fs.bytes_read;
    out.elapsed = started.elapsed();
    Ok(out)
}

// ---- directories ----

/// A directory's data blocks, by logical block: `(logical, physical, count)`.
fn dir_blocks(fs: &mut Fs, dir: &DirInode) -> Result<Vec<(u64, u64, u64)>, String> {
    let block_size = u64::from(fs.block_size);
    let blocks = dir.size.div_ceil(block_size);
    let mut out = Vec::new();
    if dir.flags & INODE_EXTENTS_FL != 0 {
        extents_into(fs, &dir.block, &mut out, 0)?;
    } else {
        // The classic map: twelve direct pointers, then single, double and triple indirection.
        let ptr = |i: usize| u64::from(le32(&dir.block, i * 4));
        let per_block = block_size / 4;
        for i in 0..12u64.min(blocks) {
            let p = ptr(i as usize);
            if p != 0 {
                out.push((i, p, 1));
            }
        }
        if blocks > 12 && ptr(12) != 0 {
            let indirect = fs.read_block(ptr(12))?;
            for j in 0..per_block.min(blocks - 12) {
                let p = u64::from(le32(&indirect, j as usize * 4));
                if p != 0 {
                    out.push((12 + j, p, 1));
                }
            }
        }
        if blocks > 12 + per_block && ptr(13) != 0 {
            let double = fs.read_block(ptr(13))?;
            let mut logical = 12 + per_block;
            for j in 0..per_block {
                if logical >= blocks {
                    break;
                }
                let p = u64::from(le32(&double, j as usize * 4));
                if p != 0 {
                    let indirect = fs.read_block(p)?;
                    for k in 0..per_block {
                        if logical + k >= blocks {
                            break;
                        }
                        let q = u64::from(le32(&indirect, k as usize * 4));
                        if q != 0 {
                            out.push((logical + k, q, 1));
                        }
                    }
                }
                logical += per_block;
            }
        }
        if blocks > 12 + per_block + per_block * per_block && ptr(14) != 0 {
            return Err("a triply indirect directory".to_string());
        }
    }
    Ok(out)
}

/// The extents of an extent tree whose node is `node` (an inode's `i_block`, or a block read),
/// leaves appended to `out` as `(logical, physical, count)`.
fn extents_into(
    fs: &mut Fs,
    node: &[u8],
    out: &mut Vec<(u64, u64, u64)>,
    depth_seen: u32,
) -> Result<(), String> {
    if node.len() < 12 || le16(node, 0) != EXTENT_MAGIC {
        return Err("an extent header that is not one".to_string());
    }
    let entries = le16(node, 2) as usize;
    let depth = le16(node, 6);
    if depth_seen > 8 {
        return Err("an extent tree deeper than eight".to_string());
    }
    for i in 0..entries {
        let at = 12 + i * 12;
        if at + 12 > node.len() {
            break;
        }
        if depth == 0 {
            let logical = u64::from(le32(node, at));
            let len = u64::from(le16(node, at + 4) & 0x7FFF);
            let start = u64::from(le32(node, at + 8)) | (u64::from(le16(node, at + 6)) << 32);
            out.push((logical, start, len));
        } else {
            let leaf = u64::from(le32(node, at + 4)) | (u64::from(le16(node, at + 8)) << 32);
            let child = fs.read_block(leaf)?;
            extents_into(fs, &child, out, depth_seen + 1)?;
        }
    }
    Ok(())
}

/// The entries in one directory block (or the inline data): `(inode, name)`, without `.`
/// and `..`, and without the empty entries htree index blocks and checksum tails are.
fn parse_dir_entries(block: &[u8], block_size: usize, into: &mut Entries) {
    let mut at = 0usize;
    while at + 8 <= block.len() {
        let ino = u64::from(le32(block, at));
        let mut rec_len = le16(block, at + 4) as usize;
        let name_len = block[at + 6] as usize;
        // 65535 spells 65536 on 64 KiB blocks; 0 would loop forever.
        if rec_len == 65535 || rec_len == 0 {
            rec_len = block_size;
        }
        if ino != 0 && name_len > 0 && at + 8 + name_len <= block.len() {
            let name = &block[at + 8..at + 8 + name_len];
            if name != b"." && name != b".." {
                into.push((ino, name.to_vec()));
            }
        }
        at += rec_len;
    }
}

// ---- the walk ----

/// Whether the filesystem holding `root` can be walked from its device by this process.
/// Cheap to say no: a `statfs`, then one `open` that fails without the right.
fn device_fs(root: &Path) -> Option<Fs> {
    let stat = ::rustix::fs::statfs(root).ok()?;
    if super::linux::filesystem::magic_of(&stat) != u32::from(MAGIC) {
        return None;
    }
    let device = device_for(root).ok()?;
    Fs::open(&device).ok()
}

/// A directory's entries as parsed: `(inode, name)`.
type Entries = Vec<(u64, Vec<u8>)>;

/// The reader's reason for stopping when the consumer has gone: the viewer quit, and nothing
/// is wrong.
const CONSUMER_GONE: &str = "the consumer went away";

/// Whether a scan of `root` by this process would read the device: ext4, and the device
/// opens. For the benchmark's report.
#[must_use]
pub fn would_read_device(root: &Path) -> bool {
    device_fs(root).is_some()
}

/// A directory the walk has reached but not yet read.
struct Pending {
    ino: u64,
    path: Arc<Path>,
    depth: usize,
}

/// The walk of an ext4 filesystem from its device, one [`DirEntries`] per directory.
pub struct Ext4Walk {
    batches: Receiver<Vec<DirEntries>>,
    current: ::std::vec::IntoIter<DirEntries>,
    reader: Option<JoinHandle<()>>,
}

impl Iterator for Ext4Walk {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        loop {
            if let Some(next) = self.current.next() {
                return Some(next);
            }
            self.current = self.batches.recv().ok()?.into_iter();
        }
    }
}

impl Drop for Ext4Walk {
    fn drop(&mut self) {
        // Let the reader see the channel close, then wait for it.
        drop(::std::mem::replace(&mut self.batches, sync_channel(1).1));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Whether a directory's shape is one this reads.
fn dir_is_readable(fs: &Fs, dir: &DirInode) -> bool {
    !(dir.flags & INODE_INLINE_DATA_FL != 0 && dir.size > 60
        || dir.flags & INODE_EXTENTS_FL == 0
            && dir.size > (12 + 1024 + 1024 * 1024) * u64::from(fs.block_size))
}

/// Walk `root` from its device, if it is on ext4 and the device opens; `None` means the kernel
/// walk should be used instead, and nothing has been read that matters.
pub fn walk_ext4(root: &Path, options: ScanOptions) -> Option<Ext4Walk> {
    let root: PathBuf = root.canonicalize().ok()?;
    let mut fs = device_fs(&root)?;
    let root_ino = ::std::fs::metadata(&root).ok()?.ino();
    let mut inodes = Inodes::default();
    inodes.fetch(&mut fs, &mut vec![root_ino], 1).ok()?;
    let root_dir = inodes.dirs.get(&root_ino)?;
    if !dir_is_readable(&fs, root_dir) {
        return None;
    }
    let root: Arc<Path> = Arc::from(root.as_path());
    let (sender, batches) = sync_channel(64);
    let reader = ::std::thread::Builder::new()
        .name("ext4_reader".to_string())
        .spawn(move || {
            if let Err(error) = read_tree(fs, inodes, root, root_ino, options, &sender)
                && error != CONSUMER_GONE
            {
                // Nothing to fall back to once directories have been handed on; say so.
                eprintln!("diskonaut: reading the device stopped: {error}");
            }
        })
        .ok()?;
    Some(Ext4Walk {
        batches,
        current: Vec::new().into_iter(),
        reader: Some(reader),
    })
}

/// Read the directories under `root` a generation at a time and send each as it is parsed.
fn read_tree(
    mut fs: Fs,
    mut inodes: Inodes,
    root: Arc<Path>,
    root_ino: u64,
    options: ScanOptions,
    sender: &SyncSender<Vec<DirEntries>>,
) -> Result<(), String> {
    // Mount points strictly inside the scan root are other filesystems: the kernel walk's.
    let mounts: Vec<PathBuf> = super::linux::mounts::read()
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.point)
        .filter(|point| point.starts_with(&*root) && point.as_path() != &*root)
        .collect();
    let threads = super::thread_count(options).clamp(1, 16);
    let block_size = u64::from(fs.block_size);
    // Printed under `--benchmark --bench-profile`, with the build profile.
    let trace = libdiskonaut::model::files::profile::enabled();
    let mut generation = 0u32;

    let mut frontier = vec![Pending {
        ino: root_ino,
        path: Arc::clone(&root),
        depth: 0,
    }];
    let mut outbox: Vec<DirEntries> = Vec::new();
    let mut outbox_entries = 0usize;
    let send = |outbox: &mut Vec<DirEntries>| -> Result<(), String> {
        sender
            .send(::std::mem::take(outbox))
            .map_err(|_| CONSUMER_GONE.to_string())
    };

    while !frontier.is_empty() {
        let started = Instant::now();
        // Where this generation's directories keep their blocks: `(block, frontier index)`.
        let mut wanted: Vec<(u64, usize)> = Vec::new();
        let mut entries_of: Vec<Entries> = (0..frontier.len()).map(|_| Vec::new()).collect();
        // Directories of a shape this does not read, handed to the kernel walker whole, like a
        // mount point; their own entry was already given by the parent's listing.
        let mut handed_over: Vec<PathBuf> = Vec::new();
        for (index, pending) in frontier.iter().enumerate() {
            let Some(dir) = inodes.dirs.get(&pending.ino).cloned() else {
                continue;
            };
            if !dir_is_readable(&fs, &dir) {
                handed_over.push(pending.path.to_path_buf());
                continue;
            }
            if dir.flags & INODE_INLINE_DATA_FL != 0 {
                parse_dir_entries(&dir.block[4..], block_size as usize, &mut entries_of[index]);
                continue;
            }
            for (_, physical, count) in dir_blocks(&mut fs, &dir)? {
                for i in 0..count {
                    wanted.push((physical + i, index));
                }
            }
        }
        wanted.sort_unstable();
        let blocks: Vec<u64> = wanted.iter().map(|w| w.0).collect();
        let runs = runs_of(&blocks, block_size, 8);
        let bytes: u64 = runs.iter().map(|r| r.1).sum();
        {
            let fs_ref: &Fs = &fs;
            let wanted_ref = &wanted;
            let parsed = sweep(fs_ref, &runs, threads, |start, buffer| {
                // Every wanted block inside this run, parsed for its directory.
                let first = start / block_size;
                let from = wanted_ref.partition_point(|w| w.0 < first);
                let mut out: Vec<(usize, Entries)> = Vec::new();
                for &(block, index) in &wanted_ref[from..] {
                    let at = ((block - first) * block_size) as usize;
                    if at + block_size as usize > buffer.len() {
                        break;
                    }
                    let mut entries = Vec::new();
                    parse_dir_entries(
                        &buffer[at..at + block_size as usize],
                        block_size as usize,
                        &mut entries,
                    );
                    out.push((index, entries));
                }
                out
            })?;
            for (index, entries) in parsed.into_iter().flatten() {
                entries_of[index].extend(entries);
            }
        }
        fs.bytes_read += bytes;
        let dirs_done = Instant::now();

        // The inodes those names point at, in one sweep.
        let mut children: Vec<u64> = entries_of.iter().flatten().map(|(ino, _)| *ino).collect();
        let names = children.len();
        let before = fs.bytes_read;
        inodes.fetch(&mut fs, &mut children, threads)?;
        if trace {
            eprintln!(
                "  ext4 generation {generation}: {} dirs, {} blocks in {} runs ({} MiB) {:.3}s; {} names, {} inodes fetched ({} MiB) {:.3}s",
                frontier.len(),
                blocks.len(),
                runs.len(),
                bytes >> 20,
                dirs_done.duration_since(started).as_secs_f64(),
                names,
                children.len(),
                (fs.bytes_read - before) >> 20,
                dirs_done.elapsed().as_secs_f64(),
            );
        }
        generation += 1;

        // Hand each directory on, and gather the next generation: the batches are built on
        // several threads, a slice of the generation each, since this is where the names are
        // copied and the entries looked up, and a generation can be sixty thousand directories.
        let emit_started = Instant::now();
        let built: Vec<(Vec<DirEntries>, Vec<Pending>, Vec<PathBuf>)> = {
            let inodes_ref = &inodes;
            let mounts_ref = &mounts;
            let frontier_ref = &frontier;
            let jobs: Vec<(usize, Entries)> = entries_of.into_iter().enumerate().collect();
            let per_thread = jobs.len().div_ceil(threads).max(1);
            ::std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .chunks(per_thread)
                    .map(|chunk| {
                        scope.spawn(move || {
                            let mut directories = Vec::with_capacity(chunk.len());
                            let mut next = Vec::new();
                            let mut mounted = Vec::new();
                            for (index, entries) in chunk {
                                let pending = &frontier_ref[*index];
                                let mut directory = DirEntries::with_capacity(
                                    Arc::clone(&pending.path),
                                    entries.len(),
                                    entries.iter().map(|(_, name)| name.len()).sum(),
                                );
                                let descend =
                                    options.max_depth.is_none_or(|max| pending.depth + 1 < max);
                                for (ino, name) in entries {
                                    let Some(meta) = inodes_ref.metas.get(ino).copied() else {
                                        directory.failed += 1;
                                        continue;
                                    };
                                    let is_dir = meta.mode & S_IFMT == S_IFDIR;
                                    let name = OsStr::from_bytes(name);
                                    directory.push(
                                        name,
                                        EntryMeta {
                                            size: meta.on_disk,
                                            apparent: meta.size,
                                            inode: *ino,
                                            links: u64::from(meta.links),
                                            is_dir,
                                            shared_extent: 0,
                                        },
                                    );
                                    if is_dir && descend {
                                        let path = pending.path.join(name);
                                        if mounts_ref.iter().any(|m| m.as_path() == path.as_path())
                                        {
                                            mounted.push(path);
                                            continue;
                                        }
                                        next.push(Pending {
                                            ino: *ino,
                                            path: Arc::from(path.as_path()),
                                            depth: pending.depth + 1,
                                        });
                                    }
                                }
                                directory.shrink();
                                directories.push(directory);
                            }
                            (directories, next, mounted)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|panic| ::std::panic::resume_unwind(panic))
                    })
                    .collect()
            })
        };
        let mut next = Vec::new();
        for (directories, more, mounted) in built {
            for directory in directories {
                outbox_entries += directory.len().max(1);
                outbox.push(directory);
                if outbox_entries >= SEND_BATCH {
                    outbox_entries = 0;
                    send(&mut outbox)?;
                }
            }
            next.extend(more);
            for path in mounted.into_iter().chain(handed_over.drain(..)) {
                // Another filesystem is mounted here, or a directory this does not read: what is
                // under it is the kernel walk's to read, if the scan would enter it at all.
                if super::linux::walk_would_enter(&root, &path, options) {
                    let depth = path
                        .strip_prefix(&*root)
                        .map_or(0, |p| p.components().count());
                    let mut below = options;
                    below.max_depth = options.max_depth.map(|max| max.saturating_sub(depth));
                    for sub in super::linux::walk_linux(&path, super::thread_count(options), below)
                    {
                        outbox_entries += sub.len().max(1);
                        outbox.push(sub);
                        if outbox_entries >= SEND_BATCH {
                            outbox_entries = 0;
                            send(&mut outbox)?;
                        }
                    }
                }
            }
        }
        if trace {
            eprintln!(
                "  ext4 generation {}: emitted in {:.3}s",
                generation - 1,
                emit_started.elapsed().as_secs_f64()
            );
        }
        // What the finished generation's directories held is no longer needed.
        for pending in &frontier {
            inodes.dirs.remove(&pending.ino);
        }
        frontier = next;
    }
    if !outbox.is_empty() {
        send(&mut outbox)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dentry(ino: u32, name: &[u8], rec_len: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(ino.to_le_bytes());
        out.extend(rec_len.to_le_bytes());
        out.push(name.len() as u8);
        out.push(1);
        out.extend(name);
        out.resize(rec_len as usize, 0);
        out
    }

    #[test]
    fn directory_block_yields_names_and_skips_dots_and_empties() {
        let mut block = Vec::new();
        block.extend(dentry(2, b".", 12));
        block.extend(dentry(2, b"..", 12));
        block.extend(dentry(12, b"hello.txt", 20));
        block.extend(dentry(0, b"deleted", 16)); // a removed entry: inode 0
        block.extend(dentry(13, b"sub", 4096 - 60)); // the last entry spans the rest
        let mut entries = Vec::new();
        parse_dir_entries(&block, 4096, &mut entries);
        assert_eq!(
            entries,
            vec![(12, b"hello.txt".to_vec()), (13, b"sub".to_vec())]
        );
    }

    #[test]
    fn checksum_tail_and_index_block_are_empty() {
        // An htree index block: one entry of inode 0 spanning the block.
        let block = dentry(0, b"", 4096);
        let mut entries = Vec::new();
        parse_dir_entries(&block, 4096, &mut entries);
        assert!(entries.is_empty());
        // A checksum tail: inode 0, rec_len 12, name_len 0, file_type 0xDE.
        let mut tail = dentry(0, b"", 12);
        tail[7] = 0xDE;
        parse_dir_entries(&tail, 4096, &mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn inline_extent_leaves_are_read_from_i_block() {
        // A depth-0 extent header with two leaves.
        let mut node = Vec::new();
        node.extend(EXTENT_MAGIC.to_le_bytes());
        node.extend(2u16.to_le_bytes()); // entries
        node.extend(4u16.to_le_bytes()); // max
        node.extend(0u16.to_le_bytes()); // depth
        node.extend(0u32.to_le_bytes()); // generation
        for (logical, len, start_hi, start_lo) in [(0u32, 1u16, 0u16, 1000u32), (1, 3, 1, 2000)] {
            node.extend(logical.to_le_bytes());
            node.extend(len.to_le_bytes());
            node.extend(start_hi.to_le_bytes());
            node.extend(start_lo.to_le_bytes());
        }
        node.resize(60, 0);
        // No device is needed for depth 0, so a closed handle will do.
        let mut fs = Fs {
            file: File::open("/dev/null").expect("open"),
            device: PathBuf::from("/dev/null"),
            block_size: 4096,
            inodes_per_group: 8192,
            first_ino: 11,
            inode_size: 256,
            huge_file: false,
            tables: Vec::new(),
            bytes_read: 0,
        };
        let mut out = Vec::new();
        extents_into(&mut fs, &node, &mut out, 0).expect("extents");
        assert_eq!(out, vec![(0, 1000, 1), (1, (1 << 32) + 2000, 3)]);
    }

    /// As root on ext4, the device walk and the kernel walk must agree on a tree, entry for
    /// entry. Elsewhere this passes without looking.
    #[test]
    fn device_walk_agrees_with_kernel_walk() {
        let dir = std::env::temp_dir().join("diskonaut_ext4_device_walk");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b/c")).expect("mkdir");
        std::fs::write(dir.join("a/one"), vec![1u8; 10_000]).expect("write");
        std::fs::write(dir.join("a/b/two"), vec![2u8; 70_000]).expect("write");
        std::fs::write(dir.join("a/b/c/three"), b"3").expect("write");
        std::fs::hard_link(dir.join("a/one"), dir.join("a/b/c/one-again")).expect("link");
        // On disk before the device is read: what the kernel has not written yet is not there.
        for name in ["a/one", "a/b/two", "a/b/c/three"] {
            File::open(dir.join(name))
                .expect("open")
                .sync_all()
                .expect("sync");
        }
        let Some(device) = walk_ext4(&dir, ScanOptions::default()) else {
            eprintln!("not root on ext4: skipped");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        type Seen = Vec<(PathBuf, Vec<(String, u64, u64, bool)>)>;
        let mut from_device: Seen = device
            .map(|d| {
                let mut entries: Vec<(String, u64, u64, bool)> = d
                    .entries()
                    .iter()
                    .map(|e| {
                        (
                            d.name(e).to_string_lossy().to_string(),
                            e.meta.size,
                            e.meta.inode,
                            e.meta.is_dir,
                        )
                    })
                    .collect();
                entries.sort();
                (d.path.to_path_buf(), entries)
            })
            .collect();
        from_device.sort();
        let mut from_kernel: Seen =
            super::super::linux::walk_linux(&dir, 2, ScanOptions::default())
                .map(|d| {
                    let mut entries: Vec<(String, u64, u64, bool)> = d
                        .entries()
                        .iter()
                        .map(|e| {
                            (
                                d.name(e).to_string_lossy().to_string(),
                                e.meta.size,
                                e.meta.inode,
                                e.meta.is_dir,
                            )
                        })
                        .collect();
                    entries.sort();
                    (d.path.to_path_buf(), entries)
                })
                .collect();
        from_kernel.sort();
        assert_eq!(from_device, from_kernel);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
