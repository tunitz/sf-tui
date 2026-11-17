//! Best-effort, per-project persistence of org context and Apex test history.
//!
//! Each Salesforce project gets its own directory under the user config directory, keyed by a
//! stable hash of the canonical project root. Files are written atomically with owner-only
//! permissions by [`crate::fsutil::write_private`]. Every read is defensive: a missing, corrupt or
//! unreadable file becomes an empty default and a malformed entry is skipped, never fatal. Only
//! already-sanitized data is stored — no raw CLI output, tokens or auth URLs.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fsutil::write_private;
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::test_results::TestRun;

/// Newest-first cap for saved Apex runs. Older runs are dropped when the cap is exceeded.
pub const MAX_SAVED_RUNS: usize = 50;

const PROJECTS_DIR: &str = "projects";
const ORGS_FILE: &str = "orgs.json";
const TEST_RUNS_FILE: &str = "test-runs.json";
const SF_VERSION_FILE: &str = "sf-version.json";

/// Current wall-clock time in milliseconds since the Unix epoch (0 if the clock is unavailable).
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Directory holding the persisted state for one project.
pub fn state_dir(root: &Path, config_dir: &Path) -> PathBuf {
    config_dir.join(PROJECTS_DIR).join(project_key(root))
}

/// Stable, deterministic 16-hex-digit key for an absolute project root.
///
/// Paths are canonicalized when possible (resolving `.`/`..` and symlinks) and otherwise made
/// absolute; non-UTF-8 bytes are lossily converted. FNV-1a is implemented locally so the value
/// never changes with the compiler version, unlike `DefaultHasher`.
fn project_key(root: &Path) -> String {
    let absolute_path = absolute(root);
    let text = absolute_path.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Read a JSON document, returning `None` for any missing or unreadable file or malformed JSON.
fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Deserialize every element of a JSON array, skipping malformed entries.
fn load_array<T: for<'de> Deserialize<'de>>(value: Option<&Value>) -> Vec<T> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

// ----- Global Salesforce CLI version --------------------------------------------------

/// Machine-wide CLI version cache. The version rarely changes, so a warm cache removes the
/// `sf version --json` startup call entirely. Purely informational: staleness is never an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SfVersionCache {
    pub version: String,
    pub saved_at: u64,
}

impl SfVersionCache {
    pub fn load(config_dir: &Path) -> Self {
        let Some(value) = read_json(&config_dir.join(SF_VERSION_FILE)) else {
            return Self::default();
        };
        let version = value
            .get("version")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .unwrap_or("")
            .to_owned();
        let saved_at = value.get("saved_at").and_then(Value::as_u64).unwrap_or(0);
        Self { version, saved_at }
    }

    pub fn save(&self, config_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_private(&config_dir.join(SF_VERSION_FILE), &bytes)
    }

    pub fn is_present(&self) -> bool {
        !self.version.is_empty()
    }
}

// ----- Org cache ---------------------------------------------------------------------

/// The last known org list plus one cached detail per username.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgCache {
    pub orgs: Vec<OrgSummary>,
    pub details: HashMap<String, OrgDetail>,
    pub saved_at: u64,
}

impl OrgCache {
    pub fn load(root: &Path, config_dir: &Path) -> Self {
        let Some(value) = read_json(&state_dir(root, config_dir).join(ORGS_FILE)) else {
            return Self::default();
        };
        let mut orgs: Vec<OrgSummary> = load_array(value.get("orgs"));
        orgs.retain(|org| !org.username.is_empty());
        let mut cache = Self {
            orgs,
            details: value
                .get("details")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(username, detail)| {
                            serde_json::from_value(detail.clone())
                                .ok()
                                .map(|detail| (username.clone(), detail))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            saved_at: value.get("saved_at").and_then(Value::as_u64).unwrap_or(0),
        };
        cache.prune_details();
        cache
    }

    /// Persist the cache, dropping details for orgs no longer in the list.
    pub fn save(&self, root: &Path, config_dir: &Path) -> io::Result<()> {
        let mut pruned = self.clone();
        pruned.saved_at = now_ms();
        pruned.prune_details();
        let bytes = serde_json::to_vec_pretty(&pruned).map_err(io::Error::other)?;
        write_private(&state_dir(root, config_dir).join(ORGS_FILE), &bytes)
    }

    /// Empty the in-memory cache.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn prune_details(&mut self) {
        // An empty list means "never loaded", not "the user has no orgs": keep details so a
        // detail fetched before the list arrives is not silently discarded.
        if self.orgs.is_empty() {
            return;
        }
        let usernames: HashSet<&str> = self.orgs.iter().map(|org| org.username.as_str()).collect();
        self.details
            .retain(|username, _| usernames.contains(username.as_str()));
    }
}

