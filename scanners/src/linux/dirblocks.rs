//! Reading the directories' blocks ahead, through the block device. Root, in practice.
//!
//! A cold walk pays one synchronous 4 KiB read per directory for its data block, inside
//! `getdents64`, with no readahead across directories: 72% of every read a cold ext4 scan makes
//! (see `docs/scan-performance.md`, "Cold cache"). ext4 puts a directory's blocks in its inode's
//! block group and the walk reads children smallest inode first, so those blocks come in
//! contiguous runs of five to nine. Where the device can be opened for reading — root, or the
//! `disk` group — one `posix_fadvise(WILLNEED)` on it from the first block of a run fetches the
//! run in one read; the buffer cache ext4 reads directories from is the device's page cache, so
//! the siblings' blocks are then in core. Where it cannot be opened, nothing changes: the walk
//! is exactly what an unprivileged one is.
//!
//! `DUSCAPE_DIRBLOCKS_DEVICE=<path>` names the file to advise instead of the device, for
//! exercising this path without the device: on a regular file the advice is harmless.

use ::std::os::fd::{AsRawFd, OwnedFd};

use ::rustix::fs::{Mode, OFlags, open};

/// How much is read ahead from a run's first block. Runs average five to nine 4 KiB blocks;
/// what is read past the run is bandwidth spent for nothing, and a run longer than this is
/// read in two.
const WINDOW: u64 = 32 * 1024;

/// A block device open for reading.
pub struct Device {
    fd: OwnedFd,
}

impl Device {
    /// The device `st_dev` names, if it can be opened for reading.
    pub fn open(device: u64) -> Option<Self> {
        if let Ok(path) = ::std::env::var("DUSCAPE_DIRBLOCKS_DEVICE") {
            let fd = open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).ok()?;
            return Some(Self { fd });
        }
        #[allow(clippy::cast_possible_truncation)]
        let (major, minor) = (
            libc::major(device as libc::dev_t),
            libc::minor(device as libc::dev_t),
        );
        let uevent =
            ::std::fs::read_to_string(format!("/sys/dev/block/{major}:{minor}/uevent")).ok()?;
        let name = uevent
            .lines()
            .find_map(|line| line.strip_prefix("DEVNAME="))?;
        let fd = open(
            format!("/dev/{name}"),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()?;
        // The device the filesystem is on, and nothing else, whatever `/dev` says.
        let stat = ::rustix::fs::fstat(&fd).ok()?;
        // `st_rdev` is not the same width on every libc.
        #[allow(clippy::unnecessary_cast)]
        let rdev = stat.st_rdev as u64;
        (rdev == device).then_some(Self { fd })
    }
}

/// One worker's view of what has been asked for: the end of the last window, so that the
/// directories inside it are not asked for again. Workers walk different subtrees, so each
/// keeps its own.
#[derive(Default)]
pub struct Prefetcher {
    /// Where the last window started and ended.
    window: Option<(u64, u64)>,
}

impl Prefetcher {
    /// A directory's first block is at byte `at`: read the window from there, unless the
    /// last one covers it.
    pub fn ahead(&mut self, device: &Device, at: u64) {
        if let Some((start, end)) = self.window
            && (start..end).contains(&at)
        {
            return;
        }
        #[allow(clippy::cast_possible_wrap)]
        // SAFETY: `posix_fadvise` takes a descriptor and three integers, and `fd` is open.
        unsafe {
            libc::posix_fadvise(
                device.fd.as_raw_fd(),
                at as libc::off_t,
                WINDOW as libc::off_t,
                libc::POSIX_FADV_WILLNEED,
            );
        }
        self.window = Some((at, at.saturating_add(WINDOW)));
    }
}
