//! Ratatui rendering. Screens read application state only and never mutate domain rules.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
};

use crate::app::{App, JobSection, JobsFocus, Modal, ResultItem, Tab, TestsFocus};
use crate::json_view;

fn accent(app: &App) -> Color {
    app.accent()
}

/// Bounded memoization of the expensive result/detail line vectors. A job that appends output or
/// finishes bumps the record revision; the elapsed label and render flags are part of the key so a
/// cached vector can never show stale content.
const DETAIL_CACHE_CAPACITY: usize = 8;

#[derive(Clone, PartialEq, Eq)]
struct DetailCacheKey {
    record_id: u64,
    revision: u64,
    width: usize,
    show_details: bool,
    show_json: bool,
    queued_position: Option<usize>,
    accent: Color,
    elapsed: String,
}

#[derive(Default)]
pub(crate) struct RenderCache {
    details: Vec<(DetailCacheKey, Vec<Line<'static>>)>,
}

impl RenderCache {
    fn get(&self, key: &DetailCacheKey) -> Option<&Vec<Line<'static>>> {
        self.details
            .iter()
            .find(|(cached_key, _)| cached_key == key)
            .map(|(_, lines)| lines)
    }

    fn put(&mut self, key: DetailCacheKey, lines: Vec<Line<'static>>) {
        if self.details.len() >= DETAIL_CACHE_CAPACITY {
            self.details.remove(0);
        }
        self.details.push((key, lines));
    }

    pub(crate) fn clear(&mut self) {
        self.details.clear();
    }
}

/// Layout density for embedded terminals (VS Code, Zed, split panes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Density {
    Tiny,
    Compact,
    Roomy,
}

fn density(area: Rect) -> Density {
    if area.width < 72 || area.height < 20 {
        Density::Tiny
    } else if area.width < 105 || area.height < 30 {
        Density::Compact
    } else {
        Density::Roomy
    }
}

/// Split the Tests body between the test list and the coverage table. The list always keeps a
/// usable minimum; when there is not enough room the coverage pane is hidden entirely.
fn tests_pane_heights(body_height: u16, density: Density) -> Option<(u16, u16)> {
    let min_list: u16 = match density {
        Density::Tiny => 3,
        Density::Compact => 4,
        Density::Roomy => 6,
    };
    let min_coverage: u16 = match density {
        Density::Tiny => 4,
        Density::Compact => 5,
        Density::Roomy => 6,
    };
    if body_height < min_list.saturating_add(min_coverage) {
        return None;
    }
    let desired = ((u32::from(body_height) * 40 / 100) as u16).clamp(min_coverage, 14);
    let coverage = desired.min(body_height - min_list);
    Some((body_height - coverage, coverage))
}

fn block(title: impl Into<String>, app: &App) -> Block<'static> {
    Block::bordered()
        .title(title.into())
        .border_style(Style::default().fg(Color::DarkGray))
        .title_style(
            Style::default()
                .fg(accent(app))
                .add_modifier(Modifier::BOLD),
        )
}

/// A pane border that shows which side of the Jobs window has keyboard focus.
fn pane_block(title: impl Into<String>, app: &App, focused: bool) -> Block<'static> {
    let title = title.into();
    Block::bordered()
        .title(if focused { format!(" {title} ") } else { title })
        .border_style(Style::default().fg(if focused {
            accent(app)
        } else {
            Color::DarkGray
        }))
        .title_style(if focused {
            Style::default()
                .fg(accent(app))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        })
}

fn faint(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::DarkGray))
}

/// Faint notice shown when streamed output was dropped by the record retention caps.
const OUTPUT_TRUNCATED_NOTICE: &str = "… streamed output truncated";

fn output_truncated_notice() -> Line<'static> {
    Line::from(faint(OUTPUT_TRUNCATED_NOTICE))
}

fn strong(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(
        text.into(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// Wrap text on word boundaries, hard-splitting a token longer than the available width.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for character in word.chars() {
                chunk.push(character);
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
            }
            current = chunk;
            continue;
        }
        let extra = usize::from(!current.is_empty());
        if current.chars().count() + extra + word_len > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn truncated(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let shortened: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{shortened}…")
}

/// Draw the complete frame, including any modal overlay.
pub fn draw(frame: &mut Frame, app: &App) {
    let density = density(frame.area());
    let footer_height = match density {
        Density::Roomy | Density::Compact => 2,
        Density::Tiny => 1,
    };
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .split(frame.area());
    draw_header(frame, layout[0], app, density);
    match app.tab {
        Tab::Home => draw_home(frame, layout[1], app),
        Tab::Retrieve | Tab::Deploy => draw_transfer(frame, layout[1], app),
        Tab::Tests => draw_tests(frame, layout[1], app),
        Tab::Orgs => draw_orgs(frame, layout[1], app),
        Tab::Setup => draw_setup(frame, layout[1], app),
    }
    draw_footer(frame, layout[2], app, density);
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, app);
    }
    if no_color_enabled() {
        strip_colors(&mut frame.buffer_mut().content);
    }
}

/// `NO_COLOR` is process-wide and never changes at runtime, so check it once instead of on every
/// frame.
fn no_color_enabled() -> bool {
    static NO_COLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some())
}

/// Remove every color from a rendered frame, keeping the reversed marker on the cursor cell.
/// Cells already fully reset are skipped so the common case writes nothing.
fn strip_colors(cells: &mut [ratatui::buffer::Cell]) {
    for cell in cells {
        if cell.fg == Color::Reset && cell.bg == Color::Reset {
            continue;
        }
        let was_cursor = cell.fg == Color::Black && cell.bg != Color::Reset;
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
        if was_cursor {
            cell.modifier.insert(Modifier::REVERSED);
        }
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    // Tiny has a single footer row, so keep it for key hints and show the transient status here
    // instead. Wider densities keep the status on their dedicated footer row.
    let (right_text, right_style) = if density == Density::Tiny && !app.status.is_empty() {
        let style = if app.status_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        let max = area.width.saturating_div(2).saturating_sub(2) as usize;
        (truncated(&app.status, max), style)
    } else {
        let frames = ["-", "\\", "|", "/"];
        let spinner = frames[app.spinner_tick() % frames.len()];
        let activity = if !app.startup_stage.done {
            format!("{spinner} {}", app.startup_stage.label)
        } else if app.busy() {
            format!("{spinner} {} job(s)", app.job_count())
        } else {
            "ready".to_owned()
        };
        let style = if app.startup_stage.done && !app.busy() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Yellow)
        };
        (activity, style)
    };

    let mut left: Vec<Span> = vec![
        strong("SFTUI", accent(app)),
        faint(format!(" {}", env!("CARGO_PKG_VERSION"))),
        Span::styled("  ›  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.tab.label(),
            Style::default()
                .fg(Color::Reset)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    left.push(faint("   "));
    left.push(faint(project_context(app)));
    left.push(faint("   "));
    left.push(faint(org_context(app)));

    let right_width = (right_text.chars().count() as u16 + 1).min(area.width / 2);
    let columns =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(right_width)]).split(area);
    frame.render_widget(Paragraph::new(Line::from(left)), columns[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(right_text, right_style)))
            .alignment(ratatui::layout::Alignment::Right),
        columns[1],
    );
}

/// `project name`, `project …` while scanning, or `no project`.
fn project_context(app: &App) -> String {
    if app.deploy_state.project_loading {
        return "project …".to_owned();
    }
    app.deploy_state.project.as_ref().map_or_else(
        || "no project".to_owned(),
        |project| format!("project {}", project.name()),
    )
}

/// The configured default org, phrased once (no doubled `org no default org`).
fn org_context(app: &App) -> String {
    let org = app.org_target_hint();
    if !org.is_empty() {
        format!("org {org}")
    } else if app.org_state.loading || !app.startup_stage.done {
        "org …".to_owned()
    } else {
        "no default org".to_owned()
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    let status_style = if app.status_error {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    if density == Density::Tiny {
        frame.render_widget(
            Paragraph::new(keycap_line(app, usize::from(area.width))),
            area,
        );
        return;
    }
    let status = if app.status.is_empty() {
        "Ready".to_owned()
    } else {
        truncated(&app.status, area.width.saturating_sub(2) as usize)
    };
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, status_style))),
        sections[0],
    );
    frame.render_widget(
        Paragraph::new(keycap_line(
            app,
            usize::from(sections[1].width.saturating_sub(1)),
        )),
        sections[1],
    );
}

/// Footer context for a screen. Tests has sub-states whose keycaps change with focus and
/// coverage availability, so they are distinct contexts here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HintContext {
    Home,
    Transfer,
    TestsSearch,
    TestsCoverage,
    TestsPlain,
    Orgs,
    Setup,
}

/// One key binding. Both the footer keycaps and the `?` help modal are derived from `KEYMAP`, so a
/// key cannot appear in one and silently drift from the other. `order` drives footer ordering only;
/// the table's own order drives the help modal.
struct KeyBinding {
    /// Keycap shown in the footer; empty for help-only rows.
    cap: &'static str,
    /// Short footer labels, one per context in which the key is shown.
    hints: &'static [(HintContext, &'static str)],
    /// Sort order within a footer.
    order: u8,
    /// Left column and description in the help modal.
    help_key: &'static str,
    help: &'static str,
}

