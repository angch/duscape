//! What `duscape --issues` says about the machine a Linux scan runs on.

use ::std::path::Path;

use ::rustix::fs::{AtFlags, StatxFlags};

use super::mounts;

/// For `duscape --issues`: the kernel, whether its `statx` answers (and what stands in when it
/// does not), the filesystem `root` is on, and whether the scan runs as root.
pub fn environment(root: &Path) -> Vec<(&'static str, String)> {
    let kernel = ::std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_string())
        .unwrap_or_else(|error| format!("unknown ({error})"));
    let asked = ::rustix::fs::statx(
        rustix::fs::CWD,
        root,
        AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    );
    let mount_roots = match &asked {
        Ok(stat)
            if stat
                .stx_attributes_mask
                .contains(::rustix::fs::StatxAttributes::MOUNT_ROOT)
                && ::std::env::var_os("DUSCAPE_NO_STATX").is_none() =>
        {
            "from statx".to_string()
        }
        _ => "from /proc/self/mountinfo (the kernel does not say, before Linux 5.8)".to_string(),
    };
    let statx_words = match asked {
        Ok(_) if ::std::env::var_os("DUSCAPE_NO_STATX").is_some() => {
            "not used (DUSCAPE_NO_STATX): sizes come from fstatat".to_string()
        }
        Ok(_) => "available".to_string(),
        Err(::rustix::io::Errno::NOSYS) => {
            "missing (the kernel is older than 4.11): sizes come from fstatat".to_string()
        }
        Err(::rustix::io::Errno::PERM) => {
            "refused (EPERM, a seccomp filter): sizes come from fstatat".to_string()
        }
        Err(error) => format!(
            "fails on the folder itself: {}",
            ::std::io::Error::from(error)
        ),
    };
    let filesystem = mounts::read()
        .and_then(|table| {
            let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            // The last of the longest mount points above the folder is what it is on.
            table
                .into_iter()
                .filter(|mount| root.starts_with(&mount.point))
                .max_by_key(|mount| mount.point.as_os_str().len())
                .map(|mount| format!("{} (mounted at {})", mount.fstype, mount.point.display()))
        })
        .unwrap_or_else(|| "unknown (no /proc/self/mountinfo)".to_string());
    let user = if ::rustix::process::geteuid().is_root() {
        "root".to_string()
    } else {
        format!("uid {}", ::rustix::process::geteuid().as_raw())
    };
    vec![
        ("kernel", kernel),
        ("statx", statx_words),
        ("mount points", mount_roots),
        ("filesystem", filesystem),
        ("running as", user),
    ]
}
