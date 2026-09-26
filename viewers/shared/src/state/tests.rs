use ::std::fs;
use ::std::sync::atomic::AtomicBool;
use ::std::sync::{Arc, Mutex, mpsc};

use super::*;
use diskonaut_scan::scan_into_tree;
use libdiskonaut::format::quote_path_for_shell;
use libdiskonaut::{EntryMeta, ScanOptions};

const ROOT: &str = "/diskonaut-mac-test-root";

/// Apparent sizes and one thread, so the figures are the files' lengths on any filesystem.
fn options() -> ScanOptions {
    ScanOptions {
        parallel: false,
        show_apparent_size: true,
        ..ScanOptions::default()
    }
}

/// A folder on disk: `big/inside` (3000 bytes), `medium.txt` (2000), `small/inside` (100),
/// `tiny.txt` (50).
fn on_disk(name: &str) -> PathBuf {
    let dir = ::std::env::temp_dir().join(format!("diskonaut_viewer_state_{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("big")).expect("create big");
    fs::create_dir_all(dir.join("small")).expect("create small");
    fs::write(dir.join("big").join("inside"), vec![b'x'; 3000]).expect("write");
    fs::write(dir.join("medium.txt"), vec![b'x'; 2000]).expect("write");
    fs::write(dir.join("small").join("inside"), vec![b'x'; 100]).expect("write");
    fs::write(dir.join("tiny.txt"), vec![b'x'; 50]).expect("write");
    dir.canonicalize().expect("canonical")
}

/// A viewer over a folder on disk, scanned, with copied text going into the returned record.
fn viewer_on(dir: &Path) -> (Viewer, Arc<Mutex<Vec<String>>>) {
    let mut viewer = Viewer::new(dir, SizeKind::Apparent, 1);
    viewer.resize(1200.0, 800.0);
    let (tree, _) = scan_into_tree(dir, options());
    viewer.finish_scan(tree);
    viewer.set_working_dir(Some(dir.to_path_buf()));
    let copied = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&copied);
    viewer.set_clipboard(move |text| {
        record.lock().expect("record").push(text.to_string());
        true
    });
    (viewer, copied)
}

fn last_copied(copied: &Mutex<Vec<String>>) -> Option<String> {
    copied.lock().expect("record").last().cloned()
}

fn quoted(relative: &str) -> String {
    quote_path_for_shell(Path::new(relative))
}

fn row_center(viewer: &Viewer, name: &str) -> (f64, f64) {
    let list = viewer.layout.list.expect("a list");
    let index = viewer
        .board
        .listing()
        .iter()
        .position(|entry| entry.name == name)
        .expect("listed");
    (list.x + 10.0, list.y + ROW * (index as f64 + 0.5))
}

fn meta(size: u64, is_dir: bool) -> EntryMeta {
    EntryMeta {
        size,
        apparent: size / 2,
        inode: 0,
        links: 1,
        is_dir,
        shared_extent: 0,
    }
}

/// A finished scan of: `big/` (a 600, b 300), `medium.txt` 400, `small/` (c 100), `tiny.bin` 50.
fn viewer() -> Viewer {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    for (path, size, is_dir) in [
        ("big", 0, true),
        ("big/a", 600, false),
        ("big/b", 300, false),
        ("medium.txt", 400, false),
        ("small", 0, true),
        ("small/c", 100, false),
        ("tiny.bin", 50, false),
    ] {
        tree.add_entry(meta(size, is_dir), &root.join(path));
    }
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    viewer
}

fn names(viewer: &Viewer) -> Vec<String> {
    viewer
        .board
        .listing()
        .iter()
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .collect()
}

fn selected(viewer: &Viewer) -> Option<String> {
    viewer
        .selected
        .as_ref()
        .map(|name| name.to_string_lossy().into_owned())
}

fn center_of(viewer: &Viewer, name: &str) -> (f64, f64) {
    let tile = viewer
        .board
        .tiles
        .iter()
        .find(|tile| tile.name == name)
        .expect("a tile");
    let rect = viewer
        .layout
        .cells_to_rect(tile.x, tile.y, tile.width, tile.height);
    (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
}

#[test]
fn layout_splits_the_window_and_fills_the_treemap_exactly() {
    let layout = Layout::new(1200.0, 800.0, true);
    let list = layout.list.expect("a list at this width");
    let info = layout.info.expect("details at this height");
    assert_eq!(list.x, 0.0);
    assert_eq!(list.y, PATH_BAR);
    assert_eq!(info.y, list.bottom());
    assert_eq!(info.bottom(), layout.status.y);
    assert_eq!(layout.treemap.x, list.right() + 1.0);
    assert_eq!(layout.treemap.right(), 1200.0);
    let whole = layout.cells_to_rect(0, 0, layout.cols, layout.rows);
    assert!((whole.right() - layout.treemap.right()).abs() < 1e-9);
    assert!((whole.bottom() - layout.treemap.bottom()).abs() < 1e-9);
}

#[test]
fn a_narrow_window_or_a_hidden_sidebar_is_all_treemap() {
    for layout in [
        Layout::new(500.0, 800.0, true),
        Layout::new(1200.0, 800.0, false),
    ] {
        assert!(layout.list.is_none() && layout.info.is_none());
        assert_eq!(layout.treemap.x, 0.0);
    }
    // Too short for details: the panel is all list.
    let short = Layout::new(1200.0, 300.0, true);
    assert!(short.list.is_some() && short.info.is_none());
}

#[test]
fn the_listing_is_largest_first_and_the_largest_is_in_hand() {
    let viewer = viewer();
    assert_eq!(names(&viewer), ["big", "medium.txt", "small", "tiny.bin"]);
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
    assert!(!viewer.scanning);
}

#[test]
fn every_tile_is_hit_at_its_center() {
    let viewer = viewer();
    for tile in &viewer.board.tiles {
        let name = tile.name.to_string_lossy();
        let (x, y) = center_of(&viewer, &name);
        assert_eq!(viewer.hit(x, y), Hit::Tile(tile.name.clone()), "{name}");
    }
}

#[test]
fn list_rows_are_hit_by_their_position() {
    let mut viewer = viewer();
    let list = viewer.layout.list.unwrap();
    assert_eq!(viewer.hit(10.0, list.y + 1.0), Hit::Row(0));
    assert_eq!(viewer.hit(10.0, list.y + ROW * 2.5), Hit::Row(2));
    assert_eq!(viewer.hit(10.0, list.y + ROW * 10.0), Hit::Nothing);
    // A list too short for its entries: the partial row at the bottom is not drawn, nor hit.
    viewer.resize(1200.0, PATH_BAR + STATUS_BAR + ROW * 2.5);
    assert_eq!(viewer.layout.list_rows(), 2);
    assert_eq!(viewer.hit(10.0, list.y + ROW * 2.2), Hit::Nothing);
    viewer.resize(1200.0, 800.0);
    viewer.list_top = 1;
    viewer.clamp_list_top();
    assert_eq!(viewer.list_top, 0, "four rows fit, so nothing scrolls");
}

#[test]
fn the_entry_in_hand_survives_a_resize() {
    let mut viewer = viewer();
    let (x, y) = center_of(&viewer, "small");
    viewer.click(x, y, Mods::default());
    assert_eq!(selected(&viewer).as_deref(), Some("small"));
    viewer.resize(700.0, 500.0);
    assert_eq!(selected(&viewer).as_deref(), Some("small"));
    let tile = viewer.board.currently_selected().expect("a tile in hand");
    assert_eq!(tile.name, "small");
}

#[test]
fn arrows_move_through_the_list_and_cross_to_the_treemap() {
    let mut viewer = viewer();
    assert_eq!(viewer.focus, Focus::List);
    viewer.arrow(Direction::Down, false);
    assert_eq!(selected(&viewer).as_deref(), Some("medium.txt"));
    viewer.arrow(Direction::Up, false);
    viewer.arrow(Direction::Up, false);
    assert_eq!(
        selected(&viewer).as_deref(),
        Some("big"),
        "stops at the top"
    );
    viewer.arrow(Direction::Right, false);
    assert_eq!(viewer.focus, Focus::Treemap);
    // `big` is the leftmost tile, so ← goes back to the list and keeps it in hand.
    viewer.arrow(Direction::Left, false);
    assert_eq!(viewer.focus, Focus::List);
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
}

#[test]
fn jumps_go_to_the_ends_of_the_list() {
    let mut viewer = viewer();
    viewer.jump(Jump::End, false);
    assert_eq!(selected(&viewer).as_deref(), Some("tiny.bin"));
    viewer.jump(Jump::Home, false);
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
}

#[test]
fn shift_marks_a_range_that_shrinks_when_reversed() {
    let mut viewer = viewer();
    viewer.arrow(Direction::Down, true);
    viewer.arrow(Direction::Down, true);
    assert_eq!(viewer.marked, ["big", "medium.txt", "small"]);
    viewer.arrow(Direction::Up, true);
    assert_eq!(viewer.marked, ["big", "medium.txt"]);
    // A plain move clears them.
    viewer.arrow(Direction::Down, false);
    assert!(viewer.marked.is_empty());
}

/// A ⇧ run adds its range to the marks there were, so an earlier mark outside the range survives
/// the run shrinking — it is not replaced by the range.
#[test]
fn a_shift_run_keeps_the_marks_it_started_from() {
    let mut viewer = viewer();
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    let (x, y) = row_center(&viewer, "big");
    viewer.click(x, y, Mods::default());
    let (x, y) = row_center(&viewer, "tiny.bin");
    viewer.click(x, y, toggle);
    assert_eq!(
        viewer.marked,
        ["big", "tiny.bin"],
        "big was picked, so it joins"
    );
    viewer.arrow(Direction::Up, true);
    viewer.arrow(Direction::Up, true);
    assert_eq!(
        viewer.marked,
        ["big", "tiny.bin", "small", "medium.txt"],
        "swept from the anchor, on top of the marks there were"
    );
    viewer.arrow(Direction::Down, true);
    assert_eq!(
        viewer.marked,
        ["big", "tiny.bin", "small"],
        "the range shrinks; the mark it started from stays"
    );
    viewer.jump(Jump::Home, false);
    assert!(viewer.marked.is_empty(), "a plain jump clears the marks");
}

/// Marks cleared by a move in the treemap are gone for good: a ⇧ move afterwards starts a range
/// from nothing rather than bringing back what the run began with.
#[test]
fn a_shift_run_does_not_bring_back_marks_cleared_in_the_treemap() {
    let mut viewer = viewer();
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    let (x, y) = row_center(&viewer, "tiny.bin");
    viewer.click(x, y, toggle);
    viewer.arrow(Direction::Up, true);
    assert_eq!(viewer.marked, ["tiny.bin", "small"]);
    viewer.toggle_focus();
    viewer.arrow(Direction::Left, false);
    assert!(
        viewer.marked.is_empty(),
        "a plain move in the treemap clears them"
    );
    viewer.focus = Focus::List;
    let from = viewer.selected_listing_index().expect("in hand");
    viewer.arrow(Direction::Down, true);
    let listing = names(&viewer);
    let to = (from + 1).min(listing.len() - 1);
    let marked: Vec<String> = viewer
        .marked
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        marked,
        listing[from..=to],
        "only the new range, from the entry in hand"
    );
}

/// A modified click takes in the entry already in hand only if the user picked it. The entry the
/// viewer put in hand after the scan was placed, not picked, so it is not marked behind their
/// back; one they moved to is.
#[test]
fn command_click_seeds_the_marks_only_with_a_chosen_entry() {
    let mut viewer = viewer();
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    assert!(!viewer.chosen, "the scan placed `big` in hand");
    let (x, y) = center_of(&viewer, "small");
    viewer.click(x, y, toggle);
    assert_eq!(viewer.marked, ["small"]);
    viewer.click(x, y, toggle);
    assert!(viewer.marked.is_empty(), "a second click takes it out");
    assert_eq!(viewer.target_names(), ["small"]);

    // Picked with an arrow, `medium.txt` joins a selection begun with a click elsewhere.
    viewer.jump(Jump::Home, false);
    viewer.arrow(Direction::Down, false);
    assert!(viewer.chosen);
    viewer.click(x, y, toggle);
    assert_eq!(viewer.marked, ["medium.txt", "small"]);
    // A plain click clears the marks.
    viewer.click(x, y, Mods::default());
    assert!(viewer.marked.is_empty());
    assert_eq!(viewer.target_names(), ["small"]);
    // The folder just left is placed, like a deleted entry's neighbour.
    viewer.enter(OsStr::new("big"));
    viewer.go_up();
    assert!(!viewer.chosen);
}

#[test]
fn marking_copies_the_paths_and_ctrl_c_copies_what_is_in_hand() {
    let dir = on_disk("copy");
    let (mut viewer, copied) = viewer_on(&dir);
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    let (x, y) = row_center(&viewer, "medium.txt");
    viewer.click(x, y, toggle);
    assert_eq!(last_copied(&copied), Some(quoted("medium.txt")));
    let (x, y) = row_center(&viewer, "tiny.txt");
    viewer.click(x, y, toggle);
    assert_eq!(
        last_copied(&copied),
        Some(format!("{} {}", quoted("medium.txt"), quoted("tiny.txt")))
    );
    let (left, _) = viewer.status();
    assert!(left.starts_with("Copied 2 paths:"), "{left}");

    viewer.jump(Jump::Home, false);
    assert!(viewer.copy_paths(false));
    assert_eq!(last_copied(&copied), Some(quoted("big")));
    assert!(viewer.copy_paths(true));
    assert_eq!(
        last_copied(&copied),
        Some(quote_path_for_shell(&dir.join("big")))
    );
    let (left, _) = viewer.status();
    assert!(left.starts_with("Copied absolute path:"), "{left}");
    viewer.enter_selected();
    viewer.copy_paths(false);
    assert_eq!(
        last_copied(&copied),
        Some(quoted(&format!("big{}inside", ::std::path::MAIN_SEPARATOR)))
    );
    // Without a clipboard nothing is copied, and the marks still work.
    let mut quiet = Viewer::new(&dir, SizeKind::Apparent, 1);
    quiet.resize(1200.0, 800.0);
    quiet.finish_scan(scan_into_tree(&dir, options()).0);
    quiet.mark_all();
    assert_eq!(quiet.marked.len(), 4);
    assert!(!quiet.copy_paths(false));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn deleting_removes_from_disk_going_on_past_a_failure() {
    let dir = on_disk("delete");
    let (mut viewer, _) = viewer_on(&dir);
    let (x, y) = row_center(&viewer, "medium.txt");
    viewer.click(x, y, Mods::default());
    let files = viewer.targets();
    assert_eq!(files.len(), 1);
    assert!(Viewer::delete_prompt(&files).contains("medium.txt"));
    viewer.delete(&files).expect("deleted");
    assert!(!dir.join("medium.txt").exists());
    assert_eq!(names(&viewer), ["big", "small", "tiny.txt"]);
    assert_eq!(viewer.tree.space_freed.get(SizeKind::Apparent), 2000);
    assert_eq!(
        selected(&viewer).as_deref(),
        Some("small"),
        "what took its place"
    );
    assert!(!viewer.chosen, "placed there, not picked");

    // Several at once, in the order marked; one already gone is reported, the rest deleted.
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    let (x, y) = row_center(&viewer, "tiny.txt");
    viewer.click(x, y, toggle);
    let (x, y) = row_center(&viewer, "small");
    viewer.click(x, y, toggle);
    let files = viewer.targets();
    assert_eq!(files.len(), 2);
    assert!(Viewer::delete_prompt(&files).starts_with("Delete these 2 entries?"));
    fs::remove_file(dir.join("tiny.txt")).expect("gone behind its back");
    let error = viewer.delete(&files).expect_err("one failed");
    assert!(error.starts_with("Deleted 1 of 2; tiny.txt"), "{error}");
    assert!(!dir.join("small").exists());
    assert!(viewer.marked.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_rescan_brings_in_what_changed_on_disk() {
    let dir = on_disk("rescan");
    let (mut viewer, _) = viewer_on(&dir);
    let (sender, answers) = mpsc::channel();
    let sender = Mutex::new(sender);
    viewer.enable_rescans(Rescanner::new(
        options(),
        Arc::new(AtomicBool::new(true)),
        move |id, outcome| {
            let _ = sender.lock().expect("sender").send((id, outcome));
        },
    ));
    fs::write(dir.join("small").join("new"), vec![b'x'; 5000]).expect("write");
    let (x, y) = row_center(&viewer, "small");
    viewer.click(x, y, Mods::default());
    viewer.rescan_selected();
    assert!(
        viewer
            .rescanning()
            .is_some_and(|what| what.contains("small")),
        "{:?}",
        viewer.rescanning()
    );
    let (_, totals) = viewer.status();
    assert!(totals.contains("rescanning 1 folder"), "{totals}");
    let (id, outcome): (u64, Outcome) = answers
        .recv_timeout(Duration::from_secs(20))
        .expect("the rescan reports back");
    viewer.rescan_done(id, outcome);
    assert_eq!(viewer.rescanning(), None);
    let small = viewer.entry_named(OsStr::new("small")).expect("small");
    assert_eq!(small.size, 5100);
    assert_eq!(selected(&viewer).as_deref(), Some("small"), "still in hand");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn entering_and_leaving_keeps_the_way_back() {
    let mut viewer = viewer();
    assert!(viewer.enter_selected());
    assert_eq!(viewer.depth(), 1);
    assert_eq!(names(&viewer), ["a", "b"]);
    assert_eq!(selected(&viewer).as_deref(), Some("a"));
    assert_eq!(viewer.title(), "big");
    // A file is not entered.
    assert!(!viewer.enter_selected());
    assert!(viewer.go_up());
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
    assert!(!viewer.go_up(), "already at the root");
}

#[test]
fn a_breadcrumb_goes_up_several_levels() {
    let mut viewer = viewer();
    viewer.enter(OsStr::new("big"));
    viewer.go_to_depth(0);
    assert_eq!(viewer.depth(), 0);
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
}

#[test]
fn zoom_is_restored_on_the_way_back_up() {
    let mut viewer = viewer();
    viewer.zoom_in();
    assert_eq!(viewer.board.zoom_level, 1);
    viewer.arrow(Direction::Down, false);
    viewer.arrow(Direction::Down, false);
    assert_eq!(selected(&viewer).as_deref(), Some("small"));
    viewer.enter_selected();
    assert_eq!(viewer.board.zoom_level, 0);
    viewer.go_up();
    assert_eq!(viewer.board.zoom_level, 1);
}

#[test]
fn toggling_the_size_keeps_the_entry_in_hand() {
    let mut viewer = viewer();
    viewer.arrow(Direction::Down, false);
    viewer.toggle_size();
    assert!(viewer.showing_apparent());
    assert_eq!(selected(&viewer).as_deref(), Some("medium.txt"));
    assert_eq!(viewer.tree.get_current_folder_size(), 725);
}

#[test]
fn removing_puts_the_next_entry_in_hand_and_counts_what_was_freed() {
    let mut viewer = viewer();
    viewer.arrow(Direction::Down, false);
    let targets = viewer.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].full_path(), Path::new(ROOT).join("medium.txt"));
    viewer.removed(&targets, true);
    assert_eq!(names(&viewer), ["big", "small", "tiny.bin"]);
    assert_eq!(selected(&viewer).as_deref(), Some("small"));
    assert_eq!(viewer.tree.space_freed.disk, 400);
    // Moved to the Trash: gone from here, but nothing freed.
    let targets = viewer.targets();
    viewer.removed(&targets, false);
    assert_eq!(viewer.tree.space_freed.disk, 400);
    // Removing what is already gone changes nothing, rather than panicking.
    viewer.removed(&targets, true);
    assert_eq!(viewer.tree.space_freed.disk, 400);
}

#[test]
fn nothing_is_deleted_while_the_outline_is_on_screen() {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.resize(1200.0, 800.0);
    let mut directory = libdiskonaut::DirEntries::new(Arc::from(root));
    directory.push(OsStr::new("folder"), meta(0, true));
    // Absorbed, a batch is in the tree but not yet on screen; catching up lays it out.
    viewer.absorb_summaries(vec![DirSummary::of(&directory)]);
    assert!(names(&viewer).is_empty(), "not laid out yet");
    assert_eq!(viewer.entries_scanned, 1);
    viewer.catch_up();
    assert_eq!(names(&viewer), ["folder"]);
    let mut more = libdiskonaut::DirEntries::new(Arc::from(root));
    more.push(OsStr::new("other"), meta(0, true));
    viewer.add_summaries(vec![DirSummary::of(&more)]);
    assert_eq!(names(&viewer).len(), 2, "add_summaries lays out at once");
    viewer.jump(Jump::Home, false);
    assert!(viewer.selected.is_some());
    assert!(viewer.targets().is_empty());
    assert!(!viewer.can_rescan());
}

#[test]
fn previews_are_asked_for_files_once_and_answers_to_old_requests_dropped() {
    let mut viewer = viewer();
    // A folder is in hand: nothing to read.
    assert_eq!(viewer.wanted_preview(), None);
    viewer.arrow(Direction::Down, false);
    let (first, path) = viewer.wanted_preview().expect("a request");
    assert_eq!(path, Path::new(ROOT).join("medium.txt"));
    assert_eq!(viewer.preview, Preview::Loading);
    assert_eq!(viewer.wanted_preview(), None, "asked once");
    viewer.arrow(Direction::Down, false);
    viewer.arrow(Direction::Down, false);
    let (second, _) = viewer.wanted_preview().expect("another request");
    assert!(!viewer.preview_ready(first, Preview::Info("old".into())));
    assert!(viewer.preview_ready(second, Preview::Info("binary file".into())));
    assert_eq!(viewer.preview, Preview::Info("binary file".into()));
}

/// A viewer that prepares the picture at its drawn size asks again when that size changes.
#[test]
fn a_sized_preview_is_asked_again_when_its_size_changes() {
    let mut viewer = viewer();
    viewer.arrow(Direction::Down, false);
    let (first, _) = viewer
        .wanted_preview_sized(Some((320, 180)))
        .expect("a request");
    assert_eq!(
        viewer.wanted_preview_sized(Some((320, 180))),
        None,
        "asked once"
    );
    let (second, _) = viewer
        .wanted_preview_sized(Some((640, 360)))
        .expect("a new size is a new request");
    assert!(second > first);
    assert!(!viewer.preview_ready(first, Preview::Picture("small".into())));
    assert!(viewer.preview_ready(second, Preview::Picture("PNG 640×360".into())));
}

#[test]
fn file_colours_follow_the_extension_and_stay_clear_of_folder_blue() {
    let colour = |name: &str| tile_color(OsStr::new(name), FileType::File, 0);
    assert_eq!(colour("a.jpg"), colour("b.JPG"));
    assert_ne!(colour("a.jpg"), colour("a.mp4"));
    for extension in ["rs", "txt", "zip", "mov", "dmg", "pdf", "o", "a", "json"] {
        let (r, g, b) = colour(&format!("x.{extension}"));
        // The folders' blues: blue strongest, green at least red.
        let bluish = b > r.max(g) && g >= r;
        assert!(!bluish, "{extension}: {r:.2} {g:.2} {b:.2}");
    }
}

#[test]
fn a_top_inset_moves_everything_down_and_the_bounds_stay_whole() {
    let plain = Layout::new(1200.0, 800.0, true);
    let inset = Layout::with_top(1200.0, 800.0, true, 32.0);
    assert_eq!(inset.bounds, plain.bounds);
    assert_eq!(inset.path_bar.y, 32.0);
    assert_eq!(inset.list.unwrap().y, plain.list.unwrap().y + 32.0);
    assert_eq!(inset.treemap.y, plain.treemap.y + 32.0);
    assert_eq!(
        inset.status, plain.status,
        "the status bar stays at the bottom"
    );
    assert!(
        inset.rows < plain.rows,
        "the treemap lost the rows the title bar took"
    );
    let mut viewer = viewer();
    viewer.top_inset = 32.0;
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.layout.path_bar.y, 32.0);
    assert!(
        matches!(viewer.hit(600.0, 10.0), Hit::Nothing),
        "the title bar is not the viewer's"
    );
}

// ---------------------------------------------------------------- the tree view

fn tree_viewer() -> Viewer {
    let mut viewer = viewer();
    viewer.tree_view = true;
    viewer
}

fn rows_of(viewer: &Viewer) -> Vec<String> {
    viewer
        .rows()
        .iter()
        .map(|row| {
            format!(
                "{}{}{}",
                "  ".repeat(row.depth),
                row.entry.name.to_string_lossy(),
                if row.open { "/" } else { "" }
            )
        })
        .collect()
}

fn cursor_of(viewer: &Viewer) -> Option<String> {
    viewer
        .cursor_entry()
        .map(|row| row.entry.name.to_string_lossy().into_owned())
}

/// With the tree view off, the rows are the listing and the arrows do what they always did.
#[test]
fn without_the_tree_view_the_rows_are_the_flat_listing() {
    let mut viewer = viewer();
    assert_eq!(rows_of(&viewer), names(&viewer));
    viewer.arrow(Direction::Right, false);
    assert_eq!(viewer.focus, Focus::Treemap, "→ crosses to the treemap");
    viewer.focus = Focus::List;
    viewer.arrow(Direction::Left, false);
    assert_eq!(rows_of(&viewer), ["big", "medium.txt", "small", "tiny.bin"]);
    assert!(
        !matches!(
            viewer.hit(8.0, viewer.layout.list.unwrap().y + 1.0),
            Hit::Expander(_)
        ),
        "no expander to hit"
    );
}

/// → opens the folder in hand in place and its entries follow, indented; → again goes down
/// into it; ← goes back up to the folder, and ← again closes it.
#[test]
fn a_folder_opens_in_place_and_the_arrows_walk_into_it() {
    let mut viewer = tree_viewer();
    viewer.arrow(Direction::Right, false);
    assert_eq!(
        rows_of(&viewer),
        ["big/", "  a", "  b", "medium.txt", "small", "tiny.bin"]
    );
    assert_eq!(cursor_of(&viewer).as_deref(), Some("big"), "stays in hand");
    assert_eq!(viewer.focus, Focus::List);
    viewer.arrow(Direction::Right, false);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    assert_eq!(
        selected(&viewer).as_deref(),
        Some("big"),
        "the treemap follows the row's top-level folder"
    );
    assert_eq!(
        viewer.board.currently_selected().map(|t| t.name.clone()),
        Some(OsString::from("big"))
    );
    viewer.arrow(Direction::Down, false);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("b"));
    viewer.arrow(Direction::Left, false);
    assert_eq!(
        cursor_of(&viewer).as_deref(),
        Some("big"),
        "← goes up to the folder"
    );
    viewer.arrow(Direction::Left, false);
    assert_eq!(rows_of(&viewer), ["big", "medium.txt", "small", "tiny.bin"]);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("big"));
    // → on a file still crosses to the treemap.
    viewer.jump(Jump::End, false);
    viewer.arrow(Direction::Right, false);
    assert_eq!(viewer.focus, Focus::Treemap);
}

