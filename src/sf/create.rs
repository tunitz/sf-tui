//! Pure domain for the Create Metadata wizard: a grouped generator catalog, per-type fields and
//! validation, destination resolution from `sfdx-project.json`, and filesystem-safe publication.
//!
//! Nothing in this module knows about ratatui or crossterm. Generation happens in a private staging
//! directory; final paths are always created with `create_new`, so an existing file is never
//! overwritten and a late collision is reported instead of clobbered.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use thiserror::Error;

/// Failures from resolving or writing a Create Metadata destination.
#[derive(Debug, Error)]
pub enum CreateError {
    #[error("no Salesforce project is open — open sfdx-project.json in your terminal first")]
    NoProject,
    #[error("unsafe project-relative path: {path}")]
    UnsafePath { path: PathBuf },
    #[error("destination is a symlink: {path}")]
    Symlink { path: PathBuf },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read package directories from sfdx-project.json: {0}")]
    ProjectJson(String),
}

/// The four groups shown in the type picker, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateGroup {
    Apex,
    Lightning,
    Schema,
    Other,
}

impl CreateGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Apex => "Apex",
            Self::Lightning => "Lightning",
            Self::Schema => "Schema",
            Self::Other => "Other",
        }
    }
}

/// Every generator the wizard can configure. Interactive-only types never become queued jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    ApexClass,
    ApexTrigger,
    Lwc,
    Aura,
    LightningApp,
    FlexiPage,
    VisualforcePage,
    VisualforceComponent,
    StaticResource,
    CustomMetadataType,
    AnalyticsTemplate,
    CustomObject,
}

/// Catalog in display order: grouped, with every group contiguous.
pub const ALL_TYPES: &[CreateKind] = &[
    CreateKind::ApexClass,
    CreateKind::ApexTrigger,
    CreateKind::Lwc,
    CreateKind::Aura,
    CreateKind::LightningApp,
    CreateKind::FlexiPage,
    CreateKind::CustomObject,
    CreateKind::CustomMetadataType,
    CreateKind::StaticResource,
    CreateKind::VisualforcePage,
    CreateKind::VisualforceComponent,
    CreateKind::AnalyticsTemplate,
];

impl CreateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ApexClass => "Apex Class",
            Self::ApexTrigger => "Apex Trigger",
            Self::Lwc => "Lightning Web Component",
            Self::Aura => "Aura Component",
            Self::LightningApp => "Lightning App",
            Self::FlexiPage => "FlexiPage",
            Self::VisualforcePage => "Visualforce Page",
            Self::VisualforceComponent => "Visualforce Component",
            Self::StaticResource => "Static Resource",
            Self::CustomMetadataType => "Custom Metadata Type",
            Self::AnalyticsTemplate => "Analytics Template",
            Self::CustomObject => "Custom Object",
        }
    }

    pub fn group(self) -> CreateGroup {
        match self {
            Self::ApexClass | Self::ApexTrigger => CreateGroup::Apex,
            Self::Lwc | Self::Aura | Self::LightningApp | Self::FlexiPage => CreateGroup::Lightning,
            Self::CustomObject | Self::CustomMetadataType => CreateGroup::Schema,
            Self::StaticResource
            | Self::VisualforcePage
            | Self::VisualforceComponent
            | Self::AnalyticsTemplate => CreateGroup::Other,
        }
    }

    /// Custom Object generation is interactive in the Salesforce CLI; sftui must never spawn it.
    pub fn interactive_only(self) -> bool {
        matches!(self, Self::CustomObject)
    }

    /// Metadata folder inside the source root. Custom Objects use the same `objects` folder.
    pub fn folder(self) -> &'static str {
        match self {
            Self::ApexClass => "classes",
            Self::ApexTrigger => "triggers",
            Self::Lwc => "lwc",
            Self::Aura | Self::LightningApp => "aura",
            Self::FlexiPage => "flexipages",
            Self::VisualforcePage => "pages",
            Self::VisualforceComponent => "components",
            Self::StaticResource => "staticresources",
            Self::CustomMetadataType | Self::CustomObject => "objects",
            Self::AnalyticsTemplate => "waveTemplates",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        let needle = label.trim();
        ALL_TYPES
            .iter()
            .copied()
            .find(|kind| kind.label().eq_ignore_ascii_case(needle))
    }
}

