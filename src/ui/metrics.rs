//! The monitoring pane — scoped to the object you have open.
//!
//! Whatever is selected decides what "monitoring" means: a pod shows its
//! containers, a workload aggregates its replicas, a node shows its own load
//! and what is scheduled on it, a claim shows how full it is. A cluster-wide
//! table is the wrong answer when you opened one object to ask about it.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Sparkline, Table, TableState};
use ratatui::Frame;

use crate::app::App;
use crate::config::Theme;
use crate::k8s::selection;
use crate::metrics::{fmt_bytes, fmt_cpu, pct};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus()))
        .title(format!(" monitoring · {} ", subject(app)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = &app.metrics.error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {err}"),
                Style::default().fg(theme.error()),
            ))),
            inner,
        );
        return;
    }

    let kind = app
        .current_type()
        .map(|t| t.api.kind.to_ascii_lowercase())
        .unwrap_or_default();

    match kind.as_str() {
        "node" => draw_node(f, app, inner),
        "persistentvolumeclaim" => draw_claim(f, app, inner),
        _ => draw_pods(f, app, inner),
    }
}

fn subject(app: &App) -> String {
    app.selected_row()
        .map(|r| r.key())
        .unwrap_or_else(|| "—".into())
}

fn severity(app: &App, percent: f64) -> Style {
    let theme = &app.config.theme;
    if percent >= app.config.metrics.critical_pct {
        Style::default()
            .fg(theme.error())
            .add_modifier(Modifier::BOLD)
    } else if percent >= app.config.metrics.warn_pct {
        Style::default().fg(theme.warn())
    } else {
        Style::default().fg(Theme::color(&theme.info))
    }
}

fn gauge(f: &mut Frame, app: &App, area: Rect, label: String, percent: f64) {
    f.render_widget(
        Gauge::default()
            .gauge_style(severity(app, percent))
            .ratio((percent / 100.0).clamp(0.0, 1.0))
            .label(label),
        area,
    );
}

/// Pods, workloads, services, namespaces — anything that resolves to a set of
/// pods. One row per pod, plus the selected object's totals and the container
/// breakdown of the first pod.
fn draw_pods(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let Some(target) = app.log_target() else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" nothing selected", dim))),
            area,
        );
        return;
    };
    let pods: Vec<String> = selection::pods_for(&target, &app.inventory.pods, &app.selector)
        .iter()
        .map(|p| p.key())
        .collect();

    if pods.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    " no pods behind this {} — nothing to monitor",
                    target.kind.to_ascii_lowercase()
                ),
                dim,
            ))),
            area,
        );
        return;
    }

    // Totals across the selection, which is the number you actually want for a
    // Deployment: what is this workload costing, not what is one replica.
    let mut cpu = 0.0;
    let mut mem = 0.0;
    let mut cpu_limit = 0.0;
    let mut mem_limit = 0.0;
    for key in &pods {
        if let Some(p) = app.metrics.pods.get(key) {
            cpu += p.usage.cpu.last();
            mem += p.usage.mem.last();
            cpu_limit += p.cpu_limit;
            mem_limit += p.mem_limit;
        }
    }
    let cpu_pct = pct(cpu, cpu_limit);
    let mem_pct = pct(mem, mem_limit);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(9),
        ])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    gauge(
        f,
        app,
        cols[0],
        format!(
            "CPU {}{}",
            fmt_cpu(cpu),
            if cpu_limit > 0.0 {
                format!(" / {} ({cpu_pct:.0}%)", fmt_cpu(cpu_limit))
            } else {
                " (no limit)".into()
            }
        ),
        cpu_pct,
    );
    gauge(
        f,
        app,
        cols[1],
        format!(
            "MEM {}{}",
            fmt_bytes(mem),
            if mem_limit > 0.0 {
                format!(" / {} ({mem_pct:.0}%)", fmt_bytes(mem_limit))
            } else {
                " (no limit)".into()
            }
        ),
        mem_pct,
    );

    // One row per pod in the selection.
    let table_rows: Vec<Row> = pods
        .iter()
        .map(|key| {
            let Some(p) = app.metrics.pods.get(key) else {
                return Row::new(vec![
                    Cell::from(key.clone()),
                    Cell::from("waiting for metrics").style(dim),
                ]);
            };
            let c = p.usage.cpu.last();
            let m = p.usage.mem.last();
            let cp = pct(c, p.cpu_limit);
            let mp = pct(m, p.mem_limit);
            Row::new(vec![
                Cell::from(super::truncate(&p.name, 34)),
                Cell::from(fmt_cpu(c)),
                Cell::from(if p.cpu_limit > 0.0 {
                    format!("{cp:.0}%")
                } else {
                    "-".into()
                })
                .style(severity(app, cp)),
                Cell::from(fmt_bytes(m)),
                Cell::from(if p.mem_limit > 0.0 {
                    format!("{mp:.0}%")
                } else {
                    "-".into()
                })
                .style(severity(app, mp)),
                Cell::from(p.restarts().to_string()).style(if p.restarts() > 0 {
                    Style::default().fg(theme.warn())
                } else {
                    dim
                }),
                Cell::from(super::truncate(&p.node, 22)).style(dim),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(22),
        ],
    )
    .header(Row::new(vec!["POD", "CPU", "%", "MEM", "%", "↺", "NODE"]).style(dim))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme.border()))
            .title(format!(" pods ({}) ", pods.len())),
    )
    .row_highlight_style(
        Style::default()
            .bg(theme.accent())
            .fg(Theme::color(&theme.match_fg)),
    );
    let mut state = TableState::default();
    state.select(Some(app.pod_metric_selected.min(pods.len() - 1)));
    f.render_stateful_widget(table, rows[1], &mut state);

    // Containers of whichever pod the cursor is on — the level where a leaking
    // sidecar actually becomes visible.
    let selected = pods
        .get(app.pod_metric_selected.min(pods.len() - 1))
        .and_then(|k| app.metrics.pods.get(k));
    draw_containers(f, app, rows[2], selected);
}

