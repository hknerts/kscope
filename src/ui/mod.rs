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
use ratatui::style::{Color, Modifier, Style};
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

/// The kscope mark: a pair of binoculars seen from the front — eyecups on
/// top, the hinge between the barrels, and two large objective lenses. Each
/// row pairs the glyphs with a paint mask: `b` body, `g` glass (the lens
/// interior, drawn as a background so half-blocks blend into it), `w` the
/// glint on the glass, and a space for nothing.
const ICON: [(&str, &str); 5] = [
    ("   ▄▄▄     ▄▄▄   ", "   bbb     bbb   "),
    ("  ▐███▙▄▄▄▟███▌  ", "  bbbbbbbbbbbbb  "),
    (" ▟█▀▀▀█▙ ▟█▀▀▀█▙ ", " bbgggbb bbgggbb "),
    (" █▌ ◜ ▐█ █▌ ◜ ▐█ ", " bbgwgbb bbgwgbb "),
    (" ▜█▄▄▄█▛ ▜█▄▄▄█▛ ", " bbgggbb bbgggbb "),
];
const ICON_WIDTH: u16 = 17;
/// Deep teal for the lens glass, and a bright highlight for its glint.
const GLASS: Color = Color::Rgb(0x10, 0x4e, 0x63);
const GLINT: Color = Color::Rgb(0xe8, 0xfc, 0xff);

/// Header: five rows, same height as ever. Identity and live cluster gauges
/// on the left; in the middle a strip of what kscope can do, lit up for what
/// is on screen right now, over the keys that apply here; the binoculars on
/// the right.
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
    let middle = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(cols[2]);
    draw_capabilities(f, app, middle[0]);
    draw_shortcuts(f, app, middle[1]);
    draw_icon(f, app, cols[4]);
}

