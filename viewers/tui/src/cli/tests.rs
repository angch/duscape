use ::std::path::PathBuf;

use clap::{CommandFactory, Parser};

use super::Opt;
use crate::error::Error;

#[test]
fn parses_defaults() {
    let opt = Opt::parse_from(["diskonaut"]);
    assert_eq!(opt.folder, None);
    assert!(!opt.apparent_size);
    assert_eq!(opt.config, None);
    assert!(!opt.benchmark);
    assert_eq!(opt.max_depth, None);
    assert_eq!(opt.threads, None);
}

#[test]
fn parses_apparent_size_and_folder() {
    let opt = Opt::parse_from(["diskonaut", "-a", "/tmp"]);
    assert_eq!(opt.folder, Some(PathBuf::from("/tmp")));
    assert!(opt.apparent_size);
    assert_eq!(opt.config, None);
}

#[test]
fn parses_long_flags() {
    let opt = Opt::parse_from(["diskonaut", "--apparent-size", "/var"]);
    assert!(opt.apparent_size);
    assert_eq!(opt.folder, Some(PathBuf::from("/var")));
}

#[test]
fn resolve_folder_errors_for_missing_path() {
    let opt = Opt::parse_from(["diskonaut", "/nonexistent_diskonaut_test_path_9f3c2a"]);
    let err = opt.resolve_folder().unwrap_err();
    assert!(matches!(err, Error::FolderNotFound(_)));
}

#[test]
fn cli_definition_is_valid() {
    Opt::command().debug_assert();
}

#[test]
fn resolve_folder_resolves_symlinks() {
    let dir = std::env::temp_dir().join("diskonaut_cli_symlink_target");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let link = std::env::temp_dir().join("diskonaut_cli_symlink_link");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir(&link);
    let res = {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, &link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&dir, &link)
        }
    };
    if let Err(e) = res {
        // Windows grants symlink creation only in Developer Mode or elevated
        // (ERROR_PRIVILEGE_NOT_HELD, 1314): not this machine's to test, then.
        if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1314) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        panic!("create symlink: {e}");
    }

    let opt = Opt::parse_from(["diskonaut", link.to_str().unwrap()]);
    let resolved = opt.resolve_folder().expect("resolve folder");
    let expected = dir.canonicalize().unwrap();
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(resolved, expected);
}