/// Single source of truth for key hints and help. Help-only rows have no `hints`.
const KEYMAP: &[KeyBinding] = &[
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "Menu r/d/c/t/o/e",
        help: "retrieve · deploy · create · tests · organization · run command",
    },
    KeyBinding {
        cap: "d",
        hints: &[(HintContext::Orgs, "default")],
        order: 6,
        help_key: "d (Orgs)",
        help: "make the selected org the default",
    },
    KeyBinding {
        cap: "Enter",
        hints: &[
            (HintContext::Home, "open"),
            (HintContext::Transfer, "confirm"),
            (HintContext::TestsSearch, "confirm"),
            (HintContext::TestsPlain, "confirm"),
            (HintContext::Orgs, "details"),
            (HintContext::Setup, "run"),
        ],
        order: 2,
        help_key: "Enter",
        help: "activate the highlighted entry; search, then confirm where needed",
    },
    KeyBinding {
        cap: "?",
        hints: &[(HintContext::Home, "help"), (HintContext::Setup, "help")],
        order: 11,
        help_key: "?",
        help: "show this help; any key closes it",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "h",
        help: "go to the Menu from any screen",
    },
    KeyBinding {
        cap: "Esc",
        hints: &[
            (HintContext::Transfer, "back"),
            (HintContext::TestsSearch, "back"),
            (HintContext::TestsCoverage, "back"),
            (HintContext::TestsPlain, "back"),
            (HintContext::Orgs, "back"),
            (HintContext::Setup, "back"),
        ],
        order: 12,
        help_key: "Esc",
        help: "back to the previous screen; jobs keep running",
    },
    KeyBinding {
        cap: "q",
        hints: &[(HintContext::Home, "quit")],
        order: 14,
        help_key: "q",
        help: "quit from the Menu; confirms first while a job runs",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "Ctrl+X / Ctrl+C",
        help: "cancel the newest job; Ctrl+C also quits when no job runs",
    },
    KeyBinding {
        cap: "x",
        hints: &[(HintContext::Orgs, "logout")],
        order: 13,
        help_key: "x (Orgs)",
        help: "log out of an org",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "y",
        help: "copy the current command/selection",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "Ctrl+Y",
        help: "copy while typing in a search field",
    },
    KeyBinding {
        cap: "F5",
        hints: &[(HintContext::Orgs, "refresh")],
        order: 15,
        help_key: "F5",
        help: "refresh orgs or component search",
    },
    KeyBinding {
        cap: "r",
        hints: &[(HintContext::Setup, "re-probe")],
        order: 7,
        help_key: "r (Setup)",
        help: "re-probe the Salesforce CLI",
    },
    KeyBinding {
        cap: "F2",
        hints: &[
            (HintContext::Home, "jobs"),
            (HintContext::Transfer, "jobs"),
            (HintContext::TestsSearch, "jobs"),
            (HintContext::TestsCoverage, "jobs"),
            (HintContext::TestsPlain, "jobs"),
            (HintContext::Setup, "jobs"),
        ],
        order: 8,
        help_key: "F2",
        help: "open the jobs window from any screen",
    },
    KeyBinding {
        cap: "F4",
        hints: &[
            (HintContext::Home, "org"),
            (HintContext::Orgs, "switch org"),
        ],
        order: 9,
        help_key: "F4",
        help: "switch the default org from any screen",
    },
    KeyBinding {
        cap: "l",
        hints: &[(HintContext::Orgs, "login")],
        order: 10,
        help_key: "l (Orgs)",
        help: "log in to an org",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "F2 (jobs)",
        help: "↑↓ select · Enter open · Tab move focus · Esc back",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "x / c (jobs)",
        help: "cancel running or queued · clear finished",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "o / z (jobs)",
        help: "raw output · JSON for a finished job",
    },
    KeyBinding {
        cap: "Space",
        hints: &[
            (HintContext::Transfer, "select"),
            (HintContext::TestsSearch, "select"),
            (HintContext::TestsPlain, "select"),
        ],
        order: 3,
        help_key: "Space (search/confirm)",
        help: "select a component, test class or row",
    },
    KeyBinding {
        cap: "Tab",
        hints: &[
            (HintContext::TestsSearch, "coverage"),
            (HintContext::TestsCoverage, "search"),
        ],
        order: 5,
        help_key: "Tab (Tests)",
        help: "toggle the per-class coverage table",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "c (confirm)",
        help: "show the exact command that will run",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "g (confirm)",
        help: "cycle the target org",
    },
    KeyBinding {
        cap: "",
        hints: &[],
        order: 0,
        help_key: "v/t/w (Deploy confirm)",
        help: "validate / test level / ignore warnings",
    },
    KeyBinding {
        cap: "↑↓",
        hints: &[
            (HintContext::Home, "move"),
            (HintContext::TestsCoverage, "scroll"),
        ],
        order: 1,
        help_key: "↑↓ PgUp/PgDn",
        help: "move or scroll lists and results",
    },
    KeyBinding {
        cap: "PgUp/PgDn",
        hints: &[(HintContext::TestsCoverage, "page")],
        order: 4,
        help_key: "↑↓ PgUp/PgDn",
        help: "move or scroll lists and results",
    },
];

fn hint_context(app: &App) -> HintContext {
    match app.tab {
        Tab::Home => HintContext::Home,
        Tab::Retrieve | Tab::Deploy => HintContext::Transfer,
        Tab::Orgs => HintContext::Orgs,
        Tab::Setup => HintContext::Setup,
        Tab::Tests => {
            let has_coverage = app
                .latest_result(crate::app::TaskKind::Tests)
                .is_some_and(|result| !result.coverage.is_empty());
            if !has_coverage {
                HintContext::TestsPlain
            } else if app.tests_focus == TestsFocus::Coverage {
                HintContext::TestsCoverage
            } else {
                HintContext::TestsSearch
            }
        }
    }
}

/// Footer keycaps for a context. Enter shows a single keycap whose label reflects reality:
/// `search` while the query is dirty, otherwise `confirm`/`run`.
fn hint_pairs(ctx: HintContext, search_dirty: bool) -> Vec<(&'static str, &'static str)> {
    let mut pairs: Vec<(u8, &'static str, &'static str)> = KEYMAP
        .iter()
        .filter_map(|binding| {
            binding
                .hints
                .iter()
                .find(|(context, _)| *context == ctx)
                .map(|(_, label)| {
                    let can_search = matches!(
                        ctx,
                        HintContext::Transfer | HintContext::TestsSearch | HintContext::TestsPlain
                    );
                    let label = if binding.cap == "Enter" && can_search && search_dirty {
                        "search"
                    } else {
                        *label
                    };
                    (binding.order, binding.cap, label)
                })
        })
        .collect();
    pairs.sort_by_key(|(order, _, _)| *order);
    pairs
        .into_iter()
        .map(|(_, cap, label)| (cap, label))
        .collect()
}

fn key_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let ctx = hint_context(app);
    let search_dirty = match ctx {
        HintContext::Transfer => app.transfer_search.needs_search(),
        HintContext::TestsSearch | HintContext::TestsPlain => app.test_search.needs_search(),
        _ => false,
    };
    hint_pairs(ctx, search_dirty)
}

fn help_rows() -> Vec<(&'static str, &'static str)> {
    let mut seen: Vec<&'static str> = Vec::new();
    let mut rows = Vec::new();
    for binding in KEYMAP {
        if binding.help.is_empty() || seen.contains(&binding.help_key) {
            continue;
        }
        seen.push(binding.help_key);
        rows.push((binding.help_key, binding.help));
    }
    rows
}

fn keycap_line(app: &App, width: usize) -> Line<'static> {
    keycap_pairs_line(&key_hints(app), app, width)
}

fn keycap_pairs_line(
    pairs: &[(&'static str, &'static str)],
    app: &App,
    width: usize,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (key, label) in pairs {
        let piece = key.chars().count() + label.chars().count() + 3;
        let separator = if spans.is_empty() { 0 } else { 2 };
        if used + separator + piece > width {
            break;
        }
        if separator > 0 {
            spans.push(Span::raw("  "));
            used += separator;
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::default()
                .fg(accent(app))
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(Color::DarkGray),
        ));
        used += piece;
    }
    Line::from(spans)
}

