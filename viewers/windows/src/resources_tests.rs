//! The build script's resource writer (`resources.rs`, included here as in `build.rs`), tested
//! on the icon it packs, on every platform.

// The writer's entry point prints to cargo; only its parts are tested here.
#![allow(dead_code)]

include!("../resources.rs");

fn icon() -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("duscape.ico")).expect("duscape.ico")
}

#[test]
fn the_icon_file_becomes_an_image_each_and_a_group_listing_them() {
    let resources = icon_resources(&icon());
    let (group, images) = resources.split_last().expect("resources");
    assert_eq!((group.0, group.1), (RT_GROUP_ICON, 1));
    assert_eq!(images.len(), 10);
    let count = u16::from_le_bytes([group.2[4], group.2[5]]);
    assert_eq!(usize::from(count), images.len());
    for (index, (kind, id, data)) in images.iter().enumerate() {
        assert_eq!((*kind, usize::from(*id)), (RT_ICON, index + 1));
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n", "each image is a PNG");
        // The group's entry names this image by id, with its size in bytes.
        let entry = &group.2[6 + 14 * index..6 + 14 * (index + 1)];
        assert_eq!(u16::from_le_bytes([entry[12], entry[13]]), *id);
        let bytes = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        assert_eq!(bytes as usize, data.len());
    }
}

#[test]
fn the_resource_file_starts_empty_and_keeps_each_entry_aligned() {
    let resources = icon_resources(&icon());
    let res = res_file(&resources);
    assert_eq!(&res[..8], &[0, 0, 0, 0, 32, 0, 0, 0], "the empty entry");
    // Walk the entries: each a header of 32 and its data, on four-byte boundaries.
    let mut at = 32;
    let mut seen = 0;
    while at < res.len() {
        let size = u32::from_le_bytes(res[at..at + 4].try_into().unwrap()) as usize;
        let header = u32::from_le_bytes(res[at + 4..at + 8].try_into().unwrap()) as usize;
        assert_eq!(header, 32);
        let kind = u16::from_le_bytes([res[at + 10], res[at + 11]]);
        assert_eq!(kind, resources[seen].0);
        assert_eq!(size, resources[seen].2.len());
        at = (at + header + size).next_multiple_of(4);
        seen += 1;
    }
    assert_eq!(seen, resources.len());
}

