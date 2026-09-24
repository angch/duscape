use super::{Record, is_root_metafile, parse_record, record_number};

const CHECK: u16 = 0x0007;

/// A 1024-byte in-use record numbered `number`, with `attributes` laid out from 0x38 and the
/// update sequence applied as it is on disk: each sector's last two bytes moved into the array
/// and replaced by the check value.
fn record(number: u32, attributes: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0u8; 1024];
    bytes[0..4].copy_from_slice(b"FILE");
    bytes[0x04..0x06].copy_from_slice(&0x30u16.to_le_bytes());
    bytes[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
    bytes[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
    bytes[0x2C..0x30].copy_from_slice(&number.to_le_bytes());
    let mut at = 0x38;
    for attribute in attributes {
        bytes[at..at + attribute.len()].copy_from_slice(attribute);
        at += attribute.len();
    }
    bytes[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    bytes[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());

    bytes[0x30..0x32].copy_from_slice(&CHECK.to_le_bytes());
    for sector in 1..3 {
        let end = sector * 512 - 2;
        let saved = [bytes[end], bytes[end + 1]];
        bytes[0x30 + sector * 2..0x32 + sector * 2].copy_from_slice(&saved);
        bytes[end..end + 2].copy_from_slice(&CHECK.to_le_bytes());
    }
    bytes
}

fn resident(kind: u32, length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
    bytes
}

/// A run: its length in clusters, and whether it has clusters behind it (`false` is a hole).
type Run = (u64, bool);

/// A non-resident attribute mapping `runs`, whose allocated-size field claims `claimed` — which
/// the parser must ignore, as `$BadClus` showed.
fn non_resident(kind: u32, runs: &[Run], claimed: u64) -> Vec<u8> {
    let mut list = Vec::new();
    for &(length, backed) in runs {
        let length_bytes: Vec<u8> = length
            .to_le_bytes()
            .into_iter()
            .take(8 - (length.leading_zeros() / 8) as usize)
            .collect();
        let offset_size = if backed { 1u8 } else { 0 };
        list.push((offset_size << 4) | length_bytes.len() as u8);
        list.extend(&length_bytes);
        if backed {
            list.push(0x10);
        }
    }
    list.push(0);
    let length = (0x40 + list.len()).next_multiple_of(8);
    let mut bytes = vec![0u8; length];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[8] = 1;
    bytes[0x20..0x22].copy_from_slice(&0x40u16.to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&claimed.to_le_bytes());
    bytes[0x40..0x40 + list.len()].copy_from_slice(&list);
    bytes
}

fn attribute_list(references: &[u64]) -> Vec<u8> {
    let length = 0x18 + references.len() * 0x20;
    let mut bytes = resident(0x20, length);
    bytes[0x10..0x14].copy_from_slice(&((references.len() * 0x20) as u32).to_le_bytes());
    for (index, reference) in references.iter().enumerate() {
        let entry = 0x18 + index * 0x20;
        bytes[entry + 4..entry + 6].copy_from_slice(&0x20u16.to_le_bytes());
        bytes[entry + 0x10..entry + 0x18].copy_from_slice(&reference.to_le_bytes());
    }
    bytes
}

/// Every non-resident attribute counts the clusters its runs occupy, whatever its size fields
/// claim; resident ones live inside the MFT and count nothing here.
#[test]
fn counts_the_clusters_runs_occupy() {
    let parsed = parse_record(&record(
        0,
        &[
            resident(0x10, 0x60),
            non_resident(0x80, &[(256, true), (3, true)], 1 << 40),
            non_resident(0xA0, &[(1, true)], 4096),
        ],
    ))
    .expect("a record");
    assert_eq!(parsed.clusters, 260);
    assert!(parsed.extensions.is_empty());
}

/// `$BadClus:$Bad` spans the whole volume in one hole and claims all of it as allocated: it
/// occupies nothing. A sparse journal is holes and runs mixed.
#[test]
fn holes_occupy_nothing() {
    let volume_clusters = 119_738_879;
    let bad_clusters = parse_record(&record(
        8,
        &[non_resident(
            0x80,
            &[(volume_clusters, false)],
            volume_clusters * 4096,
        )],
    ))
    .expect("a record");
    assert_eq!(bad_clusters.clusters, 0);

    let journal = parse_record(&record(
        40,
        &[non_resident(
            0x80,
            &[(1 << 30, false), (2048, true), (10, false), (7, true)],
            0,
        )],
    ))
    .expect("a record");
    assert_eq!(journal.clusters, 2055);
}

/// A fragmented attribute is split across records, each piece mapping its own runs, and each is
/// counted where it is found.
#[test]
fn each_piece_of_an_attribute_counts() {
    let first =
        parse_record(&record(0, &[non_resident(0x80, &[(100, true)], 1 << 20)])).expect("a record");
    let rest =
        parse_record(&record(17, &[non_resident(0x80, &[(50, true)], 0)])).expect("a record");
    assert_eq!(first.clusters + rest.clusters, 150);
}

/// A fragmented file lists the records that hold the rest of it; its own is not one of them.
#[test]
fn attribute_lists_name_extension_records() {
    let sequence = 5u64 << 48;
    let parsed = parse_record(&record(
        0,
        &[attribute_list(&[
            sequence,
            sequence | 17,
            sequence | 17,
            23,
        ])],
    ))
    .expect("a record");
    assert_eq!(parsed.extensions, vec![17, 23]);
    assert!(!parsed.incomplete);
}

/// A value straddling a sector boundary reads back whole once the update sequence is undone.
#[test]
fn the_update_sequence_is_undone() {
    // The run list starts 0x40 into the attribute; putting its header at byte 509 puts the run's
    // two length bytes over the first sector's last two (510 and 511).
    let padding = resident(0x10, 509 - 0x40 - 0x38);
    let parsed = parse_record(&record(
        0,
        &[padding, non_resident(0x80, &[(0x3456, true)], 0)],
    ))
    .expect("a record");
    assert_eq!(parsed.clusters, 0x3456);
}

#[test]
fn only_in_use_file_records_parse() {
    let mut free = record(0, &[]);
    free[0x16] = 0;
    assert_eq!(parse_record(&free), None);
    let mut bad = record(0, &[]);
    bad[0..4].copy_from_slice(b"BAAD");
    assert_eq!(parse_record(&bad), None);
    assert_eq!(parse_record(&[0u8; 8]), None);
    assert_eq!(
        parse_record(&record(0, &[])),
        Some(Record::default()),
        "an empty record allocates nothing"
    );
}

#[test]
fn names_and_references() {
    assert!(is_root_metafile("$MFT"));
    assert!(is_root_metafile("$Extend"));
    assert!(!is_root_metafile("$Recycle.Bin"));
    assert!(!is_root_metafile("Windows"));
    assert_eq!(record_number((7u64 << 48) | 42), 42);
}

/// Only a volume root's metadata entries are refused, and only on Windows.
#[test]
fn metafile_paths_are_recognised_at_a_volume_root() {
    use ::std::ffi::OsString;
    use ::std::path::Path;
    let names = |names: &[&str]| names.iter().map(OsString::from).collect::<Vec<_>>();
    let root = if cfg!(windows) {
        Path::new(r"\\?\C:\")
    } else {
        Path::new("/")
    };
    assert_eq!(
        super::is_metafile_path(root, &names(&["$MFT"])),
        cfg!(windows)
    );
    assert_eq!(
        super::is_metafile_path(root, &names(&["$Extend", "$UsnJrnl"])),
        cfg!(windows)
    );
    assert!(!super::is_metafile_path(root, &names(&["Windows"])));
    assert!(!super::is_metafile_path(
        &root.join("folder"),
        &names(&["$MFT"])
    ));
}