/// Fields the Configure step shows for each type. Order is display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldKey {
    Name,
    SObject,
    Event,
    FlexiTemplate,
    Label,
    PluralLabel,
    Visibility,
    ContentType,
    ApexTemplate,
}

impl FieldKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::SObject => "SObject",
            Self::Event => "Event",
            Self::FlexiTemplate => "Page type",
            Self::Label => "Label",
            Self::PluralLabel => "Plural label",
            Self::Visibility => "Visibility",
            Self::ContentType => "Content type",
            Self::ApexTemplate => "Class template",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub key: FieldKey,
    pub required: bool,
}

pub fn fields_for(kind: CreateKind) -> Vec<FieldSpec> {
    use FieldKey::*;
    let mut fields = match kind {
        CreateKind::CustomObject => vec![FieldSpec {
            key: Label,
            required: true,
        }],
        CreateKind::ApexClass => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: ApexTemplate,
                required: false,
            },
        ],
        CreateKind::ApexTrigger => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: SObject,
                required: true,
            },
            FieldSpec {
                key: Event,
                required: true,
            },
        ],
        CreateKind::FlexiPage => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: FlexiTemplate,
                required: true,
            },
            FieldSpec {
                key: SObject,
                required: false,
            },
        ],
        CreateKind::VisualforcePage | CreateKind::VisualforceComponent => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: Label,
                required: true,
            },
        ],
        CreateKind::StaticResource => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: ContentType,
                required: false,
            },
        ],
        CreateKind::CustomMetadataType => vec![
            FieldSpec {
                key: Name,
                required: true,
            },
            FieldSpec {
                key: Label,
                required: false,
            },
            FieldSpec {
                key: PluralLabel,
                required: false,
            },
            FieldSpec {
                key: Visibility,
                required: false,
            },
        ],
        CreateKind::Lwc
        | CreateKind::Aura
        | CreateKind::LightningApp
        | CreateKind::AnalyticsTemplate => vec![FieldSpec {
            key: Name,
            required: true,
        }],
    };
    // FlexiPage's object is required only for a RecordPage; validation checks the combination.
    if kind == CreateKind::FlexiPage
        && fields.len() == 3
        && let Some(field) = fields.last_mut()
    {
        field.required = false;
    }
    fields
}

/// Default value shown for a field. Empty means the user must type something (unless optional).
pub fn default_for(key: FieldKey) -> &'static str {
    match key {
        FieldKey::Event => "before insert",
        FieldKey::Visibility => "Public",
        FieldKey::ContentType => "application/zip",
        _ => "",
    }
}

/// A resolved package source root (relative to the project root) offered in Configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub label: String,
    pub relative: PathBuf,
}

/// Package choices plus a short explanation shown below the destination row.
#[derive(Debug, Clone, Default)]
pub struct DestinationOptions {
    pub choices: Vec<Destination>,
    pub note: String,
    /// True when several packages exist and no single `default: true` selects one for us.
    pub ambiguous: bool,
}

/// The wizard's typed, UI-independent draft. Only a validated draft becomes a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDraft {
    pub kind: CreateKind,
    pub values: BTreeMap<FieldKey, String>,
    pub destination: Destination,
}

impl CreateDraft {
    pub fn new(kind: CreateKind, destination: Destination) -> Self {
        let mut values = BTreeMap::new();
        for field in fields_for(kind) {
            values.insert(field.key, default_for(field.key).to_owned());
        }
        Self {
            kind,
            values,
            destination,
        }
    }

    pub fn get(&self, key: FieldKey) -> &str {
        self.values.get(&key).map_or("", String::as_str)
    }

    pub fn set(&mut self, key: FieldKey, value: impl Into<String>) {
        self.values.insert(key, value.into());
    }

