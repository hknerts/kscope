//! Rendering. Pure functions of [`App`] state — nothing here mutates the world
//! except the viewport height, which the log view reports back so paging knows
//! how far a "page" is.
//!
//! Two panes: contexts on the left, and on the right either the resource
//! browser or, once something is opened, its logs / metrics / events.

mod contexts;
mod describe;
mod events;
mod help;
mod logs;
mod metrics;
mod resources;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, InputMode, Pane, RightMode, StatusKind, View};
use crate::config::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, app, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(chunks[1]);

    contexts::draw(f, app, body[0]);
    match app.right {
        RightMode::Browse => resources::draw(f, app, body[1]),
        RightMode::Detail => draw_detail(f, app, body[1]),
    }

    draw_status(f, app, chunks[2]);

    // The completion dropdown floats above the content, anchored to the
    // prompt at the bottom of the screen.
    if app.mode == InputMode::Command {
        draw_completions(f, app, chunks[1]);
    }

    if app.describe_open {
        describe::draw(f, app, area);
    }

    if app.mode == InputMode::Help {
        help::draw(f, app, area);
    }
}

/// The detail pane: a tab strip naming the opened object, then the tab body.
fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    let dim = Style::default().fg(Theme::color(&theme.trace));
    let tab = |label: &'static str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(Theme::color(&theme.match_fg))
                    .bg(theme.accent())
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), dim)
        }
    };

    let subject = app
        .selected_row()
        .map(|r| r.key())
        .unwrap_or_else(|| "—".into());
    let mut spans = vec![
        Span::styled(
            format!(" {} ", truncate(&subject, 40)),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        tab("l:logs", app.view == View::Logs),
        tab("m:monitoring", app.view == View::Metrics),
        tab("E:events", app.view == View::Events),
    ];
    spans.push(Span::styled("   d: describe   Esc: back", dim));
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    match app.view {
        View::Logs => logs::draw(f, app, rows[1]),
        View::Metrics => metrics::draw(f, app, rows[1]),
        View::Events => events::draw(f, app, rows[1]),
    }
}

/// The `:` palette's completion list, drawn as a dropdown sitting directly on
/// top of the prompt line.
fn draw_completions(f: &mut Frame, app: &App, body: Rect) {
    let theme = &app.config.theme;
    let (start, rows) = app.palette.visible();
    if rows.is_empty() {
        return;
    }

    // +2 for the border. Anchor to the bottom of the body so the list grows
    // upwards out of the prompt, never off the top of the screen.
    let height = (rows.len() as u16 + 2).min(body.height);
    let width = rows
        .iter()
        .map(|r| r.chars().count() as u16 + 4)
        .max()
        .unwrap_or(20)
        .clamp(24, body.width);
    let area = Rect {
        x: body.x,
        y: body.y + body.height.saturating_sub(height),
        width,
        height,
    };
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, candidate)| {
            let selected = start + i == app.palette.selected;
            let style = if selected {
                Style::default()
                    .bg(theme.accent())
                    .fg(Theme::color(&theme.match_fg))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::color(&theme.fg))
            };
            ListItem::new(Line::from(Span::styled(format!(" {candidate}"), style)))
        })
        .collect();

    let title = format!(" {} match ", app.palette.matches.len());
    f.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border_focus()))
                .title(title),
        ),
        area,
    );
}

/// The kscope wordmark as a small binoculars glyph — five rows so it has
/// room to breathe next to the identity block and the shortcut strip.
const ICON: [&str; 5] = [
    "  ___     ___  ",
    " /   \\===/   \\ ",
    "|  o  | |  o  |",
    " \\___/   \\___/ ",
    "    kscope     ",
];
const ICON_WIDTH: u16 = 17;

