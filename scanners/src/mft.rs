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
//! Nothing can be listed until the table is read whole — a directory's files are scattered
//! through it — which on a system volume is a second or two with nothing on screen. So while it
//! is read the live view gets *running totals* (`Running`, on a thread of its own): each
//! directory's files so far, every quarter second, as a [`DirEntries`] for the view alone
//! (`Audience::View`), at the place its records give — the top levels' places seeded through
//! the kernel, since the records are in no tree order. The listing that follows is the tree's
//! alone (`Audience::Tree`). `docs/scan-performance.md`, "First paint", has the measurements.
//!
//! The MFT on disk lags the filesystem's memory by however long NTFS holds its metadata before
//! writing it: the last seconds of writes are not in it yet. A rescan (`r`) goes through the
//! kernel, so what is looked at closely is current. A volume that will not open — unelevated,
//! or not NTFS — makes this decline before it has said anything, and the kernel walk takes over.
//!
//! The parsing and the tree are plain byte handling and run on every platform, with tests;
//! only `walk_mft` touches a volume.

use ::std::ffi::OsString;
use ::std::path::Path;
use ::std::sync::Arc;

use libduscape::model::files::hash::FastMap;

use super::{Audience, DirEntries, EntryMeta, Unlisted};
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Which of record `number`'s `names` are entries: one per name, except a DOS name beside a
/// Win32 one in the same directory — that is one entry under its short name. Two Win32 names in
/// one directory are two hard links, and both are listed, as the kernel lists them. The root's
/// own `.` is nobody's entry.
fn kept_names(number: u32, names: &[Name]) -> impl Iterator<Item = &Name> {
    names.iter().filter(move |name| {
        if name.parent == number && number == ROOT_RECORD {
            return false;
        }
        name.namespace != NAMESPACE_DOS
            || !names
                .iter()
                .any(|other| other.parent == name.parent && other.namespace != NAMESPACE_DOS)
    })
}

