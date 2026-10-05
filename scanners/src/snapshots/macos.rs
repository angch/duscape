//! macOS's part of reading a volume's snapshots: listing them, mounting one, the change log,
//! and the live volume asked by id.

use ::std::collections::HashMap;
use ::std::ffi::{CString, OsStr, OsString};
use ::std::io;
use ::std::os::fd::AsRawFd;
use ::std::os::unix::ffi::{OsStrExt, OsStringExt};
use ::std::os::unix::fs::MetadataExt;
use ::std::path::{Path, PathBuf};
use ::std::process::Command;
use ::std::sync::Mutex;
use ::std::sync::atomic::{AtomicUsize, Ordering};
use ::std::time::{Duration, SystemTime};

use libduscape::model::FileTree;
use libduscape::scan::ScanOptions;

use super::extents::{Ranges, held_alone, physical};
use super::log::{Named, log_files, on_volume, parse_firmlinks, scopes, since_for};
use super::{Listed, Lookups, Mounted, Pass, Reading, Scope, build};

/// Where a scan of `/` finds the snapshots worth showing: the data volume's. `/` is the
/// sealed system volume, itself mounted from a snapshot, and lists none of the data's.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// How long the change log may take to replay, for each snapshot it would spare a whole read:
/// about what reading one whole costs, a walk of the volume (`/`: 42.8 s here, 2026-10-05).
/// The log replays at about 5,000 events a second (ten million ids, 54k events, in 10–12 s).
const REPLAY_PER_SNAPSHOT: Duration = Duration::from_secs(45);

