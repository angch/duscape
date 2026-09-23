use ::std::ffi::{OsStr, OsString};
use ::std::fs::{self, File};
use ::std::io::Write;
use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc;

use libdiskonaut::{DirEntries, FileTree, Folder, ScanOptions, scan_into_tree};
use ratatui::backend::TestBackend;

use super::{App, UiMode};
use crate::config::Keybinds;

fn temp_app_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("diskonaut_app_test_{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    // Canonicalized because the app always scans a canonical path: `Opts::resolve_folder`
    // resolves the folder before it reaches `FileTree`, which canonicalizes its own root too. On
    // macOS `temp_dir()` is `/var/...`, a symlink to `/private/var/...`, so an uncanonicalized
    // path here fails `strip_prefix` against that root and every entry is silently dropped.
    dir.canonicalize().expect("canonicalize temp dir")
}

fn app_with_scanned_dir(dir: &Path, width: u16, height: u16) -> App<TestBackend> {
    let (tx, _rx) = mpsc::sync_channel(1);
    let mut app = App::new(
        TestBackend::new(width, height),
        dir.to_path_buf(),
        tx,
        Keybinds::default(),
        false,
    );
    let options = ScanOptions {
        parallel: false,
        show_apparent_size: true,
        ..ScanOptions::default()
    };
    let (tree, _) = scan_into_tree(dir, options);
    app.finish_scan(tree);
    app.start_ui();
    app
}