    /// The name displayed in the success message. Custom Objects use their label.
    pub fn display_name(&self) -> &str {
        if self.kind.interactive_only() {
            self.get(FieldKey::Label)
        } else {
            self.get(FieldKey::Name)
        }
    }

    /// Inline validation for the Configure step. The field key points at the offending row.
    pub fn validation_error(&self) -> Option<(FieldKey, String)> {
        use FieldKey::*;
        if self.kind.interactive_only() {
            if self.get(Label).trim().is_empty() {
                return Some((Label, "Enter the object label".to_owned()));
            }
            return None;
        }
        let name = self.get(Name).trim();
        if name.is_empty() {
            return Some((Name, "Enter a name".to_owned()));
        }
        let first = name.chars().next()?;
        if !first.is_ascii_alphabetic() {
            return Some((Name, "Name must start with a letter".to_owned()));
        }
        if !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Some((Name, "Use letters, digits and underscores only".to_owned()));
        }
        if self.kind == CreateKind::Lwc && !first.is_ascii_lowercase() {
            return Some((
                Name,
                "LWC names must start with a lowercase letter".to_owned(),
            ));
        }
        match self.kind {
            CreateKind::ApexTrigger => {
                if self.get(SObject).trim().is_empty() {
                    return Some((SObject, "Enter the object API name".to_owned()));
                }
                if self.get(Event).trim().is_empty() {
                    return Some((Event, "Choose at least one trigger event".to_owned()));
                }
            }
            CreateKind::FlexiPage => {
                let template = self.get(FlexiTemplate).trim();
                if !matches!(template, "RecordPage" | "AppPage" | "HomePage") {
                    return Some((
                        FlexiTemplate,
                        "Choose RecordPage, AppPage or HomePage".to_owned(),
                    ));
                }
                if template == "RecordPage" && self.get(SObject).trim().is_empty() {
                    return Some((SObject, "RecordPage requires an object API name".to_owned()));
                }
            }
            CreateKind::VisualforcePage | CreateKind::VisualforceComponent => {
                if self.get(Label).trim().is_empty() {
                    return Some((Label, "Enter a label".to_owned()));
                }
            }
            CreateKind::StaticResource => {
                let content_type = self.get(ContentType).trim();
                if content_type.is_empty() || !content_type.contains('/') {
                    return Some((
                        ContentType,
                        "Enter a MIME type, e.g. application/zip".to_owned(),
                    ));
                }
            }
            CreateKind::CustomMetadataType => {
                let visibility = self.get(Visibility).trim();
                if !matches!(visibility, "Public" | "Protected" | "PackageProtected") {
                    return Some((
                        Visibility,
                        "Choose Public, Protected or PackageProtected".to_owned(),
                    ));
                }
            }
            CreateKind::ApexClass => {
                let template = self.get(ApexTemplate).trim();
                const OPTIONS: &[&str] = &[
                    "DefaultApexClass",
                    "ApexException",
                    "ApexUnitTest",
                    "BasicUnitTest",
                    "Batchable",
                    "InboundEmailService",
                    "Queueable",
                ];
                if !template.is_empty() && !OPTIONS.contains(&template) {
                    return Some((
                        ApexTemplate,
                        "Use a listed template or leave blank for the default".to_owned(),
                    ));
                }
            }
            _ => {}
        }
        None
    }
}

