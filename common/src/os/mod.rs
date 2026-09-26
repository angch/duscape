#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{is_user_admin, link_count, set_sparse, size_on_disk_fast, volume_id, volume_used};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{
    enable_backup_privilege, is_user_admin, link_count, set_sparse, size_on_disk_fast, volume_id,
    volume_used,
};

#[cfg(test)]
mod tests;
