//! Generic command catalog: live `sf commands --json` data with an offline snapshot fallback.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::cell::RefCell;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A flag accepted by an SF command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub name: String,
    pub description: String,
    pub value_name: Option<String>,
    pub required: bool,
}

/// An example from command help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    pub command: String,
    pub description: String,
}

/// One catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: String,
    pub summary: String,
    pub description: String,
    pub category: String,
    pub flags: Vec<Flag>,
    pub examples: Vec<Example>,
}

impl CommandSpec {
    pub fn invocation(&self) -> String {
        format!("sf {}", self.id)
    }

    pub fn find_flag(&self, name: &str) -> Option<&Flag> {
        let needle = name.trim_start_matches('-');
        self.flags.iter().find(|flag| flag.name == needle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    Live,
    Cached,
    Snapshot,
}

impl CatalogSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live commands",
            Self::Cached => "cached commands",
            Self::Snapshot => "bundled snapshot",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub commands: Vec<CommandSpec>,
    pub source: CatalogSource,
}

impl Catalog {
    pub fn categories(&self) -> Vec<String> {
        let mut categories: Vec<String> = self
            .commands
            .iter()
            .map(|command| command.category.clone())
            .collect();
        categories.sort();
        categories.dedup();
        categories
    }

    pub fn command(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|command| command.id == id)
    }
}

/// Name of the optional catalog cache beside config.toml.
pub const CACHE_FILE: &str = "catalog-cache.json";

#[derive(Serialize, Deserialize)]
struct CacheFile {
    saved_at: u64,
    json: String,
}

pub fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CACHE_FILE)
}

/// Read a cached catalog only when it is newer than `max_age`.
pub fn read_cache(path: &Path, max_age: Duration) -> Option<Vec<CommandSpec>> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > max_age {
        return None;
    }
    let cache: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    parse_commands_json(&cache.json).ok()
}

/// Store the raw `sf commands --json` response for the next launch.
pub fn write_cache(path: &Path, raw_json: &str) -> std::io::Result<()> {
    let saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cache = CacheFile {
        saved_at,
        json: raw_json.to_owned(),
    };
    crate::fsutil::write_private(path, &serde_json::to_vec(&cache).unwrap_or_default())
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("SF command JSON had no recognizable command list")]
    NoCommandList,
}

#[derive(Debug, Deserialize)]
struct SnapshotCommand {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    flags: Vec<(String, String)>,
    #[serde(default)]
    examples: Vec<(String, String)>,
}

/// Parse the compact bundled snapshot generated from the public SF command reference.
pub fn snapshot() -> Catalog {
    let snapshot: Vec<SnapshotCommand> =
        serde_json::from_str(include_str!("../../data/commands.json")).unwrap_or_default();
    Catalog {
        commands: snapshot
            .into_iter()
            .map(|command| CommandSpec {
                id: normalize_id(&command.id),
                summary: command.summary,
                description: command.description,
                category: if command.category.is_empty() {
                    category_for(&normalize_id(&command.id))
                } else {
                    command.category
                },
                flags: command
                    .flags
                    .into_iter()
                    .map(|(name, description)| Flag {
                        name,
                        description,
                        value_name: None,
                        required: false,
                    })
                    .collect(),
                examples: command
                    .examples
                    .into_iter()
                    .map(|(command, description)| Example {
                        command,
                        description,
                    })
                    .collect(),
            })
            .collect(),
        source: CatalogSource::Snapshot,
    }
}

/// Parse `sf commands --json` (also accepts a bare array and the legacy shape).
pub fn parse_commands_json(text: &str) -> Result<Vec<CommandSpec>, CatalogError> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| CatalogError::NoCommandList)?;
    let list = value
        .get("result")
        .cloned()
        .or_else(|| {
            if value.is_array() {
                Some(value.clone())
            } else {
                None
            }
        })
        .filter(Value::is_array)
        .ok_or(CatalogError::NoCommandList)?;
    let mut commands = Vec::new();
    for item in list.as_array().into_iter().flatten() {
        let Some(raw_id) = string_field(item, "id").or_else(|| string_field(item, "command"))
        else {
            continue;
        };
        let id = normalize_id(&raw_id);
        commands.push(CommandSpec {
            category: string_field(item, "category").unwrap_or_else(|| category_for(&id)),
            id,
            summary: string_field(item, "summary").unwrap_or_default(),
            description: string_field(item, "description").unwrap_or_default(),
            flags: parse_flags(item.get("flags")),
            examples: parse_examples(item.get("examples")),
        });
    }
    if commands.is_empty() {
        return Err(CatalogError::NoCommandList);
    }
    commands.sort_by(|left, right| left.id.cmp(&right.id));
    commands.dedup_by(|left, right| left.id == right.id);
    Ok(commands)
}

