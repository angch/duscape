//! `statx`, and what stands in for it on a kernel without one.

use ::std::mem::MaybeUninit;
use ::std::os::fd::AsFd;
use ::std::sync::atomic::{AtomicBool, Ordering};

use ::rustix::fs::{AtFlags, StatxFlags};

/// Set once `statx` has said the kernel has none, so that the walk stops asking. Set from the
/// start by `DUSCAPE_NO_STATX` (any value), which makes a scan read as it would on a kernel
/// before 4.11: for testing that path on a new kernel, and a way around a `statx` that misbehaves.
pub(super) static NO_STATX: AtomicBool = AtomicBool::new(false);

/// `statx`, or on a kernel that has none — before Linux 4.11, which Synology's DSM still runs,
/// among others — `fstatat`, answered in `statx`'s shape. Old container seccomp profiles refuse
/// `statx` with `EPERM` instead, which is no file's permission error (that is `EACCES`): there
/// `fstatat` is tried, and used from then on if it answers. Without it every entry of such a
/// kernel failed to read. What `fstatat` cannot say is left empty and not claimed in `stx_mask`:
/// the attributes (compression, mount roots) and the mount id, which the walk already goes
/// without on kernels before 5.8.
pub(crate) fn statx<P: ::rustix::path::Arg + Copy, Fd: AsFd>(
    dir: Fd,
    path: P,
    flags: AtFlags,
    mask: StatxFlags,
) -> ::rustix::io::Result<::rustix::fs::Statx> {
    if !NO_STATX.load(Ordering::Relaxed) {
        match ::rustix::fs::statx(dir.as_fd(), path, flags, mask) {
            Err(::rustix::io::Errno::NOSYS) => NO_STATX.store(true, Ordering::Relaxed),
            Err(::rustix::io::Errno::PERM) => {
                let stat = ::rustix::fs::statat(dir, path, flags)?;
                NO_STATX.store(true, Ordering::Relaxed);
                return Ok(statx_from_stat(&stat));
            }
            other => return other,
        }
    }
    ::rustix::fs::statat(dir, path, flags).map(|stat| statx_from_stat(&stat))
}

/// `fstatat`'s answer as a `statx` one: the basic fields, nothing else.
pub(crate) fn statx_from_stat(stat: &::rustix::fs::Stat) -> ::rustix::fs::Statx {
    // SAFETY: `Statx` is `repr(C)` and made only of integers and flag sets of integers, for
    // which all zeroes is a valid value; every field the walk reads is set below.
    let mut statx: ::rustix::fs::Statx = unsafe { MaybeUninit::zeroed().assume_init() };
    // The kernel's `struct stat` differs in its integer widths from one architecture to the
    // next; `statx`'s are fixed.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unnecessary_cast
    )]
    {
        statx.stx_mask = StatxFlags::BASIC_STATS.bits();
        statx.stx_mode = stat.st_mode as u16;
        statx.stx_nlink = stat.st_nlink as u32;
        statx.stx_uid = stat.st_uid as u32;
        statx.stx_gid = stat.st_gid as u32;
        statx.stx_ino = stat.st_ino as u64;
        statx.stx_size = stat.st_size as u64;
        statx.stx_blocks = stat.st_blocks as u64;
        statx.stx_blksize = stat.st_blksize as u32;
        let device = stat.st_dev as u64;
        statx.stx_dev_major = ::rustix::fs::major(device);
        statx.stx_dev_minor = ::rustix::fs::minor(device);
    }
    statx
}

/// The device a `statx` result names, in the same encoding `st_dev` uses.
pub(super) fn device_of(stat: &rustix::fs::Statx) -> u64 {
    ::rustix::fs::makedev(stat.stx_dev_major, stat.stx_dev_minor)
}
