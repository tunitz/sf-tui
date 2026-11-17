//! Application state, key dispatch and ownership of background SF jobs.

use std::collections::HashMap;
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
use crate::sf::catalog::{self, Catalog, CatalogSource, CommandSpec, Flag};
use crate::sf::deploy::{PayloadMode, TransferOptions, TransferReport};
use crate::sf::metadata::{SuggestionItem, SuggestionSet};
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::project::{self, SfProject};
use crate::sf::runner::{JobLine, ProcessOutcome, RunningJob, SfEvent};
use crate::sf::{self, Startup};

/// Screens shown in the tab bar. Setup is added only when no CLI was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Retrieve,
    Tests,
    Deploy,
    Catalog,
    Favorites,
    Orgs,
    Output,
    Setup,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Retrieve => "Retrieve",
            Self::Tests => "Tests",
            Self::Deploy => "Deploy",
            Self::Catalog => "Catalog",
            Self::Favorites => "Favorites",
            Self::Orgs => "Orgs",
            Self::Output => "Output",
            Self::Setup => "Setup",
        }
    }
}

/// What a background job was started for; drives the follow-up once it finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobPurpose {
    CatalogRefresh,
    Command { id: String, command: String },
    Help { id: String },
    OrgList,
    OrgDetail { username: String },
    OrgAction { description: String },
    Transfer { options: TransferOptions },
    Report { options: TransferOptions },
    Installer { command: String },
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
    },
    Startup(crate::sf::StartupEvent),
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

/// A task-oriented shortcut shown first on the Home screen.
#[derive(Debug, Clone, Copy)]
pub struct QuickAction {
    pub key: char,
    pub label: &'static str,
    pub command: &'static str,
    pub hint: &'static str,
}

