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
    let layout = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .split(frame.area());
    draw_header(frame, layout[0], app);
    match app.tab {
        Tab::Catalog => draw_catalog(frame, layout[1], app),
        Tab::Favorites => draw_favorites(frame, layout[1], app),
        Tab::Orgs => draw_orgs(frame, layout[1], app),
        Tab::Deploy => draw_deploy(frame, layout[1], app),
        Tab::Output => draw_output(frame, layout[1], app),
        Tab::Setup => draw_setup(frame, layout[1], app),
    }
    draw_footer(frame, layout[2], app);
    if let Some(modal) = &app.modal {
        draw_modal(frame, modal, app);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let version = app.startup.version.as_deref().unwrap_or("unknown");
    let catalog_source = app.startup.catalog.source.label();
    let busy = if app.busy() {
        let frames = ["-", "\\", "|", "/"];
        let spinner = frames[app.spinner_tick() % frames.len()];
        format!("{spinner} running")
    } else {
        "idle".to_owned()
    };
    let info = Line::from(vec![
        strong("SFTUI", accent(app)),
        faint("  ·  sftui v0.1.0"),
        faint(format!("  ·  sf {version}")),
        faint(format!("  ·  catalog: {catalog_source}")),
        Span::styled(
            format!("  ·  {busy}"),
            Style::default().fg(if app.busy() {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
    ]);
    let tabs = Tabs::new(app.tabs().into_iter().map(|tab| tab.label()))
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

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
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
    let common = "Tab/1-5 switch · ? help · F5 refresh · y copy · q quit";
    let specific = match app.tab {
        Tab::Catalog => "/ search · ↑↓ move · Enter help · r run · f favorite · e preset",
        Tab::Favorites => "↑↓ move · Enter run · Ctrl+↑↓ reorder · e preset · x remove",
        Tab::Orgs => {
            "↑↓ move · Enter detail · o open · d/D default · x logout · l login · c scratch"
        }
        Tab::Deploy => {
            "↑↓ move · Space select · m mode · v validate · t test · w warnings · o org · Enter run"
        }
        Tab::Output => "↑↓ records · j/k scroll · z JSON · Y copy output · Esc cancel job",
        Tab::Setup => "↑↓ option · Enter run installer · r re-probe",
    };
    format!("{specific}     {common}")
}

fn draw_catalog(frame: &mut Frame, area: Rect, app: &App) {
    let columns =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).split(area);
    let rows = app.catalog_rows();
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
                ListItem::new(vec![
                    Line::from(spans),
                    Line::from(Span::styled(
                        format!("      {summary}"),
                        Style::default().fg(Color::DarkGray),
                    )),
                ])
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
        "Enter loads live --help · r run · f favorite"
    })));
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
    lines.push(Line::from(faint(
        "Ctrl+↑/↓ reorder · e edit flags · x remove · c copy command",
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("favorite detail", app)),
        columns[1],
    );
}

fn draw_orgs(frame: &mut Frame, area: Rect, app: &App) {
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
    lines.push(Line::from(faint(
        "o open in browser · d default · D dev hub · x logout · l login · c scratch",
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("organization", app)),
        columns[1],
    );
}

fn draw_deploy(frame: &mut Frame, area: Rect, app: &App) {
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
                PayloadMode::TrackedChanges => false,
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
    lines.push(Line::from(faint(
        "m payload · v validation · t test level · w warnings · r retrieve · o target org · p poll",
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block("monitor", app)),
        sections[2],
    );
}

fn draw_output(frame: &mut Frame, area: Rect, app: &App) {
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
            let lines: Vec<Line> = HELP_ROWS
                .iter()
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
                for (byte_index, character) in field.value.char_indices() {
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
                if active && field.cursor == field.value.len() {
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
    ("Tab / Shift+Tab", "cycle screens"),
    ("1-6", "jump to a screen"),
    ("?", "show this help"),
    ("F5", "refresh orgs, project or command catalog"),
    ("q", "quit sftui"),
    ("Ctrl+C", "cancel the newest job, or quit when idle"),
    ("/", "focus the catalog fuzzy search"),
    (
        "Enter (catalog)",
        "load live --help for the selected command",
    ),
    ("r (catalog)", "run a command with optional flags"),
    ("f / e (catalog)", "toggle favorite / edit preset flags"),
    ("Enter (favorites)", "run with preset flags"),
    ("Ctrl+↑/↓", "reorder favorites"),
    ("Enter (orgs)", "load org display details"),
    ("d / D (orgs)", "set project default org / dev hub"),
    (
        "o / x / l / c",
        "open / logout / login web / create scratch",
    ),
    ("Space (deploy)", "select metadata path"),
    (
        "m / v / t / w",
        "payload mode / validation / test level / warnings",
    ),
    ("r (deploy)", "toggle deploy vs retrieve"),
    (
        "Enter (deploy)",
        "start transfer; poll reports automatically",
    ),
    ("z (output)", "toggle raw stream and parsed JSON table"),
    ("Y (output)", "copy the selected command's output"),
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
