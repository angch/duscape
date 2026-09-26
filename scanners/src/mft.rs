//! NTFS read from its master file table: the walk's answers taken from the MFT in a few large
//! sequential reads, instead of asked of the filesystem one directory handle at a time. Elevated,
//! since it opens the volume, and for a whole volume only, since the table costs all of it
//! whatever the tree (`volume::chosen` has the measurements); the Windows shape of
//! `docs/scan-roadmap.md` step 2, and what WizTree does.
//!
//! Every file and directory on an NTFS volume is one record in the `$MFT`, and the record holds
//! everything the walk asks a directory listing for: each name the file has and the directory it
//! is in (`$FILE_NAME`, one per hard link), the unnamed `$DATA` stream's length and allocation,
//! whether it is a directory, and whether it is a junction or symbolic link (`$REPARSE_POINT`).
//! So the whole volume is read as one 1–3 GB file, parsed into `(parent, name, sizes)` on a few
//! threads, and the tree under the scan root is handed on breadth-first, one [`DirEntries`] per
//! directory exactly as the kernel walk would hand it — the tree, the ledger and the viewers see
//! no difference. Hard links come out counted, so only files with more than one name go
//! through the ledger. NTFS's own files at the root and under `$Extend` are sized as
//! [`crate::ntfs`] sizes them, by the clusters their runs occupy.
//!
//! The MFT on disk lags the filesystem's memory by however long NTFS holds its metadata before
//! writing it: the last seconds of writes are not in it yet. A rescan (`r`) goes through the
//! kernel, so what is looked at closely is current. A volume that will not open — unelevated,
//! or not NTFS — makes this decline before it has said anything, and the kernel walk takes over.
//!
//! The parsing and the tree are plain byte handling and run on every platform, with tests;
//! only [`walk_mft`] touches a volume.

use ::std::ffi::OsString;
use ::std::path::Path;
use ::std::sync::Arc;

use super::{DirEntries, EntryMeta};
use crate::ntfs::{self, Run, u16_at, u32_at, u64_at};

const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;
const END: u32 = 0xFFFF_FFFF;

const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIRECTORY: u16 = 0x0002;
const ATTRIBUTE_COMPRESSED: u16 = 0x0001;
const ATTRIBUTE_SPARSE: u16 = 0x8000;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// The DOS (8.3) namespace of a `$FILE_NAME`: a second name for the same entry, never one of
/// its own.
const NAMESPACE_DOS: u8 = 2;

/// The root directory's record, and the last record NTFS reserves for its own files.
pub const ROOT_RECORD: u32 = 5;
const LAST_SYSTEM_RECORD: u32 = 15;
const EXTEND_RECORD: u32 = 11;

/// Entries per channel message to the tree builder, as the kernel walk batches.
const SEND_BATCH: usize = 4096;

/// One name a record has: the directory it is listed in, and in which namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub parent: u32,
    pub namespace: u8,
    pub name: Box<[u16]>,
}

/// What one in-use file record says, before its extension records are merged in.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub number: u32,
    pub sequence: u16,
    /// The base record this one extends, or `None` when it is a base record itself. Read
    /// from the raw reference, whose sequence number keeps an extension of record 0 apart
    /// from a base record.
    pub base: Option<u32>,
    pub is_dir: bool,
    /// A junction or symbolic link, which the walk does not follow.
    pub link: bool,
    pub names: Vec<Name>,
    /// The unnamed `$DATA` stream's size on disk and length, from the piece that maps its start.
    pub data: Option<(u64, u64)>,
    /// Clusters every non-resident attribute occupies, for NTFS's own files.
    pub clusters: u64,
}

/// Read one file record as it is in the MFT: `None` for a record not in use, or not a record.
#[must_use]
pub fn parse_file_record(record: &[u8]) -> Option<Parsed> {
    let mut copy = record.to_vec();
    parse_file_record_in_place(&mut copy)
}

