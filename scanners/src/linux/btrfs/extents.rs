//! What a btrfs file's extents occupy on disk, compression and holes accounted for.
//!
//! `stat` cannot say: btrfs reports `st_blocks` as the uncompressed size, so 64 MiB of text under
//! `compress-force=zstd` that holds 2 MiB of data reads as 64 MiB. The file-extent items in the
//! filesystem tree carry the answer — `disk_num_bytes` for what an extent takes, `num_bytes` for
//! how much of it this file refers to — and `BTRFS_IOC_TREE_SEARCH_V2` reads them, one call per
//! file on the directory's descriptor, without opening the file. It needs `CAP_SYS_ADMIN`, so an
//! ordinary user's scan keeps `st_blocks`; this is what `compsize` reads too.

use ::std::os::fd::{AsRawFd, BorrowedFd};
use ::std::path::Path;

/// `_IOWR(0x94, 17, struct btrfs_ioctl_search_args_v2)`, a 112-byte header before the buffer.
const BTRFS_IOC_TREE_SEARCH_V2: libc::Ioctl = 0xc070_9411_u32 as libc::Ioctl;
const EXTENT_DATA_KEY: u32 = 108;
/// `struct btrfs_ioctl_search_key`, then `buf_size`.
const ARGS: usize = 112;
const HEADER: usize = 32;
/// Bytes of a file-extent item before `disk_bytenr`: all an inline extent has before its data.
const EXTENT_HEAD: usize = 21;
/// A regular or preallocated extent item: the head and four `u64`s.
const EXTENT_ITEM: usize = EXTENT_HEAD + 32;
const BUFFER: usize = 16 * 1024;

/// Whether this caller may search the tree `path` is on: `CAP_SYS_ADMIN`, in practice root.
pub fn allowed(path: &Path) -> bool {
    use ::rustix::fs::{Mode, OFlags, open};
    let Ok(dir) = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) else {
        return false;
    };
    let mut args = Args::new(0, 0);
    args.set_items(1);
    // SAFETY: `args` is laid out as the kernel expects and holds `BUFFER` bytes after it.
    unsafe {
        libc::ioctl(
            dir.as_raw_fd(),
            BTRFS_IOC_TREE_SEARCH_V2,
            args.bytes.as_mut_ptr(),
        ) == 0
    }
}

/// What the extents of inode `inode`, in the subvolume `dir` is in, occupy on disk; `None` if
/// they cannot be read.
pub fn on_disk(dir: BorrowedFd<'_>, inode: u64) -> Option<u64> {
    // One buffer per walker thread, reused for every file: this runs once a file.
    thread_local! {
        static ARGS_BUFFER: ::std::cell::RefCell<Args> =
            ::std::cell::RefCell::new(Args::new(0, 0));
    }
    ARGS_BUFFER.with(|args| on_disk_with(dir, inode, &mut args.borrow_mut()))
}

fn on_disk_with(dir: BorrowedFd<'_>, inode: u64, args: &mut Args) -> Option<u64> {
    let mut total = 0u64;
    let mut from = 0u64;
    loop {
        args.ask(inode, from);
        // SAFETY: `args` is laid out as the kernel expects and holds `BUFFER` bytes after it.
        let result = unsafe {
            libc::ioctl(
                dir.as_raw_fd(),
                BTRFS_IOC_TREE_SEARCH_V2,
                args.bytes.as_mut_ptr(),
            )
        };
        if result != 0 {
            return None;
        }
        let parsed = parse(&args.bytes[ARGS..], args.items())?;
        total = total.saturating_add(parsed.bytes);
        // The kernel stops when the items run out or the buffer fills. Room left for another
        // item means they ran out — most files, answered in one call.
        let room_for_more = parsed.used + HEADER + EXTENT_ITEM <= BUFFER;
        match parsed.last {
            Some(offset) if offset < u64::MAX && !room_for_more => from = offset + 1,
            _ => return Some(total),
        }
    }
}

