//! The saved scan: every folder's listing written out as the walk ends, and read back at the
//! next start with only the folders the volume's change log names since then listed again.
//!
//! Where the metadata cannot be read off the device — APFS through a FileVault, which is every
//! Mac — a first scan costs the kernel's floor (`docs/scan-performance.md`, "macOS: what is
//! left"). Every scan after it need not: the volume keeps a log of which directories changed
//! (FSEvents on macOS), so the saved stream of [`DirEntries`] can be
//! replayed with the changed directories read afresh, the directories gone dropped with what
//! was under them, the directories new walked, and the ledger rebuilt by the tree as on any
//! scan. The save is a tee on whatever walker ran ([`Recorder`]); the replay is a walker of its
//! own ([`CachedScan`]), behind the same seam as the others, so every viewer gets it.
//!
//! The file: the magic, the version, the key it was made for and its stamp in the clear, so a
//! start can decide on the header alone; then one gzip member (`flate2`, the deflate a zip
//! reader would share — names deflate to a third) holding the stream in order, parent before
//! child as the walkers send it, one tagged, length-prefixed record a directory with its
//! entries as LEB128 varints, and last a footer record with the counts and a checksum.
//!
//! Two ways to read it. [`SavedStream`] inflates as it goes and yields the first directory
//! in milliseconds: what a viewer's first scan shows (`Cache::Saved`), current as of the
//! stamp, a catch-up owed. [`Saved::open`] reads the whole and checks the footer before a
//! byte is trusted: what the catch-up starts from (`Cache::CatchUp`, [`CachedScan`]), which
//! lists the directories the log names again first — on a pool, since each is a cold read —
//! so a hard-linked file's fresh size and link count patch every saved copy, the ledger
//! identifying a file by its disk size; then the stream with the changes applied.
//!
//! What is not carried: `later` and `extent_space` (the Linux second pass's; no macOS walker
//! fills them) and a directory's issues (the failure count is). What is deliberately not
//! handled yet, and said so in the roadmap: a new subdirectory that is a mount point is walked
//! as a root, so `-x` does not apply to it; a whole-tree rescan (`R`) does not refresh the file,
//! only the catch-up does.
#![cfg_attr(
    target_os = "macos",
    doc = "\n\nThe log is read by [`crate::fsevents`]."
)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use libduscape::scan::{DirEntries, EntryMeta, ScanOptions, Unlisted};

const MAGIC: &[u8; 12] = b"DUSCAPE-SCAN";
/// Bumped when what a recorded stream means changes, so a file made before is walked afresh
/// rather than shown: 3 when the macOS walker began keying APFS clones (`shared_extent`) and
/// leaving a disk image's volume out where the scan counts its image — a version-2 file has
/// neither, and a catch-up re-lists only the folders that changed, so it would have kept both
/// overcounts until each folder did.
const VERSION: u32 = 3;
/// A record's first byte: a directory, or the footer that ends the stream.
const DIRECTORY: u8 = 0;
const FOOTER: u8 = 1;

/// The note a saved scan's root carries (`Issues` kind), by which a tree knows a catch-up is
/// owed: the stream was read as it was.
pub const NOTE_OWED: &str = "saved scan";
/// The note of a scan brought up to date: the file was the start, the log the rest.
pub const NOTE_CAUGHT_UP: &str = "saved scan, brought up to date";

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

/// Files smaller than this, with one name and no shared blocks, are not saved one by one: a
/// folder's are summed as its [`Unlisted`] and put back by a fill pass. Folders, hard-linked
/// files and shared-extent files are always kept, since the tree cannot be right without
/// them. On an 11M-entry disk this keeps about one entry in fifty and the file under 30 MB.
pub const KEEP_FROM: u64 = 1 << 20;

/// Whether a saved scan lists `meta` one by one.
fn kept(meta: &EntryMeta) -> bool {
    meta.is_dir || meta.links != 1 || meta.shared_extent != 0 || meta.size >= KEEP_FROM
}

