//! The log pane.
//!
//! Only the visible window is styled: with a 50 000-line buffer we still build
//! at most `height` styled lines per frame, so rendering cost is independent of
//! buffer size.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::{App, RightMode, View};
use crate::config::Theme;
use crate::logs::Level;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let focused = app.right == RightMode::Detail && app.view == View::Logs;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    draw_summary(f, app, chunks[0]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme.border_focus()
        } else {
            theme.border()
        }))
        .title(title(app));

    let inner = block.inner(chunks[1]);
    f.render_widget(block, chunks[1]);

    // The viewport height drives PageUp/PageDown and follow-mode anchoring.
    app.viewport_height = inner.height as usize;
    if app.follow {
        app.scroll = app.max_scroll();
    }

    if app.attached.is_empty() {
        let hint = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no stream attached",
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("  Enter        on a pod streams it; on a deployment, statefulset,"),
            Line::from("               daemonset, job or service merges all of its pods"),
            Line::from("  Esc          back to the list"),
            Line::from("  ?            all key bindings"),
        ];
        f.render_widget(Paragraph::new(hint), inner);
        return;
    }

    if app.buffer.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  waiting for the first lines…",
                Style::default().fg(Theme::color(&theme.trace)),
            ))),
            inner,
        );
        return;
    }

    let multi = app.attached.len() > 1;
    // Across a fleet the gutter also names the cluster.
    let gutter = if app.log_members.is_empty() { 14 } else { 28 };
    let start = app.scroll.min(app.buffer.view_len());
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    let search = app.search.regex.as_ref();

    for (offset, entry) in app
        .buffer
        .view_range(start, inner.height as usize)
        .enumerate()
    {
        let mut rendered = app.highlighter.render(entry, search, app.timestamps);
        if app.timestamps {
            if let Some(ts) = entry.timestamp() {
                rendered.spans.insert(
                    0,
                    Span::styled(
                        format!("{} ", clock(ts)),
                        Style::default().fg(Theme::color(&theme.trace)),
                    ),
                );
            }
        }
        if multi {
            let short = short_source(&entry.source, gutter);
            rendered.spans.insert(
                0,
                Span::styled(
                    format!("{short:<gutter$} "),
                    Style::default().fg(source_color(&entry.source, theme)),
                ),
            );
        }
        if Some(start + offset) == app.search.current {
            rendered.spans.insert(
                0,
                Span::styled(
                    "▶",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                ),
            );
        }
        lines.push(rendered);
    }

    let mut paragraph = Paragraph::new(lines);
    if app.wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    } else {
        paragraph = paragraph.scroll((0, app.h_scroll as u16));
    }
    f.render_widget(paragraph, inner);

    // Scrollbar on the right edge of the block.
    let total = app.buffer.view_len();
    if total > inner.height as usize {
        let mut state =
            ScrollbarState::new(total.saturating_sub(inner.height as usize)).position(app.scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(Style::default().fg(theme.border())),
            chunks[1],
            &mut state,
        );
    }
}

/// `2024-05-01T10:11:12.123Z` → `10:11:12.123`, which is all that fits.
fn clock(ts: &str) -> &str {
    match ts.find('T') {
        Some(i) => {
            let rest = &ts[i + 1..];
            let end = rest.len().min(12);
            &rest[..end]
        }
        None => ts,
    }
}

/// Says what is merged: a workload's logs are every replica's, and within a
/// fleet every cluster's, which is easy to miss without being told.
fn title(app: &App) -> String {
    let follow = if app.follow { "follow" } else { "paused" };
    let wrap = if app.wrap { "wrap" } else { "nowrap" };
    let (pods, clusters) = app.log_spread();
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match app.attached.len() {
        0 => match app.selected_pod() {
            Some(pod) => format!(" logs · {} · {} · not attached ", pod.name, pod.phase),
            None => " logs ".to_string(),
        },
        _ if !app.log_members.is_empty() => format!(
            " logs · {} merged from {} of {} · {follow} · {wrap} ",
            plural(pods, "pod"),
            clusters,
            plural(app.log_members.len(), "cluster"),
        ),
        1 => format!(" logs · {} · {follow} · {wrap} ", app.attached[0]),
        n if pods > 1 => format!(
            " logs · {} merged · {} · {follow} · {wrap} ",
            plural(pods, "pod"),
            plural(n, "container")
        ),
        n => format!(" logs · {} · {follow} · {wrap} ", plural(n, "container")),
    }
}

/// One-line severity breakdown above the log pane.
fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let counter = |level: Level, style: Style| {
        vec![
            Span::styled(format!(" {} ", level.label()), style),
            Span::styled(format!("{} ", app.buffer.count_of(level)), dim),
        ]
    };

    let mut spans = vec![Span::styled(" levels:", dim)];
    spans.extend(counter(
        Level::Fatal,
        Style::default()
            .fg(theme.error())
            .add_modifier(Modifier::BOLD),
    ));
    spans.extend(counter(Level::Error, Style::default().fg(theme.error())));
    spans.extend(counter(Level::Warn, Style::default().fg(theme.warn())));
    spans.extend(counter(
        Level::Info,
        Style::default().fg(Theme::color(&theme.info)),
    ));
    spans.extend(counter(
        Level::Debug,
        Style::default().fg(Theme::color(&theme.debug)),
    ));
    spans.push(Span::styled(
        match app.buffer.capacity() {
            // Evicted lines are said here, not only in the status bar: they
            // are why `g` no longer reaches the container's first line.
            Some(cap) if app.buffer.dropped > 0 => format!(
                "  buffer {}/{} · {} evicted, oldest first",
                app.buffer.len(),
                cap,
                app.buffer.dropped
            ),
            Some(cap) => format!("  buffer {}/{}", app.buffer.len(), cap),
            None => format!(
                "  buffer {} lines · {:.1} MiB · unlimited",
                app.buffer.len(),
                app.buffer.memory_bytes() as f64 / (1024.0 * 1024.0)
            ),
        },
        dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `ns/pod:container` → `pod:container`, shortened for the gutter; within a
/// fleet `context|ns/pod:container` → `context pod:container`.
fn short_source(source: &str, width: usize) -> String {
    let (context, local) = source.split_once('|').unwrap_or(("", source));
    let tail = local.split('/').next_back().unwrap_or(local);
    if context.is_empty() {
        return super::truncate(tail, width);
    }
    let context = super::truncate(context, width / 2);
    let rest = width.saturating_sub(context.chars().count() + 1);
    format!("{context} {}", super::truncate(tail, rest))
}

/// Deterministic colour per stream so interleaved logs stay readable.
fn source_color(source: &str, theme: &crate::config::Theme) -> ratatui::style::Color {
    const PALETTE: [&str; 6] = ["cyan", "magenta", "green", "yellow", "blue", "lightcyan"];
    let hash = source
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let _ = theme;
    Theme::color(PALETTE[(hash as usize) % PALETTE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_clock_from_timestamp() {
        assert_eq!(clock("2024-05-01T10:11:12.123456789Z"), "10:11:12.123");
        assert_eq!(clock("no-timestamp"), "no-timestamp");
    }

    #[test]
    fn shortens_source_labels() {
        assert_eq!(short_source("prod/api-0:app", 14), "api-0:app");
        assert_eq!(
            short_source("stg-eu|shop/api-0:app", 28),
            "stg-eu api-0:app"
        );
    }

    #[test]
    fn source_colour_is_stable() {
        let theme = crate::config::Theme::default();
        assert_eq!(
            source_color("prod/api-0:app", &theme),
            source_color("prod/api-0:app", &theme)
        );
    }
}
