//! Ratatui rendering. Screens read application state only and never mutate domain rules.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs, Wrap,
};

use crate::app::{App, CatalogRow, Modal, Tab};
use crate::json_view;
use crate::sf::deploy::PayloadMode;

fn accent(app: &App) -> Color {
    app.accent()
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

fn short_tab(tab: Tab) -> &'static str {
    match tab {
        Tab::Home => "Home",
        Tab::Catalog => "Cat",
        Tab::Favorites => "Fav",
        Tab::Orgs => "Orgs",
        Tab::Deploy => "Dep",
        Tab::Output => "Out",
        Tab::Setup => "Setup",
    }
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

fn faint(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::DarkGray))
}

fn strong(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(
        text.into(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
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
    let (header_height, footer_height) = match density {
        Density::Roomy => (4, 3),
        Density::Compact => (2, 2),
        Density::Tiny => (1, 1),
    };
    let layout = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .split(frame.area());
    draw_header(frame, layout[0], app);
    match app.tab {
        Tab::Home => draw_home(frame, layout[1], app),
        Tab::Catalog => draw_catalog(frame, layout[1], app),
        Tab::Favorites => draw_favorites(frame, layout[1], app),
        Tab::Orgs => draw_orgs(frame, layout[1], app),
        Tab::Deploy => draw_deploy(frame, layout[1], app),
        Tab::Output => draw_output(frame, layout[1], app),
        Tab::Setup => draw_setup(frame, layout[1], app),
    }
    draw_footer(frame, layout[2], app, density);
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, app);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        draw_compact_header(frame, area, app, density);
        return;
    }
    let version = app.startup.version.as_deref().unwrap_or("detecting…");
    let project = app
        .deploy_state
        .project
        .as_ref()
        .map(|project| project.name())
        .unwrap_or_else(|| "no project".to_owned());
    let frames = ["-", "\\", "|", "/"];
    let spinner = frames[app.spinner_tick() % frames.len()];
    let activity = if !app.startup_stage.done {
        format!("{spinner} {}", app.startup_stage.label)
    } else if app.busy() {
        format!(
            "{spinner} running {}",
            app.active_job_label().unwrap_or_default()
        )
    } else {
        "ready".to_owned()
    };
    let info = Line::from(vec![
        strong("SFTUI", accent(app)),
        faint(format!(" v{}", env!("CARGO_PKG_VERSION"))),
        faint(format!("  |  sf {version}")),
        faint(format!("  |  project {project}")),
        Span::styled(
            format!("  |  {activity}"),
            Style::default().fg(if app.startup_stage.done && !app.busy() {
                Color::DarkGray
            } else {
                Color::Yellow
            }),
        ),
    ]);
    let tabs = Tabs::new(
        app.tabs()
            .into_iter()
            .enumerate()
            .map(|(index, tab)| format!("[{}] {}", index + 1, tab.label())),
    )
    .select(
        app.tabs()
            .iter()
            .position(|tab| *tab == app.tab)
            .unwrap_or(0),
    )
    .style(Style::default().fg(Color::Gray))
    .highlight_style(
        Style::default()
            .fg(accent(app))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )
    .divider("│");
    let inner = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
        .split(block("sf-tui — Salesforce CLI control room", app).inner(area));
    frame.render_widget(block("sf-tui — Salesforce CLI control room", app), area);
    frame.render_widget(Paragraph::new(info), inner[0]);
    frame.render_widget(tabs, inner[1]);
}

