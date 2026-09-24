//! ext4's metadata read from the device: what a walk asks the kernel for, file by file, taken
//! from the inode tables in a few large sequential reads instead. Root, or the `disk` group.
//!
//! This is the spike of `docs/scan-roadmap.md` step 1: no names and no tree yet, only every
//! live inode's size, summed and timed, to find the floor a device-reading walker could reach
//! and to check that the sum agrees with a scan. The layout followed is the one in the kernel's
//! `fs/ext4/ext4.h`: the superblock at byte 1024, the group descriptor table in the block after
//! it, and per group an inode table whose used part `bg_itable_unused` bounds.
//!
//! What is read is the device's page cache, which is the buffer cache ext4 itself reads its
//! metadata through, so it is as current as the disk: delayed allocation and an uncheckpointed
//! journal mean the last seconds of writes are not in it yet.

use ::std::fs::File;
use ::std::os::unix::fs::{FileExt, MetadataExt};
use ::std::path::{Path, PathBuf};
use ::std::time::{Duration, Instant};

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
const S_IFMT: u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;

/// What the inode tables held.
#[derive(Debug, Default)]
pub struct Survey {
    /// Live inodes: mode set, a link count, not deleted.
    pub inodes: u64,
    pub files: u64,
    pub directories: u64,
    /// `i_blocks` summed, in bytes: what a disk-usage scan reports.
    pub bytes_on_disk: u64,
    /// `i_size` of the regular files summed: the apparent total.
    pub apparent: u64,
    pub groups: u32,
    pub groups_read: u32,
    /// Bytes read from the device.
    pub bytes_read: u64,
    pub elapsed: Duration,
    pub device: PathBuf,
    pub block_size: u32,
    pub inode_size: u16,
}

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

/// Survey the ext4 filesystem holding `path`.
pub fn survey_for(path: &Path) -> Result<Survey, String> {
    let device = device_for(path)?;
    survey(&device)
}

/// Read the inode tables of the ext4 filesystem on `device` and sum what they hold.
pub fn survey(device: &Path) -> Result<Survey, String> {
    let started = Instant::now();
    let file = File::open(device).map_err(|e| format!("cannot open {}: {e}", device.display()))?;
    let mut read = 0u64;

    let mut sb = vec![0u8; SUPERBLOCK_LEN];
    file.read_exact_at(&mut sb, SUPERBLOCK_OFFSET)
        .map_err(|e| format!("superblock: {e}"))?;
    read += SUPERBLOCK_LEN as u64;
    if le16(&sb, 56) != MAGIC {
        return Err(format!("{} is not ext2/3/4", device.display()));
    }
    let inodes_count = le32(&sb, 0);
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
        return Err("META_BG filesystems are not followed by this spike".to_string());
    }
    let desc_size = if incompat & INCOMPAT_64BIT != 0 {
        le16(&sb, 254).max(64) as usize
    } else {
        32
    };
    let huge_file = ro_compat & RO_COMPAT_HUGE_FILE != 0;
    // `bg_itable_unused` is only kept up to date with a group descriptor checksum.
    let unused_is_valid = ro_compat & (RO_COMPAT_GDT_CSUM | RO_COMPAT_METADATA_CSUM) != 0;
    if inodes_per_group == 0 || blocks_per_group == 0 || inode_size < 128 {
        return Err("superblock does not add up".to_string());
    }
    let groups = u32::try_from(
        blocks_count
            .saturating_sub(u64::from(first_data_block))
            .div_ceil(u64::from(blocks_per_group)),
    )
    .map_err(|_| "too many groups".to_string())?;

    // The group descriptor table, in the block after the superblock.
    let gdt_block = u64::from(first_data_block) + 1;
    let mut gdt = vec![0u8; groups as usize * desc_size];
    file.read_exact_at(&mut gdt, gdt_block * u64::from(block_size))
        .map_err(|e| format!("group descriptors: {e}"))?;
    read += gdt.len() as u64;

    let mut out = Survey {
        groups,
        device: device.to_path_buf(),
        block_size,
        inode_size,
        ..Survey::default()
    };
    let mut table = Vec::new();
    for group in 0..groups {
        let desc = &gdt[group as usize * desc_size..(group as usize + 1) * desc_size];
        let flags = le16(desc, 18);
        if flags & BG_INODE_UNINIT != 0 {
            continue;
        }
        let mut inode_table = u64::from(le32(desc, 8));
        let mut unused = u32::from(le16(desc, 28));
        if desc_size >= 64 {
            inode_table |= u64::from(le32(desc, 40)) << 32;
            unused |= u32::from(le16(desc, 50)) << 16;
        }
        let used = if unused_is_valid {
            inodes_per_group.saturating_sub(unused)
        } else {
            inodes_per_group
        };
        if used == 0 {
            continue;
        }
        let len = used as usize * inode_size as usize;
        table.resize(len, 0);
        file.read_exact_at(&mut table, inode_table * u64::from(block_size))
            .map_err(|e| format!("inode table of group {group}: {e}"))?;
        read += len as u64;
        out.groups_read += 1;

        let first_in_group = u64::from(group) * u64::from(inodes_per_group) + 1;
        for (i, inode) in table.chunks_exact(inode_size as usize).enumerate() {
            let ino = first_in_group + i as u64;
            // Reserved inodes: the journal, the resize inode, and the rest below `first_ino`,
            // which no directory lists. The root directory (2) is the one exception.
            if ino != 2 && ino < u64::from(first_ino) {
                continue;
            }
            let mode = le16(inode, 0);
            let links = le16(inode, 26);
            let dtime = le32(inode, 20);
            if mode == 0 || links == 0 || dtime != 0 {
                continue;
            }
            let flags = le32(inode, 32);
            let mut blocks = u64::from(le32(inode, 28));
            if inode_size >= 128 {
                blocks |= u64::from(le16(inode, 116)) << 32;
            }
            let bytes = if huge_file && flags & INODE_HUGE_FILE_FL != 0 {
                blocks * u64::from(block_size)
            } else {
                blocks * 512
            };
            let size = u64::from(le32(inode, 4)) | (u64::from(le32(inode, 108)) << 32);
            out.inodes += 1;
            out.bytes_on_disk += bytes;
            match mode & S_IFMT {
                S_IFDIR => out.directories += 1,
                S_IFREG => {
                    out.files += 1;
                    out.apparent += size;
                }
                _ => {}
            }
        }
    }
    let _ = inodes_count;
    out.bytes_read = read;
    out.elapsed = started.elapsed();
    Ok(out)
}