pub const QUICK_ACTIONS: &[QuickAction] = &[
    QuickAction {
        key: 'r',
        label: "Retrieve metadata",
        command: "sf project retrieve start",
        hint: "",
    },
    QuickAction {
        key: 'd',
        label: "Deploy metadata",
        command: "sf project deploy start",
        hint: "",
    },
    QuickAction {
        key: 't',
        label: "Run Apex tests",
        command: "sf apex run test",
        hint: "",
    },
    QuickAction {
        key: 'g',
        label: "Get test results",
        command: "sf logic get test",
        hint: "test run id",
    },
    QuickAction {
        key: 'c',
        label: "Create metadata",
        command: "sf template generate",
        hint: "template",
    },
    QuickAction {
        key: 'a',
        label: "Log in to an org",
        command: "sf org login web",
        hint: "",
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
    pub query_changed_at: Option<Instant>,
    pub search_focused: bool,
    pub selected_id: Option<String>,
    pub help_for: Option<String>,
    pub help_flags: Vec<Flag>,
}

#[derive(Debug, Clone, Default)]
pub struct FavoritesState {
    pub selected: usize,
}

#[derive(Debug, Clone, Default)]
pub struct OrgState {
    pub orgs: Vec<OrgSummary>,
    pub selected: usize,
    pub detail: Option<OrgDetail>,
    pub detail_for: Option<String>,
    pub loading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickItem {
    pub path: String,
    pub label: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct DeployState {
    pub project: Option<SfProject>,
    pub project_error: Option<String>,
    pub items: Vec<PickItem>,
    pub selected: usize,
    pub options: TransferOptions,
    pub job_id: Option<String>,
    pub report: Option<TransferReport>,
    pub poll_after: Option<Instant>,
    pub manifest_members: Vec<String>,
    pub selected_component: Option<String>,
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
    pub dirty: bool,
    pub last_edit: Instant,
    pub searching: bool,
}

impl TransferSearchState {
    pub fn new(retrieve: bool) -> Self {
        Self {
            retrieve,
            query: String::new(),
            results: Vec::new(),
            cursor: 0,
            selected: Vec::new(),
            dirty: true,
            last_edit: Instant::now(),
            searching: false,
        }
    }

    pub fn mark_edited(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
        self.cursor = 0;
    }

    pub fn debounce_ready(&self) -> bool {
        self.dirty && self.last_edit.elapsed() >= Duration::from_millis(90)
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

/// A single recorded command invocation and its streamed output.
#[derive(Debug, Clone)]
pub struct OutputRecord {
    pub id: u64,
    pub command: String,
    pub purpose: String,
    pub lines: Vec<JobLine>,
    pub status: Option<ProcessOutcome>,
    pub json: Option<Value>,
}

impl OutputRecord {
    pub fn label(&self) -> String {
        let state = match &self.status {
            Some(outcome) if outcome.succeeded() => "ok".to_owned(),
            Some(outcome) => outcome
                .exit_code
                .map(|code| format!("exit {code}"))
                .unwrap_or_else(|| "stopped".to_owned()),
            None => "running".to_owned(),
        };
        format!("#{} [{}] {}", self.id, state, self.command)
    }
}

const MAX_RECORDS: usize = 40;
const MAX_LINES_PER_RECORD: usize = 3_000;

#[derive(Debug, Clone, Default)]
pub struct OutputState {
    pub records: Vec<OutputRecord>,
    pub selected: usize,
    pub scroll: usize,
    pub show_json: bool,
    pub show_details: bool,
}

impl OutputState {
    fn begin(&mut self, id: u64, command: String, purpose: &str) {
        self.records.push(OutputRecord {
            id,
            command,
            purpose: purpose.to_owned(),
            lines: Vec::new(),
            status: None,
            json: None,
        });
        if self.records.len() > MAX_RECORDS {
            self.records.remove(0);
        }
        self.selected = self.records.len().saturating_sub(1);
        self.scroll = 0;
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
    cached_query: String,
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
            cached_query: String::new(),
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
        self.suggestion_index = None;
        let Some(set) = &self.suggestion_set else {
            self.cached_suggestions.clear();
            return;
        };
        self.cached_query = self.value.clone();
        self.cached_suggestions = set.filter(&self.value, 5);
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
    RunApexTests,
    GetTestResults,
    CreateMetadata,
    MetadataNames,
    EditPreset { id: String },
    LoginWeb,
    CreateScratch,
    TargetOrg,
    Tests,
}

/// Action queued behind a confirmation or input dialog.
#[derive(Debug, Clone)]
pub enum PendingAction {
    None,
    Run { id: String, flags: String },
    SetDefault { username: String, devhub: bool },
    Logout { username: String },
    RefreshOrgs,
    RunTransfer,
    RunInstaller { command: String },
    RunPrepared { argv: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help,
    Message {
        title: String,
        message: String,
    },
    Confirm {
        title: String,
        message: String,
        action: PendingAction,
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
    pub favorites_state: FavoritesState,
    pub org_state: OrgState,
    pub deploy_state: DeployState,
    pub transfer_search: TransferSearchState,
    pub test_search: TransferSearchState,
    pub output_state: OutputState,
    pub modal: Option<Modal>,
    pub setup_selected: usize,
    tab_history: Vec<Tab>,
    suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    test_suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    ticks: usize,
    probe_task: Option<JoinHandle<()>>,
    search_task: Option<JoinHandle<()>>,
    event_tx: UnboundedSender<AppEvent>,
    event_rx: UnboundedReceiver<AppEvent>,
    jobs: HashMap<u64, JobEntry>,
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
            favorites_state: FavoritesState::default(),
            org_state: OrgState::default(),
            deploy_state: DeployState::default(),
            transfer_search: TransferSearchState::new(false),
            test_search: TransferSearchState::new(false),
            output_state: OutputState::default(),
            modal: None,
            setup_selected: 0,
            tab_history: Vec::new(),
            suggestion_cache: None,
            test_suggestion_cache: None,
            ticks: 0,
            probe_task: None,
            search_task: None,
            event_tx,
            event_rx,
            jobs: HashMap::new(),
            next_job_id: 1,
        };
        app.reload_project();
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

    pub fn tabs(&self) -> Vec<Tab> {
        let mut tabs = vec![
            Tab::Home,
            Tab::Retrieve,
            Tab::Deploy,
            Tab::Tests,
            Tab::Catalog,
            Tab::Favorites,
            Tab::Orgs,
            Tab::Output,
        ];
        if self.startup_stage.done && !self.startup.is_ready() {
            tabs.push(Tab::Setup);
        }
        tabs
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

    pub fn reload_project(&mut self) {
        let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match project::find_project(&start)
            .map(project::load_project)
            .transpose()
        {
            Ok(Some(project)) => {
                let items = build_pick_items(&project);
                self.deploy_state.project = Some(project);
                self.deploy_state.items = items;
                self.deploy_state.project_error = None;
            }
            Ok(None) => {
                self.deploy_state.project = None;
                self.deploy_state.items.clear();
                self.deploy_state.project_error =
                    Some("No sfdx-project.json found above the current directory".to_owned());
            }
            Err(error) => {
                self.deploy_state.project = None;
                self.deploy_state.items.clear();
                self.deploy_state.project_error = Some(error.to_string());
            }
        }
    }

    pub async fn recv_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
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
            }
            AppEvent::Startup(sf::StartupEvent::Version(version)) => {
                self.startup.version = Some(version);
            }
            AppEvent::Startup(sf::StartupEvent::CurrentOrg(org)) => {
                self.startup.current_org = Some(org);
                self.notice("Current org loaded");
            }
            AppEvent::Startup(sf::StartupEvent::Catalog(catalog)) => {
                self.startup.catalog = catalog;
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
                self.notice("Ready");
            }
            AppEvent::TransferResults {
                kind,
                retrieve,
                query,
                results,
            } => {
                let state = if kind == SearchKind::Tests {
                    &mut self.test_search
                } else {
                    &mut self.transfer_search
                };
                if state.retrieve == retrieve && state.query == query {
                    state.results = results;
                    state.cursor = 0;
                    state.searching = false;
                    state.dirty = false;
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
                {
                    record.lines.push(line);
                }
            }
            AppEvent::Sf(SfEvent::Finished { job_id, outcome }) => {
                self.finish_job(job_id, outcome);
            }
        }
    }

    fn finish_job(&mut self, job_id: u64, outcome: ProcessOutcome) {
        let purpose = self
            .jobs
            .get(&job_id)
            .map(|entry| entry.purpose.clone())
            .unwrap_or(JobPurpose::Command {
                id: String::new(),
                command: String::new(),
            });
        if let Some(entry) = self.jobs.get_mut(&job_id) {
            entry.finished_seen = true;
        }
        let parsed = sf::runner::parse_json_document(&outcome.stdout);
        if let Some(record) = self.output_state.find_mut(job_id) {
            record.status = Some(outcome.clone());
            record.json = parsed.clone();
        }
        if !outcome.succeeded() {
            let detail = if outcome.stderr.trim().is_empty() {
                outcome.stdout.trim()
            } else {
                outcome.stderr.trim()
            };
            self.error(format!(
                "{} failed: {}",
                purpose.label(),
                truncate(detail, 160)
            ));
        }
        let user_command = matches!(
            purpose,
            JobPurpose::Command { .. }
                | JobPurpose::Transfer { .. }
                | JobPurpose::OrgAction { .. }
                | JobPurpose::Installer { .. }
        );
        let succeeded = outcome.succeeded();
        match (purpose, parsed) {
            (JobPurpose::OrgList, Some(value)) => {
                self.org_state.orgs = sf::org::parse_org_list(&value);
                self.org_state.loading = false;
                self.org_state.selected = self
                    .org_state
                    .selected
                    .min(self.org_state.orgs.len().saturating_sub(1));
                self.notice(format!("Loaded {} orgs", self.org_state.orgs.len()));
            }
            (JobPurpose::OrgDetail { username }, Some(value)) => {
                self.org_state.detail = Some(sf::org::parse_org_display(&value));
                self.org_state.detail_for = Some(username);
                self.notice("Org details loaded");
            }
            (JobPurpose::Help { id }, _) => {
                let text = format!("{}{}", outcome.stdout, outcome.stderr);
                self.catalog_state.help_flags = catalog::parse_help_flags(&text);
                self.catalog_state.help_for = Some(id);
            }
            (JobPurpose::CatalogRefresh, Some(value)) => {
                if let Ok(commands) = catalog::parse_commands_json(&value.to_string()) {
                    self.startup.catalog = Catalog {
                        commands,
                        source: CatalogSource::Live,
                    };
                    self.notice("Command catalog refreshed");
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
                    self.deploy_state.report = Some(report);
                    self.deploy_state.job_id = None;
                    self.deploy_state.poll_after = None;
                    self.notice("Retrieve finished");
                } else if let Some(id) = sf::deploy::parse_started_job_id(&value) {
                    self.deploy_state.job_id = Some(id.clone());
                    self.deploy_state.poll_after = Some(Instant::now() + Duration::from_secs(3));
                    self.deploy_state.report = None;
                    self.notice(format!("Started deploy job {id}"));
                }
            }
            (JobPurpose::Report { .. }, Some(value)) => {
                let report = sf::deploy::parse_report(&value);
                let complete = report.is_complete();
                self.deploy_state.report = Some(report);
                if complete {
                    self.deploy_state.job_id = None;
                    self.deploy_state.poll_after = None;
                    self.notice("Done — h home · Backspace back");
                }
            }
            (JobPurpose::Command { command, .. }, _) => {
                self.config.record_run(command, outcome.exit_code);
            }
            (JobPurpose::OrgAction { description }, _) => {
                self.notice(format!("{description} finished"));
                self.refresh_orgs();
            }
            _ => {}
        }
        if succeeded && user_command {
            self.notice("Done — h home · Backspace back");
        }
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
        let test_mode = self.tab == Tab::Tests;
        let debounce_ready = if test_mode {
            self.test_search.debounce_ready()
        } else {
            self.transfer_search.debounce_ready()
        };
        let searching = if test_mode {
            self.test_search.searching
        } else {
            self.transfer_search.searching
        };
        let query = if test_mode {
            self.test_search.query.clone()
        } else {
            self.transfer_search.query.clone()
        };
        let set = if test_mode {
            self.test_suggestion_cache
                .as_ref()
                .map(|(_, set)| Arc::clone(set))
        } else {
            self.suggestion_cache
                .as_ref()
                .map(|(_, set)| Arc::clone(set))
        };
        if debounce_ready
            && !searching
            && let Some(set) = set
        {
            let kind = if test_mode {
                SearchKind::Tests
            } else {
                SearchKind::Transfer
            };
            let retrieve = !test_mode && self.transfer_search.retrieve;
            if test_mode {
                self.test_search.searching = true;
                self.test_search.dirty = false;
            } else {
                self.transfer_search.searching = true;
                self.transfer_search.dirty = false;
            }
            let tx = self.event_tx.clone();
            self.search_task = Some(tokio::spawn(async move {
                let results = set.filter(&query, 8);
                let _ = tx.send(AppEvent::TransferResults {
                    kind,
                    retrieve,
                    query,
                    results,
                });
            }));
        }
        if let Some(Modal::Prompt { fields, .. }) = self.modal.as_mut()
            && fields.iter().any(PromptField::debounce_ready)
        {
            for field in fields.iter_mut() {
                field.ensure_suggestions();
            }
        }
        if self.catalog_state.query != self.catalog_state.active_query
            && self
                .catalog_state
                .query_changed_at
                .is_some_and(|when| when.elapsed() >= SUGGESTION_DEBOUNCE)
        {
            self.catalog_state.active_query = self.catalog_state.query.clone();
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
        let report_in_flight = self
            .jobs
            .values()
            .any(|entry| matches!(entry.purpose, JobPurpose::Report { .. }));
        let ready = self
            .deploy_state
            .poll_after
            .is_some_and(|when| Instant::now() >= when);
        if ready
            && !report_in_flight
            && let Some(job_id) = self.deploy_state.job_id.clone()
        {
            let options = self.deploy_state.options.clone();
            self.deploy_state.poll_after = Some(Instant::now() + Duration::from_secs(4));
            if let Some(argv) = options.report_argv(&job_id) {
                self.spawn(argv, JobPurpose::Report { options });
            }
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
        let command = format!("{} {}", executable.display(), argv.join(" "));
        let show_output = matches!(
            purpose,
            JobPurpose::Command { .. }
                | JobPurpose::Help { .. }
                | JobPurpose::Transfer { .. }
                | JobPurpose::Installer { .. }
                | JobPurpose::OrgAction { .. }
        );
        self.output_state.begin(id, command, purpose.label());
        match sf::runner::spawn(&executable, &argv, id, self.event_tx.clone(), AppEvent::Sf) {
            Ok(job) => {
                self.jobs.insert(
                    id,
                    JobEntry {
                        job,
                        purpose,
                        finished_seen: false,
                    },
                );
                if show_output {
                    self.tab = Tab::Output;
                }
            }
            Err(error) => {
                if let Some(record) = self.output_state.find_mut(id) {
                    record.status = Some(ProcessOutcome {
                        exit_code: None,
                        stdout: String::new(),
                        stderr: error.to_string(),
                    });
                }
                self.error(error.to_string());
            }
        }
    }

    fn cancel_active_job(&mut self) -> bool {
        let Some(id) = self.jobs.keys().max().copied() else {
            return false;
        };
        if let Some(entry) = self.jobs.remove(&id) {
            let cancelled = entry.job.cancel();
            if cancelled {
                self.notice(format!("Cancelled job #{id}"));
            }
            cancelled
        } else {
            false
        }
    }

    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub fn active_job_label(&self) -> Option<String> {
        self.jobs
            .values()
            .next()
            .map(|entry| entry.purpose.label().to_owned())
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
                KeyCode::Char('u') => {
                    if self.catalog_state.search_focused {
                        self.catalog_state.query.clear();
                    } else if matches!(self.tab, Tab::Retrieve | Tab::Deploy) {
                        self.transfer_search.query.clear();
                        self.transfer_search.mark_edited();
                    }
                    return;
                }
                KeyCode::Up | KeyCode::Down if self.tab == Tab::Favorites => {
                    let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                    if let Some(id) = self.selected_favorite_id() {
                        self.config.move_favorite(&id, delta);
                    }
                    return;
                }
                _ => {}
            }
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
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.switch_tab(1),
            KeyCode::BackTab => self.switch_tab(-1),
            KeyCode::Char(number @ '1'..='7') => {
                let index = number as usize - '1' as usize;
                if let Some(tab) = self.tabs().get(index).copied() {
                    self.tab = tab;
                }
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::F(5) => self.refresh_current_tab(),
            KeyCode::Esc => {
                if !self.cancel_active_job() {
                    self.status.clear();
                }
            }
            KeyCode::Backspace if !self.catalog_state.search_focused => self.go_back(),
            KeyCode::Char('h') if !self.catalog_state.search_focused => self.go_home(),
            KeyCode::Char('y') => self.copy_context(),
            _ => match self.tab {
                Tab::Home => self.home_key(key),
                Tab::Retrieve | Tab::Deploy => self.transfer_key(key),
                Tab::Tests => self.test_key(key),
                Tab::Catalog => self.catalog_key(key),
                Tab::Favorites => self.favorites_key(key),
                Tab::Orgs => self.org_key(key),
                Tab::Output => self.output_key(key),
                Tab::Setup => self.setup_key(key),
            },
        }
    }

    fn switch_tab(&mut self, delta: isize) {
        let tabs = self.tabs();
        let current = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0) as isize;
        let index = (current + delta).rem_euclid(tabs.len() as isize) as usize;
        self.set_tab(tabs[index]);
    }

    fn set_tab(&mut self, tab: Tab) {
        if tab == Tab::Tests {
            self.ensure_suggestion_sets();
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
            Tab::Home => "r retrieve · d deploy · t tests · g results · c create · a login",
            Tab::Retrieve => "type component · Space select · Enter confirm · Esc home",
            Tab::Deploy => "type component · Space select · Enter confirm · Esc home",
            Tab::Tests => "type test class · Space select · Enter confirm · Esc home",
            Tab::Catalog => "/ search · Enter help · r run",
            Tab::Favorites => "Enter run · e preset · x remove",
            Tab::Orgs => "Enter details · o open · l login",
            Tab::Output => "h home · Backspace back · j/k scroll · z JSON",
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
            Tab::Home => Some(QUICK_ACTIONS[self.home_state.selected].command.to_owned()),
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
            Tab::Favorites => self.selected_favorite_id().map(|id| {
                format!("sf {id} {}", self.preset_flags(&id).join(" "))
                    .trim()
                    .to_owned()
            }),
            Tab::Orgs => self.selected_org().map(|org| org.label()),
            Tab::Output => self
                .output_state
                .records
                .get(self.output_state.selected)
                .map(|record| record.command.clone()),
            Tab::Setup => sf::install_instructions()
                .first()
                .map(|(_, command)| (*command).to_owned()),
        };
        let Some(text) = text else {
            return;
        };
        match clipboard::copy(&text) {
            Ok(()) => self.notice("Copied to clipboard"),
            Err(error) => self.error(error.to_string()),
        }
    }

    // ----- Home ---------------------------------------------------------------------

    fn home_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.home_state.selected = self.home_state.selected.saturating_sub(1),
            KeyCode::Down => {
                self.home_state.selected =
                    (self.home_state.selected + 1).min(QUICK_ACTIONS.len() - 1)
            }
            KeyCode::Enter => self.run_quick_action(self.home_state.selected),
            KeyCode::Char(character) => {
                if let Some(index) = QUICK_ACTIONS
                    .iter()
                    .position(|action| action.key == character.to_ascii_lowercase())
                {
                    self.run_quick_action(index);
                }
            }
            _ => {}
        }
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
            KeyCode::Enter => self.open_transfer_confirm(),
            KeyCode::Backspace => {
                self.transfer_search.query.pop();
                self.transfer_search.mark_edited();
            }
            KeyCode::Esc => self.go_home(),
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

    fn ensure_suggestion_sets(&mut self) {
        let root = self
            .deploy_state
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        if self
            .suggestion_cache
            .as_ref()
            .is_none_or(|(cached_root, _)| cached_root != &root)
        {
            let all = Arc::new(crate::sf::metadata::transfer_suggestions(
                self.deploy_state.project.as_ref(),
            ));
            let tests = Arc::new(crate::sf::metadata::test_suggestions(&all));
            self.suggestion_cache = Some((root.clone(), all));
            self.test_suggestion_cache = Some((root, tests));
        }
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
            KeyCode::Enter => self.open_test_confirm(),
            KeyCode::Backspace => {
                self.test_search.query.pop();
                self.test_search.mark_edited();
            }
            KeyCode::Esc => self.go_home(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.test_search.query.clear();
                self.test_search.mark_edited();
            }
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
        self.modal = Some(Modal::TestConfirm {
            entries,
            focus: 0,
            show_command: false,
            org: self.org_target_hint().into(),
        });
    }

    fn run_quick_action(&mut self, index: usize) {
        let Some(action) = QUICK_ACTIONS.get(index) else {
            return;
        };
        let target = self.org_target_hint();
        match action.command {
            "sf org login web" => {
                self.open_prompt(
                    "Log in",
                    PromptKind::LoginWeb,
                    vec![PromptField::new("Alias", "")],
                );
            }
            "sf project retrieve start" | "sf project deploy start" => {
                let retrieve = action.command == "sf project retrieve start";
                self.ensure_suggestion_sets();
                self.transfer_search = TransferSearchState::new(retrieve);
                self.set_tab(if retrieve { Tab::Retrieve } else { Tab::Deploy });
                self.notice("Type a component name to search");
            }
            "sf apex run test" => {
                self.ensure_suggestion_sets();
                self.test_search = TransferSearchState::new(false);
                self.set_tab(Tab::Tests);
                self.notice("Type a test class name to search");
            }
            "sf template generate" => {
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
            "sf logic get test" => {
                self.open_prompt(
                    "Test results",
                    PromptKind::GetTestResults,
                    vec![
                        PromptField::new("Test run id", ""),
                        PromptField::new("Org", target),
                    ],
                );
            }
            _ => {}
        }
    }

    pub fn project_source_paths(&self) -> String {
        self.deploy_state
            .project
            .as_ref()
            .map(|project| project.package_directories.join(", "))
            .unwrap_or_else(|| "force-app".to_owned())
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

    pub fn catalog_rows(&self) -> Vec<CatalogRow> {
        let catalog = &self.startup.catalog;
        let query = self.catalog_state.active_query.trim();
        if query.is_empty() {
            let mut rows = vec![CatalogRow::Header(
                "Popular — your five most used commands".to_owned(),
            )];
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
                    self.catalog_state.query_changed_at = Some(Instant::now());
                }
                KeyCode::Enter => {
                    self.catalog_state.search_focused = false;
                    self.catalog_state.active_query = self.catalog_state.query.clone();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.catalog_state.query.push(character);
                    self.catalog_state.query_changed_at = Some(Instant::now());
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

    pub fn selected_favorite_id(&self) -> Option<String> {
        self.config
            .favorites
            .get(self.favorites_state.selected)
            .map(|favorite| favorite.id.clone())
    }

    fn favorites_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                self.favorites_state.selected = self.favorites_state.selected.saturating_sub(1)
            }
            KeyCode::Down => {
                if !self.config.favorites.is_empty() {
                    self.favorites_state.selected =
                        (self.favorites_state.selected + 1).min(self.config.favorites.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(id) = self.selected_favorite_id() {
                    let flags = self.preset_flags(&id).join(" ");
                    self.run_command(&id, &flags);
                }
            }
            KeyCode::Char('e') => {
                if let Some(id) = self.selected_favorite_id() {
                    let preset = self.preset_flags(&id).join(" ");
                    self.open_prompt(
                        "Edit preset flags",
                        PromptKind::EditPreset { id: id.clone() },
                        vec![PromptField::new("Preset flags", preset)],
                    );
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(id) = self.selected_favorite_id() {
                    self.config.remove_favorite(&id);
                    self.favorites_state.selected = self
                        .favorites_state
                        .selected
                        .min(self.config.favorites.len().saturating_sub(1));
                    self.notice("Removed from favorites");
                }
            }
            _ => {}
        }
    }

    // ----- Orgs ---------------------------------------------------------------------

    pub fn selected_org(&self) -> Option<&OrgSummary> {
        self.org_state.orgs.get(self.org_state.selected)
    }

    fn refresh_orgs(&mut self) {
        self.org_state.loading = true;
        let argv = vec!["org".to_owned(), "list".to_owned(), "--json".to_owned()];
        self.spawn(argv, JobPurpose::OrgList);
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
            KeyCode::Enter => {
                if let Some(org) = self.selected_org() {
                    let username = org.username.clone();
                    let argv = vec![
                        "org".to_owned(),
                        "display".to_owned(),
                        "--target-org".to_owned(),
                        username.clone(),
                        "--json".to_owned(),
                    ];
                    self.spawn(argv, JobPurpose::OrgDetail { username });
                }
            }
            KeyCode::Char('o') => {
                if let Some(url) = self.selected_org().map(|org| org.instance_url.clone()) {
                    if url.is_empty() {
                        self.error("This org has no instance URL");
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

    fn output_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('o') => self.output_state.show_details = !self.output_state.show_details,
            KeyCode::Char('z') => {
                self.output_state.show_details = true;
                self.output_state.show_json = !self.output_state.show_json;
            }
            KeyCode::Char('r') => {
                let command = self
                    .output_state
                    .records
                    .get(self.output_state.selected)
                    .map(|record| record.command.clone());
                if let Some(command) = command {
                    let mut parts = split_args(&command).into_iter();
                    let _exe = parts.next();
                    let argv: Vec<String> = parts.collect();
                    if !argv.is_empty() {
                        self.spawn(
                            argv.clone(),
                            JobPurpose::Command {
                                id: argv.iter().take(3).cloned().collect::<Vec<_>>().join(" "),
                                command,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn run_transfer(&mut self) {
        let options = self.deploy_state.options.clone();
        let argv = options.argv();
        self.spawn(argv, JobPurpose::Transfer { options });
        self.tab = Tab::Output;
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
            Modal::Help | Modal::Message { .. } => {}
            Modal::Confirm { action, .. } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.execute(action),
                _ => self.notice("Cancelled"),
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
                    let mut argv = vec![
                        "apex".to_owned(),
                        "run".to_owned(),
                        "test".to_owned(),
                        "--result-format".to_owned(),
                        "human".to_owned(),
                        "--wait".to_owned(),
                        "10".to_owned(),
                    ];
                    for class_name in &classes {
                        argv.push("--class-names".to_owned());
                        argv.push(class_name.clone());
                    }
                    if let Some(org) = org.as_ref().filter(|org| !org.is_empty()) {
                        argv.push("--target-org".to_owned());
                        argv.push(org.clone());
                    }
                    argv.push("--json".to_owned());
                    let display = format!("sf {}", argv.join(" "));
                    self.spawn(
                        argv,
                        JobPurpose::Command {
                            id: "apex run test".to_owned(),
                            command: display,
                        },
                    );
                    self.tab = Tab::Output;
                }
                KeyCode::Esc => {
                    self.tab = Tab::Tests;
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
                    let mut choices = vec![String::new()];
                    choices.extend(
                        self.org_state
                            .orgs
                            .iter()
                            .map(|org| org.label())
                            .collect::<Vec<_>>(),
                    );
                    if let Some(current) = &org {
                        choices.push(current.clone());
                    }
                    choices.sort();
                    choices.dedup();
                    org = choices.first().cloned();
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
                    let argv = options.argv();
                    self.spawn(argv, JobPurpose::Transfer { options });
                    self.tab = Tab::Output;
                }
                KeyCode::Esc => {
                    self.tab = if retrieve { Tab::Retrieve } else { Tab::Deploy };
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
                    let mut choices = vec![String::new()];
                    choices.extend(
                        self.org_state
                            .orgs
                            .iter()
                            .map(|org| org.label())
                            .collect::<Vec<_>>(),
                    );
                    if let Some(current) = &options.target_org {
                        choices.push(current.clone());
                    }
                    choices.sort();
                    choices.dedup();
                    if let Some(index) = choices
                        .iter()
                        .position(|org| Some(org) == options.target_org.as_ref())
                    {
                        let next = (index + 1) % choices.len();
                        options.target_org = choices[next].clone().into();
                    } else {
                        options.target_org = choices.first().cloned();
                    }
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
                    self.submit_prompt(&kind, &fields);
                }
                KeyCode::Backspace => {
                    if let Some(field) = fields.get_mut(focus)
                        && field.cursor > 0
                    {
                        let previous = field.value[..field.cursor]
                            .char_indices()
                            .next_back()
                            .map(|(index, _)| index)
                            .unwrap_or(0);
                        field.value.replace_range(previous..field.cursor, "");
                        field.cursor = previous;
                        field.mark_edited();
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
                        field.cursor = field.cursor.saturating_sub(1);
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
                        field.cursor = (field.cursor + 1).min(field.value.len());
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

    fn submit_prompt(&mut self, kind: &PromptKind, fields: &[PromptField]) {
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
                    return;
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
                    return;
                };
                let name = values.get(1).map(String::as_str).unwrap_or("").trim();
                if name.is_empty() {
                    self.error("Enter the new component name");
                    return;
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
            PromptKind::RunApexTests => {
                let classes = split_names(values.get(1).map(String::as_str).unwrap_or(""));
                let format = match values.get(2).map(String::as_str).unwrap_or("human") {
                    "json" => "json",
                    _ => "human",
                };
                let mut argv = vec![
                    "apex".to_owned(),
                    "run".to_owned(),
                    "test".to_owned(),
                    "--result-format".to_owned(),
                    format.to_owned(),
                    "--wait".to_owned(),
                    "10".to_owned(),
                ];
                for class_name in classes {
                    argv.push("--class-names".to_owned());
                    argv.push(class_name);
                }
                if let Some(org) = values.first().filter(|org| !org.is_empty()) {
                    argv.push("--target-org".to_owned());
                    argv.push(org.clone());
                }
                argv.push("--json".to_owned());
                let display = format!("sf {}", argv.join(" "));
                self.spawn(
                    argv,
                    JobPurpose::Command {
                        id: "apex run test".to_owned(),
                        command: display,
                    },
                );
            }
            PromptKind::GetTestResults => {
                let Some(run_id) = values.first().filter(|id| !id.is_empty()) else {
                    self.error("A test run id is required");
                    return;
                };
                let mut argv = vec![
                    "logic".to_owned(),
                    "get".to_owned(),
                    "test".to_owned(),
                    "--test-run-id".to_owned(),
                    run_id.clone(),
                ];
                if let Some(org) = values.get(1).filter(|org| !org.is_empty()) {
                    argv.push("--target-org".to_owned());
                    argv.push(org.clone());
                }
                argv.push("--json".to_owned());
                let display = format!("sf {}", argv.join(" "));
                self.spawn(
                    argv,
                    JobPurpose::Command {
                        id: "logic get test".to_owned(),
                        command: display,
                    },
                );
            }
            PromptKind::MetadataNames => {
                let names = split_names(values.first().map(String::as_str).unwrap_or(""));
                if names.is_empty() {
                    self.error("Enter at least one metadata name, e.g. ApexClass, Flow");
                    return;
                }
                self.deploy_state.options.metadata_names = names;
                self.run_transfer();
            }
            PromptKind::LoginWeb => {
                let mut argv = vec!["org".to_owned(), "login".to_owned(), "web".to_owned()];
                if let Some(alias) = values.first().filter(|alias| !alias.is_empty()) {
                    argv.push("--alias".to_owned());
                    argv.push(alias.clone());
                }
                if values.get(1).is_some_and(|answer| {
                    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes" | "true")
                }) {
                    argv.push("--set-default".to_owned());
                }
                argv.push("--json".to_owned());
                self.spawn(
                    argv,
                    JobPurpose::OrgAction {
                        description: "Web login".to_owned(),
                    },
                );
            }
            PromptKind::CreateScratch => {
                let alias = values.first().cloned().unwrap_or_default();
                if alias.is_empty() {
                    self.error("An alias is required for a scratch org");
                    return;
                }
                let mut argv = vec![
                    "org".to_owned(),
                    "create".to_owned(),
                    "scratch".to_owned(),
                    "--alias".to_owned(),
                    alias,
                ];
                if let Some(definition) = values.get(1).filter(|path| !path.is_empty()) {
                    argv.push("--definition-file".to_owned());
                    argv.push(definition.clone());
                }
                let days = values
                    .get(2)
                    .filter(|days| !days.is_empty())
                    .map(String::as_str)
                    .unwrap_or("7");
                argv.push("--duration-days".to_owned());
                argv.push(days.to_owned());
                argv.push("--json".to_owned());
                self.spawn(
                    argv,
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
            PromptKind::Tests => {
                let tests = values
                    .first()
                    .map(|value| {
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|test| !test.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                self.deploy_state.options.tests = tests;
                self.run_transfer();
            }
        }
    }

    fn run_command(&mut self, id: &str, flags: &str) {
        let mut argv: Vec<String> = id.split_whitespace().map(str::to_owned).collect();
        argv.extend(split_args(flags));
        let display = format!("sf {id} {flags}").trim().to_owned();
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
            PendingAction::Run { id, flags } => self.run_command(&id, &flags),
            PendingAction::SetDefault { username, devhub } => {
                let name = if devhub {
                    "target-dev-hub"
                } else {
                    "target-org"
                };
                let argv = vec![
                    "config".to_owned(),
                    "set".to_owned(),
                    format!("{name}={username}"),
                ];
                self.spawn(
                    argv,
                    JobPurpose::OrgAction {
                        description: "Config update".to_owned(),
                    },
                );
            }
            PendingAction::Logout { username } => {
                let argv = vec![
                    "org".to_owned(),
                    "logout".to_owned(),
                    "--target-org".to_owned(),
                    username,
                    "--no-prompt".to_owned(),
                    "--json".to_owned(),
                ];
                self.spawn(
                    argv,
                    JobPurpose::OrgAction {
                        description: "Logout".to_owned(),
                    },
                );
            }
            PendingAction::RefreshOrgs => self.refresh_orgs(),
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

fn build_pick_items(project: &SfProject) -> Vec<PickItem> {
    let mut items: Vec<PickItem> = project
        .package_directories
        .iter()
        .map(|directory| PickItem {
            path: directory.clone(),
            label: format!("{directory} (package directory)"),
            kind: "source",
        })
        .collect();
    for manifest in &project.manifests {
        items.push(PickItem {
            path: manifest.clone(),
            label: format!("{manifest} (manifest)"),
            kind: "manifest",
        });
    }
    for source in project.source_files.iter().take(200) {
        items.push(PickItem {
            path: source.clone(),
            label: source.clone(),
            kind: "source",
        });
    }
    for component in crate::sf::metadata::transfer_suggestions(Some(project)).items {
        if component.value.contains(':') {
            items.push(PickItem {
                path: component.value,
                label: component.path,
                kind: "metadata",
            });
        }
    }
    items
}

pub fn suggestions_for(field: &PromptField) -> Vec<SuggestionItem> {
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

/// Run the main event loop using an already initialized terminal.
pub async fn run(
    terminal: &mut DefaultTerminal,
    startup: Startup,
    config: Config,
    config_dir: PathBuf,
) -> anyhow::Result<()> {
    let mut app = App::new(startup, config, config_dir);
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    while !app.should_quit {
        tokio::select! {
            _ = ticker.tick() => {}
            Some(event) = app.recv_event() => app.on_event(event),
        }
        while crossterm::event::poll(Duration::ZERO)? {
            let event = crossterm::event::read()?;
            if let crossterm::event::Event::Key(key) = event {
                app.handle_key(key);
            }
        }
        app.tick().await;
        terminal.draw(|frame| crate::ui::draw(frame, &app))?;
    }
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
    fn switching_tabs_covers_home_and_number_shortcuts() {
        let mut app = test_app();
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Output);
        app.handle_key(key(KeyCode::Char('7')));
        assert_eq!(app.tab, Tab::Orgs);
        app.handle_key(key(KeyCode::Char('1')));
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

    #[test]
    fn home_shortcuts_put_retrieve_deploy_first_and_offer_suggestions() {
        assert_eq!(QUICK_ACTIONS[0].key, 'r');
        assert_eq!(QUICK_ACTIONS[1].key, 'd');
        assert_eq!(QUICK_ACTIONS[5].key, 'a');
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.tab, Tab::Retrieve);
        assert!(app.transfer_search.retrieve);
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.tab, Tab::Deploy);
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('g')));
        assert!(matches!(
            app.modal.as_ref(),
            Some(Modal::Prompt {
                kind: PromptKind::GetTestResults,
                ..
            })
        ));
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
    fn home_key_and_backspace_navigate_between_screens() {
        let mut app = test_app();
        app.tab = Tab::Orgs;
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Backspace));
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
            flavor: crate::sf::detect::SfFlavor::Sf,
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
        app.modal = Some(Modal::Help);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.modal.is_none());
    }
}
