//! Fuzzy component/template suggestions for the quick retrieve, deploy and create flows.
//!
//! Suggestions are built once per prompt and cached on the prompt field. Ranking uses one
//! thread-local matcher instead of allocating a matcher on every keystroke or frame.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde_json::Value;

use super::org::SENSITIVE_FIELDS;
use super::project::SfProject;
use super::string_field;

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

/// Below this many candidates a linear substring scan is cheaper than building and probing an
/// index, so the index stays inactive and `filter` behaves exactly as before.
const INDEX_THRESHOLD: usize = 2_000;

/// Bigram posting lists over the lowercase name/value/path of every candidate. A query of at
/// least two bytes that occurs in any field must contain the query's first two bytes in that
/// field, so its posting list is a superset of the real substring matches; the exact `contains`
/// checks in `score_substring` still decide the final set and ranking.
#[derive(Debug, Clone, Default)]
struct SubstringIndex {
    bigrams: HashMap<u16, Vec<u32>>,
    active: bool,
}

impl SubstringIndex {
    fn build(items: &[SuggestionItem]) -> Self {
        if items.len() < INDEX_THRESHOLD {
            return Self::default();
        }
        let mut bigrams: HashMap<u16, Vec<u32>> = HashMap::new();
        let mut seen: HashSet<u16> = HashSet::new();
        for (index, item) in items.iter().enumerate() {
            seen.clear();
            for field in [&item.name_lower, &item.value_lower, &item.path_lower] {
                let bytes = field.as_bytes();
                if bytes.len() < 2 {
                    continue;
                }
                for pair in bytes.windows(2) {
                    let key = u16::from(pair[0]) << 8 | u16::from(pair[1]);
                    if seen.insert(key) {
                        bigrams.entry(key).or_default().push(index as u32);
                    }
                }
            }
        }
        Self {
            bigrams,
            active: true,
        }
    }

    /// A sorted, deduplicated superset of indices that can match `query_lower`, or `None` when the
    /// index is inactive (small set) or the query is too short to have a bigram.
    fn candidates(&self, query_lower: &str) -> Option<Vec<u32>> {
        if !self.active {
            return None;
        }
        let bytes = query_lower.as_bytes();
        if bytes.len() < 2 {
            return None;
        }
        let key = u16::from(bytes[0]) << 8 | u16::from(bytes[1]);
        let mut indices = self.bigrams.get(&key)?.clone();
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }
}

/// A bounded, precomputed set of completion candidates.
#[derive(Debug, Clone)]
pub struct SuggestionSet {
    pub kind: SuggestionKind,
    pub items: Vec<SuggestionItem>,
    index: SubstringIndex,
}

impl SuggestionSet {
    pub fn new(kind: SuggestionKind, values: Vec<String>) -> Self {
        Self::with_items(
            kind,
            values
                .into_iter()
                .map(|value| SuggestionItem::new(value, String::new()))
                .collect(),
        )
    }

    /// Build a set and its substring index together so the index always matches `items`.
    fn with_items(kind: SuggestionKind, items: Vec<SuggestionItem>) -> Self {
        let index = SubstringIndex::build(&items);
        Self { kind, items, index }
    }

    /// Fast substring ranking first; fuzzy matching only fills remaining slots.
    pub fn filter(&self, query: &str, limit: usize) -> Vec<SuggestionItem> {
        self.filter_with(query, limit, true)
    }

