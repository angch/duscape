//! Where a file's blocks are: which physical disk, and where on it. For the preview of a file
//! there is nothing else to show for — a binary.
//!
//! On Linux the filesystem says where the file's extents are (`FS_IOC_FIEMAP`, which any
//! reader of the file may ask), and sysfs says what the device is: a partition is shifted by its
//! start onto its disk; a device-mapper or md device names its members, since which one holds
//! the blocks would take the volume's table, which is root's to read; a loop device names its
//! file. `whereisthis` (the companion tool) follows those layers to the end; this is the two or
//! three lines of it that fit under a treemap.
//!
//! On Windows the same comes from `FSCTL_GET_RETRIEVAL_POINTERS` (the file's runs, in clusters
//! of its volume; none for a file resident in its MFT record), the volume's extent on its
//! physical disk (`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`, on the volume opened with no access
//! rights, which needs no elevation) and the disk's model, bus and seek penalty
//! (`IOCTL_STORAGE_QUERY_PROPERTY`). A volume over several disks names them, as an LVM volume's
//! members are named on Linux.

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

/// A file's runs on Windows, summed up: what `FSCTL_GET_RETRIEVAL_POINTERS` gives, as
/// `(next_vcn, lcn)` pairs from virtual cluster 0, in bytes. Its own module, with no Win32 in
/// it, so that it is tested on every platform.
#[cfg(any(windows, test))]
mod runs {
    /// The runs summed up: the allocated ones, whether each begins where the last ended, and
    /// the first and last byte they cover on the volume.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct Runs {
        pub count: usize,
        pub allocated: u64,
        pub contiguous: bool,
        pub first: u64,
        pub last: u64,
    }

    /// `runs` in clusters of `cluster` bytes; an LCN below zero is a hole (a sparse file's), which
    /// is no run of its own and parts the runs either side of it.
    pub(super) fn sum(runs: &[(i64, i64)], cluster: u64) -> Runs {
        let mut sum = Runs {
            count: 0,
            allocated: 0,
            contiguous: true,
            first: u64::MAX,
            last: 0,
        };
        let mut vcn = 0i64;
        let mut end: Option<u64> = None;
        let mut hole_since_last = false;
        for &(next, lcn) in runs {
            let clusters = u64::try_from(next - vcn).unwrap_or(0);
            vcn = next;
            if lcn < 0 {
                hole_since_last = sum.count > 0;
                continue;
            }
            let lcn = u64::try_from(lcn).unwrap_or(0);
            sum.count += 1;
            sum.allocated += clusters * cluster;
            sum.first = sum.first.min(lcn * cluster);
            sum.last = sum.last.max(((lcn + clusters) * cluster).saturating_sub(1));
            if hole_since_last || end.is_some_and(|end| end != lcn) {
                sum.contiguous = false;
            }
            end = Some(lcn + clusters);
            hole_since_last = false;
        }
        sum
    }

    #[cfg(test)]
    mod tests {
        use super::{Runs, sum};

        #[test]
        fn runs_are_summed_in_bytes_and_read_for_adjacency() {
            assert_eq!(
                sum(&[(4, 100)], 4096),
                Runs {
                    count: 1,
                    allocated: 4 * 4096,
                    contiguous: true,
                    first: 100 * 4096,
                    last: 104 * 4096 - 1,
                }
            );
            assert!(
                sum(&[(4, 100), (6, 104)], 4096).contiguous,
                "one after the other"
            );
            let apart = sum(&[(4, 100), (6, 200)], 4096);
            assert!(!apart.contiguous && apart.count == 2 && apart.last == 202 * 4096 - 1);
            let holed = sum(&[(4, 100), (8, -1), (10, 104)], 4096);
            assert_eq!(
                (holed.count, holed.allocated, holed.contiguous),
                (2, 6 * 4096, false),
                "a hole is no run, and parts the runs around it"
            );
            assert_eq!(sum(&[], 4096).count, 0);
        }
    }
}

