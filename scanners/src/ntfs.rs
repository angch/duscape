//! NTFS's own files, sized from their master file table records.
//!
//! `$MFT`, `$LogFile`, `$Bitmap` and the rest occupy real clusters, but no directory lists them, so
//! a walk cannot count them. On one `C:\` they came to 2.9 GiB, `$MFT` alone 2.7 GiB. Several
//! cannot even be opened by name, backup privilege or not, but every one has a record in the MFT,
//! and `FSCTL_GET_NTFS_FILE_RECORD` returns it to an administrator. This module reads what a
//! record says about allocation; fetching records is the Windows walker's business.
//!
//! Only the parsing lives here, and it is plain byte handling, so it compiles and is tested on
//! every platform.

pub use libdiskonaut::metafiles::{EXTEND, ROOT_METAFILES, is_metafile_path, is_root_metafile};

/// The record-number half of a file reference; the top 16 bits are a sequence number.
#[must_use]
pub fn record_number(file_reference: u64) -> u64 {
    file_reference & 0x0000_FFFF_FFFF_FFFF
}

/// What one record says.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// Clusters this record's non-resident attributes occupy on disk, from their run lists.
    ///
    /// Not the attributes' allocated-size fields: those give the extent, holes included, and a
    /// sparse flag is not always there to say so. `$BadClus:$Bad` spans the whole volume and
    /// occupies only its bad clusters; read from its allocated size, it came to 460 GiB on a
    /// 457 GiB volume. A run list says which ranges have clusters behind them, so holes, sparse
    /// ranges and compression's savings all fall out of it. Resident attributes sit inside the
    /// record, so are part of `$MFT`'s own clusters and are not counted again.
    pub clusters: u64,
    /// Other records holding more of this file's attributes, from its attribute list.
    pub extensions: Vec<u64>,
    /// The attribute list was itself too large to be resident, so `extensions` may be short.
    pub incomplete: bool,
}

const ATTRIBUTE_LIST: u32 = 0x20;
const END: u32 = 0xFFFF_FFFF;

/// One run of a non-resident attribute: the cluster it starts at on the volume, or `None` for
/// a hole, and how many clusters it covers.
pub type Run = (Option<u64>, u64);

/// A run list (NTFS "mapping pairs") decoded.
///
/// Each run is a header byte whose low nibble is the size of the run's length and whose high
/// nibble is the size of its starting cluster — a signed number relative to the previous run's
/// start, so a file can be laid out backwards. A run with no starting cluster is a hole.
#[must_use]
pub fn decode_runs(runs: &[u8]) -> Vec<Run> {
    let mut decoded = Vec::new();
    let mut at = 0;
    let mut lcn = 0i64;
    while let Some(&header) = runs.get(at) {
        if header == 0 {
            break;
        }
        let length_size = usize::from(header & 0x0F);
        let offset_size = usize::from(header >> 4);
        if length_size > 8 || offset_size > 8 {
            break;
        }
        let Some(length_bytes) = runs.get(at + 1..at + 1 + length_size) else {
            break;
        };
        let Some(offset_bytes) = runs.get(at + 1 + length_size..at + 1 + length_size + offset_size)
        else {
            break;
        };
        let length = length_bytes
            .iter()
            .rev()
            .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
        if offset_size == 0 {
            decoded.push((None, length));
        } else {
            let mut offset = offset_bytes
                .iter()
                .rev()
                .fold(0i64, |value, &byte| (value << 8) | i64::from(byte));
            let bits = offset_size * 8;
            if bits < 64 && offset & (1i64 << (bits - 1)) != 0 {
                offset -= 1i64 << bits;
            }
            lcn = lcn.saturating_add(offset);
            if lcn < 0 {
                break;
            }
            decoded.push((Some(lcn as u64), length));
        }
        at += 1 + length_size + offset_size;
    }
    decoded
}

