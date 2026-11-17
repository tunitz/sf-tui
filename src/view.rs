//! Pure conversion of process results into the result views rendered by `ui`.
//!
//! Keeping this out of `app.rs` makes the presentation rules unit-testable without a terminal or
//! an `App` instance.

use std::collections::HashMap;

use serde_json::Value;

use crate::app::{ResultItem, ResultKind, ResultSection, ResultView};
use crate::sf::deploy::TransferReport;
use crate::sf::runner::ProcessOutcome;
use crate::sf::test_results::TestRun;

pub fn generic_result(title: &str, outcome: &ProcessOutcome, parsed: Option<&Value>) -> ResultView {
    let ok = outcome.succeeded();
    let mut items = Vec::new();
    let mut sections = Vec::new();
    if let Some(value) = parsed {
        let view = crate::json_view::table(value);
        for row in view.rows.iter().take(200) {
            let label = row.first().cloned().unwrap_or_default();
            let detail = row.iter().skip(1).cloned().collect::<Vec<_>>().join(" | ");
            if !label.is_empty() || !detail.is_empty() {
                items.push(ResultItem {
                    ok,
                    label,
                    detail,
                    progress: None,
                });
            }
        }
    }
    if items.is_empty() {
        for line in outcome
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(200)
        {
            let bad = line.to_ascii_lowercase().contains("error")
                || line.to_ascii_lowercase().contains("failed");
            items.push(ResultItem {
                ok: !bad,
                label: line.trim().to_owned(),
                detail: String::new(),
                progress: None,
            });
        }
    }
    if !outcome.stderr.trim().is_empty() {
        let rows = outcome
            .stderr
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(200)
            .map(|line| ResultItem {
                ok: false,
                label: line.trim().to_owned(),
                detail: String::new(),
                progress: None,
            })
            .collect();
        sections.push(ResultSection {
            title: "ERRORS".to_owned(),
            rows,
        });
    }
    ResultView {
        kind: ResultKind::Generic,
        title: title.to_uppercase(),
        status: if ok {
            "SUCCESS".to_owned()
        } else {
            "FAILED".to_owned()
        },
        ok,
        summary: format!(
            "exit {} · {} output lines",
            outcome
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_owned()),
            outcome.stdout.lines().count()
        ),
        items,
        coverage: Vec::new(),
        sections,
    }
}

pub fn transfer_result(
    retrieve: bool,
    report: &TransferReport,
    job_id: Option<&str>,
    requested: &[String],
) -> ResultView {
    let ok = !report.status.eq_ignore_ascii_case("failed")
        && !report.status.eq_ignore_ascii_case("error")
        && report.failed == 0;
    let component_row = |component: &crate::sf::deploy::ComponentResult| {
        let failed = matches!(
            component.status.to_ascii_lowercase().as_str(),
            "failed" | "error" | "failure"
        );
        ResultItem {
            ok: !failed && component.problem.is_empty(),
            label: if component.component_type.is_empty() {
                component.name.clone()
            } else {
                format!("{}:{}", component.component_type, component.name)
            },
            detail: if component.problem.is_empty() {
                component.status.clone()
            } else {
                format!("{} — {}", component.status, component.problem)
            },
            progress: None,
        }
    };

    let items = if !requested.is_empty() {
        // Index both the "Type:Name" and bare-name forms to the first matching component.
        // `or_insert` keeps the earliest index so this has the same first-match-wins result as
        // the old linear `.find()` while making each lookup O(1).
        let mut by_key: HashMap<String, usize> =
            HashMap::with_capacity(report.components.len().saturating_mul(2));
        for (index, component) in report.components.iter().enumerate() {
            let full = if component.component_type.is_empty() {
                component.name.clone()
            } else {
                format!("{}:{}", component.component_type, component.name)
            };
            by_key.entry(full).or_insert(index);
            by_key.entry(component.name.clone()).or_insert(index);
        }
        requested
            .iter()
            .map(|name| {
                by_key
                    .get(name.as_str())
                    .map(|&index| component_row(&report.components[index]))
                    .unwrap_or_else(|| ResultItem {
                        ok,
                        label: name.clone(),
                        detail: if report.status.is_empty() {
                            if ok { "Succeeded" } else { "Failed" }.to_owned()
                        } else {
                            report.status.clone()
                        },
                        progress: None,
                    })
            })
            .collect()
    } else if report.components.is_empty() {
        vec![ResultItem {
            ok,
            label: if retrieve {
                "Retrieve request".to_owned()
            } else {
                "Deploy request".to_owned()
            },
            detail: job_id.unwrap_or("").to_owned(),
            progress: None,
        }]
    } else {
        report.components.iter().map(component_row).collect()
    };

    let (total, succeeded, failed) = if requested.is_empty() || report.total > 0 {
        (report.total, report.succeeded, report.failed)
    } else {
        let count = requested.len();
        if ok {
            (count, count, 0)
        } else {
            (count, 0, count)
        }
    };
    ResultView {
        kind: if retrieve {
            ResultKind::Retrieve
        } else {
            ResultKind::Deploy
        },
        title: if retrieve {
            "Retrieve".to_owned()
        } else {
            "Deploy".to_owned()
        },
        status: report.status.clone(),
        ok,
        summary: format!("{total} total · {succeeded} succeeded · {failed} failed"),
        items,
        coverage: Vec::new(),
        sections: Vec::new(),
    }
}