fn draw_home(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = vec![strong_line("MENU", accent(app))];
    for (index, item) in crate::app::MENU_ITEMS.iter().enumerate() {
        let selected = index == app.home_state.selected;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(if selected {
                    accent(app)
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                format!("[{}] ", item.key),
                Style::default()
                    .fg(if selected {
                        accent(app)
                    } else {
                        Color::DarkGray
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                item.label,
                if selected {
                    Style::default()
                        .fg(accent(app))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    if let Some(error) = &app.deploy_state.project_error {
        lines.push(Line::from(" "));
        lines.push(Line::from(faint(truncated(
            error,
            area.width.saturating_sub(2) as usize,
        ))));
    }
    if app.busy() {
        lines.push(Line::from(" "));
        lines.push(Line::from(faint(format!(
            "{} job(s) running or queued",
            app.job_count()
        ))));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_tests(frame: &mut Frame, area: Rect, app: &App) {
    let state = &app.test_search;
    let latest = app.latest_result(crate::app::TaskKind::Tests);
    let has_coverage = latest.is_some_and(|result| !result.coverage.is_empty());
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(area);
    frame.render_widget(
        Line::from(vec![strong("Apex tests", accent(app))]),
        sections[0],
    );
    let query = if state.query.is_empty() {
        "type a test class name".to_owned()
    } else {
        state.query.clone()
    };
    frame.render_widget(
        Line::from(vec![
            strong("Search: ", accent(app)),
            Span::styled(
                format!("{}{}", query, if state.searching { " …" } else { "" }),
                Style::default().fg(Color::Reset),
            ),
        ]),
        sections[1],
    );

    if has_coverage && let Some(result) = latest {
        let body = sections[2];
        if let Some((list_height, coverage_height)) = tests_pane_heights(body.height, density(area))
        {
            let rows = Layout::vertical([
                Constraint::Length(list_height),
                Constraint::Length(coverage_height),
            ])
            .split(body);
            render_test_list(frame, rows[0], app, state);
            draw_test_coverage(frame, rows[1], app, result);
        } else {
            // Too small for both panes: give the whole body to the test list.
            render_test_list(frame, body, app, state);
        }
    } else {
        let latest_lines = latest_result_lines(app, crate::app::TaskKind::Tests);
        let rows = Layout::vertical([
            Constraint::Min(2),
            Constraint::Length(latest_lines.len() as u16),
        ])
        .split(sections[2]);
        render_test_list(frame, rows[0], app, state);
        if !latest_lines.is_empty() {
            frame.render_widget(Paragraph::new(Text::from(latest_lines)), rows[1]);
        }
    }

    let footer = if has_coverage && app.tests_focus == TestsFocus::Coverage {
        let classes = latest.map_or(0, |result| result.coverage.len());
        format!("coverage focused — {classes} classes")
    } else {
        let mut text = format!("{} selected", state.checked().len());
        if state.searching {
            text.push_str("   searching…");
        } else if state.needs_search() {
            text.push_str("   results out of date");
        }
        if has_coverage {
            text.push_str("   coverage available");
        }
        text
    };
    frame.render_widget(Paragraph::new(Line::from(faint(footer))), sections[3]);
}

fn render_test_list(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    state: &crate::app::TransferSearchState,
) {
    let items: Vec<ListItem> = state
        .results
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let checked = state
                .selected
                .iter()
                .find(|entry| entry.item.value == item.value)
                .is_some_and(|entry| entry.checked);
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{} [{}] ",
                            if index == state.cursor { "›" } else { " " },
                            if checked { "✓" } else { " " }
                        ),
                        Style::default().fg(if checked { accent(app) } else { Color::Reset }),
                    ),
                    Span::styled(item.value.clone(), Style::default().fg(Color::Reset)),
                ]),
                Line::from(Span::styled(
                    format!(
                        "    {}",
                        truncated(&item.path, usize::from(area.width).saturating_sub(6))
                    ),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();
    let mut list_state = ListState::default();
    if !state.results.is_empty() {
        list_state.select(Some(state.cursor.min(state.results.len() - 1)));
    }
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut list_state,
    );
}

fn draw_test_coverage(frame: &mut Frame, area: Rect, app: &App, result: &crate::app::ResultView) {
    let focused = app.tests_focus == TestsFocus::Coverage;
    let cover_block = pane_block("coverage", app, focused);
    frame.render_widget(cover_block.clone(), area);
    let inner = cover_block.inner(area);
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let color = if result.ok { Color::Green } else { Color::Red };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("[{}] ", if result.ok { "✓" } else { "✗" }),
                Style::default().fg(color),
            ),
            strong("LATEST RESULT ", accent(app)),
            Span::styled(
                format!("{} · {}", result.status, result.summary),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        sections[0],
    );
    let visible = usize::from(sections[1].height).saturating_sub(3).max(1);
    app.tests_coverage_rows.set(visible);
    let (lines, _) = coverage_table_lines(
        &result.coverage,
        usize::from(sections[1].width),
        app.coverage_scroll,
        visible,
    );
    frame.render_widget(Paragraph::new(Text::from(lines)), sections[1]);
}

/// Percentage points parsed from the stored coverage string, falling back to covered/total.
fn coverage_percent(item: &ResultItem) -> f64 {
    if let Some(percent) = item
        .detail
        .split_whitespace()
        .next()
        .and_then(|token| token.trim_end_matches('%').parse::<f64>().ok())
    {
        return percent;
    }
    if let Some((covered, total)) = item.progress
        && total > 0
    {
        return covered as f64 * 100.0 / total as f64;
    }
    0.0
}

/// Shared per-class coverage table: worst percentage first, then class name.
/// Returns the rendered lines and the maximum scroll offset for `max_rows`.
pub(crate) fn coverage_table_lines(
    coverages: &[ResultItem],
    width: usize,
    scroll: usize,
    max_rows: usize,
) -> (Vec<Line<'static>>, usize) {
    if coverages.is_empty() {
        return (
            vec![Line::from(faint(
                "No per-class coverage returned by the CLI.",
            ))],
            0,
        );
    }
    let mut sorted: Vec<(&ResultItem, f64)> = coverages
        .iter()
        .map(|item| (item, coverage_percent(item)))
        .collect();
    sorted.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.label.cmp(&right.0.label))
    });
    let total = sorted.len();
    let max_rows = max_rows.max(1);
    let max_scroll = total.saturating_sub(max_rows);
    let scroll = scroll.min(max_scroll);
    // The bar is the first thing to drop when the pane is narrow, so the numbers stay readable.
    let show_bar = width >= 56;
    let name_width = if show_bar {
        width.saturating_sub(38).clamp(10, 40)
    } else {
        width.saturating_sub(20).clamp(8, 40)
    };
    let header = if show_bar {
        format!(
            "{:<name_width$}  {:>7}  {:>8}  {}",
            "CLASS", "COVERAGE", "LINES", " "
        )
    } else {
        format!(
            "{:<name_width$}  {:>7}  {:>8}",
            "CLASS", "COVERAGE", "LINES"
        )
    };
    let mut lines = vec![
        Line::from(faint(format!("{total} classes · worst first"))),
        Line::from(" "),
        Line::from(Span::styled(
            truncated(&header, width),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    for (item, percent) in sorted.iter().skip(scroll).take(max_rows) {
        let name = truncated(&item.label, name_width);
        let counts = item
            .progress
            .map(|(covered, total)| format!("{covered}/{total}"))
            .unwrap_or_else(|| "-".to_owned());
        let mut text = format!("{name:<name_width$}  {percent:>7.1}%  {counts:>8}");
        if show_bar
            && let Some((covered, total)) = item.progress
            && total > 0
        {
            text.push_str(&format!("  {}", coverage_bar(covered, total)));
        }
        lines.push(Line::from(Span::styled(
            truncated(&text, width),
            Style::default().fg(Color::Reset),
        )));
    }
    (lines, max_scroll)
}

fn strong_line(text: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn draw_orgs(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let show_status = density == Density::Compact;
        let rows: Vec<Row> = app
            .org_state
            .orgs
            .iter()
            .map(|org| {
                let widths = usize::from(area.width);
                let mut cells = vec![
                    if org.is_default { "*" } else { " " }.to_owned(),
                    truncated(&org.label(), widths.saturating_sub(14).min(20)),
                ];
                if show_status {
                    cells.push(truncated(&org.connected_status, 12));
                }
                Row::new(cells)
            })
            .collect();
        let widths: Vec<Constraint> = if show_status {
            vec![
                Constraint::Length(2),
                Constraint::Min(12),
                Constraint::Length(12),
            ]
        } else {
            vec![Constraint::Length(2), Constraint::Min(12)]
        };
        let mut state = TableState::default();
        state.select(if app.org_state.orgs.is_empty() {
            None
        } else {
            Some(app.org_state.selected)
        });
        let mut header = vec!["", "Alias"];
        if show_status {
            header.push("Status");
        }
        frame.render_stateful_widget(
            Table::new(rows, widths)
                .header(Row::new(header).style(Style::default().fg(accent(app))))
                .block(block("orgs", app))
                .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("›"),
            area,
            &mut state,
        );
        if app.org_state.orgs.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(faint(if app.org_state.loading {
                    "Loading orgs…"
                } else {
                    "No orgs loaded — F5 to refresh"
                }))),
                area,
            );
        }
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);
    let rows: Vec<Row> = app
        .org_state
        .orgs
        .iter()
        .map(|org| {
            let marker = if org.is_default && org.is_devhub {
                "★◆"
            } else if org.is_default {
                "★ "
            } else if org.is_devhub {
                "◆ "
            } else {
                "  "
            };
            Row::new(vec![
                marker.to_owned(),
                org.label(),
                org.username.clone(),
                if org.scratch { "scratch" } else { "full" }.to_owned(),
                org.connected_status.clone(),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Percentage(25),
        Constraint::Percentage(35),
        Constraint::Length(9),
        Constraint::Min(12),
    ];
    let mut state = TableState::default();
    state.select(if app.org_state.orgs.is_empty() {
        None
    } else {
        Some(app.org_state.selected)
    });
    let table = Table::new(rows, widths)
        .header(
            Row::new(["", "Alias", "Username", "Type", "Status"])
                .style(Style::default().fg(accent(app))),
        )
        .block(block(
            if app.org_state.loading {
                "orgs — loading…"
            } else {
                "orgs"
            },
            app,
        ))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("›");
    frame.render_stateful_widget(table, columns[0], &mut state);

    let detail_width = usize::from(columns[1].width).saturating_sub(2); // block borders
    let key_width = detail_width.saturating_sub(20).clamp(8, 22);
    let value_width = detail_width.saturating_sub(key_width + 2).max(1);
    let mut lines = vec![strong_line("ORG DETAIL", accent(app))];
    let detail_matches = app
        .selected_org()
        .is_some_and(|org| app.org_state.detail_for.as_deref() == Some(org.username.as_str()));
    if detail_matches && let Some(detail) = &app.org_state.detail {
        lines.push(Line::from(faint(format!(
            "target: {}",
            app.org_state.detail_for.as_deref().unwrap_or("")
        ))));
        for (key, value) in detail.fields.iter().take(24) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<key_width$}", truncated(key, key_width)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    truncated(value, value_width),
                    Style::default().fg(Color::Reset),
                ),
            ]));
        }
    } else {
        lines.push(Line::from(faint(
            "Enter loads sf org display for the selected org.",
        )));
        lines.push(Line::from(faint("Credentials are never displayed.")));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("organization", app)),
        columns[1],
    );
}