/// Encode one directory: its parent's record index (plus one; zero for the root, whose name
/// is then its whole path from the root, normally empty), its name, its failure count, the
/// sum of what is not kept, and the entries kept.
fn encode_directory(out: &mut Vec<u8>, parent: Option<u64>, name: &[u8], directory: &DirEntries) {
    let mut record = Vec::with_capacity(32 + directory.len() * 24);
    put_varint(&mut record, parent.map_or(0, |index| index + 1));
    put_bytes(&mut record, name);
    put_varint(&mut record, directory.failed);
    let mut unlisted = directory.unlisted;
    let kept_count = directory
        .iter()
        .filter(|(_, meta)| {
            let keep = kept(meta);
            if !keep {
                unlisted.add(meta);
            }
            keep
        })
        .count();
    put_varint(&mut record, unlisted.size);
    put_varint(&mut record, unlisted.apparent);
    put_varint(&mut record, unlisted.count);
    put_varint(&mut record, kept_count as u64);
    for (name, meta) in directory.iter().filter(|(_, meta)| kept(meta)) {
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
    out.push(DIRECTORY);
    put_bytes(out, &record);
}

/// Decode one directory's record into a listing under `root`, its path made from its parent's
/// in `paths` (the records read before it, in order) and added there for its children.
fn decode_directory(
    record: &[u8],
    root: &Path,
    paths: &mut Vec<Arc<Path>>,
) -> Option<(Option<usize>, DirEntries)> {
    let mut cursor = Cursor {
        bytes: record,
        pos: 0,
    };
    let parent = cursor.varint()?;
    let name = os_str(cursor.bytes()?);
    let (parent, path): (Option<usize>, Arc<Path>) = if parent == 0 {
        (
            None,
            Arc::from(join_relative(root, Path::new(name)).as_path()),
        )
    } else {
        let index = usize::try_from(parent - 1).ok()?;
        (
            Some(index),
            Arc::from(paths.get(index)?.join(name).as_path()),
        )
    };
    paths.push(Arc::clone(&path));
    let failed = cursor.varint()?;
    let unlisted = Unlisted {
        size: cursor.varint()?,
        apparent: cursor.varint()?,
        count: cursor.varint()?,
    };
    let count = usize::try_from(cursor.varint()?).ok()?;
    let mut directory = DirEntries::with_capacity(path, count.min(1 << 20), 0);
    directory.failed = failed;
    directory.unlisted = unlisted;
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
    cursor.done().then_some((parent, directory))
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

/// The clear part: the magic, the version, the key and the stamp, length-prefixed as one.
fn encode_clear(key: &Key, stamp: &Stamp) -> Vec<u8> {
    let mut fields = Vec::new();
    put_bytes(&mut fields, key.root.as_os_str().as_encoded_bytes());
    put_varint(&mut fields, key.max_depth.map_or(u64::MAX, |d| d as u64));
    fields.push(u8::from(key.one_file_system));
    fields.push(u8::from(key.snapshots));
    put_varint(&mut fields, stamp.event_id);
    put_varint(&mut fields, stamp.device);
    put_bytes(&mut fields, stamp.log_uuid.as_bytes());
    put_bytes(&mut fields, stamp.system.as_bytes());
    put_varint(&mut fields, stamp.saved_at);
    let mut out = Vec::with_capacity(MAGIC.len() + 8 + fields.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    put_bytes(&mut out, &fields);
    out
}

/// The key and stamp from the front of a file, and where the gzip member begins.
fn decode_clear(bytes: &[u8]) -> Option<(Key, Stamp, usize)> {
    let head = MAGIC.len() + 4;
    if bytes.len() < head || &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(bytes[MAGIC.len()..head].try_into().ok()?) != VERSION {
        return None;
    }
    let mut outer = Cursor { bytes, pos: head };
    let fields = outer.bytes()?;
    let mut cursor = Cursor {
        bytes: fields,
        pos: 0,
    };
    let root = PathBuf::from(os_str(cursor.bytes()?));
    let max_depth = match cursor.varint()? {
        u64::MAX => None,
        depth => Some(usize::try_from(depth).ok()?),
    };
    let one_file_system = *fields.get(cursor.pos)? != 0;
    let snapshots = *fields.get(cursor.pos + 1)? != 0;
    cursor.pos += 2;
    let event_id = cursor.varint()?;
    let device = cursor.varint()?;
    let log_uuid = cursor.string()?;
    let system = cursor.string()?;
    let saved_at = cursor.varint()?;
    Some((
        Key {
            root,
            max_depth,
            one_file_system,
            snapshots,
        },
        Stamp {
            event_id,
            device,
            log_uuid,
            system,
            saved_at,
        },
        outer.pos,
    ))
}

/// The footer record: the counts and the checksum over every directory record before it.
fn encode_footer(directories: u64, entries: u64, checksum: u64) -> Vec<u8> {
    let mut fields = Vec::new();
    put_varint(&mut fields, directories);
    put_varint(&mut fields, entries);
    fields.extend_from_slice(&checksum.to_le_bytes());
    let mut out = vec![FOOTER];
    put_bytes(&mut out, &fields);
    out
}

fn decode_footer(fields: &[u8]) -> Option<(u64, u64, u64)> {
    let mut cursor = Cursor {
        bytes: fields,
        pos: 0,
    };
    let directories = cursor.varint()?;
    let entries = cursor.varint()?;
    let checksum = u64::from_le_bytes(fields.get(cursor.pos..cursor.pos + 8)?.try_into().ok()?);
    Some((directories, entries, checksum))
}

/// The stamp of the saved scan for `key` in `dir`, from its clear header alone: enough to
/// decide whether to start from it, before a byte of the body is read.
#[must_use]
pub fn peek(dir: &Path, key: &Key) -> Option<Stamp> {
    let mut file = File::open(dir.join(key.file_name())).ok()?;
    let mut head = vec![0u8; 4096];
    let read = io::Read::read(&mut file, &mut head).ok()?;
    head.truncate(read);
    let (saved_key, stamp, _) = decode_clear(&head)?;
    (saved_key == *key).then_some(stamp)
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
    /// Encoded records go to the writer thread, which deflates and writes them, and on the
    /// footer finishes the file and renames it into place by itself: the scan is the walk's
    /// time and no more, one record's encoding is all it pays here, and its end does not
    /// wait for the deflate to catch up. The channel closing without a footer — a stopped
    /// scan — makes the thread remove the part file instead.
    records: Option<std::sync::mpsc::SyncSender<Message>>,
    root: PathBuf,
    header: Header,
    checksum: Fnv,
    /// Each directory recorded, by path, to its record's index: what its children name as
    /// their parent. The paths are the stream's own `Arc`s, so this costs no copies.
    indices: HashMap<Arc<Path>, u64>,
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
        plain.write_all(&encode_clear(&key, &stamp)).ok()?;
        // The default level: a sixth smaller than the fastest on a whole disk (31 MB against
        // 37), and the deflate runs on its own thread, so the walk does not pay for it.
        let mut file = GzEncoder::new(plain, Compression::default());
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Message>(1024);
        let recording = Recording::start();
        let cleanup = temp.clone();
        let (sweep_dir, sweep_stamp, sweep_keep) =
            (dir.to_path_buf(), stamp.clone(), key.file_name());
        std::thread::Builder::new()
            .name("scan_recorder".to_string())
            .spawn(move || {
                let _recording = recording;
                // First, while the walk runs and its records queue: a process that ends with
                // its scan (`--benchmark`) is gone before the sweep after the save gets to run.
                sweep(&sweep_dir, &sweep_stamp, &sweep_keep, ROOM);
                let mut finished = false;
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Record(record) => {
                            if file.write_all(&record).is_err() {
                                break;
                            }
                        }
                        Message::Footer(footer) => {
                            finished = file
                                .write_all(&footer)
                                .and_then(|()| file.finish())
                                .and_then(|mut plain| plain.flush())
                                .and_then(|()| fs::rename(&temp, &final_path))
                                .is_ok();
                            break;
                        }
                    }
                }
                if finished {
                    // Again after the save, the new file's size counted in the room.
                    sweep(&sweep_dir, &sweep_stamp, &sweep_keep, ROOM);
                } else {
                    let _ = fs::remove_file(&cleanup);
                }
            })
            .ok()?;
        let root = key.root.clone();
        Some(Writing {
            records: Some(sender),
            root,
            header: Header {
                key,
                stamp,
                directories: 0,
                entries: 0,
            },
            checksum: Fnv::new(),
            indices: HashMap::new(),
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
        let Ok(relative) = directory.path.strip_prefix(&writing.root) else {
            // A directory from outside the root has no place in the tree either.
            return;
        };
        // Under its parent's record where that came first, as the walkers send them; else by
        // its whole path from the root.
        let (parent, name) = match directory
            .path
            .parent()
            .filter(|_| !relative.as_os_str().is_empty())
            .and_then(|parent| writing.indices.get(parent))
        {
            Some(&index) => (
                Some(index),
                directory
                    .path
                    .file_name()
                    .map_or(&[][..], |n| n.as_encoded_bytes()),
            ),
            None => (None, relative.as_os_str().as_encoded_bytes()),
        };
        let mut record = Vec::with_capacity(32 + directory.len() * 24);
        encode_directory(&mut record, parent, name, directory);
        writing
            .indices
            .insert(Arc::clone(&directory.path), writing.header.directories);
        writing.checksum.update(&record[1..]);
        writing.header.directories += 1;
        writing.header.entries += directory.iter().filter(|(_, meta)| kept(meta)).count() as u64;
        let sent = writing
            .records
            .as_ref()
            .is_some_and(|records| records.send(Message::Record(record)).is_ok());
        if !sent {
            self.abandon();
        }
    }

    fn finish(&mut self) {
        let Some(mut writing) = self.writer.take() else {
            return;
        };
        let footer = encode_footer(
            writing.header.directories,
            writing.header.entries,
            writing.checksum.finish(),
        );
        if let Some(records) = writing.records.take() {
            let _ = records.send(Message::Footer(footer));
        }
    }

    /// Stop recording; the writer thread removes the part file when the channel closes.
    fn abandon(&mut self) {
        self.writer = None;
    }
}

/// Recordings whose writer thread has not ended, and the signal that one has.
static RECORDINGS: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// One recording counted in [`RECORDINGS`] while its writer thread runs, however it ends.
struct Recording;

impl Recording {
    fn start() -> Self {
        if let Ok(mut count) = RECORDINGS.0.lock() {
            *count += 1;
        }
        Recording
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if let Ok(mut count) = RECORDINGS.0.lock() {
            *count = count.saturating_sub(1);
        }
        RECORDINGS.1.notify_all();
    }
}

/// Wait, at most `limit`, for the saves under way to be written and swept: what a process that
/// ends with its scan calls before it does (the terminal viewer, `--benchmark`). The writer is
/// a thread of its own so that the scan does not wait for the deflate; a process gone before
/// it ends left a part file and no save — every `--bench-stage cached` of a small tree did.
/// A window, which outlives its scans, need not call it.
pub fn wait_for_saves(limit: std::time::Duration) {
    let deadline = std::time::Instant::now() + limit;
    let Ok(mut count) = RECORDINGS.0.lock() else {
        return;
    };
    while *count > 0 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match RECORDINGS.1.wait_timeout(count, left) {
            Ok((next, _)) => count = next,
            Err(_) => return,
        }
    }
}

/// What the recorder sends its writer thread.
enum Message {
    Record(Vec<u8>),
    Footer(Vec<u8>),
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
        // Dropped before the stream ended — a stopped scan — the channel closes without a
        // footer and the writer thread removes the part file.
        self.writer = None;
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
    /// The inflated records, the footer's included.
    bytes: Vec<u8>,
    pub header: Header,
}

/// Why a saved scan was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// No file for this key.
    None,
    /// Not a saved scan, not this version, damaged, or made for another key.
    Invalid,
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

    /// The whole file: the clear header, then the gzip member holding the records and the
    /// footer, checked against it.
    fn parse(packed: &[u8], key: &Key) -> Option<Self> {
        let (saved_key, stamp, body) = decode_clear(packed)?;
        if saved_key != *key {
            return None;
        }
        let mut bytes = Vec::with_capacity(packed.len() * 3);
        io::Read::read_to_end(&mut GzDecoder::new(&packed[body..]), &mut bytes).ok()?;
        // The footer is the last record; every directory record before it is checksummed.
        let mut pos = 0usize;
        let mut hash = Fnv::new();
        let mut counted = 0u64;
        let footer = loop {
            let tag = *bytes.get(pos)?;
            let mut cursor = Cursor {
                bytes: &bytes,
                pos: pos + 1,
            };
            let record = cursor.bytes()?;
            let start = pos + 1;
            pos = cursor.pos;
            match tag {
                DIRECTORY => {
                    hash.update(&bytes[start..pos]);
                    counted += 1;
                }
                FOOTER => break decode_footer(record)?,
                _ => return None,
            }
        };
        let (directories, entries, checksum) = footer;
        if counted != directories || hash.finish() != checksum || pos != bytes.len() {
            return None;
        }
        Some(Self {
            bytes,
            header: Header {
                key: saved_key,
                stamp,
                directories,
                entries,
            },
        })
    }

    /// The saved directory at `pos` (a byte offset, `0` for the first), decoded under
    /// `root`, and `pos` moved past it; `None` at the footer, or at a damaged record (the
    /// checksum makes that a bug, not a broken file).
    fn directory_at(
        &self,
        pos: &mut usize,
        root: &Path,
        paths: &mut Vec<Arc<Path>>,
    ) -> Option<(Option<usize>, DirEntries)> {
        if *self.bytes.get(*pos)? != DIRECTORY {
            return None;
        }
        let mut cursor = Cursor {
            bytes: &self.bytes,
            pos: *pos + 1,
        };
        let record = cursor.bytes()?;
        *pos = cursor.pos;
        decode_directory(record, root, paths)
    }

    /// Every saved directory in order, decoded under `root`.
    #[cfg(test)]
    fn directories<'a>(&'a self, root: &'a Path) -> impl Iterator<Item = DirEntries> + 'a {
        let mut pos = 0;
        let mut paths = Vec::new();
        std::iter::from_fn(move || {
            self.directory_at(&mut pos, root, &mut paths)
                .map(|(_, directory)| directory)
        })
    }
}

