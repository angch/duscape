//! Deleting what a viewer shows: from disk, and never NTFS's own files.
//!
//! A viewer confirms first, then calls [`remove`] for each [`FileToDelete`], and takes each one
//! that succeeded off its tree with [`crate::FileTree::delete_file`], so the space freed is only
//! what actually left the disk. A window calls [`remove_counting`] on a thread of its own
//! instead, and shows the [`Tally`] it keeps: a folder of thousands of files takes seconds.

use ::std::fs;
use ::std::io;
use ::std::path::{Path, PathBuf};
use ::std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
    if let Some(file) = files.iter().find(|file| in_snapshot(file)) {
        return Some(format!(
            "{} is in a local snapshot, which is read-only: its space is freed by deleting the \
             snapshot (tmutil deletelocalsnapshots)",
            name(file)
        ));
    }
    // An archive's entry is no file on disk: the archive is one file, deleted whole or changed
    // with an archive tool.
    files.iter().find_map(|file| {
        let (archive, _) = crate::archive::member_of(&file.full_path())?;
        Some(format!(
            "{} is inside {}, an archive: delete the archive, or change it with an archive tool",
            name(file),
            crate::archive::display_name(&archive)
        ))
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

/// What a delete under way has done, for whoever shows it, and a way to stop it: shared between
/// the thread removing and the window drawing, so the removal never waits on the window.
#[derive(Debug, Default)]
pub struct Tally {
    removed: AtomicU64,
    stop: AtomicBool,
}

impl Tally {
    /// Entries removed so far: files, links and folders, each one, as a folder's
    /// `num_descendants` counts them (and the folder itself).
    #[must_use]
    pub fn removed(&self) -> u64 {
        self.removed.load(Ordering::Relaxed)
    }

    /// Count `entries` removed by other means — moved to a Trash whole, say.
    pub fn add(&self, entries: u64) {
        self.removed.fetch_add(entries, Ordering::Relaxed);
    }

    /// Ask the removal to stop, after the entry it is on.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// Remove `file` from disk: a file, or a folder and everything in it. A symbolic link or junction
/// is removed itself, never what it points to. What [`refused`] refuses is refused here too.
pub fn remove(file: &FileToDelete) -> io::Result<()> {
    remove_counting(file, &Tally::default())
}

/// [`remove`], counting each entry in `tally` as it goes and stopping, with
/// [`io::ErrorKind::Interrupted`], once [`Tally::stop`] is called. Stopped or failed part way, a
/// folder keeps what was not reached yet.
pub fn remove_counting(file: &FileToDelete, tally: &Tally) -> io::Result<()> {
    if let Some(why) = refused(::std::slice::from_ref(file)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
    }
    let path = file.full_path();
    let file_type = fs::symlink_metadata(&path)?.file_type();
    if file_type.is_dir() {
        remove_tree(path, tally)
    } else {
        remove_entry(&path, file_type)?;
        tally.add(1);
        Ok(())
    }
}

/// Everything in the folder at `root`, and then the folder. A walk of our own, not
/// `fs::remove_dir_all`, which says nothing until it is done: deleting a package cache's
/// thousands of files froze the window with no word of how far it had got. Depth first with a
/// stack, not recursion, so a deep tree cannot overflow the thread's stack; each folder is
/// listed once, its subfolders pushed above it, and removed when it comes back round. The
/// first failure ends it, as `remove_dir_all`'s did. As fast as `remove_dir_all`, measured on
/// Windows (2026-10-05, NTFS, 20 folders of 1000 files: 2.9 s against 3.0 s).
fn remove_tree(root: PathBuf, tally: &Tally) -> io::Result<()> {
    let stopped = || io::Error::new(io::ErrorKind::Interrupted, "stopped");
    let mut stack = vec![(root, false)];
    while let Some((folder, listed)) = stack.pop() {
        if tally.stopped() {
            return Err(stopped());
        }
        if listed {
            remove_folder(&folder)?;
            tally.add(1);
            continue;
        }
        stack.push((folder.clone(), true));
        for entry in fs::read_dir(&folder)? {
            if tally.stopped() {
                return Err(stopped());
            }
            let entry = entry?;
            // Not followed: a link or junction is an entry of its own, removed itself.
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push((entry.path(), false));
            } else {
                remove_entry(&entry.path(), file_type)?;
                tally.add(1);
            }
        }
    }
    Ok(())
}

/// One entry that is no folder to walk: a file, or a link of any kind.
fn remove_entry(path: &Path, file_type: fs::FileType) -> io::Result<()> {
    if is_directory_link(file_type) {
        // A junction or directory symlink is not a directory to `symlink_metadata`, but Windows
        // will only remove it as one. `remove_dir` takes the link and never follows it.
        writable_and_again(path, |path| fs::remove_dir(path))
    } else {
        writable_and_again(path, |path| fs::remove_file(path))
    }
}

/// An emptied folder. On Windows a folder whose last file was just deleted can still say it is
/// not empty for a moment, while something that had the file open (an indexer, an antivirus
/// scanner) lets go of it; `remove_dir_all` tries again for that, so this does too, briefly.
fn remove_folder(path: &Path) -> io::Result<()> {
    const TRIES: u32 = 5;
    let mut tries = 0;
    loop {
        match writable_and_again(path, |path| fs::remove_dir(path)) {
            Err(error)
                if cfg!(windows)
                    && error.kind() == io::ErrorKind::DirectoryNotEmpty
                    && tries < TRIES =>
            {
                tries += 1;
                ::std::thread::sleep(::std::time::Duration::from_millis(5 << tries));
            }
            done => return done,
        }
    }
}

/// `remove(path)`, and if Windows refuses it for being read-only, deleted ignoring the
/// attribute. Git's objects and package caches hold read-only files, and `remove_dir_all`
/// deletes them regardless, so a delete of ours must too. Elsewhere the attribute is no bar:
/// removing an entry needs its folder writable, not it.
fn writable_and_again(path: &Path, remove: impl Fn(&Path) -> io::Result<()>) -> io::Result<()> {
    match remove(path) {
        Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return Err(error);
            };
            if !metadata.permissions().readonly() {
                return Err(error);
            }
            // The fallback's own error — "not empty yet", for a read-only folder, is what
            // `remove_folder` retries — unless it is the same refusal again.
            read_only_removed(path, metadata.permissions(), remove).map_err(|fallback| {
                if fallback.kind() == io::ErrorKind::PermissionDenied {
                    error
                } else {
                    fallback
                }
            })
        }
        done => done,
    }
}

