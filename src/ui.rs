//! Ratatui rendering. Screens read application state only and never mutate domain rules.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs,
};

use crate::app::{App, CatalogRow, Modal, Tab};
use crate::json_view;

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
        Tab::Retrieve => "Retrieve",
        Tab::Deploy => "Deploy",
        Tab::Tests => "Tests",
        Tab::Catalog => "Cat",
        Tab::Favorites => "Fav",
        Tab::Orgs => "Orgs",
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
        Density::Roomy | Density::Compact => (2, 2),
        Density::Tiny => (1, 1),
    };
    let layout = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .split(frame.area());
    draw_header(frame, layout[0], app, density);
    match app.tab {
        Tab::Home => draw_home(frame, layout[1], app),
        Tab::Retrieve | Tab::Deploy => draw_transfer(frame, layout[1], app),
        Tab::Tests => draw_tests(frame, layout[1], app),
        Tab::Catalog => draw_catalog(frame, layout[1], app),
        Tab::Favorites => draw_favorites(frame, layout[1], app),
        Tab::Orgs => draw_orgs(frame, layout[1], app),
        Tab::Output => draw_output(frame, layout[1], app),
        Tab::Setup => draw_setup(frame, layout[1], app),
    }
    draw_footer(frame, layout[2], app, density);
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, app);
    }
    if std::env::var_os("NO_COLOR").is_some() {
        for cell in &mut frame.buffer_mut().content {
            let was_cursor = cell.fg == Color::Black && cell.bg != Color::Reset;
            cell.fg = Color::Reset;
            cell.bg = Color::Reset;
            if was_cursor {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    let version = app.startup.version.as_deref().unwrap_or("detecting…");
    let project = if app.deploy_state.project_loading {
        "scanning…".to_owned()
    } else {
        app.deploy_state
            .project
            .as_ref()
            .map(|project| project.name())
            .unwrap_or_else(|| "no project".to_owned())
    };
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

    if density == Density::Tiny {
        let text = format!(
            "SFTUI {} | {} | {activity}",
            short_tab(app.tab),
            app.org_display()
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncated(&text, area.width.saturating_sub(1) as usize),
                Style::default().fg(accent(app)),
            ))),
            area,
        );
        return;
    }

    let info = Line::from(vec![
        strong("SFTUI", accent(app)),
        faint(format!(
            " v{} | sf {version} | project {project} | org {} | {activity}",
            env!("CARGO_PKG_VERSION"),
            app.org_display()
        )),
    ]);
    let tabs = app.tabs();
    let labels: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            if density == Density::Roomy {
                format!("[{}] {}", index + 1, tab.label())
            } else {
                format!("[{}]{}", index + 1, short_tab(*tab))
            }
        })
        .collect();
    let tabs = Tabs::new(labels)
        .select(tabs.iter().position(|tab| *tab == app.tab).unwrap_or(0))
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(accent(app))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider("│");
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    frame.render_widget(Paragraph::new(info), sections[0]);
    frame.render_widget(tabs, sections[1]);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App, density: Density) {
    let status_style = if app.status_error {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    if density == Density::Tiny {
        let text = if app.status.is_empty() {
            key_hints(app)
        } else {
            truncated(&app.status, area.width.saturating_sub(1) as usize)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, status_style))),
            area,
        );
        return;
    }
    let status = if app.status.is_empty() {
        "Ready".to_owned()
    } else {
        truncated(&app.status, area.width.saturating_sub(2) as usize)
    };
    let hints = truncated(&key_hints(app), area.width.saturating_sub(2) as usize);
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, status_style))),
        sections[0],
    );
    frame.render_widget(Paragraph::new(Line::from(faint(hints))), sections[1]);
}