// ----- Apex test history -------------------------------------------------------------

/// One finished Apex run with enough context to render its full result later.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTestRun {
    pub id: String,
    pub finished_at: u64,
    pub org: Option<String>,
    pub command: String,
    pub run: TestRun,
}

/// Newest-first history of finished Apex runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestHistory {
    pub runs: Vec<SavedTestRun>,
}

impl TestHistory {
    pub fn load(root: &Path, config_dir: &Path) -> Self {
        let value = read_json(&state_dir(root, config_dir).join(TEST_RUNS_FILE));
        let mut runs: Vec<SavedTestRun> =
            load_array(value.as_ref().and_then(|value| value.get("runs")));
        // A defaultable entry with no id/command is a truncated or foreign row, not a usable run.
        runs.retain(|run| !run.id.is_empty() && !run.command.is_empty());
        let mut history = Self { runs };
        history.runs.truncate(MAX_SAVED_RUNS);
        history
    }

    /// Persist the newest [`MAX_SAVED_RUNS`] runs.
    pub fn save(&mut self, root: &Path, config_dir: &Path) -> io::Result<()> {
        self.runs.truncate(MAX_SAVED_RUNS);
        let bytes = serde_json::to_vec_pretty(&self).map_err(io::Error::other)?;
        write_private(&state_dir(root, config_dir).join(TEST_RUNS_FILE), &bytes)
    }

    /// Insert a finished run, dropping any prior entry with the same id and the oldest rows.
    pub fn push(&mut self, run: SavedTestRun) {
        self.runs.retain(|saved| saved.id != run.id);
        self.runs.insert(0, run);
        self.runs.truncate(MAX_SAVED_RUNS);
    }

