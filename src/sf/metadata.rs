//! Fuzzy component/template suggestions for the quick retrieve, deploy and create flows.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use super::project::SfProject;

/// Which list a prompt field should complete from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Transfer,
    Create,
}

/// A bounded, precomputed set of completion candidates.
#[derive(Debug, Clone)]
pub struct SuggestionSet {
    pub kind: SuggestionKind,
    pub items: Vec<String>,
}

impl SuggestionSet {
    pub fn new(kind: SuggestionKind, items: Vec<String>) -> Self {
        Self { kind, items }
    }

    /// Rank candidates by fuzzy score against the current input.
    pub fn filter(&self, query: &str, limit: usize) -> Vec<String> {
        let mut scored: Vec<(String, i64)> = self
            .items
            .iter()
            .filter_map(|item| {
                let tokens: Vec<&str> = query.split_whitespace().collect();
                if tokens.is_empty() {
                    return Some((item.clone(), 0));
                }
                let mut total = 0i64;
                for token in tokens {
                    total = total.checked_add(matcher().fuzzy_match(item, token)?)?;
                }
                Some((item.clone(), total))
            })
            .collect();
        scored.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        scored.truncate(limit);
        scored.into_iter().map(|(item, _)| item).collect()
    }
}

fn matcher() -> SkimMatcherV2 {
    SkimMatcherV2::default().ignore_case()
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
    SuggestionSet::new(
        SuggestionKind::Create,
        CREATE_TEMPLATES
            .iter()
            .map(|template| template.label.to_owned())
            .collect(),
    )
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

/// Build completion candidates from the project's source files, manifests and common types.
pub fn transfer_suggestions(project: Option<&SfProject>) -> SuggestionSet {
    let mut items: Vec<String> = COMMON_METADATA_TYPES
        .iter()
        .map(|item| (*item).to_owned())
        .collect();
    if let Some(project) = project {
        for file in &project.source_files {
            if let Some(component) = component_from_path(file) {
                items.push(component);
            }
        }
        for manifest in &project.manifests {
            let Ok(xml) = std::fs::read_to_string(project.root.join(manifest)) else {
                continue;
            };
            items.extend(super::project::parse_manifest_components(&xml));
        }
    }
    items.sort();
    items.dedup();
    SuggestionSet::new(SuggestionKind::Transfer, items)
}

fn component_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let file = parts.last()?;
    let folder = if parts.len() > 1 {
        parts[parts.len() - 2]
    } else {
        ""
    };
    let stem = file.split('.').next()?;
    if stem.is_empty() || file.ends_with("-meta.xml") && stem.is_empty() {
        return None;
    }
    let kind = match folder {
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
    };
    // For nested object fields, use the object name as the component qualifier.
    let name = if kind == "CustomField" && parts.len() > 3 {
        parts[parts.len() - 3]
    } else {
        stem
    };
    Some(format!("{kind}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_suggestions_fuzzy_rank_and_dedupe() {
        let set = SuggestionSet::new(
            SuggestionKind::Transfer,
            vec![
                "ApexClass:Demo".to_owned(),
                "Flow:Demo".to_owned(),
                "Layout:Account".to_owned(),
            ],
        );
        let hits = set.filter("apex demo", 5);
        assert_eq!(hits.first().map(String::as_str), Some("ApexClass:Demo"));
        assert!(set.filter("zzzz", 5).is_empty());
    }

    #[test]
    fn component_paths_map_to_types() {
        assert_eq!(
            component_from_path("force-app/main/default/classes/Demo.cls").as_deref(),
            Some("ApexClass:Demo")
        );
        assert_eq!(
            component_from_path("force-app/main/default/flows/Demo.flow-meta.xml").as_deref(),
            Some("Flow:Demo")
        );
        assert!(component_from_path("force-app/main/default/classes/Demo.cls-meta.xml").is_some());
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
