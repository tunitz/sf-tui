//! Integration coverage using a synthetic `sf` stub, plus a render smoke test for every screen.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sftui::app::{App, OutputRecord};
use sftui::config::Config;
use sftui::sf::Startup;
use sftui::sf::catalog::{Catalog, CatalogSource, CommandSpec};
use sftui::sf::detect::{SfFlavor, SfInstallation};
use sftui::sf::runner::{self, OutputStream, SfEvent};
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
        flavor: SfFlavor::Sf,
        source: "test fixture".to_owned(),
    }
}

fn test_catalog() -> Catalog {
    Catalog {
        commands: vec![CommandSpec {
            id: "org list".to_owned(),
            summary: "List orgs".to_owned(),
            description: "List all orgs".to_owned(),
            category: "Orgs".to_owned(),
            flags: Vec::new(),
            examples: Vec::new(),
        }],
        source: CatalogSource::Snapshot,
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
    let mut app = App::new(
        Startup {
            installation: None,
            version: Some("99.0.0".to_owned()),
            catalog: test_catalog(),
            notices: vec!["synthetic notice".to_owned()],
        },
        Config::default(),
    );
    app.output_state.records.push(OutputRecord {
        id: 1,
        command: "sf org list --json".to_owned(),
        purpose: "orgs".to_owned(),
        lines: Vec::new(),
        status: None,
        json: Some(serde_json::json!({"result":{"status":"ok"}})),
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    for tab in app.tabs() {
        app.tab = tab;
        terminal.draw(|frame| ui::draw(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        assert!(buffer.content.iter().any(|cell| cell.symbol() != " "));
    }
}

#[test]
fn prompt_overlay_renders_with_a_cursor() {
    assert_synthetic_environment();
    let mut app = App::new(
        Startup {
            installation: None,
            version: None,
            catalog: test_catalog(),
            notices: Vec::new(),
        },
        Config::default(),
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