    /// Remove one run by id. Returns true when a row was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.runs.len();
        self.runs.retain(|run| run.id != id);
        self.runs.len() != before
    }

    /// Empty the in-memory history.
    pub fn clear(&mut self) {
        self.runs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_org(username: &str, is_default: bool) -> OrgSummary {
        OrgSummary {
            username: username.to_owned(),
            aliases: vec![format!("alias-{username}")],
            org_id: "00D000000000001".to_owned(),
            is_default,
            connected_status: "Connected".to_owned(),
            instance_url: "https://example.invalid.my.salesforce.com".to_owned(),
            ..OrgSummary::default()
        }
    }

    fn sample_run(id: &str) -> SavedTestRun {
        SavedTestRun {
            id: id.to_owned(),
            finished_at: 1_700_000_000_000,
            org: Some("alias-demo".to_owned()),
            command: "sf apex run test".to_owned(),
            run: TestRun {
                id: "08N000000000001".to_owned(),
                at: 1_700_000_000,
                command: "sf apex run test".to_owned(),
                outcome: "Passed".to_owned(),
                tests_run: 2,
                passing: 2,
                failing: 0,
                skipped: 0,
                coverage: "81%".to_owned(),
                tests: Vec::new(),
                failures: Vec::new(),
                coverages: Vec::new(),
            },
        }
    }

    #[test]
    fn project_key_is_stable_for_ascii_and_non_ascii_paths() {
        let ascii = Path::new("/home/user/projects/my-app");
        let non_ascii = Path::new("/home/user/projets/naïve-保险");
        for path in [ascii, non_ascii] {
            let first = project_key(path);
            assert_eq!(first.len(), 16, "key must be 16 hex digits");
            assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(first, project_key(path), "hash must be deterministic");
        }
        assert_ne!(project_key(ascii), project_key(non_ascii));
    }

    #[test]
    fn state_dir_is_scoped_per_project() {
        let config = Path::new("/home/user/.config/sftui");
        let first = state_dir(Path::new("/work/alpha"), config);
        let second = state_dir(Path::new("/work/beta"), config);
        assert_eq!(
            first.parent(),
            Some(Path::new("/home/user/.config/sftui/projects"))
        );
        assert_ne!(first, second);
    }

    #[test]
    fn org_cache_round_trips_and_prunes_details() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = Path::new("/work/project");
        let mut cache = OrgCache {
            orgs: vec![sample_org("demo@example.invalid", true)],
            details: HashMap::new(),
            saved_at: 0,
        };
        cache.details.insert(
            "demo@example.invalid".to_owned(),
            OrgDetail {
                fields: vec![("Id".to_owned(), "00D000000000001".to_owned())],
                instance_url: "https://example.invalid".to_owned(),
                username: "demo@example.invalid".to_owned(),
            },
        );
        cache
            .details
            .insert("gone@example.invalid".to_owned(), OrgDetail::default());
        cache.save(root, dir.path()).expect("save org cache");

        let loaded = OrgCache::load(root, dir.path());
        assert_eq!(loaded.orgs, cache.orgs);
        assert!(loaded.details.contains_key("demo@example.invalid"));
        assert!(!loaded.details.contains_key("gone@example.invalid"));
        assert!(loaded.saved_at > 0);
    }

    #[test]
    fn corrupt_org_file_falls_back_to_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = Path::new("/work/project");
        let path = state_dir(root, dir.path()).join(ORGS_FILE);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(&path, b"{not json").expect("write corrupt file");
        assert_eq!(OrgCache::load(root, dir.path()), OrgCache::default());
        let mut cache = OrgCache {
            orgs: vec![sample_org("a@example.invalid", false)],
            ..OrgCache::default()
        };
        cache.clear();
        assert!(cache.orgs.is_empty());
    }

    #[test]
    fn test_history_round_trips_caps_removes_and_clears() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = Path::new("/work/project");
        let mut history = TestHistory::default();
        for index in 0..(MAX_SAVED_RUNS + 5) {
            history.push(sample_run(&format!("job-{index}")));
        }
        assert_eq!(history.runs.len(), MAX_SAVED_RUNS);
        assert_eq!(history.runs.first().expect("newest").id, "job-54");
        // Dedupe by id: pushing an existing id moves it to the front without growing the cap.
        history.push(sample_run("job-40"));
        assert_eq!(history.runs.len(), MAX_SAVED_RUNS);
        assert_eq!(history.runs.first().expect("deduped").id, "job-40");

        history.save(root, dir.path()).expect("save history");
        let loaded = TestHistory::load(root, dir.path());
        assert_eq!(loaded.runs.len(), MAX_SAVED_RUNS);
        assert_eq!(loaded.runs.first().expect("newest").id, "job-40");
        assert_eq!(
            loaded.runs.first().expect("newest").org.as_deref(),
            Some("alias-demo")
        );

        assert!(history.remove("job-40"));
        assert!(!history.remove("job-40"));
        history.clear();
        assert!(history.runs.is_empty());
    }

    #[test]
    fn malformed_history_entries_are_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = Path::new("/work/project");
        let path = state_dir(root, dir.path()).join(TEST_RUNS_FILE);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        std::fs::write(
            &path,
            r#"{"runs":[{"id":"bad"},{"id":"job-1","finished_at":1,"org":null,"command":"sf apex run test","run":{"id":"08N","at":1,"command":"sf apex run test","outcome":"Passed","tests_run":1,"passing":1,"failing":0,"skipped":0,"coverage":"","tests":[],"failures":[],"coverages":[]}}]}"#,
        )
        .expect("write history");
        let loaded = TestHistory::load(root, dir.path());
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].id, "job-1");
    }

    #[cfg(unix)]
    #[test]
    fn state_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let root = Path::new("/work/project");
        OrgCache::default()
            .save(root, dir.path())
            .expect("save orgs");
        TestHistory::default()
            .save(root, dir.path())
            .expect("save history");
        for file in [ORGS_FILE, TEST_RUNS_FILE] {
            let mode = std::fs::metadata(state_dir(root, dir.path()).join(file))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file} must be owner-only");
        }
        SfVersionCache {
            version: "99.0.0".to_owned(),
            saved_at: 1,
        }
        .save(dir.path())
        .expect("save version");
        let mode = std::fs::metadata(dir.path().join(SF_VERSION_FILE))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "sf-version.json must be owner-only");
    }

    #[test]
    fn version_cache_round_trips_and_corruption_falls_back_to_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(SfVersionCache::load(dir.path()), SfVersionCache::default());
        let cache = SfVersionCache {
            version: "99.0.0".to_owned(),
            saved_at: now_ms(),
        };
        cache.save(dir.path()).expect("save version");
        let loaded = SfVersionCache::load(dir.path());
        assert_eq!(loaded.version, "99.0.0");
        assert!(loaded.is_present());
        assert!(loaded.saved_at > 0);

        std::fs::write(dir.path().join(SF_VERSION_FILE), b"{broken").expect("write corrupt");
        let loaded = SfVersionCache::load(dir.path());
        assert!(!loaded.is_present(), "a corrupt cache must be ignored");
    }
}
