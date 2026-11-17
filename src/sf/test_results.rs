//! Parsing and local persistence of Apex test runs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::string_field;

pub const TEST_RESULTS_FILE: &str = "test-runs.json";
const MAX_RUNS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub name: String,
    pub percent: String,
    pub covered: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub id: String,
    pub at: u64,
    pub command: String,
    pub outcome: String,
    pub tests_run: usize,
    pub passing: usize,
    pub failing: usize,
    pub skipped: usize,
    pub coverage: String,
    pub failures: Vec<TestFailure>,
    pub coverages: Vec<Coverage>,
}

pub fn path(config_dir: &Path) -> PathBuf {
    config_dir.join(TEST_RESULTS_FILE)
}

pub fn load(config_dir: &Path) -> Vec<TestRun> {
    let Ok(bytes) = std::fs::read(path(config_dir)) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save(config_dir: &Path, runs: &[TestRun]) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    std::fs::write(
        path(config_dir),
        serde_json::to_vec_pretty(runs).unwrap_or_default(),
    )
}

pub fn record(config_dir: &Path, run: TestRun) -> Vec<TestRun> {
    let mut runs = load(config_dir);
    runs.retain(|existing| existing.id != run.id || existing.at != run.at);
    runs.insert(0, run);
    runs.truncate(MAX_RUNS);
    let _ = save(config_dir, &runs);
    runs
}

pub fn parse(value: &Value, command: &str) -> TestRun {
    let result = value.get("result").unwrap_or(value);
    let summary = result.get("summary").unwrap_or(result);
    let tests = result.get("tests").and_then(Value::as_array);
    let coverages = result
        .get("coverages")
        .or_else(|| result.get("coverage"))
        .and_then(Value::as_array);
    let failures = tests
        .into_iter()
        .flatten()
        .filter(|test| {
            let outcome = string_field(test, "Outcome")
                .or_else(|| string_field(test, "outcome"))
                .unwrap_or_default();
            outcome.eq_ignore_ascii_case("Fail") || outcome.eq_ignore_ascii_case("Failed")
        })
        .map(|test| TestFailure {
            name: string_field(test, "FullName")
                .or_else(|| string_field(test, "Name"))
                .or_else(|| string_field(test, "name"))
                .unwrap_or_default(),
            message: string_field(test, "Message")
                .or_else(|| string_field(test, "message"))
                .unwrap_or_default(),
        })
        .collect();
    let coverages = coverages
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = string_field(item, "Name")
                .or_else(|| string_field(item, "name"))
                .unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            Some(Coverage {
                name,
                percent: string_field(item, "Coverage")
                    .or_else(|| string_field(item, "percent"))
                    .or_else(|| item.get("Coverage").map(Value::to_string))
                    .unwrap_or_default(),
                covered: count(item.get("CoveredLines").or_else(|| item.get("covered"))),
                total: count(item.get("NumLinesCovered").or_else(|| item.get("total"))),
            })
        })
        .collect();
    TestRun {
        id: string_field(result, "testRunId")
            .or_else(|| string_field(result, "id"))
            .unwrap_or_default(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        command: command.to_owned(),
        outcome: string_field(summary, "outcome")
            .or_else(|| string_field(summary, "status"))
            .unwrap_or_else(|| "Unknown".to_owned()),
        tests_run: count(summary.get("testsRan").or_else(|| summary.get("total"))),
        passing: count(summary.get("passing").or_else(|| summary.get("Passed"))),
        failing: count(summary.get("failing").or_else(|| summary.get("Failed"))),
        skipped: count(summary.get("skipped")),
        coverage: string_field(summary, "coverage")
            .or_else(|| summary.get("coverage").map(Value::to_string))
            .unwrap_or_default(),
        failures,
        coverages,
    }
}

fn count(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0) as usize,
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        Some(Value::Array(items)) => items.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_summary_failures_and_coverages() {
        let value = serde_json::json!({
          "status": 0,
          "result": {
            "testRunId": "08N000000000001",
            "summary": {"outcome": "Failed", "testsRan": 3, "passing": 2, "failing": 1, "skipped": 0, "coverage": "81%"},
            "tests": [
              {"FullName": "DemoTest.ok", "Outcome": "Pass"},
              {"FullName": "DemoTest.bad", "Outcome": "Fail", "Message": "assertion failed"}
            ],
            "coverages": [{"Name": "Demo", "Coverage": 81, "NumLinesCovered": 17}]
          }
        });
        let run = parse(&value, "sf apex run test");
        assert_eq!(run.outcome, "Failed");
        assert_eq!(run.failures.len(), 1);
        assert_eq!(run.failures[0].message, "assertion failed");
        assert_eq!(run.coverages[0].name, "Demo");
    }

    #[test]
    fn results_round_trip_and_are_bounded() {
        let dir = tempfile::tempdir().expect("temp dir");
        for index in 0..60 {
            let run = TestRun {
                id: format!("run-{index}"),
                at: index,
                command: "sf apex run test".to_owned(),
                outcome: "Passed".to_owned(),
                tests_run: 1,
                passing: 1,
                failing: 0,
                skipped: 0,
                coverage: "100%".to_owned(),
                failures: Vec::new(),
                coverages: Vec::new(),
            };
            record(dir.path(), run);
        }
        assert_eq!(load(dir.path()).len(), MAX_RUNS);
    }
}