fn draw_compact_header(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    let spinner = ["-", "\\", "|", "/"][app.spinner_tick() % 4];
    let activity = if !app.startup_stage.done {
        format!("{spinner} {}", app.startup_stage.label)
    } else if app.busy() {
        format!(
            "{spinner} {}",
            app.active_job_label()
                .unwrap_or_else(|| "running".to_owned())
        )
    } else {
        "ready".to_owned()
    };
    if density == Density::Tiny {
        let text = format!(
            "SFTUI {} | sf {} | {activity}",
            short_tab(app.tab),
            app.startup.version.as_deref().unwrap_or("...")
        );
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                truncated(&text, area.width.saturating_sub(1) as usize),
                accent(app),
            ))),
            area,
        );
        return;
    }
    let info = Line::from(vec![
        strong("SFTUI", accent(app)),
        faint(format!(
            " v{} | sf {} | {activity}",
            env!("CARGO_PKG_VERSION"),
            app.startup.version.as_deref().unwrap_or("...")
        )),
    ]);
    let mut tabs = vec![faint("Tab:").clone()];
    for (index, tab) in app.tabs().into_iter().enumerate() {
        if index > 0 {
            tabs.push(faint(" ").clone());
        }
        let label = format!("[{}]{}", index + 1, short_tab(tab));
        tabs.push(Span::styled(
            label,
            if tab == app.tab {
                Style::default()
                    .fg(accent(app))
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    }
    frame.render_widget(Paragraph::new(info), Rect { height: 1, ..area });
    frame.render_widget(
        Paragraph::new(Line::from(tabs)),
        Rect {
            y: area.y + 1,
            height: 1,
            ..area
        },
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    if density != Density::Roomy {
        let style = if app.status_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        let text = if app.status.is_empty() {
            key_hints(app)
        } else {
            truncated(&app.status, area.width.saturating_sub(1) as usize)
        };
        frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
        return;
    }
    let status_style = if app.status_error {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    let status = if app.status.is_empty() {
        "Ready".to_owned()
    } else {
        truncated(&app.status, area.width.saturating_sub(4) as usize)
    };
    let hints = key_hints(app);
    let inner = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
        .split(block("status", app).inner(area));
    frame.render_widget(block("status", app), area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, status_style))),
        inner[0],
    );
    frame.render_widget(Paragraph::new(Line::from(faint(hints))), inner[1]);
}

fn key_hints(app: &App) -> String {
    let common = "Tab next screen · ? help · q quit";
    let specific = match app.tab {
        Tab::Home => "a login · r retrieve · d deploy · t test · g test results",
        Tab::Catalog => "/ search · Enter help · r run · f favorite",
        Tab::Favorites => "Enter run · Ctrl+↑↓ reorder · e preset",
        Tab::Orgs => "Enter details · o open · d default · l login",
        Tab::Deploy => "Space select · m payload · Enter run",
        Tab::Output => "↑↓ records · j/k scroll · z JSON",
        Tab::Setup => "Enter run installer · r re-probe",
    };
    format!("{specific}   |   {common}")
}

fn keycap(key: &str, label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!("[{key}] {label}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn action_bar<'a>(items: Vec<(&'a str, &'a str)>, app: &App) -> Line<'a> {
    let mut spans = vec![faint("Actions  ")];
    for (index, (key, label)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(faint("  |  "));
        }
        spans.push(keycap(key, label, accent(app)));
    }
    Line::from(spans)
}

fn draw_home(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let compact = density == Density::Compact;
        let rows = Layout::vertical([
            Constraint::Length(if compact { 3 } else { 7 }),
            Constraint::Min(3),
        ])
        .split(area);
        let items: Vec<ListItem> = crate::app::QUICK_ACTIONS
            .iter()
            .map(|action| {
                let mut line = vec![
                    strong(format!("[{}] ", action.key), accent(app)),
                    strong(action.label, Color::White),
                ];
                if compact {
                    line.push(faint(truncated(
                        action.command.trim_start_matches("sf "),
                        area.width.saturating_sub(26) as usize,
                    )));
                }
                ListItem::new(Line::from(line))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(app.home_state.selected.min(QUICK_ACTIONS_LEN - 1)));
        frame.render_stateful_widget(
            List::new(items)
                .block(block("Home — a/r/d/t/g run a task", app))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            rows[1],
            &mut state,
        );
        let project = app
            .deploy_state
            .project
            .as_ref()
            .map(|project| project.name())
            .unwrap_or_else(|| "no project".to_owned());
        let org = if app.org_target_hint().is_empty() {
            "sf default".to_owned()
        } else {
            app.org_target_hint()
        };
        let mut context = if compact {
            vec![
                Line::from(faint(format!("project: {project} | org: {org}"))),
                Line::from(faint(format!(
                    "sf {} | ? help | Enter run selected",
                    app.startup.version.as_deref().unwrap_or("detecting…")
                ))),
            ]
        } else {
            vec![Line::from(faint(truncated(
                &format!("{project} | {org}"),
                area.width.saturating_sub(2) as usize,
            )))]
        };
        if !app.startup_stage.done {
            let frames = ["-", "\\", "|", "/"];
            context.push(Line::from(Span::styled(
                format!(
                    "{} {} ({}s) — usable now",
                    frames[app.spinner_tick() % frames.len()],
                    app.startup_stage.label,
                    app.startup_stage.started.elapsed().as_secs()
                ),
                Style::default().fg(Color::Yellow),
            )));
        }
        frame.render_widget(Paragraph::new(Text::from(context)), rows[0]);
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(area);
    let items: Vec<ListItem> = crate::app::QUICK_ACTIONS
        .iter()
        .map(|action| {
            ListItem::new(vec![
                Line::from(vec![
                    strong(format!("[{}] ", action.key), accent(app)),
                    strong(action.label, Color::White),
                    Span::styled(
                        format!("   {}", action.command.trim_start_matches("sf ")),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("     {}", action.hint),
                    Style::default().fg(Color::Gray),
                )),
            ])
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(app.home_state.selected.min(QUICK_ACTIONS_LEN - 1)));
    let list = List::new(items)
        .block(block(
            "What do you want to do? Five keys cover most work.",
            app,
        ))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol(">");
    frame.render_stateful_widget(list, columns[0], &mut state);

    let mut details = vec![
        strong_line("CURRENT CONTEXT", accent(app)),
        Line::from(faint(format!(
            "project: {}",
            app.deploy_state
                .project
                .as_ref()
                .map(|project| project.name())
                .unwrap_or_else(|| "none found".to_owned())
        ))),
        Line::from(faint(format!(
            "default org: {}",
            if app.org_target_hint().is_empty() {
                "sf default (not loaded yet)".to_owned()
            } else {
                app.org_target_hint()
            }
        ))),
        Line::from(faint(format!(
            "cli: {}",
            app.startup.version.as_deref().unwrap_or("detecting…")
        ))),
        Line::from(" "),
        action_bar(
            vec![
                ("1-5", "go to screen"),
                ("?", "all shortcuts"),
                ("q", "quit"),
            ],
            app,
        ),
        Line::from(" "),
        Line::from(faint(
            "Paths and the default org are pre-filled; you only confirm or edit them.",
        )),
    ];
    if !app.startup_stage.done {
        let elapsed = app.startup_stage.started.elapsed().as_secs();
        let frames = ["-", "\\", "|", "/"];
        details.push(Line::from(Span::styled(
            format!(
                "{} {} ({}s) — the UI is usable now",
                frames[app.spinner_tick() % frames.len()],
                app.startup_stage.label,
                elapsed
            ),
            Style::default().fg(Color::Yellow),
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(details)).block(block("context", app)),
        columns[1],
    );
}

const QUICK_ACTIONS_LEN: usize = 5;

fn draw_catalog(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    let rows = app.catalog_rows();
    if density != Density::Roomy {
        let sections = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(1),
        ])
        .split(area);
        let query = if app.catalog_state.query.is_empty() {
            "/ fuzzy search".to_owned()
        } else {
            format!("/ {}", app.catalog_state.query)
        };
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                truncated(&query, area.width.saturating_sub(1) as usize),
                if app.catalog_state.search_focused {
                    Color::Yellow
                } else {
                    accent(app)
                },
            ))),
            sections[0],
        );
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| match row {
                CatalogRow::Header(name) => {
                    ListItem::new(Line::from(strong(truncated(name, 60), accent(app))))
                }
                CatalogRow::Command {
                    id,
                    summary,
                    favorite,
                    ..
                } => ListItem::new(Line::from(vec![
                    Span::styled(
                        if *favorite { "*" } else { " " },
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(id.clone(), Style::default().fg(Color::White)),
                    faint(truncated(
                        &format!("  {summary}"),
                        usize::from(area.width).saturating_sub(id.len() + 5),
                    )),
                ])),
            })
            .collect();
        let selected = app.catalog_state.selected_id.as_ref().and_then(|id| {
            rows.iter().position(
                |row| matches!(row, CatalogRow::Command { id: row_id, .. } if row_id == id),
            )
        });
        let mut state = ListState::default();
        state.select(selected);
        frame.render_stateful_widget(
            List::new(items)
                .block(block("commands", app))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            sections[1],
            &mut state,
        );
        if let Some(command) = app.selected_command() {
            frame.render_widget(
                Paragraph::new(Line::from(faint(truncated(
                    &format!("sf {} — Enter help | r run | ? keys", command.id),
                    area.width.saturating_sub(1) as usize,
                )))),
                sections[2],
            );
        }
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).split(area);
    let title = if app.catalog_state.search_focused {
        format!("commands — search: {}|", app.catalog_state.query)
    } else if app.catalog_state.query.is_empty() {
        format!(
            "commands — {} total ( / to search )",
            app.startup.catalog.commands.len()
        )
    } else {
        format!(
            "commands — '{}' ({} hits)",
            app.catalog_state.query,
            rows.len()
        )
    };
    let left = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(columns[0]);
    let search_style = if app.catalog_state.search_focused {
        Style::default()
            .fg(accent(app))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            strong("Search  ", accent(app)),
            Span::styled(
                if app.catalog_state.query.is_empty() {
                    "fuzzy command id or summary…".to_owned()
                } else {
                    app.catalog_state.query.clone()
                },
                search_style,
            ),
        ]))
        .block(block("catalog filter", app)),
        left[0],
    );
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            CatalogRow::Header(name) => {
                ListItem::new(Line::from(strong(format!("  {name}"), accent(app))))
            }
            CatalogRow::Command {
                id,
                summary,
                matched,
                favorite,
            } => {
                let mut spans = vec![Span::styled(
                    if *favorite { "★ " } else { "  " },
                    Style::default().fg(if *favorite {
                        Color::Yellow
                    } else {
                        Color::DarkGray
                    }),
                )];
                for (byte_index, character) in id.char_indices() {
                    let highlighted = matched
                        .as_ref()
                        .is_some_and(|indices| indices.contains(&byte_index));
                    spans.push(Span::styled(
                        character.to_string(),
                        if highlighted {
                            Style::default()
                                .fg(accent(app))
                                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ));
                }
                spans.push(Span::styled(
                    format!("  — {summary}"),
                    Style::default().fg(Color::DarkGray),
                ));
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    let selected = app.catalog_state.selected_id.as_ref().and_then(|id| {
        rows.iter()
            .position(|row| matches!(row, CatalogRow::Command { id: row_id, .. } if row_id == id))
    });
    let mut state = ListState::default();
    state.select(selected);
    let list = List::new(items)
        .block(block(title, app))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, left[1], &mut state);
    draw_command_detail(frame, columns[1], app);
}

