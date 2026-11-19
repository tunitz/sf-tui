//! Builders for the small set of Salesforce CLI invocations the TUI constructs itself.
//!
//! Keeping these in one place means the command preview and the executed argv cannot drift.

use std::path::Path;

use super::create::{CreateDraft, CreateKind, FieldKey};

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

/// `sf org list metadata --metadata-type <type> --target-org <username> --json`.
///
/// The username (never the alias) is passed to the CLI; aliases are only for display labels.
pub fn org_list_metadata(metadata_type: &str, org: &str) -> Vec<String> {
    vec![
        "org".to_owned(),
        "list".to_owned(),
        "metadata".to_owned(),
        "--metadata-type".to_owned(),
        metadata_type.to_owned(),
        "--target-org".to_owned(),
        org.to_owned(),
        "--json".to_owned(),
    ]
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

/// Build the exact argv for a Create Metadata generator.
///
/// `output_dir` is the private per-job staging directory: every generator that accepts an output
/// flag writes there, and sftui publishes the result into the project with exclusive creation.
/// The interactive Custom Object generator gets no output flag and is never run by sftui.
pub fn create_metadata_argv(draft: &CreateDraft, output_dir: &Path) -> Vec<String> {
    let dir = output_dir.to_string_lossy().into_owned();
    let name = draft.get(FieldKey::Name).trim();
    let mut argv: Vec<String> = match draft.kind {
        CreateKind::ApexClass => {
            let mut argv = vec![
                "template".to_owned(),
                "generate".to_owned(),
                "apex".to_owned(),
                "class".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
            ];
            let template = draft.get(FieldKey::ApexTemplate);
            if !template.trim().is_empty() {
                argv.push("--template".to_owned());
                argv.push(template.trim().to_owned());
            }
            argv
        }
        CreateKind::ApexTrigger => vec![
            "template".to_owned(),
            "generate".to_owned(),
            "apex".to_owned(),
            "trigger".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
            "--sobject".to_owned(),
            draft.get(FieldKey::SObject).trim().to_owned(),
            "--event".to_owned(),
            draft.get(FieldKey::Event).trim().to_owned(),
        ],
        CreateKind::Lwc | CreateKind::Aura => vec![
            "template".to_owned(),
            "generate".to_owned(),
            "lightning".to_owned(),
            "component".to_owned(),
            "--type".to_owned(),
            if draft.kind == CreateKind::Lwc {
                "lwc".to_owned()
            } else {
                "aura".to_owned()
            },
            "--name".to_owned(),
            name.to_owned(),
        ],
        CreateKind::LightningApp => vec![
            "template".to_owned(),
            "generate".to_owned(),
            "lightning".to_owned(),
            "app".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
        ],
        CreateKind::FlexiPage => {
            let mut argv = vec![
                "template".to_owned(),
                "generate".to_owned(),
                "flexipage".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
                "--template".to_owned(),
                draft.get(FieldKey::FlexiTemplate).trim().to_owned(),
            ];
            let sobject = draft.get(FieldKey::SObject).trim();
            if draft.get(FieldKey::FlexiTemplate) == "RecordPage" && !sobject.is_empty() {
                argv.push("--sobject".to_owned());
                argv.push(sobject.to_owned());
            }
            argv
        }
        CreateKind::VisualforcePage => {
            let mut argv = vec![
                "template".to_owned(),
                "generate".to_owned(),
                "visualforce".to_owned(),
                "page".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
                "--label".to_owned(),
                draft.get(FieldKey::Label).trim().to_owned(),
            ];
            argv.push("--output-dir".to_owned());
            argv.push(dir);
            return argv;
        }
        CreateKind::VisualforceComponent => {
            let mut argv = vec![
                "template".to_owned(),
                "generate".to_owned(),
                "visualforce".to_owned(),
                "component".to_owned(),
                "--name".to_owned(),
                name.to_owned(),
                "--label".to_owned(),
                draft.get(FieldKey::Label).trim().to_owned(),
            ];
            argv.push("--output-dir".to_owned());
            argv.push(dir);
            return argv;
        }
        CreateKind::StaticResource => vec![
            "template".to_owned(),
            "generate".to_owned(),
            "static-resource".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
            "--type".to_owned(),
            draft.get(FieldKey::ContentType).trim().to_owned(),
        ],
        CreateKind::CustomMetadataType => {
            let mut argv = vec![
                "cmdt".to_owned(),
                "generate".to_owned(),
                "object".to_owned(),
                "--type-name".to_owned(),
                name.to_owned(),
            ];
            for (flag, key) in [
                ("--label", FieldKey::Label),
                ("--plural-label", FieldKey::PluralLabel),
            ] {
                let value = draft.get(key).trim();
                if !value.is_empty() {
                    argv.push(flag.to_owned());
                    argv.push(value.to_owned());
                }
            }
            argv.push("--visibility".to_owned());
            argv.push(draft.get(FieldKey::Visibility).trim().to_owned());
            // The cmdt generator uses `--output-directory`, not `--output-dir`.
            argv.push("--output-directory".to_owned());
            argv.push(dir);
            return argv;
        }
        CreateKind::AnalyticsTemplate => vec![
            "template".to_owned(),
            "generate".to_owned(),
            "analytics".to_owned(),
            "template".to_owned(),
            "--name".to_owned(),
            name.to_owned(),
        ],
        CreateKind::CustomObject => {
            // Interactive in the CLI. The caller copies this command; it is never queued.
            return vec![
                "schema".to_owned(),
                "generate".to_owned(),
                "sobject".to_owned(),
                "--label".to_owned(),
                draft.get(FieldKey::Label).trim().to_owned(),
            ];
        }
    };
    argv.push("--output-dir".to_owned());
    argv.push(dir);
    argv
}

/// Human-readable `sf …` rendering of the same argv, for display and the job record only.
pub fn create_metadata_display(argv: &[String]) -> String {
    format!("sf {}", argv.join(" "))
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

    #[test]
    fn org_list_metadata_uses_the_username_for_the_cli() {
        let argv = org_list_metadata("ApexClass", "dev-user@example.invalid");
        assert_eq!(
            argv,
            vec![
                "org",
                "list",
                "metadata",
                "--metadata-type",
                "ApexClass",
                "--target-org",
                "dev-user@example.invalid",
                "--json",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        // A type is one argv element; the user's alias is never sent to the CLI.
        let bundle = org_list_metadata("LightningComponentBundle", "dev-user@example.invalid");
        assert!(bundle.contains(&"LightningComponentBundle".to_owned()));
        assert!(!bundle.iter().any(|part| part.contains('*')));
        assert!(!bundle.iter().any(|part| part == "dev-alias"));
    }
}
