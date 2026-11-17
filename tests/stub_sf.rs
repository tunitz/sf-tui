//! Integration coverage using a synthetic `sf` stub, plus a render smoke test for every screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    let (tx, mut rx) = mpsc::unbounded_channel::<SfEvent>();
    let argv = vec!["org".to_owned(), "list".to_owned(), "--json".to_owned()];
    let job = runner::spawn_sf(&stub_installation(), &argv, 42, tx).expect("spawn");
    let mut finished = None;
    let mut saw_stdout_line = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            match event {
                SfEvent::Line(line) if line.stream == OutputStream::Stdout => {
                    saw_stdout_line = line.text.contains("test-scratch");
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
                assert!(rendered.contains("[r] Retrieve"), "menu missing");
                assert!(rendered.contains("MENU"), "menu heading missing");
            }
            if (width, height) == (60, 16) && tab == sftui::app::Tab::Tests {
                assert!(
                    rendered.to_lowercase().contains("coverage"),
                    "coverage pane missing"
                );
            }
            if (width, height) == (120, 40) && tab == sftui::app::Tab::Home {
                assert!(!rendered.contains("[1]"), "tab bar must be gone");
                assert!(rendered.contains("F2 jobs"), "key hints missing");
            }
        }
        for modal in [
            sftui::app::Modal::Help { scroll: 0 },
            sftui::app::Modal::Message {
                title: "Notice".to_owned(),
                message: "Synthetic modal".to_owned(),
            },
            sftui::app::Modal::Confirm {
                title: "Confirm".to_owned(),
                message: "Synthetic confirmation".to_owned(),
                action: sftui::app::PendingAction::None,
            },
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
