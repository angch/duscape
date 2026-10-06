//! Zip archives seen as folders: `.zip` and the formats built on it — `.jar`, `.war`, `.apk`,
//! `.whl`, `.nupkg`… — whose index (the central directory) is at the end of the file, so one
//! read near the end lists every entry with its sizes, and no entry has to be unpacked to be
//! counted.
//!
//! The walk notes archives by name as it builds the tree ([`is_archive_name`]); after the tree
//! is on screen an idle pass reads their indexes ([`read_index`]) and each archive becomes a
//! folder of its entries (`FileTree::expand_archive`). A preview of an entry unpacks it in
//! memory on the previewer's thread ([`read_member`]), within limits. Nothing in an archive is
//! a path on disk: [`member_of`] finds the archive a path is inside, for a delete to refuse, a
//! rescan to go to the folder holding it, and Open to open the archive.
//!
//! Read here rather than with the `zip` crate: listing is the central directory and nothing
//! else, and unpacking for a preview is stored or deflated data, which `flate2` reads.

use ::std::ffi::{OsStr, OsString};
use ::std::fs::File;
use ::std::io::{self, Read, Seek, SeekFrom};
use ::std::path::{Path, PathBuf};

/// The extensions of the archives shown as folders: zip and the package formats built on it.
/// Office documents and e-books are zips too, and left as the files people think of them as.
pub const EXTENSIONS: [&str; 17] = [
    "zip", "jar", "war", "ear", "aar", "apk", "apks", "xapk", "aab", "whl", "egg", "nupkg",
    "snupkg", "vsix", "xpi", "appx", "msix",
];

/// An index larger than this is not read: a central directory is about 80 bytes an entry, so
/// this is millions of entries, and reading one costs the memory it takes.
const MAX_DIRECTORY: u64 = 256 * 1024 * 1024;
/// At most this many entries of one archive are listed; the rest are left out of its folder.
pub const MAX_MEMBERS: usize = 1_000_000;

const END: u32 = 0x0605_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

/// Whether a file of this name is an archive shown as a folder: by its extension
/// ([`EXTENSIONS`]), any case. Asked of every file the walk finds, so on the name's bytes.
#[must_use]
pub fn is_archive_name(name: &[u8]) -> bool {
    let Some(dot) = name.iter().rposition(|&byte| byte == b'.') else {
        return false;
    };
    let extension = &name[dot + 1..];
    if extension.is_empty() || extension.len() > 6 || dot == 0 {
        return false;
    }
    EXTENSIONS
        .iter()
        .any(|known| known.as_bytes().eq_ignore_ascii_case(extension))
}

/// One entry of an archive: its path inside it, its sizes, and how to unpack it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its path inside the archive, a name a component: `["META-INF", "MANIFEST.MF"]`.
    pub path: Vec<String>,
    /// Bytes it takes in the archive, and unpacked.
    pub compressed: u64,
    pub uncompressed: u64,
    /// 0 stored, 8 deflated; other methods are listed but not unpacked.
    pub method: u16,
    pub encrypted: bool,
    pub is_dir: bool,
    /// Where its local header is, from the start of the file.
    header: u64,
}

impl Member {
    /// A member as an index would list it, for tests elsewhere.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(path: &[&str], compressed: u64, is_dir: bool) -> Self {
        Member {
            path: path.iter().map(|name| (*name).to_string()).collect(),
            compressed,
            uncompressed: compressed,
            method: 0,
            encrypted: false,
            is_dir,
            header: 0,
        }
    }

    /// How it is kept, for a preview's description: `stored`, `deflated`, or the method.
    #[must_use]
    pub fn how(&self) -> String {
        match self.method {
            0 => "stored".to_string(),
            8 => "deflated".to_string(),
            9 => "deflate64".to_string(),
            12 => "bzip2".to_string(),
            14 => "LZMA".to_string(),
            93 => "zstd".to_string(),
            95 => "xz".to_string(),
            other => format!("method {other}"),
        }
    }
}

/// An archive's entries, as its index lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub members: Vec<Member>,
    /// More than [`MAX_MEMBERS`]: only the first are listed.
    pub truncated: bool,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Where the central directory is and how many entries it holds, from the end of the file —