fn draw_command_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(command) = app.selected_command() else {
        frame.render_widget(
            Paragraph::new("No command selected").block(block("detail", app)),
            area,
        );
        return;
    };
    let mut lines: Vec<Line<'static>> = vec![
        strong("sf ", accent(app)) + strong(command.id.clone(), Color::White),
        Line::from(Span::styled(
            command.summary.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(faint(format!("category: {}", command.category))),
        Line::from(" "),
    ];
    for paragraph in command
        .description
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .take(14)
    {
        lines.push(Line::from(Span::styled(
            truncated(paragraph, area.width.saturating_sub(4) as usize),
            Style::default().fg(Color::Gray),
        )));
    }
    lines.push(Line::from(" "));
    let show_help = app.catalog_state.help_for.as_deref() == Some(command.id.as_str())
        && !app.catalog_state.help_flags.is_empty();
    let flags: Vec<&crate::sf::catalog::Flag> = if show_help {
        app.catalog_state.help_flags.iter().take(16).collect()
    } else {
        command.flags.iter().take(16).collect()
    };
    lines.push(strong_line("FLAGS", accent(app)));
    for flag in flags {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  --{}", flag.name),
                Style::default().fg(accent(app)),
            ),
            Span::styled(
                format!(
                    "  {}",
                    truncated(&flag.description, area.width.saturating_sub(30) as usize)
                ),
                Style::default().fg(Color::Gray),
            ),
        ]));
    }
    if !command.examples.is_empty() {
        lines.push(Line::from(" "));
        lines.push(strong_line("EXAMPLES", accent(app)));
        for example in command.examples.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    truncated(&example.command, area.width.saturating_sub(4) as usize)
                ),
                Style::default().fg(Color::White),
            )));
            if !example.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "      {}",
                        truncated(&example.description, area.width.saturating_sub(4) as usize)
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }
    lines.push(Line::from(faint(if show_help {
        "Flags from live --help"
    } else {
        "Enter loads live --help"
    })));
    lines.push(Line::from(" "));
    lines.push(action_bar(
        vec![
            ("/", "search"),
            ("Enter", "help"),
            ("r", "run"),
            ("f", "favorite"),
            ("c", "copy"),
        ],
        app,
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("command detail", app)),
        area,
    );
}

