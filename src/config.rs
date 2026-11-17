//! User configuration: favorites, run history and theme settings.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while reading or writing the configuration file.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine a configuration directory for this user")]
    NoConfigDirectory,
    #[error("could not read configuration at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse configuration at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not write configuration at {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

const CONFIG_FILE: &str = "config.toml";
const MAX_RECENT_RUNS: usize = 100;

/// A command marked as a favorite, with optional pre-filled flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub id: String,
    #[serde(default)]
    pub preset_flags: Vec<String>,
}

/// A previously executed command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentRun {
    pub command: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// Persisted appearance settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Name of the accent color: cyan, green, yellow, blue, magenta or red.
    pub accent: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "cyan".to_owned(),
        }
    }
}

/// The complete on-disk configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default)]
    pub favorites: Vec<Favorite>,
    #[serde(default)]
    pub recent_runs: Vec<RecentRun>,
    #[serde(default)]
    pub theme: Theme,
}

impl Config {
    /// Load the configuration from the user config directory, or use defaults when absent.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Self::path()?)
    }

    /// Load configuration, falling back to defaults with a human-readable reason.
    ///
    /// A malformed or unreadable file must never prevent the TUI from starting. The original is
    /// left on disk so the user can inspect it; the next save replaces it atomically.
    pub fn load_or_recover() -> (Self, Option<String>) {
        match Self::load() {
            Ok(config) => (config, None),
            Err(error) => (Self::default(), Some(error.to_string())),
        }
    }

    /// Load configuration from an explicit path. A missing file is not an error.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Persist the configuration to the user config directory.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(Self::path()?)
    }

    /// Persist the configuration to an explicit path.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        let contents = toml::to_string_pretty(self).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source: std::io::Error::other(source.to_string()),
        })?;
        crate::fsutil::write_private(path, contents.as_bytes()).map_err(|source| {
            ConfigError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    /// Path of the config file, honouring `SFTUI_CONFIG_DIR` for portable/test setups.
    pub fn path() -> Result<PathBuf, ConfigError> {
        if let Some(dir) = std::env::var_os("SFTUI_CONFIG_DIR")
            && !dir.is_empty()
        {
            return Ok(PathBuf::from(dir).join(CONFIG_FILE));
        }
        ProjectDirs::from("", "tunitz", "sf-tui")
            .map(|dirs| dirs.config_dir().join(CONFIG_FILE))
            .ok_or(ConfigError::NoConfigDirectory)
    }

    /// Add or remove a favorite by command id. Returns whether it is now a favorite.
    pub fn toggle_favorite(&mut self, id: &str) -> bool {
        if let Some(index) = self.favorites.iter().position(|favorite| favorite.id == id) {
            self.favorites.remove(index);
            false
        } else {
            self.favorites.push(Favorite {
                id: id.to_owned(),
                preset_flags: Vec::new(),
            });
            true
        }
    }

    pub fn is_favorite(&self, id: &str) -> bool {
        self.favorites.iter().any(|favorite| favorite.id == id)
    }

    pub fn set_preset_flags(&mut self, id: &str, flags: Vec<String>) {
        if let Some(favorite) = self.favorites.iter_mut().find(|favorite| favorite.id == id) {
            favorite.preset_flags = flags;
        }
    }

    /// Move a favorite one position towards the start (-1) or end (+1).
    pub fn move_favorite(&mut self, id: &str, delta: isize) {
        let Some(from) = self.favorites.iter().position(|favorite| favorite.id == id) else {
            return;
        };
        let target = from as isize + delta;
        if target < 0 || target >= self.favorites.len() as isize {
            return;
        }
        let item = self.favorites.remove(from);
        self.favorites.insert(target as usize, item);
    }

    pub fn remove_favorite(&mut self, id: &str) {
        self.favorites.retain(|favorite| favorite.id != id);
    }

    /// Record an executed command, keeping the most recent entries first.
    pub fn record_run(&mut self, command: impl Into<String>, exit_code: Option<i32>) {
        let command = command.into();
        self.recent_runs.retain(|run| run.command != command);
        self.recent_runs.insert(0, RecentRun { command, exit_code });
        self.recent_runs.truncate(MAX_RECENT_RUNS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = Config::load_from(dir.path().join("missing.toml")).expect("load");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn malformed_config_reports_the_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "favorites = [oops").expect("write");
        let error = Config::load_from(&path).expect_err("parse must fail");
        assert!(error.to_string().contains("could not parse configuration"));
        assert!(error.to_string().contains("bad.toml"));
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("config.toml");
        Config::default().save_to(&path).expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn favorites_round_trip_through_toml() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("config.toml");
        let mut config = Config::default();
        assert!(config.toggle_favorite("org list"));
        config.set_preset_flags("org list", vec!["--json".to_owned()]);
        config.save_to(&path).expect("save");
        let loaded = Config::load_from(&path).expect("load");
        assert_eq!(loaded.favorites.len(), 1);
        assert_eq!(loaded.favorites[0].id, "org list");
        assert_eq!(loaded.favorites[0].preset_flags, vec!["--json"]);
    }

    #[test]
    fn toggling_and_reordering_favorites_is_stable() {
        let mut config = Config::default();
        config.toggle_favorite("a");
        config.toggle_favorite("b");
        config.toggle_favorite("c");
        config.move_favorite("c", -1);
        assert_eq!(
            config
                .favorites
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
        config.move_favorite("a", -1);
        assert_eq!(config.favorites[0].id, "a");
        assert!(!config.toggle_favorite("c"));
        assert_eq!(config.favorites.len(), 2);
    }

    #[test]
    fn recent_runs_are_unique_bounded_and_newest_first() {
        let mut config = Config::default();
        for index in 0..120 {
            config.record_run(format!("sf org list --{index}"), Some(0));
        }
        config.record_run("sf org list --0", Some(1));
        assert!(config.recent_runs.len() <= MAX_RECENT_RUNS);
        assert_eq!(config.recent_runs[0].command, "sf org list --0");
        assert_eq!(config.recent_runs[0].exit_code, Some(1));
    }
}