fn key_hints(app: &App) -> String {
    let typing = matches!(app.tab, Tab::Retrieve | Tab::Deploy | Tab::Tests)
        || (app.tab == Tab::Catalog && app.catalog_state.search_focused);
    let common = if typing {
        "Esc back · Ctrl+Y copy · Ctrl+X cancel · Ctrl+C cancel/quit"
    } else {
        "Tab next · ? help · q quit · y copy · Ctrl+X cancel · Esc back"
    };
    let specific = match app.tab {
        Tab::Home => "r retrieve · d deploy · t tests · c create · o orgs · a login · g results",
        Tab::Retrieve | Tab::Deploy | Tab::Tests => {
            "type · Enter search · Space select · Enter confirm"
        }
        Tab::Catalog => "/ search · Enter help · r run · f favorite",
        Tab::Favorites => "Enter run · Ctrl+↑↓ reorder · e preset · x remove",
        Tab::Orgs => "Enter details · o open · d default · l login · x logout · F5 refresh",
        Tab::Output => "↑↓ records · j/k scroll · o details · z JSON · r rerun · v history",
        Tab::Setup => "Enter run installer · r re-probe",
    };
    format!("{common}   |   {specific}")
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
    let project = if app.deploy_state.project_loading {
        "scanning…".to_owned()
    } else {
        app.deploy_state
            .project
            .as_ref()
            .map(|project| project.name())
            .unwrap_or_else(|| "no project".to_owned())
    };
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            strong("SF TUI", accent(app)),
            faint(format!("   project {project} · org {}", app.org_display())),
        ]),
        Line::from(" "),
        strong_line("QUICK ACTIONS", accent(app)),
    ];
    for (index, action) in crate::app::QUICK_ACTIONS.iter().enumerate() {
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
                format!("[{}] ", action.key),
                Style::default()
                    .fg(if selected {
                        accent(app)
                    } else {
                        Color::DarkGray
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                action.label,
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
    if !app.config.recent_runs.is_empty() {
        lines.push(Line::from(" "));
        lines.push(strong_line("RECENT", accent(app)));
        for run in app.config.recent_runs.iter().take(3) {
            let ok = run.exit_code == Some(0);
            lines.push(Line::from(vec![
                Span::styled(
                    if ok { "[✓] " } else { "[✗] " },
                    Style::default().fg(if ok { Color::Green } else { Color::Red }),
                ),
                Span::styled(
                    truncated(&run.command, area.width.saturating_sub(6) as usize),
                    Style::default(),
                ),
            ]));
        }
    }
    lines.push(Line::from(" "));
    lines.push(Line::from(faint(
        "↑↓ move · Enter run · ? help · q quit · y copy",
    )));
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_tests(frame: &mut Frame, area: Rect, app: &App) {
    let state = &app.test_search;
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(area);
    let project = if app.deploy_state.project_loading {
        "scanning…".to_owned()
    } else {
        app.deploy_state
            .project
            .as_ref()
            .map(|project| project.name())
            .unwrap_or_else(|| "no project".to_owned())
    };
    frame.render_widget(
        Line::from(vec![
            strong("Run tests", accent(app)),
            faint(format!("   project {project} · org {}", app.org_display())),
            faint("   [Esc] back"),
        ]),
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
    frame.render_widget(
        Line::from(vec![
            faint(format!("{} selected", state.checked().len())),
            faint(if state.searching {
                "                        searching…"
            } else if state.needs_search() {
                "                        [Enter] search"
            } else {
                "              [Space] select   [Enter] confirm"
            }),
        ]),
        sections[3],
    );
}

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
                    Span::styled(id.clone(), Style::default().fg(Color::Reset)),
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
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("›"),
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
            "commands — {} · {} total ( / to search )",
            app.startup.catalog.source.label(),
            app.startup.catalog.commands.len()
        )
    } else {
        format!(
            "commands — '{}' ({} rows)",
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
                } else if app.catalog_state.search_focused {
                    format!("{}|", app.catalog_state.query)
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
                            Style::default().fg(Color::Reset)
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
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
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
        strong("sf ", accent(app)) + strong(command.id.clone(), Color::Reset),
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
                Style::default().fg(Color::Reset),
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
            ("y", "copy"),
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
                    Span::styled(favorite.id.clone(), Style::default().fg(Color::Reset)),
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
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("›"),
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
                    Style::default().fg(Color::Reset),
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
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
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
        lines.push(Line::from(strong(format!("sf {}", id), Color::Reset)));
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
            ("y", "copy"),
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
            sections[1],
            &mut state,
        );
        if app.org_state.orgs.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(faint(if app.org_state.loading {
                    "Loading orgs…"
                } else {
                    "No orgs loaded — F5 to refresh"
                }))),
                sections[1],
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
                "orgs — F5 to refresh"
            },
            app,
        ))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("›");
    frame.render_stateful_widget(table, columns[0], &mut state);

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
                Span::styled(format!("  {key:22}"), Style::default().fg(Color::DarkGray)),
                Span::styled(truncated(value, 40), Style::default().fg(Color::Reset)),
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

