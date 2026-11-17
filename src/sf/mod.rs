//! The SF CLI integration layer: detection, process execution and response parsing.

pub mod commands;
pub mod deploy;
pub mod detect;
pub mod metadata;
pub mod org;
pub mod project;
pub mod runner;
pub mod test_results;

use std::time::Duration;

use serde_json::Value;

use detect::SfInstallation;

use self::runner::parse_json_document;

/// Result of probing the CLI. The TUI starts with a pending snapshot and fills this in
/// through [`StartupEvent`]s, so a slow CLI never blocks the first frame.
#[derive(Debug)]
pub struct Startup {
    pub installation: Option<SfInstallation>,
    pub version: Option<String>,
    pub current_org: Option<String>,
    pub notices: Vec<String>,
}

impl Startup {
    pub fn pending() -> Self {
        Self {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.installation.is_some()
    }
}

/// Progress reported while the CLI is being detected and probed.
#[derive(Debug)]
pub enum StartupEvent {
    Stage(&'static str),
    Detected(SfInstallation),
    Version(String),
    CurrentOrg(String),
    Notice(String),
    Finished,
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Stage the startup work so the UI can render immediately and show real progress.
///
/// Only the version and the configured target org are probed. This is deliberate: the first
/// user command must not queue behind a catalog refresh or an org list.
pub async fn probe(
    config_dir: std::path::PathBuf,
    events: tokio::sync::mpsc::UnboundedSender<StartupEvent>,
) -> Startup {
    probe_with(config_dir, events, None).await
}

/// Same probe with a caller-supplied installation, used by tests and custom launchers.
pub async fn probe_with(
    _config_dir: std::path::PathBuf,
    events: tokio::sync::mpsc::UnboundedSender<StartupEvent>,
    supplied: Option<SfInstallation>,
) -> Startup {
    let send = |event: StartupEvent| {
        let _ = events.send(event);
    };
    let mut result = Startup::pending();
    send(StartupEvent::Stage("Detecting Salesforce CLI"));
    let installation = match supplied {
        Some(installation) => installation,
        None => match detect::detect() {
            Some(installation) => installation,
            None => {
                send(StartupEvent::Notice(
                    "Salesforce CLI not found; open Setup for install options".to_owned(),
                ));
                send(StartupEvent::Finished);
                return result;
            }
        },
    };
    result.installation = Some(installation.clone());
    send(StartupEvent::Detected(installation.clone()));

    let version_probe = async {
        send(StartupEvent::Stage("Reading Salesforce CLI version"));
        let argv = installation.argv(&["version".to_owned(), "--json".to_owned()]);
        match runner::capture_with_env(
            &installation.executable,
            &argv,
            VERSION_TIMEOUT,
            runner::sf_env(),
        )
        .await
        {
            Ok(outcome) => match parse_version(&format!("{}{}", outcome.stdout, outcome.stderr)) {
                Some(version) => Some(version),
                None => {
                    send(StartupEvent::Notice(
                        "Version unavailable; run sftui update if sf is broken".to_owned(),
                    ));
                    None
                }
            },
            Err(error) => {
                send(StartupEvent::Notice(format!(
                    "Version probe skipped: {error}"
                )));
                None
            }
        }
    };

    let org_probe = async {
        let argv = installation.argv(&[
            "config".to_owned(),
            "get".to_owned(),
            "target-org".to_owned(),
            "--json".to_owned(),
        ]);
        if let Ok(outcome) = runner::capture_with_env(
            &installation.executable,
            &argv,
            Duration::from_secs(4),
            runner::sf_env(),
        )
        .await
            && let Some(value) = parse_json_document(&outcome.stdout)
            && let Some(org) = org::parse_current_org(&value)
        {
            send(StartupEvent::CurrentOrg(org.clone()));
            return Some(org);
        }
        None
    };

    let (version, current_org) = tokio::join!(version_probe, org_probe);
    if let Some(version) = version {
        result.version = Some(version.clone());
        send(StartupEvent::Version(version));
    }
    if let Some(org) = current_org {
        result.current_org = Some(org);
    }
    send(StartupEvent::Finished);
    result
}

/// Convenience wrapper for tests and tools that do not need live progress.
pub async fn startup(config_dir: std::path::PathBuf) -> Startup {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(probe(config_dir, tx));
    while rx.recv().await.is_some() {}
    task.await.unwrap_or_else(|_| Startup::pending())
}

/// A non-empty string field shared by the JSON parsers.
pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
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
                "start \"\" https://developer.salesforce.com/tools/salesforcecli",
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
                "xdg-open https://developer.salesforce.com/tools/salesforcecli",
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
