//! Application state, key dispatch and ownership of background SF jobs.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use serde_json::Value;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;

use crate::clipboard;
use crate::config::Config;
use crate::sf::create::{
    self, CreateDraft, CreateKind, Destination, FieldKey, FieldSpec, PublishReport,
};
use crate::sf::deploy::{PayloadMode, TransferOptions};
use crate::sf::metadata::{ORG_BROWSE_TYPES, OrgComponent, SuggestionItem, SuggestionSet};
use crate::sf::org::{OrgDetail, OrgSummary};
use crate::sf::project::{self, SfProject};
use crate::sf::runner::{JobLine, ProcessOutcome, RunningJob, SfEvent};
use crate::sf::test_results;
use crate::sf::{self, Startup};
use crate::state_store::{self, OrgCache, SavedTestRun, SfVersionCache, TestHistory};
use crate::ui::RenderCache;
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
    /// Internal helper that cancels a deploy by its exact server job id. Never a user slot and
    /// never rerunnable; only updates the parent transfer row.
    DeployCancel {
        transfer_job: u64,
    },
    /// A typed Create Metadata scaffold. Generates into a private staging directory; on success
    /// its output is published with exclusive creation into the resolved package source root.
    Create {
        draft: CreateDraft,
        staging: PathBuf,
        project_root: PathBuf,
        expected: Vec<PathBuf>,
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
            Self::DeployCancel { .. } => "deploy cancel",
            Self::Create { .. } => "create",
        }
    }
}

/// Which picker a search belongs to. A later Create wizard can add its own variant and reuse the
/// same scheduler, request/reply types and blocking worker without changing any ranking code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Transfer,
    Tests,
    /// Deferred fuzzy ranking for the Create wizard's grouped type picker.
    Create,
}

/// Immutable scope a reply must match before it may change a picker. Captures the project root
/// whose index produced the ranking plus the mode/org that were active at request time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchScope {
    pub root: PathBuf,
    /// Transfer mode: `true` = retrieve, `false` = deploy. Ignored by other kinds.
    pub retrieve: bool,
    /// Org scope for Apex tests. Ignored by other kinds.
    pub org: Option<String>,
}

/// A request from a picker into the shared search controller. `generation` increments on every
/// edit, so an older reply can be discarded without ever aborting blocking work.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub kind: SearchKind,
    pub generation: u64,
    pub scope: SearchScope,
    pub query: String,
    /// Identity of the highlighted result, so the cursor can follow its item across re-filters.
    pub cursor_value: Option<String>,
    /// Identities checked at request time. A reply may preserve these but must never introduce a
    /// new check from result data.
    pub checked: Vec<String>,
}

/// Why a ranking attempt produced no results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// The project index is still building; a fresh reply arrives when it is installed.
    Indexing,
    /// No usable index exists for this request (failed build or unreadable project).
    Unavailable,
}

/// Outcome of one ranking attempt, tagged with the exact request it answers.
#[derive(Debug, Clone)]
pub struct SearchReply {
    pub request: SearchRequest,
    pub outcome: Result<Vec<SuggestionItem>, SearchError>,
}

/// Messages sent from background processes into the UI thread.
pub enum AppEvent {
    Sf(SfEvent),
    /// A picker ranking finished. Always processed so the single in-flight slot is released, then
    /// discarded unless this is still the newest request for its picker and scope.
    SearchResults(SearchReply),
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
    /// Asynchronous pre-flight check before the Review step of the Create wizard.
    CreateChecked {
        draft: CreateDraft,
        paths: Vec<PathBuf>,
        collisions: Vec<PathBuf>,
        error: Option<String>,
    },
    /// Publication result for a finished Create job, including any partial writes.
    CreatePublished {
        job_id: u64,
        draft: CreateDraft,
        staging: PathBuf,
        report: PublishReport,
    },
    /// Result of a background `sf org list metadata` helper (not a user job).
    OrgMetadata {
        scope: String,
        username: String,
        metadata_type: String,
        generation: u64,
        result: Result<Vec<OrgComponent>, String>,
    },
}

/// A destination or action on the main menu. Retrieve and Deploy share one Metadata screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Metadata,
    Create,
    Tests,
}

#[derive(Debug, Clone, Copy)]
pub struct MenuItem {
    pub key: char,
    pub label: &'static str,
    pub action: MenuAction,
}

pub const MENU_ITEMS: &[MenuItem] = &[
    MenuItem {
        key: '1',
        label: "Retrieve/Deploy Metadata",
        action: MenuAction::Metadata,
    },
    MenuItem {
        key: '2',
        label: "Create Metadata",
        action: MenuAction::Create,
    },
    MenuItem {
        key: '3',
        label: "Apex Tests",
        action: MenuAction::Tests,
    },
];

/// Which column of the F6 browser receives navigation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrgBrowseFocus {
    #[default]
    Orgs,
    Types,
    Components,
}

/// Per-session state for the F6 "Browse Org" picker.
///
/// Listings are cached by `username + metadata type`, and each scope keeps its own selection so
/// switching org or type can never apply a component from the wrong org.
#[derive(Debug, Clone, Default)]
pub struct OrgBrowseState {
    pub open: bool,
    pub focus: OrgBrowseFocus,
    pub org_selected: usize,
    pub type_selected: usize,
    /// Username used for the CLI (aliases are display-only).
    pub username: String,
    pub org_label: String,
    pub metadata_type: String,
    pub rows: Vec<OrgComponent>,
    pub cursor: usize,
    pub checked: Vec<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub cache: HashMap<String, Vec<OrgComponent>>,
    pub exact_rows: HashMap<String, Vec<OrgComponent>>,
    pub local_identities: HashSet<String>,
}

impl OrgBrowseState {
    /// Stable cache key for one browsed org and metadata type.
    pub fn scope(username: &str, metadata_type: &str) -> String {
        format!("{username}\u{1f}{metadata_type}")
    }

    pub fn current_scope(&self) -> String {
        Self::scope(&self.username, &self.metadata_type)
    }

    pub fn has_scope(&self) -> bool {
        !self.username.is_empty() && !self.metadata_type.is_empty()
    }

    /// Rows from the active listing plus any exact names added with F9, deduplicated.
    pub fn visible_rows(&self) -> Vec<OrgComponent> {
        let mut rows = self.rows.clone();
        if let Some(exact) = self.exact_rows.get(&self.current_scope()) {
            rows.extend(exact.iter().cloned());
        }
        let mut seen = HashSet::new();
        rows.retain(|row| seen.insert(row.identity()));
        rows.sort_by_key(|row| row.identity());
        rows
    }
}

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
    /// Cached `org display` details keyed by username, restored from the project state store.
    pub detail_cache: HashMap<String, OrgDetail>,
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
    command: String,
    poll_after: Instant,
    attempts: u32,
    failures: u32,
    /// Extra polls spent waiting for per-class coverage after a terminal outcome.
    coverage_polls: u32,
    /// A terminal report already seen for this run. Late or missing coverage must never replace
    /// a known outcome with "Report unavailable".
    final_run: Option<test_results::TestRun>,
}

#[derive(Debug, Clone)]
pub struct TransferSelection {
    pub item: SuggestionItem,
    pub checked: bool,
}

/// Fine-grained picker state. `searching` is kept in sync so the existing UI readers still work;
/// the additional phases let a later UI distinguish loading from empty and failed searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPhase {
    Idle,
    Indexing,
    Searching,
    NoMatches,
    Failed,
}

/// Which pane of a picker receives list navigation and Space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Input,
    List,
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
    pub phase: SearchPhase,
    pub focus: SearchFocus,
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
            phase: SearchPhase::Idle,
            focus: SearchFocus::Input,
        }
    }

    /// Mark the visible results stale without discarding the user's selection. Results stay on
    /// screen until a fresh reply replaces them atomically; no ranking happens in the key path.
    pub fn mark_edited(&mut self) {
        self.searched_query = None;
        self.searching = true;
        self.phase = SearchPhase::Searching;
    }

    pub fn needs_search(&self) -> bool {
        if self.query.trim().is_empty() {
            return false;
        }
        self.searched_query.as_deref().map(str::trim) != Some(self.query.trim())
    }

    pub fn set_phase(&mut self, phase: SearchPhase) {
        self.phase = phase;
        self.searching = matches!(phase, SearchPhase::Searching | SearchPhase::Indexing);
    }

    pub fn clear_results(&mut self) {
        self.results.clear();
        self.cursor = 0;
        self.searched_query = None;
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.results.len() as isize;
        let current = (self.cursor as isize).clamp(0, len - 1);
        self.cursor = (current + delta).clamp(0, len - 1) as usize;
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

/// One replaceable, debounced request. `ready_at` is a monotonic deadline checked by the tick
/// loop; storing it here keeps every key/render path free of ranking work.
#[derive(Debug)]
struct PendingSearch {
    request: SearchRequest,
    ready_at: Instant,
}

/// Shared scheduler for every picker. Holds one replaceable pending request per picker and a
/// single in-flight ranking operation. Generations decide which reply is current, so a blocking
/// ranking that cannot be stopped by aborting never leaks stale results into a picker.
#[derive(Debug, Default)]
pub struct SearchController {
    generations: HashMap<SearchKind, u64>,
    pending: HashMap<SearchKind, PendingSearch>,
    in_flight: Option<SearchRequest>,
}

impl SearchController {
    pub fn generation(&self, kind: SearchKind) -> u64 {
        self.generations.get(&kind).copied().unwrap_or(0)
    }

    /// Replace the pending request for `kind` and bump its generation. Only bookkeeping happens
    /// here: the actual ranking is started later by `take_due` from the tick loop.
    pub fn schedule(&mut self, mut request: SearchRequest, debounce: Duration) -> u64 {
        let generation = self.generation(request.kind).wrapping_add(1);
        self.generations.insert(request.kind, generation);
        request.generation = generation;
        let ready_at = Instant::now()
            .checked_add(debounce)
            .unwrap_or_else(Instant::now);
        self.pending
            .insert(request.kind, PendingSearch { request, ready_at });
        generation
    }

    /// Drop a pending request (for example when the query is cleared). An already in-flight
    /// ranking keeps running but its reply is discarded by generation.
    pub fn cancel_pending(&mut self, kind: SearchKind) {
        self.pending.remove(&kind);
    }

    pub fn has_pending(&self, kind: SearchKind) -> bool {
        self.pending.contains_key(&kind)
    }

    pub fn is_current(&self, request: &SearchRequest) -> bool {
        self.generation(request.kind) == request.generation
    }

    /// Claim at most one due request. An older operation still in flight keeps this slot busy;
    /// its reply is ignored rather than waited on or forcibly cancelled.
    pub fn take_due(&mut self, now: Instant) -> Option<SearchRequest> {
        if self.in_flight.is_some() {
            return None;
        }
        let kind = self
            .pending
            .iter()
            .filter(|(_, pending)| now >= pending.ready_at)
            .min_by_key(|(_, pending)| pending.ready_at)
            .map(|(kind, _)| *kind)?;
        let pending = self.pending.remove(&kind)?;
        self.in_flight = Some(pending.request.clone());
        Some(pending.request)
    }

    /// Release the single in-flight slot when its operation reports back, current or stale.
    pub fn clear_in_flight(&mut self) {
        self.in_flight = None;
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
    /// Incremented whenever rendered content (lines, result or JSON) changes so the UI can cache
    /// its built line vectors without ever showing stale output.
    pub render_revision: u64,
    /// Set when streamed lines were dropped by the retention caps; the UI shows a faint notice.
    pub lines_truncated: bool,
    /// Typed lifecycle shown in the Jobs window and used for action dispatch.
    pub lifecycle: JobLifecycle,
    /// Stored only for user transfers/Apex runs so `[r]` can enqueue a fresh job with the exact
    /// original arguments without reading the current picker selection.
    pub invocation: Option<InvocationSpec>,
    /// Set when process-tree cleanup failed during a local stop; success is never claimed.
    pub cleanup_error: Option<String>,
}

impl OutputRecord {
    /// Mark rendered content as changed so cached line vectors are rebuilt on the next draw.
    pub(crate) fn bump_render_revision(&mut self) {
        self.render_revision = self.render_revision.wrapping_add(1);
    }

    /// Whether streamed output was dropped by the per-record retention caps.
    pub fn lines_truncated(&self) -> bool {
        self.lines_truncated
    }

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

    /// Human-readable status, preferring the typed lifecycle over exit-code glyphs.
    pub fn status_text(&self) -> &'static str {
        self.lifecycle.status_text()
    }

    /// The remote id for a deploy or Apex run, shown once in the summary detail.
    pub fn remote_id(&self) -> Option<&str> {
        match &self.lifecycle {
            JobLifecycle::DeployPending { job_id, .. } => Some(job_id.as_str()),
            JobLifecycle::ApexPending { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        }
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

/// The state a job presents in the Jobs window. Local children and remote server jobs are
/// distinct: stopping a local process cancels it, while a remote deploy must be cancelled on the
/// server and an Apex run can only have its local polling stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobLifecycle {
    /// Waiting for a free user-job slot; can be dequeued before it ever starts.
    Queued,
    /// A local child process is running.
    Running,
    /// A local cancellation was requested; the child and its descendants are being reaped.
    Stopping,
    /// Deploy accepted by the org; the exact server id is polled until terminal.
    DeployPending {
        job_id: String,
        org: Option<String>,
        cancel_requested: bool,
        report_failed: bool,
    },
    /// Apex run accepted by the org; the run id is polled until terminal.
    ApexPending {
        run_id: String,
        org: Option<String>,
        tracking: bool,
        report_failed: bool,
    },
    /// Removed from the queue or local process stopped before the remote outcome was known.
    Cancelled,
    /// Local work stopped; the org may still be working, so its status is not claimed.
    StoppedLocally { server_status: Option<String> },
    /// A terminal result was recorded.
    Finished,
}

impl JobLifecycle {
    /// Status in words, not only a glyph or colour (also readable via screen readers/NO_COLOR).
    pub fn status_text(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Stopping => "Stopping",
            Self::DeployPending {
                cancel_requested: true,
                ..
            } => "Cancel requested",
            Self::DeployPending {
                report_failed: true,
                ..
            } => "Report unavailable",
            Self::DeployPending { .. } => "Deploy in progress",
            Self::ApexPending {
                report_failed: true,
                ..
            } => "Report unavailable",
            Self::ApexPending {
                tracking: false, ..
            } => "Stopped locally - server status unknown",
            Self::ApexPending { .. } => "Apex tests running",
            Self::Cancelled => "Cancelled",
            Self::StoppedLocally {
                server_status: None,
            } => "Stopped locally - server status unknown",
            Self::StoppedLocally { .. } => "Stopped locally",
            Self::Finished => "Finished",
        }
    }

    /// Whether `[t]` can recover this job by refetching the original remote id.
    pub fn can_retry_report(&self) -> bool {
        matches!(
            self,
            Self::ApexPending {
                tracking: false,
                ..
            } | Self::ApexPending {
                report_failed: true,
                ..
            } | Self::DeployPending {
                report_failed: true,
                ..
            }
        )
    }
}

/// Exact details needed to enqueue a new job from an old one. Only stored for user transfers and
/// Apex runs, never for internal report/cancel/auth/installer or secret commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationSpec {
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub display: String,
    pub purpose: JobPurpose,
    /// Explicitly resolved org, when one was captured at submission time.
    pub org: Option<String>,
    pub working_dir: Option<PathBuf>,
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

/// Which detail view the Jobs window shows for the selected job. Summary is the default; raw
/// output and JSON are on demand so a long command is not repeated in every view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobDetailView {
    Summary,
    Output,
    Json,
}

/// Which pane of the F8 Apex test history modal receives navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestHistoryFocus {
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
            render_revision: 0,
            lines_truncated: false,
            lifecycle: JobLifecycle::Queued,
            invocation: None,
            cleanup_error: None,
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
/// Upper bound on ranked results returned to a picker.
const SEARCH_RESULT_LIMIT: usize = 20;

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
    LoginWeb,
    CreateScratch,
    TargetOrg,
    SwitchOrg,
    OrgExactName,
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

/// The three-step Create wizard plus its two side views. Kept as one enum so Esc and Tab behave
/// predictably and a partially filled draft survives going back a step.
#[derive(Debug, Clone)]
pub enum CreateWizardState {
    /// Step 1: grouped catalog with deferred fuzzy ranking. Enter selects the highlighted row.
    Choose {
        query: String,
        results: Vec<CreateKind>,
        selected: usize,
        searching: bool,
        searched_query: Option<String>,
    },
    /// Step 2: per-type fields plus destination choice (a package or an explicit relative path).
    Configure {
        kind: CreateKind,
        values: BTreeMap<FieldKey, String>,
        focus: usize,
        cursor: usize,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
        error: Option<String>,
        error_field: Option<FieldKey>,
        checking: bool,
    },
    /// Step 3: typed draft with expected paths and the exact argv, checked for collisions.
    Review {
        draft: CreateDraft,
        show_command: bool,
        paths: Vec<PathBuf>,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
    },
    /// A failed job keeps its draft here so `[e] Edit draft` in that job restores every value.
    Failed {
        draft: CreateDraft,
        message: String,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
    },
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help {
        scroll: usize,
    },
    /// Global F7 command catalog: type to filter, arrows to move, Enter to confirm and run.
    Catalog {
        query: String,
        selected: usize,
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
        /// Authoritative selection: a job id, never a row index, so section moves keep it.
        selected_id: Option<u64>,
        focus: JobsFocus,
        scroll: usize,
        follow: bool,
        detail_view: JobDetailView,
    },
    /// Review before enqueuing a rerun. Shows the exact stored arguments, org and working dir.
    RerunReview {
        job_id: u64,
        org: String,
        org_required: bool,
    },
    /// F8 on the Tests screen: two-pane list of saved Apex runs and their full results.
    TestHistory {
        selected: usize,
        list_scroll: usize,
        detail_scroll: usize,
        focus: TestHistoryFocus,
        confirm_clear: bool,
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
    /// Full-screen Create Metadata wizard: Choose type -> Configure -> Review.
    CreateWizard {
        state: CreateWizardState,
    },
    /// F6 org browser. The listing and cache live on `App::org_browse` so they survive key presses.
    OrgBrowse,
}

struct JobEntry {
    job: RunningJob,
    purpose: JobPurpose,
    finished_seen: bool,
    /// Local cancellation requested; the row shows Stopping until the task (and its owned child
    /// tree) has been joined and the user slot is free again.
    stopping: bool,
}

struct PendingJob {
    id: u64,
    executable: PathBuf,
    argv: Vec<String>,
    purpose: JobPurpose,
}

/// Aborts a nested probe task when its owner is cancelled before the task can be joined.
struct AbortOnDrop(Option<JoinHandle<Startup>>);

impl AbortOnDrop {
    fn take(&mut self) -> Option<JoinHandle<Startup>> {
        self.0.take()
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

const MAX_PARALLEL_JOBS: usize = 4;
/// Bounded capacity for the UI event channel. More than enough to absorb a burst of batched
/// stream events while giving the producer backpressure instead of unbounded memory growth.
const EVENT_CHANNEL_CAPACITY: usize = 1_024;

/// How long a notice or error stays visible before the footer returns to its key hints.
const STATUS_TTL: Duration = Duration::from_secs(5);

/// Root application state.
pub struct App {
    pub startup: Startup,
    pub config: Config,
    pub tab: Tab,
    pub startup_stage: StartupStage,
    pub config_dir: PathBuf,
    /// Canonical project root and derived state directory, once a Salesforce project is known.
    pub state_root: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    /// Best-effort caches restored at startup and re-saved when their source data changes.
    pub org_cache: OrgCache,
    pub test_history: TestHistory,
    pub home_state: HomeState,
    pub should_quit: bool,
    pub status: String,
    pub status_error: bool,
    /// When `status` was last set, so notices and errors expire back to the key hints.
    status_set_at: Option<Instant>,
    pub org_state: OrgState,
    pub deploy_state: DeployState,
    pub org_browse: OrgBrowseState,
    pub transfer_search: TransferSearchState,
    /// Action chosen in the confirmation dialog, remembered when it is reopened.
    pub transfer_action: bool,
    pub test_search: TransferSearchState,
    pub output_state: OutputState,
    pub modal: Option<Modal>,
    pub setup_selected: usize,
    tab_history: Vec<Tab>,
    suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    test_suggestion_cache: Option<(PathBuf, Arc<SuggestionSet>)>,
    suggestions_building: bool,
    ticks: usize,
    probe_task: Option<JoinHandle<()>>,
    /// In-flight `sf org list metadata` helper for the F6 browser. Not a user job.
    org_browse_task: Option<JoinHandle<()>>,
    /// Shared debounced scheduler for the Transfer and Tests pickers.
    search: SearchController,
    /// Handle for the single in-flight ranking wrapper, kept only for shutdown cleanup.
    search_task: Option<JoinHandle<()>>,
    /// Static fuzzy index for the Create type picker, ranked on the same blocking worker.
    create_type_set: Arc<SuggestionSet>,
    create_check_task: Option<JoinHandle<()>>,
    create_publish_task: Option<JoinHandle<()>>,
    /// Final paths claimed by queued Create jobs, so two wizards cannot target the same files.
    reserved_create_paths: HashSet<PathBuf>,
    /// Most recent failed Create job and its editable draft keyed by job id.
    create_failed_draft: Option<(u64, CreateDraft)>,
    /// Set while F5 re-scans the project; overlapping refreshes coalesce into this one.
    refreshing_index: bool,
    event_tx: Sender<AppEvent>,
    event_rx: Receiver<AppEvent>,
    jobs: HashMap<u64, JobEntry>,
    pending_jobs: VecDeque<PendingJob>,
    deploy_reports: HashMap<u64, DeployReport>,
    test_reports: HashMap<u64, TestReport>,
    next_job_id: u64,
    /// Rows available in the Jobs detail pane at the last draw, used to clamp scrolling.
    pub(crate) jobs_detail_height: Cell<u16>,
    /// List rows and detail line count recorded for the F8 history modal at the last draw.
    pub(crate) test_history_list_rows: Cell<usize>,
    pub(crate) test_history_detail_lines: Cell<usize>,
    /// Memoized result/detail line vectors, keyed by record revision, width and render flags.
    pub(crate) render_cache: RefCell<RenderCache>,
}

/// Send a UI event from any context without blocking the UI thread. In a runtime the send is
/// awaited on a short-lived task so backpressure is respected; outside one (unit tests) the
/// bounded channel has ample room and `blocking_send` cannot stall an async worker.
fn emit_event(tx: Sender<AppEvent>, event: AppEvent) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = tx.send(event).await;
        });
    } else {
        let _ = tx.blocking_send(event);
    }
}

impl App {
    pub fn new(startup: Startup, config: Config, config_dir: PathBuf) -> Self {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        // Finding the project root is a short upward walk for sfdx-project.json, so the small
        // per-project state files can be read before the probe starts. A cached default org then
        // lets the probe skip `sf config get target-org` entirely.
        let state_root =
            project::find_project(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let (state_dir, org_cache, test_history) = match state_root.as_deref() {
            Some(root) => (
                Some(state_store::state_dir(root, &config_dir)),
                OrgCache::load(root, &config_dir),
                TestHistory::load(root, &config_dir),
            ),
            None => (None, OrgCache::default(), TestHistory::default()),
        };
        let cached_default = org_cache
            .orgs
            .iter()
            .find(|org| org.is_default)
            .map(|org| org.label());
        // The CLI version is machine-wide, so one small file in the config dir covers every
        // project. With both caches warm, startup makes no sf calls at all.
        let version_cache = SfVersionCache::load(&config_dir);
        let cached_version = version_cache
            .is_present()
            .then(|| version_cache.version.clone());
        let mut startup = startup;
        if let Some(org) = cached_default.clone() {
            startup.current_org = Some(org);
        }
        if let Some(version) = cached_version.clone() {
            startup.version = Some(version);
        }
        let initial_orgs = org_cache.orgs.clone();
        let initial_details = org_cache.details.clone();
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
            state_root,
            state_dir,
            org_cache,
            test_history,
            home_state: HomeState::default(),
            should_quit: false,
            status: String::new(),
            status_error: false,
            status_set_at: None,
            org_state: OrgState {
                orgs: initial_orgs,
                detail_cache: initial_details,
                ..OrgState::default()
            },
            deploy_state: DeployState::default(),
            org_browse: OrgBrowseState::default(),
            transfer_search: TransferSearchState::new(true),
            transfer_action: true,
            test_search: TransferSearchState::new(false),
            output_state: OutputState::default(),
            modal: None,
            setup_selected: 0,
            tab_history: Vec::new(),
            suggestion_cache: None,
            test_suggestion_cache: None,
            suggestions_building: false,
            ticks: 0,
            probe_task: None,
            org_browse_task: None,
            search: SearchController::default(),
            search_task: None,
            create_type_set: Arc::new(crate::sf::metadata::create_suggestions()),
            create_check_task: None,
            create_publish_task: None,
            reserved_create_paths: HashSet::new(),
            create_failed_draft: None,
            refreshing_index: false,
            event_tx,
            event_rx,
            jobs: HashMap::new(),
            pending_jobs: VecDeque::new(),
            deploy_reports: HashMap::new(),
            test_reports: HashMap::new(),
            next_job_id: 1,
            jobs_detail_height: Cell::new(10),
            test_history_list_rows: Cell::new(6),
            test_history_detail_lines: Cell::new(10),
            render_cache: RefCell::new(RenderCache::default()),
        };
        app.start_project_scan();
        // With both caches warm the probe makes no sf calls, so don't claim it is probing.
        if cached_default.is_some() && cached_version.is_some() {
            app.notice("Ready — F5 refreshes the CLI and orgs");
        } else {
            app.notice("Starting sftui — the interface is ready while sf is probed");
        }
        app.start_probe(cached_default, cached_version);
        app
    }