/// A nested row is acted on where it is: previewed, copied, entered, deleted.
#[test]
fn a_nested_row_is_what_is_acted_on() {
    let mut viewer = tree_viewer();
    viewer.arrow(Direction::Right, false);
    viewer.arrow(Direction::Right, false);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    let (_, path) = viewer.wanted_preview().expect("a file is in hand");
    assert_eq!(path, Path::new(ROOT).join("big").join("a"));
    assert_eq!(
        viewer.target_paths(),
        [Path::new(ROOT).join("big").join("a")]
    );
    let targets = viewer.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path_to_file, ["big", "a"]);
    assert_eq!(targets[0].size, 600);
    // Removed, the folder stays open but, smaller now than medium.txt, re-sorts below it, its
    // rows with it; the row now where the deleted one was — the folder itself — is in hand.
    viewer.removed(&targets, true);
    assert_eq!(
        rows_of(&viewer),
        ["medium.txt", "big/", "  b", "small", "tiny.bin"]
    );
    assert_eq!(cursor_of(&viewer).as_deref(), Some("big"));
    assert!(!viewer.chosen, "placed, not picked");
    assert_eq!(viewer.tree.space_freed.disk, 600);
    // Entering a nested folder goes down through the folders above it.
    viewer.arrow(Direction::Down, false);
    viewer.arrow(Direction::Down, false);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("small"));
    viewer.arrow(Direction::Right, false);
    viewer.arrow(Direction::Right, false);
    assert_eq!(cursor_of(&viewer).as_deref(), Some("c"));
    assert!(!viewer.enter_selected(), "a file is not entered");
    viewer.arrow(Direction::Left, false);
    assert!(viewer.enter_selected());
    assert_eq!(viewer.depth(), 1);
    assert_eq!(viewer.title(), "small");
    assert_eq!(
        rows_of(&viewer),
        ["c"],
        "opened afresh: nothing open in the new folder"
    );
    viewer.go_up();
    assert_eq!(
        rows_of(&viewer),
        ["medium.txt", "big", "small", "tiny.bin"],
        "closed again on the way back"
    );
}

