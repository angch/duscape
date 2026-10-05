//! A volume's local snapshots, shown as one folder at the volume's root ([`FOLDER`]) holding a
//! folder per snapshot: what each snapshot still holds that is gone from the disk.
//!
//! On APFS the space a snapshot keeps for itself is used space no walk of the live files finds,
//! so it shows as "not seen by the scan" — on a Mac with Time Machine's hourly local snapshots,
//! tens of gigabytes. Listing the snapshots needs no privilege; reading inside one needs root
//! (its mount is refused with `EPERM` otherwise, `fs_snapshot_mount` as `mount_apfs -s`,
//! measured 2026-10-05). So the folder comes in two steps, as the rest of a scan does: the
//! snapshots' names at once, each an empty folder, and as root an idle pass after every other
//! (`duscape_scan::snapshots`) that fills each with the files it holds and the live volume does
//! not.
//!
//! The folder is *virtual*: nothing is at its path on disk. So nothing under it may be deleted
//! ([`crate::delete::refused`]) or rescanned (`Rescans::start`), since a rescan of a path that is
//! not there takes it off the tree as gone.

use ::std::ffi::OsStr;

/// The folder at the volume's root holding one folder per snapshot. In brackets, as no folder a
/// person or a system makes is named; should the volume hold one of the name anyway, no snapshot
/// folder is added ([`crate::FileTree::note_snapshots`]).
pub const FOLDER: &str = "(local snapshots)";

/// What a tree knows of its volume's snapshots ([`crate::FileTree::snapshots`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Noted {
    /// How many there are, each a folder in [`FOLDER`]; none, and there is no such folder.
    pub count: usize,
    /// Whether what they hold has been read into their folders. Until it has (or without root,
    /// ever) the folders are empty.
    pub read: bool,
}

/// What the snapshots' folder, or a Time Machine snapshot's, is: for a status line. By name, as
/// [`crate::nas::describe`] does a NAS's folders.
#[must_use]
pub fn describe(name: &OsStr) -> Option<String> {
    if name == FOLDER {
        return Some(
            "the volume's local snapshots: what each still holds that is gone from the disk \
             (read as root)"
                .to_string(),
        );
    }
    let name = name.to_str()?;
    let date = name
        .strip_prefix("com.apple.TimeMachine.")?
        .strip_suffix(".local")?;
    Some(format!(
        "a Time Machine local snapshot of {date}: the files it holds that are gone from the disk"
    ))
}

/// Whether a mount's source (`statfs`'s `f_mntfromname`) is a snapshot of a volume: APFS names
/// it `<snapshot>@<device>` (`com.apple.TimeMachine.2026-09-29-234550.local@/dev/disk3s5`), where
/// a volume's own mount is its device alone. `/` on a sealed system is mounted from a snapshot
/// too, but by its device node (`/dev/disk3s1s1`), and a scan's root is walked whatever it is.
#[must_use]
pub fn is_snapshot_source(source: &str) -> bool {
    source
        .split_once('@')
        .is_some_and(|(snapshot, device)| !snapshot.is_empty() && device.starts_with("/dev/"))
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsStr;

    use super::{FOLDER, describe, is_snapshot_source};

    #[test]
    fn the_folder_and_time_machine_snapshots_are_described() {
        assert!(describe(OsStr::new(FOLDER)).is_some());
        let tm = describe(OsStr::new("com.apple.TimeMachine.2026-09-29-234550.local"))
            .expect("a Time Machine snapshot");
        assert!(tm.contains("2026-09-29-234550"), "{tm}");
        assert_eq!(describe(OsStr::new("Documents")), None);
        assert_eq!(describe(OsStr::new("com.apple.TimeMachine.notes")), None);
    }

    #[test]
    fn a_snapshot_mount_is_told_by_its_source() {
        assert!(is_snapshot_source(
            "com.apple.TimeMachine.2026-09-29-234550.local@/dev/disk3s5"
        ));
        assert!(!is_snapshot_source("/dev/disk3s5"));
        assert!(!is_snapshot_source("/dev/disk3s1s1"));
        assert!(!is_snapshot_source("map auto_home"));
        assert!(!is_snapshot_source("user@server:/share"));
        assert!(!is_snapshot_source("@/dev/disk3s5"));
    }
}
