//! Integration coverage using a synthetic `sf` stub, plus a render smoke test for every screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sftui::app::{App, OutputRecord};
use sftui::config::Config;
use sftui::sf::detect::SfInstallation;
use sftui::sf::runner::{self, OutputStream, SfEvent};
use sftui::sf::{Startup, StartupEvent};
use sftui::ui;
use tokio::sync::mpsc;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn assert_synthetic_environment() {
    for key in [
        "SF_ACCESS_TOKEN",
        "SFDX_AUTH_URL",
        "SF_PASSWORD",
        "SF_CLIENT_SECRET",
    ] {
        assert!(
            std::env::var_os(key).is_none(),
            "refusing to run stub tests while {key} is set in this environment"
        );
    }
}

fn stub_installation() -> SfInstallation {
    SfInstallation {
        executable: fixture(if cfg!(windows) {
            "stub_sf.bat"
        } else {
            "stub_sf.sh"
        }),
        source: "test fixture".to_owned(),
    }
}

#[tokio::test]
async fn stub_version_probe_returns_synthetic_version() {
    assert_synthetic_environment();
    let outcome = runner::capture(
        &stub_installation().executable,
        &["version".to_owned(), "--json".to_owned()],
        Duration::from_secs(5),
    )
    .await
    .expect("probe");
    assert!(outcome.succeeded());
    let value = runner::parse_json_document(&outcome.stdout).expect("json");
    assert_eq!(value["result"]["version"], "99.0.0");
}

#[tokio::test]
async fn stub_lists_curated_org_metadata_for_the_username() {
    assert_synthetic_environment();
    for metadata_type in ["ApexClass", "Flow", "LightningComponentBundle"] {
        let argv =
            sftui::sf::commands::org_list_metadata(metadata_type, "test-scratch@example.invalid");
        let outcome = runner::capture(
            &stub_installation().executable,
            &argv,
            Duration::from_secs(5),
        )
        .await
        .expect("org list metadata");
        assert!(outcome.succeeded(), "{metadata_type}: {outcome:?}");
        let value = runner::parse_json_document(&outcome.stdout).expect("json");
        let components = sftui::sf::metadata::parse_org_components(&value, metadata_type);
        assert!(
            components
                .iter()
                .any(|item| item.identity().starts_with(metadata_type)),
            "{metadata_type} must produce synthetic rows"
        );
        assert!(
            components
                .iter()
                .all(|item| !item.identity().contains("SYNTHETIC-DO-NOT-USE"))
        );
    }
}