/// the end record, or Zip64's for an archive past 4 GiB or 65,535 entries — and how far the
/// offsets it gives are off: data before the archive (a self-extracting stub) shifts it all.
fn directory(file: &mut File, len: u64) -> io::Result<(u64, u64, u64, u64)> {
    const END_SIZE: u64 = 22;
    if len < END_SIZE {
        return Err(invalid("too short for a zip"));
    }
    // The end record is the last thing in the file, but for a comment of up to 65,535 bytes.
    let tail_len = len.min(END_SIZE + 65_535);
    let tail_start = len - tail_len;
    let tail = read_at(file, tail_start, tail_len as usize)?;
    let at = (0..=tail.len() - END_SIZE as usize)
        .rev()
        .find(|&at| {
            u32_at(&tail, at) == Some(END)
                && u16_at(&tail, at + 20)
                    .is_some_and(|comment| at + END_SIZE as usize + comment as usize <= tail.len())
        })
        .ok_or_else(|| invalid("no zip index at its end"))?;
    let end_offset = tail_start + at as u64;
    let mut entries = u64::from(u16_at(&tail, at + 10).unwrap_or(0));
    let mut size = u64::from(u32_at(&tail, at + 12).unwrap_or(0));
    let mut offset = u64::from(u32_at(&tail, at + 16).unwrap_or(0));
    let mut directory_end = end_offset;
    if (entries == 0xFFFF || size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF)
        && at >= 20
        && u32_at(&tail, at - 20) == Some(ZIP64_LOCATOR)
    {
        let record_offset = u64_at(&tail, at - 20 + 8).unwrap_or(u64::MAX);
        if record_offset < end_offset {
            let record = read_at(file, record_offset, 56)?;
            if u32_at(&record, 0) == Some(ZIP64_END) {
                entries = u64_at(&record, 32).unwrap_or(entries);
                size = u64_at(&record, 40).unwrap_or(size);
                offset = u64_at(&record, 48).unwrap_or(offset);
                directory_end = record_offset;
            }
        }
    }
    // The directory ends where the end records begin; where it says it starts plus its size
    // falls short of that, everything was shifted by what came before the archive.
    let shift = directory_end.saturating_sub(offset.saturating_add(size));
    if offset.saturating_add(shift).saturating_add(size) > len {
        return Err(invalid("zip index past the end of the file"));
    }
    if size > MAX_DIRECTORY {
        return Err(invalid("zip index too large to read"));
    }
    Ok((offset + shift, size, entries, shift))
}

/// Read the index of the archive at `path`: every entry, with its sizes.
pub fn read_index(path: &Path) -> io::Result<Index> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let (offset, size, entries, shift) = directory(&mut file, len)?;
    let bytes = read_at(&mut file, offset, size as usize)?;
    let mut index = Index::default();
    let mut at = 0usize;
    let mut seen = 0u64;
    while seen < entries && u32_at(&bytes, at) == Some(CENTRAL) {
        if index.members.len() >= MAX_MEMBERS {
            index.truncated = true;
            break;
        }
        let field = |offset: usize| u16_at(&bytes, at + offset).unwrap_or(0);
        let flags = field(8);
        let method = field(10);
        let mut compressed = u64::from(u32_at(&bytes, at + 20).unwrap_or(0));
        let mut uncompressed = u64::from(u32_at(&bytes, at + 24).unwrap_or(0));
        let (name_len, extra_len, comment_len) =
            (field(28) as usize, field(30) as usize, field(32) as usize);
        let mut header = u64::from(u32_at(&bytes, at + 42).unwrap_or(0));
        let name_at = at + 46;
        let Some(name) = bytes.get(name_at..name_at + name_len) else {
            break;
        };
        let extra = bytes
            .get(name_at + name_len..name_at + name_len + extra_len)
            .unwrap_or(&[]);
        zip64_sizes(extra, &mut uncompressed, &mut compressed, &mut header);
        let text = name_text(name, flags & 0x0800 != 0);
        let is_dir = text.ends_with('/') || text.ends_with('\\');
        if let Some(path) = member_path(&text) {
            index.members.push(Member {
                path,
                compressed,
                uncompressed,
                method,
                encrypted: flags & 1 != 0,
                is_dir,
                header: header.saturating_add(shift),
            });
        }
        at = name_at + name_len + extra_len + comment_len;
        seen += 1;
    }
    Ok(index)
}

/// Zip64's extra field (id 1): the full sizes and offset, those of them that did not fit, in
/// that order.
fn zip64_sizes(extra: &[u8], uncompressed: &mut u64, compressed: &mut u64, header: &mut u64) {
    let mut at = 0;
    while let (Some(id), Some(len)) = (u16_at(extra, at), u16_at(extra, at + 2)) {
        let data = extra.get(at + 4..at + 4 + len as usize).unwrap_or(&[]);
        if id == 1 {
            let mut field = 0;
            for value in [uncompressed, compressed, header] {
                if *value == 0xFFFF_FFFF
                    && let Some(full) = u64_at(data, field)
                {
                    *value = full;
                    field += 8;
                }
            }
            return;
        }
        at += 4 + len as usize;
    }
}

/// An entry's name as the names of the folders it is in and its own, or `None` for a name that
/// has no place inside the archive's folder: one going up (`..`), or with a part that is no plain
/// name here — `C:` on Windows, which a path joined onto the archive's takes for the drive, so a
/// delete, a rescan or a preview of the entry would reach the drive. Such an entry is left out.
fn member_path(text: &str) -> Option<Vec<String>> {
    let mut path = Vec::new();
    for part in text.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            part => {
                let mut components = Path::new(part).components();
                match (components.next(), components.next()) {
                    (Some(::std::path::Component::Normal(_)), None) => path.push(part.to_string()),
                    _ => return None,
                }
            }
        }
    }
    (!path.is_empty()).then_some(path)
}

