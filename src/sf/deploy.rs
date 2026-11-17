//! Deploy/retrieve command construction and tolerant report parsing.

use serde_json::Value;

use super::string_field;

/// What the user selected as the deployment payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadMode {
    Source,
    Manifest,
    Metadata,
}

impl PayloadMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Paths",
            Self::Manifest => "Manifest",
            Self::Metadata => "Component",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestLevel {
    NoTestRun,
    RunSpecifiedTests,
    RunLocalTests,
    RunAllTestsInOrg,
}

impl TestLevel {
    pub fn cycle(self) -> Self {
        match self {
            Self::NoTestRun => Self::RunSpecifiedTests,
            Self::RunSpecifiedTests => Self::RunLocalTests,
            Self::RunLocalTests => Self::RunAllTestsInOrg,
            Self::RunAllTestsInOrg => Self::NoTestRun,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoTestRun => "NoTestRun",
            Self::RunSpecifiedTests => "RunSpecifiedTests",
            Self::RunLocalTests => "RunLocalTests",
            Self::RunAllTestsInOrg => "RunAllTestsInOrg",
        }
    }
}

/// Options assembled by the Deploy/Retrieve screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOptions {
    pub retrieve: bool,
    pub mode: PayloadMode,
    pub paths: Vec<String>,
    pub manifest: Option<String>,
    pub metadata_names: Vec<String>,
    pub check_only: bool,
    pub ignore_warnings: bool,
    pub test_level: TestLevel,
    pub tests: Vec<String>,
    pub target_org: Option<String>,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            retrieve: false,
            mode: PayloadMode::Source,
            paths: Vec::new(),
            manifest: None,
            metadata_names: Vec::new(),
            check_only: false,
            ignore_warnings: false,
            test_level: TestLevel::NoTestRun,
            tests: Vec::new(),
            target_org: None,
        }
    }
}

impl TransferOptions {
    /// The exact `sf` command line shown in the output pane and executed by the runner.
    pub fn argv(&self) -> Vec<String> {
        let mut argv: Vec<String> = [
            "project",
            if self.retrieve { "retrieve" } else { "deploy" },
            "start",
        ]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
        match self.mode {
            PayloadMode::Source => {
                for path in &self.paths {
                    argv.push("--source-dir".to_owned());
                    argv.push(path.clone());
                }
            }
            PayloadMode::Manifest => {
                if let Some(manifest) = &self.manifest {
                    argv.push("--manifest".to_owned());
                    argv.push(manifest.clone());
                }
            }
            PayloadMode::Metadata => {
                for name in &self.metadata_names {
                    argv.push("--metadata".to_owned());
                    argv.push(name.clone());
                }
            }
        }
        if !self.retrieve {
            if self.check_only {
                argv.push("--dry-run".to_owned());
            }
            if self.check_only || self.test_level != TestLevel::NoTestRun {
                argv.push("--test-level".to_owned());
                argv.push(self.test_level.as_str().to_owned());
            }
            if self.test_level == TestLevel::RunSpecifiedTests {
                for test in &self.tests {
                    argv.push("--tests".to_owned());
                    argv.push(test.clone());
                }
            }
            if self.ignore_warnings {
                argv.push("--ignore-warnings".to_owned());
            }
        }
        if let Some(org) = &self.target_org
            && !org.is_empty()
        {
            argv.push("--target-org".to_owned());
            argv.push(org.clone());
        }
        argv.push("--json".to_owned());
        argv
    }

    /// The current CLI has `project deploy report` but no `project retrieve report`.
    /// Retrieve commands are run with `--wait` and produce their final result directly.
    pub fn report_argv(&self, job_id: &str) -> Option<Vec<String>> {
        if self.retrieve {
            return None;
        }
        let mut argv = vec![
            "project".to_owned(),
            "deploy".to_owned(),
            "report".to_owned(),
            "--job-id".to_owned(),
            job_id.to_owned(),
        ];
        if let Some(org) = &self.target_org
            && !org.is_empty()
        {
            argv.push("--target-org".to_owned());
            argv.push(org.clone());
        }
        argv.push("--json".to_owned());
        Some(argv)
    }
}

/// Per-component result from a deploy/retrieve report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentResult {
    pub name: String,
    pub component_type: String,
    pub status: String,
    pub problem: String,
}

/// Normalized deploy/retrieve report for the results view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferReport {
    pub id: String,
    pub status: String,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub components: Vec<ComponentResult>,
}

impl TransferReport {
    pub fn is_complete(&self) -> bool {
        matches!(
            self.status.to_ascii_lowercase().as_str(),
            "succeeded"
                | "succeededpartial"
                | "partiallysucceeded"
                | "completed"
                | "failed"
                | "canceled"
                | "cancelled"
        )
    }
}

