//! The events pane.
//!
//! Cluster events are the first place you look when a pod will not start, so
//! the table leads with age and severity and gives the message whatever width
//! is left over.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, RightMode, View};
use crate::config::Theme;
use crate::k8s::fmt_age;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let focused = app.right == RightMode::Detail && app.view == View::Events;

    // The pane is always scoped to the object open in the detail view.
    let subject = app.event_selector().unwrap_or_else(|| "—".into());
    let title = if app.warnings_only {
        format!(" events · {subject} · warnings ")
    } else {
        format!(" events · {subject} ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme.border_focus()
        } else {
            theme.border()
        }))
        .title(title);

    if let Some(err) = &app.events_error {
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

    if app.event_view.is_empty() {
        // An empty list is the normal case for a healthy resource, so say
        // which resource that is rather than implying something is wrong.
        // Say what *is* there. An object with no events is the healthy case,
        // and silently showing an empty box reads like a broken pane.
        let hint = if app.events.is_empty() {
            format!(" no events in {} at all", app.scope.label())
        } else if app.warnings_only {
            format!(
                " no warnings for {subject} — press W to show all {} events",
                app.events.len()
            )
        } else {
            format!(
                " no events for {subject} — the namespace has {} for other objects",
                app.events.len()
            )
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, dim))).block(block),
            area,
        );
        return;
    }

    let rows: Vec<Row> = app
        .visible_events()
        .map(|e| {
            let severity = if e.is_warning() {
                Style::default()
                    .fg(theme.error())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::color(&theme.info))
            };
            Row::new(vec![
                Cell::from(fmt_age(e.age_seconds)).style(dim),
                Cell::from(if e.is_warning() { "W" } else { "N" }).style(severity),
                Cell::from(e.reason.clone()).style(severity),
                Cell::from(e.resource()).style(Style::default().fg(Theme::color(&theme.fg))),
                Cell::from(e.namespace.clone()).style(dim),
                Cell::from(if e.count > 1 {
                    format!("x{}", e.count)
                } else {
                    String::new()
                })
                .style(dim),
                Cell::from(e.message.clone()).style(Style::default().fg(Theme::color(&theme.fg))),
            ])
        })
        .collect();

    let header = Row::new(vec![
        "age",
        "",
        "reason",
        "resource",
        "namespace",
        "n",
        "message",
    ])
    .style(dim.add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Length(22),
            Constraint::Length(34),
            Constraint::Length(16),
            Constraint::Length(5),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(block)
    .row_highlight_style(
        Style::default()
            .bg(theme.accent())
            .fg(Theme::color(&theme.match_fg))
            .add_modifier(Modifier::BOLD),
    );

    let mut state = TableState::default();
    state.select(Some(
        app.event_selected
            .min(app.event_view.len().saturating_sub(1)),
    ));
    f.render_stateful_widget(table, area, &mut state);
}
