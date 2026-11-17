//! Self-update from the project's GitHub releases.
//!
//! The updater deliberately uses the same small set of platform tools as the installers
//! (`curl`/`wget` and `tar` on Unix, PowerShell/certutil on Windows) instead of pulling a large
//! HTTP and TLS stack into a single-binary CLI. It always verifies the release checksum before
//! replacing the running executable.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;
use thiserror::Error;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/tunitz/sf-tui/releases/latest";
const RELEASE_DOWNLOAD: &str = "https://github.com/tunitz/sf-tui/releases/download";

/// Options accepted by `sftui update`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateOptions {
    pub check_only: bool,
    pub force: bool,
}

/// Errors surfaced by the updater.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("this platform is not supported by the prebuilt releases ({os}/{arch})")]
    UnsupportedTarget {
        os: &'static str,
        arch: &'static str,
    },
    #[error("could not run {tool}: {source}")]
    Tool {
        tool: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{tool} failed: {detail}")]
    ToolFailed { tool: &'static str, detail: String },
    #[error("could not read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the latest release response did not include a tag")]
    MissingTag,
    #[error("release tag is not a valid version: {0}")]
    InvalidVersion(String),
    #[error("could not parse release metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("release {version} does not publish a SHA256SUMS file")]
    MissingChecksums { version: String },
    #[error("SHA256SUMS has no entry for {0}")]
    MissingChecksum(String),
    #[error("checksum mismatch for {archive}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        archive: String,
        expected: String,
        actual: String,
    },
    #[error("the release archive did not contain {0}")]
    MissingBinary(String),
    #[error("could not replace {path}: {source}")]
    Replace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl UpdateError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Check for or install the latest release.
pub fn run(options: UpdateOptions) -> Result<(), UpdateError> {
    let target = target_triple()?;
    let current = env!("CARGO_PKG_VERSION");
    let work = TempDir::new("sftui-update")?;

    println!("Checking the latest sftui release…");
    let latest = latest_release(&work)?;
    let ordering = compare_versions(&latest, current);

    if !options.force {
        if ordering == Some(Ordering::Equal) {
            println!("sftui {current} is up to date.");
            return Ok(());
        }
        if ordering == Some(Ordering::Less) {
            println!(
                "The latest published release is {latest}, older than this build ({current})."
            );
            println!("Use `sftui update --force` to install it anyway.");
            return Ok(());
        }
    }

    if options.check_only {
        println!("Update available: {current} → {latest}.");
        return Ok(());
    }

    let archive_name = archive_name(&latest, target);
    let archive_path = work.path().join(&archive_name);
    println!("Downloading {archive_name}…");
    download(
        &format!("{RELEASE_DOWNLOAD}/v{latest}/{archive_name}"),
        &archive_path,
    )?;

    let sums_path = work.path().join("SHA256SUMS");
    let sums_url = format!("{RELEASE_DOWNLOAD}/v{latest}/SHA256SUMS");
    if download(&sums_url, &sums_path).is_err() {
        return Err(UpdateError::MissingChecksums {
            version: latest.clone(),
        });
    }
    let sums =
        fs::read_to_string(&sums_path).map_err(|error| UpdateError::io(&sums_path, error))?;
    let expected = parse_checksum(&sums, &archive_name)
        .ok_or_else(|| UpdateError::MissingChecksum(archive_name.clone()))?;
    let actual = sha256_file(&archive_path)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(UpdateError::ChecksumMismatch {
            archive: archive_name,
            expected,
            actual,
        });
    }
    println!("Checksum verified.");

    let extracted = work.path().join("extracted");
    fs::create_dir_all(&extracted).map_err(|error| UpdateError::io(&extracted, error))?;
    extract(&archive_path, &extracted)?;
    let binary_name = binary_name();
    let new_binary = extracted.join(binary_name);
    if !new_binary.is_file() {
        return Err(UpdateError::MissingBinary(binary_name.to_owned()));
    }

    match install(&new_binary)? {
        InstallOutcome::Replaced => {
            println!("Updated sftui to {latest}. Restart the app to use it.");
        }
        InstallOutcome::Staged => {
            println!("Update to {latest} downloaded and staged.");
            println!("It will replace the running executable after sftui exits.");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallOutcome {
    Replaced,
    #[cfg_attr(not(windows), allow(dead_code))]
    Staged,
}

fn target_triple() -> Result<&'static str, UpdateError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        _ => Err(UpdateError::UnsupportedTarget { os, arch }),
    }
}

fn archive_name(version: &str, target: &str) -> String {
    let extension = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("sftui-{version}-{target}.{extension}")
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "sftui.exe" } else { "sftui" }
}