fn latest_result_lines(app: &App, kind: crate::app::TaskKind) -> Vec<Line<'static>> {
    let Some(result) = app.latest_result(kind) else {
        return Vec::new();
    };
    let color = if result.ok { Color::Green } else { Color::Red };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("[{}] ", if result.ok { "✓" } else { "✗" }),
            Style::default().fg(color),
        ),
        strong("LATEST RESULT ", accent(app)),
        Span::styled(
            format!("{} · {}", result.status, result.summary),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("   F2 jobs", Style::default().fg(Color::DarkGray)),
    ])];
    for item in result.items.iter().take(4) {
        lines.push(result_item_line(item));
    }
    if result.items.len() > 4 {
        lines.push(Line::from(faint(format!(
            "   … {} more · F2 opens the jobs window",
            result.items.len() - 4
        ))));
    }
    lines
}

fn draw_transfer(frame: &mut Frame, area: Rect, app: &App) {
    let state = &app.transfer_search;
    let kind = if state.retrieve {
        crate::app::TaskKind::Retrieve
    } else {
        crate::app::TaskKind::Deploy
    };
    let latest = latest_result_lines(app, kind);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(latest.len() as u16),
        Constraint::Length(1),
    ])
    .split(area);
    let title = if state.retrieve { "Retrieve" } else { "Deploy" };
    let mut heading = vec![strong(title, accent(app))];
    if let Some(error) = &app.deploy_state.project_error {
        heading.push(faint(format!(
            "   {}",
            truncated(error, usize::from(area.width).saturating_sub(16))
        )));
    }
    frame.render_widget(Line::from(heading), sections[0]);
    let query = if state.query.is_empty() {
        "type a component name".to_owned()
    } else {
        state.query.clone()
    };
    frame.render_widget(
        Line::from(vec![
            strong("Search: ", accent(app)),
            Span::styled(
                format!("{query}{}", if state.searching { " …" } else { "" }),
                Style::default().fg(Color::Reset),
            ),
        ]),
        sections[1],
    );
    let items: Vec<ListItem> = state
        .results
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let checked = state
                .selected
                .iter()
                .find(|entry| entry.item.value == item.value)
                .is_some_and(|entry| entry.checked);
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{} [{}] ",
                            if index == state.cursor { "›" } else { " " },
                            if checked { "✓" } else { " " }
                        ),
                        Style::default().fg(if checked { accent(app) } else { Color::Reset }),
                    ),
                    Span::styled(item.value.clone(), Style::default().fg(Color::Reset)),
                ]),
                Line::from(Span::styled(
                    format!(
                        "    {}",
                        truncated(&item.path, usize::from(area.width).saturating_sub(6))
                    ),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();
    let mut list_state = ListState::default();
    if !state.results.is_empty() {
        list_state.select(Some(state.cursor.min(state.results.len() - 1)));
    }
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        sections[2],
        &mut list_state,
    );
    if !latest.is_empty() {
        frame.render_widget(Paragraph::new(Text::from(latest)), sections[3]);
    }
    let count = state.checked().len();
    let status = if state.searching {
        "searching…"
    } else if state.needs_search() {
        "results out of date"
    } else {
        ""
    };
    frame.render_widget(
        Line::from(vec![
            faint(format!("{count} selected")),
            faint(format!("   {status}")),
        ]),
        sections[4],
    );
}

fn summary_chips(result: &crate::app::ResultView) -> Line<'static> {
    let mut spans = Vec::new();
    // Severity comes from structured result state, not from the chip text: a label that merely
    // contains "fail" must not look like an error when the result actually succeeded.
    let failed_items = result.items.iter().filter(|item| !item.ok).count();
    let has_failure = !result.ok || failed_items > 0;
    for (index, part) in result
        .summary
        .split('·')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let count = part
            .split_whitespace()
            .next()
            .and_then(|token| token.trim_end_matches('%').parse::<f64>().ok())
            .unwrap_or(1.0);
        let lower = part.to_ascii_lowercase();
        let color = if (lower.contains("fail") || lower.contains("error")) && has_failure {
            if count > 0.0 { Color::Red } else { Color::Gray }
        } else if lower.contains("pass") || lower.contains("succeed") {
            if count > 0.0 {
                Color::Green
            } else {
                Color::Gray
            }
        } else if lower.contains("coverage") || lower.contains('%') {
            Color::Yellow
        } else {
            Color::Gray
        };
        spans.push(Span::styled(part.to_owned(), Style::default().fg(color)));
    }
    Line::from(spans)
}

fn coverage_bar(covered: usize, total: usize) -> String {
    if total == 0 {
        return String::new();
    }
    let width = 12usize;
    let filled = (covered * width).div_ceil(total).min(width);
    let mut bar = String::new();
    for index in 0..width {
        bar.push(if index < filled { '█' } else { '░' });
    }
    bar
}

fn result_item_line(item: &crate::app::ResultItem) -> Line<'static> {
    let icon = if item.ok { "✓" } else { "✗" };
    let color = if item.ok { Color::Green } else { Color::Red };
    let mut spans = vec![
        Span::styled(format!("[{icon}] "), Style::default().fg(color)),
        Span::styled(item.label.clone(), Style::default().fg(Color::Reset)),
    ];
    if let Some((covered, total)) = item.progress {
        let pct = (covered * 100).checked_div(total).unwrap_or(0);
        let bar_color = if pct >= 80 {
            Color::Green
        } else if pct >= 50 {
            Color::Yellow
        } else {
            Color::Red
        };
        spans.push(Span::styled(
            format!(" {} ", coverage_bar(covered, total)),
            Style::default().fg(bar_color),
        ));
    }
    if !item.detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", item.detail),
            Style::default().fg(if item.ok { Color::DarkGray } else { Color::Red }),
        ));
    }
    Line::from(spans)
}

