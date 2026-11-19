//! Parsing of Apex test runs.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::string_field;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResultRow {
    pub name: String,
    pub outcome: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub name: String,
    pub percent: String,
    pub covered: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    for key in ["codecoverage", "codeCoverage", "coverages"] {
        if let Some(items) = result.get(key).and_then(Value::as_array) {
            extend_coverages(&mut coverages, items);
        }
    }
    // Newer CLI builds nest the per-class table under `coverage`:
    //   "coverage": { "coverage": [ {name, totalLines, totalCovered, coveredPercent} ],
    //                 "records": [ {ApexClassOrTrigger: {Name}, Coverage: {coveredLines, uncoveredLines}} ],
    //                 "summary": {...} }
    if let Some(coverage) = result.get("coverage") {
        if let Some(items) = coverage.as_array() {
            extend_coverages(&mut coverages, items);
        } else {
            if let Some(items) = coverage.get("coverage").and_then(Value::as_array) {
                extend_class_coverages(&mut coverages, items);
            }
            if coverages.is_empty()
                && let Some(records) = coverage.get("records").and_then(Value::as_array)
            {
                extend_record_coverages(&mut coverages, records);
            }
        }
    }
    coverages
}

/// Class-level rows from `result.coverage.coverage`.
fn extend_class_coverages(coverages: &mut Vec<Coverage>, items: &[Value]) {
    for item in items {
        let name = string_field(item, "name")
            .or_else(|| string_field(item, "Name"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let (mut covered, mut total) = item
            .get("lines")
            .and_then(Value::as_object)
            .map(|lines| {
                let covered = lines
                    .values()
                    .filter(|hits| hits.as_u64().unwrap_or(0) > 0)
                    .count();
                (covered, lines.len())
            })
            .unwrap_or((0, 0));
        if covered == 0 {
            covered = count(
                item.get("totalCovered")
                    .or_else(|| item.get("numLinesCovered")),
            );
        }
        if total == 0 {
            total = count(item.get("totalLines"));
        }
        if total == 0 {
            total = covered
                + count(
                    item.get("numLinesUncovered")
                        .or_else(|| item.get("totalUncovered")),
                );
        }
        let percent = coverage_percent_text(item)
            .or_else(|| percent_from_counts(covered, total))
            .unwrap_or_default();
        push_coverage(
            coverages,
            Coverage {
                name,
                percent,
                covered,
                total,
            },
        );
    }
}

/// Fallback: union the covered/uncovered line numbers of every test-method record per class.
fn extend_record_coverages(coverages: &mut Vec<Coverage>, records: &[Value]) {
    use std::collections::{BTreeMap, BTreeSet};

    let mut classes: BTreeMap<String, (BTreeSet<u64>, BTreeSet<u64>)> = BTreeMap::new();
    for record in records {
        let name = record
            .get("ApexClassOrTrigger")
            .and_then(|value| string_field(value, "Name").or_else(|| string_field(value, "name")))
            .or_else(|| string_field(record, "apexClassOrTriggerName"))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let entry = classes.entry(name).or_default();
        if let Some(coverage) = record.get("Coverage") {
            for key in ["coveredLines", "CoveredLines"] {
                if let Some(lines) = coverage.get(key).and_then(Value::as_array) {
                    entry.0.extend(lines.iter().filter_map(Value::as_u64));
                }
            }
            for key in ["uncoveredLines", "UncoveredLines"] {
                if let Some(lines) = coverage.get(key).and_then(Value::as_array) {
                    entry.1.extend(lines.iter().filter_map(Value::as_u64));
                }
            }
        }
    }
    for (name, (covered_lines, uncovered_lines)) in classes {
        let covered = covered_lines.len();
        let total = covered + uncovered_lines.len();
        let percent = percent_from_counts(covered, total).unwrap_or_default();
        push_coverage(
            coverages,
            Coverage {
                name,
                percent,
                covered,
                total,
            },
        );
    }
}

fn coverage_percent_text(item: &Value) -> Option<String> {
    string_field(item, "percentage")
        .or_else(|| string_field(item, "percent"))
        .or_else(|| {
            item.get("coveredPercent")
                .or_else(|| item.get("percentage"))
                .and_then(Value::as_f64)
                .map(format_percent)
        })
        .or_else(|| {
            string_field(item, "coveredPercent")
                .and_then(|text| text.parse::<f64>().ok())
                .map(format_percent)
        })
}

fn format_percent(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

fn percent_from_counts(covered: usize, total: usize) -> Option<String> {
    (total > 0).then(|| format_percent(covered as f64 * 100.0 / total as f64))
}

fn push_coverage(coverages: &mut Vec<Coverage>, entry: Coverage) {
    if !coverages.iter().any(|existing| existing.name == entry.name) {
        coverages.push(entry);
    }
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
        let percent = coverage_percent_text(item)
            .or_else(|| percent_from_counts(covered, covered + uncovered))
            .unwrap_or_default();
        push_coverage(
            coverages,
            Coverage {
                name,
                percent,
                covered,
                total: covered + uncovered,
            },
        );
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

/// Outcomes that mean the org has finished the run and `apex get test` has the final result.
pub fn is_terminal_outcome(outcome: &str) -> bool {
    matches!(
        outcome.to_ascii_lowercase().as_str(),
        "passed" | "failed" | "completed" | "aborted" | "compilefail"
    )
}

/// Recognise an authoritative Apex test report inside a CLI response.
///
/// `sf apex get test --json` prints the complete report and exits 100 when tests fail, so the
/// exit code cannot decide whether the payload is usable. A payload is accepted only when it has
/// a terminal summary whose run id matches the one we requested: an error envelope, an empty
/// response and a still-running status are never presented as test results.
pub fn parse_terminal_report(
    value: &Value,
    command: &str,
    expected_run_id: &str,
) -> Option<TestRun> {
    let result = value.get("result").unwrap_or(value);
    let summary = result.get("summary")?;
    let outcome = summary.get("outcome").and_then(Value::as_str)?;
    if !is_terminal_outcome(outcome) {
        return None;
    }
    let run = parse(value, command);
    (run.id == expected_run_id).then_some(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_failed_report_is_parsed_with_every_class_and_method() {
        let value = serde_json::json!({
          "status": 100,
          "result": {
            "testRunId": "08N000000000002",
            "summary": {"outcome": "Failed", "testsRan": 3, "passing": 2, "failing": 1,
                        "skipped": 0, "testRunCoverage": "75%"},
            "tests": [
              {"fullName": "AlphaTest.ok", "outcome": "Pass"},
              {"fullName": "AlphaTest.alsoOk", "outcome": "Pass"},
              {"fullName": "BetaTest.bad", "outcome": "Fail",
               "message": "System.AssertException: synthetic assertion failed"}
            ],
            "codecoverage": [{"name": "BetaService", "percentage": "75%",
                              "numLinesCovered": 3, "numLinesUncovered": 1}]
          }
        });
        let run = parse_terminal_report(&value, "sf apex get test", "08N000000000002")
            .expect("terminal report");
        assert_eq!(run.outcome, "Failed");
        assert_eq!(run.tests.len(), 3);
        assert_eq!(run.tests[0].name, "AlphaTest.ok");
        assert_eq!(run.tests[2].name, "BetaTest.bad");
        assert_eq!(
            run.failures[0].message,
            "System.AssertException: synthetic assertion failed"
        );
        assert_eq!(run.coverages[0].name, "BetaService");
    }

    #[test]
    fn error_payload_is_not_a_terminal_report() {
        let value = serde_json::json!({
          "status": 100,
          "name": "SfError",
          "message": "synthetic report failure",
          "exitCode": 100
        });
        assert!(
            parse_terminal_report(&value, "sf apex get test", "08N000000000003").is_none(),
            "a CLI error envelope must not become a test result"
        );
    }

    #[test]
    fn non_terminal_and_foreign_run_reports_are_rejected() {
        let processing = serde_json::json!({
          "status": 0,
          "result": {"testRunId": "08N000000000002",
                     "summary": {"outcome": "Processing"}}
        });
        assert!(
            parse_terminal_report(&processing, "sf apex get test", "08N000000000002").is_none()
        );

        let failed = serde_json::json!({
          "status": 100,
          "result": {"testRunId": "08N000000000009",
                     "summary": {"outcome": "Failed", "failing": 1}}
        });
        assert!(parse_terminal_report(&failed, "sf apex get test", "08N000000000002").is_none());
    }

    #[test]
    fn terminal_outcomes_are_recognized() {
        for outcome in ["Passed", "Failed", "Completed", "Aborted", "CompileFail"] {
            assert!(is_terminal_outcome(outcome), "{outcome} must be terminal");
        }
        for outcome in ["Processing", "Queued", "", "Unknown"] {
            assert!(
                !is_terminal_outcome(outcome),
                "{outcome} must not be terminal"
            );
        }
    }

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
    fn parses_the_cli_coverage_object_with_all_classes() {
        let value = serde_json::json!({
          "status": 0,
          "result": {
            "summary": {"outcome": "Passed", "testsRan": 1, "passing": 1, "failing": 0,
                        "testRunCoverage": "60%", "orgWideCoverage": "62%"},
            "tests": [{"FullName": "ContactTest.test1", "Outcome": "Pass"}],
            "coverage": {
              "coverage": [
                {"id": "01p1", "name": "ContactService", "totalLines": 20, "totalCovered": 8, "coveredPercent": 40},
                {"id": "01p2", "name": "ContactHelper", "totalLines": 10, "totalCovered": 10, "coveredPercent": 100}
              ],
              "records": [],
              "summary": {"totalLines": 30, "coveredLines": 18, "orgWideCoverage": "62%", "testRunCoverage": "60%"}
            }
          }
        });
        let run = parse(&value, "sf apex get test");
        assert_eq!(run.coverages.len(), 2);
        let service = run
            .coverages
            .iter()
            .find(|coverage| coverage.name == "ContactService")
            .expect("service coverage");
        assert_eq!(service.covered, 8);
        assert_eq!(service.total, 20);
        assert_eq!(service.percent, "40%");
        assert_eq!(run.coverage, "60%");
    }

    #[test]
    fn falls_back_to_unioning_test_method_coverage_records() {
        let value = serde_json::json!({
          "status": 0,
          "result": {
            "summary": {"outcome": "Passed", "testsRan": 2, "passing": 2, "failing": 0},
            "coverage": {
              "records": [
                {"ApexClassOrTrigger": {"Name": "Svc"},
                 "Coverage": {"coveredLines": [1, 2], "uncoveredLines": [3]}},
                {"ApexClassOrTrigger": {"Name": "Svc"},
                 "Coverage": {"coveredLines": [2], "uncoveredLines": [3, 4]}}
              ]
            }
          }
        });
        let run = parse(&value, "sf apex get test");
        assert_eq!(run.coverages.len(), 1);
        assert_eq!(run.coverages[0].covered, 2);
        assert_eq!(run.coverages[0].total, 4);
        assert_eq!(run.coverages[0].percent, "50%");
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
}
