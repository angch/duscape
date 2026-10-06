use ::std::ffi::OsString;
use ::std::io::Write;
use ::std::path::{Path, PathBuf};

use super::{Member, archive_of, is_archive_name, member_of, read_index, read_member};

/// One entry for [`zip`]: its name, its bytes, whether deflated, and whether it is marked as
/// UTF-8 (a name in code page 437 is not).
struct Entry<'a> {
    name: &'a [u8],
    data: &'a [u8],
    deflate: bool,
    utf8: bool,
}

fn entry<'a>(name: &'a str, data: &'a [u8], deflate: bool) -> Entry<'a> {
    Entry {
        name: name.as_bytes(),
        data,
        deflate,
        utf8: true,
    }
}

/// A zip of `entries`, after `prefix` (a self-extracting stub's bytes), with `comment` at its
/// end; with `zip64`, its end records are Zip64's (the classic one's fields all 0xFF…).
fn zip(prefix: &[u8], entries: &[Entry], comment: &[u8], zip64: bool) -> Vec<u8> {
    let mut out = prefix.to_vec();
    let mut central = Vec::new();
    for item in entries {
        let packed = if item.deflate {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(item.data).expect("deflate");
            encoder.finish().expect("deflate")
        } else {
            item.data.to_vec()
        };
        let method: u16 = if item.deflate { 8 } else { 0 };
        let flags: u16 = if item.utf8 { 0x0800 } else { 0 };
        // Offsets in a self-extracting archive are from the archive's start, not the file's.
        let header = (out.len() - prefix.len()) as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0]);
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 8]); // time, date, crc
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(item.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(item.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(item.name);
        out.extend_from_slice(&packed);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0]);
        central.extend_from_slice(&flags.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(item.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(item.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
        central.extend_from_slice(&header.to_le_bytes());
        central.extend_from_slice(item.name);
    }
    let directory_offset = (out.len() - prefix.len()) as u64;
    out.extend_from_slice(&central);
    let count = entries.len() as u64;
    if zip64 {
        let record = (out.len() - prefix.len()) as u64;
        out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&[45, 0, 45, 0]);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&(central.len() as u64).to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(record + prefix.len() as u64).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    if zip64 {
        out.extend_from_slice(&[0xFF; 4]);
        out.extend_from_slice(&[0xFF; 4]);
        out.extend_from_slice(&[0xFF; 4]);
    } else {
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&(directory_offset as u32).to_le_bytes());
    }
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    out
}

fn temp(name: &str) -> PathBuf {
    let dir = ::std::env::temp_dir().join(format!("duscape_archive_{name}"));
    let _ = ::std::fs::remove_dir_all(&dir);
    ::std::fs::create_dir_all(&dir).expect("create");
    dir
}

fn paths(members: &[Member]) -> Vec<String> {
    members
        .iter()
        .map(|member| member.path.join("/") + if member.is_dir { "/" } else { "" })
        .collect()
}

#[test]
fn archives_are_known_by_their_extension_in_any_case() {
    for name in [
        "a.zip",
        "lib.JAR",
        "app.apk",
        "pkg-1.0-py3-none-any.whl",
        "x.nupkg",
    ] {
        assert!(is_archive_name(name.as_bytes()), "{name}");
    }
    for name in [
        "zip",
        ".zip",
        "report.docx",
        "book.epub",
        "a.zip.part",
        "notes.txt",
    ] {
        assert!(!is_archive_name(name.as_bytes()), "{name}");
    }
}

/// The index lists every entry with both sizes, folders as folders, one going up (`..`) left
/// out; an archive's comment and a stub before it are no obstacle.
#[test]
fn the_index_lists_every_entry_with_its_sizes() {
    let text = b"hello hello hello hello hello hello hello\n".repeat(50);
    let bytes = zip(
        b"MZ a self-extracting stub",
        &[
            entry("META-INF/", b"", false),
            entry("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n", false),
            entry("com/example/Main.class", &text, true),
            entry("../evil/../up.txt", b"x", false),
        ],
        b"an archive comment",
        false,
    );
    let dir = temp("index");
    let path = dir.join("app.jar");
    ::std::fs::write(&path, &bytes).expect("write");
    let index = read_index(&path).expect("read");
    assert_eq!(
        paths(&index.members),
        [
            "META-INF/",
            "META-INF/MANIFEST.MF",
            "com/example/Main.class",
        ]
    );
    let class = &index.members[2];
    assert_eq!(class.uncompressed, text.len() as u64);
    assert!(class.compressed < class.uncompressed, "deflated");
    assert_eq!(class.how(), "deflated");
    assert!(!index.truncated);
    // And an entry is unpacked from it, stored or deflated, up to the limit asked.
    let (head, member) = read_member(
        &path,
        &[
            OsString::from("com"),
            OsString::from("example"),
            OsString::from("Main.class"),
        ],
        10,
    )
    .expect("unpacked");
    assert_eq!(head, b"hello hell");
    assert_eq!(member.uncompressed, text.len() as u64);
    let (manifest, _) = read_member(
        &path,
        &[OsString::from("META-INF"), OsString::from("MANIFEST.MF")],
        1024,
    )
    .expect("stored");
    assert_eq!(manifest, b"Manifest-Version: 1.0\n");
    let _ = ::std::fs::remove_dir_all(&dir);
}