/// Header: a five-row "pro" bar — identity (context/version/user) on the
/// left, the context-sensitive keybinding cheat sheet in the middle, and the
/// kscope binoculars glyph on the right.
fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme.border()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(44),
            Constraint::Length(2),
            Constraint::Min(20),
            Constraint::Length(2),
            Constraint::Length(ICON_WIDTH),
        ])
        .split(inner);

    draw_identity(f, app, cols[0]);
    draw_shortcuts(f, app, cols[2]);
    draw_icon(f, app, cols[4]);
}

fn draw_identity(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let accent = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let fg = Style::default().fg(Theme::color(&theme.fg));

    let (cpu_used, cpu_cap, mem_used, mem_cap) = app.metrics.cluster_totals();
    let field = |label: &'static str, value: &str| {
        let value = if value.is_empty() { "-" } else { value }.to_string();
        Line::from(vec![
            Span::styled(format!(" {label:<8}", label = label), dim),
            Span::styled(value, fg),
        ])
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(" kscope ", accent),
            Span::styled(format!(":{} ", app.resource_label()), fg),
            if app.problems_only {
                Span::styled("problems ", Style::default().fg(theme.error()))
            } else {
                Span::raw("")
            },
            if app.selector_text.is_empty() {
                Span::raw("")
            } else {
                Span::styled(format!("-l {} ", truncate(&app.selector_text, 20)), dim)
            },
        ]),
        field("context", &app.context_name),
        field("k8s", &app.k8s_version),
        field("user", &app.user_name),
        Line::from(vec![
            Span::styled(" ns:", dim),
            Span::styled(truncate(app.scope.label(), 10), fg),
            Span::styled("  items:", dim),
            Span::styled(app.rows.len().to_string(), fg),
            Span::styled("  nodes:", dim),
            Span::styled(app.inventory.nodes.len().to_string(), fg),
            Span::styled("  cpu:", dim),
            Span::styled(
                format!("{:.0}%", crate::metrics::pct(cpu_used, cpu_cap)),
                fg,
            ),
            Span::styled(" mem:", dim),
            Span::styled(
                format!("{:.0}%", crate::metrics::pct(mem_used, mem_cap)),
                fg,
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_icon(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let accent = Style::default().fg(theme.accent());
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let lines: Vec<Line> = ICON
        .iter()
        .enumerate()
        .map(|(i, row)| {
            Line::from(Span::styled(
                *row,
                if i == ICON.len() - 1 {
                    dim.add_modifier(Modifier::BOLD)
                } else {
                    accent
                },
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// A persistent, context-sensitive keybinding grid in the header — the
/// "always visible" cheat sheet `?` otherwise hides behind an overlay.
/// Bindings are laid out in columns of `area.height` rows so they use the
/// full height of the header instead of running off one long line.
fn draw_shortcuts(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let key = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Theme::color(&theme.trace));

    let mut bindings: Vec<(&str, &str)> = vec![(":", "resource"), ("Tab", "pane")];
    match (app.pane, app.right) {
        (Pane::Contexts, _) => {
            bindings.push(("j/k", "move"));
            bindings.push(("Enter", "switch context"));
        }
        (Pane::Resources, RightMode::Browse) => {
            bindings.push(("j/k", "move"));
            bindings.push(("Enter", "open"));
            bindings.push(("Enter", "open logs"));
            bindings.push(("d", "describe"));
            bindings.push(("/", "filter list"));
            bindings.push(("!", "problems only"));
            bindings.push(("L", "label selector"));
            bindings.push(("Ctrl-n", "namespace"));
            bindings.push(("Ctrl-r", "refresh"));
        }
        (Pane::Resources, RightMode::Detail) => {
            bindings.push(("l/m/E", "logs/monitoring/events"));
            bindings.push(("d", "describe"));
            bindings.push(("Esc", "back to list"));
            match app.view {
                View::Logs => {
                    bindings.push(("v", "node service"));
                    bindings.push(("/", "search"));
                    bindings.push(("\\", "filter"));
                    bindings.push(("L/e", "level/errors"));
                    bindings.push(("F/w", "follow/wrap"));
                    bindings.push(("t/p", "time/prev"));
                    bindings.push(("c/s", "clear/save"));
                }
                View::Metrics => {
                    bindings.push(("S", "sort"));
                }
                View::Events => {
                    bindings.push(("W", "warnings only"));
                }
            }
        }
    }
    bindings.push(("?", "help"));
    bindings.push(("q", "quit"));

    // Each column is "KEY    description" (7 + up to 14 chars) plus a 2-cell
    // gutter, so adjacent columns never visually run into each other even
    // when a description fills the whole column width.
    const COL_WIDTH: u16 = 21;
    const GUTTER: u16 = 2;
    let rows = (area.height as usize).max(1);
    let col_count = bindings.len().div_ceil(rows);
    let mut constraints = Vec::with_capacity(col_count * 2);
    for i in 0..col_count {
        if i > 0 {
            constraints.push(Constraint::Length(GUTTER));
        }
        constraints.push(Constraint::Length(COL_WIDTH));
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (col_area, chunk) in cols.iter().step_by(2).zip(bindings.chunks(rows)) {
        let lines: Vec<Line> = chunk
            .iter()
            .map(|(k, desc)| {
                Line::from(vec![
                    Span::styled(format!("{k:<7}"), key),
                    Span::styled(*desc, label),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), *col_area);
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    if app.mode.prompt() != "" {
        let line = Line::from(vec![
            Span::styled(
                app.mode.prompt(),
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(app.input.clone()),
            Span::styled("█", Style::default().fg(theme.accent())),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let kind_style = match app.status.kind {
        StatusKind::Info => Style::default().fg(Theme::color(&theme.fg)),
        StatusKind::Warn => Style::default().fg(theme.warn()),
        StatusKind::Error => Style::default()
            .fg(theme.error())
            .add_modifier(Modifier::BOLD),
    };
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let mut spans = vec![Span::styled(format!(" {}", app.status.text), kind_style)];
    spans.push(Span::styled("  │  ", dim));

    match app.right {
        RightMode::Browse => {
            spans.push(Span::styled(
                format!(
                    "{}/{} {}",
                    app.row_view.len(),
                    app.rows.len(),
                    app.resource_label()
                ),
                dim,
            ));
        }
        RightMode::Detail => match app.view {
            View::Logs => {
                spans.push(Span::styled(
                    format!(
                        "{}/{} lines  drop:{}  {}{}",
                        app.buffer.view_len(),
                        app.buffer.len(),
                        app.buffer.dropped,
                        if app.buffer.filter.is_active() {
                            format!("filter:{}  ", app.buffer.filter.describe())
                        } else {
                            String::new()
                        },
                        if app.follow { "FOLLOW" } else { "paused" }
                    ),
                    dim,
                ));
                if app.search.regex.is_some() {
                    spans.push(Span::styled(
                        format!("  /{}/ {} hits", app.search.query, app.search.total),
                        Style::default().fg(theme.accent()),
                    ));
                }
            }
            View::Events => {
                let warnings = app.visible_events().filter(|e| e.is_warning()).count();
                spans.push(Span::styled(
                    format!("{} events", app.event_view.len()),
                    dim,
                ));
                if warnings > 0 {
                    spans.push(Span::styled(
                        format!("  {warnings} warnings"),
                        Style::default().fg(theme.error()),
                    ));
                }
            }
            View::Metrics => {
                let age = app
                    .metrics_age()
                    .map(|d| format!("{}s ago", d.as_secs()))
                    .unwrap_or_else(|| "waiting".into());
                spans.push(Span::styled(
                    format!(
                        "sort:{}  refresh:{}  samples:{}",
                        app.sort_by.label(),
                        age,
                        app.metrics.history
                    ),
                    dim,
                ));
            }
        },
    }
    spans.push(Span::styled("  │  ? help", dim));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// A centred popup rect covering `pct_x` / `pct_y` percent of `area`.
pub(crate) fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("averylongpodname", 8), "averylo…");
    }
}