    /// `use_index = false` is the brute-force reference used by tests to prove the index returns
    /// exactly the same items in exactly the same order.
    fn filter_with(&self, query: &str, limit: usize, use_index: bool) -> Vec<SuggestionItem> {
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
        let mut scored: Vec<(usize, i64)> = Vec::new();
        let mut seen = HashSet::new();
        let candidates = if use_index {
            self.index.candidates(&query_lower)
        } else {
            None
        };
        if let Some(indices) = candidates {
            self.score_substring(
                indices
                    .into_iter()
                    .map(|index| (index as usize, &self.items[index as usize])),
                &query_lower,
                &mut scored,
                &mut seen,
            );
        } else {
            self.score_substring(
                self.items.iter().enumerate(),
                &query_lower,
                &mut scored,
                &mut seen,
            );
        }
        if scored.len() < limit {
            let tokens: Vec<&str> = query_lower.split_whitespace().collect();
            for (index, item) in self.items.iter().enumerate() {
                if seen.contains(item.value.as_str()) {
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
                if matched && seen.insert(item.value.as_str()) {
                    scored.push((index, total));
                }
            }
        }
        scored.sort_unstable_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| {
                    self.items[left.0]
                        .path
                        .is_empty()
                        .cmp(&self.items[right.0].path.is_empty())
                })
                .then_with(|| self.items[left.0].value.cmp(&self.items[right.0].value))
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(index, _)| self.items[index].clone())
            .collect()
    }

    /// Apply the exact substring tiers and dedup to the supplied item order. Callers pass either
    /// the index's candidate superset (ascending index order) or every item; the result is the
    /// same because both contain all real matches in original order.
    fn score_substring<'a>(
        &self,
        items: impl Iterator<Item = (usize, &'a SuggestionItem)>,
        query_lower: &str,
        scored: &mut Vec<(usize, i64)>,
        seen: &mut HashSet<&'a str>,
    ) {
        for (index, item) in items {
            let mut score = if item.name_lower == query_lower {
                100_000
            } else if item.name_lower.contains(query_lower) {
                50_000
            } else if item.value_lower.contains(query_lower) {
                20_000
            } else if item.path_lower.contains(query_lower) {
                5_000
            } else {
                continue;
            };
            // A bare metadata type means "everything of this kind", which is far slower and
            // broader than the concrete component the user is usually after. Keep exact type
            // names at the top, but rank partial type matches below real components.
            if item.path == "type" && item.name_lower != query_lower {
                score -= 45_000;
            }
            if seen.insert(item.value.as_str()) {
                scored.push((index, score));
            }
        }
    }
}

/// Apex classes from the project index, used by the test-run search screen.
pub fn test_suggestions(all: &SuggestionSet, project: Option<&SfProject>) -> SuggestionSet {
    let mut remaining_bytes: u64 = 8 * 1024 * 1024;
    SuggestionSet::with_items(
        SuggestionKind::Transfer,
        all.items
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
                let path = project.root.join(&item.path);
                let Ok(metadata) = std::fs::metadata(&path) else {
                    return false;
                };
                if metadata.len() > remaining_bytes || metadata.len() > 1024 * 1024 {
                    return false;
                }
                let Ok(source) = std::fs::read_to_string(&path) else {
                    return false;
                };
                remaining_bytes = remaining_bytes.saturating_sub(metadata.len());
                source.to_ascii_lowercase().contains("@istest")
            })
            .cloned()
            .collect(),
    )
}

/// Fuzzy index for the Create wizard's grouped type picker. The catalog lives in `sf::create`;
/// this module only owns the ranking set so the shared search controller can rank it off-thread.
pub fn create_suggestions() -> SuggestionSet {
    SuggestionSet::with_items(
        SuggestionKind::Create,
        super::create::ALL_TYPES
            .iter()
            .map(|kind| {
                let detail = if kind.interactive_only() {
                    format!("{} · External CLI (copy command)", kind.group().label())
                } else {
                    format!("{} · {}/", kind.group().label(), kind.folder())
                };
                SuggestionItem::new(kind.label(), detail)
            })
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
    SuggestionSet::with_items(SuggestionKind::Transfer, items)
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
    // Decomposed custom fields live at objects/<Object>/fields/<Field>.field-meta.xml. The org
    // identity is CustomField:Object.Field, not the bare field name.
    if file.contains(".field-meta.xml")
        && let Some(object) = part_after(&parts, "objects")
    {
        return Some((format!("CustomField:{object}.{stem}"), path.to_owned()));
    }
    // Lightning bundles are directories: lwc/<bundle>/... and aura/<bundle>/....
    for marker in ["lwc", "aura"] {
        if let Some(bundle) = part_after(&parts, marker) {
            return Some((
                format!("LightningComponentBundle:{bundle}"),
                path.to_owned(),
            ));
        }
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

fn part_after<'a>(parts: &[&'a str], marker: &str) -> Option<&'a str> {
    parts
        .iter()
        .position(|part| *part == marker)
        .and_then(|index| parts.get(index + 1).copied())
}

