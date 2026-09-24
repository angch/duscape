use super::Error;

#[test]
fn folder_not_found_display() {
    let err = Error::FolderNotFound("/nope".into());
    assert_eq!(err.to_string(), "Folder '/nope' does not exist");
}

#[test]
fn no_stdout_display() {
    let err = Error::NoStdout;
    assert_eq!(
        err.to_string(),
        "Failed to get stdout: are you trying to pipe 'diskonaut'?"
    );
}
