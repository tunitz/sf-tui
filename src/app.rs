//! Application state, key dispatch and ownership of background SF jobs.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::clipboard;
use crate::config::Config;
use crate::sf::deploy::{PayloadMode, TransferOptions};
use crate::sf::metadata::{SuggestionItem, SuggestionSet};
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::project::{self, SfProject};
use crate::sf::runner::{JobLine, ProcessOutcome, RunningJob, SfEvent};
use crate::sf::test_results;
use crate::sf::{self, Startup};
use crate::view::{apex_result, generic_result, transfer_result};

/// Screens shown in the tab bar. Setup is added only when no CLI was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Retrieve,
    Tests,
    Deploy,
    Orgs,
    Setup,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Menu",
            Self::Retrieve => "Retrieve",
            Self::Tests => "Tests",
            Self::Deploy => "Deploy",
            Self::Orgs => "Orgs",
            Self::Setup => "Setup",
        }
    }
}

/// What a background job was started for; drives the follow-up once it finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobPurpose {
    Command {
        id: String,
        command: String,
    },
    OrgList,
    OrgDetail {
        username: String,
    },
    OrgAction {
        description: String,
    },
    Transfer {
        options: TransferOptions,
    },
    Report {
        options: TransferOptions,
        transfer_job: u64,
    },
    Installer {
        command: String,
    },
    ApexTests {
        command: String,
        org: Option<String>,
    },
    TestReport {
        record_id: u64,
    },
}

impl JobPurpose {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Command { .. } => "command",
            Self::OrgList => "orgs",
            Self::OrgDetail { .. } => "org detail",
            Self::OrgAction { .. } => "org action",
            Self::Transfer { .. } => "transfer",
            Self::Report { .. } => "report",
            Self::Installer { .. } => "installer",
            Self::ApexTests { .. } => "apex tests",
            Self::TestReport { .. } => "apex report",
        }
    }
}

/// Messages sent from background processes into the UI thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Transfer,
    Tests,
}

pub enum AppEvent {
    Sf(SfEvent),
    TransferResults {
        kind: SearchKind,
        retrieve: bool,
        query: String,
        results: Vec<SuggestionItem>,
        generation: u64,
    },
    Startup(crate::sf::StartupEvent),
    Project {
        project: Option<SfProject>,
        error: Option<String>,
    },
    Suggestions {
        root: PathBuf,
        all: Arc<SuggestionSet>,
        tests: Arc<SuggestionSet>,
    },
    Notice {
        text: String,
        error: bool,
    },
}

/// A destination or action on the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Retrieve,
    Deploy,
    Create,
    Tests,
    Organizations,
    RunCommand,
}

#[derive(Debug, Clone, Copy)]
pub struct MenuItem {
    pub key: char,
    pub label: &'static str,
    pub action: MenuAction,
}

pub const MENU_ITEMS: &[MenuItem] = &[
    MenuItem {
        key: 'r',
        label: "Retrieve Metadata",
        action: MenuAction::Retrieve,
    },
    MenuItem {
        key: 'd',
        label: "Deploy Metadata",
        action: MenuAction::Deploy,
    },
    MenuItem {
        key: 'c',
        label: "Create Metadata",
        action: MenuAction::Create,
    },
    MenuItem {
        key: 't',
        label: "Apex Tests",
        action: MenuAction::Tests,
    },
    MenuItem {
        key: 'o',
        label: "Organization",
        action: MenuAction::Organizations,
    },
    MenuItem {
        key: 'e',
        label: "Run command",
        action: MenuAction::RunCommand,
    },
];

#[derive(Debug, Clone, Default)]
pub struct HomeState {
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct StartupStage {
    pub label: String,
    pub started: Instant,
    pub done: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OrgState {
    pub orgs: Vec<OrgSummary>,
    pub selected: usize,
    pub detail: Option<OrgDetail>,
    pub detail_for: Option<String>,
    pub loading: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DeployState {
    pub project: Option<SfProject>,
    pub project_error: Option<String>,
    pub project_loading: bool,
    pub options: TransferOptions,
}

/// A deploy accepted by the org whose report still has to be polled.
#[derive(Debug, Clone)]
struct DeployReport {
    server_job_id: String,
    options: TransferOptions,
    poll_after: Instant,
    attempts: u32,
    failures: u32,
}

/// An Apex test run accepted by the org whose results still have to be polled.
#[derive(Debug, Clone)]
struct TestReport {
    run_id: String,
    org: Option<String>,
    poll_after: Instant,
    attempts: u32,
    failures: u32,
    /// Extra polls spent waiting for per-class coverage after a terminal outcome.
    coverage_polls: u32,
}

/// Which pane of the Tests screen receives navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestsFocus {
    Search,
    Coverage,
}

#[derive(Debug, Clone)]
pub struct TransferSelection {
    pub item: SuggestionItem,
    pub checked: bool,
}

#[derive(Debug, Clone)]
pub struct TransferSearchState {
    pub retrieve: bool,
    pub query: String,
    pub results: Vec<SuggestionItem>,
    pub cursor: usize,
    pub selected: Vec<TransferSelection>,
    pub searching: bool,
    pub searched_query: Option<String>,
}

impl TransferSearchState {
    pub fn new(retrieve: bool) -> Self {
        Self {
            retrieve,
            query: String::new(),
            results: Vec::new(),
            cursor: 0,
            selected: Vec::new(),
            searching: false,
            searched_query: None,
        }
    }

    /// Mark the visible results stale without discarding the user's selection.
    pub fn mark_edited(&mut self) {
        self.cursor = 0;
        self.searched_query = None;
        self.searching = false;
        self.results.clear();
    }

    pub fn needs_search(&self) -> bool {
        self.searched_query.as_deref().map(str::trim) != Some(self.query.trim())
    }

    pub fn toggle_result(&mut self) {
        let Some(item) = self.results.get(self.cursor).cloned() else {
            return;
        };
        if let Some(entry) = self
            .selected
            .iter_mut()
            .find(|entry| entry.item.value == item.value)
        {
            entry.checked = !entry.checked;
        } else {
            self.selected.push(TransferSelection {
                item,
                checked: true,
            });
        }
    }

    pub fn checked(&self) -> Vec<SuggestionItem> {
        self.selected
            .iter()
            .filter(|entry| entry.checked)
            .map(|entry| entry.item.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Generic,
    Retrieve,
    Deploy,
    ApexTests,
}

#[derive(Debug, Clone)]
pub struct ResultItem {
    pub ok: bool,
    pub label: String,
    pub detail: String,
    pub progress: Option<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct ResultSection {
    pub title: String,
    pub rows: Vec<ResultItem>,
}

#[derive(Debug, Clone)]
pub struct ResultView {
    pub kind: ResultKind,
    pub title: String,
    pub status: String,
    pub ok: bool,
    pub summary: String,
    pub items: Vec<ResultItem>,
    pub coverage: Vec<ResultItem>,
    pub sections: Vec<ResultSection>,
}

/// The task a recorded job belongs to, used as the label in the Jobs window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Retrieve,
    Deploy,
    Tests,
    Command,
    Organization,
    Setup,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Retrieve => "Retrieve",
            Self::Deploy => "Deploy",
            Self::Tests => "Apex tests",
            Self::Command => "Command",
            Self::Organization => "Organization",
            Self::Setup => "Setup",
        }
    }
}

/// A single recorded command invocation and its streamed output.
#[derive(Debug, Clone)]
pub struct OutputRecord {
    pub id: u64,
    pub command: String,
    pub purpose: String,
    pub task: TaskKind,
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub lines: Vec<JobLine>,
    pub line_bytes: usize,
    pub exit_code: Option<i32>,
    pub finished: bool,
    pub queued: bool,
    pub started_at: Option<Instant>,
    pub ended_at: Option<Instant>,
    pub json: Option<Value>,
    pub result: Option<ResultView>,
    pub pending_report: bool,
}

impl OutputRecord {
    pub fn label(&self) -> String {
        let state = if self.queued {
            "queued".to_owned()
        } else if !self.finished {
            "running".to_owned()
        } else {
            match self.exit_code {
                Some(0) => "ok".to_owned(),
                Some(code) => format!("exit {code}"),
                None => "stopped".to_owned(),
            }
        };
        format!("#{} [{}] {}", self.id, state, self.display_command())
    }

    /// The command with its executable path shortened, for job rows and result headers.
    pub fn display_command(&self) -> String {
        let prefix = self.executable.display().to_string();
        self.command
            .strip_prefix(&prefix)
            .map(|rest| format!("sf{rest}"))
            .unwrap_or_else(|| self.command.clone())
    }

    pub fn is_running(&self) -> bool {
        self.pending_report || (!self.finished && !self.queued)
    }

    /// `12s` for a short job, `1m 04s` once it passes a minute.
    pub fn elapsed_label(&self, now: Instant) -> String {
        let Some(started) = self.started_at else {
            return "—".to_owned();
        };
        let elapsed = if self.pending_report {
            now
        } else {
            self.ended_at.unwrap_or(now)
        }
        .saturating_duration_since(started);
        let seconds = elapsed.as_secs();
        if seconds < 60 {
            format!("{seconds}s")
        } else {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        }
    }

    /// A compact outcome for a job row: `3/3 ✓`, `exit 2`, `stopped`.
    pub fn outcome_summary(&self) -> String {
        if self.queued || self.pending_report || !self.finished {
            return String::new();
        }
        if let Some(result) = &self.result {
            if result.items.is_empty() {
                return if result.ok { "✓" } else { "✗" }.to_owned();
            }
            let total = result.items.len();
            let ok = result.items.iter().filter(|item| item.ok).count();
            return format!("{ok}/{total} {}", if result.ok { "✓" } else { "✗" });
        }
        match self.exit_code {
            Some(0) => "ok".to_owned(),
            Some(code) => format!("exit {code}"),
            None => "stopped".to_owned(),
        }
    }
}

/// One row of the global Jobs window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSection {
    Running,
    Queued,
    Finished,
}

impl JobSection {
    pub fn title(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Queued => "QUEUED",
            Self::Finished => "FINISHED",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JobRow<'a> {
    pub section: JobSection,
    pub record: &'a OutputRecord,
}

/// Which pane of the Jobs window receives list navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobsFocus {
    List,
    Detail,
}

const MAX_RECORDS: usize = 40;
const MAX_LINES_PER_RECORD: usize = 3_000;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct OutputState {
    pub records: Vec<OutputRecord>,
    pub show_json: bool,
    pub show_details: bool,
}

impl OutputState {
    fn begin(
        &mut self,
        id: u64,
        command: String,
        purpose: &str,
        task: TaskKind,
        executable: PathBuf,
        argv: Vec<String>,
    ) {
        self.records.push(OutputRecord {
            id,
            command,
            purpose: purpose.to_owned(),
            task,
            executable,
            argv,
            lines: Vec::new(),
            line_bytes: 0,
            exit_code: None,
            finished: false,
            queued: true,
            started_at: None,
            ended_at: None,
            json: None,
            result: None,
            pending_report: false,
        });
        if self.records.len() > MAX_RECORDS {
            self.records.remove(0);
        }
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut OutputRecord> {
        self.records.iter_mut().rev().find(|record| record.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct PromptField {
    pub label: String,
    pub value: String,
    pub cursor: usize,
    pub suggestion_set: Option<Arc<SuggestionSet>>,
    pub suggestion_index: Option<usize>,
    pub selected: Vec<String>,
    pub multi_select: bool,
    cached_suggestions: Vec<SuggestionItem>,
    suggestion_dirty: bool,
    last_edit: Instant,
}

const SUGGESTION_DEBOUNCE: Duration = Duration::from_millis(90);

impl PromptField {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            label: label.into(),
            value,
            cursor,
            suggestion_set: None,
            suggestion_index: None,
            selected: Vec::new(),
            multi_select: false,
            cached_suggestions: Vec::new(),
            suggestion_dirty: false,
            last_edit: Instant::now(),
        }
    }

    pub fn with_suggestions(mut self, suggestion_set: Arc<SuggestionSet>) -> Self {
        self.suggestion_set = Some(suggestion_set);
        self.suggestion_dirty = true;
        self.ensure_suggestions();
        self
    }

    pub fn multi_select(mut self) -> Self {
        self.multi_select = true;
        self
    }

    pub fn toggle_current(&mut self, value: &str) {
        if let Some(position) = self.selected.iter().position(|item| item == value) {
            self.selected.remove(position);
        } else {
            self.selected.push(value.to_owned());
        }
    }

    /// Mark the candidate list stale without doing any fuzzy work in the key handler.
    pub fn mark_edited(&mut self) {
        self.suggestion_dirty = true;
        self.last_edit = Instant::now();
    }

    pub fn debounce_ready(&self) -> bool {
        self.suggestion_dirty && self.last_edit.elapsed() >= SUGGESTION_DEBOUNCE
    }

    /// Recompute only when stale; navigation calls this directly so it never sees old results.
    pub fn ensure_suggestions(&mut self) {
        if !self.suggestion_dirty {
            return;
        }
        self.suggestion_dirty = false;
        let Some(set) = &self.suggestion_set else {
            self.cached_suggestions.clear();
            self.suggestion_index = None;
            return;
        };
        self.cached_suggestions = set.filter(&self.value, 5);
        self.suggestion_index = if self.cached_suggestions.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    pub fn cached_suggestions(&self) -> &[SuggestionItem] {
        &self.cached_suggestions
    }
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    RunCommand,
    QuickRetrieve,
    QuickDeploy,
    CreateMetadata,
    LoginWeb,
    CreateScratch,
    TargetOrg,
    SwitchOrg,
}

/// Action queued behind a confirmation or input dialog.
#[derive(Debug, Clone)]
pub enum PendingAction {
    None,
    SetDefault { username: String, devhub: bool },
    Logout { username: String },
    RunTransfer,
    RunInstaller { command: String },
    RunPrepared { argv: Vec<String> },
    RunCommand { argv: Vec<String>, command: String },
    Quit,
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help {
        scroll: usize,
    },
    Message {
        title: String,
        message: String,
    },
    Confirm {
        title: String,
        message: String,
        action: PendingAction,
    },
    SwitchOrg {
        selected: usize,
    },
    Jobs {
        selected: usize,
        focus: JobsFocus,
        scroll: usize,
        follow: bool,
    },
    TestConfirm {
        entries: Vec<TransferSelection>,
        focus: usize,
        show_command: bool,
        org: Option<String>,
    },
    TransferConfirm {
        retrieve: bool,
        entries: Vec<TransferSelection>,
        focus: usize,
        show_command: bool,
        options: TransferOptions,
    },
    Prompt {
        title: String,
        kind: PromptKind,
        fields: Vec<PromptField>,
        focus: usize,
    },
}

struct JobEntry {
    job: RunningJob,
    purpose: JobPurpose,
    finished_seen: bool,
}

struct PendingJob {
    id: u64,
    executable: PathBuf,
    argv: Vec<String>,
    purpose: JobPurpose,
}

const MAX_PARALLEL_JOBS: usize = 4;

/// Root application state.
pub struct App {
    pub startup: Startup,
    pub config: Config,
    pub tab: Tab,
    pub startup_stage: StartupStage,
    pub config_dir: PathBuf,
    pub home_state: HomeState,
    pub should_quit: bool,
    pub status: String,
    pub status_error: bool,
    pub org_state: OrgState,
    pub deploy_state: DeployState,
    pub transfer_search: TransferSearchState,
    pub test_search: TransferSearchState,
    pub output_state: OutputState,
    pub modal: Option<Modal>,
    pub tests_focus: TestsFocus,
    pub coverage_scroll: usize,
    pub setup_selected: usize,
    tab_history: Vec<Tab>,
    suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    test_suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    suggestions_building: bool,
    ticks: usize,
    probe_task: Option<JoinHandle<()>>,
    search_task: Option<JoinHandle<()>>,
    transfer_search_generation: u64,
    test_search_generation: u64,
    event_tx: UnboundedSender<AppEvent>,
    event_rx: UnboundedReceiver<AppEvent>,
    jobs: HashMap<u64, JobEntry>,
    pending_jobs: VecDeque<PendingJob>,
    deploy_reports: HashMap<u64, DeployReport>,
    test_reports: HashMap<u64, TestReport>,
    next_job_id: u64,
    /// Rows available in the Jobs detail pane at the last draw, used to clamp scrolling.
    pub(crate) jobs_detail_height: Cell<u16>,
    /// Coverage rows visible at the last Tests draw, used to clamp table scrolling.
    pub(crate) tests_coverage_rows: Cell<usize>,
}

impl App {
    pub fn new(startup: Startup, config: Config, config_dir: PathBuf) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let mut app = Self {
            startup,
            config,
            tab: Tab::Home,
            startup_stage: StartupStage {
                label: "Starting sftui".to_owned(),
                started: Instant::now(),
                done: false,
            },
            config_dir,
            home_state: HomeState::default(),
            should_quit: false,
            status: String::new(),
            status_error: false,
            org_state: OrgState::default(),
            deploy_state: DeployState::default(),
            transfer_search: TransferSearchState::new(false),
            test_search: TransferSearchState::new(false),
            output_state: OutputState::default(),
            modal: None,
            tests_focus: TestsFocus::Search,
            coverage_scroll: 0,
            setup_selected: 0,
            tab_history: Vec::new(),
            suggestion_cache: None,
            test_suggestion_cache: None,
            suggestions_building: false,
            ticks: 0,
            probe_task: None,
            search_task: None,
            transfer_search_generation: 0,
            test_search_generation: 0,
            event_tx,
            event_rx,
            jobs: HashMap::new(),
            pending_jobs: VecDeque::new(),
            deploy_reports: HashMap::new(),
            test_reports: HashMap::new(),
            next_job_id: 1,
            jobs_detail_height: Cell::new(10),
            tests_coverage_rows: Cell::new(6),
        };
        app.start_project_scan();
        app.status = "Starting sftui — the interface is ready while sf is probed".to_owned();
        app.start_probe();
        app
    }

    fn start_probe(&mut self) {
        let config_dir = self.config_dir.clone();
        let app_tx = self.event_tx.clone();
        self.probe_task = Some(tokio::spawn(async move {
            let (probe_tx, mut probe_rx) = mpsc::unbounded_channel::<sf::StartupEvent>();
            let probe = tokio::spawn(sf::probe(config_dir, probe_tx));
            while let Some(event) = probe_rx.recv().await {
                if app_tx.send(AppEvent::Startup(event)).is_err() {
                    break;
                }
            }
            let _ = probe.await;
        }));
    }

    /// Screens rendered by headless smoke tests; navigation itself is menu-only.
    pub fn screens(&self) -> Vec<Tab> {
        let mut screens = vec![Tab::Home, Tab::Retrieve, Tab::Deploy, Tab::Tests, Tab::Orgs];
        if self.startup_stage.done && !self.startup.is_ready() {
            screens.push(Tab::Setup);
        }
        screens
    }

    pub fn accent(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self.config.theme.accent.to_ascii_lowercase().as_str() {
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "red" => Color::Red,
            _ => Color::Cyan,
        }
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_error = false;
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_error = true;
    }

    /// Discover and enumerate the project without blocking the first frame.
    fn start_project_scan(&mut self) {
        self.deploy_state.project_loading = true;
        self.deploy_state.project_error = None;
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(|| {
                let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                match project::find_project(&start) {
                    Some(root) => project::load_project(root).map(Some),
                    None => Ok(None),
                }
            })
            .await;
            let event = match result {
                Ok(Ok(project)) => AppEvent::Project {
                    project,
                    error: None,
                },
                Ok(Err(error)) => AppEvent::Project {
                    project: None,
                    error: Some(error.to_string()),
                },
                Err(error) => AppEvent::Project {
                    project: None,
                    error: Some(format!("project scan failed: {error}")),
                },
            };
            let _ = tx.send(event);
        });
    }