#[tokio::test]
async fn runner_streams_stdout_and_reports_completion() {
    assert_synthetic_environment();
    let (tx, mut rx) = mpsc::channel::<SfEvent>(16);
    let argv = vec!["org".to_owned(), "list".to_owned(), "--json".to_owned()];
    let job = runner::spawn_sf(&stub_installation(), &argv, 42, tx).expect("spawn");
    let mut finished = None;
    let mut saw_stdout_line = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            match event {
                SfEvent::Line(line) if line.stream == OutputStream::Stdout => {
                    saw_stdout_line |= line.text.contains("test-scratch");
                }
                SfEvent::Lines(lines) => {
                    saw_stdout_line |= lines.iter().any(|line| {
                        line.stream == OutputStream::Stdout && line.text.contains("test-scratch")
                    });
                }
                SfEvent::Finished { job_id, outcome } => {
                    finished = Some((job_id, outcome));
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("job completed in time");
    let (job_id, outcome) = finished.expect("final event");
    assert_eq!(job_id, 42);
    assert!(outcome.succeeded());
    assert!(saw_stdout_line);
    let value = runner::parse_json_document(&outcome.stdout).expect("json");
    assert_eq!(sftui::sf::org::parse_org_list(&value).len(), 1);
    let _ = job.join().await;
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_a_synthetic_stub_job_reaps_it_quickly() {
    assert_synthetic_environment();
    let (tx, _rx) = mpsc::channel::<SfEvent>(16);
    let job = runner::spawn_sf(&stub_installation(), &["slow".to_owned()], 91, tx)
        .expect("spawn slow stub");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let started = Instant::now();
    let _outcome = tokio::time::timeout(Duration::from_secs(5), job.cancel_and_join())
        .await
        .expect("stub job reaped after stop");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop must reap the child instead of waiting for it to finish"
    );
}

#[tokio::test]
async fn staged_startup_reports_progress_without_catalog_probe() {
    assert_synthetic_environment();
    let dir = tempfile::tempdir().expect("temp dir");
    let (tx, mut rx) = mpsc::unbounded_channel::<StartupEvent>();
    let task = tokio::spawn(sftui::sf::probe_with(
        dir.path().to_path_buf(),
        tx,
        Some(stub_installation()),
    ));
    let mut saw_stage = false;
    let mut saw_detected = false;
    let mut saw_version = false;
    let mut saw_org = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            match event {
                StartupEvent::Stage(_) => saw_stage = true,
                StartupEvent::Detected(_) => saw_detected = true,
                StartupEvent::Version(version) => saw_version = version == "99.0.0",
                StartupEvent::CurrentOrg(org) => saw_org = org == "test-scratch",
                StartupEvent::Finished => break,
                StartupEvent::Notice(_) => {}
            }
        }
    })
    .await
    .expect("startup probe finished");
    let startup = task.await.expect("probe task");
    assert!(saw_stage && saw_detected && saw_version && saw_org);
    assert_eq!(startup.version.as_deref(), Some("99.0.0"));
}

#[tokio::test]
async fn startup_never_announces_a_catalog_refresh() {
    assert_synthetic_environment();
    let dir = tempfile::tempdir().expect("temp dir");
    let (tx, mut rx) = mpsc::unbounded_channel::<StartupEvent>();
    let task = tokio::spawn(sftui::sf::probe_with(
        dir.path().to_path_buf(),
        tx,
        Some(stub_installation()),
    ));
    let mut noisy = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            if matches!(event, StartupEvent::Stage("Loading command catalog"))
                || matches!(event, StartupEvent::Notice(ref text) if text.contains("catalog"))
            {
                noisy = true;
            }
            if matches!(event, StartupEvent::Finished) {
                break;
            }
        }
    })
    .await
    .expect("probe finished");
    let _ = task.await.expect("probe task");
    assert!(!noisy);
}

#[tokio::test]
async fn failed_stub_command_captures_stderr_and_exit_code() {
    assert_synthetic_environment();
    let outcome = runner::capture(
        &stub_installation().executable,
        &["echo-error".to_owned()],
        Duration::from_secs(5),
    )
    .await
    .expect("capture");
    assert_eq!(outcome.exit_code, Some(3));
    assert!(outcome.stderr.contains("synthetic failure"));
}

fn apex_report_argv(run_id: &str) -> Vec<String> {
    vec![
        "apex".to_owned(),
        "get".to_owned(),
        "test".to_owned(),
        "--test-run-id".to_owned(),
        run_id.to_owned(),
        "--result-format".to_owned(),
        "json".to_owned(),
        "--code-coverage".to_owned(),
        "--target-org".to_owned(),
        "test-scratch".to_owned(),
        "--json".to_owned(),
    ]
}

