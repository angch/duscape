//! How long the desktop viewers' treemap takes to lay out on a real disk: the board and its
//! nesting, which every resize, folder change and outline batch redoes on the window's thread.
//!
//! ```text
//! DUSCAPE_LAYOUT_PATH='E:\' cargo test --release -p duscape-viewer --test layout_speed -- --ignored --nocapture
//! ```
//!
//! `DUSCAPE_LAYOUT_SCALE` sets the pixels per point (default 1.5).

use ::std::path::PathBuf;
use ::std::time::Instant;

use duscape_scan::{ScanOptions, scan_into_tree};
use duscape_viewer::state::Viewer;
use libduscape::model::SizeKind;

#[test]
#[ignore = "scans the folder named by DUSCAPE_LAYOUT_PATH"]
fn layout_speed() {
    let Some(root) = ::std::env::var_os("DUSCAPE_LAYOUT_PATH").map(PathBuf::from) else {
        eprintln!("DUSCAPE_LAYOUT_PATH is not set; nothing measured");
        return;
    };
    let scale: f64 = ::std::env::var("DUSCAPE_LAYOUT_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.5);
    let start = Instant::now();
    let (tree, _) = scan_into_tree(&root, ScanOptions::default());
    eprintln!(
        "scanned {} entries in {:.1?}",
        tree.get_total_descendants(),
        start.elapsed()
    );
    let mut viewer = Viewer::new(&root, SizeKind::Disk, 0);
    viewer.set_tree_view(true);
    viewer.set_pixel_scale(scale);
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    for (width, height) in [(1200.0, 800.0), (1600.0, 1000.0), (2560.0, 1400.0)] {
        let mut best = f64::MAX;
        for round in 0..5 {
            // A size a point off each round, so the board is laid out again every time.
            let start = Instant::now();
            viewer.resize(width + f64::from(round), height);
            best = best.min(start.elapsed().as_secs_f64());
        }
        eprintln!(
            "{width}x{height} pt at {scale}x: {} tiles, {} nested, {} in the corner, deepest {}, best {:.2} ms",
            viewer.board.tiles.len(),
            viewer.nested().len(),
            viewer.dust().len(),
            viewer.nested().iter().map(|t| t.depth).max().unwrap_or(0),
            best * 1000.0
        );
    }
    // As a window lays it out: the tiles first when the specks would make it slow, then the
    // specks in a second pass.
    viewer.defer_to_second_pass(true);
    for (width, height) in [(1600.0, 1000.0), (2560.0, 1400.0)] {
        let start = Instant::now();
        viewer.resize(width + 0.5, height);
        let first = start.elapsed();
        let deferred = viewer.second_pass_owed();
        let start = Instant::now();
        viewer.finish_second_pass();
        eprintln!(
            "{width}x{height} pt, specks {}: first pass {:.2} ms, second {:.2} ms",
            if deferred { "deferred" } else { "inline" },
            first.as_secs_f64() * 1000.0,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