/// Metadata types the org browser offers. Browsing the whole org for arbitrary types is slow, so
/// this curated list keeps the picker focused on the components people retrieve most often.
pub const ORG_BROWSE_TYPES: &[&str] = &[
    "ApexClass",
    "ApexTrigger",
    "Flow",
    "PermissionSet",
    "CustomObject",
    "LightningComponentBundle",
];

/// One component reported by `sf org list metadata --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgComponent {
    pub metadata_type: String,
    pub full_name: String,
}

impl OrgComponent {
    /// The `Type:fullName` identity used for tooltips, deduplication and `--metadata` arguments.
    pub fn identity(&self) -> String {
        format!("{}:{}", self.metadata_type, self.full_name)
    }
}

/// Parse `sf org list metadata --json` defensively.
pub fn parse_org_components(value: &Value, fallback_type: &str) -> Vec<OrgComponent> {
    let result = value.get("result").unwrap_or(value);
    let mut rows: Vec<&Value> = Vec::new();
    if let Some(items) = result.as_array() {
        rows.extend(items);
    } else if let Some(map) = result.as_object() {
        for key in ["metadata", "components", "items", "records", "result"] {
            if let Some(items) = map.get(key).and_then(Value::as_array) {
                rows.extend(items);
            }
        }
        if rows.is_empty() {
            rows.push(result);
        }
    }
    let mut seen = HashSet::new();
    let mut components = Vec::new();
    for row in rows {
        let Some(full_name) = string_field(row, "fullName")
            .or_else(|| string_field(row, "fullname"))
            .or_else(|| string_field(row, "name"))
        else {
            continue;
        };
        if has_secret_material(&full_name) {
            continue;
        }
        let metadata_type = string_field(row, "type")
            .or_else(|| string_field(row, "metadataType"))
            .or_else(|| string_field(row, "metadata_type"))
            .unwrap_or_else(|| fallback_type.to_owned());
        if has_secret_material(&metadata_type) {
            continue;
        }
        let component = OrgComponent {
            metadata_type,
            full_name,
        };
        if seen.insert(component.identity()) {
            components.push(component);
        }
    }
    components.sort_by_key(|component| component.identity());
    components
}

fn has_secret_material(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("force://") || SENSITIVE_FIELDS.iter().any(|secret| lower.contains(secret))
}