fn strong_line(text: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn draw_favorites(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                "Favorites — Enter run | e preset | x remove",
                accent(app),
            ))),
            sections[0],
        );
        let items: Vec<ListItem> = app
            .config
            .favorites
            .iter()
            .enumerate()
            .map(|(index, favorite)| {
                let flags = favorite.preset_flags.join(" ");
                ListItem::new(Line::from(vec![
                    strong(format!("{} ", index + 1), accent(app)),
                    Span::styled(favorite.id.clone(), Style::default().fg(Color::White)),
                    faint(truncated(&format!("  {flags}"), 24)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(if app.config.favorites.is_empty() {
            None
        } else {
            Some(app.favorites_state.selected)
        });
        frame.render_stateful_widget(
            List::new(items)
                .block(block("starred", app))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            sections[1],
            &mut state,
        );
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let items: Vec<ListItem> = app
        .config
        .favorites
        .iter()
        .enumerate()
        .map(|(index, favorite)| {
            let flags = if favorite.preset_flags.is_empty() {
                "no preset flags".to_owned()
            } else {
                favorite.preset_flags.join(" ")
            };
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("{} {}", index + 1, favorite.id),
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    format!("    {flags}"),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();
    let mut state = ListState::default();
    state.select(if app.config.favorites.is_empty() {
        None
    } else {
        Some(app.favorites_state.selected)
    });
    let list = List::new(items)
        .block(block("starred commands", app))
        .highlight_style(Style::default().bg(Color::DarkGray).fg(accent(app)))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, columns[0], &mut state);
    let mut lines = vec![
        strong_line("ONE-KEY RUN", accent(app)),
        Line::from(faint(
            "Enter runs the selected favorite with its preset flags.",
        )),
        Line::from(" "),
    ];
    if let Some(id) = app.selected_favorite_id() {
        let favorite = app
            .config
            .favorites
            .iter()
            .find(|favorite| favorite.id == id);
        lines.push(Line::from(strong(format!("sf {}", id), Color::White)));
        if let Some(favorite) = favorite {
            lines.push(Line::from(faint(format!(
                "flags: {}",
                favorite.preset_flags.join(" ")
            ))));
        }
        if let Some(command) = app.startup.catalog.command(&id) {
            lines.push(Line::from(" "));
            lines.push(Line::from(Span::styled(
                truncated(
                    &command.summary,
                    columns[1].width.saturating_sub(4) as usize,
                ),
                Style::default().fg(Color::Gray),
            )));
        }
    } else {
        lines.push(Line::from(faint(
            "Press f in the Catalog to star a command.",
        )));
    }
    lines.push(Line::from(" "));
    lines.push(action_bar(
        vec![
            ("Enter", "run"),
            ("e", "preset"),
            ("x", "remove"),
            ("c", "copy"),
        ],
        app,
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("favorite detail", app)),
        columns[1],
    );
}

fn draw_orgs(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                "Orgs — Enter details | o open | l login | F5 refresh",
                accent(app),
            ))),
            sections[0],
        );
        let rows: Vec<Row> = app
            .org_state
            .orgs
            .iter()
            .map(|org| {
                let widths = usize::from(area.width);
                Row::new(vec![
                    if org.is_default { "*" } else { " " }.to_owned(),
                    truncated(&org.label(), widths.saturating_sub(14).min(20)),
                    if density == Density::Compact {
                        truncated(&org.connected_status, 12)
                    } else {
                        String::new()
                    },
                ])
            })
            .collect();
        let widths: Vec<Constraint> = if density == Density::Compact {
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
        frame.render_stateful_widget(
            Table::new(rows, widths)
                .header(
                    Row::new(vec!["", "Alias", "Status"]).style(Style::default().fg(accent(app))),
                )
                .block(block("orgs", app))
                .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            sections[1],
            &mut state,
        );
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
                "orgs — F5 to refresh"
            },
            app,
        ))
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("›");
    frame.render_stateful_widget(table, columns[0], &mut state);

    let mut lines = vec![strong_line("ORG DETAIL", accent(app))];
    if let Some(detail) = &app.org_state.detail {
        lines.push(Line::from(faint(format!(
            "target: {}",
            app.org_state.detail_for.as_deref().unwrap_or("")
        ))));
        for (key, value) in detail.fields.iter().take(24) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:22}"), Style::default().fg(Color::DarkGray)),
                Span::styled(truncated(value, 40), Style::default().fg(Color::White)),
            ]));
        }
    } else {
        lines.push(Line::from(faint(
            "Enter loads sf org display for the selected org.",
        )));
        lines.push(Line::from(faint("Credentials are never displayed.")));
    }
    lines.push(Line::from(" "));
    lines.push(action_bar(
        vec![
            ("Enter", "details"),
            ("o", "open"),
            ("d", "default"),
            ("l", "login"),
            ("x", "logout"),
        ],
        app,
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("organization", app)),
        columns[1],
    );
}

