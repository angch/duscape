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

use crate::focus::{Focus, FocusWatch};
use crate::{DirEntries, EntryMeta};
use libduscape::nas;

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

/// A failure while reading a directory: the entry's name when it is known, what was being done,
/// and what the system said.
type Failure = (Option<OsString>, &'static str, String);

/// Collect owned entries into the packed form the rest of the scan expects.
fn packed(path: &Path, entries: Vec<MacosEntry>, failures: Vec<Failure>) -> DirEntries {
    let name_bytes = entries.iter().map(|entry| entry.name.len()).sum();
    let mut directory = DirEntries::with_capacity(Arc::from(path), entries.len(), name_bytes);
    for entry in entries {
        directory.push(&entry.name, entry.meta);
    }
    for (name, action, error) in failures {
        directory.fail(action, name.as_deref(), error);
    }
    directory
}

/// Darwin vnode type for a directory (`<sys/vnode.h>`, absent from `libc`).
const VDIR: u32 = 2;
/// Per-entry bulk enumeration error attribute (`<sys/attr.h>`, absent from `libc`).
const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
/// Marks a directory that macOS transparently redirects onto the data volume
/// (`<sys/stat.h>`'s `SF_FIRMLINK`, absent from `libc`).
const SF_FIRMLINK: u32 = 0x0080_0000;
/// A file that shares all of its blocks with another, a pure clone (`<sys/stat.h>`'s
/// `EF_SHARES_ALL_BLOCKS`, absent from `libc`).
const EF_SHARES_ALL_BLOCKS: u64 = 0x40;
/// The extended common attributes asked for (in the fork group, under
/// `FSOPT_ATTR_CMN_EXTENDED`): which data stream a file's blocks are, and whether all of them are
/// shared with another file.
const CLONE_ATTRIBUTES: libc::attrgroup_t = libc::ATTR_CMNEXT_CLONEID | libc::ATTR_CMNEXT_EXT_FLAGS;
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
        // APFS clones: see [`clone_identity`].
        forkattr: CLONE_ATTRIBUTES,
    }
}

