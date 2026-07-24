//! The metrics pane.
//!
//! Three levels of granularity are always available, because in practice you
//! need all of them: the **node** (is the machine saturated?), the **pod** (is
//! my workload the cause?) and the **container** (which sidecar is leaking?).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Sparkline, Table, TableState};
use ratatui::Frame;

use crate::app::{App, MetricPane, RightMode, View};
use crate::config::Theme;
use crate::metrics::{fmt_bytes, fmt_cpu, pct};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Percentage(28),
            Constraint::Percentage(28),
            Constraint::Min(7),
            Constraint::Length(10),
        ])
        .split(area);

    draw_cluster(f, app, chunks[0]);
    draw_nodes(f, app, chunks[1]);
    draw_pods(f, app, chunks[2]);
    draw_volumes(f, app, chunks[3]);
    draw_containers(f, app, chunks[4]);
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

fn draw_cluster(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border()))
        .title(" cluster ");
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

    let (cpu_used, cpu_cap, mem_used, mem_cap) = app.metrics.cluster_totals();
    let cpu_pct = pct(cpu_used, cpu_cap);
    let mem_pct = pct(mem_used, mem_cap);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    f.render_widget(
        Gauge::default()
            .gauge_style(severity(app, cpu_pct))
            .ratio((cpu_pct / 100.0).clamp(0.0, 1.0))
            .label(format!(
                "CPU {} / {} ({cpu_pct:.0}%)",
                fmt_cpu(cpu_used),
                fmt_cpu(cpu_cap)
            )),
        cols[0],
    );
    f.render_widget(
        Gauge::default()
            .gauge_style(severity(app, mem_pct))
            .ratio((mem_pct / 100.0).clamp(0.0, 1.0))
            .label(format!(
                "MEM {} / {} ({mem_pct:.0}%)",
                fmt_bytes(mem_used),
                fmt_bytes(mem_cap)
            )),
        cols[1],
    );
}

fn pane_border(app: &App, pane: MetricPane) -> Style {
    let theme = &app.config.theme;
    let focused =
        app.right == RightMode::Detail && app.view == View::Metrics && app.metric_pane == pane;
    Style::default().fg(if focused {
        theme.border_focus()
    } else {
        theme.border()
    })
}

