//! The folders a system keeps for itself, known by name: what Synology's DSM, QNAP's QTS, ZFS,
//! NetApp, Unraid, Samba and snapper put on a volume or in a share — seen from the machine
//! itself or over a network share (SMB, NFS, AFP), where nothing but the name says what they
//! are — and the metadata folders macOS and Windows leave on any volume they touch.
//!
//! One kind is left out of a walk unless `--snapshots` ([`left_out`]): a share's snapshots,
//! each a whole earlier copy of the share (Synology's `#snapshot`, QNAP's `@Recently-Snapshot`,
//! ZFS's `.zfs`, NetApp's `.snapshot` and `~snapshot`, snapper's `.snapshots`) — walking a share
//! with hourly snapshots walked it once an hour of retention, and over the network the blocks a
//! snapshot shares with the live files cannot be seen shared. On the NAS itself the snapshots
//! are read-only btrfs subvolumes the Linux walker recognises as such. Everything else holds
//! space the disk gives up to it, a recycle bin's deleted files included, and is walked; the
//! viewers say what it is ([`describe`]).

use ::std::ffi::OsStr;

/// What a system keeps a folder of the name for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A whole earlier copy of the share each: left out unless asked for.
    Snapshots,
    /// Deleted files kept until the bin is emptied: space, and walked.
    RecycleBin,
    /// What the system keeps about a folder's files: thumbnails, indexes, forks.
    Metadata,
    /// The system's own data: packages, containers, backups, databases, temporary files.
    Data,
}

/// A folder name a system uses, whose it is and what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub name: &'static str,
    pub vendor: &'static str,
    pub kind: Kind,
    pub what: &'static str,
}

const SYNOLOGY: &str = "Synology";
const QNAP: &str = "QNAP";

/// The names, as each system spells them (the comparison is exact: `@eaDir` is never `@eadir`).
#[rustfmt::skip]
pub const KNOWN: &[Known] = &[
    // Snapshots: left out of a walk unless `--snapshots`.
    Known { name: "#snapshot", vendor: SYNOLOGY, kind: Kind::Snapshots, what: "the share's snapshots, a whole earlier copy each" },
    Known { name: "@sharesnap", vendor: SYNOLOGY, kind: Kind::Snapshots, what: "the shares' snapshots, a whole earlier copy each" },
    Known { name: "@Recently-Snapshot", vendor: QNAP, kind: Kind::Snapshots, what: "the share's snapshots, a whole earlier copy each" },
    Known { name: ".zfs", vendor: "ZFS", kind: Kind::Snapshots, what: "the dataset's snapshots (.zfs/snapshot), a whole earlier copy each" },
    Known { name: ".snapshot", vendor: "NetApp", kind: Kind::Snapshots, what: "the volume's snapshots, a whole earlier copy each" },
    Known { name: "~snapshot", vendor: "NetApp", kind: Kind::Snapshots, what: "the volume's snapshots over SMB, a whole earlier copy each" },
    Known { name: ".snapshots", vendor: "snapper", kind: Kind::Snapshots, what: "the subvolume's snapshots, a whole earlier copy each" },
    // Recycle bins: deleted files still holding space.
    Known { name: "#recycle", vendor: SYNOLOGY, kind: Kind::RecycleBin, what: "the share's recycle bin" },
    Known { name: "@Recycle", vendor: QNAP, kind: Kind::RecycleBin, what: "the share's recycle bin" },
    Known { name: ".recycle", vendor: "Samba", kind: Kind::RecycleBin, what: "the share's recycle bin (vfs_recycle; TrueNAS)" },
    Known { name: ".Recycle.Bin", vendor: "Unraid", kind: Kind::RecycleBin, what: "the share's recycle bin" },
    Known { name: "$RECYCLE.BIN", vendor: "Windows", kind: Kind::RecycleBin, what: "the Recycle Bin on this volume" },
    Known { name: ".Trashes", vendor: "macOS", kind: Kind::RecycleBin, what: "the Trash on this volume" },
    // Metadata about a folder's files.
    Known { name: "@eaDir", vendor: SYNOLOGY, kind: Kind::Metadata, what: "thumbnails and indexes of the folder's files" },
    Known { name: ".@__thumb", vendor: QNAP, kind: Kind::Metadata, what: "thumbnails of the folder's pictures" },
    Known { name: ".streams", vendor: QNAP, kind: Kind::Metadata, what: "NTFS alternate data streams of the folder's files, kept by Samba" },
    Known { name: ".AppleDouble", vendor: "Netatalk", kind: Kind::Metadata, what: "resource forks and Finder info of the folder's files" },
    Known { name: ".Spotlight-V100", vendor: "macOS", kind: Kind::Metadata, what: "the Spotlight index of this volume" },
    Known { name: ".fseventsd", vendor: "macOS", kind: Kind::Metadata, what: "the file system events log of this volume" },
    Known { name: ".DocumentRevisions-V100", vendor: "macOS", kind: Kind::Metadata, what: "document versions kept on this volume" },
    Known { name: ".TemporaryItems", vendor: "macOS", kind: Kind::Metadata, what: "temporary files of applications" },
    Known { name: "System Volume Information", vendor: "Windows", kind: Kind::Metadata, what: "restore points, shadow copies' catalogue and the search index" },
    Known { name: "lost+found", vendor: "ext4", kind: Kind::Metadata, what: "files fsck found orphaned" },
    // The system's own data.
    Known { name: "@docker", vendor: SYNOLOGY, kind: Kind::Data, what: "Container Manager's containers, images and volumes" },
    Known { name: "@appstore", vendor: SYNOLOGY, kind: Kind::Data, what: "the packages installed (Package Center)" },
    Known { name: "@ActiveBackup", vendor: SYNOLOGY, kind: Kind::Data, what: "Active Backup for Business's backups" },
    Known { name: "@iSCSI", vendor: SYNOLOGY, kind: Kind::Data, what: "the iSCSI LUNs" },
    Known { name: "@img_bkp_cache", vendor: SYNOLOGY, kind: Kind::Data, what: "Hyper Backup's local cache" },
    Known { name: "@S2S", vendor: SYNOLOGY, kind: Kind::Data, what: "Shared Folder Sync's data" },
    Known { name: "@cloudstation", vendor: SYNOLOGY, kind: Kind::Data, what: "Cloud Station's versions and database" },
    Known { name: "@synologydrive", vendor: SYNOLOGY, kind: Kind::Data, what: "Synology Drive's versions and database" },
    Known { name: "@database", vendor: SYNOLOGY, kind: Kind::Data, what: "the system's databases" },
    Known { name: "@tmp", vendor: SYNOLOGY, kind: Kind::Data, what: "temporary files" },
    Known { name: ".qpkg", vendor: QNAP, kind: Kind::Data, what: "the packages installed (App Center)" },
    Known { name: ".system", vendor: QNAP, kind: Kind::Data, what: "the system's data: indexes, thumbnails, the apps' data" },
    Known { name: ".Qsync", vendor: QNAP, kind: Kind::Data, what: "Qsync's synced files" },
];

