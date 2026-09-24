//! Headless scan benchmark (`--benchmark`).
//!
//! Runs the scan with no terminal attached and reports where the time goes. The stages nest, so
//! subtracting one from the next attributes cost to a layer:
//!
//! * `*-walk` — traversal alone, with entries dropped as they arrive.
//! * `*-tree` — traversal plus building the in-memory folder tree on the consuming thread.
//! * `pipeline` — traversal on worker threads and one tree builder on another, across a channel.
//! * `sharded` — traversal feeding several tree builders, then a merge and a replay. This is what
//!   the app runs (`parallel::build_tree`); its default shard count is the app's. On a walk-bound
//!   volume where one builder keeps pace, `sharded` at one shard reduces to `pipeline`.
//!
//! The `dua-*` stages measure the general-purpose `dua-core` walker, the others the walker the app
//! now uses. Comparing them is the point: they scan the same tree, so the difference is the walker.

use ::std::path::Path;
use ::std::sync::mpsc::{self, Receiver, SyncSender};
use ::std::thread;
use ::std::time::{Duration, Instant};

use diskonaut_scan::{parallel, scan_directories, scan_folder, thread_count};
use libdiskonaut::{DirEntries, FileTree, Folder, ScanItem, ScanOptions};

/// Which part of the scan pipeline to measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum BenchStage {
    /// The `dua-core` walk with nothing layered on top.
    DuaWalk,
    /// The `dua-core` walk feeding the folder tree.
    DuaTree,
    /// The current walk, entries counted but discarded.
    Walk,
    /// The current walk feeding the folder tree.
    Tree,
    /// The folder tree alone: entries are collected first, untimed, then fed to the model.
    TreeOnly,
    /// The current walk and tree build on separate threads, exactly as the app runs them.
    Pipeline,
    /// The walk feeding several tree builders at once, by directory, merged and reconciled at
    /// the end. What a parallel model would cost before it is wired into the app.
    Sharded,
    /// `sharded`, then the second pass over small files that may share extents: what the app
    /// shows once it has finished refining.
    Refined,
    /// ext4's inode tables read from the device (root): every live inode's size summed, no
    /// names, no tree — the floor a device-reading walker could reach. Linux, ext4, root.
    Ext4Raw,
    /// Run every stage in order.
    All,
}

const ALL_STAGES: &[BenchStage] = &[
    BenchStage::DuaWalk,
    BenchStage::DuaTree,
    BenchStage::Walk,
    BenchStage::Tree,
    BenchStage::TreeOnly,
    BenchStage::Pipeline,
    BenchStage::Sharded,
    BenchStage::Refined,
];

/// Outcome of one benchmark run.
struct StageResult {
    stage: &'static str,
    elapsed: Duration,
    entries: u64,
    failed: u64,
    total_size: u128,
    /// Distinct hard-linked files, counted once however many names point at them.
    hard_linked: usize,
    /// Distinct reflinked files, counted once however many copies share their blocks.
    reflinked: usize,
}

impl StageResult {
    fn report(&self) {
        let seconds = self.elapsed.as_secs_f64();
        let rate = if seconds > 0.0 {
            self.entries as f64 / seconds
        } else {
            0.0
        };
        let hard_linked = if self.hard_linked > 0 {
            format!("  {} hard-linked", self.hard_linked)
        } else {
            String::new()
        };
        let reflinked = if self.reflinked > 0 {
            format!("  {} reflinked", self.reflinked)
        } else {
            String::new()
        };
        println!(
            "{:<11}{:>8.3}s  {:>11} entries  {:>10.0} entries/s  {:>7} unreadable  {:>10} ({} B){}",
            self.stage,
            seconds,
            self.entries,
            rate,
            self.failed,
            human_size(self.total_size),
            self.total_size,
            format_args!("{hard_linked}{reflinked}"),
        );
    }
}

fn human_size(bytes: u128) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn new_tree(path: &Path) -> FileTree {
    FileTree::new(Folder::new(path), path.to_path_buf())
}

/// Finish a tree-building stage.
///
/// Dropping a multi-million-node tree takes longer than building it and is no part of the scan;
/// the app leaks it deliberately for the same reason.
fn finish(
    stage: &'static str,
    start: Instant,
    entries: u64,
    failed: u64,
    mut tree: FileTree,
) -> StageResult {
    let result = StageResult {
        stage,
        elapsed: start.elapsed(),
        entries,
        failed,
        total_size: tree.get_total_size(),
        hard_linked: tree.hard_linked_files(),
        reflinked: tree.reflinked_files(),
    };
    if let Some(profile) = tree.take_profile() {
        eprint!("{}", profile.report("  "));
    }
    std::mem::forget(tree);
    result
}