/// `███░░░░░`, coloured by the same thresholds as the metrics view.
fn gauge(app: &App, label: &'static str, percent: Option<f64>) -> Vec<Span<'static>> {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    const CELLS: usize = 8;
    let Some(percent) = percent else {
        return vec![
            Span::styled(format!("{label} "), dim),
            Span::styled("░".repeat(CELLS), dim),
            Span::styled("   – ", dim),
        ];
    };
    let filled = ((percent / 100.0) * CELLS as f64)
        .round()
        .clamp(0.0, CELLS as f64) as usize;
    let color = if percent >= app.config.metrics.critical_pct {
        theme.error()
    } else if percent >= app.config.metrics.warn_pct {
        theme.warn()
    } else {
        Theme::color(&theme.info)
    };
    vec![
        Span::styled(format!("{label} "), dim),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(CELLS - filled), dim),
        Span::styled(
            format!("{:>4} ", format!("{percent:.0}%")),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]
}

fn draw_identity(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let accent = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let fg = Style::default().fg(Theme::color(&theme.fg));
    let badge = Style::default()
        .bg(theme.accent())
        .fg(Theme::color(&theme.match_fg))
        .add_modifier(Modifier::BOLD);

    // Title: the wordmark as a badge, then what is being browsed and how it
    // is narrowed.
    let mut title = vec![
        Span::raw(" "),
        Span::styled(" kscope ", badge),
        Span::styled(format!(" :{} ", app.resource_label()), accent),
    ];
    if let Some(fleet) = &app.fleet {
        let answered = fleet.members.len() - app.fleet_failed.len();
        let style = if app.fleet_failed.is_empty() {
            accent
        } else {
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD)
        };
        title.push(Span::styled(
            format!(
                "◆ {} {answered}/{} ",
                truncate(&fleet.name, 14),
                fleet.members.len()
            ),
            style,
        ));
    }
    if app.problems_only {
        title.push(Span::styled(
            "! problems ",
            Style::default().fg(theme.error()),
        ));
    }
    if !app.selector_text.is_empty() {
        title.push(Span::styled(
            format!("-l {} ", truncate(&app.selector_text, 14)),
            dim,
        ));
    }

    // The dot says whether the apiserver has answered: green once it has
    // reported a version, amber while connecting, red when it never did.
    let health = match app.k8s_version.as_str() {
        "…" | "" => Style::default().fg(theme.warn()),
        "unknown" => Style::default().fg(theme.error()),
        _ => Style::default().fg(Theme::color(&theme.info)),
    };
    let context = Line::from(vec![
        Span::styled(" ● ", health),
        Span::styled(
            truncate(&app.context_name, 26),
            fg.add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", app.k8s_version), dim),
    ]);
    let who = Line::from(vec![
        Span::styled("   user ", dim),
        Span::styled(
            truncate(
                if app.user_name.is_empty() {
                    "-"
                } else {
                    &app.user_name
                },
                16,
            ),
            fg,
        ),
        Span::styled("  ns ", dim),
        Span::styled(truncate(app.scope.label(), 12), fg),
    ]);

    let (cpu_used, cpu_cap, mem_used, mem_cap) = app.metrics.cluster_totals();
    let have = app.metrics.error.is_none() && cpu_cap > 0.0;
    let mut gauges = vec![Span::raw("   ")];
    gauges.extend(gauge(
        app,
        "cpu",
        have.then(|| crate::metrics::pct(cpu_used, cpu_cap)),
    ));
    gauges.push(Span::raw(" "));
    gauges.extend(gauge(
        app,
        "mem",
        have.then(|| crate::metrics::pct(mem_used, mem_cap)),
    ));

    let warnings = app.events.iter().filter(|e| e.type_ == "Warning").count();
    let troubled = app.rows.iter().filter(|r| r.unhealthy).count();
    let counts = Line::from(vec![
        Span::styled("   ", dim),
        Span::styled(app.rows.len().to_string(), fg.add_modifier(Modifier::BOLD)),
        Span::styled(" items · ", dim),
        Span::styled(
            troubled.to_string(),
            if troubled > 0 {
                Style::default()
                    .fg(theme.error())
                    .add_modifier(Modifier::BOLD)
            } else {
                fg
            },
        ),
        Span::styled(" trouble · ", dim),
        Span::styled(
            warnings.to_string(),
            if warnings > 0 {
                Style::default()
                    .fg(theme.warn())
                    .add_modifier(Modifier::BOLD)
            } else {
                fg
            },
        ),
        Span::styled(" warn · ", dim),
        Span::styled(app.inventory.nodes.len().to_string(), fg),
        Span::styled(" nodes", dim),
    ]);

    let lines = vec![Line::from(title), context, who, Line::from(gauges), counts];
    f.render_widget(Paragraph::new(lines), area);
}

/// One chip per thing kscope does, lit when it is what you are looking at:
/// the open tab, the describe overlay, a fleet, the triage filter.
fn draw_capabilities(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let detail = app.right == RightMode::Detail;
    let chips: [(&str, &str, bool); 6] = [
        (
            "l",
            "logs",
            detail && app.view == View::Logs && !app.describe_open,
        ),
        (
            "m",
            "metrics",
            detail && app.view == View::Metrics && !app.describe_open,
        ),
        (
            "E",
            "events",
            detail && app.view == View::Events && !app.describe_open,
        ),
        ("d", "describe", app.describe_open),
        ("◆", "fleet", app.fleet.is_some()),
        ("!", "triage", app.problems_only),
    ];
    let on = Style::default()
        .bg(theme.accent())
        .fg(Theme::color(&theme.match_fg))
        .add_modifier(Modifier::BOLD);
    let off_key = Style::default().fg(theme.accent());
    let off = Style::default().fg(Theme::color(&theme.fg));
    let dim = Style::default().fg(Theme::color(&theme.trace));

    // Whole chips only: one that would be cut at the edge is left out. The
    // read-only promise goes last, and only when there is room for it.
    let promise = "   ⊘ read-only";
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (key, name, active) in chips {
        let width = key.chars().count() + name.chars().count() + 3 + usize::from(used > 0);
        if used + width > area.width as usize {
            break;
        }
        if used > 0 {
            spans.push(Span::styled("│", dim));
        }
        if active {
            spans.push(Span::styled(format!(" {key} {name} "), on));
        } else {
            spans.push(Span::styled(format!(" {key} "), off_key));
            spans.push(Span::styled(format!("{name} "), off));
        }
        used += width;
    }
    if used + promise.chars().count() <= area.width as usize {
        spans.push(Span::styled(
            promise,
            Style::default().fg(Theme::color(&theme.info)),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_icon(f: &mut Frame, app: &App, area: Rect) {
    let body = Style::default().fg(app.config.theme.accent());
    let lines: Vec<Line> = ICON
        .iter()
        .map(|(glyphs, mask)| {
            Line::from(
                glyphs
                    .chars()
                    .zip(mask.chars())
                    .map(|(c, m)| {
                        let style = match m {
                            // Half-blocks on the rim: body colour on glass.
                            'b' => body.bg(if matches!(c, '▀' | '▄') {
                                GLASS
                            } else {
                                Color::Reset
                            }),
                            'g' => body.bg(GLASS),
                            'w' => Style::default()
                                .fg(GLINT)
                                .bg(GLASS)
                                .add_modifier(Modifier::BOLD),
                            _ => Style::default(),
                        };
                        Span::styled(c.to_string(), style)
                    })
                    .collect::<Vec<_>>(),
            )
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
            bindings.push(("Enter", "switch context / fleet"));
            bindings.push(("/", "fleet by glob"));
        }
        (Pane::Resources, RightMode::Browse) => {
            bindings.push(("j/k", "move"));
            bindings.push(("Enter", "open logs"));
            bindings.push(("d", "describe"));
            bindings.push(("/", "filter list"));
            bindings.push(("!", "problems only"));
            bindings.push(("L", "label selector"));
            bindings.push(("Ctrl-n", "namespace"));
            bindings.push(("Ctrl-r", "refresh"));
        }
        (Pane::Resources, RightMode::Detail) => {
            bindings.push(("l/m/E", "switch tab"));
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
    // On a narrow terminal, show the columns that fit whole rather than
    // squeezing every column until keys read "En" and "Ct". `?` and `q` are
    // kept: help lists everything that had to go.
    let fit = ((area.width + GUTTER) / (COL_WIDTH + GUTTER)).max(1) as usize;
    if bindings.len() > fit * rows {
        let tail = bindings.split_off(bindings.len() - 2);
        bindings.truncate(fit * rows - tail.len());
        bindings.extend(tail);
    }
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

/// The slice of a `len`-row table that is on screen with `selected`
/// highlighted, given `height` visible rows — the same window ratatui would
/// scroll to from a fresh `TableState`. Building only these rows keeps a frame
/// O(screen) rather than O(list) on a 10 000-pod namespace.
pub(crate) fn table_window(selected: usize, len: usize, height: usize) -> std::ops::Range<usize> {
    let height = height.max(1);
    let selected = selected.min(len.saturating_sub(1));
    let start = (selected + 1).saturating_sub(height);
    start..(start + height).min(len)
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
    fn table_window_keeps_the_selection_on_screen() {
        assert_eq!(table_window(0, 100, 10), 0..10);
        assert_eq!(table_window(9, 100, 10), 0..10);
        assert_eq!(table_window(10, 100, 10), 1..11);
        assert_eq!(table_window(99, 100, 10), 90..100);
        assert_eq!(table_window(3, 5, 10), 0..5);
        assert_eq!(table_window(0, 0, 10), 0..0);
    }

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("averylongpodname", 8), "averylo…");
    }
}

#[cfg(test)]
mod render_tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::tests::test_app;
    use crate::app::App;
    use crate::config::Config;
    use crate::k8s::fleet::{Fleet, Listing};
    use crate::k8s::resources::ResourceRow;

    /// Draw one frame and return it as plain text, one string per row.
    fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| super::draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[tokio::test]
    async fn the_header_shows_identity_capabilities_and_the_promise() {
        let mut app = test_app(Config::default());
        let screen = render(&mut app, 160, 30);
        let header = screen[..5].join("\n");
        assert!(header.contains(" kscope "), "{header}");
        assert!(header.contains(":pods"), "{header}");
        assert!(header.contains("stg-eu"), "{header}");
        for chip in ["logs", "metrics", "events", "describe", "fleet", "triage"] {
            assert!(header.contains(chip), "missing chip {chip}:\n{header}");
        }
        assert!(header.contains("read-only"), "{header}");
        // The binoculars: two objective lenses side by side.
        assert!(header.contains("▜█▄▄▄█▛ ▜█▄▄▄█▛"), "{header}");
    }

    #[tokio::test]
    async fn a_narrow_terminal_drops_whole_columns_instead_of_cutting_them() {
        let mut app = test_app(Config::default());
        for width in [90u16, 110, 130] {
            let screen = render(&mut app, width, 24);
            let header = screen[..5].join("\n");
            // Help and quit are always kept.
            assert!(
                header.contains("help") && header.contains("quit"),
                "{width}:\n{header}"
            );
            // No chip is cut mid-word: each name that appears is whole.
            for chip in ["describe", "metrics"] {
                let cut = &chip[..chip.len() - 1];
                assert!(
                    !header.contains(cut) || header.contains(chip),
                    "{width}: {chip} cut:\n{header}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_fleet_list_has_a_context_column_and_a_fleet_badge() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        let row = |context: &str, name: &str| ResourceRow {
            context: context.into(),
            namespace: "shop".into(),
            name: name.into(),
            status: "Running".into(),
            ..Default::default()
        };
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "checkout-1"), row("stg-us", "checkout-2")],
            failed: vec![],
        }));
        let screen = render(&mut app, 160, 30).join("\n");
        assert!(screen.contains("◆ stg 2/2"), "{screen}");
        assert!(screen.contains("fleet stg"), "{screen}");
        assert!(screen.contains("context"), "{screen}");
        assert!(
            screen.contains("stg-us") && screen.contains("checkout-2"),
            "{screen}"
        );
    }
}