/// A read-only entry deleted as `remove_dir_all` deletes one: asking Windows to ignore the
/// attribute (`FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE`), which changes nothing on the
/// file. Clearing the attribute first, as this did until 2026-10-05, cleared it on every hard
/// link of the file — a `git clone --local`'s objects are links of the original's — and left it
/// cleared where the delete then failed. Where the flag is refused (FAT, which has no hard
/// links, or a Windows before 10 1809), the attribute is cleared, and put back on a failure.
#[cfg(windows)]
fn read_only_removed(
    path: &Path,
    permissions: fs::Permissions,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if ignoring_read_only(path).is_ok() {
        return Ok(());
    }
    let mut writable = permissions.clone();
    // Windows alone, where it clears the read-only attribute and nothing else.
    #[allow(clippy::permissions_set_readonly_false)]
    writable.set_readonly(false);
    fs::set_permissions(path, writable)?;
    remove(path).inspect_err(|_| {
        let _ = fs::set_permissions(path, permissions);
    })
}

#[cfg(not(windows))]
fn read_only_removed(
    _path: &Path,
    _permissions: fs::Permissions,
    _remove: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::PermissionDenied))
}

/// Delete `path` — a file, a link, or an empty folder — with POSIX semantics, the read-only
/// attribute ignored: through a handle opened for delete, never following a reparse point.
#[cfg(windows)]
fn ignoring_read_only(path: &Path) -> io::Result<()> {
    use ::std::os::windows::fs::OpenOptionsExt;
    use ::std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let handle = fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the handle is open for delete for the call; `info` is the struct the class names,
    // alive for the call, and its size is passed.
    let done = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const info).cast(),
            ::std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if done == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
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

    use super::{Tally, refused, remove, remove_counting};
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

    /// Every entry is counted as it goes, the folder too — what a folder's `num_descendants`
    /// and itself make — and a read-only file is no bar, as it was none to `remove_dir_all`.
    #[test]
    fn a_folder_is_removed_counting_each_entry_read_only_ones_too() {
        let dir = ::std::env::temp_dir().join("duscape_delete_counting_test");
        let _ = fs::remove_dir_all(&dir);
        let inner = dir.join("folder").join("inner");
        fs::create_dir_all(inner.join("deeper")).expect("create folders");
        for name in ["a", "b", "c"] {
            fs::write(inner.join(name), b"x").expect("write");
        }
        let locked = inner.join("deeper").join("locked");
        fs::write(&locked, b"x").expect("write");
        let mut permissions = fs::metadata(&locked).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&locked, permissions).expect("read-only");

        let tally = Tally::default();
        remove_counting(
            &to_delete(dir.clone(), &["folder"], FileType::Folder),
            &tally,
        )
        .expect("removed");
        assert!(!dir.join("folder").exists());
        // folder, inner, deeper, a, b, c, locked.
        assert_eq!(tally.removed(), 7);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A read-only file deleted from one folder stays read-only where it is hard-linked from
    /// another: the attribute belongs to the file, and is never cleared to delete it.
    #[test]
    fn a_read_only_files_other_links_stay_read_only() {
        let dir = ::std::env::temp_dir().join("duscape_delete_link_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("clone")).expect("create");
        fs::create_dir_all(dir.join("original")).expect("create");
        let original = dir.join("original").join("object");
        fs::write(&original, b"x").expect("write");
        let mut permissions = fs::metadata(&original).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&original, permissions).expect("read-only");
        if fs::hard_link(&original, dir.join("clone").join("object")).is_err() {
            // A filesystem with no hard links has nothing to show here.
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        remove(&to_delete(dir.clone(), &["clone"], FileType::Folder)).expect("removed");
        assert!(!dir.join("clone").exists());
        assert!(
            fs::metadata(&original)
                .expect("still there")
                .permissions()
                .readonly(),
            "the original's attribute untouched"
        );
        let mut permissions = fs::metadata(&original).expect("metadata").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(&original, permissions);
        let _ = fs::remove_dir_all(&dir);
    }

    /// What is inside an archive is no file on disk: refused, before anything is touched; the
    /// archive itself is deleted like any file.
    #[test]
    fn what_is_inside_an_archive_is_refused_and_the_archive_is_not() {
        let dir = ::std::env::temp_dir().join("duscape_delete_archive_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create");
        fs::write(
            dir.join("lib.jar"),
            crate::archive::stored_zip(&[("a/b.class", b"x")]),
        )
        .expect("write");
        let inside = to_delete(dir.clone(), &["lib.jar", "a", "b.class"], FileType::File);
        let why = refused(::std::slice::from_ref(&inside)).expect("refused");
        assert!(why.starts_with("b.class is inside lib.jar"), "{why}");
        assert!(remove(&inside).is_err());
        let archive = to_delete(dir.clone(), &["lib.jar"], FileType::Folder);
        assert_eq!(refused(::std::slice::from_ref(&archive)), None);
        remove(&archive).expect("the archive deleted");
        assert!(!dir.join("lib.jar").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Stopped, it ends at once and says so, leaving what it had not reached.
    #[test]
    fn a_stopped_removal_leaves_the_rest() {
        let dir = ::std::env::temp_dir().join("duscape_delete_stop_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("folder")).expect("create");
        fs::write(dir.join("folder").join("kept"), b"x").expect("write");
        let tally = Tally::default();
        tally.stop();
        let error = remove_counting(
            &to_delete(dir.clone(), &["folder"], FileType::Folder),
            &tally,
        )
        .expect_err("stopped");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(dir.join("folder").join("kept").exists());
        assert_eq!(tally.removed(), 0);
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

        // Inside a folder being removed, the walk takes the junction and does not go in.
        fs::create_dir_all(dir.join("holder")).expect("create holder");
        let made = ::std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("holder").join("link"))
            .arg(dir.join("target"))
            .output()
            .is_ok_and(|output| output.status.success());
        assert!(made, "mklink /J needs no privilege");
        remove(&to_delete(dir.clone(), &["holder"], FileType::Folder)).expect("remove holder");
        assert!(!dir.join("holder").exists());
        assert!(dir.join("target").join("kept").exists(), "not walked into");
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
