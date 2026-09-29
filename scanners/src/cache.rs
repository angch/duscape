//! The saved scan: every folder's listing written out as the walk ends, and read back at the
//! next start with only the folders the volume's change log names since then listed again.
//!
//! Where the metadata cannot be read off the device — APFS through a FileVault, which is every
//! Mac — a first scan costs the kernel's floor (`docs/scan-performance.md`, "macOS: what is
//! left"). Every scan after it need not: the volume keeps a log of which directories changed
//! (FSEvents on macOS, [`crate::fsevents`]), so the saved stream of [`DirEntries`] can be
//! replayed with the changed directories read afresh, the directories gone dropped with what
//! was under them, the directories new walked, and the ledger rebuilt by the tree as on any
//! scan. The save is a tee on whatever walker ran ([`Recorder`]); the replay is a walker of its
//! own ([`CachedScan`]), behind the same seam as the others, so every viewer gets it.
//!
//! The file (`Saved`) is the stream in order — parent before child, as the walkers send it —
//! one length-prefixed record a directory, its entries as LEB128 varints, a footer with the
//! counts, the key it was made for, the change log's id and a checksum; after the magic and
//! the version, the lot is one gzip member (`flate2`, the deflate a zip reader would share):
//! names deflate to a third, and the read is a fraction of the walk it replaces. Nothing yields until
//! the log has been replayed and the changed directories listed: that pass is what patches a
//! hard-linked file's size everywhere it is named, since the ledger identifies a file by its
//! disk size and a saved copy at the old size would count as a second file.
//!
//! What is not carried: `later` and `extent_space` (the Linux second pass's; no macOS walker
//! fills them) and a directory's issues (the failure count is). What is deliberately not
//! handled yet, and said so in the roadmap: a new subdirectory that is a mount point is walked
//! as a root, so `-x` does not apply to it; a whole-tree rescan (`R`) does not refresh the file.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use libduscape::scan::{DirEntries, EntryMeta, ScanOptions};

const MAGIC: &[u8; 12] = b"DUSCAPE-SCAN";
const FOOT: &[u8; 8] = b"END-SCAN";
const VERSION: u32 = 1;

/// A saved scan is not brought up to date when the log names more directories than this
/// share of what it holds: listing them costs about what walking would, and the file is
/// then a day too old to be worth keeping.
pub const RELIST_AT_MOST: f64 = 0.25;

/// What a saved scan is keyed on: the root and the options that shape what a walk of it
/// reads. A file made for another key is not used, whatever its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub root: PathBuf,
    pub max_depth: Option<usize>,
    pub one_file_system: bool,
    pub snapshots: bool,
}

impl Key {
    #[must_use]
    pub fn new(root: &Path, options: ScanOptions) -> Self {
        Self {
            root: root.to_path_buf(),
            max_depth: options.max_depth,
            one_file_system: options.one_file_system,
            snapshots: options.snapshots,
        }
    }

    /// The file's name under the cache directory: a hash of the key, the key itself checked
    /// on reading.
    #[must_use]
    pub fn file_name(&self) -> String {
        let mut hash = Fnv::new();
        hash.update(self.root.as_os_str().as_encoded_bytes());
        hash.update(&self.max_depth.map_or(u64::MAX, |d| d as u64).to_le_bytes());
        hash.update(&[u8::from(self.one_file_system), u8::from(self.snapshots)]);
        format!("{:016x}.scan", hash.finish())
    }
}

/// What the file says about when it was made: what it takes to know whether the change log
/// since then is the whole story.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Stamp {
    /// The change log's id at the moment the walk began — before it, so a change made while
    /// it ran is replayed next time.
    pub event_id: u64,
    /// The root's device, and the log's identity for it (`FSEventsCopyUUIDForDevice`): a
    /// device that shows another has had its log reset, and nothing since is known.
    pub device: u64,
    pub log_uuid: String,
    /// The kernel's version: an OS update rewrites the sealed system volume with no events.
    pub system: String,
    /// Seconds since the epoch, for the note the next scan carries.
    pub saved_at: u64,
}