/// The expander is its own target, and a click on it opens the folder without moving on.
#[test]
fn the_expander_opens_a_folder_on_a_click() {
    let mut viewer = tree_viewer();
    let list = viewer.layout.list.expect("a list");
    let y = list.y + ROW / 2.0;
    assert_eq!(viewer.hit(list.x + LIST_PAD + 2.0, y), Hit::Expander(0));
    assert_eq!(
        viewer.hit(list.x + LIST_PAD + EXPANDER + 2.0, y),
        Hit::Row(0)
    );
    // A file's row has no expander.
    assert_eq!(viewer.hit(list.x + LIST_PAD + 2.0, y + ROW), Hit::Row(1));
    viewer.toggle_row(0);
    assert_eq!(rows_of(&viewer)[..3], ["big/", "  a", "  b"]);
    // Its own rows are indented, so their expanders sit one level in.
    viewer.toggle_row(0);
    assert_eq!(rows_of(&viewer), ["big", "medium.txt", "small", "tiny.bin"]);
    // Clicking a nested row takes it in hand; the marks are the top-level folder's.
    viewer.toggle_row(0);
    viewer.click(list.x + 100.0, y + ROW, Mods::default());
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    viewer.click(
        list.x + 100.0,
        y + ROW * 4.0,
        Mods {
            toggle: true,
            range: false,
        },
    );
    assert_eq!(
        viewer.marked,
        ["big", "small"],
        "a picked nested row marks its folder"
    );
    let (left, _) = viewer.status();
    assert!(left.starts_with("2 marked"), "{left}");
}