/// Read package directories from a project's `sfdx-project.json` and resolve each source root.
///
/// A package path of `force-app` resolves to `force-app/main/default` when that layout exists, so
/// a package that already ends in `main/default` is never doubled up.
pub fn destination_options(project_root: Option<&Path>) -> DestinationOptions {
    let Some(root) = project_root else {
        return DestinationOptions {
            note: "No sfdx-project.json found above this directory. Open a Salesforce project in your terminal, then reopen Create Metadata. Nothing is generated in sftui's own directory."
                .to_owned(),
            ..DestinationOptions::default()
        };
    };
    let config_path = root.join("sfdx-project.json");
    let Ok(bytes) = std::fs::read(&config_path) else {
        return DestinationOptions {
            note: format!("Cannot read {}", config_path.display()),
            ..DestinationOptions::default()
        };
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return DestinationOptions {
            note: "sfdx-project.json is not valid JSON".to_owned(),
            ..DestinationOptions::default()
        };
    };
    let Some(entries) = value.get("packageDirectories").and_then(Value::as_array) else {
        return DestinationOptions {
            note: "sfdx-project.json has no packageDirectories".to_owned(),
            ..DestinationOptions::default()
        };
    };
    let mut choices: Vec<Destination> = Vec::new();
    let mut default_count = 0usize;
    for entry in entries {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            continue;
        };
        let path = path.trim().trim_end_matches(['/', '\\']);
        if path.is_empty() {
            continue;
        }
        let is_default = entry
            .get("default")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_default {
            default_count += 1;
        }
        let relative = source_root_for(root, path);
        let label = if is_default {
            format!("{relative}  (default)")
        } else {
            relative.clone()
        };
        choices.push(Destination {
            label,
            relative: PathBuf::from(relative),
        });
    }
    if choices.is_empty() {
        return DestinationOptions {
            note: "sfdx-project.json has no usable packageDirectories".to_owned(),
            ..DestinationOptions::default()
        };
    }
    choices.sort_by(|left, right| {
        let left_default = left.label.ends_with("(default)");
        let right_default = right.label.ends_with("(default)");
        right_default
            .cmp(&left_default)
            .then(left.label.cmp(&right.label))
    });
    let ambiguous = choices.len() > 1 && default_count != 1;
    let note = if ambiguous {
        "Multiple packages and no single default — choose one explicitly".to_owned()
    } else {
        String::new()
    };
    DestinationOptions {
        choices,
        note,
        ambiguous,
    }
}

fn source_root_for(root: &Path, package: &str) -> String {
    // A package that already encodes the standard layout must not get a second `main/default`.
    let trimmed = package.trim_end_matches('/').trim_end_matches('\\');
    if trimmed.ends_with("main/default") || trimmed.ends_with("main\\default") {
        return trimmed.replace('\\', "/");
    }
    let with_layout = Path::new(trimmed).join("main").join("default");
    if root.join(&with_layout).is_dir() {
        with_layout.to_string_lossy().replace('\\', "/")
    } else {
        trimmed.replace('\\', "/")
    }
}

/// Join a project-relative path without allowing escapes or symlinked ancestors.
pub fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf, CreateError> {
    let mut result = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CreateError::UnsafePath {
                    path: relative.to_path_buf(),
                });
            }
        }
    }
    if result == root && relative.components().next().is_none() {
        return Err(CreateError::UnsafePath {
            path: relative.to_path_buf(),
        });
    }
    let mut cursor = root.to_path_buf();
    for component in result.components().skip(root.components().count()) {
        cursor.push(component.as_os_str());
        match cursor.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CreateError::Symlink { path: cursor });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CreateError::Io {
                    path: cursor,
                    source,
                });
            }
        }
    }
    Ok(result)
}

/// The final metadata folder for a draft, fully resolved and safety-checked.
pub fn target_dir(draft: &CreateDraft, project_root: &Path) -> Result<PathBuf, CreateError> {
    let base = safe_join(project_root, &draft.destination.relative)?;
    safe_join(&base, Path::new(draft.kind.folder()))
}

