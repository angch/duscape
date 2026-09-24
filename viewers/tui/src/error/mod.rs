use ::std::path::PathBuf;

use thiserror::Error;

use crate::config::ConfigError;

/// Errors surfaced at the `diskonaut` binary boundary.
#[derive(Debug, Error)]
pub enum Error {
    #[error("Folder '{0}' does not exist")]
    FolderNotFound(String),

    #[error("Failed to get stdout: are you trying to pipe 'diskonaut'?")]
    NoStdout,

    #[error("config error in {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests;
