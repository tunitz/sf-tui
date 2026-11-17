//! Application state, key dispatch and ownership of background SF jobs.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::clipboard;
use crate::config::Config;
use crate::sf::catalog::{self, Catalog, CatalogSource, CommandSpec, Flag};
use crate::sf::deploy::{PayloadMode, TransferOptions};
use crate::sf::metadata::{SuggestionItem, SuggestionSet};
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::project::{self, SfProject};
use crate::sf::runner::{JobLine, ProcessOutcome, RunningJob, SfEvent};
use crate::sf::test_results::{self, TestRun};
use crate::sf::{self, Startup};
use crate::view::{apex_result, generic_result, transfer_result};

/// Screens shown in the tab bar. Setup is added only when no CLI was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Retrieve,
    Tests,
    Deploy,
    Catalog,
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
            Self::Catalog => "Catalog",
            Self::Orgs => "Orgs",
            Self::Setup => "Setup",
        }
    }
}

/// What a background job was started for; drives the follow-up once it finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobPurpose {
    CatalogRefresh,
    Command {
        id: String,
        command: String,
    },
    Help {
        id: String,
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
    },
}

impl JobPurpose {
    pub fn label(&self) -> &'static str {
        match self {
            Self::CatalogRefresh => "catalog",
            Self::Command { .. } => "command",
            Self::Help { .. } => "help",
            Self::OrgList => "orgs",
            Self::OrgDetail { .. } => "org detail",
            Self::OrgAction { .. } => "org action",
            Self::Transfer { .. } => "transfer",
            Self::Report { .. } => "report",
            Self::Installer { .. } => "installer",
            Self::ApexTests { .. } => "apex tests",
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

/// One row of the fuzzy-searchable catalog tree.
#[derive(Debug, Clone)]
pub enum CatalogRow {
    Header(String),
    Command {
        id: String,
        summary: String,
        matched: Option<Vec<usize>>,
        favorite: bool,
    },
}

const POPULAR_COMMAND_IDS: &[&str] = &[
    "org login web",
    "project retrieve start",
    "project deploy start",
    "apex run test",
    "logic get test",
];

/// A destination or action on the main menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Retrieve,
    Deploy,
    Create,
    Tests,
    Organizations,
    Catalog,
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
        key: 'l',
        label: "Command Catalog",
        action: MenuAction::Catalog,
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
pub struct CatalogState {
    pub query: String,
    pub active_query: String,
    pub search_focused: bool,
    pub selected_id: Option<String>,
    pub help_for: Option<String>,
    pub help_flags: Vec<Flag>,
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

/// The task a recorded job belongs to, used by per-screen history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Retrieve,
    Deploy,
    Tests,
    Catalog,
    Organization,
    Setup,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Retrieve => "Retrieve",
            Self::Deploy => "Deploy",
            Self::Tests => "Apex tests",
            Self::Catalog => "Command catalog",
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
    pub json: Option<Value>,
    pub result: Option<ResultView>,
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

    /// The command with its executable path shortened, for history rows and result headers.
    pub fn display_command(&self) -> String {
        let prefix = self.executable.display().to_string();
        self.command
            .strip_prefix(&prefix)
            .map(|rest| format!("sf{rest}"))
            .unwrap_or_else(|| self.command.clone())
    }
}

const MAX_RECORDS: usize = 40;
const MAX_LINES_PER_RECORD: usize = 3_000;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;

type CatalogRowsCache = RefCell<Option<(u64, String, Rc<Vec<CatalogRow>>)>>;

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
            json: None,
            result: None,
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
    RunFlags { id: String },
    QuickRetrieve,
    QuickDeploy,
    CreateMetadata,
    EditPreset { id: String },
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
    TestHistory {
        runs: Vec<TestRun>,
        selected: usize,
    },
    TaskHistory {
        kind: TaskKind,
        selected: usize,
    },
    JobResult {
        kind: TaskKind,
        record_id: u64,
        scroll: usize,
    },
    TestRun {
        run: TestRun,
        scroll: usize,
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
    pub catalog_state: CatalogState,
    pub org_state: OrgState,
    pub deploy_state: DeployState,
    pub transfer_search: TransferSearchState,
    pub test_search: TransferSearchState,
    pub output_state: OutputState,
    pub test_runs: Vec<TestRun>,
    pub modal: Option<Modal>,
    pub setup_selected: usize,
    tab_history: Vec<Tab>,
    suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    test_suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    suggestions_building: bool,
    catalog_revision: u64,
    catalog_rows_cache: CatalogRowsCache,
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
    next_job_id: u64,
}

impl App {
    pub fn new(startup: Startup, config: Config, config_dir: PathBuf) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let selected_id = startup
            .catalog
            .commands
            .first()
            .map(|command| command.id.clone());
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
            catalog_state: CatalogState {
                selected_id,
                ..CatalogState::default()
            },
            org_state: OrgState::default(),
            deploy_state: DeployState::default(),
            transfer_search: TransferSearchState::new(false),
            test_search: TransferSearchState::new(false),
            output_state: OutputState::default(),
            test_runs: Vec::new(),
            modal: None,
            setup_selected: 0,
            tab_history: Vec::new(),
            suggestion_cache: None,
            test_suggestion_cache: None,
            suggestions_building: false,
            catalog_revision: 0,
            catalog_rows_cache: RefCell::new(None),
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
            next_job_id: 1,
        };
        app.start_project_scan();
        app.test_runs = test_results::load(&app.config_dir);
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
        let mut screens = vec![
            Tab::Home,
            Tab::Retrieve,
            Tab::Deploy,
            Tab::Tests,
            Tab::Catalog,
            Tab::Orgs,
        ];
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
                self.startup.installation = Some(installation);
                if self.org_state.orgs.is_empty() && !self.org_state.loading {
                    self.refresh_orgs();
                }
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
            AppEvent::Startup(sf::StartupEvent::Catalog(catalog)) => {
                self.startup.catalog = catalog;
                self.catalog_revision = self.catalog_revision.wrapping_add(1);
                if let Some(id) = self.catalog_state.selected_id.clone()
                    && self.startup.catalog.command(&id).is_none()
                {
                    self.catalog_state.selected_id = self
                        .startup
                        .catalog
                        .commands
                        .first()
                        .map(|command| command.id.clone());
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
            // The job was cancelled and removed from the map. Record the outcome on its output
            // row but do not invent an empty command or overwrite the cancellation notice.
            if let Some(record) = self.output_state.find_mut(job_id) {
                record.exit_code = outcome.exit_code;
                record.finished = true;
                record.queued = false;
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
            record.json = if outcome.stdout.len() <= MAX_JSON_BYTES {
                parsed.clone()
            } else {
                None
            };
        }
        let succeeded = outcome.succeeded();
        let secret_output = match &purpose {
            JobPurpose::Command { command, .. } | JobPurpose::ApexTests { command } => {
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
            (JobPurpose::Help { id }, _) => {
                let text = format!("{}{}", outcome.stdout, outcome.stderr);
                self.catalog_state.help_flags = catalog::parse_help_flags(&text);
                self.catalog_state.help_for = Some(id);
            }
            (JobPurpose::CatalogRefresh, Some(value)) => {
                match catalog::parse_commands_json(&value.to_string()) {
                    Ok(commands) => {
                        let _ = catalog::write_cache(&self.config_dir, &value.to_string());
                        self.startup.catalog = Catalog {
                            commands,
                            source: CatalogSource::Live,
                        };
                        self.catalog_revision = self.catalog_revision.wrapping_add(1);
                        self.notice("Command catalog refreshed");
                    }
                    Err(error) => self.error(format!("Catalog refresh failed: {error}")),
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
                        self.notice("Retrieve finished — F2 opens the result");
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
                        self.notice(format!("Deploy {id} started — F2 shows progress"));
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
                        self.notice("Deploy finished — F2 opens the result");
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
            (JobPurpose::ApexTests { command }, Some(value)) => {
                let run = test_results::parse(&value, &command);
                result_view = Some(apex_result(&run));
                self.test_runs = test_results::record(&self.config_dir, run.clone());
                self.output_state.show_details = false;
                if succeeded {
                    self.notice("Apex test results saved");
                }
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
            self.notice("Done — F2 history · Esc back");
        }
        self.start_pending_jobs();
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
                self.error("Deploy report timed out; F2 keeps the last result");
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
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.config.save().map_err(anyhow::Error::from)
    }

    fn persist_config(&mut self) {
        if let Err(error) = self.config.save() {
            self.error(format!("Could not save configuration: {error}"));
        }
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
                    | TaskKind::Catalog
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
                }
                if matches!(purpose, JobPurpose::OrgList) {
                    self.org_state.loading = false;
                }
                if let JobPurpose::Report { transfer_job, .. } = purpose {
                    self.deploy_reports.remove(&transfer_job);
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
            }
            self.notice(format!("Cancelled queued job #{id}"));
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
        !self.jobs.is_empty() || !self.pending_jobs.is_empty()
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len() + self.pending_jobs.len()
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
                    if self.catalog_state.search_focused {
                        self.catalog_state.query.clear();
                        self.catalog_state.active_query.clear();
                    } else if matches!(self.tab, Tab::Retrieve | Tab::Deploy) {
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
            if let Some(kind) = Self::task_for_tab(self.tab) {
                self.open_task_history(kind);
            } else {
                self.notice("F2 opens the history of the screen you are working in");
            }
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
        if self.catalog_state.search_focused {
            self.catalog_key(key);
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
            KeyCode::Char('h') if !self.catalog_state.search_focused => self.go_home(),
            KeyCode::Char('y') => self.copy_context(),
            _ => match self.tab {
                Tab::Home => self.home_key(key),
                Tab::Retrieve | Tab::Deploy => self.transfer_key(key),
                Tab::Tests => self.test_key(key),
                Tab::Catalog => self.catalog_key(key),
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
        self.notice(format!("{} — {}", self.tab.label(), self.tab_action_hint()));
    }

    fn go_home(&mut self) {
        self.set_tab(Tab::Home);
    }

    fn go_back(&mut self) {
        let previous = self.tab_history.pop().unwrap_or(Tab::Home);
        self.tab = previous;
        self.notice(format!(
            "Back to {} — {}",
            self.tab.label(),
            self.tab_action_hint()
        ));
    }

    fn tab_action_hint(&self) -> &'static str {
        match self.tab {
            Tab::Home => "↑↓ move · Enter open · F2 history",
            Tab::Retrieve => "type component · Enter search · Space select · Enter confirm",
            Tab::Deploy => "type component · Enter search · Space select · Enter confirm",
            Tab::Tests => "type test class · Enter search · Space select · Enter confirm",
            Tab::Catalog => "/ search · Enter help · r run",
            Tab::Orgs => "Enter details · o open · l login",
            Tab::Setup => "Enter installer · r re-probe",
        }
    }

    fn refresh_current_tab(&mut self) {
        match self.tab {
            Tab::Orgs => self.refresh_orgs(),
            Tab::Retrieve | Tab::Deploy => {
                self.transfer_search.mark_edited();
                self.notice("Component search refreshed");
            }
            Tab::Catalog => {
                let argv = vec!["commands".to_owned(), "--json".to_owned()];
                self.spawn(argv, JobPurpose::CatalogRefresh);
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
            Tab::Catalog => self.selected_catalog_id().map(|id| format!("sf {id}")),
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
            MenuAction::Catalog => self.set_tab(Tab::Catalog),
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

    // ----- Catalog -----------------------------------------------------------------

    pub fn catalog_rows(&self) -> Rc<Vec<CatalogRow>> {
        let query = self.catalog_state.active_query.trim().to_owned();
        if let Some((revision, cached_query, rows)) = self.catalog_rows_cache.borrow().as_ref()
            && *revision == self.catalog_revision
            && cached_query == &query
        {
            return Rc::clone(rows);
        }
        let rows = Rc::new(self.build_catalog_rows(&query));
        *self.catalog_rows_cache.borrow_mut() =
            Some((self.catalog_revision, query, Rc::clone(&rows)));
        rows
    }

    fn build_catalog_rows(&self, query: &str) -> Vec<CatalogRow> {
        let catalog = &self.startup.catalog;
        if query.is_empty() {
            let mut rows = vec![CatalogRow::Header("Popular commands".to_owned())];
            for id in POPULAR_COMMAND_IDS {
                if let Some(command) = catalog.command(id) {
                    rows.push(command_row(
                        command,
                        None,
                        self.config.is_favorite(&command.id),
                    ));
                }
            }
            let mut categories = catalog.categories();
            categories.sort();
            for category in categories {
                rows.push(CatalogRow::Header(category.clone()));
                for command in catalog
                    .commands
                    .iter()
                    .filter(|command| command.category == category)
                    .filter(|command| !POPULAR_COMMAND_IDS.contains(&command.id.as_str()))
                {
                    rows.push(command_row(
                        command,
                        None,
                        self.config.is_favorite(&command.id),
                    ));
                }
            }
            rows
        } else {
            let mut rows = Vec::new();
            let mut last_category = String::new();
            for hit in catalog::search(&catalog.commands, query, 400) {
                if hit.command.category != last_category {
                    rows.push(CatalogRow::Header(hit.command.category.clone()));
                    last_category = hit.command.category.clone();
                }
                rows.push(command_row(
                    hit.command,
                    hit.matched,
                    self.config.is_favorite(&hit.command.id),
                ));
            }
            rows
        }
    }

    pub fn selected_catalog_id(&self) -> Option<String> {
        let rows = self.catalog_rows();
        if let Some(id) = &self.catalog_state.selected_id
            && rows
                .iter()
                .any(|row| matches!(row, CatalogRow::Command { id: row_id, .. } if row_id == id))
        {
            return Some(id.clone());
        }
        rows.iter().find_map(|row| match row {
            CatalogRow::Command { id, .. } => Some(id.clone()),
            CatalogRow::Header(_) => None,
        })
    }

    pub fn selected_command(&self) -> Option<&CommandSpec> {
        self.selected_catalog_id()
            .and_then(|id| self.startup.catalog.command(&id))
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

    pub fn task_records(&self, kind: TaskKind) -> Vec<&OutputRecord> {
        self.output_state
            .records
            .iter()
            .rev()
            .filter(|record| record.task == kind)
            .collect()
    }

    fn task_history_len(&self, kind: TaskKind) -> usize {
        self.task_records(kind).len()
            + if kind == TaskKind::Tests {
                self.test_runs.len()
            } else {
                0
            }
    }

    fn open_task_history(&mut self, kind: TaskKind) {
        if self.task_history_len(kind) == 0 {
            self.notice(format!("No {} history yet", kind.label()));
            return;
        }
        self.modal = Some(Modal::TaskHistory { kind, selected: 0 });
    }

    fn task_for_tab(tab: Tab) -> Option<TaskKind> {
        match tab {
            Tab::Retrieve => Some(TaskKind::Retrieve),
            Tab::Deploy => Some(TaskKind::Deploy),
            Tab::Tests => Some(TaskKind::Tests),
            Tab::Catalog => Some(TaskKind::Catalog),
            Tab::Orgs => Some(TaskKind::Organization),
            Tab::Setup => Some(TaskKind::Setup),
            Tab::Home => None,
        }
    }

    fn catalog_key(&mut self, key: KeyEvent) {
        if self.catalog_state.search_focused {
            match key.code {
                KeyCode::Esc => {
                    if self.catalog_state.query.is_empty() {
                        self.catalog_state.search_focused = false;
                    } else {
                        self.catalog_state.query.clear();
                        self.catalog_state.active_query.clear();
                    }
                }
                KeyCode::Backspace => {
                    self.catalog_state.query.pop();
                }
                KeyCode::Enter => {
                    self.catalog_state.search_focused = false;
                    self.catalog_state.active_query = self.catalog_state.query.clone();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.catalog_state.query.push(character);
                }
                KeyCode::Up | KeyCode::Down => {
                    self.move_catalog(if key.code == KeyCode::Up { -1 } else { 1 })
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('/') => self.catalog_state.search_focused = true,
            KeyCode::Up => self.move_catalog(-1),
            KeyCode::Down => self.move_catalog(1),
            KeyCode::PageUp => self.move_catalog(-10),
            KeyCode::PageDown => self.move_catalog(10),
            KeyCode::Enter => self.fetch_help(),
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_catalog_id() {
                    let preset = self.preset_flags(&id).join(" ");
                    self.open_prompt(
                        "Run command",
                        PromptKind::RunFlags { id: id.clone() },
                        vec![PromptField::new(
                            "Flags (e.g. --json --target-org dev)",
                            preset,
                        )],
                    );
                }
            }
            KeyCode::Char('f') => {
                if let Some(id) = self.selected_catalog_id() {
                    let added = self.config.toggle_favorite(&id);
                    self.catalog_revision = self.catalog_revision.wrapping_add(1);
                    self.persist_config();
                    self.notice(if added {
                        "Added to favorites"
                    } else {
                        "Removed from favorites"
                    });
                }
            }
            KeyCode::Char('e') => {
                if let Some(id) = self.selected_catalog_id() {
                    let preset = self.preset_flags(&id).join(" ");
                    self.open_prompt(
                        "Edit preset flags",
                        PromptKind::EditPreset { id: id.clone() },
                        vec![PromptField::new("Preset flags", preset)],
                    );
                }
            }
            _ => {}
        }
    }

    fn move_catalog(&mut self, delta: isize) {
        let rows = self.catalog_rows();
        let command_indexes: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                CatalogRow::Command { .. } => Some(index),
                CatalogRow::Header(_) => None,
            })
            .collect();
        if command_indexes.is_empty() {
            return;
        }
        let current = self.catalog_state.selected_id.as_ref().and_then(|id| {
            rows.iter().position(
                |row| matches!(row, CatalogRow::Command { id: row_id, .. } if row_id == id),
            )
        });
        let position = current
            .and_then(|current| command_indexes.iter().position(|index| *index == current))
            .unwrap_or(0) as isize;
        let next = (position + delta).clamp(0, command_indexes.len() as isize - 1) as usize;
        if let CatalogRow::Command { id, .. } = &rows[command_indexes[next]] {
            self.catalog_state.selected_id = Some(id.clone());
        }
    }

    fn fetch_help(&mut self) {
        let Some(id) = self.selected_catalog_id() else {
            return;
        };
        if self.catalog_state.help_for.as_deref() == Some(id.as_str()) {
            return;
        }
        let mut argv: Vec<String> = id.split_whitespace().map(str::to_owned).collect();
        argv.push("--help".to_owned());
        self.spawn(argv, JobPurpose::Help { id });
    }

    // ----- Favorites ----------------------------------------------------------------

    fn preset_flags(&self, id: &str) -> Vec<String> {
        self.config
            .favorites
            .iter()
            .find(|favorite| favorite.id == id)
            .map(|favorite| favorite.preset_flags.clone())
            .unwrap_or_default()
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
            Modal::TaskHistory { kind, mut selected } => {
                let len = self.task_history_len(kind);
                if len == 0 {
                    self.modal = None;
                    return;
                }
                selected = selected.min(len - 1);
                match key.code {
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.modal = Some(Modal::TaskHistory { kind, selected });
                    }
                    KeyCode::Down => {
                        selected = (selected + 1).min(len - 1);
                        self.modal = Some(Modal::TaskHistory { kind, selected });
                    }
                    KeyCode::PageUp => {
                        selected = selected.saturating_sub(5);
                        self.modal = Some(Modal::TaskHistory { kind, selected });
                    }
                    KeyCode::PageDown => {
                        selected = (selected + 5).min(len - 1);
                        self.modal = Some(Modal::TaskHistory { kind, selected });
                    }
                    KeyCode::Esc => self.modal = None,
                    KeyCode::Char('x') => {
                        let records = self.task_records(kind);
                        if let Some(record) = records.get(selected) {
                            let id = record.id;
                            let open = !record.finished || record.queued;
                            drop(records);
                            if open {
                                self.cancel_job(id);
                            } else {
                                self.notice("That job already finished");
                            }
                        }
                    }
                    KeyCode::Enter => {
                        let records = self.task_records(kind);
                        if let Some(record) = records.get(selected) {
                            let record_id = record.id;
                            drop(records);
                            self.modal = Some(Modal::JobResult {
                                kind,
                                record_id,
                                scroll: 0,
                            });
                        } else if kind == TaskKind::Tests {
                            let index = selected.saturating_sub(records.len());
                            drop(records);
                            if let Some(run) = self.test_runs.get(index) {
                                self.modal = Some(Modal::TestRun {
                                    run: run.clone(),
                                    scroll: 0,
                                });
                            }
                        }
                    }
                    _ => self.modal = Some(Modal::TaskHistory { kind, selected }),
                }
            }
            Modal::JobResult {
                kind,
                record_id,
                mut scroll,
            } => match key.code {
                KeyCode::Esc => {
                    self.modal = Some(Modal::TaskHistory { kind, selected: 0 });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll,
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll,
                    });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll,
                    });
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll,
                    });
                }
                KeyCode::Home => {
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll: 0,
                    });
                }
                KeyCode::End => {
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll: u16::MAX as usize,
                    });
                }
                KeyCode::Char('o') => {
                    self.output_state.show_details = !self.output_state.show_details;
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll: 0,
                    });
                }
                KeyCode::Char('z') => {
                    self.output_state.show_details = true;
                    self.output_state.show_json = !self.output_state.show_json;
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll: 0,
                    });
                }
                KeyCode::Char('v') => {
                    if self.test_runs.is_empty() {
                        self.notice("No saved test runs yet");
                    } else {
                        self.modal = Some(Modal::TestHistory {
                            runs: self.test_runs.clone(),
                            selected: 0,
                        });
                    }
                }
                KeyCode::Char('x') => {
                    if self.cancel_job(record_id) {
                        self.modal = Some(Modal::TaskHistory { kind, selected: 0 });
                    }
                }
                _ => {
                    self.modal = Some(Modal::JobResult {
                        kind,
                        record_id,
                        scroll,
                    });
                }
            },
            Modal::TestHistory { runs, mut selected } => {
                selected = selected.min(runs.len().saturating_sub(1));
                match key.code {
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        self.modal = Some(Modal::TestHistory { runs, selected });
                    }
                    KeyCode::Down => {
                        if !runs.is_empty() {
                            selected = (selected + 1).min(runs.len() - 1);
                        }
                        self.modal = Some(Modal::TestHistory { runs, selected });
                    }
                    KeyCode::PageUp => {
                        selected = selected.saturating_sub(5);
                        self.modal = Some(Modal::TestHistory { runs, selected });
                    }
                    KeyCode::PageDown => {
                        if !runs.is_empty() {
                            selected = (selected + 5).min(runs.len() - 1);
                        }
                        self.modal = Some(Modal::TestHistory { runs, selected });
                    }
                    KeyCode::Enter => {
                        if let Some(run) = runs.get(selected) {
                            self.modal = Some(Modal::TestRun {
                                run: run.clone(),
                                scroll: 0,
                            });
                        } else {
                            self.modal = None;
                        }
                    }
                    KeyCode::Esc => self.modal = None,
                    _ => self.modal = Some(Modal::TestHistory { runs, selected }),
                }
            }
            Modal::TestRun { run, mut scroll } => match key.code {
                KeyCode::Esc | KeyCode::Enter => self.modal = None,
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.modal = Some(Modal::TestRun { run, scroll });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    self.modal = Some(Modal::TestRun { run, scroll });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.modal = Some(Modal::TestRun { run, scroll });
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    self.modal = Some(Modal::TestRun { run, scroll });
                }
                KeyCode::Home => {
                    self.modal = Some(Modal::TestRun { run, scroll: 0 });
                }
                KeyCode::End => {
                    self.modal = Some(Modal::TestRun {
                        run,
                        scroll: u16::MAX as usize,
                    });
                }
                _ => self.modal = Some(Modal::TestRun { run, scroll }),
            },
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
                    self.spawn(argv, JobPurpose::ApexTests { command: display });
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

    fn submit_prompt(&mut self, kind: &PromptKind, fields: &[PromptField]) -> bool {
        let values: Vec<String> = fields
            .iter()
            .map(|field| field.value.trim().to_owned())
            .collect();
        match kind {
            PromptKind::RunFlags { id } => {
                self.run_command(id, values.first().map(String::as_str).unwrap_or(""))
            }
            PromptKind::EditPreset { id } => {
                let flags = split_args(values.first().map(String::as_str).unwrap_or(""));
                if !self.config.is_favorite(id) {
                    self.config.toggle_favorite(id);
                }
                self.config.set_preset_flags(id, flags);
                self.catalog_revision = self.catalog_revision.wrapping_add(1);
                self.persist_config();
                self.notice("Preset flags saved");
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

    fn run_command(&mut self, id: &str, flags: &str) {
        let mut argv: Vec<String> = id.split_whitespace().map(str::to_owned).collect();
        argv.extend(split_args(flags));
        let display = format!("sf {id} {flags}").trim().to_owned();
        if is_destructive_command(id) {
            self.modal = Some(Modal::Confirm {
                title: "Run this command?".to_owned(),
                message: format!("{display}\n\nEnter runs · Esc cancels"),
                action: PendingAction::RunPrepared { argv },
            });
            return;
        }
        self.spawn(
            argv,
            JobPurpose::Command {
                id: id.to_owned(),
                command: display,
            },
        );
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
Output appears in the Output tab."
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

fn command_row(command: &CommandSpec, matched: Option<Vec<usize>>, favorite: bool) -> CatalogRow {
    CatalogRow::Command {
        id: command.id.clone(),
        summary: command.summary.clone(),
        matched,
        favorite,
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

fn task_for_purpose(purpose: &JobPurpose) -> Option<TaskKind> {
    match purpose {
        JobPurpose::Transfer { options } if options.retrieve => Some(TaskKind::Retrieve),
        JobPurpose::Transfer { .. } => Some(TaskKind::Deploy),
        JobPurpose::ApexTests { .. } => Some(TaskKind::Tests),
        JobPurpose::Command { .. } => Some(TaskKind::Catalog),
        JobPurpose::Installer { .. } => Some(TaskKind::Setup),
        JobPurpose::OrgList | JobPurpose::OrgDetail { .. } | JobPurpose::OrgAction { .. } => {
            Some(TaskKind::Organization)
        }
        JobPurpose::CatalogRefresh | JobPurpose::Help { .. } | JobPurpose::Report { .. } => None,
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

/// Commands that change org, project or package state need an explicit confirmation.
fn is_destructive_command(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "org delete",
        "org logout",
        "org create",
        "data delete",
        "project deploy",
        "project retrieve",
        "package install",
        "package uninstall",
        "config set",
        "config unset",
        "alias delete",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
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
    use crate::sf::catalog::CatalogSource;

    fn test_app() -> App {
        let _guard = tokio::runtime::Runtime::new()
            .expect("test runtime")
            .enter();
        App::new(
            Startup {
                installation: None,
                version: None,
                current_org: None,
                catalog: Catalog {
                    commands: vec![
                        CommandSpec {
                            id: "org list".to_owned(),
                            summary: "List orgs".to_owned(),
                            description: String::new(),
                            category: "Orgs".to_owned(),
                            flags: Vec::new(),
                            examples: Vec::new(),
                        },
                        CommandSpec {
                            id: "project deploy start".to_owned(),
                            summary: "Deploy".to_owned(),
                            description: String::new(),
                            category: "Project".to_owned(),
                            flags: Vec::new(),
                            examples: Vec::new(),
                        },
                    ],
                    source: CatalogSource::Snapshot,
                },
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
    fn catalog_search_navigates_and_toggles_favorite() {
        let mut app = test_app();
        app.tab = Tab::Catalog;
        app.handle_key(key(KeyCode::Char('/')));
        for character in "deploy".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.selected_catalog_id().as_deref(),
            Some("project deploy start")
        );
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.config.is_favorite("project deploy start"));
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
            finished: exit_code.is_some(),
            queued: false,
            json: None,
            result: None,
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
    fn task_history_lists_only_that_tasks_records() {
        let mut app = test_app();
        for (id, task, exit_code) in [
            (1, TaskKind::Retrieve, Some(0)),
            (2, TaskKind::Deploy, Some(1)),
            (3, TaskKind::Retrieve, None),
        ] {
            app.output_state.records.push(record(id, task, exit_code));
        }
        let retrieves = app.task_records(TaskKind::Retrieve);
        assert_eq!(retrieves.len(), 2);
        assert_eq!(retrieves[0].id, 3);
        assert!(!retrieves[0].queued);
        assert_eq!(app.task_records(TaskKind::Deploy).len(), 1);
        app.open_task_history(TaskKind::Deploy);
        assert!(matches!(
            app.modal.as_ref(),
            Some(Modal::TaskHistory {
                kind: TaskKind::Deploy,
                ..
            })
        ));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            app.modal.as_ref(),
            Some(Modal::JobResult {
                kind: TaskKind::Deploy,
                record_id: 2,
                ..
            })
        ));
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
                "Command Catalog",
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
