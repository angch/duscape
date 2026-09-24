use ::std::fs::File;
use ::std::io::Write;

#[test]
fn is_user_admin_returns_bool() {
    let _ = crate::os::is_user_admin();
}

#[test]
fn size_on_disk_fast_is_at_least_file_length() {
    let dir = std::env::temp_dir().join("diskonaut_os_block_size_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let file_path = dir.join("data.bin");
    let mut file = File::create(&file_path).expect("create file");
    file.write_all(&[0u8; 1024]).expect("write file");

    let metadata = std::fs::metadata(&file_path).expect("stat file");
    let on_disk = crate::os::size_on_disk_fast(&metadata);

    assert!(
        on_disk >= 1024,
        "on-disk size {on_disk} should cover 1024 logical bytes"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Only a volume's root is comparable with the volume's usage; a folder inside it is not.
///
/// Worked out from a folder made for the test, never from the temp directory itself: on the many
/// desktops that mount `/tmp` as tmpfs, the temp directory *is* a volume root.
#[test]
fn volume_used_is_reported_for_a_volume_root_only() {
    let dir = std::env::temp_dir().join("diskonaut_os_volume_used_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let dir = dir.canonicalize().expect("canonicalize temp dir");
    let inside = crate::os::volume_used(&dir);

    // The mount the folder is on: up from it while the device stays the same — `/tmp` itself
    // where that is a tmpfs, the root filesystem where it is not.
    let device = crate::os::volume_id(&dir).expect("device of the temp dir");
    let mount = dir
        .ancestors()
        .take_while(|path| crate::os::volume_id(path) == Some(device))
        .last()
        .expect("the folder itself at least")
        .to_path_buf();
    let used = crate::os::volume_used(&mount);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(inside, None, "a folder is not a volume");
    assert!(
        used.is_some(),
        "{} is where its volume is mounted",
        mount.display()
    );
}

/// Unelevated, the privilege is not held; the call must say so rather than fail.
#[cfg(windows)]
#[test]
fn enabling_the_backup_privilege_does_not_fail() {
    let _ = crate::os::enable_backup_privilege();
}
