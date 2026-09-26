//! A lean macOS directory walker built directly on `getattrlistbulk(2)`.
//!
//! The general-purpose walker in `dua-core` reproduces the Apple FTS contract: it asks the kernel
//! for the full `stat` attribute set on every entry, and because bulk enumeration does not
//! synthesize those fields for directories, it follows up with a path-based `lstat` for each one.
//! Disk usage needs none of that — only a name, whether the entry is a directory, and one size
//! field — so this walker requests exactly those attributes and never leaves the bulk call.
//!
//! Entries are delivered one directory at a time. A whole directory shares a parent path, so the
//! consumer resolves that parent once instead of re-parsing a full path per file.

use ::std::collections::HashMap;
use ::std::ffi::OsString;
use ::std::io;
use ::std::os::fd::{AsRawFd, OwnedFd, RawFd};
use ::std::os::unix::ffi::OsStringExt;
use ::std::os::unix::fs::OpenOptionsExt;
use ::std::path::{Path, PathBuf};
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use ::std::sync::{Arc, Condvar, Mutex, PoisonError};
use ::std::thread;

use crate::{DirEntries, EntryMeta};

/// An entry inside a directory, named relative to it.
///
/// `DirEntries` packs its names into one buffer, but this walker collects entries before it knows
/// it has a whole directory — a record can send it back to the `readdir` fallback part-way. So it
/// keeps names owned here and packs them where a `DirEntries` is built, which costs one copy per
/// entry on macOS and leaves the Linux walker's allocation-free path intact.
#[derive(Debug)]
pub struct MacosEntry {
    pub name: OsString,
    pub meta: EntryMeta,
}

/// Collect owned entries into the packed form the rest of the scan expects.
fn packed(path: &Path, entries: Vec<MacosEntry>, failed: u64) -> DirEntries {
    let name_bytes = entries.iter().map(|entry| entry.name.len()).sum();
    let mut directory = DirEntries::with_capacity(Arc::from(path), entries.len(), name_bytes);
    for entry in entries {
        directory.push(&entry.name, entry.meta);
    }
    directory.failed = failed;
    directory
}

/// Darwin vnode type for a directory (`<sys/vnode.h>`, absent from `libc`).
const VDIR: u32 = 2;
/// Per-entry bulk enumeration error attribute (`<sys/attr.h>`, absent from `libc`).
const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
/// Marks a directory that macOS transparently redirects onto the data volume
/// (`<sys/stat.h>`'s `SF_FIRMLINK`, absent from `libc`).
const SF_FIRMLINK: u32 = 0x0080_0000;
/// Larger buffers mean fewer `getattrlistbulk` calls per directory. Big directories are the ones
/// worth optimising; small ones fit in one call either way.
const BUFFER_BYTES: usize = 128 * 1024;

/// `getattrlistbulk(2)` requires each returned record to begin on an eight-byte boundary.
#[repr(align(8))]
struct AlignedBuffer([u8; BUFFER_BYTES]);

/// Reads attribute records packed without padding, as `getattrlist(2)` returns them.
struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (value, rest) = self.bytes.split_first_chunk::<N>()?;
        self.bytes = rest;
        Some(*value)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_ne_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_ne_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_ne_bytes)
    }
}

/// Attributes to request: a name, the object type, the flags and inode needed to decide where to
/// descend, and exactly one size field.
///
/// `ATTR_FILE_*` attributes apply only to non-directories and are left out of a directory's record
/// altogether, which is why the returned bitmap has to be consulted before reading the size.
fn requested_attributes() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        // Both sizes, which the tree keeps side by side: blocks allocated, and length. Packed in
        // bit order, so the allocation comes first.
        fileattr: libc::ATTR_FILE_LINKCOUNT
            | libc::ATTR_FILE_ALLOCSIZE
            | libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    }
}

/// Which size the scan asks for, decided per filesystem; the other one is not requested at all.
///
/// macOS's `msdosfs` sets the `ATTR_FILE_ALLOCSIZE` bit in a record's returned-attributes bitmap
/// and then packs the value as zero, so on a FAT12/16/32 volume every file reads as empty and the
/// whole scan totals nothing. The bitmap claims the attribute is present, so [`parse_record`]
/// cannot tell the difference, and the [`REQUIRED_COMMON`] guard does not cover file attributes.
///
/// There is no allocated size to recover on such a volume: `msdosfs` reports `f_bsize` as 512
/// rather than the cluster size and `st_blocks` as `ceil(size / 512)`, so neither `statfs` nor
/// `lstat` knows the real allocation either. The data length is the closest honest answer, and is
/// what an `--apparent-size` scan of the same volume already reports. exFAT vends a true
/// allocation size and is left alone.
struct SizeAttribute {
    /// Whether each filesystem seen so far is FAT, keyed by device.
    per_device: HashMap<u64, bool>,
}