/// [`parse_file_record`] on a record that may be written to: the update sequence is undone in
/// place rather than in a copy, which is what reading a whole table wants.
pub fn parse_file_record_in_place(record: &mut [u8]) -> Option<Parsed> {
    if record.get(0..4)? != b"FILE" {
        return None;
    }
    ntfs::apply_fixups(record)?;
    let record: &[u8] = record;
    let flags = u16_at(record, 0x16)?;
    if flags & RECORD_IN_USE == 0 {
        return None;
    }
    let used = (u32_at(record, 0x18)? as usize).min(record.len());
    let mut parsed = Parsed {
        number: u32_at(record, 0x2C)?,
        sequence: u16_at(record, 0x10)?,
        base: match u64_at(record, 0x20)? {
            0 => None,
            reference => Some(u32::try_from(ntfs::record_number(reference)).ok()?),
        },
        is_dir: flags & RECORD_IS_DIRECTORY != 0,
        ..Parsed::default()
    };
    let mut at = usize::from(u16_at(record, 0x14)?);
    while at + 8 <= used {
        let kind = u32_at(record, at)?;
        if kind == END {
            break;
        }
        let length = u32_at(record, at + 4)? as usize;
        if length < 0x18 || at + length > used {
            break;
        }
        let attribute = &record[at..at + length];
        let resident = attribute[8] == 0;
        let name_length = attribute[9];
        let attribute_flags = u16_at(attribute, 0x0C)?;
        if resident {
            let value_length = u32_at(attribute, 0x10)? as usize;
            let value_offset = usize::from(u16_at(attribute, 0x14)?);
            let value = attribute.get(value_offset..value_offset + value_length);
            match (kind, value) {
                (FILE_NAME, Some(value)) => {
                    if let Some((name, reparse_tag)) = parse_file_name(value) {
                        parsed.names.push(name);
                        parsed.link |= is_link(reparse_tag);
                    }
                }
                (DATA, Some(value)) if name_length == 0 && parsed.data.is_none() => {
                    // Resident: the bytes live in the record, and the listing says 0 allocated.
                    parsed.data = Some((0, value.len() as u64));
                }
                (REPARSE_POINT, Some(value)) => {
                    parsed.link |= is_link(u32_at(value, 0));
                }
                _ => {}
            }
        } else if length >= 0x40 {
            let runs_at = usize::from(u16_at(attribute, 0x20)?);
            parsed.clusters = parsed
                .clusters
                .saturating_add(ntfs::occupied_clusters(attribute.get(runs_at..)?));
            let lowest_vcn = u64_at(attribute, 0x10)?;
            if kind == DATA && name_length == 0 && lowest_vcn == 0 && parsed.data.is_none() {
                let allocated = u64_at(attribute, 0x28)?;
                let real = u64_at(attribute, 0x30)?;
                // Compressed and sparse files carry what they actually occupy in a field of
                // their own; the allocated size counts the holes.
                let on_disk = if attribute_flags & (ATTRIBUTE_COMPRESSED | ATTRIBUTE_SPARSE) != 0
                    && length >= 0x48
                {
                    u64_at(attribute, 0x40)?
                } else {
                    allocated
                };
                parsed.data = Some((on_disk, real));
            }
        }
        at += length;
    }
    Some(parsed)
}

/// A junction or symbolic link, which the walk does not follow, by its reparse tag; every
/// other reparse point (OneDrive placeholders, dedup) is a real file or directory with a tag.
fn is_link(reparse_tag: Option<u32>) -> bool {
    matches!(
        reparse_tag,
        Some(IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK)
    )
}

/// A `$FILE_NAME` value: the parent's reference, the file attributes at 0x38 and — for a
/// reparse point — its tag at 0x3C, then the name's length and namespace at 0x40. The tag is
/// read here rather than from `$REPARSE_POINT`, which may be non-resident and out of reach.
fn parse_file_name(value: &[u8]) -> Option<(Name, Option<u32>)> {
    let parent = u32::try_from(ntfs::record_number(u64_at(value, 0)?)).ok()?;
    let attributes = u32_at(value, 0x38)?;
    let reparse_tag = (attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .then(|| u32_at(value, 0x3C))
        .flatten();
    let length = usize::from(*value.get(0x40)?);
    let namespace = *value.get(0x41)?;
    let bytes = value.get(0x42..0x42 + length * 2)?;
    let name: Box<[u16]> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some((
        Name {
            parent,
            namespace,
            name,
        },
        reparse_tag,
    ))
}

/// The runs of `$MFT`'s own data, from record 0: where the table is on the volume. A piece of
/// the stream in an extension record is returned with the VCN it starts at, for ordering.
#[must_use]
pub fn data_runs(record: &[u8]) -> Option<(u64, Vec<Run>)> {
    let mut record = record.to_vec();
    ntfs::apply_fixups(&mut record)?;
    let used = (u32_at(&record, 0x18)? as usize).min(record.len());
    let mut at = usize::from(u16_at(&record, 0x14)?);
    while at + 8 <= used {
        let kind = u32_at(&record, at)?;
        if kind == END {
            break;
        }
        let length = u32_at(&record, at + 4)? as usize;
        if length < 0x18 || at + length > used {
            break;
        }
        let attribute = &record[at..at + length];
        if kind == DATA && attribute[8] != 0 && attribute[9] == 0 && length >= 0x40 {
            let runs_at = usize::from(u16_at(attribute, 0x20)?);
            return Some((
                u64_at(attribute, 0x10)?,
                ntfs::decode_runs(attribute.get(runs_at..)?),
            ));
        }
        at += length;
    }
    None
}

/// One directory entry as the walk hands it on, with the record it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub record: u32,
    pub meta: EntryMeta,
    /// Clusters the record's attributes occupy: the size of one of NTFS's own files.
    clusters: u64,
}

