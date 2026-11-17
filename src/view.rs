//! Pure conversion of process results into the result views rendered by `ui`.
//!
//! Keeping this out of `app.rs` makes the presentation rules unit-testable without a terminal or
//! an `App` instance.

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
) -> ResultView {
    let ok = !report.status.eq_ignore_ascii_case("failed")
        && !report.status.eq_ignore_ascii_case("error")
        && report.failed == 0;
    let items = if report.components.is_empty() {
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
        report
            .components
            .iter()
            .map(|component| {
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
            })
            .collect()
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
        summary: format!(
            "{} total · {} succeeded · {} failed",
            report.total, report.succeeded, report.failed
        ),
        items,
        coverage: Vec::new(),
        sections: Vec::new(),
    }
}

pub fn apex_result(run: &TestRun) -> ResultView {
    let ok = run.outcome.eq_ignore_ascii_case("Passed") || run.outcome.eq_ignore_ascii_case("Pass");
    let items = run
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
    ResultView {
        kind: ResultKind::ApexTests,
        title: "Apex tests".to_owned(),
        status: run.outcome.clone(),
        ok,
        summary: format!(
            "{} run · {} passed · {} failed · {} skipped · coverage {}",
            run.tests_run, run.passing, run.failing, run.skipped, run.coverage
        ),
        items,
        coverage,
        sections: Vec::new(),
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
        let view = transfer_result(false, &report, None);
        assert!(!view.ok);
        assert!(view.items.iter().any(|item| !item.ok));
    }
}
