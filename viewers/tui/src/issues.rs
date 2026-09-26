//! `duscape --issues`: a scan with nothing drawn, for when entries fail to read — where it runs,
//! what the scan found, and every kind of failure with the system's error and examples of where.
//! What a problem report needs, on any machine the terminal viewer runs on.

use ::std::path::Path;
use ::std::time::Instant;

use duscape_scan::parallel;
use libduscape::{DisplayCount, DisplaySize, ScanOptions};

/// Scan `path` as the app does and print the report on stdout.
pub fn run(path: &Path, options: ScanOptions) {
    println!(
        "duscape {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        ::std::env::consts::OS,
        ::std::env::consts::ARCH
    );
    println!("  folder: {}", path.display());
    for (what, words) in duscape_scan::environment(path, options) {
        println!("  {what}: {words}");
    }
    let start = Instant::now();
    let Some((tree, failed, _, _)) = parallel::build_tree(
        path,
        options,
        parallel::SHARDS,
        parallel::SHARD_DEPTH,
        |_| true,
    ) else {
        return;
    };
    println!(
        "\nScanned {} entries, {}, in {:.1}s; {} failed to read.\n",
        DisplayCount(tree.get_total_descendants()),
        DisplaySize(tree.get_total_size() as f64),
        start.elapsed().as_secs_f64(),
        DisplayCount(failed),
    );
    print!("{}", tree.issues.report());
    // The process ends here: taking the tree apart would only cost time.
    ::std::mem::forget(tree);
}
