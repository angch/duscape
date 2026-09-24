use thiserror::Error;

/// Errors from scan, model, and OS helpers in `libdiskonaut`.
#[derive(Debug, Error)]
pub enum DiskonautError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