fn latest_release(work: &TempDir) -> Result<String, UpdateError> {
    let path = work.path().join("latest.json");
    download(LATEST_RELEASE_API, &path)?;
    let bytes = fs::read(&path).map_err(|error| UpdateError::io(&path, error))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or(UpdateError::MissingTag)?;
    let version = tag.trim().trim_start_matches('v');
    if !valid_version(version) {
        return Err(UpdateError::InvalidVersion(tag.to_owned()));
    }
    Ok(version.to_owned())
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-+".contains(character))
}

fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let left = core_version(left);
    let right = core_version(right);
    match (left, right) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        _ => None,
    }
}

fn core_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .split(['-', '+'])
        .next()
        .filter(|core| !core.is_empty())?;
    let mut numbers = core.split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    let patch = numbers.next().unwrap_or("0").parse().ok()?;
    if numbers.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Extract the checksum for `archive_name` from a `SHA256SUMS` document.
fn parse_checksum(sums: &str, archive_name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let name = name.trim_start_matches('*');
        if name == archive_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    #[cfg(windows)]
    {
        if let Ok(output) = Command::new("certutil")
            .arg("-hashfile")
            .arg(path)
            .arg("SHA256")
            .output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(hash) = text.lines().find_map(|line| {
                let line = line.trim();
                (line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
                    .then(|| line.to_ascii_lowercase())
            }) {
                return Ok(hash);
            }
        }
        let script = format!(
            "$h = (Get-FileHash -Algorithm SHA256 -LiteralPath {}).Hash.ToLowerInvariant(); $h",
            ps_quote(&path.to_string_lossy())
        );
        let output = powershell(&script)?;
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
        return Err(UpdateError::ToolFailed {
            tool: "Get-FileHash",
            detail: "no SHA-256 digest returned".to_owned(),
        });
    }
    #[cfg(not(windows))]
    {
        if let Ok(output) = Command::new("sha256sum").arg(path).output()
            && output.status.success()
            && let Some(hash) = first_hex_token(&String::from_utf8_lossy(&output.stdout))
        {
            return Ok(hash);
        }
        let output = Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .map_err(|source| UpdateError::Tool {
                tool: "shasum",
                source,
            })?;
        if !output.status.success() {
            return Err(UpdateError::ToolFailed {
                tool: "shasum",
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        first_hex_token(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
            UpdateError::ToolFailed {
                tool: "shasum",
                detail: "no SHA-256 digest returned".to_owned(),
            }
        })
    }
}

#[cfg(not(windows))]
fn first_hex_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

fn download(url: &str, destination: &Path) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        let script = format!(
            "$ProgressPreference = 'SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing -Headers @{{'User-Agent'='sftui-update'}} -Uri {} -OutFile {}",
            ps_quote(url),
            ps_quote(&destination.to_string_lossy())
        );
        powershell(&script)?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let curl = Command::new("curl")
            .args([
                "-fsSL",
                "--retry",
                "3",
                "--proto",
                "=https",
                "--tlsv1.2",
                "-A",
                "sftui-update",
                "-o",
            ])
            .arg(destination)
            .arg(url)
            .output();
        match curl {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(UpdateError::ToolFailed {
                tool: "curl",
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }),
            Err(_) => {
                let output = Command::new("wget")
                    .args(["-q", "--user-agent=sftui-update", "-O"])
                    .arg(destination)
                    .arg(url)
                    .output()
                    .map_err(|source| UpdateError::Tool {
                        tool: "wget",
                        source,
                    })?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(UpdateError::ToolFailed {
                        tool: "wget",
                        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    })
                }
            }
        }
    }
}