#[tokio::test]
async fn stub_failed_report_with_exit_100_shows_every_class_and_method_in_jobs_and_history() {
    assert_synthetic_environment();
    let outcome = runner::capture(
        &stub_installation().executable,
        &apex_report_argv("08N000000000002"),
        Duration::from_secs(5),
    )
    .await
    .expect("capture");
    assert_eq!(outcome.exit_code, Some(100));
    assert!(!outcome.succeeded());
    let value = runner::parse_json_document(&outcome.stdout).expect("json");
    let run = sftui::sf::test_results::parse_terminal_report(
        &value,
        "sf apex get test",
        "08N000000000002",
    )
    .expect("terminal report");
    assert_eq!(run.outcome, "Failed");
    let view = sftui::view::apex_result(&run);
    assert!(!view.ok);
    assert!(view.summary.starts_with("Tests failed · "));
    let methods = view
        .sections
        .iter()
        .find(|section| section.title == "TEST METHODS")
        .expect("method section");
    assert_eq!(methods.rows.len(), 3);
    assert!(methods.rows.iter().any(
        |row| row.label == "BetaTest.bad" && row.detail.contains("synthetic assertion failed")
    ));

    // The same report drives the F8 history detail view.
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.tab = sftui::app::Tab::Tests;
    app.test_history
        .runs
        .push(sftui::state_store::SavedTestRun {
            id: "stub-failed-report".to_owned(),
            finished_at: 1_700_000_000_000,
            org: Some("test-scratch".to_owned()),
            command: "sf apex run test".to_owned(),
            run,
        });
    app.modal = Some(sftui::app::Modal::TestHistory {
        selected: 0,
        list_scroll: 0,
        detail_scroll: 0,
        focus: sftui::app::TestHistoryFocus::Detail,
        confirm_clear: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(180, 30)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    for expected in [
        "AlphaTest",
        "BetaTest.bad",
        "synthetic assertion failed",
        "Tests failed",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}: {rendered}"
        );
    }
}

#[tokio::test]
async fn stub_exit_100_error_payload_is_not_a_terminal_report() {
    assert_synthetic_environment();
    let outcome = runner::capture(
        &stub_installation().executable,
        &apex_report_argv("08N000000000003"),
        Duration::from_secs(5),
    )
    .await
    .expect("capture");
    assert_eq!(outcome.exit_code, Some(100));
    let value = runner::parse_json_document(&outcome.stdout).expect("json");
    assert!(
        sftui::sf::test_results::parse_terminal_report(
            &value,
            "sf apex get test",
            "08N000000000003",
        )
        .is_none(),
        "a CLI error envelope must never become a test result"
    );
}

