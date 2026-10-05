//! Deleting what a viewer shows: from disk, and never NTFS's own files.
//!
//! A viewer confirms first, then calls [`remove`] for each [`FileToDelete`], and takes each one
//! that succeeded off its tree with [`crate::FileTree::delete_file`], so the space freed is only
//! what actually left the disk.

use ::std::fs;
use ::std::io;
use ::std::path::Path;

use crate::FileToDelete;
use crate::metafiles::is_metafile_path;

/// Why `files` must not be deleted, if one of them must not, naming the first such: an NTFS
/// metadata file the Windows walker added at a volume's root, or anything in a volume's local
/// snapshots ([`crate::snapshots::FOLDER`]), which is no path on disk. The filesystem would
/// refuse the one and find nothing at the other, but a viewer should refuse the whole request
/// up front — deleting the rest of a selection and quietly skipping one would leave it unclear
/// what happened.
#[must_use]
pub fn refused(files: &[FileToDelete]) -> Option<String> {
    let name = |file: &FileToDelete| {
        file.path_to_file
            .last()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if let Some(file) = files
        .iter()
        .find(|file| is_metafile_path(&file.path_in_filesystem, &file.path_to_file))
    {
        return Some(format!(
            "NTFS metadata belongs to the filesystem and cannot be deleted: {}",
            name(file)
        ));
    }
    files.iter().find(|file| in_snapshot(file)).map(|file| {
        format!(
            "{} is in a local snapshot, which is read-only: its space is freed by deleting the \
             snapshot (tmutil deletelocalsnapshots)",
            name(file)
        )
    })
}

/// Whether `file` is in the snapshots' folder: under its name at the root, and not on disk (a
/// real folder of the name keeps the name, and the tree adds no snapshots there).
fn in_snapshot(file: &FileToDelete) -> bool {
    file.path_to_file
        .first()
        .is_some_and(|name| name == crate::snapshots::FOLDER)
        && fs::symlink_metadata(file.full_path()).is_err()
}

/// Remove `file` from disk: a file, or a folder and everything in it. A symbolic link or junction
/// is removed itself, never what it points to. What [`refused`] refuses is refused here too.
pub fn remove(file: &FileToDelete) -> io::Result<()> {
    if let Some(why) = refused(::std::slice::from_ref(file)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
    }
    remove_path(&file.full_path())
}

fn remove_path(path: &Path) -> io::Result<()> {
    let file_type = fs::symlink_metadata(path)?.file_type();
    if file_type.is_dir() {
        fs::remove_dir_all(path)
    } else if is_directory_link(file_type) {
        // A junction or directory symlink is not a directory to `symlink_metadata`, but Windows
        // will only remove it as one. `remove_dir` takes the link and never follows it.
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(windows)]
fn is_directory_link(file_type: fs::FileType) -> bool {
    use ::std::os::windows::fs::FileTypeExt;
    file_type.is_symlink_dir()
}

/// Elsewhere a symbolic link is removed like a file, whatever it points to.
#[cfg(not(windows))]
fn is_directory_link(_file_type: fs::FileType) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsString;
    use ::std::fs;
    use ::std::io;
    use ::std::path::PathBuf;

    use super::{refused, remove};
    use crate::FileToDelete;
    use crate::model::Sizes;
    use crate::tiles::FileType;

    fn to_delete(root: PathBuf, path: &[&str], file_type: FileType) -> FileToDelete {
        FileToDelete {
            path_in_filesystem: root,
            path_to_file: path.iter().map(OsString::from).collect(),
            file_type,
            num_descendants: None,
            size: 0,
            sizes: Sizes::ZERO,
        }
    }

    #[test]
    fn a_file_and_a_folder_are_removed() {
        let dir = ::std::env::temp_dir().join("duscape_delete_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("folder").join("inner")).expect("create folders");
        fs::write(dir.join("folder").join("inner").join("file"), b"x").expect("write");
        fs::write(dir.join("loose"), b"x").expect("write");

        remove(&to_delete(dir.clone(), &["loose"], FileType::File)).expect("remove a file");
        remove(&to_delete(dir.clone(), &["folder"], FileType::Folder)).expect("remove a folder");
        assert!(!dir.join("loose").exists());
        assert!(!dir.join("folder").exists());
        assert!(remove(&to_delete(dir.clone(), &["loose"], FileType::File)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A junction is removed itself: what it points to, and everything in that, stays.
    #[cfg(windows)]
    #[test]
    fn a_junction_is_removed_not_its_target() {
        let dir = ::std::env::temp_dir().join("duscape_delete_junction_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("target")).expect("create target");
        fs::write(dir.join("target").join("kept"), b"x").expect("write");
        let made = ::std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("link"))
            .arg(dir.join("target"))
            .output()
            .is_ok_and(|output| output.status.success());
        assert!(made, "mklink /J needs no privilege");

        remove(&to_delete(dir.clone(), &["link"], FileType::File)).expect("remove the junction");
        assert!(!dir.join("link").exists(), "the junction is gone");
        assert!(
            dir.join("target").join("kept").exists(),
            "its target is untouched"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Only a volume's root holds metadata files, and only on Windows are they added.
    #[test]
    fn ntfs_metadata_is_refused_at_a_volume_root_only() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        let files = [
            to_delete(root.clone(), &["Windows"], FileType::Folder),
            to_delete(root.clone(), &["$MFT"], FileType::File),
        ];
        let refusal = refused(&files);
        assert_eq!(refusal.is_some(), cfg!(windows));
        assert!(refusal.is_none_or(|why| why.ends_with(": $MFT")));
        let below = to_delete(root.join("Users"), &["$MFT"], FileType::File);
        assert_eq!(refused(&[below]), None, "not at a volume's root");
    }

    /// The snapshots' folder is no path on disk: what is in it is refused, before a viewer asks,
    /// and by `remove` itself. A real folder of the name is not.
    #[test]
    fn what_is_in_a_local_snapshot_is_refused_unless_the_folder_is_real() {
        let dir = ::std::env::temp_dir().join("duscape_delete_snapshot_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create");
        let snapshot = crate::snapshots::FOLDER;
        let inside = to_delete(
            dir.clone(),
            &[snapshot, "a snapshot", "old.mov"],
            FileType::File,
        );
        let why = refused(&[
            to_delete(dir.clone(), &["kept"], FileType::File),
            inside.clone(),
        ])
        .expect("refused");
        assert!(why.starts_with("old.mov is in a local snapshot"), "{why}");
        assert_eq!(
            remove(&inside).map_err(|error| error.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );

        fs::create_dir_all(dir.join(snapshot)).expect("a real folder of the name");
        let real = to_delete(dir.clone(), &[snapshot], FileType::Folder);
        assert_eq!(refused(::std::slice::from_ref(&real)), None);
        remove(&real).expect("removed");
        let _ = fs::remove_dir_all(&dir);
    }
}
