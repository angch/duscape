//! How far the treemap's tiles move from one outline batch to the next while a scan runs, and
//! when the finished tree replaces the outline: with the steady layout and without.
//!
//! ```text
//! DUSCAPE_LAYOUT_PATH=~ cargo test --release -p duscape-viewer --test layout_stability -- --ignored --nocapture
//! ```
//!
//! A tile *jumps* when its centre moves more than [`JUMP`] of the tile it is in: what an
//! animation would have to slide across other tiles. `DUSCAPE_LAYOUT_BURST`
//! batches are laid out at a time (default 4, about a window's 100 ms burst).

use ::std::collections::HashMap;
use ::std::ffi::OsString;
use ::std::path::{Path, PathBuf};
use ::std::sync::{Arc, atomic::AtomicBool, mpsc};
use ::std::time::{Duration, Instant};

use duscape_viewer::state::Viewer;
use libduscape::model::SizeKind;
use libduscape::tiles::Tile;
use libduscape::{DirSummary, FileTree, ScanOptions};

/// A tile's centre as a fraction of the tile it is in (the board for a top-level tile), its
/// area and its squareness.
type Geometry = HashMap<Vec<OsString>, (f64, f64, f64, f64)>;

fn geometry(tile: &Tile, parent: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    let (w, h) = (f64::from(tile.width), f64::from(tile.height));
    let (px, py, pw, ph) = parent;
    (
        (f64::from(tile.x) + w / 2.0 - px) / pw.max(1.0),
        (f64::from(tile.y) + h / 2.0 - py) / ph.max(1.0),
        w * h,
        w.min(h) / w.max(h),
    )
}

fn rect(tile: &Tile) -> (f64, f64, f64, f64) {
    (
        f64::from(tile.x),
        f64::from(tile.y),
        f64::from(tile.width),
        f64::from(tile.height),
    )
}

/// Every tile on show, by its path from the listed folder.
fn snapshot(viewer: &Viewer) -> Geometry {
    let board = viewer
        .board
        .tiles
        .iter()
        .fold((0.0, 0.0, 1.0f64, 1.0f64), |b, t| {
            (
                0.0,
                0.0,
                b.2.max(f64::from(t.x + t.width)),
                b.3.max(f64::from(t.y + t.height)),
            )
        });
    let mut all: Geometry = viewer
        .board
        .tiles
        .iter()
        .map(|tile| (vec![tile.name.clone()], geometry(tile, board)))
        .collect();
    let nested = viewer.nested();
    for (index, tile) in nested.iter().enumerate() {
        let parent = match tile.parent {
            Some(parent) => rect(&nested[parent].tile),
            None => rect(&viewer.board.tiles[tile.top]),
        };
        all.insert(viewer.nested_path(index), geometry(&tile.tile, parent));
    }
    all
}

/// A tile jumps when its centre moves more than this share of the tile it is in either way:
/// growing and shrinking in place moves it less, being cut again into another row more.
const JUMP: f64 = 0.15;

#[derive(Default)]
struct Moves {
    /// Per relayout: the share of the kept tiles' area that jumped.
    jumped: Vec<f64>,
    tiles_jumped: usize,
    tiles_kept: usize,
    /// Per relayout: the area-weighted mean of min(w, h) / max(w, h).
    squareness: Vec<f64>,
    took: Vec<Duration>,
}

impl Moves {
    fn add(&mut self, before: &Geometry, after: &Geometry, took: Duration) -> f64 {
        let (mut area, mut jumped_area, mut square) = (0.0, 0.0, 0.0);
        for (path, &(x, y, a, s)) in after {
            square += a * s;
            let Some(&(px, py, _, _)) = before.get(path) else {
                continue;
            };
            self.tiles_kept += 1;
            area += a;
            if (x - px).abs().max((y - py).abs()) > JUMP {
                self.tiles_jumped += 1;
                jumped_area += a;
            }
        }
        let total: f64 = after.values().map(|t| t.2).sum();
        let jumped = jumped_area / area.max(1.0);
        self.jumped.push(jumped);
        self.squareness.push(square / total.max(1.0));
        self.took.push(took);
        jumped
    }

