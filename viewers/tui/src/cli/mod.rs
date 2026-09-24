use ::std::path::PathBuf;

use clap::Parser;

use crate::error::Error;

/// Command-line options for `diskonaut`.
///
/// `name` fixes the identity shown by `--version` to the fork's, regardless of whether the program
/// was invoked as `diskonaut-angch` or the `diskonaut` alias.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(name = "diskonaut-angch", version)]
pub struct Opt {
    /// The folder to scan
    pub folder: Option<PathBuf>,
    /// Show file sizes rather than their block usage on disk
    #[arg(short, long)]
    pub apparent_size: bool,
    /// Path to config file (default: `~/.config/diskonaut/config.toml`)
    #[arg(short = 'c', long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Do not cross filesystem boundaries (like `du -x`)
    #[arg(short = 'x', long = "one-file-system")]
    pub one_file_system: bool,
    /// Windows: count hard links once for every file of at least this many bytes, in every folder.
    /// Tracking costs about 100 bytes of memory a file, so by default only the places hard links
    /// are normally made are tracked (the Windows directory, Edge, package stores such as pnpm's);
    /// run as administrator, every file is
    #[arg(long, value_name = "BYTES")]
    pub hard_link_threshold: Option<u64>,
    /// Scan headlessly and report timings instead of starting the UI
    #[arg(long)]
    pub benchmark: bool,
    /// Which part of the scan pipeline to time under `--benchmark`
    #[arg(long, value_name = "STAGE", default_value = "all")]
    pub bench_stage: crate::bench::BenchStage,
    /// Repeat each benchmark stage this many times
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub bench_repeat: u32,
    /// Under `--benchmark`, report where the tree build's time went, phase by phase
    #[arg(long)]
    pub bench_profile: bool,
    /// Tree-building threads for the `sharded` benchmark stage (default: what the app uses)
    #[arg(long, value_name = "N", default_value_t = diskonaut_scan::parallel::SHARDS)]
    pub bench_shards: usize,
    /// Shard directories by their first N path components, 0 for the whole path (default: what
    /// the app uses)
    #[arg(long, value_name = "N", default_value_t = diskonaut_scan::parallel::SHARD_DEPTH)]
    pub bench_shard_depth: usize,
    /// Stop descending below this depth (partial scans; the root is depth 0)
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,
    /// Scan with a single thread
    #[arg(long)]
    pub single_thread: bool,
    /// Number of scan worker threads (default: one per core; three on Linux, where a cold cache
    /// keeps them waiting on the disk)
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
}

impl Opt {
    /// Resolves the scan root: explicit `--folder` or the current working directory.
    pub fn resolve_folder(&self) -> Result<PathBuf, Error> {
        let folder = match &self.folder {
            Some(folder) => folder.clone(),
            None => std::env::current_dir()?,
        };
        if !folder.as_path().is_dir() {
            return Err(Error::FolderNotFound(folder.to_string_lossy().into_owned()));
        }
        Ok(folder.canonicalize().unwrap_or(folder))
    }
}

#[cfg(test)]
mod tests;
