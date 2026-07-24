//! The resource browser: a table of whatever kind `:` last selected.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, Pane, RightMode};
use crate::config::Theme;
use crate::k8s::fmt_age;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let focused = app.pane == Pane::Resources && app.right == RightMode::Browse;

    let title = match &app.row_filter {
        Some(filter) => format!(
            " {} /{filter}/ · {} ",
            app.resource_label(),
            app.scope.label()
        ),
        None => format!(" {} · {} ", app.resource_label(), app.scope.label()),
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

    let rows: Vec<Row> = app
        .visible_rows()
        .map(|r| {
            let status_style = if r.unhealthy {
                Style::default()
                    .fg(theme.error())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::color(&theme.info))
            };
            let mut cells = Vec::with_capacity(4);
            if namespaced {
                cells.push(Cell::from(r.namespace.clone()).style(dim));
            }
            cells.push(
                Cell::from(r.name.clone()).style(Style::default().fg(Theme::color(&theme.fg))),
            );
            cells.push(Cell::from(r.status.clone()).style(status_style));
            cells.push(Cell::from(fmt_age(r.age_seconds)).style(dim));
            Row::new(cells)
        })
        .collect();

    let (header, widths): (Vec<&str>, Vec<Constraint>) = if namespaced {
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
        app.row_selected.min(app.row_view.len().saturating_sub(1)),
    ));
    f.render_stateful_widget(table, area, &mut state);
}
