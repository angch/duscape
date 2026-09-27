//! Copy-on-write sharing, asked about one file at a time.
//!
//! There is no bulk answer available to an ordinary user: `XFS_IOC_GETFSMAP` would give the whole
//! volume's extent ownership in one sweep, but the kernel redacts every owner for callers without
//! `CAP_SYS_ADMIN`, and `XFS_IOC_BULKSTAT` is refused outright. `FS_IOC_FIEMAP` needs the file
//! open, so the cost is an `openat` and an `ioctl` per file asked about — which is why only files
//! big enough to matter are asked about at all.

use ::std::ffi::CStr;
use ::std::os::fd::{AsRawFd, BorrowedFd};

use ::rustix::fs::{Mode, OFlags, openat};

/// This extent is shared with another file.
const FIEMAP_EXTENT_SHARED: u32 = 0x0000_2000;
/// The last extent of the file; there is nothing beyond it.
const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
/// Extents asked for in one go; a longer map is read a page at a time.
const MAX_EXTENTS: usize = 64;
/// Extents read before giving up on a file, a page of [`MAX_EXTENTS`] at a time: a 32 GiB
/// file compressed in 128 KiB extents.
const MAX_EXTENTS_IN_ALL: usize = 1 << 18;
/// `_IOWR('f', 11, struct fiemap)`, where `struct fiemap` is 32 bytes. `ioctl`'s request type is
/// `c_ulong` on glibc but `c_int` on musl, so the bits are cast into whichever it is.
const FS_IOC_FIEMAP: libc::Ioctl = 0xC020_660B_u32 as libc::Ioctl;

/// Below this, a file is not worth an `openat` and an `ioctl` to ask about.
///
/// Reflinks of small files exist, but they cost the same two syscalls to find as a large one
/// and are worth a rounding error of the total. On a 4.2M-entry volume this leaves under 4% of
/// files to probe. The threshold is on blocks allocated, so a sparse file that is mostly hole
/// is judged on what it actually occupies.
pub const PROBE_ABOVE_BYTES: u64 = 64 * 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Extent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct Request {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [Extent; MAX_EXTENTS],
}

/// Where the first extent of the open file or directory `fd` is, as a byte offset into the
/// device, if it has one. Inline data (ext4 keeps a very small directory in its inode) has no
/// extent. Asks for one extent, so the cost is the `ioctl` alone.
pub fn first_extent(fd: BorrowedFd<'_>) -> Option<u64> {
    let mut request = Request {
        start: 0,
        length: u64::MAX,
        flags: 0,
        mapped_extents: 0,
        extent_count: 1,
        reserved: 0,
        extents: [Extent::default(); MAX_EXTENTS],
    };
    // SAFETY: `request` is a live, correctly shaped `struct fiemap` with room for the extent
    // it promises, and `fd` is open for the duration of the call.
    let result = unsafe { libc::ioctl(fd.as_raw_fd(), FS_IOC_FIEMAP, &raw mut request) };
    (result == 0 && request.mapped_extents >= 1).then_some(request.extents[0].physical)
}

/// An identity for `name`'s blocks, if every one of them is shared with another file.
///
/// The first extent alone is not enough, and assuming it was is a way to *understate*. Two
/// files of equal size that share only their opening extent — a reflink copy with its middle
/// overwritten, say — would be merged, and one of them counted as nothing. So the whole map is
/// read and folded into the identity, and anything less than wholly shared is refused:
///
/// * an extent that is not shared means the file is only partly a copy, so it is counted in
///   full, which overstates rather than understates
/// * no `LAST` flag means the file has more extents than were asked for, and what was not seen
///   cannot be vouched for
///
/// Two files agreeing on this identity have the same extents at the same places, so they are
/// the same blocks. The size check in the ledger still applies on top.
pub fn shared_identity(dir: BorrowedFd<'_>, name: &CStr, extent_space: u64) -> Option<u64> {
    let file = openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;

    // FNV-1a over the filesystem and then the extent map. The filesystem has to be in there:
    // a physical block offset means nothing without the address space it indexes, and the walk
    // probes every sharing filesystem it meets rather than only the scan root's. Two unrelated
    // files on two volumes sharing an offset and a size is otherwise an easy collision, and it
    // would merge them — the same undercount that keying on one extent used to cause. It is
    // the filesystem and not the device: see `Job::extent_space`.
    let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fold = |value: u64| {
        identity = (identity ^ value).wrapping_mul(0x0000_0100_0000_01b3);
    };
    fold(extent_space);

    // A page of extents at a time, until the last. btrfs compresses in 128 KiB extents, so a
    // compressed file has one per 128 KiB of it: stopping at the first page would leave every
    // compressed file over 8 MiB counted once per snapshot.
    let mut start = 0u64;
    let mut seen = 0usize;
    loop {
        let mut request = Request {
            start,
            length: u64::MAX - start,
            flags: 0,
            mapped_extents: 0,
            extent_count: MAX_EXTENTS as u32,
            reserved: 0,
            extents: [Extent::default(); MAX_EXTENTS],
        };
        // SAFETY: `request` is a live, correctly shaped `struct fiemap` with room for the
        // `extent_count` extents it promises, and `file` is open for the duration of the call.
        let result = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &raw mut request) };
        let mapped = request.mapped_extents as usize;
        if result != 0 || mapped == 0 || mapped > MAX_EXTENTS {
            return None;
        }
        let mut saw_last = false;
        for extent in &request.extents[..mapped] {
            if extent.flags & FIEMAP_EXTENT_SHARED == 0 {
                return None;
            }
            fold(extent.physical);
            fold(extent.length);
            saw_last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
        }
        if saw_last {
            break;
        }
        seen += mapped;
        let last = &request.extents[mapped - 1];
        let next = last.logical.saturating_add(last.length);
        // What was not seen cannot be vouched for: a file past the cap, or a map that does not
        // move forward, is counted in full.
        if seen >= MAX_EXTENTS_IN_ALL || next <= start {
            return None;
        }
        start = next;
    }

    // Zero is how `EntryMeta` spells "not shared", so it cannot also mean an identity.
    Some(if identity == 0 { 1 } else { identity })
}
