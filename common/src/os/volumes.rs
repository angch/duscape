//! The volumes a scan could start from: what is mounted, how big, and how full — for a
//! window opened with no folder to offer, in place of a folder dialog.
//!
//! Linux reads `/proc/self/mounts` and keeps the block-backed and the well-known network
//! filesystems, one line a source (a bind mount or a btrfs subvolume of a volume already
//! listed is left out); macOS asks `getmntinfo` for the local mounts; Windows walks the drive
//! letters. Each is sized with the platform's free-space call, as `df` would be.

use ::std::path::PathBuf;

/// One mounted volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Where it is mounted: `/`, `/home`, `C:\`.
    pub path: PathBuf,
    /// What it is: the device on Unix (`/dev/nvme0n1p2`, `//server/share`), the label on
    /// Windows (`Local Disk`) or a mapped drive's share (`\\server\share`), possibly empty.
    pub label: String,
    /// The filesystem: `ext4`, `apfs`, `NTFS` — or, for a network drive on Windows, how it is
    /// reached (`cifs`, `nfs`, `webdav`), by the network provider that serves it.
    pub filesystem: String,
    /// Bytes in all, and in use.
    pub total: u64,
    pub used: u64,
}

/// The volumes worth offering, sorted by mount point; empty where nothing can be read.
#[must_use]
pub fn volumes() -> Vec<Volume> {
    let mut found = imp::volumes();
    found.retain(|volume| volume.total > 0);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

#[cfg(target_os = "linux")]
mod imp {
    use ::std::collections::HashSet;
    use ::std::path::{Path, PathBuf};

    use super::Volume;

    /// Network filesystems worth listing though they are not block-backed; the scan itself
    /// refuses to cross onto them, but named as the root they are scanned.
    const NETWORK: [&str; 8] = [
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "fuse.sshfs",
        "fuse.rclone",
        "9p",
        "virtiofs",
    ];
    /// Block-backed mounts that are no disk of the user's: snaps, live images.
    const SKIPPED: [&str; 3] = ["squashfs", "iso9660", "udf"];

    /// One line of `/proc/self/mounts`: source, mount point, filesystem.
    pub(super) fn parse(table: &str) -> Vec<(String, PathBuf, String)> {
        table
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                let source = unescape(fields.next()?);
                let target = unescape(fields.next()?);
                let kind = fields.next()?.to_string();
                Some((source, PathBuf::from(target), kind))
            })
            .collect()
    }

    /// `\040` and friends, as the kernel writes a space, a tab, a newline or a backslash.
    fn unescape(field: &str) -> String {
        let mut out = String::with_capacity(field.len());
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let digits: String = chars.clone().take(3).collect();
                if digits.len() == 3
                    && let Ok(code) = u8::from_str_radix(&digits, 8)
                {
                    out.push(code as char);
                    chars.nth(2);
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    /// Whether a mount is one to offer: a device, or a network filesystem.
    pub(super) fn wanted(source: &str, kind: &str) -> bool {
        (source.starts_with("/dev/") && !SKIPPED.contains(&kind))
            || kind == "zfs"
            || NETWORK.contains(&kind)
    }

    /// The mounts to list from a mount table: the first of each source, which is the shortest
    /// path for a bind mount or a subvolume mounted again below it.
    pub(super) fn choose(mounts: Vec<(String, PathBuf, String)>) -> Vec<(String, PathBuf, String)> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut sorted = mounts;
        sorted.sort_by_key(|(_, path, _)| path.as_os_str().len());
        sorted
            .into_iter()
            .filter(|(source, _, kind)| wanted(source, kind) && seen.insert(source.clone()))
            .collect()
    }

    fn sized(source: String, path: PathBuf, filesystem: String) -> Option<Volume> {
        let fs = ::rustix::fs::statvfs(&path).ok()?;
        let total = fs.f_blocks.saturating_mul(fs.f_frsize);
        let used = fs
            .f_blocks
            .saturating_sub(fs.f_bfree)
            .saturating_mul(fs.f_frsize);
        Some(Volume {
            path,
            label: source,
            filesystem,
            total,
            used,
        })
    }

    pub fn volumes() -> Vec<Volume> {
        let Ok(table) = ::std::fs::read_to_string(Path::new("/proc/self/mounts")) else {
            return Vec::new();
        };
        choose(parse(&table))
            .into_iter()
            .filter_map(|(source, path, kind)| sized(source, path, kind))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::{choose, parse, wanted};
        use ::std::path::Path;

        #[test]
        fn the_mount_table_is_read_and_sifted() {
            let table = "\
sysfs /sys sysfs rw,nosuid 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/loop3 /snap/core22/1234 squashfs ro 0 0
tmpfs /run tmpfs rw 0 0
/dev/nvme0n1p2 /home/me/bind ext4 rw,relatime 0 0
/dev/sdb1 /mnt/my\\040disk ntfs3 rw 0 0
nas:/export /mnt/nas nfs4 rw 0 0
pool/data /data zfs rw 0 0
";
            let mounts = parse(table);
            assert_eq!(mounts.len(), 9);
            assert_eq!(mounts[6].1, Path::new("/mnt/my disk"), "\\040 is a space");
            assert!(wanted("/dev/sda1", "ext4") && !wanted("tmpfs", "tmpfs"));
            assert!(!wanted("/dev/loop3", "squashfs"), "a snap is not a disk");
            let chosen = choose(mounts);
            let paths: Vec<&Path> = chosen.iter().map(|(_, path, _)| path.as_path()).collect();
            assert_eq!(
                paths,
                [
                    Path::new("/"),
                    Path::new("/data"),
                    Path::new("/mnt/nas"),
                    Path::new("/boot/efi"),
                    Path::new("/mnt/my disk"),
                ],
                "each source once, at its shortest path; pseudo filesystems and snaps left out"
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use ::std::ffi::CStr;
    use ::std::path::{Path, PathBuf};

    use super::Volume;

    fn text(bytes: &[libc::c_char]) -> String {
        // SAFETY: the kernel NUL-terminates the names it fills in.
        unsafe { CStr::from_ptr(bytes.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Whether a local mount is one to offer: not a device or automount table, and of the
    /// system's own volumes only the data one (the user's files) and the root.
    pub(super) fn wanted(path: &Path, filesystem: &str, local: bool) -> bool {
        if !local || matches!(filesystem, "devfs" | "autofs" | "nullfs") {
            return false;
        }
        let under_system =
            path.starts_with("/System/Volumes") && path != Path::new("/System/Volumes/Data");
        !under_system && !path.starts_with("/private/var/vm") && !path.starts_with("/dev")
    }

    pub fn volumes() -> Vec<Volume> {
        let mut list: *mut libc::statfs = ::std::ptr::null_mut();
        // SAFETY: `getmntinfo` fills `list` with a kernel-owned array it also sizes; the
        // memory stays valid until the next call on this thread.
        let count = unsafe { libc::getmntinfo(&raw mut list, libc::MNT_NOWAIT) };
        if count <= 0 || list.is_null() {
            return Vec::new();
        }
        // SAFETY: `count` entries were filled in at `list`.
        let mounts = unsafe { ::std::slice::from_raw_parts(list, count as usize) };
        mounts
            .iter()
            .filter_map(|mount| {
                let path = PathBuf::from(text(&mount.f_mntonname));
                let filesystem = text(&mount.f_fstypename);
                let local = mount.f_flags & libc::MNT_LOCAL as u32 != 0;
                if !wanted(&path, &filesystem, local) {
                    return None;
                }
                let block = u64::from(mount.f_bsize);
                Some(Volume {
                    path,
                    label: text(&mount.f_mntfromname),
                    filesystem,
                    total: mount.f_blocks.saturating_mul(block),
                    used: mount
                        .f_blocks
                        .saturating_sub(mount.f_bfree)
                        .saturating_mul(block),
                })
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::wanted;
        use ::std::path::Path;

        #[test]
        fn the_system_volumes_are_sifted() {
            assert!(wanted(Path::new("/"), "apfs", true));
            assert!(wanted(Path::new("/System/Volumes/Data"), "apfs", true));
            assert!(!wanted(Path::new("/System/Volumes/Preboot"), "apfs", true));
            assert!(wanted(Path::new("/Volumes/Backup"), "hfs", true));
            assert!(!wanted(Path::new("/Volumes/share"), "smbfs", false));
            assert!(!wanted(Path::new("/dev"), "devfs", true));
        }
    }
}

/// What to call a Windows network drive's filesystem, by the network provider that serves it
/// (`NETRESOURCE::lpProvider`): `cifs` for the Windows network (SMB), as Linux names the same
/// mount, `nfs` for an NFS client's, `webdav` for the Web Client's, else the provider's own
/// name. Not the volume's filesystem name: for a share, Windows gives the server's (`NTFS` for
/// a share on an NTFS disk), and a mapped drive was offered as a local NTFS volume.
// Windows's alone, and tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
#[must_use]
pub fn network_filesystem(provider: &str) -> String {
    let lower = provider.to_ascii_lowercase();
    if lower.contains("nfs") {
        "nfs".to_string()
    } else if lower.contains("web client") || lower.contains("webdav") {
        "webdav".to_string()
    } else if lower.contains("microsoft windows network") || lower.contains("smb") {
        "cifs".to_string()
    } else if provider.is_empty() {
        "network".to_string()
    } else {
        provider.to_string()
    }
}

#[cfg(windows)]
mod imp {
    use ::std::ffi::OsString;
    use ::std::os::windows::ffi::OsStringExt;
    use ::std::path::PathBuf;

    use windows_sys::Win32::NetworkManagement::WNet::{
        NETRESOURCEW, RESOURCETYPE_DISK, WNetGetConnectionW, WNetGetResourceInformationW,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };

    use super::{Volume, network_filesystem};

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;

    fn until_nul(wide: &[u16]) -> String {
        let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        OsString::from_wide(&wide[..len])
            .to_string_lossy()
            .into_owned()
    }

    fn drive(letter: u8) -> Option<Volume> {
        let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
        // SAFETY: `root` is NUL-terminated.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if !matches!(kind, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_REMOTE) {
            return None;
        }
        let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: `root` is NUL-terminated and the three outputs are live `u64`s. An empty
        // removable drive fails here, and is left out.
        let sized = unsafe {
            GetDiskFreeSpaceExW(
                root.as_ptr(),
                &raw mut available,
                &raw mut total,
                &raw mut free,
            )
        };
        if sized == 0 || total == 0 {
            return None;
        }
        let mut label = [0u16; 256];
        let mut filesystem = [0u16; 256];
        let (mut serial, mut max_component, mut flags) = (0u32, 0u32, 0u32);
        // SAFETY: `root` is NUL-terminated; the two buffers are as long as the lengths passed.
        let named = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &raw mut serial,
                &raw mut max_component,
                &raw mut flags,
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        };
        let (mut label, mut filesystem) = if named != 0 {
            (until_nul(&label), until_nul(&filesystem))
        } else {
            (String::new(), String::new())
        };
        // A network drive: the share, and how it is reached — not the server's filesystem.
        if kind == DRIVE_REMOTE {
            let (share, provider) = share_of(letter);
            if let Some(share) = share {
                label = share;
            }
            filesystem = network_filesystem(&provider.unwrap_or_default());
        }
        Some(Volume {
            path: PathBuf::from(format!("{}:\\", letter as char)),
            label,
            filesystem,
            total,
            used: total.saturating_sub(free),
        })
    }

    /// A mapped drive's share (`\\server\share`) and the network provider that serves it,
    /// each if Windows says. Asked of the providers (`mpr.dll`) after `GetDiskFreeSpaceExW` has
    /// reached the server already: the listing took 20 ms warm either way with three shares
    /// mapped (2026-10-05), its first call 56 → 109 ms as `mpr.dll` loads.
    fn share_of(letter: u8) -> (Option<String>, Option<String>) {
        let local = [u16::from(letter), u16::from(b':'), 0];
        let mut remote = [0u16; 1024];
        let mut length = remote.len() as u32;
        // SAFETY: `local` is NUL-terminated and `remote` holds `length` characters.
        let connected =
            unsafe { WNetGetConnectionW(local.as_ptr(), remote.as_mut_ptr(), &raw mut length) };
        if connected != 0 {
            return (None, None);
        }
        let share = until_nul(&remote);
        let mut resource = NETRESOURCEW {
            dwScope: 0,
            dwType: RESOURCETYPE_DISK,
            dwDisplayType: 0,
            dwUsage: 0,
            lpLocalName: ::std::ptr::null_mut(),
            lpRemoteName: remote.as_mut_ptr(),
            lpComment: ::std::ptr::null_mut(),
            lpProvider: ::std::ptr::null_mut(),
        };
        // The answer is a NETRESOURCEW followed by the strings it points into; a buffer of
        // `u64`s keeps it aligned for the struct.
        let mut buffer = vec![0u64; 1024];
        let mut bytes = (buffer.len() * 8) as u32;
        let mut system: windows_sys::core::PWSTR = ::std::ptr::null_mut();
        // SAFETY: `resource` names the share by a NUL-terminated string that outlives the call;
        // `buffer` holds `bytes` bytes, aligned for a NETRESOURCEW.
        let found = unsafe {
            WNetGetResourceInformationW(
                &raw mut resource,
                buffer.as_mut_ptr().cast(),
                &raw mut bytes,
                &raw mut system,
            )
        };
        if found != 0 {
            return (Some(share), None);
        }
        // SAFETY: on success the buffer starts with a NETRESOURCEW, its strings inside it.
        let provider = unsafe {
            let answer = &*buffer.as_ptr().cast::<NETRESOURCEW>();
            let provider = answer.lpProvider;
            if provider.is_null() {
                None
            } else {
                let len = (0..).take_while(|&i| *provider.add(i) != 0).count();
                Some(
                    OsString::from_wide(::std::slice::from_raw_parts(provider, len))
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        };
        (Some(share), provider)
    }

    pub fn volumes() -> Vec<Volume> {
        // SAFETY: no preconditions.
        let mask = unsafe { GetLogicalDrives() };
        (0..26u8)
            .filter(|bit| mask & (1 << bit) != 0)
            .filter_map(|bit| drive(b'A' + bit))
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    pub fn volumes() -> Vec<super::Volume> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::network_filesystem;

    /// A network drive is named by how it is reached, never by the server's filesystem.
    #[test]
    fn a_network_drive_is_named_by_its_provider() {
        assert_eq!(network_filesystem("Microsoft Windows Network"), "cifs");
        assert_eq!(network_filesystem("NFS Network"), "nfs");
        assert_eq!(network_filesystem("Web Client Network"), "webdav");
        assert_eq!(
            network_filesystem("Some Other Provider"),
            "Some Other Provider"
        );
        assert_eq!(network_filesystem(""), "network");
    }
}