/// A saved scan read as it is, inflating as it goes: the first directory comes in the time
/// it takes to read one record. Not checked until the end — the gzip member's own check ends
/// a damaged file early — so it is for what is shown first and replaced by a catch-up.
pub struct SavedStream {
    records: Records<GzDecoder<io::BufReader<File>>>,
    root: PathBuf,
    paths: Vec<Arc<Path>>,
    pub stamp: Stamp,
    note: Option<String>,
}

impl SavedStream {
    /// Open `dir/<key's file>` for streaming, when its clear header is for `key`. `note` goes
    /// on the root directory, kind [`NOTE_OWED`].
    pub fn open(dir: &Path, key: &Key, note: String) -> Result<Self, Unusable> {
        let path = dir.join(key.file_name());
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Unusable::None),
            Err(_) => return Err(Unusable::Invalid),
        };
        let mut head = vec![0u8; 4096];
        let read = io::Read::read(&mut file, &mut head).map_err(|_| Unusable::Invalid)?;
        head.truncate(read);
        let (saved_key, stamp, body) = decode_clear(&head).ok_or(Unusable::Invalid)?;
        if saved_key != *key {
            return Err(Unusable::Invalid);
        }
        io::Seek::seek(&mut file, io::SeekFrom::Start(body as u64))
            .map_err(|_| Unusable::Invalid)?;
        let reader = GzDecoder::new(io::BufReader::with_capacity(1 << 20, file));
        Ok(Self {
            records: Records { reader },
            root: key.root.clone(),
            paths: Vec::new(),
            stamp,
            note: Some(note),
        })
    }
}