    pub async fn recv_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Apply queued events before the next draw so a noisy CLI cannot force one frame per line.
    /// The per-call cap keeps keystrokes responsive when the queue is very deep.
    pub fn drain_events(&mut self) {
        for _ in 0..2_048 {
            let Ok(event) = self.event_rx.try_recv() else {
                break;
            };
            self.on_event(event);
        }
    }

    pub fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Startup(sf::StartupEvent::Stage(label)) => {
                self.startup_stage = StartupStage {
                    label: label.to_owned(),
                    started: Instant::now(),
                    done: false,
                };
            }
            AppEvent::Startup(sf::StartupEvent::Detected(installation)) => {
                // No automatic org list here: the first user command must not queue behind a
                // startup refresh. The Orgs screen loads them when opened.
                self.startup.installation = Some(installation);
            }
            AppEvent::Startup(sf::StartupEvent::Version(version)) => {
                self.startup.version = Some(version);
            }
            AppEvent::Startup(sf::StartupEvent::CurrentOrg(org)) => {
                self.startup.current_org = Some(org);
                if !self.status_error {
                    self.notice("Current org loaded");
                }
            }
            AppEvent::Startup(sf::StartupEvent::Notice(text)) => {
                self.startup.notices.push(text.clone());
                self.notice(text);
            }
            AppEvent::Startup(sf::StartupEvent::Finished) => {
                self.startup_stage.done = true;
                if !self.status_error {
                    self.notice("Ready");
                }
            }
            AppEvent::Project { project, error } => {
                let missing = project.is_none();
                self.deploy_state.project_loading = false;
                self.deploy_state.project = project;
                self.deploy_state.project_error = if missing {
                    error.or(Some(
                        "No sfdx-project.json found above the current directory".to_owned(),
                    ))
                } else {
                    error
                };
                if self.deploy_state.project.is_some() {
                    self.ensure_suggestion_sets();
                }
            }
            AppEvent::Suggestions { root, all, tests } => {
                self.suggestions_building = false;
                if root != self.suggestion_root() {
                    self.ensure_suggestion_sets();
                    return;
                }
                self.suggestion_cache = Some((root.clone(), all));
                self.test_suggestion_cache = Some((root, tests));
                let pending = match self.tab {
                    Tab::Retrieve | Tab::Deploy => {
                        !self.transfer_search.query.trim().is_empty()
                            && self.transfer_search.needs_search()
                    }
                    Tab::Tests => {
                        !self.test_search.query.trim().is_empty() && self.test_search.needs_search()
                    }
                    _ => false,
                };
                if pending {
                    self.start_search(if self.tab == Tab::Tests {
                        SearchKind::Tests
                    } else {
                        SearchKind::Transfer
                    });
                }
            }
            AppEvent::TransferResults {
                kind,
                retrieve,
                query,
                results,
                generation,
            } => {
                let current = if kind == SearchKind::Tests {
                    self.test_search_generation
                } else {
                    self.transfer_search_generation
                };
                if generation != current {
                    return;
                }
                let state = if kind == SearchKind::Tests {
                    &mut self.test_search
                } else {
                    &mut self.transfer_search
                };
                // A reply for the current request always ends the loading state, even when the
                // user has typed since pressing Enter.
                state.searching = false;
                if state.retrieve == retrieve && state.query.trim() == query {
                    let count = results.len();
                    state.results = results;
                    state.cursor = 0;
                    state.searched_query = Some(query);
                    self.notice(if count == 0 {
                        "No matches — refine the query and press Enter".to_owned()
                    } else {
                        format!("{count} matches — Space select · Enter confirm")
                    });
                }
            }
            AppEvent::Notice { text, error } => {
                if error {
                    self.error(text);
                } else {
                    self.notice(text);
                }
            }
            AppEvent::Sf(SfEvent::Line(line)) => {
                if let Some(record) = self.output_state.find_mut(line.job_id)
                    && record.lines.len() < MAX_LINES_PER_RECORD
                    && record.line_bytes.saturating_add(line.text.len()) <= MAX_RECORD_BYTES
                    && !is_secret_command(&record.command)
                {
                    record.line_bytes = record.line_bytes.saturating_add(line.text.len());
                    record.lines.push(line);
                }
            }
            AppEvent::Sf(SfEvent::Finished { job_id, outcome }) => {
                self.finish_job(job_id, outcome);
            }
        }
    }

    fn finish_job(&mut self, job_id: u64, outcome: ProcessOutcome) {
        let Some(purpose) = self.jobs.get(&job_id).map(|entry| entry.purpose.clone()) else {
            // The job was cancelled and removed from the map. Keep its cancellation status on
            // the output row instead of letting a late completion reclassify it.
            if let Some(record) = self.output_state.find_mut(job_id)
                && !record.finished
            {
                record.exit_code = outcome.exit_code;
                record.finished = true;
                record.queued = false;
                record.ended_at = Some(Instant::now());
            }
            self.start_pending_jobs();
            return;
        };
        if let Some(entry) = self.jobs.get_mut(&job_id) {
            entry.finished_seen = true;
        }
        let parsed = sf::runner::parse_json_document(&outcome.stdout)
            .map(|value| crate::json_view::sanitize(&value));
        if let Some(record) = self.output_state.find_mut(job_id) {
            record.exit_code = outcome.exit_code;
            record.finished = true;
            record.queued = false;
            record.ended_at.get_or_insert_with(Instant::now);
            record.json = if outcome.stdout.len() <= MAX_JSON_BYTES {
                parsed.clone()
            } else {
                None
            };
        }
        let succeeded = outcome.succeeded();
        let secret_output = match &purpose {
            JobPurpose::Command { command, .. } | JobPurpose::ApexTests { command, .. } => {
                is_secret_command(command)
            }
            JobPurpose::OrgAction { .. } | JobPurpose::OrgDetail { .. } => true,
            _ => false,
        };
        if !outcome.succeeded() {
            let detail = if secret_output {
                "[output redacted]".to_owned()
            } else if outcome.stderr.trim().is_empty() {
                outcome.stdout.trim().to_owned()
            } else {
                outcome.stderr.trim().to_owned()
            };
            self.error(format!(
                "{} failed: {}",
                purpose.label(),
                truncate(&redact_command(&detail), 160)
            ));
        }

        let needs_generic = matches!(
            purpose,
            JobPurpose::Command { .. }
                | JobPurpose::Transfer { .. }
                | JobPurpose::OrgAction { .. }
                | JobPurpose::Installer { .. }
        ) && !secret_output;
        let purpose_title = purpose.label().to_owned();
        if let JobPurpose::Report { transfer_job, .. } = &purpose {
            let parsed_status = parsed
                .as_ref()
                .map(sf::deploy::parse_report)
                .is_some_and(|report| !report.status.is_empty());
            if succeeded && parsed_status {
                if let Some(report) = self.deploy_reports.get_mut(transfer_job) {
                    report.failures = 0;
                }
            } else {
                let failed = self
                    .deploy_reports
                    .get_mut(transfer_job)
                    .is_some_and(|report| {
                        report.failures = report.failures.saturating_add(1);
                        report.failures >= 5
                    });
                if failed {
                    self.deploy_reports.remove(transfer_job);
                    self.error("Deploy report failed repeatedly; stopped polling");
                }
            }
        }
        let mut result_view: Option<ResultView> = None;
        match (purpose.clone(), parsed.clone()) {
            (JobPurpose::OrgList, Some(value)) => {
                self.org_state.orgs = sf::org::parse_org_list(&value);
                self.org_state.loading = false;
                self.org_state.selected = self
                    .org_state
                    .selected
                    .min(self.org_state.orgs.len().saturating_sub(1));
                if succeeded {
                    self.notice(format!("Loaded {} orgs", self.org_state.orgs.len()));
                }
            }
            (JobPurpose::OrgList, None) => {
                self.org_state.loading = false;
            }
            (JobPurpose::OrgDetail { username }, Some(value)) => {
                self.org_state.detail = Some(sf::org::parse_org_display(&value));
                self.org_state.detail_for = Some(username);
                if succeeded {
                    self.notice("Org details loaded");
                }
            }
            (JobPurpose::Transfer { options }, Some(value)) => {
                if options.retrieve {
                    let mut report = sf::deploy::parse_report(&value);
                    if report.status.is_empty() {
                        report.status = if outcome.succeeded() {
                            "Succeeded".to_owned()
                        } else {
                            "Failed".to_owned()
                        };
                    }
                    result_view = Some(transfer_result(
                        true,
                        &report,
                        None,
                        &options.metadata_names,
                    ));
                    if succeeded {
                        self.notice("Retrieve finished — F2 jobs shows the result");
                    }
                } else if let Some(id) = sf::deploy::parse_started_job_id(&value) {
                    self.deploy_reports.insert(
                        job_id,
                        DeployReport {
                            server_job_id: id.clone(),
                            options: options.clone(),
                            poll_after: Instant::now() + Duration::from_secs(3),
                            attempts: 0,
                            failures: 0,
                        },
                    );
                    result_view = Some(ResultView {
                        kind: ResultKind::Deploy,
                        title: "Deploy".to_owned(),
                        status: "Queued".to_owned(),
                        ok: true,
                        summary: format!("Job {id} started"),
                        items: vec![ResultItem {
                            ok: true,
                            label: "Deploy request accepted".to_owned(),
                            detail: id.clone(),
                            progress: None,
                        }],
                        coverage: Vec::new(),
                        sections: Vec::new(),
                    });
                    if succeeded {
                        self.notice(format!("Deploy {id} started — F2 jobs shows progress"));
                    }
                }
            }
            (JobPurpose::Transfer { .. }, None) => {}
            (
                JobPurpose::Report {
                    options,
                    transfer_job,
                },
                Some(value),
            ) => {
                let report = sf::deploy::parse_report(&value);
                let complete = report.is_complete();
                let view =
                    transfer_result(false, &report, Some(&report.id), &options.metadata_names);
                if let Some(record) = self.output_state.find_mut(transfer_job) {
                    record.result = Some(view);
                }
                if complete {
                    self.deploy_reports.remove(&transfer_job);
                    if succeeded {
                        self.notice("Deploy finished — F2 jobs shows the result");
                    }
                }
            }
            (JobPurpose::Report { .. }, None) => {}
            (JobPurpose::OrgAction { description }, _) => {
                if succeeded {
                    self.notice(format!("{description} finished"));
                }
                self.refresh_orgs();
            }
            (JobPurpose::ApexTests { command, org }, Some(value)) => {
                let run = test_results::parse(&value, &command);
                match apex_report_action(&run) {
                    ApexReportAction::Final => {
                        result_view = Some(apex_result(&run));
                        self.output_state.show_details = false;
                        self.tests_focus = TestsFocus::Search;
                        self.coverage_scroll = 0;
                        if succeeded {
                            self.notice("Apex test results ready — F2 jobs shows the details");
                        }
                    }
                    ApexReportAction::Poll { run_id } => {
                        result_view = Some(started_test_view(&run_id));
                        if let Some(record) = self.output_state.find_mut(job_id) {
                            record.pending_report = true;
                        }
                        self.tests_focus = TestsFocus::Search;
                        self.coverage_scroll = 0;
                        self.test_reports.insert(
                            job_id,
                            TestReport {
                                run_id: run_id.clone(),
                                org,
                                poll_after: Instant::now() + Duration::from_secs(3),
                                attempts: 0,
                                failures: 0,
                                coverage_polls: 0,
                            },
                        );
                        self.notice(format!(
                            "Apex test run {run_id} started — polling for results"
                        ));
                    }
                }
            }
            (JobPurpose::TestReport { record_id }, Some(value)) => {
                self.apply_test_report(record_id, &value, succeeded);
            }
            (JobPurpose::TestReport { record_id }, None) => {
                self.fail_test_report(record_id);
            }
            _ => {}
        }
        if result_view.is_none() && needs_generic {
            result_view = Some(generic_result(&purpose_title, &outcome, parsed.as_ref()));
        }
        if let Some(view) = result_view
            && let Some(record) = self.output_state.find_mut(job_id)
        {
            record.result = Some(view);
        }
        let user_command = matches!(
            purpose,
            JobPurpose::Command { .. } | JobPurpose::Installer { .. }
        );
        if succeeded && user_command {
            self.notice("Done — F2 jobs · Esc back");
        }
        self.start_pending_jobs();
    }

    /// Apply one polled `apex get test` response: finish the record or keep polling.
    fn apply_test_report(&mut self, record_id: u64, value: &Value, succeeded: bool) {
        let Some(report) = self.test_reports.get(&record_id) else {
            return;
        };
        let run_id = report.run_id.clone();
        if !succeeded {
            self.fail_test_report(record_id);
            return;
        }
        let run = test_results::parse(value, "");
        let terminal = test_results::is_terminal_outcome(&run.outcome);
        let coverage_pending = run.coverages.is_empty() && !run.coverage.is_empty();
        let coverage_polls = report.coverage_polls;
        if terminal && (!coverage_pending || coverage_polls >= 3) {
            let view = apex_result(&run);
            if let Some(record) = self.output_state.find_mut(record_id) {
                record.result = Some(view);
                record.pending_report = false;
            }
            self.test_reports.remove(&record_id);
            self.coverage_scroll = 0;
            self.tests_focus = TestsFocus::Search;
            self.notice("Apex tests finished — F2 jobs");
            return;
        }
        if let Some(report) = self.test_reports.get_mut(&record_id) {
            report.failures = 0;
            if terminal {
                report.coverage_polls = report.coverage_polls.saturating_add(1);
                report.poll_after = Instant::now() + Duration::from_secs(2);
            }
        }
        // Show progress in the running job while the org keeps working.
        if let Some(record) = self.output_state.find_mut(record_id)
            && let Some(view) = record.result.as_mut()
        {
            view.summary = if terminal {
                format!("test run {run_id} passed — collecting coverage")
            } else {
                format!(
                    "test run {run_id} {}",
                    if run.outcome.is_empty() {
                        "running"
                    } else {
                        run.outcome.as_str()
                    }
                )
            };
        }
    }

    /// Count a failed poll and stop after five consecutive failures.
    fn fail_test_report(&mut self, record_id: u64) {
        let Some(report) = self.test_reports.get_mut(&record_id) else {
            return;
        };
        report.failures = report.failures.saturating_add(1);
        if report.failures < 5 {
            return;
        }
        self.abandon_test_report(record_id, "could not read the test result");
    }

    /// Stop polling a started test run and mark its row as failed rather than a false success.
    fn abandon_test_report(&mut self, record_id: u64, reason: &str) {
        self.test_reports.remove(&record_id);
        if let Some(record) = self.output_state.find_mut(record_id) {
            record.pending_report = false;
            record.exit_code = Some(1);
            record.ended_at = Some(Instant::now());
            if let Some(view) = record.result.as_mut() {
                view.ok = false;
                view.status = "Report unavailable".to_owned();
                view.summary = reason.to_owned();
                view.items = vec![ResultItem {
                    ok: false,
                    label: "Test results unavailable".to_owned(),
                    detail: reason.to_owned(),
                    progress: None,
                }];
            }
        }
        self.error(format!("Apex test report stopped: {reason}"));
    }

    pub async fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self
            .search_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.search_task.take()
        {
            let _ = task.await;
        }
        if let Some(Modal::Prompt { fields, .. }) = self.modal.as_mut()
            && fields.iter().any(PromptField::debounce_ready)
        {
            for field in fields.iter_mut() {
                field.ensure_suggestions();
            }
        }
        if self
            .probe_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.probe_task.take()
        {
            let _ = task.await;
        }
        let finished_ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, entry)| entry.finished_seen && entry.job.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished_ids {
            if let Some(entry) = self.jobs.remove(&id) {
                let _ = entry.job.join().await;
            }
        }
        self.start_pending_jobs();
        let in_flight: Vec<u64> = self
            .jobs
            .values()
            .filter_map(|entry| match entry.purpose {
                JobPurpose::Report { transfer_job, .. } => Some(transfer_job),
                _ => None,
            })
            .collect();
        let now = Instant::now();
        let due: Vec<(u64, String, TransferOptions)> = self
            .deploy_reports
            .iter()
            .filter(|(id, report)| now >= report.poll_after && !in_flight.contains(id))
            .map(|(id, report)| (*id, report.server_job_id.clone(), report.options.clone()))
            .collect();
        for (record_id, server_job_id, options) in due {
            let Some(report) = self.deploy_reports.get_mut(&record_id) else {
                continue;
            };
            if report.attempts >= 150 {
                self.deploy_reports.remove(&record_id);
                if let Some(record) = self.output_state.find_mut(record_id) {
                    record.exit_code = Some(1);
                    record.ended_at = Some(Instant::now());
                    if let Some(view) = record.result.as_mut() {
                        view.ok = false;
                        view.status = "Report unavailable".to_owned();
                        view.summary = "deploy report timed out".to_owned();
                    }
                }
                self.error("Deploy report timed out; F2 jobs shows the last state");
                continue;
            }
            report.attempts = report.attempts.saturating_add(1);
            report.poll_after = Instant::now() + Duration::from_secs(4);
            if let Some(argv) = options.report_argv(&server_job_id) {
                self.spawn(
                    argv,
                    JobPurpose::Report {
                        options,
                        transfer_job: record_id,
                    },
                );
            }
        }

        let test_in_flight: Vec<u64> = self
            .jobs
            .values()
            .filter_map(|entry| match entry.purpose {
                JobPurpose::TestReport { record_id } => Some(record_id),
                _ => None,
            })
            .collect();
        let due_tests: Vec<(u64, String, Option<String>)> = self
            .test_reports
            .iter()
            .filter(|(id, report)| now >= report.poll_after && !test_in_flight.contains(id))
            .map(|(id, report)| (*id, report.run_id.clone(), report.org.clone()))
            .collect();
        for (record_id, run_id, org) in due_tests {
            let attempts = self
                .test_reports
                .get(&record_id)
                .map(|report| report.attempts)
                .unwrap_or(0);
            if attempts >= 200 {
                self.abandon_test_report(record_id, "timed out waiting for the org");
                continue;
            }
            if let Some(report) = self.test_reports.get_mut(&record_id) {
                report.attempts = report.attempts.saturating_add(1);
                report.poll_after = now + Duration::from_secs(3);
            }
            self.spawn(
                crate::sf::commands::apex_test_result_argv(&run_id, org.as_deref()),
                JobPurpose::TestReport { record_id },
            );
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.config.save().map_err(anyhow::Error::from)
    }

    pub fn spinner_tick(&self) -> usize {
        self.ticks
    }

    fn spawn(&mut self, argv: Vec<String>, purpose: JobPurpose) {
        let Some(installation) = self.startup.installation.clone() else {
            self.error("Salesforce CLI is not installed");
            self.tab = Tab::Setup;
            return;
        };
        self.spawn_external(
            installation.executable.clone(),
            installation.argv(&argv),
            purpose,
        );
    }

    fn spawn_external(
        &mut self,
        executable: std::path::PathBuf,
        argv: Vec<String>,
        purpose: JobPurpose,
    ) {
        let id = self.next_job_id;
        self.next_job_id += 1;
        let command = redact_command(&format!("{} {}", executable.display(), argv.join(" ")));
        let task = task_for_purpose(&purpose);
        if let Some(task) = task
            && matches!(
                task,
                TaskKind::Retrieve
                    | TaskKind::Deploy
                    | TaskKind::Tests
                    | TaskKind::Command
                    | TaskKind::Setup
            )
        {
            self.output_state.begin(
                id,
                command.clone(),
                purpose.label(),
                task,
                executable.clone(),
                argv.clone(),
            );
        }

        if is_user_purpose(&purpose) && self.running_user_jobs() >= MAX_PARALLEL_JOBS {
            self.pending_jobs.push_back(PendingJob {
                id,
                executable,
                argv,
                purpose,
            });
            self.notice(format!(
                "Queued job #{id} — {MAX_PARALLEL_JOBS} jobs already running"
            ));
            return;
        }
        self.start_job(id, executable, argv, purpose);
    }

    fn start_job(&mut self, id: u64, executable: PathBuf, argv: Vec<String>, purpose: JobPurpose) {
        if let Some(record) = self.output_state.find_mut(id) {
            record.queued = false;
            record.started_at = Some(Instant::now());
        }
        match sf::runner::spawn_with_env(
            &executable,
            &argv,
            id,
            self.event_tx.clone(),
            AppEvent::Sf,
            sf::runner::sf_env(),
        ) {
            Ok(job) => {
                self.jobs.insert(
                    id,
                    JobEntry {
                        job,
                        purpose,
                        finished_seen: false,
                    },
                );
            }
            Err(error) => {
                if let Some(record) = self.output_state.find_mut(id) {
                    record.exit_code = None;
                    record.finished = true;
                    record.queued = false;
                    record.ended_at.get_or_insert_with(Instant::now);
                }
                if matches!(purpose, JobPurpose::OrgList) {
                    self.org_state.loading = false;
                }
                if let JobPurpose::Report { transfer_job, .. } = purpose {
                    self.deploy_reports.remove(&transfer_job);
                }
                if let JobPurpose::TestReport { record_id } = purpose {
                    self.fail_test_report(record_id);
                }
                self.error(error.to_string());
            }
        }
    }

    fn running_user_jobs(&self) -> usize {
        self.jobs
            .values()
            .filter(|entry| is_user_purpose(&entry.purpose))
            .count()
    }

    fn start_pending_jobs(&mut self) {
        while self.running_user_jobs() < MAX_PARALLEL_JOBS {
            let Some(pending) = self.pending_jobs.pop_front() else {
                break;
            };
            self.start_job(
                pending.id,
                pending.executable,
                pending.argv,
                pending.purpose,
            );
        }
    }

    fn cancel_active_job(&mut self) -> bool {
        let id = self
            .jobs
            .keys()
            .copied()
            .chain(self.pending_jobs.iter().map(|pending| pending.id))
            .chain(self.test_reports.keys().copied())
            .max();
        id.is_some_and(|id| self.cancel_job(id))
    }

    fn cancel_job(&mut self, id: u64) -> bool {
        let queued = self
            .pending_jobs
            .iter()
            .position(|pending| pending.id == id)
            .and_then(|index| self.pending_jobs.remove(index));
        if let Some(pending) = queued {
            if let Some(record) = self.output_state.find_mut(pending.id) {
                record.finished = true;
                record.queued = false;
                record.ended_at.get_or_insert_with(Instant::now);
            }
            self.notice(format!("Cancelled queued job #{id}"));
            return true;
        }
        let poll_jobs: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, entry)| {
                matches!(entry.purpose, JobPurpose::TestReport { record_id } if record_id == id)
            })
            .map(|(job_id, _)| *job_id)
            .collect();
        if self.test_reports.remove(&id).is_some() {
            for job_id in poll_jobs {
                if let Some(entry) = self.jobs.remove(&job_id) {
                    let _ = entry.job.cancel();
                }
            }
            if let Some(record) = self.output_state.find_mut(id) {
                record.finished = true;
                record.pending_report = false;
                record.exit_code = None;
                record.queued = false;
                record.ended_at.get_or_insert_with(Instant::now);
            }
            self.notice(format!("Cancelled job #{id}"));
            return true;
        }
        let Some(entry) = self.jobs.remove(&id) else {
            return false;
        };
        let purpose = entry.purpose.clone();
        let cancelled = entry.job.cancel();
        if cancelled {
            if let JobPurpose::Report { transfer_job, .. } = purpose {
                self.deploy_reports.remove(&transfer_job);
            }
            self.deploy_reports.remove(&id);
            if let Some(record) = self.output_state.find_mut(id) {
                record.finished = true;
                record.exit_code = None;
                record.queued = false;
                record.ended_at.get_or_insert_with(Instant::now);
            }
            self.notice(format!("Cancelled job #{id}"));
            self.start_pending_jobs();
        }
        cancelled
    }

    /// Cancel every tracked job. Used during shutdown and by the quit confirmation.
    pub async fn shutdown(&mut self) {
        self.pending_jobs.clear();
        let jobs = std::mem::take(&mut self.jobs);
        for (_, entry) in jobs {
            let _ = entry.job.cancel_and_join().await;
        }
    }

    pub fn busy(&self) -> bool {
        !self.jobs.is_empty() || !self.pending_jobs.is_empty() || !self.test_reports.is_empty()
    }

    pub fn job_count(&self) -> usize {
        // Apex poll jobs are internal; their pending run is counted once via `test_reports`.
        self.jobs
            .values()
            .filter(|entry| !matches!(entry.purpose, JobPurpose::TestReport { .. }))
            .count()
            + self.pending_jobs.len()
            + self.test_reports.len()
    }

    pub fn active_job_label(&self) -> Option<String> {
        if self.jobs.is_empty() {
            return self.pending_jobs.front().map(|_| "queued".to_owned());
        }
        self.jobs
            .iter()
            .max_by_key(|(id, _)| **id)
            .map(|(_, entry)| entry.purpose.label().to_owned())
    }

    /// Handle one key press. Returns true when the application should exit.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if let Some(modal) = self.modal.take() {
            self.handle_modal_key(modal, key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    if !self.cancel_active_job() {
                        self.should_quit = true;
                    }
                    return;
                }
                KeyCode::Char('x') => {
                    self.cancel_active_job();
                    return;
                }
                KeyCode::Char('y') => {
                    self.copy_context();
                    return;
                }
                KeyCode::Char('u') => {
                    if matches!(self.tab, Tab::Retrieve | Tab::Deploy) {
                        self.transfer_search.query.clear();
                        self.transfer_search.mark_edited();
                    } else if self.tab == Tab::Tests {
                        self.test_search.query.clear();
                        self.test_search.mark_edited();
                    }
                    return;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::F(2) {
            self.open_jobs();
            return;
        }
        if key.code == KeyCode::F(4) {
            self.open_switch_org();
            return;
        }
        if matches!(self.tab, Tab::Retrieve | Tab::Deploy) {
            self.transfer_key(key);
            return;
        }
        if self.tab == Tab::Tests {
            self.test_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                if self.busy() {
                    self.modal = Some(Modal::Confirm {
                        title: "Quit while a job is running?".to_owned(),
                        message:
                            "The running command will be cancelled.\n\nEnter quits · Esc stays"
                                .to_owned(),
                        action: PendingAction::Quit,
                    });
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help { scroll: 0 }),
            KeyCode::F(5) => self.refresh_current_tab(),
            KeyCode::Esc => self.go_back(),
            KeyCode::Char('h') => self.go_home(),
            KeyCode::Char('y') => self.copy_context(),
            _ => match self.tab {
                Tab::Home => self.home_key(key),
                Tab::Retrieve | Tab::Deploy => self.transfer_key(key),
                Tab::Tests => self.test_key(key),
                Tab::Orgs => self.org_key(key),
                Tab::Setup => self.setup_key(key),
            },
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if tab == Tab::Tests {
            self.ensure_suggestion_sets();
        }
        if tab == Tab::Orgs
            && self.startup.installation.is_some()
            && self.org_state.orgs.is_empty()
            && !self.org_state.loading
        {
            self.refresh_orgs();
        }
        if matches!(tab, Tab::Retrieve | Tab::Deploy) {
            let retrieve = tab == Tab::Retrieve;
            if self.transfer_search.retrieve != retrieve {
                self.transfer_search = TransferSearchState::new(retrieve);
            }
        }
        if tab != self.tab {
            self.tab_history.push(self.tab);
            if self.tab_history.len() > 16 {
                self.tab_history.remove(0);
            }
        }
        self.tab = tab;
        // Key hints live in the footer; navigation itself needs no status text.
        self.status.clear();
    }

    fn go_home(&mut self) {
        self.set_tab(Tab::Home);
    }

    fn go_back(&mut self) {
        let previous = self.tab_history.pop().unwrap_or(Tab::Home);
        self.tab = previous;
        self.notice(format!("Back to {}", self.tab.label()));
    }

    fn refresh_current_tab(&mut self) {
        match self.tab {
            Tab::Orgs => self.refresh_orgs(),
            Tab::Retrieve | Tab::Deploy => {
                self.transfer_search.mark_edited();
                self.notice("Component search refreshed");
            }
            _ => self.notice("Nothing to refresh on this tab"),
        }
    }

    fn copy_context(&mut self) {
        let text = match self.tab {
            Tab::Home => None,
            Tab::Retrieve | Tab::Deploy => Some(
                self.transfer_search
                    .checked()
                    .iter()
                    .map(|item| item.value.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Tab::Tests => Some(
                self.test_search
                    .checked()
                    .iter()
                    .map(|item| item.value.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Tab::Orgs => self.selected_org().map(|org| org.username.clone()),
            Tab::Setup => sf::install_instructions()
                .first()
                .map(|(_, command)| (*command).to_owned()),
        };
        let Some(text) = text else {
            return;
        };
        let tx = self.event_tx.clone();
        let send_result = move |result: Result<(), clipboard::ClipboardError>| {
            let event = match result {
                Ok(()) => AppEvent::Notice {
                    text: "Copied to clipboard".to_owned(),
                    error: false,
                },
                Err(error) => AppEvent::Notice {
                    text: error.to_string(),
                    error: true,
                },
            };
            let _ = tx.send(event);
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || send_result(clipboard::copy(&text)));
        } else {
            send_result(clipboard::copy(&text));
        }
    }

    // ----- Home ---------------------------------------------------------------------

    fn home_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.home_state.selected = self.home_state.selected.saturating_sub(1),
            KeyCode::Down => {
                self.home_state.selected = (self.home_state.selected + 1).min(MENU_ITEMS.len() - 1)
            }
            KeyCode::Enter => self.run_menu_action(self.home_state.selected),
            KeyCode::Char(character) => {
                if let Some(index) = MENU_ITEMS
                    .iter()
                    .position(|item| item.key == character.to_ascii_lowercase())
                {
                    self.run_menu_action(index);
                }
            }
            _ => {}
        }
    }

    fn run_menu_action(&mut self, index: usize) {
        let Some(item) = MENU_ITEMS.get(index) else {
            return;
        };
        match item.action {
            MenuAction::Retrieve | MenuAction::Deploy => {
                let retrieve = item.action == MenuAction::Retrieve;
                self.ensure_suggestion_sets();
                // Keep the previous query and selection so more components can be added while
                // an earlier retrieve or deploy is still running.
                if self.transfer_search.retrieve != retrieve {
                    self.transfer_search = TransferSearchState::new(retrieve);
                }
                self.set_tab(if retrieve { Tab::Retrieve } else { Tab::Deploy });
                self.notice("Type a component name to search");
            }
            MenuAction::Create => {
                let target = self.org_target_hint();
                self.open_prompt(
                    "Create metadata",
                    PromptKind::CreateMetadata,
                    vec![
                        PromptField::new("Template", "")
                            .with_suggestions(Arc::new(crate::sf::metadata::create_suggestions())),
                        PromptField::new("Name", ""),
                        PromptField::new("Org", target),
                    ],
                );
            }
            MenuAction::Tests => {
                self.ensure_suggestion_sets();
                self.set_tab(Tab::Tests);
                self.notice("Type a test class name to search");
            }
            MenuAction::Organizations => self.set_tab(Tab::Orgs),
            MenuAction::RunCommand => self.open_prompt(
                "Run command",
                PromptKind::RunCommand,
                vec![PromptField::new("Command", "")],
            ),
        }
    }

    fn start_search(&mut self, kind: SearchKind) {
        self.ensure_suggestion_sets();
        let state = if kind == SearchKind::Tests {
            &self.test_search
        } else {
            &self.transfer_search
        };
        if state.searching {
            self.notice("Search still running");
            return;
        }
        let query = state.query.trim().to_owned();
        let retrieve = kind == SearchKind::Transfer && self.transfer_search.retrieve;
        let root = self.suggestion_root();
        let set = if kind == SearchKind::Tests {
            self.test_suggestion_cache
                .as_ref()
                .filter(|(cached, _)| cached == &root)
                .map(|(_, set)| Arc::clone(set))
        } else {
            self.suggestion_cache
                .as_ref()
                .filter(|(cached, _)| cached == &root)
                .map(|(_, set)| Arc::clone(set))
        };
        let Some(set) = set else {
            self.error("Component index is still building; try again in a moment");
            self.ensure_suggestion_sets();
            return;
        };
        let generation = if kind == SearchKind::Tests {
            self.test_search_generation = self.test_search_generation.wrapping_add(1);
            self.test_search_generation
        } else {
            self.transfer_search_generation = self.transfer_search_generation.wrapping_add(1);
            self.transfer_search_generation
        };
        if kind == SearchKind::Tests {
            self.test_search.searching = true;
        } else {
            self.transfer_search.searching = true;
        }
        let tx = self.event_tx.clone();
        let filter_query = query.clone();
        self.search_task = Some(tokio::spawn(async move {
            let results = tokio::task::spawn_blocking(move || set.filter(&filter_query, 20))
                .await
                .unwrap_or_default();
            let _ = tx.send(AppEvent::TransferResults {
                kind,
                retrieve,
                query,
                results,
                generation,
            });
        }));
        self.notice("Searching…");
    }

    fn transfer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                self.transfer_search.cursor = self.transfer_search.cursor.saturating_sub(1)
            }
            KeyCode::Down => {
                if !self.transfer_search.results.is_empty() {
                    self.transfer_search.cursor = (self.transfer_search.cursor + 1)
                        .min(self.transfer_search.results.len() - 1);
                }
            }
            KeyCode::Char(' ') => self.transfer_search.toggle_result(),
            KeyCode::Enter => {
                let needs_search = self.transfer_search.needs_search();
                if self.transfer_search.searching {
                    self.notice("Search still running");
                } else if needs_search {
                    self.start_search(SearchKind::Transfer);
                } else {
                    self.open_transfer_confirm();
                }
            }
            KeyCode::Backspace => {
                self.transfer_search.query.pop();
                self.transfer_search.mark_edited();
            }
            KeyCode::F(5) => {
                self.transfer_search.mark_edited();
                if self.transfer_search.query.trim().is_empty() {
                    self.notice("Type a component name, then press Enter");
                } else {
                    self.start_search(SearchKind::Transfer);
                }
            }
            KeyCode::Esc => self.go_back(),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transfer_search.query.push(character);
                self.transfer_search.mark_edited();
            }
            _ => {}
        }
    }

    fn open_transfer_confirm(&mut self) {
        let checked = self.transfer_search.checked();
        if checked.is_empty() {
            self.error("Select at least one component with Space");
            return;
        }
        let retrieve = self.transfer_search.retrieve;
        let mut options = TransferOptions {
            retrieve,
            mode: PayloadMode::Metadata,
            metadata_names: checked.iter().map(|item| item.value.clone()).collect(),
            target_org: self.org_target_hint().into(),
            ..TransferOptions::default()
        };
        options.target_org = options.target_org.filter(|org| !org.is_empty());
        let entries = self
            .transfer_search
            .selected
            .iter()
            .map(|entry| TransferSelection {
                item: entry.item.clone(),
                checked: entry.checked,
            })
            .collect();
        self.modal = Some(Modal::TransferConfirm {
            retrieve,
            entries,
            focus: 0,
            show_command: false,
            options,
        });
    }

    /// Start building the completion index without blocking the interface.
    ///
    /// Reading every manifest in a large project takes long enough to be felt as input lag, so
    /// the index is built on a blocking pool task and installed when it arrives.
    fn suggestion_root(&self) -> PathBuf {
        self.deploy_state
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default()
    }

    fn ensure_suggestion_sets(&mut self) {
        let root = self.suggestion_root();
        if self
            .suggestion_cache
            .as_ref()
            .is_some_and(|(cached_root, _)| cached_root == &root)
        {
            return;
        }
        if self.suggestions_building {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // Without a runtime (unit tests) build inline so behaviour stays deterministic.
            let (all, tests) = build_suggestion_sets(self.deploy_state.project.clone());
            self.suggestion_cache = Some((root.clone(), all));
            self.test_suggestion_cache = Some((root, tests));
            return;
        };
        self.suggestions_building = true;
        let project = self.deploy_state.project.clone();
        let cache_root = root.clone();
        let tx = self.event_tx.clone();
        handle.spawn(async move {
            let built = tokio::task::spawn_blocking(move || build_suggestion_sets(project)).await;
            match built {
                Ok((all, tests)) => {
                    let _ = tx.send(AppEvent::Suggestions {
                        root: cache_root,
                        all,
                        tests,
                    });
                }
                Err(_) => {
                    let _ = tx.send(AppEvent::Notice {
                        text: "Could not build the component index".to_owned(),
                        error: true,
                    });
                }
            }
        });
    }

    fn test_key(&mut self, key: KeyEvent) {
        let coverage = self
            .latest_result(TaskKind::Tests)
            .map(|result| result.coverage.len())
            .unwrap_or(0);
        if self.tests_focus == TestsFocus::Coverage && coverage == 0 {
            self.tests_focus = TestsFocus::Search;
        }
        if self.tests_focus == TestsFocus::Coverage && coverage > 0 {
            let visible = self.tests_coverage_rows.get().max(1);
            let max_scroll = coverage.saturating_sub(visible);
            match key.code {
                KeyCode::Tab => {
                    self.tests_focus = TestsFocus::Search;
                    self.notice("Back to test search");
                }
                KeyCode::Esc => self.go_back(),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.coverage_scroll = self.coverage_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.coverage_scroll = (self.coverage_scroll + 1).min(max_scroll);
                }
                KeyCode::PageUp => {
                    self.coverage_scroll = self.coverage_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.coverage_scroll = (self.coverage_scroll + 10).min(max_scroll);
                }
                KeyCode::Home => self.coverage_scroll = 0,
                KeyCode::End => self.coverage_scroll = max_scroll,
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::Tab && coverage > 0 {
            self.tests_focus = TestsFocus::Coverage;
            let visible = self.tests_coverage_rows.get().max(1);
            self.coverage_scroll = self.coverage_scroll.min(coverage.saturating_sub(visible));
            self.notice("Coverage table — ↑↓ scroll · Tab back to search");
            return;
        }
        match key.code {
            KeyCode::Up => self.test_search.cursor = self.test_search.cursor.saturating_sub(1),
            KeyCode::Down => {
                if !self.test_search.results.is_empty() {
                    self.test_search.cursor =
                        (self.test_search.cursor + 1).min(self.test_search.results.len() - 1);
                }
            }
            KeyCode::Char(' ') => self.test_search.toggle_result(),
            KeyCode::Enter => {
                let needs_search = self.test_search.needs_search();
                if self.test_search.searching {
                    self.notice("Search still running");
                } else if needs_search {
                    self.start_search(SearchKind::Tests);
                } else {
                    self.open_test_confirm();
                }
            }
            KeyCode::Backspace => {
                self.test_search.query.pop();
                self.test_search.mark_edited();
            }
            KeyCode::F(5) => {
                self.test_search.mark_edited();
                if self.test_search.query.trim().is_empty() {
                    self.notice("Type a test class name, then press Enter");
                } else {
                    self.start_search(SearchKind::Tests);
                }
            }
            KeyCode::Esc => self.go_back(),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.test_search.query.push(character);
                self.test_search.mark_edited();
            }
            _ => {}
        }
    }

    fn open_test_confirm(&mut self) {
        let checked = self.test_search.checked();
        if checked.is_empty() {
            self.error("Select at least one test class with Space");
            return;
        }
        let entries = self
            .test_search
            .selected
            .iter()
            .map(|entry| TransferSelection {
                item: entry.item.clone(),
                checked: entry.checked,
            })
            .collect();
        let org = self.org_target_hint();
        self.modal = Some(Modal::TestConfirm {
            entries,
            focus: 0,
            show_command: false,
            org: if org.is_empty() { None } else { Some(org) },
        });
    }

    pub fn org_display(&self) -> String {
        let org = self.org_target_hint();
        if !org.is_empty() {
            org
        } else if self.org_state.loading {
            "loading org…".to_owned()
        } else {
            "no default org".to_owned()
        }
    }

    pub fn org_target_hint(&self) -> String {
        self.org_state
            .orgs
            .iter()
            .find(|org| org.is_default)
            .map(|org| org.label())
            .or_else(|| self.startup.current_org.clone())
            .unwrap_or_default()
    }

    /// The newest finished result for a task, used to show check marks on its own screen.
    pub fn latest_result(&self, kind: TaskKind) -> Option<&ResultView> {
        self.output_state
            .records
            .iter()
            .rev()
            .find(|record| record.task == kind && record.finished)
            .and_then(|record| record.result.as_ref())
    }

    /// Every visible job in Jobs-window order: running newest first, then queued in FIFO
    /// order, then finished newest first.
    pub fn job_rows(&self) -> Vec<JobRow<'_>> {
        let mut running: Vec<&OutputRecord> = self
            .output_state
            .records
            .iter()
            .filter(|record| record.is_running())
            .collect();
        running.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let mut queued: Vec<&OutputRecord> = self
            .output_state
            .records
            .iter()
            .filter(|record| record.queued && !record.finished)
            .collect();
        queued.sort_by_key(|record| record.id);
        let mut finished: Vec<&OutputRecord> = self
            .output_state
            .records
            .iter()
            .filter(|record| record.finished && !record.pending_report)
            .collect();
        finished.sort_by(|left, right| {
            right
                .ended_at
                .cmp(&left.ended_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let mut rows = Vec::with_capacity(running.len() + queued.len() + finished.len());
        rows.extend(running.into_iter().map(|record| JobRow {
            section: JobSection::Running,
            record,
        }));
        rows.extend(queued.into_iter().map(|record| JobRow {
            section: JobSection::Queued,
            record,
        }));
        rows.extend(finished.into_iter().map(|record| JobRow {
            section: JobSection::Finished,
            record,
        }));
        rows
    }

    /// Open the global Jobs window, selecting the newest running job or the newest result.
    /// F4: pick the default org from anywhere without visiting the Orgs screen.
    fn open_switch_org(&mut self) {
        if self.startup.installation.is_none() {
            self.error("Salesforce CLI is not installed");
            return;
        }
        if self.org_state.orgs.is_empty() && !self.org_state.loading {
            self.refresh_orgs();
        }
        let selected = self
            .org_state
            .orgs
            .iter()
            .position(|org| org.is_default)
            .unwrap_or(0);
        self.modal = Some(Modal::SwitchOrg { selected });
    }

    fn open_jobs(&mut self) {
        let rows = self.job_rows();
        let selected = rows
            .iter()
            .position(|row| row.section == JobSection::Running)
            .or_else(|| {
                rows.iter()
                    .position(|row| row.section == JobSection::Finished)
            })
            .unwrap_or(0);
        self.modal = Some(Modal::Jobs {
            selected,
            focus: JobsFocus::List,
            scroll: 0,
            follow: true,
        });
    }

    /// Drop every finished job row; running, queued and pending-report rows are untouched.
    fn clear_finished_jobs(&mut self) {
        self.output_state
            .records
            .retain(|record| !record.finished || record.pending_report);
        let kept: Vec<u64> = self
            .output_state
            .records
            .iter()
            .map(|record| record.id)
            .collect();
        // Cleared deploy-start and test-run rows must stop their report polling as well.
        self.deploy_reports.retain(|id, _| kept.contains(id));
        self.test_reports.retain(|id, _| kept.contains(id));
    }

    // ----- Orgs ---------------------------------------------------------------------

    pub fn selected_org(&self) -> Option<&OrgSummary> {
        self.org_state.orgs.get(self.org_state.selected)
    }

    fn refresh_orgs(&mut self) {
        self.org_state.loading = true;
        self.spawn(crate::sf::commands::org_list(), JobPurpose::OrgList);
    }

    fn org_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.org_state.selected = self.org_state.selected.saturating_sub(1),
            KeyCode::Down => {
                if !self.org_state.orgs.is_empty() {
                    self.org_state.selected =
                        (self.org_state.selected + 1).min(self.org_state.orgs.len() - 1);
                }
            }
            KeyCode::PageUp => self.org_state.selected = self.org_state.selected.saturating_sub(10),
            KeyCode::PageDown => {
                if !self.org_state.orgs.is_empty() {
                    self.org_state.selected =
                        (self.org_state.selected + 10).min(self.org_state.orgs.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(org) = self.selected_org() {
                    let username = org.username.clone();
                    self.org_state.detail = None;
                    self.org_state.detail_for = None;
                    let argv = crate::sf::commands::org_display(Some(&username));
                    self.spawn(argv, JobPurpose::OrgDetail { username });
                }
            }
            KeyCode::Char('o') => {
                if let Some(url) = self.selected_org().map(|org| org.instance_url.clone()) {
                    if url.is_empty() {
                        self.error("This org has no instance URL");
                    } else if !url.starts_with("https://") {
                        self.error("Refusing to open a non-HTTPS instance URL");
                    } else {
                        match open::that(&url) {
                            Ok(()) => self.notice("Opened org in browser"),
                            Err(error) => self.error(format!("Could not open browser: {error}")),
                        }
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(org) = self.selected_org() {
                    let username = org.username.clone();
                    let devhub = key.code == KeyCode::Char('D');
                    let name = if devhub {
                        "target-dev-hub"
                    } else {
                        "target-org"
                    };
                    self.modal = Some(Modal::Confirm {
                        title: if devhub {
                            "Set default Dev Hub?"
                        } else {
                            "Set default org?"
                        }
                        .to_owned(),
                        message: format!("Run: sf config set {name}={username} (project config)"),
                        action: PendingAction::SetDefault { username, devhub },
                    });
                }
            }
            KeyCode::Char('x') => {
                if let Some(org) = self.selected_org() {
                    let username = org.username.clone();
                    self.modal = Some(Modal::Confirm {
                        title: "Log out of org?".to_owned(),
                        message: format!("This removes local authentication for {username}."),
                        action: PendingAction::Logout { username },
                    });
                }
            }
            KeyCode::Char('l') => {
                self.open_prompt(
                    "Log in",
                    PromptKind::LoginWeb,
                    vec![PromptField::new("Alias", "")],
                );
            }
            KeyCode::Char('c') => {
                self.open_prompt(
                    "Create scratch org",
                    PromptKind::CreateScratch,
                    vec![
                        PromptField::new("Alias", ""),
                        PromptField::new("Definition file (optional)", ""),
                        PromptField::new("Duration in days", "7"),
                    ],
                );
            }
            _ => {}
        }
    }

    // ----- Deploy / Retrieve --------------------------------------------------------

    fn run_transfer(&mut self) {
        let options = self.deploy_state.options.clone();
        let argv = options.argv();
        self.spawn(argv, JobPurpose::Transfer { options });
    }

    // ----- Dialogs and actions -------------------------------------------------------

    fn open_prompt(&mut self, title: &str, kind: PromptKind, fields: Vec<PromptField>) {
        self.modal = Some(Modal::Prompt {
            title: title.to_owned(),
            kind,
            fields,
            focus: 0,
        });
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) {
        match modal {
            Modal::Help { mut scroll } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.modal = Some(Modal::Help { scroll });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    self.modal = Some(Modal::Help { scroll });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.modal = Some(Modal::Help { scroll });
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    self.modal = Some(Modal::Help { scroll });
                }
                KeyCode::Home => self.modal = Some(Modal::Help { scroll: 0 }),
                KeyCode::End => {
                    self.modal = Some(Modal::Help {
                        scroll: u16::MAX as usize,
                    })
                }
                _ => {}
            },
            Modal::Message { .. } => {}
            Modal::Confirm {
                title,
                message,
                action,
            } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.execute(action),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => self.notice("Cancelled"),
                _ => {
                    self.modal = Some(Modal::Confirm {
                        title,
                        message,
                        action,
                    });
                }
            },
            Modal::Jobs {
                selected,
                focus,
                scroll,
                follow,
            } => self.handle_jobs_key(selected, focus, scroll, follow, key),
            Modal::SwitchOrg { mut selected } => {
                let count = self.org_state.orgs.len();
                match key.code {
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.modal = Some(Modal::SwitchOrg { selected });
                    }
                    KeyCode::Down => {
                        if count > 0 {
                            selected = (selected + 1).min(count - 1);
                        }
                        self.modal = Some(Modal::SwitchOrg { selected });
                    }
                    KeyCode::PageUp => {
                        selected = selected.saturating_sub(10);
                        self.modal = Some(Modal::SwitchOrg { selected });
                    }
                    KeyCode::PageDown => {
                        if count > 0 {
                            selected = (selected + 10).min(count - 1);
                        }
                        self.modal = Some(Modal::SwitchOrg { selected });
                    }
                    KeyCode::F(5) => {
                        self.refresh_orgs();
                        self.modal = Some(Modal::SwitchOrg { selected });
                    }
                    KeyCode::Char('l') => {
                        self.open_prompt(
                            "Log in",
                            PromptKind::LoginWeb,
                            vec![PromptField::new("Alias", "")],
                        );
                    }
                    KeyCode::Enter => {
                        let Some(org) = self.org_state.orgs.get(selected) else {
                            self.notice("No orgs loaded yet — F5 refreshes the list");
                            self.modal = Some(Modal::SwitchOrg { selected });
                            return;
                        };
                        let username = org.username.clone();
                        let label = org.label();
                        if org.is_default {
                            self.notice(format!("{label} is already the default org"));
                            self.modal = None;
                            return;
                        }
                        // Optimistic header update; the org refresh confirms it.
                        self.startup.current_org = Some(label.clone());
                        self.spawn(
                            crate::sf::commands::config_set("target-org", &username),
                            JobPurpose::OrgAction {
                                description: format!("Default org set to {label}"),
                            },
                        );
                        self.modal = None;
                    }
                    KeyCode::Esc => self.modal = None,
                    _ => self.modal = Some(Modal::SwitchOrg { selected }),
                }
            }
            Modal::TestConfirm {
                mut entries,
                mut focus,
                mut show_command,
                mut org,
            } => match key.code {
                KeyCode::Up => {
                    focus = focus.saturating_sub(1);
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
                KeyCode::Down => {
                    if !entries.is_empty() {
                        focus = (focus + 1).min(entries.len() - 1);
                    }
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
                KeyCode::Char(' ') => {
                    if let Some(entry) = entries.get_mut(focus) {
                        entry.checked = !entry.checked;
                    }
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
                KeyCode::Enter => {
                    let classes: Vec<String> = entries
                        .iter()
                        .filter(|entry| entry.checked)
                        .map(|entry| {
                            entry
                                .item
                                .value
                                .split_once(':')
                                .map_or(entry.item.value.clone(), |(_, name)| name.to_owned())
                        })
                        .collect();
                    if classes.is_empty() {
                        self.error("Select at least one test class");
                        self.modal = Some(Modal::TestConfirm {
                            entries,
                            focus,
                            show_command,
                            org,
                        });
                        return;
                    }
                    let argv = crate::sf::commands::apex_test_argv(&classes, org.as_deref());
                    let display = crate::sf::commands::apex_test_display(&classes, org.as_deref());
                    self.spawn(
                        argv,
                        JobPurpose::ApexTests {
                            command: display,
                            org,
                        },
                    );
                }
                KeyCode::Esc => {
                    self.set_tab(Tab::Tests);
                    self.notice("Back to test selection");
                }
                KeyCode::Char('c') => {
                    show_command = !show_command;
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
                KeyCode::Char('g') => {
                    org = cycle_org(org.clone(), &self.org_state.orgs);
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
                _ => {
                    self.modal = Some(Modal::TestConfirm {
                        entries,
                        focus,
                        show_command,
                        org,
                    });
                }
            },
            Modal::TransferConfirm {
                retrieve,
                mut entries,
                mut focus,
                mut show_command,
                mut options,
            } => match key.code {
                KeyCode::Up => {
                    focus = focus.saturating_sub(1);
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Down => {
                    if !entries.is_empty() {
                        focus = (focus + 1).min(entries.len() - 1);
                    }
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Char(' ') => {
                    if let Some(entry) = entries.get_mut(focus) {
                        entry.checked = !entry.checked;
                    }
                    options.metadata_names = entries
                        .iter()
                        .filter(|entry| entry.checked)
                        .map(|entry| entry.item.value.clone())
                        .collect();
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Enter => {
                    let names: Vec<String> = entries
                        .iter()
                        .filter(|entry| entry.checked)
                        .map(|entry| entry.item.value.clone())
                        .collect();
                    if names.is_empty() {
                        self.error("Select at least one component");
                        self.modal = Some(Modal::TransferConfirm {
                            retrieve,
                            entries,
                            focus,
                            show_command,
                            options,
                        });
                        return;
                    }
                    options.metadata_names = names;
                    self.deploy_state.options = options;
                    self.run_transfer();
                }
                KeyCode::Esc => {
                    self.set_tab(if retrieve { Tab::Retrieve } else { Tab::Deploy });
                    self.notice("Back to selection");
                }
                KeyCode::Char('c') => {
                    show_command = !show_command;
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Char('v') if !retrieve => {
                    options.check_only = !options.check_only;
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Char('w') if !retrieve => {
                    options.ignore_warnings = !options.ignore_warnings;
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Char('t') if !retrieve => {
                    options.test_level = options.test_level.cycle();
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                KeyCode::Char('g') => {
                    options.target_org =
                        cycle_org(options.target_org.clone(), &self.org_state.orgs);
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
                _ => {
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve,
                        entries,
                        focus,
                        show_command,
                        options,
                    });
                }
            },
            Modal::Prompt {
                title,
                kind,
                mut fields,
                mut focus,
            } => match key.code {
                KeyCode::Esc => self.notice("Cancelled"),
                KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                    if fields.is_empty() {
                        return;
                    }
                    if let Some(field) = fields.get_mut(focus) {
                        field.ensure_suggestions();
                    }
                    let suggestions = fields.get(focus).map(suggestions_for).unwrap_or_default();
                    let use_suggestions = !suggestions.is_empty()
                        && (key.code == KeyCode::Tab || focus_has_suggestions(&fields, focus));
                    if use_suggestions && key.code != KeyCode::Tab {
                        if let Some(field) = fields.get_mut(focus) {
                            let current = field.suggestion_index.unwrap_or(0) as isize;
                            let delta = if key.code == KeyCode::Down {
                                1
                            } else {
                                suggestions.len() as isize - 1
                            };
                            field.suggestion_index =
                                Some((current + delta).rem_euclid(suggestions.len() as isize)
                                    as usize);
                        }
                    } else {
                        if key.code == KeyCode::Tab && use_suggestions {
                            accept_suggestion(fields.get_mut(focus), &suggestions);
                        }
                        let delta = if matches!(key.code, KeyCode::Tab | KeyCode::Down) {
                            1
                        } else {
                            fields.len() as isize - 1
                        };
                        focus = (focus as isize + delta).rem_euclid(fields.len() as isize) as usize;
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Enter => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.ensure_suggestions();
                        let suggestions = suggestions_for(field);
                        if field.suggestion_index.is_some() || suggestions.len() == 1 {
                            accept_suggestion(Some(field), &suggestions);
                        }
                    }
                    if !self.submit_prompt(&kind, &fields) {
                        self.modal = Some(Modal::Prompt {
                            title,
                            kind,
                            fields,
                            focus,
                        });
                    }
                }
                KeyCode::Backspace => {
                    if let Some(field) = fields.get_mut(focus) {
                        let cursor = previous_boundary(&field.value, field.cursor);
                        if cursor < field.cursor {
                            field.value.replace_range(cursor..field.cursor, "");
                            field.cursor = cursor;
                            field.mark_edited();
                        }
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Left => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.cursor = previous_boundary(&field.value, field.cursor);
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Right => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.cursor = next_boundary(&field.value, field.cursor);
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Char(' ') if fields.get(focus).is_some_and(|field| field.multi_select) => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.ensure_suggestions();
                        let value = field
                            .cached_suggestions()
                            .get(field.suggestion_index.unwrap_or(0))
                            .map(|item| item.value.clone());
                        if let Some(value) = value {
                            field.toggle_current(&value);
                        }
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.cursor = floor_boundary(&field.value, field.cursor);
                        field.value.insert(field.cursor, character);
                        field.cursor += character.len_utf8();
                        field.mark_edited();
                    }
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                _ => {
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    })
                }
            },
        }
    }

    fn handle_jobs_key(
        &mut self,
        mut selected: usize,
        mut focus: JobsFocus,
        mut scroll: usize,
        mut follow: bool,
        key: KeyEvent,
    ) {
        let mut close = false;
        let mut reset_view = false;
        let rows = self.job_rows();
        if rows.is_empty() {
            if key.code == KeyCode::Esc {
                close = true;
            }
        } else {
            let count = rows.len();
            selected = selected.min(count - 1);
            let row = rows[selected];
            let record = row.record;
            let live = row.section != JobSection::Finished;
            let queued_position = rows
                .iter()
                .filter(|row| row.section == JobSection::Queued)
                .position(|row| row.record.id == record.id)
                .map(|index| index + 1);
            let scroll_key = matches!(
                key.code,
                KeyCode::Char('j' | 'k' | 'g')
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
            );
            let content_len = if scroll_key {
                crate::ui::job_detail_len(self, record, queued_position)
            } else {
                0
            };
            let height = usize::from(self.jobs_detail_height.get()).max(1);
            let max_scroll = content_len.saturating_sub(height);
            let record_id = record.id;
            let record_finished = record.finished;
            drop(rows);

            match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    reset_view = true;
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(count - 1);
                    reset_view = true;
                }
                KeyCode::PageUp if focus == JobsFocus::List => {
                    selected = selected.saturating_sub(5);
                    reset_view = true;
                }
                KeyCode::PageDown if focus == JobsFocus::List => {
                    selected = (selected + 5).min(count - 1);
                    reset_view = true;
                }
                KeyCode::Home if focus == JobsFocus::List => {
                    selected = 0;
                    reset_view = true;
                }
                KeyCode::End if focus == JobsFocus::List => {
                    selected = count - 1;
                    reset_view = true;
                }
                KeyCode::Tab => {
                    focus = match focus {
                        JobsFocus::List => JobsFocus::Detail,
                        JobsFocus::Detail => JobsFocus::List,
                    };
                }
                KeyCode::Enter => {
                    // Enter opens the selected job: focus its content and start at the top,
                    // or at the live tail for a running job. Tab only moves focus.
                    if focus == JobsFocus::List {
                        focus = JobsFocus::Detail;
                        reset_view = true;
                        if live {
                            follow = true;
                        }
                    }
                }
                KeyCode::Esc => {
                    if focus == JobsFocus::Detail {
                        focus = JobsFocus::List;
                    } else {
                        close = true;
                    }
                }
                KeyCode::Char('x') => {
                    if self.cancel_job(record_id) {
                        reset_view = true;
                    } else if record_finished {
                        self.notice("That job already finished");
                    }
                }
                KeyCode::Char('c') => {
                    self.clear_finished_jobs();
                    reset_view = true;
                }
                KeyCode::Char('o') if record_finished => {
                    self.output_state.show_details = !self.output_state.show_details;
                    reset_view = true;
                }
                KeyCode::Char('z') if record_finished => {
                    self.output_state.show_details = true;
                    self.output_state.show_json = !self.output_state.show_json;
                    reset_view = true;
                }
                KeyCode::Char('g') => {
                    scroll = max_scroll;
                    follow = live;
                }
                KeyCode::Char('j') if focus == JobsFocus::Detail => {
                    let current = if live && follow {
                        max_scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    scroll = (current + 1).min(max_scroll);
                    follow = live && scroll >= max_scroll;
                }
                KeyCode::Char('k') if focus == JobsFocus::Detail => {
                    let current = if live && follow {
                        max_scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    scroll = current.saturating_sub(1);
                    follow = false;
                }
                KeyCode::PageUp if focus == JobsFocus::Detail => {
                    let current = if live && follow {
                        max_scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    scroll = current.saturating_sub(10);
                    follow = false;
                }
                KeyCode::PageDown if focus == JobsFocus::Detail => {
                    let current = if live && follow {
                        max_scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    scroll = (current + 10).min(max_scroll);
                    follow = live && scroll >= max_scroll;
                }
                KeyCode::Home if focus == JobsFocus::Detail => {
                    scroll = 0;
                    follow = false;
                }
                KeyCode::End if focus == JobsFocus::Detail => {
                    scroll = max_scroll;
                    follow = live;
                }
                _ => {}
            }
        }
        if close {
            self.modal = None;
            return;
        }
        let rows = self.job_rows();
        let count = rows.len();
        let selected = selected.min(count.saturating_sub(1));
        if reset_view && count > 0 {
            scroll = 0;
            follow = rows[selected].section != JobSection::Finished;
        }
        drop(rows);
        self.modal = Some(Modal::Jobs {
            selected,
            focus,
            scroll,
            follow,
        });
    }

    fn submit_prompt(&mut self, kind: &PromptKind, fields: &[PromptField]) -> bool {
        let values: Vec<String> = fields
            .iter()
            .map(|field| field.value.trim().to_owned())
            .collect();
        match kind {
            PromptKind::RunCommand => {
                let Some(argv) =
                    parse_run_command(values.first().map(String::as_str).unwrap_or(""))
                else {
                    self.error("Enter an sf command, for example: sf org list --json");
                    return false;
                };
                let command = redact_command(&format!("sf {}", argv.join(" ")));
                let warning = if is_secret_command(&command) {
                    "\n\nSecrets are shown as [redacted]; this command may expose credentials, so its output is not retained."
                } else {
                    ""
                };
                self.modal = Some(Modal::Confirm {
                    title: "Run command?".to_owned(),
                    message: format!("{command}{warning}\n\nEnter runs · Esc cancels"),
                    action: PendingAction::RunCommand { argv, command },
                });
            }
            PromptKind::QuickRetrieve | PromptKind::QuickDeploy => {
                let retrieve = matches!(kind, PromptKind::QuickRetrieve);
                let component_field = fields.first();
                let mut components: Vec<String> = component_field
                    .map(|field| field.selected.clone())
                    .unwrap_or_default();
                if components.is_empty() {
                    components = values
                        .first()
                        .map(|value| split_names(value))
                        .unwrap_or_default();
                }
                if components.is_empty() {
                    self.error("Select at least one component with Space, or type a name");
                    return false;
                }
                let mut argv: Vec<String> = vec![
                    "project".to_owned(),
                    if retrieve { "retrieve" } else { "deploy" }.to_owned(),
                    "start".to_owned(),
                ];
                for component in components {
                    argv.push("--metadata".to_owned());
                    argv.push(component);
                }
                if let Some(org) = values.get(1).filter(|org| !org.is_empty()) {
                    argv.push("--target-org".to_owned());
                    argv.push(org.clone());
                }
                argv.push("--json".to_owned());
                let command = format!("sf {}", argv.join(" "));
                self.modal = Some(Modal::Confirm {
                    title: if retrieve {
                        "Confirm retrieve".to_owned()
                    } else {
                        "Confirm deploy".to_owned()
                    },
                    message: format!("{command}\n\nEnter runs · Esc back"),
                    action: PendingAction::RunPrepared { argv },
                });
            }
            PromptKind::CreateMetadata => {
                let template_input = values.first().map(String::as_str).unwrap_or("");
                let template =
                    crate::sf::metadata::create_template_for(template_input).or_else(|| {
                        let hit = crate::sf::metadata::create_suggestions()
                            .filter(template_input, 1)
                            .into_iter()
                            .next()?;
                        crate::sf::metadata::create_template_for(&hit.value)
                    });
                let Some(template) = template else {
                    self.error(
                        "No matching template; try ApexClass, Flow or Lightning Web Component",
                    );
                    return false;
                };
                let name = values.get(1).map(String::as_str).unwrap_or("").trim();
                if name.is_empty() {
                    self.error("Enter the new component name");
                    return false;
                }
                let mut argv: Vec<String> = template
                    .command
                    .iter()
                    .map(|part| (*part).to_owned())
                    .collect();
                argv.push(template.name_flag.to_owned());
                argv.push(name.to_owned());
                let display = format!("sf {}", argv.join(" "));
                self.spawn(
                    argv,
                    JobPurpose::Command {
                        id: template.command.join(" "),
                        command: display,
                    },
                );
            }
            PromptKind::SwitchOrg => {
                let org = values.first().map(String::as_str).unwrap_or("").trim();
                if org.is_empty() {
                    self.error("Enter an org alias");
                    return false;
                }
                self.spawn(
                    crate::sf::commands::config_set("target-org", org),
                    JobPurpose::OrgAction {
                        description: "Switch org".to_owned(),
                    },
                );
            }
            PromptKind::LoginWeb => {
                let alias = values.first().map(String::as_str);
                let set_default = values.get(1).is_some_and(|answer| {
                    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes" | "true")
                });
                self.spawn(
                    crate::sf::commands::login_web(alias, set_default),
                    JobPurpose::OrgAction {
                        description: "Web login".to_owned(),
                    },
                );
            }
            PromptKind::CreateScratch => {
                let alias = values.first().cloned().unwrap_or_default();
                if alias.is_empty() {
                    self.error("An alias is required for a scratch org");
                    return false;
                }
                let definition = values.get(1).map(String::as_str);
                let days = values
                    .get(2)
                    .filter(|days| !days.is_empty())
                    .map(String::as_str)
                    .unwrap_or("7");
                self.spawn(
                    crate::sf::commands::create_scratch(&alias, definition, days),
                    JobPurpose::OrgAction {
                        description: "Scratch org creation".to_owned(),
                    },
                );
            }
            PromptKind::TargetOrg => {
                self.deploy_state.options.target_org =
                    values.first().filter(|value| !value.is_empty()).cloned();
                self.notice("Target org updated");
            }
        }
        true
    }

    fn execute(&mut self, action: PendingAction) {
        match action {
            PendingAction::None => {}
            PendingAction::SetDefault { username, devhub } => {
                let name = if devhub {
                    "target-dev-hub"
                } else {
                    "target-org"
                };
                self.spawn(
                    crate::sf::commands::config_set(name, &username),
                    JobPurpose::OrgAction {
                        description: "Config update".to_owned(),
                    },
                );
            }
            PendingAction::Logout { username } => {
                self.spawn(
                    crate::sf::commands::logout(&username),
                    JobPurpose::OrgAction {
                        description: "Logout".to_owned(),
                    },
                );
            }
            PendingAction::RunTransfer => self.run_transfer(),
            PendingAction::RunInstaller { command } => self.run_installer(&command),
            PendingAction::RunPrepared { argv } => {
                let id = argv.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
                let display = format!("sf {}", argv.join(" "));
                self.spawn(
                    argv,
                    JobPurpose::Command {
                        id,
                        command: display,
                    },
                );
            }
            PendingAction::RunCommand { argv, command } => self.spawn(
                argv,
                JobPurpose::Command {
                    id: "run".to_owned(),
                    command,
                },
            ),
            PendingAction::Quit => self.should_quit = true,
        }
    }

    fn setup_key(&mut self, key: KeyEvent) {
        let count = sf::install_instructions().len();
        match key.code {
            KeyCode::Up => self.setup_selected = self.setup_selected.saturating_sub(1),
            KeyCode::Down => {
                self.setup_selected = (self.setup_selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char('r') => self.reprobe(),
            KeyCode::Enter => {
                if let Some((name, command)) = sf::install_instructions().get(self.setup_selected) {
                    let command = (*command).to_owned();
                    self.modal = Some(Modal::Confirm {
                        title: format!("Run {name} installer?"),
                        message: format!(
                            "This runs: {command}
Output appears in the Jobs window (F2)."
                        ),
                        action: PendingAction::RunInstaller { command },
                    });
                }
            }
            _ => {}
        }
    }

    fn run_installer(&mut self, command: &str) {
        let (shell, argv) = if cfg!(windows) {
            ("cmd.exe", vec!["/C".to_owned(), command.to_owned()])
        } else {
            ("sh", vec!["-c".to_owned(), command.to_owned()])
        };
        self.spawn_external(
            std::path::PathBuf::from(shell),
            argv,
            JobPurpose::Installer {
                command: command.to_owned(),
            },
        );
        self.notice("Installer started — press r on Setup when it finishes");
    }

    fn reprobe(&mut self) {
        if self.probe_task.is_some() {
            self.notice("A probe is already running");
            return;
        }
        self.start_probe();
        self.notice("Re-probing the Salesforce CLI…");
    }
}

/// Cycle `default org (empty) → each known org → default org`.
fn cycle_org(current: Option<String>, orgs: &[OrgSummary]) -> Option<String> {
    let mut choices = vec![String::new()];
    choices.extend(orgs.iter().map(OrgSummary::label));
    if let Some(current) = current.as_ref().filter(|value| !value.is_empty()) {
        choices.push(current.clone());
    }
    choices.sort();
    choices.dedup();
    let index = current
        .as_ref()
        .and_then(|value| choices.iter().position(|choice| choice == value))
        .unwrap_or(0);
    let next = (index + 1) % choices.len().max(1);
    choices.get(next).cloned()
}

fn suggestions_for(field: &PromptField) -> Vec<SuggestionItem> {
    field.cached_suggestions().to_vec()
}

fn focus_has_suggestions(fields: &[PromptField], focus: usize) -> bool {
    fields
        .get(focus)
        .is_some_and(|field| field.suggestion_set.is_some())
}

fn accept_suggestion(field: Option<&mut PromptField>, suggestions: &[SuggestionItem]) {
    let Some(field) = field else {
        return;
    };
    let index = field
        .suggestion_index
        .unwrap_or(0)
        .min(suggestions.len().saturating_sub(1));
    if let Some(suggestion) = suggestions.get(index) {
        field.value = suggestion.value.clone();
        field.cursor = field.value.len();
        field.mark_edited();
        field.ensure_suggestions();
    }
}

/// Split a user-entered list on commas or whitespace, keeping quoted paths intact.
pub(crate) fn split_names(input: &str) -> Vec<String> {
    split_args(input)
        .into_iter()
        .flat_map(|part| {
            part.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Parse the Run command prompt: strip a leading `sf`/`sfdx`, keep quoted arguments together.
pub(crate) fn parse_run_command(input: &str) -> Option<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("sf") || trimmed.eq_ignore_ascii_case("sfdx") {
        return None;
    }
    let mut rest = trimmed;
    if let Some((first, tail)) = trimmed.split_once(char::is_whitespace)
        && (first.eq_ignore_ascii_case("sf") || first.eq_ignore_ascii_case("sfdx"))
    {
        rest = tail.trim_start();
    }
    if rest.is_empty() {
        return None;
    }
    let argv = split_args(rest);
    if argv.is_empty() || argv.first().is_some_and(|arg| arg.starts_with('-')) {
        None
    } else {
        Some(argv)
    }
}

pub(crate) fn split_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in input.chars() {
        match quote {
            Some(marker) if character == marker => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let shortened: String = text.chars().take(max).collect();
    format!("{shortened}…")
}

/// Boundary immediately before the character at or before `cursor`.
fn previous_boundary(value: &str, cursor: usize) -> usize {
    let mut index = cursor.min(value.len());
    while index > 0 {
        index -= 1;
        if value.is_char_boundary(index) {
            return index;
        }
    }
    0
}

/// Largest char boundary at or before `cursor` (safe for slicing/insertion).
fn floor_boundary(value: &str, cursor: usize) -> usize {
    let mut index = cursor.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Smallest char boundary strictly after `cursor`.
fn next_boundary(value: &str, cursor: usize) -> usize {
    if cursor >= value.len() {
        return value.len();
    }
    let mut index = cursor + 1;
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

const SECRET_FLAGS: &[&str] = &[
    "--sfdx-url",
    "--access-token",
    "--password",
    "--client-secret",
    "--private-key",
];

/// Mask values passed to auth/secret flags before a command is displayed, copied or persisted.
fn redact_command(command: &str) -> String {
    let mut parts = Vec::new();
    let mut hide_next = false;
    for part in command.split_whitespace() {
        if hide_next {
            parts.push("[redacted]".to_owned());
            hide_next = false;
            continue;
        }
        let lower = part.to_ascii_lowercase();
        if let Some(flag) = SECRET_FLAGS
            .iter()
            .find(|flag| lower == **flag || lower.starts_with(&format!("{flag}=")))
        {
            if part.split_once('=').is_some() {
                parts.push(format!("{flag}=[redacted]"));
            } else {
                parts.push((*flag).to_owned());
                hide_next = true;
            }
        } else if part.contains("force://") {
            parts.push("[redacted-auth-url]".to_owned());
        } else {
            parts.push(part.to_owned());
        }
    }
    parts.join(" ")
}

/// Build the component and test-class completion indexes for a project.
fn build_suggestion_sets(project: Option<SfProject>) -> (Arc<SuggestionSet>, Arc<SuggestionSet>) {
    let all = Arc::new(crate::sf::metadata::transfer_suggestions(project.as_ref()));
    let tests = Arc::new(crate::sf::metadata::test_suggestions(
        &all,
        project.as_ref(),
    ));
    (all, tests)
}

/// Whether a parsed `apex run test` response is final or needs polling.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApexReportAction {
    Final,
    Poll { run_id: String },
}

fn apex_report_action(run: &test_results::TestRun) -> ApexReportAction {
    // A terminal summary can arrive before the org finishes aggregating per-class coverage.
    // Keep polling when the CLI reported a total but no class rows yet.
    let coverage_pending = run.coverages.is_empty() && !run.coverage.is_empty();
    if run.id.is_empty() || (test_results::is_terminal_outcome(&run.outcome) && !coverage_pending) {
        ApexReportAction::Final
    } else {
        ApexReportAction::Poll {
            run_id: run.id.clone(),
        }
    }
}

/// A lightweight result shown while the org still works on the test run.
fn started_test_view(run_id: &str) -> ResultView {
    ResultView {
        kind: ResultKind::ApexTests,
        title: "Apex tests".to_owned(),
        status: "Running".to_owned(),
        ok: true,
        summary: format!("test run {run_id} started"),
        items: vec![ResultItem {
            ok: true,
            label: format!("Test run {run_id}"),
            detail: "waiting for org results".to_owned(),
            progress: None,
        }],
        coverage: Vec::new(),
        sections: Vec::new(),
    }
}

fn task_for_purpose(purpose: &JobPurpose) -> Option<TaskKind> {
    match purpose {
        JobPurpose::Transfer { options } if options.retrieve => Some(TaskKind::Retrieve),
        JobPurpose::Transfer { .. } => Some(TaskKind::Deploy),
        JobPurpose::ApexTests { .. } => Some(TaskKind::Tests),
        JobPurpose::Command { .. } => Some(TaskKind::Command),
        JobPurpose::Installer { .. } => Some(TaskKind::Setup),
        JobPurpose::OrgList | JobPurpose::OrgDetail { .. } | JobPurpose::OrgAction { .. } => {
            Some(TaskKind::Organization)
        }
        JobPurpose::Report { .. } | JobPurpose::TestReport { .. } => None,
    }
}

fn is_user_purpose(purpose: &JobPurpose) -> bool {
    matches!(
        purpose,
        JobPurpose::Command { .. }
            | JobPurpose::Transfer { .. }
            | JobPurpose::ApexTests { .. }
            | JobPurpose::Installer { .. }
            | JobPurpose::OrgAction { .. }
    )
}

/// Commands whose output or arguments must not be retained or shown verbatim.
fn is_secret_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "org display",
        "org auth",
        "org login",
        "access-token",
        "sfdx-url",
        "user-password",
        "client-secret",
        "--password",
        "private-key",
        "force://",
        "[redacted-auth-url]",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Input delivered by the blocking reader thread.
enum InputEvent {
    Key(KeyEvent),
    Resize,
}

/// Read terminal input on its own thread so a key press wakes the event loop immediately,
/// instead of waiting for the next animation tick. Only this thread reads input events.
fn spawn_input_reader() -> tokio::sync::mpsc::UnboundedReceiver<InputEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            let forward = match event {
                crossterm::event::Event::Key(key) => InputEvent::Key(key),
                crossterm::event::Event::Resize(_, _) => InputEvent::Resize,
                _ => continue,
            };
            if tx.send(forward).is_err() {
                break;
            }
        }
    });
    rx
}

/// Run the main event loop using an already initialized terminal.
pub async fn run(
    terminal: &mut DefaultTerminal,
    startup: Startup,
    config: Config,
    config_dir: PathBuf,
    config_notice: Option<String>,
) -> anyhow::Result<()> {
    let mut app = App::new(startup, config, config_dir);
    if let Some(notice) = config_notice {
        app.error(format!("Configuration reset: {notice}"));
    }
    let mut input_rx = spawn_input_reader();
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    let mut dirty = true;
    while !app.should_quit {
        tokio::select! {
            _ = ticker.tick() => {
                let active = app.busy() || !app.startup_stage.done;
                let prompt_due = match &app.modal {
                    Some(Modal::Prompt { fields, .. }) => {
                        fields.iter().any(PromptField::debounce_ready)
                    }
                    _ => false,
                };
                app.tick().await;
                if active || app.busy() || prompt_due {
                    dirty = true;
                }
            }
            Some(event) = app.recv_event() => {
                app.on_event(event);
                app.drain_events();
                dirty = true;
            }
            Some(input) = input_rx.recv() => {
                match input {
                    InputEvent::Key(key) => app.handle_key(key),
                    InputEvent::Resize => {}
                }
                // Coalesce buffered keys into one frame: typing feels instant and a fast
                // key repeat never queues a redraw per character.
                while let Ok(input) = input_rx.try_recv() {
                    match input {
                        InputEvent::Key(key) => app.handle_key(key),
                        InputEvent::Resize => {}
                    }
                }
                dirty = true;
            }
        }
        if dirty {
            if let Err(error) = terminal.draw(|frame| crate::ui::draw(frame, &app)) {
                app.shutdown().await;
                let _ = app.save();
                return Err(error.into());
            }
            dirty = false;
        }
    }
    app.shutdown().await;
    app.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let _guard = tokio::runtime::Runtime::new()
            .expect("test runtime")
            .enter();
        App::new(
            Startup {
                installation: None,
                version: None,
                current_org: None,
                notices: Vec::new(),
            },
            Config::default(),
            std::path::PathBuf::from("/tmp/sftui-test"),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn menu_keys_open_screens_and_escape_returns_to_previous() {
        let mut app = test_app();
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Char('o')));
        assert_eq!(app.tab, Tab::Orgs);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.tab, Tab::Deploy);
    }

    #[test]
    fn number_and_tab_keys_no_longer_switch_screens() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('4')));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Home);
    }

    #[test]
    fn prompt_text_editing_supports_typing_backspace_and_submit() {
        let mut app = test_app();
        app.open_prompt(
            "Target org",
            PromptKind::TargetOrg,
            vec![PromptField::new("Alias", "dev")],
        );
        app.handle_key(key(KeyCode::Char('!')));
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.deploy_state.options.target_org.as_deref(), Some("dev"));
    }

    #[test]
    fn split_args_respects_quotes() {
        assert_eq!(
            split_args("--target-org 'my org' --json"),
            vec!["--target-org", "my org", "--json"]
        );
    }

    fn record(id: u64, task: TaskKind, exit_code: Option<i32>) -> OutputRecord {
        let now = Instant::now();
        let finished = exit_code.is_some();
        OutputRecord {
            id,
            command: format!("sf project transfer --job {id}"),
            purpose: task.label().to_owned(),
            task,
            executable: "sf".into(),
            argv: vec!["project".to_owned()],
            lines: Vec::new(),
            line_bytes: 0,
            exit_code,
            finished,
            queued: false,
            started_at: Some(now),
            ended_at: finished.then_some(now),
            json: None,
            result: None,
            pending_report: false,
        }
    }

    #[test]
    fn latest_result_belongs_to_its_own_task() {
        let mut app = test_app();
        let mut retrieve = record(1, TaskKind::Retrieve, Some(0));
        retrieve.result = Some(ResultView {
            kind: ResultKind::Retrieve,
            title: "Retrieve".to_owned(),
            status: "Succeeded".to_owned(),
            ok: true,
            summary: "2 total · 2 succeeded · 0 failed".to_owned(),
            items: Vec::new(),
            coverage: Vec::new(),
            sections: Vec::new(),
        });
        app.output_state.records.push(retrieve);
        app.output_state
            .records
            .push(record(2, TaskKind::Deploy, None));
        let result = app.latest_result(TaskKind::Retrieve).expect("retrieve");
        assert_eq!(result.summary, "2 total · 2 succeeded · 0 failed");
        assert!(app.latest_result(TaskKind::Deploy).is_none());
        assert!(app.latest_result(TaskKind::Tests).is_none());
    }

    #[test]
    fn job_rows_order_running_then_queued_then_finished() {
        let mut app = test_app();
        let now = Instant::now();
        let mut older = record(1, TaskKind::Deploy, None);
        older.started_at = now.checked_sub(Duration::from_secs(5));
        let mut newer = record(2, TaskKind::Deploy, None);
        newer.started_at = Some(now);
        let mut queued = record(3, TaskKind::Retrieve, None);
        queued.queued = true;
        queued.started_at = None;
        let mut finished_older = record(4, TaskKind::Tests, Some(0));
        finished_older.ended_at = now.checked_sub(Duration::from_secs(5));
        let mut finished_newer = record(5, TaskKind::Tests, Some(1));
        finished_newer.ended_at = Some(now);
        app.output_state.records = vec![finished_older, queued, newer, finished_newer, older];

        let rows = app.job_rows();
        let ids: Vec<u64> = rows.iter().map(|row| row.record.id).collect();
        assert_eq!(ids, vec![2, 1, 3, 5, 4]);
        let sections: Vec<JobSection> = rows.iter().map(|row| row.section).collect();
        assert_eq!(
            sections,
            vec![
                JobSection::Running,
                JobSection::Running,
                JobSection::Queued,
                JobSection::Finished,
                JobSection::Finished,
            ]
        );
    }

    #[test]
    fn clearing_finished_jobs_keeps_running_and_queued() {
        let mut app = test_app();
        let mut queued = record(2, TaskKind::Retrieve, None);
        queued.queued = true;
        app.output_state.records = vec![
            record(1, TaskKind::Deploy, None),
            queued,
            record(3, TaskKind::Tests, Some(0)),
        ];
        app.clear_finished_jobs();
        let ids: Vec<u64> = app
            .output_state
            .records
            .iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn cancelling_a_queued_job_marks_it_stopped() {
        let mut app = test_app();
        let mut queued = record(7, TaskKind::Retrieve, None);
        queued.queued = true;
        queued.started_at = None;
        app.output_state.records.push(queued);
        app.pending_jobs.push_back(PendingJob {
            id: 7,
            executable: PathBuf::from("sf"),
            argv: vec!["project".to_owned(), "retrieve".to_owned()],
            purpose: JobPurpose::Transfer {
                options: TransferOptions::default(),
            },
        });
        assert!(app.cancel_job(7));
        let record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == 7)
            .expect("record");
        assert!(record.finished);
        assert!(!record.queued);
        assert!(record.ended_at.is_some());
        assert!(app.pending_jobs.is_empty());
        let rows = app.job_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].section, JobSection::Finished);
    }

    #[test]
    fn jobs_detail_scroll_moves_after_end_and_resets_on_selection() {
        let mut app = test_app();
        let mut finished = record(1, TaskKind::Deploy, Some(0));
        finished.lines = (0..40)
            .map(|index| JobLine {
                job_id: 1,
                stream: crate::sf::runner::OutputStream::Stdout,
                text: format!("line {index}"),
            })
            .collect();
        app.output_state.records = vec![finished, record(2, TaskKind::Deploy, Some(0))];
        app.open_jobs();
        // The newer empty record is selected first; move to the one with output.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::End));
        let end_scroll = match app.modal {
            Some(Modal::Jobs { scroll, .. }) => scroll,
            other => panic!("expected jobs modal, got {other:?}"),
        };
        assert!(end_scroll > 0);
        app.handle_key(key(KeyCode::Char('k')));
        match app.modal {
            Some(Modal::Jobs { scroll, focus, .. }) => {
                assert_eq!(scroll + 1, end_scroll);
                assert_eq!(focus, JobsFocus::Detail);
            }
            other => panic!("expected jobs modal, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Up));
        match app.modal {
            Some(Modal::Jobs {
                scroll,
                follow,
                focus,
                ..
            }) => {
                assert_eq!(scroll, 0);
                assert!(!follow);
                assert_eq!(focus, JobsFocus::Detail);
            }
            other => panic!("expected jobs modal, got {other:?}"),
        }
    }

    #[test]
    fn f2_opens_the_jobs_window_from_every_screen() {
        let mut app = test_app();
        for tab in [
            Tab::Home,
            Tab::Retrieve,
            Tab::Deploy,
            Tab::Tests,
            Tab::Orgs,
            Tab::Setup,
        ] {
            app.tab = tab;
            app.modal = None;
            app.handle_key(key(KeyCode::F(2)));
            assert!(
                matches!(app.modal, Some(Modal::Jobs { .. })),
                "F2 did not open the Jobs window on {tab:?}"
            );
        }
    }

    #[test]
    fn jobs_window_selects_the_newest_running_job() {
        let mut app = test_app();
        let now = Instant::now();
        let mut older = record(1, TaskKind::Deploy, None);
        older.started_at = now.checked_sub(Duration::from_secs(5));
        let mut newer = record(2, TaskKind::Deploy, None);
        newer.started_at = Some(now);
        app.output_state.records = vec![record(3, TaskKind::Tests, Some(0)), older, newer];
        app.open_jobs();
        match app.modal {
            Some(Modal::Jobs {
                selected, focus, ..
            }) => {
                assert_eq!(selected, 0);
                assert_eq!(focus, JobsFocus::List);
            }
            other => panic!("expected jobs modal, got {other:?}"),
        }
    }

    #[test]
    fn jobs_window_opens_with_an_empty_state_and_stays_open() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::F(2)));
        app.handle_key(key(KeyCode::Down));
        assert!(matches!(app.modal, Some(Modal::Jobs { .. })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.modal.is_none());
    }

    #[test]
    fn f4_opens_the_org_switcher_at_the_default_org() {
        let mut app = test_app();
        app.startup.installation = Some(crate::sf::detect::SfInstallation {
            executable: std::path::PathBuf::from("sf"),
            source: "test".to_owned(),
        });
        app.org_state.orgs = vec![
            crate::sf::org::OrgSummary {
                username: "a@example.invalid".to_owned(),
                aliases: vec!["a".to_owned()],
                org_id: String::new(),
                is_default: false,
                is_devhub: false,
                connected_status: "Connected".to_owned(),
                instance_url: String::new(),
                scratch: true,
            },
            crate::sf::org::OrgSummary {
                username: "b@example.invalid".to_owned(),
                aliases: vec!["b".to_owned()],
                org_id: String::new(),
                is_default: true,
                is_devhub: false,
                connected_status: "Connected".to_owned(),
                instance_url: String::new(),
                scratch: true,
            },
        ];
        app.handle_key(key(KeyCode::F(4)));
        match app.modal {
            Some(Modal::SwitchOrg { selected }) => assert_eq!(selected, 1),
            ref other => panic!("expected the org switcher, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Down));
        assert!(matches!(app.modal, Some(Modal::SwitchOrg { selected: 1 })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.modal.is_none());
    }

    #[test]
    fn validation_errors_keep_the_prompt_open() {
        let mut app = test_app();
        app.open_prompt(
            "Create metadata",
            PromptKind::CreateMetadata,
            vec![
                PromptField::new("Template", "ApexClass"),
                PromptField::new("Name", ""),
            ],
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.modal, Some(Modal::Prompt { .. })));
        assert!(app.status.contains("component name"));
    }

    #[test]
    fn prompt_text_editing_handles_multibyte_characters() {
        let mut app = test_app();
        app.open_prompt(
            "Target org",
            PromptKind::TargetOrg,
            vec![PromptField::new("Alias", "")],
        );
        app.handle_key(key(KeyCode::Char('é')));
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Backspace));
        match app.modal.as_ref() {
            Some(Modal::Prompt { fields, .. }) => {
                assert_eq!(fields[0].value, "é");
                assert_eq!(fields[0].cursor, 0);
            }
            other => panic!("expected prompt, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Right));
        app.handle_key(key(KeyCode::Backspace));
        match app.modal.as_ref() {
            Some(Modal::Prompt { fields, .. }) => {
                assert_eq!(fields[0].value, "");
                assert_eq!(fields[0].cursor, 0);
            }
            other => panic!("expected prompt, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Char('x')));
        match app.modal.as_ref() {
            Some(Modal::Prompt { fields, .. }) => {
                assert_eq!(fields[0].value, "x");
            }
            other => panic!("expected prompt, got {other:?}"),
        }
    }

    #[test]
    fn editing_keeps_selection_and_clears_the_loading_state() {
        let mut state = TransferSearchState::new(false);
        state.selected.push(TransferSelection {
            item: SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            checked: true,
        });
        state.searching = true;
        state.query = "Demo".to_owned();
        state.mark_edited();
        assert_eq!(state.checked().len(), 1);
        assert!(!state.searching);
        assert!(state.results.is_empty());
    }

    #[test]
    fn trailing_whitespace_does_not_force_another_search() {
        let mut state = TransferSearchState::new(false);
        state.query = "Demo ".to_owned();
        state.searched_query = Some("Demo".to_owned());
        assert!(!state.needs_search());
    }

    #[test]
    fn org_cycling_wraps_through_default_and_known_orgs() {
        let orgs = vec![
            OrgSummary {
                username: "a@example.invalid".to_owned(),
                aliases: vec!["a".to_owned()],
                org_id: String::new(),
                is_default: false,
                is_devhub: false,
                connected_status: "Connected".to_owned(),
                instance_url: String::new(),
                scratch: false,
            },
            OrgSummary {
                username: "b@example.invalid".to_owned(),
                aliases: vec!["b".to_owned()],
                org_id: String::new(),
                is_default: false,
                is_devhub: false,
                connected_status: "Connected".to_owned(),
                instance_url: String::new(),
                scratch: false,
            },
        ];
        assert_eq!(cycle_org(None, &orgs).as_deref(), Some("a"));
        assert_eq!(cycle_org(Some("a".to_owned()), &orgs).as_deref(), Some("b"));
        assert_eq!(cycle_org(Some("b".to_owned()), &orgs).as_deref(), Some(""));
    }

    #[test]
    fn run_command_strips_sf_and_keeps_quoted_arguments() {
        assert_eq!(
            parse_run_command("sf org list --json"),
            Some(vec![
                "org".to_owned(),
                "list".to_owned(),
                "--json".to_owned()
            ])
        );
        assert_eq!(
            parse_run_command("sfdx data query --query \"SELECT Id FROM Account\""),
            Some(vec![
                "data".to_owned(),
                "query".to_owned(),
                "--query".to_owned(),
                "SELECT Id FROM Account".to_owned()
            ])
        );
        assert_eq!(parse_run_command("sf"), None);
        assert_eq!(parse_run_command("sf --json"), None);
        assert_eq!(parse_run_command("   "), None);
    }

    #[test]
    fn run_command_prompt_confirms_the_exact_command_and_redacts_secrets() {
        let mut app = test_app();
        assert!(app.submit_prompt(
            &PromptKind::RunCommand,
            &[PromptField::new("Command", "sf org list --json")]
        ));
        match app.modal.take() {
            Some(Modal::Confirm {
                message,
                action: PendingAction::RunCommand { argv, command },
                ..
            }) => {
                assert!(message.contains("sf org list --json"));
                assert_eq!(
                    argv,
                    vec!["org".to_owned(), "list".to_owned(), "--json".to_owned()]
                );
                assert_eq!(command, "sf org list --json");
            }
            other => panic!("expected run confirmation, got {other:?}"),
        }
        assert!(app.submit_prompt(
            &PromptKind::RunCommand,
            &[PromptField::new(
                "Command",
                "sf org login web --client-secret hunter2"
            )]
        ));
        match app.modal.take() {
            Some(Modal::Confirm { message, .. }) => {
                assert!(message.contains("[redacted]"));
                assert!(!message.contains("hunter2"));
                assert!(message.contains("output is not retained"));
            }
            other => panic!("expected run confirmation, got {other:?}"),
        }
        assert!(!app.submit_prompt(
            &PromptKind::RunCommand,
            &[PromptField::new("Command", "  ")]
        ));
    }

    #[test]
    fn apex_report_decision_polls_only_non_terminal_runs_with_an_id() {
        let processing = test_results::parse(
            &serde_json::json!({
                "result": {"testRunId": "08N000000000001", "summary": {"outcome": "Processing"}}
            }),
            "",
        );
        assert_eq!(
            apex_report_action(&processing),
            ApexReportAction::Poll {
                run_id: "08N000000000001".to_owned()
            }
        );
        let passed = test_results::parse(
            &serde_json::json!({"result": {"summary": {"outcome": "Passed"}}}),
            "",
        );
        assert_eq!(apex_report_action(&passed), ApexReportAction::Final);
        let no_id = test_results::parse(
            &serde_json::json!({"result": {"summary": {"outcome": "Processing"}}}),
            "",
        );
        assert_eq!(apex_report_action(&no_id), ApexReportAction::Final);
    }

    #[test]
    fn pending_report_records_stay_in_the_running_section() {
        let mut app = test_app();
        let mut pending = record(1, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        app.output_state.records.push(pending);
        let rows = app.job_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].section, JobSection::Running);
        assert!(rows[0].record.is_running());
    }

    #[test]
    fn cancelling_a_pending_report_removes_the_poll_entry() {
        let mut app = test_app();
        let mut pending = record(4, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        app.output_state.records.push(pending);
        app.test_reports.insert(
            4,
            TestReport {
                run_id: "08N000000000001".to_owned(),
                org: Some("test-scratch".to_owned()),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                coverage_polls: 0,
            },
        );
        assert!(app.cancel_job(4));
        assert!(!app.test_reports.contains_key(&4));
        let record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == 4)
            .expect("record");
        assert!(!record.pending_report);
        assert!(record.finished);
        assert_eq!(record.exit_code, None);
        assert_eq!(app.job_rows()[0].section, JobSection::Finished);
    }

    #[test]
    fn tests_tab_switches_to_coverage_only_when_a_table_exists() {
        let mut app = test_app();
        app.tab = Tab::Tests;
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.tests_focus, TestsFocus::Search);

        let mut finished = record(1, TaskKind::Tests, Some(0));
        finished.result = Some(ResultView {
            kind: ResultKind::ApexTests,
            title: "Apex tests".to_owned(),
            status: "Passed".to_owned(),
            ok: true,
            summary: "2 run · 2 passed · 0 failed".to_owned(),
            items: Vec::new(),
            coverage: (0..8)
                .map(|index| ResultItem {
                    ok: true,
                    label: format!("Demo{index}"),
                    detail: format!("{}%", 30 + index * 5),
                    progress: Some((index + 1, 10)),
                })
                .collect(),
            sections: Vec::new(),
        });
        app.output_state.records.push(finished);
        app.tests_coverage_rows.set(2);

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.tests_focus, TestsFocus::Coverage);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.coverage_scroll, 1);
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.coverage_scroll, 6);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.tests_focus, TestsFocus::Search);
    }

    #[test]
    fn menu_lists_task_screens_in_order() {
        let labels: Vec<&str> = MENU_ITEMS.iter().map(|item| item.label).collect();
        assert_eq!(
            labels,
            vec![
                "Retrieve Metadata",
                "Deploy Metadata",
                "Create Metadata",
                "Apex Tests",
                "Organization",
                "Run command",
            ]
        );
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.tab, Tab::Retrieve);
        assert!(app.transfer_search.retrieve);
        app.go_home();
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.tab, Tab::Deploy);
        app.go_home();
        app.handle_key(key(KeyCode::Char('o')));
        assert_eq!(app.tab, Tab::Orgs);
    }

    #[test]
    fn suggestion_keys_open_confirmation_and_consume_arrows() {
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('d')));
        app.transfer_search.results = vec![
            SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            SuggestionItem::new("Flow:Other", "force-app/flows/Other.flow-meta.xml"),
        ];
        app.transfer_search.searched_query = Some(app.transfer_search.query.clone());
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { entries, focus, .. }) => {
                assert_eq!(*focus, 0);
                assert_eq!(entries.len(), 1);
                assert!(entries[0].checked);
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.tab, Tab::Deploy);
    }

    #[test]
    fn digits_are_typed_into_the_component_search() {
        let mut app = test_app();
        app.tab = Tab::Retrieve;
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Retrieve);
        assert_eq!(app.transfer_search.query, "3");
    }

    #[test]
    fn test_run_uses_search_checkboxes_and_confirmation() {
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.tab, Tab::Tests);
        app.test_search.results = vec![
            SuggestionItem::new("ApexClass:DemoTest", "force-app/classes/DemoTest.cls"),
            SuggestionItem::new("ApexClass:Helper", "force-app/classes/Helper.cls"),
        ];
        app.test_search.searched_query = Some(app.test_search.query.clone());
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::TestConfirm { entries, .. }) => {
                assert_eq!(entries.len(), 1);
                assert!(entries[0].checked);
            }
            other => panic!("expected test confirmation, got {other:?}"),
        }
    }

    #[test]
    fn component_search_waits_for_enter() {
        let mut app = test_app();
        app.tab = Tab::Retrieve;
        // No runtime yet, so the index is built inline instead of on a background task.
        app.ensure_suggestion_sets();
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let _guard = runtime.enter();
        app.handle_key(key(KeyCode::Char('A')));
        assert!(app.transfer_search.needs_search());
        assert!(app.transfer_search.results.is_empty());
        app.handle_key(key(KeyCode::Enter));
        assert!(app.transfer_search.searching || !app.transfer_search.needs_search());
    }

    #[test]
    fn home_key_and_esc_navigate_between_screens() {
        let mut app = test_app();
        app.tab = Tab::Orgs;
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.tab, Tab::Orgs);
    }

    #[test]
    fn quick_transfer_builds_a_confirmation_without_the_picker() {
        let mut app = test_app();
        app.submit_prompt(
            &PromptKind::QuickRetrieve,
            &[
                PromptField::new("Component", "ApexClass:Demo"),
                PromptField::new("Org", String::new()),
            ],
        );
        match app.modal.as_ref() {
            Some(Modal::Confirm { title, message, .. }) => {
                assert_eq!(title, "Confirm retrieve");
                assert!(
                    message.contains("sf project retrieve start --metadata ApexClass:Demo --json")
                );
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
        assert_eq!(app.tab, Tab::Home);
    }

    #[test]
    fn space_toggles_multiple_components_before_confirming() {
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('d')));
        app.transfer_search.results = vec![
            SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            SuggestionItem::new("Flow:Other", "force-app/flows/Other.flow-meta.xml"),
        ];
        app.transfer_search.searched_query = Some(app.transfer_search.query.clone());
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { entries, .. }) => {
                assert_eq!(entries.iter().filter(|entry| entry.checked).count(), 2);
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
    }

    #[test]
    fn suggestions_wait_for_the_debounce_before_recomputing() {
        let mut field = PromptField::new("Component", "Demo").with_suggestions(
            crate::sf::metadata::SuggestionSet::new(
                crate::sf::metadata::SuggestionKind::Transfer,
                vec!["ApexClass:Demo".to_owned()],
            )
            .into(),
        );
        field.value.push('X');
        field.mark_edited();
        assert!(!field.debounce_ready());
        std::thread::sleep(Duration::from_millis(100));
        assert!(field.debounce_ready());
        field.ensure_suggestions();
        assert!(field.cached_suggestions().is_empty());
    }

    #[test]
    fn prompt_suggestions_are_cached_until_text_changes() {
        let mut field = PromptField::new("Component", "Demo").with_suggestions(
            crate::sf::metadata::SuggestionSet::new(
                crate::sf::metadata::SuggestionKind::Transfer,
                vec!["ApexClass:Demo".to_owned(), "Flow:Other".to_owned()],
            )
            .into(),
        );
        assert_eq!(field.cached_suggestions().len(), 1);
        field.value = "Other".to_owned();
        field.mark_edited();
        field.ensure_suggestions();
        assert_eq!(field.cached_suggestions()[0].value, "Flow:Other");
    }

    #[test]
    fn component_text_without_slash_becomes_metadata_name() {
        let mut app = test_app();
        app.submit_prompt(
            &PromptKind::QuickDeploy,
            &[
                PromptField::new("Component", "ApexClass:Demo"),
                PromptField::new("Target org", String::new()),
            ],
        );
        match app.modal.as_ref() {
            Some(Modal::Confirm { message, .. }) => {
                assert!(message.contains("--metadata ApexClass:Demo"));
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
    }

    #[test]
    fn create_metadata_builds_the_generate_command() {
        let _guard = tokio::runtime::Runtime::new()
            .expect("test runtime")
            .enter();
        let mut app = test_app();
        app.startup.installation = Some(crate::sf::detect::SfInstallation {
            executable: std::path::PathBuf::from("sf"),
            source: "test".to_owned(),
        });
        app.submit_prompt(
            &PromptKind::CreateMetadata,
            &[
                PromptField::new("Template", "ApexClass"),
                PromptField::new("Name", "DemoController"),
                PromptField::new("Org", String::new()),
            ],
        );
        let record = app.output_state.records.last().expect("record");
        assert!(
            record
                .command
                .contains("template generate apex class --name DemoController")
        );
    }

    #[test]
    fn split_names_handles_commas_and_spaces() {
        assert_eq!(
            split_names("force-app, \"my package/src\",other"),
            vec!["force-app", "my package/src", "other"]
        );
    }

    #[test]
    fn help_modal_closes_on_any_key() {
        let mut app = test_app();
        app.modal = Some(Modal::Help { scroll: 0 });
        app.handle_key(key(KeyCode::Enter));
        assert!(app.modal.is_none());
    }
}
