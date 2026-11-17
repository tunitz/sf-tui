//! Builders for the small set of Salesforce CLI invocations the TUI constructs itself.
//!
//! Keeping these in one place means the command preview and the executed argv cannot drift.

/// The exact `sf apex run test` argv used by the Tests screen.
pub fn apex_test_argv(class_names: &[String], org: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "apex".to_owned(),
        "run".to_owned(),
        "test".to_owned(),
        "--result-format".to_owned(),
        "json".to_owned(),
        "--code-coverage".to_owned(),
    ];
    for class_name in class_names {
        argv.push("--class-names".to_owned());
        argv.push(class_name.clone());
    }
    if let Some(org) = org.filter(|org| !org.is_empty()) {
        argv.push("--target-org".to_owned());
        argv.push(org.to_owned());
    }
    argv.push("--json".to_owned());
    argv
}

/// `sf apex get test` for the run id returned by `apex run test`.
pub fn apex_test_result_argv(run_id: &str, org: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "apex".to_owned(),
        "get".to_owned(),
        "test".to_owned(),
        "--test-run-id".to_owned(),
        run_id.to_owned(),
        "--result-format".to_owned(),
        "json".to_owned(),
        "--code-coverage".to_owned(),
    ];
    if let Some(org) = org.filter(|org| !org.is_empty()) {
        argv.push("--target-org".to_owned());
        argv.push(org.to_owned());
    }
    argv.push("--json".to_owned());
    argv
}

/// Human-readable rendering of the same command for the confirmation dialog and history.
pub fn apex_test_display(class_names: &[String], org: Option<&str>) -> String {
    format!("sf {}", apex_test_argv(class_names, org).join(" "))
}

pub fn config_set(name: &str, value: &str) -> Vec<String> {
    vec![
        "config".to_owned(),
        "set".to_owned(),
        format!("{name}={value}"),
    ]
}

pub fn org_list() -> Vec<String> {
    vec!["org".to_owned(), "list".to_owned(), "--json".to_owned()]
}

pub fn org_display(target: Option<&str>) -> Vec<String> {
    let mut argv = vec!["org".to_owned(), "display".to_owned()];
    if let Some(target) = target.filter(|target| !target.is_empty()) {
        argv.push("--target-org".to_owned());
        argv.push(target.to_owned());
    }
    argv.push("--json".to_owned());
    argv
}

pub fn login_web(alias: Option<&str>, set_default: bool) -> Vec<String> {
    let mut argv = vec!["org".to_owned(), "login".to_owned(), "web".to_owned()];
    if let Some(alias) = alias.filter(|alias| !alias.is_empty()) {
        argv.push("--alias".to_owned());
        argv.push(alias.to_owned());
    }
    if set_default {
        argv.push("--set-default".to_owned());
    }
    argv.push("--json".to_owned());
    argv
}

pub fn logout(target: &str) -> Vec<String> {
    vec![
        "org".to_owned(),
        "logout".to_owned(),
        "--target-org".to_owned(),
        target.to_owned(),
        "--no-prompt".to_owned(),
        "--json".to_owned(),
    ]
}

pub fn create_scratch(alias: &str, definition: Option<&str>, days: &str) -> Vec<String> {
    let mut argv = vec![
        "org".to_owned(),
        "create".to_owned(),
        "scratch".to_owned(),
        "--alias".to_owned(),
        alias.to_owned(),
    ];
    if let Some(definition) = definition.filter(|path| !path.is_empty()) {
        argv.push("--definition-file".to_owned());
        argv.push(definition.to_owned());
    }
    argv.push("--duration-days".to_owned());
    argv.push(days.to_owned());
    argv.push("--json".to_owned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apex_test_command_has_the_same_shape_for_preview_and_run() {
        let classes = vec!["DemoTest".to_owned()];
        let argv = apex_test_argv(&classes, Some("dev"));
        assert!(argv.contains(&"--result-format".to_owned()));
        assert!(argv.contains(&"json".to_owned()));
        assert!(argv.contains(&"--code-coverage".to_owned()));
        assert!(argv.contains(&"--target-org".to_owned()));
        assert!(!argv.contains(&"--wait".to_owned()));
        assert_eq!(
            apex_test_display(&classes, Some("dev")),
            format!("sf {}", argv.join(" "))
        );
    }

    #[test]
    fn apex_get_test_argv_requests_the_run_and_coverage() {
        let argv = apex_test_result_argv("08N000000000001", Some("dev"));
        assert_eq!(
            argv,
            vec![
                "apex",
                "get",
                "test",
                "--test-run-id",
                "08N000000000001",
                "--result-format",
                "json",
                "--code-coverage",
                "--target-org",
                "dev",
                "--json",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        assert!(!apex_test_result_argv("id", None).contains(&"--target-org".to_owned()));
    }

    #[test]
    fn org_display_and_list_include_json() {
        assert_eq!(org_display(None).last().map(String::as_str), Some("--json"));
        assert_eq!(org_list().last().map(String::as_str), Some("--json"));
    }
}
