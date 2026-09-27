// The Windows binaries' own resources — the icon Explorer shows for the `.exe`, and on GNU
// targets the manifest beside it — written here, not by a resource compiler, which the zig
// cross-build of the release does not have. Included by the build scripts of `duscape-windows`
// and of `duscape` (`include!`), so both binaries carry the same icon.
//
// On MSVC the resources go to the linker as a `.res` file, which `link.exe` converts itself;
// the manifest is `embed-manifest`'s, by linker options. On GNU they go as one COFF object with
// a `.rsrc` section — the manifest in it too, since a second `.rsrc` object would not link.

use ::std::env;
use ::std::fs;
use ::std::path::{Path, PathBuf};

/// Resource types, as `winuser.h` numbers them.
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;
/// en-US, what `rc` and `embed-manifest` give when nothing is said.
const LANGUAGE: u16 = 1033;

/// A resource: its type, its id, and its bytes.
type Resource = (u16, u16, Vec<u8>);

/// Link `icon` (a `.ico` file) into the binaries as their icon, and `manifest` on GNU targets.
fn embed_resources(icon: &Path, manifest: Option<&Path>) {
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ico = fs::read(icon).unwrap_or_else(|error| panic!("{}: {error}", icon.display()));
    let mut resources = icon_resources(&ico);
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let path = if msvc {
        let path = out.join("duscape-resources.res");
        fs::write(&path, res_file(&resources)).expect("writing the resource file");
        path
    } else {
        if let Some(manifest) = manifest {
            println!("cargo:rerun-if-changed={}", manifest.display());
            let bytes = fs::read(manifest)
                .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
            resources.push((RT_MANIFEST, 1, bytes));
        }
        let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let (machine, relocation) = match arch.as_str() {
            "x86" => (0x014c, 7),     // IMAGE_REL_I386_DIR32NB
            "aarch64" => (0xaa64, 2), // IMAGE_REL_ARM64_ADDR32NB
            _ => (0x8664, 3),         // IMAGE_REL_AMD64_ADDR32NB
        };
        let path = out.join("duscape-resources.o");
        fs::write(&path, coff_object(&resources, machine, relocation))
            .expect("writing the resource object");
        path
    };
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// A `.ico` file as resources: each image an `RT_ICON` (ids from 1), and the `RT_GROUP_ICON`
/// (id 1) listing them, which is what Explorer looks up.
fn icon_resources(ico: &[u8]) -> Vec<Resource> {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    assert!(ico.len() >= 6 && u16_at(2) == 1, "not an icon file");
    let count = usize::from(u16_at(4));
    let mut group = Vec::with_capacity(6 + 14 * count);
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&(count as u16).to_le_bytes());
    let mut resources = Vec::with_capacity(count + 1);
    for index in 0..count {
        let entry = 6 + 16 * index;
        let (size, offset) = (u32_at(entry + 8) as usize, u32_at(entry + 12) as usize);
        let id = index as u16 + 1;
        // The directory entry as it was, less the offset, and with the image's id in its place.
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
        resources.push((RT_ICON, id, ico[offset..offset + size].to_vec()));
    }
    resources.push((RT_GROUP_ICON, 1, group));
    resources
}

/// A `.res` file of `resources`: an empty entry first, then each with its header of ordinals.
fn res_file(resources: &[Resource]) -> Vec<u8> {
    let mut out = Vec::new();
    let entry = |out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, language: u16, data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&32u32.to_le_bytes()); // header size
        out.extend_from_slice(&[0xFF, 0xFF]);
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&[0xFF, 0xFF]);
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // data version
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&language.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // version
        out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    };
    entry(&mut out, 0, 0, 0, 0, &[]);
    for (kind, id, data) in resources {
        // Moveable, pure, discardable: what `rc` gives icons.
        entry(&mut out, *kind, *id, 0x1030, LANGUAGE, data);
    }
    out
}

