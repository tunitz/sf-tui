//! Copy text to the OS clipboard using utilities already present on the host.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("no clipboard helper found (install xclip/xsel on Linux)")]
    NoHelper,
    #[error("clipboard helper failed: {0}")]
    Failed(String),
}

/// Copy `text` to the clipboard on Linux, macOS or Windows without extra runtime services.
pub fn copy(text: &str) -> Result<(), ClipboardError> {
    #[cfg(target_os = "windows")]
    {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Set-Clipboard -Value $input"])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| ClipboardError::Failed(error.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(text.as_bytes())
                .map_err(|error| ClipboardError::Failed(error.to_string()))?;
        }
        let status = child
            .wait()
            .map_err(|error| ClipboardError::Failed(error.to_string()))?;
        return if status.success() {
            Ok(())
        } else {
            Err(ClipboardError::Failed(
                "Set-Clipboard returned an error".to_owned(),
            ))
        };
    }
    #[cfg(target_os = "macos")]
    {
        return pipe_to("pbcopy", text);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        for helper in ["xclip", "xsel", "wl-copy"] {
            match pipe_to(helper, text) {
                Ok(()) => return Ok(()),
                Err(ClipboardError::NoHelper) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ClipboardError::NoHelper)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
fn pipe_to(helper: &str, text: &str) -> Result<(), ClipboardError> {
    let mut child = Command::new(helper)
        .args(match helper {
            "xclip" => vec!["-selection", "clipboard"],
            "xsel" => vec!["--clipboard", "--input"],
            _ => vec![],
        })
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ClipboardError::NoHelper,
            _ => ClipboardError::Failed(error.to_string()),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| ClipboardError::Failed(error.to_string()))?;
    }
    let status = child
        .wait()
        .map_err(|error| ClipboardError::Failed(error.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(ClipboardError::Failed(format!(
            "{helper} returned an error"
        )))
    }
}

/// Helper used in tests to explain a missing clipboard utility without panicking.
pub fn helper_paths() -> Vec<PathBuf> {
    ["xclip", "xsel", "wl-copy", "pbcopy"]
        .iter()
        .filter_map(|name| which::which(name).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_paths_do_not_panic() {
        let _ = helper_paths();
    }
}