    fn start_probe(&mut self, cached_org: Option<String>, cached_version: Option<String>) {
        let config_dir = self.config_dir.clone();
        let app_tx = self.event_tx.clone();
        self.probe_task = Some(tokio::spawn(async move {
            let (probe_tx, mut probe_rx) = mpsc::unbounded_channel::<sf::StartupEvent>();
            // The inner task owns the `sf version`/`sf config get` children. If this outer task is
            // aborted at an await, dropping the guard aborts the inner task and kills its children.
            // Warm org/version caches replace both probes, so no sf call is made for either.
            let mut probe = AbortOnDrop(Some(tokio::spawn(sf::probe_cached(
                config_dir,
                probe_tx,
                None,
                cached_org,
                cached_version,
            ))));
            while let Some(event) = probe_rx.recv().await {
                if app_tx.send(AppEvent::Startup(event)).await.is_err() {
                    break;
                }
            }
            if let Some(task) = probe.take() {
                let _ = task.await;
            }
        }));
    }

    /// Bind a project discovered by the async scan when synchronous discovery found none.
    /// `App::new` normally does this work before the probe; this is the fallback path.
    fn initialize_project_state(&mut self, root: &Path) {
        if self.state_root.is_some() {
            return;
        }
        self.state_root = Some(root.to_path_buf());
        self.state_dir = Some(state_store::state_dir(root, &self.config_dir));
        let cache = OrgCache::load(root, &self.config_dir);
        if self.org_state.orgs.is_empty() {
            self.org_state.orgs = cache.orgs.clone();
        }
        for (username, detail) in cache.details.clone() {
            self.org_state
                .detail_cache
                .entry(username)
                .or_insert(detail);
        }
        if self.startup.current_org.is_none() {
            self.startup.current_org = cache
                .orgs
                .iter()
                .find(|org| org.is_default)
                .map(|org| org.label());
        }
        self.org_cache = cache;
        self.test_history = TestHistory::load(root, &self.config_dir);
    }

    /// Persist the current org list and cached details. Skipped silently outside a project.
    fn save_org_cache(&mut self) {
        let Some(root) = self.state_root.clone() else {
            return;
        };
        let cache = OrgCache {
            orgs: self.org_state.orgs.clone(),
            details: self.org_state.detail_cache.clone(),
            saved_at: state_store::now_ms(),
        };
        self.org_cache = cache.clone();
        let _ = cache.save(&root, &self.config_dir);
    }

    /// Persist a finished Apex run into the current project's capped history.
    fn remember_apex_run(&mut self, run: &test_results::TestRun, org: Option<String>, job_id: u64) {
        let Some(root) = self.state_root.clone() else {
            return;
        };
        let finished_at = state_store::now_ms();
        let id = format!("{job_id}:{finished_at}");
        let command = if run.command.is_empty() {
            format!("sf apex run test — run {}", run.id)
        } else {
            run.command.clone()
        };
        self.test_history.push(SavedTestRun {
            id,
            finished_at,
            org,
            command,
            run: run.clone(),
        });
        let _ = self.test_history.save(&root, &self.config_dir);
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
        // Red and yellow are reserved for error/warning status, so a configured red or yellow
        // accent falls back to cyan to keep those signals unambiguous.
        match self.config.theme.accent.to_ascii_lowercase().as_str() {
            "green" => Color::Green,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            _ => Color::Cyan,
        }
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_error = false;
        self.status_set_at = Some(Instant::now());
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_error = true;
        self.status_set_at = Some(Instant::now());
    }