fn draw_transfer(frame: &mut Frame, area: Rect, app: &App) {
    let state = &app.transfer_search;
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(area);
    let title = if state.retrieve { "Retrieve" } else { "Deploy" };
    let project = if app.deploy_state.project_loading {
        "scanning…".to_owned()
    } else {
        app.deploy_state
            .project
            .as_ref()
            .map(|project| project.name())
            .unwrap_or_else(|| "no project".to_owned())
    };
    let mut heading = vec![
        strong(title, accent(app)),
        faint(format!("   project {project} · org {}", app.org_display())),
    ];
    if let Some(error) = &app.deploy_state.project_error {
        heading.push(faint(format!(
            "   {}",
            truncated(error, usize::from(area.width).saturating_sub(48))
        )));
    }
    heading.push(faint("   [Esc] back"));
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
    let count = state.checked().len();
    frame.render_widget(
        Line::from(vec![
            faint(format!("{count} selected")),
            faint(if state.searching {
                "                            searching…"
            } else if state.needs_search() {
                "                            [Enter] search"
            } else {
                "                  [Space] select   [Enter] confirm"
            }),
        ]),
        sections[3],
    );
}

fn test_run_lines(
    app: &App,
    run: &crate::sf::test_results::TestRun,
    expanded: bool,
) -> Vec<Line<'static>> {
    let passed =
        run.outcome.eq_ignore_ascii_case("Passed") || run.outcome.eq_ignore_ascii_case("Pass");
    let mut lines = vec![
        strong("APEX TESTS", accent(app))
            + Span::styled(" — ", Style::default().fg(Color::DarkGray))
            + strong(
                if passed { "PASS" } else { "FAILED" },
                if passed { Color::Green } else { Color::Red },
            ),
        Line::from(faint(&run.command)),
        Line::from(faint(format!(
            "{} · {} run · {} passed · {} failed · {} skipped · coverage {}",
            run.outcome, run.tests_run, run.passing, run.failing, run.skipped, run.coverage
        ))),
        Line::from(" "),
        Line::from(faint("[Esc] close   [↑↓] scroll")),
    ];
    if expanded {
        lines.push(Line::from(" "));
        lines.push(strong_line("HIGHLIGHTS", accent(app)));
        lines.push(Line::from(faint(format!("Outcome: {}", run.outcome))));
        lines.push(Line::from(faint(format!(
            "Failures: {}",
            run.failures.len()
        ))));
        lines.push(Line::from(faint(format!("Coverage: {}", run.coverage))));
        if !run.failures.is_empty() {
            lines.push(Line::from(" "));
            lines.push(strong_line("ERRORS", Color::Red));
            for failure in run.failures.iter().take(200) {
                lines.push(Line::from(Span::styled(
                    truncated(&format!("{}: {}", failure.name, failure.message), 100),
                    Style::default().fg(Color::Red),
                )));
            }
        }
        lines.push(Line::from(" "));
        lines.push(strong_line("APEX CLASS COVERAGE", accent(app)));
        if run.coverages.is_empty() {
            lines.push(Line::from(faint("No coverage data returned.")));
        }
        for coverage in run.coverages.iter().take(200) {
            lines.push(Line::from(faint(format!(
                "{:<32} {:>6}  ({}/{})",
                truncated(&coverage.name, 32),
                coverage.percent,
                coverage.covered,
                coverage.total
            ))));
        }
        lines.push(Line::from(" "));
    }
    lines
}

