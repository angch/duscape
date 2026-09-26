use super::*;

#[test]
fn default_config_has_expected_keybinds() {
    let kb = DuscapeConfig::default().keybinds().unwrap();
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
    let cfg: DuscapeConfig = toml::from_str(toml).unwrap();
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
    let path = std::env::temp_dir().join("duscape_config_test_v2.toml");
    std::fs::write(&path, "version = 2\n").unwrap();
    let err = DuscapeConfig::load(Some(&path)).unwrap_err();
    assert!(matches!(err, ConfigError::UnsupportedVersion(2)));
    let _ = std::fs::remove_file(path);
}

#[test]
fn invalid_keybind_returns_error() {
    let cfg: DuscapeConfig = toml::from_str(
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
    let kb = DuscapeConfig::default().keybinds().unwrap();
    // Terminals report an upper-case letter with Shift held; the binding is the letter alone.
    let shifted_r = Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    let plain_r = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(kb.rescan_all.matches_event(&shifted_r));
    assert!(!kb.rescan.matches_event(&shifted_r));
    assert!(kb.rescan.matches_event(&plain_r));
    assert!(!kb.rescan_all.matches_event(&plain_r));
}

#[test]
fn a_config_from_before_the_rename_is_read_until_there_is_a_new_one() {
    let home = std::env::temp_dir().join(format!("duscape_config_home_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let new = home.join(".config").join("duscape").join("config.toml");
    let old = home.join(".config").join("diskonaut").join("config.toml");
    // Neither: the new place, for a missing file to be defaults.
    assert_eq!(config_path_in(&home), new);
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "version = 1\n").unwrap();
    assert_eq!(config_path_in(&home), old);
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::write(&new, "version = 1\n").unwrap();
    assert_eq!(config_path_in(&home), new);
    std::fs::remove_dir_all(&home).unwrap();
}