fn result_lines(
    app: &App,
    record: &crate::app::OutputRecord,
    result: &crate::app::ResultView,
    width: usize,
) -> Vec<Line<'static>> {
    let color = if result.ok { Color::Green } else { Color::Red };
    let mut lines = vec![
        strong(&result.title, accent(app))
            + Span::styled("  ", Style::default().fg(Color::DarkGray))
            + strong(&result.status, color),
        Line::from(Span::styled(
            "─".repeat(width.saturating_sub(2)),
            Style::default().fg(Color::DarkGray),
        )),
        summary_chips(result),
        Line::from(" "),
    ];
    for item in result.items.iter().take(200) {
        lines.push(result_item_line(item));
    }
    if result.items.len() > 200 {
        lines.push(Line::from(faint(format!(
            "… {} more items",
            result.items.len() - 200
        ))));
    }
    if !result.coverage.is_empty() {
        lines.push(Line::from(" "));
        let (table, _) = coverage_table_lines(&result.coverage, width, 0, usize::MAX);
        lines.extend(table);
    }
    for section in &result.sections {
        lines.push(Line::from(" "));
        let header_color = if section.title.to_ascii_uppercase().contains("ERROR") {
            Color::Red
        } else {
            accent(app)
        };
        lines.push(strong_line(&section.title, header_color));
        for item in section.rows.iter().take(200) {
            lines.push(result_item_line(item));
        }
        if section.rows.len() > 200 {
            lines.push(Line::from(faint(format!(
                "… {} more rows",
                section.rows.len() - 200
            ))));
        }
    }
    if app.output_state.show_details {
        lines.push(Line::from(" "));
        if app.output_state.show_json
            && let Some(value) = &record.json
        {
            lines.push(strong_line("JSON", accent(app)));
            let view = json_view::table(value);
            for row in view.rows.iter().take(500) {
                lines.push(Line::from(faint(row.join(" | "))));
            }
            if view.rows.len() > 500 {
                lines.push(Line::from(faint(format!(
                    "… {} more JSON rows",
                    view.rows.len() - 500
                ))));
            }
        } else {
            lines.push(strong_line("RAW OUTPUT", accent(app)));
            for line in &record.lines {
                lines.push(Line::from(Span::styled(
                    line.text.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if record.lines.is_empty() {
                lines.push(Line::from(faint("No streamed output retained.")));
            }
            if record.lines_truncated() {
                lines.push(output_truncated_notice());
            }
        }
    }
    lines
}

fn job_state_label(record: &crate::app::OutputRecord) -> &'static str {
    if record.queued {
        "queued"
    } else if record.pending_report || !record.finished {
        "running"
    } else {
        match record.exit_code {
            Some(0) => "ok",
            Some(_) => "failed",
            None => "stopped",
        }
    }
}

fn job_detail_lines(
    app: &App,
    record: &crate::app::OutputRecord,
    queued_position: Option<usize>,
    width: usize,
) -> Vec<Line<'static>> {
    let now = std::time::Instant::now();
    let key = DetailCacheKey {
        record_id: record.id,
        revision: record.render_revision,
        width,
        show_details: app.output_state.show_details,
        show_json: app.output_state.show_json,
        queued_position,
        accent: accent(app),
        // The elapsed label is part of the drawn header; keying on it keeps the clock accurate
        // without rebuilding the whole detail pane on every animation frame.
        elapsed: record.elapsed_label(now),
    };
    {
        let cache = app.render_cache.borrow();
        if let Some(lines) = cache.get(&key) {
            return lines.clone();
        }
    }
    let lines = build_job_detail_lines(app, record, queued_position, width, now);
    app.render_cache.borrow_mut().put(key, lines.clone());
    lines
}

fn build_job_detail_lines(
    app: &App,
    record: &crate::app::OutputRecord,
    queued_position: Option<usize>,
    width: usize,
    now: std::time::Instant,
) -> Vec<Line<'static>> {
    let (glyph, color) = record_status(record);
    let elapsed = record.elapsed_label(now);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            strong(format!("#{} ", record.id), accent(app)),
            strong(record.task.label(), accent(app)),
            Span::styled(
                format!("  {} · {}", job_state_label(record), elapsed),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(Span::styled(
            truncated(&record.display_command(), width),
            Style::default().fg(Color::Reset),
        )),
    ];
    if let Some(position) = queued_position {
        lines.push(Line::from(faint(format!(
            "queue position {position} — waits for a free job slot"
        ))));
    }
    lines.push(Line::from(" "));
    if record.pending_report {
        lines.push(Line::from(faint("running — waiting for org results")));
        lines.push(Line::from(" "));
    }
    if let Some(result) = &record.result {
        lines.extend(result_lines(app, record, result, width));
    } else if app.output_state.show_json && record.finished {
        lines.push(strong_line("JSON", accent(app)));
        if let Some(value) = &record.json {
            let view = json_view::table(value);
            for row in view.rows.iter().take(500) {
                lines.push(Line::from(faint(row.join(" | "))));
            }
            if view.rows.len() > 500 {
                lines.push(Line::from(faint(format!(
                    "… {} more JSON rows",
                    view.rows.len() - 500
                ))));
            }
        } else {
            lines.push(Line::from(faint("No JSON captured.")));
        }
    } else {
        lines.push(strong_line("OUTPUT", accent(app)));
        if record.lines.is_empty() {
            lines.push(Line::from(faint(if record.finished {
                "No streamed output retained."
            } else {
                "Waiting for output…"
            })));
        }
        for line in &record.lines {
            lines.push(Line::from(Span::styled(
                line.text.clone(),
                Style::default().fg(if line.stream == crate::sf::runner::OutputStream::Stderr {
                    Color::Red
                } else {
                    Color::Gray
                }),
            )));
        }
        if record.lines_truncated() {
            lines.push(output_truncated_notice());
        }
    }
    lines
}

/// Scrollable length of the detail pane; the key handler clamps against it.
pub(crate) fn job_detail_len(
    app: &App,
    record: &crate::app::OutputRecord,
    queued_position: Option<usize>,
) -> usize {
    job_detail_lines(app, record, queued_position, 80).len()
}