/// The treemap nested: a folder's tile holds its entries' tiles; pointing at one names it,
/// clicking it opens the folders above it in the tree and puts it in hand.
#[test]
fn a_tile_inside_a_folders_tile_is_pointed_at_and_reveals_its_row() {
    let mut viewer = tree_viewer();
    viewer.set_tree_view(true);
    assert!(!viewer.nested().is_empty(), "big's tile holds a and b");
    let a = viewer
        .nested()
        .iter()
        .find(|t| t.tile.name == "a")
        .expect("a's tile");
    assert_eq!(a.path, ["big", "a"]);
    let rect = viewer
        .layout
        .cells_to_rect(a.tile.x, a.tile.y, a.tile.width, a.tile.height);
    let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let index = match viewer.hit(x, y) {
        Hit::Nested(index) => index,
        other => panic!("expected a nested tile, got {other:?}"),
    };
    assert_eq!(viewer.nested()[index].path, ["big", "a"]);
    assert!(viewer.hover_at(x, y));
    let (left, _) = viewer.status();
    assert!(left.starts_with("a — 600"), "{left}");

    viewer.click(x, y, Mods::default());
    assert_eq!(
        rows_of(&viewer)[..3],
        ["big/", "  a", "  b"],
        "big opened in the tree"
    );
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
    assert_eq!(viewer.cursor_nested(), Some(index));
    // Off, nothing is nested and the tile is the folder's again.
    viewer.set_tree_view(false);
    assert!(viewer.nested().is_empty());
    assert_eq!(viewer.hit(x, y), Hit::Tile(OsString::from("big")));
}