/// Every directory's entries, by the directory's record number.
pub struct Catalog {
    children: Vec<Vec<Entry>>,
    cluster_bytes: u64,
}

fn os_name(name: &[u16]) -> OsString {
    #[cfg(windows)]
    {
        ::std::os::windows::ffi::OsStringExt::from_wide(name)
    }
    #[cfg(not(windows))]
    {
        OsString::from(String::from_utf16_lossy(name))
    }
}

/// The identity the kernel walk gives a file: its reference, folded with the volume's serial as
/// `windows::read_directory` folds a listing's id, so a rescan through the kernel grafts into a
/// table's tree with the same ids.
fn file_id(number: u32, sequence: u16, volume: u64) -> u64 {
    let reference = u64::from(number) | (u64::from(sequence) << 48);
    ntfs::fold_file_id(reference, 0, volume)
}

impl Catalog {
    /// Put every record's names under their directories. Extension records go into their base
    /// first, so a file whose attributes spill over several records is one file.
    #[must_use]
    pub fn assemble(mut records: Vec<Option<Parsed>>, volume: u64, cluster_bytes: u64) -> Self {
        let count = records.len();
        for index in 0..count {
            let extension = match &records[index] {
                Some(parsed) if parsed.base.is_some_and(|base| base != parsed.number) => {
                    records[index].take()
                }
                _ => None,
            };
            if let Some(extension) = extension
                && let Some(base) = extension.base
                && let Some(Some(base)) = records.get_mut(base as usize)
            {
                base.names.extend(extension.names);
                base.data = base.data.or(extension.data);
                base.clusters = base.clusters.saturating_add(extension.clusters);
                base.link |= extension.link;
            }
        }
        let mut children: Vec<Vec<Entry>> = (0..count).map(|_| Vec::new()).collect();
        for parsed in records.into_iter().flatten() {
            // One entry per name, except a DOS name beside a Win32 one in the same directory:
            // that is one entry under its short name. Two Win32 names in one directory are two
            // hard links, and both are listed, as the kernel lists them. The root's own `.` is
            // nobody's entry.
            let kept: Vec<&Name> = parsed
                .names
                .iter()
                .filter(|name| {
                    if name.parent == parsed.number && parsed.number == ROOT_RECORD {
                        return false;
                    }
                    name.namespace != NAMESPACE_DOS
                        || !parsed.names.iter().any(|other| {
                            other.parent == name.parent && other.namespace != NAMESPACE_DOS
                        })
                })
                .collect();
            if kept.is_empty() {
                continue;
            }
            let (size, apparent) = parsed.data.unwrap_or((0, 0));
            let meta = EntryMeta {
                size,
                apparent,
                inode: file_id(parsed.number, parsed.sequence, volume),
                links: kept.len() as u64,
                is_dir: parsed.is_dir && !parsed.link,
                shared_extent: 0,
            };
            for name in kept {
                if let Some(siblings) = children.get_mut(name.parent as usize) {
                    siblings.push(Entry {
                        name: os_name(&name.name),
                        record: parsed.number,
                        meta,
                        clusters: parsed.clusters,
                    });
                }
            }
        }
        Catalog {
            children,
            cluster_bytes,
        }
    }

    /// How many directories have entries: for the report.
    #[must_use]
    pub fn directories(&self) -> usize {
        self.children.iter().filter(|c| !c.is_empty()).count()
    }

