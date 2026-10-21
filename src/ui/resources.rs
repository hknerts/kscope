//! The resource browser: a table of whatever kind `:` last selected.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, Pane, RightMode};
use crate::config::Theme;
use crate::k8s::fmt_age;
use crate::k8s::resources::{fmt_usage, short_image};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let focused = app.pane == Pane::Resources && app.right == RightMode::Browse;

    let place = match &app.fleet {
        Some(fleet) => format!("{} · fleet {}", app.scope.label(), fleet.name),
        None => app.scope.label().to_string(),
    };
    let title = match &app.row_filter {
        Some(filter) => format!(" {} /{filter}/ · {place} ", app.resource_label()),
        None => format!(" {} · {place} ", app.resource_label()),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme.border_focus()
        } else {
            theme.border()
        }))
        .title(title);

    if let Some(err) = &app.rows_error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {err}"),
                Style::default().fg(theme.error()),
            )))
            .block(block),
            area,
        );
        return;
    }

    if app.row_view.is_empty() {
        let hint = if app.loading {
            " loading…".to_string()
        } else if app.current_type().is_none() {
            " press : to pick a resource type".to_string()
        } else if app.rows.is_empty() {
            format!(" no {} in {}", app.resource_label(), app.scope.label())
        } else if app.problems_only {
            format!(
                " no {} in trouble — press ! to show all",
                app.resource_label()
            )
        } else {
            " nothing matches the filter".to_string()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, dim))).block(block),
            area,
        );
        return;
    }

    // Cluster-scoped kinds have no namespace worth a column.
    let namespaced = app.current_type().map(|t| t.namespaced).unwrap_or(true);
    let fleet = app.fleet.is_some();
    // Pods and workloads get an image column; kinds without containers don't.
    let with_images = app
        .row_view
        .iter()
        .any(|&i| app.rows.get(i).is_some_and(|r| !r.images.is_empty()));
    // Anything that runs or selects pods gets cpu and memory, "-" without
    // metrics-server.
    let with_usage = app
        .row_view
        .iter()
        .any(|&i| app.rows.get(i).is_some_and(|r| r.runs_pods()));

    // Borders and the header row take three lines.
    let window = super::table_window(
        app.row_selected,
        app.row_view.len(),
        area.height.saturating_sub(3) as usize,
    );
    let rows: Vec<Row> = app.row_view[window.clone()]
        .iter()
        .filter_map(|&i| app.rows.get(i))
        .map(|r| {
            let status_style = if r.unhealthy {
                Style::default()
                    .fg(theme.error())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::color(&theme.info))
            };
            let mut cells = Vec::with_capacity(5);
            if fleet {
                // The connected member stands out: its rows open without a
                // context switch.
                let style = if r.context == app.context_name {
                    Style::default().fg(theme.accent())
                } else {
                    dim
                };
                cells.push(Cell::from(r.context.clone()).style(style));
            }
            if namespaced {
                cells.push(Cell::from(r.namespace.clone()).style(dim));
            }
            cells.push(
                Cell::from(r.name.clone()).style(Style::default().fg(Theme::color(&theme.fg))),
            );
            cells.push(Cell::from(r.status.clone()).style(status_style));
            if with_usage {
                let (cpu, mem) = fmt_usage(r.usage);
                let style = Style::default().fg(Theme::color(&theme.fg));
                cells.push(Cell::from(Line::from(cpu).right_aligned()).style(style));
                cells.push(Cell::from(Line::from(mem).right_aligned()).style(style));
            }
            if with_images {
                let images: Vec<String> = r.images.iter().map(|i| short_image(i)).collect();
                cells.push(Cell::from(images.join(", ")).style(dim));
            }
            cells.push(Cell::from(fmt_age(r.age_seconds)).style(dim));
            Row::new(cells)
        })
        .collect();

    let (mut header, mut widths): (Vec<&str>, Vec<Constraint>) = if namespaced {
        (
            vec!["namespace", "name", "status", "age"],
            vec![
                Constraint::Length(18),
                Constraint::Min(20),
                Constraint::Length(18),
                Constraint::Length(8),
            ],
        )
    } else {
        (
            vec!["name", "status", "age"],
            vec![
                Constraint::Min(20),
                Constraint::Length(18),
                Constraint::Length(8),
            ],
        )
    };

    // Both go between status and age, usage first.
    if with_usage {
        let at = header.len() - 1;
        header.splice(at..at, ["cpu", "memory"]);
        widths.splice(at..at, [Constraint::Length(7), Constraint::Length(9)]);
    }
    if with_images {
        // Name and image share what is left.
        let at = header.len() - 1;
        header.insert(at, "image");
        widths.insert(at, Constraint::Min(20));
    }

    if fleet {
        let width = app.rows.iter().map(|r| r.context.len()).max().unwrap_or(7);
        header.insert(0, "context");
        widths.insert(0, Constraint::Length(width.clamp(7, 24) as u16));
    }

    let table = Table::new(rows, widths)
        .header(Row::new(header).style(dim.add_modifier(Modifier::BOLD)))
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(theme.accent())
                .fg(Theme::color(&theme.match_fg))
                .add_modifier(Modifier::BOLD),
        );

    let mut state = TableState::default();
    state.select(Some(
        app.row_selected.min(app.row_view.len().saturating_sub(1)) - window.start,
    ));
    f.render_stateful_widget(table, area, &mut state);
}