/// Primary paths the generator is expected to produce, used for the pre-flight collision check.
pub fn expected_paths(
    draft: &CreateDraft,
    project_root: &Path,
) -> Result<Vec<PathBuf>, CreateError> {
    if draft.kind.interactive_only() {
        return Ok(Vec::new());
    }
    let dir = target_dir(draft, project_root)?;
    let name = draft.get(FieldKey::Name);
    Ok(match draft.kind {
        CreateKind::ApexClass => vec![
            dir.join(format!("{name}.cls")),
            dir.join(format!("{name}.cls-meta.xml")),
        ],
        CreateKind::ApexTrigger => vec![
            dir.join(format!("{name}.trigger")),
            dir.join(format!("{name}.trigger-meta.xml")),
        ],
        CreateKind::VisualforcePage => vec![
            dir.join(format!("{name}.page")),
            dir.join(format!("{name}.page-meta.xml")),
        ],
        CreateKind::VisualforceComponent => vec![
            dir.join(format!("{name}.component")),
            dir.join(format!("{name}.component-meta.xml")),
        ],
        CreateKind::StaticResource => vec![
            dir.join(format!("{name}.resource")),
            dir.join(format!("{name}.resource-meta.xml")),
        ],
        CreateKind::FlexiPage => vec![dir.join(format!("{name}.flexipage-meta.xml"))],
        CreateKind::CustomMetadataType => vec![dir.join(format!("{name}__mdt"))],
        CreateKind::Lwc
        | CreateKind::Aura
        | CreateKind::LightningApp
        | CreateKind::AnalyticsTemplate => vec![dir.join(name)],
        CreateKind::CustomObject => Vec::new(),
    })
}

/// Paths from `expected_paths` that already exist. Never call this a "safe overwrite".
pub fn existing_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|path| path.exists()).cloned().collect()
}

/// Create a private, unique staging directory inside the project for one job.
pub fn create_staging(project_root: &Path, job_id: u64) -> Result<PathBuf, CreateError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let relative = format!(".sftui-create-{job_id}-{nanos}");
    let path = safe_join(project_root, Path::new(&relative))?;
    std::fs::create_dir(&path).map_err(|source| CreateError::Io {
        path: path.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(path)
}

/// Report what publication wrote, what it refused to overwrite, and any error it hit.
#[derive(Debug, Clone, Default)]
pub struct PublishReport {
    pub written: Vec<PathBuf>,
    pub collisions: Vec<PathBuf>,
    pub error: Option<String>,
}

impl PublishReport {
    pub fn succeeded(&self) -> bool {
        self.error.is_none() && self.collisions.is_empty()
    }
}

/// Publish everything the CLI generated in `staging` under `target`, using exclusive creation.
///
/// Files are published in path order, deepest parents first. A file that appears at a final path
/// after the pre-flight check causes a collision; it is left exactly as it was. Nothing is deleted
/// at the destination, and the caller is told which paths were written before the failure.
pub fn publish(staging: &Path, target: &Path) -> PublishReport {
    let mut report = PublishReport::default();
    let mut entries = Vec::new();
    collect_entries(staging, staging, &mut entries, &mut report.error);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (relative, is_dir) in entries {
        let Ok(final_path) = safe_join(target, &relative) else {
            report.error = Some(format!("unsafe generated path: {}", relative.display()));
            break;
        };
        if final_path.symlink_metadata().is_ok() {
            report.collisions.push(final_path);
            continue;
        }
        if is_dir {
            if let Some(parent) = final_path.parent()
                && let Err(source) = std::fs::create_dir_all(parent)
            {
                report.error = Some(format!("cannot create {}: {source}", parent.display()));
                break;
            }
            if let Err(source) = std::fs::create_dir(&final_path) {
                if source.kind() == std::io::ErrorKind::AlreadyExists {
                    report.collisions.push(final_path);
                } else {
                    report.error =
                        Some(format!("cannot create {}: {source}", final_path.display()));
                    break;
                }
            }
        } else if let Some(parent) = final_path.parent()
            && let Err(source) = std::fs::create_dir_all(parent)
        {
            report.error = Some(format!("cannot create {}: {source}", parent.display()));
            break;
        } else {
            let opened = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&final_path);
            match opened {
                Ok(mut destination) => match std::fs::read(staging.join(&relative)) {
                    Ok(bytes) => {
                        if let Err(source) = destination.write_all(&bytes) {
                            report.error =
                                Some(format!("cannot write {}: {source}", final_path.display()));
                            break;
                        }
                        report.written.push(final_path);
                    }
                    Err(source) => {
                        report.error = Some(format!(
                            "cannot read generated file {}: {source}",
                            staging.join(&relative).display()
                        ));
                        break;
                    }
                },
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    report.collisions.push(final_path);
                }
                Err(source) => {
                    report.error =
                        Some(format!("cannot create {}: {source}", final_path.display()));
                    break;
                }
            }
        }
    }
    report
}