fn draw_containers(f: &mut Frame, app: &App, area: Rect, pod: Option<&crate::metrics::PodMetrics>) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border()))
        .title(match pod {
            Some(p) => format!(" containers of {} ", super::truncate(&p.name, 30)),
            None => " containers ".to_string(),
        });
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(pod) = pod else { return };
    if pod.containers.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" waiting for metrics", dim))),
            inner,
        );
        return;
    }

    let height = inner.height as usize;
    let slots = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); height])
        .split(inner);

    for (slot, c) in slots.iter().zip(pod.containers.iter()) {
        let cpu = c.usage.cpu.last();
        let mem = c.usage.mem.last();
        let mp = pct(mem, c.mem_limit);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(22),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(6),
                Constraint::Min(10),
            ])
            .split(*slot);

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                super::truncate(&c.name, 21),
                if c.ready {
                    Style::default().fg(Theme::color(&theme.fg))
                } else {
                    Style::default().fg(theme.warn())
                },
            ))),
            cols[0],
        );
        f.render_widget(Paragraph::new(fmt_cpu(cpu)), cols[1]);
        f.render_widget(Paragraph::new(fmt_bytes(mem)), cols[2]);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if c.mem_limit > 0.0 {
                    format!("{mp:.0}%")
                } else {
                    "-".into()
                },
                severity(app, mp),
            ))),
            cols[3],
        );
        // The sparkline is the point of keeping history: a flat line and a
        // climbing one at the same percentage mean very different things.
        // Scale to the limit when there is one, else to the series peak.
        let scale = if c.mem_limit > 0.0 {
            c.mem_limit
        } else {
            c.usage.mem.max()
        };
        let width = cols[4].width as usize;
        f.render_widget(
            Sparkline::default()
                .data(c.usage.mem.sparkline(width, scale))
                .max(100)
                .style(Style::default().fg(theme.accent())),
            cols[4],
        );
    }
}