/// A saved scan's footer: what it was, and how much of it there is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub key: Key,
    pub stamp: Stamp,
    pub directories: u64,
    pub entries: u64,
}

// --- the varint encoding ---------------------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// A cursor over saved bytes; every read is bounds-checked, so a truncated or damaged file
/// reads as `None` and is discarded, never trusted.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.bytes.get(self.pos)?;
            self.pos += 1;
            if shift >= 64 {
                return None;
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.varint()?).ok()?;
        let slice = self.bytes.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(slice)
    }

    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }
}

/// FNV-1a, 64-bit: the checksum over the body, and the file name's hash.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

// --- one directory's record --------------------------------------------------------------------

const IS_DIR: u8 = 1;
const APPARENT_DIFFERS: u8 = 2;
const LINKED: u8 = 4;
const SHARED: u8 = 8;

/// Encode one directory: its path relative to the root, its failure count, its entries.
fn encode_directory(out: &mut Vec<u8>, relative: &[u8], directory: &DirEntries) {
    let mut record = Vec::with_capacity(32 + directory.len() * 24);
    put_bytes(&mut record, relative);
    put_varint(&mut record, directory.failed);
    put_varint(&mut record, directory.len() as u64);
    for (name, meta) in directory.iter() {
        put_bytes(&mut record, name.as_encoded_bytes());
        let mut flags = 0u8;
        if meta.is_dir {
            flags |= IS_DIR;
        }
        if meta.apparent != meta.size {
            flags |= APPARENT_DIFFERS;
        }
        if meta.links != 1 {
            flags |= LINKED;
        }
        if meta.shared_extent != 0 {
            flags |= SHARED;
        }
        record.push(flags);
        put_varint(&mut record, meta.size);
        if flags & APPARENT_DIFFERS != 0 {
            put_varint(&mut record, meta.apparent);
        }
        put_varint(&mut record, meta.inode);
        if flags & LINKED != 0 {
            put_varint(&mut record, meta.links);
        }
        if flags & SHARED != 0 {
            put_varint(&mut record, meta.shared_extent);
        }
    }
    put_bytes(out, &record);
}

/// Decode one directory's record into a listing under `root`.
fn decode_directory(record: &[u8], root: &Path) -> Option<(PathBuf, DirEntries)> {
    let mut cursor = Cursor {
        bytes: record,
        pos: 0,
    };
    let relative = PathBuf::from(os_str(cursor.bytes()?));
    let failed = cursor.varint()?;
    let count = usize::try_from(cursor.varint()?).ok()?;
    let path: Arc<Path> = Arc::from(join_relative(root, &relative).as_path());
    let mut directory = DirEntries::with_capacity(path, count.min(1 << 20), 0);
    directory.failed = failed;
    for _ in 0..count {
        let name = os_str(cursor.bytes()?);
        let flags = *record.get(cursor.pos)?;
        cursor.pos += 1;
        let size = cursor.varint()?;
        let apparent = if flags & APPARENT_DIFFERS != 0 {
            cursor.varint()?
        } else {
            size
        };
        let inode = cursor.varint()?;
        let links = if flags & LINKED != 0 {
            cursor.varint()?
        } else {
            1
        };
        let shared_extent = if flags & SHARED != 0 {
            cursor.varint()?
        } else {
            0
        };
        directory.push(
            name,
            EntryMeta {
                size,
                apparent,
                inode,
                links,
                is_dir: flags & IS_DIR != 0,
                shared_extent,
            },
        );
    }
    cursor.done().then_some((relative, directory))
}

fn os_str(bytes: &[u8]) -> &OsStr {
    // SAFETY: the bytes were written from `OsStr::as_encoded_bytes` by `encode_directory`,
    // and a file that was not written so is one whose names are read as this platform's
    // encoding permits — a damaged name, not undefined behaviour, on Unix where every byte
    // string is an `OsStr`. The cache is written and read on the same machine.
    unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
}