    fn report(&mut self, label: &str) {
        self.jumped.sort_by(f64::total_cmp);
        self.took.sort();
        let at = |v: &[f64], q: f64| v[((v.len() - 1) as f64 * q) as usize] * 100.0;
        let n = self.jumped.len();
        let squareness = self.squareness.iter().sum::<f64>() / n as f64;
        eprintln!(
            "{label:>7}: {n} relayouts; area jumping median {:.1}%, p90 {:.1}%, p99 {:.1}%, max {:.1}%; \
             >5%: {}, >25%: {}; tiles jumping {:.1}%; squareness {:.2}; layout median {:.2?}, max {:.2?}",
            at(&self.jumped, 0.5),
            at(&self.jumped, 0.9),
            at(&self.jumped, 0.99),
            at(&self.jumped, 1.0),
            self.jumped.iter().filter(|&&j| j > 0.05).count(),
            self.jumped.iter().filter(|&&j| j > 0.25).count(),
            100.0 * self.tiles_jumped as f64 / self.tiles_kept.max(1) as f64,
            squareness,
            self.took[n / 2],
            self.took[n - 1],
        );
    }
}

/// Scan `root`: the outline's batches, then the finished tree.
fn scan(root: &Path) -> (Vec<Vec<DirSummary>>, FileTree) {
    let (batch, batches) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    duscape_viewer::scan::spawn(
        root.to_path_buf(),
        ScanOptions::default(),
        Arc::new(AtomicBool::new(true)),
        duscape_scan::Focus::default(),
        move |summaries| drop(batch.send(summaries)),
        move |tree| drop(done.send(tree)),
    );
    let tree = finished.recv().unwrap().expect("the scan finished");
    (batches.try_iter().collect(), tree)
}

/// A scan's outline laid out as a window would, `burst` batches at a time, then the tree.
fn replay(root: &Path, steady: bool, burst: usize) {
    let (batches, tree) = scan(root);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 0);
    viewer.set_steady(steady);
    viewer.set_tree_view(true);
    viewer.set_pixel_scale(1.5);
    viewer.resize(1600.0, 1000.0);
    let mut moves = Moves::default();
    let mut before = snapshot(&viewer);
    let count = batches.len();
    let mut batches = batches.into_iter().peekable();
    while batches.peek().is_some() {
        for batch in batches.by_ref().take(burst) {
            viewer.absorb_summaries(batch);
        }
        let start = Instant::now();
        viewer.catch_up();
        let took = start.elapsed();
        let after = snapshot(&viewer);
        moves.add(&before, &after, took);
        before = after;
    }
    let start = Instant::now();
    viewer.finish_scan(tree);
    let took = start.elapsed();
    let after = snapshot(&viewer);
    let mut finish = Moves::default();
    let jumped = finish.add(&before, &after, took);
    eprintln!("{count} batches, laid out {burst} at a time, 1600x1000 pt at 1.5x");
    let outcomes: Vec<usize> = libduscape::tiles::STEADY_OUTCOMES
        .iter()
        .map(|n| n.swap(0, ::std::sync::atomic::Ordering::Relaxed))
        .collect();
    eprintln!(
        "plans kept {}, dropped for new entries {}, for a hidden entry {}, for a shape {}",
        outcomes[0], outcomes[1], outcomes[2], outcomes[3]
    );
    moves.report(if steady { "steady" } else { "fresh" });
    eprintln!(
        "{:>7}  finish: {:.1}% of the area jumped, {} tiles, squareness {:.2}, layout {took:.2?}",
        "",
        jumped * 100.0,
        after.len(),
        finish.squareness[0]
    );
}

#[test]
#[ignore = "scans the folder named by DUSCAPE_LAYOUT_PATH"]
fn layout_stability() {
    let Some(root) = ::std::env::var_os("DUSCAPE_LAYOUT_PATH").map(PathBuf::from) else {
        eprintln!("DUSCAPE_LAYOUT_PATH is not set; nothing measured");
        return;
    };
    let burst = ::std::env::var("DUSCAPE_LAYOUT_BURST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    // Each replay scans for itself (a scan's batches are taken by the tree): warm, the two
    // see the same folders in much the same order.
    replay(&root, false, burst);
    replay(&root, true, burst);
}
