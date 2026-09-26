use ::std::path::PathBuf;

use clap::Parser;

use crate::error::Error;

/// Command-line options for `duscape`.
///
/// `name` fixes the identity shown by `--version`, whatever name the program was started by (a
/// link, `duscape-gui`); `about` is what `--help` opens with.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(
    name = "duscape",
    version,
    about = "Where the disk space went: a treemap of FOLDER (else this one) in the terminal, or \
             in a window (--gui, and the default when started from a desktop)",
    long_about = None
)]
pub struct Opt {
    /// The folder to scan
    pub folder: Option<PathBuf>,
    /// Open the window instead: the default when started from a desktop, with no terminal
    #[arg(long, conflicts_with = "tui")]
    pub gui: bool,
    /// The terminal viewer, even with no terminal on stdin or stdout
    #[arg(long)]
    pub tui: bool,
    /// Windows, the window: do not ask to run as administrator when the folder is a whole
    /// volume (elevated, the volume is read from its master file table)
    #[arg(long)]
    pub no_elevate: bool,
    /// Show file sizes rather than their block usage on disk
    #[arg(short, long)]
    pub apparent_size: bool,
    /// Path to config file (default: `~/.config/duscape/config.toml`)
    #[arg(short = 'c', long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Linux: walk into the read-only btrfs snapshots inside the folder too (Synology's
    /// `#snapshot`, snapper's `.snapshots`), each a whole earlier copy of what is scanned; by
    /// default they are left empty
    #[arg(long)]
    pub snapshots: bool,
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
    /// Scan headlessly and print what could not be read and why — each kind of failure with the
    /// system's error and examples of where — with the kernel, filesystem and walker, instead of
    /// starting the UI: what to send when entries fail to read
    #[arg(long)]
    pub issues: bool,
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
    #[arg(long, value_name = "N", default_value_t = duscape_scan::parallel::SHARDS)]
    pub bench_shards: usize,
    /// Shard directories by their first N path components, 0 for the whole path (default: what
    /// the app uses)
    #[arg(long, value_name = "N", default_value_t = duscape_scan::parallel::SHARD_DEPTH)]
    pub bench_shard_depth: usize,
    /// Stop descending below this depth (partial scans; the root is depth 0)
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,
    /// Scan with a single thread
    #[arg(long)]
    pub single_thread: bool,
    /// Ask the kernel for every entry even where the filesystem's metadata could be read from
    /// its device (root on ext4 on Linux; elevated on NTFS on Windows, the master file table):
    /// slower, but shows the last seconds of writes too
    #[arg(long)]
    pub no_device_read: bool,
    /// Number of scan worker threads (default: one per core; three on Linux, where a cold cache
    /// keeps them waiting on the disk)
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
}

impl Opt {
    /// How to scan, as the flags say; `apparent` from the config file as well.
    pub fn scan_options(&self, apparent: bool) -> libduscape::ScanOptions {
        libduscape::ScanOptions {
            parallel: !self.single_thread,
            threads: self.threads,
            show_apparent_size: self.apparent_size || apparent,
            max_depth: self.max_depth,
            one_file_system: self.one_file_system,
            hard_link_threshold: self.hard_link_threshold,
            read_device: !self.no_device_read,
            snapshots: self.snapshots,
        }
    }

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