#[cfg(unix)]
fn extract(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .output()
        .map_err(|source| UpdateError::Tool {
            tool: "tar",
            source,
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(UpdateError::ToolFailed {
            tool: "tar",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(windows)]
fn extract(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    if let Ok(output) = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .output()
        && output.status.success()
    {
        return Ok(());
    }
    let script = format!(
        "Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
        ps_quote(&archive.to_string_lossy()),
        ps_quote(&destination.to_string_lossy())
    );
    powershell(&script).map(|_| ())
}

fn install(new_binary: &Path) -> Result<InstallOutcome, UpdateError> {
    let executable = std::env::current_exe().map_err(|source| UpdateError::Io {
        path: PathBuf::from("current executable"),
        source,
    })?;
    let file_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sftui");
    let staged = executable.with_file_name(format!(".{file_name}.update-{}", std::process::id()));
    fs::copy(new_binary, &staged).map_err(|source| UpdateError::Replace {
        path: staged.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).map_err(|source| {
            UpdateError::Replace {
                path: staged.clone(),
                source,
            }
        })?;
        fs::rename(&staged, &executable).map_err(|source| {
            let _ = fs::remove_file(&staged);
            UpdateError::Replace {
                path: executable.clone(),
                source,
            }
        })?;
        Ok(InstallOutcome::Replaced)
    }
    #[cfg(windows)]
    {
        let script_path =
            std::env::temp_dir().join(format!("sftui-apply-{}.cmd", std::process::id()));
        let script = format!(
            "@echo off\r\n:retry\r\ntimeout /t 1 /nobreak >nul\r\ncopy /Y \"{}\" \"{}\" >nul 2>&1\r\nif errorlevel 1 goto retry\r\ndel \"{}\" >nul 2>&1\r\ndel \"%~f0\" >nul 2>&1\r\n",
            staged.display(),
            executable.display(),
            staged.display()
        );
        fs::write(&script_path, script).map_err(|source| UpdateError::io(&script_path, source))?;
        Command::new("cmd.exe")
            .arg("/C")
            .arg(&script_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| UpdateError::Tool {
                tool: "cmd.exe",
                source,
            })?;
        Ok(InstallOutcome::Staged)
    }
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<std::process::Output, UpdateError> {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|source| UpdateError::Tool {
            tool: "powershell.exe",
            source,
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(UpdateError::ToolFailed {
            tool: "powershell.exe",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

#[cfg(windows)]
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Result<Self, UpdateError> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).map_err(|error| UpdateError::io(&path, error))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_and_archive_names_are_derived_from_the_platform() {
        assert_eq!(
            archive_name("1.2.3", "x86_64-unknown-linux-gnu"),
            "sftui-1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            archive_name("1.2.3", "x86_64-pc-windows-msvc"),
            "sftui-1.2.3-x86_64-pc-windows-msvc.zip"
        );
        assert!(
            target_triple().is_ok()
                || cfg!(not(any(
                    target_os = "linux",
                    target_os = "macos",
                    target_os = "windows"
                )))
        );
    }

    #[test]
    fn versions_compare_by_core_numbers() {
        assert_eq!(
            compare_versions("0.1.14", "0.1.13"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_versions("0.1.13", "0.1.13"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.1.12", "0.1.13"), Some(Ordering::Less));
        assert_eq!(compare_versions("v0.1.13", "0.1.13"), None);
    }

    #[test]
    fn checksum_lines_are_parsed_and_star_prefixes_are_tolerated() {
        let sums = "aaaa  other.tar.gz\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  *sftui-0.1.14-x86_64-unknown-linux-gnu.tar.gz\n";
        assert_eq!(
            parse_checksum(sums, "sftui-0.1.14-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert!(parse_checksum(sums, "missing.tar.gz").is_none());
    }

    #[test]
    fn unsafe_versions_are_rejected() {
        assert!(valid_version("0.1.14"));
        assert!(!valid_version("../0.1.14"));
        assert!(!valid_version("0.1.14/../../x"));
        assert!(!valid_version(""));
    }
}