/// The root itself is saved as the empty relative path.
fn join_relative(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

// --- the footer ---------------------------------------------------------------------------

fn encode_header(header: &Header, checksum: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, header.key.root.as_os_str().as_encoded_bytes());
    put_varint(
        &mut out,
        header.key.max_depth.map_or(u64::MAX, |d| d as u64),
    );
    out.push(u8::from(header.key.one_file_system));
    out.push(u8::from(header.key.snapshots));
    put_varint(&mut out, header.stamp.event_id);
    put_varint(&mut out, header.stamp.device);
    put_bytes(&mut out, header.stamp.log_uuid.as_bytes());
    put_bytes(&mut out, header.stamp.system.as_bytes());
    put_varint(&mut out, header.stamp.saved_at);
    put_varint(&mut out, header.directories);
    put_varint(&mut out, header.entries);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

fn decode_header(bytes: &[u8]) -> Option<(Header, u64)> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let root = PathBuf::from(os_str(cursor.bytes()?));
    let max_depth = match cursor.varint()? {
        u64::MAX => None,
        depth => Some(usize::try_from(depth).ok()?),
    };
    let one_file_system = *bytes.get(cursor.pos)? != 0;
    let snapshots = *bytes.get(cursor.pos + 1)? != 0;
    cursor.pos += 2;
    let event_id = cursor.varint()?;
    let device = cursor.varint()?;
    let log_uuid = cursor.string()?;
    let system = cursor.string()?;
    let saved_at = cursor.varint()?;
    let directories = cursor.varint()?;
    let entries = cursor.varint()?;
    let checksum = u64::from_le_bytes(bytes.get(cursor.pos..cursor.pos + 8)?.try_into().ok()?);
    Some((
        Header {
            key: Key {
                root,
                max_depth,
                one_file_system,
                snapshots,
            },
            stamp: Stamp {
                event_id,
                device,
                log_uuid,
                system,
                saved_at,
            },
            directories,
            entries,
        },
        checksum,
    ))
}

// --- writing -------------------------------------------------------------------------------

/// The scan being written as it streams: a tee on the walker, into a file beside the final
/// one that is renamed into place only when the stream ends. Dropped before that, the file is
/// removed: a stopped scan saves nothing.
pub struct Recorder<I> {
    inner: I,
    writer: Option<Writing>,
}

struct Writing {
    /// Encoded records go to the writer thread, which deflates and writes them: the scan is
    /// the walk's time and no more, and one record's encoding is all it pays here.
    records: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<Option<GzEncoder<BufWriter<File>>>>>,
    temp: PathBuf,
    final_path: PathBuf,
    root: PathBuf,
    header: Header,
    checksum: Fnv,
}

impl<I: Iterator<Item = DirEntries>> Recorder<I> {
    /// Tee `inner` into `dir/<key's file name>`, stamped with `stamp`. A directory that cannot
    /// be made, or a file that cannot be opened, means no recording, silently: the scan is
    /// not held up by its cache.
    pub fn new(inner: I, dir: &Path, key: Key, stamp: Stamp) -> Self {
        let writer = Self::open(dir, key, stamp);
        Self { inner, writer }
    }

    fn open(dir: &Path, key: Key, stamp: Stamp) -> Option<Writing> {
        fs::create_dir_all(dir).ok()?;
        let final_path = dir.join(key.file_name());
        let temp = dir.join(format!("{}.{}.part", key.file_name(), std::process::id()));
        let file = open_private(&temp).ok()?;
        let mut plain = BufWriter::with_capacity(1 << 20, file);
        plain.write_all(MAGIC).ok()?;
        plain.write_all(&VERSION.to_le_bytes()).ok()?;
        // The fastest level: the names are what compress, and the write runs beside the walk.
        let mut file = GzEncoder::new(plain, Compression::fast());
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
        let writer = std::thread::Builder::new()
            .name("scan_recorder".to_string())
            .spawn(move || {
                while let Ok(record) = receiver.recv() {
                    if file.write_all(&record).is_err() {
                        return None;
                    }
                }
                Some(file)
            })
            .ok()?;
        let root = key.root.clone();
        Some(Writing {
            records: Some(sender),
            writer: Some(writer),
            temp,
            final_path,
            root,
            header: Header {
                key,
                stamp,
                directories: 0,
                entries: 0,
            },
            checksum: Fnv::new(),
        })
    }

