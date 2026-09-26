use thiserror::Error;

/// Errors from scan, model, and OS helpers in `libduscape`.
#[derive(Debug, Error)]
pub enum DuscapeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