/// The public command reference uses `org:list`; the CLI accepts `org list`.
fn normalize_id(id: &str) -> String {
    id.replace(':', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn parse_flags(value: Option<&Value>) -> Vec<Flag> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut flags = Vec::new();
    match value {
        Value::Array(items) => {
            for item in items {
                let flag = match item {
                    Value::Object(_) => Flag {
                        name: string_field(item, "name")
                            .or_else(|| string_field(item, "long"))
                            .unwrap_or_default(),
                        description: string_field(item, "description")
                            .or_else(|| string_field(item, "summary"))
                            .unwrap_or_default(),
                        value_name: string_field(item, "type").filter(|kind| kind != "boolean"),
                        required: item
                            .get("required")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                    Value::String(name) => Flag {
                        name: name.clone(),
                        description: String::new(),
                        value_name: None,
                        required: false,
                    },
                    Value::Array(parts) => Flag {
                        name: parts
                            .first()
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        description: parts
                            .get(1)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        value_name: None,
                        required: false,
                    },
                    _ => continue,
                };
                if !flag.name.is_empty() {
                    flags.push(flag);
                }
            }
        }
        Value::Object(map) => {
            for (name, item) in map {
                flags.push(Flag {
                    name: name.clone(),
                    description: item.as_str().unwrap_or_default().to_owned(),
                    value_name: None,
                    required: false,
                });
            }
        }
        _ => {}
    }
    flags
}

fn parse_examples(value: Option<&Value>) -> Vec<Example> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::Array(parts) => Some(Example {
                command: parts
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                description: parts
                    .get(1)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            Value::Object(_) => Some(Example {
                command: string_field(item, "command").unwrap_or_default(),
                description: string_field(item, "description").unwrap_or_default(),
            }),
            _ => None,
        })
        .filter(|example| !example.command.is_empty())
        .collect()
}

/// Parse flags from `sf <command> --help` output for the detail pane.
pub fn parse_help_flags(help: &str) -> Vec<Flag> {
    let mut flags = Vec::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        let Some(long) = trimmed
            .split_whitespace()
            .find(|word| word.starts_with("--"))
        else {
            continue;
        };
        let name = long
            .trim_start_matches('-')
            .trim_end_matches(|character: char| !character.is_alphanumeric() && character != '-')
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let value_name = trimmed
            .split_whitespace()
            .find(|word| word.starts_with('<') || word.starts_with('['))
            .map(|word| {
                word.trim_matches(|character: char| "<>[]".contains(character))
                    .to_owned()
            });
        let lower = trimmed.to_ascii_lowercase();
        let description = trimmed.split_once("  ").map_or(String::new(), |(_, rest)| {
            rest.trim()
                .split("  ")
                .next()
                .unwrap_or_default()
                .to_owned()
        });
        flags.push(Flag {
            name,
            description,
            value_name,
            required: lower.contains("(required)") || lower.contains("[required]"),
        });
    }
    flags
}

/// Group commands by the first word of their id.
pub fn category_for(id: &str) -> String {
    let prefix = id.split([':', ' ']).next().unwrap_or_default();
    let known = match prefix {
        "agent" => "Agents",
        "api" => "API",
        "apex" => "Apex",
        "alias" => "Aliases",
        "auth" | "login" | "logout" => "Auth",
        "config" => "Config",
        "data" | "tree" => "Data",
        "deploy" => "Deploy",
        "docs" => "Docs",
        "flow" => "Flows",
        "functions" => "Functions",
        "limits" => "Limits",
        "mcp" => "MCP",
        "object" | "sobject" | "customobject" => "Objects",
        "org" | "display" | "list" => "Orgs",
        "package" => "Packages",
        "plugin" | "plugins" => "Plugins",
        "project" => "Project",
        "retrieve" => "Retrieve",
        "scaffold" | "generate" => "Scaffold",
        "scratch" => "Scratch Orgs",
        "source" | "force" => "Source",
        "telemetry" => "Telemetry",
        "test" => "Testing",
        "update" => "Update",
        "user" => "Users",
        "version" => "Version",
        _ => prefix,
    };
    if known.is_empty() {
        "Other".to_owned()
    } else {
        let name = known.to_owned();
        let mut characters = name.chars();
        match characters.next() {
            Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
            None => "Other".to_owned(),
        }
    }
}