/// Whether an entry named in `parent` by record `number` is one of NTFS's own files, sized by
/// its clusters: the root's first records, and `$Extend`'s.
fn is_system_file(number: u32, parent: u32) -> bool {
    (parent == ROOT_RECORD && number <= LAST_SYSTEM_RECORD) || parent == EXTEND_RECORD
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
            let kept: Vec<&Name> = kept_names(parsed.number, &parsed.names).collect();
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
    /// about `SEND_BATCH` entries, until `send` says the consumer has gone.
    pub fn emit(
        mut self,
        root_record: u32,
        root: Arc<Path>,
        max_depth: Option<usize>,
        snapshots: bool,
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
            // The live view has had this directory as running totals while the table was
            // read (`Running`); the listing is the tree's alone.
            directory.audience = Audience::Tree;
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
                // A share's snapshots by name (`.snapshots`, `.zfs` mirrored onto NTFS): listed,
                // not entered, as every walker leaves them.
                let left_out = meta.is_dir && directory.leave_out(&entry.name, descend, snapshots);
                if meta.is_dir && descend && !left_out {
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

/// What one chunk of records, as parsed, adds toward the live view: each file's sizes under the
/// directory record its name is in; the directories among them, by record, with the name and
/// parent the view resolves places by; and the names an extension record carries for a base
/// record elsewhere in the table, to be charged with that record's data once both are read.
/// Made on the parser's thread ([`Charges::of`]), so the thread reading the table only hands
/// them on, and the view's own thread takes them in ([`Running::take`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Charges {
    /// Directory record → what its files in this chunk add.
    pub files: Vec<(u32, Unlisted)>,
    /// The directories in this chunk, by record.
    pub dirs: Vec<(u32, DirRecord)>,
    /// Base record → the names an extension record of it carries.
    pub spilled: Vec<(u32, Vec<Name>)>,
    /// Base records with data and no name of their own — their names are in an extension
    /// record — kept whole for when those names come.
    pub nameless: Vec<Parsed>,
}

/// A directory's record as the view needs it: its name, and the directory it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRecord {
    pub parent: u32,
    pub name: Box<[u16]>,
}

impl Charges {
    /// The charges `chunk` makes. A directory charges nothing but is noted, with its Win32 name
    /// and parent (a junction is not: nothing is under it); a file is charged to each directory
    /// it is named in, hard links counted in full as the outline counts them; NTFS's own files
    /// by their clusters as the listing sizes them.
    #[must_use]
    pub fn of(chunk: &[Option<Parsed>], cluster_bytes: u64) -> Charges {
        let mut files: FastMap<u32, Unlisted> = FastMap::default();
        let mut dirs = Vec::new();
        let mut spilled = Vec::new();
        let mut nameless = Vec::new();
        for parsed in chunk.iter().flatten() {
            if parsed.base.is_some_and(|base| base != parsed.number) {
                if let Some(base) = parsed.base
                    && !parsed.names.is_empty()
                {
                    spilled.push((base, parsed.names.clone()));
                }
                continue;
            }
            if parsed.is_dir && !parsed.link {
                if let Some(named) = win32_name(&parsed.names) {
                    dirs.push((
                        parsed.number,
                        DirRecord {
                            parent: named.parent,
                            name: named.name.clone(),
                        },
                    ));
                }
                continue;
            }
            if parsed.names.is_empty() {
                if parsed.data.is_some() {
                    nameless.push(parsed.clone());
                }
                continue;
            }
            for name in kept_names(parsed.number, &parsed.names) {
                let (size, apparent) = sizes_of(parsed, name.parent, cluster_bytes);
                let slot = files.entry(name.parent).or_default();
                slot.size = slot.size.saturating_add(size);
                slot.apparent = slot.apparent.saturating_add(apparent);
                slot.count += 1;
            }
        }
        Charges {
            files: files.into_iter().collect(),
            dirs,
            spilled,
            nameless,
        }
    }
}

/// The Win32 name among `names`, or the one name there is: a directory has one, in one parent.
fn win32_name(names: &[Name]) -> Option<&Name> {
    names
        .iter()
        .find(|name| name.namespace != NAMESPACE_DOS)
        .or(names.first())
}

/// A file's sizes as the listing will give them for its name in `parent`.
fn sizes_of(parsed: &Parsed, parent: u32, cluster_bytes: u64) -> (u64, u64) {
    if is_system_file(parsed.number, parent) {
        (parsed.clusters.saturating_mul(cluster_bytes), 0)
    } else {
        parsed.data.unwrap_or((0, 0))
    }
}

/// How the view resolves a directory record to its place in the scan.
enum Place {
    /// Its path.
    At(Arc<Path>),
    /// An ancestor's record is not read yet: later.
    Unknown,
    /// The listing will not have it — past `--max-depth`, under a folder left out by name, or
    /// under a link — so the view is not to either.
    Excluded,
}

/// The live view's share of the table as it is read: every file's sizes charged to its
/// directory's record as its chunk is parsed, and every so often ([`Running::flush`]) each
/// directory's total since last time handed on as one [`DirEntries`] for the view alone
/// (`Audience::View`, the sizes as its `unlisted`), at the path its ancestors' records give —
/// a directory whose ancestors are not all read yet waits for a later flush.
///
/// The records come in table order, a directory's files scattered through the whole of it, so
/// nothing can be *listed* until the table is read whole: a second or two on a system volume
/// with nothing on screen. The totals run up from the first chunk, so the treemap is up at
/// once and its tiles grow as the table is read — the view the kernel walk gives, by other
/// means. The records are in no tree order either (on an upgraded Windows `C:\Windows` itself
/// is a late record), so the top levels' places are *seeded* through the kernel's listing
/// ([`Running::seed`]) rather than waited for.
///
/// Where the listing would stop — `--max-depth`, a snapshot folder left out by name, a link —
/// nothing is charged, so the totals are what the listing will add up to, hard links counted in
/// full as the outline counts them. A file whose names are in an extension record is charged
/// when that record and its base are both read. What never resolves (an orphaned record, a
/// chain through a junction) is dropped with the view; the finished tree takes its place anyway.
pub struct Running {
    root: Arc<Path>,
    root_record: u32,
    max_depth: Option<usize>,
    snapshots: bool,
    cluster_bytes: u64,
    /// Directory record → its name and parent, from the chunks read so far.
    dirs: FastMap<u32, DirRecord>,
    /// Base records with data and no names, by record, for the names an extension brings.
    nameless: FastMap<u32, Parsed>,
    /// Directory record → what its files have added since it was last handed on.
    pending: FastMap<u32, Unlisted>,
    /// Names from extension records, waiting for their base record to be read.
    spilled: Vec<(u32, Vec<Name>)>,
    /// Each directory record's place once resolved — its path and depth, or `None` where the
    /// listing will not go — so a directory charged in every chunk is resolved once, not once
    /// a flush: resolved every time, eight flushes of a 2.46M-record table cost 1.65 s (the
    /// chain of records to the root, a name made of each step).
    places: FastMap<u32, Option<(Arc<Path>, usize)>>,
}

impl Running {
    #[must_use]
    pub fn new(
        root: Arc<Path>,
        root_record: u32,
        max_depth: Option<usize>,
        snapshots: bool,
        cluster_bytes: u64,
    ) -> Self {
        Self {
            root,
            root_record,
            max_depth,
            snapshots,
            cluster_bytes,
            dirs: FastMap::default(),
            nameless: FastMap::default(),
            pending: FastMap::default(),
            spilled: Vec::new(),
            places: FastMap::default(),
        }
    }

    /// Take one chunk's charges in.
    pub fn take(&mut self, charges: Charges) {
        for (directory, unlisted) in charges.files {
            self.charge(directory, unlisted);
        }
        for (record, dir) in charges.dirs {
            self.dirs.insert(record, dir);
        }
        for parsed in charges.nameless {
            self.nameless.insert(parsed.number, parsed);
        }
        self.spilled.extend(charges.spilled);
    }

    fn charge(&mut self, directory: u32, unlisted: Unlisted) {
        let slot = self.pending.entry(directory).or_default();
        slot.size = slot.size.saturating_add(unlisted.size);
        slot.apparent = slot.apparent.saturating_add(unlisted.apparent);
        slot.count += unlisted.count;
    }

    /// Whether anything waits to be handed on.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.spilled.is_empty()
    }

    /// How many directories' totals wait for an ancestor's record: for the profile.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.pending.len()
    }

    /// A directory's place known from elsewhere — the kernel's listing of its parent, which
    /// says its record — before its record is read from the table: `Some` its path and depth,
    /// `None` a folder the listing will not enter. Records are in no tree order: on an
    /// upgraded Windows `C:\Windows` itself is a late record (a feature update makes it anew),
    /// and every chain through it waited for that one chunk — four flushes of 2.46M records
    /// resolved 5, 185, 11 and 11 directories, then 139k at once.
    pub fn seed(&mut self, record: u32, place: Option<(Arc<Path>, usize)>) {
        self.places.entry(record).or_insert(place);
    }

    /// Hand on every directory's total since the last flush whose place is known. What is not
    /// known yet stays for the next flush; what the listing will not have is dropped.
    pub fn flush(&mut self) -> Vec<DirEntries> {
        // Names an extension record carried, now that their base record may be read.
        let spilled = ::std::mem::take(&mut self.spilled);
        for (base, names) in spilled {
            let Some(record) = self.nameless.get(&base) else {
                self.spilled.push((base, names));
                continue;
            };
            let charges: Vec<(u32, Unlisted)> = kept_names(base, &names)
                .map(|name| {
                    let (size, apparent) = sizes_of(record, name.parent, self.cluster_bytes);
                    (
                        name.parent,
                        Unlisted {
                            size,
                            apparent,
                            count: 1,
                        },
                    )
                })
                .collect();
            for (directory, unlisted) in charges {
                self.charge(directory, unlisted);
            }
        }
        let mut out = Vec::new();
        let pending = ::std::mem::take(&mut self.pending);
        for (directory, unlisted) in pending {
            match self.place(directory) {
                Place::At(path) => {
                    let mut total = DirEntries::new(path);
                    total.unlisted = unlisted;
                    total.audience = Audience::View { last: false };
                    out.push(total);
                }
                Place::Unknown => {
                    self.pending.insert(directory, unlisted);
                }
                Place::Excluded => {}
            }
        }
        out
    }

    /// Where directory record `directory` is, from its and its ancestors' records: as the
    /// listing walks down, left out where it would not go. Resolved once and kept (`places`),
    /// each ancestor on the way too; a chain cut short by a record not read yet keeps nothing,
    /// and is walked again at the next flush from where its known part ends.
    fn place(&mut self, directory: u32) -> Place {
        // Deeper than any real tree: a parent reference that loops.
        const MOST_STEPS: usize = 1024;
        if let Some(known) = self.places.get(&directory) {
            return match known {
                Some((path, depth)) => self.within_depth(Arc::clone(path), *depth),
                None => Place::Excluded,
            };
        }
        // Up from the directory to the first record whose place is known — the root's is —
        // collecting the records on the way with their names.
        let mut chain: Vec<(u32, OsString)> = Vec::new();
        let mut at = directory;
        let base: Option<(Arc<Path>, usize)> = loop {
            if at == self.root_record {
                break Some((Arc::clone(&self.root), 0));
            }
            if let Some(known) = self.places.get(&at) {
                break known.clone();
            }
            let Some(record) = self.dirs.get(&at) else {
                return Place::Unknown;
            };
            if chain.len() > MOST_STEPS {
                break None;
            }
            let name = os_name(&record.name);
            if libduscape::nas::left_out(&name, self.snapshots).is_some() {
                break None;
            }
            chain.push((at, name));
            at = record.parent;
        };
        let Some((mut path, mut depth)) = base else {
            // Under a folder left out, or a loop: nothing under it is the listing's.
            for (record, _) in chain {
                self.places.insert(record, None);
            }
            return Place::Excluded;
        };
        // Down the chain, each step's path made once from its parent's.
        for (record, name) in chain.into_iter().rev() {
            path = Arc::from(path.join(name).as_path());
            depth += 1;
            self.places.insert(record, Some((Arc::clone(&path), depth)));
        }
        self.within_depth(path, depth)
    }

    /// A resolved directory's place, unless it is past `--max-depth`: as the listing counts it,
    /// `--max-depth 1` is the root's entries alone, so a folder `max` deep is listed but not
    /// entered, and its files are nobody's.
    fn within_depth(&self, path: Arc<Path>, depth: usize) -> Place {
        if self.max_depth.is_some_and(|max| depth >= max) {
            Place::Excluded
        } else {
            Place::At(path)
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
    use ::std::sync::atomic::{AtomicBool, Ordering};
    use ::std::sync::mpsc::{Receiver, sync_channel};
    use ::std::thread::JoinHandle;
    use ::std::time::{Duration, Instant};

    use super::{
        Audience, Catalog, Charges, Parsed, Running, data_runs, parse_file_record_in_place,
    };
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
        /// Open for reading and, elevated, writing — one handle for the probe, the flush and
        /// the read: a volume handle's first I/O costs 0.4–0.8 s (see `entries_per_directory`),
        /// and a handle each paid it three times over.
        file: File,
        /// Whether `file` may be flushed: opened for writing.
        writable: bool,
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
            // For writing too where allowed, so the flush needs no handle of its own; nothing
            // is ever written through it.
            let (file, writable) = match ::std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&device)
            {
                Ok(file) => (file, true),
                Err(_) => (File::open(&device).ok()?, false),
            };
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
                    writable,
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
        fn flush(&self) -> bool {
            if !self.writable {
                return false;
            }
            // SAFETY: the handle is open; the call has no other preconditions.
            unsafe { FlushFileBuffers(self.file.as_raw_handle()) != 0 }
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
        /// the base records in a sample of `SAMPLE_RECORDS` spread evenly over the table. What
        /// decides between the table and the walk: see [`chosen`].
        ///
        /// Fetched one by one with `FSCTL_GET_NTFS_FILE_RECORD`, not read from the volume: a
        /// volume handle's *first read* costs 0.4–0.8 s whatever its size (4 KiB: 390–811 ms;
        /// the same handle's next 8 MiB: 5.6 ms; a fresh unbuffered handle: 390 ms again —
        /// 2026-10-01, `docs/scan-performance.md`), and the probe runs before anything is on
        /// screen, so an 8 MiB slice read here was 0.4 s of the first paint. The ioctl shows no
        /// such cost (the table's runs come through it in 0.1 ms), and the first read is paid
        /// once, by the table's first chunk, after the window is up.
        fn entries_per_directory(&self) -> Option<f64> {
            const SAMPLE_RECORDS: u64 = 1024;
            let count = self.valid_bytes / self.record_bytes as u64;
            if count == 0 {
                return None;
            }
            let stride = (count / SAMPLE_RECORDS).max(1);
            let (mut files, mut directories) = (0u64, 0u64);
            let mut number = stride / 2;
            while number < count {
                // A record not in use comes back as the in-use one below it, which
                // `fetch_record` declines; the flags and the base reference are in the first
                // sector, clear of the fixups.
                if let Some(record) = self.fetch_record(number)
                    && record.len() >= 0x28
                    && &record[0..4] == b"FILE"
                {
                    let flags = u16::from_le_bytes([record[0x16], record[0x17]]);
                    let base = u64_le(&record, 0x20);
                    if flags & 0x0001 != 0 && ntfs::record_number(base) == 0 {
                        if flags & 0x0002 != 0 {
                            directories += 1;
                        } else {
                            files += 1;
                        }
                    }
                }
                number += stride;
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
        let began = Instant::now();
        let volume = Volume::open(root)
            .ok_or_else(|| "the volume does not open (not NTFS, or not elevated)".to_string())?;
        let opened = began.elapsed();
        let runs = volume
            .table_runs()
            .ok_or_else(|| "the table's runs could not be read".to_string())?;
        let runs_read = began.elapsed();
        let ratio = volume
            .entries_per_directory()
            .ok_or_else(|| "the table could not be sampled".to_string())?;
        // The probe runs before anything is on screen, so its cost is the first paint's.
        if libduscape::model::files::profile::enabled() {
            eprintln!(
                "  mft: the volume opened in {:.1} ms, its table's {} runs read by {:.1} ms, sampled by {:.1} ms",
                opened.as_secs_f64() * 1000.0,
                runs.len(),
                runs_read.as_secs_f64() * 1000.0,
                began.elapsed().as_secs_f64() * 1000.0
            );
        }
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
        let root = libduscape::os::canonical_root(root);
        chosen(&root).map(|_| ())
    }

    /// What the thread reading the table hands the walk.
    enum Report {
        /// Directories: running totals for the view while the table is read, then the listing
        /// for the tree.
        Directories(Vec<DirEntries>),
        /// The read failed part way; the kernel walk is to take over.
        Failed(String),
    }

    /// The walk of an NTFS volume from its table, one [`DirEntries`] per directory: the view's
    /// running totals as the table is read, then the tree's listing. `Err` once, if the read
    /// failed part way (`Report::Failed`): the caller walks through the kernel instead.
    pub struct MftWalk {
        reports: Receiver<Report>,
        current: ::std::vec::IntoIter<DirEntries>,
        /// Set to stop the reader between chunks, when the walk is dropped before it ends.
        stop: Arc<AtomicBool>,
        reader: Option<JoinHandle<()>>,
    }

    impl Iterator for MftWalk {
        type Item = Result<DirEntries, String>;
        fn next(&mut self) -> Option<Result<DirEntries, String>> {
            loop {
                if let Some(next) = self.current.next() {
                    return Some(Ok(next));
                }
                match self.reports.recv().ok()? {
                    Report::Directories(batch) => self.current = batch.into_iter(),
                    Report::Failed(error) => return Some(Err(error)),
                }
            }
        }
    }

    impl Drop for MftWalk {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            drop(::std::mem::replace(&mut self.reports, sync_channel(1).1));
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    /// Read and parse the whole table: every record, by number. `live` sees each chunk's
    /// charges toward the view as the chunk lands, with the table as read so far; `stop` ends
    /// the read between chunks.
    fn read_table(
        volume: &Volume,
        runs: Vec<(u64, u64)>,
        threads: usize,
        stop: &AtomicBool,
        mut live: impl FnMut(Charges),
    ) -> Result<Vec<Option<Parsed>>, String> {
        let record_bytes = volume.record_bytes;
        let cluster_bytes = volume.cluster_bytes;
        let count = usize::try_from(volume.valid_bytes / record_bytes as u64)
            .map_err(|_| "too many records".to_string())?;
        let chunk_bytes = CHUNK - CHUNK % record_bytes;
        let mut records: Vec<Option<Parsed>> = (0..count).map(|_| None).collect();
        let (chunks, chunk_inbox) = sync_channel::<(usize, Vec<u8>)>(threads * 2);
        let (parsed_out, parsed_inbox) =
            sync_channel::<(usize, Vec<Option<Parsed>>, Charges)>(threads * 2);
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
                            // The view's charges here, on the parser's thread, so the reading
                            // thread only merges them between reads.
                            let charges = if live_totals_off() {
                                Charges::default()
                            } else {
                                Charges::of(&parsed, cluster_bytes)
                            };
                            if out.send((first, parsed, charges)).is_err() {
                                return;
                            }
                        }
                    })
                })
                .collect();
            drop(parsed_out);
            let mut place =
                |records: &mut Vec<Option<Parsed>>,
                 (start, parsed, charges): (usize, Vec<Option<Parsed>>, Charges)| {
                    for (index, record) in parsed.into_iter().enumerate() {
                        if let Some(slot) = records.get_mut(start + index) {
                            *slot = record;
                        }
                    }
                    live(charges);
                };
            // Read on this thread, collecting what the workers hand back between reads so that
            // neither side waits on the other for long.
            let mut read_all = || -> Result<(), String> {
                let mut first = 0usize;
                for &(offset, length) in &runs {
                    let mut at = 0u64;
                    while at < length {
                        if stop.load(Ordering::Acquire) {
                            return Err("the scan was stopped".to_string());
                        }
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

    /// How often the running totals go to the view while the table is read: the first chunk's
    /// at once, so the treemap is up as soon as the root's folders are read, then no oftener
    /// than this — a flush resolves every directory charged since the last by its records, on
    /// the reading thread, between two reads of the table.
    const LIVE_EVERY: Duration = Duration::from_millis(250);

    /// How deep the kernel's listing goes for the directories' records (`seed_places`): the
    /// outline's own depth, below which the view rolls folders up into the one above anyway.
    /// Breadth first, so the levels the view shows come first; it stops when the table is read.
    const SEED_DEPTH: usize = 6;

    /// A directory's record and its place, from the kernel's listing of its parent.
    type Seed = (u32, Option<(Arc<Path>, usize)>);

    /// `DUSCAPE_NO_LIVE_TOTALS`: the table read with no running totals to the view, as it was
    /// before 2026-10-01 — the baseline their cost is measured against (`--bench-profile`'s
    /// `mft:` line), never the default.
    fn live_totals_off() -> bool {
        static OFF: ::std::sync::OnceLock<bool> = ::std::sync::OnceLock::new();
        *OFF.get_or_init(|| ::std::env::var_os("DUSCAPE_NO_LIVE_TOTALS").is_some())
    }

    /// List the tree under `root` through the kernel, a level at a time to `SEED_DEPTH`, and
    /// send each directory's record with its place (`Running::seed`): the table's records are in
    /// no tree order, and a chain of them to the root resolves only once every one is read —
    /// through the kernel the top levels are known in milliseconds, so the totals under them
    /// resolve from the first chunk. A listing costs a handle a directory (about 12 µs on a
    /// system volume), the whole tree seconds, so it runs beside the volume's flush and the
    /// read and ends with them (`done`), having covered the levels that matter most.
    fn seed_places(
        root: Arc<Path>,
        snapshots: bool,
        stop: &AtomicBool,
        done: &AtomicBool,
        seeds: &::std::sync::mpsc::SyncSender<Vec<Seed>>,
    ) {
        // Elevated, so every folder lists, as the kernel walk's does.
        let _ = libduscape::os::enable_backup_privilege();
        let mut level: Vec<Arc<Path>> = vec![root];
        let mut depth = 0usize;
        while !level.is_empty() && depth < SEED_DEPTH {
            let mut next = Vec::new();
            for path in level {
                if stop.load(Ordering::Acquire) || done.load(Ordering::Acquire) {
                    return;
                }
                let mut batch: Vec<Seed> = Vec::new();
                for (name, reference, is_dir) in crate::windows::list_entries(&path) {
                    if !is_dir {
                        continue;
                    }
                    let Ok(record) = u32::try_from(ntfs::record_number(reference)) else {
                        continue;
                    };
                    if libduscape::nas::left_out(&name, snapshots).is_some() {
                        batch.push((record, None));
                        continue;
                    }
                    let child: Arc<Path> = Arc::from(path.join(&name).as_path());
                    batch.push((record, Some((Arc::clone(&child), depth + 1))));
                    next.push(child);
                }
                if !batch.is_empty() && seeds.send(batch).is_err() {
                    return;
                }
            }
            level = next;
            depth += 1;
        }
    }

    /// The reading thread's work: the volume flushed, the table read with the view's running
    /// totals going out as it is, then the listing. What `walk_mft` decided on, and where the
    /// reports go.
    struct Reading {
        volume: Volume,
        runs: Vec<(u64, u64)>,
        root: Arc<Path>,
        root_record: u32,
        serial: u64,
        ratio: f64,
        threads: usize,
        options: ScanOptions,
        stop: Arc<AtomicBool>,
        sender: ::std::sync::mpsc::SyncSender<Report>,
    }

    /// What the live totals' thread reports at its end: how many flushes, what they took, how
    /// many places the kernel's listing seeded.
    type LiveReport = (u32, Duration, usize);

    /// The view's running totals, on a thread of their own (`mft_live`): the chunks' charges
    /// and the kernel's seeds come in, and every `LIVE_EVERY` each directory's total since last
    /// time goes out, the last of them marked so the outline sends the batch at once. On the
    /// reading thread the flushes — resolving 340k places, half a second in all — were a
    /// quarter of the read (2.65 s against 2.13 s without them); here they cost it nothing.
    /// Ends when the charges' channel does (the table read, or the scan stopped), with the
    /// last totals, every place known by then.
    fn live_totals(
        mut running: Running,
        charges: ::std::sync::mpsc::Receiver<Charges>,
        seeds: ::std::sync::mpsc::Receiver<Vec<Seed>>,
        sender: ::std::sync::mpsc::SyncSender<Report>,
        started: Instant,
    ) -> LiveReport {
        let mut seeded = 0usize;
        let mut flushes = 0u32;
        let mut took = Duration::ZERO;
        let mut last: Option<Instant> = None;
        let mut dirty = false;
        let flush = |running: &mut Running, flushes: &mut u32, took: &mut Duration| -> bool {
            let began = Instant::now();
            let mut totals = running.flush();
            *took += began.elapsed();
            *flushes += 1;
            if libduscape::model::files::profile::enabled() {
                eprintln!(
                    "  mft: running totals {} at {:.3}s: {} directories to the view, {} waiting for a record, {:.1} ms",
                    *flushes,
                    started.elapsed().as_secs_f64(),
                    totals.len(),
                    running.waiting(),
                    began.elapsed().as_secs_f64() * 1000.0
                );
            }
            if let Some(total) = totals.last_mut() {
                total.audience = Audience::View { last: true };
            }
            totals.is_empty() || sender.send(Report::Directories(totals)).is_ok()
        };
        loop {
            let wait = match last {
                Some(last) if dirty => LIVE_EVERY.saturating_sub(last.elapsed()),
                _ => Duration::from_millis(50),
            };
            match charges.recv_timeout(wait) {
                Ok(charges) => {
                    running.take(charges);
                    dirty = true;
                }
                Err(::std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(::std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(batch) = seeds.try_recv() {
                seeded += batch.len();
                for (record, place) in batch {
                    running.seed(record, place);
                }
            }
            if dirty && last.is_none_or(|last| last.elapsed() >= LIVE_EVERY) {
                if !flush(&mut running, &mut flushes, &mut took) {
                    return (flushes, took, seeded);
                }
                last = Some(Instant::now());
                dirty = false;
            }
        }
        // The table is read: every directory's place is known, so the last totals go whole.
        while let Ok(batch) = seeds.try_recv() {
            seeded += batch.len();
            for (record, place) in batch {
                running.seed(record, place);
            }
        }
        flush(&mut running, &mut flushes, &mut took);
        (flushes, took, seeded)
    }

    impl Reading {
        fn run(mut self) {
            let started = Instant::now();
            // The directories' places through the kernel, beside the flush and the read.
            let (seed_sender, seeds) = ::std::sync::mpsc::sync_channel::<Vec<Seed>>(256);
            let seeding_done = Arc::new(AtomicBool::new(false));
            let seeder = {
                let root = Arc::clone(&self.root);
                let stop = Arc::clone(&self.stop);
                let done = Arc::clone(&seeding_done);
                let snapshots = self.options.snapshots;
                ::std::thread::Builder::new()
                    .name("mft_seeder".to_string())
                    .spawn(move || seed_places(root, snapshots, &stop, &done, &seed_sender))
            };
            // The view's running totals, on their thread; the charges' channel is unbounded,
            // so the read never waits on a flush.
            let (charges_sender, charges) = ::std::sync::mpsc::channel::<Charges>();
            let live = if live_totals_off() {
                drop((charges, seeds));
                None
            } else {
                let running = Running::new(
                    Arc::clone(&self.root),
                    self.root_record,
                    self.options.max_depth,
                    self.options.snapshots,
                    self.volume.cluster_bytes,
                );
                let sender = self.sender.clone();
                ::std::thread::Builder::new()
                    .name("mft_live".to_string())
                    .spawn(move || live_totals(running, charges, seeds, sender, started))
                    .ok()
            };
            // The flush, then the read, whose first chunk pays the handle's first-read cost
            // (0.4–0.8 s, see `entries_per_directory`): the two do not overlap — tried side by
            // side on 2026-10-01, the read of one sector took 0.46–0.74 s and both were done
            // by 0.84–1.11 s, as one after the other — so the seeding is what runs beside them.
            let runs = ::std::mem::take(&mut self.runs);
            self.volume.flushed = self.volume.flush();
            let flush_took = started.elapsed();
            let read = read_table(&self.volume, runs, self.threads, &self.stop, |charges| {
                let _ = charges_sender.send(charges);
            });
            // The live thread's cue that the table is read, and the seeder's.
            drop(charges_sender);
            seeding_done.store(true, Ordering::Release);
            let records = match read {
                Ok(records) => records,
                Err(_) if self.stop.load(Ordering::Acquire) => return,
                Err(error) => {
                    let _ = self.sender.send(Report::Failed(error));
                    return;
                }
            };
            let read_took = started.elapsed();
            let catalog = Catalog::assemble(records, self.serial, self.volume.cluster_bytes);
            let assembled = started.elapsed();
            // The last totals have gone out meanwhile; the seeder ends with the live thread,
            // whose seeds' channel it sends into.
            let (flushes, live_took, seeded) =
                live.and_then(|live| live.join().ok()).unwrap_or_default();
            if let Ok(seeder) = seeder {
                let _ = seeder.join();
            }
            if libduscape::model::files::profile::enabled() {
                eprintln!(
                    "  mft: {} of table{} in {:.3}s, read and parsed by {:.3}s ({flushes} running totals to the view, {:.3}s of it on their thread; {seeded} places seeded through the kernel), {} directories assembled by {:.3}s; {:.1} entries a directory in the sample",
                    libduscape::DisplaySize(self.volume.valid_bytes as f64),
                    if self.volume.flushed {
                        " flushed"
                    } else {
                        " not flushed"
                    },
                    flush_took.as_secs_f64(),
                    read_took.as_secs_f64(),
                    live_took.as_secs_f64(),
                    catalog.directories(),
                    assembled.as_secs_f64(),
                    self.ratio
                );
            }
            catalog.emit(
                self.root_record,
                self.root,
                self.options.max_depth,
                self.options.snapshots,
                |batch| self.sender.send(Report::Directories(batch)).is_ok(),
            );
        }
    }

    /// Walk `root` from its volume's table, if the volume is NTFS and opens; `None` means the
    /// kernel walk should be used instead, and nothing has been read that matters. The decision
    /// is made before this returns; the table is read on a thread of its own (`mft_reader`),
    /// the view's running totals coming as it goes and the tree's listing once it is read
    /// whole, so the window shows the root's folders within the first chunk rather than after
    /// the last. A read that fails part way is reported through the walk (`Err`), and the
    /// caller walks through the kernel instead.
    pub fn walk_mft(root: &Path, threads: usize, options: ScanOptions) -> Option<MftWalk> {
        root.canonicalize().ok()?;
        let root: PathBuf = libduscape::os::canonical_root(root);
        let Chosen {
            volume,
            record: root_record,
            serial,
            runs,
            ratio,
        } = chosen(&root).ok()?;
        let root: Arc<Path> = Arc::from(root.as_path());
        let threads = threads.clamp(1, 8);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let (sender, reports) = sync_channel::<Report>(64);
        let reading = Reading {
            volume,
            runs,
            root,
            root_record,
            serial,
            ratio,
            threads,
            options,
            stop: reader_stop,
            sender,
        };
        let reader = ::std::thread::Builder::new()
            .name("mft_reader".to_string())
            .spawn(move || reading.run())
            .ok()?;
        Some(MftWalk {
            reports,
            current: Vec::new().into_iter(),
            stop,
            reader: Some(reader),
        })
    }
}

#[cfg(test)]
mod tests;