/// Pull the async job id out of a `deploy start` / `retrieve start` response.
pub fn parse_started_job_id(value: &Value) -> Option<String> {
    let result = value.get("result").unwrap_or(value);
    string_field(result, "id")
        .or_else(|| string_field(result, "jobId"))
        .or_else(|| result.get("id").and_then(Value::as_str).map(str::to_owned))
}

/// Parse a report from several SF and legacy response shapes.
pub fn parse_report(value: &Value) -> TransferReport {
    let result = value.get("result").unwrap_or(value);
    let mut report = TransferReport {
        id: string_field(result, "id").unwrap_or_default(),
        status: string_field(result, "status").unwrap_or_default(),
        ..TransferReport::default()
    };
    let components_value = result.get("components");
    if let Some(counts) = components_value.and_then(|value| value.get("total")) {
        report.total = count_value(counts);
    }
    if let Some(counts) = components_value.and_then(|value| value.get("successes")) {
        report.succeeded = count_value(counts);
    }
    if let Some(counts) = components_value.and_then(|value| value.get("failed")) {
        report.failed = count_value(counts);
    }
    if let Some(items) = components_value.and_then(Value::as_array) {
        for item in items {
            let component = ComponentResult {
                name: string_field(item, "fullName")
                    .or_else(|| string_field(item, "name"))
                    .unwrap_or_default(),
                component_type: string_field(item, "type").unwrap_or_default(),
                status: string_field(item, "status").unwrap_or_default(),
                problem: string_field(item, "problem")
                    .or_else(|| string_field(item, "error"))
                    .unwrap_or_default(),
            };
            if component.name.is_empty() && component.problem.is_empty() {
                continue;
            }
            report.components.push(component);
        }
        report.total = report.total.max(report.components.len());
        report.failed = report.failed.max(
            report
                .components
                .iter()
                .filter(|item| is_failure(&item.status))
                .count(),
        );
        report.succeeded = report
            .succeeded
            .max(report.components.len().saturating_sub(report.failed));
    }
    report
}

fn is_failure(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "failed" | "error" | "failure"
    )
}

fn count_value(value: &Value) -> usize {
    match value {
        Value::Number(number) => number.as_u64().unwrap_or(0) as usize,
        Value::String(text) => text.parse().unwrap_or(0),
        Value::Array(items) => items.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_argv_uses_current_cli_flags() {
        let options = TransferOptions {
            paths: vec!["force-app/main/default".to_owned()],
            check_only: true,
            test_level: TestLevel::RunSpecifiedTests,
            tests: vec!["DemoTest".to_owned()],
            target_org: Some("test-scratch".to_owned()),
            ..TransferOptions::default()
        };
        assert_eq!(
            options.argv(),
            vec![
                "project",
                "deploy",
                "start",
                "--source-dir",
                "force-app/main/default",
                "--dry-run",
                "--test-level",
                "RunSpecifiedTests",
                "--tests",
                "DemoTest",
                "--target-org",
                "test-scratch",
                "--json"
            ]
        );
    }

    #[test]
    fn retrieve_argv_uses_metadata_names_and_has_no_report_command() {
        let options = TransferOptions {
            retrieve: true,
            mode: PayloadMode::Metadata,
            metadata_names: vec!["ApexClass".to_owned()],
            ..TransferOptions::default()
        };
        assert_eq!(
            options.argv(),
            vec![
                "project",
                "retrieve",
                "start",
                "--metadata",
                "ApexClass",
                "--json"
            ]
        );
        assert!(options.report_argv("0Af000000000001").is_none());
    }

    #[test]
    fn report_argv_keeps_the_target_org() {
        let options = TransferOptions {
            target_org: Some("dev".to_owned()),
            ..TransferOptions::default()
        };
        let expected: Vec<String> = [
            "project",
            "deploy",
            "report",
            "--job-id",
            "0Af000000000001",
            "--target-org",
            "dev",
            "--json",
        ]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
        assert_eq!(options.report_argv("0Af000000000001"), Some(expected));
    }

    #[test]
    fn report_parses_component_failures() {
        let value = serde_json::json!({"result": {
            "id": "0Af000000000001",
            "status": "Failed",
            "components": {"total": 2, "successes": 1, "failed": 1}
        }});
        let report = parse_report(&value);
        assert!(report.is_complete());
        assert_eq!((report.total, report.succeeded, report.failed), (2, 1, 1));

        let detailed = serde_json::json!({"result": {
            "id": "0Af000000000002",
            "status": "Succeeded",
            "components": [{
                "fullName": "Demo", "type": "ApexClass", "status": "Failed", "problem": "compile error"
            }]
        }});
        let report = parse_report(&detailed);
        assert_eq!(report.components[0].problem, "compile error");
        assert_eq!(report.failed, 1);
    }

    #[test]
    fn started_job_id_is_extracted() {
        let value = serde_json::json!({"result": {"id": "0Af000000000003"}});
        assert_eq!(
            parse_started_job_id(&value).as_deref(),
            Some("0Af000000000003")
        );
    }
}