fn collect_entries(
    root: &Path,
    directory: &Path,
    out: &mut Vec<(PathBuf, bool)>,
    error: &mut Option<String>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        *error = Some(format!("cannot read {}", directory.display()));
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            *error = Some(format!(
                "generated symlink is not allowed: {}",
                entry.path().display()
            ));
            continue;
        }
        let relative = match entry.path().strip_prefix(root) {
            Ok(path) => path.to_path_buf(),
            Err(_) => PathBuf::new(),
        };
        if file_type.is_dir() {
            out.push((relative.clone(), true));
            collect_entries(root, &entry.path(), out, error);
        } else if file_type.is_file() {
            out.push((relative, false));
        }
    }
}

/// Remove a staging directory after publication. Only our own private directory is touched.
pub fn discard_staging(staging: &Path) {
    let _ = std::fs::remove_dir_all(staging);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for (relative, content) in files {
            let path = dir.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(path, content).expect("write");
        }
        dir
    }

    fn destination(label: &str, relative: &str) -> Destination {
        Destination {
            label: label.to_owned(),
            relative: PathBuf::from(relative),
        }
    }

    #[test]
    fn catalog_is_grouped_and_interactive_type_is_marked() {
        for kind in ALL_TYPES {
            let indices: Vec<usize> = ALL_TYPES
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.group() == kind.group())
                .map(|(index, _)| index)
                .collect();
            let contiguous = indices
                .iter()
                .zip(indices.iter().skip(1))
                .all(|(left, right)| right == &(left + 1));
            assert!(
                contiguous,
                "{} group is not contiguous",
                kind.group().label()
            );
        }
        assert!(CreateKind::CustomObject.interactive_only());
        assert!(ALL_TYPES.iter().all(|kind| !kind.label().is_empty()));
        assert_eq!(
            CreateKind::from_label("apex class"),
            Some(CreateKind::ApexClass)
        );
    }

    #[test]
    fn destination_resolves_default_package_and_does_not_double_main_default() {
        let dir = project(&[
            (
                "sfdx-project.json",
                r#"{"packageDirectories":[{"path":"force-app","default":true}]}"#,
            ),
            ("force-app/main/default/classes/.keep", ""),
            ("packages/salesforce/main/default/classes/.keep", ""),
        ]);
        let options = destination_options(Some(dir.path()));
        assert_eq!(options.choices.len(), 1);
        assert_eq!(
            options.choices[0].relative,
            PathBuf::from("force-app/main/default")
        );
        assert!(!options.ambiguous);
    }

    #[test]
    fn nested_package_source_root_is_resolved_from_its_own_layout() {
        let dir = project(&[
            (
                "sfdx-project.json",
                r#"{"packageDirectories":[{"path":"packages/salesforce","default":true}]}"#,
            ),
            ("packages/salesforce/main/default/classes/.keep", ""),
        ]);
        let options = destination_options(Some(dir.path()));
        assert_eq!(
            options.choices[0].relative,
            PathBuf::from("packages/salesforce/main/default")
        );
    }

    #[test]
    fn ambiguous_packages_force_an_explicit_choice() {
        let dir = project(&[(
            "sfdx-project.json",
            r#"{"packageDirectories":[{"path":"a"},{"path":"b"}]}"#,
        )]);
        let options = destination_options(Some(dir.path()));
        assert!(options.ambiguous);
        assert_eq!(options.choices.len(), 2);
    }

    #[test]
    fn missing_project_explains_how_to_open_one() {
        let options = destination_options(None);
        assert!(options.choices.is_empty());
        assert!(options.note.contains("sfdx-project.json"));
    }

    #[test]
    fn validation_rejects_bad_names_and_required_options() {
        let mut draft = CreateDraft::new(
            CreateKind::Lwc,
            destination("pkg", "force-app/main/default"),
        );
        draft.set(FieldKey::Name, "BadName");
        assert!(
            draft
                .validation_error()
                .is_some_and(|(key, _)| key == FieldKey::Name)
        );
        draft.set(FieldKey::Name, "goodName");
        assert!(draft.validation_error().is_none());

        let mut trigger = CreateDraft::new(
            CreateKind::ApexTrigger,
            destination("pkg", "force-app/main/default"),
        );
        trigger.set(FieldKey::Name, "DemoTrigger");
        assert!(
            trigger
                .validation_error()
                .is_some_and(|(key, _)| key == FieldKey::SObject)
        );
        trigger.set(FieldKey::SObject, "Account");
        assert!(trigger.validation_error().is_none());

        let mut page = CreateDraft::new(
            CreateKind::FlexiPage,
            destination("pkg", "force-app/main/default"),
        );
        page.set(FieldKey::Name, "Demo_Page");
        assert!(
            page.validation_error()
                .is_some_and(|(key, _)| key == FieldKey::FlexiTemplate)
        );
        page.set(FieldKey::FlexiTemplate, "RecordPage");
        assert!(
            page.validation_error()
                .is_some_and(|(key, _)| key == FieldKey::SObject)
        );
    }

    #[test]
    fn safe_join_rejects_escapes_and_symlinks() {
        let dir = project(&[("sfdx-project.json", "{}")]);
        assert!(safe_join(dir.path(), Path::new("../escape")).is_err());
        assert!(safe_join(dir.path(), Path::new("/absolute")).is_err());
        assert!(safe_join(dir.path(), Path::new("")).is_err());
    }

    #[test]
    fn publish_uses_exclusive_creation_and_reports_collisions() {
        let dir = project(&[("sfdx-project.json", "{}")]);
        let staging = create_staging(dir.path(), 7).expect("staging");
        std::fs::write(staging.join("Demo.cls"), "public class Demo {}").expect("write staged");
        let final_dir = dir.path().join("force-app/main/default/classes");
        std::fs::create_dir_all(&final_dir).expect("final dir");
        std::fs::write(final_dir.join("Demo.cls"), "existing").expect("existing file");

        let report = publish(&staging, &final_dir);
        assert!(report.written.is_empty());
        assert_eq!(report.collisions, vec![final_dir.join("Demo.cls")]);
        assert_eq!(
            std::fs::read_to_string(final_dir.join("Demo.cls")).expect("read"),
            "existing"
        );
        discard_staging(&staging);
    }

    #[test]
    fn publish_writes_every_file_and_reports_partial_success() {
        let dir = project(&[("sfdx-project.json", "{}")]);
        let staging = create_staging(dir.path(), 9).expect("staging");
        std::fs::create_dir_all(staging.join("Demo")).expect("bundle dir");
        std::fs::write(staging.join("Demo/demo.js"), "export default {}").expect("js");
        std::fs::write(staging.join("Demo/demo.html"), "<template></template>").expect("html");
        let final_dir = dir.path().join("force-app/main/default/lwc");
        let report = publish(&staging, &final_dir);
        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.written.len(), 2);
        assert!(final_dir.join("Demo/demo.js").is_file());
        assert!(final_dir.join("Demo/demo.html").is_file());
        discard_staging(&staging);
    }

    #[test]
    fn expected_paths_cover_each_artifact_kind() {
        let dir = project(&[("sfdx-project.json", "{}")]);
        let mut draft = CreateDraft::new(
            CreateKind::StaticResource,
            destination("pkg", "force-app/main/default"),
        );
        draft.set(FieldKey::Name, "Assets");
        let paths = expected_paths(&draft, dir.path()).expect("paths");
        assert!(paths.iter().any(|path| path.ends_with("Assets.resource")));
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("Assets.resource-meta.xml"))
        );

        let mut cmdt = CreateDraft::new(
            CreateKind::CustomMetadataType,
            destination("pkg", "force-app/main/default"),
        );
        cmdt.set(FieldKey::Name, "Setting");
        let paths = expected_paths(&cmdt, dir.path()).expect("paths");
        assert!(paths.iter().any(|path| path.ends_with("Setting__mdt")));
    }
}