    /// Hand on every directory under `root_record` — at `root` — breadth-first, in batches of
    /// about [`SEND_BATCH`] entries, until `send` says the consumer has gone.
    pub fn emit(
        mut self,
        root_record: u32,
        root: Arc<Path>,
        max_depth: Option<usize>,
        mut send: impl FnMut(Vec<DirEntries>) -> bool,
    ) {
        struct Pending {
            record: u32,
            path: Arc<Path>,
            depth: usize,
            /// `$Extend` or below it: NTFS's own files, sized by their clusters.
            system: bool,
        }
        let mut frontier = ::std::collections::VecDeque::from([Pending {
            record: root_record,
            path: root,
            depth: 0,
            system: false,
        }]);
        let mut outbox: Vec<DirEntries> = Vec::new();
        let mut outbox_entries = 0usize;
        while let Some(pending) = frontier.pop_front() {
            let Some(entries) = self.children.get_mut(pending.record as usize) else {
                continue;
            };
            let entries = ::std::mem::take(entries);
            let name_bytes = entries.iter().map(|e| e.name.len()).sum();
            let mut directory =
                DirEntries::with_capacity(Arc::clone(&pending.path), entries.len(), name_bytes);
            // As the kernel walkers count it (`job.depth + 1 < max`): `--max-depth 1` is the
            // root's entries alone.
            let descend = max_depth.is_none_or(|max| pending.depth + 1 < max);
            for entry in entries {
                let mut meta = entry.meta;
                let system_file = pending.system
                    || (pending.record == ROOT_RECORD && entry.record <= LAST_SYSTEM_RECORD);
                if system_file && !meta.is_dir {
                    // Blocks of the volume's, with no length a user would recognise: no
                    // apparent size, as the kernel walk gives them.
                    meta.size = entry.clusters.saturating_mul(self.cluster_bytes);
                    meta.apparent = 0;
                }
                if meta.is_dir && descend {
                    let path = pending.path.join(&entry.name);
                    frontier.push_back(Pending {
                        record: entry.record,
                        path: Arc::from(path.as_path()),
                        depth: pending.depth + 1,
                        system: pending.system
                            || (pending.record == ROOT_RECORD && entry.record == EXTEND_RECORD),
                    });
                }
                directory.push(&entry.name, meta);
            }
            outbox_entries += directory.len().max(1);
            outbox.push(directory);
            if outbox_entries >= SEND_BATCH {
                outbox_entries = 0;
                if !send(::std::mem::take(&mut outbox)) {
                    return;
                }
            }
        }
        if !outbox.is_empty() {
            send(outbox);
        }
    }
}

#[cfg(windows)]
pub use volume::{MftWalk, walk_mft, would_read_device};

/// Reading the volume: Windows only.
#[cfg(windows)]
mod volume {
    use ::std::fs::File;
    use ::std::os::windows::fs::{FileExt, OpenOptionsExt};
    use ::std::os::windows::io::AsRawHandle;
    use ::std::path::{Component, Path, PathBuf, Prefix};
    use ::std::sync::Arc;
    use ::std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use ::std::thread::JoinHandle;
    use ::std::time::Instant;

    use super::{Catalog, Parsed, data_runs, parse_file_record_in_place};
    use crate::ntfs::{self, Run};
    use crate::{DirEntries, ScanOptions};