fn draw_nodes(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let nodes = app.sorted_node_metrics();
    let rows: Vec<Row> = nodes
        .iter()
        .map(|n| {
            let cpu = n.cpu_pct();
            let mem = n.mem_pct();
            Row::new(vec![
                Cell::from(super::truncate(&n.name, 28)).style(if n.ready {
                    Style::default().fg(Theme::color(&theme.fg))
                } else {
                    Style::default().fg(theme.error())
                }),
                Cell::from(fmt_cpu(n.usage.cpu.last())),
                Cell::from(format!("{cpu:.0}%")).style(severity(app, cpu)),
                Cell::from(bar(cpu)).style(severity(app, cpu)),
                Cell::from(fmt_bytes(n.usage.mem.last())),
                Cell::from(format!("{mem:.0}%")).style(severity(app, mem)),
                Cell::from(bar(mem)).style(severity(app, mem)),
                Cell::from(n.pods.to_string()).style(dim),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(28),
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(5),
        ],
    )
    .header(Row::new(vec!["NODE", "CPU", "%", "", "MEM", "%", "", "PODS"]).style(dim))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(pane_border(app, MetricPane::Nodes))
            .title(format!(" nodes ({}) ", nodes.len())),
    )
    .row_highlight_style(
        Style::default()
            .bg(theme.accent())
            .fg(Theme::color(&theme.match_fg)),
    );

    let mut state = TableState::default();
    if !nodes.is_empty() {
        state.select(Some(app.node_selected.min(nodes.len() - 1)));
    }
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_pods(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let pods = app.sorted_pod_metrics();
    let rows: Vec<Row> = pods
        .iter()
        .map(|p| {
            let cpu = p.usage.cpu.last();
            let mem = p.usage.mem.last();
            let cpu_pct = pct(cpu, p.cpu_limit);
            let mem_pct = pct(mem, p.mem_limit);
            Row::new(vec![
                Cell::from(super::truncate(&p.namespace, 12)).style(dim),
                Cell::from(super::truncate(&p.name, 30)),
                Cell::from(fmt_cpu(cpu)),
                Cell::from(if p.cpu_limit > 0.0 {
                    format!("{cpu_pct:.0}%")
                } else {
                    "-".into()
                })
                .style(severity(app, cpu_pct)),
                Cell::from(fmt_bytes(mem)),
                Cell::from(if p.mem_limit > 0.0 {
                    format!("{mem_pct:.0}%")
                } else {
                    "-".into()
                })
                .style(severity(app, mem_pct)),
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
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(22),
        ],
    )
    .header(
        Row::new(vec![
            "NAMESPACE",
            "POD",
            "CPU",
            "LIM%",
            "MEM",
            "LIM%",
            "RS",
            "NODE",
        ])
        .style(dim),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(pane_border(app, MetricPane::Pods))
            .title(format!(
                " pods ({}) · sort:{} ",
                pods.len(),
                app.sort_by.label()
            )),
    )
    .row_highlight_style(
        Style::default()
            .bg(theme.accent())
            .fg(Theme::color(&theme.match_fg)),
    );

    let mut state = TableState::default();
    if !pods.is_empty() {
        state.select(Some(app.pod_metric_selected.min(pods.len() - 1)));
    }
    f.render_stateful_widget(table, area, &mut state);
}

/// PersistentVolumeClaims: capacity and status only. kscope reads these
/// straight from the Kubernetes API, not kubelet's stats/summary, so there is
/// no live "bytes actually used" figure — see `VolumeInfo`.
fn draw_volumes(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let volumes = &app.inventory.volumes;
    let rows: Vec<Row> = volumes
        .iter()
        .map(|v| {
            let phase_style = match v.phase.as_str() {
                "Bound" => Style::default().fg(Theme::color(&theme.info)),
                "Pending" => Style::default().fg(theme.warn()),
                _ => Style::default().fg(theme.error()),
            };
            let used_by = if v.used_by.is_empty() {
                "-".to_string()
            } else {
                v.used_by.join(",")
            };
            Row::new(vec![
                Cell::from(super::truncate(&v.namespace, 12)).style(dim),
                Cell::from(super::truncate(&v.name, 24)),
                Cell::from(v.phase.clone()).style(phase_style),
                Cell::from(fmt_bytes(v.capacity_bytes.max(v.requested_bytes))),
                Cell::from(super::truncate(&v.storage_class, 14)).style(dim),
                Cell::from(super::truncate(&v.access_modes, 10)).style(dim),
                Cell::from(super::truncate(&used_by, 26)).style(dim),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(24),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec![
            "NAMESPACE",
            "PVC",
            "STATUS",
            "SIZE",
            "CLASS",
            "ACCESS",
            "USED BY",
        ])
        .style(dim),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(pane_border(app, MetricPane::Volumes))
            .title(format!(" volumes ({}) ", volumes.len())),
    )
    .row_highlight_style(
        Style::default()
            .bg(theme.accent())
            .fg(Theme::color(&theme.match_fg)),
    );

    let mut state = TableState::default();
    if !volumes.is_empty() {
        state.select(Some(app.volume_selected.min(volumes.len() - 1)));
    }
    f.render_stateful_widget(table, area, &mut state);
}

/// Per-container detail for the pod selected in the metrics table, including a
/// live sparkline of the last N samples.
fn draw_containers(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let pods = app.sorted_pod_metrics();
    let selected = pods.get(app.pod_metric_selected.min(pods.len().saturating_sub(1)));

    let title = match selected {
        Some(p) => format!(
            " containers · {}/{} · ready {} ",
            p.namespace,
            p.name,
            p.ready_string()
        ),
        None => " containers ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border()))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(pod) = selected else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " waiting for metrics-server samples…",
                dim,
            ))),
            inner,
        );
        return;
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    // Left: one row per container.
    let mut lines: Vec<Line> = Vec::with_capacity(pod.containers.len() + 1);
    lines.push(Line::from(Span::styled(
        format!(
            " {:<18} {:>9} {:>6} {:>10} {:>6}  {:<9}",
            "CONTAINER", "CPU", "LIM%", "MEM", "LIM%", "STATE"
        ),
        dim,
    )));
    for c in &pod.containers {
        let cpu_pct = c.cpu_pct();
        let mem_pct = c.mem_pct();
        lines.push(Line::from(vec![
            Span::raw(format!(" {:<18} ", super::truncate(&c.name, 18))),
            Span::raw(format!("{:>9} ", fmt_cpu(c.usage.cpu.last()))),
            Span::styled(
                format!(
                    "{:>6} ",
                    if c.cpu_limit > 0.0 {
                        format!("{cpu_pct:.0}%")
                    } else {
                        "-".into()
                    }
                ),
                severity(app, cpu_pct),
            ),
            Span::raw(format!("{:>10} ", fmt_bytes(c.usage.mem.last()))),
            Span::styled(
                format!(
                    "{:>6} ",
                    if c.mem_limit > 0.0 {
                        format!("{mem_pct:.0}%")
                    } else {
                        "-".into()
                    }
                ),
                severity(app, mem_pct),
            ),
            Span::styled(
                format!(" {:<9}", super::truncate(&c.state, 9)),
                if c.ready {
                    Style::default().fg(Theme::color(&theme.info))
                } else {
                    Style::default().fg(theme.warn())
                },
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), cols[0]);

    // Right: pod-level sparklines.
    let spark = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(1),
            Constraint::Min(2),
        ])
        .split(cols[1]);

    if pod.usage.cpu.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" no samples yet", dim))),
            cols[1],
        );
        return;
    }

    let width = spark[1].width as usize;
    let cpu_scale = pod.usage.cpu.max().max(1.0);
    let mem_scale = pod.usage.mem.max().max(1.0);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                " cpu  now {}  avg {}  peak {}",
                fmt_cpu(pod.usage.cpu.last()),
                fmt_cpu(pod.usage.cpu.avg()),
                fmt_cpu(cpu_scale)
            ),
            dim,
        ))),
        spark[0],
    );
    f.render_widget(
        Sparkline::default()
            .data(pod.usage.cpu.sparkline(width, cpu_scale))
            .max(100)
            .style(Style::default().fg(theme.accent())),
        spark[1],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                " mem  now {}  avg {}  peak {}",
                fmt_bytes(pod.usage.mem.last()),
                fmt_bytes(pod.usage.mem.avg()),
                fmt_bytes(mem_scale)
            ),
            dim,
        ))),
        spark[2],
    );
    f.render_widget(
        Sparkline::default()
            .data(pod.usage.mem.sparkline(width, mem_scale))
            .max(100)
            .style(Style::default().fg(Theme::color(&theme.debug))),
        spark[3],
    );
}

/// Tiny inline bar for table cells.
fn bar(percent: f64) -> String {
    const WIDTH: usize = 10;
    let filled = ((percent / 100.0) * WIDTH as f64)
        .round()
        .clamp(0.0, WIDTH as f64) as usize;
    let mut s = String::with_capacity(WIDTH);
    for i in 0..WIDTH {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_scales() {
        assert_eq!(bar(0.0), "░░░░░░░░░░");
        assert_eq!(bar(100.0), "██████████");
        assert_eq!(bar(50.0).chars().filter(|c| *c == '█').count(), 5);
    }
}