fn summary_chips(summary: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, part) in summary
        .split('·')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let lower = part.to_ascii_lowercase();
        let is_zero = part
            .split_whitespace()
            .next()
            .and_then(|token| token.trim_end_matches('%').parse::<f64>().ok())
            .is_some_and(|value| value == 0.0);
        let color = if (lower.contains("fail") || lower.contains("error")) && !is_zero {
            Color::Red
        } else if (lower.contains("pass") || lower.contains("succeed")) && !is_zero {
            Color::Green
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

fn pinned_lines(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    lines: Vec<Line<'static>>,
    hints: String,
) {
    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let max_scroll = lines.len().saturating_sub(usize::from(sections[0].height));
    let scroll = app
        .output_state
        .scroll
        .min(max_scroll)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(Text::from(lines)).scroll((scroll, 0)),
        sections[0],
    );
    frame.render_widget(Paragraph::new(Line::from(faint(hints))), sections[1]);
}

fn draw_result_view(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    record: &crate::app::OutputRecord,
    result: &crate::app::ResultView,
) {
    let color = if result.ok { Color::Green } else { Color::Red };
    let mut lines = vec![
        strong(&result.title, accent(app))
            + Span::styled("  ", Style::default().fg(Color::DarkGray))
            + strong(&result.status, color),
        Line::from(Span::styled(
            "─".repeat(area.width.saturating_sub(2) as usize),
            Style::default().fg(Color::DarkGray),
        )),
        summary_chips(&result.summary),
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
        lines.push(strong_line("COVERAGE", accent(app)));
        for item in result.coverage.iter().take(200) {
            lines.push(result_item_line(item));
        }
        if result.coverage.len() > 200 {
            lines.push(Line::from(faint(format!(
                "… {} more classes",
                result.coverage.len() - 200
            ))));
        }
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
        }
    }
    let mut hints = "[j/k/PgDn] scroll   [o] raw   [z] JSON   [r] rerun".to_owned();
    if result.kind == crate::app::ResultKind::ApexTests {
        hints.push_str("   [v] history");
    }
    hints.push_str("   [Esc] back");
    pinned_lines(frame, area, app, lines, hints);
}

fn draw_output_detail(frame: &mut Frame, area: Rect, app: &App, record: &crate::app::OutputRecord) {
    if let Some(result) = &record.result {
        draw_result_view(frame, area, app, record, result);
        return;
    }
    let label = match (record.finished, record.exit_code) {
        (false, _) => "RUNNING",
        (true, Some(0)) => "SUCCESS",
        (true, Some(_)) => "FAILED",
        (true, None) => "STOPPED",
    };
    let color = match (record.finished, record.exit_code) {
        (false, _) => Color::Yellow,
        (true, Some(0)) => Color::Green,
        (true, Some(_)) => Color::Red,
        (true, None) => Color::Yellow,
    };
    let mut lines = vec![
        strong(record.purpose.to_uppercase(), accent(app))
            + Span::styled(" — ", Style::default().fg(Color::DarkGray))
            + strong(label, color),
        Line::from(faint(&record.command)),
        Line::from(" "),
    ];
    if app.output_state.show_details || !record.finished {
        lines.push(strong_line("DETAILS", accent(app)));
        if app.output_state.show_json
            && let Some(value) = &record.json
        {
            let view = json_view::table(value);
            for header in &view.headers {
                lines.push(strong_line(header, accent(app)));
            }
            for row in view.rows.iter().take(500) {
                lines.push(Line::from(faint(row.join(" | "))));
            }
        } else {
            for line in &record.lines {
                lines.push(Line::from(Span::styled(
                    line.text.clone(),
                    Style::default().fg(
                        if line.stream == crate::sf::runner::OutputStream::Stderr {
                            Color::Red
                        } else {
                            Color::Gray
                        },
                    ),
                )));
            }
            if record.lines.is_empty() {
                lines.push(Line::from(faint("No output.")));
            }
        }
    } else {
        lines.push(Line::from(faint(
            "[o] details   [z] JSON   [r] rerun   [Esc] back",
        )));
    }
    let hints = "[↑↓ records]   [j/k] scroll   [o] details   [z] JSON   [r] rerun   [Esc] back";
    pinned_lines(frame, area, app, lines, hints.to_owned());
}