impl Iterator for SavedStream {
    type Item = DirEntries;
    fn next(&mut self) -> Option<DirEntries> {
        let (tag, record) = self.records.next()?;
        if tag != DIRECTORY {
            return None;
        }
        let (_, mut directory) = decode_directory(&record, &self.root, &mut self.paths)?;
        if let Some(note) = self.note.take() {
            directory.note(NOTE_OWED, None, note);
        }
        Some(directory)
    }
}

/// Tagged, length-prefixed records off a reader.
struct Records<R> {
    reader: R,
}

impl<R: io::Read> Records<R> {
    /// The next record's tag and body; `None` at the end or at anything unreadable.
    fn next(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut tag = [0u8; 1];
        self.reader.read_exact(&mut tag).ok()?;
        let mut len = 0u64;
        let mut shift = 0u32;
        loop {
            let mut byte = [0u8; 1];
            self.reader.read_exact(&mut byte).ok()?;
            if shift >= 64 {
                return None;
            }
            len |= u64::from(byte[0] & 0x7f) << shift;
            if byte[0] & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let len = usize::try_from(len).ok()?;
        if len > 1 << 30 {
            return None;
        }
        let mut record = vec![0u8; len];
        self.reader.read_exact(&mut record).ok()?;
        Some((tag[0], record))
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
/// gone, with everything under it. Called from several threads at once.
pub type Lister<'a> = &'a (dyn Fn(&Path, usize) -> io::Result<DirEntries> + Sync);
/// Walks a subtree afresh, as the walker would from that root at that depth.
pub type Walker<'a> =
    Box<dyn FnMut(&Path, usize) -> Box<dyn Iterator<Item = DirEntries> + 'a> + 'a>;

/// The saved stream replayed with the changes applied: a walker like any other.
pub struct CachedScan<'a> {
    saved: Saved,
    pos: usize,
    paths: Vec<Arc<Path>>,
    /// Whether each saved record so far is under a folder gone or walked afresh: a record's
    /// flag is its parent's, or its own if the log named it — one lookup a directory, where
    /// a hash lookup per ancestor cost more than the catch-up's reads on a whole disk.
    skip: Vec<bool>,
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
    /// with `list` on `threads` threads (each is a cold read; one at a time, a day's changes
    /// on a home folder took five seconds), the subtrees walked now with `walk`. `note` goes
    /// on the root directory, for `--issues`.
    pub fn new(
        saved: Saved,
        root: &Path,
        changes: &Changes,
        list: Lister<'_>,
        threads: usize,
        walk: Walker<'a>,
        note: String,
    ) -> Self {
        let mut fresh = HashMap::new();
        let mut linked = HashMap::new();
        let mut dropped = HashSet::new();
        let mut relisted = 0;
        for (relative, listed) in relist_all(root, &changes.listed, list, threads) {
            match listed {
                Ok(listing) => {
                    relisted += 1;
                    note_links(&mut linked, &listing);
                    fresh.insert(relative, listing);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    dropped.insert(relative);
                }
                Err(_) => {
                    // Unreadable now: the walker would count that and go on; so does this,
                    // through the fresh listing the saved one is replaced by.
                    let mut failed =
                        DirEntries::new(Arc::from(join_relative(root, &relative).as_path()));
                    failed.fail("open", None, "could not be listed again");
                    fresh.insert(relative, failed);
                }
            }
        }
        Self {
            saved,
            pos: 0,
            paths: Vec::new(),
            skip: Vec::new(),
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

    /// Take one saved directory forward: into `pending`, or a walk, or nothing.
    fn advance(&mut self) -> bool {
        let Some((parent, saved)) =
            self.saved
                .directory_at(&mut self.pos, &self.root, &mut self.paths)
        else {
            return false;
        };
        let under_skipped =
            parent.is_some_and(|index| self.skip.get(index).copied().unwrap_or(false));
        // With nothing named, nothing is looked up: the stream goes through as it is.
        let untouched =
            self.dropped.is_empty() && self.walk_whole.is_empty() && self.fresh.is_empty();
        if under_skipped || (untouched && self.note.is_none()) {
            self.skip.push(under_skipped);
            if !under_skipped {
                self.pending.push_back(saved);
            }
            return true;
        }
        let relative = saved
            .path
            .strip_prefix(&self.root)
            .map_or_else(|_| PathBuf::new(), Path::to_path_buf);
        if self.dropped.contains(&relative) {
            self.skip.push(true);
            return true;
        }
        self.skip.push(false);
        let depth = relative.components().count();
        if self.walk_whole.contains(&relative) {
            if let Some(flag) = self.skip.last_mut() {
                *flag = true;
            }
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
            directory.note(NOTE_CAUGHT_UP, None, note);
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
        patched.unlisted = saved.unlisted;
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

/// List every one of `relatives` under `root` with `list`, on `threads` threads: the results
/// in no particular order, each with the path it was asked for.
fn relist_all(
    root: &Path,
    relatives: &[PathBuf],
    list: Lister<'_>,
    threads: usize,
) -> Vec<(PathBuf, io::Result<DirEntries>)> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::with_capacity(relatives.len()));
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, 64).min(relatives.len().max(1)) {
            scope.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(relative) = relatives.get(index) else {
                        break;
                    };
                    let depth = relative.components().count();
                    mine.push((
                        relative.clone(),
                        list(&join_relative(root, relative), depth),
                    ));
                }
                results.lock().expect("no panic").extend(mine);
            });
        }
    });
    results.into_inner().expect("no panic")
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

mod sweep;
pub use sweep::{ROOM, Removed, clear, sweep};

#[cfg(test)]
mod tests;