    /// The same iterator, recording nothing: for a scan that is not to be saved.
    #[must_use]
    pub fn plain(inner: I) -> Self {
        Self {
            inner,
            writer: None,
        }
    }

    fn record(&mut self, directory: &DirEntries) {
        let Some(writing) = self.writer.as_mut() else {
            return;
        };
        let relative = match directory.path.strip_prefix(&writing.root) {
            Ok(relative) => relative.as_os_str().as_encoded_bytes().to_vec(),
            // A directory from outside the root has no place in the tree either.
            Err(_) => return,
        };
        let mut record = Vec::with_capacity(32 + directory.len() * 24);
        encode_directory(&mut record, &relative, directory);
        writing.checksum.update(&record);
        writing.header.directories += 1;
        writing.header.entries += directory.len() as u64;
        let sent = writing
            .records
            .as_ref()
            .is_some_and(|records| records.send(record).is_ok());
        if !sent {
            self.abandon();
        }
    }

    fn finish(&mut self) {
        let Some(mut writing) = self.writer.take() else {
            return;
        };
        let footer = encode_header(&writing.header, writing.checksum.finish());
        let mut tail = footer.clone();
        tail.extend_from_slice(&(footer.len() as u32).to_le_bytes());
        tail.extend_from_slice(FOOT);
        let done = writing
            .records
            .take()
            .and_then(|records| records.send(tail).ok())
            .and_then(|()| writing.writer.take()?.join().ok()?)
            .and_then(|file| file.finish().ok())
            .and_then(|mut plain| plain.flush().ok())
            .and_then(|()| fs::rename(&writing.temp, &writing.final_path).ok());
        if done.is_none() {
            let _ = fs::remove_file(&writing.temp);
        }
    }

    fn abandon(&mut self) {
        if let Some(mut writing) = self.writer.take() {
            drop(writing.records.take());
            if let Some(writer) = writing.writer.take() {
                let _ = writer.join();
            }
            let _ = fs::remove_file(&writing.temp);
        }
    }
}

impl<I: Iterator<Item = DirEntries>> Iterator for Recorder<I> {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        match self.inner.next() {
            Some(directory) => {
                self.record(&directory);
                Some(directory)
            }
            None => {
                self.finish();
                None
            }
        }
    }
}

impl<I> Drop for Recorder<I> {
    fn drop(&mut self) {
        if let Some(mut writing) = self.writer.take() {
            drop(writing.records.take());
            if let Some(writer) = writing.writer.take() {
                let _ = writer.join();
            }
            let _ = fs::remove_file(&writing.temp);
        }
    }
}

/// The file holds every name on the disk: readable by its owner alone.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

// --- reading -------------------------------------------------------------------------------

/// A saved scan read whole and checked: the footer's counts and checksum against the body.
pub struct Saved {
    bytes: Vec<u8>,
    body_end: usize,
    pub header: Header,
}

/// Why a saved scan was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// No file for this key.
    None,
    /// Not a saved scan, not this version, damaged, or made for another key.
    Invalid,
    /// The change log since it was made cannot be trusted: reset, wrapped, or missing.
    LogLost,
    /// The system was updated since; the sealed volume was rewritten without events.
    SystemChanged,
    /// The log names too much of it; a walk costs less.
    TooStale,
}