#[test]
fn every_screen_renders_without_panicking() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: Some("99.0.0".to_owned()),
            current_org: Some("test-scratch".to_owned()),
            notices: vec!["synthetic notice".to_owned()],
        },
        Config::default(),
        std::path::PathBuf::from("/tmp/sftui-test"),
    );
    app.output_state.records.push(OutputRecord {
        id: 1,
        command: "sf project deploy start --json".to_owned(),
        purpose: "Deploy".to_owned(),
        task: sftui::app::TaskKind::Deploy,
        executable: "sf".into(),
        argv: vec!["project".to_owned(), "deploy".to_owned()],
        lines: Vec::new(),
        line_bytes: 0,
        exit_code: None,
        finished: false,
        queued: false,
        started_at: Some(Instant::now()),
        ended_at: None,
        json: Some(serde_json::json!({"result":{"status":"ok"}})),
        result: None,
        pending_report: false,
        render_revision: 0,
        lines_truncated: false,
        lifecycle: sftui::app::JobLifecycle::Finished,
        invocation: None,
        cleanup_error: None,
    });
    app.output_state.records.push(OutputRecord {
        id: 2,
        command: "sf apex run test --json".to_owned(),
        purpose: "apex tests".to_owned(),
        task: sftui::app::TaskKind::Tests,
        executable: "sf".into(),
        argv: vec!["apex".to_owned(), "run".to_owned(), "test".to_owned()],
        lines: Vec::new(),
        line_bytes: 0,
        exit_code: Some(0),
        finished: true,
        queued: false,
        started_at: Some(Instant::now()),
        ended_at: Some(Instant::now()),
        json: None,
        result: Some(sftui::app::ResultView {
            kind: sftui::app::ResultKind::ApexTests,
            title: "Apex tests".to_owned(),
            status: "Passed".to_owned(),
            ok: true,
            summary: "2 run · 2 passed · 0 failed · 0 skipped · coverage 81%".to_owned(),
            items: Vec::new(),
            coverage: (0..8usize)
                .map(|index| sftui::app::ResultItem {
                    ok: true,
                    label: format!("Demo{index}"),
                    detail: format!("{}% · {}/10 lines", 40 + index * 5, 4 + index),
                    progress: Some((4 + index, 10)),
                })
                .collect(),
            sections: Vec::new(),
        }),
        pending_report: false,
        render_revision: 0,
        lines_truncated: false,
        lifecycle: sftui::app::JobLifecycle::Finished,
        invocation: None,
        cleanup_error: None,
    });
    for (width, height) in [(120, 40), (80, 24), (60, 16), (48, 12)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        for tab in app.screens() {
            app.tab = tab;
            terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
            let buffer = terminal.backend().buffer();
            let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(
                rendered.trim().len() > 20,
                "empty frame at {width}x{height} on {tab:?}"
            );
            if (width, height) == (60, 16) && tab == sftui::app::Tab::Home {
                assert!(rendered.contains("[1] Retrieve"), "numeric menu missing");
                assert!(rendered.contains("MENU"), "menu heading missing");
            }
            if (width, height) == (60, 16) && tab == sftui::app::Tab::Tests {
                assert!(
                    !rendered.to_lowercase().contains("coverage"),
                    "Tests screen must not render a coverage pane"
                );
            }
            if (width, height) == (120, 40) && tab == sftui::app::Tab::Home {
                assert!(rendered.contains("[1] Retrieve"), "numeric menu missing");
                assert!(
                    !rendered.contains("Organization"),
                    "org menu entry must be gone"
                );
                assert!(
                    !rendered.contains("Run command"),
                    "run-command menu entry must be gone"
                );
                assert!(rendered.contains("[F2] jobs"), "key hints missing");
            }
        }
        for modal in [
            sftui::app::Modal::Help { scroll: 0 },
            sftui::app::Modal::Catalog {
                query: String::new(),
                selected: 0,
                scroll: 0,
            },
            sftui::app::Modal::Message {
                title: "Notice".to_owned(),
                message: "Synthetic modal".to_owned(),
            },
            sftui::app::Modal::Confirm {
                title: "Confirm".to_owned(),
                message: "Synthetic confirmation".to_owned(),
                action: sftui::app::PendingAction::None,
            },
            sftui::app::Modal::SwitchOrg { selected: 0 },
            sftui::app::Modal::Prompt {
                title: "Prompt".to_owned(),
                kind: sftui::app::PromptKind::TargetOrg,
                fields: vec![
                    sftui::app::PromptField::new("First", "value one"),
                    sftui::app::PromptField::new("Second", "value two"),
                    sftui::app::PromptField::new("Third", "value three"),
                ],
                focus: 0,
            },
            sftui::app::Modal::Jobs {
                selected_id: None,
                focus: sftui::app::JobsFocus::List,
                scroll: 0,
                follow: true,
                detail_view: sftui::app::JobDetailView::Summary,
            },
            sftui::app::Modal::RerunReview {
                job_id: 1,
                org: String::new(),
                org_required: true,
            },
            sftui::app::Modal::TestConfirm {
                entries: Vec::new(),
                focus: 0,
                show_command: false,
                org: None,
            },
            sftui::app::Modal::TransferConfirm {
                retrieve: false,
                entries: Vec::new(),
                focus: 0,
                show_command: false,
                options: sftui::sf::deploy::TransferOptions::default(),
            },
        ] {
            app.modal = Some(modal);
            terminal
                .draw(|frame| ui::draw(frame, &app))
                .expect("draw modal");
        }
        app.modal = None;
    }
}

#[test]
fn tiny_footer_keeps_key_hints_and_moves_status_to_the_header() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: Some("99.0.0".to_owned()),
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        std::path::PathBuf::from("/tmp/sftui-test"),
    );
    app.tab = sftui::app::Tab::Home;
    app.error("tiny status");

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");

    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    let header = row(0);
    let footer = row(buffer.area.height - 1);
    assert!(
        header.contains("tiny status"),
        "status missing from tiny header: {header}"
    );
    assert!(
        footer.contains("[F2]") || footer.contains("[?]") || footer.contains("[q]"),
        "key hints missing from tiny footer: {footer}"
    );
    assert!(
        !footer.contains("tiny status"),
        "status must not replace the tiny footer: {footer}"
    );
}

