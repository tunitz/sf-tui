//! Application state, key dispatch and ownership of background SF jobs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::clipboard;
use crate::config::Config;
use crate::sf::catalog::{self, Catalog, CatalogSource, CommandSpec, Flag};
use crate::sf::deploy::{PayloadMode, TestLevel, TransferOptions, TransferReport};
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::project::{self, SfProject};
use crate::sf::runner::{JobLine, ProcessOutcome, RunningJob, SfEvent};
use crate::sf::{self, Startup};

/// Screens shown in the tab bar. Setup is added only when no CLI was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Catalog,
    Favorites,
    Orgs,
    Deploy,
    Output,
    Setup,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Catalog => "Catalog",
            Self::Favorites => "Favorites",
            Self::Orgs => "Orgs",
            Self::Deploy => "Deploy/Retrieve",
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
#[derive(Debug)]
pub enum AppEvent {
    Sf(SfEvent),
    Startup(Startup),
    Notice { text: String, error: bool },
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

#[derive(Debug, Clone, Default)]
pub struct CatalogState {
    pub query: String,
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
}

impl PromptField {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            label: label.into(),
            value,
            cursor,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    RunFlags { id: String },
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
    pub should_quit: bool,
    pub status: String,
    pub status_error: bool,
    pub catalog_state: CatalogState,
    pub favorites_state: FavoritesState,
    pub org_state: OrgState,
    pub deploy_state: DeployState,
    pub output_state: OutputState,
    pub modal: Option<Modal>,
    pub setup_selected: usize,
    ticks: usize,
    probe_task: Option<JoinHandle<()>>,
    event_tx: UnboundedSender<AppEvent>,
    event_rx: UnboundedReceiver<AppEvent>,
    jobs: HashMap<u64, JobEntry>,
    next_job_id: u64,
}

impl App {
    pub fn new(startup: Startup, config: Config) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let selected_id = startup
            .catalog
            .commands
            .first()
            .map(|command| command.id.clone());
        let mut app = Self {
            startup,
            config,
            tab: Tab::Catalog,
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
            output_state: OutputState::default(),
            modal: None,
            setup_selected: 0,
            ticks: 0,
            probe_task: None,
            event_tx,
            event_rx,
            jobs: HashMap::new(),
            next_job_id: 1,
        };
        app.reload_project();
        if !app.startup.is_ready() {
            app.tab = Tab::Setup;
            app.status = "Salesforce CLI not found".to_owned();
            app.status_error = true;
        } else if !app.startup.notices.is_empty() {
            app.status = app.startup.notices.first().cloned().unwrap_or_default();
        }
        app
    }