impl Saved {
    /// Read `dir/<key's file>`, checking it is a whole saved scan for `key`.
    pub fn open(dir: &Path, key: &Key) -> Result<Self, Unusable> {
        let path = dir.join(key.file_name());
        let packed = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Unusable::None),
            Err(_) => return Err(Unusable::Invalid),
        };
        Self::parse(&packed, key).ok_or(Unusable::Invalid)
    }

    /// The whole file: the magic and version in the clear, then one gzip member holding the
    /// body and the footer.
    fn parse(packed: &[u8], key: &Key) -> Option<Self> {
        let head = MAGIC.len() + 4;
        if packed.len() < head || &packed[..MAGIC.len()] != MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(packed[MAGIC.len()..head].try_into().ok()?);
        if version != VERSION {
            return None;
        }
        let mut bytes = Vec::with_capacity(packed.len() * 3);
        bytes.extend_from_slice(&packed[..head]);
        io::Read::read_to_end(&mut GzDecoder::new(&packed[head..]), &mut bytes).ok()?;
        if bytes.len() < head + 4 + FOOT.len() {
            return None;
        }
        let tail = bytes.len() - FOOT.len();
        if &bytes[tail..] != FOOT {
            return None;
        }
        let footer_len = u32::from_le_bytes(bytes[tail - 4..tail].try_into().ok()?) as usize;
        let body_end = (tail - 4).checked_sub(footer_len)?;
        if body_end < head {
            return None;
        }
        let (header, checksum) = decode_header(&bytes[body_end..tail - 4])?;
        if header.key != *key {
            return None;
        }
        let mut hash = Fnv::new();
        hash.update(&bytes[head..body_end]);
        if hash.finish() != checksum {
            return None;
        }
        Some(Self {
            bytes,
            body_end,
            header,
        })
    }

    /// The saved directory at `pos` (a byte offset, `0` for the first), decoded under
    /// `root`, and `pos` moved past it; `None` at the end, or at a damaged record (the
    /// checksum makes that a bug, not a broken file).
    fn directory_at(&self, pos: &mut usize, root: &Path) -> Option<(PathBuf, DirEntries)> {
        let head = MAGIC.len() + 4;
        let mut cursor = Cursor {
            bytes: &self.bytes[head..self.body_end],
            pos: *pos,
        };
        if cursor.done() {
            return None;
        }
        let record = cursor.bytes()?;
        *pos = cursor.pos;
        decode_directory(record, root)
    }

    /// Every saved directory in order, decoded under `root`.
    #[cfg(test)]
    fn directories<'a>(
        &'a self,
        root: &'a Path,
    ) -> impl Iterator<Item = (PathBuf, DirEntries)> + 'a {
        let mut pos = 0;
        std::iter::from_fn(move || self.directory_at(&mut pos, root))
    }
}

// --- what the log said ---------------------------------------------------------------------

/// The directories the volume's change log names since a saved scan's stamp, relative to the
/// scan root.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    /// To be listed again, one level.
    pub listed: Vec<PathBuf>,
    /// To be walked again whole (`MustScanSubDirs`): the log lost events under them.
    pub walked: Vec<PathBuf>,
    /// How many events the replay saw, for the note.
    pub events: u64,
}

// --- the cached scan -----------------------------------------------------------------------

/// Lists one directory afresh, as the walker would: `(path, depth)` to its entries, `depth`
/// counted from the scan root so the depth cap applies. `NotFound` means the directory is
/// gone, with everything under it.
pub type Lister<'a> = Box<dyn FnMut(&Path, usize) -> io::Result<DirEntries> + 'a>;
/// Walks a subtree afresh, as the walker would from that root at that depth.
pub type Walker<'a> =
    Box<dyn FnMut(&Path, usize) -> Box<dyn Iterator<Item = DirEntries> + 'a> + 'a>;

/// The saved stream replayed with the changes applied: a walker like any other.
pub struct CachedScan<'a> {
    saved: Saved,
    pos: usize,
    root: PathBuf,
    /// Fresh listings of the directories the log named, by relative path; taken as the saved
    /// stream reaches each.
    fresh: HashMap<PathBuf, DirEntries>,
    /// The subtrees to walk again whole, and the ones already walked or gone, whose saved
    /// directories are skipped.
    walk_whole: HashSet<PathBuf>,
    dropped: HashSet<PathBuf>,
    /// A hard-linked file's sizes and link count as listed afresh, by inode: patched into
    /// every saved directory that still names it.
    linked: HashMap<u64, (u64, u64, u64)>,
    walker: Walker<'a>,
    walking: Option<Box<dyn Iterator<Item = DirEntries> + 'a>>,
    pending: VecDeque<DirEntries>,
    note: Option<String>,
    relisted: u64,
}