#[test]
fn header_is_left_aligned_at_all_sizes() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: Some("99.0.0".to_owned()),
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.startup_stage.done = true;
    app.status.clear();
    app.tab = sftui::app::Tab::Home;

    for (width, height) in [(80, 24), (60, 12)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let header: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            header.contains("SFTUI"),
            "brand missing at {width}x{height}: {header}"
        );
        assert!(
            header.contains("no default org"),
            "org context missing at {width}x{height}: {header}"
        );
        assert!(
            header.contains("ready"),
            "activity missing at {width}x{height}: {header}"
        );
        let context_at = header.find("no default org").expect("org context position");
        let activity_at = header.find("ready").expect("activity position");
        assert!(
            activity_at > context_at,
            "activity must follow the org context in one left-aligned row: {header}"
        );
    }
}

#[test]
fn prompt_overlay_renders_with_a_cursor() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        std::path::PathBuf::from("/tmp/sftui-test"),
    );
    app.modal = Some(sftui::app::Modal::Prompt {
        title: "Synthetic prompt".to_owned(),
        kind: sftui::app::PromptKind::TargetOrg,
        fields: vec![sftui::app::PromptField::new("Alias", "test-scratch")],
        focus: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(90, 25)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("test-scratch"));
}

#[test]
fn help_modal_renders_as_a_table_at_multiple_sizes() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.modal = Some(sftui::app::Modal::Help { scroll: 0 });

    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("draw help");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("Key") && rendered.contains("Action"),
            "help table header missing at {width}x{height}"
        );
        assert!(
            rendered.contains("F2"),
            "known key row missing at {width}x{height}"
        );
        if width == 80 {
            assert!(
                rendered.contains("open the command catalog"),
                "F7 help row missing at {width}x{height}"
            );
            assert!(
                rendered.contains("Apex test history (Tests screen)"),
                "F8 help row missing at {width}x{height}"
            );
        }
        assert!(
            !rendered.contains("Menu r/d/c/t/o/e"),
            "stale menu row at {width}x{height}"
        );
    }
}

#[test]
fn f7_catalog_renders_at_small_and_roomy_sizes() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
    assert!(
        matches!(app.modal, Some(sftui::app::Modal::Catalog { .. })),
        "F7 must open the catalog"
    );
    for character in "org list".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("draw catalog");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("command catalog"),
            "catalog title missing at {width}x{height}"
        );
        assert!(
            rendered.contains("org list"),
            "filtered command row missing at {width}x{height}"
        );
        assert!(
            rendered.contains("key flags"),
            "detail pane missing at {width}x{height}"
        );
    }
}

#[test]
fn create_wizard_renders_grouped_types_and_external_cli_hint() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.modal = Some(sftui::app::Modal::CreateWizard {
        state: sftui::app::CreateWizardState::Choose {
            query: String::new(),
            results: sftui::sf::create::ALL_TYPES.to_vec(),
            selected: 0,
            searching: false,
            searched_query: Some(String::new()),
        },
    });

    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("Apex Class"),
            "grouped type missing at {width}x{height}"
        );
        assert!(
            rendered.contains("External CLI"),
            "interactive-only type must be marked at {width}x{height}"
        );
        assert!(
            !rendered.contains("Org"),
            "the irrelevant Org field must be gone at {width}x{height}"
        );
    }
}

#[test]
fn narrow_terminal_truncates_long_org_names_without_panicking() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.org_state.orgs.push(sftui::sf::org::OrgSummary {
        username: "a-very-long-username-that-exceeds-the-narrow-pane@example.com".to_owned(),
        aliases: vec!["an-extremely-long-alias-that-will-not-fit".to_owned()],
        org_id: "00D000000000000".to_owned(),
        is_default: false,
        is_devhub: false,
        connected_status: String::new(),
        instance_url: String::new(),
        scratch: true,
    });
    app.modal = Some(sftui::app::Modal::SwitchOrg { selected: 0 });
    let mut terminal = Terminal::new(TestBackend::new(48, 12)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        rendered.contains('…'),
        "truncation marker missing: {rendered}"
    );
}

