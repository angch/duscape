//! Moving a file to the Trash, the freedesktop.org way: `gio trash` if it is installed (it knows
//! every desktop's rules), else the specification by hand — the home trash for files on the home
//! filesystem, `.Trash-<uid>` at the top of any other, each with a `.trashinfo` naming where the
//! file came from so it can be put back.

use ::std::ffi::OsString;
use ::std::io::Write;
use ::std::os::unix::fs::MetadataExt;
use ::std::path::{Path, PathBuf};
use ::std::process::Command;
use ::std::time::{SystemTime, UNIX_EPOCH};
use rustix::fd::OwnedFd;
use rustix::fs::{self as at, AtFlags, Mode, OFlags};

/// Move `path` to the Trash — a link itself, never what it points to.
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
pub fn trash(path: &Path) -> Result<(), String> {
    if let Ok(status) = Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path)
        .status()
        && status.success()
    {
        return Ok(());
    }
    by_hand(path)
}

fn by_hand(path: &Path) -> Result<(), String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        ::std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let meta = path.symlink_metadata().map_err(|error| error.to_string())?;
    let (trash_dir, recorded) = trash_dir_for(&path, meta.dev())?;
    move_to_trash(&path, &trash_dir, &recorded).map_err(|error| error.to_string())
}

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

// Inspect the opened directory, not a path checked before opening it. All subsequent
// writes are relative to these handles, even if a parent is renamed while we work.
fn private_dir(parent: &OwnedFd, name: &std::ffi::OsStr) -> std::io::Result<OwnedFd> {
    match at::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let fd = at::openat(parent, name, DIRECTORY, Mode::empty())?;
    let meta = at::fstat(&fd)?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if meta.st_uid != uid || meta.st_mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "trash directory must be owned by this user with mode 0700",
        ));
    }
    Ok(fd)
}

