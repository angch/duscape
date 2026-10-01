//! Keeping the saved scans to what can still be used, in a bounded room.
//!
//! A saved scan is replaced only when the same folder is scanned again with the same options,
//! so without this the directory only grew: a format left behind by an update, a folder scanned
//! once from a temporary directory, a recording cut short by a crash. After every save the
//! recorder's thread sweeps the directory ([`sweep`]): what can never be read again goes, and
//! the rest is kept within [`ROOM`], the least recently saved first out.
//!
//! What is *not* a reason to remove one, and why — so neither is tried:
//! - **Age.** A saved scan pays most on a slow disk scanned rarely (an archive drive, a
//!   spinning RAID, scanned every few months), and whether one is past use is the catch-up's to
//!   say: it walks afresh when the log names more than a quarter of the folders
//!   ([`super::too_stale`]) or can no longer be replayed.
//! - **The folder being gone.** An unplugged drive's folder is gone until it is plugged back in.
//!   The room bounds what such files can cost.

use std::fs;
use std::path::{Path, PathBuf};

use super::{MAGIC, Stamp, VERSION, decode_clear};

/// The most the saved scans may take together, in bytes. A whole terabyte disk's file is
/// 10–60 MB (61 MB for this Mac's `/`, its 830k APFS clones kept one by one), so this holds a
/// few whole-disk scans and many folders'; and `~/Library/Caches`, where they live on macOS,
/// is the system's to empty when space runs short anyway.
pub const ROOM: u64 = 256 << 20;

/// How long a recording's part file may stand where whether its process lives cannot be asked
/// (not Unix): a recording takes the walk's time, minutes at most.
#[cfg(not(unix))]
const PART_FILE_LIFE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// A file the sweep removed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub path: PathBuf,
    pub bytes: u64,
    pub why: &'static str,
}

/// Remove from `dir` what cannot be used again, and then the least recently saved scans while
/// they take more than `room`; never `keep` (the file just saved) nor what another build of
/// duscape wrote (a newer format, or a recording another process is making). `now` is the
/// stamp just saved: its system, and its device's log.
pub fn sweep(dir: &Path, now: &Stamp, keep: &str, room: u64) -> Vec<Removed> {
    let mut removed = Vec::new();
    let mut kept: Vec<(u64, u64, PathBuf)> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let bytes = meta.len();
        if name.ends_with(".part") {
            if part_is_abandoned(name, &meta) {
                remove(&path, bytes, "a recording cut short", &mut removed);
            }
            continue;
        }
        if !name.ends_with(".scan") || name == keep {
            if name == keep {
                kept.push((u64::MAX, bytes, path));
            }
            continue;
        }
        match judge(&path, now) {
            Judged::Dead(why) => remove(&path, bytes, why, &mut removed),
            Judged::Foreign => {}
            Judged::Live(saved_at) => kept.push((saved_at, bytes, path)),
        }
    }
    // The room: the least recently saved go first; `keep` sorts last and is never taken.
    kept.sort_by_key(|(saved_at, _, _)| *saved_at);
    let mut total: u64 = kept.iter().map(|(_, bytes, _)| bytes).sum();
    for (saved_at, bytes, path) in kept {
        if total <= room || saved_at == u64::MAX {
            break;
        }
        total -= bytes;
        remove(
            &path,
            bytes,
            "the least recently saved, past the room",
            &mut removed,
        );
    }
    removed
}

/// Remove every saved scan in `dir`, and every recording cut short: `duscape --clear-cache`.
/// A recording under way is left to finish.
pub fn clear(dir: &Path) -> Vec<Removed> {
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if name.ends_with(".scan") {
            remove(&path, meta.len(), "cleared", &mut removed);
        } else if name.ends_with(".part") && part_is_abandoned(name, &meta) {
            remove(&path, meta.len(), "a recording cut short", &mut removed);
        }
    }
    removed
}

fn remove(path: &Path, bytes: u64, why: &'static str, removed: &mut Vec<Removed>) {
    if fs::remove_file(path).is_ok() {
        removed.push(Removed {
            path: path.to_path_buf(),
            bytes,
            why,
        });
    }
}

enum Judged {
    /// Never readable again, for this reason.
    Dead(&'static str),
    /// Another build's: a newer format, left alone.
    Foreign,
    /// Usable, saved at this time.
    Live(u64),
}

/// What a saved scan's clear header says about whether it can be used again.
fn judge(path: &Path, now: &Stamp) -> Judged {
    let mut head = vec![0u8; 4096];
    let read = fs::File::open(path).and_then(|mut file| io_read(&mut file, &mut head));
    let Ok(read) = read else {
        return Judged::Foreign;
    };
    head.truncate(read);
    let at = MAGIC.len() + 4;
    if head.len() < at || &head[..MAGIC.len()] != MAGIC {
        return Judged::Dead("not a saved scan");
    }
    let version = u32::from_le_bytes(head[MAGIC.len()..at].try_into().unwrap_or_default());
    if version > VERSION {
        return Judged::Foreign;
    }
    if version < VERSION {
        return Judged::Dead("an older format, which this build cannot read");
    }
    let Some((_, stamp, _)) = decode_clear(&head) else {
        return Judged::Dead("a damaged header");
    };
    // An OS update rewrites the sealed system volume with no events: every saved scan made
    // before it is walked afresh (`stamp_holds`), so none can be used again.
    if stamp.system != now.system {
        return Judged::Dead("saved under another system version");
    }
    // The same volume with another log: it was reset, and nothing since the stamp is known.
    // Only the device just saved is asked, the one whose log the stamp in hand names.
    if stamp.device == now.device && stamp.log_uuid != now.log_uuid {
        return Judged::Dead("its volume's change log was reset since");
    }
    Judged::Live(stamp.saved_at)
}

fn io_read(file: &mut fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    std::io::Read::read(file, buffer)
}

/// Whether a recording's part file (`<key>.scan.<pid>.part`) belongs to no live process. A
/// dead process's pid taken by a live one keeps its part file until that one ends too: the
/// cost of asking by pid, which needs no lock file.
#[cfg(unix)]
fn part_is_abandoned(name: &str, _meta: &fs::Metadata) -> bool {
    let Some(pid) = name
        .strip_suffix(".part")
        .and_then(|rest| rest.rsplit('.').next())
        .and_then(|pid| pid.parse::<libc::pid_t>().ok())
    else {
        return false;
    };
    if pid == std::process::id() as libc::pid_t {
        // This process's own: a recording of another folder may be under way.
        return false;
    }
    // SAFETY: signal 0 sends nothing; it asks whether the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    !alive
}

#[cfg(not(unix))]
fn part_is_abandoned(_name: &str, meta: &fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > PART_FILE_LIFE)
}