pub fn apex_result(run: &TestRun) -> ResultView {
    let ok = run.outcome.eq_ignore_ascii_case("Passed") || run.outcome.eq_ignore_ascii_case("Pass");
    let mut classes: Vec<(String, usize, usize, bool)> = Vec::new();
    for test in &run.tests {
        let class = test
            .name
            .split_once('.')
            .map_or(test.name.as_str(), |(class, _)| class);
        let passed = !matches!(
            test.outcome.to_ascii_lowercase().as_str(),
            "fail" | "failed" | "compilefail"
        );
        if let Some(entry) = classes.iter_mut().find(|entry| entry.0 == class) {
            entry.1 += 1;
            entry.2 += usize::from(passed);
            entry.3 &= passed;
        } else {
            classes.push((class.to_owned(), 1, usize::from(passed), passed));
        }
    }
    let class_rows: Vec<ResultItem> = classes
        .into_iter()
        .map(|(class, total, passed, class_ok)| ResultItem {
            ok: class_ok,
            label: class,
            detail: format!("{passed}/{total} methods passed"),
            progress: None,
        })
        .collect();
    let method_rows: Vec<ResultItem> = run
        .tests
        .iter()
        .map(|test| ResultItem {
            ok: !matches!(
                test.outcome.to_ascii_lowercase().as_str(),
                "fail" | "failed" | "compilefail"
            ),
            label: test.name.clone(),
            detail: if test.message.is_empty() {
                test.outcome.clone()
            } else {
                format!("{} — {}", test.outcome, test.message)
            },
            progress: None,
        })
        .collect();
    let coverage = run
        .coverages
        .iter()
        .map(|item| ResultItem {
            ok: true,
            label: item.name.clone(),
            detail: if item.total == 0 {
                item.percent.clone()
            } else {
                let uncovered = item.total.saturating_sub(item.covered);
                format!(
                    "{} · {}/{} lines · {} uncovered",
                    item.percent, item.covered, item.total, uncovered
                )
            },
            progress: if item.total == 0 {
                None
            } else {
                Some((item.covered, item.total))
            },
        })
        .collect();
    let mut sections = Vec::new();
    if run.coverages.is_empty() && !run.coverage.is_empty() {
        sections.push(ResultSection {
            title: "COVERAGE".to_owned(),
            rows: vec![ResultItem {
                ok: false,
                label: "Per-class coverage not returned by the CLI".to_owned(),
                detail:
                    "the org reported the total only; enable aggregate coverage in Setup › Apex Test Execution"
                        .to_owned(),
                progress: None,
            }],
        });
    }
    if !method_rows.is_empty() {
        sections.push(ResultSection {
            title: "TEST METHODS".to_owned(),
            rows: method_rows,
        });
    }
    ResultView {
        kind: ResultKind::ApexTests,
        title: "Apex tests".to_owned(),
        status: run.outcome.clone(),
        ok,
        summary: format!(
            "{} run · {} passed · {} failed · {} skipped · coverage {}",
            run.tests_run, run.passing, run.failing, run.skipped, run.coverage
        ),
        items: class_rows,
        coverage,
        sections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(stdout: &str, exit_code: i32) -> ProcessOutcome {
        ProcessOutcome {
            exit_code: Some(exit_code),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[test]
    fn generic_result_unwraps_the_json_envelope() {
        let parsed = serde_json::json!({"status":0,"result":{"username":"u","id":"00D"}});
        let view = generic_result("org display", &outcome("", 0), Some(&parsed));
        assert!(view.items.iter().any(|item| item.label == "username"));
        assert!(!view.items.iter().any(|item| item.label == "result"));
    }

    #[test]
    fn transfer_result_marks_failed_components() {
        let report = TransferReport {
            status: "Failed".to_owned(),
            total: 2,
            succeeded: 1,
            failed: 1,
            components: vec![crate::sf::deploy::ComponentResult {
                name: "Demo".to_owned(),
                component_type: "ApexClass".to_owned(),
                status: "Failed".to_owned(),
                problem: "compile error".to_owned(),
            }],
            ..TransferReport::default()
        };
        let view = transfer_result(false, &report, None, &[]);
        assert!(!view.ok);
        assert!(view.items.iter().any(|item| !item.ok));
    }

    #[test]
    fn transfer_result_lists_every_requested_item_when_the_cli_reports_none() {
        let report = TransferReport {
            status: "Succeeded".to_owned(),
            ..TransferReport::default()
        };
        let requested = vec![
            "ApexClass:One".to_owned(),
            "ApexClass:Two".to_owned(),
            "Flow:Three".to_owned(),
        ];
        let view = transfer_result(true, &report, None, &requested);
        assert!(view.ok);
        assert_eq!(view.items.len(), 3);
        assert!(view.items.iter().all(|item| item.ok));
        assert_eq!(view.summary, "3 total · 3 succeeded · 0 failed");
    }

    #[test]
    fn transfer_result_lookup_matches_linear_scan_with_duplicate_names() {
        use crate::sf::deploy::ComponentResult;
        let component = |kind: &str, name: &str, status: &str, problem: &str| ComponentResult {
            name: name.to_owned(),
            component_type: kind.to_owned(),
            status: status.to_owned(),
            problem: problem.to_owned(),
        };
        let report = TransferReport {
            status: "Failed".to_owned(),
            total: 5,
            succeeded: 3,
            failed: 2,
            components: vec![
                component("ApexClass", "Demo", "Failed", "compile error"),
                component("", "ApexClass:Demo", "Succeeded", ""),
                component("ApexClass", "Demo", "Succeeded", ""),
                component("Flow", "Demo", "Failed", "flow error"),
                component("", "Solo", "Succeeded", ""),
            ],
            ..TransferReport::default()
        };
        let requested = vec![
            "ApexClass:Demo".to_owned(),
            "Demo".to_owned(),
            "Flow:Demo".to_owned(),
            "Solo".to_owned(),
            "Missing".to_owned(),
        ];

        // Reproduce the original linear `.find()` semantics as a reference.
        let ok = !report.status.eq_ignore_ascii_case("failed")
            && !report.status.eq_ignore_ascii_case("error")
            && report.failed == 0;
        let expected: Vec<(bool, String, String)> = requested
            .iter()
            .map(|name| {
                report
                    .components
                    .iter()
                    .find(|component| {
                        let full = if component.component_type.is_empty() {
                            component.name.clone()
                        } else {
                            format!("{}:{}", component.component_type, component.name)
                        };
                        &full == name || &component.name == name
                    })
                    .map(|component| {
                        let failed = matches!(
                            component.status.to_ascii_lowercase().as_str(),
                            "failed" | "error" | "failure"
                        );
                        let label = if component.component_type.is_empty() {
                            component.name.clone()
                        } else {
                            format!("{}:{}", component.component_type, component.name)
                        };
                        let detail = if component.problem.is_empty() {
                            component.status.clone()
                        } else {
                            format!("{} — {}", component.status, component.problem)
                        };
                        (!failed && component.problem.is_empty(), label, detail)
                    })
                    .unwrap_or_else(|| {
                        let detail = if report.status.is_empty() {
                            if ok { "Succeeded" } else { "Failed" }.to_owned()
                        } else {
                            report.status.clone()
                        };
                        (ok, name.clone(), detail)
                    })
            })
            .collect();

        let view = transfer_result(false, &report, None, &requested);
        let actual: Vec<(bool, String, String)> = view
            .items
            .into_iter()
            .map(|item| (item.ok, item.label, item.detail))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn transfer_result_marks_a_requested_item_from_the_cli_status() {
        let report = TransferReport {
            status: "Failed".to_owned(),
            total: 2,
            succeeded: 1,
            failed: 1,
            components: vec![crate::sf::deploy::ComponentResult {
                name: "Two".to_owned(),
                component_type: "ApexClass".to_owned(),
                status: "Failed".to_owned(),
                problem: "compile error".to_owned(),
            }],
            ..TransferReport::default()
        };
        let requested = vec!["ApexClass:One".to_owned(), "ApexClass:Two".to_owned()];
        let view = transfer_result(false, &report, None, &requested);
        assert_eq!(view.items.len(), 2);
        // The CLI reported no per-item status for One, so it follows the overall failure.
        assert!(!view.items[0].ok);
        assert!(!view.items[1].ok);
        assert!(view.items[1].detail.contains("compile error"));
    }
}
