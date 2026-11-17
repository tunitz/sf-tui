//! SFDX project discovery and bounded source/manifest enumeration.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

const MAX_ENTRIES: usize = 4_000;
const MAX_DEPTH: usize = 8;
const IGNORED_DIRS: &[&str] = &[".git", ".sf", ".sfdx", "node_modules", "target", ".vscode"];

/// Errors from loading an SFDX project.
#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct RawProject {
    #[serde(default, rename = "packageDirectories")]
    package_directories: Vec<RawPackageDirectory>,
    #[serde(default, rename = "sourceApiVersion")]
    source_api_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPackageDirectory {
    #[serde(default)]
    path: String,
}

/// A discovered project with selection candidates relative to its root.
#[derive(Debug, Clone)]
pub struct SfProject {
    pub root: PathBuf,
    pub package_directories: Vec<String>,
    pub source_api_version: Option<String>,
    pub manifests: Vec<String>,
    pub source_files: Vec<String>,
}

impl SfProject {
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }
}

/// Walk upwards from `start` looking for `sfdx-project.json`.
pub fn find_project(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        let candidate = current.join("sfdx-project.json");
        if candidate.is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Load and enumerate a project; directory walking is depth- and count-bounded.
pub fn load_project(root: PathBuf) -> Result<SfProject, ProjectError> {
    let path = root.join("sfdx-project.json");
    let bytes = fs::read(&path).map_err(|source| ProjectError::Read {
        path: path.clone(),
        source,
    })?;
    let raw: RawProject = serde_json::from_slice(&bytes).map_err(|source| ProjectError::Parse {
        path: path.clone(),
        source,
    })?;
    let package_directories: Vec<String> = raw
        .package_directories
        .into_iter()
        .map(|directory| directory.path)
        .filter(|path| !path.is_empty())
        .collect();
    let mut manifests = Vec::new();
    let mut source_files = Vec::new();
    // Scan the project root, not only declared package directories: Apex can live in custom
    // folders and still need to be searchable by name.
    let entries = walk(&root);
    let mut paths: Vec<String> = entries.iter().map(|path| relative(&root, path)).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        if path.ends_with("package.xml") {
            manifests.push(path);
        } else {
            source_files.push(path);
        }
    }
    Ok(SfProject {
        root,
        package_directories,
        source_api_version: raw.source_api_version,
        manifests,
        source_files,
    })
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        if found.len() >= MAX_ENTRIES || depth > MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !IGNORED_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file() {
                found.push(path);
                if found.len() >= MAX_ENTRIES {
                    break;
                }
            }
        }
    }
    found
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Extract `Type:Member` pairs from package.xml `<types>` blocks.
pub fn parse_manifest_components(xml: &str) -> Vec<String> {
    let mut components = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find("<types>") {
        let after_open = &remaining[start + "<types>".len()..];
        let Some(end) = after_open.find("</types>") else {
            break;
        };
        let block = &after_open[..end];
        let kind = tag_content(block, "name").unwrap_or_default();
        for member in all_tag_contents(block, "members") {
            if !kind.is_empty() && !member.is_empty() {
                components.push(format!("{kind}:{member}"));
            }
        }
        remaining = &after_open[end + "</types>".len()..];
    }
    components.sort();
    components.dedup();
    components
}

fn tag_content(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(xml[start..end].trim().to_owned())
}

fn all_tag_contents(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find(&open) {
        let after_open = &remaining[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        let value = after_open[..end].trim().to_owned();
        if !value.is_empty() {
            values.push(value);
        }
        remaining = &after_open[end + close.len()..];
    }
    values
}

/// Extract `<members>` values from a package.xml manifest.
pub fn parse_manifest_members(xml: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find("<members>") {
        let after_open = &remaining[start + "<members>".len()..];
        let Some(end) = after_open.find("</members>") else {
            break;
        };
        let member = after_open[..end].trim();
        if !member.is_empty() {
            members.push(member.to_owned());
        }
        remaining = &after_open[end + "</members>".len()..];
    }
    members.sort();
    members.dedup();
    members
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_discovery_walks_upwards() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("force-app").join("main").join("default");
        fs::create_dir_all(&nested).expect("create dirs");
        fs::write(dir.path().join("sfdx-project.json"), "{}").expect("write");
        assert_eq!(find_project(&nested), Some(dir.path().to_path_buf()));
    }

    #[test]
    fn project_loading_lists_manifest_and_source_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("sfdx-project.json"),
            r#"{"packageDirectories":[{"path":"force-app","default":true}],"sourceApiVersion":"65.0"}"#,
        )
        .expect("write project");
        let source = dir.path().join("force-app/main/default/classes");
        fs::create_dir_all(&source).expect("create dirs");
        fs::write(source.join("Demo.cls"), "public class Demo {}").expect("write cls");
        fs::create_dir_all(dir.path().join("force-app/manifest")).expect("create manifest dir");
        fs::write(
            dir.path().join("force-app/manifest/package.xml"),
            "<Package/>",
        )
        .expect("write manifest");
        let project = load_project(dir.path().to_path_buf()).expect("load");
        assert_eq!(project.source_api_version.as_deref(), Some("65.0"));
        assert!(
            project
                .manifests
                .iter()
                .any(|path| path.ends_with("package.xml"))
        );
        assert!(
            project
                .source_files
                .iter()
                .any(|path| path.ends_with("Demo.cls"))
        );
    }

    #[test]
    fn project_scan_includes_sources_outside_declared_package_dirs() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("sfdx-project.json"),
            r#"{"packageDirectories":[{"path":"force-app","default":true}]}"#,
        )
        .expect("write project");
        let apex = dir.path().join("src/apex");
        fs::create_dir_all(&apex).expect("create dirs");
        fs::write(
            apex.join("DemoController.cls"),
            "public class DemoController {}",
        )
        .expect("write class");
        let project = load_project(dir.path().to_path_buf()).expect("load");
        assert!(
            project
                .source_files
                .iter()
                .any(|path| path.ends_with("DemoController.cls"))
        );
    }

    #[test]
    fn manifest_components_include_type_and_member() {
        let xml = "<Package><types><members>Demo</members><name>ApexClass</name></types><types><members>Account</members><name>CustomObject</name></types></Package>";
        assert_eq!(
            parse_manifest_components(xml),
            vec!["ApexClass:Demo", "CustomObject:Account"]
        );
    }

    #[test]
    fn manifest_members_are_extracted_and_deduplicated() {
        let xml = "<Package><types><members>ApexClass</members><members>ApexClass</members><members>CustomObject</members></types></Package>";
        assert_eq!(
            parse_manifest_members(xml),
            vec!["ApexClass", "CustomObject"]
        );
    }
}
