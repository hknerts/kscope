//! Rendering. Pure functions of [`App`] state — nothing here mutates the world
//! except the viewport height, which the log view reports back so paging knows
//! how far a "page" is.

mod help;
mod logs;
mod metrics;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, InputMode, Pane, SidebarItem, StatusKind, View};
use crate::config::Theme;
use crate::k8s::fmt_age;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, app, chunks[0]);
    draw_hints(f, app, chunks[1]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(20)])
        .split(chunks[2]);

    draw_sidebar(f, app, body[0]);
    match app.view {
        View::Logs => logs::draw(f, app, body[1]),
        View::Metrics => metrics::draw(f, app, body[1]),
    }

    draw_status(f, app, chunks[3]);

    if app.mode == InputMode::Help {
        help::draw(f, app, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let accent = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
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

    let (cpu_used, cpu_cap, mem_used, mem_cap) = app.metrics.cluster_totals();
    let line = Line::from(vec![
        Span::styled(" kscope ", accent),
        tab("1:logs", app.view == View::Logs),
        tab("2:metrics", app.view == View::Metrics),
        Span::raw("  "),
        Span::styled("ns:", dim),
        Span::raw(app.scope.label().to_string()),
        Span::raw("  "),
        Span::styled("pods:", dim),
        Span::raw(app.inventory.pods.len().to_string()),
        Span::raw("  "),
        Span::styled("nodes:", dim),
        Span::raw(app.inventory.nodes.len().to_string()),
        Span::raw("  "),
        Span::styled("cluster:", dim),
        Span::raw(format!(
            "cpu {:.0}% mem {:.0}%",
            crate::metrics::pct(cpu_used, cpu_cap),
            crate::metrics::pct(mem_used, mem_cap)
        )),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// A persistent, context-sensitive keybinding strip under the header —
/// the "always visible" cheat sheet `?` otherwise hides behind an overlay.
fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let key = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Theme::color(&theme.trace));

    let mut bindings: Vec<(&str, &str)> = vec![("Tab", "pane"), ("1/2", "view")];
    if app.pane == Pane::Sidebar {
        bindings.push(("j/k", "move"));
        bindings.push(("Enter", "open/expand"));
        bindings.push(("a/x", "attach/detach"));
        bindings.push(("Ctrl-n", "namespace"));
        bindings.push(("Ctrl-p", "filter pods"));
    } else {
        match app.view {
            View::Logs => {
                bindings.push(("j/k", "scroll"));
                bindings.push(("/", "search"));
                bindings.push(("\\", "filter"));
                bindings.push(("L/e", "level/errors"));
                bindings.push(("F/w", "follow/wrap"));
                bindings.push(("t/p", "time/prev"));
                bindings.push(("c/s", "clear/save"));
            }
            View::Metrics => {
                bindings.push(("j/k", "move"));
                bindings.push(("m", "nodes/pods/volumes"));
                bindings.push(("S", "sort"));
            }
        }
    }
    bindings.push(("?", "help"));
    bindings.push(("q", "quit"));

    let mut spans = Vec::with_capacity(bindings.len() * 3);
    for (i, (k, desc)) in bindings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", label));
        }
        spans.push(Span::styled(*k, key));
        spans.push(Span::styled(format!(" {desc}"), label));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let focused = app.pane == Pane::Sidebar;
    let border = if focused {
        theme.border_focus()
    } else {
        theme.border()
    };

    let title = match &app.pod_filter {
        Some(f) => format!(" pods /{f}/ "),
        None => " pods ".to_string(),
    };

    let mut items: Vec<ListItem> = Vec::with_capacity(app.sidebar.len());
    for item in &app.sidebar {
        match item {
            SidebarItem::Group { key, kind, name } => {
                let marker = if app.collapsed_groups.contains(key) {
                    "▸"
                } else {
                    "▾"
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(theme.border())),
                    Span::styled(
                        truncate(name, 22),
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {kind}"),
                        Style::default().fg(Theme::color(&theme.trace)),
                    ),
                ])));
            }
            SidebarItem::Pod { pod, key } => {
                let Some(info) = app.inventory.pods.get(*pod) else {
                    continue;
                };
                let (ready, total) = info.ready();
                let marker = if app.expanded.contains(key) { "▾" } else { "▸" };
                let color = if info.healthy() {
                    Theme::color(&theme.info)
                } else {
                    theme.error()
                };
                let indent = if info.owner_kind.is_empty() { "" } else { "  " };
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(format!("{marker} "), Style::default().fg(theme.border())),
                    Span::styled(truncate(&info.name, 20), Style::default().fg(color)),
                    Span::styled(
                        format!(
                            " {ready}/{total} ↺{} {}",
                            info.restarts(),
                            fmt_age(info.age_seconds)
                        ),
                        Style::default().fg(Theme::color(&theme.trace)),
                    ),
                ])));
            }
            SidebarItem::Container { name, key, pod } => {
                let attached = app.attached.iter().any(|s| s.as_ref() == key);
                let state = app
                    .inventory
                    .pods
                    .get(*pod)
                    .and_then(|p| p.containers.iter().find(|c| &c.name == name));
                let ok = state.map(|c| c.ready).unwrap_or(false);
                let indent = app
                    .inventory
                    .pods
                    .get(*pod)
                    .map(|p| if p.owner_kind.is_empty() { "   " } else { "     " })
                    .unwrap_or("   ");
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(
                        if attached { "● " } else { "○ " },
                        Style::default().fg(if attached {
                            theme.accent()
                        } else {
                            theme.border()
                        }),
                    ),
                    Span::styled(
                        truncate(name, 18),
                        Style::default().fg(if ok {
                            Theme::color(&theme.fg)
                        } else {
                            theme.warn()
                        }),
                    ),
                    Span::styled(
                        format!(" ↺{}", state.map(|c| c.restarts).unwrap_or(0)),
                        Style::default().fg(Theme::color(&theme.trace)),
                    ),
                ])));
            }
        }
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(theme.accent())
                .fg(Theme::color(&theme.match_fg))
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default();
    if !app.sidebar.is_empty() {
        state.select(Some(app.sidebar_selected.min(app.sidebar.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
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
    match app.view {
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