/// `statfs` at `path`: the mount's source, where it is mounted, and its filesystem.
fn mount_of(path: &Path) -> Option<(String, PathBuf, String)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut status = ::std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `status` is a writable `statfs` allocation.
    if unsafe { libc::statfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` returning zero means it initialized the structure.
    let status = unsafe { status.assume_init() };
    let text = |chars: &[libc::c_char]| -> Vec<u8> {
        chars
            .iter()
            .take_while(|character| **character != 0)
            .map(|character| *character as u8)
            .collect()
    };
    Some((
        String::from_utf8_lossy(&text(&status.f_mntfromname)).into_owned(),
        PathBuf::from(OsString::from_vec(text(&status.f_mntonname))),
        String::from_utf8_lossy(&text(&status.f_fstypename)).into_owned(),
    ))
}

/// Whether the mount at `path` is a volume's snapshot: Time Machine's, when its local
/// snapshots are browsed, or this pass's own while it runs. The walk leaves it out unless
/// `--snapshots`, as it does a btrfs snapshot: walked, it counts the volume's files again.
/// Written from the source's documented form (`libduscape::snapshots::is_snapshot_source`)
/// and not yet run against a snapshot mount, which needs root.
#[must_use]
pub fn is_snapshot_mount(path: &Path) -> bool {
    mount_of(path).is_some_and(|(from, _, _)| libduscape::snapshots::is_snapshot_source(&from))
}

/// The volume whose snapshots a scan of `root` shows: `root` when it is an APFS volume's
/// mount point (the data volume for `/`). A folder inside a volume shows none: its share of
/// a snapshot's space is not known without reading the snapshot.
#[must_use]
pub fn volume_for(root: &Path) -> Option<PathBuf> {
    let volume = if root == Path::new("/") {
        Path::new(DATA_VOLUME)
    } else {
        root
    };
    let (_, on, kind) = mount_of(volume)?;
    (kind == "apfs" && on == volume).then(|| volume.to_path_buf())
}

// Not in `libc`: <sys/snapshot.h>.
unsafe extern "C" {
    fn fs_snapshot_list(
        dirfd: libc::c_int,
        alist: *mut libc::attrlist,
        attrbuf: *mut libc::c_void,
        bufsize: libc::size_t,
        options: u32,
    ) -> libc::c_int;
}

/// The names of `volume`'s snapshots, oldest first. Needs no privilege.
pub fn list(volume: &Path) -> io::Result<Vec<OsString>> {
    Ok(list_with_times(volume)?
        .into_iter()
        .map(|listed| listed.name)
        .collect())
}

/// `volume`'s snapshots with when each was made and whether it is dataless, oldest first.
pub fn list_with_times(volume: &Path) -> io::Result<Vec<Listed>> {
    let directory = ::std::fs::File::open(volume)?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        // `RETURNED_ATTRS` too, or the call is refused with `EINVAL` (measured).
        commonattr: libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_CRTIME
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = vec![0u64; 4096 / 8];
    let mut snapshots = Vec::new();
    loop {
        // SAFETY: the descriptor is open, `attributes` is a valid request, and the buffer
        // is writable for the length given.
        let count = unsafe {
            fs_snapshot_list(
                directory.as_raw_fd(),
                &raw mut attributes,
                buffer.as_mut_ptr().cast(),
                buffer.len() * 8,
                0,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            break;
        }
        // SAFETY: the buffer is plain bytes, as long as its `u64`s and aligned for them.
        let bytes: &[u8] =
            unsafe { ::std::slice::from_raw_parts(buffer.as_ptr().cast(), buffer.len() * 8) };
        snapshots.extend(super::parse_listing(bytes, count as usize));
    }
    Ok(snapshots)
}

/// Whether this process can mount a snapshot: root only (`EPERM` otherwise, measured).
#[must_use]
pub fn can_read() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Whether this process has Full Disk Access: whether it can read a TCC database, which macOS
/// guards for every process without it, root's too. Mounting a snapshot needs it as well as
/// root: without it the mount is refused with `EPERM` as root as unprivileged (reported
/// 2026-10-05, `mount_apfs` refused on every snapshot of a root run).
#[must_use]
pub fn full_disk_access() -> bool {
    let user = ::std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/com.apple.TCC/TCC.db"));
    ["/Library/Application Support/com.apple.TCC/TCC.db".into()]
        .into_iter()
        .chain(user)
        .any(|path: PathBuf| ::std::fs::File::open(path).is_ok())
}

/// Mount `name`, a snapshot of `volume`, read-only and out of the Finder's sight in a folder
/// of its own, unmounted when the result is dropped; why not, if it cannot be. By the
/// volume's device node, the `special` `mount_apfs(8)` documents, and failing that by its
/// mount point, as the examples that work give it: this path is root-only and was not run
/// where it was written, so both are tried and both errors kept.
fn mount(volume: &Path, name: &OsStr) -> Result<Mounted, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let device = mount_of(volume)
        .map(|(from, _, _)| from)
        .filter(|from| from.starts_with("/dev/"))
        .map(PathBuf::from);
    let place = ::std::env::temp_dir().join(format!(
        "duscape-snapshot-{}-{}",
        ::std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    ::std::fs::create_dir_all(&place).map_err(|error| format!("{}: {error}", place.display()))?;
    let mut errors = Vec::new();
    for special in device.iter().map(PathBuf::as_path).chain([volume]) {
        match mount_at(name, special, &place) {
            Ok(()) => return Ok(unmounting(place)),
            Err(why) => errors.push(why),
        }
    }
    let _ = ::std::fs::remove_dir(&place);
    let mut why = errors.join("; ");
    if why.contains("Operation not permitted") && !full_disk_access() {
        why.push_str(
            " — macOS refuses to mount a snapshot for a process without Full Disk Access, root's \
             too: give it to the app running duscape (System Settings ▸ Privacy & Security ▸ \
             Full Disk Access) and run it again",
        );
    }
    Err(why)
}

/// The folders snapshots are mounted in, `<temp>/duscape-snapshot-<pid>-<n>`: the pid.
fn mount_owner(name: &OsStr) -> Option<i32> {
    name.to_str()?
        .strip_prefix("duscape-snapshot-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// Unmount and remove what a pass left behind in a process since gone: quitting does not
/// wait for the pass (the viewers let their threads go), so a quit while it reads leaves its
/// snapshot mounted, out of the Finder's sight, until the machine restarts.
fn sweep_left_behind() {
    let temp = ::std::env::temp_dir();
    let Ok(entries) = ::std::fs::read_dir(&temp) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = mount_owner(&entry.file_name()) else {
            continue;
        };
        // SAFETY: signal 0 sends nothing; it asks whether the process is there.
        let gone = unsafe { libc::kill(pid, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            drop(unmounting(entry.path()));
        }
    }
}

/// `mount_apfs -o nobrowse -s name special place`, and its error if it fails.
fn mount_at(name: &OsStr, special: &Path, place: &Path) -> Result<(), String> {
    let output = Command::new("/sbin/mount_apfs")
        .args(["-o", "nobrowse", "-s"])
        .arg(name)
        .arg(special)
        .arg(place)
        .output()
        .map_err(|error| format!("mount_apfs: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "mount_apfs -s {} {}: {}",
        name.to_string_lossy(),
        special.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// The snapshot mounted at `place`, unmounted (forced if it must be) and its folder removed
/// when dropped.
fn unmounting(place: PathBuf) -> Mounted {
    Mounted::new(place.clone(), move || {
        let unmount = |force: bool| {
            let mut command = Command::new("/sbin/umount");
            if force {
                command.arg("-f");
            }
            command
                .arg(&place)
                .output()
                .is_ok_and(|output| output.status.success())
        };
        // Not mounted (a sweep's folder whose mount went with a restart) fails both, and the
        // folder goes all the same: `remove_dir` takes an empty folder, never a mount in use.
        if !unmount(false) {
            unmount(true);
        }
        let _ = ::std::fs::remove_dir(&place);
    })
}

/// The live volume `volume` asked as the pass asks it, for `run`: folders listed as the walk
/// lists them, and by id through volfs; a file written over since counted by its extents,
/// against `counted`, what the pass has counted so far. `None` if the volume cannot be read.
pub fn with_lookups<R>(
    volume: &Path,
    options: ScanOptions,
    run: impl FnOnce(&Lookups<'_>) -> R,
) -> Option<R> {
    let device = ::std::fs::metadata(volume).ok()?.dev();
    let by_id = |id: u64| PathBuf::from(format!("/.vol/{device}/{id}"));
    let list = |path: &Path| crate::macos::list_one(path, 0, None, options.snapshots);
    let live_folder = |id: u64| {
        let path = by_id(id);
        ::std::fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.is_dir())
            .then_some(path)
    };
    let live = |id: u64| ::std::fs::symlink_metadata(by_id(id)).is_ok();
    let same_place = |relative: &Path, id: u64| {
        ::std::fs::symlink_metadata(volume.join(relative)).is_ok_and(|meta| meta.ino() == id)
    };
    let counted = Mutex::new(Ranges::default());
    let written_over = |path: &Path, id: u64| written_over(path, &by_id(id), &counted);
    Some(run(&Lookups {
        list: &list,
        live_folder: &live_folder,
        live: &live,
        same_place: &same_place,
        written_over: &written_over,
    }))
}

/// The bytes the snapshot's file at `snapshot` alone holds against the live file at `live`:
/// none when the two are alike in size and time, else its extents less the live one's and
/// less `counted`, which takes them in.
fn written_over(snapshot: &Path, live: &Path, counted: &Mutex<Ranges>) -> u64 {
    // A file whose contents are in iCloud, not on disk (`SF_DATALESS`), is never opened:
    // opening it starts a download, and what is not on disk holds no blocks to count.
    const SF_DATALESS: u32 = 0x4000_0000;
    let stamp = |path: &Path| {
        use ::std::os::macos::fs::MetadataExt as _;
        ::std::fs::symlink_metadata(path)
            .ok()
            .filter(|meta| meta.is_file() && meta.st_flags() & SF_DATALESS == 0)
            .map(|meta| (meta.len(), meta.mtime(), meta.mtime_nsec()))
    };
    let (Some(then), Some(now)) = (stamp(snapshot), stamp(live)) else {
        return 0;
    };
    if then == now {
        return 0;
    }
    let (Ok(old), Ok(new)) = (physical(snapshot), physical(live)) else {
        return 0;
    };
    let new = Ranges::of(&new);
    counted
        .lock()
        .map_or(0, |mut counted| held_alone(&old, &new, &mut counted))
}

/// Where the change log of `volume` is streamed from, and how its paths are taken onto the
/// volume: on `/` through the firmlinks for the data volume, on the volume itself otherwise.
fn stream_root(volume: &Path) -> (PathBuf, Vec<(PathBuf, PathBuf)>) {
    if volume == Path::new(DATA_VOLUME) {
        let firmlinks = ::std::fs::read_to_string("/usr/share/firmlinks")
            .map(|text| parse_firmlinks(&text))
            .unwrap_or_default();
        (PathBuf::from("/"), firmlinks)
    } else {
        (volume.to_path_buf(), Vec::new())
    }
}

/// The log's files at `volume`'s root (root only).
fn log_files_of(volume: &Path) -> Vec<super::log::LogFile> {
    let Ok(entries) = ::std::fs::read_dir(volume.join(".fseventsd")) else {
        return Vec::new();
    };
    log_files(entries.filter_map(|entry| {
        let entry = entry.ok()?;
        Some((entry.file_name(), entry.metadata().ok()?.modified().ok()?))
    }))
}

/// Each of `names` with what to read of it: what the change log named since it, where the log
/// reaches back that far and replays in time, else all of it.
fn scopes_of(
    volume: &Path,
    names: &[OsString],
    keep_going: &(dyn Fn() -> bool + Sync),
) -> Vec<(OsString, Scope)> {
    let listed: HashMap<OsString, Listed> = list_with_times(volume)
        .unwrap_or_default()
        .into_iter()
        .map(|listed| (listed.name.clone(), listed))
        .collect();
    let dataless = |name: &OsString| listed.get(name).is_some_and(|listed| listed.dataless);
    // Dataless ones hold nothing to read, and are not let pull the replay back to their time.
    let made: HashMap<OsString, SystemTime> = listed
        .values()
        .filter(|listed| !listed.dataless)
        .map(|listed| (listed.name.clone(), listed.made))
        .collect();
    let scopes = scopes_by_log(volume, names, &made, keep_going);
    scopes
        .into_iter()
        .map(|(name, scope)| {
            let scope = if dataless(&name) {
                Scope::Dataless
            } else {
                scope
            };
            (name, scope)
        })
        .collect()
}

/// Each of `names` with what the change log says to read of it, for those `made` when.
fn scopes_by_log(
    volume: &Path,
    names: &[OsString],
    made: &HashMap<OsString, SystemTime>,
    keep_going: &(dyn Fn() -> bool + Sync),
) -> Vec<(OsString, Scope)> {
    let files = log_files_of(volume);
    let now = crate::fsevents::current_event_id().unwrap_or(0);
    let sinces: Vec<(OsString, Option<u64>)> = names
        .iter()
        .map(|name| {
            let since = made
                .get(name)
                .and_then(|&made| since_for(made, &files))
                // An id past the log's own is no id of this log: read it whole.
                .filter(|&since| since <= now);
            (name.clone(), since)
        })
        .collect();
    let narrowed = sinces.iter().filter(|(_, since)| since.is_some()).count();
    let Some(earliest) = sinces.iter().filter_map(|(_, since)| *since).min() else {
        return scopes(&sinces, Err("the change log does not reach back to it"));
    };
    let (root, firmlinks) = stream_root(volume);
    let give_up = REPLAY_PER_SNAPSHOT * u32::try_from(narrowed).unwrap_or(u32::MAX);
    match crate::fsevents::events_while(&root, earliest, give_up, &keep_going) {
        crate::fsevents::Events::Events(events) => {
            let named: Vec<Named> = events
                .into_iter()
                .filter_map(|event| {
                    Some(Named {
                        relative: on_volume(&event.path, event.whole, volume, &firmlinks)?,
                        id: event.id,
                        whole: event.whole,
                    })
                })
                .collect();
            scopes(&sinces, Ok(&named))
        }
        crate::fsevents::Events::Lost => scopes(&sinces, Err("the change log's ids started over")),
        crate::fsevents::Events::GaveUp => scopes(
            &sinces,
            Err("the change log took longer to replay than reading it whole"),
        ),
    }
}

/// The snapshots' folder for the scan of `scan_root`, every one of `names` (snapshots of
/// `volume`) mounted, read where the change log says and unmounted in turn, on the walk's
/// threads, with how each was read. Root only.
pub fn read(
    scan_root: &Path,
    volume: &Path,
    names: &[OsString],
    options: ScanOptions,
    keep_going: &(dyn Fn() -> bool + Sync),
) -> Option<(FileTree, Vec<Reading>)> {
    sweep_left_behind();
    let snapshots = scopes_of(volume, names, keep_going);
    with_lookups(volume, options, |lookups| {
        let pass = Pass {
            live_root: volume,
            lookups,
            threads: crate::thread_count(options),
            keep_going,
        };
        build(scan_root, &snapshots, &|name| mount(volume, name), &pass)
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use ::std::ffi::OsStr;

    #[test]
    fn a_mount_folder_names_its_process() {
        assert_eq!(
            super::mount_owner(OsStr::new("duscape-snapshot-4242-0")),
            Some(4242)
        );
        assert_eq!(super::mount_owner(OsStr::new("duscape-snapshot-x-0")), None);
        assert_eq!(super::mount_owner(OsStr::new("duscape_scan_test")), None);
    }
}
