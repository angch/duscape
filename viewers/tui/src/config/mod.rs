mod keybind;

use ::std::fs;
use ::std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub use keybind::{KeyBinding, Keybinds};

/// Supported config file format version (top-level `version = 1`).
pub const CONFIG_VERSION: u32 = 1;

/// User configuration loaded from TOML.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DuscapeConfig {
    pub version: u32,
    pub base: BaseConfig,
    pub keybinds: KeybindConfig,
}

/// `[base]` section — defaults used when CLI flags are not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BaseConfig {
    #[serde(rename = "apparent-size")]
    pub apparent_size: bool,
}

/// `[keybinds]` section — optional per-action overrides (see [`Keybinds::default`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct KeybindConfig {
    pub quit: Option<String>,
    pub delete: Option<String>,
    #[serde(rename = "move-left")]
    pub move_left: Option<String>,
    #[serde(rename = "move-right")]
    pub move_right: Option<String>,
    #[serde(rename = "move-up")]
    pub move_up: Option<String>,
    #[serde(rename = "move-down")]
    pub move_down: Option<String>,
    pub enter: Option<String>,
    pub parent: Option<String>,
    #[serde(rename = "zoom-in")]
    pub zoom_in: Option<String>,
    #[serde(rename = "zoom-out")]
    pub zoom_out: Option<String>,
    #[serde(rename = "reset-zoom")]
    pub reset_zoom: Option<String>,
    pub confirm: Option<String>,
    pub cancel: Option<String>,
    #[serde(rename = "switch-panel")]
    pub switch_panel: Option<String>,
    pub rescan: Option<String>,
    #[serde(rename = "rescan-all")]
    pub rescan_all: Option<String>,
    #[serde(rename = "toggle-size")]
    pub toggle_size: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unsupported config version {0} (expected {CONFIG_VERSION})")]
    UnsupportedVersion(u32),

    #[error("invalid keybind '{name}': {reason}")]
    InvalidKeybind { name: &'static str, reason: String },

    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse config file: {0}")]
    Parse(#[from] toml::de::Error),
}

impl Default for DuscapeConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            base: BaseConfig::default(),
            keybinds: KeybindConfig::default(),
        }
    }
}

impl DuscapeConfig {
    /// Loads config from `-c` / `--config`, or `~/.config/duscape/config.toml` when unset.
    ///
    /// A missing default file yields [`DuscapeConfig::default`]. An explicit path must exist.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match default_config_path() {
                Some(p) if p.is_file() => p,
                _ => return Ok(Self::default()),
            },
        };
        Self::read_file(&path)
    }

    fn read_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)?;
        let config: DuscapeConfig = toml::from_str(&contents)?;
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion(config.version));
        }
        Ok(config)
    }

    pub fn keybinds(&self) -> Result<Keybinds, ConfigError> {
        self.keybinds.resolve()
    }
}

impl KeybindConfig {
    pub fn resolve(&self) -> Result<Keybinds, ConfigError> {
        let defaults = Keybinds::default();
        Ok(Keybinds {
            quit: parse_keybind(self.quit.as_deref(), "quit", defaults.quit)?,
            delete: parse_keybind(self.delete.as_deref(), "delete", defaults.delete)?,
            move_left: parse_keybind(self.move_left.as_deref(), "move-left", defaults.move_left)?,
            move_right: parse_keybind(
                self.move_right.as_deref(),
                "move-right",
                defaults.move_right,
            )?,
            move_up: parse_keybind(self.move_up.as_deref(), "move-up", defaults.move_up)?,
            move_down: parse_keybind(self.move_down.as_deref(), "move-down", defaults.move_down)?,
            enter: parse_keybind(self.enter.as_deref(), "enter", defaults.enter)?,
            parent: parse_keybind(self.parent.as_deref(), "parent", defaults.parent)?,
            zoom_in: parse_keybind(self.zoom_in.as_deref(), "zoom-in", defaults.zoom_in)?,
            zoom_out: parse_keybind(self.zoom_out.as_deref(), "zoom-out", defaults.zoom_out)?,
            reset_zoom: parse_keybind(
                self.reset_zoom.as_deref(),
                "reset-zoom",
                defaults.reset_zoom,
            )?,
            confirm: parse_keybind(self.confirm.as_deref(), "confirm", defaults.confirm)?,
            cancel: parse_keybind(self.cancel.as_deref(), "cancel", defaults.cancel)?,
            switch_panel: parse_keybind(
                self.switch_panel.as_deref(),
                "switch-panel",
                defaults.switch_panel,
            )?,
            rescan: parse_keybind(self.rescan.as_deref(), "rescan", defaults.rescan)?,
            rescan_all: parse_keybind(
                self.rescan_all.as_deref(),
                "rescan-all",
                defaults.rescan_all,
            )?,
            toggle_size: parse_keybind(
                self.toggle_size.as_deref(),
                "toggle-size",
                defaults.toggle_size,
            )?,
        })
    }
}

fn parse_keybind(
    value: Option<&str>,
    name: &'static str,
    default: KeyBinding,
) -> Result<KeyBinding, ConfigError> {
    match value {
        Some(s) => {
            KeyBinding::parse(s).map_err(|reason| ConfigError::InvalidKeybind { name, reason })
        }
        None => Ok(default),
    }
}

/// `~/.config/duscape/config.toml` (requires `HOME`); see [`config_path_in`].
pub fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| config_path_in(Path::new(&home)))
}

/// The config file under `home`: `.config/duscape/config.toml`, or where it was before the
/// rename, `.config/diskonaut/config.toml`, while only that one exists — so a config made then
/// goes on working until it is moved.
pub fn config_path_in(home: &Path) -> PathBuf {
    let config = home.join(".config");
    let path = config.join("duscape").join("config.toml");
    let before = config.join("diskonaut").join("config.toml");
    if !path.is_file() && before.is_file() {
        before
    } else {
        path
    }
}

#[cfg(test)]
mod tests;