/// The folder a system keeps under `name`, if it is one.
#[must_use]
pub fn known(name: &OsStr) -> Option<&'static Known> {
    KNOWN.iter().find(|known| name == known.name)
}

/// The folder a walk leaves empty under `name` — listed, not entered: a share's snapshots,
/// unless `snapshots` asks for them. Anywhere in the scan, not only at a share's top, since
/// nothing else is named so; named as the scan's root, any folder is scanned. Every walker
/// asks through [`crate::scan::DirEntries::leave_out`], which notes it too.
#[must_use]
pub fn left_out(name: &OsStr, snapshots: bool) -> Option<&'static Known> {
    if snapshots {
        return None;
    }
    known(name).filter(|known| known.kind == Kind::Snapshots)
}

/// What a walker notes in the directory holding a folder it left out (`DirEntries::note`, kind
/// `left out`), for `--issues`.
#[must_use]
pub fn left_out_note(known: &Known) -> String {
    format!(
        "{}: {}, left empty (--snapshots walks it)",
        known.vendor, known.what
    )
}

/// What a system's folder is, for a viewer to say beside its name: `Synology: the share's
/// recycle bin`, or a volume's local snapshots ([`crate::snapshots::describe`]). `None` for any
/// other name.
#[must_use]
pub fn describe(name: &OsStr) -> Option<String> {
    known(name)
        .map(|known| format!("{}: {}", known.vendor, known.what))
        // The volume's local snapshots, as a scan's root shows them: every viewer's status line
        // asks here.
        .or_else(|| crate::snapshots::describe(name))
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsStr;

    use super::{KNOWN, Kind, describe, known, left_out, left_out_note};

    #[test]
    fn snapshots_are_left_out_unless_asked_for_and_everything_else_is_walked() {
        for name in [
            "#snapshot",
            "@sharesnap",
            "@Recently-Snapshot",
            ".zfs",
            ".snapshot",
            "~snapshot",
            ".snapshots",
        ] {
            assert!(left_out(OsStr::new(name), false).is_some(), "{name}");
            assert!(
                left_out(OsStr::new(name), true).is_none(),
                "{name}, with --snapshots"
            );
        }
        for name in [
            "#recycle",
            "@Recycle",
            ".recycle",
            "$RECYCLE.BIN",
            "@docker",
            "@eaDir",
            ".@__thumb",
            "photos",
            "#snapshot2",
        ] {
            assert!(
                left_out(OsStr::new(name), false).is_none(),
                "{name} is walked: it is space"
            );
        }
        assert!(
            left_out(OsStr::new("#Snapshot"), false).is_none(),
            "the spelling is the system's"
        );
    }

    #[test]
    fn every_known_folder_is_described_with_its_vendor_and_names_are_unique() {
        for (index, known) in KNOWN.iter().enumerate() {
            let words = describe(OsStr::new(known.name)).expect(known.name);
            assert!(words.starts_with(&format!("{}: ", known.vendor)), "{words}");
            assert!(
                !KNOWN[..index].iter().any(|other| other.name == known.name),
                "{} twice",
                known.name
            );
        }
        assert_eq!(
            describe(OsStr::new("@docker")).as_deref(),
            Some("Synology: Container Manager's containers, images and volumes")
        );
        assert_eq!(describe(OsStr::new("docker")), None);
        assert_eq!(
            known(OsStr::new("@Recycle")).map(|k| k.kind),
            Some(Kind::RecycleBin)
        );
        assert_eq!(
            left_out_note(left_out(OsStr::new("@Recently-Snapshot"), false).expect("left out")),
            "QNAP: the share's snapshots, a whole earlier copy each, left empty (--snapshots walks it)"
        );
    }
}