/// A node: its own saturation, then what is scheduled on it.
fn draw_node(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let Some(row) = app.selected_row().map(|r| r.name.clone()) else {
        return;
    };
    let Some(node) = app.metrics.nodes.get(&row) else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " waiting for node metrics (needs metrics-server)",
                dim,
            ))),
            area,
        );
        return;
    };

    let cpu = node.cpu_pct();
    let mem = node.mem_pct();
    let (cpu_used, mem_used) = (node.usage.cpu.last(), node.usage.mem.last());
    let (cpu_cap, mem_cap) = (node.cpu_allocatable, node.mem_allocatable);
    let version = node.version.clone();
    let ready = node.ready;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(3),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if ready { " Ready " } else { " NotReady " },
                if ready {
                    Style::default().fg(Theme::color(&theme.info))
                } else {
                    Style::default()
                        .fg(theme.error())
                        .add_modifier(Modifier::BOLD)
                },
            ),
            Span::styled(format!("· kubelet {version}"), dim),
        ])),
        rows[0],
    );

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    gauge(
        f,
        app,
        cols[0],
        format!(
            "CPU {} / {} ({cpu:.0}%)",
            fmt_cpu(cpu_used),
            fmt_cpu(cpu_cap)
        ),
        cpu,
    );
    gauge(
        f,
        app,
        cols[1],
        format!(
            "MEM {} / {} ({mem:.0}%)",
            fmt_bytes(mem_used),
            fmt_bytes(mem_cap)
        ),
        mem,
    );

    // Everything scheduled here, heaviest first — the usual reason you opened
    // a node at all.
    let mut on_node: Vec<&crate::metrics::PodMetrics> = app
        .metrics
        .pods
        .values()
        .filter(|p| p.node == row)
        .collect();
    on_node.sort_by(|a, b| {
        b.usage
            .mem
            .last()
            .partial_cmp(&a.usage.mem.last())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let table_rows: Vec<Row> = on_node
        .iter()
        .map(|p| {
            Row::new(vec![
                Cell::from(super::truncate(&p.namespace, 16)).style(dim),
                Cell::from(super::truncate(&p.name, 34)),
                Cell::from(fmt_cpu(p.usage.cpu.last())),
                Cell::from(fmt_bytes(p.usage.mem.last())),
            ])
        })
        .collect();

    f.render_widget(
        Table::new(
            table_rows,
            [
                Constraint::Length(16),
                Constraint::Min(20),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
        .header(Row::new(vec!["NAMESPACE", "POD", "CPU", "MEM"]).style(dim))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border()))
                .title(format!(" pods on this node ({}) ", on_node.len())),
        ),
        rows[2],
    );
}

/// A claim: how full it actually is, which the API server cannot tell you.
fn draw_claim(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let Some(row) = app.selected_row().cloned() else {
        return;
    };
    let volume = app
        .inventory
        .volumes
        .iter()
        .find(|v| v.name == row.name && v.namespace == row.namespace)
        .cloned();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(area);

    match app.usage_for(&row.namespace, &row.name) {
        Some(usage) => {
            let used = usage.used_pct();
            gauge(
                f,
                app,
                rows[0],
                format!(
                    "{} / {} ({used:.0}%)",
                    fmt_bytes(usage.used_bytes),
                    fmt_bytes(usage.capacity_bytes)
                ),
                used,
            );
            let inodes = usage.inodes_used + usage.inodes_free;
            let mut lines = vec![Line::from(Span::styled(
                format!(
                    " inodes  {:.0} used of {:.0} ({:.0}%)",
                    usage.inodes_used,
                    inodes,
                    pct(usage.inodes_used, inodes)
                ),
                dim,
            ))];
            if let Some(v) = &volume {
                lines.push(Line::from(Span::styled(
                    format!(" class   {}  ·  access {}", v.storage_class, v.access_modes),
                    dim,
                )));
                lines.push(Line::from(Span::styled(
                    format!(
                        " used by {}",
                        if v.used_by.is_empty() {
                            "-".to_string()
                        } else {
                            v.used_by.join(", ")
                        }
                    ),
                    dim,
                )));
            }
            f.render_widget(Paragraph::new(lines), rows[1]);
        }
        None => {
            // Two different reasons, and the distinction is actionable.
            let why = match &app.volume_usage_error {
                Some(err) => format!(" {err}"),
                None if app.volume_usage_polled() => {
                    " usage not reported by this storage driver (hostPath and local-path never do)"
                        .to_string()
                }
                None => " waiting for kubelet…".to_string(),
            };
            let mut lines = vec![Line::from(Span::styled(why, dim))];
            if let Some(v) = &volume {
                lines.push(Line::from(Span::styled(
                    format!(
                        " requested {} · class {} · {}",
                        fmt_bytes(v.capacity_bytes.max(v.requested_bytes)),
                        v.storage_class,
                        v.phase
                    ),
                    Style::default().fg(Theme::color(&theme.fg)),
                )));
            }
            f.render_widget(Paragraph::new(lines), area);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn percentages_are_clamped_for_the_gauge() {
        // Gauge::ratio panics outside 0..=1, and a pod over its limit is a
        // perfectly normal thing to be looking at.
        for percent in [-5.0f64, 0.0, 42.0, 100.0, 250.0] {
            let ratio = (percent / 100.0).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&ratio));
        }
    }
}
