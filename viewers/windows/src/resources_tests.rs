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
    // The root lists the three types, in order.
    let root_ids = u16::from_le_bytes([section[14], section[15]]);
    assert_eq!(root_ids, 3);
    let types: Vec<u32> = (0..3)
        .map(|index| {
            u32::from_le_bytes(section[16 + 8 * index..20 + 8 * index].try_into().unwrap())
        })
        .collect();
    assert_eq!(types, [3, 14, 24]);
    // The manifest's bytes are in the section, where its data entry says.
    let manifest = section
        .windows(11)
        .position(|window| window == b"<assembly/>")
        .expect("the manifest");
    assert_eq!(manifest % 8, 0, "data is 8-aligned");
}
