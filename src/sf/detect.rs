//! Locate the Salesforce CLI (`sf`) on PATH and at common per-user install locations.
//!
//! The deprecated `sfdx` CLI is deliberately not detected or translated. `sf` is the supported
//! command surface, and silently rewriting legacy syntax makes failures harder to understand.

use std::path::{Path, PathBuf};

/// A located Salesforce CLI executable and how it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfInstallation {
    pub executable: PathBuf,
    pub source: String,
}

impl SfInstallation {
    /// Build the argument vector for this installation.
    pub fn argv(&self, parts: &[String]) -> Vec<String> {
        parts.to_vec()
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.executable.display(), self.source)
    }
}

/// Detect `sf` using PATH first, then well-known user-writable install locations.
pub fn detect() -> Option<SfInstallation> {
    if let Some(path) = std::env::var_os("SFTUI_SF_PATH") {
        let path = PathBuf::from(path);
        if is_usable_executable(&path) {
            return Some(SfInstallation {
                executable: path,
                source: "SFTUI_SF_PATH".to_owned(),
            });
        }
    }

    if let Ok(path) = which::which("sf")
        && is_usable_executable(&path)
    {
        return Some(SfInstallation {
            executable: path,
            source: "PATH".to_owned(),
        });
    }

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    for path in user_candidates(&home) {
        if is_usable_executable(&path) {
            return Some(SfInstallation {
                executable: path,
                source: "install directory".to_owned(),
            });
        }
    }
    None
}

/// Per-user locations that do not require administrator rights.
pub fn user_candidates(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".local").join("share").join("sf").join("bin")];
    if cfg!(windows) {
        for key in ["LOCALAPPDATA", "PROGRAMFILES", "APPDATA"] {
            if let Some(root) = std::env::var_os(key) {
                let root = PathBuf::from(root);
                roots.push(root.join("Salesforce CLI").join("bin"));
                roots.push(root.join("npm"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs").join("sf").join("bin"));
        }
    } else {
        roots.push(home.join(".local").join("bin"));
        roots.push(home.join(".npm-global").join("bin"));
        roots.push(home.join(".bun").join("bin"));
        roots.push(PathBuf::from("/usr/local/bin"));
        if cfg!(target_os = "macos") {
            roots.push(PathBuf::from("/opt/homebrew/bin"));
        }
    }

    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    let mut paths = Vec::new();
    for root in roots {
        for extension in extensions {
            paths.push(root.join(format!("sf{extension}")));
        }
    }
    paths
}

fn is_usable_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_candidates_include_npm_bun_and_sf_install_dirs() {
        let home = Path::new("/tmp/test-home");
        let paths = user_candidates(home);
        assert!(paths.contains(&home.join(".local/share/sf/bin/sf")));
        if cfg!(windows) {
            assert!(
                paths
                    .iter()
                    .any(|path| path.to_string_lossy().contains("Salesforce CLI"))
            );
        } else {
            assert!(paths.contains(&home.join(".npm-global/bin/sf")));
            assert!(paths.contains(&home.join(".bun/bin/sf")));
        }
    }

    #[test]
    fn sf_installation_keeps_native_arguments() {
        let installation = SfInstallation {
            executable: PathBuf::from("sf"),
            source: "PATH".to_owned(),
        };
        let parts = vec!["org".to_owned(), "list".to_owned()];
        assert_eq!(installation.argv(&parts), parts);
    }

    #[test]
    fn candidates_do_not_include_the_deprecated_sfdx_binary() {
        let paths = user_candidates(Path::new("/tmp/test-home"));
        assert!(paths.iter().all(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !name.starts_with("sfdx"))
        }));
    }
}
