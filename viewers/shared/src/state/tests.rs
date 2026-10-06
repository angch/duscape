use ::std::fs;
use ::std::sync::atomic::AtomicBool;
use ::std::sync::{Arc, Mutex, mpsc};

use super::*;
use crate::deleting;
use duscape_scan::scan_into_tree;
use libduscape::format::quote_path_for_shell;
use libduscape::{EntryMeta, ScanOptions};

const ROOT: &str = "/duscape-mac-test-root";

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
    let dir = ::std::env::temp_dir().join(format!("duscape_viewer_state_{name}"));
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

/// The marks, each as its row's path from the listed folder joined with `/`.
fn marks(viewer: &Viewer) -> Vec<String> {
    viewer
        .marked
        .iter()
        .map(|path| {
            path.iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
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
    assert_eq!(viewer.focus(), Focus::List);
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
    assert_eq!(viewer.focus(), Focus::Treemap);
    // `big` is the leftmost tile, so ← goes back to the list and keeps it in hand.
    viewer.arrow(Direction::Left, false);
    assert_eq!(viewer.focus(), Focus::List);
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
}

#[test]
fn the_keyboard_goes_back_to_the_list_when_it_has_room_again() {
    // Every desktop viewer says its scale before its window has a size.
    let dir = on_disk("scale_before_size");
    let mut viewer = Viewer::new(&dir, SizeKind::Apparent, 1);
    viewer.set_pixel_scale(1.5);
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.focus(), Focus::List);
    viewer.set_pixel_scale(2.0);
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.focus(), Focus::List, "a new scale keeps the focus");
    // Minimised, Windows says the window is 0×0.
    viewer.resize(0.0, 0.0);
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.focus(), Focus::List, "minimised and restored");
    // Too narrow for the list, the treemap has the keyboard; widened, the list again.
    viewer.resize(300.0, 800.0);
    assert_eq!(viewer.focus(), Focus::Treemap);
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.focus(), Focus::List, "narrowed and widened");
    // Given to the treemap, it stays there.
    viewer.toggle_focus();
    viewer.resize(300.0, 800.0);
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.focus(), Focus::Treemap);
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
    assert_eq!(marks(&viewer), ["big", "medium.txt", "small"]);
    viewer.arrow(Direction::Up, true);
    assert_eq!(marks(&viewer), ["big", "medium.txt"]);
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
        marks(&viewer),
        ["big", "tiny.bin"],
        "big was picked, so it joins"
    );
    viewer.arrow(Direction::Up, true);
    viewer.arrow(Direction::Up, true);
    assert_eq!(
        marks(&viewer),
        ["big", "tiny.bin", "small", "medium.txt"],
        "swept from the anchor, on top of the marks there were"
    );
    viewer.arrow(Direction::Down, true);
    assert_eq!(
        marks(&viewer),
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
    assert_eq!(marks(&viewer), ["tiny.bin", "small"]);
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
    let marked = marks(&viewer);
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
    assert_eq!(marks(&viewer), ["small"]);
    viewer.click(x, y, toggle);
    assert!(viewer.marked.is_empty(), "a second click takes it out");
    assert_eq!(viewer.target_rows(), [vec![OsString::from("small")]]);

    // Picked with an arrow, `medium.txt` joins a selection begun with a click elsewhere.
    viewer.jump(Jump::Home, false);
    viewer.arrow(Direction::Down, false);
    assert!(viewer.chosen);
    viewer.click(x, y, toggle);
    assert_eq!(marks(&viewer), ["medium.txt", "small"]);
    // A plain click clears the marks.
    viewer.click(x, y, Mods::default());
    assert!(viewer.marked.is_empty());
    assert_eq!(viewer.target_rows(), [vec![OsString::from("small")]]);
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

/// Delete `files` for good on the deleting thread, and wait for its report.
fn delete_now(viewer: &mut Viewer, files: Vec<FileToDelete>) -> Option<Failure> {
    let (sender, report) = mpsc::channel();
    viewer
        .start_delete(files, true, deleting::for_good, move |id, ended| {
            let _ = sender.send((id, ended));
        })
        .expect("started");
    assert!(viewer.deleting().is_some(), "under way");
    let (id, ended) = report.recv().expect("reported");
    let failure = viewer.delete_done(id, &ended);
    assert!(viewer.deleting().is_none(), "over");
    failure
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
    assert_eq!(delete_now(&mut viewer, files), None, "deleted");
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
    let failure = delete_now(&mut viewer, files).expect("one failed");
    assert!(failure.title.ends_with("tiny.txt"), "{failure:?}");
    assert!(
        failure.detail.ends_with("1 of 2 were removed."),
        "{failure:?}"
    );
    assert!(!dir.join("small").exists());
    assert!(viewer.marked.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

/// While a delete runs the viewer says so; Cancel stops it, and what it stopped is no failure to
/// report and stays in the tree.
#[test]
fn a_cancelled_delete_reports_nothing_and_keeps_what_it_did_not_remove() {
    let dir = on_disk("delete_cancel");
    let (mut viewer, _) = viewer_on(&dir);
    viewer.mark_all();
    let files = viewer.targets();
    let count = files.len();
    let (sender, report) = mpsc::channel();
    viewer
        .start_delete(
            files,
            true,
            |_, tally| {
                while !tally.stopped() {
                    ::std::thread::sleep(Duration::from_millis(1));
                }
                Err("stopped".to_string())
            },
            move |id, ended| {
                let _ = sender.send((id, ended));
            },
        )
        .expect("started");
    let deletion = viewer.deleting().expect("under way");
    assert_eq!(deletion.progress().0, 0);
    assert!(!deletion.cancelling());
    assert!(
        viewer
            .start_delete(Vec::new(), true, deleting::for_good, |_, _| {})
            .is_err(),
        "one at a time"
    );
    viewer.cancel_delete();
    assert!(viewer.deleting().expect("still").cancelling());
    let (id, ended) = report.recv().expect("reported");
    assert_eq!(ended.len(), count);
    assert_eq!(viewer.delete_done(id, &ended), None, "nothing to report");
    assert!(viewer.deleting().is_none());
    assert_eq!(names(&viewer).len(), count, "all still there");
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
    // A nested row is what is rescanned: a nested folder itself, a nested file's folder.
    viewer.set_tree_view(true);
    viewer.jump(Jump::Home, false);
    viewer.arrow(Direction::Right, false);
    viewer.arrow(Direction::Right, false);
    assert_eq!(
        cursor_of(&viewer).as_deref(),
        Some("new"),
        "small/new in hand: small is the larger since the rescan"
    );
    viewer.rescan_selected();
    assert_eq!(
        viewer.rescanning().as_deref(),
        Some("small"),
        "the file's folder is rescanned"
    );
    let (id, outcome): (u64, Outcome) = answers
        .recv_timeout(Duration::from_secs(20))
        .expect("the rescan reports back");
    viewer.rescan_done(id, outcome);
    assert_eq!(viewer.rescanning(), None);
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
    let mut directory = libduscape::DirEntries::new(Arc::from(root));
    directory.push(OsStr::new("folder"), meta(0, true));
    // Absorbed, a batch is in the tree but not yet on screen; catching up lays it out.
    viewer.absorb_summaries(vec![DirSummary::of(&directory)]);
    assert!(names(&viewer).is_empty(), "not laid out yet");
    assert_eq!(viewer.entries_scanned, 1);
    viewer.catch_up();
    assert_eq!(names(&viewer), ["folder"]);
    let mut more = libduscape::DirEntries::new(Arc::from(root));
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
fn a_folder_keeps_its_colour_when_the_scan_reorders_the_listing() {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.resize(1200.0, 800.0);
    let batch = |folder: &str, file: &str, size: u64| {
        let mut directory = libduscape::DirEntries::new(Arc::from(root.join(folder)));
        directory.push(OsStr::new(file), meta(size, false));
        DirSummary::of(&directory)
    };
    let mut top = libduscape::DirEntries::new(Arc::from(root));
    for folder in ["alpha", "beta", "gamma", "delta"] {
        top.push(OsStr::new(folder), meta(0, true));
    }
    let colours = |viewer: &Viewer| {
        let mut colours: Vec<(String, (f64, f64, f64))> = (0..viewer.board.tiles.len())
            .map(|index| {
                let name = viewer.board.tiles[index]
                    .name
                    .to_string_lossy()
                    .into_owned();
                (name, viewer.board_color(index))
            })
            .collect();
        colours.sort_by(|a, b| a.0.cmp(&b.0));
        colours
    };
    viewer.add_summaries(vec![
        DirSummary::of(&top),
        batch("alpha", "a", 4000),
        batch("beta", "b", 3000),
        batch("gamma", "c", 2000),
        batch("delta", "d", 1000),
    ]);
    let before = colours(&viewer);
    assert_eq!(names(&viewer), ["alpha", "beta", "gamma", "delta"]);
    // The walk finds more in the smallest: it goes to the top, the rest move down a place.
    viewer.add_summaries(vec![batch("delta", "e", 9000)]);
    assert_eq!(names(&viewer), ["delta", "alpha", "beta", "gamma"]);
    assert_eq!(
        colours(&viewer),
        before,
        "each folder's colour is its own, not its place's"
    );
    // And the four are not all one blue.
    let mut blues: Vec<_> = before
        .iter()
        .map(|(_, colour)| format!("{colour:?}"))
        .collect();
    blues.dedup();
    assert!(blues.len() > 1);
}

#[test]
fn file_colours_follow_the_extension_and_stay_clear_of_folder_blue() {
    let colour = |name: &str| tile_color(OsStr::new(name), FileType::File);
    assert_eq!(colour("a.jpg"), colour("b.JPG"));
    assert_ne!(colour("a.jpg"), colour("a.mp4"));
    for extension in ["rs", "txt", "zip", "mov", "dmg", "pdf", "o", "a", "json"] {
        let (r, g, b) = colour(&format!("x.{extension}"));
        // The folders' blues: blue strongest, green at least red.
        let bluish = b > r.max(g) && g >= r;
        assert!(!bluish, "{extension}: {r:.2} {g:.2} {b:.2}");
    }
}

/// The chooser's button is the path bar's left end: the breadcrumbs start after it, and it
/// moves down with the bar under a title bar the viewer draws itself.
#[test]
fn the_chooser_button_is_at_the_path_bars_left() {
    let layout = Layout::new(1200.0, 800.0, true);
    assert_eq!(layout.chooser_button.x, 0.0);
    assert_eq!(layout.chooser_button.y, 0.0);
    assert_eq!(layout.chooser_button.h, PATH_BAR);
    assert_eq!(layout.path_bar.x, layout.chooser_button.right());
    assert_eq!(layout.path_bar.right(), 1200.0);
    assert!(layout.chooser_button.contains(15.0, 15.0));
    assert!(!layout.path_bar.contains(15.0, 15.0));
    let inset = Layout::with_top(1200.0, 800.0, true, 32.0);
    assert_eq!(inset.chooser_button.y, 32.0);
    // A window too narrow for the button still has a layout.
    let narrow = Layout::new(10.0, 800.0, true);
    assert_eq!(narrow.chooser_button.w, 10.0);
    assert_eq!(narrow.path_bar.w, 0.0);
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
    assert_eq!(viewer.focus(), Focus::Treemap, "→ crosses to the treemap");
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
    assert_eq!(viewer.focus(), Focus::List);
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
    assert_eq!(viewer.focus(), Focus::Treemap);
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
    // Clicking a nested row takes it in hand; a mark is the row's own.
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
        marks(&viewer),
        ["big/a", "small"],
        "a picked nested row joins as itself"
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
    let at = viewer
        .nested()
        .iter()
        .position(|t| t.tile.name == "a")
        .expect("a's tile");
    assert_eq!(viewer.nested_path(at), ["big", "a"]);
    let a = &viewer.nested()[at];
    let rect = viewer
        .layout
        .cells_to_rect(a.tile.x, a.tile.y, a.tile.width, a.tile.height);
    let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let index = match viewer.hit(x, y) {
        Hit::Nested(index) => index,
        other => panic!("expected a nested tile, got {other:?}"),
    };
    assert_eq!(viewer.nested_path(index), ["big", "a"]);
    assert!(viewer.hover_at(x, y));
    let (left, _) = viewer.status();
    assert!(left.starts_with("a — 600"), "{left}");

    assert_eq!(
        viewer.hover, None,
        "the nested tile is hovered, not the folder's around it"
    );
    viewer.click(x, y, Mods::default());
    assert_eq!(
        rows_of(&viewer)[..3],
        ["big/", "  a", "  b"],
        "big opened in the tree"
    );
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
    assert_eq!(viewer.cursor_nested(), Some(index));
    // A relayout moves the tiles: nothing is hovered until the pointer moves again.
    viewer.resize(1200.0, 800.0);
    assert_eq!(viewer.hover_nested, None);
    // The modifiers apply to a nested tile as to any target: Ctrl+click marks it and keeps the
    // marks there were.
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    // (`big` is open, so the row is found among the rows, not the flat listing.)
    let list = viewer.layout.list.expect("a list");
    let tiny = rows_of(&viewer)
        .iter()
        .position(|row| row == "tiny.bin")
        .expect("a row");
    viewer.click(list.x + 10.0, list.y + ROW * (tiny as f64 + 0.5), toggle);
    assert_eq!(
        marks(&viewer),
        ["big/a", "tiny.bin"],
        "a's row was picked, so it joins first"
    );
    let a = viewer.nested().iter().find(|t| t.tile.name == "a").unwrap();
    let rect = viewer
        .layout
        .cells_to_rect(a.tile.x, a.tile.y, a.tile.width, a.tile.height);
    let (ax, ay) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    viewer.click(ax, ay, toggle);
    assert_eq!(marks(&viewer), ["tiny.bin"], "a toggled off, tiny.bin kept");
    assert_eq!(cursor_of(&viewer).as_deref(), Some("a"));
    viewer.click(ax, ay, toggle);
    assert_eq!(marks(&viewer), ["tiny.bin", "big/a"], "and on again");
    let a_tile = viewer.cursor_nested().expect("a's tile");
    let marked = viewer.marked_nested();
    assert_eq!(
        marked.iter().filter(|marked| **marked).count(),
        1,
        "a's tile alone"
    );
    assert!(marked[a_tile]);
    // With marks present, hovering a nested tile still names it, as hovering any tile does.
    assert!(viewer.hover_at(ax, ay));
    let (left, _) = viewer.status();
    assert!(left.starts_with("a — 600"), "{left}");
    // Off, nothing is nested and the tile is the folder's again.
    viewer.set_tree_view(false);
    assert!(viewer.nested().is_empty());
    assert_eq!(viewer.hit(x, y), Hit::Tile(OsString::from("big")));
}

// ---------------------------------------------------------------- the context menu

const DESKTOP: crate::menu::Platform = crate::menu::Platform {
    reveal: "Show in File Manager",
    quick_look: false,
    pathname: false,
    trash: true,
    about: false,
};

fn menu_labels(menu: &[crate::menu::Entry]) -> Vec<String> {
    use crate::menu::Entry;
    menu.iter()
        .map(|entry| match entry {
            Entry::Item { label, enabled, .. } => {
                format!("{label}{}", if *enabled { "" } else { " (off)" })
            }
            Entry::Separator => "-".to_string(),
        })
        .collect()
}

#[test]
fn the_context_menu_is_about_the_folder_in_hand() {
    let viewer = viewer();
    assert_eq!(selected(&viewer).as_deref(), Some("big"));
    assert_eq!(
        menu_labels(&viewer.context_menu(&DESKTOP)),
        [
            "Open",
            "Show in File Manager",
            "-",
            "Copy Path",
            "Copy Full Path",
            "-",
            // No rescanner in this viewer.
            "Rescan Folder (off)",
            "Rescan Everything (off)",
            "-",
            "Move to Trash",
            "Delete Immediately…",
        ]
    );
}

#[test]
fn the_context_menu_counts_the_marks_and_opens_none_of_them() {
    use crate::menu::{Action, Platform};
    let mut viewer = viewer();
    viewer.mark_all();
    let menu = viewer.context_menu(&Platform {
        trash: false,
        quick_look: true,
        pathname: true,
        ..DESKTOP
    });
    assert_eq!(
        menu_labels(&menu),
        [
            "Open (off)",
            "Quick Look",
            "Show in File Manager",
            "-",
            "Copy 4 Paths",
            "Copy 4 Full Paths",
            "Copy as Pathname",
            "-",
            "Rescan Folder (off)",
            "Rescan Everything (off)",
            "-",
            "Delete 4 Items…",
        ]
    );
    assert_eq!(menu[0].chosen(), None);
    assert_eq!(menu[1].chosen(), Some(Action::QuickLook));
    assert_eq!(menu[3].chosen(), None);
}

#[test]
fn the_context_menu_rescans_a_files_folder_and_waits_for_the_scan() {
    let mut viewer = viewer();
    viewer.jump(Jump::End, false);
    let menu = menu_labels(&viewer.context_menu(&DESKTOP));
    assert!(
        menu.contains(&"Rescan Enclosing Folder (off)".to_string()),
        "{menu:?}"
    );
    viewer.scanning = true;
    let menu = menu_labels(&viewer.context_menu(&DESKTOP));
    assert!(
        menu.contains(&"Move to Trash (off)".to_string()),
        "{menu:?}"
    );
}

#[test]
fn copying_a_nested_row_copies_its_own_path() {
    let (mut viewer, copied) = viewer_on(&on_disk("copy_nested"));
    viewer.set_tree_view(true);
    // `big` in hand: → opens it, → again goes to its entry.
    viewer.arrow(Direction::Right, false);
    viewer.arrow(Direction::Right, false);
    assert_eq!(
        viewer.cursor_entry().map(|row| row.path.clone()),
        Some(vec![OsString::from("big"), OsString::from("inside")])
    );
    assert!(viewer.copy_paths(false));
    assert_eq!(
        last_copied(&copied).as_deref(),
        Some(quoted(&Path::new("big").join("inside").to_string_lossy()).as_str())
    );
}

/// Told the screen's pixels, the treemap is laid out in them: entries far too small for the
/// terminal-shaped cells get tiles of their own, a folder too short for its label still holds
/// its entries, and a tile a few pixels wide is pointed at like any other.
#[test]
fn in_pixel_cells_the_nesting_goes_down_to_what_the_screen_can_show() {
    let root = Path::new(ROOT);
    let tree = || {
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        tree.add_entry(meta(0, true), &root.join("big"));
        tree.add_entry(meta(400_000, false), &root.join("big/huge"));
        tree.add_entry(meta(0, true), &root.join("big/many"));
        for index in 0..2000 {
            tree.add_entry(meta(1_000, false), &root.join(format!("big/many/f{index}")));
        }
        tree
    };
    let nested_files = |viewer: &Viewer| {
        viewer
            .nested()
            .iter()
            .filter(|t| t.tile.name.to_string_lossy().starts_with('f'))
            .count()
    };
    let mut cells = Viewer::new(root, SizeKind::Disk, 1);
    cells.set_tree_view(true);
    cells.resize(1200.0, 800.0);
    cells.finish_scan(tree());
    let mut pixels = Viewer::new(root, SizeKind::Disk, 1);
    pixels.set_tree_view(true);
    pixels.set_pixel_scale(2.0);
    pixels.resize(1200.0, 800.0);
    pixels.finish_scan(tree());

    let treemap = pixels.layout.treemap;
    assert_eq!(f64::from(pixels.layout.cols), (treemap.w * 2.0).floor());
    assert_eq!(f64::from(pixels.layout.rows), (treemap.h * 2.0).floor());
    let (few, many) = (nested_files(&cells), nested_files(&pixels));
    assert!(
        many > few.max(100),
        "{few} tiles in cells, {many} in pixels"
    );
    for t in pixels.nested() {
        assert!(
            t.tile.width >= MIN_TILE_PIXELS && t.tile.height >= MIN_TILE_PIXELS,
            "{t:?}"
        );
    }

    // The smallest tile, a few pixels across, is still what a click there hits.
    let smallest = (0..pixels.nested().len())
        .min_by_key(|&index| {
            let t = &pixels.nested()[index].tile;
            u32::from(t.width) * u32::from(t.height)
        })
        .expect("nested tiles");
    let t = &pixels.nested()[smallest].tile;
    let rect = pixels.layout.cells_to_rect(t.x, t.y, t.width, t.height);
    // Smaller than the least tile the terminal-shaped cells have, 8×3 of them.
    assert!(rect.w < 8.0 * CELL_W || rect.h < 3.0 * CELL_H, "{rect:?}");
    assert_eq!(
        pixels.hit(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
        Hit::Nested(smallest)
    );
    assert_eq!(pixels.nested_path(smallest)[..2], ["big", "many"]);

    // Only a folder gives up its label for its entries: a file of the same height keeps it.
    let short = |file_type| Tile {
        x: 0,
        y: 0,
        width: 200,
        height: 30,
        name: "short".into(),
        size: 1,
        descendants: None,
        percentage: 0.1,
        file_type,
    };
    assert!(!pixels.labelled(&short(FileType::Folder)));
    assert!(pixels.labelled(&short(FileType::File)));
}

/// In pixel cells the "small files" corner is filled in: each entry too small for a tile is a
/// speck there, inside the corner, and no speck is an entry that has a tile. In the
/// terminal-shaped cells it stays a plain corner.
#[test]
fn the_small_files_corner_is_filled_with_a_speck_for_each_entry_in_pixel_cells() {
    let root = Path::new(ROOT);
    let tree = || {
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        tree.add_entry(meta(10_000_000, false), &root.join("huge.bin"));
        for index in 0..3000 {
            tree.add_entry(meta(100, false), &root.join(format!("f{index}.txt")));
        }
        tree
    };
    let mut cells = Viewer::new(root, SizeKind::Disk, 1);
    cells.resize(1200.0, 800.0);
    cells.finish_scan(tree());
    assert!(cells.board.unrenderable_tile_coordinates.is_some());
    assert!(cells.dust().is_empty());

    let mut pixels = Viewer::new(root, SizeKind::Disk, 1);
    pixels.set_pixel_scale(1.0);
    pixels.resize(1200.0, 800.0);
    pixels.finish_scan(tree());
    let (sx, sy) = pixels
        .board
        .unrenderable_tile_coordinates
        .expect("3000 files of 100 bytes beside 10 MB do not all get tiles");
    let hidden = pixels.board.hidden().len();
    assert_eq!(hidden + pixels.board.tiles.len(), 3001);
    assert!(
        pixels.dust().len() > hidden / 2,
        "{} of {hidden}",
        pixels.dust().len()
    );
    for dust in pixels.dust() {
        assert!(dust.x >= sx && dust.y >= sy, "{dust:?}");
        assert!(dust.x + dust.width <= pixels.layout.cols, "{dust:?}");
        assert!(dust.y + dust.height <= pixels.layout.rows, "{dust:?}");
    }
    // A click on a speck is the corner's: they are a picture, not targets.
    let dust = pixels.dust()[0];
    let rect = pixels
        .layout
        .cells_to_rect(dust.x, dust.y, dust.width, dust.height);
    assert_eq!(
        pixels.hit(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
        Hit::SmallFiles
    );
}

/// A folder tile's own "small files" corner is filled in too: a folder of many small entries
/// nested inside another shows them as specks inside its tile, not the folder's colour alone.
#[test]
fn a_nested_folders_small_files_corner_is_filled_with_specks() {
    let root = Path::new(ROOT);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    tree.add_entry(meta(0, true), &root.join("winsxs"));
    tree.add_entry(meta(2_000_000, false), &root.join("winsxs/big.cab"));
    for index in 0..3000 {
        tree.add_entry(meta(0, true), &root.join(format!("winsxs/c{index}")));
        tree.add_entry(
            meta(10, false),
            &root.join(format!("winsxs/c{index}/f.dll")),
        );
    }
    tree.add_entry(meta(1_000_000, false), &root.join("other.bin"));
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.set_tree_view(true);
    viewer.set_pixel_scale(1.0);
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    let winsxs = viewer
        .board
        .tiles
        .iter()
        .find(|tile| tile.name == "winsxs")
        .expect("winsxs has a tile")
        .clone();
    let inside: Vec<&Dust> = viewer
        .dust()
        .iter()
        .filter(|dust| {
            dust.x >= winsxs.x
                && dust.y >= winsxs.y
                && dust.x + dust.width <= winsxs.x + winsxs.width
                && dust.y + dust.height <= winsxs.y + winsxs.height
        })
        .collect();
    assert!(inside.len() > 1000, "{} specks inside winsxs", inside.len());
    // And none of them lies on one of the folder's tiles.
    for tile in viewer.nested().iter().filter(|t| t.depth == 1) {
        let t = &tile.tile;
        for dust in &inside {
            let apart = dust.x >= t.x + t.width
                || dust.x + dust.width <= t.x
                || dust.y >= t.y + t.height
                || dust.y + dust.height <= t.y;
            assert!(apart, "{dust:?} on {t:?}");
        }
    }
}

/// With the specks deferred, a layout not yet timed with them leaves them for the second pass,
/// which lays them out; one that was quick enough then lays them out at once.
#[test]
fn deferred_specks_come_in_a_second_pass_and_then_inline_once_they_are_quick() {
    let root = Path::new(ROOT);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    tree.add_entry(meta(10_000_000, false), &root.join("huge.bin"));
    for index in 0..500 {
        tree.add_entry(meta(100, false), &root.join(format!("f{index}.txt")));
    }
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.set_tree_view(true);
    viewer.set_pixel_scale(1.0);
    viewer.defer_to_second_pass(true);
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    assert!(viewer.second_pass_owed(), "not yet timed: the tiles first");
    assert!(viewer.dust().is_empty());
    let tiles = viewer.board.tiles.len();

    viewer.finish_second_pass();
    assert!(!viewer.second_pass_owed());
    assert!(
        !viewer.dust().is_empty(),
        "the second pass lays the specks out"
    );
    assert_eq!(
        viewer.board.tiles.len(),
        tiles,
        "and leaves the tiles as they were"
    );

    // A few hundred specks take well under the budget: the next relayout has them at once.
    viewer.resize(1100.0, 800.0);
    assert!(!viewer.second_pass_owed());
    assert!(!viewer.dust().is_empty());
}

/// A first pass cut at its deadline owes the second, which lays the nesting out as a single
/// pass without a deadline would.
#[test]
fn the_second_pass_completes_a_nesting_the_first_cut_short() {
    let root = Path::new(ROOT);
    let tree = || {
        let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
        for a in 0..6 {
            for b in 0..6 {
                for c in 0..4 {
                    let path = format!("d{a}/e{b}/f{c}.bin");
                    tree.add_entry(
                        meta(1_000 + (a * 36 + b * 6 + c) as u64, false),
                        &root.join(path),
                    );
                }
            }
        }
        tree
    };
    let mut whole = Viewer::new(root, SizeKind::Disk, 1);
    whole.set_tree_view(true);
    whole.set_pixel_scale(1.0);
    whole.resize(1200.0, 800.0);
    whole.finish_scan(tree());

    let mut passes = Viewer::new(root, SizeKind::Disk, 1);
    passes.set_tree_view(true);
    passes.set_pixel_scale(1.0);
    passes.defer_to_second_pass(true);
    passes.resize(1200.0, 800.0);
    passes.finish_scan(tree());
    // Not yet timed with the specks, so they wait at least.
    assert!(passes.second_pass_owed());
    let before = passes.layout_generation();
    passes.finish_second_pass();
    assert!(!passes.second_pass_owed());
    assert_ne!(
        passes.layout_generation(),
        before,
        "a new layout to paint in full"
    );
    let names = |viewer: &Viewer| {
        let mut names: Vec<Vec<OsString>> = (0..viewer.nested().len())
            .map(|index| viewer.nested_path(index))
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(&passes), names(&whole));
    assert_eq!(passes.dust().len(), whole.dust().len());
}

/// `viewer()`'s folder with `big/a` at `a` bytes.
fn tree_with(a: u64) -> FileTree {
    let root = Path::new(ROOT);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    for (path, size, is_dir) in [
        ("big", 0, true),
        ("big/a", a, false),
        ("big/b", 300, false),
        ("medium.txt", 400, false),
        ("small", 0, true),
        ("small/c", 100, false),
        ("tiny.bin", 50, false),
    ] {
        tree.add_entry(meta(size, is_dir), &root.join(path));
    }
    tree
}

#[test]
fn tiles_slide_to_a_new_layout_and_end_exactly_there() {
    let mut sliding = viewer();
    sliding.set_tree_view(true);
    sliding.set_animation(true);
    sliding.finish_scan(tree_with(100));
    assert!(sliding.animating());
    let mut still = viewer();
    still.set_tree_view(true);
    still.finish_scan(tree_with(100));
    assert!(!still.animating());
    // Part way, the board is neither layout; at the end it is the new one exactly.
    let start = Instant::now();
    assert!(sliding.animate(start + TWEEN / 2));
    assert!(!sliding.animate(start + TWEEN * 2));
    assert!(!sliding.animating());
    let board = |viewer: &Viewer| -> Vec<_> {
        viewer
            .board
            .tiles
            .iter()
            .map(|t| (t.name.clone(), t.x, t.y, t.width, t.height))
            .collect()
    };
    assert_eq!(board(&sliding), board(&still));
    let places = |viewer: &Viewer| -> Vec<_> {
        viewer
            .nested()
            .iter()
            .map(|n| {
                (
                    n.tile.name.clone(),
                    n.tile.x,
                    n.tile.y,
                    n.tile.width,
                    n.tile.height,
                    n.inside,
                )
            })
            .collect()
    };
    assert_eq!(places(&sliding), places(&still));
}

#[test]
fn a_zoom_during_a_slide_lays_the_tiles_out_where_the_zoom_puts_them() {
    let mut sliding = viewer();
    sliding.set_tree_view(true);
    sliding.set_animation(true);
    sliding.finish_scan(tree_with(100));
    assert!(sliding.animating());
    sliding.zoom_in();
    let mut still = viewer();
    still.set_tree_view(true);
    still.finish_scan(tree_with(100));
    still.zoom_in();
    let start = Instant::now();
    sliding.animate(start + TWEEN * 2);
    let board = |viewer: &Viewer| -> Vec<_> {
        viewer
            .board
            .tiles
            .iter()
            .map(|t| (t.name.clone(), t.x, t.y, t.width, t.height))
            .collect()
    };
    assert_eq!(board(&sliding), board(&still));
    let nested = |viewer: &Viewer| -> Vec<_> {
        viewer
            .nested()
            .iter()
            .map(|n| (n.tile.name.clone(), n.tile.x, n.tile.y, n.inside))
            .collect()
    };
    assert_eq!(nested(&sliding), nested(&still));
}

#[test]
fn the_details_follow_a_tile_the_pointer_rests_on_and_come_back_after_it_leaves() {
    use super::PEEK;
    let mut viewer = viewer();
    let in_hand = viewer.shown_entry().map(|entry| entry.name.clone());
    assert_eq!(in_hand.as_deref(), Some(OsStr::new("big")));
    let (x, y) = center_of(&viewer, "medium.txt");
    viewer.hover_at(x, y);
    assert!(viewer.peek_due().is_some_and(|due| due <= PEEK));
    assert!(!viewer.peek_tick(), "not before it has rested");
    assert_eq!(viewer.shown_entry().map(|e| e.name.clone()), in_hand);
    ::std::thread::sleep(PEEK + Duration::from_millis(10));
    assert!(viewer.peek_tick());
    assert_eq!(
        viewer.shown_entry().map(|e| e.name.clone()).as_deref(),
        Some(OsStr::new("medium.txt"))
    );
    assert_eq!(
        viewer.peek_due(),
        None,
        "nothing pending while it rests there"
    );
    let (generation, path) = viewer.wanted_preview().expect("the file's preview");
    assert!(path.ends_with("medium.txt") && generation > 0);
    // Off the tile: the entry in hand comes back after PEEK, not at once.
    viewer.hover_at(-1.0, -1.0);
    assert!(viewer.peek_due().is_some());
    assert!(!viewer.peek_tick());
    ::std::thread::sleep(PEEK + Duration::from_millis(10));
    assert!(viewer.peek_tick());
    assert_eq!(viewer.shown_entry().map(|e| e.name.clone()), in_hand);
    assert_eq!(viewer.peek_due(), None);
    assert!(viewer.wanted_preview().is_none(), "a folder: no preview");
    // A key press ends it at once.
    viewer.hover_at(x, y);
    ::std::thread::sleep(PEEK + Duration::from_millis(10));
    assert!(viewer.peek_tick());
    viewer.arrow(Direction::Down, false);
    assert_ne!(viewer.shown_entry().map(|e| e.name.clone()), in_hand);
    assert_eq!(viewer.peek_due(), None);
}

/// The status line says what a NAS's own folder is, beside its name.
#[test]
fn a_nas_folder_is_described_in_the_status_line() {
    let entry = FileMetadata {
        name: OsString::from("#recycle"),
        size: 4096,
        descendants: Some(3),
        percentage: 0.5,
        file_type: FileType::Folder,
    };
    let words = describe(&entry);
    assert!(
        words.ends_with("folder, 3 items · Synology: the share's recycle bin"),
        "{words}"
    );
    let plain = FileMetadata {
        name: OsString::from("recycle"),
        ..entry.clone()
    };
    assert!(
        describe(&plain).ends_with("folder, 3 items"),
        "{}",
        describe(&plain)
    );
    let file = FileMetadata {
        file_type: FileType::File,
        descendants: None,
        ..entry
    };
    assert!(
        describe(&file).ends_with(" · file"),
        "a file of the name is a file: {}",
        describe(&file)
    );
}

#[test]
fn a_specks_colour_is_its_tiles_whatever_came_before_it() {
    // Runs of a kind, kinds apart only by case, and no extension beside an empty one.
    let names = [
        "Makefile", "notes.", "Makefile", "a.RS", "b.rs", "c.rs", "notes.", "d.txt", "e.rs",
    ];
    let mut colors = SpeckColors::default();
    for depth in [0, 2, 0] {
        for name in names {
            let name = OsStr::new(name);
            assert_eq!(
                colors.file(name, depth),
                entry_color(name, FileType::File, depth),
                "{name:?} at {depth}"
            );
        }
    }
}

/// What `viewer()` builds, with the volume's free space answered by `source`: the test's
/// root is no volume's, so the OS would say none.
fn viewer_with_free(source: fn(&Path) -> Option<(u64, u64)>) -> Viewer {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.set_volume_free_source(source);
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

/// As much free as used, and the scan has found all that is used.
fn as_much_free_as_used(_: &Path) -> Option<(u64, u64)> {
    Some((1450, 1450))
}

/// The volume says it uses more than the scan finds: 1450 found of 2900 used, 2900 free.
fn half_unscanned(_: &Path) -> Option<(u64, u64)> {
    Some((2900, 2900))
}

fn no_volume(_: &Path) -> Option<(u64, u64)> {
    None
}

/// The used space the scan has not found is a tile of its own before the free space, which
/// is along the treemap's bottom; neither is a target.
#[test]
fn unscanned_space_is_shown_beside_the_free_space() {
    let viewer = viewer_with_free(half_unscanned);
    let board = &viewer.board;
    let free = &board.tiles[board.free_tile().expect("a free-space tile")];
    let unscanned_index = board.unscanned_tile().expect("an unscanned tile");
    let unscanned = &board.tiles[unscanned_index];
    assert!((free.percentage - 0.5).abs() < 1e-9);
    assert!((unscanned.percentage - 0.25).abs() < 1e-9);
    assert_eq!(unscanned.size, 1450);
    let right = board
        .tiles
        .iter()
        .map(|tile| tile.x + tile.width)
        .max()
        .unwrap_or_default();
    let bottom = board
        .tiles
        .iter()
        .map(|tile| tile.y + tile.height)
        .max()
        .unwrap_or_default();
    assert_eq!((free.x + free.width, free.y + free.height), (right, bottom));
    assert_eq!(viewer.board_color(unscanned_index), UNSCANNED_COLOR);
    assert_eq!(
        unscanned.name, "Not seen by the scan",
        "the scan is over: what is left, no walk saw"
    );
    let (x, y) = center_of(&viewer, "Not seen by the scan");
    assert!(matches!(viewer.hit(x, y), Hit::Nothing));
}

/// What is unscanned is counted on disk whatever is shown: the volume counts blocks.
#[test]
fn unscanned_space_is_counted_on_disk_when_lengths_are_shown() {
    let mut viewer = viewer_with_free(half_unscanned);
    let on_disk = viewer.board.tiles[viewer.board.unscanned_tile().expect("unscanned")].size;
    viewer.toggle_size();
    assert!(viewer.showing_apparent());
    let shown = viewer.board.tiles[viewer.board.unscanned_tile().expect("unscanned")].size;
    assert_eq!(shown, on_disk);
}

/// At the root of a volume the free space is a tile of its share — half, here — beside the
/// entries, on by default and offered as a toggle; it is no entry: not listed, not a target,
/// never in hand. Off, or in a folder below the root, the entries have the board to themselves.
#[test]
fn free_space_is_shown_at_a_volumes_root_and_is_no_entry() {
    let mut viewer = viewer_with_free(as_much_free_as_used);
    assert_eq!(viewer.free_toggle(), Some(("Free space", true)));
    let free = viewer.board.free_tile().expect("a free-space tile");
    assert!((viewer.board.tiles[free].percentage - 0.5).abs() < 1e-9);
    assert_eq!(viewer.board_color(free), FREE_SPACE_COLOR);
    assert!(
        !names(&viewer).iter().any(|name| name == "Free space"),
        "the list holds entries alone: {:?}",
        names(&viewer)
    );
    let (x, y) = center_of(&viewer, "Free space");
    assert!(matches!(viewer.hit(x, y), Hit::Nothing));
    assert!(viewer.click(x, y, Mods::default()).is_none());
    assert_eq!(
        selected(&viewer).as_deref(),
        Some("big"),
        "the entry placed in hand at the scan's end stays there"
    );

    // The keyboard never lands on it.
    viewer.click(
        center_of(&viewer, "big").0,
        center_of(&viewer, "big").1,
        Mods::default(),
    );
    for _ in 0..6 {
        for direction in [
            Direction::Right,
            Direction::Down,
            Direction::Left,
            Direction::Up,
        ] {
            viewer.arrow(direction, false);
            assert_ne!(selected(&viewer).as_deref(), Some("Free space"));
        }
    }

    viewer.toggle_free_space();
    assert_eq!(viewer.free_toggle(), Some(("Free space", false)));
    assert!(viewer.board.free_tile().is_none());
    viewer.toggle_free_space();
    assert!(viewer.board.free_tile().is_some());

    // Below the root there is no volume to show, and no toggle to offer.
    viewer.select(Some("big".into()), true);
    assert!(viewer.enter_selected());
    assert_eq!(viewer.free_toggle(), None);
    assert!(viewer.board.free_tile().is_none());
    assert!(viewer.go_up());
    assert_eq!(viewer.free_toggle(), Some(("Free space", true)));
    assert!(viewer.board.free_tile().is_some());
}

/// A root that is no volume's — a folder — shows the entries alone and offers no toggle.
#[test]
fn no_free_space_is_shown_off_a_volumes_root() {
    let viewer = viewer_with_free(no_volume);
    assert_eq!(viewer.free_toggle(), None);
    assert!(viewer.board.free_tile().is_none());
    assert!(
        (viewer
            .board
            .tiles
            .iter()
            .map(|tile| tile.percentage)
            .sum::<f64>()
            - 1.0)
            .abs()
            < 1e-9
    );
}

/// A Shift+click marks every entry from the one in hand to the one clicked, in the list and on
/// the treemap alike; a later Shift+click moves the range's far end, the anchor staying.
#[test]
fn a_shift_click_marks_the_range_from_the_entry_in_hand() {
    let dir = on_disk("shift_click");
    let (mut viewer, _) = viewer_on(&dir);
    let shift = Mods {
        toggle: false,
        range: true,
    };
    assert_eq!(names(&viewer), ["big", "medium.txt", "small", "tiny.txt"]);
    let (x, y) = row_center(&viewer, "big");
    viewer.click(x, y, Mods::default());
    let (x, y) = row_center(&viewer, "small");
    viewer.click(x, y, shift);
    assert_eq!(marks(&viewer), ["big", "medium.txt", "small"]);
    let (x, y) = center_of(&viewer, "tiny.txt");
    viewer.click(x, y, shift);
    assert_eq!(marks(&viewer), ["big", "medium.txt", "small", "tiny.txt"]);
    let (x, y) = row_center(&viewer, "medium.txt");
    viewer.click(x, y, shift);
    assert_eq!(marks(&viewer), ["big", "medium.txt"], "shrunk back");
    assert_eq!(viewer.targets().len(), 2, "what a delete takes");
    let _ = fs::remove_dir_all(&dir);
}

/// In the tree view a ⇧ range runs over the rows on show, a folder's opened in place with the
/// rest, and marks those rows: a click, a ⇧+click and ⇧+arrows alike. A delete takes a folder
/// and not also the rows marked inside it; closing a folder takes its rows' marks off.
#[test]
fn a_shift_range_marks_the_rows_of_a_folder_opened_in_place() {
    let mut viewer = tree_viewer();
    let shift = Mods {
        toggle: false,
        range: true,
    };
    viewer.toggle_row(0);
    assert_eq!(
        rows_of(&viewer),
        ["big/", "  a", "  b", "medium.txt", "small", "tiny.bin"]
    );
    let list = viewer.layout.list.expect("a list");
    let row = |index: usize| (list.x + 100.0, list.y + ROW * (index as f64 + 0.5));
    let (x, y) = row(1);
    viewer.click(x, y, Mods::default());
    let (x, y) = row(2);
    viewer.click(x, y, shift);
    assert_eq!(marks(&viewer), ["big/a", "big/b"], "a's and b's, not big's");
    let (left, _) = viewer.status();
    assert!(left.starts_with("2 marked"), "{left}");
    // On down past the folder's end: the range keeps its anchor, and runs on.
    viewer.arrow(Direction::Down, true);
    assert_eq!(marks(&viewer), ["big/a", "big/b", "medium.txt"]);
    // Back up to the folder: its row joins, and a delete takes it whole, its rows with it.
    let (x, y) = row(0);
    viewer.click(x, y, shift);
    assert_eq!(
        marks(&viewer),
        ["big/a", "big"],
        "swept back up from the anchor"
    );
    // Counted as what a command acts on: big, its row a with it.
    assert_eq!(viewer.marked_count(), 1);
    let (left, _) = viewer.status();
    assert!(left.starts_with("1 marked"), "{left}");
    let platform = crate::menu::Platform {
        reveal: "Show",
        quick_look: false,
        pathname: false,
        trash: false,
        about: false,
    };
    let labels: Vec<String> = viewer
        .context_menu(&platform)
        .into_iter()
        .filter_map(|entry| match entry {
            crate::menu::Entry::Item { label, .. } => Some(label),
            crate::menu::Entry::Separator => None,
        })
        .collect();
    assert!(
        labels.iter().all(|label| !label.contains('2')),
        "one item, not two: {labels:?}"
    );
    let targets: Vec<String> = viewer
        .target_rows()
        .iter()
        .map(|path| {
            path.iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    assert_eq!(targets, ["big"], "big's rows go with it");
    assert_eq!(viewer.targets().len(), 1);
    // Closed, big's rows are off show and so are their marks; big's own stays.
    viewer.toggle_row(0);
    assert_eq!(marks(&viewer), ["big"]);
}

/// An unframed speck has the least tiles' grid over it, aligned to the board so the lines run
/// on across a corner, and is darker under it; one with room for a frame has neither.
#[test]
fn a_speck_too_small_for_a_frame_has_the_least_tiles_grid_over_it() {
    let speck = |x, y, width, height, framed| Dust {
        x,
        y,
        width,
        height,
        color: (0.5, 0.5, 0.5),
        framed,
    };
    let pitch = MIN_TILE_PIXELS;
    // One pixel on a grid column: a line of its own height. Off it: nothing.
    assert_eq!(
        speck(pitch, 1, 1, 1, false).grid().collect::<Vec<_>>(),
        [(pitch, 1, 1, 1)]
    );
    assert_eq!(speck(pitch + 1, 1, 1, 1, false).grid().count(), 0);
    // Wider than the pitch: a column and a row a pitch apart, at the board's multiples.
    let lines: Vec<_> = speck(pitch - 1, pitch - 1, pitch + 2, 2, false)
        .grid()
        .collect();
    assert_eq!(
        lines,
        [
            (pitch, pitch - 1, 1, 2),
            (2 * pitch, pitch - 1, 1, 2),
            (pitch - 1, pitch, pitch + 2, 1),
        ]
    );
    assert_eq!(speck(0, 0, 2 * pitch, 2 * pitch, true).grid().count(), 0);
    const { assert!(SPECK_SHADE < 1.0 && SPECK_SHADE > 0.5) };
}

/// Ctrl+A on a folder of tens of thousands of entries, and what every frame then asks of the
/// marks — the status's count and size, each tile's and row's mark, the delete's targets —
/// costs a lookup a question, not a pass over the marks: quadratic, this was billions of
/// comparisons on the keypress and again every paint.
#[test]
fn marking_a_folder_of_many_entries_stays_quick() {
    const FILES: usize = 40_000;
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    for index in 0..FILES {
        tree.add_entry(
            meta(1000 + index as u64, false),
            &root.join(format!("f{index}")),
        );
    }
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    let started = Instant::now();
    viewer.mark_all();
    assert_eq!(viewer.marked.len(), FILES);
    for _ in 0..3 {
        let (left, _) = viewer.status();
        assert!(left.starts_with("40,000 marked"), "{left}");
        let names: Vec<OsString> = viewer
            .board
            .listing()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        assert!(names.iter().all(|name| viewer.is_marked(name)));
    }
    assert_eq!(viewer.targets().len(), FILES);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

/// A remove that panics is that entry's failure, reported like any other: the delete still
/// ends, and the window still gets its input back.
#[test]
fn a_panic_while_deleting_is_a_failure_not_a_stuck_window() {
    let dir = on_disk("delete_panic");
    let (mut viewer, _) = viewer_on(&dir);
    viewer.mark_all();
    let files = viewer.targets();
    let (sender, report) = mpsc::channel();
    viewer
        .start_delete(
            files,
            true,
            |_, _| panic!("a remove that panics"),
            move |id, ended| {
                let _ = sender.send((id, ended));
            },
        )
        .expect("started");
    let (id, ended) = report
        .recv_timeout(Duration::from_secs(5))
        .expect("reported");
    let failure = viewer.delete_done(id, &ended).expect("a failure");
    assert!(failure.detail.contains("unexpectedly"), "{failure:?}");
    assert!(viewer.deleting().is_none(), "the window takes input again");
    let _ = fs::remove_dir_all(&dir);
}

/// A share's volume is the server's: what of it the scan did not find is the server's other
/// shares, so a share's root shows the free space and no "not seen by the scan" strip.
#[test]
fn a_network_share_shows_no_unscanned_strip() {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    viewer.set_volume_free_source(half_unscanned);
    viewer.set_network_root(true);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    tree.add_entry(meta(1450, false), &root.join("film.mkv"));
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    assert!(
        viewer.board.free_tile().is_some(),
        "the server's free space"
    );
    assert!(
        viewer.board.unscanned_tile().is_none(),
        "no strip of other shares"
    );
}

/// One entry is one entry: the status says so in the singular.
#[test]
fn a_single_entry_is_counted_in_the_singular() {
    let root = Path::new(ROOT);
    let mut viewer = Viewer::new(root, SizeKind::Disk, 1);
    let mut tree = FileTree::new(Folder::new(root), root.to_path_buf());
    tree.add_entry(meta(100, false), &root.join("only.txt"));
    viewer.resize(1200.0, 800.0);
    viewer.finish_scan(tree);
    let (_, right) = viewer.status();
    assert!(right.starts_with("1 entry in "), "{right}");
}