impl<'a> CachedScan<'a> {
    /// Replay `saved` under `root` with `changes` applied: the named directories listed now
    /// with `list`, the subtrees walked now with `walk`. `note` goes on the root directory,
    /// for `--issues`.
    pub fn new(
        saved: Saved,
        root: &Path,
        changes: &Changes,
        mut list: Lister<'_>,
        walk: Walker<'a>,
        note: String,
    ) -> Self {
        let mut fresh = HashMap::new();
        let mut linked = HashMap::new();
        let mut dropped = HashSet::new();
        let mut relisted = 0;
        for relative in &changes.listed {
            let depth = relative.components().count();
            match list(&join_relative(root, relative), depth) {
                Ok(listing) => {
                    relisted += 1;
                    note_links(&mut linked, &listing);
                    fresh.insert(relative.clone(), listing);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    dropped.insert(relative.clone());
                }
                Err(_) => {
                    // Unreadable now: the walker would count that and go on; so does this,
                    // through the fresh listing the saved one is replaced by.
                    let mut failed =
                        DirEntries::new(Arc::from(join_relative(root, relative).as_path()));
                    failed.fail("open", None, "could not be listed again");
                    fresh.insert(relative.clone(), failed);
                }
            }
        }
        Self {
            saved,
            pos: 0,
            root: root.to_path_buf(),
            fresh,
            walk_whole: changes.walked.iter().cloned().collect(),
            dropped,
            linked,
            walker: walk,
            walking: None,
            pending: VecDeque::new(),
            note: Some(note),
            relisted,
        }
    }

    /// How many directories were listed again rather than read from the file.
    #[must_use]
    pub fn relisted(&self) -> u64 {
        self.relisted
    }

    /// What the file said about itself.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.saved.header
    }

    /// Whether `relative` or any folder above it is gone or walked afresh.
    fn skipped(&self, relative: &Path) -> bool {
        relative
            .ancestors()
            .any(|above| self.dropped.contains(above))
    }

    /// Take one saved directory forward: into `pending`, or a walk, or nothing.
    fn advance(&mut self) -> bool {
        let Some((relative, saved)) = self.saved.directory_at(&mut self.pos, &self.root) else {
            return false;
        };
        if self.skipped(&relative) {
            return true;
        }
        let depth = relative.components().count();
        if self.walk_whole.contains(&relative) {
            self.dropped.insert(relative.clone());
            let path = join_relative(&self.root, &relative);
            self.walking = Some((self.walker)(&path, depth));
            return true;
        }
        let (mut directory, new) = match self.fresh.remove(&relative) {
            Some(fresh) => {
                let new = self.reconcile(&relative, &saved, &fresh);
                (fresh, new)
            }
            None => (self.patch_links(saved), Vec::new()),
        };
        if let Some(note) = self.note.take() {
            directory.note("cache", None, note);
        }
        // The directory first, then the subtrees new under it: parent before child.
        self.pending.push_back(directory);
        for sub in new {
            let path = join_relative(&self.root, &sub);
            let walk = (self.walker)(&path, depth + 1);
            for directory in walk {
                note_links(&mut self.linked, &directory);
                self.pending.push_back(directory);
            }
        }
        true
    }

    /// A directory listed afresh against its saved listing: subfolders gone are dropped with
    /// what was under them; subfolders new are returned, to be walked.
    fn reconcile(
        &mut self,
        relative: &Path,
        saved: &DirEntries,
        fresh: &DirEntries,
    ) -> Vec<PathBuf> {
        let now: HashSet<&OsStr> = fresh
            .iter()
            .filter(|(_, meta)| meta.is_dir)
            .map(|(name, _)| name)
            .collect();
        let mut before: HashSet<&OsStr> = HashSet::new();
        for (name, _) in saved.iter().filter(|(_, meta)| meta.is_dir) {
            before.insert(name);
            if !now.contains(name) {
                self.dropped.insert(relative.join(name));
            }
        }
        let mut new: Vec<PathBuf> = now
            .into_iter()
            .filter(|name| !before.contains(name))
            .map(|name| relative.join(name))
            .collect();
        // A new subfolder that the log also says to walk whole is walked once, here.
        for sub in &new {
            self.walk_whole.remove(sub);
            self.dropped.insert(sub.clone());
        }
        new.sort();
        new
    }

    /// A saved directory with its hard-linked files as they were listed now: their sizes, and
    /// their link counts — a file linked into a changed folder since the save is listed there
    /// with two names, and its saved copy must say so too, or the ledger sees it once and the
    /// tree counts it twice.
    fn patch_links(&self, saved: DirEntries) -> DirEntries {
        if self.linked.is_empty()
            || !saved
                .iter()
                .any(|(_, meta)| !meta.is_dir && self.linked.contains_key(&meta.inode))
        {
            return saved;
        }
        let mut patched = DirEntries::with_capacity(Arc::clone(&saved.path), saved.len(), 0);
        patched.failed = saved.failed;
        for (name, meta) in saved.iter() {
            let mut meta = *meta;
            if !meta.is_dir
                && let Some(&(size, apparent, links)) = self.linked.get(&meta.inode)
            {
                meta.size = size;
                meta.apparent = apparent;
                meta.links = links;
            }
            patched.push(name, meta);
        }
        patched
    }
}

