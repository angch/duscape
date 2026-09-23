use ::std::path::PathBuf;
use ::std::sync::mpsc;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::controls::{
    handle_keypress_delete_file_mode, handle_keypress_exiting_mode, handle_keypress_loading_mode,
    handle_keypress_normal_mode, handle_keypress_screen_too_small, is_key_release, is_mouse_noise,
};
use crate::app::{App, UiMode};
use crate::config::Keybinds;

fn key_char(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn test_app(width: u16, height: u16) -> App<TestBackend> {
    let dir = std::env::temp_dir().join("diskonaut_input_test");
    let _ = std::fs::create_dir_all(&dir);
    let (tx, _rx) = mpsc::sync_channel(1);
    App::new(
        TestBackend::new(width, height),
        dir,
        tx,
        Keybinds::default(),
        false,
    )
}

#[test]
fn loading_mode_q_prompts_exit() {
    let mut app = test_app(80, 24);
    app.ui_mode = UiMode::Loading;
    handle_keypress_loading_mode(key_char('q'), &mut app);
    assert!(matches!(app.ui_mode, UiMode::Exiting { .. }));
}

#[test]
fn normal_mode_d_opens_delete_flow() {
    let mut app = test_app(80, 24);
    app.ui_mode = UiMode::Normal;
    app.loaded = true;
    handle_keypress_normal_mode(key_char('d'), &mut app);
    // Without a selected tile, mode stays normal.
    assert!(matches!(app.ui_mode, UiMode::Normal));
}

#[test]
fn screen_too_small_ctrl_c_exits() {
    let mut app = test_app(80, 24);
    app.ui_mode = UiMode::ScreenTooSmall;
    handle_keypress_screen_too_small(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &mut app,
    );
    assert!(!app.is_running);
}

#[test]
fn exiting_mode_y_quits() {
    let mut app = test_app(80, 24);
    app.ui_mode = UiMode::Exiting { app_loaded: true };
    handle_keypress_exiting_mode(key_char('y'), &mut app);
    assert!(!app.is_running);
}

#[test]
fn delete_mode_n_returns_to_normal() {
    let mut app = test_app(80, 24);
    let file = libdiskonaut::FileToDelete {
        path_in_filesystem: PathBuf::from("/tmp"),
        path_to_file: vec!["file".into()],
        file_type: libdiskonaut::tiles::FileType::File,
        num_descendants: None,
        size: 1,
        sizes: libdiskonaut::model::Sizes::new(1, 1),
    };
    app.ui_mode = UiMode::DeleteFiles(vec![file.clone()]);
    handle_keypress_delete_file_mode(key_char('n'), &mut app, vec![file]);
    assert!(matches!(app.ui_mode, UiMode::Normal));
}

/// Windows reports letting go of a key as an event of its own; only presses may reach a handler.
#[test]
fn key_releases_are_not_keypresses() {
    let release = Event::Key(KeyEvent::new_with_kind(
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    ));
    assert!(is_key_release(&release));
    assert!(!is_key_release(&key_char('q')));
    let repeat = Event::Key(KeyEvent::new_with_kind(
        KeyCode::Char('j'),
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ));
    assert!(!is_key_release(&repeat), "holding a key down still moves");
}

/// Only mouse presses reach the handlers. Capture reports every movement, and the warning modal
/// closes on any event, so a stray movement must not get that far.
#[test]
fn mouse_movement_is_not_passed_on() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mouse = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(!is_mouse_noise(&mouse(MouseEventKind::Down(
        MouseButton::Left
    ))));
    assert!(!is_mouse_noise(&mouse(MouseEventKind::Down(
        MouseButton::Right
    ))));
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
    ] {
        assert!(is_mouse_noise(&mouse(kind)), "{kind:?}");
    }
    assert!(!is_mouse_noise(&key_char('q')), "keys are not mouse noise");
}