/// An identity for a file's blocks when it is a pure clone — every block shared with another
/// file, as `cp -c` and the Finder's duplicate make — or `0`: what [`EntryMeta::shared_extent`]
/// is for reflinks, so the ledger counts the blocks once wherever the copies are.
///
/// Files that are pure clones of each other share a clone id (`ATTR_CMNEXT_CLONEID`, "which
/// data stream"); a file written to after cloning gets an id of its own, so the same id is the
/// same blocks. An ordinary file's id is its own inode number, which is how a clone is told
/// apart: `EF_SHARES_ALL_BLOCKS` says so for clones `cp -c` made, but the system's own on
/// `/System/Volumes/Preboot` (its cryptexes' `OS` and `Incoming/OS`, 11 GiB each) carry no
/// flags at all, only an id that is not their inode. A clone whose twins are gone, or one
/// partly written since, is keyed alone and counted in full, as partly shared reflinks are.
/// The id is a volume's, so the device is folded in: a scan of `/` covers several volumes.
/// Without this Preboot read as 27.7 GiB on a volume using 8.4.
fn clone_identity(clone_id: u64, flags: u64, inode: u64, device: u64) -> u64 {
    if clone_id == 0 || (flags & EF_SHARES_ALL_BLOCKS == 0 && clone_id == inode) {
        return 0;
    }
    let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
    for value in [clone_id, device] {
        identity = (identity ^ value).wrapping_mul(0x0000_0100_0000_01b3);
    }
    identity.max(1)
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
pub(crate) fn is_remote(path: &Path) -> bool {
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
fn parse_record(record: &[u8], length_for_disk: bool, device: u64) -> Option<ParsedRecord> {
    let mut cursor = Cursor { bytes: record };
    let _length = cursor.u32()?;
    let returned_common = cursor.u32()?;
    let _volume = cursor.u32()?;
    let _directory = cursor.u32()?;
    let returned_file = cursor.u32()?;
    let returned_extended = cursor.u32()?;
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
    // Last in the record, in bit order: the clone id, then the flags. Read only when both came
    // back (a filesystem that is not APFS answers neither) and only for a file, and a record cut
    // short of them is a file not known to be a clone, not one unreadable.
    let shared_extent = if object_type != VDIR
        && returned_extended & CLONE_ATTRIBUTES == CLONE_ATTRIBUTES
        && let (Some(clone_id), Some(flags)) = (cursor.u64(), cursor.u64())
    {
        clone_identity(clone_id, flags, inode, device)
    } else {
        0
    };

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
                shared_extent,
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
    let mut failures: Vec<Failure> = Vec::new();
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
                u64::from(libc::FSOPT_PACK_INVAL_ATTRS | libc::FSOPT_ATTR_CMN_EXTENDED),
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
                for _ in index..count {
                    failures.push((
                        None,
                        "list",
                        "a bulk record that could not be located".into(),
                    ));
                }
                break;
            };
            if index == 0
                && entries.is_empty()
                && record_common_attributes(&buffer.0[offset..offset + length])
                    .is_none_or(|returned| returned & REQUIRED_COMMON != REQUIRED_COMMON)
            {
                return read_dir_stat(path, inode, device);
            }
            match parse_record(&buffer.0[offset..offset + length], length_for_disk, device) {
                Some(record) => {
                    entries.push(record.entry);
                    listed.push(ListedAs {
                        firmlink: record.firmlink,
                        inode: record.inode,
                    });
                }
                None => failures.push((None, "list", "a bulk record marked unreadable".into())),
            }
            offset += length;
        }
    }

    Ok(DirRead {
        entries: packed(path, entries, failures),
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
    let mut failures: Vec<Failure> = Vec::new();
    for entry in ::std::fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push((None, "list", error.to_string()));
                continue;
            }
        };
        // `DirEntry::metadata` does not follow symlinks, matching the bulk path.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                failures.push((Some(entry.file_name()), "stat", error.to_string()));
                continue;
            }
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
        entries: packed(path, entries, failures),
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
    /// The jobs toward the folder the user is in (`crate::focus`), served before `pending`.
    toward: Vec<Job>,
    /// Workers currently reading a directory; the walk is over when this and `pending` are empty.
    working: usize,
    focus: FocusWatch,
}

impl QueueState {
    /// Take `jobs` in, those toward the focus onto their own stack. The focus having moved,
    /// both stacks are sorted again first.
    fn absorb(&mut self, jobs: Vec<Job>) {
        if self.focus.refresh() {
            self.sort_again();
        }
        self.place(jobs);
    }

    /// The focus moved: both stacks placed again by the new one.
    fn sort_again(&mut self) {
        let all: Vec<Job> = self
            .toward
            .drain(..)
            .chain(self.pending.drain(..))
            .collect();
        self.place(all);
    }

    fn place(&mut self, jobs: Vec<Job>) {
        if self.focus.current().is_none() {
            self.pending.extend(jobs);
            return;
        }
        for job in jobs {
            if self.focus.toward(&job.path) {
                self.toward.push(job);
            } else {
                self.pending.push(job);
            }
        }
    }

    fn next(&mut self) -> Option<Job> {
        if self.focus.refresh() {
            self.sort_again();
        }
        self.toward.pop().or_else(|| self.pending.pop())
    }

    fn is_empty(&self) -> bool {
        self.toward.is_empty() && self.pending.is_empty()
    }
}