/// The `dua-core` walk, optionally building the tree from it.
fn bench_dua(path: &Path, options: ScanOptions, build_tree: bool) -> StageResult {
    let start = Instant::now();
    let mut tree = new_tree(path);
    let mut entries = 0u64;
    let mut failed = 0u64;
    let mut total_size = 0u128;
    for item in scan_folder(path, options) {
        match item {
            ScanItem::Entry { meta, path } => {
                entries += 1;
                if build_tree {
                    tree.add_entry(meta, &path);
                } else {
                    total_size += u128::from(meta.size);
                }
            }
            ScanItem::ReadError => failed += 1,
        }
    }
    if build_tree {
        return finish("dua-tree", start, entries, failed, tree);
    }
    StageResult {
        stage: "dua-walk",
        elapsed: start.elapsed(),
        entries,
        failed,
        total_size,
        hard_linked: 0,
        reflinked: 0,
    }
}

/// The current walk, optionally building the tree from it.
fn bench_scan(path: &Path, options: ScanOptions, build_tree: bool) -> StageResult {
    let start = Instant::now();
    let mut tree = new_tree(path);
    let mut entries = 0u64;
    let mut failed = 0u64;
    let mut total_size = 0u128;
    for directory in scan_directories(path, options) {
        entries += directory.len() as u64;
        failed += directory.failed;
        if build_tree {
            tree.add_dir_entries(directory);
        } else {
            total_size += directory
                .entries()
                .iter()
                .map(|entry| u128::from(entry.meta.size))
                .sum::<u128>();
        }
    }
    if build_tree {
        return finish("tree", start, entries, failed, tree);
    }
    StageResult {
        stage: "walk",
        elapsed: start.elapsed(),
        entries,
        failed,
        total_size,
        hard_linked: 0,
        reflinked: 0,
    }
}

/// The tree build with the walk taken out of the measurement.
///
/// `walk` against `tree` cannot separate the two on Linux, because the consuming thread drives the
/// walk iterator and a slow consumer stalls the walk's workers. Collecting every directory first
/// and timing only the model answers "what would the scan cost if the walk were free", which is
/// the floor a faster walker can reach.
fn bench_tree_only(path: &Path, options: ScanOptions) -> StageResult {
    let directories: Vec<DirEntries> = scan_directories(path, options).collect();

    let start = Instant::now();
    let mut tree = new_tree(path);
    let mut entries = 0u64;
    let mut failed = 0u64;
    for directory in directories {
        entries += directory.len() as u64;
        failed += directory.failed;
        tree.add_dir_entries(directory);
    }

    finish("tree-only", start, entries, failed, tree)
}

/// Number of entries batched into one channel message, matching the app.
const BATCH: usize = 4096;

/// Scan on worker threads and build the tree on another, across a channel, as the app does.
fn bench_pipeline(path: &Path, options: ScanOptions) -> StageResult {
    let start = Instant::now();
    let (sender, receiver): (SyncSender<Vec<DirEntries>>, Receiver<Vec<DirEntries>>) =
        mpsc::sync_channel(64);

    let scanner = thread::spawn({
        let path = path.to_path_buf();
        move || {
            let mut batch = Vec::with_capacity(128);
            let mut batched = 0usize;
            for directory in scan_directories(&path, options) {
                batched += directory.len().max(1);
                batch.push(directory);
                if batched >= BATCH {
                    batched = 0;
                    let to_send = std::mem::replace(&mut batch, Vec::with_capacity(128));
                    if sender.send(to_send).is_err() {
                        return;
                    }
                }
            }
            let _ = sender.send(batch);
        }
    });

    let mut tree = new_tree(path);
    let mut entries = 0u64;
    let mut failed = 0u64;
    while let Ok(batch) = receiver.recv() {
        for directory in batch {
            entries += directory.len() as u64;
            failed += directory.failed;
            tree.add_dir_entries(directory);
        }
    }
    let _ = scanner.join();

    finish("pipeline", start, entries, failed, tree)
}

/// The walk feeding several tree builders at once, then one merge and one reconciliation —
/// the parallel model the app uses, timed phase by phase on stderr.
fn bench_sharded(
    path: &Path,
    options: ScanOptions,
    shards: usize,
    depth: usize,
    refined: bool,
) -> StageResult {
    let start = Instant::now();
    let mut entries = 0u64;
    let (mut tree, failed, timings, small) =
        parallel::build_tree(path, options, shards, depth, |directory| {
            entries += directory.len() as u64;
            true
        })
        .expect("nothing asked the scan to stop");
    eprintln!(
        "  sharded x{shards} depth {depth}: walk+build {:.3}s  merge {:.3}s  replay {:.3}s",
        timings.built.as_secs_f64(),
        timings.merged.as_secs_f64(),
        timings.replayed.as_secs_f64(),
    );
    if !refined {
        return finish("sharded", start, entries, failed, tree);
    }
    let files = small.len();
    let second = Instant::now();
    let mut charged = 0;
    diskonaut_scan::refine::refine(
        small,
        thread_count(options),
        &::std::sync::Mutex::new(None),
        &|| true,
        |found, _| {
            charged += tree.apply_found(&found);
            true
        },
    );
    eprintln!(
        "  second pass: {files} small files probed, {charged} shared, {:.3}s",
        second.elapsed().as_secs_f64()
    );
    finish("refined", start, entries, failed, tree)
}

