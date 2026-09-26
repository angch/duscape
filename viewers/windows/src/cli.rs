//! The command line: the terminal viewer's scan options, so both scan the same way.

use ::std::path::PathBuf;

use clap::Parser;
use libdiskonaut::ScanOptions;

/// A disk-usage treemap. With no folder, asks for one.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(name = "diskonaut-windows", version)]
pub struct Opt {
    /// The folder to scan
    pub folder: Option<PathBuf>,
    /// Show file sizes rather than their block usage on disk (switch with `a`)
    #[arg(short, long)]
    pub apparent_size: bool,
    /// Count hard links once for every file of at least this many bytes, in every folder. By
    /// default only the places hard links are normally made are tracked; run as administrator,
    /// every file is
    #[arg(long, value_name = "BYTES")]
    pub hard_link_threshold: Option<u64>,
    /// Stop descending below this depth (the root is depth 0)
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,
    /// Number of scan worker threads (default: two thirds of the cores, at most 12)
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
    /// Do not ask to run as administrator when the folder is a whole volume (elevated, the
    /// volume is read from its master file table, every hard link is counted and every folder
    /// opens)
    #[arg(long)]
    pub no_elevate: bool,
    /// The window rather than the terminal viewer, which `diskonaut` has already chosen
    #[arg(long, hide = true)]
    pub gui: bool,
}

impl Opt {
    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            show_apparent_size: self.apparent_size,
            hard_link_threshold: self.hard_link_threshold,
            max_depth: self.max_depth,
            threads: self.threads,
            ..ScanOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Opt;

    #[test]
    fn the_scan_options_come_from_the_flags() {
        let opt = Opt::try_parse_from(["diskonaut-windows", "-a", "--max-depth", "3", r"C:\data"])
            .expect("parses");
        assert_eq!(
            opt.folder.as_deref(),
            Some(::std::path::Path::new(r"C:\data"))
        );
        let options = opt.scan_options();
        assert!(options.show_apparent_size);
        assert_eq!(options.max_depth, Some(3));
        assert!(
            options.parallel,
            "the walk is parallel as in the terminal viewer"
        );
        assert!(Opt::try_parse_from(["diskonaut-windows", "--bogus"]).is_err());
        assert!(!opt.no_elevate);
        assert!(
            Opt::try_parse_from(["diskonaut-windows", crate::elevate::NO_ELEVATE])
                .expect("parses")
                .no_elevate
        );
    }
}
