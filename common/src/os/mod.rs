#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{
    is_user_admin, link_count, set_sparse, size_on_disk_fast, volume_free, volume_id, volume_used,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{
    FileIdentity, enable_backup_privilege, file_identity, is_user_admin, link_count, set_sparse,
    size_on_disk_fast, volume_free, volume_id, volume_used,
};

#[cfg(target_os = "macos")]
mod purgeable;
#[cfg(target_os = "macos")]
pub use purgeable::volume_purgeable;

/// Elsewhere nothing says what a volume could free.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn volume_purgeable(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests;

mod volumes;
pub use volumes::{Volume, volumes};

use ::std::path::{Component, Path, PathBuf};

/// The scan root as every scan and tree name it: canonical (or as given, when it cannot be
/// resolved), and a share's root with the separator after its prefix ([`scan_root`]).
#[must_use]
pub fn canonical_root(path: &Path) -> PathBuf {
    scan_root(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

/// `path` with a separator after its prefix, where it is a prefix alone.
///
/// On Windows, `canonicalize` gives the root of a network share (`T:` mapped to
/// `\\server\share`) as `\\?\UNC\server\share`, a prefix and nothing after it — unlike a
/// drive's, `\\?\C:\`. A path below it, `\\?\UNC\server\share\sub`, has a root component
/// after the prefix, so stripped of that root it is `\sub`, and `\` was a folder of the
/// tree holding everything while every real folder in it was empty. With the separator the
/// share's root is a path like a drive's, and what is under it strips to `sub`.
#[must_use]
pub fn scan_root(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    if matches!(components.next(), Some(Component::Prefix(_))) && components.next().is_none() {
        let mut with_root = path.into_os_string();
        with_root.push(::std::path::MAIN_SEPARATOR_STR);
        return PathBuf::from(with_root);
    }
    path
}