impl Queue {
    fn push_all(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.absorb(jobs);
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
            if let Some(job) = state.next() {
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
        if state.working == 0 && state.is_empty() {
            self.wakeup.notify_all();
        }
    }
}

/// A share's snapshots, seen over the network (`DirEntries::leave_out`): noted in the directory
/// and named, so that the walk lists them and does not enter them. Asked only where the walk
/// would go down (`descend`).
fn leave_out(entries: &mut DirEntries, descend: bool, snapshots: bool) -> Vec<OsString> {
    if !descend {
        return Vec::new();
    }
    let folders: Vec<OsString> = entries
        .iter()
        .filter(|(name, meta)| meta.is_dir && nas::left_out(name, snapshots).is_some())
        .map(|(name, _)| name.to_os_string())
        .collect();
    folders
        .into_iter()
        .filter(|name| entries.leave_out(name, true, snapshots))
        .collect()
}

/// List one directory as the walk would, for the saved scan brought up to date
/// ([`crate::cache`]): `depth` from the scan root, so the depth cap and the folders left out
/// by name apply as they did. `NotFound` means it is gone.
pub fn list_one(
    path: &Path,
    depth: usize,
    max_depth: Option<usize>,
    snapshots: bool,
) -> io::Result<DirEntries> {
    let mut buffer = Box::new(AlignedBuffer([0; BUFFER_BYTES]));
    let mut size = SizeAttribute::new();
    let mut read = read_dir_bulk(path, &mut size, &mut buffer)?;
    let descend = max_depth.is_none_or(|max| depth + 1 < max);
    leave_out(&mut read.entries, descend, snapshots);
    Ok(read.entries)
}

/// The directory for a mount at `path` that is a disk image's volume, its image under the scan
/// (`real_root`, as [`crate::disk_image::real_path`] gives it): empty, and noted, so `--issues`
/// says why. `None` for any other mount, which is walked.
fn image_left_out(path: &Path, real_root: &Path) -> Option<DirEntries> {
    let image = crate::disk_image::counted_by_the_scan(path, real_root)?;
    let mut left = DirEntries::new(Arc::from(path));
    left.note(
        "image",
        None,
        format!(
            "a disk image's volume, counted as its file {}",
            image.display()
        ),
    );
    Some(left)
}

/// The directory for a mount at `path` that is a volume's snapshot — Time Machine's local
/// snapshots as the Finder browses them, or the snapshots' pass's own while it reads one
/// (`crate::snapshots`): empty, and noted, unless `snapshots` (`--snapshots`), as a btrfs
/// snapshot is. Walked, it is the volume's files again. `None` for any other mount.
fn snapshot_left_out(path: &Path, snapshots: bool) -> Option<DirEntries> {
    if snapshots || !crate::snapshots::is_snapshot_mount(path) {
        return None;
    }
    let mut left = DirEntries::new(Arc::from(path));
    left.note(
        "left out",
        None,
        "a volume's snapshot, mounted: its files again (--snapshots walks it)",
    );
    Some(left)
}

/// The folders of `read` to walk, at `depth`, less those `left_out`.
fn children(read: &DirRead, depth: usize, left_out: &[OsString]) -> Vec<Job> {
    read.entries
        .iter()
        .zip(&read.listed)
        .filter(|((name, meta), _)| meta.is_dir && !left_out.iter().any(|n| n == name))
        .map(|((name, _), listed)| Job {
            path: read.entries.path.join(name),
            depth,
            listed_inode: listed.inode,
            firmlink: listed.firmlink,
        })
        .collect()
}

/// A mount the walk leaves empty and notes: a disk image's counted as a file, or a snapshot.
fn mount_left_out(path: &Path, real_root: &Path, snapshots: bool) -> Option<DirEntries> {
    image_left_out(path, real_root).or_else(|| snapshot_left_out(path, snapshots))
}

/// Walk `root` in parallel, yielding one message per directory read.
///
/// Mount points are entered, as `du` enters them, except: under `-x`; a network share (no
/// `MNT_LOCAL`); a mount of the root's own device, a second route to files already counted — on
/// `/`, macOS mounts the data volume at `/System/Volumes/Data` *and* grafts it into `/` through
/// firmlinks, so a walk that followed both would count nearly every file on the machine twice;
/// a disk image's volume whose image the scan counts as a file; and a volume's snapshot, unless
/// `snapshots`. So a
/// scan of `/` covers the volume group and the other volumes mounted under it (Preboot, VM,
/// what is under `/Volumes`), which `os::volume_used` counts on at `/`. Firmlinks are followed,
/// since they are the only route to what they point at.
///
/// The iterator ends when the whole tree has been read. Dropping it early stops the workers.
// The disk image's volume is `image_left_out` and the snapshot `snapshot_left_out`, private
// (kept out of the public docs, which cannot link them).
pub fn walk_macos(
    root: &Path,
    threads: usize,
    max_depth: Option<usize>,
    one_file_system: bool,
    snapshots: bool,
    focus: &Focus,
) -> MacosWalk {
    // Which filesystem the scan starts on. A mount point leading back to it is a second route to
    // files the scan already reaches, rather than somewhere new.
    let root_device = ::std::fs::metadata(root)
        .map(|metadata| ::std::os::unix::fs::MetadataExt::dev(&metadata))
        .unwrap_or_default();
    // The root as it is on its volume, to tell whether a disk image is inside the scan.
    let real_root: Arc<Path> =
        Arc::from(crate::disk_image::real_path(root).unwrap_or_else(|| root.to_path_buf()));
    let queue = Arc::new(Queue {
        state: Mutex::new(QueueState {
            pending: vec![Job {
                path: root.to_path_buf(),
                depth: 0,
                listed_inode: 0,
                // The root is always read, mount point or not.
                firmlink: true,
            }],
            toward: Vec::new(),
            working: 0,
            focus: focus.watch_under(root),
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
            let real_root = Arc::clone(&real_root);
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
                            Ok(mut read) => {
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
                                // A counted disk image's volume, or a snapshot: walked, twice.
                                if mounted
                                    && let Some(left) =
                                        mount_left_out(&job.path, &real_root, snapshots)
                                {
                                    let stop = sender.send(left).is_err();
                                    queue.finish();
                                    if stop {
                                        break;
                                    }
                                    continue;
                                }
                                let descend = max_depth.is_none_or(|max| job.depth + 1 < max);
                                let left_out = leave_out(&mut read.entries, descend, snapshots);
                                let children = if descend {
                                    children(&read, job.depth + 1, &left_out)
                                } else {
                                    Vec::new()
                                };
                                // Sent before its children are queued so that a consumer building
                                // a tree sees each directory before the directories inside it.
                                stop = sender.send(read.entries).is_err();
                                queue.push_all(children);
                            }
                            Err(error) => {
                                let mut unreadable = DirEntries::new(Arc::from(job.path.as_path()));
                                unreadable.fail("open", None, error);
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

/// The walk under way: its workers, and the directories they send. Owns everything it needs,
/// so it outlives the arguments it was made from.
pub struct MacosWalk {
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

        let total: u64 = walk_macos(
            &mount,
            2,
            None,
            false,
            false,
            &crate::focus::Focus::default(),
        )
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

    /// A clone is keyed by its data stream where the flags say so or its id is not its inode;
    /// an ordinary file, whose id is its inode, is not; the device is part of the key.
    #[test]
    fn a_clone_is_told_by_its_flags_or_an_id_not_its_inode() {
        use super::{EF_SHARES_ALL_BLOCKS, clone_identity};
        assert_eq!(clone_identity(7, 0, 7, 1), 0, "an ordinary file");
        assert_eq!(clone_identity(0, EF_SHARES_ALL_BLOCKS, 7, 1), 0, "no id");
        let flagged = clone_identity(7, EF_SHARES_ALL_BLOCKS, 7, 1);
        assert_ne!(flagged, 0, "a clone `cp -c` made, still the original");
        assert_eq!(
            clone_identity(7, 0, 9, 1),
            flagged,
            "a clone with no flags kept"
        );
        assert_ne!(
            clone_identity(7, 0, 9, 2),
            flagged,
            "the same id on another volume"
        );
    }

    /// Three pure clones of a file count its blocks once; a clone written to since is counted
    /// in full beside them, as partly shared reflinks are.
    #[test]
    fn clones_are_counted_once() {
        use ::std::ffi::CString;
        use ::std::os::unix::ffi::OsStrExt;
        let dir = ::std::env::temp_dir().join("duscape_clones_test");
        let _ = ::std::fs::remove_dir_all(&dir);
        ::std::fs::create_dir_all(dir.join("a")).expect("create a");
        ::std::fs::create_dir_all(dir.join("b")).expect("create b");
        let original = dir.join("a/original.bin");
        let bytes: Vec<u8> = (0..1u32 << 20).map(|index| (index * 7 + 3) as u8).collect();
        ::std::fs::write(&original, &bytes).expect("write the original");
        let clone = |to: &Path| {
            let from = CString::new(original.as_os_str().as_bytes()).expect("a path");
            let to = CString::new(to.as_os_str().as_bytes()).expect("a path");
            // SAFETY: two NUL-terminated paths; the call reads them and nothing else.
            unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) == 0 }
        };
        if !clone(&dir.join("a/one.bin")) {
            // The temporary folder is not on APFS: there are no clones to count.
            let _ = ::std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(clone(&dir.join("b/two.bin")));
        let (tree, failed) = crate::scan_into_tree(&dir, crate::ScanOptions::default());
        assert_eq!(failed, 0);
        let one_copy = tree.get_total_size();
        assert!(
            (1 << 20..(1 << 20) + (1 << 16)).contains(&one_copy),
            "three clones of a MiB count one: {one_copy}"
        );

        assert!(clone(&dir.join("b/written.bin")));
        let mut written = ::std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("b/written.bin"))
            .expect("open the clone");
        ::std::io::Write::write_all(&mut written, b"more").expect("write to the clone");
        drop(written);
        let (tree, _) = crate::scan_into_tree(&dir, crate::ScanOptions::default());
        assert!(
            tree.get_total_size() >= 2 * one_copy,
            "a clone written to is its own file: {}",
            tree.get_total_size()
        );

        // Written over in place, its length the same: the size guard cannot tell it from its
        // twins, the clone id does (APFS gives the written copy an id of its own).
        assert!(clone(&dir.join("b/overwritten.bin")));
        let mut overwritten = ::std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("b/overwritten.bin"))
            .expect("open the clone");
        ::std::io::Write::write_all(&mut overwritten, &[0xa5; 4096]).expect("write over it");
        drop(overwritten);
        let (again, _) = crate::scan_into_tree(&dir, crate::ScanOptions::default());
        let _ = ::std::fs::remove_dir_all(&dir);
        assert!(
            again.get_total_size() >= tree.get_total_size() + one_copy,
            "a clone written over is its own file: {} after {}",
            again.get_total_size(),
            tree.get_total_size()
        );
    }

    /// A disk image's volume mounted inside the scan, from an image inside it too, is left
    /// empty and noted: the image counts its blocks. Ignored by default because it creates and
    /// mounts a disk image; `cargo test -p duscape-scan --lib -- --ignored disk_image`.
    #[test]
    #[ignore = "creates and mounts a disk image with hdiutil"]
    fn a_disk_image_mounted_inside_the_scan_is_counted_once() {
        let dir = ::std::env::temp_dir().join("duscape_disk_image_test");
        let _ = ::std::fs::remove_dir_all(&dir);
        ::std::fs::create_dir_all(dir.join("mounted")).expect("create the mount point");
        let image = dir.join("volume.dmg");
        let created = Command::new("hdiutil")
            .args(["create", "-size", "16m", "-fs", "APFS", "-volname", "DSIMG"])
            .arg(&image)
            .output()
            .expect("run hdiutil create");
        assert!(
            created.status.success(),
            "hdiutil create failed: {created:?}"
        );
        let attached = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-mountpoint"])
            .arg(dir.join("mounted"))
            .arg(&image)
            .output()
            .expect("run hdiutil attach");
        assert!(
            attached.status.success(),
            "hdiutil attach failed: {attached:?}"
        );
        let _attached = Attached {
            device: attached_device(&attached.stdout).expect("hdiutil attach reported a disk"),
            image: image.clone(),
        };
        ::std::fs::write(dir.join("mounted/inside.bin"), vec![1u8; 1 << 20]).expect("write");

        let mut noted = Vec::new();
        let mut inside = 0;
        for directory in walk_macos(&dir, 2, None, false, false, &crate::focus::Focus::default()) {
            if directory.path.ends_with("mounted") {
                inside += directory.iter().count();
                noted.extend(directory.issues.examples.iter().map(|issue| issue.action));
            }
        }
        assert_eq!(inside, 0, "the volume is not walked");
        assert_eq!(noted, ["image"], "and the scan says why");
        assert_eq!(
            crate::disk_image::backing_file(&dir.join("mounted"))
                .and_then(|path| path.canonicalize().ok()),
            image.canonicalize().ok()
        );
    }
}
