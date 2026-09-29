//! The folders a NAS keeps for itself, known by name: what Synology's DSM and QNAP's QTS put on
//! a volume and in a share, seen from the NAS itself or over a network share (SMB, NFS, AFP),
//! where nothing but the name says what they are.
//!
//! Two kinds are left out of a walk unless `--snapshots` ([`left_out`]): a share's snapshots,
//! each a whole earlier copy of the share (Synology's `#snapshot`, QNAP's `@Recently-Snapshot`,
//! Synology's `@sharesnap` on the volume) — walking a share with hourly snapshots walked it once
//! an hour of retention — and a share's recycle bin (`#recycle`, `@Recycle`). On the NAS itself
//! the snapshots are read-only btrfs subvolumes the Linux walker recognises as such; over the
//! network they are plain folders. The rest hold real space and are walked, and the viewers
//! say what they are ([`describe`]): Container Manager's images, the packages installed, the
//! thumbnails and indexes a folder's files get.

use ::std::ffi::OsStr;

/// What a NAS keeps a folder of the name for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A whole earlier copy of the share each: left out unless asked for.
    Snapshots,
    /// Deleted files kept: left out unless asked for, with the snapshots.
    RecycleBin,
    /// What the NAS keeps about a folder's files: thumbnails, indexes.
    Metadata,
    /// The NAS's own data: packages, containers, databases, temporary files.
    Data,
}

/// A folder name a NAS uses, whose it is and what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub name: &'static str,
    pub vendor: &'static str,
    pub kind: Kind,
    pub what: &'static str,
}

const SYNOLOGY: &str = "Synology";
const QNAP: &str = "QNAP";

/// The names, as each NAS spells them (the comparison is exact: `@eaDir` is never `@eadir`).
pub const KNOWN: &[Known] = &[
    Known {
        name: "#snapshot",
        vendor: SYNOLOGY,
        kind: Kind::Snapshots,
        what: "the share's snapshots, a whole earlier copy each",
    },
    Known {
        name: "@sharesnap",
        vendor: SYNOLOGY,
        kind: Kind::Snapshots,
        what: "the shares' snapshots, a whole earlier copy each",
    },
    Known {
        name: "#recycle",
        vendor: SYNOLOGY,
        kind: Kind::RecycleBin,
        what: "the share's recycle bin",
    },
    Known {
        name: "@eaDir",
        vendor: SYNOLOGY,
        kind: Kind::Metadata,
        what: "thumbnails and indexes of the folder's files",
    },
    Known {
        name: "@docker",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "Container Manager's containers, images and volumes",
    },
    Known {
        name: "@appstore",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "the packages installed (Package Center)",
    },
    Known {
        name: "@cloudstation",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "Cloud Station's versions and database",
    },
    Known {
        name: "@synologydrive",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "Synology Drive's versions and database",
    },
    Known {
        name: "@database",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "the system's databases",
    },
    Known {
        name: "@tmp",
        vendor: SYNOLOGY,
        kind: Kind::Data,
        what: "temporary files",
    },
    Known {
        name: "@Recently-Snapshot",
        vendor: QNAP,
        kind: Kind::Snapshots,
        what: "the share's snapshots, a whole earlier copy each",
    },
    Known {
        name: "@Recycle",
        vendor: QNAP,
        kind: Kind::RecycleBin,
        what: "the share's recycle bin",
    },
    Known {
        name: ".@__thumb",
        vendor: QNAP,
        kind: Kind::Metadata,
        what: "thumbnails of the folder's pictures",
    },
    Known {
        name: ".qpkg",
        vendor: QNAP,
        kind: Kind::Data,
        what: "the packages installed (App Center)",
    },
];

/// The folder a NAS keeps under `name`, if it is one.
#[must_use]
pub fn known(name: &OsStr) -> Option<&'static Known> {
    KNOWN.iter().find(|known| name == known.name)
}

/// Whether a walk leaves the folder `name` empty — listed, not entered: a share's snapshots or
/// recycle bin, unless `snapshots` asks for them. Anywhere in the scan, not only at a share's
/// top, since nothing else is named so; named as the scan's root, any folder is scanned.
#[must_use]
pub fn left_out(name: &OsStr, snapshots: bool) -> bool {
    !snapshots
        && known(name).is_some_and(|known| matches!(known.kind, Kind::Snapshots | Kind::RecycleBin))
}

/// What a walker notes in the directory holding a folder it left out (`DirEntries::note`, kind
/// `left out`), for `--issues`.
#[must_use]
pub fn left_out_note(name: &OsStr) -> String {
    match known(name) {
        Some(known) => format!(
            "{}: {}, left empty (--snapshots walks it)",
            known.vendor, known.what
        ),
        None => "left empty (--snapshots walks it)".to_string(),
    }
}

/// What a NAS folder is, for a viewer to say beside its name: `Synology: the share's recycle
/// bin`. `None` for any other name.
#[must_use]
pub fn describe(name: &OsStr) -> Option<String> {
    known(name).map(|known| format!("{}: {}", known.vendor, known.what))
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsStr;

    use super::{KNOWN, Kind, describe, known, left_out, left_out_note};

    #[test]
    fn snapshots_and_recycle_bins_are_left_out_unless_asked_for_and_the_rest_walked() {
        for name in [
            "#snapshot",
            "#recycle",
            "@Recently-Snapshot",
            "@Recycle",
            "@sharesnap",
        ] {
            assert!(left_out(OsStr::new(name), false), "{name}");
            assert!(
                !left_out(OsStr::new(name), true),
                "{name}, with --snapshots"
            );
        }
        for name in [
            "@docker",
            "@eaDir",
            ".@__thumb",
            "@appstore",
            "photos",
            "#recycle2",
        ] {
            assert!(!left_out(OsStr::new(name), false), "{name} is walked");
        }
        assert!(
            !left_out(OsStr::new("@recycle"), false),
            "the spelling is the NAS's"
        );
    }

    #[test]
    fn every_known_folder_is_described_with_its_vendor() {
        for known in KNOWN {
            let words = describe(OsStr::new(known.name)).expect(known.name);
            assert!(words.starts_with(&format!("{}: ", known.vendor)), "{words}");
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
            left_out_note(OsStr::new("@Recently-Snapshot")),
            "QNAP: the share's snapshots, a whole earlier copy each, left empty (--snapshots walks it)"
        );
    }
}
