//! Fuzzy component/template suggestions for the quick retrieve, deploy and create flows.
//!
//! Suggestions are built once per prompt and cached on the prompt field. Ranking uses one
//! thread-local matcher instead of allocating a matcher on every keystroke or frame.

use std::cell::RefCell;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use super::project::SfProject;

thread_local! {
    static MATCHER: RefCell<SkimMatcherV2> =
        RefCell::new(SkimMatcherV2::default().ignore_case());
}

/// Which list a prompt field should complete from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Transfer,
    Create,
}

/// One ranked candidate: the value inserted on accept and the muted source line shown below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionItem {
    pub value: String,
    pub path: String,
    value_lower: String,
    name_lower: String,
    path_lower: String,
}

impl SuggestionItem {
    pub fn new(value: impl Into<String>, path: impl Into<String>) -> Self {
        let value = value.into();
        let path = path.into();
        let value_lower = value.to_ascii_lowercase();
        let name_lower = value
            .split_once(':')
            .map_or(value_lower.as_str(), |(_, name)| name)
            .to_ascii_lowercase();
        let path_lower = path.to_ascii_lowercase();
        Self {
            value,
            path,
            value_lower,
            name_lower,
            path_lower,
        }
    }
}

/// A bounded, precomputed set of completion candidates.
#[derive(Debug, Clone)]
pub struct SuggestionSet {
    pub kind: SuggestionKind,
    pub items: Vec<SuggestionItem>,
}

impl SuggestionSet {
    pub fn new(kind: SuggestionKind, values: Vec<String>) -> Self {
        Self {
            kind,
            items: values
                .into_iter()
                .map(|value| SuggestionItem::new(value, String::new()))
                .collect(),
        }
    }

    /// Fast substring ranking first; fuzzy matching only fills remaining slots.
    pub fn filter(&self, query: &str, limit: usize) -> Vec<SuggestionItem> {
        let query_lower = query.trim().to_ascii_lowercase();
        if query_lower.is_empty() {
            return self
                .items
                .iter()
                .filter(|item| !item.path.is_empty() || self.kind == SuggestionKind::Create)
                .take(limit)
                .cloned()
                .collect();
        }
        let mut scored: Vec<(SuggestionItem, i64)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in &self.items {
            let score = if item.name_lower == query_lower {
                100_000
            } else if item.name_lower.contains(&query_lower) {
                50_000
            } else if item.value_lower.contains(&query_lower) {
                20_000
            } else if item.path_lower.contains(&query_lower) {
                5_000
            } else {
                continue;
            };
            if seen.insert(item.value.clone()) {
                scored.push((item.clone(), score));
            }
        }
        if scored.len() < limit {
            let tokens: Vec<&str> = query_lower.split_whitespace().collect();
            for item in &self.items {
                if seen.contains(&item.value) {
                    continue;
                }
                let mut total = 0i64;
                let mut matched = true;
                for token in &tokens {
                    match MATCHER
                        .with(|matcher| matcher.borrow().fuzzy_match(&item.value_lower, token))
                    {
                        Some(score) => total = total.saturating_add(score),
                        None => {
                            matched = false;
                            break;
                        }
                    }
                }
                if matched && seen.insert(item.value.clone()) {
                    scored.push((item.clone(), total));
                }
            }
        }
        scored.sort_unstable_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.path.is_empty().cmp(&right.0.path.is_empty()))
                .then_with(|| left.0.value.cmp(&right.0.value))
        });
        scored.truncate(limit);
        scored.into_iter().map(|(item, _)| item).collect()
    }
}

/// Apex classes from the project index, used by the test-run search screen.
pub fn test_suggestions(all: &SuggestionSet, project: Option<&SfProject>) -> SuggestionSet {
    SuggestionSet {
        kind: SuggestionKind::Transfer,
        items: all
            .items
            .iter()
            .filter(|item| {
                let Some(name) = item.value.strip_prefix("ApexClass:") else {
                    return false;
                };
                if name.to_ascii_lowercase().ends_with("test") {
                    return true;
                }
                let Some(project) = project else {
                    return false;
                };
                let Ok(source) = std::fs::read_to_string(project.root.join(&item.path)) else {
                    return false;
                };
                source.to_ascii_lowercase().contains("@istest")
            })
            .cloned()
            .collect(),
    }
}

/// A scaffold command supported by the Create metadata shortcut.
#[derive(Debug, Clone, Copy)]
pub struct CreateTemplate {
    pub label: &'static str,
    pub command: &'static [&'static str],
    pub name_flag: &'static str,
}

