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
use crate::k8s::discovery::Scope;

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
        // Describe takes the place of the list, in the same window.
        RightMode::Browse if app.describe_open => describe::draw(f, app, body[1]),
        RightMode::Browse => resources::draw(f, app, body[1]),
        RightMode::Detail => draw_detail(f, app, body[1]),
    }

    draw_status(f, app, chunks[2]);

    // The completion dropdown floats above the content, anchored to the
    // prompt at the bottom of the screen.
    if app.mode == InputMode::Command {
        draw_completions(f, app, chunks[1]);
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
    let key_style = Style::default().fg(theme.accent());
    let name_style = Style::default().fg(Theme::color(&theme.fg));
    let on = Style::default()
        .fg(Theme::color(&theme.match_fg))
        .bg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let subject = app
        .selected_row()
        .map(|r| r.key())
        .unwrap_or_else(|| "—".into());

    // One tab per view, each with its key, the open one highlighted; the
    // same four names the header and help use.
    let mut spans = vec![Span::styled(
        format!(" {}   ", truncate(&subject, 40)),
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD),
    )];
    let tabs = [
        ("l", "logs", app.view == View::Logs && !app.describe_open),
        (
            "m",
            "metrics",
            app.view == View::Metrics && !app.describe_open,
        ),
        (
            "e",
            "events",
            app.view == View::Events && !app.describe_open,
        ),
        ("d", "describe", app.describe_open),
    ];
    for (key, name, active) in tabs {
        if active {
            spans.push(Span::styled(format!(" {key} {name} "), on));
        } else {
            spans.push(Span::styled(format!(" {key} "), key_style));
            spans.push(Span::styled(format!("{name} "), name_style));
        }
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("    Esc back", dim));
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    if app.describe_open {
        describe::draw(f, app, rows[1]);
        return;
    }
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

/// Header: five rows. On the left, where you are — cluster, what is listed,
/// how loaded it is, what is wrong — as labelled, aligned rows. In the
/// middle, only the keys that do something on this screen, in aligned
/// columns. On the right, the binoculars.
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
            Constraint::Length(IDENTITY_WIDTH),
            Constraint::Length(3),
            Constraint::Min(20),
            Constraint::Length(2),
            Constraint::Length(ICON_WIDTH),
        ])
        .split(inner);

    draw_identity(f, app, cols[0]);
    draw_shortcuts(f, app, cols[2]);
    draw_icon(f, app, cols[4]);
}

const IDENTITY_WIDTH: u16 = 46;
/// Width of the label column on the left, so every value starts aligned.
const LABEL_WIDTH: usize = 10;

