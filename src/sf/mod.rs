//! The SF CLI integration layer: detection, process execution and response parsing.

pub mod catalog;
pub mod deploy;
pub mod detect;
pub mod org;
pub mod project;
pub mod runner;

use std::time::Duration;

use catalog::Catalog;
use detect::SfInstallation;

use self::runner::{RunnerError, parse_json_document};

/// Result of probing the CLI before the UI starts.
#[derive(Debug)]
pub struct Startup {
    pub installation: Option<SfInstallation>,
    pub version: Option<String>,
    pub catalog: Catalog,
    pub notices: Vec<String>,
}

impl Startup {
    pub fn is_ready(&self) -> bool {
        self.installation.is_some()
    }
}

/// Detect the CLI, probe its version and refresh the command catalog. Failures degrade to
/// the bundled snapshot so that the TUI still opens on an offline or broken installation.
pub async fn startup() -> Startup {
    let mut result = Startup {
        installation: None,
        version: None,
        catalog: catalog::snapshot(),
        notices: Vec::new(),
    };
    let Some(installation) = detect::detect() else {
        result.notices.push(
            "Salesforce CLI was not found. Open the Setup tab for install options.".to_owned(),
        );
        return result;
    };
    result.notices.push(format!(
        "Found {} at {}",
        installation.flavor.label(),
        installation.executable.display()
    ));

    let version_argv = installation.argv(&["version".to_owned(), "--json".to_owned()]);
    match runner::capture(
        &installation.executable,
        &version_argv,
        Duration::from_secs(12),
    )
    .await
    {
        Ok(outcome) => {
            let combined = format!("{}{}", outcome.stdout, outcome.stderr);
            result.version = parse_version(&combined);
            if result.version.is_none() {
                result
                    .notices
                    .push("Could not read the Salesforce CLI version.".to_owned());
            }
        }
        Err(error) => result
            .notices
            .push(format!("Version probe failed: {error}")),
    }

    if installation.flavor == detect::SfFlavor::Sf {
        let commands_argv = installation.argv(&["commands".to_owned(), "--json".to_owned()]);
        match runner::capture(
            &installation.executable,
            &commands_argv,
            Duration::from_secs(20),
        )
        .await
        {
            Ok(outcome) => match catalog::parse_commands_json(&outcome.stdout) {
                Ok(commands) => {
                    result.catalog = Catalog {
                        commands,
                        source: catalog::CatalogSource::Live,
                    };
                }
                Err(error) => result
                    .notices
                    .push(format!("Catalog refresh failed: {error}")),
            },
            Err(RunnerError::TimedOut { .. }) => {
                result
                    .notices
                    .push("Catalog refresh timed out; using bundled snapshot.".to_owned());
            }
            Err(error) => result
                .notices
                .push(format!("Catalog refresh failed: {error}")),
        }
    } else {
        result
            .notices
            .push("Legacy sfdx detected; using bundled command catalog.".to_owned());
    }
    result.installation = Some(installation);
    result
}

fn parse_version(text: &str) -> Option<String> {
    if let Some(value) = parse_json_document(text) {
        let candidate = value
            .get("result")
            .and_then(|result| result.get("version"))
            .or_else(|| value.get("version"))
            .and_then(|version| version.as_str());
        if let Some(version) = candidate {
            return Some(version.to_owned());
        }
    }
    text.split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_owned)
}

/// User-writable install hints shown by the bootstrap screen.
pub fn install_instructions() -> Vec<(&'static str, &'static str)> {
    #[cfg(target_os = "windows")]
    {
        vec![
            ("winget", "winget install Salesforce.CLI"),
            (
                "Installer (per-user)",
                "https://developer.salesforce.com/tools/salesforcecli",
            ),
            ("npm (no admin)", "npm install -g @salesforce/cli"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            ("Homebrew", "brew install salesforce-cli"),
            ("npm (no admin)", "npm install -g @salesforce/cli"),
            ("bun (no admin)", "bun add -g @salesforce/cli"),
        ]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            ("npm (no admin)", "npm install -g @salesforce/cli"),
            ("bun (no admin)", "bun add -g @salesforce/cli"),
            (
                "Tarball",
                "https://developer.salesforce.com/tools/salesforcecli",
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_read_from_json_or_plain_output() {
        assert_eq!(
            parse_version("{\"status\":0,\"result\":{\"version\":\"2.100.0\"}}").as_deref(),
            Some("2.100.0")
        );
        assert_eq!(
            parse_version("sf version 2.101.0\n").as_deref(),
            Some("2.101.0")
        );
        assert_eq!(parse_version("no version"), None);
    }

    #[test]
    fn install_instructions_are_nonempty() {
        assert!(!install_instructions().is_empty());
    }
}
