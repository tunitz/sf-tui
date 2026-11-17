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
                    rendered.to_lowercase().contains("coverage"),
                    "coverage pane missing"
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
                selected: 0,
                focus: sftui::app::JobsFocus::List,
                scroll: 0,
                follow: true,
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
                "F3 help row missing at {width}x{height}"
            );
            assert!(
                rendered.contains("Apex test history (Tests screen)"),
                "F6 help row missing at {width}x{height}"
            );
        }
        assert!(
            !rendered.contains("Menu r/d/c/t/o/e"),
            "stale menu row at {width}x{height}"
        );
    }
}

#[test]
fn f3_catalog_renders_at_small_and_roomy_sizes() {
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
    app.handle_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
    assert!(
        matches!(app.modal, Some(sftui::app::Modal::Catalog { .. })),
        "F3 must open the catalog"
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
fn create_metadata_modal_shows_local_output_and_scannable_templates() {
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
    app.modal = Some(sftui::app::Modal::Prompt {
        title: "Create metadata".to_owned(),
        kind: sftui::app::PromptKind::CreateMetadata,
        fields: vec![
            sftui::app::PromptField::new("Template", "").with_suggestions(std::sync::Arc::new(
                sftui::sf::metadata::create_suggestions(),
            )),
            sftui::app::PromptField::new("Component name (PascalCase)", ""),
            sftui::app::PromptField::new("Output dir (optional)", ""),
        ],
        focus: 0,
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
            rendered.contains("ApexClass"),
            "template suggestions missing at {width}x{height}"
        );
        assert!(
            rendered.contains("classes/"),
            "destination folder missing at {width}x{height}"
        );
        assert!(
            !rendered.contains("Org"),
            "the irrelevant Org field must be gone at {width}x{height}"
        );
        if width == 80 {
            assert!(
                rendered.contains("Component name (PascalCase)")
                    && rendered.contains("Output dir (optional)"),
                "improved field labels missing: {rendered}"
            );
        }
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
fn tests_screen_with_coverage_renders_at_small_and_roomy_sizes() {
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
    });
    for (width, height) in [(48, 12), (80, 24)] {
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
            rendered.to_lowercase().contains("coverage"),
            "coverage pane missing at {width}x{height}"
        );
        assert!(
            rendered.contains("VisibleTestClass"),
            "test list has no usable rows at {width}x{height}"
        );
    }
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
fn f6_history_modal_renders_at_small_and_roomy_sizes() {
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
fn f6_hint_is_shown_only_on_the_tests_screen() {
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
    assert!(
        !render(&app).contains("[F6]"),
        "F6 must not show outside Tests"
    );
    app.tab = sftui::app::Tab::Tests;
    let tests = render(&app);
    assert!(tests.contains("[F6]"), "F6 hint missing on Tests: {tests}");
    assert!(tests.to_lowercase().contains("history"), "F6 label missing");
}
