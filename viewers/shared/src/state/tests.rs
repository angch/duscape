use super::*;
use libdiskonaut::EntryMeta;

const ROOT: &str = "/diskonaut-mac-test-root";

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

#[test]
fn command_click_toggles_marks_starting_from_the_entry_in_hand() {
    let mut viewer = viewer();
    let toggle = Mods {
        toggle: true,
        range: false,
    };
    let (x, y) = center_of(&viewer, "small");
    viewer.click(x, y, toggle);
    assert_eq!(viewer.marked, ["big", "small"]);
    viewer.click(x, y, toggle);
    assert_eq!(viewer.marked, ["big"]);
    assert_eq!(viewer.target_names(), ["big"]);
    // A plain click clears the marks.
    viewer.click(x, y, Mods::default());
    assert!(viewer.marked.is_empty());
    assert_eq!(viewer.target_names(), ["small"]);
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
    viewer.add_summaries(vec![DirSummary::of(&directory)]);
    assert_eq!(names(&viewer), ["folder"]);
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