/// An archive past 4 GiB or 65,535 entries keeps its index's figures in Zip64's records.
#[test]
fn a_zip64_index_is_read() {
    let bytes = zip(b"", &[entry("big.bin", b"0123456789", false)], b"", true);
    let dir = temp("zip64");
    let path = dir.join("big.zip");
    ::std::fs::write(&path, &bytes).expect("write");
    let index = read_index(&path).expect("read");
    assert_eq!(paths(&index.members), ["big.bin"]);
    let (data, _) = read_member(&path, &[OsString::from("big.bin")], 100).expect("unpacked");
    assert_eq!(data, b"0123456789");
    let _ = ::std::fs::remove_dir_all(&dir);
}

/// A name the archive does not mark as UTF-8, and that is not, is code page 437's.
#[test]
fn an_old_name_is_read_as_code_page_437() {
    let bytes = zip(
        b"",
        &[Entry {
            name: b"r\x82sum\x82.txt",
            data: b"x",
            deflate: false,
            utf8: false,
        }],
        b"",
        false,
    );
    let dir = temp("cp437");
    let path = dir.join("old.zip");
    ::std::fs::write(&path, &bytes).expect("write");
    assert_eq!(
        paths(&read_index(&path).expect("read").members),
        ["résumé.txt"]
    );
    let _ = ::std::fs::remove_dir_all(&dir);
}

/// What is not a zip says so, and nothing is listed.
#[test]
fn a_file_that_is_no_zip_is_an_error() {
    let dir = temp("not");
    let path = dir.join("fake.zip");
    ::std::fs::write(&path, b"this is not a zip at all, whatever its name").expect("write");
    assert!(read_index(&path).is_err());
    ::std::fs::write(&path, b"").expect("write");
    assert!(read_index(&path).is_err());
    let _ = ::std::fs::remove_dir_all(&dir);
}

/// A path inside an archive is found to be, by the archive above it on disk; the archive itself,
/// a real file and a missing path in a folder are not inside one.
#[test]
fn a_path_inside_an_archive_finds_its_archive() {
    let dir = temp("member_of");
    let archive = dir.join("lib.jar");
    ::std::fs::write(
        &archive,
        zip(b"", &[entry("a/b.txt", b"x", false)], b"", false),
    )
    .expect("write");
    let inside = archive.join("a").join("b.txt");
    assert_eq!(
        member_of(&inside),
        Some((
            archive.clone(),
            vec![OsString::from("a"), OsString::from("b.txt")]
        ))
    );
    assert_eq!(member_of(&archive), None, "the archive itself is on disk");
    assert_eq!(archive_of(&archive), Some(archive.clone()));
    assert_eq!(archive_of(&inside), Some(archive.clone()));
    assert_eq!(member_of(&dir.join("missing.txt")), None, "under a folder");
    let plain = dir.join("plain.txt");
    ::std::fs::write(&plain, b"x").expect("write");
    assert_eq!(
        member_of(&plain.join("x")),
        None,
        "under a file that is no archive"
    );
    assert_eq!(super::on_disk(&inside), archive);
    assert_eq!(super::on_disk(Path::new(&plain)), plain);
    let _ = ::std::fs::remove_dir_all(&dir);
}

/// A name with no place inside the archive's folder is left out: one going up, and on Windows
/// one with a drive (`C:`), which a path joined onto the archive's takes for the drive — a
/// delete of the entry reached it. Empty parts and `.` are dropped, a backslash is a separator.
#[test]
fn a_name_outside_the_archive_is_left_out() {
    let dir = temp("names");
    let path = dir.join("odd.zip");
    let mut entries = vec![
        ("../up.txt", b"x".as_slice()),
        ("a/../../up.txt", b"x"),
        ("./a//b\\c.txt", b"x"),
        ("kept.txt", b"x"),
    ];
    if cfg!(windows) {
        entries.push(("C:/Windows/drive.txt", b"x"));
        entries.push(("a/D:x", b"x"));
    }
    ::std::fs::write(&path, super::stored_zip(&entries)).expect("write");
    let index = read_index(&path).expect("read");
    assert_eq!(paths(&index.members), ["a/b/c.txt", "kept.txt"]);
    let _ = ::std::fs::remove_dir_all(&dir);
}
