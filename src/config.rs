//! User configuration: theme settings.

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
    fn config_with_removed_favorites_still_loads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("old.toml");
        std::fs::write(
            &path,
            "[[favorites]]\nid = \"org list\"\npreset_flags = [\"--json\"]\n\n[theme]\naccent = \"green\"\n",
        )
        .expect("write");
        let config = Config::load_from(&path).expect("old config must still load");
        assert_eq!(config.theme.accent, "green");
        assert_eq!(
            config,
            Config {
                theme: Theme {
                    accent: "green".to_owned()
                }
            }
        );
    }
}
