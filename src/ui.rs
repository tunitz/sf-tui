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
        faint(format!("  |  org {}", app.org_display())),
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
            "SFTUI {} | {} | {activity}",
            short_tab(app.tab),
            app.org_display()
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
            " v{} | {} | {activity}",
            env!("CARGO_PKG_VERSION"),
            app.org_display()
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
        Tab::Home => "r retrieve · d deploy · t tests · g results · c create · a login",
        Tab::Retrieve | Tab::Deploy | Tab::Tests => {
            "type · Space select · Enter confirm · Esc back"
        }
        Tab::Catalog => "/ search · Enter help · r run · f favorite",
        Tab::Favorites => "Enter run · Ctrl+↑↓ reorder · e preset",
        Tab::Orgs => "Enter details · o open · d default · l login",
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
    let lines: Vec<Line> = std::iter::once(Line::from(vec![
        strong("SF TUI", accent(app)),
        faint(format!(
            "                         Org: {}",
            app.org_display()
        )),
    ]))
    .chain(
        crate::app::QUICK_ACTIONS
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let selected = index == app.home_state.selected;
                Line::from(vec![Span::styled(
                    format!(
                        "{} [{}] {}\n",
                        if selected { ">" } else { " " },
                        action.key,
                        action.label
                    ),
                    Style::default()
                        .fg(if selected { accent(app) } else { Color::White })
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )])
            }),
    )
    .chain(std::iter::once(Line::from(faint("[?] Help    [q] Quit"))))
    .collect();
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
    frame.render_widget(
        Line::from(vec![
            strong("Run tests", accent(app)),
            faint(format!(
                "                             Org: {}",
                app.org_display()
            )),
            faint("       [Esc] back"),
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
                Style::default().fg(Color::White),
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
                            if index == state.cursor { ">" } else { " " },
                            if checked { "x" } else { " " }
                        ),
                        Style::default().fg(if checked { accent(app) } else { Color::White }),
                    ),
                    Span::styled(item.value.clone(), Style::default().fg(Color::White)),
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
    frame.render_widget(List::new(items), sections[2]);
    frame.render_widget(
        Line::from(vec![
            faint(format!("{} selected", state.checked().len())),
            faint(if state.needs_search() {
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
    frame.render_widget(
        Line::from(vec![
            strong(title, accent(app)),
            faint(format!(
                "                              Org: {}",
                app.org_display()
            )),
            faint("       [Esc] back"),
        ]),
        sections[0],
    );
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
                Style::default().fg(Color::White),
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
                            if index == state.cursor { ">" } else { " " },
                            if checked { "x" } else { " " }
                        ),
                        Style::default().fg(if checked { accent(app) } else { Color::White }),
                    ),
                    Span::styled(item.value.clone(), Style::default().fg(Color::White)),
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
    frame.render_widget(List::new(items), sections[2]);
    let count = state.checked().len();
    frame.render_widget(
        Line::from(vec![
            faint(format!("{count} selected")),
            faint(if state.needs_search() {
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
        Line::from(faint(
            "[o] details   [v] history   [h] Home   [Esc] Back   [r] Rerun",
        )),
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
            for failure in run.failures.iter().take(8) {
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
        for coverage in run.coverages.iter().take(20) {
            lines.push(Line::from(faint(format!(
                "{:<32} {:>6}  ({}/{})",
                truncated(&coverage.name, 32),
                coverage.percent,
                coverage.covered,
                coverage.total
            ))));
        }
        lines.push(Line::from(" "));
        lines.push(Line::from(faint("[z] raw output / JSON")));
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
        let color = if lower.contains("fail") || lower.contains("error") {
            Color::Red
        } else if lower.contains("pass") || lower.contains("succeed") {
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
    let icon = if item.ok { "✓" } else { "x" };
    let color = if item.ok { Color::Green } else { Color::Red };
    let mut spans = vec![
        Span::styled(format!("[{icon}] "), Style::default().fg(color)),
        Span::styled(item.label.clone(), Style::default().fg(Color::White)),
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
    for item in result.items.iter().take(20) {
        lines.push(result_item_line(item));
    }
    if !result.coverage.is_empty() {
        lines.push(Line::from(" "));
        lines.push(strong_line("COVERAGE", accent(app)));
        for item in result.coverage.iter().take(20) {
            lines.push(result_item_line(item));
        }
    }
    for section in &result.sections {
        lines.push(Line::from(" "));
        lines.push(strong_line(&section.title, Color::Red));
        for item in section.rows.iter().take(12) {
            lines.push(result_item_line(item));
        }
    }
    lines.push(Line::from(" "));
    lines.push(Line::from(faint(
        "[o] raw output   [Esc] Back   [h] Home   [r] Rerun",
    )));
    if result.kind == crate::app::ResultKind::ApexTests {
        lines.push(Line::from(faint("[v] saved results")));
    }
    if app.output_state.show_details {
        lines.push(Line::from(" "));
        lines.push(strong_line("RAW OUTPUT", accent(app)));
        for line in record.lines.iter().rev().take(20).rev() {
            lines.push(Line::from(Span::styled(
                line.text.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(faint("[z] JSON")));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_output(frame: &mut Frame, area: Rect, app: &App) {
    let Some(record) = app.output_state.records.get(app.output_state.selected) else {
        frame.render_widget(
            Paragraph::new("No commands yet. Use Home to retrieve or deploy."),
            area,
        );
        return;
    };
    if let Some(result) = &record.result {
        draw_result_view(frame, area, app, record, result);
        return;
    }
    if record.purpose == "apex tests" {
        let run = app.latest_test_run.clone().or_else(|| {
            record
                .json
                .as_ref()
                .map(|value| crate::sf::test_results::parse(value, &record.command))
        });
        if let Some(run) = run {
            let lines = test_run_lines(app, &run, app.output_state.show_details);
            frame.render_widget(Paragraph::new(Text::from(lines)), area);
            return;
        }
    }
    let succeeded = record
        .status
        .as_ref()
        .is_some_and(|status| status.succeeded());
    let label = match &record.status {
        Some(status) if status.succeeded() => "SUCCESS",
        Some(_) => "FAILED",
        None => "RUNNING",
    };
    let color = if succeeded {
        Color::Green
    } else if record.status.is_some() {
        Color::Red
    } else {
        Color::Yellow
    };
    let mut lines = vec![
        strong(record.purpose.to_uppercase(), accent(app))
            + Span::styled(" — ", Style::default().fg(Color::DarkGray))
            + strong(label, color),
        Line::from(faint(&record.command)),
        Line::from(" "),
        Line::from(faint("[o] details   [h] Home   [Esc] Back   [r] Rerun")),
    ];
    if app.output_state.show_details {
        lines.push(Line::from(" "));
        lines.push(strong_line("DETAILS", accent(app)));
        if app.output_state.show_json
            && let Some(value) = &record.json
        {
            let view = json_view::table(value);
            for row in view.rows.iter().take(12) {
                lines.push(Line::from(faint(row.join(" | "))));
            }
            lines.push(Line::from(faint("[z] raw output")));
        } else {
            for line in record.lines.iter().rev().take(20).rev() {
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
            lines.push(Line::from(faint("[z] JSON")));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
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
        Modal::OrgMenu { selected } => {
            let items = crate::app::ORG_MENU;
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    Line::from(Span::styled(
                        format!(
                            "{} [{}] {}",
                            if index == *selected { ">" } else { " " },
                            index + 1,
                            label
                        ),
                        Style::default().fg(if index == *selected {
                            accent(app)
                        } else {
                            Color::White
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
            let mut lines = vec![strong_line("SAVED APEX TEST RUNS", accent(app))];
            for (index, run) in runs.iter().take(6).enumerate() {
                let passed = run.outcome.eq_ignore_ascii_case("Passed")
                    || run.outcome.eq_ignore_ascii_case("Pass");
                let marker = if index == *selected { ">" } else { " " };
                let color = if index == *selected {
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
            lines.push(Line::from(faint("↑↓ move   Enter open   Esc close")));
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
                        org.as_deref().unwrap_or("default org")
                    ),
                    accent(app),
                )),
                Line::from(" "),
            ];
            for (index, entry) in entries.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} [{}] {}",
                        if index == *focus { ">" } else { " " },
                        if entry.checked { "x" } else { " " },
                        entry.item.value
                    ),
                    Style::default().fg(if entry.checked {
                        Color::White
                    } else {
                        Color::DarkGray
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
                let mut command = "sf apex run test --result-format human --wait 10".to_owned();
                for class_name in classes {
                    command.push_str(&format!(" --class-names {class_name}"));
                }
                if let Some(org) = org.as_ref().filter(|org| !org.is_empty()) {
                    command.push_str(&format!(" --target-org {org}"));
                }
                command.push_str(" --json");
                lines.push(Line::from(" "));
                lines.push(Line::from(faint(command)));
            }
            lines.push(Line::from(" "));
            lines.push(Line::from(faint(
                "[Enter] run   [Esc] back   [Space] toggle   [c] command   [g] org",
            )));
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
                        options.target_org.as_deref().unwrap_or("default org")
                    ),
                    accent(app),
                )),
                Line::from(" "),
            ];
            for (index, entry) in entries.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} [{}] {}",
                        if index == *focus { ">" } else { " " },
                        if entry.checked { "x" } else { " " },
                        entry.item.value
                    ),
                    Style::default().fg(if entry.checked {
                        Color::White
                    } else {
                        Color::DarkGray
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
            lines.push(Line::from(faint(
                "[Enter] run   [Esc] back   [Space] toggle   [c] command",
            )));
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
                let suggestions = field.cached_suggestions();
                for (index, suggestion) in suggestions.iter().take(5).enumerate() {
                    let chosen = active && field.suggestion_index.unwrap_or(0) == index;
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(
                                "    {} ",
                                if field.multi_select {
                                    if field.selected.contains(&suggestion.value) {
                                        "[x]"
                                    } else {
                                        "[ ]"
                                    }
                                } else if chosen {
                                    ">"
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
                            Style::default().fg(if chosen { Color::White } else { Color::Gray }),
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
    ("h", "go Home from any screen"),
    ("Esc", "cancel active job or go back to previous screen"),
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
