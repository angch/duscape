//! Walk through open directories, never through queued paths: another process can rename a
//! child and put a symlink in its place between discovery and deletion.
use std::ffi::CString;
use std::io;
use std::path::Path;

use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, Dir, Mode, OFlags};

use super::Tally;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

struct Frame {
    fd: OwnedFd,
    entries: Dir,
    name: CString,
}

fn open(parent: &OwnedFd, name: &std::ffi::CStr) -> io::Result<Frame> {
    let fd = fs::openat(parent, name, DIRECTORY, Mode::empty())?;
    let entries = Dir::read_from(&fd)?;
    Ok(Frame {
        fd,
        entries,
        name: name.to_owned(),
    })
}

pub(super) fn remove_tree(root: &Path, tally: &Tally) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let name = root
        .file_name()
        .ok_or_else(|| io::Error::other("cannot remove a filesystem root"))?;
    let name = CString::new(name.as_bytes())?;
    let parent = fs::open(
        root.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        DIRECTORY,
        Mode::empty(),
    )?;
    walk(&parent, &name, tally, &mut || {})
}

fn walk(
    parent: &OwnedFd,
    name: &std::ffi::CStr,
    tally: &Tally,
    before_open: &mut impl FnMut(),
) -> io::Result<()> {
    let mut stack = vec![open(parent, name)?];
    while let Some(frame) = stack.last_mut() {
        if tally.stopped() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        if let Some(entry) = frame.entries.next() {
            let entry = entry?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            // unlinkat never follows a symlink. Try it first, avoiding a separate stat and
            // its race; only a directory requires opening and descending.
            match fs::unlinkat(&frame.fd, name, AtFlags::empty()) {
                Ok(()) => tally.add(1),
                Err(rustix::io::Errno::ISDIR | rustix::io::Errno::PERM) => {
                    before_open();
                    let child = open(&frame.fd, name)?;
                    stack.push(child);
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            let done = stack.pop().expect("a frame is in hand");
            let parent = stack.last().map_or(parent, |frame| &frame.fd);
            fs::unlinkat(parent, &done.name, AtFlags::REMOVEDIR)?;
            tally.add(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_replaced_by_a_symlink_is_not_followed() {
        let root = std::env::temp_dir().join(format!("duscape-delete-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("selected/child")).unwrap();
        std::fs::create_dir(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/keep"), b"keep").unwrap();
        let parent = fs::open(&root, DIRECTORY, Mode::empty()).unwrap();
        let result = walk(&parent, c"selected", &Tally::default(), &mut || {
            std::fs::rename(root.join("selected/child"), root.join("moved")).unwrap();
            std::os::unix::fs::symlink(root.join("outside"), root.join("selected/child")).unwrap();
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(root.join("outside/keep")).unwrap(), b"keep");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_bare_relative_directory_uses_the_current_directory() {
        let root =
            std::path::PathBuf::from(format!(".duscape-delete-relative-{}", std::process::id()));
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("child/file"), b"x").unwrap();
        let tally = Tally::default();
        remove_tree(&root, &tally).unwrap();
        assert_eq!(tally.removed(), 3);
        assert!(!root.exists());
    }
}