/// A fuzzy-search result, including character indices in the command id for highlighting.
#[derive(Debug, Clone)]
pub struct SearchHit<'a> {
    pub command: &'a CommandSpec,
    pub score: i64,
    pub matched: Option<Vec<usize>>,
}

thread_local! {
    static MATCHER: RefCell<SkimMatcherV2> = RefCell::new(SkimMatcherV2::default().ignore_case());
}

/// Rank commands by fuzzy score over id and summary.
pub fn search<'a>(commands: &'a [CommandSpec], query: &str, limit: usize) -> Vec<SearchHit<'a>> {
    let query = query.trim();
    let mut hits: Vec<SearchHit<'a>> = commands
        .iter()
        .filter_map(|command| {
            if query.is_empty() {
                return Some(SearchHit {
                    command,
                    score: 0,
                    matched: None,
                });
            }
            let haystack = format!("{} {}", command.id, command.summary);
            let (score, indices) =
                MATCHER.with(|matcher| matcher.borrow().fuzzy_indices(&haystack, query))?;
            let matched = if indices.iter().all(|&index| index < command.id.len()) {
                Some(indices)
            } else {
                None
            };
            Some(SearchHit {
                command,
                score,
                matched,
            })
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.command.id.cmp(&right.command.id))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_full_catalog() {
        let catalog = snapshot();
        assert!(catalog.commands.len() > 250);
        assert!(catalog.command("org list").is_some());
        assert!(catalog.command("project deploy start").is_some());
        assert!(
            catalog
                .commands
                .iter()
                .all(|command| !command.category.is_empty())
        );
    }

    #[test]
    fn live_command_json_with_result_is_parsed() {
        let json = r#"{"status":0,"result":[{"id":"org list","summary":"List orgs","category":"Orgs","flags":[{"name":"json","description":"JSON output","type":"boolean","required":false}],"examples":[["sf org list","List them"]]}]}"#;
        let commands = parse_commands_json(json).expect("parse");
        assert_eq!(commands[0].id, "org list");
        assert_eq!(commands[0].flags[0].name, "json");
        assert_eq!(commands[0].examples[0].description, "List them");
    }

    #[test]
    fn live_command_json_without_result_is_rejected() {
        assert!(parse_commands_json("{\"status\":1}").is_err());
    }

    #[test]
    fn help_flags_extract_names_values_and_required_markers() {
        let help = "FLAGS\n  -o, --target-org <value>   Username or alias (required)\n  --json                     Format output as json.\n";
        let flags = parse_help_flags(help);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].name, "target-org");
        assert_eq!(flags[0].value_name.as_deref(), Some("value"));
        assert!(flags[0].required);
        assert_eq!(flags[1].name, "json");
        assert!(!flags[1].required);
    }

    #[test]
    fn catalog_cache_round_trips_and_expires() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = cache_path(dir.path());
        let raw = r#"{"result":[{"id":"org list","summary":"List orgs"}]}"#;
        write_cache(&path, raw).expect("write cache");
        let commands = read_cache(&path, Duration::from_secs(60)).expect("fresh cache");
        assert_eq!(commands[0].id, "org list");
        assert!(read_cache(&path, Duration::ZERO).is_none());
    }

    #[test]
    fn fuzzy_search_ranks_exact_id_first_and_returns_indices() {
        let catalog = snapshot();
        let hits = search(&catalog.commands, "org list", 5);
        assert_eq!(hits[0].command.id, "org list");
        assert!(hits[0].matched.is_some());
        assert!(search(&catalog.commands, "zzzznotacommand", 5).is_empty());
    }

    #[test]
    fn category_capitalization_handles_non_ascii_prefixes() {
        assert_eq!(category_for("éclair run"), "Éclair");
    }
}
