use ::std::collections::BTreeMap;
use ::std::path::PathBuf;
use ::std::sync::Arc;

use super::{Catalog, Parsed, ROOT_RECORD, data_runs, parse_file_record};
use crate::ntfs::decode_runs;
use crate::{DirEntries, EntryMeta};

const CHECK: u16 = 0x0007;
const CLUSTER: u64 = 4096;
const VOLUME: u64 = 0xABCD;

/// A 1024-byte in-use record, `attributes` laid out from 0x38, the update sequence applied as
/// it is on disk. `flags` are the record's (2 = directory), `base` the record this one extends.
fn record(number: u32, sequence: u16, flags: u16, base: u64, attributes: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0u8; 1024];
    bytes[0..4].copy_from_slice(b"FILE");
    bytes[0x04..0x06].copy_from_slice(&0x30u16.to_le_bytes());
    bytes[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
    bytes[0x10..0x12].copy_from_slice(&sequence.to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
    bytes[0x16..0x18].copy_from_slice(&(1u16 | flags).to_le_bytes());
    bytes[0x20..0x28].copy_from_slice(&base.to_le_bytes());
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

fn resident(kind: u32, value: &[u8]) -> Vec<u8> {
    let length = (0x18 + value.len()).next_multiple_of(8);
    let mut bytes = vec![0u8; length];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
    bytes[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
    bytes[0x18..0x18 + value.len()].copy_from_slice(value);
    bytes
}

/// A `$FILE_NAME` in `parent`, in `namespace` (1 Win32, 2 DOS, 3 both).
fn file_name(parent: u64, namespace: u8, name: &str) -> Vec<u8> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    let mut value = vec![0u8; 0x42 + wide.len() * 2];
    value[0..8].copy_from_slice(&parent.to_le_bytes());
    value[0x40] = wide.len() as u8;
    value[0x41] = namespace;
    for (index, unit) in wide.iter().enumerate() {
        value[0x42 + index * 2..0x44 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    resident(0x30, &value)
}

/// A `$FILE_NAME` for a reparse point: the attribute flag set and the tag at 0x3C, as NTFS
/// keeps them in the name.
fn file_name_reparse(parent: u64, name: &str, tag: u32) -> Vec<u8> {
    let mut attribute = file_name(parent, 3, name);
    attribute[0x18 + 0x38..0x18 + 0x3C].copy_from_slice(&0x0410u32.to_le_bytes());
    attribute[0x18 + 0x3C..0x18 + 0x40].copy_from_slice(&tag.to_le_bytes());
    attribute
}

fn data_resident(bytes: usize) -> Vec<u8> {
    resident(0x80, &vec![b'x'; bytes])
}

/// A non-resident unnamed `$DATA`: sizes, attribute flags, and runs of `(lcn, clusters)` with
/// `None` for a hole, mapped from `lowest_vcn`.
fn data_non_resident(
    lowest_vcn: u64,
    allocated: u64,
    real: u64,
    compressed: u64,
    flags: u16,
    runs: &[(Option<u64>, u64)],
) -> Vec<u8> {
    let mut list = Vec::new();
    let mut previous = 0i64;
    for &(lcn, length) in runs {
        let length_bytes: Vec<u8> =
            length.to_le_bytes()[..8 - (length.leading_zeros() / 8) as usize].to_vec();
        match lcn {
            None => {
                list.push(length_bytes.len() as u8);
                list.extend(&length_bytes);
            }
            Some(lcn) => {
                let delta = lcn as i64 - previous;
                previous = lcn as i64;
                let delta_bytes: Vec<u8> = delta.to_le_bytes()[..2].to_vec();
                list.push((2u8 << 4) | length_bytes.len() as u8);
                list.extend(&length_bytes);
                list.extend(&delta_bytes);
            }
        }
    }
    list.push(0);
    let length = (0x48 + list.len()).next_multiple_of(8);
    let mut bytes = vec![0u8; length];
    bytes[0..4].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[8] = 1;
    bytes[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
    bytes[0x10..0x18].copy_from_slice(&lowest_vcn.to_le_bytes());
    bytes[0x20..0x22].copy_from_slice(&0x48u16.to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
    bytes[0x30..0x38].copy_from_slice(&real.to_le_bytes());
    bytes[0x40..0x48].copy_from_slice(&compressed.to_le_bytes());
    bytes[0x48..0x48 + list.len()].copy_from_slice(&list);
    bytes
}

fn reparse(tag: u32) -> Vec<u8> {
    let mut value = vec![0u8; 8];
    value[0..4].copy_from_slice(&tag.to_le_bytes());
    resident(0xC0, &value)
}

/// A volume: the root (5) holds `docs\` (64, holding `a.txt` 65 resident and `big.bin` 66
/// non-resident), `link.txt` (67, hard-linked into `docs\` as `also.txt`), `LONGNAME.TXT`
/// (68, a Win32 name and a DOS name), `junction` (69, a directory with a mount point on it),
/// `twin-a` and `twin-b` (71, one file hard-linked twice in the root), and `$MFT` (0). Record 70
/// extends 66 and carries its name.
fn volume() -> Vec<Option<Parsed>> {
    let root = u64::from(ROOT_RECORD);
    let records: Vec<(u32, Vec<u8>)> = vec![
        (
            0,
            record(
                0,
                1,
                0,
                0,
                &[
                    file_name(root, 3, "$MFT"),
                    data_non_resident(0, 8 * CLUSTER, 8 * CLUSTER, 0, 0, &[(Some(4), 8)]),
                ],
            ),
        ),
        (5, record(5, 5, 2, 0, &[file_name(root, 3, ".")])),
        (64, record(64, 1, 2, 0, &[file_name(root, 3, "docs")])),
        (
            65,
            record(65, 1, 0, 0, &[file_name(64, 1, "a.txt"), data_resident(10)]),
        ),
        (
            66,
            record(
                66,
                7,
                0,
                0,
                &[data_non_resident(
                    0,
                    3 * CLUSTER,
                    9000,
                    0,
                    0,
                    &[(Some(100), 3)],
                )],
            ),
        ),
        (70, record(70, 1, 0, 66, &[file_name(64, 1, "big.bin")])),
        (
            67,
            record(
                67,
                2,
                0,
                0,
                &[
                    file_name(root, 1, "link.txt"),
                    file_name(64, 1, "also.txt"),
                    data_non_resident(0, CLUSTER, 100, 0, 0, &[(Some(200), 1)]),
                ],
            ),
        ),
        (
            68,
            record(
                68,
                1,
                0,
                0,
                &[
                    file_name(root, 2, "LONGNA~1.TXT"),
                    file_name(root, 1, "long name.txt"),
                    data_resident(3),
                ],
            ),
        ),
        (
            69,
            record(
                69,
                1,
                2,
                0,
                &[file_name(root, 3, "junction"), reparse(0xA000_0003)],
            ),
        ),
        (
            71,
            record(
                71,
                1,
                0,
                0,
                &[
                    file_name(root, 1, "twin-a"),
                    file_name(root, 1, "twin-b"),
                    data_resident(1),
                ],
            ),
        ),
    ];
    let mut table: Vec<Option<Parsed>> = (0..80).map(|_| None).collect();
    for (number, bytes) in records {
        table[number as usize] = parse_file_record(&bytes);
    }
    table
}

fn listed(directories: &[DirEntries]) -> BTreeMap<PathBuf, BTreeMap<String, EntryMeta>> {
    directories
        .iter()
        .map(|directory| {
            (
                directory.path.to_path_buf(),
                directory
                    .iter()
                    .map(|(name, meta)| (name.to_string_lossy().into_owned(), *meta))
                    .collect(),
            )
        })
        .collect()
}

/// Where the volume is scanned from: a drive root here, a directory elsewhere.
fn root() -> PathBuf {
    PathBuf::from(if cfg!(windows) { r"R:\" } else { "/R" })
}

fn walk(max_depth: Option<usize>) -> Vec<DirEntries> {
    let catalog = Catalog::assemble(volume(), VOLUME, CLUSTER);
    let mut out = Vec::new();
    catalog.emit(
        ROOT_RECORD,
        Arc::from(root().as_path()),
        max_depth,
        |batch| {
            out.extend(batch);
            true
        },
    );
    out
}

#[test]
fn records_parse_into_names_sizes_and_kinds() {
    let table = volume();
    let big = table[66].as_ref().expect("in use");
    assert_eq!(big.data, Some((3 * CLUSTER, 9000)));
    assert_eq!(big.sequence, 7);
    assert!(big.names.is_empty(), "its name is in the extension record");
    let extension = table[70].as_ref().expect("in use");
    assert_eq!(extension.base, Some(66));
    assert_eq!(big.base, None);
    assert_eq!(extension.names.len(), 1);
    let small = table[65].as_ref().expect("in use");
    assert_eq!(small.data, Some((0, 10)), "resident: nothing allocated");
    assert!(table[64].as_ref().expect("in use").is_dir);
    let junction = table[69].as_ref().expect("in use");
    assert!(junction.is_dir && junction.link);
    assert_eq!(table[1], None, "never written");
    assert_eq!(parse_file_record(b"BAAD"), None);
}

#[test]
fn the_tree_comes_out_as_the_kernel_walk_would_give_it() {
    let directories = walk(None);
    let tree = listed(&directories);
    assert_eq!(
        tree.keys().cloned().collect::<Vec<_>>(),
        [root(), root().join("docs")],
        "the junction is not entered"
    );
    let top = &tree[&root()];

    assert_eq!(
        top.keys().collect::<Vec<_>>(),
        [
            "$MFT",
            "docs",
            "junction",
            "link.txt",
            "long name.txt",
            "twin-a",
            "twin-b"
        ],
        "one name per entry: the Win32 one, never the DOS one beside it, never `.`; two names in one directory are two entries"
    );
    assert_eq!(top["twin-a"].links, 2, "hard-linked twice in one directory");
    assert_eq!(top["twin-a"], top["twin-b"]);
    assert!(top["docs"].is_dir);
    assert!(
        !top["junction"].is_dir,
        "a mount point is not a directory to enter"
    );
    assert_eq!(
        top["long name.txt"],
        EntryMeta {
            size: 0,
            apparent: 3,
            inode: top["long name.txt"].inode,
            links: 1,
            is_dir: false,
            shared_extent: 0,
        }
    );
    // NTFS's own file: sized by its clusters, with no length.
    assert_eq!((top["$MFT"].size, top["$MFT"].apparent), (8 * CLUSTER, 0));

    let docs = &tree[&root().join("docs")];
    assert_eq!(docs["a.txt"].size, 0);
    assert_eq!(docs["a.txt"].apparent, 10);
    assert_eq!(
        docs["big.bin"].size,
        3 * CLUSTER,
        "merged from its extension record"
    );
    assert_eq!(docs["big.bin"].apparent, 9000);
    // The hard link: the same file, with two names, counted as such.
    assert_eq!(top["link.txt"].links, 2);
    assert_eq!(docs["also.txt"], top["link.txt"]);
    assert_ne!(top["link.txt"].inode, docs["a.txt"].inode);
    // The id is the reference (sequence in the top bits) folded with the volume.
    assert_eq!(
        docs["big.bin"].inode,
        (66 | (7u64 << 48)) ^ VOLUME.rotate_left(48)
    );
}

#[test]
fn the_depth_limit_lists_but_does_not_enter() {
    // As the kernel walkers count it: `--max-depth 1` is the root's entries alone.
    let directories = walk(Some(1));
    assert_eq!(directories.len(), 1, "the root only");
    assert!(
        directories[0]
            .iter()
            .any(|(name, meta)| name == "docs" && meta.is_dir)
    );
    assert_eq!(walk(Some(0)).len(), 1);
    assert_eq!(walk(Some(2)).len(), 2);
}

#[test]
fn compressed_and_sparse_files_take_their_compressed_size() {
    let plain = parse_file_record(&record(
        66,
        1,
        0,
        0,
        &[data_non_resident(
            0,
            40960,
            40000,
            8192,
            0,
            &[(Some(1), 10)],
        )],
    ))
    .expect("in use");
    assert_eq!(plain.data, Some((40960, 40000)));
    let sparse = parse_file_record(&record(
        66,
        1,
        0,
        0,
        &[data_non_resident(
            0,
            40960,
            40000,
            8192,
            0x8000,
            &[(Some(1), 2), (None, 8)],
        )],
    ))
    .expect("in use");
    assert_eq!(sparse.data, Some((8192, 40000)));
    let compressed = parse_file_record(&record(
        66,
        1,
        0,
        0,
        &[data_non_resident(
            0,
            40960,
            40000,
            12288,
            0x0001,
            &[(Some(1), 3)],
        )],
    ))
    .expect("in use");
    assert_eq!(compressed.data, Some((12288, 40000)));
    // A later piece of the stream carries no sizes.
    let later = parse_file_record(&record(
        66,
        1,
        0,
        0,
        &[data_non_resident(16, 40960, 40000, 0, 0, &[(Some(1), 3)])],
    ))
    .expect("in use");
    assert_eq!(later.data, None);
}

#[test]
fn runs_are_decoded_with_signed_relative_offsets() {
    // 0x21 length 1 byte, offset 1 byte: 3 clusters at 16; then 0x11: 2 clusters at 16 - 4 = 12;
    // then a hole of 5; then 0x21: 1 cluster at 12 + 0x100 (two-byte offset).
    let runs = [
        0x11, 0x03, 0x10, 0x11, 0x02, 0xFC, 0x01, 0x05, 0x21, 0x01, 0x00, 0x01, 0x00,
    ];
    assert_eq!(
        decode_runs(&runs),
        [
            (Some(16), 3),
            (Some(12), 2),
            (None, 5),
            (Some(12 + 0x100), 1)
        ]
    );
    let zero = record(
        0,
        1,
        0,
        0,
        &[
            file_name(5, 3, "$MFT"),
            data_non_resident(0, 8 * CLUSTER, 8 * CLUSTER, 0, 0, &[(Some(4), 8)]),
        ],
    );
    assert_eq!(data_runs(&zero), Some((0, vec![(Some(4), 8)])));
}

/// A junction whose `$REPARSE_POINT` is out of reach (non-resident, or elsewhere) is still known
/// from its name's tag, so it is not entered; a placeholder with another tag is a directory.
#[test]
fn a_reparse_tag_in_the_name_marks_a_link() {
    let junction = parse_file_record(&record(
        69,
        1,
        2,
        0,
        &[file_name_reparse(5, "junction", 0xA000_0003)],
    ))
    .expect("in use");
    assert!(junction.is_dir && junction.link);
    let placeholder = parse_file_record(&record(
        69,
        1,
        2,
        0,
        &[file_name_reparse(5, "cloud", 0x9000_601A)],
    ))
    .expect("in use");
    assert!(placeholder.is_dir && !placeholder.link);
}
