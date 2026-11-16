//! Locate the Salesforce CLI on PATH and at common per-user install locations.

use std::path::{Path, PathBuf};

/// Which CLI generation was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfFlavor {
    Sf,
    LegacySfdx,
}

impl SfFlavor {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sf => "sf",
            Self::LegacySfdx => "sfdx (legacy)",
        }
    }
}

/// A located Salesforce CLI executable and how it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfInstallation {
    pub executable: PathBuf,
    pub flavor: SfFlavor,
    pub source: String,
}

impl SfInstallation {
    /// Build the argument vector for this installation, translating legacy syntax if needed.
    pub fn argv(&self, parts: &[String]) -> Vec<String> {
        match self.flavor {
            SfFlavor::Sf => parts.to_vec(),
            SfFlavor::LegacySfdx => translate_legacy(parts),
        }
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.executable.display(), self.source)
    }
}

/// Detect the CLI using PATH first, then well-known user-writable install locations.
pub fn detect() -> Option<SfInstallation> {
    if let Some(path) = std::env::var_os("SFTUI_SF_PATH") {
        let path = PathBuf::from(path);
        if is_usable_executable(&path) {
            return Some(SfInstallation {
                flavor: flavor_for(&path),
                executable: path,
                source: "SFTUI_SF_PATH".to_owned(),
            });
        }
    }

    for name in ["sf", "sfdx"] {
        if let Ok(path) = which::which(name)
            && is_usable_executable(&path)
        {
            let flavor = if name == "sf" {
                SfFlavor::Sf
            } else {
                SfFlavor::LegacySfdx
            };
            return Some(SfInstallation {
                executable: path,
                flavor,
                source: "PATH".to_owned(),
            });
        }
    }

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    for path in user_candidates(&home) {
        if is_usable_executable(&path) {
            return Some(SfInstallation {
                flavor: flavor_for(&path),
                executable: path,
                source: "install directory".to_owned(),
            });
        }
    }
    None
}

fn flavor_for(path: &Path) -> SfFlavor {
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("sfdx") {
        SfFlavor::LegacySfdx
    } else {
        SfFlavor::Sf
    }
}

/// Per-user locations that do not require administrator rights.
pub fn user_candidates(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".local").join("share").join("sf").join("bin")];
    if cfg!(windows) {
        for key in ["LOCALAPPDATA", "PROGRAMFILES", "APPDATA"] {
            if let Some(root) = std::env::var_os(key) {
                let root = PathBuf::from(root);
                roots.push(root.join("salesforcedx").join("bin"));
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
        for name in ["sf", "sfdx"] {
            for extension in extensions {
                paths.push(root.join(format!("{name}{extension}")));
            }
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

/// Translate a small set of `sf` commands and flags into legacy `sfdx` syntax.
pub fn translate_legacy(parts: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.starts_with('-') {
            result.push(match part.as_str() {
                "--target-org" => "--targetusername".to_owned(),
                "--target-dev-hub" => "--targetdevhubusername".to_owned(),
                "--check-only" | "--dry-run" => "--checkonly".to_owned(),
                "--source-dir" => "--sourcepath".to_owned(),
                "--test-level" => "--testlevel".to_owned(),
                "--definition-file" => "--definitionfile".to_owned(),
                "--duration-days" => "--durationdays".to_owned(),
                "--api-version" => "--apiversion".to_owned(),
                _ => part.clone(),
            });
            continue;
        }
        let translated = match (
            index,
            part.as_str(),
            parts.get(index + 1).map(String::as_str),
        ) {
            (0, "org", _) => "force:org",
            (0, "data", _) => "force:data",
            (0, "source", _) => "force:source",
            (0, "user", _) => "force:user",
            (0, "alias", _) => "force:alias",
            (0, "project", Some("deploy")) => "force:source",
            (0, "project", Some("retrieve")) => "force:source",
            (1, "deploy", Some(_)) if parts.first().map(String::as_str) == Some("force:source") => {
                "deploy"
            }
            (1, "retrieve", Some(_))
                if parts.first().map(String::as_str) == Some("force:source") =>
            {
                "retrieve"
            }
            _ => part,
        };
        result.push(translated.to_owned());
    }
    // `sf project deploy start` -> `sfdx force:source:deploy`; the extra "start" is dropped.
    if result.first().map(String::as_str) == Some("force:source")
        && matches!(
            result.get(1).map(String::as_str),
            Some("deploy") | Some("retrieve")
        )
        && result.get(2).map(String::as_str) == Some("start")
    {
        result.remove(2);
    }
    result
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
    fn legacy_translation_maps_org_commands_and_flags() {
        let parts = vec![
            "org".to_owned(),
            "list".to_owned(),
            "--json".to_owned(),
            "--target-org".to_owned(),
            "dev".to_owned(),
        ];
        assert_eq!(
            translate_legacy(&parts),
            vec!["force:org", "list", "--json", "--targetusername", "dev"]
        );
    }

    #[test]
    fn legacy_translation_collapses_deploy_start() {
        let parts = vec![
            "project".to_owned(),
            "deploy".to_owned(),
            "start".to_owned(),
            "--dry-run".to_owned(),
        ];
        assert_eq!(
            translate_legacy(&parts),
            vec!["force:source", "deploy", "--checkonly"]
        );
    }

    #[test]
    fn sf_installation_keeps_native_arguments() {
        let installation = SfInstallation {
            executable: PathBuf::from("sf"),
            flavor: SfFlavor::Sf,
            source: "PATH".to_owned(),
        };
        let parts = vec!["org".to_owned(), "list".to_owned()];
        assert_eq!(installation.argv(&parts), parts);
    }
}