/// A COFF object with one `.rsrc` section holding `resources` as a resource tree — types, then
/// ids, then one language each — whose data entries the linker relocates to image addresses.
fn coff_object(resources: &[Resource], machine: u16, relocation: u16) -> Vec<u8> {
    let mut sorted: Vec<&Resource> = resources.iter().collect();
    sorted.sort_by_key(|(kind, id, _)| (*kind, *id));
    let mut kinds: Vec<u16> = sorted.iter().map(|(kind, _, _)| *kind).collect();
    kinds.dedup();
    let table = |entries: usize| 16 + 8 * entries;
    let ids_of = |kind: u16| sorted.iter().filter(|(k, _, _)| *k == kind).count();
    // Where each part of the tree goes: the root, a table a type, a table a resource (its one
    // language), a data entry a resource, then the data.
    let root = 0;
    let mut at = table(kinds.len());
    let mut type_tables = Vec::new();
    for &kind in &kinds {
        type_tables.push(at);
        at += table(ids_of(kind));
    }
    let language_tables: Vec<usize> = (0..sorted.len())
        .map(|index| at + index * table(1))
        .collect();
    at += sorted.len() * table(1);
    let data_entries: Vec<usize> = (0..sorted.len()).map(|index| at + index * 16).collect();
    at += sorted.len() * 16;
    let mut data_at = Vec::new();
    for (_, _, data) in &sorted {
        data_at.push(at);
        at = (at + data.len()).next_multiple_of(8);
    }
    let size = at;

    let mut section = vec![0u8; size];
    let put = |section: &mut Vec<u8>, at: usize, bytes: &[u8]| {
        section[at..at + bytes.len()].copy_from_slice(bytes);
    };
    let directory = |section: &mut Vec<u8>, at: usize, entries: usize| {
        put(section, at + 14, &(entries as u16).to_le_bytes());
    };
    let entry = |section: &mut Vec<u8>, at: usize, id: u16, target: usize, subdirectory: bool| {
        put(section, at, &u32::from(id).to_le_bytes());
        let flag = if subdirectory { 0x8000_0000 } else { 0 };
        put(section, at + 4, &(target as u32 | flag).to_le_bytes());
    };
    directory(&mut section, root, kinds.len());
    let mut resource = 0;
    for (index, &kind) in kinds.iter().enumerate() {
        entry(
            &mut section,
            root + table(0) + 8 * index,
            kind,
            type_tables[index],
            true,
        );
        directory(&mut section, type_tables[index], ids_of(kind));
        for slot in 0..ids_of(kind) {
            let (_, id, data) = sorted[resource];
            let at = type_tables[index] + table(0) + 8 * slot;
            entry(&mut section, at, *id, language_tables[resource], true);
            directory(&mut section, language_tables[resource], 1);
            let language = language_tables[resource] + table(0);
            entry(
                &mut section,
                language,
                LANGUAGE,
                data_entries[resource],
                false,
            );
            // The data's offset in the section, which the relocation makes an image address.
            put(
                &mut section,
                data_entries[resource],
                &(data_at[resource] as u32).to_le_bytes(),
            );
            put(
                &mut section,
                data_entries[resource] + 4,
                &(data.len() as u32).to_le_bytes(),
            );
            put(&mut section, data_at[resource], data);
            resource += 1;
        }
    }

    coff_file(&section, &data_entries, machine, relocation)
}

/// A COFF object of `section` as `.rsrc`, with a relocation at each of `data_entries` (the
/// offset each data entry's address is at) against the section's symbol.
fn coff_file(section: &[u8], data_entries: &[usize], machine: u16, relocation: u16) -> Vec<u8> {
    let size = section.len();
    let relocations = data_entries.len();
    let raw = 20 + 40;
    let symbols = raw + size + 10 * relocations;
    let mut out = Vec::with_capacity(symbols + 2 * 18 + 4);
    // File header: machine, one section, no time stamp (reproducible), the symbol table.
    out.extend_from_slice(&machine.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(symbols as u32).to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // no optional header, no characteristics
    // Section header.
    out.extend_from_slice(b".rsrc\0\0\0");
    out.extend_from_slice(&[0; 8]); // virtual size and address
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&(raw as u32).to_le_bytes());
    out.extend_from_slice(&((raw + size) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]); // line numbers
    out.extend_from_slice(&(relocations as u16).to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    // Initialized data, 4-byte aligned, readable, writable.
    out.extend_from_slice(&0xC030_0040u32.to_le_bytes());
    out.extend_from_slice(section);
    // Each data entry's first field, against the section's symbol (0).
    for &data_entry in data_entries {
        out.extend_from_slice(&(data_entry as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&relocation.to_le_bytes());
    }
    // The section's symbol, static, with its auxiliary record.
    out.extend_from_slice(b".rsrc\0\0\0");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&[0, 0, 3, 1]);
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&(relocations as u16).to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    // An empty string table: its size, which counts itself.
    out.extend_from_slice(&4u32.to_le_bytes());
    out
}