fn draw_deploy(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let sections = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(2),
            Constraint::Length(2),
        ])
        .split(area);
        let options = &app.deploy_state.options;
        let preview = truncated(
            &app.transfer_preview(),
            usize::from(area.width).saturating_sub(1),
        );
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(vec![
                    strong(
                        if options.retrieve {
                            "RETRIEVE"
                        } else {
                            "DEPLOY"
                        },
                        accent(app),
                    ),
                    faint(format!("  {}", options.mode.label())),
                ]),
                Line::from(Span::styled(preview, Style::default().fg(accent(app)))),
            ])),
            sections[0],
        );
        let rows: Vec<Row> = app
            .deploy_state
            .items
            .iter()
            .map(|item| {
                let selected = match options.mode {
                    PayloadMode::Source => options.paths.contains(&item.path),
                    PayloadMode::Manifest => {
                        options.manifest.as_deref() == Some(item.path.as_str())
                    }
                    PayloadMode::Metadata => false,
                };
                Row::new(vec![
                    if selected { "x" } else { " " }.to_owned(),
                    truncated(&item.label, usize::from(area.width).saturating_sub(8)),
                ])
            })
            .collect();
        let mut state = TableState::default();
        state.select(if app.deploy_state.items.is_empty() {
            None
        } else {
            Some(app.deploy_state.selected)
        });
        frame.render_stateful_widget(
            Table::new(rows, [Constraint::Length(2), Constraint::Min(8)])
                .header(Row::new(vec!["", "Path"]).style(Style::default().fg(accent(app))))
                .block(block("metadata — Space selects", app))
                .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            sections[1],
            &mut state,
        );
        let status = if let Some(report) = &app.deploy_state.report {
            format!(
                "{}: {} ok / {} failed",
                report.status, report.succeeded, report.failed
            )
        } else if let Some(job) = &app.deploy_state.job_id {
            format!("polling {job}…")
        } else {
            "Space select · m mode · v validate · Enter run".to_owned()
        };
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                truncated(&status, usize::from(area.width).saturating_sub(1)),
                accent(app),
            ))),
            sections[2],
        );
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(4),
        Constraint::Length(9),
    ])
    .split(area);
    let options = &app.deploy_state.options;
    let project_name = app
        .deploy_state
        .project
        .as_ref()
        .map(|project| project.name())
        .unwrap_or_else(|| "no project".to_owned());
    let test = if options.retrieve {
        "n/a".to_owned()
    } else {
        options.test_level.as_str().to_owned()
    };
    let mut summary = vec![
        Line::from(vec![
            strong(
                if options.retrieve {
                    "RETRIEVE"
                } else {
                    "DEPLOY"
                },
                accent(app),
            ),
            faint(format!("  project: {project_name}")),
            faint(format!("  payload: {}", options.mode.label())),
        ]),
        Line::from(vec![
            faint("flags: "),
            Span::styled(
                format!(
                    "target={} validate={} warnings={} test={}",
                    options.target_org.as_deref().unwrap_or("default"),
                    options.check_only,
                    options.ignore_warnings,
                    test
                ),
                Style::default().fg(Color::White),
            ),
        ]),
    ];
    if let Some(error) = &app.deploy_state.project_error {
        summary.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Yellow),
        )));
    }
    summary.push(Line::from(Span::styled(
        truncated(
            &app.transfer_preview(),
            area.width.saturating_sub(4) as usize,
        ),
        Style::default().fg(accent(app)),
    )));
    frame.render_widget(
        Paragraph::new(Text::from(summary)).block(block("transfer options", app)),
        sections[0],
    );

    let rows: Vec<Row> = app
        .deploy_state
        .items
        .iter()
        .map(|item| {
            let selected = match options.mode {
                PayloadMode::Source => options.paths.contains(&item.path),
                PayloadMode::Manifest => options.manifest.as_deref() == Some(item.path.as_str()),
                PayloadMode::Metadata => false,
            };
            Row::new(vec![
                if selected { "✓" } else { " " }.to_owned(),
                item.kind.to_owned(),
                item.label.clone(),
            ])
        })
        .collect();
    let mut state = TableState::default();
    state.select(if app.deploy_state.items.is_empty() {
        None
    } else {
        Some(app.deploy_state.selected)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["", "kind", "path"]).style(Style::default().fg(accent(app))))
    .block(block("metadata picker — Space selects", app))
    .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
    .highlight_symbol("›");
    frame.render_stateful_widget(table, sections[1], &mut state);

    let mut lines = vec![strong_line("PROGRESS / RESULTS", accent(app))];
    if let Some(report) = &app.deploy_state.report {
        let frames = ["-", "\\", "|", "/"];
        let spinner = frames[app.spinner_tick() % frames.len()];
        lines.push(Line::from(vec![Span::styled(
            format!(
                "{} {} · total {} · ok {} · failed {}",
                spinner, report.status, report.total, report.succeeded, report.failed
            ),
            Style::default().fg(if report.failed > 0 {
                Color::Red
            } else {
                Color::Green
            }),
        )]));
        for component in report.components.iter().take(6) {
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} {} — {} {}",
                    component.component_type, component.name, component.status, component.problem
                ),
                Style::default().fg(if component.problem.is_empty() {
                    Color::Gray
                } else {
                    Color::Red
                }),
            )));
        }
    } else if let Some(job_id) = &app.deploy_state.job_id {
        let frames = ["-", "\\", "|", "/"];
        lines.push(Line::from(Span::styled(
            format!(
                "{} polling job {job_id}…",
                frames[app.spinner_tick() % frames.len()]
            ),
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(faint(
            "Select metadata and press Enter to start. Progress is polled automatically.",
        )));
    }
    lines.push(action_bar(
        vec![
            ("Space", "select"),
            ("m", "payload"),
            ("v", "validate"),
            ("t", "test"),
            ("o", "org"),
            ("Enter", "run"),
        ],
        app,
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("monitor", app)),
        sections[2],
    );
}

fn draw_output(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(2)]).split(area);
        let records: Vec<ListItem> = app
            .output_state
            .records
            .iter()
            .map(|record| ListItem::new(Line::from(faint(truncated(&record.label(), 60)))))
            .collect();
        let mut state = ListState::default();
        state.select(if records.is_empty() {
            None
        } else {
            Some(app.output_state.selected)
        });
        frame.render_stateful_widget(
            List::new(records)
                .block(block("history", app))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
            rows[0],
            &mut state,
        );
        let text = app
            .output_state
            .records
            .get(app.output_state.selected)
            .map(|record| {
                record
                    .lines
                    .iter()
                    .rev()
                    .take(usize::from(rows[1].height).saturating_sub(2))
                    .rev()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| "No output yet — run a command from Home.".to_owned());
        frame.render_widget(
            Paragraph::new(text)
                .block(block("output", app))
                .wrap(Wrap { trim: false }),
            rows[1],
        );
        return;
    }
    let rows = Layout::vertical([Constraint::Length(7), Constraint::Min(3)]).split(area);
    let records: Vec<ListItem> = app
        .output_state
        .records
        .iter()
        .map(|record| {
            ListItem::new(Line::from(Span::styled(
                record.label(),
                Style::default().fg(Color::Gray),
            )))
        })
        .collect();
    let mut state = ListState::default();
    state.select(if records.is_empty() {
        None
    } else {
        Some(app.output_state.selected)
    });
    frame.render_stateful_widget(
        List::new(records)
            .block(block("history — most recent last", app))
            .highlight_style(Style::default().bg(Color::DarkGray).fg(accent(app)))
            .highlight_symbol("› "),
        rows[0],
        &mut state,
    );
    let Some(record) = app.output_state.records.get(app.output_state.selected) else {
        frame.render_widget(
            Paragraph::new("No commands yet. Run one from the Catalog, Favorites or Orgs tab.")
                .block(block("streamed output", app)),
            rows[1],
        );
        return;
    };
    if app.output_state.show_json
        && let Some(value) = &record.json
    {
        let view = json_view::table(value);
        let table_rows: Vec<Row> = view
            .rows
            .iter()
            .skip(app.output_state.scroll)
            .take(rows[1].height.saturating_sub(3) as usize)
            .map(|row| Row::new(row.clone()))
            .collect();
        let widths: Vec<Constraint> = view.headers.iter().map(|_| Constraint::Min(14)).collect();
        let table = Table::new(table_rows, widths)
            .header(Row::new(view.headers.clone()).style(Style::default().fg(accent(app))))
            .block(block("parsed JSON — z for raw output", app));
        frame.render_widget(table, rows[1]);
        return;
    }
    let lines: Vec<Line> = record
        .lines
        .iter()
        .map(|line| {
            let color = if line.stream == crate::sf::runner::OutputStream::Stderr {
                Color::Red
            } else {
                Color::Gray
            };
            Line::from(Span::styled(line.text.clone(), Style::default().fg(color)))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block(format!("output #{} — sf stream", record.id), app))
            .wrap(Wrap { trim: false })
            .scroll((app.output_state.scroll as u16, 0)),
        rows[1],
    );
}

