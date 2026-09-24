//! Where a file's blocks are: which physical disk, and where on it. For the preview of a file
//! there is nothing else to show for — a binary.
//!
//! Linux only. The filesystem says where the file's extents are (`FS_IOC_FIEMAP`, which any
//! reader of the file may ask), and sysfs says what the device is: a partition is shifted by its
//! start onto its disk; a device-mapper or md device names its members, since which one holds
//! the blocks would take the volume's table, which is root's to read; a loop device names its
//! file. `whereisthis` (the companion tool) follows those layers to the end; this is the two or
//! three lines of it that fit under a treemap.

/// A few short lines about where `path`'s blocks are, or none where nothing is known.
#[must_use]
pub fn describe(path: &::std::path::Path) -> Vec<String> {
    imp::describe(path)
}

#[cfg(target_os = "linux")]
mod imp {
    use ::std::fs;
    use ::std::os::unix::fs::MetadataExt;
    use ::std::os::unix::io::AsRawFd;
    use ::std::path::{Path, PathBuf};

    use crate::format::DisplaySize;

    /// `_IOWR('f', 11, struct fiemap)`.
    const FS_IOC_FIEMAP: libc::Ioctl = 0xC020_660B_u32 as libc::Ioctl;
    const FIEMAP_EXTENT_LAST: u32 = 0x1;
    const FIEMAP_EXTENT_DATA_INLINE: u32 = 0x200;
    const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
    const PER_CALL: usize = 128;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }

    #[repr(C)]
    struct Request {
        start: u64,
        length: u64,
        flags: u32,
        mapped_extents: u32,
        extent_count: u32,
        reserved: u32,
        extents: [Extent; PER_CALL],
    }

    /// What FIEMAP said, summed up.
    struct Extents {
        count: usize,
        allocated: u64,
        covered: u64,
        shared: u64,
        inline: bool,
        contiguous: bool,
        first: u64,
        last: u64,
    }

    fn extents(path: &Path, len: u64) -> Option<Extents> {
        let file = fs::File::open(path).ok()?;
        let mut sum = Extents {
            count: 0,
            allocated: 0,
            covered: 0,
            shared: 0,
            inline: false,
            contiguous: true,
            first: u64::MAX,
            last: 0,
        };
        let mut previous_end: Option<(u64, u64)> = None;
        let mut start = 0u64;
        loop {
            // No `FIEMAP_FLAG_SYNC`: that would write the file's dirty pages out first, and a
            // preview must not touch the disk. A file being written may show fewer extents.
            let mut request = Request {
                start,
                length: u64::MAX - start,
                flags: 0,
                mapped_extents: 0,
                extent_count: PER_CALL as u32,
                reserved: 0,
                extents: [Extent::default(); PER_CALL],
            };
            // SAFETY: `request` is a live, correctly shaped `struct fiemap` with room for the
            // extents it promises, and the file is open for the duration of the call.
            let result = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &raw mut request) };
            if result != 0 {
                return None;
            }
            let mapped = (request.mapped_extents as usize).min(PER_CALL);
            if mapped == 0 {
                break;
            }
            let mut saw_last = false;
            for extent in &request.extents[..mapped] {
                sum.count += 1;
                sum.allocated += extent.length;
                sum.covered += extent.length.min(len.saturating_sub(extent.logical));
                if extent.flags & FIEMAP_EXTENT_SHARED != 0 {
                    sum.shared += extent.length;
                }
                if extent.flags & FIEMAP_EXTENT_DATA_INLINE != 0 {
                    sum.inline = true;
                } else {
                    sum.first = sum.first.min(extent.physical);
                    sum.last = sum
                        .last
                        .max(extent.physical + extent.length.saturating_sub(1));
                    if let Some((logical_end, physical_end)) = previous_end
                        && (logical_end != extent.logical || physical_end != extent.physical)
                    {
                        sum.contiguous = false;
                    }
                    previous_end = Some((
                        extent.logical + extent.length,
                        extent.physical + extent.length,
                    ));
                }
                saw_last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
            }
            if saw_last || mapped < PER_CALL {
                break;
            }
            let last = &request.extents[mapped - 1];
            let next = last.logical.saturating_add(last.length);
            if next <= start {
                break;
            }
            start = next;
        }
        Some(sum)
    }

    fn sysfs(name: &str) -> PathBuf {
        Path::new("/sys/class/block").join(name)
    }

    fn read(path: &Path) -> Option<String> {
        fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// The block device a file's `st_dev` names, by sysfs name: `sda3`, `dm-0`. None for
    /// btrfs (an anonymous device per subvolume), tmpfs, and network filesystems.
    fn device_of(dev: u64) -> Option<String> {
        #[allow(clippy::cast_possible_truncation)]
        let (major, minor) = (
            libc::major(dev as libc::dev_t),
            libc::minor(dev as libc::dev_t),
        );
        let target = fs::read_link(format!("/sys/dev/block/{major}:{minor}")).ok()?;
        Some(target.file_name()?.to_string_lossy().to_string())
    }

    fn parent_disk(partition: &str) -> Option<String> {
        let target = fs::read_link(sysfs(partition)).ok()?;
        let parent = target.parent()?.file_name()?.to_string_lossy().to_string();
        sysfs(&parent).exists().then_some(parent)
    }

    fn members(device: &str) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(sysfs(device).join("slaves"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// `QEMU HARDDISK · SSD`, what the disk is.
    fn disk_words(disk: &str) -> String {
        let dev = sysfs(disk);
        let mut words = Vec::new();
        if let Some(model) = read(&dev.join("device/model")) {
            words.push(model);
        }
        match read(&dev.join("queue/rotational")).as_deref() {
            Some("0") => words.push(
                if disk.starts_with("nvme") {
                    "NVMe"
                } else {
                    "SSD"
                }
                .to_string(),
            ),
            Some("1") => words.push("HDD".to_string()),
            _ => {}
        }
        words.join(" · ")
    }

    fn size(bytes: u64) -> String {
        DisplaySize(bytes as f64).to_string()
    }

    pub fn describe(path: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        let Ok(metadata) = fs::metadata(path) else {
            return lines;
        };
        let Some(found) = extents(path, metadata.len()) else {
            return lines;
        };
        if found.count == 0 {
            return lines;
        }

        let mut shape = if found.inline {
            "inline in the inode".to_string()
        } else if found.count == 1 {
            "1 extent".to_string()
        } else if found.contiguous {
            format!("{} extents, contiguous", found.count)
        } else {
            format!("{} extents, fragmented", found.count)
        };
        let holes = metadata.len().saturating_sub(found.covered);
        if holes > 0 {
            shape.push_str(&format!(", {} sparse", size(holes)));
        }
        if found.shared > 0 {
            shape.push_str(", blocks shared with another file");
        }
        lines.push(shape);

        let Some(device) = device_of(metadata.dev()) else {
            return lines;
        };
        if found.inline {
            return lines;
        }

        // A partition: onto its disk, shifted by its start.
        let dev = sysfs(&device);
        if dev.join("partition").exists()
            && let (Some(start), Some(disk)) = (
                read(&dev.join("start")).and_then(|s| s.parse::<u64>().ok()),
                parent_disk(&device),
            )
        {
            let start = start * 512;
            lines.push(format!("on /dev/{disk} · {}", disk_words(&disk)));
            lines.push(position(found.first + start, found.last + start));
            lines.push(format!("(in /dev/{device})"));
            return lines;
        }
        // A volume over other devices: one of them, or several.
        let below = members(&device);
        if !below.is_empty() {
            let name = read(&dev.join("dm/name"))
                .map(|n| format!("/dev/mapper/{n}"))
                .unwrap_or_else(|| format!("/dev/{device}"));
            lines.push(format!("in {name}, which is on"));
            let named: Vec<String> = below.iter().map(|d| format!("/dev/{d}")).collect();
            lines.push(named.join(", "));
            return lines;
        }
        if device.starts_with("loop")
            && let Some(file) = read(&dev.join("loop/backing_file"))
        {
            lines.push(format!("in /dev/{device}, a loop device over"));
            lines.push(file);
            return lines;
        }
        // A whole disk.
        lines.push(format!("on /dev/{device} · {}", disk_words(&device)));
        lines.push(position(found.first, found.last));
        lines
    }

    fn position(first: u64, last: u64) -> String {
        let (a, b) = (size(first), size(last));
        if a == b {
            format!("at {a} into the disk")
        } else {
            format!("at {a} to {b} into the disk")
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn describe(_path: &::std::path::Path) -> Vec<String> {
        Vec::new()
    }
}