/// An entry's name as text: UTF-8 where the archive says so (or the bytes are), else the
/// IBM PC's code page 437, which zip names were in before.
fn name_text(name: &[u8], utf8: bool) -> String {
    if utf8 || ::std::str::from_utf8(name).is_ok() {
        return String::from_utf8_lossy(name).into_owned();
    }
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    let high: Vec<char> = HIGH.chars().collect();
    name.iter()
        .map(|&byte| {
            if byte < 0x80 {
                byte as char
            } else {
                high.get(usize::from(byte - 0x80)).copied().unwrap_or('?')
            }
        })
        .collect()
}

/// The archive `path` is inside, and its path in there, if it is in one: the nearest file above
/// it on disk that is named as an archive ([`is_archive_name`]). `None` for a path that is on
/// disk itself, or under a folder, or under a file that is no archive. Nothing is asked of the
/// disk unless a folder above the path is named as an archive — a delete's check runs on the
/// window's thread over every marked entry — and then a stat or two.
#[must_use]
pub fn member_of(path: &Path) -> Option<(PathBuf, Vec<OsString>)> {
    let named_above = path
        .parent()?
        .components()
        .any(|part| is_archive_name(part.as_os_str().as_encoded_bytes()));
    if !named_above || ::std::fs::symlink_metadata(path).is_ok() {
        return None;
    }
    let mut inner: Vec<OsString> = vec![path.file_name()?.to_os_string()];
    let mut above = path.parent()?;
    loop {
        match ::std::fs::symlink_metadata(above) {
            Ok(meta) if meta.is_file() => {
                let named = above
                    .file_name()
                    .is_some_and(|name| is_archive_name(name.as_encoded_bytes()));
                inner.reverse();
                return named.then(|| (above.to_path_buf(), inner));
            }
            Ok(_) => return None,
            Err(_) => {
                inner.push(above.file_name()?.to_os_string());
                above = above.parent()?;
            }
        }
    }
}

/// The archive `path` is, or is inside: what is on disk for it.
#[must_use]
pub fn archive_of(path: &Path) -> Option<PathBuf> {
    if let Ok(meta) = ::std::fs::symlink_metadata(path) {
        let named = path
            .file_name()
            .is_some_and(|name| is_archive_name(name.as_encoded_bytes()));
        return (meta.is_file() && named).then(|| path.to_path_buf());
    }
    member_of(path).map(|(archive, _)| archive)
}

/// What on disk stands for `path`: the archive it is inside, or itself — for Open and Show in
/// the file manager, which have no way into an archive.
#[must_use]
pub fn on_disk(path: &Path) -> PathBuf {
    member_of(path).map_or_else(|| path.to_path_buf(), |(archive, _)| archive)
}

/// The start of an entry, up to `limit` bytes unpacked, and the entry: for a preview, on the
/// previewer's thread. Stored and deflated entries unpack; an encrypted one, or one packed
/// another way, is an error saying so.
pub fn read_member(
    archive: &Path,
    inner: &[OsString],
    limit: u64,
) -> io::Result<(Vec<u8>, Member)> {
    let index = read_index(archive)?;
    let wanted: Vec<String> = inner
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let member = index
        .members
        .into_iter()
        .find(|member| !member.is_dir && member.path == wanted)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not in the archive"))?;
    if member.encrypted {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "encrypted"));
    }
    let mut file = File::open(archive)?;
    let local = read_at(&mut file, member.header, 30)?;
    if u32_at(&local, 0) != Some(LOCAL) {
        return Err(invalid("no entry where the index says"));
    }
    let skip = 30
        + u64::from(u16_at(&local, 26).unwrap_or(0))
        + u64::from(u16_at(&local, 28).unwrap_or(0));
    file.seek(SeekFrom::Start(member.header + skip))?;
    let packed = (&mut file).take(member.compressed);
    let mut bytes = Vec::new();
    match member.method {
        0 => packed.take(limit).read_to_end(&mut bytes)?,
        8 => flate2::read::DeflateDecoder::new(packed)
            .take(limit)
            .read_to_end(&mut bytes)?,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("packed with {}", member.how()),
            ));
        }
    };
    Ok((bytes, member))
}

/// A zip of `entries` (name, bytes), stored, for tests elsewhere: what a test of the scan, the
/// preview or a delete makes an archive of.
#[doc(hidden)]
#[must_use]
pub fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let header = out.len() as u32;
        let sizes = (data.len() as u32).to_le_bytes();
        let name_len = (name.len() as u16).to_le_bytes();
        out.extend_from_slice(&LOCAL.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0x08, 0, 0]);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&sizes);
        out.extend_from_slice(&sizes);
        out.extend_from_slice(&name_len);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&CENTRAL.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0x08, 0, 0]);
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&sizes);
        central.extend_from_slice(&sizes);
        central.extend_from_slice(&name_len);
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&header.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&END.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// The name of the archive file at `path`, for descriptions.
#[must_use]
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(OsStr::new(""))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