/// `███░░░░░ 41%`, coloured by the same thresholds as the metrics view.
fn gauge(app: &App, percent: Option<f64>) -> Vec<Span<'static>> {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    const CELLS: usize = 8;
    let Some(percent) = percent else {
        return vec![Span::styled(format!("{}   –", "░".repeat(CELLS)), dim)];
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
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(CELLS - filled), dim),
        Span::styled(
            format!("{:>4}", format!("{percent:.0}%")),
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
    let strong = fg.add_modifier(Modifier::BOLD);
    let bad = Style::default()
        .fg(theme.error())
        .add_modifier(Modifier::BOLD);
    let warn = Style::default()
        .fg(theme.warn())
        .add_modifier(Modifier::BOLD);
    let label = |text: &str| Span::styled(format!(" {text:<w$}", w = LABEL_WIDTH - 1), dim);

    // Brand, the promise, and the fleet when one is being browsed.
    let mut title = vec![
        Span::raw(" "),
        Span::styled(
            " kscope ",
            Style::default()
                .bg(theme.accent())
                .fg(Theme::color(&theme.match_fg))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  read-only",
            Style::default().fg(Theme::color(&theme.info)),
        ),
    ];
    if let Some(fleet) = &app.fleet {
        let answered = fleet.members.len() - app.fleet_failed.len();
        title.push(Span::styled("   fleet ", dim));
        title.push(Span::styled(truncate(&fleet.name, 12), accent));
        title.push(Span::styled(
            format!(" {answered}/{} clusters", fleet.members.len()),
            if app.fleet_failed.is_empty() {
                dim
            } else {
                warn
            },
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
        label("context"),
        Span::styled("● ", health),
        Span::styled(truncate(&app.context_name, 24), strong),
        Span::styled(format!("  {}", app.k8s_version), dim),
    ]);

    // What the list holds, in words: "pods in shop · only problems".
    let mut showing = vec![
        label("showing"),
        Span::styled(app.resource_label(), strong),
        Span::styled(" in ", dim),
        Span::styled(
            match &app.scope {
                Scope::AllNamespaces => "all namespaces".to_string(),
                Scope::Namespace(ns) => truncate(ns, 16),
            },
            strong,
        ),
    ];
    if app.problems_only {
        showing.push(Span::styled(" · only problems", bad));
    }
    if !app.selector_text.is_empty() {
        showing.push(Span::styled(
            format!(" · {}", truncate(&app.selector_text, 12)),
            accent,
        ));
    }

    let (cpu_used, cpu_cap, mem_used, mem_cap) = app.metrics.cluster_totals();
    let have = app.metrics.error.is_none() && cpu_cap > 0.0;
    let mut usage = vec![label("cpu")];
    usage.extend(gauge(
        app,
        have.then(|| crate::metrics::pct(cpu_used, cpu_cap)),
    ));
    usage.push(Span::styled("   mem ", dim));
    usage.extend(gauge(
        app,
        have.then(|| crate::metrics::pct(mem_used, mem_cap)),
    ));

    let warnings = app.events.iter().filter(|e| e.type_ == "Warning").count();
    let troubled = app.rows.iter().filter(|r| r.unhealthy).count();
    let count = |n: usize, one: &str, many: &str, style: Style| {
        vec![
            Span::styled(n.to_string(), if n > 0 { style } else { fg }),
            Span::styled(format!(" {}", if n == 1 { one } else { many }), dim),
        ]
    };
    let mut status = vec![label("health")];
    status.extend(count(app.rows.len(), "item", "items", strong));
    status.push(Span::styled(" · ", dim));
    status.extend(count(troubled, "problem", "problems", bad));
    status.push(Span::styled(" · ", dim));
    status.extend(count(warnings, "warning", "warnings", warn));

    let lines = vec![
        Line::from(title),
        context,
        Line::from(showing),
        Line::from(usage),
        Line::from(status),
    ];
    f.render_widget(Paragraph::new(lines), area);
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

/// The keys that do something on this screen, in columns of `area.height`
/// rows. Within a column every key is padded to the widest one, so the
/// descriptions line up; columns that do not fit whole are left out (`?`
/// lists everything) rather than squeezed.
fn draw_shortcuts(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let key = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Theme::color(&theme.fg));

    let mut bindings: Vec<(&str, &str)> = Vec::new();
    match (app.pane, app.right) {
        (Pane::Contexts, _) => {
            bindings.push(("Enter", "switch to it"));
            bindings.push(("/", "fleet, e.g. stg"));
            bindings.push(("↑/↓", "fleets and contexts"));
            bindings.push(("Tab", "back to the list"));
        }
        (Pane::Resources, RightMode::Browse) => {
            bindings.push(("Enter", "open logs"));
            bindings.push(("m e d", "metrics, events, yaml"));
            bindings.push(("/", "filter by name"));
            bindings.push(("!", "only problems"));
            bindings.push((":", "resource type"));
            bindings.push(("L", "label selector"));
            bindings.push(("Ctrl-n", "namespace"));
            bindings.push(("Tab", "clusters"));
        }
        (Pane::Resources, RightMode::Detail) => {
            match app.view {
                View::Logs => {
                    bindings.push(("/", "search"));
                    bindings.push(("n N", "next, previous"));
                    bindings.push(("\\", "filter lines"));
                    bindings.push(("L", "minimum level"));
                    bindings.push(("F", "follow"));
                    bindings.push(("w", "wrap"));
                    bindings.push(("t", "timestamps"));
                    bindings.push(("p", "previous container"));
                    if app
                        .log_target()
                        .is_some_and(|t| t.kind.eq_ignore_ascii_case("node"))
                    {
                        bindings.push(("v", "node service"));
                    }
                    bindings.push(("s", "save to file"));
                }
                View::Metrics => {
                    bindings.push(("S", "sort"));
                    bindings.push(("j/k", "move"));
                }
                View::Events => {
                    bindings.push(("W", "warnings only"));
                    bindings.push(("j/k", "move"));
                }
            }
            bindings.push(("Esc", "back to the list"));
        }
    }
    let tail = [("?", "all keys"), ("q", "quit")];

    let rows = (area.height as usize).max(1);
    const GUTTER: usize = 3;
    let width_of = |column: &[(&str, &str)]| {
        let keys = column
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        let descs = column
            .iter()
            .map(|(_, d)| d.chars().count())
            .max()
            .unwrap_or(0);
        (keys, keys + 1 + descs)
    };
    // Drop bindings from the end (keeping `?` and `q`) until the columns fit.
    let columns = loop {
        let all: Vec<(&str, &str)> = bindings.iter().copied().chain(tail).collect();
        let columns: Vec<Vec<(&str, &str)>> = all.chunks(rows).map(<[_]>::to_vec).collect();
        let total: usize = columns.iter().map(|c| width_of(c).1).sum::<usize>()
            + GUTTER * columns.len().saturating_sub(1);
        if total <= area.width as usize || bindings.is_empty() {
            break columns;
        }
        bindings.pop();
    };

    let mut x = area.x;
    for column in columns {
        let (key_width, width) = width_of(&column);
        let lines: Vec<Line> = column
            .iter()
            .map(|(k, desc)| {
                Line::from(vec![
                    Span::styled(format!("{k:<key_width$} "), key),
                    Span::styled(*desc, label),
                ])
            })
            .collect();
        let width = (width as u16).min(area.right().saturating_sub(x));
        f.render_widget(
            Paragraph::new(lines),
            Rect::new(x, area.y, width, area.height),
        );
        x = x.saturating_add(width + GUTTER as u16);
        if x >= area.right() {
            break;
        }
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
    let accent = Style::default().fg(theme.accent());

    // Right-aligned facts about what is on screen; only the ones that say
    // something (no "drop:0", no filter when nothing is filtered).
    let mut facts: Vec<Span> = Vec::new();
    let mut fact = |text: String, style: Style| {
        if !facts.is_empty() {
            facts.push(Span::styled(" · ", dim));
        }
        facts.push(Span::styled(text, style));
    };
    match app.right {
        RightMode::Browse => {
            let (shown, total) = (app.row_view.len(), app.rows.len());
            fact(
                if shown == total {
                    format!("{total} {}", app.resource_label())
                } else {
                    format!("{shown} of {total} {}", app.resource_label())
                },
                dim,
            );
        }
        RightMode::Detail => match app.view {
            View::Logs => {
                let (shown, kept) = (app.buffer.view_len(), app.buffer.len());
                fact(
                    if shown == kept {
                        format!("{kept} lines")
                    } else {
                        format!("{shown} of {kept} lines")
                    },
                    dim,
                );
                if app.buffer.dropped > 0 {
                    fact(format!("{} evicted", app.buffer.dropped), dim);
                }
                if app.buffer.filter.is_active() {
                    fact(format!("filter {}", app.buffer.filter.describe()), accent);
                }
                fact(
                    if app.follow { "following" } else { "paused" }.to_string(),
                    if app.follow {
                        dim
                    } else {
                        Style::default().fg(theme.warn())
                    },
                );
                if app.search.regex.is_some() {
                    fact(
                        format!("/{}/ {} matches", app.search.query, app.search.total),
                        accent,
                    );
                }
            }
            View::Events => {
                let warnings = app.visible_events().filter(|e| e.is_warning()).count();
                fact(format!("{} events", app.event_view.len()), dim);
                if warnings > 0 {
                    fact(
                        format!("{warnings} warnings"),
                        Style::default().fg(theme.warn()),
                    );
                }
            }
            View::Metrics => {
                fact(format!("sorted by {}", app.sort_by.label()), dim);
                fact(
                    app.metrics_age()
                        .map(|d| format!("updated {}s ago", d.as_secs()))
                        .unwrap_or_else(|| "waiting for metrics".into()),
                    dim,
                );
            }
        },
    }
    fact("? help".to_string(), dim);
    facts.push(Span::raw(" "));

    let right_width = facts
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>() as u16;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(right_width)])
        .split(area);
    let message = truncate(&app.status.text, cols[0].width.saturating_sub(2) as usize);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {message}"), kind_style))),
        cols[0],
    );
    f.render_widget(Paragraph::new(Line::from(facts)), cols[1]);
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
    async fn the_header_says_where_you_are_in_words() {
        let mut app = test_app(Config::default());
        let screen = render(&mut app, 160, 30);
        let header = screen[..5].join("\n");
        assert!(header.contains(" kscope "), "{header}");
        assert!(header.contains("read-only"), "{header}");
        assert!(header.contains("context  ● stg-eu"), "{header}");
        assert!(header.contains("showing  pods in default"), "{header}");
        assert!(
            header.contains("health   0 items · 0 problems · 0 warnings"),
            "{header}"
        );
        // The binoculars: two objective lenses side by side.
        assert!(header.contains("▜█▄▄▄█▛ ▜█▄▄▄█▛"), "{header}");

        app.problems_only = true;
        let header = render(&mut app, 160, 30)[..5].join("\n");
        assert!(
            header.contains("pods in default · only problems"),
            "{header}"
        );
    }

    #[tokio::test]
    async fn shortcut_descriptions_line_up_within_a_column() {
        let mut app = test_app(Config::default());
        let screen = render(&mut app, 160, 30);
        // The first shortcut column holds Enter, `m e d`, `/`, `!` and `:`.
        let starts: Vec<usize> = [
            "open logs",
            "metrics, events, yaml",
            "filter by name",
            "only problems",
        ]
        .iter()
        .map(|desc| {
            let row = screen[..5].iter().find(|r| r.contains(desc)).expect(desc);
            // Columns are counted in characters, not bytes: the gauges are
            // multi-byte.
            row[..row.find(desc).unwrap()].chars().count()
        })
        .collect();
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "{starts:?}\n{}",
            screen[..5].join("\n")
        );
    }

    #[tokio::test]
    async fn a_narrow_terminal_drops_whole_columns_instead_of_cutting_them() {
        let mut app = test_app(Config::default());
        for width in [90u16, 110, 130] {
            let screen = render(&mut app, width, 24);
            let header = screen[..5].join("\n");
            // Help and quit are always kept.
            assert!(
                header.contains("all keys") && header.contains("quit"),
                "{width}:\n{header}"
            );
            // No description is cut mid-word.
            for desc in ["label selector", "resource type", "namespace"] {
                let cut = &desc[..desc.len() - 2];
                assert!(
                    !header.contains(cut) || header.contains(desc),
                    "{width}: {desc} cut:\n{header}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_status_bar_only_states_what_says_something() {
        let mut app = test_app(Config::default());
        let screen = render(&mut app, 160, 30);
        let status = &screen[29];
        assert!(status.trim_end().ends_with("0 pods · ? help"), "{status}");
        assert!(
            !status.contains("drop:") && !status.contains("filter"),
            "{status}"
        );
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
        assert!(screen.contains("fleet stg 2/2 clusters"), "{screen}");
        assert!(screen.contains("fleet stg"), "{screen}");
        assert!(screen.contains("context"), "{screen}");
        assert!(
            screen.contains("stg-us") && screen.contains("checkout-2"),
            "{screen}"
        );
    }

    #[tokio::test]
    async fn fleets_sit_in_their_own_box_above_the_contexts() {
        let mut config = Config::default();
        config.fleets.insert("stg".into(), vec!["stg-*".into()]);
        let mut app = test_app(config);
        app.pane = crate::app::Pane::Contexts;
        app.context_selected = 0;
        let screen = render(&mut app, 160, 30);
        let fleets = screen
            .iter()
            .position(|r| r.contains("┌ fleets ─"))
            .unwrap();
        let contexts = screen
            .iter()
            .position(|r| r.contains("┌ contexts ─"))
            .unwrap();
        let row = |text: &str| screen.iter().position(|r| r.contains(text)).unwrap();
        assert!(fleets < row("◇ stg ×2"), "{}", screen.join("\n"));
        assert!(row("◇ stg ×2") < contexts, "{}", screen.join("\n"));
        assert!(contexts < row("stg-us") && contexts < row("prod-eu"));

        // One cursor runs over both boxes: down from the last fleet lands
        // on the first context, with no Tab in between.
        app.on_event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Down),
        ));
        assert_eq!(app.context_selected, 1);
        assert_eq!(
            app.contexts[app.context_selected - app.fleets.len()].name,
            "stg-eu"
        );
    }

    #[tokio::test]
    async fn describe_opens_in_the_right_pane_not_over_the_screen() {
        let mut app = test_app(Config::default());
        app.describe_open = true;
        app.on_describe(Ok(serde_json::json!({
            "kind": "Pod",
            "metadata": { "name": "checkout-1" },
        })));
        let screen = render(&mut app, 160, 30);
        let body = screen[6..28].join("\n");
        // The contexts pane is still drawn beside it, every row of it.
        assert!(body.contains("┌ contexts ─"), "{body}");
        assert!(
            body.contains("stg-us") && body.contains("prod-eu"),
            "{body}"
        );
        // Describe starts where the list starts, right of the contexts.
        let row = screen.iter().find(|r| r.contains(" describe · ")).unwrap();
        let col = row[..row.find(" describe · ").unwrap()].chars().count();
        assert_eq!(col, 29, "{row}");
        assert!(body.contains("kind: Pod"), "{body}");
    }
}
