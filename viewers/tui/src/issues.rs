//! `duscape --issues`: a scan with nothing drawn, for when entries fail to read — where it runs,
//! what the scan found, and every kind of failure with the system's error and examples of where.
//! What a problem report needs, on any machine the terminal viewer runs on.

use ::std::path::Path;
use ::std::time::Instant;

use duscape_scan::parallel;
use libduscape::{DisplayCount, DisplaySize, ScanOptions};

/// Scan `path` as the app does and print the report on stdout.
pub fn run(path: &Path, mut options: ScanOptions) {
    // A report on what the disk holds now, not on what was saved.
    options.cache = libduscape::Cache::Off;
    println!(
        "duscape {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        ::std::env::consts::OS,
        ::std::env::consts::ARCH
    );
    println!("  folder: {}", libduscape::format::shown_path(path));
    for (what, words) in duscape_scan::environment(path, options) {
        println!("  {what}: {words}");
    }
    let start = Instant::now();
    let Some((tree, failed, _, _)) = parallel::build_tree(
        path,
        options,
        parallel::SHARDS,
        parallel::SHARD_DEPTH,
        &duscape_scan::Focus::default(),
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
    snapshots(path, options);
    // The process ends here: taking the tree apart would only cost time.
    ::std::mem::forget(tree);
}

/// The volume's local snapshots, and as root what each holds that the disk does not: how each
/// was mounted and read, which the viewers do not say.
fn snapshots(path: &Path, options: ScanOptions) {
    use duscape_scan::snapshots;
    let Some(volume) = snapshots::volume_for(path) else {
        return;
    };
    let names = snapshots::list(&volume).unwrap_or_default();
    println!(
        "\nLocal snapshots of {}: {}",
        volume.display(),
        DisplayCount(names.len() as u64)
    );
    if names.is_empty() {
        return;
    }
    println!(
        "  full disk access: {}",
        if snapshots::full_disk_access() {
            "yes"
        } else {
            "no — mounting a snapshot needs it, as root too (System Settings ▸ Privacy & Security \
             ▸ Full Disk Access, for the terminal)"
        }
    );
    if !snapshots::can_read() {
        for name in &names {
            println!("  {}", name.to_string_lossy());
        }
        println!("  not read: reading a snapshot needs root");
        return;
    }
    let start = Instant::now();
    let Some((tree, readings)) = snapshots::read(path, &volume, &names, options, &|| true) else {
        println!("  not read: the volume could not be asked");
        return;
    };
    for reading in &readings {
        println!("  {}", reading.describe());
    }
    println!(
        "  held by the snapshots alone: {}, in {:.1}s",
        DisplaySize(tree.get_total_size() as f64),
        start.elapsed().as_secs_f64()
    );
    println!("  where, largest first:");
    largest(tree.get_current_folder(), 2, 0);
    ::std::mem::forget(tree);
}

/// The largest of `folder`'s entries, and theirs in turn: a few a level, to six levels, each
/// of a hundredth of the folder above or more — where what they hold is.
fn largest(folder: &libduscape::Folder, indent: usize, depth: usize) {
    use libduscape::FileOrFolder;
    use libduscape::model::SizeKind;
    const SHOWN: usize = 6;
    const DEPTH: usize = 6;
    let floor = folder.sizes.disk / 100;
    let mut entries: Vec<_> = folder.contents.iter().collect();
    entries.sort_by_key(|(_, entry)| ::std::cmp::Reverse(entry.size(SizeKind::Disk)));
    for (name, entry) in entries.into_iter().take(SHOWN) {
        let size = entry.size(SizeKind::Disk);
        if size == 0 || (depth > 0 && size < floor) {
            break;
        }
        println!(
            "{:indent$}{} {}",
            "",
            DisplaySize(size as f64),
            name.to_string_lossy()
        );
        if let FileOrFolder::Folder(inner) = entry
            && depth < DEPTH
        {
            largest(inner, indent + 2, depth + 1);
        }
    }
}
