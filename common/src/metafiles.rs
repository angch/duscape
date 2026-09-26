//! NTFS's metadata files, by name: what an elevated Windows scan shows at a volume's root, and what
//! no viewer may offer to delete.
//!
//! The names are here rather than with the NTFS record parser in `duscape-scan` because two
//! things need them: the Windows walker, which sizes these files, and [`crate::delete`], which
//! refuses them — and a viewer that deletes should not need a scanner to know what it must not.

/// The metadata files in a volume's root, by the record numbers NTFS gives them.
///
/// Record 5 is the root directory itself, and 11 is `$Extend`, whose children are found by
/// listing it.
pub const ROOT_METAFILES: &[(u64, &str)] = &[
    (0, "$MFT"),
    (1, "$MFTMirr"),
    (2, "$LogFile"),
    (3, "$Volume"),
    (4, "$AttrDef"),
    (6, "$Bitmap"),
    (7, "$Boot"),
    (8, "$BadClus"),
    (9, "$Secure"),
    (10, "$UpCase"),
];

/// The directory holding the later metadata files: `$UsnJrnl`, `$ObjId`, `$Quota`, `$Reparse`,
/// `$RmMetadata`.
pub const EXTEND: &str = "$Extend";

/// Whether `name`, in a volume's root, is one of NTFS's metadata files. The names are reserved
/// there, so nothing else can have them.
#[must_use]
pub fn is_root_metafile(name: &str) -> bool {
    name == EXTEND || ROOT_METAFILES.iter().any(|(_, known)| *known == name)
}

/// Whether `path_to_file`, below the scan root `root`, is one of the metadata entries the Windows
/// walker adds — which the filesystem would refuse to delete, and which must not be offered.
#[must_use]
pub fn is_metafile_path(root: &::std::path::Path, path_to_file: &[::std::ffi::OsString]) -> bool {
    cfg!(windows)
        && root.parent().is_none()
        && path_to_file
            .first()
            .and_then(|name| name.to_str())
            .is_some_and(is_root_metafile)
}
