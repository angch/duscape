use ::std::ffi::{OsStr, OsString};
use ::std::fs::{self, File};
use ::std::io::Write;
use ::std::path::{Path, PathBuf};
use ::std::sync::mpsc;

use libdiskonaut::{DirEntries, FileTree, Folder, ScanOptions, scan_into_tree};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::MouseButton;

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
    assert!(matches!(app.ui_mode, UiMode::DeleteFiles(_)));
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

/// A folder `big` holding most of the data, a file `loose.txt`, and a smaller folder `small`, each
/// large enough for a tile of its own.
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
        .write_all(&vec![b'x'; 24 * 1024])
        .expect("write file");
    // Wide enough that the treemap, two thirds of it beside the side panel, gives each entry a
    // tile of its own.
    let app = app_with_scanned_dir(&dir, 120, 30);
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
    app.click(MouseButton::Left, column, row);

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
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(200),
    );

    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_slow_clicks_only_select() {
    let (dir, mut app) = app_with_two_folders("slow_clicks");
    let (column, row) = centre_of(&app, "small");
    let start = ::std::time::Instant::now();
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(900),
    );

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
    app.click_at(MouseButton::Left, small_column, small_row, start);
    app.click_at(
        MouseButton::Left,
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
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(100),
    );
    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));

    let (column, row) = centre_of(&app, "data");
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(200),
    );
    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));
    assert_eq!(selected_name(&app), Some(OsString::from("data")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_double_click_on_a_file_stays_put() {
    let (dir, mut app) = app_with_two_folders("double_click_file");
    let (column, row) = centre_of(&app, "loose.txt");
    let start = ::std::time::Instant::now();
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(100),
    );

    assert!(app.file_tree.current_folder_names.is_empty());
    assert_eq!(selected_name(&app), Some(OsString::from("loose.txt")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_click_outside_every_tile_changes_nothing() {
    let (dir, mut app) = app_with_two_folders("click_outside");
    let (column, row) = centre_of(&app, "small");
    app.click(MouseButton::Left, column, row);
    app.click(MouseButton::Left, 0, 0); // the title line

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
    app.click_at(MouseButton::Left, column, row, start);
    let index = app.board.get_selected_index().expect("selected");
    // Stand in for the relayout: whatever sits at that index now has another name.
    app.board.tiles[index].name = OsString::from("renamed");
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(100),
    );

    assert!(app.file_tree.current_folder_names.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

/// Collects what the app copies instead of touching the real clipboard.
#[derive(Clone, Default)]
struct Recorder(::std::sync::Arc<::std::sync::Mutex<Vec<String>>>);

impl crate::clipboard::Clipboard for Recorder {
    fn copy(&mut self, text: &str) {
        self.0.lock().expect("recorder").push(text.to_string());
    }
}

impl Recorder {
    fn copied(&self) -> Vec<String> {
        self.0.lock().expect("recorder").clone()
    }
}

/// Record copies, as if diskonaut had been started from `working_dir`.
fn recording(app: &mut App<TestBackend>, working_dir: &Path) -> Recorder {
    let recorder = Recorder::default();
    app.set_clipboard(Box::new(recorder.clone()));
    app.set_working_dir(Some(working_dir.to_path_buf()));
    recorder
}

fn right_click(app: &mut App<TestBackend>, name: &str, at: ::std::time::Instant) {
    let (column, row) = centre_of(app, name);
    app.click_at(MouseButton::Right, column, row, at);
}

#[test]
fn a_right_click_copies_the_relative_path_and_selects() {
    let (dir, mut app) = app_with_two_folders("right_click");
    let recorder = recording(&mut app, &dir);
    let now = ::std::time::Instant::now();
    right_click(&mut app, "small", now);

    assert_eq!(recorder.copied(), vec!["small".to_string()]);
    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    assert!(
        app.file_tree.current_folder_names.is_empty(),
        "a right click never enters"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_double_right_click_copies_the_absolute_path() {
    let (dir, mut app) = app_with_two_folders("double_right_click");
    let recorder = recording(&mut app, &dir);
    let start = ::std::time::Instant::now();
    right_click(&mut app, "small", start);
    right_click(
        &mut app,
        "small",
        start + ::std::time::Duration::from_millis(150),
    );

    let absolute = libdiskonaut::format::quote_path_for_shell(&dir.join("small"));
    assert_eq!(recorder.copied(), vec!["small".to_string(), absolute]);
    assert!(app.file_tree.current_folder_names.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

/// Started from the scan root, a relative path inside a folder includes that folder.
#[test]
fn the_relative_path_includes_the_folders_entered() {
    let (dir, mut app) = app_with_two_folders("right_click_nested");
    let recorder = recording(&mut app, &dir);
    app.board.move_to_largest_folder();
    app.handle_enter();
    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));
    right_click(&mut app, "data", ::std::time::Instant::now());

    assert_eq!(recorder.copied(), vec!["big/data".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn copied_paths_are_quoted_for_the_shell() {
    let dir = temp_app_dir("right_click_quoting");
    for name in ["it's a file", "-rf"] {
        File::create(dir.join(name))
            .expect("create file")
            .write_all(&[b'x'; 4096])
            .expect("write file");
    }
    let mut app = app_with_scanned_dir(&dir, 80, 24);
    let recorder = recording(&mut app, &dir);
    right_click(&mut app, "it's a file", ::std::time::Instant::now());
    right_click(
        &mut app,
        "-rf",
        ::std::time::Instant::now() + ::std::time::Duration::from_secs(5),
    );

    assert_eq!(
        recorder.copied(),
        vec![r"'it'\''s a file'".to_string(), "./-rf".to_string()]
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A right click then a left click on one tile is not a double click of either kind.
#[test]
fn mixed_buttons_are_not_a_double_click() {
    let (dir, mut app) = app_with_two_folders("mixed_buttons");
    let recorder = recording(&mut app, &dir);
    let start = ::std::time::Instant::now();
    right_click(&mut app, "small", start);
    let (column, row) = centre_of(&app, "small");
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(100),
    );

    assert!(
        app.file_tree.current_folder_names.is_empty(),
        "did not enter"
    );
    assert_eq!(
        recorder.copied(),
        vec!["small".to_string()],
        "no absolute copy"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The title shows what was copied, then drops it once the flash has run out.
#[test]
fn a_copy_flashes_in_the_title_and_then_expires() {
    let (dir, mut app) = app_with_two_folders("copy_flash");
    let _recorder = recording(&mut app, &dir);
    let now = ::std::time::Instant::now();
    right_click(&mut app, "small", now);

    assert_eq!(
        app.ui_effects.clipboard_flash_at(now),
        Some("relative path: small")
    );
    assert_eq!(
        app.ui_effects
            .clipboard_flash_at(now + ::std::time::Duration::from_secs(3)),
        None
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The case that defines "relative": in `/home/user/foo`, `diskonaut ../bar/` with `baz` selected
/// copies `../bar/baz` — relative to where the command was run, not to the folder it was given.
#[test]
fn relative_paths_start_from_the_working_directory() {
    let base = temp_app_dir("right_click_cwd");
    let (foo, bar) = (base.join("foo"), base.join("bar"));
    fs::create_dir_all(&foo).expect("create foo");
    fs::create_dir_all(bar.join("baz")).expect("create bar/baz");
    File::create(bar.join("baz").join("data"))
        .expect("create file")
        .write_all(&[b'x'; 8192])
        .expect("write file");
    let mut app = app_with_scanned_dir(&bar, 80, 24);
    let recorder = recording(&mut app, &foo);
    right_click(&mut app, "baz", ::std::time::Instant::now());

    assert_eq!(recorder.copied(), vec!["../bar/baz".to_string()]);
    assert_eq!(
        app.ui_effects
            .clipboard_flash_at(::std::time::Instant::now()),
        Some("relative path: ../bar/baz")
    );
    let _ = fs::remove_dir_all(&base);
}

/// Started from inside the scanned tree, a relative path can climb out of the working directory
/// to a sibling.
#[test]
fn relative_paths_climb_out_of_a_working_directory_inside_the_scan() {
    let (dir, mut app) = app_with_two_folders("right_click_cwd_inside");
    let recorder = recording(&mut app, &dir.join("big"));
    right_click(&mut app, "small", ::std::time::Instant::now());

    assert_eq!(recorder.copied(), vec!["../small".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

/// With no working directory to start from, a relative copy is the absolute path, and the title
/// says it is absolute rather than claim otherwise.
#[test]
fn an_unknown_working_directory_copies_the_absolute_path() {
    let (dir, mut app) = app_with_two_folders("right_click_no_cwd");
    let recorder = recording(&mut app, &dir);
    app.set_working_dir(None);
    let now = ::std::time::Instant::now();
    right_click(&mut app, "small", now);

    let absolute = libdiskonaut::format::quote_path_for_shell(&dir.join("small"));
    assert_eq!(recorder.copied(), vec![absolute.clone()]);
    assert_eq!(
        app.ui_effects.clipboard_flash_at(now),
        Some(format!("absolute path: {absolute}").as_str())
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The side-panel cell showing `name`'s row, as drawn for the app's current selection.
fn list_row_of(app: &App<TestBackend>, name: &str) -> (u16, u16) {
    use crate::ui::side_panel::{HEADER_ROWS, list_window};
    let panel = app
        .display
        .areas()
        .side_panel
        .expect("wide enough for the side panel");
    let listing = app.board.listing();
    let index = listing
        .iter()
        .position(|entry| entry.name == OsStr::new(name))
        .unwrap_or_else(|| panic!("{name} is not listed"));
    let window = list_window(
        listing.len(),
        app.highlighted_listing_index(),
        usize::from(panel.height - HEADER_ROWS),
    );
    assert!(window.contains(&index), "{name} is scrolled out of view");
    let offset = u16::try_from(index - window.start).expect("row");
    (panel.x + 2, panel.y + HEADER_ROWS + offset)
}

#[test]
fn a_click_on_a_list_row_selects_its_tile() {
    let (dir, mut app) = app_with_two_folders("list_click");
    let (column, row) = list_row_of(&app, "small");
    app.click(MouseButton::Left, column, row);

    assert_eq!(selected_name(&app), Some(OsString::from("small")));
    assert!(app.file_tree.current_folder_names.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_double_click_on_a_list_row_enters_the_folder() {
    let (dir, mut app) = app_with_two_folders("list_double_click");
    let (column, row) = list_row_of(&app, "small");
    let start = ::std::time::Instant::now();
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(150),
    );

    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_right_click_on_a_list_row_copies_its_path() {
    let (dir, mut app) = app_with_two_folders("list_right_click");
    let recorder = recording(&mut app, &dir);
    let (column, row) = list_row_of(&app, "loose.txt");
    app.click(MouseButton::Right, column, row);

    assert_eq!(recorder.copied(), vec!["loose.txt".to_string()]);
    assert_eq!(selected_name(&app), Some(OsString::from("loose.txt")));
    let _ = fs::remove_dir_all(&dir);
}

/// Zoomed in, the largest entry has no tile; its row is still there and can still be entered.
#[test]
fn a_folder_off_the_board_can_be_entered_from_the_list() {
    let (dir, mut app) = app_with_two_folders("list_off_board");
    app.zoom_in();
    assert!(
        !app.board
            .tiles
            .iter()
            .any(|tile| tile.name == OsStr::new("big")),
        "zooming in leaves the largest entry off the board"
    );
    let (column, row) = list_row_of(&app, "big");
    let start = ::std::time::Instant::now();
    app.click_at(MouseButton::Left, column, row, start);
    app.click_at(
        MouseButton::Left,
        column,
        row,
        start + ::std::time::Duration::from_millis(150),
    );

    assert_eq!(app.file_tree.get_current_path(), dir.join("big"));
    let _ = fs::remove_dir_all(&dir);
}

/// Enter on a file opens nothing, so it must leave nothing for Esc to undo: Esc then goes up from
/// the folder as it would have anyway, not back to a stale selection.
#[test]
fn enter_on_a_file_leaves_nothing_to_go_back_to() {
    let (dir, mut app) = app_with_two_folders("enter_file");
    app.switch_focus(); // to the treemap, whose selection Enter then acts on
    let file = app
        .board
        .tiles
        .iter()
        .position(|tile| tile.name == OsStr::new("loose.txt"))
        .expect("file tile");
    app.board.set_selected_index(&file);
    app.handle_enter();

    assert!(app.file_tree.current_folder_names.is_empty(), "not entered");
    assert!(
        app.board.previous_indices_and_zoom_level.is_empty(),
        "nothing recorded to go back to"
    );
    let _ = fs::remove_dir_all(&dir);
}

fn listed_name(app: &App<TestBackend>, index: usize) -> OsString {
    app.board.listing()[index].name.clone()
}

fn list_cursor_name(app: &App<TestBackend>) -> Option<OsString> {
    app.highlighted_listing_index()
        .map(|index| listed_name(app, index))
}

#[test]
fn a_click_puts_the_keyboard_on_the_panel_clicked() {
    use super::Focus;
    let (dir, mut app) = app_with_two_folders("focus_follows_click");
    assert_eq!(app.focus(), Focus::List, "the list starts with it");
    let (column, row) = centre_of(&app, "big");
    app.click(MouseButton::Left, column, row);
    assert_eq!(app.focus(), Focus::Treemap);
    let (column, row) = list_row_of(&app, "small");
    app.click(MouseButton::Left, column, row);
    assert_eq!(app.focus(), Focus::List);
    let _ = fs::remove_dir_all(&dir);
}

/// With the list in hand, Up and Down walk the list's order, largest first — not the treemap's
/// geometry — and the treemap's selection follows along.
#[test]
fn up_and_down_in_the_list_walk_its_order() {
    use super::Focus;
    let (dir, mut app) = app_with_two_folders("list_keys");
    assert_eq!(app.focus(), Focus::List);
    assert_eq!(
        list_cursor_name(&app),
        Some(listed_name(&app, 0)),
        "starts at the top"
    );

    for expected in [1, 2, 2] {
        app.move_selected_down();
        assert_eq!(list_cursor_name(&app), Some(listed_name(&app, expected)));
        assert_eq!(
            selected_name(&app),
            Some(listed_name(&app, expected)),
            "tile follows"
        );
    }
    app.move_selected_up();
    assert_eq!(list_cursor_name(&app), Some(listed_name(&app, 1)));
    let _ = fs::remove_dir_all(&dir);
}

/// An entry with no tile can be reached from the list, and the treemap then selects nothing
/// rather than leave some other entry looking selected.
#[test]
fn the_list_reaches_entries_without_a_tile() {
    let (dir, mut app) = app_with_two_folders("list_no_tile");
    app.zoom_in();
    app.jump_list(super::ListJump::Home);
    assert_eq!(list_cursor_name(&app), Some(OsString::from("big")));
    assert_eq!(selected_name(&app), None, "big has no tile while zoomed in");
    assert_eq!(
        app.selected_entry().map(|entry| entry.name),
        Some(OsString::from("big")),
        "but it is the entry in hand"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn page_and_end_keys_jump_through_the_list() {
    use super::ListJump;
    let dir = temp_app_dir("list_jumps");
    for index in 0..60 {
        File::create(dir.join(format!("file{index:02}")))
            .expect("create file")
            .write_all(&vec![b'x'; 1024 * (61 - index)])
            .expect("write file");
    }
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let last = app.board.listing().len() - 1;
    app.jump_list(ListJump::End);
    assert_eq!(app.highlighted_listing_index(), Some(last));
    app.jump_list(ListJump::Home);
    assert_eq!(app.highlighted_listing_index(), Some(0));
    app.jump_list(ListJump::PageDown);
    let page = app.highlighted_listing_index().expect("moved");
    assert!(
        page > 10 && page < last,
        "a page is most of the panel: {page}"
    );
    app.jump_list(ListJump::PageUp);
    assert_eq!(app.highlighted_listing_index(), Some(0));
    let _ = fs::remove_dir_all(&dir);
}

/// Left off the treemap's left edge moves into the list, on the entry that was selected; Right
/// from the list goes back, on the same entry.
#[test]
fn left_off_the_treemap_enters_the_list_and_right_comes_back() {
    use super::Focus;
    let (dir, mut app) = app_with_two_folders("edge_keys");
    app.switch_focus();
    assert_eq!(app.focus(), Focus::Treemap, "starts on the treemap");
    let left_edge = app
        .board
        .tiles
        .iter()
        .map(|tile| tile.x)
        .min()
        .expect("tiles");
    let (index, tile) = app
        .board
        .tiles
        .iter()
        .enumerate()
        .find(|(_, tile)| tile.x == left_edge)
        .map(|(index, tile)| (index, tile.name.clone()))
        .expect("a tile on the left edge");
    app.board.set_selected_index(&index);
    app.move_selected_left();

    assert_eq!(app.focus(), Focus::List);
    assert_eq!(
        list_cursor_name(&app),
        Some(tile.clone()),
        "same entry, now in the list"
    );
    app.move_selected_down();
    let below = list_cursor_name(&app).expect("moved");
    app.move_selected_right();
    assert_eq!(app.focus(), Focus::Treemap);
    assert_eq!(
        selected_name(&app),
        Some(below),
        "the treemap takes the list's entry"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Enter and Esc with the list in hand: into the highlighted folder, starting at the top of its
/// list, and back out onto the folder just left.
#[test]
fn enter_and_esc_from_the_list() {
    use super::Focus;
    let (dir, mut app) = app_with_two_folders("list_enter_esc");
    let (column, row) = list_row_of(&app, "small");
    app.click(MouseButton::Left, column, row);
    app.handle_enter();

    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    assert_eq!(app.focus(), Focus::List);
    assert_eq!(list_cursor_name(&app), Some(OsString::from("data")));
    app.go_up();
    assert_eq!(app.file_tree.get_current_path(), dir);
    assert_eq!(list_cursor_name(&app), Some(OsString::from("small")));
    let _ = fs::remove_dir_all(&dir);
}

/// `d` with the list in hand offers to delete the list's entry, even one with no tile.
#[test]
fn delete_from_the_list_offers_the_listed_entry() {
    let (dir, mut app) = app_with_two_folders("list_delete");
    app.zoom_in();
    app.jump_list(super::ListJump::Home);
    app.prompt_file_deletion();

    match &app.ui_mode {
        UiMode::DeleteFiles(files) => {
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path_to_file, vec![OsString::from("big")]);
        }
        _ => panic!("expected the delete prompt"),
    }
    assert!(
        dir.join("big").exists(),
        "nothing deleted before confirming"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A terminal too narrow for the panel keeps the keyboard on the treemap, whatever was last
/// focused: arrows must move something that can be seen.
#[test]
fn without_the_panel_the_keyboard_stays_on_the_treemap() {
    use super::Focus;
    let dir = temp_app_dir("narrow_focus");
    File::create(dir.join("a")).expect("create file");
    let app = app_with_scanned_dir(&dir, 79, 24);
    assert_eq!(
        app.focus(),
        Focus::Treemap,
        "the list's default does not apply without it"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The list has the keyboard from the start, its top entry highlighted, and the treemap shows
/// that entry selected.
#[test]
fn the_list_starts_with_the_keyboard_on_its_top_entry() {
    use super::Focus;
    let (dir, mut app) = app_with_two_folders("list_default");
    app.render();
    assert_eq!(app.focus(), Focus::List);
    assert_eq!(app.highlighted_listing_index(), Some(0));
    assert_eq!(selected_name(&app), Some(listed_name(&app, 0)));
    app.move_selected_down();
    assert_eq!(
        app.highlighted_listing_index(),
        Some(1),
        "Down goes to the second row"
    );
    let _ = fs::remove_dir_all(&dir);
}

fn marked(app: &App<TestBackend>) -> Vec<String> {
    app.marked
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// Shift+Down marks from where it started to the cursor and copies them; Shift+Up takes back
/// what it passes over.
#[test]
fn shift_arrows_mark_a_range_and_copy_it() {
    let (dir, mut app) = app_with_two_folders("shift_range");
    let recorder = recording(&mut app, &dir);
    let order: Vec<String> = (0..3)
        .map(|index| listed_name(&app, index).to_string_lossy().into_owned())
        .collect();
    let now = ::std::time::Instant::now();
    app.extend_selection_at(1, now);
    app.extend_selection_at(1, now);
    assert_eq!(marked(&app), order);
    app.extend_selection_at(-1, now);
    assert_eq!(marked(&app), order[..2].to_vec());

    let copied = recorder.copied();
    assert_eq!(copied.last(), Some(&order[..2].join(" ")));
    assert_eq!(copied[1], order.join(" "));
    assert_eq!(
        app.ui_effects.clipboard_flash_at(now),
        Some(format!("2 paths: {}", order[..2].join(" ")).as_str())
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Ctrl+click adds entries from either panel and takes them out again; each change is copied.
/// With nothing chosen yet, the list's default top entry is not swept in.
#[test]
fn ctrl_click_toggles_entries_in_either_panel() {
    let (dir, mut app) = app_with_two_folders("ctrl_click");
    let recorder = recording(&mut app, &dir);
    let now = ::std::time::Instant::now();
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click_at(column, row, now);
    assert_eq!(marked(&app), vec!["small"]);
    let (column, row) = centre_of(&app, "big");
    app.ctrl_click_at(column, row, now);
    assert_eq!(marked(&app), vec!["small", "big"]);
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click_at(column, row, now);
    assert_eq!(marked(&app), vec!["big"]);

    assert_eq!(
        recorder.copied(),
        vec![
            "small".to_string(),
            "small big".to_string(),
            "big".to_string()
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Starting a Ctrl+click selection takes in the entry already chosen, as a file manager does.
#[test]
fn ctrl_click_after_a_click_includes_the_first_entry() {
    let (dir, mut app) = app_with_two_folders("ctrl_after_click");
    let _recorder = recording(&mut app, &dir);
    let (column, row) = list_row_of(&app, "loose.txt");
    app.click(MouseButton::Left, column, row);
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click(column, row);
    assert_eq!(marked(&app), vec!["loose.txt", "small"]);
    let _ = fs::remove_dir_all(&dir);
}

/// A Shift range adds to what Ctrl+click marked, rather than replacing it.
#[test]
fn a_shift_range_keeps_earlier_marks() {
    let (dir, mut app) = app_with_two_folders("shift_after_ctrl");
    let _recorder = recording(&mut app, &dir);
    // Listed largest first: big, loose.txt, small.
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click(column, row);
    let (column, row) = list_row_of(&app, "big");
    app.ctrl_click(column, row);
    assert_eq!(marked(&app), vec!["small", "big"]);
    // From big, Shift+Down takes in loose.txt and keeps what was already marked.
    app.extend_selection(1);
    assert_eq!(marked(&app), vec!["small", "big", "loose.txt"]);
    // Back up again: the range shrinks to big, and small, marked before it, stays.
    app.extend_selection(-1);
    assert_eq!(marked(&app), vec!["small", "big"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn plain_moves_clicks_and_folder_changes_clear_the_marks() {
    let (dir, mut app) = app_with_two_folders("clear_marks");
    let _recorder = recording(&mut app, &dir);
    app.extend_selection(1);
    assert_eq!(app.marked.len(), 2);
    app.move_selected_down();
    assert!(app.marked.is_empty(), "a plain move clears");

    app.extend_selection(1);
    app.jump_list(super::ListJump::Home);
    assert!(app.marked.is_empty(), "a jump is a plain move, and clears");

    app.extend_selection(1);
    let (column, row) = list_row_of(&app, "small");
    app.click(MouseButton::Left, column, row);
    assert!(app.marked.is_empty(), "a plain click clears");

    // `small` is the last entry, so this marks it without moving off it, and Enter opens it.
    app.extend_selection(1);
    assert_eq!(marked(&app), vec!["small"]);
    app.handle_enter();
    assert_eq!(app.file_tree.get_current_path(), dir.join("small"));
    assert!(app.marked.is_empty(), "entering a folder clears");
    let _ = fs::remove_dir_all(&dir);
}

/// With the treemap in hand there is no order for a range to follow: Shift+arrows do nothing.
#[test]
fn shift_arrows_do_nothing_on_the_treemap() {
    let (dir, mut app) = app_with_two_folders("shift_treemap");
    let recorder = recording(&mut app, &dir);
    app.switch_focus();
    app.extend_selection(1);
    assert!(app.marked.is_empty());
    assert!(recorder.copied().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn every_marked_path_is_quoted_on_its_own() {
    let dir = temp_app_dir("marked_quoting");
    for (name, size) in [("my file", 8192), ("it's", 4096)] {
        File::create(dir.join(name))
            .expect("create file")
            .write_all(&vec![b'x'; size])
            .expect("write file");
    }
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let recorder = recording(&mut app, &dir);
    app.extend_selection(1);
    assert_eq!(recorder.copied(), vec![r"'my file' 'it'\''s'".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

/// The real events: Ctrl+click and Shift+Down through the normal-mode handler.
#[test]
fn ctrl_click_and_shift_arrow_events_reach_the_app() {
    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
    };
    let (dir, mut app) = app_with_two_folders("modifier_events");
    let _recorder = recording(&mut app, &dir);
    let (column, row) = list_row_of(&app, "loose.txt");
    crate::input::handle_keypress_normal_mode(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::CONTROL,
        }),
        &mut app,
    );
    assert_eq!(marked(&app), vec!["loose.txt"]);
    crate::input::handle_keypress_normal_mode(
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)),
        &mut app,
    );
    assert_eq!(marked(&app), vec!["loose.txt", "small"]);
    let _ = fs::remove_dir_all(&dir);
}

fn press(app: &mut App<TestBackend>, c: char) {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let evt = Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    match app.ui_mode.clone() {
        UiMode::DeleteFiles(files) => {
            crate::input::handle_keypress_delete_file_mode(evt, app, files);
        }
        _ => crate::input::handle_keypress_normal_mode(evt, app),
    }
}

/// With entries marked, `d` offers to delete all of them, in the order marked, and `y` does.
#[test]
fn d_deletes_every_marked_entry() {
    let (dir, mut app) = app_with_two_folders("delete_marked");
    let _recorder = recording(&mut app, &dir);
    let before = app.file_tree.get_total_size();
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click(column, row);
    let (column, row) = list_row_of(&app, "loose.txt");
    app.ctrl_click(column, row);
    press(&mut app, 'd');

    let UiMode::DeleteFiles(files) = &app.ui_mode else {
        panic!("expected the delete prompt");
    };
    let names: Vec<_> = files.iter().map(|file| file.path_to_file.clone()).collect();
    assert_eq!(
        names,
        vec![
            vec![OsString::from("small")],
            vec![OsString::from("loose.txt")]
        ]
    );
    let freed: u128 = files.iter().map(|file| file.size).sum();
    assert!(
        dir.join("small").exists() && dir.join("loose.txt").exists(),
        "not yet"
    );

    press(&mut app, 'y');
    assert!(matches!(app.ui_mode, UiMode::Normal));
    assert!(!dir.join("small").exists());
    assert!(!dir.join("loose.txt").exists());
    assert!(dir.join("big").exists(), "the unmarked entry stays");
    assert_eq!(app.file_tree.space_freed, freed);
    assert_eq!(app.file_tree.get_total_size(), before - freed);
    assert!(app.marked.is_empty());
    assert_eq!(app.board.listing().len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_a_marked_deletion_deletes_nothing() {
    let (dir, mut app) = app_with_two_folders("delete_marked_cancel");
    let _recorder = recording(&mut app, &dir);
    app.extend_selection(1);
    press(&mut app, 'd');
    press(&mut app, 'n');
    assert!(matches!(app.ui_mode, UiMode::Normal));
    for name in ["big", "loose.txt", "small"] {
        assert!(dir.join(name).exists(), "{name}");
    }
    let _ = fs::remove_dir_all(&dir);
}

/// One entry failing does not stop the rest, and the error says what happened; only what was
/// really removed counts as freed.
#[test]
fn a_failure_part_way_deletes_the_rest_and_says_so() {
    let (dir, mut app) = app_with_two_folders("delete_marked_failure");
    let _recorder = recording(&mut app, &dir);
    let (column, row) = list_row_of(&app, "loose.txt");
    app.ctrl_click(column, row);
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click(column, row);
    press(&mut app, 'd');
    let UiMode::DeleteFiles(files) = app.ui_mode.clone() else {
        panic!("expected the delete prompt");
    };
    let small = files[1].size;
    // Something else removes one of them while the prompt is up.
    fs::remove_file(dir.join("loose.txt")).expect("remove behind the app's back");
    press(&mut app, 'y');

    let UiMode::ErrorMessage(message) = &app.ui_mode else {
        panic!("expected an error, got a different mode");
    };
    assert!(
        message.starts_with("Deleted 1 of 2; loose.txt:"),
        "{message}"
    );
    assert!(!dir.join("small").exists(), "the other was still deleted");
    assert_eq!(app.file_tree.space_freed, small);
    let _ = fs::remove_dir_all(&dir);
}

/// A name that is not UTF-8 is drawn lossily in the prompt rather than crashing the app.
#[cfg(unix)]
#[test]
fn a_non_utf8_name_can_be_offered_for_deletion() {
    use ::std::os::unix::ffi::OsStrExt;
    let dir = temp_app_dir("delete_non_utf8");
    let name = OsStr::from_bytes(b"bad\xffname");
    if File::create(dir.join(name)).is_err() {
        // APFS and some other filesystems refuse names that are not UTF-8.
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    app.prompt_file_deletion();
    assert!(matches!(app.ui_mode, UiMode::DeleteFiles(_)));
    let _ = fs::remove_dir_all(&dir);
}

/// Where the app put the cursor itself — the top of a folder just entered, the neighbour of what
/// was just deleted — nothing was chosen, and a Ctrl+click selection must not sweep it in.
#[test]
fn ctrl_click_never_sweeps_in_an_entry_nobody_chose() {
    let (dir, mut app) = app_with_two_folders("ctrl_unchosen");
    let _recorder = recording(&mut app, &dir);
    // Delete one entry; the cursor lands on its neighbour without anyone choosing it.
    let (column, row) = list_row_of(&app, "loose.txt");
    app.click(MouseButton::Left, column, row);
    press(&mut app, 'd');
    press(&mut app, 'y');
    assert!(!dir.join("loose.txt").exists());
    let (column, row) = list_row_of(&app, "big");
    app.ctrl_click(column, row);
    assert_eq!(marked(&app), vec!["big"], "the neighbour was not swept in");

    // Toggling it back off leaves nothing chosen either.
    app.ctrl_click(column, row);
    assert!(app.marked.is_empty());
    let (column, row) = list_row_of(&app, "small");
    app.ctrl_click(column, row);
    assert_eq!(marked(&app), vec!["small"]);
    let _ = fs::remove_dir_all(&dir);
}

/// Remembers every placement the app asks for.
#[derive(Clone, Default)]
struct PictureRecorder(
    ::std::sync::Arc<::std::sync::Mutex<Vec<Option<crate::preview::Placement>>>>,
);

impl crate::preview::Graphics for PictureRecorder {
    fn show(&mut self, placement: Option<crate::preview::Placement>) {
        self.0.lock().expect("recorder").push(placement);
    }
}

impl PictureRecorder {
    fn last(&self) -> Option<Option<crate::preview::Placement>> {
        self.0.lock().expect("recorder").last().cloned()
    }
}

type Answers = ::std::sync::mpsc::Receiver<(u64, crate::preview::Preview)>;

/// Previews on, with answers collected for the test to hand back, as the event loop would.
fn previewing(app: &mut App<TestBackend>, graphics: bool) -> (Answers, PictureRecorder) {
    let (sender, answers) = mpsc::channel();
    let sender = ::std::sync::Mutex::new(sender);
    let previewer = crate::preview::Previewer::spawn(move |generation, preview| {
        let _ = sender.lock().expect("sender").send((generation, preview));
    });
    let recorder = PictureRecorder::default();
    app.enable_previews(previewer, graphics, Box::new(recorder.clone()));
    (answers, recorder)
}

fn deliver(app: &mut App<TestBackend>, answers: &Answers) {
    let (generation, preview) = answers
        .recv_timeout(::std::time::Duration::from_secs(10))
        .expect("a preview");
    app.preview_ready(generation, preview);
}

fn preview_fixture(name: &str) -> PathBuf {
    let dir = temp_app_dir(name);
    fs::create_dir(dir.join("folder")).expect("create folder");
    File::create(dir.join("folder").join("inside"))
        .expect("create file")
        .write_all(&vec![b'x'; 64 * 1024])
        .expect("write file");
    fs::write(
        dir.join("readme.txt"),
        "first line\nsecond line\n".repeat(900),
    )
    .expect("write");
    image::RgbImage::from_pixel(800, 450, image::Rgb([200, 30, 30]))
        .save(dir.join("photo.png"))
        .expect("write picture");
    dir
}

/// A text file in hand shows its first lines below the list, under a caption naming it.
#[test]
fn a_text_file_is_previewed_below_the_list() {
    let dir = preview_fixture("preview_text");
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let (answers, _recorder) = previewing(&mut app, false);
    let (column, row) = list_row_of(&app, "readme.txt");
    app.click(MouseButton::Left, column, row);
    deliver(&mut app, &answers);

    let preview = app.display.areas().preview.expect("room for a preview");
    let screen = app.display.screen_text();
    let caption = &screen[usize::from(preview.y)];
    assert!(caption.starts_with("readme.txt · "), "{caption:?}");
    assert!(screen[usize::from(preview.y) + 1].starts_with("first line"));
    assert!(screen[usize::from(preview.y) + 2].starts_with("second line"));
    let _ = fs::remove_dir_all(&dir);
}

/// A picture is placed in the preview area once read, and taken away when the selection moves
/// to something that is not one.
#[test]
fn a_picture_is_placed_and_taken_away() {
    let dir = preview_fixture("preview_picture");
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let (answers, recorder) = previewing(&mut app, true);
    let (column, row) = list_row_of(&app, "photo.png");
    app.click(MouseButton::Left, column, row);
    deliver(&mut app, &answers);

    let placement = recorder.last().flatten().expect("a picture placed");
    let area = crate::ui::side_panel::picture_area(app.display.areas().preview.expect("preview"));
    assert!(area.contains(ratatui::layout::Position::new(
        placement.column,
        placement.row
    )));
    assert!(placement.column + placement.image.columns <= area.x + area.width);
    assert!(placement.row + placement.image.rows <= area.y + area.height);
    let screen = app.display.screen_text();
    assert!(
        screen[usize::from(area.y) - 1].contains("PNG 800×450"),
        "caption: {:?}",
        screen[usize::from(area.y) - 1]
    );

    let (column, row) = list_row_of(&app, "folder");
    app.click(MouseButton::Left, column, row);
    assert_eq!(recorder.last(), Some(None), "a folder has no picture");
    let _ = fs::remove_dir_all(&dir);
}

/// Folders and multi-selections are not previewed, and nothing is read for them.
#[test]
fn folders_and_selections_are_not_previewed() {
    use crate::preview::Preview;
    let dir = preview_fixture("preview_none");
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let (answers, _recorder) = previewing(&mut app, false);
    let (column, row) = list_row_of(&app, "folder");
    app.click(MouseButton::Left, column, row);
    assert_eq!(app.preview, Preview::None);

    app.extend_selection(1);
    assert!(app.marked.len() > 1);
    assert_eq!(app.preview, Preview::None);
    let screen = app.display.screen_text();
    let preview = app.display.areas().preview.expect("preview");
    assert!(
        screen[usize::from(preview.y)].contains("marked"),
        "caption counts them"
    );
    assert!(
        answers
            .recv_timeout(::std::time::Duration::from_millis(300))
            .is_err(),
        "nothing was read"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// An answer to a request the selection has moved past is dropped.
#[test]
fn a_stale_preview_is_dropped() {
    use crate::preview::Preview;
    let dir = preview_fixture("preview_stale");
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let (_answers, _recorder) = previewing(&mut app, false);
    let (column, row) = list_row_of(&app, "readme.txt");
    app.click(MouseButton::Left, column, row);
    let stale = app.preview_generation;
    let (column, row) = list_row_of(&app, "photo.png");
    app.click(MouseButton::Left, column, row);
    app.preview_ready(stale, Preview::Text(vec!["old".into()]));
    assert_eq!(app.preview, Preview::Loading, "still waiting for the photo");
    let _ = fs::remove_dir_all(&dir);
}

/// Showing the first file asks for its preview once: the request is sized for the cells the
/// frame is drawn with, so drawing that frame does not make it out of date.
#[test]
fn the_first_preview_is_asked_for_once() {
    let dir = preview_fixture("preview_once");
    let mut app = app_with_scanned_dir(&dir, 120, 30);
    let (answers, _recorder) = previewing(&mut app, false);
    let (column, row) = list_row_of(&app, "readme.txt");
    app.click(MouseButton::Left, column, row);
    let asked = app.preview_generation;
    app.render();
    app.render();
    assert_eq!(app.preview_generation, asked, "no second request");
    deliver(&mut app, &answers);
    assert!(matches!(app.preview, crate::preview::Preview::Text(_)));
    let _ = fs::remove_dir_all(&dir);
}
