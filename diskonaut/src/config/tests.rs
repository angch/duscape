use super::*;

#[test]
fn default_config_has_expected_keybinds() {
    let kb = DiskonautConfig::default().keybinds().unwrap();
    assert_eq!(kb.delete, KeyBinding::char('d'));
    assert_eq!(kb.quit, KeyBinding::char('q'));
}

#[test]
fn parses_example_config() {
    let toml = r#"
version = 1

[base]
apparent-size = false

[keybinds]
delete = "x"
move-left = "left"
"#;
    let cfg: DiskonautConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.version, CONFIG_VERSION);
    assert!(!cfg.base.apparent_size);
    let kb = cfg.keybinds().unwrap();
    assert_eq!(kb.delete, KeyBinding::char('x'));
    assert_eq!(
        kb.move_left,
        KeyBinding::key(ratatui::crossterm::event::KeyCode::Left)
    );
    assert_eq!(kb.move_right, KeyBinding::char('l'));
}

#[test]
fn rejects_unsupported_version() {
    let path = std::env::temp_dir().join("diskonaut_config_test_v2.toml");
    std::fs::write(&path, "version = 2\n").unwrap();
    let err = DiskonautConfig::load(Some(&path)).unwrap_err();
    assert!(matches!(err, ConfigError::UnsupportedVersion(2)));
    let _ = std::fs::remove_file(path);
}

#[test]
fn invalid_keybind_returns_error() {
    let cfg: DiskonautConfig = toml::from_str(
        r#"
version = 1

[keybinds]
delete = "not-a-real-key-name-here"
"#,
    )
    .unwrap();
    let err = cfg.keybinds().unwrap_err();
    assert!(matches!(err, ConfigError::InvalidKeybind { .. }));
}

#[test]
fn rescan_keys_default_to_r_and_shift_r() {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let kb = DiskonautConfig::default().keybinds().unwrap();
    // Terminals report an upper-case letter with Shift held; the binding is the letter alone.
    let shifted_r = Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    let plain_r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(kb.rescan_all.matches_event(&shifted_r));
    assert!(!kb.rescan.matches_event(&shifted_r));
    assert!(kb.rescan.matches_event(&plain_r));
    assert!(!kb.rescan_all.matches_event(&plain_r));
}