    const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;
    const FSCTL_GET_NTFS_FILE_RECORD: u32 = 0x0009_0068;
    const FILE_ID_INFO: i32 = 18;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    /// One read of the table at most this long: a few dozen of them for a whole `C:\`.
    const CHUNK: usize = 32 << 20;

    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn DeviceIoControl(
            hDevice: *mut ::std::ffi::c_void,
            dwIoControlCode: u32,
            lpInBuffer: *const u8,
            nInBufferSize: u32,
            lpOutBuffer: *mut u8,
            nOutBufferSize: u32,
            lpBytesReturned: *mut u32,
            lpOverlapped: *mut u8,
        ) -> i32;
        fn GetFileInformationByHandleEx(
            hFile: *mut ::std::ffi::c_void,
            FileInformationClass: i32,
            lpFileInformation: *mut ::std::ffi::c_void,
            dwBufferSize: u32,
        ) -> i32;
        fn FlushFileBuffers(hFile: *mut ::std::ffi::c_void) -> i32;
        fn GetVolumeInformationByHandleW(
            hFile: *mut ::std::ffi::c_void,
            lpVolumeNameBuffer: *mut u16,
            nVolumeNameSize: u32,
            lpVolumeSerialNumber: *mut u32,
            lpMaximumComponentLength: *mut u32,
            lpFileSystemFlags: *mut u32,
            lpFileSystemNameBuffer: *mut u16,
            nFileSystemNameSize: u32,
        ) -> i32;
    }

    fn control(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> Option<usize> {
        let mut returned = 0u32;
        // SAFETY: both buffers are live for the call and their lengths are the ones passed.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                code,
                input.as_ptr(),
                input.len() as u32,
                output.as_mut_ptr(),
                output.len() as u32,
                &raw mut returned,
                ::std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(returned as usize)
    }

    fn u32_le(buffer: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(buffer[at..at + 4].try_into().expect("4 bytes"))
    }
    fn u64_le(buffer: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(buffer[at..at + 8].try_into().expect("8 bytes"))
    }

    /// An NTFS volume open for reading, and where its table is.
    struct Volume {
        file: File,
        device: String,
        /// Whether NTFS was asked to write its metadata out first, so the table is current.
        flushed: bool,
        cluster_bytes: u64,
        record_bytes: usize,
        /// Bytes of the table in use: records past this are not there.
        valid_bytes: u64,
        volume_bytes: u64,
    }

    impl Volume {
        /// Open the volume `root` is on. `None` unelevated, or off NTFS.
        fn open(root: &Path) -> Option<Volume> {
            let Some(Component::Prefix(prefix)) = root.components().next() else {
                return None;
            };
            let letter = match prefix.kind() {
                Prefix::VerbatimDisk(letter) | Prefix::Disk(letter) => char::from(letter),
                _ => return None,
            };
            let device = format!(r"\\.\{letter}:");
            let file = File::open(&device).ok()?;
            // `NTFS_VOLUME_DATA_BUFFER`: total clusters at 16, bytes per cluster at 44, bytes
            // per file record at 48, the table's valid data length at 56.
            // ReFS and FAT refuse the call.
            let mut data = [0u8; 128];
            control(&file, FSCTL_GET_NTFS_VOLUME_DATA, &[], &mut data)?;
            let cluster_bytes = u64::from(u32_le(&data, 44));
            let record_bytes = u32_le(&data, 48) as usize;
            let valid_bytes = u64_le(&data, 56);
            let volume_bytes = u64_le(&data, 16).saturating_mul(cluster_bytes);
            ((512..=65536).contains(&record_bytes)
                && cluster_bytes > 0
                && cluster_bytes % 512 == 0
                && valid_bytes > 0)
                .then_some(Volume {
                    file,
                    device,
                    flushed: false,
                    cluster_bytes,
                    record_bytes,
                    valid_bytes,
                    volume_bytes,
                })
        }

        /// Ask NTFS to write out the metadata it is holding, so the table is current: the
        /// table on disk otherwise lags the filesystem by seconds. `FlushFileBuffers` on the
        /// volume — what `Write-VolumeCache` does — takes a handle open for writing, which an
        /// administrator may have; nothing is written by this process. Only once the table is
        /// going to be read: a flush of a large volume can take longer than a walk of a small
        /// tree, so the gate's probe must not pay it.
        fn flush(&mut self) {
            let Ok(writable) = ::std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.device)
            else {
                return;
            };
            // SAFETY: the handle is open; the call has no other preconditions.
            self.flushed = unsafe { FlushFileBuffers(writable.as_raw_handle()) } != 0;
        }

        /// A record as the filesystem returns it — the table's own, before its runs are known.
        fn fetch_record(&self, number: u64) -> Option<Vec<u8>> {
            let mut output = vec![0u8; 12 + self.record_bytes];
            control(
                &self.file,
                FSCTL_GET_NTFS_FILE_RECORD,
                &number.to_le_bytes(),
                &mut output,
            )?;
            if ntfs::record_number(u64_le(&output, 0)) != number {
                return None;
            }
            let length = (u32_le(&output, 8) as usize).min(self.record_bytes);
            Some(output[12..12 + length].to_vec())
        }

        /// Where the table's bytes are on the volume, in order: `(offset, length)`, `valid_bytes`
        /// of them in all. The table's data stream may spill into extension records when the
        /// volume has fragmented it; each piece says which VCN it starts at.
        fn table_runs(&self) -> Option<Vec<(u64, u64)>> {
            let zero = self.fetch_record(0)?;
            let mut pieces = vec![data_runs(&zero)?];
            for extension in ntfs::parse_record(&zero)?.extensions {
                if let Some(record) = self.fetch_record(extension)
                    && let Some(piece) = data_runs(&record)
                {
                    pieces.push(piece);
                }
            }
            pieces.sort_by_key(|(vcn, _)| *vcn);
            let mut runs = Vec::new();
            let mut left = self.valid_bytes;
            for (_, piece) in pieces {
                for run in piece {
                    let (Some(lcn), clusters): Run = run else {
                        return None;
                    };
                    let length = clusters.saturating_mul(self.cluster_bytes).min(left);
                    if length == 0 {
                        continue;
                    }
                    // A run that ends mid-record (clusters smaller than a record, an odd
                    // count) would put every record after it out of step: decline.
                    if length % self.record_bytes as u64 != 0 {
                        return None;
                    }
                    let offset = lcn.saturating_mul(self.cluster_bytes);
                    if offset.saturating_add(length) > self.volume_bytes {
                        return None;
                    }
                    runs.push((offset, length));
                    left -= length;
                }
            }
            // Anything short of the whole table would be a tree with files silently missing:
            // decline instead, and the kernel walk takes over.
            (left == 0 && !runs.is_empty()).then_some(runs)
        }

        /// Entries a directory holds on this volume, on average — files over directories among
        /// the base records in a slice from the middle of the table's first run. What decides
        /// between the table and the walk: see [`chosen`].
        fn entries_per_directory(&self, runs: &[(u64, u64)]) -> Option<f64> {
            const SAMPLE: u64 = 8 << 20;
            let record_bytes = self.record_bytes as u64;
            let &(offset, length) = runs.first()?;
            let sample = SAMPLE.min(length).min(self.valid_bytes) / record_bytes * record_bytes;
            let start = offset + (length - sample) / 2 / record_bytes * record_bytes;
            let mut buffer = vec![0u8; usize::try_from(sample).ok()?];
            let mut got = 0usize;
            while got < buffer.len() {
                let read = self
                    .file
                    .seek_read(&mut buffer[got..], start + got as u64)
                    .ok()?;
                if read == 0 {
                    break;
                }
                got += read;
            }
            let (mut files, mut directories) = (0u64, 0u64);
            for record in buffer[..got].chunks_exact(self.record_bytes) {
                // The flags and the base reference are in the first sector, clear of the fixups.
                if &record[0..4] != b"FILE" {
                    continue;
                }
                let flags = u16::from_le_bytes([record[0x16], record[0x17]]);
                let base = u64_le(record, 0x20);
                if flags & 0x0001 == 0 || ntfs::record_number(base) != 0 {
                    continue;
                }
                if flags & 0x0002 != 0 {
                    directories += 1;
                } else {
                    files += 1;
                }
            }
            (files + directories > 0).then(|| files as f64 / directories.max(1) as f64)
        }
    }

    /// The record number of the directory at `path`, and the volume's serial: from the
    /// filesystem, since the path is all that is known.
    fn directory_record(path: &Path) -> Option<(u32, u64)> {
        let dir = ::std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()?;
        // `FILE_ID_INFO`: the volume serial, then the 128-bit id whose low half is the file
        // reference.
        let mut info = [0u8; 24];
        // SAFETY: `info` is live and as long as the size passed.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                dir.as_raw_handle(),
                FILE_ID_INFO,
                info.as_mut_ptr().cast(),
                info.len() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let record = u32::try_from(ntfs::record_number(u64_le(&info, 8))).ok()?;
        let mut serial = 0u32;
        // SAFETY: only the serial is asked for; every other pointer is null as the API allows.
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                dir.as_raw_handle(),
                ::std::ptr::null_mut(),
                0,
                &raw mut serial,
                ::std::ptr::null_mut(),
                ::std::ptr::null_mut(),
                ::std::ptr::null_mut(),
                0,
            )
        };
        (ok != 0).then_some((record, u64::from(serial)))
    }

    /// Entries per directory above which the walk is left to the kernel.
    ///
    /// The table costs the whole volume's records whatever the tree — about 1.3 µs a record
    /// here, 2.3 GB of `C:\` in 1.3 s — and the walk costs a handle per directory, about 12 µs
    /// on a system volume with its filter drivers. So the table pays where directories are
    /// small: on that `C:\` (5.4 entries a directory) it took 3.1 s against the walk's 5.8 s
    /// warm and 8.0 s cold; on a data volume of large files (16 a directory) 0.63 s against
    /// 0.15 s. Measured in `docs/scan-performance.md`, "The master file table".
    const TABLE_UP_TO: f64 = 8.0;

    /// Whether, and with what, `root` is read from the table: a volume root — a subtree is a
    /// fraction of the volume, and the table costs all of it: `C:\Windows` (415k entries)
    /// took 5.7 s against the walk's 4.6 s, `C:\Program Files` 2.3 s against 0.6 s — on a
    /// volume that opens and reads as NTFS, whose directories are small enough for the table
    /// to pay.
    fn chosen(root: &Path) -> Result<Chosen, String> {
        let is_volume_root = !root
            .components()
            .any(|component| matches!(component, Component::Normal(_)));
        if !is_volume_root {
            return Err("not a volume root".to_string());
        }
        let volume = Volume::open(root)
            .ok_or_else(|| "the volume does not open (not NTFS, or not elevated)".to_string())?;
        let runs = volume
            .table_runs()
            .ok_or_else(|| "the table's runs could not be read".to_string())?;
        let ratio = volume
            .entries_per_directory(&runs)
            .ok_or_else(|| "the table could not be sampled".to_string())?;
        // Printed under `--benchmark --bench-profile`, with the build profile.
        if libduscape::model::files::profile::enabled() {
            eprintln!(
                "  mft: {:.1} entries a directory in the sample, {} of table{}",
                ratio,
                libduscape::DisplaySize(volume.valid_bytes as f64),
                if ratio > TABLE_UP_TO {
                    ": walking instead"
                } else {
                    ""
                }
            );
        }
        if ratio > TABLE_UP_TO {
            return Err(format!(
                "{ratio:.1} entries a directory in the table's sample, over {TABLE_UP_TO}"
            ));
        }
        let (record, serial) = directory_record(root)
            .ok_or_else(|| "the root's record could not be found".to_string())?;
        Ok(Chosen {
            volume,
            record,
            serial,
            runs,
            ratio,
        })
    }

    /// What [`chosen`] decided on: the volume, the scan root's record, the volume's serial,
    /// the table's runs, and the entries-per-directory figure.
    struct Chosen {
        volume: Volume,
        record: u32,
        serial: u64,
        runs: Vec<(u64, u64)>,
        ratio: f64,
    }

    /// Whether a scan of `root` by this process would read the table — NTFS, the volume opens
    /// (elevated), and the tree and the volume are ones the table pays for — or why not. For
    /// the benchmark's report.
    pub fn would_read_device(root: &Path) -> Result<(), String> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        chosen(&root).map(|_| ())
    }

    /// The walk of an NTFS volume from its table, one [`DirEntries`] per directory.
    pub struct MftWalk {
        batches: Receiver<Vec<DirEntries>>,
        current: ::std::vec::IntoIter<DirEntries>,
        emitter: Option<JoinHandle<()>>,
    }

    impl Iterator for MftWalk {
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

    impl Drop for MftWalk {
        fn drop(&mut self) {
            drop(::std::mem::replace(&mut self.batches, sync_channel(1).1));
            if let Some(emitter) = self.emitter.take() {
                let _ = emitter.join();
            }
        }
    }

    /// Read and parse the whole table: every record, by number.
    fn read_table(
        volume: &Volume,
        runs: Vec<(u64, u64)>,
        threads: usize,
    ) -> Result<Vec<Option<Parsed>>, String> {
        let record_bytes = volume.record_bytes;
        let count = usize::try_from(volume.valid_bytes / record_bytes as u64)
            .map_err(|_| "too many records".to_string())?;
        let chunk_bytes = CHUNK - CHUNK % record_bytes;
        let mut records: Vec<Option<Parsed>> = (0..count).map(|_| None).collect();
        let (chunks, chunk_inbox) = sync_channel::<(usize, Vec<u8>)>(threads * 2);
        let (parsed_out, parsed_inbox) = sync_channel::<(usize, Vec<Option<Parsed>>)>(threads * 2);
        let chunk_inbox = ::std::sync::Mutex::new(chunk_inbox);
        ::std::thread::scope(|scope| -> Result<(), String> {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    let inbox = &chunk_inbox;
                    let out = parsed_out.clone();
                    scope.spawn(move || {
                        loop {
                            let next = inbox.lock().expect("chunk inbox poisoned").recv();
                            let Ok((first, bytes)) = next else { return };
                            let mut bytes = bytes;
                            let parsed: Vec<Option<Parsed>> = bytes
                                .chunks_exact_mut(record_bytes)
                                .map(parse_file_record_in_place)
                                .collect();
                            if out.send((first, parsed)).is_err() {
                                return;
                            }
                        }
                    })
                })
                .collect();
            drop(parsed_out);
            let place = |records: &mut Vec<Option<Parsed>>,
                         (start, parsed): (usize, Vec<Option<Parsed>>)| {
                for (index, record) in parsed.into_iter().enumerate() {
                    if let Some(slot) = records.get_mut(start + index) {
                        *slot = record;
                    }
                }
            };
            // Read on this thread, collecting what the workers hand back between reads so that
            // neither side waits on the other for long.
            let mut read_all = || -> Result<(), String> {
                let mut first = 0usize;
                for &(offset, length) in &runs {
                    let mut at = 0u64;
                    while at < length {
                        let take = usize::try_from((length - at).min(chunk_bytes as u64))
                            .map_err(|_| "chunk".to_string())?;
                        let mut buffer = vec![0u8; take];
                        let mut got = 0usize;
                        while got < take {
                            let read = volume
                                .file
                                .seek_read(&mut buffer[got..], offset + at + got as u64)
                                .map_err(|e| format!("reading the table: {e}"))?;
                            if read == 0 {
                                return Err("the table ended early".to_string());
                            }
                            got += read;
                        }
                        let records_in = take / record_bytes;
                        chunks
                            .send((first, buffer))
                            .map_err(|_| "a parser stopped".to_string())?;
                        first += records_in;
                        at += take as u64;
                        while let Ok(done) = parsed_inbox.try_recv() {
                            place(&mut records, done);
                        }
                    }
                }
                Ok(())
            };
            let outcome = read_all();
            // Whatever happened: close the workers' inbox, take everything they still hand
            // back — one blocked on a full channel is waiting for exactly that — then join.
            drop(chunks);
            while let Ok(done) = parsed_inbox.recv() {
                place(&mut records, done);
            }
            for worker in workers {
                let _ = worker.join();
            }
            outcome
        })?;
        Ok(records)
    }

    /// Walk `root` from its volume's table, if the volume is NTFS and opens; `None` means the
    /// kernel walk should be used instead, and nothing has been read that matters. The table
    /// is read and parsed before this returns, so that a failure can still fall back.
    pub fn walk_mft(root: &Path, threads: usize, options: ScanOptions) -> Option<MftWalk> {
        let root: PathBuf = root.canonicalize().ok()?;
        let Chosen {
            mut volume,
            record: root_record,
            serial,
            runs,
            ratio,
        } = chosen(&root).ok()?;
        let started = Instant::now();
        volume.flush();
        let threads = threads.clamp(1, 8);
        let records = match read_table(&volume, runs, threads) {
            Ok(records) => records,
            Err(error) => {
                eprintln!("duscape: reading the volume's table failed, walking instead: {error}");
                return None;
            }
        };
        let read_took = started.elapsed();
        let catalog = Catalog::assemble(records, serial, volume.cluster_bytes);
        if libduscape::model::files::profile::enabled() {
            eprintln!(
                "  mft: {} of table{} read and parsed in {:.3}s, {} directories assembled by {:.3}s; {:.1} entries a directory in the sample",
                libduscape::DisplaySize(volume.valid_bytes as f64),
                if volume.flushed {
                    " (flushed)"
                } else {
                    " (not flushed)"
                },
                read_took.as_secs_f64(),
                catalog.directories(),
                started.elapsed().as_secs_f64(),
                ratio
            );
        }
        let root: Arc<Path> = Arc::from(root.as_path());
        let (sender, batches): (SyncSender<Vec<DirEntries>>, Receiver<Vec<DirEntries>>) =
            sync_channel(64);
        let emitter = ::std::thread::Builder::new()
            .name("mft_emitter".to_string())
            .spawn(move || {
                catalog.emit(root_record, root, options.max_depth, |batch| {
                    sender.send(batch).is_ok()
                });
            })
            .ok()?;
        Some(MftWalk {
            batches,
            current: Vec::new().into_iter(),
            emitter: Some(emitter),
        })
    }
}

#[cfg(test)]
mod tests;