fn draw_setup(frame: &mut Frame, area: Rect, app: &App) {
    let density = density(area);
    if density != Density::Roomy {
        let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(strong(
                "Setup — Enter install | r re-probe | Tab leave",
                accent(app),
            ))),
            sections[0],
        );
        let items: Vec<ListItem> = crate::sf::install_instructions()
            .iter()
            .enumerate()
            .map(|(index, (name, command))| {
                ListItem::new(Line::from(vec![
                    strong(format!("{} ", index + 1), accent(app)),
                    Span::styled((*name).to_owned(), Style::default().fg(Color::White)),
                    faint(truncated(&format!("  {command}"), 40)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(app.setup_selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(block("installers", app))
                .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .highlight_symbol(">"),
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
                    Style::default().fg(Color::White),
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
            .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
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
        "Enter runs the selected installer in the Output tab.",
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("next steps", app)),
        columns[1],
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(20);
    let height = height.min(area.height.saturating_sub(2)).max(5);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_modal(frame: &mut Frame, modal: &Modal, app: &App) {
    match modal {
        Modal::Help => {
            let area = centered(frame.area(), 86, 24);
            frame.render_widget(Clear, area);
            let max_rows = usize::from(area.height.saturating_sub(2));
            let lines: Vec<Line> = HELP_ROWS
                .iter()
                .take(max_rows)
                .map(|(keys, description)| {
                    Line::from(vec![
                        strong(format!("  {keys:<20}"), accent(app)),
                        Span::styled((*description).to_owned(), Style::default().fg(Color::Gray)),
                    ])
                })
                .collect();
            frame.render_widget(
                Paragraph::new(Text::from(lines))
                    .block(block("keyboard shortcuts — any key to close", app)),
                area,
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
            let area = centered(frame.area(), 68, 9);
            frame.render_widget(Clear, area);
            let lines = vec![
                Line::from(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::White),
                )),
                Line::from(" "),
                Line::from(faint("Enter / y — confirm       Esc / n — cancel")),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block(title.clone(), app)),
                area,
            );
        }
        Modal::Prompt {
            title,
            fields,
            focus,
            ..
        } => {
            let height = (fields.len() as u16 * 2 + 7).min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), 76, height);
            frame.render_widget(Clear, area);
            let mut lines = Vec::new();
            for (index, field) in fields.iter().enumerate() {
                let active = index == *focus;
                let mut spans = vec![Span::styled(
                    format!("{} {:<28}", if active { "›" } else { " " }, field.label),
                    Style::default().fg(if active { accent(app) } else { Color::DarkGray }),
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
                            Style::default().fg(Color::White)
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
                lines.push(Line::from(" "));
            }
            lines.push(Line::from(faint(
                "Enter submit · Tab next field · ←→ cursor · Esc cancel",
            )));
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block(title.clone(), app)),
                area,
            );
        }
    }
}

const HELP_ROWS: &[(&str, &str)] = &[
    ("a (Home)", "log in to an org with the browser flow"),
    ("r (Home)", "retrieve using paths from sfdx-project.json"),
    ("d (Home)", "deploy using paths from sfdx-project.json"),
    (
        "t (Home)",
        "run Apex tests; leave org blank to use the default",
    ),
    ("g (Home)", "get results for a test run id"),
    ("Tab / 1-7", "next screen / jump directly"),
    ("Enter", "the highlighted action"),
    ("?", "show this help; any key closes it"),
    ("q / Ctrl+C", "quit / cancel the newest job"),
    ("/ (Catalog)", "fuzzy search the full CLI catalog"),
    ("f (Catalog)", "star the selected command"),
    ("Enter (Orgs)", "show org display details"),
    ("o / l (Orgs)", "open in browser / web login"),
    ("Space (Deploy)", "select a source path"),
    (
        "m (Deploy)",
        "cycle source path / manifest / metadata names",
    ),
    (
        "v / t / w (Deploy)",
        "validate / test level / ignore warnings",
    ),
    ("z (Output)", "toggle raw stream and parsed JSON table"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_overlay_fits_the_screen() {
        let area = Rect::new(0, 0, 40, 12);
        let modal = centered(area, 80, 40);
        assert!(modal.width <= area.width);
        assert!(modal.height <= area.height);
        assert!(modal.x >= area.x && modal.y >= area.y);
    }

    #[test]
    fn tracer_never_exceeds_budget() {
        assert_eq!(truncated("abcdef", 3), "ab…");
        assert_eq!(truncated("abc", 10), "abc");
    }
}