    /// Clear an expired status so it stops occupying the header (Tiny) or the status footer row
    /// (Compact and Roomy).
    fn expire_status(&mut self, now: Instant) {
        if !self.status.is_empty()
            && self
                .status_set_at
                .is_some_and(|set_at| now.saturating_duration_since(set_at) >= STATUS_TTL)
        {
            self.status.clear();
            self.status_error = false;
            self.status_set_at = None;
        }
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
            let _ = tx.send(event).await;
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
                self.startup.version = Some(version.clone());
                // Every freshly probed version (startup or F5 / Setup re-probe) updates the global
                // cache. Staleness is informational only, so no status text is emitted here.
                let cache = SfVersionCache {
                    version,
                    saved_at: state_store::now_ms(),
                };
                let _ = cache.save(&self.config_dir);
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
                // Org rows are labelled Local/Org only against the current checkout, so refresh
                // that identity set whenever the project scan changes.
                self.org_browse.local_identities =
                    crate::sf::metadata::local_identities(project.as_ref());
                self.deploy_state.project_loading = false;
                self.deploy_state.project = project;
                self.deploy_state.project_error = if missing {
                    error.or(Some(
                        "No sfdx-project.json found above the current directory".to_owned(),
                    ))
                } else {
                    error
                };
                let discovered_root = self
                    .deploy_state
                    .project
                    .as_ref()
                    .map(|project| project.root.clone());
                if let Some(ref root) = discovered_root {
                    self.initialize_project_state(root);
                }
                // An F5 refresh rebuilds the index even when the project root did not change.
                // A stale build event can never replace the current index (see Suggestions).
                let was_refresh = self.refreshing_index;
                self.refreshing_index = false;
                if was_refresh || discovered_root.is_some() {
                    self.ensure_suggestion_sets(was_refresh);
                }
            }
            AppEvent::Suggestions { root, all, tests } => {
                self.suggestions_building = false;
                if root != self.suggestion_root() {
                    // A build from an old project root must never replace the current index.
                    self.ensure_suggestion_sets(false);
                    return;
                }
                self.suggestion_cache = Some((root.clone(), all));
                self.test_suggestion_cache = Some((root, tests));
                // A search waiting on the index now has a usable set. Re-scheduling also advances
                // the generation so replies produced against the old index are discarded.
                let kind = match self.tab {
                    Tab::Tests => Some(SearchKind::Tests),
                    Tab::Retrieve | Tab::Deploy => Some(SearchKind::Transfer),
                    _ => None,
                };
                if let Some(kind) = kind
                    && self.search_state(kind).needs_search()
                {
                    self.queue_search(kind);
                }
            }
            AppEvent::SearchResults(reply) => self.apply_search_reply(reply),
            AppEvent::CreateChecked {
                draft,
                paths,
                collisions,
                error,
            } => {
                let mut review_target = None;
                if let Some(Modal::CreateWizard {
                    state:
                        CreateWizardState::Configure {
                            checking,
                            error: inline,
                            destinations,
                            destination_index,
                            custom_path,
                            ..
                        },
                }) = self.modal.as_mut()
                {
                    *checking = false;
                    let problem = error.or_else(|| {
                        (!collisions.is_empty()).then(|| {
                            format!(
                                "Already exists: {}",
                                truncate(&paths_display(&collisions), 160)
                            )
                        })
                    });
                    if let Some(problem) = problem {
                        *inline = Some(problem);
                    } else {
                        review_target = Some((
                            draft,
                            destinations.clone(),
                            *destination_index,
                            custom_path.clone(),
                        ));
                    }
                }
                let _ = paths;
                if let Some((draft, destinations, destination_index, custom_path)) = review_target {
                    self.goto_create_review(draft, destinations, destination_index, custom_path);
                }
            }
            AppEvent::CreatePublished {
                job_id,
                draft,
                staging,
                report,
            } => self.apply_create_published(job_id, &draft, staging, report),
            AppEvent::OrgMetadata {
                scope,
                username,
                metadata_type,
                generation,
                result,
            } => self.apply_org_metadata(scope, username, metadata_type, generation, result),
            AppEvent::Notice { text, error } => {
                if error {
                    self.error(text);
                } else {
                    self.notice(text);
                }
            }
            AppEvent::Sf(SfEvent::Line(line)) => self.append_stream_lines(std::iter::once(line)),
            AppEvent::Sf(SfEvent::Lines(lines)) => self.append_stream_lines(lines),
            AppEvent::Sf(SfEvent::Finished { job_id, outcome }) => {
                self.finish_job(job_id, outcome);
            }
            AppEvent::Sf(SfEvent::CleanupFailed { job_id, error }) => {
                if let Some(record) = self.output_state.find_mut(job_id) {
                    record.cleanup_error = Some(error.clone());
                    record.bump_render_revision();
                }
                self.error(format!("Job #{job_id} stop cleanup failed: {error}"));
            }
        }
    }

    /// Append streamed lines to each job's record, applying the same per-line caps regardless of
    /// whether the sender delivered one line or a coalesced batch.
    fn append_stream_lines(&mut self, lines: impl IntoIterator<Item = JobLine>) {
        let mut deploy_id = None;
        for line in lines {
            let Some(record) = self.output_state.find_mut(line.job_id) else {
                continue;
            };
            // A labelled deploy id can appear in streamed human output before the CLI exits.
            // Capture it as soon as it is emitted; the structured result is only a fallback.
            if deploy_id.is_none()
                && record.task == TaskKind::Deploy
                && matches!(record.lifecycle, JobLifecycle::Running)
                && let Some(id) = sf::deploy::parse_labelled_deploy_id(&line.text)
            {
                deploy_id = Some((line.job_id, id));
            }
            if is_secret_command(&record.command) {
                // Redacted by design, not truncation: never show a retention notice for it.
                continue;
            }
            if record.lines.len() >= MAX_LINES_PER_RECORD
                || record.line_bytes.saturating_add(line.text.len()) > MAX_RECORD_BYTES
            {
                if !record.lines_truncated {
                    record.lines_truncated = true;
                    record.bump_render_revision();
                }
                continue;
            }
            record.line_bytes = record.line_bytes.saturating_add(line.text.len());
            record.lines.push(line);
            record.bump_render_revision();
        }
        if let Some((job_id, id)) = deploy_id {
            let options = self
                .output_state
                .records
                .iter()
                .find(|record| record.id == job_id)
                .and_then(|record| record.invocation.as_ref())
                .and_then(|spec| match &spec.purpose {
                    JobPurpose::Transfer { options } if !options.retrieve => Some(options.clone()),
                    _ => None,
                });
            if let Some(options) = options
                && !self.deploy_reports.contains_key(&job_id)
            {
                let view = self.register_started_deploy(&options, job_id, &id);
                if let Some(record) = self.output_state.find_mut(job_id) {
                    record.result = Some(view);
                    record.bump_render_revision();
                }
                self.notice(format!("Deploy {id} started - F2 jobs shows progress"));
            }
        }
    }

    /// Seed deploy report polling from a started deploy's job id and return its initial result
    /// view. Both JSON and human-readable `deploy start` output funnel through here.
    fn register_started_deploy(
        &mut self,
        options: &TransferOptions,
        job_id: u64,
        id: &str,
    ) -> ResultView {
        self.deploy_reports.insert(
            job_id,
            DeployReport {
                server_job_id: id.to_owned(),
                options: options.clone(),
                poll_after: Instant::now() + Duration::from_secs(3),
                attempts: 0,
                failures: 0,
            },
        );
        if let Some(record) = self.output_state.find_mut(job_id) {
            record.lifecycle = JobLifecycle::DeployPending {
                job_id: id.to_owned(),
                org: options.target_org.clone(),
                cancel_requested: false,
                report_failed: false,
            };
            record.bump_render_revision();
        }
        ResultView {
            kind: ResultKind::Deploy,
            title: "Deploy".to_owned(),
            status: "Queued".to_owned(),
            ok: true,
            summary: format!("Job {id} started"),
            items: vec![ResultItem {
                ok: true,
                label: "Deploy request accepted".to_owned(),
                detail: id.to_owned(),
                progress: None,
            }],
            coverage: Vec::new(),
            sections: Vec::new(),
        }
    }

    fn finish_job(&mut self, job_id: u64, outcome: ProcessOutcome) {
        if let Some(record) = self.output_state.find_mut(job_id) {
            record.bump_render_revision();
        }
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
        // A locally stopped job never gets its output parsed into a result: its remote outcome is
        // unknown, so claiming success or failure from partial stdout would be misleading.
        if self.jobs.get(&job_id).is_some_and(|entry| entry.stopping) {
            if let Some(record) = self.output_state.find_mut(job_id) {
                record.exit_code = None;
                record.finished = true;
                record.queued = false;
                record.ended_at.get_or_insert_with(Instant::now);
                record.lifecycle = JobLifecycle::StoppedLocally {
                    server_status: None,
                };
                record.bump_render_revision();
            }
            self.notice(format!(
                "Job #{job_id} stopped locally - server status unknown"
            ));
            self.start_pending_jobs();
            return;
        }
        // A deploy cancel helper updates its parent row only; it never becomes a user job row.
        if let JobPurpose::DeployCancel { transfer_job } = &purpose {
            if let Some(record) = self.output_state.find_mut(*transfer_job) {
                if let Some(view) = record.result.as_mut() {
                    view.summary = if outcome.succeeded() {
                        "Cancel requested - waiting for the server".to_owned()
                    } else {
                        "Cancel request failed".to_owned()
                    };
                }
                record.bump_render_revision();
            }
            if !outcome.succeeded() {
                self.error("Deploy cancel request failed - check the deploy id");
            }
            self.start_pending_jobs();
            return;
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
        // A failing Apex run makes `apex get test` exit 100 even though it printed the full report.
        // Only a terminal report for the requested run makes that exit code expected; every other
        // error payload is logged as a redacted diagnostic, never as a test result.
        let apex_report_ok = match (&purpose, parsed.as_ref()) {
            (JobPurpose::TestReport { record_id }, Some(value)) => {
                self.test_reports.get(record_id).is_some_and(|report| {
                    test_results::parse_terminal_report(value, &report.command, &report.run_id)
                        .is_some()
                })
            }
            _ => false,
        };
        if !succeeded && !apex_report_ok {
            let report_diagnostic = matches!(purpose, JobPurpose::TestReport { .. });
            let detail = if secret_output || report_diagnostic {
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
                    self.mark_deploy_report_failed(*transfer_job);
                    self.error("Deploy report failed repeatedly - press [t] to retry");
                }
            }
        }
        let mut result_view: Option<ResultView> = None;
        // Create jobs never present raw CLI JSON as their result: success is measured by the files
        // we actually publish, and failure must keep the draft for `[e] Edit draft`.
        if let JobPurpose::Create {
            draft,
            staging,
            project_root,
            expected,
        } = &purpose
        {
            let draft = draft.clone();
            let staging = staging.clone();
            let project_root = project_root.clone();
            let expected = expected.clone();
            let name = draft.display_name().to_owned();
            let mut view = ResultView {
                kind: ResultKind::Generic,
                title: format!("Create {name}"),
                status: String::new(),
                ok: succeeded,
                summary: String::new(),
                items: Vec::new(),
                coverage: Vec::new(),
                sections: Vec::new(),
            };
            let staging_for_failure = (!succeeded).then(|| staging.clone());
            if succeeded {
                view.status = "Publishing".to_owned();
                view.summary = format!("Generating {name} — publishing local files");
                let tx = self.event_tx.clone();
                let job = draft.clone();
                let publish_from = staging.clone();
                self.create_publish_task = Some(tokio::spawn(async move {
                    let report = tokio::task::spawn_blocking(move || {
                        create::publish(&publish_from, &project_root)
                    })
                    .await
                    .unwrap_or_else(|_| PublishReport {
                        error: Some("publisher task was cancelled".to_owned()),
                        ..PublishReport::default()
                    });
                    let _ = tx
                        .send(AppEvent::CreatePublished {
                            job_id,
                            draft: job,
                            staging,
                            report,
                        })
                        .await;
                }));
            } else {
                let detail = if outcome.stderr.trim().is_empty() {
                    outcome.stdout.trim()
                } else {
                    outcome.stderr.trim()
                };
                view.status = "Failed".to_owned();
                view.summary = format!("Could not create {name}");
                view.items.push(ResultItem {
                    ok: false,
                    label: "CLI error".to_owned(),
                    detail: truncate(detail, 240),
                    progress: None,
                });
                view.items.push(ResultItem {
                    ok: true,
                    label: "Edit draft".to_owned(),
                    detail: "[e] restores every value".to_owned(),
                    progress: None,
                });
                if self
                    .create_failed_draft
                    .as_ref()
                    .is_none_or(|(id, _)| *id <= job_id)
                {
                    self.create_failed_draft = Some((job_id, draft));
                }
                self.error(format!(
                    "Create {name} failed — press e in F2 to edit the draft"
                ));
            }
            if let Some(record) = self.output_state.find_mut(job_id) {
                record.result = Some(view);
                record.bump_render_revision();
            }
            for path in &expected {
                self.reserved_create_paths.remove(path);
            }
            if let Some(path) = staging_for_failure {
                create::discard_staging(&path);
            }
            self.start_pending_jobs();
            return;
        }
        match (purpose.clone(), parsed.clone()) {
            (JobPurpose::OrgList, Some(value)) => {
                self.org_state.orgs = sf::org::parse_org_list(&value);
                self.org_state.loading = false;
                self.org_state.selected = self
                    .org_state
                    .selected
                    .min(self.org_state.orgs.len().saturating_sub(1));
                result_view = Some(generic_result(&purpose_title, &outcome, parsed.as_ref()));
                if succeeded {
                    // A failed list must not overwrite a good cache with an empty one.
                    self.save_org_cache();
                    self.notice(format!(
                        "Loaded {} orgs — F2 jobs shows the result",
                        self.org_state.orgs.len()
                    ));
                }
            }
            (JobPurpose::OrgList, None) => {
                self.org_state.loading = false;
                result_view = Some(generic_result(&purpose_title, &outcome, None));
            }
            (JobPurpose::OrgDetail { username }, Some(value)) => {
                let detail = sf::org::parse_org_display(&value);
                result_view = Some(org_display_result(&username, &detail, succeeded));
                self.org_state.detail = Some(detail.clone());
                self.org_state.detail_for = Some(username.clone());
                self.org_state.detail_cache.insert(username, detail);
                self.save_org_cache();
                if succeeded {
                    self.notice("Org details loaded — F2 jobs shows the result");
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
                        // Newly retrieved files must appear in the Deploy and Apex test pickers.
                        self.rescan_project();
                        self.notice("Retrieve finished — F2 jobs shows the result");
                    }
                } else if let Some(id) = sf::deploy::parse_started_job_id(&value).or_else(|| {
                    if succeeded {
                        sf::deploy::parse_started_job_id_from_text(&outcome.stdout)
                    } else {
                        None
                    }
                }) {
                    result_view = Some(self.register_started_deploy(&options, job_id, &id));
                    if succeeded {
                        self.notice(format!("Deploy {id} started — F2 jobs shows progress"));
                    }
                }
            }
            (JobPurpose::Transfer { options }, None) => {
                if succeeded
                    && let Some(id) = sf::deploy::parse_started_job_id_from_text(&outcome.stdout)
                {
                    result_view = Some(self.register_started_deploy(&options, job_id, &id));
                    self.notice(format!("Deploy {id} started — F2 jobs shows progress"));
                }
            }
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
                    record.bump_render_revision();
                    record.result = Some(view);
                    if complete {
                        record.lifecycle = JobLifecycle::Finished;
                        record.ended_at.get_or_insert_with(Instant::now);
                    }
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
                result_view = Some(redacted_result(&purpose_title, succeeded, &description));
                if succeeded {
                    self.notice(format!("{description} finished — F2 jobs shows the result"));
                }
                self.refresh_orgs();
            }
            (JobPurpose::ApexTests { command, org }, Some(value)) => {
                let run = test_results::parse(&value, &command);
                match apex_report_action(&run) {
                    ApexReportAction::Final => {
                        result_view = Some(apex_result(&run));
                        self.output_state.show_details = false;
                        self.remember_apex_run(&run, org.clone(), job_id);
                        self.keep_jobs_selection_on(job_id);
                        if succeeded {
                            self.notice("Apex test results ready — F2 jobs shows the details");
                        }
                    }
                    ApexReportAction::Poll { run_id } => {
                        result_view = Some(started_test_view(&run_id));
                        if let Some(record) = self.output_state.find_mut(job_id) {
                            record.pending_report = true;
                            record.lifecycle = JobLifecycle::ApexPending {
                                run_id: run_id.clone(),
                                org: org.clone(),
                                tracking: true,
                                report_failed: false,
                            };
                            record.bump_render_revision();
                        }
                        self.test_reports.insert(
                            job_id,
                            TestReport {
                                run_id: run_id.clone(),
                                org,
                                command: command.clone(),
                                poll_after: Instant::now() + Duration::from_secs(3),
                                attempts: 0,
                                failures: 0,
                                coverage_polls: 0,
                                final_run: None,
                            },
                        );
                        self.notice(format!(
                            "Apex test run {run_id} started — polling for results"
                        ));
                    }
                }
            }
            (JobPurpose::TestReport { record_id }, Some(value)) => {
                self.apply_test_report(record_id, &value);
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
            // The final result (including a start-then-poll placeholder) is assigned here, after
            // every earlier revision bump, so the detail cache key must change with it.
            record.bump_render_revision();
        }
        let user_command = matches!(
            purpose,
            JobPurpose::Command { .. } | JobPurpose::Installer { .. }
        );
        if succeeded && user_command {
            self.notice("Done — F2 jobs · Esc back");
        }
        if let Some(record) = self.output_state.find_mut(job_id)
            && matches!(
                record.lifecycle,
                JobLifecycle::Queued | JobLifecycle::Running | JobLifecycle::Stopping
            )
        {
            record.lifecycle = JobLifecycle::Finished;
            record.bump_render_revision();
        }
        self.start_pending_jobs();
    }

    /// Publish finished: show the real written paths, or the exact collision/error with the list
    /// of files already written. A successful scaffold refreshes the shared project index.
    fn apply_create_published(
        &mut self,
        job_id: u64,
        draft: &CreateDraft,
        staging: PathBuf,
        report: PublishReport,
    ) {
        let name = draft.display_name();
        let ok = report.succeeded();
        let items: Vec<ResultItem> = report
            .written
            .iter()
            .map(|path| ResultItem {
                ok: true,
                label: "Created".to_owned(),
                detail: path.to_string_lossy().into_owned(),
                progress: None,
            })
            .chain(report.collisions.iter().map(|path| ResultItem {
                ok: false,
                label: "Not overwritten".to_owned(),
                detail: path.to_string_lossy().into_owned(),
                progress: None,
            }))
            .chain(report.error.iter().map(|error| ResultItem {
                ok: false,
                label: "Filesystem error".to_owned(),
                detail: truncate(error, 240),
                progress: None,
            }))
            .collect();
        let view = ResultView {
            kind: ResultKind::Generic,
            title: format!("Create {name}"),
            status: if ok {
                "Created".to_owned()
            } else {
                "Incomplete".to_owned()
            },
            ok,
            summary: if ok {
                format!("Created {name} — {} file(s)", report.written.len())
            } else {
                format!("Created {name} partially — check the paths below")
            },
            items,
            coverage: Vec::new(),
            sections: Vec::new(),
        };
        if let Some(record) = self.output_state.find_mut(job_id) {
            record.result = Some(view);
            record.bump_render_revision();
        }
        if !ok {
            if self
                .create_failed_draft
                .as_ref()
                .is_none_or(|(id, _)| *id <= job_id)
            {
                self.create_failed_draft = Some((job_id, draft.clone()));
            }
            self.error(format!(
                "Create {name} incomplete — press e in F2 to edit the draft"
            ));
        } else {
            self.notice(format!("Created {name} — F2 jobs shows the paths"));
            self.start_project_scan();
        }
        create::discard_staging(&staging);
    }

    /// Apply one polled `apex get test` response: finish the record or keep polling.
    fn apply_test_report(&mut self, record_id: u64, value: &Value) {
        let Some(report) = self.test_reports.get(&record_id) else {
            return;
        };
        let run_id = report.run_id.clone();
        let command = report.command.clone();
        let coverage_polls = report.coverage_polls;

        // A complete terminal report is authoritative even with exit 100: the CLI uses that code
        // for failing tests. An error envelope or a "Processing" status is not a test result.
        let Some(run) = test_results::parse_terminal_report(value, &command, &run_id) else {
            self.fail_test_report(record_id);
            return;
        };
        let coverage_pending = run.coverages.is_empty() && !run.coverage.is_empty();
        if !coverage_pending || coverage_polls >= 3 {
            self.finish_test_report(record_id, &run);
            return;
        }
        // The outcome is known; keep the report so a later failed or coverage-free poll cannot
        // replace it with "Report unavailable".
        if let Some(report) = self.test_reports.get_mut(&record_id) {
            report.final_run = Some(run.clone());
            report.failures = 0;
            report.coverage_polls = coverage_polls.saturating_add(1);
            report.poll_after = Instant::now() + Duration::from_secs(2);
        }
        // Show progress in the running job while the org finishes aggregating coverage.
        if let Some(record) = self.output_state.find_mut(record_id) {
            if let Some(view) = record.result.as_mut() {
                view.summary = format!("test run {run_id} finished — collecting coverage");
            }
            record.bump_render_revision();
        }
    }

    /// Finish a started test run from a known terminal report, clearing its polling state.
    fn finish_test_report(&mut self, record_id: u64, run: &test_results::TestRun) {
        let view = apex_result(run);
        if let Some(record) = self.output_state.find_mut(record_id) {
            record.result = Some(view);
            record.pending_report = false;
            record.ended_at.get_or_insert_with(Instant::now);
            record.lifecycle = JobLifecycle::Finished;
            // Bump after every mutation so the cached detail cannot survive the new result.
            record.bump_render_revision();
        }
        let org = self
            .test_reports
            .remove(&record_id)
            .and_then(|report| report.org);
        self.remember_apex_run(run, org, record_id);
        self.keep_jobs_selection_on(record_id);
        self.notice("Apex tests finished — F2 jobs");
    }

    /// Count a failed poll and stop after five consecutive failures. When an outcome is already
    /// known, keep it instead of replacing a real result with "Report unavailable".
    fn fail_test_report(&mut self, record_id: u64) {
        let Some(report) = self.test_reports.get_mut(&record_id) else {
            return;
        };
        report.failures = report.failures.saturating_add(1);
        if report.failures < 5 {
            return;
        }
        if let Some(run) = report.final_run.clone() {
            self.finish_test_report(record_id, &run);
        } else {
            self.abandon_test_report(record_id, "could not read the test result");
        }
    }

    /// Stop polling a started test run and mark its row as failed rather than a false success.
    fn abandon_test_report(&mut self, record_id: u64, reason: &str) {
        let remote = self
            .test_reports
            .remove(&record_id)
            .map(|report| (report.run_id, report.org));
        if let Some(record) = self.output_state.find_mut(record_id) {
            record.pending_report = false;
            record.exit_code = Some(1);
            record.ended_at = Some(Instant::now());
            // Keep the original run id/org so `[t]` can retry the report without a new submission.
            record.lifecycle = match remote {
                Some((run_id, org)) => JobLifecycle::ApexPending {
                    run_id,
                    org,
                    tracking: false,
                    report_failed: true,
                },
                None => JobLifecycle::ApexPending {
                    run_id: String::new(),
                    org: None,
                    tracking: false,
                    report_failed: true,
                },
            };
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
            } else {
                // A poll that failed before the placeholder result existed still needs a clear
                // failed result instead of an empty "waiting" row.
                record.result = Some(ResultView {
                    kind: ResultKind::ApexTests,
                    title: "Apex tests".to_owned(),
                    status: "Report unavailable".to_owned(),
                    ok: false,
                    summary: reason.to_owned(),
                    items: vec![ResultItem {
                        ok: false,
                        label: "Test results unavailable".to_owned(),
                        detail: reason.to_owned(),
                        progress: None,
                    }],
                    coverage: Vec::new(),
                    sections: Vec::new(),
                });
            }
            record.bump_render_revision();
        }
        self.keep_jobs_selection_on(record_id);
        self.error(format!("Apex test report stopped: {reason}"));
    }

    pub async fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        self.expire_status(Instant::now());
        if self
            .search_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.search_task.take()
        {
            let _ = task.await;
        }
        self.poll_search_deadlines(Instant::now());
        for task in [&mut self.create_check_task, &mut self.create_publish_task] {
            if task.as_ref().is_some_and(JoinHandle::is_finished)
                && let Some(task) = task.take()
            {
                let _ = task.await;
            }
        }
        if self
            .org_browse_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.org_browse_task.take()
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
                    record.bump_render_revision();
                    record.exit_code = Some(1);
                    record.ended_at = Some(Instant::now());
                    if let Some(view) = record.result.as_mut() {
                        view.ok = false;
                        view.status = "Report unavailable".to_owned();
                        view.summary = "deploy report timed out".to_owned();
                    }
                }
                self.mark_deploy_report_failed(record_id);
                self.error("Deploy report timed out - press [t] to retry");
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
                let known = self
                    .test_reports
                    .get(&record_id)
                    .and_then(|report| report.final_run.clone());
                if let Some(run) = known {
                    // Coverage arrived late or never; the known outcome still stands.
                    self.finish_test_report(record_id, &run);
                } else {
                    self.abandon_test_report(record_id, "timed out waiting for the org");
                }
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
                    | TaskKind::Organization
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
            // Only user transfers and Apex runs are rerunnable: report/cancel/auth/installer and
            // secret commands are deliberately excluded so they can never be replayed with [r].
            if let Some(record) = self.output_state.find_mut(id) {
                record.invocation = invocation_spec(&purpose, &executable, &argv, &command);
            }
            // A recycled record id must not pick up a stale cached line vector.
            self.render_cache.borrow_mut().clear();
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
            record.bump_render_revision();
            record.queued = false;
            record.started_at = Some(Instant::now());
            if matches!(record.lifecycle, JobLifecycle::Queued) {
                record.lifecycle = JobLifecycle::Running;
            }
        }
        let cwd = match &purpose {
            JobPurpose::Create { project_root, .. } => Some(project_root.clone()),
            _ => None,
        };
        match sf::runner::spawn_with_env_in(
            &executable,
            &argv,
            id,
            self.event_tx.clone(),
            AppEvent::Sf,
            sf::runner::sf_env(),
            cwd.as_deref(),
        ) {
            Ok(job) => {
                self.jobs.insert(
                    id,
                    JobEntry {
                        job,
                        purpose,
                        finished_seen: false,
                        stopping: false,
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
            // A queue wait can be long: check the filesystem again the moment the job leaves the
            // queue. A file created meanwhile is never overwritten.
            if let JobPurpose::Create {
                draft,
                staging,
                expected,
                ..
            } = &pending.purpose
            {
                let collisions = create::existing_paths(expected);
                if !collisions.is_empty() {
                    let name = draft.display_name().to_owned();
                    let detail = format!("Already exists: {}", paths_display(&collisions));
                    if let Some(record) = self.output_state.find_mut(pending.id) {
                        record.result = Some(ResultView {
                            kind: ResultKind::Generic,
                            title: format!("Create {name}"),
                            status: "Collision".to_owned(),
                            ok: false,
                            summary: format!("Could not create {name} — nothing was overwritten"),
                            items: collisions
                                .iter()
                                .map(|path| ResultItem {
                                    ok: false,
                                    label: "Already exists".to_owned(),
                                    detail: path.to_string_lossy().into_owned(),
                                    progress: None,
                                })
                                .collect(),
                            coverage: Vec::new(),
                            sections: Vec::new(),
                        });
                        record.lifecycle = JobLifecycle::Finished;
                        record.bump_render_revision();
                    }
                    self.error(truncate(&detail, 180));
                    if self
                        .create_failed_draft
                        .as_ref()
                        .is_none_or(|(id, _)| *id <= pending.id)
                    {
                        self.create_failed_draft = Some((pending.id, draft.clone()));
                    }
                    for path in expected {
                        self.reserved_create_paths.remove(path);
                    }
                    create::discard_staging(staging);
                    continue;
                }
            }
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

    /// The `x` action for the selected job. Behaviour is chosen by the typed lifecycle:
    /// dequeue, local process stop, remote deploy cancel, or stopping Apex tracking.
    fn cancel_job(&mut self, id: u64) -> bool {
        if let Some(record) = self.output_state.find_mut(id) {
            record.bump_render_revision();
        }
        // 1. Queued user job: remove it from the FIFO before it ever starts.
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
                record.lifecycle = JobLifecycle::Cancelled;
                record.bump_render_revision();
            }
            self.notice(format!("Dequeued job #{id} - Cancelled"));
            return true;
        }
        // 2. Apex run: stop local polling only. The org keeps running the tests; keep the run id
        // and org so `[t]` can resume tracking without submitting a new run.
        let poll_jobs: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, entry)| {
                matches!(entry.purpose, JobPurpose::TestReport { record_id } if record_id == id)
            })
            .map(|(job_id, _)| *job_id)
            .collect();
        if let Some(report) = self.test_reports.remove(&id) {
            for job_id in poll_jobs {
                if let Some(entry) = self.jobs.get_mut(&job_id) {
                    entry.job.request_cancel();
                }
            }
            if let Some(record) = self.output_state.find_mut(id) {
                record.finished = true;
                record.pending_report = false;
                record.exit_code = None;
                record.queued = false;
                record.ended_at.get_or_insert_with(Instant::now);
                record.lifecycle = JobLifecycle::ApexPending {
                    run_id: report.run_id,
                    org: report.org,
                    tracking: false,
                    report_failed: false,
                };
                record.bump_render_revision();
            }
            self.notice(format!(
                "Stopped tracking run #{id} - Salesforce may still be running tests; [t] resumes"
            ));
            return true;
        }
        // 3. Deploy with a known server id: cancel the exact remote job, never --use-most-recent.
        let deploy = self
            .output_state
            .records
            .iter()
            .find(|record| record.id == id)
            .and_then(|record| match &record.lifecycle {
                JobLifecycle::DeployPending {
                    job_id,
                    org,
                    cancel_requested,
                    ..
                } => Some((job_id.clone(), org.clone(), *cancel_requested)),
                _ => None,
            });
        if let Some((server_id, org, cancel_requested)) = deploy {
            if cancel_requested {
                self.notice(format!("Cancel already requested for deploy {server_id}"));
                return true;
            }
            let Some(username) = org.filter(|value| !value.is_empty()) else {
                self.error("Deploy org was not captured - choose an org before cancelling");
                return false;
            };
            if let Some(argv) = crate::sf::deploy::deploy_cancel_argv(&server_id, &username) {
                self.spawn(argv, JobPurpose::DeployCancel { transfer_job: id });
            }
            if let Some(record) = self.output_state.find_mut(id)
                && let JobLifecycle::DeployPending {
                    cancel_requested, ..
                } = &mut record.lifecycle
            {
                *cancel_requested = true;
                record.bump_render_revision();
            }
            self.notice(format!("Cancel requested for deploy {server_id}"));
            return true;
        }
        // 4. Local child: request cancellation and show Stopping until the owned process tree is
        // reaped and the user slot is free again.
        let Some(entry) = self.jobs.get_mut(&id) else {
            return false;
        };
        if entry.stopping {
            return true;
        }
        entry.stopping = true;
        entry.job.request_cancel();
        if let Some(record) = self.output_state.find_mut(id) {
            record.lifecycle = JobLifecycle::Stopping;
            record.bump_render_revision();
        }
        self.notice(format!("Stopping job #{id}..."));
        true
    }

    /// Mark a deploy's parent row as report-unavailable while keeping its exact id/org for `[t]`.
    fn mark_deploy_report_failed(&mut self, record_id: u64) {
        if let Some(record) = self.output_state.find_mut(record_id) {
            record.lifecycle = match &record.lifecycle {
                JobLifecycle::DeployPending { job_id, org, .. } => JobLifecycle::DeployPending {
                    job_id: job_id.clone(),
                    org: org.clone(),
                    cancel_requested: false,
                    report_failed: true,
                },
                other => other.clone(),
            };
            record.bump_render_revision();
        }
    }

    /// `[t]`: refetch the original remote id without submitting anything new.
    fn retry_report(&mut self, id: u64) -> bool {
        let details = self
            .output_state
            .records
            .iter()
            .find(|record| record.id == id)
            .and_then(|record| match &record.lifecycle {
                JobLifecycle::ApexPending { run_id, org, .. } if !run_id.is_empty() => {
                    Some(ReportTarget::Apex {
                        run_id: run_id.clone(),
                        org: org.clone(),
                    })
                }
                JobLifecycle::DeployPending { job_id, org, .. } if !job_id.is_empty() => {
                    Some(ReportTarget::Deploy {
                        job_id: job_id.clone(),
                        options: self
                            .deploy_reports
                            .get(&id)
                            .map(|report| report.options.clone())
                            .or_else(|| {
                                self.output_state.records.iter().find_map(|record| {
                                    match &record.invocation {
                                        Some(spec) => match &spec.purpose {
                                            JobPurpose::Transfer { options } => {
                                                Some(options.clone())
                                            }
                                            _ => None,
                                        },
                                        None => None,
                                    }
                                })
                            })
                            .unwrap_or_default(),
                        org: org.clone(),
                    })
                }
                _ => None,
            });
        match details {
            Some(ReportTarget::Apex { run_id, org }) => {
                if self.test_reports.contains_key(&id) {
                    self.notice("Tracking is already active");
                    return true;
                }
                self.test_reports.insert(
                    id,
                    TestReport {
                        run_id: run_id.clone(),
                        org: org.clone(),
                        command: format!("sf apex get test --test-run-id {run_id}"),
                        poll_after: Instant::now(),
                        attempts: 0,
                        failures: 0,
                        coverage_polls: 0,
                        final_run: None,
                    },
                );
                if let Some(record) = self.output_state.find_mut(id)
                    && let JobLifecycle::ApexPending {
                        tracking,
                        report_failed,
                        ..
                    } = &mut record.lifecycle
                {
                    *tracking = true;
                    *report_failed = false;
                    record.bump_render_revision();
                }
                self.spawn(
                    crate::sf::commands::apex_test_result_argv(&run_id, org.as_deref()),
                    JobPurpose::TestReport { record_id: id },
                );
                self.notice(format!("Resumed tracking run {run_id} - no new submission"));
                true
            }
            Some(ReportTarget::Deploy {
                job_id,
                mut options,
                org,
            }) => {
                if self.deploy_reports.contains_key(&id) {
                    self.notice("Deploy polling is already active");
                    return true;
                }
                options.target_org = org.filter(|value| !value.is_empty());
                self.deploy_reports.insert(
                    id,
                    DeployReport {
                        server_job_id: job_id.clone(),
                        options: options.clone(),
                        poll_after: Instant::now(),
                        attempts: 0,
                        failures: 0,
                    },
                );
                if let Some(record) = self.output_state.find_mut(id)
                    && let JobLifecycle::DeployPending { report_failed, .. } = &mut record.lifecycle
                {
                    *report_failed = false;
                    record.bump_render_revision();
                }
                if let Some(argv) = options.report_argv(&job_id) {
                    self.spawn(
                        argv,
                        JobPurpose::Report {
                            options,
                            transfer_job: id,
                        },
                    );
                }
                self.notice(format!("Retrying report for deploy {job_id}"));
                true
            }
            None => false,
        }
    }

    /// `[r]`: enqueue a brand-new job from the stored invocation spec, leaving the old result
    /// untouched. The org is chosen in the review modal, never defaulted silently.
    fn enqueue_rerun(&mut self, job_id: u64, org: Option<String>) -> bool {
        let Some(record) = self.output_state.records.iter().find(|r| r.id == job_id) else {
            return false;
        };
        if record.invocation.is_none() {
            self.error("This job cannot be rerun");
            return false;
        }
        let Some(spec) = record.invocation.clone() else {
            return false;
        };
        if spec.org.is_none() && org.as_deref().unwrap_or("").is_empty() {
            self.error("Choose an org before rerunning");
            return false;
        }
        let mut argv = spec.argv.clone();
        if let Some(org) = org.as_ref().filter(|value| !value.is_empty()) {
            if let Some(position) = argv.iter().position(|part| part == "--target-org") {
                if let Some(value) = argv.get_mut(position + 1) {
                    *value = org.clone();
                }
            } else {
                argv.push("--target-org".to_owned());
                argv.push(org.clone());
            }
        }
        let mut purpose = spec.purpose.clone();
        match &mut purpose {
            JobPurpose::Transfer { options } => {
                // Transfers rerun the same paths; the files on disk are read fresh by the CLI.
                options.target_org = org.filter(|value| !value.is_empty());
            }
            JobPurpose::ApexTests { org: spec_org, .. } => {
                // Exact original classes/options come from the stored purpose; only the reviewed
                // org is updated. The current picker selection is never consulted.
                *spec_org = org.filter(|value| !value.is_empty());
            }
            _ => return false,
        }
        self.spawn_external(spec.executable, argv, purpose);
        true
    }

    /// Cancel every tracked job and helper task. Used during shutdown and by the quit confirmation.
    pub async fn shutdown(&mut self) {
        // Closing the receiver makes any in-flight bounded-channel sends fail immediately with a
        // SendError instead of blocking forever while we await the jobs that are sending them.
        // Dropping their final events during exit is correct; without this, a full channel can
        // deadlock shutdown because the loop no longer drains `event_rx`.
        self.event_rx.close();
        self.pending_jobs.clear();
        // A dropped JoinHandle detaches its task, leaving any in-flight `sf version`/`sf config
        // get` probe running after exit. Abort then await so the runner's kill_on_drop children
        // are torn down before we return.
        if let Some(task) = self.probe_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.search_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.org_browse_task.take() {
            task.abort();
            let _ = task.await;
        }
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
        choose_active_job(
            self.jobs.iter().map(|(id, entry)| {
                (
                    *id,
                    entry.purpose.label(),
                    !entry.job.is_finished(),
                    is_user_purpose(&entry.purpose),
                )
            }),
            !self.pending_jobs.is_empty(),
        )
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
                        self.edit_search(SearchKind::Transfer, String::clear);
                    } else if self.tab == Tab::Tests {
                        self.edit_search(SearchKind::Tests, String::clear);
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
        if key.code == KeyCode::F(7) {
            self.open_catalog();
            return;
        }
        if key.code == KeyCode::F(4) {
            self.open_switch_org();
            return;
        }
        if key.code == KeyCode::F(8) {
            // Apex test history is a Tests-only popup; F8 is inert on every other screen.
            if self.tab == Tab::Tests {
                self.modal = Some(Modal::TestHistory {
                    selected: 0,
                    list_scroll: 0,
                    detail_scroll: 0,
                    focus: TestHistoryFocus::List,
                    confirm_clear: false,
                });
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
            self.ensure_suggestion_sets(false);
        }
        if tab == Tab::Orgs
            && self.startup.installation.is_some()
            && self.org_state.orgs.is_empty()
            && !self.org_state.loading
        {
            self.refresh_orgs();
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
        self.status_set_at = None;
    }

    fn go_home(&mut self) {
        self.set_tab(Tab::Home);
    }

    fn go_back(&mut self) {
        let previous = self.tab_history.pop().unwrap_or(Tab::Home);
        self.tab = previous;
        self.notice(format!("Back to {}", self.tab.label()));
    }

    /// Force a fresh `sf version` probe through the existing background probe task. The resulting
    /// `Version` event re-saves the global cache without blocking the UI thread.
    fn refresh_version(&mut self) {
        if self.probe_task.is_some() {
            self.notice("A probe is already running");
            return;
        }
        self.start_probe(None, None);
    }

    fn refresh_current_tab(&mut self) {
        // F5 is the global refresh: re-probe the CLI version in addition to the tab's own data.
        self.refresh_version();
        match self.tab {
            Tab::Orgs => self.refresh_orgs(),
            Tab::Retrieve | Tab::Deploy => {
                if self.org_browse.has_scope() {
                    self.fetch_org_listing(true);
                }
                self.refresh_index(SearchKind::Transfer);
            }
            Tab::Tests => self.refresh_index(SearchKind::Tests),
            _ => self.notice("Nothing to refresh on this tab"),
        }
    }

    /// Rescan the project and rebuild the index off-thread. The previous index stays installed
    /// until its replacement arrives; overlapping refreshes coalesce into the running scan.
    fn refresh_index(&mut self, kind: SearchKind) {
        if self.refreshing_index {
            self.notice("Index refresh already running");
            return;
        }
        self.refreshing_index = true;
        self.search.cancel_pending(kind);
        {
            let state = self.search_state_mut(kind);
            state.set_phase(SearchPhase::Indexing);
            state.searched_query = None;
        }
        self.start_project_scan();
        self.notice("Refreshing project index…");
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
            emit_event(tx, event);
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
            MenuAction::Metadata => {
                self.ensure_suggestion_sets(false);
                // One mode-neutral screen. The action is chosen in the confirmation dialog.
                self.set_tab(Tab::Retrieve);
                self.notice("Type a component name · Enter chooses Retrieve or Deploy");
            }
            MenuAction::Create => self.open_create_wizard(),
            MenuAction::Tests => {
                self.ensure_suggestion_sets(false);
                self.set_tab(Tab::Tests);
                self.notice("Type a test class name to search");
            }
        }
    }

    fn search_state(&self, kind: SearchKind) -> &TransferSearchState {
        if kind == SearchKind::Tests {
            &self.test_search
        } else {
            &self.transfer_search
        }
    }

    fn search_state_mut(&mut self, kind: SearchKind) -> &mut TransferSearchState {
        if kind == SearchKind::Tests {
            &mut self.test_search
        } else {
            &mut self.transfer_search
        }
    }

    /// Update a picker's query from a keystroke and schedule a debounced search. No ranking or
    /// filesystem scanning happens here, so typing and Backspace stay immediate.
    fn edit_search(&mut self, kind: SearchKind, edit: impl FnOnce(&mut String)) {
        {
            let state = self.search_state_mut(kind);
            edit(&mut state.query);
            state.focus = SearchFocus::Input;
            state.mark_edited();
        }
        self.queue_search(kind);
    }

    /// Replace the picker's pending request. An empty query has nothing to rank, so any pending
    /// work is dropped and old results are cleared instead.
    fn queue_search(&mut self, kind: SearchKind) {
        let query = self.search_state(kind).query.trim().to_owned();
        if query.is_empty() {
            self.search.cancel_pending(kind);
            let state = self.search_state_mut(kind);
            state.clear_results();
            state.set_phase(SearchPhase::Idle);
            return;
        }
        let scope = SearchScope {
            root: self.suggestion_root(),
            retrieve: self.transfer_search.retrieve,
            org: if kind == SearchKind::Tests {
                Some(self.org_target_hint()).filter(|org| !org.is_empty())
            } else {
                None
            },
        };
        let request = {
            let state = self.search_state(kind);
            SearchRequest {
                kind,
                generation: 0,
                scope,
                query,
                cursor_value: state
                    .results
                    .get(state.cursor)
                    .map(|item| item.value.clone()),
                checked: state.checked().into_iter().map(|item| item.value).collect(),
            }
        };
        self.search.schedule(request, SUGGESTION_DEBOUNCE);
    }

    /// The index snapshot a request was built against. A missing or mismatched index must never
    /// silently answer the query with another project's components.
    fn suggestion_set_for(&self, request: &SearchRequest) -> Option<Arc<SuggestionSet>> {
        if request.kind == SearchKind::Create {
            return Some(Arc::clone(&self.create_type_set));
        }
        if request.kind == SearchKind::Tests {
            self.test_suggestion_cache
                .as_ref()
                .filter(|(root, _)| root == &request.scope.root)
                .map(|(_, set)| Arc::clone(set))
        } else {
            self.suggestion_cache
                .as_ref()
                .filter(|(root, _)| root == &request.scope.root)
                .map(|(_, set)| Arc::clone(set))
        }
    }

    /// Start one due ranking operation from the tick loop. The matcher runs on the blocking pool;
    /// key handling and drawing never reach this method.
    fn poll_search_deadlines(&mut self, now: Instant) {
        let Some(request) = self.search.take_due(now) else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.search.clear_in_flight();
            self.apply_search_reply(SearchReply {
                request,
                outcome: Err(SearchError::Unavailable),
            });
            return;
        };
        let set = self.suggestion_set_for(&request);
        let tx = self.event_tx.clone();
        self.search_task = Some(handle.spawn(async move {
            let outcome = match set {
                Some(set) => {
                    let query = request.query.clone();
                    match tokio::task::spawn_blocking(move || {
                        set.filter(&query, SEARCH_RESULT_LIMIT)
                    })
                    .await
                    {
                        Ok(results) => Ok(results),
                        Err(_) => Err(SearchError::Unavailable),
                    }
                }
                None => Err(SearchError::Indexing),
            };
            let _ = tx
                .send(AppEvent::SearchResults(SearchReply { request, outcome }))
                .await;
        }));
    }

    /// Apply a ranking reply, discarding it entirely unless generation, project root, mode and org
    /// still describe the picker. Checked identities are preserved; results never create a check.
    fn apply_search_reply(&mut self, reply: SearchReply) {
        self.search.clear_in_flight();
        let request = reply.request;
        if !self.search.is_current(&request) {
            return;
        }
        if request.kind == SearchKind::Create {
            if let Some(Modal::CreateWizard {
                state:
                    CreateWizardState::Choose {
                        results,
                        selected,
                        searching,
                        searched_query,
                        query,
                    },
            }) = self.modal.as_mut()
            {
                *searching = false;
                if *query == request.query {
                    let hits: Vec<CreateKind> = match reply.outcome {
                        Ok(items) => items
                            .iter()
                            .filter_map(|item| CreateKind::from_label(&item.value))
                            .collect(),
                        Err(_) => create::ALL_TYPES.to_vec(),
                    };
                    *selected = 0;
                    *results = hits;
                    *searched_query = Some(request.query.clone());
                }
            }
            return;
        }
        if request.scope.root != self.suggestion_root()
            || (request.kind == SearchKind::Transfer
                && request.scope.retrieve != self.transfer_search.retrieve)
            || (request.kind == SearchKind::Tests
                && request.scope.org != Some(self.org_target_hint()).filter(|org| !org.is_empty()))
        {
            return;
        }
        let state = self.search_state_mut(request.kind);
        match reply.outcome {
            Ok(mut results) => {
                results.truncate(SEARCH_RESULT_LIMIT);
                let count = results.len();
                // Refresh the snapshot of each previously touched identity from the new results;
                // its checked flag is owned by the picker and survives a re-filter unchanged.
                for entry in &mut state.selected {
                    if let Some(fresh) = results.iter().find(|item| item.value == entry.item.value)
                    {
                        entry.item = fresh.clone();
                    }
                }
                state.cursor = request
                    .cursor_value
                    .as_deref()
                    .and_then(|value| results.iter().position(|item| item.value == value))
                    .unwrap_or(0);
                state.results = results;
                state.searched_query = Some(request.query.clone());
                if count == 0 {
                    state.set_phase(SearchPhase::NoMatches);
                    self.notice("No matches — refine the query");
                } else {
                    state.set_phase(SearchPhase::Idle);
                    self.notice(format!(
                        "{count} matches — Tab results · Space select · Enter confirm"
                    ));
                }
            }
            Err(SearchError::Indexing) => state.set_phase(SearchPhase::Indexing),
            Err(SearchError::Unavailable) => {
                state.clear_results();
                state.set_phase(SearchPhase::Failed);
                self.error("Could not load the component index — press F5 to retry");
            }
        }
    }

    /// Enter in a results picker. An edited query is searched now and focus moves to the results
    /// so Space selects immediately; an up-to-date query opens the normal confirm dialog. Focus is
    /// moved only here, never in the debounced reply path, so a pause mid-query cannot turn the
    /// next Space into a selection.
    fn enter_in_picker(&mut self, kind: SearchKind) {
        if self.search_state(kind).needs_search() {
            self.queue_search(kind);
            self.search_state_mut(kind).focus = SearchFocus::List;
            return;
        }
        match kind {
            SearchKind::Transfer => self.open_transfer_confirm(),
            SearchKind::Tests => self.open_test_confirm(),
            SearchKind::Create => {}
        }
    }

    fn transfer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                self.transfer_search.focus = match self.transfer_search.focus {
                    SearchFocus::Input => SearchFocus::List,
                    SearchFocus::List => SearchFocus::Input,
                };
            }
            KeyCode::Up => {
                self.transfer_search.focus = SearchFocus::List;
                self.transfer_search.move_cursor(-1);
            }
            KeyCode::Down => {
                self.transfer_search.focus = SearchFocus::List;
                self.transfer_search.move_cursor(1);
            }
            KeyCode::Char(' ') if self.transfer_search.focus == SearchFocus::List => {
                self.transfer_search.toggle_result();
            }
            KeyCode::Enter => self.enter_in_picker(SearchKind::Transfer),
            KeyCode::Backspace => self.edit_search(SearchKind::Transfer, |query| {
                query.pop();
            }),
            KeyCode::F(5) => self.refresh_current_tab(),
            KeyCode::F(6) => self.open_org_browse(),
            KeyCode::F(9) => self.open_org_exact_prompt(),
            KeyCode::Esc => self.go_back(),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_search(SearchKind::Transfer, |query| query.push(character));
            }
            _ => {}
        }
    }

    fn open_transfer_confirm(&mut self) {
        let checked = self.transfer_search.checked();
        if checked.is_empty() {
            self.error("Select at least one component — focus results with Tab, then Space");
            return;
        }
        let retrieve = self.transfer_action;
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

    /// Build the index off-thread. `force` is set by F5 so an unchanged project root still gets a
    /// fresh index; the previous index remains installed until its replacement arrives.
    fn ensure_suggestion_sets(&mut self, force: bool) {
        let root = self.suggestion_root();
        if !force
            && self
                .suggestion_cache
                .as_ref()
                .is_some_and(|(cached_root, _)| cached_root == &root)
        {
            return;
        }
        if self.suggestions_building {
            // Coalesce overlapping builds; the running build's event installs the new index (or
            // triggers another build when its root is stale).
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
                    let _ = tx
                        .send(AppEvent::Suggestions {
                            root: cache_root,
                            all,
                            tests,
                        })
                        .await;
                }
                Err(_) => {
                    let _ = tx
                        .send(AppEvent::Notice {
                            text: "Could not build the component index".to_owned(),
                            error: true,
                        })
                        .await;
                }
            }
        });
    }

    fn test_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                self.test_search.focus = match self.test_search.focus {
                    SearchFocus::Input => SearchFocus::List,
                    SearchFocus::List => SearchFocus::Input,
                };
            }
            KeyCode::Up => {
                self.test_search.focus = SearchFocus::List;
                self.test_search.move_cursor(-1);
            }
            KeyCode::Down => {
                self.test_search.focus = SearchFocus::List;
                self.test_search.move_cursor(1);
            }
            KeyCode::Char(' ') if self.test_search.focus == SearchFocus::List => {
                self.test_search.toggle_result();
            }
            KeyCode::Enter => self.enter_in_picker(SearchKind::Tests),
            KeyCode::Backspace => self.edit_search(SearchKind::Tests, |query| {
                query.pop();
            }),
            KeyCode::F(5) => self.refresh_current_tab(),
            KeyCode::Esc => self.go_back(),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_search(SearchKind::Tests, |query| query.push(character));
            }
            _ => {}
        }
    }

    fn open_test_confirm(&mut self) {
        let checked = self.test_search.checked();
        if checked.is_empty() {
            self.error("Select at least one test class — focus results with Tab, then Space");
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

    /// Number of Apex test runs still active: the initial `apex run test` command and its
    /// start-then-poll follow-up share one Tests record, so each run is counted once.
    pub fn active_apex_test_runs(&self) -> usize {
        self.output_state
            .records
            .iter()
            .filter(|record| record.task == TaskKind::Tests && record.is_running())
            .count()
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

    /// Open the bundled command catalog. F7: discover an sf command from any screen without
    /// adding a menu entry or probing the CLI.
    fn open_catalog(&mut self) {
        self.modal = Some(Modal::Catalog {
            query: String::new(),
            selected: 0,
            scroll: 0,
        });
    }

    fn open_jobs(&mut self) {
        let rows = self.job_rows();
        let selected_id = rows
            .iter()
            .find(|row| row.section == JobSection::Running)
            .or_else(|| rows.iter().find(|row| row.section == JobSection::Finished))
            .or_else(|| rows.first())
            .map(|row| row.record.id);
        self.modal = Some(Modal::Jobs {
            selected_id,
            focus: JobsFocus::List,
            scroll: 0,
            follow: true,
            detail_view: JobDetailView::Summary,
        });
    }

    /// Keep an open Jobs window pointing at `record_id` when that record changes section (a
    /// pending Apex run moving to FINISHED reorders `job_rows`). Selection is stored as an id, so
    /// the detail pane follows the same job regardless of its new row position.
    fn keep_jobs_selection_on(&mut self, record_id: u64) {
        let finished = self
            .job_rows()
            .into_iter()
            .find(|row| row.record.id == record_id)
            .is_some_and(|row| row.section == JobSection::Finished);
        if let Some(Modal::Jobs {
            selected_id,
            scroll,
            follow,
            ..
        }) = self.modal.as_mut()
        {
            *selected_id = Some(record_id);
            if finished {
                *scroll = 0;
                *follow = false;
            }
        }
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

    /// Refresh the set of local `Type:fullName` identities used to label org rows.
    fn refresh_local_identities(&mut self) {
        self.org_browse.local_identities =
            crate::sf::metadata::local_identities(self.deploy_state.project.as_ref());
    }

    /// F6 opens the org browser to find components that exist only in the org.
    fn open_org_browse(&mut self) {
        if self.org_state.orgs.is_empty() && !self.org_state.loading {
            self.refresh_orgs();
        }
        self.org_browse.open = true;
        self.org_browse.focus = OrgBrowseFocus::Orgs;
        self.org_browse.org_selected = self
            .org_browse
            .org_selected
            .min(self.org_state.orgs.len().saturating_sub(1));
        self.org_browse.type_selected = self
            .org_browse
            .type_selected
            .min(ORG_BROWSE_TYPES.len().saturating_sub(1));
        self.refresh_local_identities();
        self.modal = Some(Modal::OrgBrowse);
        self.notice("↑↓ org · Tab type · Enter load · Space select · F9 exact · F5 refresh");
    }

    /// Bind the picker to an org/type scope. Changing either clears the previous selection and
    /// reuses a successful cached listing when one exists.
    fn select_org_scope(&mut self, username: String, label: String, metadata_type: String) {
        let scope = OrgBrowseState::scope(&username, &metadata_type);
        if self.org_browse.has_scope() && self.org_browse.current_scope() == scope {
            return;
        }
        self.org_browse.username = username;
        self.org_browse.org_label = label;
        self.org_browse.metadata_type = metadata_type;
        self.org_browse.checked.clear();
        self.org_browse.cursor = 0;
        self.org_browse.error = None;
        self.org_browse.rows = self
            .org_browse
            .cache
            .get(&scope)
            .cloned()
            .unwrap_or_default();
    }

    fn load_org_listing(&mut self, force: bool) {
        let Some(org) = self
            .org_state
            .orgs
            .get(self.org_browse.org_selected)
            .cloned()
        else {
            self.error("No org available — refresh orgs with F5 first");
            return;
        };
        let Some(metadata_type) = ORG_BROWSE_TYPES.get(self.org_browse.type_selected).copied()
        else {
            return;
        };
        // The username goes to the CLI; the alias/label is only used for display.
        let label = org.label();
        self.select_org_scope(org.username, label, metadata_type.to_owned());
        self.fetch_org_listing(force);
    }

    /// Start (or reuse) the background `sf org list metadata` helper for the active scope.
    ///
    /// This is deliberately not a user job: it must not consume a command slot or appear in Jobs.
    fn fetch_org_listing(&mut self, force: bool) {
        if !self.org_browse.has_scope() {
            return;
        }
        let scope = self.org_browse.current_scope();
        if !force && let Some(cached) = self.org_browse.cache.get(&scope).cloned() {
            self.org_browse.rows = cached;
            self.org_browse.loading = false;
            self.notice(format!(
                "{} loaded from this session's cache",
                self.org_browse.metadata_type
            ));
            return;
        }
        let Some(installation) = self.startup.installation.clone() else {
            self.error("Salesforce CLI is not installed");
            return;
        };
        let username = self.org_browse.username.clone();
        let metadata_type = self.org_browse.metadata_type.clone();
        let generation = self.org_browse.generation.wrapping_add(1);
        self.org_browse.generation = generation;
        self.org_browse.loading = true;
        self.org_browse.error = None;
        let command = crate::sf::commands::org_list_metadata(&metadata_type, &username);
        let argv = installation.argv(&command);
        let notice = format!("Listing {metadata_type} from {username}…");
        let tx = self.event_tx.clone();
        if let Some(task) = self.org_browse_task.take() {
            task.abort();
        }
        self.org_browse_task = Some(tokio::spawn(async move {
            let result = match crate::sf::runner::capture(
                &installation.executable,
                &argv,
                Duration::from_secs(60),
            )
            .await
            {
                Ok(outcome) if outcome.succeeded() => {
                    match crate::sf::runner::parse_json_document(&outcome.stdout) {
                        Some(value) => Ok(crate::sf::metadata::parse_org_components(
                            &value,
                            &metadata_type,
                        )),
                        None => Err("could not parse the org metadata response".to_owned()),
                    }
                }
                Ok(outcome) => Err(format!(
                    "sf exited with status {}",
                    outcome.exit_code.unwrap_or(-1)
                )),
                Err(error) => Err(error.to_string()),
            };
            emit_event(
                tx,
                AppEvent::OrgMetadata {
                    scope,
                    username,
                    metadata_type,
                    generation,
                    result,
                },
            );
        }));
        self.notice(notice);
    }

    /// Apply one org-listing reply. A failed refresh never replaces a good cached listing with a
    /// false empty one, and a reply for a scope the user has since left is discarded.
    fn apply_org_metadata(
        &mut self,
        scope: String,
        username: String,
        metadata_type: String,
        generation: u64,
        result: Result<Vec<OrgComponent>, String>,
    ) {
        if generation != self.org_browse.generation
            || !self.org_browse.has_scope()
            || scope != self.org_browse.current_scope()
        {
            return;
        }
        self.org_browse.loading = false;
        match result {
            Ok(components) => {
                self.org_browse.cache.insert(scope, components.clone());
                self.org_browse.rows = components;
                self.org_browse.cursor = self
                    .org_browse
                    .cursor
                    .min(self.org_browse.rows.len().saturating_sub(1));
                self.org_browse.error = None;
                self.notice(format!(
                    "{} {metadata_type} components from {username}",
                    self.org_browse.rows.len()
                ));
            }
            Err(error) => {
                self.org_browse.error = Some(error.clone());
                self.error(format!("Could not list {metadata_type}: {error}"));
            }
        }
    }

    /// F9 adds one exact `Type:FullName`. Bare types, wildcards and blanks are rejected before
    /// anything reaches the CLI, and spaces in the full name stay inside a single argv element.
    fn open_org_exact_prompt(&mut self) {
        if !self.org_browse.has_scope() {
            self.error("Pick an org and a type with F6 first");
            return;
        }
        let current_type = self.org_browse.metadata_type.clone();
        self.open_prompt(
            "Add exact org component",
            PromptKind::OrgExactName,
            vec![PromptField::new(
                "Type:FullName",
                format!("{current_type}:"),
            )],
        );
    }

    /// Confirm an org-only retrieve with the browsed username pinned and exact `--metadata` args.
    fn open_org_confirm(&mut self) {
        let names: Vec<String> = self.org_browse.checked.clone();
        if names.is_empty() {
            self.error("Select at least one org component with Space");
            return;
        }
        let label = self.org_browse.org_label.clone();
        let options = TransferOptions {
            retrieve: true,
            mode: PayloadMode::Metadata,
            metadata_names: names.clone(),
            target_org: Some(self.org_browse.username.clone()),
            ..TransferOptions::default()
        };
        let entries = names
            .into_iter()
            .map(|value| TransferSelection {
                item: SuggestionItem::new(value, format!("org:{label}")),
                checked: true,
            })
            .collect();
        self.modal = Some(Modal::TransferConfirm {
            retrieve: true,
            entries,
            focus: 0,
            show_command: false,
            options,
        });
    }

    /// Rescan the project after a successful retrieve so new files appear in Deploy and Tests.
    /// `refreshing_index` makes the Project event force a fresh suggestion index.
    fn rescan_project(&mut self) {
        self.refreshing_index = true;
        self.start_project_scan();
        self.notice("Project rescanned — new components are ready to use");
    }

    /// Keys for the F6 browser modal. The listing itself lives on `App` so it survives each key.
    fn org_browse_key(&mut self, key: KeyEvent) {
        let mut close = false;
        match key.code {
            KeyCode::Esc | KeyCode::F(6) => close = true,
            KeyCode::F(5) => self.fetch_org_listing(true),
            KeyCode::F(9) => self.open_org_exact_prompt(),
            KeyCode::Tab => {
                self.org_browse.focus = match self.org_browse.focus {
                    OrgBrowseFocus::Orgs => OrgBrowseFocus::Types,
                    OrgBrowseFocus::Types => OrgBrowseFocus::Components,
                    OrgBrowseFocus::Components => OrgBrowseFocus::Orgs,
                };
            }
            KeyCode::Left => {
                self.org_browse.focus = match self.org_browse.focus {
                    OrgBrowseFocus::Types => OrgBrowseFocus::Orgs,
                    OrgBrowseFocus::Components => OrgBrowseFocus::Types,
                    OrgBrowseFocus::Orgs => OrgBrowseFocus::Orgs,
                };
            }
            KeyCode::Right => {
                self.org_browse.focus = match self.org_browse.focus {
                    OrgBrowseFocus::Orgs => OrgBrowseFocus::Types,
                    OrgBrowseFocus::Types => OrgBrowseFocus::Components,
                    OrgBrowseFocus::Components => OrgBrowseFocus::Components,
                };
            }
            KeyCode::Up => match self.org_browse.focus {
                OrgBrowseFocus::Orgs => {
                    self.org_browse.org_selected = self.org_browse.org_selected.saturating_sub(1)
                }
                OrgBrowseFocus::Types => {
                    self.org_browse.type_selected = self.org_browse.type_selected.saturating_sub(1)
                }
                OrgBrowseFocus::Components => {
                    self.org_browse.cursor = self.org_browse.cursor.saturating_sub(1)
                }
            },
            KeyCode::Down => match self.org_browse.focus {
                OrgBrowseFocus::Orgs => {
                    if !self.org_state.orgs.is_empty() {
                        self.org_browse.org_selected =
                            (self.org_browse.org_selected + 1).min(self.org_state.orgs.len() - 1);
                    }
                }
                OrgBrowseFocus::Types => {
                    self.org_browse.type_selected =
                        (self.org_browse.type_selected + 1).min(ORG_BROWSE_TYPES.len() - 1);
                }
                OrgBrowseFocus::Components => {
                    let rows = self.org_browse.visible_rows().len();
                    if rows > 0 {
                        self.org_browse.cursor = (self.org_browse.cursor + 1).min(rows - 1);
                    }
                }
            },
            KeyCode::Char(' ') if self.org_browse.focus == OrgBrowseFocus::Components => {
                if let Some(component) = self.org_browse.visible_rows().get(self.org_browse.cursor)
                {
                    let identity = component.identity();
                    if let Some(position) = self
                        .org_browse
                        .checked
                        .iter()
                        .position(|item| item == &identity)
                    {
                        self.org_browse.checked.remove(position);
                    } else {
                        self.org_browse.checked.push(identity);
                    }
                }
            }
            KeyCode::Enter => match self.org_browse.focus {
                OrgBrowseFocus::Orgs => self.org_browse.focus = OrgBrowseFocus::Types,
                OrgBrowseFocus::Types => self.load_org_listing(false),
                OrgBrowseFocus::Components => self.open_org_confirm(),
            },
            _ => {}
        }
        if close {
            self.org_browse.open = false;
            self.modal = None;
        } else if self.modal.is_none() {
            self.org_browse.open = true;
            self.modal = Some(Modal::OrgBrowse);
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

    /// Keys for the Tests-only F8 history popup. The list keeps ↑↓ selection while the detail
    /// pane scrolls with j/k/PgUp/PgDn/Home/End once focused.
    fn handle_test_history_key(
        &mut self,
        selected: usize,
        list_scroll: usize,
        mut detail_scroll: usize,
        focus: TestHistoryFocus,
        mut confirm_clear: bool,
        key: KeyEvent,
    ) {
        let count = self.test_history.runs.len();
        let mut selected = selected.min(count.saturating_sub(1));
        let mut list_scroll = list_scroll;
        let mut focus = focus;
        let mut close = false;
        match key.code {
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                detail_scroll = 0;
            }
            KeyCode::Down => {
                if count > 0 {
                    selected = (selected + 1).min(count - 1);
                }
                detail_scroll = 0;
            }
            KeyCode::PageUp if focus == TestHistoryFocus::Detail => {
                detail_scroll = detail_scroll.saturating_sub(10);
            }
            KeyCode::PageDown if focus == TestHistoryFocus::Detail => detail_scroll += 10,
            KeyCode::Home if focus == TestHistoryFocus::Detail => detail_scroll = 0,
            KeyCode::End if focus == TestHistoryFocus::Detail => {
                detail_scroll = self.test_history_detail_lines.get().saturating_sub(1);
            }
            KeyCode::PageUp => {
                selected = selected.saturating_sub(10);
                detail_scroll = 0;
            }
            KeyCode::PageDown => {
                if count > 0 {
                    selected = (selected + 10).min(count - 1);
                }
                detail_scroll = 0;
            }
            KeyCode::Home => {
                selected = 0;
                detail_scroll = 0;
            }
            KeyCode::End => {
                selected = count.saturating_sub(1);
                detail_scroll = 0;
            }
            KeyCode::Enter | KeyCode::Tab => {
                focus = match focus {
                    TestHistoryFocus::List => TestHistoryFocus::Detail,
                    TestHistoryFocus::Detail => TestHistoryFocus::List,
                };
            }
            KeyCode::Esc => {
                if focus == TestHistoryFocus::Detail {
                    focus = TestHistoryFocus::List;
                } else {
                    close = true;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(run) = self.test_history.runs.get(selected).cloned() {
                    let root = self.state_root.clone();
                    if self.test_history.remove(&run.id) {
                        if let Some(root) = root {
                            let _ = self.test_history.save(&root, &self.config_dir);
                        }
                        self.notice(format!("Removed saved run {}", run.id));
                    }
                    selected = selected.min(self.test_history.runs.len().saturating_sub(1));
                    detail_scroll = 0;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if confirm_clear {
                    let root = self.state_root.clone();
                    self.test_history.clear();
                    if let Some(root) = root {
                        let _ = self.test_history.save(&root, &self.config_dir);
                    }
                    confirm_clear = false;
                    selected = 0;
                    list_scroll = 0;
                    detail_scroll = 0;
                    self.notice("Apex test history cleared");
                } else {
                    confirm_clear = true;
                    self.notice("Press c again to clear all saved runs");
                }
            }
            KeyCode::Char('j') if focus == TestHistoryFocus::Detail => detail_scroll += 1,
            KeyCode::Char('k') if focus == TestHistoryFocus::Detail => {
                detail_scroll = detail_scroll.saturating_sub(1);
            }
            _ => {}
        }
        if !matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            confirm_clear = false;
        }
        if close {
            self.modal = None;
            return;
        }
        let visible = self.test_history_list_rows.get().max(1);
        if selected < list_scroll {
            list_scroll = selected;
        } else if selected >= list_scroll + visible {
            list_scroll = selected + 1 - visible;
        }
        self.modal = Some(Modal::TestHistory {
            selected,
            list_scroll,
            detail_scroll,
            focus,
            confirm_clear,
        });
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) {
        match modal {
            Modal::CreateWizard { state } => self.handle_create_wizard_key(state, key),
            Modal::OrgBrowse => self.org_browse_key(key),
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
            Modal::Catalog {
                mut query,
                mut selected,
                mut scroll,
            } => {
                let hits = catalog_hits(&query);
                match key.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return;
                    }
                    KeyCode::Enter => {
                        if let Some(command) = hits.get(selected) {
                            let argv = catalog_command_argv(command);
                            let display = redact_command(&format!("sf {}", argv.join(" ")));
                            let warning = if is_secret_command(&display) {
                                "\n\nSecrets are shown as [redacted]; this command may expose credentials, so its output is not retained."
                            } else {
                                ""
                            };
                            self.modal = Some(Modal::Confirm {
                                title: "Run catalog command?".to_owned(),
                                message: format!("{display}{warning}\n\nEnter runs · Esc cancels"),
                                action: PendingAction::RunCommand {
                                    argv,
                                    command: display,
                                },
                            });
                        } else {
                            self.modal = Some(Modal::Catalog {
                                query,
                                selected,
                                scroll,
                            });
                        }
                        return;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                        scroll = 0;
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(hits.len().saturating_sub(1)),
                    KeyCode::PageUp => selected = selected.saturating_sub(CATALOG_VISIBLE_ROWS),
                    KeyCode::PageDown => {
                        selected =
                            (selected + CATALOG_VISIBLE_ROWS).min(hits.len().saturating_sub(1))
                    }
                    KeyCode::Home => {
                        selected = 0;
                        scroll = 0;
                    }
                    KeyCode::End => selected = hits.len().saturating_sub(1),
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(character);
                        selected = 0;
                        scroll = 0;
                    }
                    _ => {}
                }
                scroll = catalog_scroll_for(selected, scroll, hits.len());
                self.modal = Some(Modal::Catalog {
                    query,
                    selected,
                    scroll,
                });
            }
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
                selected_id,
                focus,
                scroll,
                follow,
                detail_view,
            } => self.handle_jobs_key(selected_id, focus, scroll, follow, detail_view, key),
            Modal::RerunReview {
                job_id,
                org,
                org_required,
            } => self.handle_rerun_review_key(job_id, org, org_required, key),
            Modal::TestHistory {
                selected,
                list_scroll,
                detail_scroll,
                focus,
                confirm_clear,
            } => self.handle_test_history_key(
                selected,
                list_scroll,
                detail_scroll,
                focus,
                confirm_clear,
                key,
            ),
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
                        let already_default = org.is_default;
                        if already_default {
                            self.notice(format!("{label} is already the default org"));
                            self.modal = None;
                            return;
                        }
                        // Optimistic header and cache update; the org refresh confirms it and makes
                        // the next launch instant.
                        self.startup.current_org = Some(label.clone());
                        for entry in &mut self.org_state.orgs {
                            entry.is_default = entry.username == username;
                        }
                        self.save_org_cache();
                        self.spawn(
                            crate::sf::commands::config_set("target-org", &username),
                            JobPurpose::OrgAction {
                                description: format!("Default org set to {label}"),
                            },
                        );
                        self.modal = None;
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        let Some(org) = self.org_state.orgs.get(selected) else {
                            self.modal = Some(Modal::SwitchOrg { selected });
                            return;
                        };
                        let username = org.username.clone();
                        if !self.org_state.detail_cache.contains_key(&username) {
                            self.org_state.detail = None;
                            self.org_state.detail_for = None;
                            let argv = crate::sf::commands::org_display(Some(&username));
                            self.spawn(argv, JobPurpose::OrgDetail { username });
                        }
                        self.modal = Some(Modal::SwitchOrg { selected });
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
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.transfer_action = true;
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve: true,
                        entries,
                        focus,
                        show_command,
                        options: TransferOptions {
                            retrieve: true,
                            ..options
                        },
                    });
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    let mut next = retrieve;
                    if let Some(problem) = deploy_problem(&entries) {
                        self.error(problem);
                    } else {
                        self.transfer_action = false;
                        next = false;
                        options.retrieve = false;
                    }
                    self.modal = Some(Modal::TransferConfirm {
                        retrieve: next,
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
                    if !retrieve && let Some(problem) = deploy_problem(&entries) {
                        self.error(problem);
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
        mut selected_id: Option<u64>,
        mut focus: JobsFocus,
        mut scroll: usize,
        mut follow: bool,
        mut detail_view: JobDetailView,
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
            // Authoritative lookup by id, clamped to a real row when a selection disappears.
            let mut selected = selected_id
                .and_then(|id| rows.iter().position(|row| row.record.id == id))
                .unwrap_or(0);
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
                crate::ui::job_detail_len(self, record, queued_position, detail_view)
            } else {
                0
            };
            let height = usize::from(self.jobs_detail_height.get()).max(1);
            let max_scroll = content_len.saturating_sub(height);
            let record_id = record.id;
            let record_finished = record.finished;
            let can_rerun = record.invocation.is_some()
                && !matches!(
                    record.lifecycle,
                    JobLifecycle::DeployPending { .. }
                        | JobLifecycle::ApexPending { .. }
                        | JobLifecycle::Running
                        | JobLifecycle::Stopping
                );
            let can_retry = record.lifecycle.can_retry_report();
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
                    // Enter on a narrow screen opens the selected job's detail; Tab only moves focus.
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
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if self
                        .create_failed_draft
                        .as_ref()
                        .is_some_and(|(id, _)| *id == record_id)
                        && let Some((_, draft)) = self.create_failed_draft.take()
                    {
                        let destinations = vec![draft.destination.clone()];
                        self.modal = Some(Modal::CreateWizard {
                            state: Self::create_configure_from_draft(
                                draft,
                                Some("Edit the draft, then press Enter to review again".to_owned()),
                                destinations,
                                0,
                                String::new(),
                            ),
                        });
                        close = true;
                    } else {
                        self.notice("That job has no editable Create draft");
                    }
                }
                KeyCode::Char('x') => {
                    if self.cancel_job(record_id) {
                        reset_view = true;
                    } else if record_finished {
                        self.notice("That job already finished");
                    }
                }
                KeyCode::Char('t') if can_retry => {
                    if self.retry_report(record_id) {
                        close = true;
                    }
                }
                KeyCode::Char('r') if can_rerun => {
                    let org = self
                        .output_state
                        .records
                        .iter()
                        .find(|record| record.id == record_id)
                        .and_then(|record| record.invocation.as_ref())
                        .map(|spec| spec.org.clone().unwrap_or_default())
                        .unwrap_or_default();
                    let org_required = self
                        .output_state
                        .records
                        .iter()
                        .find(|record| record.id == record_id)
                        .and_then(|record| record.invocation.as_ref())
                        .is_some_and(|spec| spec.org.is_none());
                    self.modal = Some(Modal::RerunReview {
                        job_id: record_id,
                        org,
                        org_required,
                    });
                    return;
                }
                KeyCode::Char('c') => {
                    self.clear_finished_jobs();
                    reset_view = true;
                }
                KeyCode::Char('s') | KeyCode::Char('o') | KeyCode::Char('z') => {
                    detail_view = match key.code {
                        KeyCode::Char('o') => JobDetailView::Output,
                        KeyCode::Char('z') => JobDetailView::Json,
                        _ => JobDetailView::Summary,
                    };
                    focus = JobsFocus::Detail;
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
            // Convert the working row index back to the stable job id.
            let rows = self.job_rows();
            selected_id = rows.get(selected).map(|row| row.record.id);
        }
        if close {
            self.modal = None;
            return;
        }
        let rows = self.job_rows();
        let selected = selected_id
            .and_then(|id| rows.iter().position(|row| row.record.id == id))
            .unwrap_or(0);
        if reset_view && !rows.is_empty() {
            scroll = 0;
            follow = rows[selected].section != JobSection::Finished;
        }
        drop(rows);
        self.modal = Some(Modal::Jobs {
            selected_id,
            focus,
            scroll,
            follow,
            detail_view,
        });
    }

    /// Rerun review: the org must be resolved before a new job is enqueued. No current picker
    /// selection is read here; the stored invocation spec supplies the exact arguments.
    fn handle_rerun_review_key(
        &mut self,
        job_id: u64,
        mut org: String,
        org_required: bool,
        key: KeyEvent,
    ) {
        match key.code {
            KeyCode::Esc => {
                self.modal = Some(Modal::Jobs {
                    selected_id: Some(job_id),
                    focus: JobsFocus::List,
                    scroll: 0,
                    follow: false,
                    detail_view: JobDetailView::Summary,
                });
            }
            KeyCode::Enter => {
                let chosen = if org.trim().is_empty() {
                    None
                } else {
                    Some(org.trim().to_owned())
                };
                if org_required && chosen.is_none() {
                    self.error("Enter the org to rerun against");
                    self.modal = Some(Modal::RerunReview {
                        job_id,
                        org,
                        org_required,
                    });
                    return;
                }
                if self.enqueue_rerun(job_id, chosen) {
                    self.modal = Some(Modal::Jobs {
                        selected_id: Some(job_id),
                        focus: JobsFocus::List,
                        scroll: 0,
                        follow: false,
                        detail_view: JobDetailView::Summary,
                    });
                } else {
                    self.modal = Some(Modal::RerunReview {
                        job_id,
                        org,
                        org_required,
                    });
                }
            }
            KeyCode::Backspace => {
                org.pop();
                self.modal = Some(Modal::RerunReview {
                    job_id,
                    org,
                    org_required,
                });
            }
            KeyCode::Char(character) => {
                if character != ' ' || !org.is_empty() {
                    org.push(character);
                }
                self.modal = Some(Modal::RerunReview {
                    job_id,
                    org,
                    org_required,
                });
            }
            _ => {
                self.modal = Some(Modal::RerunReview {
                    job_id,
                    org,
                    org_required,
                });
            }
        }
    }

    // ----- Create Metadata wizard ----------------------------------------------------

    fn open_create_wizard(&mut self) {
        self.search.cancel_pending(SearchKind::Create);
        self.modal = Some(Modal::CreateWizard {
            state: CreateWizardState::Choose {
                query: String::new(),
                results: create::ALL_TYPES.to_vec(),
                selected: 0,
                searching: false,
                searched_query: Some(String::new()),
            },
        });
        self.notice("Choose a metadata type · type to filter · Enter selects");
    }

    /// Schedule ranking on the shared controller; no matcher runs in a key handler or render.
    fn queue_create_type_search(&mut self, query: String) {
        self.search.schedule(
            SearchRequest {
                kind: SearchKind::Create,
                generation: 0,
                scope: SearchScope {
                    root: PathBuf::new(),
                    retrieve: false,
                    org: None,
                },
                query,
                cursor_value: None,
                checked: Vec::new(),
            },
            SUGGESTION_DEBOUNCE,
        );
    }

    fn handle_create_wizard_key(&mut self, state: CreateWizardState, key: KeyEvent) {
        match state {
            CreateWizardState::Choose {
                query,
                results,
                selected,
                searching,
                searched_query,
            } => self.create_choose_key(query, results, selected, searching, searched_query, key),
            CreateWizardState::Configure {
                kind,
                values,
                focus,
                cursor,
                destinations,
                destination_index,
                custom_path,
                error,
                error_field,
                checking,
            } => self.create_configure_key(
                kind,
                values,
                focus,
                cursor,
                destinations,
                destination_index,
                custom_path,
                error,
                error_field,
                checking,
                key,
            ),
            CreateWizardState::Review {
                draft,
                show_command,
                paths,
                destinations,
                destination_index,
                custom_path,
            } => self.create_review_key(
                draft,
                show_command,
                paths,
                destinations,
                destination_index,
                custom_path,
                key,
            ),
            CreateWizardState::Failed {
                draft,
                message,
                destinations,
                destination_index,
                custom_path,
            } => match key.code {
                KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Enter => self.edit_create_draft(
                    draft,
                    message,
                    destinations,
                    destination_index,
                    custom_path,
                ),
                _ => self.modal = None,
            },
        }
    }

    fn create_choose_key(
        &mut self,
        mut query: String,
        results: Vec<CreateKind>,
        mut selected: usize,
        searching: bool,
        searched_query: Option<String>,
        key: KeyEvent,
    ) {
        let count = results.len();
        let dirty = searched_query.as_deref() != Some(query.as_str());
        match key.code {
            KeyCode::Esc => {
                self.search.cancel_pending(SearchKind::Create);
                self.modal = None;
                return;
            }
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down if count > 0 => selected = (selected + 1).min(count - 1),
            KeyCode::PageUp => selected = selected.saturating_sub(8),
            KeyCode::PageDown if count > 0 => selected = (selected + 8).min(count - 1),
            KeyCode::Home => selected = 0,
            KeyCode::End if count > 0 => selected = count - 1,
            KeyCode::Backspace => {
                query.pop();
                self.queue_create_type_search(query.clone());
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.clear();
                self.queue_create_type_search(query.clone());
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(character);
                self.queue_create_type_search(query.clone());
            }
            KeyCode::Enter => {
                // A fuzzy hit never auto-submits: Enter always selects the highlighted row.
                let kind = match results.get(selected).copied() {
                    Some(kind) => kind,
                    None => {
                        self.error("No matching metadata type");
                        self.modal = Some(Modal::CreateWizard {
                            state: CreateWizardState::Choose {
                                query,
                                results,
                                selected,
                                searching,
                                searched_query,
                            },
                        });
                        return;
                    }
                };
                if results.is_empty() {
                    self.modal = Some(Modal::CreateWizard {
                        state: CreateWizardState::Choose {
                            query,
                            results,
                            selected,
                            searching,
                            searched_query,
                        },
                    });
                    return;
                }
                self.search.cancel_pending(SearchKind::Create);
                self.start_create_configure(kind, query, results, selected);
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::CreateWizard {
            state: CreateWizardState::Choose {
                query,
                results,
                selected,
                searching: searching || dirty,
                searched_query,
            },
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn create_configure_key(
        &mut self,
        kind: CreateKind,
        mut values: BTreeMap<FieldKey, String>,
        mut focus: usize,
        mut cursor: usize,
        destinations: Vec<Destination>,
        mut destination_index: usize,
        mut custom_path: String,
        mut error: Option<String>,
        mut error_field: Option<FieldKey>,
        mut checking: bool,
        key: KeyEvent,
    ) {
        let fields = create::fields_for(kind);
        let other_index = destinations.len();
        let rows = fields.len() + 1 + usize::from(destination_index == other_index);
        match key.code {
            KeyCode::Esc => {
                self.search.cancel_pending(SearchKind::Create);
                self.modal = Some(Modal::CreateWizard {
                    state: CreateWizardState::Choose {
                        query: String::new(),
                        results: create::ALL_TYPES.to_vec(),
                        selected: create::ALL_TYPES
                            .iter()
                            .position(|item| *item == kind)
                            .unwrap_or(0),
                        searching: false,
                        searched_query: Some(String::new()),
                    },
                });
                return;
            }
            KeyCode::Tab => {
                focus = (focus + 1).rem_euclid(rows);
                cursor = Self::create_text_end(&values, &fields, focus, &custom_path);
            }
            KeyCode::BackTab => {
                focus = (focus + rows - 1).rem_euclid(rows);
                cursor = Self::create_text_end(&values, &fields, focus, &custom_path);
            }
            KeyCode::Up | KeyCode::Down => {
                if focus == fields.len() {
                    let delta = if key.code == KeyCode::Down {
                        1
                    } else {
                        destinations.len() - 1
                    };
                    destination_index =
                        (destination_index + delta).rem_euclid(destinations.len() + 1);
                } else {
                    let delta = if key.code == KeyCode::Down {
                        1
                    } else {
                        rows - 1
                    };
                    focus = (focus + delta).rem_euclid(rows);
                }
                cursor = Self::create_text_end(&values, &fields, focus, &custom_path);
            }
            KeyCode::Left | KeyCode::Right if focus == fields.len() => {
                let delta = if key.code == KeyCode::Right {
                    1
                } else {
                    destinations.len()
                };
                destination_index = (destination_index + delta).rem_euclid(destinations.len() + 1);
                cursor = 0;
            }
            KeyCode::Left if focus < rows => {
                cursor = previous_boundary(
                    Self::create_text(&values, &fields, focus, &custom_path),
                    cursor,
                );
            }
            KeyCode::Right if focus < rows => {
                let text = Self::create_text(&values, &fields, focus, &custom_path).to_owned();
                cursor = next_boundary(&text, cursor);
            }
            KeyCode::Backspace if focus < rows => {
                let text = if focus < fields.len() {
                    values.get_mut(&fields[focus].key)
                } else {
                    Some(&mut custom_path)
                };
                if let Some(text) = text {
                    let start = previous_boundary(text, cursor);
                    if start < cursor {
                        text.replace_range(start..cursor, "");
                        cursor = start;
                    }
                }
                error = None;
                error_field = None;
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && focus < rows =>
            {
                // The package choices change with arrows/Tab; the "other path" row is free text.
                if focus == fields.len() && destination_index != other_index {
                    destination_index = other_index;
                }
                if focus != fields.len() || destination_index == other_index {
                    let text = if focus < fields.len() {
                        values.get_mut(&fields[focus].key)
                    } else {
                        Some(&mut custom_path)
                    };
                    if let Some(text) = text {
                        cursor = floor_boundary(text, cursor);
                        text.insert(cursor, character);
                        cursor += character.len_utf8();
                    }
                    error = None;
                    error_field = None;
                }
            }
            KeyCode::Enter if !checking => {
                let mut checked_draft = None;
                match Self::create_draft_from_configure(
                    kind,
                    &values,
                    &destinations,
                    destination_index,
                    &custom_path,
                ) {
                    Ok(draft) => checked_draft = Some(draft),
                    Err(problem) => error = Some(problem),
                }
                if let Some(draft) = checked_draft {
                    match draft.validation_error() {
                        Some((key, problem)) => {
                            error = Some(problem);
                            error_field = Some(key);
                        }
                        None if kind.interactive_only() => {
                            self.goto_create_review(
                                draft,
                                destinations,
                                destination_index,
                                custom_path,
                            );
                            return;
                        }
                        None => {
                            checking = true;
                            error = None;
                            self.queue_create_check(&draft);
                        }
                    }
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::CreateWizard {
            state: CreateWizardState::Configure {
                kind,
                values,
                focus,
                cursor,
                destinations,
                destination_index,
                custom_path,
                error,
                error_field,
                checking,
            },
        });
    }

    /// Text of the focused Configure row, used only for cursor movement.
    fn create_text<'a>(
        values: &'a BTreeMap<FieldKey, String>,
        fields: &[FieldSpec],
        focus: usize,
        custom_path: &'a str,
    ) -> &'a str {
        if focus < fields.len() {
            values.get(&fields[focus].key).map_or("", String::as_str)
        } else {
            custom_path
        }
    }

    fn create_text_end(
        values: &BTreeMap<FieldKey, String>,
        fields: &[FieldSpec],
        focus: usize,
        custom_path: &str,
    ) -> usize {
        Self::create_text(values, fields, focus, custom_path).len()
    }

    fn create_draft_from_configure(
        kind: CreateKind,
        values: &BTreeMap<FieldKey, String>,
        destinations: &[Destination],
        destination_index: usize,
        custom_path: &str,
    ) -> Result<CreateDraft, String> {
        let destination = if let Some(package) = destinations.get(destination_index) {
            package.clone()
        } else {
            let relative = custom_path.trim();
            if relative.is_empty() {
                return Err("Enter a project-relative path, e.g. force-app/main/default".to_owned());
            }
            Destination {
                label: relative.to_owned(),
                relative: PathBuf::from(relative),
            }
        };
        let mut draft = CreateDraft::new(kind, destination);
        for (key, value) in values {
            draft.set(*key, value.clone());
        }
        Ok(draft)
    }

    fn create_configure_from_draft(
        draft: CreateDraft,
        error: Option<String>,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
    ) -> CreateWizardState {
        let fields = create::fields_for(draft.kind);
        let values = fields
            .iter()
            .map(|field| (field.key, draft.get(field.key).to_owned()))
            .collect();
        CreateWizardState::Configure {
            kind: draft.kind,
            values,
            focus: 0,
            cursor: 0,
            destinations,
            destination_index,
            custom_path,
            error,
            error_field: None,
            checking: false,
        }
    }

    fn goto_create_review(
        &mut self,
        draft: CreateDraft,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
    ) {
        let paths = if draft.kind.interactive_only() {
            Vec::new()
        } else {
            self.state_root
                .as_deref()
                .and_then(|root| create::expected_paths(&draft, root).ok())
                .unwrap_or_default()
        };
        self.modal = Some(Modal::CreateWizard {
            state: CreateWizardState::Review {
                draft,
                show_command: false,
                paths,
                destinations,
                destination_index,
                custom_path,
            },
        });
    }

    fn start_create_configure(
        &mut self,
        kind: CreateKind,
        _query: String,
        _results: Vec<CreateKind>,
        _selected: usize,
    ) {
        let options = create::destination_options(self.state_root.as_deref());
        let destination_index = usize::from(options.choices.is_empty());
        let error = if options.choices.is_empty() {
            Some(options.note.clone())
        } else {
            None
        };
        let values = create::fields_for(kind)
            .iter()
            .map(|field| (field.key, create::default_for(field.key).to_owned()))
            .collect();
        self.modal = Some(Modal::CreateWizard {
            state: CreateWizardState::Configure {
                kind,
                values,
                focus: 0,
                cursor: 0,
                destinations: options.choices,
                destination_index,
                custom_path: String::new(),
                error,
                error_field: None,
                checking: false,
            },
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn create_review_key(
        &mut self,
        draft: CreateDraft,
        show_command: bool,
        paths: Vec<PathBuf>,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
        key: KeyEvent,
    ) {
        match key.code {
            KeyCode::Esc => {
                self.modal = Some(Modal::CreateWizard {
                    state: Self::create_configure_from_draft(
                        draft,
                        None,
                        destinations,
                        destination_index,
                        custom_path,
                    ),
                })
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.modal = Some(Modal::CreateWizard {
                    state: CreateWizardState::Review {
                        draft,
                        show_command: !show_command,
                        paths,
                        destinations,
                        destination_index,
                        custom_path,
                    },
                })
            }
            KeyCode::Enter => {
                if draft.kind.interactive_only() {
                    self.copy_create_command(&draft);
                } else {
                    self.enqueue_create_job(draft.clone());
                }
                self.modal = Some(Modal::CreateWizard {
                    state: CreateWizardState::Review {
                        draft,
                        show_command,
                        paths,
                        destinations,
                        destination_index,
                        custom_path,
                    },
                });
            }
            _ => {
                self.modal = Some(Modal::CreateWizard {
                    state: CreateWizardState::Review {
                        draft,
                        show_command,
                        paths,
                        destinations,
                        destination_index,
                        custom_path,
                    },
                })
            }
        }
    }

    fn edit_create_draft(
        &mut self,
        draft: CreateDraft,
        message: String,
        destinations: Vec<Destination>,
        destination_index: usize,
        custom_path: String,
    ) {
        self.modal = Some(Modal::CreateWizard {
            state: Self::create_configure_from_draft(
                draft,
                Some(message),
                destinations,
                destination_index,
                custom_path,
            ),
        });
    }

    /// Pre-flight collision check runs on the blocking pool, never in a key handler or draw.
    fn queue_create_check(&mut self, draft: &CreateDraft) {
        let Some(root) = self.state_root.clone() else {
            self.error("No Salesforce project is open — nothing is generated in sftui's directory");
            return;
        };
        let paths = match create::expected_paths(draft, &root) {
            Ok(paths) => paths,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        let reserved = self.reserved_create_paths.clone();
        let busy: Vec<PathBuf> = paths
            .iter()
            .filter(|path| reserved.contains(*path))
            .cloned()
            .collect();
        if !busy.is_empty() {
            self.error(format!(
                "Another queued Create job targets: {}",
                truncate(&paths_display(&busy), 140)
            ));
            return;
        }
        let tx = self.event_tx.clone();
        let job = draft.clone();
        self.create_check_task = Some(tokio::spawn(async move {
            let collision_paths = paths.clone();
            let collisions =
                tokio::task::spawn_blocking(move || create::existing_paths(&collision_paths))
                    .await
                    .unwrap_or_default();
            let _ = tx
                .send(AppEvent::CreateChecked {
                    draft: job,
                    paths,
                    collisions,
                    error: None,
                })
                .await;
        }));
    }

    /// Create a typed job. Final paths are reserved so a second queued wizard cannot claim them.
    fn enqueue_create_job(&mut self, draft: CreateDraft) {
        if draft.kind.interactive_only() {
            // Custom Object generation is interactive in the sf CLI. Never spawn it as a job.
            self.copy_create_command(&draft);
            return;
        }
        let Some(root) = self.state_root.clone() else {
            self.error("No Salesforce project is open — open one before creating metadata");
            return;
        };
        let expected = match create::expected_paths(&draft, &root) {
            Ok(paths) => paths,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        let collisions = create::existing_paths(&expected);
        if !collisions.is_empty() {
            self.error(format!(
                "Already exists: {}",
                truncate(&paths_display(&collisions), 160)
            ));
            return;
        }
        if expected
            .iter()
            .any(|path| self.reserved_create_paths.contains(path))
        {
            self.error("Another queued Create job already targets one of these paths");
            return;
        }
        let staging = match create::create_staging(&root, self.next_job_id) {
            Ok(path) => path,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        let argv = crate::sf::commands::create_metadata_argv(&draft, &staging);
        let display = crate::sf::commands::create_metadata_display(&argv);
        for path in &expected {
            self.reserved_create_paths.insert(path.clone());
        }
        let name = draft.display_name().to_owned();
        let purpose = JobPurpose::Create {
            draft,
            staging,
            project_root: root,
            expected,
        };
        self.spawn(argv, purpose);
        self.notice(format!("Creating {name} — {display}"));
    }

    fn copy_create_command(&mut self, draft: &CreateDraft) {
        let dir = self.state_root.as_deref().map_or_else(
            || "(open a Salesforce project first)".to_owned(),
            |root| root.display().to_string(),
        );
        let argv = crate::sf::commands::create_metadata_argv(draft, Path::new("."));
        let text = format!(
            "cd {dir}\n{}\n\nThis command is interactive; sftui does not run it for you.",
            crate::sf::commands::create_metadata_display(&argv)
        );
        self.copy_text(text);
    }

    fn copy_text(&mut self, text: String) {
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
            emit_event(tx, event);
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || send_result(clipboard::copy(&text)));
        } else {
            send_result(clipboard::copy(&text));
        }
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
                if !retrieve {
                    argv.push("--ignore-conflicts".to_owned());
                }
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
            PromptKind::OrgExactName => {
                if !self.org_browse.has_scope() {
                    self.error("Pick an org and a type with F6 first");
                    return false;
                }
                let raw = values.first().map(String::as_str).unwrap_or("").trim();
                let Some((kind, name)) = raw.split_once(':') else {
                    self.error("Use Type:FullName, for example ApexClass:MyClass");
                    return false;
                };
                let (kind, name) = (kind.trim(), name.trim());
                let Some(canonical) = ORG_BROWSE_TYPES
                    .iter()
                    .find(|candidate| candidate.eq_ignore_ascii_case(kind))
                else {
                    self.error("Unsupported metadata type for an exact name");
                    return false;
                };
                if name.is_empty() {
                    self.error("The full name cannot be blank");
                    return false;
                }
                if name.contains(['*', '%', '?']) {
                    self.error("Wildcards are not allowed for an exact name");
                    return false;
                }
                let component = OrgComponent {
                    metadata_type: (*canonical).to_owned(),
                    full_name: name.to_owned(),
                };
                let identity = component.identity();
                let scope = self.org_browse.current_scope();
                let exact = self.org_browse.exact_rows.entry(scope).or_default();
                if !exact.iter().any(|row| row.identity() == identity) {
                    exact.push(component);
                }
                if !self.org_browse.checked.contains(&identity) {
                    self.org_browse.checked.push(identity.clone());
                }
                self.org_browse.focus = OrgBrowseFocus::Components;
                self.modal = Some(Modal::OrgBrowse);
                self.notice(format!("Added {identity}"));
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
        // An explicit re-probe always asks sf, even when the org/version caches are warm.
        self.start_probe(None, None);
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

/// Rows shown in the catalog list at once. Shared with the renderer so keyboard scrolling and
/// the visible table stay in step.
pub(crate) const CATALOG_VISIBLE_ROWS: usize = 8;

/// Filtered catalog commands for the current query, best match first.
pub(crate) fn catalog_hits(query: &str) -> Vec<&'static sf::catalog::CommandSpec> {
    sf::catalog::search(&sf::catalog::catalog().commands, query, usize::MAX)
        .into_iter()
        .map(|hit| hit.command)
        .collect()
}

/// Keep the selected row inside the visible window.
pub(crate) fn catalog_scroll_for(selected: usize, scroll: usize, len: usize) -> usize {
    let max_start = len.saturating_sub(CATALOG_VISIBLE_ROWS);
    let mut next = scroll.min(max_start);
    if selected < next {
        next = selected;
    } else if selected >= next.saturating_add(CATALOG_VISIBLE_ROWS) {
        next = selected
            .saturating_add(1)
            .saturating_sub(CATALOG_VISIBLE_ROWS);
    }
    next.min(max_start)
}

/// Build argv for a catalog command from its first example, falling back to the command id.
/// Only modern `sf` examples without unfilled `<placeholder>` tokens are usable: a legacy
/// `sfdx force:` example or a placeholder such as `<test run id>` would otherwise produce a
/// command that is guaranteed to fail. No flags are invented.
pub(crate) fn catalog_command_argv(command: &sf::catalog::CommandSpec) -> Vec<String> {
    let example = command
        .examples
        .first()
        .map(|item| item.command.trim())
        .unwrap_or("");
    let usable = example.starts_with("sf ")
        && !example.contains('<')
        && !example.contains('>')
        && !example.to_ascii_lowercase().contains("sfdx");
    if usable && let Some(argv) = parse_run_command(example) {
        return argv;
    }
    split_args(&command.id.replace(':', " "))
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

fn paths_display(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ")
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

/// A compact result for `org display`: the parser has already dropped secret fields.
fn org_display_result(username: &str, detail: &OrgDetail, ok: bool) -> ResultView {
    let items = detail
        .fields
        .iter()
        .take(24)
        .map(|(label, value)| ResultItem {
            ok: true,
            label: label.clone(),
            detail: value.clone(),
            progress: None,
        })
        .collect();
    ResultView {
        kind: ResultKind::Generic,
        title: "ORG DETAIL".to_owned(),
        status: if ok { "SUCCESS" } else { "FAILED" }.to_owned(),
        ok,
        summary: format!("{username} · {} fields", detail.fields.len()),
        items,
        coverage: Vec::new(),
        sections: Vec::new(),
    }
}

/// A result for org actions whose raw output must never be retained or shown.
fn redacted_result(title: &str, ok: bool, detail: &str) -> ResultView {
    ResultView {
        kind: ResultKind::Generic,
        title: title.to_uppercase(),
        status: if ok { "SUCCESS" } else { "FAILED" }.to_owned(),
        ok,
        summary: format!("{detail} · output redacted"),
        items: vec![ResultItem {
            ok,
            label: "[output redacted]".to_owned(),
            detail: String::new(),
            progress: None,
        }],
        coverage: Vec::new(),
        sections: Vec::new(),
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
        JobPurpose::Create { .. } => Some(TaskKind::Command),
        JobPurpose::Report { .. }
        | JobPurpose::TestReport { .. }
        | JobPurpose::DeployCancel { .. } => None,
    }
}

/// Target of a `[t]` report retry. Both variants refetch an existing remote id; neither submits
/// a new deploy or test run.
enum ReportTarget {
    Apex {
        run_id: String,
        org: Option<String>,
    },
    Deploy {
        job_id: String,
        options: TransferOptions,
        org: Option<String>,
    },
}

/// Build the rerun spec only for user transfers and Apex runs. Everything else (reports, cancel,
/// auth, installer and secret commands) returns None so it cannot be replayed with `[r]`.
fn invocation_spec(
    purpose: &JobPurpose,
    executable: &Path,
    argv: &[String],
    display: &str,
) -> Option<InvocationSpec> {
    let org = match purpose {
        JobPurpose::Transfer { options } => options.target_org.clone(),
        JobPurpose::ApexTests { org, .. } => org.clone(),
        _ => return None,
    };
    Some(InvocationSpec {
        executable: executable.to_path_buf(),
        argv: argv.to_vec(),
        display: display.to_owned(),
        purpose: purpose.clone(),
        org,
        working_dir: std::env::current_dir().ok(),
    })
}

/// Pick the job whose label best describes current activity: a running user job first, then any
/// running job, then a queued job, then the newest finished job. Poll/report jobs must not steal
/// the label from the user command that is actually running.
fn choose_active_job(
    jobs: impl Iterator<Item = (u64, &'static str, bool, bool)>,
    queued: bool,
) -> Option<String> {
    let entries: Vec<(u64, &'static str, bool, bool)> = jobs.collect();
    let newest = |candidates: &[(u64, &'static str, bool, bool)]| {
        candidates
            .iter()
            .max_by_key(|(id, _, _, _)| *id)
            .map(|(_, label, _, _)| (*label).to_owned())
    };
    let running_user: Vec<_> = entries
        .iter()
        .copied()
        .filter(|(_, _, running, user)| *running && *user)
        .collect();
    if let Some(label) = newest(&running_user) {
        return Some(label);
    }
    let running: Vec<_> = entries
        .iter()
        .copied()
        .filter(|(_, _, running, _)| *running)
        .collect();
    if let Some(label) = newest(&running) {
        return Some(label);
    }
    if queued {
        return Some("queued".to_owned());
    }
    newest(&entries)
}

fn is_user_purpose(purpose: &JobPurpose) -> bool {
    matches!(
        purpose,
        JobPurpose::Command { .. }
            | JobPurpose::Transfer { .. }
            | JobPurpose::ApexTests { .. }
            | JobPurpose::Installer { .. }
            | JobPurpose::Create { .. }
    )
}

/// Whether a picker value is a concrete `Type:Name` (not a bare type, blank or wildcard).
fn is_explicit_component(value: &str) -> bool {
    let Some((kind, name)) = value.split_once(':') else {
        return false;
    };
    !kind.trim().is_empty() && !name.trim().is_empty() && !name.contains(['*', '%', '?'])
}

/// Why the checked entries cannot be deployed. Org-only components have no local source, and a
/// bare type or wildcard would deploy far more than the user expects.
fn deploy_problem(entries: &[TransferSelection]) -> Option<String> {
    for entry in entries.iter().filter(|entry| entry.checked) {
        if entry.item.path.starts_with("org:") {
            return Some(format!(
                "{} exists only in the org — it can be retrieved, not deployed",
                entry.item.value
            ));
        }
        if !is_explicit_component(&entry.item.value) {
            return Some(format!(
                "Deploy needs an exact Type:Name, not '{}'",
                entry.item.value
            ));
        }
    }
    None
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

    #[tokio::test]
    async fn shutdown_closes_the_event_channel() {
        let mut app = App::new(
            Startup {
                installation: None,
                version: None,
                current_org: None,
                notices: Vec::new(),
            },
            Config::default(),
            std::path::PathBuf::from("/tmp/sftui-shutdown-test"),
        );
        assert!(!app.event_rx.is_closed());
        app.shutdown().await;
        assert!(app.event_rx.is_closed());
        // A send after shutdown must fail immediately instead of blocking on a full channel,
        // which is what used to deadlock when the process tasks were being joined.
        let late = app.event_tx.send(AppEvent::Notice {
            text: "late".to_owned(),
            error: false,
        });
        let result = tokio::time::timeout(Duration::from_secs(1), late).await;
        assert!(matches!(result, Ok(Err(_))));
    }

    struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn shutdown_aborts_and_joins_helper_tasks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut app = App::new(
            Startup {
                installation: None,
                version: None,
                current_org: None,
                notices: Vec::new(),
            },
            Config::default(),
            std::path::PathBuf::from("/tmp/sftui-shutdown-abort-test"),
        );
        let ended = Arc::new(AtomicBool::new(false));
        let guard = DropFlag(Arc::clone(&ended));
        let waiting = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        let previous = app.probe_task.replace(waiting);
        if let Some(previous) = previous {
            previous.abort();
        }
        app.shutdown().await;
        assert!(
            ended.load(Ordering::SeqCst),
            "shutdown must abort the stored probe task"
        );
        assert!(app.probe_task.is_none());
        assert!(app.search_task.is_none());
    }

    #[test]
    fn status_expires_after_its_time_to_live() {
        let mut app = test_app();
        app.notice("transient note");
        assert_eq!(app.status, "transient note");
        assert!(!app.status_error);

        // Before the TTL elapses the notice is still visible.
        app.expire_status(Instant::now() + STATUS_TTL - Duration::from_millis(1));
        assert_eq!(app.status, "transient note");

        // At the TTL it clears, so the footer key hints reappear.
        app.expire_status(Instant::now() + STATUS_TTL);
        assert!(app.status.is_empty());
    }

    #[test]
    fn accent_never_collides_with_error_or_warning_colours() {
        use ratatui::style::Color;
        let mut app = test_app();
        for value in ["red", "RED", "yellow", "Yellow", "unknown"] {
            app.config.theme.accent = value.to_owned();
            assert_eq!(app.accent(), Color::Cyan, "accent {value} must fall back");
        }
        app.config.theme.accent = "green".to_owned();
        assert_eq!(app.accent(), Color::Green);
        app.config.theme.accent = "magenta".to_owned();
        assert_eq!(app.accent(), Color::Magenta);
    }

    #[test]
    fn set_tab_clears_status_and_its_timestamp() {
        let mut app = test_app();
        app.error("a failure");
        assert!(app.status_error);
        app.set_tab(Tab::Tests);
        assert!(app.status.is_empty());
        assert!(app.status_set_at.is_none());
    }

    #[test]
    fn menu_keys_open_screens_and_escape_returns_to_previous() {
        let mut app = test_app();
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Tests);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.tab, Tab::Home);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.tab, Tab::Retrieve);
    }

    #[test]
    fn number_keys_open_menu_items_and_tab_does_not_switch_screens() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('1')));
        assert_eq!(app.tab, Tab::Retrieve);
        assert!(app.transfer_search.retrieve);
        app.go_home();
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Tests);
        app.go_home();
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
            render_revision: 0,
            lines_truncated: false,
            lifecycle: if finished {
                JobLifecycle::Finished
            } else {
                JobLifecycle::Running
            },
            invocation: None,
            cleanup_error: None,
        }
    }

    #[test]
    fn active_job_label_prefers_a_running_user_job() {
        // (id, label, running, is_user_purpose)
        let jobs = [
            (1, "transfer", true, true),
            (2, "apex report", false, false), // newest id, but a finished poll job
            (3, "orgs", true, false),         // running, but not a user command
        ];
        assert_eq!(
            choose_active_job(jobs.into_iter(), false).as_deref(),
            Some("transfer")
        );
        // Without a running user job, any running job wins.
        assert_eq!(
            choose_active_job(
                [(1, "transfer", false, true), (2, "orgs", true, false)].into_iter(),
                false
            )
            .as_deref(),
            Some("orgs")
        );
        // A queued job outranks finished ones; otherwise the newest finished job is shown.
        assert_eq!(
            choose_active_job([(1, "transfer", false, true)].into_iter(), true).as_deref(),
            Some("queued")
        );
        assert_eq!(
            choose_active_job(
                [(1, "transfer", false, true), (2, "orgs", false, false)].into_iter(),
                false
            )
            .as_deref(),
            Some("orgs")
        );
        assert_eq!(choose_active_job([].into_iter(), false), None);
    }

    #[test]
    fn streamed_lines_beyond_the_byte_cap_are_flagged_as_truncated() {
        let mut app = test_app();
        app.output_state.begin(
            1,
            "sf test".to_owned(),
            "tests",
            TaskKind::Tests,
            PathBuf::from("sf"),
            Vec::new(),
        );
        if let Some(record) = app.output_state.find_mut(1) {
            record.line_bytes = MAX_RECORD_BYTES;
        }
        let before = app.output_state.records[0].render_revision;
        app.append_stream_lines(std::iter::once(JobLine {
            job_id: 1,
            stream: crate::sf::runner::OutputStream::Stdout,
            text: "overflow".to_owned(),
        }));
        assert!(app.output_state.records[0].lines_truncated());
        assert!(app.output_state.records[0].lines.is_empty());
        assert!(app.output_state.records[0].render_revision > before);
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

    /// Spawn a harmless local process so the job is tracked and `finish_job` can be driven with a
    /// synthetic outcome instead of waiting for a real CLI.
    fn spawn_tracked(app: &mut App, purpose: JobPurpose) -> u64 {
        let executable = if cfg!(windows) {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".to_owned())
        } else {
            "/bin/true".to_owned()
        };
        app.spawn_external(std::path::PathBuf::from(executable), Vec::new(), purpose);
        app.output_state
            .records
            .last()
            .map(|record| record.id)
            .expect("spawned record")
    }

    #[test]
    fn org_jobs_get_a_record_and_store_results_in_jobs() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let _guard = runtime.enter();
        let mut app = test_app();

        let list_id = spawn_tracked(&mut app, JobPurpose::OrgList);
        let list_record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == list_id)
            .expect("org list record");
        assert_eq!(list_record.task, TaskKind::Organization);
        assert!(app.job_rows().iter().any(|row| row.record.id == list_id));
        assert!(!is_user_purpose(&JobPurpose::OrgList));
        assert!(!is_user_purpose(&JobPurpose::OrgAction {
            description: "switch".to_owned(),
        }));

        app.finish_job(
            list_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"result":{"scratchOrgs":[{"username":"demo@example.com","aliases":["demo"]}]}}"#
                    .to_owned(),
                stderr: String::new(),
            },
        );
        let list_record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == list_id)
            .expect("org list record");
        assert_eq!(
            list_record.result.as_ref().map(|view| view.title.as_str()),
            Some("ORGS")
        );
        assert_eq!(app.org_state.orgs.len(), 1);

        let detail_id = spawn_tracked(
            &mut app,
            JobPurpose::OrgDetail {
                username: "demo@example.com".to_owned(),
            },
        );
        app.finish_job(
            detail_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"result":{"username":"demo@example.com","instanceUrl":"https://example.my.salesforce.com","accessToken":"SUPER_SECRET"}}"#
                    .to_owned(),
                stderr: String::new(),
            },
        );
        let detail_record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == detail_id)
            .expect("org detail record");
        let detail_view = detail_record.result.as_ref().expect("org detail result");
        assert_eq!(detail_view.title, "ORG DETAIL");
        assert!(!format!("{detail_view:?}").contains("SUPER_SECRET"));

        let action_id = spawn_tracked(
            &mut app,
            JobPurpose::OrgAction {
                description: "Switch org".to_owned(),
            },
        );
        app.finish_job(
            action_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: "token SECRET".to_owned(),
                stderr: String::new(),
            },
        );
        let action_record = app
            .output_state
            .records
            .iter()
            .find(|record| record.id == action_id)
            .expect("org action record");
        let action_view = action_record.result.as_ref().expect("org action result");
        assert!(action_view.summary.contains("output redacted"));
        assert!(!format!("{action_view:?}").contains("SECRET"));
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
            executable: PathBuf::from("/bin/true"),
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
        assert_eq!(record.lifecycle, JobLifecycle::Cancelled);
        assert_eq!(record.status_text(), "Cancelled");
        assert!(app.pending_jobs.is_empty());
        let rows = app.job_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].section, JobSection::Finished);
        // A dequeued job was never submitted, so it cannot be rerun or retried.
        assert!(record.invocation.is_none());
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
        app.handle_key(key(KeyCode::Char('o')));
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
                selected_id, focus, ..
            }) => {
                assert_eq!(selected_id, Some(2));
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
    fn validation_errors_keep_the_create_scratch_prompt_open() {
        let mut app = test_app();
        app.open_prompt(
            "Create scratch org",
            PromptKind::CreateScratch,
            vec![PromptField::new("Alias", "")],
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.modal, Some(Modal::Prompt { .. })));
        assert!(app.status.contains("alias"));
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
    fn editing_keeps_selection_and_marks_the_search_pending() {
        let mut state = TransferSearchState::new(false);
        state.selected.push(TransferSelection {
            item: SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            checked: true,
        });
        state.query = "Demo".to_owned();
        state.mark_edited();
        assert_eq!(state.checked().len(), 1);
        assert!(state.searching);
        assert_eq!(state.phase, SearchPhase::Searching);
        assert!(state.needs_search());
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
    fn f7_opens_the_catalog_from_every_screen() {
        let mut app = test_app();
        for tab in app.screens() {
            app.tab = tab;
            app.handle_key(key(KeyCode::F(7)));
            assert!(
                matches!(app.modal, Some(Modal::Catalog { .. })),
                "F7 did not open the catalog on {tab:?}"
            );
            app.modal = None;
        }
    }

    #[test]
    fn plain_letters_and_digits_reach_the_search_fields() {
        let mut app = test_app();
        for tab in [Tab::Retrieve, Tab::Deploy, Tab::Tests] {
            app.tab = tab;
            app.modal = None;
            app.transfer_search.query.clear();
            app.test_search.query.clear();
            for character in "e2v".chars() {
                app.handle_key(key(KeyCode::Char(character)));
            }
            assert!(app.modal.is_none(), "plain keys opened a modal on {tab:?}");
            let query = if tab == Tab::Tests {
                app.test_search.query.as_str()
            } else {
                app.transfer_search.query.as_str()
            };
            assert_eq!(query, "e2v", "search field missed plain keys on {tab:?}");
        }
    }

    #[test]
    fn catalog_selection_confirms_the_exact_command() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::F(7)));
        for character in "org list".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        app.handle_key(key(KeyCode::Enter));
        match app.modal.take() {
            Some(Modal::Confirm {
                message,
                action: PendingAction::RunCommand { argv, command },
                ..
            }) => {
                assert!(message.contains("sf org list"), "message: {message}");
                assert_eq!(argv, vec!["org".to_owned(), "list".to_owned()]);
                assert_eq!(command, "sf org list");
            }
            other => panic!("expected catalog confirmation, got {other:?}"),
        }
    }

    #[test]
    fn catalog_command_uses_the_first_example_or_falls_back_to_the_id() {
        let hits = catalog_hits("project retrieve start");
        assert_eq!(
            hits.first().map(|command| command.id.as_str()),
            Some("project retrieve start")
        );
        assert_eq!(
            catalog_command_argv(hits[0]),
            vec![
                "project".to_owned(),
                "retrieve".to_owned(),
                "start".to_owned()
            ]
        );
        let no_examples = sf::catalog::catalog()
            .commands
            .iter()
            .find(|command| command.examples.is_empty())
            .expect("a command without examples");
        assert_eq!(
            catalog_command_argv(no_examples),
            split_args(&no_examples.id.replace(':', " "))
        );
    }

    #[test]
    fn catalog_argv_ignores_placeholders_and_legacy_examples() {
        let command = |id: &str, example: &str| sf::catalog::CommandSpec {
            id: id.to_owned(),
            summary: String::new(),
            description: String::new(),
            category: String::new(),
            flags: Vec::new(),
            examples: vec![sf::catalog::Example {
                command: example.to_owned(),
                description: String::new(),
            }],
        };
        let placeholder = command(
            "apex get test",
            "sf apex get test --test-run-id <test run id> --json",
        );
        assert_eq!(
            catalog_command_argv(&placeholder),
            vec!["apex", "get", "test"]
        );
        let legacy = command(
            "force:lightning:lwc:test:run",
            "sfdx force:lightning:lwc:test:run",
        );
        assert_eq!(
            catalog_command_argv(&legacy),
            vec!["force", "lightning", "lwc", "test", "run"]
        );
    }

    #[test]
    fn catalog_empty_search_keeps_the_modal_open_on_enter() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::F(7)));
        for character in "zzzznotacommand".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.modal, Some(Modal::Catalog { .. })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.modal.is_none());
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
    fn active_apex_test_runs_counts_running_and_pending_runs_once() {
        let mut app = test_app();
        assert_eq!(app.active_apex_test_runs(), 0);

        let mut initial = record(1, TaskKind::Tests, Some(0));
        initial.finished = false; // initial `apex run test` command still running
        app.output_state.records.push(initial);
        assert_eq!(app.active_apex_test_runs(), 1);

        let mut polling = record(2, TaskKind::Tests, Some(0));
        polling.pending_report = true; // start-then-poll phase of a second run
        app.output_state.records.push(polling);
        // A running deploy and a finished Apex run are not active test runs.
        app.output_state
            .records
            .push(record(3, TaskKind::Deploy, None));
        app.output_state
            .records
            .push(record(4, TaskKind::Tests, Some(0)));
        assert_eq!(app.active_apex_test_runs(), 2);

        // When the first command returns, start-then-poll keeps it active via pending_report.
        app.output_state.records[0].finished = true;
        app.output_state.records[0].pending_report = true;
        assert_eq!(app.active_apex_test_runs(), 2);
        app.output_state.records[1].pending_report = false;
        assert_eq!(app.active_apex_test_runs(), 1);
    }

    #[test]
    fn finalized_apex_report_refreshes_the_detail_and_keeps_the_jobs_selection() {
        let mut app = test_app();
        let mut pending = record(7, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        pending.result = Some(started_test_view("08N000000000001"));
        app.output_state.records.push(pending);
        app.test_reports.insert(
            7,
            TestReport {
                run_id: "08N000000000001".to_owned(),
                org: None,
                command: "sf apex run test".to_owned(),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                // Last extra poll for per-class coverage: a terminal result must finalize here.
                coverage_polls: 3,
                final_run: None,
            },
        );
        app.modal = Some(Modal::Jobs {
            selected_id: Some(7),
            focus: JobsFocus::Detail,
            scroll: 0,
            follow: true,
            detail_view: JobDetailView::Summary,
        });

        let before = crate::ui::job_detail_text(&app, 7);
        assert!(
            before
                .iter()
                .any(|line| line.contains("Waiting for org results")),
            "pending detail should show the waiting placeholder"
        );
        let before_revision = app.output_state.records[0].render_revision;

        app.apply_test_report(
            7,
            &serde_json::json!({
                "result": {
                    "testRunId": "08N000000000001",
                    "summary": {
                        "outcome": "Passed",
                        "testsRan": 1,
                        "passing": 1,
                        "failing": 0,
                        "testRunCoverage": "80%"
                    },
                    "tests": [{"FullName": "DemoTest.ok", "Outcome": "Pass"}]
                }
            }),
        );

        let record = &app.output_state.records[0];
        assert!(!record.pending_report);
        assert!(record.ended_at.is_some());
        assert!(
            record.render_revision > before_revision,
            "detail cache key must change"
        );
        assert_eq!(
            record.result.as_ref().map(|view| view.status.as_str()),
            Some("Passed")
        );
        let after = crate::ui::job_detail_text(&app, 7);
        assert!(
            !after
                .iter()
                .any(|line| line.contains("Waiting for org results")),
            "final detail must not show the waiting placeholder: {after:?}"
        );
        assert!(after.iter().any(|line| line.contains("Passed")));
        // The row moved from RUNNING to FINISHED; the Jobs window must still select it.
        assert_eq!(app.job_rows()[0].record.id, 7);
        assert_eq!(app.job_rows()[0].section, JobSection::Finished);
        assert!(matches!(
            app.modal,
            Some(Modal::Jobs {
                selected_id: Some(7),
                follow: false,
                ..
            })
        ));
    }

    fn pending_report_app(record_id: u64, run_id: &str) -> App {
        let mut app = test_app();
        let mut pending = record(record_id, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        pending.result = Some(started_test_view(run_id));
        app.output_state.records.push(pending);
        app.test_reports.insert(
            record_id,
            TestReport {
                run_id: run_id.to_owned(),
                org: Some("test-scratch".to_owned()),
                command: "sf apex get test".to_owned(),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                coverage_polls: 0,
                final_run: None,
            },
        );
        app
    }

    fn mixed_failed_report() -> serde_json::Value {
        serde_json::json!({
          "status": 100,
          "result": {
            "testRunId": "08N000000000002",
            "summary": {"outcome": "Failed", "testsRan": 3, "passing": 2, "failing": 1,
                        "skipped": 0},
            "tests": [
              {"FullName": "AlphaTest.ok", "Outcome": "Pass"},
              {"FullName": "AlphaTest.alsoOk", "Outcome": "Pass"},
              {"FullName": "BetaTest.bad", "Outcome": "Fail",
               "Message": "synthetic assertion failed"}
            ]
          }
        })
    }

    fn report_error_payload() -> serde_json::Value {
        serde_json::json!({
          "status": 100,
          "name": "SfError",
          "message": "synthetic report failure",
          "exitCode": 100
        })
    }

    #[test]
    fn failed_report_with_exit_100_is_finalized_with_every_class_and_method() {
        let mut app = pending_report_app(9, "08N000000000002");
        app.apply_test_report(9, &mixed_failed_report());

        assert!(!app.test_reports.contains_key(&9), "poll must be finished");
        let record = &app.output_state.records[0];
        assert!(!record.pending_report);
        let view = record.result.as_ref().expect("final result");
        assert!(!view.ok);
        assert_eq!(view.status, "Failed");
        assert!(
            view.summary.starts_with("Tests failed · "),
            "overall outcome must be visible: {}",
            view.summary
        );
        assert!(view.items.iter().any(|item| item.label == "AlphaTest"));
        assert!(view.items.iter().any(|item| item.label == "BetaTest"));
        let methods = view
            .sections
            .iter()
            .find(|section| section.title == "TEST METHODS")
            .expect("methods section");
        assert_eq!(methods.rows.len(), 3);
        assert!(methods.rows.iter().any(|row| {
            row.label == "BetaTest.bad" && row.detail.contains("synthetic assertion failed")
        }));
        let jobs_detail = crate::ui::job_detail_text(&app, 9);
        for expected in [
            "AlphaTest",
            "BetaTest.bad",
            "synthetic assertion failed",
            "Tests failed",
        ] {
            assert!(
                jobs_detail.iter().any(|line| line.contains(expected)),
                "Jobs detail must show {expected}: {jobs_detail:?}"
            );
        }
    }

    #[test]
    fn error_payload_with_exit_100_is_never_a_test_result() {
        let mut app = pending_report_app(10, "08N000000000002");
        for _ in 0..5 {
            app.apply_test_report(10, &report_error_payload());
        }
        let record = &app.output_state.records[0];
        let view = record.result.as_ref().expect("result");
        assert_eq!(
            view.status, "Report unavailable",
            "error envelope must be rejected"
        );
        assert_eq!(view.items.len(), 1);
        assert_eq!(view.items[0].label, "Test results unavailable");
    }

    #[test]
    fn missing_coverage_preserves_a_known_failed_outcome() {
        let mut app = pending_report_app(11, "08N000000000002");
        let with_pending_coverage = serde_json::json!({
          "status": 100,
          "result": {
            "testRunId": "08N000000000002",
            "summary": {"outcome": "Failed", "testsRan": 2, "passing": 1, "failing": 1,
                        "testRunCoverage": "80%"},
            "tests": [
              {"FullName": "AlphaTest.ok", "Outcome": "Pass"},
              {"FullName": "BetaTest.bad", "Outcome": "Fail", "Message": "synthetic assertion failed"}
            ]
          }
        });
        app.apply_test_report(11, &with_pending_coverage);
        assert!(app.test_reports.contains_key(&11));
        assert!(
            app.test_reports[&11]
                .final_run
                .as_ref()
                .is_some_and(|run| run.outcome == "Failed"),
            "the terminal outcome must be remembered"
        );

        // Coverage never arrives and the follow-up polls fail: the known outcome still wins.
        for _ in 0..5 {
            app.apply_test_report(11, &report_error_payload());
        }
        assert!(!app.test_reports.contains_key(&11));
        let view = app.output_state.records[0]
            .result
            .as_ref()
            .expect("final result");
        assert_eq!(view.status, "Failed");
        assert!(!view.ok);
        assert!(view.summary.starts_with("Tests failed · "));
        assert!(view.items.iter().any(|item| item.label == "BetaTest"));
    }

    #[tokio::test]
    async fn exit_100_report_does_not_log_a_cli_failure_but_an_error_payload_is_redacted() {
        let mut app = state_app(std::path::Path::new("/tmp/sftui-test"));
        let pending_id = spawn_tracked(
            &mut app,
            JobPurpose::ApexTests {
                command: "sf apex run test".to_owned(),
                org: Some("test-scratch".to_owned()),
            },
        );
        app.finish_job(
            pending_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"status":0,"result":{"testRunId":"08N000000000002","summary":{"outcome":"Processing"}}}"#.to_owned(),
                stderr: String::new(),
            },
        );
        let record_id = app
            .test_reports
            .keys()
            .copied()
            .next()
            .expect("started report");
        spawn_tracked(&mut app, JobPurpose::TestReport { record_id });
        let poll_id = app.jobs.keys().copied().max().expect("poll job");

        app.finish_job(
            poll_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(100),
                stdout: mixed_failed_report().to_string(),
                stderr: String::new(),
            },
        );
        assert!(
            !app.status_error,
            "a real failed-test report is not a CLI error: {}",
            app.status
        );
        assert!(!app.status.contains("synthetic assertion failed"));

        // A second run whose poll returns only an error envelope.
        let error_id = spawn_tracked(
            &mut app,
            JobPurpose::ApexTests {
                command: "sf apex run test".to_owned(),
                org: Some("test-scratch".to_owned()),
            },
        );
        app.finish_job(
            error_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"status":0,"result":{"testRunId":"08N000000000003","summary":{"outcome":"Processing"}}}"#.to_owned(),
                stderr: String::new(),
            },
        );
        let record_id = app
            .test_reports
            .keys()
            .copied()
            .next()
            .expect("second started report");
        spawn_tracked(&mut app, JobPurpose::TestReport { record_id });
        let poll_id = app.jobs.keys().copied().max().expect("poll job");
        app.finish_job(
            poll_id,
            sf::runner::ProcessOutcome {
                exit_code: Some(100),
                stdout: report_error_payload().to_string(),
                stderr: "synthetic diagnostic".to_owned(),
            },
        );
        assert!(app.status_error, "an error payload must stay a CLI failure");
        assert!(
            !app.status.contains("synthetic diagnostic"),
            "diagnostics must be redacted: {}",
            app.status
        );
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
                command: "sf apex run test".to_owned(),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                coverage_polls: 0,
                final_run: None,
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
        assert_eq!(
            record.lifecycle,
            JobLifecycle::ApexPending {
                run_id: "08N000000000001".to_owned(),
                org: Some("test-scratch".to_owned()),
                tracking: false,
                report_failed: false,
            }
        );
        assert_eq!(
            record.status_text(),
            "Stopped locally - server status unknown"
        );
        assert_eq!(app.job_rows()[0].section, JobSection::Finished);
    }

    fn finished_with_invocation(
        id: u64,
        task: TaskKind,
        purpose: JobPurpose,
        argv: Vec<String>,
        org: Option<String>,
    ) -> OutputRecord {
        let mut rec = record(id, task, Some(0));
        rec.lifecycle = JobLifecycle::Finished;
        rec.invocation = Some(InvocationSpec {
            executable: PathBuf::from("/bin/true"),
            argv,
            display: "sf original".to_owned(),
            purpose,
            org,
            working_dir: None,
        });
        rec
    }

    #[test]
    fn rerun_uses_stored_classes_after_picker_changes() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let _guard = runtime.enter();
        let mut app = test_app();
        let purpose = JobPurpose::ApexTests {
            command: "sf apex run test --class-names OriginalTest".to_owned(),
            org: Some("original-org".to_owned()),
        };
        let argv =
            crate::sf::commands::apex_test_argv(&["OriginalTest".to_owned()], Some("original-org"));
        app.output_state.records.push(finished_with_invocation(
            1,
            TaskKind::Tests,
            purpose,
            argv,
            Some("original-org".to_owned()),
        ));
        // Simulate the picker/default org changing to something else.
        app.deploy_state.options.target_org = Some("new-default".to_owned());
        assert!(app.enqueue_rerun(1, Some("original-org".to_owned())));
        assert_eq!(app.output_state.records.len(), 2);
        let new_record = app.output_state.records.last().expect("new job");
        assert!(
            new_record.argv.contains(&"OriginalTest".to_owned()),
            "rerun must use the original class names"
        );
        assert!(
            !new_record.argv.contains(&"CurrentPickerTest".to_owned()),
            "rerun must not read the current picker"
        );
        assert!(new_record.argv.contains(&"original-org".to_owned()));
    }

    #[test]
    fn rerun_is_not_offered_for_internal_or_secret_commands() {
        let mut app = test_app();
        let mut report = record(1, TaskKind::Deploy, Some(0));
        report.invocation = invocation_spec(
            &JobPurpose::Report {
                options: TransferOptions::default(),
                transfer_job: 1,
            },
            Path::new("sf"),
            &[],
            "sf report",
        );
        app.output_state.records.push(report);
        let mut installer = record(2, TaskKind::Setup, Some(0));
        installer.invocation = invocation_spec(
            &JobPurpose::Installer {
                command: "install".to_owned(),
            },
            Path::new("sh"),
            &[],
            "sh install",
        );
        app.output_state.records.push(installer);
        assert!(app.output_state.records[0].invocation.is_none());
        assert!(app.output_state.records[1].invocation.is_none());
        assert!(!app.enqueue_rerun(1, None));
        assert!(!app.enqueue_rerun(2, None));
    }

    #[test]
    fn apex_stop_tracking_then_retry_report_does_not_submit_a_new_run() {
        let mut app = test_app();
        let mut pending = record(9, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        pending.lifecycle = JobLifecycle::ApexPending {
            run_id: "08N9".to_owned(),
            org: Some("dev".to_owned()),
            tracking: true,
            report_failed: false,
        };
        app.output_state.records.push(pending);
        app.test_reports.insert(
            9,
            TestReport {
                run_id: "08N9".to_owned(),
                org: Some("dev".to_owned()),
                command: "sf apex run test".to_owned(),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                coverage_polls: 0,
                final_run: None,
            },
        );
        assert!(app.cancel_job(9));
        assert!(!app.test_reports.contains_key(&9));
        assert!(app.output_state.records[0].lifecycle.can_retry_report());
        // [t] refetches the exact original id/org; no new `apex run test` job is created.
        assert!(app.retry_report(9));
        assert!(app.test_reports.contains_key(&9));
        assert_eq!(app.test_reports[&9].run_id, "08N9");
        assert_eq!(app.test_reports[&9].org.as_deref(), Some("dev"));
        assert_eq!(
            app.output_state.records[0].status_text(),
            "Apex tests running"
        );
    }

    #[test]
    fn deploy_cancel_uses_exact_id_and_updates_parent_only() {
        let mut app = test_app();
        let mut deploy = record(11, TaskKind::Deploy, Some(0));
        deploy.lifecycle = JobLifecycle::DeployPending {
            job_id: "0Af000000000001".to_owned(),
            org: Some("dev".to_owned()),
            cancel_requested: false,
            report_failed: false,
        };
        app.output_state.records.push(deploy);
        assert!(app.cancel_job(11));
        assert!(
            app.output_state.records[0]
                .status_text()
                .contains("Cancel requested")
        );
        assert!(
            app.jobs.is_empty(),
            "cancel must not create a user job slot"
        );
        let purposes: Vec<_> = app
            .jobs
            .values()
            .map(|entry| entry.purpose.clone())
            .collect();
        assert!(purposes.is_empty());
        // The queued helper carries the exact id/org and no --use-most-recent.
        let cancel_argv =
            crate::sf::deploy::deploy_cancel_argv("0Af000000000001", "dev").expect("cancel argv");
        assert!(cancel_argv.contains(&"--job-id".to_owned()));
        assert!(cancel_argv.contains(&"0Af000000000001".to_owned()));
        assert!(cancel_argv.contains(&"--target-org".to_owned()));
        assert!(cancel_argv.contains(&"dev".to_owned()));
        assert!(cancel_argv.contains(&"--async".to_owned()));
        assert!(
            !cancel_argv
                .iter()
                .any(|part| part.contains("use-most-recent"))
        );
    }

    #[test]
    fn selection_stays_on_a_job_when_it_moves_section() {
        let mut app = test_app();
        let mut pending = record(5, TaskKind::Tests, Some(0));
        pending.pending_report = true;
        app.output_state.records.push(pending);
        app.test_reports.insert(
            5,
            TestReport {
                run_id: "08N000000000002".to_owned(),
                org: None,
                command: "sf apex get test".to_owned(),
                poll_after: Instant::now(),
                attempts: 1,
                failures: 0,
                coverage_polls: 0,
                final_run: None,
            },
        );
        app.modal = Some(Modal::Jobs {
            selected_id: Some(5),
            focus: JobsFocus::List,
            scroll: 0,
            follow: true,
            detail_view: JobDetailView::Summary,
        });
        assert_eq!(app.job_rows()[0].section, JobSection::Running);
        app.apply_test_report(5, &mixed_failed_report());
        assert_eq!(app.job_rows()[0].section, JobSection::Finished);
        match app.modal {
            Some(Modal::Jobs { selected_id, .. }) => assert_eq!(selected_id, Some(5)),
            other => panic!("expected jobs modal, got {other:?}"),
        }
    }

    #[test]
    fn menu_lists_task_screens_in_order() {
        let items: Vec<(char, &str)> = MENU_ITEMS
            .iter()
            .map(|item| (item.key, item.label))
            .collect();
        assert_eq!(
            items,
            vec![
                ('1', "Retrieve/Deploy Metadata"),
                ('2', "Create Metadata"),
                ('3', "Apex Tests"),
            ]
        );
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('1')));
        assert_eq!(app.tab, Tab::Retrieve);
        assert!(app.transfer_action, "the dialog defaults to Retrieve");
        app.go_home();
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Tests);
    }

    #[test]
    fn suggestion_keys_open_confirmation_and_consume_arrows() {
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('1')));
        app.transfer_search.results = vec![
            SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            SuggestionItem::new("Flow:Other", "force-app/flows/Other.flow-meta.xml"),
        ];
        app.transfer_search.searched_query = Some(app.transfer_search.query.clone());
        app.handle_key(key(KeyCode::Tab)); // focus the result list before toggling
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
        assert_eq!(app.tab, Tab::Retrieve);
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
        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::Tests);
        app.test_search.results = vec![
            SuggestionItem::new("ApexClass:DemoTest", "force-app/classes/DemoTest.cls"),
            SuggestionItem::new("ApexClass:Helper", "force-app/classes/Helper.cls"),
        ];
        app.test_search.searched_query = Some(app.test_search.query.clone());
        app.handle_key(key(KeyCode::Tab)); // focus the result list before toggling
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

    /// Install a small in-memory index for a picker without touching the filesystem.
    fn install_test_index(app: &mut App, values: Vec<String>) {
        let root = app.suggestion_root();
        let set = Arc::new(SuggestionSet::new(
            crate::sf::metadata::SuggestionKind::Transfer,
            values,
        ));
        app.suggestion_cache = Some((root.clone(), Arc::clone(&set)));
        app.test_suggestion_cache = Some((root, set));
    }

    #[tokio::test]
    async fn typing_triggers_a_background_search_after_debounce_without_enter() {
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
        app.tab = Tab::Retrieve;
        install_test_index(
            &mut app,
            vec!["ApexClass:Demo".to_owned(), "ApexClass:DemoTest".to_owned()],
        );

        app.handle_key(key(KeyCode::Char('D')));
        assert_eq!(app.transfer_search.query, "D");
        assert!(app.search.has_pending(SearchKind::Transfer));
        assert!(app.search.in_flight.is_none(), "no ranking in the key path");
        assert!(app.transfer_search.results.is_empty());

        // A tick before the 90 ms quiet period must not start any ranking.
        app.tick().await;
        assert!(app.search.in_flight.is_none());

        tokio::time::sleep(SUGGESTION_DEBOUNCE + Duration::from_millis(40)).await;
        app.tick().await;
        assert!(
            app.search.in_flight.is_some(),
            "debounced ranking must start from the tick loop"
        );

        let mut applied = false;
        for _ in 0..40 {
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(100), app.recv_event()).await
            {
                app.on_event(event);
                if app.transfer_search.searched_query.as_deref() == Some("D") {
                    applied = true;
                    break;
                }
            }
        }
        assert!(applied, "a live search must apply without pressing Enter");
        assert_eq!(app.transfer_search.results.len(), 2);
        assert_eq!(app.transfer_search.results[0].value, "ApexClass:Demo");
        assert!(!app.transfer_search.searching);
        assert_eq!(app.transfer_search.phase, SearchPhase::Idle);
    }

    #[test]
    fn stale_reply_is_discarded_entirely() {
        let mut app = test_app();
        app.tab = Tab::Retrieve;
        let base = SearchRequest {
            kind: SearchKind::Transfer,
            generation: 0,
            scope: SearchScope {
                root: app.suggestion_root(),
                retrieve: app.transfer_search.retrieve,
                org: None,
            },
            query: "Demo".to_owned(),
            cursor_value: None,
            checked: Vec::new(),
        };
        app.search.schedule(base.clone(), SUGGESTION_DEBOUNCE);
        let old_generation = app.search.generation(SearchKind::Transfer);
        assert!(old_generation > 0);

        app.transfer_search.results = vec![
            SuggestionItem::new("ApexClass:Demo", "a.cls"),
            SuggestionItem::new("Flow:Other", "b.flow-meta.xml"),
        ];
        app.transfer_search.cursor = 1;
        app.transfer_search.searched_query = Some("Demo".to_owned());
        app.transfer_search.set_phase(SearchPhase::Idle);

        // A newer edit supersedes the in-flight request.
        app.edit_search(SearchKind::Transfer, |query| query.push('X'));
        assert_eq!(
            app.search.generation(SearchKind::Transfer),
            old_generation + 1
        );

        let mut stale = base;
        stale.generation = old_generation;
        app.on_event(AppEvent::SearchResults(SearchReply {
            request: stale,
            outcome: Ok(vec![SuggestionItem::new("Flow:Old", "old.flow-meta.xml")]),
        }));
        assert_eq!(app.transfer_search.results.len(), 2);
        assert_eq!(app.transfer_search.results[0].value, "ApexClass:Demo");
        assert_eq!(app.transfer_search.cursor, 1);
        assert!(
            app.transfer_search.searching,
            "stale reply must not clear loading"
        );
        assert!(app.transfer_search.needs_search());

        // A reply from another project root is equally unusable.
        let mut wrong_root = SearchRequest {
            generation: app.search.generation(SearchKind::Transfer),
            ..stale_request_template(SearchKind::Transfer, &app)
        };
        wrong_root.scope.root.push("elsewhere");
        app.on_event(AppEvent::SearchResults(SearchReply {
            request: wrong_root,
            outcome: Ok(Vec::new()),
        }));
        assert_eq!(app.transfer_search.results.len(), 2);
    }

    fn stale_request_template(kind: SearchKind, app: &App) -> SearchRequest {
        SearchRequest {
            kind,
            generation: 0,
            scope: SearchScope {
                root: app.suggestion_root(),
                retrieve: app.transfer_search.retrieve,
                org: None,
            },
            query: "DemoX".to_owned(),
            cursor_value: None,
            checked: Vec::new(),
        }
    }

    #[test]
    fn checked_item_survives_edits_and_refilters() {
        let mut app = test_app();
        app.tab = Tab::Tests;
        let checked = SuggestionItem::new("ApexClass:DemoTest", "classes/DemoTest.cls");
        app.test_search.results = vec![
            checked.clone(),
            SuggestionItem::new("ApexClass:Helper", "classes/Helper.cls"),
        ];
        app.test_search.searched_query = Some(String::new());
        app.test_search.toggle_result();
        assert_eq!(app.test_search.checked().len(), 1);

        app.edit_search(SearchKind::Tests, |query| query.push('D'));
        assert_eq!(
            app.test_search.checked().len(),
            1,
            "an edit keeps the check"
        );

        let mut request = stale_request_template(SearchKind::Tests, &app);
        request.generation = app.search.generation(SearchKind::Tests);
        request.checked = vec![checked.value.clone()];
        app.on_event(AppEvent::SearchResults(SearchReply {
            request,
            outcome: Ok(vec![SuggestionItem::new(
                "ApexClass:Helper",
                "classes/Helper-v2.cls",
            )]),
        }));
        let still_checked = app.test_search.checked();
        assert_eq!(still_checked.len(), 1);
        assert_eq!(still_checked[0].value, checked.value);

        // When the identity returns in a later result, its snapshot is refreshed, not duplicated.
        let mut request = stale_request_template(SearchKind::Tests, &app);
        request.generation = app.search.generation(SearchKind::Tests);
        request.checked = vec![checked.value.clone()];
        app.on_event(AppEvent::SearchResults(SearchReply {
            request,
            outcome: Ok(vec![SuggestionItem::new(
                checked.value.as_str(),
                "classes/DemoTest-v2.cls",
            )]),
        }));
        let still_checked = app.test_search.checked();
        assert_eq!(still_checked.len(), 1);
        assert_eq!(still_checked[0].path, "classes/DemoTest-v2.cls");
    }

    #[test]
    fn enter_with_no_results_does_not_search_or_start_jobs() {
        let mut app = test_app();
        app.tab = Tab::Retrieve;
        app.handle_key(key(KeyCode::Enter));
        assert!(app.modal.is_none());
        assert!(!app.search.has_pending(SearchKind::Transfer));
        assert!(app.search.in_flight.is_none());
        assert_eq!(app.job_count(), 0);
        assert!(app.status.contains("Select at least one"));

        // An earlier check is still reviewable on Enter, and that still starts no search or job.
        app.transfer_search.selected.push(TransferSelection {
            item: SuggestionItem::new("ApexClass:Demo", "classes/Demo.cls"),
            checked: true,
        });
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.modal, Some(Modal::TransferConfirm { .. })));
        assert!(!app.search.has_pending(SearchKind::Transfer));
        assert_eq!(app.job_count(), 0);
    }

    #[test]
    fn picker_focus_controls_space_and_tab_switches_panes() {
        let mut app = test_app();
        app.tab = Tab::Tests;
        app.test_search.results = vec![SuggestionItem::new("ApexClass:DemoTest", "a.cls")];
        app.test_search.searched_query = Some(String::new());
        assert_eq!(app.test_search.focus, SearchFocus::Input);

        // Typing goes to the input and does not check a result.
        app.handle_key(key(KeyCode::Char('D')));
        assert_eq!(app.test_search.query, "D");
        assert_eq!(app.test_search.focus, SearchFocus::Input);
        assert!(app.test_search.selected.is_empty());

        // Space in the input types a space; it does not check a result.
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.test_search.query, "D ");
        assert!(app.test_search.selected.is_empty());

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.test_search.focus, SearchFocus::List);
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.test_search.checked().len(), 1);

        // Typing focuses the input again and appends to the query.
        app.handle_key(key(KeyCode::Char('X')));
        assert_eq!(app.test_search.focus, SearchFocus::Input);
        assert_eq!(app.test_search.query, "D X");

        // Navigation keys hand focus to the list so the next Space selects.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.test_search.focus, SearchFocus::List);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.test_search.focus, SearchFocus::List);
    }

    #[test]
    fn enter_while_dirty_searches_and_focuses_results_without_confirm() {
        for tab in [Tab::Retrieve, Tab::Tests] {
            let mut app = test_app();
            app.tab = tab;
            let kind = if tab == Tab::Tests {
                SearchKind::Tests
            } else {
                SearchKind::Transfer
            };
            // Simulate a typed but not-yet-searched query (the debounced reply has not arrived).
            app.search_state_mut(kind).query = "Demo".to_owned();
            app.search_state_mut(kind).mark_edited();
            assert!(app.search_state(kind).needs_search());

            app.handle_key(key(KeyCode::Enter));

            assert!(
                app.modal.is_none(),
                "{tab:?}: Enter on a dirty query must not open confirm"
            );
            assert!(
                app.search.has_pending(kind),
                "{tab:?}: Enter must run the search"
            );
            assert_eq!(
                app.search_state(kind).focus,
                SearchFocus::List,
                "{tab:?}: Enter-driven search focuses the results"
            );
            assert_eq!(app.search_state(kind).query, "Demo");
        }
    }

    #[test]
    fn search_reply_does_not_steal_focus_from_the_input() {
        let mut app = test_app();
        app.tab = Tab::Retrieve;
        app.handle_key(key(KeyCode::Char('D')));
        assert_eq!(app.transfer_search.focus, SearchFocus::Input);

        let request = SearchRequest {
            kind: SearchKind::Transfer,
            generation: app.search.generation(SearchKind::Transfer),
            scope: SearchScope {
                root: app.suggestion_root(),
                retrieve: app.transfer_search.retrieve,
                org: None,
            },
            query: "D".to_owned(),
            cursor_value: None,
            checked: Vec::new(),
        };
        app.on_event(AppEvent::SearchResults(SearchReply {
            request,
            outcome: Ok(vec![SuggestionItem::new("ApexClass:Demo", "a.cls")]),
        }));
        assert_eq!(app.transfer_search.searched_query.as_deref(), Some("D"));
        assert_eq!(
            app.transfer_search.focus,
            SearchFocus::Input,
            "a debounced reply must not auto-focus the list"
        );
        // Space still types a space after the mid-query pause.
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.transfer_search.query, "D ");
    }

    #[tokio::test]
    async fn multi_word_query_results_arrive_after_enter_search() {
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
        app.tab = Tab::Retrieve;
        install_test_index(
            &mut app,
            vec![
                "ApexClass:DemoController".to_owned(),
                "ApexClass:DemoTest".to_owned(),
            ],
        );

        // A multi-word query: the space is typed into the input, then Enter searches.
        app.handle_key(key(KeyCode::Char('D')));
        app.handle_key(key(KeyCode::Char('e')));
        app.handle_key(key(KeyCode::Char('m')));
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Char('T')));
        app.handle_key(key(KeyCode::Char('e')));
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.transfer_search.query, "Demo Test");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.transfer_search.focus, SearchFocus::List);
        assert!(app.modal.is_none());

        tokio::time::sleep(SUGGESTION_DEBOUNCE + Duration::from_millis(40)).await;
        app.tick().await;
        let mut applied = false;
        for _ in 0..40 {
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(100), app.recv_event()).await
            {
                app.on_event(event);
                if app.transfer_search.searched_query.as_deref() == Some("Demo Test") {
                    applied = true;
                    break;
                }
            }
        }
        assert!(applied, "the multi-word query must return results");
        assert!(!app.transfer_search.results.is_empty());
        // Enter-driven focus survives the reply, so Space selects immediately.
        assert_eq!(app.transfer_search.focus, SearchFocus::List);
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.transfer_search.checked().len(), 1);
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
                assert!(message.contains("sf project retrieve start --metadata ApexClass:Demo"));
                assert!(!message.contains("--ignore-conflicts"));
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
        assert_eq!(app.tab, Tab::Home);
    }

    #[test]
    fn space_toggles_multiple_components_before_confirming() {
        let mut app = test_app();
        app.tab = Tab::Home;
        app.handle_key(key(KeyCode::Char('1')));
        app.transfer_search.results = vec![
            SuggestionItem::new("ApexClass:Demo", "force-app/classes/Demo.cls"),
            SuggestionItem::new("Flow:Other", "force-app/flows/Other.flow-meta.xml"),
        ];
        app.transfer_search.searched_query = Some(app.transfer_search.query.clone());
        app.handle_key(key(KeyCode::Tab)); // focus the result list before toggling
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
                assert!(message.contains("sf project deploy start --metadata ApexClass:Demo"));
                assert!(message.contains("--ignore-conflicts"));
                assert!(!message.contains("--json"));
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
    }

    #[test]
    fn human_deploy_output_registers_a_report_poll() {
        let options = TransferOptions {
            metadata_names: vec!["ApexClass:Demo".to_owned()],
            ..TransferOptions::default()
        };
        let stdout = "Deploy ID: 0Af0Y000003Dabc\nStatus: Queued\n";
        let id = sf::deploy::parse_started_job_id_from_text(stdout).expect("job id");
        let mut app = test_app();
        let view = app.register_started_deploy(&options, 42, &id);
        let report = app.deploy_reports.get(&42).expect("deploy report");
        assert_eq!(report.server_job_id, id);
        assert_eq!(report.options, options);
        assert_eq!(report.attempts, 0);
        assert_eq!(view.status, "Queued");
        assert!(view.summary.contains(&id));
    }

    // ----- Phase 5: Create Metadata wizard tests --------------------------------------

    #[test]
    fn create_wizard_choose_uses_explicit_enter_without_autosubmit() {
        let mut app = test_app();
        app.open_create_wizard();
        // Typing filters the static index asynchronously; nothing is submitted until Enter.
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('p')));
        assert!(matches!(
            app.modal,
            Some(Modal::CreateWizard {
                state: CreateWizardState::Choose { .. }
            })
        ));
        // Enter explicitly selects the highlighted row and advances to Configure.
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::CreateWizard {
                state: CreateWizardState::Configure { kind, .. },
            }) => assert_eq!(*kind, create::ALL_TYPES[0]),
            other => panic!("expected Configure after Enter, got {other:?}"),
        }
    }

    #[test]
    fn create_wizard_back_navigation_preserves_values() {
        let mut app = test_app();
        app.open_create_wizard();
        app.handle_key(key(KeyCode::Enter));
        for character in "DemoController".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        let (kind, values) = match app.modal.as_ref() {
            Some(Modal::CreateWizard {
                state: CreateWizardState::Configure { kind, values, .. },
            }) => (*kind, values.clone()),
            other => panic!("expected Configure, got {other:?}"),
        };
        assert_eq!(
            values.get(&FieldKey::Name).map(String::as_str),
            Some("DemoController")
        );
        let destination = Destination {
            label: "force-app/main/default".to_owned(),
            relative: PathBuf::from("force-app/main/default"),
        };
        let mut draft = CreateDraft::new(kind, destination.clone());
        for (key, value) in &values {
            draft.set(*key, value.clone());
        }
        // Review -> Esc must restore the exact Configure values.
        app.goto_create_review(draft, vec![destination], 0, String::new());
        app.handle_key(key(KeyCode::Esc));
        match app.modal.as_ref() {
            Some(Modal::CreateWizard {
                state: CreateWizardState::Configure { values, .. },
            }) => assert_eq!(
                values.get(&FieldKey::Name).map(String::as_str),
                Some("DemoController")
            ),
            other => panic!("expected Configure after back, got {other:?}"),
        }
    }

    #[test]
    fn create_wizard_argv_per_preset_uses_verified_flags() {
        let destination = Destination {
            label: "force-app/main/default".to_owned(),
            relative: PathBuf::from("force-app/main/default"),
        };
        let stage = Path::new("/staging");

        let mut trigger = CreateDraft::new(CreateKind::ApexTrigger, destination.clone());
        trigger.set(FieldKey::Name, "DemoTrigger");
        trigger.set(FieldKey::SObject, "Account");
        assert!(trigger.validation_error().is_none());
        let argv = crate::sf::commands::create_metadata_argv(&trigger, stage);
        assert!(argv.windows(2).any(|pair| pair == ["--sobject", "Account"]));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--event", "before insert"])
        );
        assert!(argv.iter().any(|part| part == "/staging"));

        let mut cmdt = CreateDraft::new(CreateKind::CustomMetadataType, destination.clone());
        cmdt.set(FieldKey::Name, "Setting");
        let argv = crate::sf::commands::create_metadata_argv(&cmdt, stage);
        assert!(argv.starts_with(&["cmdt", "generate", "object"].map(str::to_owned)));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--output-directory", "/staging"])
        );
        assert!(!argv.iter().any(|part| part == "--output-dir"));

        let mut lwc = CreateDraft::new(CreateKind::Lwc, destination.clone());
        lwc.set(FieldKey::Name, "BadName");
        assert!(
            lwc.validation_error()
                .is_some_and(|(key, _)| key == FieldKey::Name)
        );
        lwc.set(FieldKey::Name, "goodName");
        let argv = crate::sf::commands::create_metadata_argv(&lwc, stage);
        assert!(argv.windows(2).any(|pair| pair == ["--type", "lwc"]));
        assert!(!argv.iter().any(|part| part == "--template"));

        let mut page = CreateDraft::new(CreateKind::FlexiPage, destination);
        page.set(FieldKey::Name, "Account_Page");
        assert!(
            page.validation_error()
                .is_some_and(|(key, _)| key == FieldKey::FlexiTemplate)
        );
        page.set(FieldKey::FlexiTemplate, "RecordPage");
        assert!(
            page.validation_error()
                .is_some_and(|(key, _)| key == FieldKey::SObject)
        );
        page.set(FieldKey::SObject, "Account");
        let argv = crate::sf::commands::create_metadata_argv(&page, stage);
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--template", "RecordPage"])
        );
        assert!(argv.windows(2).any(|pair| pair == ["--sobject", "Account"]));
    }

    #[test]
    fn create_wizard_nested_package_root_resolves_without_double_default() {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join("sfdx-project.json"),
            r#"{"packageDirectories":[{"path":"packages/salesforce","default":true}]}"#,
        )
        .expect("write project");
        std::fs::create_dir_all(dir.path().join("packages/salesforce/main/default/classes"))
            .expect("source root");
        let options = create::destination_options(Some(dir.path()));
        assert_eq!(options.choices.len(), 1);
        assert_eq!(
            options.choices[0].relative,
            PathBuf::from("packages/salesforce/main/default")
        );
        assert!(!options.ambiguous);
    }

    #[test]
    fn create_wizard_missing_project_explains_how_to_open_one() {
        let mut app = test_app();
        app.state_root = None;
        app.open_create_wizard();
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::CreateWizard {
                state: CreateWizardState::Configure { error, .. },
            }) => assert!(
                error
                    .as_deref()
                    .is_some_and(|message| message.contains("sfdx-project.json")),
                "missing-project guidance missing: {error:?}"
            ),
            other => panic!("expected Configure with a guidance error, got {other:?}"),
        }
    }

    #[test]
    fn create_wizard_collision_with_existing_file_does_not_spawn() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join("sfdx-project.json"),
            r#"{"packageDirectories":[]}"#,
        )
        .expect("project");
        let target = dir
            .path()
            .join("force-app/main/default/classes/DemoController.cls");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("dirs");
        std::fs::write(&target, "existing").expect("existing file");
        let mut app = test_app();
        app.state_root = Some(dir.path().to_path_buf());
        app.startup.installation = Some(crate::sf::detect::SfInstallation {
            executable: PathBuf::from("/bin/true"),
            source: "test".to_owned(),
        });
        let mut draft = CreateDraft::new(
            CreateKind::ApexClass,
            Destination {
                label: "force-app/main/default".to_owned(),
                relative: PathBuf::from("force-app/main/default"),
            },
        );
        draft.set(FieldKey::Name, "DemoController");
        let before = app.next_job_id;
        app.enqueue_create_job(draft);
        assert_eq!(app.next_job_id, before);
        assert!(app.pending_jobs.is_empty());
        assert!(app.status.contains("Already exists"));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "existing");
    }

    #[tokio::test]
    async fn create_wizard_interactive_type_never_spawns_and_success_refreshes_project() {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join("sfdx-project.json"),
            r#"{"packageDirectories":[{"path":"force-app","default":true}]}"#,
        )
        .expect("project");
        std::fs::create_dir_all(dir.path().join("force-app/main/default/classes"))
            .expect("source root");
        let mut app = App::new(
            Startup::pending(),
            Config::default(),
            PathBuf::from("/tmp/sftui-create-success"),
        );
        app.state_root = Some(dir.path().to_path_buf());
        app.startup.installation = Some(crate::sf::detect::SfInstallation {
            executable: PathBuf::from("/bin/true"),
            source: "test".to_owned(),
        });
        let interactive = CreateDraft::new(
            CreateKind::CustomObject,
            Destination {
                label: "force-app/main/default".to_owned(),
                relative: PathBuf::from("force-app/main/default"),
            },
        );
        app.enqueue_create_job(interactive);
        assert!(app.jobs.is_empty() && app.pending_jobs.is_empty());

        let mut draft = CreateDraft::new(
            CreateKind::ApexClass,
            Destination {
                label: "force-app/main/default".to_owned(),
                relative: PathBuf::from("force-app/main/default"),
            },
        );
        draft.set(FieldKey::Name, "DemoController");
        app.enqueue_create_job(draft.clone());
        assert_eq!(
            app.jobs.len(),
            1,
            "the Create job must be tracked; status={}",
            app.status
        );
        let staged = match &app.jobs.values().next().expect("job").purpose {
            JobPurpose::Create { staging, .. } => staging.clone(),
            other => panic!("expected Create job, got {other:?}"),
        };
        assert!(staged.starts_with(dir.path()));
        // Start the job so the job id exists, then simulate a successful publication.
        let job_id = app.jobs.keys().copied().next().expect("job id");
        let report = PublishReport {
            written: vec![target_path(dir.path())],
            ..PublishReport::default()
        };
        app.apply_create_published(job_id, &draft, staged, report);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_refresh = false;
        while Instant::now() < deadline && !saw_refresh {
            let Some(event) = tokio::time::timeout(Duration::from_secs(2), app.recv_event())
                .await
                .ok()
                .flatten()
            else {
                break;
            };
            saw_refresh = matches!(event, AppEvent::Project { .. });
        }
        assert!(
            saw_refresh,
            "successful Create must trigger a project refresh"
        );
    }

    fn target_path(root: &Path) -> PathBuf {
        root.join("force-app/main/default/classes/DemoController.cls")
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

    /// App wired to a throwaway config dir. Call only inside a Tokio runtime (`#[tokio::test]`).
    fn state_app(config_dir: &Path) -> App {
        App::new(
            Startup::pending(),
            Config::default(),
            config_dir.to_path_buf(),
        )
    }

    fn sample_org(username: &str, alias: &str, is_default: bool) -> OrgSummary {
        OrgSummary {
            username: username.to_owned(),
            aliases: vec![alias.to_owned()],
            org_id: "00D000000000001".to_owned(),
            is_default,
            is_devhub: false,
            connected_status: "Connected".to_owned(),
            instance_url: "https://example.invalid.my.salesforce.com".to_owned(),
            scratch: true,
        }
    }

    fn saved_run(id: &str) -> SavedTestRun {
        SavedTestRun {
            id: id.to_owned(),
            finished_at: state_store::now_ms(),
            org: Some("alias".to_owned()),
            command: "sf apex run test".to_owned(),
            run: test_results::TestRun {
                command: "sf apex run test".to_owned(),
                outcome: "Passed".to_owned(),
                ..test_results::TestRun::default()
            },
        }
    }

    #[tokio::test]
    async fn cached_default_org_is_applied_to_the_header_at_startup() {
        let config = tempfile::tempdir().expect("config dir");
        let root = PathBuf::from("/work/project");
        let cache = OrgCache {
            orgs: vec![sample_org("cached@example.invalid", "cached-scratch", true)],
            ..OrgCache::default()
        };
        cache.save(&root, config.path()).expect("save cache");

        let mut app = state_app(config.path());
        app.initialize_project_state(&root);
        assert_eq!(app.startup.current_org.as_deref(), Some("cached-scratch"));
        assert_eq!(app.org_target_hint(), "cached-scratch");
        assert_eq!(app.org_state.orgs.len(), 1);
        assert!(app.state_dir.is_some());
    }

    #[tokio::test]
    async fn switching_the_default_org_updates_and_saves_the_cache() {
        let config = tempfile::tempdir().expect("config dir");
        let root = PathBuf::from("/work/project");
        let cache = OrgCache {
            orgs: vec![
                sample_org("a@example.invalid", "alias-a", false),
                sample_org("b@example.invalid", "alias-b", true),
            ],
            ..OrgCache::default()
        };
        cache.save(&root, config.path()).expect("save cache");

        let mut app = state_app(config.path());
        app.initialize_project_state(&root);
        app.startup.installation = Some(crate::sf::detect::SfInstallation {
            executable: PathBuf::from("/bin/true"),
            source: "test".to_owned(),
        });
        app.modal = Some(Modal::SwitchOrg { selected: 0 });
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.startup.current_org.as_deref(), Some("alias-a"));
        let saved = OrgCache::load(&root, config.path());
        assert!(saved.orgs[0].is_default, "new default must be persisted");
        assert!(!saved.orgs[1].is_default);
    }

    #[tokio::test]
    async fn org_detail_finish_populates_and_persists_the_detail_cache() {
        let config = tempfile::tempdir().expect("config dir");
        let root = PathBuf::from("/work/project");
        let mut app = state_app(config.path());
        app.initialize_project_state(&root);
        let id = spawn_tracked(
            &mut app,
            JobPurpose::OrgDetail {
                username: "demo@example.invalid".to_owned(),
            },
        );
        app.finish_job(
            id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"result":{"username":"demo@example.invalid","instanceUrl":"https://example.invalid","accessToken":"SUPER_SECRET"}}"#.to_owned(),
                stderr: String::new(),
            },
        );
        assert!(
            app.org_state
                .detail_cache
                .contains_key("demo@example.invalid")
        );
        let saved = OrgCache::load(&root, config.path());
        assert!(saved.details.contains_key("demo@example.invalid"));
        let raw =
            std::fs::read_to_string(state_store::state_dir(&root, config.path()).join("orgs.json"))
                .expect("orgs file");
        assert!(
            !raw.contains("SUPER_SECRET"),
            "credentials must never be saved"
        );
    }

    #[tokio::test]
    async fn terminal_apex_result_is_saved_to_project_history() {
        let config = tempfile::tempdir().expect("config dir");
        let root = PathBuf::from("/work/project");
        let mut app = state_app(config.path());
        app.initialize_project_state(&root);
        let id = spawn_tracked(
            &mut app,
            JobPurpose::ApexTests {
                command: "sf apex run test".to_owned(),
                org: Some("alias".to_owned()),
            },
        );
        app.finish_job(
            id,
            sf::runner::ProcessOutcome {
                exit_code: Some(0),
                stdout: r#"{"result":{"testRunId":"08N000000000001","summary":{"outcome":"Passed","testsRan":1,"passing":1,"failing":0},"tests":[{"FullName":"DemoTest.ok","Outcome":"Pass"}]}}"#.to_owned(),
                stderr: String::new(),
            },
        );
        assert_eq!(app.test_history.runs.len(), 1);
        let saved = TestHistory::load(&root, config.path());
        assert_eq!(saved.runs.len(), 1);
        assert_eq!(saved.runs[0].org.as_deref(), Some("alias"));
        assert_eq!(saved.runs[0].run.outcome, "Passed");
    }

    #[test]
    fn f8_opens_test_history_only_on_the_tests_screen() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::F(8)));
        assert!(app.modal.is_none(), "F8 must be inert outside Tests");
        app.tab = Tab::Tests;
        app.handle_key(key(KeyCode::F(8)));
        assert!(matches!(app.modal, Some(Modal::TestHistory { .. })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.modal.is_none());
    }

    #[tokio::test]
    async fn cached_version_is_applied_at_startup() {
        let config = tempfile::tempdir().expect("config dir");
        SfVersionCache {
            version: "99.0.0".to_owned(),
            saved_at: state_store::now_ms(),
        }
        .save(config.path())
        .expect("save version");

        let app = state_app(config.path());
        assert_eq!(app.startup.version.as_deref(), Some("99.0.0"));
    }

    #[tokio::test]
    async fn corrupt_version_cache_keeps_the_probe_path() {
        let config = tempfile::tempdir().expect("config dir");
        std::fs::write(config.path().join("sf-version.json"), b"{broken").expect("write corrupt");
        // A corrupt file loads as no cache, so start_probe still receives None and runs sf version.
        let app = state_app(config.path());
        assert!(app.startup.version.is_none());
        assert!(app.probe_task.is_some());
    }

    #[tokio::test]
    async fn probed_version_is_persisted_to_the_global_cache() {
        let config = tempfile::tempdir().expect("config dir");
        let mut app = state_app(config.path());
        app.on_event(AppEvent::Startup(sf::StartupEvent::Version(
            "99.0.0".to_owned(),
        )));
        assert_eq!(SfVersionCache::load(config.path()).version, "99.0.0");
    }

    #[tokio::test]
    async fn f5_and_setup_reprobe_force_a_fresh_version_probe() {
        let config = tempfile::tempdir().expect("config dir");
        SfVersionCache {
            version: "99.0.0".to_owned(),
            saved_at: state_store::now_ms(),
        }
        .save(config.path())
        .expect("save version");
        let mut app = state_app(config.path());

        // Simulate a completed startup probe, then check each manual refresh starts a new one.
        if let Some(task) = app.probe_task.take() {
            task.abort();
        }
        app.handle_key(key(KeyCode::F(5)));
        assert!(app.probe_task.is_some(), "F5 must re-probe the CLI version");
        if let Some(task) = app.probe_task.take() {
            task.abort();
        }
        app.tab = Tab::Setup;
        app.handle_key(key(KeyCode::Char('r')));
        assert!(
            app.probe_task.is_some(),
            "Setup r must re-probe the CLI version"
        );
    }

    #[tokio::test]
    async fn test_history_delete_and_two_step_clear_work() {
        let config = tempfile::tempdir().expect("config dir");
        let root = PathBuf::from("/work/project");
        let mut app = state_app(config.path());
        app.initialize_project_state(&root);
        app.test_history.push(saved_run("job-1"));
        app.test_history.push(saved_run("job-2"));
        app.modal = Some(Modal::TestHistory {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            focus: TestHistoryFocus::List,
            confirm_clear: false,
        });

        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.test_history.runs.len(), 1);
        app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(
            app.test_history.runs.len(),
            1,
            "first c only asks for confirmation"
        );
        app.handle_key(key(KeyCode::Char('c')));
        assert!(app.test_history.runs.is_empty());
        assert!(TestHistory::load(&root, config.path()).runs.is_empty());
    }

    #[test]
    fn dialog_chooses_the_action_without_clearing_the_selection() {
        let mut app = test_app();
        app.set_tab(Tab::Retrieve);
        app.transfer_search.results = vec![SuggestionItem::new(
            "ApexClass:Demo",
            "force-app/classes/Demo.cls",
        )];
        app.transfer_search.searched_query = Some(String::new());
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.transfer_search.checked().len(), 1);
        app.handle_key(key(KeyCode::Enter));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { retrieve, .. }) => assert!(*retrieve),
            other => panic!("expected transfer confirmation, got {other:?}"),
        }

        app.handle_key(key(KeyCode::Char('d')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm {
                retrieve, options, ..
            }) => {
                assert!(!retrieve, "d selects Deploy in the dialog");
                assert!(!options.retrieve);
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
        assert_eq!(app.transfer_search.checked().len(), 1, "selection survives");

        // Unchecking still works in the dialog.
        app.handle_key(key(KeyCode::Char(' ')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { entries, .. }) => {
                assert!(entries.iter().all(|entry| !entry.checked));
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }

        app.handle_key(key(KeyCode::Char('r')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { retrieve, .. }) => assert!(*retrieve),
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
    }

    #[test]
    fn org_only_components_cannot_be_deployed_from_the_dialog() {
        let mut app = test_app();
        app.set_tab(Tab::Retrieve);
        app.transfer_search.results =
            vec![SuggestionItem::new("ApexClass:OrgOnly", "org:dev-alias")];
        app.transfer_search.searched_query = Some(String::new());
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('d')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { retrieve, .. }) => {
                assert!(*retrieve, "Deploy must be refused for org-only components");
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
        assert!(app.status.contains("only in the org"));
    }

    #[test]
    fn org_only_row_builds_an_exact_retrieve_with_the_browsed_username() {
        let mut app = test_app();
        app.org_browse.username = "dev-user@example.invalid".to_owned();
        app.org_browse.org_label = "dev-alias".to_owned();
        app.org_browse.metadata_type = "ApexClass".to_owned();
        app.org_browse.checked = vec!["ApexClass:Org Only Class".to_owned()];

        app.open_org_confirm();
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm {
                retrieve,
                options,
                entries,
                ..
            }) => {
                assert!(*retrieve);
                assert_eq!(
                    options.target_org.as_deref(),
                    Some("dev-user@example.invalid"),
                    "the username, not the alias, must be pinned"
                );
                assert_eq!(
                    options.metadata_names,
                    vec!["ApexClass:Org Only Class".to_owned()]
                );
                let argv = options.argv();
                assert_eq!(
                    argv,
                    vec![
                        "project",
                        "retrieve",
                        "start",
                        "--metadata",
                        "ApexClass:Org Only Class",
                        "--target-org",
                        "dev-user@example.invalid",
                    ]
                );
                assert_eq!(entries.len(), 1);
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
    }

    #[test]
    fn switching_org_or_type_clears_the_previous_org_selection() {
        let mut app = test_app();
        let first = OrgSummary {
            username: "first@example.invalid".to_owned(),
            aliases: vec!["first-alias".to_owned()],
            ..OrgSummary::default()
        };
        let second = OrgSummary {
            username: "second@example.invalid".to_owned(),
            aliases: vec!["second-alias".to_owned()],
            ..OrgSummary::default()
        };
        app.org_state.orgs = vec![first.clone(), second.clone()];
        let first_scope = OrgBrowseState::scope(&first.username, "ApexClass");
        let second_scope = OrgBrowseState::scope(&second.username, "Flow");
        app.org_browse.cache.insert(
            first_scope.clone(),
            vec![OrgComponent {
                metadata_type: "ApexClass".to_owned(),
                full_name: "FirstOnly".to_owned(),
            }],
        );
        app.org_browse.cache.insert(
            second_scope.clone(),
            vec![OrgComponent {
                metadata_type: "Flow".to_owned(),
                full_name: "SecondOnly".to_owned(),
            }],
        );

        app.org_browse.org_selected = 0;
        app.org_browse.type_selected = 0;
        app.load_org_listing(false);
        app.org_browse
            .checked
            .push("ApexClass:FirstOnly".to_owned());
        assert_eq!(app.org_browse.username, first.username);

        app.org_browse.org_selected = 1;
        app.org_browse.type_selected = 2; // Flow in ORG_BROWSE_TYPES
        app.load_org_listing(false);
        assert_eq!(app.org_browse.username, second.username);
        assert_eq!(app.org_browse.metadata_type, "Flow");
        assert!(
            app.org_browse.checked.is_empty(),
            "selection must follow the scope"
        );
        assert_eq!(
            app.org_browse.rows,
            vec![OrgComponent {
                metadata_type: "Flow".to_owned(),
                full_name: "SecondOnly".to_owned()
            }]
        );
        assert!(
            app.org_browse
                .cache
                .get(&first_scope)
                .is_some_and(|rows| rows.len() == 1)
        );
    }

    #[test]
    fn local_deploy_picker_rejects_bare_and_wildcard_items() {
        let mut app = test_app();
        app.set_tab(Tab::Retrieve);
        app.transfer_search.results = vec![SuggestionItem::new("ApexClass", "type")];
        app.transfer_search.searched_query = Some(String::new());
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('d')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { retrieve, .. }) => {
                assert!(
                    *retrieve,
                    "a bare type must not switch the dialog to Deploy"
                );
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
        assert!(app.status.contains("exact Type:Name"));

        app.handle_key(key(KeyCode::Esc));
        app.transfer_search = TransferSearchState::new(true);
        app.transfer_search.results = vec![SuggestionItem::new("ApexClass:%.cls", "types")];
        app.transfer_search.searched_query = Some(String::new());
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('d')));
        match app.modal.as_ref() {
            Some(Modal::TransferConfirm { retrieve, .. }) => {
                assert!(*retrieve, "wildcards must not switch the dialog to Deploy");
            }
            other => panic!("expected transfer confirmation, got {other:?}"),
        }
        assert!(!is_explicit_component("ApexClass:*"));
        assert!(is_explicit_component("ApexClass:Demo"));
    }

    #[test]
    fn failed_org_refresh_does_not_replace_a_good_cache_with_an_empty_listing() {
        let mut app = test_app();
        app.org_browse.username = "dev@example.invalid".to_owned();
        app.org_browse.metadata_type = "ApexClass".to_owned();
        let scope = app.org_browse.current_scope();
        let good = vec![OrgComponent {
            metadata_type: "ApexClass".to_owned(),
            full_name: "Demo".to_owned(),
        }];
        app.org_browse.cache.insert(scope.clone(), good.clone());
        app.org_browse.rows = good.clone();

        app.apply_org_metadata(
            scope.clone(),
            "dev@example.invalid".to_owned(),
            "ApexClass".to_owned(),
            app.org_browse.generation,
            Err("synthetic failure".to_owned()),
        );
        assert_eq!(app.org_browse.rows, good);
        assert_eq!(app.org_browse.cache.get(&scope), Some(&good));
        assert!(app.org_browse.error.is_some());
    }

    #[test]
    fn exact_name_input_rejects_invalid_forms_and_preserves_spaces() {
        let mut app = test_app();
        app.org_browse.username = "dev@example.invalid".to_owned();
        app.org_browse.org_label = "dev-alias".to_owned();
        app.org_browse.metadata_type = "ApexClass".to_owned();
        let invalid = [
            "ApexClass",
            "ApexClass:",
            "Unknown:Thing",
            "Flow:My*Flow",
            "ApexClass:   ",
        ];
        for value in invalid {
            assert!(
                !app.submit_prompt(
                    &PromptKind::OrgExactName,
                    &[PromptField::new("Type:FullName", value)]
                ),
                "{value} must be rejected"
            );
        }
        assert!(app.submit_prompt(
            &PromptKind::OrgExactName,
            &[PromptField::new(
                "Type:FullName",
                "apexclass:Org Only Class"
            )]
        ));
        let scope = app.org_browse.current_scope();
        let rows = app.org_browse.exact_rows.get(&scope).expect("exact row");
        assert_eq!(
            rows[0],
            OrgComponent {
                metadata_type: "ApexClass".to_owned(),
                full_name: "Org Only Class".to_owned()
            }
        );
        assert!(
            app.org_browse
                .checked
                .contains(&"ApexClass:Org Only Class".to_owned())
        );
        assert!(matches!(app.modal, Some(Modal::OrgBrowse)));
    }

    #[tokio::test]
    async fn successful_retrieve_requests_a_project_rescan_event() {
        let config = tempfile::tempdir().expect("config dir");
        let mut app = App::new(
            Startup {
                installation: None,
                version: None,
                current_org: None,
                notices: Vec::new(),
            },
            Config::default(),
            config.path().to_path_buf(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut scans = 0usize;
        while Instant::now() < deadline && scans < 2 {
            let event = tokio::time::timeout(Duration::from_secs(3), app.recv_event())
                .await
                .expect("a project event arrives");
            if matches!(event, Some(AppEvent::Project { .. })) {
                scans += 1;
                if scans == 1 {
                    // This is what a successful retrieve does after its result is built.
                    app.rescan_project();
                }
            }
        }
        assert!(
            scans >= 2,
            "expected the startup scan and the post-retrieve rescan"
        );
    }
}