/// Clusters with disk space behind them in a run list: every run that is not a hole.
pub(crate) fn occupied_clusters(runs: &[u8]) -> u64 {
    // The same walk as `decode_runs`, without decoding the offsets or allocating: this runs
    // once per attribute of every record when a whole table is read.
    let mut clusters = 0u64;
    let mut at = 0;
    while let Some(&header) = runs.get(at) {
        if header == 0 {
            break;
        }
        let length_size = usize::from(header & 0x0F);
        let offset_size = usize::from(header >> 4);
        if length_size > 8 || offset_size > 8 {
            break;
        }
        let Some(length_bytes) = runs.get(at + 1..at + 1 + length_size) else {
            break;
        };
        if at + 1 + length_size + offset_size > runs.len() {
            break;
        }
        if offset_size > 0 {
            let length = length_bytes
                .iter()
                .rev()
                .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
            clusters = clusters.saturating_add(length);
        }
        at += 1 + length_size + offset_size;
    }
    clusters
}

pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// Undo the update sequence: NTFS stores a check value in the last two bytes of every 512-byte
/// sector of a record and keeps the real bytes in an array in the header.
///
/// A record that already has them restored is left as it is. Where a sector ends in the check
/// value it is replaced by the saved bytes; where the saved bytes are already there, replacing
/// them with themselves changes nothing. So this is right whether or not the caller's copy was
/// fixed up.
pub(crate) fn apply_fixups(record: &mut [u8]) -> Option<()> {
    let array = usize::from(u16_at(record, 0x04)?);
    let count = usize::from(u16_at(record, 0x06)?);
    if count == 0 {
        return Some(());
    }
    let check = u16_at(record, array)?;
    for sector in 1..count {
        let end = sector * 512 - 2;
        let saved = u16_at(record, array + sector * 2)?;
        if end + 2 > record.len() {
            break;
        }
        if u16_at(record, end)? == check {
            record[end..end + 2].copy_from_slice(&saved.to_le_bytes());
        }
    }
    Some(())
}

/// Read one file record, as `FSCTL_GET_NTFS_FILE_RECORD` returns it.
///
/// `None` for anything that is not an in-use `FILE` record, or that runs off its own end.
#[must_use]
pub fn parse_record(record: &[u8]) -> Option<Record> {
    if record.get(0..4)? != b"FILE" {
        return None;
    }
    let mut record = record.to_vec();
    apply_fixups(&mut record)?;
    let flags = u16_at(&record, 0x16)?;
    if flags & 0x0001 == 0 {
        return None;
    }
    let used = (u32_at(&record, 0x18)? as usize).min(record.len());
    let own_number = u64::from(u32_at(&record, 0x2C)?);

    let mut parsed = Record::default();
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
        let resident = attribute[8] == 0;
        if resident {
            if kind == ATTRIBUTE_LIST {
                let value_length = u32_at(attribute, 0x10)? as usize;
                let value_offset = usize::from(u16_at(attribute, 0x14)?);
                let list = attribute.get(value_offset..value_offset + value_length)?;
                read_attribute_list(list, own_number, &mut parsed.extensions);
            }
        } else if length >= 0x40 {
            // Every piece of a fragmented attribute carries the runs it maps, so each piece is
            // counted, wherever it lives, and none is counted twice.
            let runs_at = usize::from(u16_at(attribute, 0x20)?);
            parsed.clusters += occupied_clusters(attribute.get(runs_at..)?);
            if kind == ATTRIBUTE_LIST {
                parsed.incomplete = true;
            }
        }
        at += length;
    }
    Some(parsed)
}

/// The records other than `own` that an attribute list points into.
fn read_attribute_list(list: &[u8], own: u64, extensions: &mut Vec<u64>) {
    let mut at = 0;
    while at + 0x1A <= list.len() {
        let Some(length) = u16_at(list, at + 4).map(usize::from) else {
            break;
        };
        if length == 0 {
            break;
        }
        if let Some(reference) = u64_at(list, at + 0x10) {
            let number = record_number(reference);
            if number != own && !extensions.contains(&number) {
                extensions.push(number);
            }
        }
        at += length;
    }
}

#[cfg(test)]
mod tests;