/// Remember every hard-linked file as listed now: its sizes and its link count.
fn note_links(linked: &mut HashMap<u64, (u64, u64, u64)>, listing: &DirEntries) {
    for (_, meta) in listing.iter() {
        if meta.links > 1 && !meta.is_dir {
            linked.insert(meta.inode, (meta.size, meta.apparent, meta.links));
        }
    }
}

impl Iterator for CachedScan<'_> {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        loop {
            if let Some(walking) = self.walking.as_mut() {
                match walking.next() {
                    Some(directory) => {
                        note_links(&mut self.linked, &directory);
                        return Some(directory);
                    }
                    None => self.walking = None,
                }
            }
            if let Some(directory) = self.pending.pop_front() {
                return Some(directory);
            }
            if !self.advance() {
                return None;
            }
        }
    }
}

// --- where the files live ------------------------------------------------------------------

/// The cache directory: `~/Library/Caches/duscape` on macOS, `$XDG_CACHE_HOME/duscape` or
/// `~/.cache/duscape` elsewhere; `DUSCAPE_CACHE_DIR` overrides (the tests use it).
#[must_use]
pub fn directory() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DUSCAPE_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches/duscape"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
            return Some(PathBuf::from(dir).join("duscape"));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/duscape"))
    }
}

/// Whether the log names too much of the saved scan for it to be worth bringing up to date.
#[must_use]
pub fn too_stale(changes: &Changes, header: &Header) -> bool {
    let named = (changes.listed.len() + changes.walked.len()) as f64;
    let root_whole = changes
        .walked
        .iter()
        .any(|path| path.as_os_str().is_empty());
    root_whole || named > (header.directories as f64 * RELIST_AT_MOST).max(64.0)
}

/// The note the root directory carries: where the tree came from.
#[must_use]
pub fn note(header: &Header, changes: &Changes, relisted: u64) -> String {
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs().saturating_sub(header.stamp.saved_at));
    format!(
        "read from the scan saved {} ago ({} folders); {relisted} listed again and {} walked again after {} events",
        age_words(age),
        header.directories,
        changes.walked.len(),
        changes.events,
    )
}

/// How long ago `saved_at` (seconds since the epoch) was, in words.
#[must_use]
pub fn age_words_ago(saved_at: u64) -> String {
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs().saturating_sub(saved_at));
    format!("{} ago", age_words(age))
}

fn age_words(seconds: u64) -> String {
    match seconds {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s if s < 172_800 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86400),
    }
}

#[cfg(test)]
mod tests;