#[test]
fn the_object_holds_one_rsrc_section_with_a_relocated_entry_a_resource() {
    let mut resources = icon_resources(&icon());
    resources.push((RT_VERSION, 1, version_resource(&version())));
    resources.push((RT_MANIFEST, 1, b"<assembly/>".to_vec()));
    let object = coff_object(&resources, 0x8664, 3);
    let u16_at = |at: usize| u16::from_le_bytes([object[at], object[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(object[at..at + 4].try_into().unwrap());
    assert_eq!(u16_at(0), 0x8664);
    assert_eq!(u16_at(2), 1, "one section");
    assert_eq!(&object[20..28], b".rsrc\0\0\0");
    let (size, raw) = (u32_at(20 + 16) as usize, u32_at(20 + 20) as usize);
    assert_eq!(
        u16_at(20 + 32) as usize,
        resources.len(),
        "a relocation a resource"
    );
    let section = &object[raw..raw + size];
    // The root lists the four types, in order.
    let root_ids = u16::from_le_bytes([section[14], section[15]]);
    assert_eq!(root_ids, 4);
    let types: Vec<u32> = (0..4)
        .map(|index| {
            u32::from_le_bytes(section[16 + 8 * index..20 + 8 * index].try_into().unwrap())
        })
        .collect();
    assert_eq!(types, [3, 14, 16, 24]);
    // The manifest's bytes are in the section, where its data entry says.
    let manifest = section
        .windows(11)
        .position(|window| window == b"<assembly/>")
        .expect("the manifest");
    assert_eq!(manifest % 8, 0, "data is 8-aligned");
}

/// A version-resource node read back: its key, its value's bytes, and its children.
struct Node {
    key: String,
    text: bool,
    value: Vec<u8>,
    children: Vec<Node>,
}

/// The node at the start of `bytes`, read as `VerQueryValueW` reads it: lengths, key, value and
/// children each on a four-byte boundary of the resource (`bytes` starts on one).
fn read_node(bytes: &[u8]) -> Node {
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let length = usize::from(u16_at(0));
    assert!(length <= bytes.len(), "a node within its parent");
    let (value_length, text) = (usize::from(u16_at(2)), u16_at(4) == 1);
    let mut at = 6;
    let mut key = Vec::new();
    while u16_at(at) != 0 {
        key.push(u16_at(at));
        at += 2;
    }
    at = (at + 2).next_multiple_of(4);
    let value_bytes = if text { 2 * value_length } else { value_length };
    let value = bytes[at..at + value_bytes].to_vec();
    at = (at + value_bytes).next_multiple_of(4);
    let mut children = Vec::new();
    while at < length {
        let child = read_node(&bytes[at..length]);
        at = (at + usize::from(u16_at(at))).next_multiple_of(4);
        children.push(child);
    }
    Node {
        key: String::from_utf16(&key).expect("a key"),
        text,
        value,
        children,
    }
}

fn text_of(value: &[u8]) -> String {
    let units: Vec<u16> = value
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let (last, text) = units.split_last().expect("a terminated string");
    assert_eq!(*last, 0, "strings end with a zero");
    String::from_utf16(text).expect("UTF-16")
}

fn version() -> Version {
    Version {
        numbers: [0, 2, 1, 0],
        text: "0.2.1".to_owned(),
        prerelease: false,
        name: "duscape-windows".to_owned(),
        company: "Ang Chin Han".to_owned(),
    }
}

#[test]
fn the_version_resource_gives_the_numbers_and_the_strings_explorer_shows() {
    let bytes = version_resource(&version());
    let root = read_node(&bytes);
    assert_eq!(
        usize::from(u16::from_le_bytes([bytes[0], bytes[1]])),
        bytes.len()
    );
    assert_eq!((root.key.as_str(), root.text), ("VS_VERSION_INFO", false));
    let field =
        |index: usize| u32::from_le_bytes(root.value[4 * index..4 * index + 4].try_into().unwrap());
    assert_eq!(root.value.len(), 52);
    assert_eq!(field(0), 0xFEEF_04BD, "the signature");
    assert_eq!(
        (field(2), field(3)),
        (0x0000_0002, 0x0001_0000),
        "file version 0.2.1.0"
    );
    assert_eq!(
        (field(4), field(5)),
        (0x0000_0002, 0x0001_0000),
        "product version"
    );
    assert_eq!(field(7), 0, "a release");
    assert_eq!(field(9), 1, "an application");

    let [strings, vars] = &root.children[..] else {
        panic!("StringFileInfo and VarFileInfo");
    };
    assert_eq!(strings.key, "StringFileInfo");
    let [table] = &strings.children[..] else {
        panic!("one table")
    };
    assert_eq!(table.key, "040904B0");
    let found = |key: &str| {
        let node = table.children.iter().find(|node| node.key == key);
        text_of(&node.unwrap_or_else(|| panic!("{key}")).value)
    };
    assert_eq!(found("FileDescription"), "duscape", "Task Manager's name");
    assert_eq!(found("ProductName"), "duscape");
    assert_eq!(found("FileVersion"), "0.2.1");
    assert_eq!(found("ProductVersion"), "0.2.1");
    assert_eq!(found("OriginalFilename"), "duscape-windows.exe");
    assert_eq!(found("InternalName"), "duscape-windows");
    assert_eq!(found("CompanyName"), "Ang Chin Han");

    // The translation names the table: US English, UTF-16.
    assert_eq!(vars.key, "VarFileInfo");
    let [translation] = &vars.children[..] else {
        panic!("one var")
    };
    assert_eq!(translation.key, "Translation");
    assert_eq!(translation.value, [0x09, 0x04, 0xB0, 0x04]);
}

#[test]
fn the_copyright_is_the_licence_files() {
    let licence = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE"))
        .expect("LICENSE");
    let line = licence
        .lines()
        .find(|line| line.starts_with("Copyright"))
        .expect("a copyright line");
    assert!(COPYRIGHT.starts_with(line), "{COPYRIGHT:?} is not {line:?}");
}

#[test]
fn the_package_version_is_the_one_cargo_gives() {
    // Cargo sets the same variables for a test as for the build script.
    let version = Version::of_package("duscape-windows");
    assert_eq!(version.text, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        version.prerelease,
        !env!("CARGO_PKG_VERSION_PRE").is_empty()
    );
    let numbers: Vec<u16> = env!("CARGO_PKG_VERSION")
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.parse().expect("a number"))
        .collect();
    assert_eq!(version.numbers[..3], numbers[..]);
    assert!(!version.company.is_empty() && !version.company.contains('<'));
}

#[test]
fn a_pre_release_says_so_in_the_fixed_flags() {
    let version = Version {
        prerelease: true,
        text: "0.3.0-rc.1".to_owned(),
        ..version()
    };
    let root = read_node(&version_resource(&version));
    let flags = u32::from_le_bytes(root.value[28..32].try_into().unwrap());
    assert_eq!(flags, 0x2, "VS_FF_PRERELEASE");
}