pub const CREATE_TEMPLATES: &[CreateTemplate] = &[
    CreateTemplate {
        label: "ApexClass",
        command: &["template", "generate", "apex", "class"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "ApexTrigger",
        command: &["template", "generate", "apex", "trigger"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "Lightning Web Component",
        command: &[
            "template",
            "generate",
            "lightning",
            "component",
            "--type",
            "lwc",
        ],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "Aura Component",
        command: &[
            "template",
            "generate",
            "lightning",
            "component",
            "--type",
            "aura",
        ],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "FlexiPage",
        command: &["template", "generate", "flexipage"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "StaticResource",
        command: &["template", "generate", "static-resource"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "CustomObject",
        command: &["schema", "generate", "sobject"],
        name_flag: "--label",
    },
    CreateTemplate {
        label: "Custom Metadata Type",
        command: &["cmdt", "generate", "object"],
        name_flag: "--type-name",
    },
    CreateTemplate {
        label: "Visualforce Page",
        command: &["template", "generate", "visualforce", "page"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "Visualforce Component",
        command: &["template", "generate", "visualforce", "component"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "Lightning App",
        command: &["template", "generate", "lightning", "app"],
        name_flag: "--name",
    },
    CreateTemplate {
        label: "Analytics Template",
        command: &["template", "generate", "analytics", "template"],
        name_flag: "--name",
    },
];

pub fn create_template_for(label: &str) -> Option<&'static CreateTemplate> {
    let needle = label.trim().to_ascii_lowercase();
    CREATE_TEMPLATES
        .iter()
        .find(|template| template.label.eq_ignore_ascii_case(&needle))
}

pub fn create_suggestions() -> SuggestionSet {
    SuggestionSet {
        kind: SuggestionKind::Create,
        items: CREATE_TEMPLATES
            .iter()
            .map(|template| {
                SuggestionItem::new(template.label, format!("sf {}", template.command.join(" ")))
            })
            .collect(),
    }
}

const COMMON_METADATA_TYPES: &[&str] = &[
    "ApexClass",
    "ApexTrigger",
    "Flow",
    "CustomObject",
    "Layout",
    "PermissionSet",
    "Profile",
    "StaticResource",
    "LightningComponentBundle",
    "ApexPage",
    "CustomLabel",
    "ValidationRule",
];

/// Build completion candidates from every source file, manifest and common metadata type.
pub fn transfer_suggestions(project: Option<&SfProject>) -> SuggestionSet {
    let mut items: Vec<SuggestionItem> = COMMON_METADATA_TYPES
        .iter()
        .map(|kind| SuggestionItem::new(*kind, "type"))
        .collect();
    if let Some(project) = project {
        for file in &project.source_files {
            if let Some((component, path)) = component_from_path(file) {
                items.push(SuggestionItem::new(component, path));
            }
        }
        for manifest in &project.manifests {
            let Ok(xml) = std::fs::read_to_string(project.root.join(manifest)) else {
                continue;
            };
            for component in super::project::parse_manifest_components(&xml) {
                items.push(SuggestionItem::new(component, manifest.clone()));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.value.clone()));
    items.sort_by(|left, right| left.value.cmp(&right.value));
    SuggestionSet {
        kind: SuggestionKind::Transfer,
        items,
    }
}

fn component_from_path(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('/').collect();
    let file = *parts.last()?;
    let folder = if parts.len() > 1 {
        parts[parts.len() - 2]
    } else {
        ""
    };
    let stem = file.split('.').next()?;
    if stem.is_empty() {
        return None;
    }
    // File extensions win: Salesforce projects sometimes keep Apex outside `classes/`.
    let kind = if file.contains(".cls-meta.xml") || file.ends_with(".cls") {
        "ApexClass"
    } else if file.contains(".trigger-meta.xml") || file.ends_with(".trigger") {
        "ApexTrigger"
    } else {
        match folder {
            "classes" => "ApexClass",
            "triggers" => "ApexTrigger",
            "flows" => "Flow",
            "objects" if parts.len() > 3 && parts[parts.len() - 2] == "fields" => "CustomField",
            "objects" => "CustomObject",
            "layouts" => "Layout",
            "permissionSets" => "PermissionSet",
            "profiles" => "Profile",
            "staticresources" => "StaticResource",
            "labels" => "CustomLabel",
            "aura" | "lwc" => "LightningComponentBundle",
            "pages" => "ApexPage",
            "flexipages" => "FlexiPage",
            _ => return None,
        }
    };
    let name = if kind == "CustomField" && parts.len() > 3 {
        parts[parts.len() - 3]
    } else {
        stem
    };
    Some((format!("{kind}:{name}"), path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_components_out_rank_type_only_matches_and_keep_paths() {
        let set = SuggestionSet {
            kind: SuggestionKind::Transfer,
            items: vec![
                SuggestionItem::new("ApexClass", "type"),
                SuggestionItem::new("ApexClass:Demo", "force-app/main/default/classes/Demo.cls"),
            ],
        };
        let hits = set.filter("Demo", 5);
        assert_eq!(hits[0].value, "ApexClass:Demo");
        assert!(hits[0].path.ends_with("Demo.cls"));
        assert!(set.filter("zzzz", 5).is_empty());
    }

    #[test]
    fn component_paths_map_to_types_with_source_paths() {
        let (value, path) =
            component_from_path("force-app/main/default/classes/Demo.cls").expect("component");
        assert_eq!(value, "ApexClass:Demo");
        assert_eq!(path, "force-app/main/default/classes/Demo.cls");
        assert!(component_from_path("force-app/main/default/flows/Demo.flow-meta.xml").is_some());
        assert_eq!(
            component_from_path("src/apex/Demo.cls").map(|(value, _)| value),
            Some("ApexClass:Demo".to_owned())
        );
    }

    #[test]
    fn apex_classes_in_the_current_project_are_suggested_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let relative = "force-app/main/default/classes/DemoController.cls";
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        std::fs::write(&path, "public class DemoController {}").expect("write class");
        let project = SfProject {
            root: dir.path().to_path_buf(),
            package_directories: vec!["force-app".to_owned()],
            source_api_version: None,
            manifests: Vec::new(),
            source_files: vec![relative.to_owned()],
        };
        let hits = transfer_suggestions(Some(&project)).filter("DemoController", 5);
        assert_eq!(hits[0].value, "ApexClass:DemoController");
        assert_eq!(hits[0].path, relative);
    }

    #[test]
    fn long_underscored_apex_class_name_is_found() {
        let name = "C3S_TRG26_FeedBackLoopHandler";
        let relative = format!("force-app/main/default/classes/{name}.cls");
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(&relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        std::fs::write(&path, "public class C3S_TRG26_FeedBackLoopHandler {}")
            .expect("write class");
        let project = SfProject {
            root: dir.path().to_path_buf(),
            package_directories: vec!["force-app".to_owned()],
            source_api_version: None,
            manifests: Vec::new(),
            source_files: vec![relative.clone()],
        };
        let set = transfer_suggestions(Some(&project));
        eprintln!(
            "set contains: {:?}",
            set.items
                .iter()
                .find(|item| item.value.contains("FeedBack"))
        );
        let hits = set.filter(name, 5);
        eprintln!("hits: {:?}", hits);
        assert!(hits.iter().any(|item| item.value.contains(name)));
    }

    #[test]
    fn test_suggestions_keep_only_apex_classes() {
        let all = SuggestionSet::new(
            SuggestionKind::Transfer,
            vec![
                "ApexClass:DemoTest".to_owned(),
                "Flow:Other".to_owned(),
                "ApexClass:Helper".to_owned(),
            ],
        );
        let tests = test_suggestions(&all, None);
        assert_eq!(tests.items.len(), 1);
        assert_eq!(tests.items[0].value, "ApexClass:DemoTest");
        assert!(
            tests
                .items
                .iter()
                .all(|item| item.value.starts_with("ApexClass:"))
        );
    }

    #[test]
    fn test_suggestions_include_is_test_annotated_classes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let relative = "force-app/classes/Helper.cls";
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        std::fs::write(&path, "@IsTest private class Helper {}").expect("write class");
        let all = SuggestionSet {
            kind: SuggestionKind::Transfer,
            items: vec![SuggestionItem::new("ApexClass:Helper", relative)],
        };
        let project = SfProject {
            root: dir.path().to_path_buf(),
            package_directories: Vec::new(),
            source_api_version: None,
            manifests: Vec::new(),
            source_files: vec![relative.to_owned()],
        };
        let tests = test_suggestions(&all, Some(&project));
        assert_eq!(tests.items.len(), 1);
    }

    #[test]
    fn create_template_lookup_is_case_insensitive() {
        assert_eq!(
            create_template_for("apexclass").map(|template| template.label),
            Some("ApexClass")
        );
        assert!(create_template_for("unknown").is_none());
    }
}