#[cfg(windows)]
mod imp {
    use ::std::ffi::{OsString, c_void};
    use ::std::fs;
    use ::std::mem::size_of;
    use ::std::os::windows::ffi::{OsStrExt, OsStringExt};
    use ::std::os::windows::fs::MetadataExt;
    use ::std::os::windows::io::AsRawHandle;
    use ::std::path::Path;
    use ::std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_HANDLE_EOF, ERROR_MORE_DATA, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BusTypeAta, BusTypeNvme, BusTypeRAID, BusTypeSata, BusTypeSd, BusTypeUsb,
        FILE_ATTRIBUTE_COMPRESSED, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceW,
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, FSCTL_GET_RETRIEVAL_POINTERS,
        IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, RETRIEVAL_POINTERS_BUFFER_0,
        STARTING_VCN_INPUT_BUFFER, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
    };

    use super::runs;
    use crate::format::DisplaySize;

    // `windows-sys` puts `CreateFileW` behind `Win32_Security` (for the security attributes
    // it takes, unused here); declared directly, as `os::windows` does.
    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *mut c_void,
        ) -> HANDLE;
    }

    const PER_CALL: usize = 128;

    /// `RETRIEVAL_POINTERS_BUFFER` with room for [`PER_CALL`] extents.
    #[repr(C)]
    struct Pointers {
        count: u32,
        starting_vcn: i64,
        extents: [RETRIEVAL_POINTERS_BUFFER_0; PER_CALL],
    }

    /// `VOLUME_DISK_EXTENTS` with room for a volume over eight disks.
    #[repr(C)]
    struct VolumeExtents {
        count: u32,
        extents: [DISK_EXTENT; 8],
    }

    fn size(bytes: u64) -> String {
        DisplaySize(bytes as f64).to_string()
    }

    /// `DeviceIoControl` on `handle`: `Ok` with the bytes returned, `Err` with the error.
    fn control<I, O>(
        handle: HANDLE,
        code: u32,
        input: Option<&I>,
        out: &mut O,
    ) -> Result<u32, u32> {
        let mut returned = 0u32;
        let (input_ptr, input_len) = match input {
            Some(input) => (
                ::std::ptr::from_ref(input).cast::<c_void>(),
                size_of::<I>() as u32,
            ),
            None => (::std::ptr::null(), 0),
        };
        // SAFETY: `input` and `out` are live for the call and as long as the lengths say;
        // `handle` is open, held by the caller.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                code,
                input_ptr,
                input_len,
                ::std::ptr::from_mut(out).cast::<c_void>(),
                size_of::<O>() as u32,
                &raw mut returned,
                null_mut(),
            )
        };
        if ok != 0 {
            Ok(returned)
        } else {
            // SAFETY: no preconditions; read before anything else can overwrite it.
            Err(unsafe { GetLastError() })
        }
    }

    /// The file's runs, `(next_vcn, lcn)` from virtual cluster 0; empty for one resident in
    /// its MFT record; `None` when the filesystem does not say (FAT, a share).
    fn runs_of(file: &fs::File) -> Option<Vec<(i64, i64)>> {
        let handle = file.as_raw_handle().cast::<c_void>();
        let mut runs = Vec::new();
        let mut start = 0i64;
        loop {
            let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: start };
            let mut out = Pointers {
                count: 0,
                starting_vcn: 0,
                extents: [RETRIEVAL_POINTERS_BUFFER_0 { NextVcn: 0, Lcn: 0 }; PER_CALL],
            };
            let result = control(handle, FSCTL_GET_RETRIEVAL_POINTERS, Some(&input), &mut out);
            match result {
                Ok(_) | Err(ERROR_MORE_DATA) => {}
                Err(ERROR_HANDLE_EOF) => break,
                Err(_) => return None,
            }
            let count = (out.count as usize).min(PER_CALL);
            runs.extend(out.extents[..count].iter().map(|e| (e.NextVcn, e.Lcn)));
            if result.is_ok() || count == 0 {
                break;
            }
            let next = out.extents[count - 1].NextVcn;
            if next <= start {
                break;
            }
            start = next;
        }
        Some(runs)
    }

    /// The root of the volume holding `path` (`C:\`, or a mounted folder), NUL-terminated.
    fn volume_root(path: &Path) -> Option<Vec<u16>> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut root = [0u16; 1024];
        // SAFETY: `wide` is NUL-terminated and `root` is as long as the length passed.
        if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return None;
        }
        let len = root.iter().position(|&c| c == 0)?;
        Some(root[..=len].to_vec())
    }

    fn cluster_size(root: &[u16]) -> Option<u64> {
        let (mut per_cluster, mut per_sector) = (0u32, 0u32);
        // SAFETY: `root` is NUL-terminated, the two outputs are live, and the counts may be null.
        let ok = unsafe {
            GetDiskFreeSpaceW(
                root.as_ptr(),
                &raw mut per_cluster,
                &raw mut per_sector,
                null_mut(),
                null_mut(),
            )
        };
        (ok != 0 && per_cluster > 0 && per_sector > 0)
            .then(|| u64::from(per_cluster) * u64::from(per_sector))
    }

    /// A device opened for control alone (no access rights, so no elevation), closed on drop.
    struct Device(HANDLE);

    impl Device {
        fn open(name: &[u16]) -> Option<Self> {
            // SAFETY: `name` is NUL-terminated; the handle is closed by `drop`.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null_mut(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            (handle != INVALID_HANDLE_VALUE).then_some(Device(handle))
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: opened by `open`, closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(::std::iter::once(0)).collect()
    }

    /// Where the volume at `root` lies: on one disk, at an offset into it, or over several.
    fn volume_extents(root: &[u16]) -> Option<Result<(u32, u64), Vec<u32>>> {
        let mut name = [0u16; 64];
        // SAFETY: `root` is NUL-terminated and `name` is as long as the length passed.
        if unsafe {
            GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
        } == 0
        {
            return None;
        }
        // `\\?\Volume{…}\`: the volume itself is that without its trailing backslash.
        let len = name.iter().position(|&c| c == 0)?;
        let mut name = name[..len].to_vec();
        if name.last() == Some(&u16::from(b'\\')) {
            name.pop();
        }
        name.push(0);
        let volume = Device::open(&name)?;
        let mut out = VolumeExtents {
            count: 0,
            extents: [DISK_EXTENT {
                DiskNumber: 0,
                StartingOffset: 0,
                ExtentLength: 0,
            }; 8],
        };
        match control::<(), _>(
            volume.0,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            &mut out,
        ) {
            Ok(_) | Err(ERROR_MORE_DATA) => {}
            Err(_) => return None,
        }
        let count = (out.count as usize).min(8);
        match out.extents[..count] {
            [] => None,
            [only] => Some(Ok((
                only.DiskNumber,
                u64::try_from(only.StartingOffset).unwrap_or(0),
            ))),
            ref several => {
                let mut disks: Vec<u32> = several.iter().map(|e| e.DiskNumber).collect();
                disks.sort_unstable();
                disks.dedup();
                Some(Err(disks))
            }
        }
    }

    /// A NUL-terminated ASCII string at `offset` into `bytes`, trimmed; none if empty.
    fn string_at(bytes: &[u8], offset: u32) -> Option<String> {
        let start = usize::try_from(offset).ok()?;
        if start == 0 || start >= bytes.len() {
            return None;
        }
        let rest = &bytes[start..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let text = String::from_utf8_lossy(&rest[..end]).trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    /// The disk's model, bus and kind: `SAMSUNG MZVL2512HCJQ · NVMe`, `ST2000DM008 · SATA HDD`.
    fn disk_words(disk: u32) -> String {
        let mut words = Vec::new();
        let Some(device) = Device::open(&wide(&format!("\\\\.\\PhysicalDrive{disk}"))) else {
            return String::new();
        };
        #[repr(C, align(8))]
        struct Buffer([u8; 1024]);
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut buffer = Buffer([0; 1024]);
        let mut bus = None;
        if let Ok(returned) = control(
            device.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query),
            &mut buffer,
        ) && returned as usize >= size_of::<STORAGE_DEVICE_DESCRIPTOR>()
        {
            // SAFETY: the buffer is 8-aligned and the call filled at least a descriptor's worth.
            let descriptor =
                unsafe { buffer.0.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>().read() };
            let bytes = &buffer.0[..(returned as usize).min(buffer.0.len())];
            let vendor = string_at(bytes, descriptor.VendorIdOffset);
            let product = string_at(bytes, descriptor.ProductIdOffset);
            if let Some(model) = match (vendor, product) {
                (Some(vendor), Some(product)) => Some(format!("{vendor} {product}")),
                (vendor, product) => vendor.or(product),
            } {
                words.push(model);
            }
            bus = Some(descriptor.BusType);
        }
        let seek = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut penalty = DEVICE_SEEK_PENALTY_DESCRIPTOR {
            Version: 0,
            Size: 0,
            IncursSeekPenalty: 0,
        };
        let seeks = control(
            device.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&seek),
            &mut penalty,
        )
        .ok()
        .map(|_| penalty.IncursSeekPenalty != 0);
        let bus_name = match bus {
            Some(b) if b == BusTypeNvme => Some("NVMe"),
            Some(b) if b == BusTypeSata || b == BusTypeAta => Some("SATA"),
            Some(b) if b == BusTypeUsb => Some("USB"),
            Some(b) if b == BusTypeSd => Some("SD"),
            Some(b) if b == BusTypeRAID => Some("RAID"),
            _ => None,
        };
        let kind = match (bus_name, seeks) {
            (Some("NVMe"), _) => Some("NVMe".to_string()),
            (Some(bus), Some(true)) => Some(format!("{bus} HDD")),
            (Some(bus), Some(false)) => Some(format!("{bus} SSD")),
            (Some(bus), None) => Some(bus.to_string()),
            (None, Some(true)) => Some("HDD".to_string()),
            (None, Some(false)) => Some("SSD".to_string()),
            (None, None) => None,
        };
        words.extend(kind);
        words.join(" · ")
    }

    pub fn describe(path: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        let Ok(metadata) = fs::metadata(path) else {
            return lines;
        };
        let len = metadata.len();
        if len == 0 {
            return lines;
        }
        let Some(root) = volume_root(path) else {
            return lines;
        };
        let Some(cluster) = cluster_size(&root) else {
            return lines;
        };
        let Some(runs) = fs::File::open(path).ok().as_ref().and_then(runs_of) else {
            return lines;
        };
        let found = runs::sum(&runs, cluster);
        let resident = found.count == 0;
        let mut shape = if resident {
            "resident in the MFT".to_string()
        } else if found.count == 1 {
            "1 extent".to_string()
        } else if found.contiguous {
            format!("{} extents, contiguous", found.count)
        } else {
            format!("{} extents, fragmented", found.count)
        };
        // A compressed file's runs hold fewer bytes than its length too, so it is not called
        // sparse on that account.
        let compressed = metadata.file_attributes() & FILE_ATTRIBUTE_COMPRESSED != 0;
        let holes = len.saturating_sub(found.allocated);
        if !resident && !compressed && holes > 0 {
            shape.push_str(&format!(", {} sparse", size(holes)));
        }
        if compressed {
            shape.push_str(", compressed");
        }
        lines.push(shape);
        if resident {
            return lines;
        }

        let Some(extents) = volume_extents(&root) else {
            return lines;
        };
        let mut in_volume = OsString::from_wide(&root[..root.len() - 1])
            .to_string_lossy()
            .into_owned();
        if in_volume.len() > 2 && in_volume.ends_with('\\') {
            in_volume.pop();
        }
        match extents {
            Ok((disk, offset)) => {
                let words = disk_words(disk);
                lines.push(if words.is_empty() {
                    format!("on disk {disk}")
                } else {
                    format!("on disk {disk} · {words}")
                });
                lines.push(position(found.first + offset, found.last + offset));
                lines.push(format!("(in {in_volume})"));
            }
            Err(disks) => {
                let named: Vec<String> = disks.iter().map(|d| format!("disk {d}")).collect();
                lines.push(format!("in {in_volume}, a volume over"));
                lines.push(named.join(", "));
            }
        }
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

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn describe(_path: &::std::path::Path) -> Vec<String> {
        Vec::new()
    }
}
