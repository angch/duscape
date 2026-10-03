// The Windows binaries' own resources — the icon Explorer shows for the `.exe`, the version
// (Explorer's Details tab, Task Manager's name for the process, what an installer reads as the
// file's version), and on GNU targets the manifest beside them — written here, not by a
// resource compiler, which the zig cross-build of the release does not have. Included by the build scripts of `duscape-windows`
// and of `duscape` (`include!`), so both binaries carry the same icon and version.
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
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// en-US, what `rc` and `embed-manifest` give when nothing is said.
const LANGUAGE: u16 = 1033;

/// A resource: its type, its id, and its bytes.
type Resource = (u16, u16, Vec<u8>);

/// Link `icon` (a `.ico` file) into the binaries as their icon, the package's version as the
/// binary `name`'s (`name.exe`), and `manifest` on GNU targets.
fn embed_resources(icon: &Path, name: &str, manifest: Option<&Path>) {
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ico = fs::read(icon).unwrap_or_else(|error| panic!("{}: {error}", icon.display()));
    let mut resources = icon_resources(&ico);
    resources.push((RT_VERSION, 1, version_resource(&Version::of_package(name))));
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

/// What a binary's version resource says.
struct Version {
    /// Major, minor, patch, build: the numbers Windows and installers compare.
    numbers: [u16; 4],
    /// The version as written, pre-release and all.
    text: String,
    /// A pre-release (`0.3.0-rc.1`), which the fixed flags say.
    prerelease: bool,
    /// The binary's name, without `.exe`.
    name: String,
    /// The publisher: the package's first author, less the address.
    company: String,
}

impl Version {
    /// The version cargo gives the build script.
    fn of_package(name: &str) -> Self {
        let number = |part: &str| {
            env::var(format!("CARGO_PKG_VERSION_{part}"))
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0)
        };
        let authors = env::var("CARGO_PKG_AUTHORS").unwrap_or_default();
        let first = authors.split(':').next().unwrap_or_default();
        Self {
            numbers: [number("MAJOR"), number("MINOR"), number("PATCH"), 0],
            text: env::var("CARGO_PKG_VERSION").unwrap_or_default(),
            prerelease: env::var("CARGO_PKG_VERSION_PRE").is_ok_and(|pre| !pre.is_empty()),
            name: name.to_owned(),
            company: first.split('<').next().unwrap_or_default().trim().to_owned(),
        }
    }
}

/// The copyright lines of the repository's `LICENSE`, the installer's too (a test holds both
/// to the file).
const COPYRIGHT: &str = "Copyright (c) 2020 Aram Drevekenin; Copyright (c) 2026 Ang Chin Han and duscape contributors; MIT licence";

/// A `VS_VERSIONINFO`: the fixed numbers, then the strings (US English, Unicode) and the
/// translation that says which table of strings to read.
fn version_resource(version: &Version) -> Vec<u8> {
    let [major, minor, patch, build] = version.numbers.map(u32::from);
    let mut fixed = Vec::with_capacity(52);
    for field in [
        0xFEEF_04BD,            // signature
        0x0001_0000,            // structure version
        major << 16 | minor,    // file version
        patch << 16 | build,
        major << 16 | minor,    // product version
        patch << 16 | build,
        0x3F,                   // every flag is meaningful,
        // and only VS_FF_PRERELEASE is ever set: a debug build is not marked, since nothing
        // that reads the flags (Explorer shows none) would do otherwise for it
        if version.prerelease { 0x2 } else { 0 },
        0x0004_0004,            // VOS_NT_WINDOWS32
        1,                      // VFT_APP
        0,                      // no subtype
        0,                      // no date
        0,
    ] {
        fixed.extend_from_slice(&u32::to_le_bytes(field));
    }
    let file = format!("{}.exe", version.name);
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", version.company.as_str()),
        ("FileDescription", "duscape"),
        ("FileVersion", &version.text),
        ("InternalName", &version.name),
        ("LegalCopyright", COPYRIGHT),
        ("OriginalFilename", &file),
        ("ProductName", "duscape"),
        ("ProductVersion", &version.text),
    ]
    .into_iter()
    .map(|(key, value)| {
        let words = value.encode_utf16().count() + 1;
        version_node(key, &utf16z(value), words as u16, true, &[])
    })
    .collect();
    let table = version_node(&format!("{LANGUAGE:04X}04B0"), &[], 0, true, &strings);
    let string_info = version_node("StringFileInfo", &[], 0, true, &[table]);
    let mut translation = LANGUAGE.to_le_bytes().to_vec();
    translation.extend_from_slice(&1200u16.to_le_bytes()); // UTF-16
    let var = version_node("Translation", &translation, 4, false, &[]);
    let var_info = version_node("VarFileInfo", &[], 0, true, &[var]);
    version_node("VS_VERSION_INFO", &fixed, 52, false, &[string_info, var_info])
}

/// A node of a version resource: its length, its value's length (in characters for text, in
/// bytes otherwise), whether the value is text, its key, then the value and each child, each
/// on a four-byte boundary.
fn version_node(key: &str, value: &[u8], length: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0, 0];
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&u16::from(text).to_le_bytes());
    out.extend_from_slice(&utf16z(key));
    for part in ::std::iter::once(value).chain(children.iter().map(Vec::as_slice)) {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend_from_slice(part);
    }
    let total = out.len() as u16;
    out[..2].copy_from_slice(&total.to_le_bytes());
    out
}

/// `text` as UTF-16 with its terminating zero.
fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
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