impl SizeAttribute {
    fn new() -> Self {
        Self {
            per_device: HashMap::new(),
        }
    }

    /// Whether entries of the directory open on `fd`, which lives on `device`, must take their
    /// size on disk from the data length, `ATTR_FILE_ALLOCSIZE` being zero there.
    ///
    /// The filesystem is identified once per device rather than once per directory: a scan can
    /// span a FAT stick and an APFS disk, so one answer for the whole walk would be wrong, but an
    /// `fstatfs` per directory would be paid everywhere to catch a rare case.
    fn length_for_disk(&mut self, fd: RawFd, device: u64) -> bool {
        *self
            .per_device
            .entry(device)
            .or_insert_with(|| is_msdos(fd))
    }
}

/// Whether the filesystem mounted at `path` is on another machine: SMB, NFS, AFP, WebDAV and the
/// like, which the kernel marks by leaving out `MNT_LOCAL`. Unknown counts as local.
fn is_remote(path: &Path) -> bool {
    use ::std::os::unix::ffi::OsStrExt;
    let Ok(path) = ::std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `status` is a writable `statfs` allocation.
    if unsafe { libc::statfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `statfs` returning zero means it initialized the structure.
    let status = unsafe { status.assume_init() };
    status.f_flags & libc::MNT_LOCAL as u32 == 0
}

/// Whether the filesystem behind `fd` is the macOS FAT driver, whose `ATTR_FILE_ALLOCSIZE` is zero.
fn is_msdos(fd: RawFd) -> bool {
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is owned and open, and `status` is a writable `statfs` allocation.
    if unsafe { libc::fstatfs(fd, status.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `fstatfs` returning zero means it initialized the structure.
    let status = unsafe { status.assume_init() };
    status
        .f_fstypename
        .iter()
        .take_while(|character| **character != 0)
        .map(|character| *character as u8)
        .eq(b"msdos".iter().copied())
}

/// Attributes every record must carry for the fixed-layout parse above to be valid.
///
/// APFS returns all of them. A filesystem that does not (some network and foreign filesystems may
/// not vend `ATTR_CMN_FILEID`, for instance) would shift every following field, and a garbage
/// inode would make whole subtrees look like mount points and vanish. Detecting that and falling
/// back is much safer than parsing a layout we did not get.
const REQUIRED_COMMON: libc::attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;

/// The set of common attributes a record says it carries, without decoding the rest of it.
fn record_common_attributes(record: &[u8]) -> Option<libc::attrgroup_t> {
    let mut cursor = Cursor { bytes: record };
    let _length = cursor.u32()?;
    cursor.u32()
}

/// A decoded record: the entry itself, plus what the listing said about where it leads.
struct ParsedRecord {
    entry: MacosEntry,
    firmlink: bool,
    inode: u64,
}

/// Decode one packed record, or `None` if the kernel reported this entry as unreadable.
///
/// Attributes appear in the fixed order of their bits — common attributes first, then file
/// attributes — so each field sits at a position determined by the request.
fn parse_record(record: &[u8], length_for_disk: bool) -> Option<ParsedRecord> {
    let mut cursor = Cursor { bytes: record };
    let _length = cursor.u32()?;
    let returned_common = cursor.u32()?;
    let _volume = cursor.u32()?;
    let _directory = cursor.u32()?;
    let returned_file = cursor.u32()?;
    let _fork = cursor.u32()?;
    if returned_common & REQUIRED_COMMON != REQUIRED_COMMON {
        return None;
    }

    let error = cursor.u32()?;

    // The name is stored out of line; its offset is relative to the reference itself.
    let reference_at = record.len() - cursor.bytes.len();
    let name_offset = cursor.i32()?;
    let name_length = cursor.u32()? as usize;
    let object_type = cursor.u32()?;
    let flags = cursor.u32()?;
    let inode = cursor.u64()?;
    // A directory's record stops here: file attributes are absent rather than zero-filled.
    // Within the file group the link count precedes the size, in ascending bit order.
    let links = if returned_file & libc::ATTR_FILE_LINKCOUNT != 0 {
        u64::from(cursor.u32()?)
    } else {
        1
    };
    let allocated = if returned_file & libc::ATTR_FILE_ALLOCSIZE != 0 {
        cursor.u64()?
    } else {
        0
    };
    let length = if returned_file & libc::ATTR_FILE_DATALENGTH != 0 {
        cursor.u64()?
    } else {
        0
    };
    let size = if length_for_disk { length } else { allocated };

    if error != 0 {
        return None;
    }

    let start = reference_at.checked_add_signed(name_offset as isize)?;
    let end = start
        .checked_add(name_length)
        .filter(|end| *end <= record.len())?;
    let mut name = &record[start..end];
    if name.last() == Some(&0) {
        name = &name[..name.len() - 1];
    }

    Some(ParsedRecord {
        entry: MacosEntry {
            name: OsString::from_vec(name.to_vec()),
            meta: EntryMeta {
                size,
                apparent: length,
                inode,
                links,
                is_dir: object_type == VDIR,
                // APFS clones share blocks the way reflinks do, but `getattrlistbulk` does not
                // report sharing and there is no cheap per-file equivalent of FIEMAP here.
                shared_extent: 0,
            },
        },
        firmlink: flags & SF_FIRMLINK != 0,
        inode,
    })
}

/// One directory's contents, plus what the walker needs to decide where to go next.
struct DirRead {
    entries: DirEntries,
    /// Inode of the directory that was actually opened.
    inode: u64,
    /// Filesystem the opened directory turned out to live on.
    device: u64,
    /// What the listing said about each entry, parallel to `entries.entries`.
    listed: Vec<ListedAs>,
}

/// What a directory's listing said about one entry, before it was opened.
#[derive(Clone, Copy)]
struct ListedAs {
    firmlink: bool,
    inode: u64,
}

/// Enumerate one directory.
fn read_dir_bulk(
    path: &Path,
    size: &mut SizeAttribute,
    buffer: &mut AlignedBuffer,
) -> io::Result<DirRead> {
    let directory: OwnedFd = ::std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)?
        .into();
    // Bulk records describe entries without crossing mount points, so a directory that something
    // is mounted over is listed with the inode of the directory it covers. Opening it does cross
    // the mount, which is what makes the two distinguishable.
    let (inode, device) = {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor is owned and open, and `status` is a writable `stat` allocation.
        if unsafe { libc::fstat(directory.as_raw_fd(), status.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstat` returning zero means it initialized the structure.
        let status = unsafe { status.assume_init() };
        (status.st_ino, status.st_dev as u64)
    };

    let length_for_disk = size.length_for_disk(directory.as_raw_fd(), device);
    let mut entries = Vec::new();
    let mut listed = Vec::new();
    let mut failed = 0u64;
    loop {
        let mut attributes = requested_attributes();
        // SAFETY: the descriptor is owned and open, `attributes` is a valid initialized attrlist,
        // and the buffer is writable, eight-byte aligned, and described by its own length.
        let count = unsafe {
            libc::getattrlistbulk(
                directory.as_raw_fd(),
                (&raw mut attributes).cast(),
                buffer.0.as_mut_ptr().cast(),
                buffer.0.len(),
                u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
            )
        };
        if count == 0 {
            break;
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }

        let count = count as usize;
        let mut offset = 0usize;
        for index in 0..count {
            let Some(length) = buffer.0[offset..]
                .split_first_chunk::<4>()
                .map(|(length, _)| u32::from_ne_bytes(*length) as usize)
                .filter(|length| *length >= 4 && offset + *length <= buffer.0.len())
            else {
                // The rest of the batch cannot be located without this record's length.
                failed += (count - index) as u64;
                break;
            };
            if index == 0
                && entries.is_empty()
                && record_common_attributes(&buffer.0[offset..offset + length])
                    .is_none_or(|returned| returned & REQUIRED_COMMON != REQUIRED_COMMON)
            {
                return read_dir_stat(path, inode, device);
            }
            match parse_record(&buffer.0[offset..offset + length], length_for_disk) {
                Some(record) => {
                    entries.push(record.entry);
                    listed.push(ListedAs {
                        firmlink: record.firmlink,
                        inode: record.inode,
                    });
                }
                None => failed += 1,
            }
            offset += length;
        }
    }

    Ok(DirRead {
        entries: packed(path, entries, failed),
        inode,
        device,
        listed,
    })
}

/// Enumerate one directory with `readdir` and `lstat`, for filesystems whose bulk records do not
/// carry the attributes [`parse_record`] relies on.
///
/// `lstat` resolves through a mount point, so the inodes recorded here cannot distinguish a
/// mounted directory from the directory it covers. Nothing reachable this way has firmlinks, and
/// nested mounts on such volumes are unusual, so the walk simply descends.
fn read_dir_stat(path: &Path, inode: u64, device: u64) -> io::Result<DirRead> {
    use ::std::os::unix::fs::MetadataExt;

    let mut entries = Vec::new();
    let mut listed = Vec::new();
    let mut failed = 0u64;
    for entry in ::std::fs::read_dir(path)? {
        // `DirEntry::metadata` does not follow symlinks, matching the bulk path.
        let Ok((entry, metadata)) = entry.and_then(|entry| {
            let metadata = entry.metadata()?;
            Ok((entry, metadata))
        }) else {
            failed += 1;
            continue;
        };
        let size = libduscape::os::size_on_disk_fast(&metadata);
        entries.push(MacosEntry {
            name: entry.file_name(),
            meta: EntryMeta {
                size: if metadata.is_dir() { 0 } else { size },
                apparent: if metadata.is_dir() { 0 } else { metadata.len() },
                inode: metadata.ino(),
                links: metadata.nlink(),
                is_dir: metadata.is_dir(),
                shared_extent: 0,
            },
        });
        listed.push(ListedAs {
            firmlink: false,
            inode: metadata.ino(),
        });
    }

    Ok(DirRead {
        entries: packed(path, entries, failed),
        inode,
        device,
        listed,
    })
}

/// A directory waiting to be read.
struct Job {
    path: PathBuf,
    depth: usize,
    /// Inode the parent directory listed for this one, before any mount was crossed.
    listed_inode: u64,
    /// Whether the parent listed this directory as a firmlink.
    firmlink: bool,
}

/// Directories still to be read, shared by the worker pool.
struct Queue {
    state: Mutex<QueueState>,
    wakeup: Condvar,
    /// Set when the consumer has gone away and the remaining work no longer matters.
    stop: AtomicBool,
}

struct QueueState {
    /// Used as a stack: depth-first keeps the queue short and the parent directory cache-warm.
    pending: Vec<Job>,
    /// Workers currently reading a directory; the walk is over when this and `pending` are empty.
    working: usize,
}

impl Queue {
    fn push_all(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending.extend(jobs);
        self.wakeup.notify_all();
    }

    /// Abandon the rest of the walk and wake every worker waiting for more of it.
    ///
    /// Without this, dropping a walk early would leave the workers to finish the whole tree while
    /// the consumer waited to join them — on a whole disk, half a minute of scanning nobody wants.
    fn request_stop(&self) {
        // Taken so that a worker cannot be between checking the flag and waiting on the condvar.
        let _state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.stop.store(true, Ordering::Release);
        self.wakeup.notify_all();
    }

    /// Claim the next directory, or `None` once the walk is over or abandoned.
    fn pop(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.stop.load(Ordering::Acquire) {
                return None;
            }
            if let Some(job) = state.pending.pop() {
                state.working += 1;
                return Some(job);
            }
            if state.working == 0 {
                // Nothing pending, and nobody left who could produce more.
                self.wakeup.notify_all();
                return None;
            }
            state = self
                .wakeup
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.working -= 1;
        if state.working == 0 && state.pending.is_empty() {
            self.wakeup.notify_all();
        }
    }
}

/// Walk `root` in parallel, yielding one message per directory read.
///
/// Mount points other than the root are not entered, so each volume is visited at most once. That
/// matters most on `/`: macOS mounts the data volume at `/System/Volumes/Data` *and* grafts it into
/// `/` through firmlinks, so a walk that follows both counts nearly every file on the machine
/// twice. Firmlinks are followed, since they are the only route to what they point at.
///
/// The iterator ends when the whole tree has been read. Dropping it early stops the workers.
pub fn walk_macos(
    root: &Path,
    threads: usize,
    max_depth: Option<usize>,
    one_file_system: bool,
) -> impl Iterator<Item = DirEntries> {
    // Which filesystem the scan starts on. A mount point leading back to it is a second route to
    // files the scan already reaches, rather than somewhere new.
    let root_device = ::std::fs::metadata(root)
        .map(|metadata| ::std::os::unix::fs::MetadataExt::dev(&metadata))
        .unwrap_or_default();
    let queue = Arc::new(Queue {
        state: Mutex::new(QueueState {
            pending: vec![Job {
                path: root.to_path_buf(),
                depth: 0,
                listed_inode: 0,
                // The root is always read, mount point or not.
                firmlink: true,
            }],
            working: 0,
        }),
        wakeup: Condvar::new(),
        stop: AtomicBool::new(false),
    });
    let (sender, receiver): (SyncSender<DirEntries>, Receiver<DirEntries>) =
        sync_channel(threads * 4);

    let workers: Vec<_> = (0..threads.max(1))
        .map(|_| {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            thread::Builder::new()
                .name("macos_scanner".to_string())
                .spawn(move || {
                    let mut buffer = AlignedBuffer([0; BUFFER_BYTES]);
                    let mut size = SizeAttribute::new();
                    while let Some(job) = queue.pop() {
                        // `dua-core` descends into a directory entry whose own depth is below the
                        // limit; a job's depth is that same depth, so the test matches it. Only
                        // the root can reach here, since children are filtered before queueing.
                        if max_depth.is_some_and(|max| job.depth >= max) {
                            queue.finish();
                            continue;
                        }
                        let stop;
                        match read_dir_bulk(&job.path, &mut size, &mut buffer) {
                            Ok(read) => {
                                // A directory whose opened inode differs from the one its parent
                                // listed has something mounted over it.
                                let mounted = read.inode != job.listed_inode && !job.firmlink;
                                // Reaching the starting filesystem again through a mount point
                                // means those files are already being counted by another path --
                                // macOS mounts the data volume at `/System/Volumes/Data` and also
                                // grafts it into `/` with firmlinks -- so descending would count
                                // everything twice.
                                let already_counted = read.device == root_device;
                                // A network share holds another machine's files, not this disk's
                                // (see `linux::filesystem::NETWORK`). Asked only at a mount point.
                                if mounted
                                    && (one_file_system || already_counted || is_remote(&job.path))
                                {
                                    queue.finish();
                                    continue;
                                }
                                let children = if max_depth.is_none_or(|max| job.depth + 1 < max) {
                                    read.entries
                                        .iter()
                                        .zip(&read.listed)
                                        .filter(|((_, meta), _)| meta.is_dir)
                                        .map(|((name, _), listed)| Job {
                                            path: read.entries.path.join(name),
                                            depth: job.depth + 1,
                                            listed_inode: listed.inode,
                                            firmlink: listed.firmlink,
                                        })
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                                // Sent before its children are queued so that a consumer building
                                // a tree sees each directory before the directories inside it.
                                stop = sender.send(read.entries).is_err();
                                queue.push_all(children);
                            }
                            Err(_) => {
                                let mut unreadable = DirEntries::new(Arc::from(job.path.as_path()));
                                unreadable.failed = 1;
                                stop = sender.send(unreadable).is_err();
                            }
                        }
                        queue.finish();
                        if stop {
                            break;
                        }
                    }
                })
                .expect("could not spawn scan worker")
        })
        .collect();
    drop(sender);

    MacosWalk {
        receiver,
        workers: Some(workers),
        queue,
    }
}

struct MacosWalk {
    receiver: Receiver<DirEntries>,
    workers: Option<Vec<thread::JoinHandle<()>>>,
    queue: Arc<Queue>,
}

impl Iterator for MacosWalk {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        match self.receiver.recv() {
            Ok(entries) => Some(entries),
            Err(_) => {
                self.join();
                None
            }
        }
    }
}

impl MacosWalk {
    fn join(&mut self) {
        for worker in self.workers.take().into_iter().flatten() {
            let _ = worker.join();
        }
    }
}

impl Drop for MacosWalk {
    fn drop(&mut self) {
        self.queue.request_stop();
        // Draining releases any worker blocked sending into a full channel, so it can reach the
        // top of its loop and see that the walk has been abandoned.
        while self.receiver.recv().is_ok() {}
        self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{SizeAttribute, walk_macos};
    use ::std::os::fd::AsRawFd;
    use ::std::path::{Path, PathBuf};
    use ::std::process::Command;

    /// The mount point `hdiutil attach` reported for the partition that actually mounted.
    ///
    /// The output is three tab-separated columns and only a mounted partition fills the third, so
    /// all three are required: matching on "the last field starting with a slash" would accept the
    /// device node from the untabbed scheme line.
    fn mount_point(output: &[u8]) -> Option<PathBuf> {
        ::std::str::from_utf8(output)
            .ok()?
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\t');
                let device = fields.next()?.trim();
                let _scheme = fields.next()?;
                let mount = fields.next()?.trim();
                (device.starts_with("/dev/") && mount.starts_with('/'))
                    .then(|| PathBuf::from(mount))
            })
    }

    /// The disk `hdiutil attach` opened, which exists even when nothing mounted.
    fn attached_device(output: &[u8]) -> Option<String> {
        ::std::str::from_utf8(output)
            .ok()?
            .split_whitespace()
            .find(|token| token.starts_with("/dev/disk"))
            .map(str::to_string)
    }

    /// Detaches the image and deletes it however the test ends.
    ///
    /// Without this a panic between attaching and detaching leaves the volume mounted, which is
    /// exactly what happened while this test was being written.
    struct Attached {
        device: String,
        image: PathBuf,
    }

    impl Drop for Attached {
        fn drop(&mut self) {
            let _ = Command::new("hdiutil")
                .arg("detach")
                .arg(&self.device)
                .output();
            let _ = ::std::fs::remove_file(&self.image);
        }
    }

    /// Open a directory the way [`super::read_dir_bulk`] does, for the device it reports.
    fn open_dir(path: &Path) -> (::std::fs::File, u64) {
        use ::std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = ::std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(path)
            .expect("open directory");
        let device = file.metadata().expect("stat directory").dev();
        (file, device)
    }

    #[test]
    fn an_ordinary_filesystem_takes_its_size_on_disk_from_the_allocation() {
        let (directory, device) = open_dir(&::std::env::temp_dir());
        let mut size = SizeAttribute::new();
        assert!(!size.length_for_disk(directory.as_raw_fd(), device));
        // The answer is cached per device, so a second directory on it costs no `fstatfs`.
        assert_eq!(size.per_device.len(), 1);
        assert!(!size.length_for_disk(directory.as_raw_fd(), device));
        assert_eq!(size.per_device.len(), 1);
    }

    /// A FAT32 volume, created and mounted for the test, must not scan as empty.
    ///
    /// Nothing synthetic reproduces this: `msdosfs` sets the `ATTR_FILE_ALLOCSIZE` bit in the
    /// returned-attributes bitmap and packs the value as zero, so a hand-built record either
    /// carries the attribute or does not, and neither case is the one that broke. Only the real
    /// driver lies this way.
    ///
    /// Ignored by default because it creates and mounts a disk image, which CI does not do:
    /// `cargo test -p libduscape --lib -- --ignored fat32`.
    #[test]
    #[ignore = "creates and mounts a FAT32 disk image with hdiutil"]
    fn a_fat32_volume_does_not_scan_as_empty() {
        let image = ::std::env::temp_dir().join("duscape_fat32_test.dmg");
        let _ = ::std::fs::remove_file(&image);

        let created = Command::new("hdiutil")
            .args([
                "create",
                "-size",
                "64m",
                "-fs",
                "MS-DOS FAT32",
                "-volname",
                "DSKNAUT",
            ])
            .arg(&image)
            .output()
            .expect("run hdiutil create");
        assert!(
            created.status.success(),
            "hdiutil create failed: {created:?}"
        );
        let attached = Command::new("hdiutil")
            .arg("attach")
            .arg(&image)
            .output()
            .expect("run hdiutil attach");
        assert!(
            attached.status.success(),
            "hdiutil attach failed: {attached:?}"
        );
        // Registered before anything that can panic, so the volume is detached either way.
        let _attached = Attached {
            device: attached_device(&attached.stdout).expect("hdiutil attach reported a disk"),
            image: image.clone(),
        };
        // Read the mount point back rather than assuming it: a FAT label longer than eleven
        // characters is silently replaced with "NO NAME", so a name-derived path can be wrong.
        let mount = mount_point(&attached.stdout).expect("hdiutil attach reported a mount point");

        ::std::fs::write(mount.join("a.bin"), vec![0u8; 40 * 1024]).expect("write a.bin");
        ::std::fs::create_dir(mount.join("nested")).expect("create nested");
        ::std::fs::write(mount.join("nested/b.bin"), vec![0u8; 24 * 1024]).expect("write b.bin");

        let total: u64 = walk_macos(&mount, 2, None, false)
            .flat_map(|directory| {
                directory
                    .entries()
                    .iter()
                    .map(|entry| entry.meta.size)
                    .collect::<Vec<_>>()
            })
            .sum();

        assert!(
            total >= 64 * 1024,
            "FAT32 volume scanned as {total} bytes; the two files hold 64 KiB. \
             `msdosfs` reports ATTR_FILE_ALLOCSIZE as zero while claiming to return it, \
             so the walk must fall back to ATTR_FILE_DATALENGTH there."
        );
    }
}
