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
        // Both start commands run without --json: retrieve renders its human stdout, and deploy's
        // job id is parsed from that same stdout. Only the follow-up report command uses --json.
        // Retrieves must not silently overwrite local changes, so --ignore-conflicts is deploy-only.
        if !self.retrieve {
            argv.push("--ignore-conflicts".to_owned());
        }
        argv
    }

    /// The current CLI has `project deploy report` but no `project retrieve report`. Retrieve runs
    /// synchronously; deploy polls this report for structured status, which is why this one query
    /// keeps `--json` even though neither `start` command does.
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

/// Cancel an async deploy by its exact server job id. The target org is the resolved username
/// captured when the deploy started; `--use-most-recent` is deliberately never used because it
/// can cancel an unrelated deploy.
pub fn deploy_cancel_argv(job_id: &str, target_org: &str) -> Option<Vec<String>> {
    if job_id.trim().is_empty() || target_org.trim().is_empty() {
        return None;
    }
    Some(vec![
        "project".to_owned(),
        "deploy".to_owned(),
        "cancel".to_owned(),
        "--job-id".to_owned(),
        job_id.to_owned(),
        "--target-org".to_owned(),
        target_org.to_owned(),
        "--async".to_owned(),
        "--json".to_owned(),
    ])
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

/// Pull the async job id out of a `deploy start` / `retrieve start` JSON response.
pub fn parse_started_job_id(value: &Value) -> Option<String> {
    let result = value.get("result").unwrap_or(value);
    string_field(result, "id")
        .or_else(|| string_field(result, "jobId"))
        .or_else(|| result.get("id").and_then(Value::as_str).map(str::to_owned))
}

/// Fallback for `deploy start` without `--json`: scan human output for a Salesforce job id.
/// Job ids start with `0Af` and are 15 or 18 alphanumeric characters. Output is untrusted, so only
/// whole alphanumeric tokens are considered and no byte offsets into the text are assumed.
pub fn parse_started_job_id_from_text(stdout: &str) -> Option<String> {
    stdout
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .find(|token| {
            let bytes = token.as_bytes();
            (bytes.len() == 15 || bytes.len() == 18)
                && token.starts_with("0Af")
                && bytes.iter().all(u8::is_ascii_alphanumeric)
        })
        .map(str::to_owned)
}

/// Capture a labelled Deploy ID from streamed human output, e.g. `Deploy ID: 0Af...`. Only a
/// validated 15/18-character `0Af` token after the label is accepted.
pub fn parse_labelled_deploy_id(line: &str) -> Option<String> {
    let (_, value) = line.split_once("Deploy ID")?;
    let value = value.trim_start().trim_start_matches(':').trim();
    let token = value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .next()
        .unwrap_or("");
    let bytes = token.as_bytes();
    if (bytes.len() == 15 || bytes.len() == 18)
        && token.starts_with("0Af")
        && bytes.iter().all(u8::is_ascii_alphanumeric)
    {
        Some(token.to_owned())
    } else {
        None
    }
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
                "--ignore-conflicts"
            ]
        );
        assert!(!options.argv().contains(&"--json".to_owned()));
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
            vec!["project", "retrieve", "start", "--metadata", "ApexClass"]
        );
        assert!(!options.argv().contains(&"--json".to_owned()));
        assert!(!options.argv().contains(&"--ignore-conflicts".to_owned()));
        assert!(options.report_argv("0Af000000000001").is_none());
    }

    #[test]
    fn retrieve_keeps_exact_component_names_as_single_arguments() {
        let options = TransferOptions {
            retrieve: true,
            mode: PayloadMode::Metadata,
            metadata_names: vec![
                "ApexClass:Org Only Class".to_owned(),
                "Flow:Demo Flow".to_owned(),
            ],
            target_org: Some("dev-user@example.invalid".to_owned()),
            ..TransferOptions::default()
        };
        assert_eq!(
            options.argv(),
            vec![
                "project",
                "retrieve",
                "start",
                "--metadata",
                "ApexClass:Org Only Class",
                "--metadata",
                "Flow:Demo Flow",
                "--target-org",
                "dev-user@example.invalid",
            ]
        );
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

    #[test]
    fn deploy_cancel_uses_exact_id_and_async_json() {
        let argv = deploy_cancel_argv("0Af000000000001", "dev").expect("argv");
        assert_eq!(
            argv,
            vec![
                "project",
                "deploy",
                "cancel",
                "--job-id",
                "0Af000000000001",
                "--target-org",
                "dev",
                "--async",
                "--json",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        assert!(!argv.iter().any(|part| part.contains("use-most-recent")));
        assert!(deploy_cancel_argv("", "dev").is_none());
        assert!(deploy_cancel_argv("0Af000000000001", "").is_none());
    }

    #[test]
    fn labelled_deploy_id_is_captured_from_streamed_output() {
        assert_eq!(
            parse_labelled_deploy_id("Deploy ID: 0Af000000000001").as_deref(),
            Some("0Af000000000001")
        );
        assert_eq!(
            parse_labelled_deploy_id("  Deploy ID: 0AfAAAAAAAAAAAAAAA ").as_deref(),
            Some("0AfAAAAAAAAAAAAAAA")
        );
        assert_eq!(parse_labelled_deploy_id("Deploy ID: nope"), None);
        assert_eq!(parse_labelled_deploy_id("no label here"), None);
    }

    #[test]
    fn started_job_id_is_extracted_from_human_output() {
        let fifteen = format!("0Af{}", "A".repeat(12));
        let eighteen = format!("0Af{}", "B".repeat(15));
        let human = format!("Deploy ID: {fifteen}\nStatus: Queued — Deploy ID: {eighteen}\n");
        assert_eq!(
            parse_started_job_id_from_text(&human).as_deref(),
            Some(fifteen.as_str())
        );
        assert_eq!(parse_started_job_id_from_text("nothing to see here"), None);
        assert_eq!(parse_started_job_id_from_text("0Afshort"), None);
        assert_eq!(parse_started_job_id_from_text("0AfAAAAAAAAAAAAAAAA"), None);
    }
}