/// What one search returned: what its extents occupy, the file offset of the last one (to
/// continue from), and how much of the buffer the items filled.
pub(crate) struct Parsed {
    pub bytes: u64,
    pub last: Option<u64>,
    pub used: usize,
}

/// Sum what the extent items in `buffer` occupy.
pub(crate) fn parse(buffer: &[u8], items: u32) -> Option<Parsed> {
    let u64_at = |at: usize| -> Option<u64> {
        Some(u64::from_ne_bytes(buffer.get(at..at + 8)?.try_into().ok()?))
    };
    let mut at = 0usize;
    let mut total = 0u64;
    let mut last = None;
    for _ in 0..items {
        let offset = u64_at(at + 16)?;
        let len = u32::from_ne_bytes(buffer.get(at + 28..at + 32)?.try_into().ok()?) as usize;
        let item = buffer.get(at + HEADER..at + HEADER + len)?;
        at += HEADER + len;
        last = Some(offset);
        let ram_bytes = u64::from_ne_bytes(item.get(8..16)?.try_into().ok()?);
        let compression = *item.get(16)?;
        let kind = *item.get(20)?;
        if kind == 0 {
            // Inline: the data is in the item itself, compressed or not.
            total = total.saturating_add((len - EXTENT_HEAD.min(len)) as u64);
            continue;
        }
        let field = |index: usize| -> Option<u64> {
            Some(u64::from_ne_bytes(
                item.get(EXTENT_HEAD + index * 8..EXTENT_HEAD + index * 8 + 8)?
                    .try_into()
                    .ok()?,
            ))
        };
        let (disk_bytenr, disk_num_bytes, num_bytes) = (field(0)?, field(1)?, field(3)?);
        if disk_bytenr == 0 {
            continue; // a hole
        }
        total = total.saturating_add(if compression == 0 || ram_bytes == 0 {
            num_bytes
        } else {
            // A compressed extent is stored whole; this file's share is the part it refers to.
            (u128::from(disk_num_bytes) * u128::from(num_bytes)).div_ceil(u128::from(ram_bytes))
                as u64
        });
    }
    Some(Parsed {
        bytes: total,
        last,
        used: at,
    })
}

/// `struct btrfs_ioctl_search_args_v2` with its buffer, asking for inode `inode`'s extent
/// items from file offset `from` on, in the subvolume of the descriptor it is used on.
struct Args {
    bytes: Vec<u8>,
}

impl Args {
    fn new(inode: u64, from: u64) -> Self {
        let mut args = Self {
            bytes: vec![0u8; ARGS + BUFFER],
        };
        args.ask(inode, from);
        args
    }
    /// Set the key to ask for inode `inode`'s extent items from file offset `from` on. Only
    /// the key is rewritten; the next answer overwrites what the last one left in the buffer.
    fn ask(&mut self, inode: u64, from: u64) {
        let bytes = &mut self.bytes;
        let mut put =
            |at: usize, value: u64| bytes[at..at + 8].copy_from_slice(&value.to_ne_bytes());
        put(0, 0); // tree_id: the descriptor's subvolume
        put(8, inode); // min_objectid
        put(16, inode); // max_objectid
        put(24, from); // min_offset
        put(32, u64::MAX); // max_offset
        put(40, 0); // min_transid
        put(48, u64::MAX); // max_transid
        put(104, BUFFER as u64); // buf_size
        bytes[56..60].copy_from_slice(&EXTENT_DATA_KEY.to_ne_bytes()); // min_type
        bytes[60..64].copy_from_slice(&EXTENT_DATA_KEY.to_ne_bytes()); // max_type
        self.set_items(u32::MAX);
    }
    fn set_items(&mut self, items: u32) {
        self.bytes[64..68].copy_from_slice(&items.to_ne_bytes());
    }
    /// How many items the kernel returned.
    fn items(&self) -> u32 {
        u32::from_ne_bytes(self.bytes[64..68].try_into().expect("four bytes"))
    }
}