#[test]
fn render_marks_screen_too_small_below_minimum_size() {
    let dir = temp_app_dir("too_small");
    let mut app = app_with_scanned_dir(&dir, 40, 10);
    app.render();
    assert!(matches!(app.ui_mode, UiMode::ScreenTooSmall));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_selected_enters_subfolder() {
    let dir = temp_app_dir("enter_subfolder");
    let sub = dir.join("sub");
    fs::create_dir(&sub).expect("create subfolder");
    let mut big_in_sub = File::create(sub.join("large.dat")).expect("create file in sub");
    big_in_sub
        .write_all(&vec![b'x'; 8192])
        .expect("write subfolder data");
    drop(big_in_sub);
    File::create(dir.join("tiny.txt"))
        .expect("create small root file")
        .write_all(b"x")
        .expect("write root file");

    let mut app = app_with_scanned_dir(&dir, 80, 24);
    assert!(
        app.file_tree
            .item_in_current_folder(OsStr::new("sub"))
            .is_some(),
        "scan should register subfolder in tree"
    );
    app.board.move_to_largest_folder();
    let selected = app
        .board
        .currently_selected()
        .expect("a folder tile should be selected");
    assert_eq!(selected.name, OsStr::new("sub"));
    assert_eq!(selected.file_type, libdiskonaut::tiles::FileType::Folder);
    app.handle_enter();

    assert_eq!(app.file_tree.get_current_path(), sub);
    assert_eq!(
        app.file_tree.current_folder_names,
        vec![OsString::from("sub")]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prompt_file_deletion_shows_confirmation() {
    let dir = temp_app_dir("delete_confirm");
    let target = dir.join("remove_me.txt");
    File::create(&target).expect("create file");

    let (tx, _rx) = mpsc::sync_channel(1);
    let mut app = App::new(
        TestBackend::new(80, 24),
        dir.clone(),
        tx,
        Keybinds::default(),
        false,
    );
    let meta = fs::metadata(&target).expect("metadata");
    let mut scanned = DirEntries::new(std::sync::Arc::from(dir.as_path()));
    scanned.push(
        OsStr::new("remove_me.txt"),
        libdiskonaut::EntryMeta {
            size: meta.len(),
            links: 1,
            is_dir: false,
            ..libdiskonaut::EntryMeta::default()
        },
    );
    let mut tree = FileTree::new(Folder::new(&dir), dir.clone());
    tree.add_dir_entries(scanned);
    app.finish_scan(tree);
    app.start_ui();
    let file_index = app
        .board
        .tiles
        .iter()
        .position(|t| t.name == OsStr::new("remove_me.txt"))
        .expect("file tile");
    app.board.set_selected_index(&file_index);
    app.prompt_file_deletion();

    assert!(target.exists(), "file should remain until user confirms");
    assert!(matches!(app.ui_mode, UiMode::DeleteFile(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reset_ui_mode_from_error_returns_to_normal() {
    let dir = temp_app_dir("reset_mode");
    let mut app = app_with_scanned_dir(&dir, 80, 24);
    app.ui_mode = UiMode::ErrorMessage("oops".into());
    app.reset_ui_mode();
    assert!(matches!(app.ui_mode, UiMode::Normal));
    let _ = fs::remove_dir_all(&dir);
}

/// A folder `big` holding most of the data, a smaller folder `small`, and a loose file, so each
/// gets a tile of its own.
fn app_with_two_folders(name: &str) -> (PathBuf, App<TestBackend>) {
    let dir = temp_app_dir(name);
    for (folder, bytes) in [("big", 64 * 1024), ("small", 16 * 1024)] {
        fs::create_dir(dir.join(folder)).expect("create folder");
        File::create(dir.join(folder).join("data"))
            .expect("create file")
            .write_all(&vec![b'x'; bytes])
            .expect("write file");
    }
    File::create(dir.join("loose.txt"))
        .expect("create file")
        .write_all(&vec![b'x'; 8 * 1024])
        .expect("write file");
    let app = app_with_scanned_dir(&dir, 80, 24);
    (dir, app)
}

/// The middle of the named tile, where a user would click.
fn centre_of(app: &App<TestBackend>, name: &str) -> (u16, u16) {
    let tile = app
        .board
        .tiles
        .iter()
        .find(|tile| tile.name == OsStr::new(name))
        .unwrap_or_else(|| panic!("no tile for {name}"));
    (tile.x + tile.width / 2, tile.y + tile.height / 2)
}

fn selected_name(app: &App<TestBackend>) -> Option<OsString> {
    app.board.currently_selected().map(|tile| tile.name.clone())
}

#[test]
fn a_click_selects_the_tile_under_the_pointer() {
    let (dir, mut app) = app_with_two_folders("click_selects");
    let (column, row) = centre_of(&app, "small");
    app.click(column, row);

    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    assert!(
        app.file_tree.current_folder_names.is_empty(),
        "a single click must not enter the folder"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_double_click_enters_the_folder() {
    let (dir, mut app) = app_with_two_folders("double_click_enters");
    let (column, row) = centre_of(&app, "small");
    let start = ::std::time::Instant::now();
    app.click_at(column, row, start);
    app.click_at(column, row, start + ::std::time::Duration::from_millis(200));

    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_slow_clicks_only_select() {
    let (dir, mut app) = app_with_two_folders("slow_clicks");
    let (column, row) = centre_of(&app, "small");
    let start = ::std::time::Instant::now();
    app.click_at(column, row, start);
    app.click_at(column, row, start + ::std::time::Duration::from_millis(900));

    assert!(app.file_tree.current_folder_names.is_empty());
    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn quick_clicks_on_two_tiles_select_the_second() {
    let (dir, mut app) = app_with_two_folders("two_tiles");
    let (small_column, small_row) = centre_of(&app, "small");
    let (big_column, big_row) = centre_of(&app, "big");
    let start = ::std::time::Instant::now();
    app.click_at(small_column, small_row, start);
    app.click_at(
        big_column,
        big_row,
        start + ::std::time::Duration::from_millis(100),
    );

    assert!(app.file_tree.current_folder_names.is_empty());
    assert_eq!(selected_name(&app), Some(OsString::from("big")));
    let _ = fs::remove_dir_all(&dir);
}

/// The press that enters a folder is not the first click of the next double click: a third quick
/// click lands on whatever tile is now under the pointer and only selects it.
#[test]
fn a_third_quick_click_after_entering_only_selects() {
    let (dir, mut app) = app_with_two_folders("third_click");
    let (column, row) = centre_of(&app, "big");
    let start = ::std::time::Instant::now();
    app.click_at(column, row, start);
    app.click_at(column, row, start + ::std::time::Duration::from_millis(100));
    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));

    let (column, row) = centre_of(&app, "data");
    app.click_at(column, row, start + ::std::time::Duration::from_millis(200));
    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));
    assert_eq!(selected_name(&app), Some(OsString::from("data")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_double_click_on_a_file_stays_put() {
    let (dir, mut app) = app_with_two_folders("double_click_file");
    let (column, row) = centre_of(&app, "loose.txt");
    let start = ::std::time::Instant::now();
    app.click_at(column, row, start);
    app.click_at(column, row, start + ::std::time::Duration::from_millis(100));

    assert!(app.file_tree.current_folder_names.is_empty());
    assert_eq!(selected_name(&app), Some(OsString::from("loose.txt")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_click_outside_every_tile_changes_nothing() {
    let (dir, mut app) = app_with_two_folders("click_outside");
    let (column, row) = centre_of(&app, "small");
    app.click(column, row);
    app.click(0, 0); // the title line

    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    let _ = fs::remove_dir_all(&dir);
}

/// The full path a real click takes: a crossterm mouse press through the normal-mode handler.
#[test]
fn a_mouse_press_event_reaches_the_board() {
    use ratatui::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let (dir, mut app) = app_with_two_folders("mouse_event");
    let (column, row) = centre_of(&app, "small");
    let press = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    });
    crate::input::handle_keypress_normal_mode(press.clone(), &mut app);
    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    crate::input::handle_keypress_normal_mode(press, &mut app);
    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    let _ = fs::remove_dir_all(&dir);
}

/// A double click needs both clicks on the same tile. If the board is laid out again between
/// them — as when the finished tree replaces the scan's outline — a tile at the same index is a
/// different tile, and the second click only selects it.
#[test]
fn a_relayout_between_clicks_is_not_a_double_click() {
    let (dir, mut app) = app_with_two_folders("relayout");
    let (column, row) = centre_of(&app, "small");
    let start = ::std::time::Instant::now();
    app.click_at(column, row, start);
    let index = app.board.get_selected_index().expect("selected");
    // Stand in for the relayout: whatever sits at that index now has another name.
    app.board.tiles[index].name = OsString::from("renamed");
    app.click_at(column, row, start + ::std::time::Duration::from_millis(100));

    assert!(app.file_tree.current_folder_names.is_empty());
    let _ = fs::remove_dir_all(&dir);
}
