//! Parsing and local persistence of Apex test runs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::string_field;

pub const TEST_RESULTS_FILE: &str = "test-runs.json";
const MAX_RUNS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResultRow {
    pub name: String,
    pub outcome: String,
    pub message: String,
}

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
    #[serde(default)]
    pub tests: Vec<TestResultRow>,
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
    let test_values = result.get("tests").and_then(Value::as_array);
    let tests: Vec<TestResultRow> = test_values
        .into_iter()
        .flatten()
        .map(|test| TestResultRow {
            name: string_field(test, "fullName")
                .or_else(|| string_field(test, "FullName"))
                .or_else(|| string_field(test, "name"))
                .unwrap_or_default(),
            outcome: string_field(test, "outcome")
                .or_else(|| string_field(test, "Outcome"))
                .unwrap_or_default(),
            message: string_field(test, "message")
                .or_else(|| string_field(test, "Message"))
                .unwrap_or_default(),
        })
        .filter(|test| !test.name.is_empty())
        .collect();
    let failures = tests
        .iter()
        .filter(|test| is_failure(&test.outcome))
        .map(|test| TestFailure {
            name: test.name.clone(),
            message: test.message.clone(),
        })
        .collect();
    let mut coverages = collect_coverages(result);
    for test in test_values.into_iter().flatten() {
        if let Some(per_class) = test
            .get("perClassCoverage")
            .or_else(|| test.get("PerClassCoverage"))
            .and_then(Value::as_array)
        {
            extend_coverages(&mut coverages, per_class);
        }
    }
    TestRun {
        id: string_field(result, "testRunId")
            .or_else(|| string_field(result, "id"))
            .or_else(|| string_field(summary, "testRunId"))
            .unwrap_or_default(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        command: command.to_owned(),
        outcome: string_field(summary, "outcome")
            .or_else(|| string_field(result, "status"))
            .unwrap_or_else(|| "Unknown".to_owned()),
        tests_run: count(summary.get("testsRan").or_else(|| summary.get("total"))),
        passing: count(summary.get("passing").or_else(|| summary.get("Passed"))),
        failing: count(summary.get("failing").or_else(|| summary.get("Failed"))),
        skipped: count(summary.get("skipped")),
        coverage: string_field(summary, "testRunCoverage")
            .or_else(|| string_field(summary, "orgWideCoverage"))
            .or_else(|| string_field(summary, "coverage"))
            .or_else(|| summary.get("coverage").map(Value::to_string))
            .unwrap_or_default(),
        tests,
        failures,
        coverages,
    }
}

fn is_failure(outcome: &str) -> bool {
    matches!(
        outcome.to_ascii_lowercase().as_str(),
        "fail" | "failed" | "compilefail"
    )
}

fn collect_coverages(result: &Value) -> Vec<Coverage> {
    let mut coverages = Vec::new();
    for key in ["codecoverage", "codeCoverage", "coverages", "coverage"] {
        if let Some(items) = result.get(key).and_then(Value::as_array) {
            extend_coverages(&mut coverages, items);
        }
    }
    coverages
}

fn extend_coverages(coverages: &mut Vec<Coverage>, items: &[Value]) {
    for item in items {
        let name = string_field(item, "name")
            .or_else(|| string_field(item, "Name"))
            .or_else(|| string_field(item, "apexClassOrTriggerName"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let nested = item.get("coverage");
        let mut covered = count(
            item.get("numLinesCovered")
                .or_else(|| item.get("NumLinesCovered"))
                .or_else(|| item.get("coveredLines")),
        );
        if covered == 0 {
            covered = count(nested.and_then(|value| value.get("coveredLines")));
        }
        let mut uncovered = count(
            item.get("numLinesUncovered")
                .or_else(|| item.get("NumLinesUncovered"))
                .or_else(|| item.get("uncoveredLines")),
        );
        if uncovered == 0 {
            uncovered = count(nested.and_then(|value| value.get("uncoveredLines")));
        }
        let percent = string_field(item, "percentage")
            .or_else(|| string_field(item, "percent"))
            .or_else(|| item.get("percentage").map(Value::to_string))
            .or_else(|| item.get("percent").map(Value::to_string))
            .unwrap_or_default();
        let entry = Coverage {
            name,
            percent,
            covered,
            total: covered + uncovered,
        };
        if !coverages.iter().any(|existing| existing.name == entry.name) {
            coverages.push(entry);
        }
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
            "codecoverage": [{"name": "Demo", "percentage": "81%", "numLinesCovered": 17, "numLinesUncovered": 4}]
          }
        });
        let run = parse(&value, "sf apex run test");
        assert_eq!(run.outcome, "Failed");
        assert_eq!(run.failures.len(), 1);
        assert_eq!(run.failures[0].message, "assertion failed");
        assert_eq!(run.coverages[0].name, "Demo");
    }

    #[test]
    fn parses_test_rows_and_nested_per_class_coverage() {
        let value = serde_json::json!({
          "status": 0,
          "result": {
            "summary": {"outcome": "Failed", "testsRan": 2, "passing": 1, "failing": 1, "testRunCoverage": "80%"},
            "tests": [
              {"fullName": "DemoTest.ok", "outcome": "Pass"},
              {"fullName": "DemoTest.bad", "outcome": "Fail", "message": "boom",
               "perClassCoverage": [{"apexClassOrTriggerName": "Demo", "percentage": "80%", "numLinesCovered": 8, "numLinesUncovered": 2}]}
            ]
          }
        });
        let run = parse(&value, "sf apex run test");
        assert_eq!(run.tests.len(), 2);
        assert_eq!(run.tests[1].message, "boom");
        assert_eq!(run.failures.len(), 1);
        assert_eq!(run.coverages[0].name, "Demo");
        assert_eq!(run.coverages[0].total, 10);
    }

    #[test]
    fn parses_nested_coverage_line_counts() {
        let value = serde_json::json!({
          "status": 0,
          "result": {
            "summary": {"outcome": "Passed", "testsRan": 1, "passing": 1, "failing": 0, "testRunCoverage": "75%"},
            "codecoverage": [{
              "name": "Demo",
              "percentage": "75%",
              "coverage": {"coveredLines": [1, 2, 3], "uncoveredLines": [4]}
            }]
          }
        });
        let run = parse(&value, "sf apex run test");
        assert_eq!(run.coverages[0].covered, 3);
        assert_eq!(run.coverages[0].total, 4);
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
                tests: Vec::new(),
                failures: Vec::new(),
                coverages: Vec::new(),
            };
            record(dir.path(), run);
        }
        assert_eq!(load(dir.path()).len(), MAX_RUNS);
    }
}