fn draw_setup(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(strong("Setup", accent(app)))),
            sections[0],
        );
        let command_width = usize::from(sections[1].width).saturating_sub(6).max(1);
        let items: Vec<ListItem> = crate::sf::install_instructions()
            .iter()
            .enumerate()
            .map(|(index, (name, command))| {
                ListItem::new(Line::from(vec![
                    strong(format!("{} ", index + 1), accent(app)),
                    Span::styled((*name).to_owned(), Style::default().fg(Color::Reset)),
                    faint(truncated(&format!("  {command}"), command_width)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(app.setup_selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(block("installers", app))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("›"),
            sections[1],
            &mut state,
        );
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    let instructions = crate::sf::install_instructions();
    let items: Vec<ListItem> = instructions
        .iter()
        .enumerate()
        .map(|(index, (name, command))| {
            ListItem::new(vec![
                Line::from(strong(format!("{}. {}", index + 1, name), accent(app))),
                Line::from(Span::styled(
                    format!("   {command}"),
                    Style::default().fg(Color::Reset),
                )),
            ])
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(
        app.setup_selected.min(instructions.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(
        List::new(items)
            .block(block(
                "Setup — install the Salesforce CLI without admin rights",
                app,
            ))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› "),
        columns[0],
        &mut state,
    );
    let mut lines = vec![
        strong_line("AFTER INSTALLING", accent(app)),
        Line::from(faint("1. Open a fresh terminal so PATH is updated.")),
        Line::from(faint("2. Return here and press r to re-probe.")),
        Line::from(faint(
            "3. If sf is in a custom location, set SFTUI_SF_PATH to its full path.",
        )),
        Line::from(" "),
    ];
    for notice in app.startup.notices.iter().take(6) {
        lines.push(Line::from(Span::styled(
            truncated(notice, columns[1].width.saturating_sub(4) as usize),
            Style::default().fg(Color::Gray),
        )));
    }
    lines.push(Line::from(" "));
    lines.push(Line::from(faint(
        "Enter runs the selected installer; output appears in the Jobs window (F2).",
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("next steps", app)),
        columns[1],
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width
        .min(area.width.saturating_sub(2))
        .max(20)
        .min(area.width);
    let height = height
        .min(area.height.saturating_sub(2))
        .max(5)
        .min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn record_status(record: &crate::app::OutputRecord) -> (&'static str, Color) {
    if record.queued {
        ("…", Color::Yellow)
    } else if record.pending_report || !record.finished {
        ("▶", Color::Yellow)
    } else {
        match record.exit_code {
            Some(0) => ("✓", Color::Green),
            Some(_) => ("✗", Color::Red),
            None => ("■", Color::Yellow),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct JobsView {
    selected: usize,
    focus: JobsFocus,
    scroll: usize,
    follow: bool,
}

fn draw_jobs(
    frame: &mut Frame,
    app: &App,
    selected: usize,
    focus: JobsFocus,
    scroll: usize,
    follow: bool,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let rows = app.job_rows();
    let running = rows
        .iter()
        .filter(|row| row.section == JobSection::Running)
        .count();
    let queued = rows
        .iter()
        .filter(|row| row.section == JobSection::Queued)
        .count();
    let finished = rows
        .iter()
        .filter(|row| row.section == JobSection::Finished)
        .count();
    let title = if rows.is_empty() {
        "Jobs".to_owned()
    } else {
        format!(
            "Jobs — {} running · {} queued · {} finished",
            running, queued, finished
        )
    };
    let outer = block(title, app);
    frame.render_widget(outer.clone(), area);
    let inner = outer.inner(area);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(faint(
                "No jobs yet — start a retrieve, deploy or test run.",
            ))),
            inner,
        );
        return;
    }
    let sections = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(inner);
    let view = JobsView {
        selected: selected.min(rows.len().saturating_sub(1)),
        focus,
        scroll,
        follow,
    };
    if area.width >= 100 {
        let columns = Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)])
            .split(sections[0]);
        draw_job_list(frame, columns[0], app, &rows, view);
        draw_job_detail(frame, columns[1], app, &rows, view);
    } else {
        // Stacked layout: give the list only the rows it needs (up to half the window) so the
        // selected job's result keeps as much vertical space as possible.
        let wanted = (rows.len() as u16).saturating_add(5);
        let list_height = wanted.min((sections[0].height / 2).max(5));
        let panes = Layout::vertical([Constraint::Length(list_height), Constraint::Min(3)])
            .split(sections[0]);
        draw_job_list(frame, panes[0], app, &rows, view);
        draw_job_detail(frame, panes[1], app, &rows, view);
    }
    let record = rows.get(view.selected).map(|row| row.record);
    let live = rows
        .get(view.selected)
        .is_some_and(|row| row.section != JobSection::Finished);
    let hints = jobs_hint_pairs(record, view.focus, live, view.follow);
    frame.render_widget(
        Paragraph::new(keycap_pairs_line(&hints, app, usize::from(inner.width))),
        sections[1],
    );
}

fn draw_job_list(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    rows: &[crate::app::JobRow<'_>],
    view: JobsView,
) {
    let width = usize::from(area.width).saturating_sub(4);
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 3);
    let mut selected_item = None;
    let mut section = None;
    for (index, row) in rows.iter().enumerate() {
        if section != Some(row.section) {
            section = Some(row.section);
            items.push(ListItem::new(Line::from(strong(
                format!(" {}", row.section.title()),
                accent(app),
            ))));
        }
        if index == view.selected {
            selected_item = Some(items.len());
        }
        let (glyph, color) = record_status(row.record);
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {glyph} "), Style::default().fg(color)),
            Span::styled(
                job_row_text(row.record, width.saturating_sub(2)),
                Style::default().fg(Color::Reset),
            ),
        ])));
    }
    let mut state = ListState::default();
    state.select(selected_item);
    let title = if view.focus == JobsFocus::List {
        "› job queue"
    } else {
        "job queue"
    };
    let list = List::new(items)
        .block(pane_block(title, app, view.focus == JobsFocus::List))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Reset)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("›");
    frame.render_stateful_widget(list, area, &mut state);
}

/// One row per job: `✓ #2  Apex tests  sf apex …   7s  4/4 ✓`.
fn job_row_text(record: &crate::app::OutputRecord, width: usize) -> String {
    let elapsed = record.elapsed_label(std::time::Instant::now());
    let summary = record.outcome_summary();
    let tail = if summary.is_empty() {
        elapsed
    } else {
        format!("{elapsed}  {summary}")
    };
    let head = format!("#{:<3} {:<11} ", record.id, record.task.label());
    let budget = width.saturating_sub(head.chars().count() + tail.chars().count() + 2);
    let command = truncated(&record.display_command(), budget.max(1));
    truncated(&format!("{head}{command}  {tail}"), width)
}

fn draw_job_detail(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    rows: &[crate::app::JobRow<'_>],
    view: JobsView,
) {
    let Some(row) = rows.get(view.selected) else {
        frame.render_widget(
            Paragraph::new(Line::from(faint("No job selected")))
                .block(pane_block("result", app, false)),
            area,
        );
        return;
    };
    let record = row.record;
    let live = row.section != JobSection::Finished;
    let queued_position = rows
        .iter()
        .filter(|row| row.section == JobSection::Queued)
        .position(|row| row.record.id == record.id)
        .map(|index| index + 1);
    let title = if live {
        format!("live output — #{} {}", record.id, record.task.label())
    } else {
        format!("result — #{} {}", record.id, record.task.label())
    };
    let outer = pane_block(&title, app, view.focus == JobsFocus::Detail);
    frame.render_widget(outer.clone(), area);
    let inner = outer.inner(area);
    app.jobs_detail_height.set(inner.height);
    let lines = job_detail_lines(app, record, queued_position, usize::from(inner.width));
    let max_scroll = lines.len().saturating_sub(usize::from(inner.height));
    let top = if live && view.follow {
        max_scroll
    } else {
        view.scroll.min(max_scroll)
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines)).scroll((top.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
}

/// Context-sensitive keycaps for the Jobs window, most useful key first.
fn jobs_hint_pairs(
    record: Option<&crate::app::OutputRecord>,
    focus: JobsFocus,
    live: bool,
    follow: bool,
) -> Vec<(&'static str, &'static str)> {
    if focus == JobsFocus::List {
        let mut hints = vec![
            ("↑↓", "select"),
            ("Enter", "open"),
            ("Esc", "close"),
            ("Tab", "detail"),
            ("x", "cancel"),
            ("c", "clear finished"),
        ];
        if record.is_some_and(|record| record.queued) {
            hints[3] = ("x", "dequeue");
        }
        return hints;
    }
    if live {
        let mut hints = vec![("Tab", "list"), ("Esc", "back"), ("PgUp/PgDn", "scroll")];
        if follow {
            hints.push(("↑", "pause follow"));
        } else {
            hints.push(("End", "follow"));
        }
        if record.is_some_and(crate::app::OutputRecord::is_running) {
            hints.push(("x", "cancel"));
        }
        return hints;
    }
    vec![
        ("Tab", "list"),
        ("Esc", "back"),
        ("j/k", "scroll"),
        ("o", "raw output"),
        ("z", "JSON"),
    ]
}

fn draw_modal(frame: &mut Frame, modal: &Modal, app: &App) {
    match modal {
        Modal::Help { scroll } => {
            let area = centered(frame.area(), 86, frame.area().height.min(24));
            frame.render_widget(Clear, area);
            let help = help_rows();
            let lines: Vec<Line> = help
                .iter()
                .map(|(keys, description)| {
                    Line::from(vec![
                        strong(format!("  {keys:<20}"), accent(app)),
                        Span::styled((*description).to_owned(), Style::default().fg(Color::Gray)),
                    ])
                })
                .collect();
            let title = block("keyboard shortcuts — ↑↓ scroll · any other key closes", app);
            frame.render_widget(title.clone(), area);
            let inner = title.inner(area);
            let max_scroll = lines.len().saturating_sub(usize::from(inner.height));
            frame.render_widget(
                Paragraph::new(Text::from(lines))
                    .scroll(((*scroll).min(max_scroll).min(u16::MAX as usize) as u16, 0)),
                inner,
            );
        }
        Modal::Message { title, message } => {
            let area = centered(frame.area(), 68, 8);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(message.clone()).block(block(title.clone(), app)),
                area,
            );
        }
        Modal::Confirm { title, message, .. } => {
            let width = 76u16.min(frame.area().width);
            let inner_width = usize::from(width).saturating_sub(4).max(10);
            let mut lines: Vec<Line> = Vec::new();
            for paragraph in message.split('\n') {
                if paragraph.is_empty() {
                    lines.push(Line::from(" "));
                    continue;
                }
                for chunk in wrap_text(paragraph, inner_width) {
                    lines.push(Line::from(Span::styled(
                        chunk,
                        Style::default().fg(Color::Reset),
                    )));
                }
            }
            lines.push(Line::from(" "));
            lines.push(Line::from(faint(
                "Enter / y — confirm       Esc / n — cancel",
            )));
            let height = (lines.len() as u16 + 2).min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), width, height);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block(title.clone(), app)),
                area,
            );
        }
        Modal::SwitchOrg { selected } => {
            let width = 78u16.min(frame.area().width);
            let rows = app.org_state.orgs.len();
            let height = (rows as u16)
                .saturating_add(6)
                .min(frame.area().height.saturating_sub(2))
                .max(7);
            let area = centered(frame.area(), width, height);
            frame.render_widget(Clear, area);
            let outer = pane_block("Switch default org", app, true);
            frame.render_widget(outer.clone(), area);
            let inner = outer.inner(area);
            let sections =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
            let row_width = usize::from(sections[0].width).saturating_sub(1); // highlight symbol
            let label_width = row_width.saturating_sub(20).clamp(8, 24);
            let username_width = row_width.saturating_sub(label_width + 6).clamp(8, 30);
            if app.org_state.orgs.is_empty() {
                let text = if app.org_state.loading {
                    "Loading orgs…"
                } else {
                    "No orgs yet — press l to log in, F5 to refresh"
                };
                frame.render_widget(Paragraph::new(Line::from(faint(text))), sections[0]);
            } else {
                let items: Vec<ListItem> = app
                    .org_state
                    .orgs
                    .iter()
                    .map(|org| {
                        let marker = if org.is_default { "★" } else { " " };
                        let mut spans = vec![
                            Span::styled(
                                format!(" {marker} "),
                                Style::default().fg(if org.is_default {
                                    accent(app)
                                } else {
                                    Color::DarkGray
                                }),
                            ),
                            Span::styled(
                                truncated(&org.label(), label_width),
                                Style::default()
                                    .fg(Color::Reset)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ];
                        if org.label() != org.username {
                            spans.push(faint(format!(
                                "  {}",
                                truncated(&org.username, username_width)
                            )));
                        }
                        if !org.connected_status.is_empty() {
                            spans.push(faint(format!("  {}", org.connected_status)));
                        }
                        ListItem::new(Line::from(spans))
                    })
                    .collect();
                let mut state = ListState::default();
                state.select(Some((*selected).min(rows.saturating_sub(1))));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .fg(Color::Reset)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol("›"),
                    sections[0],
                    &mut state,
                );
            }
            frame.render_widget(
                Paragraph::new(keycap_pairs_line(
                    &[
                        ("↑↓", "select"),
                        ("Enter", "switch"),
                        ("F5", "refresh"),
                        ("l", "log in"),
                        ("Esc", "close"),
                    ],
                    app,
                    usize::from(inner.width),
                )),
                sections[1],
            );
        }
        Modal::Jobs {
            selected,
            focus,
            scroll,
            follow,
        } => draw_jobs(frame, app, *selected, *focus, *scroll, *follow),
        Modal::TestConfirm {
            entries,
            focus,
            show_command,
            org,
        } => {
            let modal_width = 90u16.min(frame.area().width);
            let path_width = usize::from(modal_width).saturating_sub(6).max(1);
            let mut lines = vec![
                Line::from(strong(
                    format!(
                        "Run {} test classes in {}",
                        entries.iter().filter(|entry| entry.checked).count(),
                        org.as_deref()
                            .filter(|org| !org.is_empty())
                            .unwrap_or("default org")
                    ),
                    accent(app),
                )),
                Line::from(" "),
                Line::from(faint(
                    "[Enter] run   [Esc] back   [Space] toggle   [c] command   [g] org",
                )),
                Line::from(" "),
            ];
            for (index, entry) in entries.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} [{}] {}",
                        if index == *focus { "›" } else { " " },
                        if entry.checked { "✓" } else { " " },
                        entry.item.value
                    ),
                    Style::default()
                        .fg(if entry.checked {
                            Color::Reset
                        } else {
                            Color::DarkGray
                        })
                        .add_modifier(if index == *focus {
                            Modifier::REVERSED
                        } else {
                            Modifier::empty()
                        }),
                )));
                lines.push(Line::from(Span::styled(
                    format!("    {}", truncated(&entry.item.path, path_width)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if *show_command {
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
                let command = crate::sf::commands::apex_test_display(&classes, org.as_deref());
                lines.push(Line::from(" "));
                lines.push(Line::from(faint(command)));
            }
            lines.push(Line::from(" "));
            let height = (lines.len() as u16 + 2).min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), modal_width, height);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block("Confirm tests", app)),
                area,
            );
        }
        Modal::TransferConfirm {
            retrieve,
            entries,
            focus,
            show_command,
            options,
        } => {
            let modal_width = 90u16.min(frame.area().width);
            let path_width = usize::from(modal_width).saturating_sub(6).max(1);
            let mut lines = vec![
                Line::from(strong(
                    format!(
                        "{} {} components to {}",
                        if *retrieve { "Retrieve" } else { "Deploy" },
                        entries.iter().filter(|entry| entry.checked).count(),
                        options
                            .target_org
                            .as_deref()
                            .filter(|org| !org.is_empty())
                            .unwrap_or("default org")
                    ),
                    accent(app),
                )),
                Line::from(" "),
                Line::from(faint(
                    "[Enter] run   [Esc] back   [Space] toggle   [c] command   [g] org",
                )),
                Line::from(" "),
            ];
            for (index, entry) in entries.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} [{}] {}",
                        if index == *focus { "›" } else { " " },
                        if entry.checked { "✓" } else { " " },
                        entry.item.value
                    ),
                    Style::default()
                        .fg(if entry.checked {
                            Color::Reset
                        } else {
                            Color::DarkGray
                        })
                        .add_modifier(if index == *focus {
                            Modifier::REVERSED
                        } else {
                            Modifier::empty()
                        }),
                )));
                lines.push(Line::from(Span::styled(
                    format!("    {}", truncated(&entry.item.path, path_width)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.push(Line::from(" "));
            if *show_command {
                lines.push(Line::from(faint(format!(
                    "sf {}",
                    options.argv().join(" ")
                ))));
                lines.push(Line::from(" "));
            }
            lines.push(Line::from(" "));
            if !*retrieve {
                lines.push(Line::from(faint(
                    "[v] validate   [t] test level   [w] warnings",
                )));
            }
            lines.push(Line::from(faint("[g] org")));
            let height = (lines.len() as u16 + 2).min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), modal_width, height);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block("Confirm", app)),
                area,
            );
        }
        Modal::Prompt {
            title,
            fields,
            focus,
            ..
        } => {
            let suggestion_lines: usize = fields
                .iter()
                .map(|field| {
                    field
                        .cached_suggestions()
                        .len()
                        .min(5)
                        .saturating_mul(2)
                        .saturating_add(1)
                })
                .sum();
            let height = (fields.len() as u16 * 2 + 7 + suggestion_lines as u16)
                .min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), 76, height);
            frame.render_widget(Clear, area);
            let mut lines = Vec::new();
            for (index, field) in fields.iter().enumerate() {
                let active = index == *focus;
                let mut spans = vec![Span::styled(
                    format!("{} {:<28}", if active { "›" } else { " " }, field.label),
                    Style::default()
                        .fg(if active { accent(app) } else { Color::DarkGray })
                        .add_modifier(if active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )];
                let visible = truncated(
                    &field.value,
                    usize::from(area.width).saturating_sub(field.label.len() + 8),
                );
                for (byte_index, character) in visible.char_indices() {
                    let is_cursor = active && byte_index == field.cursor;
                    spans.push(Span::styled(
                        character.to_string(),
                        if is_cursor {
                            Style::default().bg(accent(app)).fg(Color::Black)
                        } else {
                            Style::default().fg(Color::Reset)
                        },
                    ));
                }
                if active && field.cursor >= visible.len() {
                    spans.push(Span::styled(
                        " ",
                        Style::default().bg(accent(app)).fg(Color::Black),
                    ));
                }
                lines.push(Line::from(spans));
                let suggestions = field.cached_suggestions();
                for (index, suggestion) in suggestions.iter().take(5).enumerate() {
                    let chosen = active && field.suggestion_index.unwrap_or(0) == index;
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(
                                "    {} ",
                                if field.multi_select {
                                    if field.selected.contains(&suggestion.value) {
                                        "[✓]"
                                    } else {
                                        "[ ]"
                                    }
                                } else if chosen {
                                    "›"
                                } else {
                                    " "
                                }
                            ),
                            Style::default().fg(
                                if chosen || field.selected.contains(&suggestion.value) {
                                    accent(app)
                                } else {
                                    Color::DarkGray
                                },
                            ),
                        ),
                        Span::styled(
                            truncated(
                                &suggestion.value,
                                usize::from(area.width).saturating_sub(10),
                            ),
                            Style::default().fg(if chosen { Color::Reset } else { Color::Gray }),
                        ),
                    ]));
                    if !suggestion.path.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!(
                                "       {}",
                                truncated(
                                    &suggestion.path,
                                    usize::from(area.width).saturating_sub(12)
                                )
                            ),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
                lines.push(Line::from(" "));
            }
            lines.push(Line::from(faint(
                "Space select · Enter confirm · ↑↓ suggestions · Tab next field · Esc cancel",
            )));
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block(title.clone(), app)),
                area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_colors_clears_colors_and_keeps_the_cursor_marker() {
        use ratatui::buffer::Cell;
        let mut cells = vec![Cell::default(); 3];
        cells[0].fg = Color::Red;
        cells[0].bg = Color::Blue;
        // Already reset: must be left untouched (including modifiers).
        cells[1].fg = Color::Reset;
        cells[1].bg = Color::Reset;
        cells[1].modifier.insert(Modifier::BOLD);
        // The cursor is encoded as black-on-colored.
        cells[2].fg = Color::Black;
        cells[2].bg = Color::White;

        strip_colors(&mut cells);

        assert_eq!(cells[0].fg, Color::Reset);
        assert_eq!(cells[0].bg, Color::Reset);
        assert!(!cells[0].modifier.contains(Modifier::REVERSED));
        assert_eq!(cells[1].fg, Color::Reset);
        assert_eq!(cells[1].bg, Color::Reset);
        assert!(cells[1].modifier.contains(Modifier::BOLD));
        assert_eq!(cells[2].fg, Color::Reset);
        assert_eq!(cells[2].bg, Color::Reset);
        assert!(cells[2].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn render_cache_is_bounded_and_keyed_on_revision() {
        let key = |record_id: u64, revision: u64| DetailCacheKey {
            record_id,
            revision,
            width: 80,
            show_details: false,
            show_json: false,
            queued_position: None,
            accent: Color::Reset,
            elapsed: "0s".to_owned(),
        };
        let mut cache = RenderCache::default();
        for id in 0..(DETAIL_CACHE_CAPACITY as u64 + 3) {
            cache.put(key(id, 0), vec![Line::from(format!("line {id}"))]);
        }
        assert_eq!(cache.details.len(), DETAIL_CACHE_CAPACITY);
        // The oldest entry was evicted and the newest is still present.
        assert!(cache.get(&key(0, 0)).is_none());
        let newest = DETAIL_CACHE_CAPACITY as u64 + 2;
        assert!(cache.get(&key(newest, 0)).is_some());
        // A revision bump invalidates the entry for that record.
        assert!(cache.get(&key(newest, 1)).is_none());
        cache.clear();
        assert!(cache.get(&key(newest, 0)).is_none());
    }

    #[test]
    fn centered_overlay_fits_the_screen() {
        let area = Rect::new(0, 0, 40, 12);
        let modal = centered(area, 80, 40);
        assert!(modal.width <= area.width);
        assert!(modal.height <= area.height);
        assert!(modal.x >= area.x && modal.y >= area.y);
    }

    const ALL_CONTEXTS: [HintContext; 7] = [
        HintContext::Home,
        HintContext::Transfer,
        HintContext::TestsSearch,
        HintContext::TestsCoverage,
        HintContext::TestsPlain,
        HintContext::Orgs,
        HintContext::Setup,
    ];

    #[test]
    fn footer_hints_have_unique_keys() {
        for ctx in ALL_CONTEXTS {
            for dirty in [false, true] {
                let pairs = hint_pairs(ctx, dirty);
                let mut keys: Vec<&str> = pairs.iter().map(|(key, _)| *key).collect();
                let count = keys.len();
                keys.sort_unstable();
                keys.dedup();
                assert_eq!(keys.len(), count, "duplicate footer key in {ctx:?}");
            }
        }
    }

    #[test]
    fn transfer_enter_hint_tracks_search_state() {
        let dirty = hint_pairs(HintContext::Transfer, true);
        let enters: Vec<(&str, &str)> = dirty
            .iter()
            .copied()
            .filter(|(key, _)| *key == "Enter")
            .collect();
        assert_eq!(enters, vec![("Enter", "search")]);
        let searched = hint_pairs(HintContext::Transfer, false);
        let enters: Vec<(&str, &str)> = searched
            .iter()
            .copied()
            .filter(|(key, _)| *key == "Enter")
            .collect();
        assert_eq!(enters, vec![("Enter", "confirm")]);
    }

    #[test]
    fn quit_hint_and_help_agree_with_key_handling() {
        let help = help_rows();
        let quit = help
            .iter()
            .find(|(key, _)| *key == "q")
            .map(|(_, text)| *text);
        assert_eq!(
            quit,
            Some("quit from the Menu; confirms first while a job runs")
        );
        let cancel = help
            .iter()
            .find(|(key, _)| *key == "Ctrl+X / Ctrl+C")
            .map(|(_, text)| *text);
        assert!(
            cancel.is_some_and(|text| text.contains("cancel") && text.contains("quits")),
            "Ctrl keys must describe cancel and idle quit"
        );
        let home_quit: Vec<(&str, &str)> = hint_pairs(HintContext::Home, false)
            .into_iter()
            .filter(|(key, _)| *key == "q")
            .collect();
        assert_eq!(home_quit, vec![("q", "quit")]);
    }

    #[test]
    fn every_footer_key_is_documented_in_help() {
        let help_text: String = help_rows()
            .iter()
            .map(|(key, description)| format!("{key} {description} "))
            .collect();
        for ctx in ALL_CONTEXTS {
            for dirty in [false, true] {
                for (cap, _) in hint_pairs(ctx, dirty) {
                    assert!(
                        help_text.contains(cap),
                        "footer key {cap} in {ctx:?} is missing from help"
                    );
                }
            }
        }
    }

    fn summary_result(summary: &str, ok: bool, failed: usize) -> crate::app::ResultView {
        let items = (0..failed)
            .map(|index| ResultItem {
                ok: false,
                label: format!("Failed{index}"),
                detail: String::new(),
                progress: None,
            })
            .collect();
        crate::app::ResultView {
            kind: crate::app::ResultKind::ApexTests,
            title: "Tests".to_owned(),
            status: if ok { "Passed" } else { "Failed" }.to_owned(),
            ok,
            summary: summary.to_owned(),
            items,
            coverage: Vec::new(),
            sections: Vec::new(),
        }
    }

    fn red_chip_text(line: &Line<'_>) -> Vec<String> {
        line.spans
            .iter()
            .filter(|span| span.style.fg == Some(Color::Red))
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn zero_failure_summary_has_no_red_chip() {
        let result = summary_result("2 run · 2 passed · 0 failed", true, 0);
        let line = summary_chips(&result);
        assert!(red_chip_text(&line).is_empty());
        let text = line_text(&line);
        assert_eq!(text, "2 run │ 2 passed │ 0 failed");
    }

    #[test]
    fn real_failure_colours_only_the_failure_chip_red() {
        let result = summary_result("2 run · 1 passed · 1 failed", false, 1);
        let line = summary_chips(&result);
        let red = red_chip_text(&line);
        assert_eq!(red, vec!["1 failed".to_owned()]);
    }

    #[test]
    fn failed_named_chip_is_not_red_when_the_result_succeeded() {
        let result = summary_result("FailedLoginTests 2 · 2 passed", true, 0);
        let line = summary_chips(&result);
        assert!(red_chip_text(&line).is_empty());
    }

    #[test]
    fn tests_layout_keeps_a_usable_list_at_small_sizes() {
        let (list, coverage) = tests_pane_heights(7, Density::Tiny).expect("tiny split");
        assert!(list >= 3, "tiny list too short: {list}");
        assert!(coverage >= 4, "tiny coverage too short: {coverage}");
        let (list, coverage) = tests_pane_heights(18, Density::Compact).expect("compact split");
        assert!(list >= 4, "compact list too short: {list}");
        assert!(coverage >= 5, "compact coverage too short: {coverage}");
        let (list, coverage) = tests_pane_heights(30, Density::Roomy).expect("roomy split");
        assert!(list >= 6 && coverage >= 6);
    }

    #[test]
    fn tests_layout_hides_coverage_only_without_room() {
        assert!(tests_pane_heights(4, Density::Tiny).is_none());
        assert!(tests_pane_heights(0, Density::Roomy).is_none());
    }

    #[test]
    fn truncation_notice_is_visible() {
        assert_eq!(
            line_text(&output_truncated_notice()),
            OUTPUT_TRUNCATED_NOTICE
        );
    }

    #[test]
    fn wrap_text_keeps_words_and_splits_long_tokens() {
        assert_eq!(
            wrap_text("one two three", 7),
            vec!["one two".to_owned(), "three".to_owned()]
        );
        assert_eq!(
            wrap_text("abcdefgh", 4),
            vec!["abcd".to_owned(), "efgh".to_owned()]
        );
        assert!(wrap_text("", 10).is_empty());
    }

    #[test]
    fn tracer_never_exceeds_budget() {
        assert_eq!(truncated("abcdef", 3), "ab…");
        assert_eq!(truncated("abc", 10), "abc");
    }

    fn coverage_item(label: &str, percent: &str, covered: usize, total: usize) -> ResultItem {
        ResultItem {
            ok: true,
            label: label.to_owned(),
            detail: format!("{percent} · {covered}/{total} lines"),
            progress: (total > 0).then_some((covered, total)),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn coverage_table_sorts_worst_first() {
        let items = vec![
            coverage_item("Best", "90%", 9, 10),
            coverage_item("Worst", "50%", 5, 10),
            coverage_item("Middle", "70%", 7, 10),
        ];
        let (lines, max_scroll) = coverage_table_lines(&items, 80, 0, 10);
        assert_eq!(max_scroll, 0);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        let worst = text
            .iter()
            .position(|line| line.contains("Worst"))
            .expect("worst");
        let middle = text
            .iter()
            .position(|line| line.contains("Middle"))
            .expect("middle");
        let best = text
            .iter()
            .position(|line| line.contains("Best"))
            .expect("best");
        assert!(worst < middle && middle < best);
        assert!(
            text.iter()
                .any(|line| line.contains("3 classes · worst first"))
        );
        assert!(text.iter().any(|line| line.contains("CLASS")));
    }

    #[test]
    fn coverage_table_empty_and_scroll_are_bounded() {
        let (empty, max) = coverage_table_lines(&[], 80, 0, 10);
        assert_eq!(max, 0);
        assert!(line_text(&empty[0]).contains("No per-class coverage returned"));
        let items: Vec<ResultItem> = (0..10)
            .map(|index| coverage_item(&format!("C{index:02}"), "50%", 5, 10))
            .collect();
        let (lines, max) = coverage_table_lines(&items, 80, 99, 4);
        assert_eq!(max, 6);
        assert_eq!(lines.len(), 3 + 4);
        assert!(line_text(&lines[3]).contains("C06"));
    }

    #[test]
    fn coverage_percent_prefers_the_stored_string_then_covered_total() {
        let stored = ResultItem {
            ok: true,
            label: "Demo".to_owned(),
            detail: "42% · 1/2 lines".to_owned(),
            progress: Some((1, 2)),
        };
        assert!((coverage_percent(&stored) - 42.0).abs() < f64::EPSILON);
        let fallback = ResultItem {
            ok: true,
            label: "Demo".to_owned(),
            detail: "unknown".to_owned(),
            progress: Some((1, 4)),
        };
        assert!((coverage_percent(&fallback) - 25.0).abs() < f64::EPSILON);
    }
}