#[test]
fn tests_screen_has_no_coverage_pane_at_small_and_roomy_sizes() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            current_org: None,
            notices: Vec::new(),
        },
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.tab = sftui::app::Tab::Tests;
    app.test_search
        .results
        .push(sftui::sf::metadata::SuggestionItem::new(
            "VisibleTestClass",
            "VisibleTestClass.cls",
        ));
    app.output_state.records.push(OutputRecord {
        id: 1,
        command: "sf apex run test --json".to_owned(),
        purpose: "apex tests".to_owned(),
        task: sftui::app::TaskKind::Tests,
        executable: "sf".into(),
        argv: vec!["apex".to_owned()],
        lines: Vec::new(),
        line_bytes: 0,
        exit_code: Some(0),
        finished: true,
        queued: false,
        started_at: Some(Instant::now()),
        ended_at: Some(Instant::now()),
        json: None,
        result: Some(sftui::app::ResultView {
            kind: sftui::app::ResultKind::ApexTests,
            title: "Apex tests".to_owned(),
            status: "Passed".to_owned(),
            ok: true,
            summary: "2 run · 2 passed · 0 failed · coverage 81%".to_owned(),
            items: Vec::new(),
            coverage: (0..8usize)
                .map(|index| sftui::app::ResultItem {
                    ok: true,
                    label: format!("Demo{index}"),
                    detail: format!("{}% · {}/10 lines", 40 + index * 5, 4 + index),
                    progress: Some((4 + index, 10)),
                })
                .collect(),
            sections: Vec::new(),
        }),
        pending_report: false,
        render_revision: 0,
        lines_truncated: false,
        lifecycle: sftui::app::JobLifecycle::Finished,
        invocation: None,
        cleanup_error: None,
    });
    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            !rendered.to_lowercase().contains("coverage"),
            "Tests body must not render a coverage pane at {width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("VisibleTestClass"),
            "test list has no usable rows at {width}x{height}"
        );
    }
}

#[test]
fn tests_header_shows_active_apex_run_count() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.tab = sftui::app::Tab::Tests;
    let render = |app: &App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    };
    let test_record = |id, finished, pending_report| OutputRecord {
        id,
        command: "sf apex run test".to_owned(),
        purpose: "apex tests".to_owned(),
        task: sftui::app::TaskKind::Tests,
        executable: "sf".into(),
        argv: vec!["apex".to_owned()],
        lines: Vec::new(),
        line_bytes: 0,
        exit_code: Some(0),
        finished,
        queued: false,
        started_at: Some(Instant::now()),
        ended_at: None,
        json: None,
        result: None,
        pending_report,
        render_revision: 0,
        lines_truncated: false,
        lifecycle: sftui::app::JobLifecycle::Finished,
        invocation: None,
        cleanup_error: None,
    };

    assert!(
        !render(&app).contains("test class running"),
        "no running count when no Apex test job is active"
    );
    app.output_state.records.push(test_record(1, false, false));
    let one = render(&app);
    assert!(one.contains("(1 test class running)"), "header: {one}");

    app.output_state.records.push(test_record(2, true, true));
    let two = render(&app);
    assert!(two.contains("(2 test classes running)"), "header: {two}");
}

