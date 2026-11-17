//! Offline snapshot of the public Salesforce CLI command reference.
//!
//! The catalog parses the vendored `data/commands.json` once and offers fuzzy search plus
//! helpers for building a command preview. There is deliberately no live `sf commands --json`
//! refresh or cache: that probe was slow and noisy, and the bundled snapshot is enough for
//! discovering commands and their flags.

use std::cell::RefCell;
use std::sync::OnceLock;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::Deserialize;

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

#[derive(Debug, Clone)]
pub struct Catalog {
    pub commands: Vec<CommandSpec>,
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

static SNAPSHOT: OnceLock<Catalog> = OnceLock::new();

/// The parsed snapshot, shared by the key handler and the renderer so the large JSON is
/// decoded only once.
pub fn catalog() -> &'static Catalog {
    SNAPSHOT.get_or_init(snapshot)
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
    }
}

/// The public command reference uses `org:list`; the CLI accepts `org list`.
fn normalize_id(id: &str) -> String {
    id.replace(':', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Group commands by the first word of their id. Capitalization is char-safe so a
/// non-ASCII prefix cannot be sliced mid-codepoint.
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

/// A fuzzy-search result over the snapshot.
#[derive(Debug, Clone)]
pub struct SearchHit<'a> {
    pub command: &'a CommandSpec,
    pub score: i64,
    pub matched: Option<Vec<usize>>,
}

thread_local! {
    static MATCHER: RefCell<SkimMatcherV2> = RefCell::new(SkimMatcherV2::default().ignore_case());
}

/// Rank commands by fuzzy score over id and summary. An empty query returns every command
/// in id order.
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
    fn snapshot_parses_the_bundled_reference() {
        let catalog = catalog();
        assert!(catalog.commands.len() > 250);
        assert!(
            catalog
                .commands
                .iter()
                .all(|command| !command.category.is_empty())
        );
    }

    #[test]
    fn known_commands_are_present() {
        let catalog = catalog();
        assert!(catalog.command("project retrieve start").is_some());
        assert!(catalog.command("apex run test").is_some());
        assert!(
            catalog
                .command("project retrieve start")
                .is_some_and(
                    |command| command.invocation() == "sf project retrieve start"
                        && command.examples.first().is_some_and(|example| example
                            .command
                            .starts_with("sf project retrieve start"))
                )
        );
    }

    #[test]
    fn search_finds_commands_by_id_and_rejects_nonsense() {
        let hits = search(&catalog().commands, "org list", 5);
        assert_eq!(
            hits.first().map(|hit| hit.command.id.as_str()),
            Some("org list")
        );
        assert!(search(&catalog().commands, "zzzznotacommand", 5).is_empty());
    }

    #[test]
    fn category_capitalization_handles_non_ascii_prefixes() {
        assert_eq!(category_for("éclair run"), "Éclair");
        assert_eq!(category_for("org:list"), "Orgs");
    }
}
