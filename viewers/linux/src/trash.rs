//! Moving a file to the Trash, the freedesktop.org way: `gio trash` if it is installed (it knows
//! every desktop's rules), else the specification by hand — the home trash for files on the home
//! filesystem, `.Trash-<uid>` at the top of any other, each with a `.trashinfo` naming where the
//! file came from so it can be put back.

use ::std::ffi::OsString;
use ::std::os::unix::fs::{MetadataExt, PermissionsExt};
use ::std::path::{Path, PathBuf};
use ::std::process::Command;
use ::std::time::{SystemTime, UNIX_EPOCH};

/// Move `path` to the Trash — a link itself, never what it points to.
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
    let files = trash_dir.join("files");
    let info = trash_dir.join("info");
    for dir in [&files, &info] {
        ::std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| "a path with no name".to_string())?;
    let (target, info_file) = free_names(&files, &info, name)?;
    let contents = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(recorded.as_os_str().as_encoded_bytes()),
        iso8601(SystemTime::now())
    );
    // The info file first, so a trash that is listed meanwhile never shows a file without one.
    ::std::fs::write(&info_file, contents).map_err(|error| error.to_string())?;
    if let Err(error) = ::std::fs::rename(&path, &target) {
        let _ = ::std::fs::remove_file(&info_file);
        return Err(format!("could not move to the Trash: {error}"));
    }
    Ok(())
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
    if !dir.is_dir() {
        ::std::fs::create_dir(&dir)
            .map_err(|error| format!("no trash on that filesystem ({}): {error}", top.display()))?;
        let _ = ::std::fs::set_permissions(&dir, ::std::fs::Permissions::from_mode(0o700));
    }
    let relative = path.strip_prefix(&top).unwrap_or(path).to_path_buf();
    Ok((dir, relative))
}

/// A name free in both `files/` and `info/`: the file's own, else with a number.
fn free_names(
    files: &Path,
    info: &Path,
    name: &::std::ffi::OsStr,
) -> Result<(PathBuf, PathBuf), String> {
    for attempt in 0..10_000u32 {
        let mut candidate = OsString::from(name);
        if attempt > 0 {
            candidate.push(format!(".{attempt}"));
        }
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        let (target, info_file) = (files.join(&candidate), info.join(&info_name));
        if !target.exists() && !info_file.exists() {
            return Ok((target, info_file));
        }
    }
    Err("the Trash already holds too many files of that name".to_string())
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
        outcome.unwrap();
        assert!(!file.exists());
        assert!(dir.join("files/gone.txt").is_file());
        let info = ::std::fs::read_to_string(dir.join("info/gone.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
        assert!(info.contains("DeletionDate="), "{info}");
        ::std::fs::remove_dir_all(&root).unwrap();
    }
}