/// The `Type:Name` identities that actually exist in the current project.
pub fn local_identities(project: Option<&SfProject>) -> HashSet<String> {
    let mut identities = HashSet::new();
    let Some(project) = project else {
        return identities;
    };
    for path in &project.source_files {
        if let Some((identity, _)) = component_from_path(path) {
            identities.insert(identity);
        }
    }
    for manifest in &project.manifests {
        let Ok(xml) = std::fs::read_to_string(project.root.join(manifest)) else {
            continue;
        };
        for component in super::project::parse_manifest_components(&xml) {
            identities.insert(component);
        }
    }
    identities
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_components_out_rank_type_only_matches_and_keep_paths() {
        let set = SuggestionSet::with_items(
            SuggestionKind::Transfer,
            vec![
                SuggestionItem::new("ApexClass", "type"),
                SuggestionItem::new("ApexClass:Demo", "force-app/main/default/classes/Demo.cls"),
            ],
        );
        let hits = set.filter("Demo", 5);
        assert_eq!(hits[0].value, "ApexClass:Demo");
        assert!(hits[0].path.ends_with("Demo.cls"));
        assert!(set.filter("zzzz", 5).is_empty());
    }

    #[test]
    fn partial_type_matches_rank_below_concrete_components() {
        let set = SuggestionSet::with_items(
            SuggestionKind::Transfer,
            vec![
                SuggestionItem::new("ApexClass", "type"),
                SuggestionItem::new("ApexClass:Demo", "force-app/main/default/classes/Demo.cls"),
            ],
        );
        let hits = set.filter("apex", 5);
        assert_eq!(hits[0].value, "ApexClass:Demo");
        // Typing the complete type name still offers the whole type first.
        let hits = set.filter("ApexClass", 5);
        assert_eq!(hits[0].value, "ApexClass");
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
        let all = SuggestionSet::with_items(
            SuggestionKind::Transfer,
            vec![SuggestionItem::new("ApexClass:Helper", relative)],
        );
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
    fn indexed_filter_matches_full_scan_exactly() {
        let mut items: Vec<SuggestionItem> = Vec::new();
        // Well above INDEX_THRESHOLD so the index is active, with duplicates, a type-only entry,
        // UTF-8 and short names to exercise the fallback and dedup paths.
        for index in 0..(INDEX_THRESHOLD + 400) {
            let kind = ["ApexClass", "Flow", "CustomObject"][index % 3];
            let name = format!("DemoController{index}");
            items.push(SuggestionItem::new(
                format!("{kind}:{name}"),
                format!("force-app/main/default/classes/{name}.cls"),
            ));
            if index % 7 == 0 {
                items.push(SuggestionItem::new(kind, "type"));
            }
        }
        items.push(SuggestionItem::new(
            "ApexClass:Café",
            "force-app/classes/Café.cls",
        ));
        items.push(SuggestionItem::new(
            "ApexClass:A",
            "force-app/classes/A.cls",
        ));
        let set = SuggestionSet::with_items(SuggestionKind::Transfer, items);
        assert!(set.index.active, "test set must be large enough to index");

        for query in [
            "Demo",
            "apex",
            "ApexClass",
            "Flow",
            "Controller",
            "test",
            "a",
            "Café",
            "zzzz",
            "Demo Controller",
            "  DEMO  ",
            "",
            "A",
        ] {
            for limit in [1, 5, 20, 200] {
                let indexed = set.filter(query, limit);
                let scanned = set.filter_with(query, limit, false);
                assert_eq!(
                    indexed, scanned,
                    "indexed result diverged for query {query:?} limit {limit}"
                );
            }
        }
    }

    #[test]
    fn create_suggestions_cover_the_grouped_catalog() {
        let set = create_suggestions();
        assert_eq!(set.items.len(), super::super::create::ALL_TYPES.len());
        assert!(
            set.items
                .iter()
                .all(|item| !item.path.is_empty() && !item.value.is_empty())
        );
        assert!(
            set.items
                .iter()
                .any(|item| item.path.contains("External CLI"))
        );
        let hits = set.filter("apex", 10);
        assert!(hits.iter().any(|item| item.value == "Apex Class"));
        assert!(
            set.filter("lwc", 10)
                .iter()
                .any(|item| item.value == "Lightning Web Component")
        );
    }

    #[test]
    fn org_components_parse_wrapped_and_bare_arrays_and_dedupe() {
        let wrapped = serde_json::json!({"status": 0, "result": [
            {"type": "ApexClass", "fullName": "Demo"},
            {"type": "ApexClass", "fullName": "Demo"},
            {"fullName": "OrgOnly"},
            {"name": "Demo Flow", "type": "Flow"},
            {"fullName": ""},
            {"type": "ApexClass", "fullName": "force://secret"}
        ]});
        let parsed = parse_org_components(&wrapped, "ApexClass");
        let identities: Vec<String> = parsed.iter().map(OrgComponent::identity).collect();
        assert_eq!(
            identities,
            vec!["ApexClass:Demo", "ApexClass:OrgOnly", "Flow:Demo Flow",]
        );

        let object_wrapped = serde_json::json!({"result": {"metadata": [
            {"type": "Flow", "fullName": "Only"}
        ]}});
        assert_eq!(
            parse_org_components(&object_wrapped, "Flow")
                .first()
                .map(OrgComponent::identity),
            Some("Flow:Only".to_owned())
        );
    }

    #[test]
    fn local_identities_include_bundles_and_decomposed_fields() {
        let project = SfProject {
            root: std::path::PathBuf::from("."),
            package_directories: Vec::new(),
            source_api_version: None,
            manifests: Vec::new(),
            source_files: vec![
                "force-app/main/default/lwc/demoCard/demoCard.js".to_owned(),
                "force-app/main/default/aura/demoCmp/demoCmp.cmp".to_owned(),
                "force-app/main/default/objects/Account/fields/Score__c.field-meta.xml".to_owned(),
            ],
        };
        let identities = local_identities(Some(&project));
        assert!(identities.contains("LightningComponentBundle:demoCard"));
        assert!(identities.contains("LightningComponentBundle:demoCmp"));
        assert!(identities.contains("CustomField:Account.Score__c"));
    }
}