fn saved_apex_run(id: &str, outcome: &str) -> sftui::state_store::SavedTestRun {
    sftui::state_store::SavedTestRun {
        id: id.to_owned(),
        finished_at: 1_700_000_000_000,
        org: Some("scratch-org".to_owned()),
        command: "sf apex run test".to_owned(),
        run: sftui::sf::test_results::TestRun {
            id: "08N000000000001".to_owned(),
            at: 1_700_000_000,
            command: "sf apex run test".to_owned(),
            outcome: outcome.to_owned(),
            tests_run: 3,
            passing: 2,
            failing: 1,
            skipped: 0,
            coverage: "81%".to_owned(),
            tests: vec![sftui::sf::test_results::TestResultRow {
                name: "DemoTest.bad".to_owned(),
                outcome: "Fail".to_owned(),
                message: "assertion failed".to_owned(),
            }],
            failures: vec![sftui::sf::test_results::TestFailure {
                name: "DemoTest.bad".to_owned(),
                message: "assertion failed".to_owned(),
            }],
            coverages: Vec::new(),
        },
    }
}

#[test]
fn f8_history_modal_renders_at_small_and_roomy_sizes() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.tab = sftui::app::Tab::Tests;
    app.test_history.runs = vec![
        saved_apex_run("job-2", "Failed"),
        saved_apex_run("job-1", "Passed"),
    ];
    app.modal = Some(sftui::app::Modal::TestHistory {
        selected: 0,
        list_scroll: 0,
        detail_scroll: 0,
        focus: sftui::app::TestHistoryFocus::List,
        confirm_clear: false,
    });

    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("draw history");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("Apex test history"),
            "history title missing at {width}x{height}"
        );
        assert!(
            rendered.contains("scratch-org"),
            "org column missing at {width}x{height}"
        );
        assert!(
            rendered.contains("2023-11-14"),
            "date/time column missing at {width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("assertion failed") || rendered.contains("Failed"),
            "result pane missing at {width}x{height}"
        );
        assert!(
            rendered.to_lowercase().contains("delete") && rendered.to_lowercase().contains("clear"),
            "modal hints missing at {width}x{height}"
        );
    }
}

#[test]
fn f4_switcher_shows_cached_org_detail() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    app.org_state.orgs.push(sftui::sf::org::OrgSummary {
        username: "demo@example.invalid".to_owned(),
        aliases: vec!["demo".to_owned()],
        org_id: "00D000000000001".to_owned(),
        is_default: true,
        is_devhub: false,
        connected_status: "Connected".to_owned(),
        instance_url: "https://example.invalid".to_owned(),
        scratch: true,
    });
    app.org_state.detail_cache.insert(
        "demo@example.invalid".to_owned(),
        sftui::sf::org::OrgDetail {
            fields: vec![("Id".to_owned(), "00D000000000001".to_owned())],
            instance_url: "https://cached.example.invalid".to_owned(),
            username: "demo@example.invalid".to_owned(),
        },
    );
    app.modal = Some(sftui::app::Modal::SwitchOrg { selected: 0 });

    for (width, height) in [(80, 24), (60, 16)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| ui::draw(frame, &app))
            .expect("draw org switcher");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("https://cached.example.invalid"),
            "cached instance URL missing at {width}x{height}"
        );
        assert!(
            rendered.contains("00D000000000001"),
            "cached org id missing at {width}x{height}"
        );
        assert!(
            rendered.contains("details"),
            "d details hint missing at {width}x{height}"
        );
    }
}

#[test]
fn f8_hint_is_shown_only_on_the_tests_screen() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test"),
    );
    let render = |app: &App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    };
    let home = render(&app);
    assert!(
        !home.contains("[F8]"),
        "F8 history key must not show outside Tests"
    );
    assert!(
        home.contains("[F7]"),
        "F7 catalog hint missing on Home: {home}"
    );
    app.tab = sftui::app::Tab::Tests;
    let tests = render(&app);
    assert!(tests.contains("[F8]"), "F8 hint missing on Tests: {tests}");
    assert!(
        tests.contains("[F7]"),
        "F7 catalog hint missing on Tests: {tests}"
    );
    assert!(
        tests.to_lowercase().contains("history"),
        "F8 history label missing"
    );
}

