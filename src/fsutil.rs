//! Small filesystem helpers shared by configuration and cache writers.
//!
//! Writes go to a sibling temporary file and are renamed into place, so a crash cannot leave a
//! half-written document. On Unix the file is created 0600 and its directory 0700 because the
//! configuration can contain command text supplied by the user.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Write `contents` to `path` atomically and with owner-only permissions on Unix.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sftui-state");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(".{file_name}.tmp-{}-{stamp}", std::process::id()));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let write_result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(windows)]
        {
            // Windows does not replace an existing file with rename.
            let _ = fs::remove_file(path);
        }
        fs::rename(&temporary, path)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}