    pub fn tabs(&self) -> Vec<Tab> {
        let mut tabs = vec![
            Tab::Catalog,
            Tab::Favorites,
            Tab::Orgs,
            Tab::Deploy,
            Tab::Output,
        ];
        if !self.startup.is_ready() {
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
            AppEvent::Startup(startup) => {
                let was_ready = self.startup.is_ready();
                let tab_was_setup = self.tab == Tab::Setup;
                self.startup = startup;
                if self.startup.is_ready() && (tab_was_setup || !was_ready) {
                    self.tab = Tab::Catalog;
                    self.notice("Salesforce CLI is ready");
                }
                if let Some(notice) = self.startup.notices.first() {
                    self.notice(notice.clone());
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
                if let Some(id) = sf::deploy::parse_started_job_id(&value) {
                    self.deploy_state.job_id = Some(id.clone());
                    self.deploy_state.poll_after = Some(Instant::now() + Duration::from_secs(3));
                    self.deploy_state.report = None;
                    self.notice(format!(
                        "Started {} job {id}",
                        if options.retrieve {
                            "retrieve"
                        } else {
                            "deploy"
                        }
                    ));
                }
            }
            (JobPurpose::Report { .. }, Some(value)) => {
                let report = sf::deploy::parse_report(&value);
                let complete = report.is_complete();
                self.deploy_state.report = Some(report);
                if complete {
                    self.deploy_state.job_id = None;
                    self.deploy_state.poll_after = None;
                    self.notice("Transfer finished");
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
    }

    pub async fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
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
            self.spawn(options.report_argv(&job_id), JobPurpose::Report { options });
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
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.switch_tab(1),
            KeyCode::BackTab => self.switch_tab(-1),
            KeyCode::Char(number @ '1'..='6') => {
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
            KeyCode::Char('y') => self.copy_context(),
            _ => match self.tab {
                Tab::Catalog => self.catalog_key(key),
                Tab::Favorites => self.favorites_key(key),
                Tab::Orgs => self.org_key(key),
                Tab::Deploy => self.deploy_key(key),
                Tab::Output => self.output_key(key),
                Tab::Setup => self.setup_key(key),
            },
        }
    }

    fn switch_tab(&mut self, delta: isize) {
        let tabs = self.tabs();
        let current = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0) as isize;
        let index = (current + delta).rem_euclid(tabs.len() as isize) as usize;
        self.tab = tabs[index];
    }

    fn refresh_current_tab(&mut self) {
        match self.tab {
            Tab::Orgs => self.refresh_orgs(),
            Tab::Deploy => self.reload_project(),
            Tab::Catalog => {
                let argv = vec!["commands".to_owned(), "--json".to_owned()];
                self.spawn(argv, JobPurpose::CatalogRefresh);
            }
            _ => self.notice("Nothing to refresh on this tab"),
        }
    }

    fn copy_context(&mut self) {
        let text = match self.tab {
            Tab::Catalog => self.selected_catalog_id().map(|id| format!("sf {id}")),
            Tab::Favorites => self.selected_favorite_id().map(|id| {
                format!("sf {id} {}", self.preset_flags(&id).join(" "))
                    .trim()
                    .to_owned()
            }),
            Tab::Orgs => self.selected_org().map(|org| org.label()),
            Tab::Deploy => Some(self.deploy_state.options.argv().join(" ")),
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

    // ----- Catalog -----------------------------------------------------------------

    pub fn catalog_rows(&self) -> Vec<CatalogRow> {
        let catalog = &self.startup.catalog;
        let query = self.catalog_state.query.trim();
        if query.is_empty() {
            let mut rows = Vec::new();
            let mut categories = catalog.categories();
            categories.sort();
            for category in categories {
                rows.push(CatalogRow::Header(category.clone()));
                for command in catalog
                    .commands
                    .iter()
                    .filter(|command| command.category == category)
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
                    }
                }
                KeyCode::Backspace => {
                    self.catalog_state.query.pop();
                    self.catalog_state.selected_id = self.selected_catalog_id();
                }
                KeyCode::Enter => self.catalog_state.search_focused = false,
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.catalog_state.query.push(character);
                    self.catalog_state.selected_id = None;
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
                    "Log in to an org (web)",
                    PromptKind::LoginWeb,
                    vec![PromptField::new("Alias (optional)", "")],
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

    fn deploy_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                self.deploy_state.selected = self.deploy_state.selected.saturating_sub(1)
            }
            KeyCode::Down => {
                if !self.deploy_state.items.is_empty() {
                    self.deploy_state.selected =
                        (self.deploy_state.selected + 1).min(self.deploy_state.items.len() - 1);
                }
            }
            KeyCode::Char(' ') => self.toggle_pick(),
            KeyCode::Char('m') => {
                self.deploy_state.options.mode = match self.deploy_state.options.mode {
                    PayloadMode::Source => PayloadMode::Manifest,
                    PayloadMode::Manifest => PayloadMode::TrackedChanges,
                    PayloadMode::TrackedChanges => PayloadMode::Source,
                };
            }
            KeyCode::Char('v') => {
                self.deploy_state.options.check_only = !self.deploy_state.options.check_only
            }
            KeyCode::Char('w') => {
                self.deploy_state.options.ignore_warnings =
                    !self.deploy_state.options.ignore_warnings
            }
            KeyCode::Char('t') => {
                self.deploy_state.options.test_level = self.deploy_state.options.test_level.cycle()
            }
            KeyCode::Char('r') => {
                self.deploy_state.options.retrieve = !self.deploy_state.options.retrieve
            }
            KeyCode::Char('R') => {
                self.reload_project();
                self.notice("Project reloaded");
            }
            KeyCode::Char('o') => {
                let current = self
                    .deploy_state
                    .options
                    .target_org
                    .clone()
                    .unwrap_or_default();
                self.open_prompt(
                    "Target org",
                    PromptKind::TargetOrg,
                    vec![PromptField::new("Username or alias", current)],
                );
            }
            KeyCode::Char('p') => {
                if let Some(job_id) = self.deploy_state.job_id.clone() {
                    let options = self.deploy_state.options.clone();
                    self.spawn(options.report_argv(&job_id), JobPurpose::Report { options });
                    self.deploy_state.poll_after = Some(Instant::now() + Duration::from_secs(4));
                }
            }
            KeyCode::Enter => {
                if self.deploy_state.options.test_level == TestLevel::RunSpecifiedTests
                    && !self.deploy_state.options.retrieve
                    && self.deploy_state.options.tests.is_empty()
                {
                    self.open_prompt(
                        "Tests to run",
                        PromptKind::Tests,
                        vec![PromptField::new("Comma-separated Apex test classes", "")],
                    );
                } else {
                    self.run_transfer();
                }
            }
            KeyCode::Esc => {
                self.deploy_state.report = None;
                self.deploy_state.job_id = None;
                self.deploy_state.poll_after = None;
            }
            _ => {}
        }
    }

    fn toggle_pick(&mut self) {
        let Some(item) = self
            .deploy_state
            .items
            .get(self.deploy_state.selected)
            .cloned()
        else {
            return;
        };
        let options = &mut self.deploy_state.options;
        match options.mode {
            PayloadMode::Source => {
                if item.kind == "manifest" {
                    self.deploy_state.options.mode = PayloadMode::Manifest;
                    self.deploy_state.options.manifest = Some(item.path);
                    return;
                }
                if let Some(position) = options.paths.iter().position(|path| *path == item.path) {
                    options.paths.remove(position);
                } else {
                    options.paths.push(item.path);
                }
            }
            PayloadMode::Manifest => {
                options.manifest = Some(item.path);
            }
            PayloadMode::TrackedChanges => {}
        }
    }

    fn run_transfer(&mut self) {
        let options = self.deploy_state.options.clone();
        if options.mode == PayloadMode::Source && options.paths.is_empty() {
            self.error("Select at least one source path (Space) or switch payload mode (m)");
            return;
        }
        if options.mode == PayloadMode::Manifest && options.manifest.is_none() {
            self.error("Select a package.xml manifest first");
            return;
        }
        let argv = options.argv();
        self.deploy_state.report = None;
        self.spawn(argv, JobPurpose::Transfer { options });
    }

    pub fn transfer_preview(&self) -> String {
        format!("sf {}", self.deploy_state.options.argv().join(" "))
    }

    // ----- Output -------------------------------------------------------------------

    fn output_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => {
                self.output_state.selected = self.output_state.selected.saturating_sub(1)
            }
            KeyCode::Down => {
                if !self.output_state.records.is_empty() {
                    self.output_state.selected =
                        (self.output_state.selected + 1).min(self.output_state.records.len() - 1);
                }
            }
            KeyCode::PageUp => {
                self.output_state.scroll = self.output_state.scroll.saturating_sub(15)
            }
            KeyCode::PageDown => self.output_state.scroll += 15,
            KeyCode::Char('j') | KeyCode::Char('k') => {
                let delta = if key.code == KeyCode::Char('j') {
                    1
                } else {
                    -1
                };
                if delta < 0 {
                    self.output_state.scroll = self.output_state.scroll.saturating_sub(1);
                } else {
                    self.output_state.scroll += 1;
                }
            }
            KeyCode::Char('z') => self.output_state.show_json = !self.output_state.show_json,
            KeyCode::Char('Y') => {
                let text = self
                    .output_state
                    .records
                    .get(self.output_state.selected)
                    .map(|record| {
                        record
                            .lines
                            .iter()
                            .map(|line| line.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                match clipboard::copy(&text) {
                    Ok(()) => self.notice("Copied output to clipboard"),
                    Err(error) => self.error(error.to_string()),
                }
            }
            _ => {}
        }
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
            Modal::Prompt {
                title,
                kind,
                mut fields,
                mut focus,
            } => match key.code {
                KeyCode::Esc => self.notice("Cancelled"),
                KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                    let delta = if matches!(key.code, KeyCode::Tab | KeyCode::Down) {
                        1
                    } else {
                        fields.len() as isize - 1
                    };
                    focus = (focus as isize + delta).rem_euclid(fields.len() as isize) as usize;
                    self.modal = Some(Modal::Prompt {
                        title,
                        kind,
                        fields,
                        focus,
                    });
                }
                KeyCode::Enter => {
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
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(field) = fields.get_mut(focus) {
                        field.value.insert(field.cursor, character);
                        field.cursor += character.len_utf8();
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
            PromptKind::LoginWeb => {
                let mut argv = vec!["org".to_owned(), "login".to_owned(), "web".to_owned()];
                if let Some(alias) = values.first().filter(|alias| !alias.is_empty()) {
                    argv.push("--alias".to_owned());
                    argv.push(alias.clone());
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
        let tx = self.event_tx.clone();
        self.probe_task = Some(tokio::spawn(async move {
            let startup = sf::startup().await;
            let _ = tx.send(AppEvent::Startup(startup));
        }));
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
    items
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
) -> anyhow::Result<()> {
    let mut app = App::new(startup, config);
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
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
        App::new(
            Startup {
                installation: None,
                version: None,
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
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn switching_tabs_wraps_and_skips_missing_setup_tab() {
        let mut app = test_app();
        app.tab = Tab::Catalog;
        assert_eq!(app.tab, Tab::Catalog);
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Setup);
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Orgs);
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
    fn help_modal_closes_on_any_key() {
        let mut app = test_app();
        app.modal = Some(Modal::Help);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.modal.is_none());
    }
}