fn move_to_trash(path: &Path, trash_dir: &Path, recorded: &Path) -> std::io::Result<()> {
    let parent = trash_dir
        .parent()
        .ok_or_else(|| std::io::Error::other("trash has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let parent = at::open(parent, DIRECTORY, Mode::empty())?;
    let trash = private_dir(
        &parent,
        trash_dir
            .file_name()
            .ok_or_else(|| std::io::Error::other("trash has no name"))?,
    )?;
    let files = private_dir(&trash, std::ffi::OsStr::new("files"))?;
    let info = private_dir(&trash, std::ffi::OsStr::new("info"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("a path with no name"))?;
    let source = at::open(
        path.parent().unwrap_or(Path::new(".")),
        DIRECTORY,
        Mode::empty(),
    )?;
    let contents = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(recorded.as_os_str().as_encoded_bytes()),
        iso8601(SystemTime::now())
    );
    for attempt in 0..10_000u32 {
        let mut candidate = OsString::from(name);
        if attempt > 0 {
            candidate.push(format!(".{attempt}"));
        }
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        let fd = match at::openat(
            &info,
            &info_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        };
        let mut metadata = std::fs::File::from(fd);
        let result = metadata
            .write_all(contents.as_bytes())
            .and_then(|()| move_exclusive(&source, name, &files, &candidate));
        if result.is_ok() {
            return Ok(());
        }
        // Only our exclusively created reservation is removed on failure.
        at::unlinkat(&info, &info_name, AtFlags::empty())?;
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            other => return other,
        }
    }
    Err(std::io::Error::other(
        "the Trash already holds too many files of that name",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn move_exclusive(
    source: &OwnedFd,
    name: &std::ffi::OsStr,
    files: &OwnedFd,
    candidate: &std::ffi::OsStr,
) -> std::io::Result<()> {
    at::renameat_with(source, name, files, candidate, at::RenameFlags::NOREPLACE)?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn move_exclusive(
    _source: &OwnedFd,
    _name: &std::ffi::OsStr,
    _files: &OwnedFd,
    _candidate: &std::ffi::OsStr,
) -> std::io::Result<()> {
    // A plain rename overwrites another trashed file. Without an atomic exclusive rename,
    // leave the source intact and require the desktop's trash implementation.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe trash fallback unavailable; install gio",
    ))
}

/// The trash directory for a file on device `dev`, and the path to record for it: absolute in the
/// home trash, relative to the top directory in a `.Trash-<uid>`.
fn trash_dir_for(path: &Path, dev: u64) -> Result<(PathBuf, PathBuf), String> {
    let home_trash = match ::std::env::var_os("XDG_DATA_HOME") {
        Some(data) if !data.is_empty() => PathBuf::from(data).join("Trash"),
        _ => {
            let home = ::std::env::var_os("HOME").ok_or("HOME is not set")?;
            PathBuf::from(home).join(".local/share/Trash")
        }
    };
    // The trash may not exist yet: the device is that of its nearest existing ancestor.
    let home_dev = home_trash
        .ancestors()
        .skip(1)
        .find_map(|ancestor| ancestor.metadata().ok())
        .map(|meta| meta.dev());
    if home_dev == Some(dev) {
        return Ok((home_trash, path.to_path_buf()));
    }
    // Another filesystem: its top directory, found by walking up while the device stays the same.
    let mut top = path.parent().unwrap_or(path).to_path_buf();
    while let Some(parent) = top.parent() {
        match parent.metadata() {
            Ok(meta) if meta.dev() == dev => top = parent.to_path_buf(),
            _ => break,
        }
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = top.join(format!(".Trash-{uid}"));
    let relative = path.strip_prefix(&top).unwrap_or(path).to_path_buf();
    Ok((dir, relative))
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `YYYY-MM-DDThh:mm:ss`, in UTC: the specification wants local time, but a reader takes either
/// and nothing here knows the zone.
fn iso8601(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_iso8601() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00");
        // 2026-09-24 12:34:56 UTC.
        let time = UNIX_EPOCH + ::std::time::Duration::from_secs(1_790_253_296);
        assert_eq!(iso8601(time), "2026-09-24T12:34:56");
    }

    #[test]
    fn paths_are_percent_encoded_but_slashes_kept() {
        assert_eq!(
            percent_encode(b"/home/me/a b%c.txt"),
            "/home/me/a%20b%25c.txt"
        );
    }

    #[test]
    fn trashing_by_hand_moves_the_file_and_writes_its_info() {
        let root =
            ::std::env::temp_dir().join(format!("duscape-linux-trash-{}", ::std::process::id()));
        let _ = ::std::fs::remove_dir_all(&root);
        ::std::fs::create_dir_all(&root).unwrap();
        let file = root.join("gone.txt");
        ::std::fs::write(&file, "bye").unwrap();
        let meta = file.metadata().unwrap();
        // Route the home trash into the temp dir, so the test never touches the real one.
        let previous = ::std::env::var_os("XDG_DATA_HOME");
        // SAFETY: the test binary's other tests do not read this variable.
        unsafe { ::std::env::set_var("XDG_DATA_HOME", root.join("data")) };
        let result = trash_dir_for(&file, meta.dev());
        let outcome = by_hand(&file);
        match previous {
            // SAFETY: as above.
            Some(value) => unsafe { ::std::env::set_var("XDG_DATA_HOME", value) },
            None => unsafe { ::std::env::remove_var("XDG_DATA_HOME") },
        }
        let (dir, recorded) = result.unwrap();
        assert_eq!(
            dir,
            root.join("data/Trash"),
            "the file is on the home trash's device"
        );
        assert_eq!(recorded, file);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            assert!(
                outcome
                    .unwrap_err()
                    .contains("safe trash fallback unavailable")
            );
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "bye");
            std::fs::remove_dir_all(root).unwrap();
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        verify_moved(outcome, &file, &dir, &root);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn verify_moved(outcome: Result<(), String>, file: &Path, dir: &Path, root: &Path) {
        outcome.unwrap();
        assert!(!file.exists());
        assert!(dir.join("files/gone.txt").is_file());
        let info = ::std::fs::read_to_string(dir.join("info/gone.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
        assert!(info.contains("DeletionDate="), "{info}");
        ::std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod collision_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn setup(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("duscape-trash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Trash/files")).unwrap();
        std::fs::create_dir_all(root.join("Trash/info")).unwrap();
        for dir in ["Trash", "Trash/files", "Trash/info"] {
            std::fs::set_permissions(root.join(dir), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        root
    }

    #[test]
    fn dangling_links_and_existing_payloads_are_not_overwritten() {
        let root = setup("links");
        let trash = root.join("Trash");
        symlink(root.join("outside"), trash.join("info/item.trashinfo")).unwrap();
        symlink(root.join("missing"), trash.join("files/item.1")).unwrap();
        std::fs::write(trash.join("files/item.2"), "old").unwrap();
        let source = root.join("item");
        std::fs::write(&source, "new").unwrap();
        move_to_trash(&source, &trash, &source).unwrap();
        assert!(!root.join("outside").exists());
        assert!(
            trash
                .join("info/item.trashinfo")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert!(
            trash
                .join("files/item.1")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(trash.join("files/item.2")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(trash.join("files/item.3")).unwrap(),
            "new"
        );
        assert!(!trash.join("info/item.1.trashinfo").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn simultaneous_same_names_keep_both_files_and_metadata() {
        let root = setup("concurrent");
        for dir in ["a", "b"] {
            std::fs::create_dir(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("item"), dir).unwrap();
        }
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for dir in ["a", "b"] {
                let root = &root;
                let barrier = &barrier;
                scope.spawn(move || {
                    let source = root.join(dir).join("item");
                    barrier.wait();
                    move_to_trash(&source, &root.join("Trash"), &source).unwrap();
                });
            }
        });
        let mut contents = Vec::new();
        for name in ["item", "item.1"] {
            let text = std::fs::read_to_string(root.join("Trash/files").join(name)).unwrap();
            let info =
                std::fs::read_to_string(root.join("Trash/info").join(format!("{name}.trashinfo")))
                    .unwrap();
            assert!(info.contains(&format!("/{text}/item\n")), "{info}");
            contents.push(text);
        }
        contents.sort();
        assert_eq!(contents, ["a", "b"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlinked_or_public_trash_directories_are_refused() {
        let root = setup("directories");
        let source = root.join("item");
        std::fs::write(&source, "keep").unwrap();
        std::fs::rename(root.join("Trash/info"), root.join("elsewhere")).unwrap();
        symlink(root.join("elsewhere"), root.join("Trash/info")).unwrap();
        assert!(move_to_trash(&source, &root.join("Trash"), &source).is_err());
        std::fs::remove_file(root.join("Trash/info")).unwrap();
        std::fs::rename(root.join("elsewhere"), root.join("Trash/info")).unwrap();
        std::fs::set_permissions(root.join("Trash"), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(move_to_trash(&source, &root.join("Trash"), &source).is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "keep");
        std::fs::remove_dir_all(root).unwrap();
    }
}