fn draw_output(frame: &mut Frame, area: Rect, app: &App) {
    if app.output_state.records.is_empty() {
        frame.render_widget(
            Paragraph::new("No commands yet. Press r to retrieve or d to deploy from Home.")
                .block(block("output", app)),
            area,
        );
        return;
    }
    let selected = app
        .output_state
        .selected
        .min(app.output_state.records.len().saturating_sub(1));
    let Some(record) = app.output_state.records.get(selected) else {
        return;
    };
    if density(area) == Density::Roomy && app.output_state.records.len() > 1 {
        let columns = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(area);
        let width = columns[0].width.saturating_sub(3) as usize;
        let items: Vec<ListItem> = app
            .output_state
            .records
            .iter()
            .map(|record| {
                ListItem::new(Line::from(Span::styled(
                    truncated(&record.label(), width),
                    Style::default(),
                )))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(block("commands", app))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        draw_output_detail(frame, columns[1], app, record);
        return;
    }
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(faint(format!(
            "record {}/{} — ↑↓ switch · {}",
            selected + 1,
            app.output_state.records.len(),
            record.label()
        )))),
        sections[0],
    );
    draw_output_detail(frame, sections[1], app, record);
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
                    Span::styled((*name).to_owned(), Style::default().fg(Color::Reset)),
                    faint(truncated(&format!("  {command}"), 40)),
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
        "Enter runs the selected installer in the Output tab.",
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

fn draw_modal(frame: &mut Frame, modal: &Modal, app: &App) {
    match modal {
        Modal::Help { scroll } => {
            let area = centered(frame.area(), 86, frame.area().height.min(24));
            frame.render_widget(Clear, area);
            let lines: Vec<Line> = HELP_ROWS
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
            let area = centered(frame.area(), 68, 9);
            frame.render_widget(Clear, area);
            let lines = vec![
                Line::from(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::Reset),
                )),
                Line::from(" "),
                Line::from(faint("Enter / y — confirm       Esc / n — cancel")),
            ];
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block(title.clone(), app)),
                area,
            );
        }
        Modal::OrgMenu { selected } => {
            let items = crate::app::ORG_MENU;
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    Line::from(Span::styled(
                        format!(
                            "{} [{}] {}",
                            if index == *selected { "›" } else { " " },
                            index + 1,
                            label
                        ),
                        Style::default().fg(if index == *selected {
                            accent(app)
                        } else {
                            Color::Reset
                        }),
                    ))
                })
                .chain(std::iter::once(Line::from(faint(
                    "↑↓ move   Enter select   Esc close",
                ))))
                .collect();
            let area = centered(frame.area(), 52, lines.len() as u16 + 2);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block("Org", app)),
                area,
            );
        }
        Modal::TestHistory { runs, selected } => {
            let mut lines = vec![
                strong_line("SAVED APEX TEST RUNS", accent(app)),
                Line::from(faint("↑↓ move   Enter open   Esc close")),
            ];
            let window = 4usize;
            let selected = (*selected).min(runs.len().saturating_sub(1));
            let start = selected
                .saturating_sub(window.saturating_sub(1))
                .min(runs.len().saturating_sub(window));
            for (index, run) in runs.iter().enumerate().skip(start).take(window) {
                let passed = run.outcome.eq_ignore_ascii_case("Passed")
                    || run.outcome.eq_ignore_ascii_case("Pass");
                let marker = if index == selected { "›" } else { " " };
                let color = if index == selected {
                    accent(app)
                } else {
                    Color::Gray
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {}  run {}  {}",
                        marker,
                        if passed { "PASS" } else { "FAIL" },
                        run.at,
                        run.outcome
                    ),
                    Style::default().fg(color),
                )));
                lines.push(Line::from(Span::styled(
                    format!(
                        "    {} run · {} passed · {} failed · {} skipped · coverage {}",
                        run.tests_run, run.passing, run.failing, run.skipped, run.coverage
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    format!("    {}", truncated(&run.command, 68)),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(" "));
            }
            let position = if runs.is_empty() {
                "0/0".to_owned()
            } else {
                format!("{}/{}", selected + 1, runs.len())
            };
            lines.push(Line::from(faint(position)));
            let area = centered(
                frame.area(),
                82,
                (lines.len() as u16 + 2).min(frame.area().height.saturating_sub(2)),
            );
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(Text::from(lines)).block(block("History", app)),
                area,
            );
        }
        Modal::TestRun { run, scroll } => {
            let lines = test_run_lines(app, run, true);
            let height = (lines.len() as u16 + 4).min(frame.area().height.saturating_sub(2));
            let area = centered(frame.area(), 100, height);
            frame.render_widget(Clear, area);
            frame.render_widget(block("saved test run", app), area);
            let inner = block("saved test run", app).inner(area);
            let sections =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
            let max_scroll = lines.len().saturating_sub(usize::from(sections[0].height));
            frame.render_widget(
                Paragraph::new(Text::from(lines))
                    .scroll(((*scroll).min(max_scroll).min(u16::MAX as usize) as u16, 0)),
                sections[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(faint("↑↓ scroll · Esc close"))),
                sections[1],
            );
        }
        Modal::TestConfirm {
            entries,
            focus,
            show_command,
            org,
        } => {
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
                    format!("    {}", truncated(&entry.item.path, 60)),
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
            let area = centered(frame.area(), 90, height);
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
                    format!("    {}", truncated(&entry.item.path, 60)),
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
            let area = centered(frame.area(), 90, height);
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

const HELP_ROWS: &[(&str, &str)] = &[
    (
        "Home r/d/t/c/o/a/g",
        "retrieve · deploy · tests · create · orgs · login · results",
    ),
    ("Tab / 1-9", "next screen / jump directly"),
    ("Enter", "the highlighted action"),
    ("?", "show this help; any key closes it"),
    ("h", "go Home from any screen"),
    (
        "Esc",
        "back one level; a job is never cancelled by navigation",
    ),
    ("q", "quit; asks first while a job is running"),
    (
        "Ctrl+X / Ctrl+C",
        "cancel the newest job; Ctrl+C quits when idle",
    ),
    ("y", "copy the current command/selection"),
    ("Ctrl+Y", "copy while typing in a search field"),
    ("F5", "refresh orgs, catalog or component search"),
    ("/ (Catalog)", "fuzzy search the full CLI catalog"),
    ("Enter (Catalog)", "load live --help for the selection"),
    ("f / e (Catalog)", "favorite / edit preset flags"),
    (
        "Space (search/confirm)",
        "select a component, test class or row",
    ),
    ("c (confirm)", "show the exact command that will run"),
    ("g (confirm)", "cycle the target org"),
    (
        "v/t/w (Deploy confirm)",
        "validate / test level / ignore warnings",
    ),
    ("↑↓ PgUp/PgDn", "move or scroll lists and results"),
    ("z / v / r (Output)", "JSON · saved test runs · rerun"),
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