#[test]
fn home_has_one_combined_metadata_entry_and_no_separate_deploy_entry() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test-home"),
    );
    app.tab = sftui::app::Tab::Home;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        rendered.contains("[1] Retrieve/Deploy Metadata"),
        "combined Home entry missing: {rendered}"
    );
    assert!(
        !rendered.contains("[2] Deploy Metadata"),
        "Deploy must not have a separate Home entry"
    );
    assert!(
        !rendered.contains("[4] Apex Tests"),
        "Apex Tests must be [3] after the menu merge"
    );
}

#[test]
fn org_browse_modal_renders_and_preserves_scoped_selection() {
    assert_synthetic_environment();
    let _runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = _runtime.enter();
    let mut app = App::new(
        Startup::pending(),
        Config::default(),
        PathBuf::from("/tmp/sftui-test-browse"),
    );
    app.tab = sftui::app::Tab::Retrieve;
    app.org_state.orgs.push(sftui::sf::org::OrgSummary {
        username: "dev@example.invalid".to_owned(),
        aliases: vec!["dev-alias".to_owned()],
        ..Default::default()
    });
    app.org_browse.username = "dev@example.invalid".to_owned();
    app.org_browse.org_label = "dev-alias".to_owned();
    app.org_browse.metadata_type = "ApexClass".to_owned();
    app.org_browse.rows = vec![sftui::sf::metadata::OrgComponent {
        metadata_type: "ApexClass".to_owned(),
        full_name: "OrgOnly".to_owned(),
    }];
    app.org_browse.checked = vec!["ApexClass:OrgOnly".to_owned()];
    app.modal = Some(sftui::app::Modal::OrgBrowse);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &app))
        .expect("draw browse");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("ORG"), "org column missing: {rendered}");
    assert!(
        rendered.contains("Org only"),
        "org-only label missing: {rendered}"
    );
    assert!(
        rendered.contains("ApexClass:OrgOnly"),
        "component row missing: {rendered}"
    );
}

#[tokio::test]
async fn stub_answers_create_metadata_generator_commands() {
    assert_synthetic_environment();
    let installation = stub_installation();
    let destination = PathBuf::from("/tmp/sftui-create-stub");
    let mut draft = sftui::sf::create::CreateDraft::new(
        sftui::sf::create::CreateKind::ApexClass,
        sftui::sf::create::Destination {
            label: "force-app/main/default".to_owned(),
            relative: PathBuf::from("force-app/main/default"),
        },
    );
    draft.set(sftui::sf::create::FieldKey::Name, "DemoController");
    for kind in [
        sftui::sf::create::CreateKind::ApexClass,
        sftui::sf::create::CreateKind::ApexTrigger,
        sftui::sf::create::CreateKind::Lwc,
        sftui::sf::create::CreateKind::LightningApp,
        sftui::sf::create::CreateKind::FlexiPage,
        sftui::sf::create::CreateKind::VisualforcePage,
        sftui::sf::create::CreateKind::VisualforceComponent,
        sftui::sf::create::CreateKind::StaticResource,
        sftui::sf::create::CreateKind::CustomMetadataType,
        sftui::sf::create::CreateKind::AnalyticsTemplate,
    ] {
        let mut spec = draft.clone();
        spec.kind = kind;
        let argv = sftui::sf::commands::create_metadata_argv(&spec, &destination);
        let outcome = runner::capture(&installation.executable, &argv, Duration::from_secs(5))
            .await
            .expect("stub generator call");
        assert!(outcome.succeeded(), "{kind:?}: {outcome:?}");
    }
    let mut interactive = draft.clone();
    interactive.kind = sftui::sf::create::CreateKind::CustomObject;
    interactive.set(sftui::sf::create::FieldKey::Label, "My Object");
    let argv = sftui::sf::commands::create_metadata_argv(&interactive, &destination);
    assert!(!argv.iter().any(|part| part.starts_with("--output")));
    let outcome = runner::capture(&installation.executable, &argv, Duration::from_secs(5))
        .await
        .expect("stub interactive command call");
    assert!(
        outcome.succeeded(),
        "interactive Copy CLI command: {outcome:?}"
    );
}