/// The ext4 device read of the filesystem holding `path`: the whole filesystem, not the subtree,
/// so compare it with a scan of the mount point, as root.
#[cfg(target_os = "linux")]
fn bench_ext4_raw(path: &Path) -> StageResult {
    let start = Instant::now();
    match diskonaut_scan::ext4::survey_for(path) {
        Ok(survey) => {
            eprintln!(
                "  ext4-raw: {} — {} of {} groups read, {} read in {:.3}s ({:.0} MiB/s); {} files, {} directories; apparent {} ({} B); block {} B, inode {} B",
                survey.device.display(),
                survey.groups_read,
                survey.groups,
                human_size(u128::from(survey.bytes_read)),
                survey.elapsed.as_secs_f64(),
                survey.bytes_read as f64 / 1048576.0 / survey.elapsed.as_secs_f64().max(1e-9),
                survey.files,
                survey.directories,
                human_size(u128::from(survey.apparent)),
                survey.apparent,
                survey.block_size,
                survey.inode_size,
            );
            StageResult {
                stage: "ext4-raw",
                elapsed: start.elapsed(),
                entries: survey.inodes,
                failed: 0,
                total_size: u128::from(survey.bytes_on_disk),
                hard_linked: 0,
                reflinked: 0,
            }
        }
        Err(error) => {
            eprintln!("  ext4-raw: {error}");
            StageResult {
                stage: "ext4-raw",
                elapsed: start.elapsed(),
                entries: 0,
                failed: 1,
                total_size: 0,
                hard_linked: 0,
                reflinked: 0,
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn bench_ext4_raw(_path: &Path) -> StageResult {
    eprintln!("  ext4-raw: Linux only");
    StageResult {
        stage: "ext4-raw",
        elapsed: Duration::ZERO,
        entries: 0,
        failed: 1,
        total_size: 0,
        hard_linked: 0,
        reflinked: 0,
    }
}

/// Whether the scan will read the filesystem from its device: asked for, and possible here.
#[cfg(target_os = "linux")]
fn device_read_words(path: &Path, options: ScanOptions) -> &'static str {
    if !options.read_device {
        "off (--no-device-read)"
    } else if diskonaut_scan::ext4::would_read_device(path) {
        "yes (ext4, as root)"
    } else {
        "no (not ext4, or not root)"
    }
}
#[cfg(not(target_os = "linux"))]
fn device_read_words(_path: &Path, _options: ScanOptions) -> &'static str {
    "no (Linux only)"
}

/// Run the requested benchmark stages against `path` and print a report.
pub fn run(
    path: &Path,
    stage: BenchStage,
    options: ScanOptions,
    repeat: u32,
    shards: usize,
    shard_depth: usize,
    profile: bool,
) {
    if profile {
        libdiskonaut::model::files::profile::enable();
    }
    println!("benchmarking {}", path.display());
    if let Some(used) = libdiskonaut::os::volume_used(path) {
        // What the stages' totals fall short of is what the walk could not see: unreadable
        // folders, filesystem metadata, snapshots.
        println!("  volume used: {} ({used} B)", human_size(u128::from(used)));
    }
    println!(
        "  threads: {}   apparent-size: {}   max-depth: {}   device read: {}\n",
        thread_count(options),
        options.show_apparent_size,
        options
            .max_depth
            .map_or_else(|| "unlimited".to_string(), |depth| depth.to_string()),
        device_read_words(path, options),
    );

    let stages = match stage {
        BenchStage::All => ALL_STAGES,
        BenchStage::Ext4Raw => &[BenchStage::Ext4Raw],
        other => std::slice::from_ref(
            ALL_STAGES
                .iter()
                .find(|candidate| **candidate == other)
                .expect("every stage but `all` is listed in ALL_STAGES"),
        ),
    };

    for _ in 0..repeat.max(1) {
        for stage in stages {
            let result = match stage {
                BenchStage::DuaWalk => bench_dua(path, options, false),
                BenchStage::DuaTree => bench_dua(path, options, true),
                BenchStage::Walk => bench_scan(path, options, false),
                BenchStage::Tree => bench_scan(path, options, true),
                BenchStage::TreeOnly => bench_tree_only(path, options),
                BenchStage::Pipeline | BenchStage::All => bench_pipeline(path, options),
                BenchStage::Sharded => bench_sharded(path, options, shards, shard_depth, false),
                BenchStage::Refined => bench_sharded(path, options, shards, shard_depth, true),
                BenchStage::Ext4Raw => bench_ext4_raw(path),
            };
            result.report();
        }
    }
}
