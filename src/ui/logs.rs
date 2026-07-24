//! The log pane.
//!
//! Only the visible window is styled: with a 50 000-line buffer we still build
//! at most `height` styled lines per frame, so rendering cost is independent of
//! buffer size.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

use crate::app::{App, Pane, View};
use crate::config::Theme;
use crate::logs::Level;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let focused = app.pane == Pane::Content && app.view == View::Logs;

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
            Line::from("  ↑/↓ or j/k   select a pod in the sidebar"),
            Line::from("  Enter        expand a pod / attach a container"),
            Line::from("  a            attach every container of the pod"),
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
            let short = short_source(&entry.source);
            rendered.spans.insert(
                0,
                Span::styled(
                    format!("{short:<14} "),
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
        let mut state = ScrollbarState::new(total.saturating_sub(inner.height as usize))
            .position(app.scroll);
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

fn title(app: &App) -> String {
    let follow = if app.follow { "follow" } else { "paused" };
    let wrap = if app.wrap { "wrap" } else { "nowrap" };
    match app.attached.len() {
        0 => match app.selected_pod() {
            Some(pod) => format!(" logs · {} · {} · not attached ", pod.name, pod.phase),
            None => " logs ".to_string(),
        },
        1 => format!(" logs · {} · {follow} · {wrap} ", app.attached[0]),
        n => format!(" logs · {n} streams · {follow} · {wrap} "),
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
        format!(
            "  buffer {}/{}",
            app.buffer.len(),
            app.buffer.capacity()
        ),
        dim,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// `ns/pod:container` → `pod:container`, shortened for the gutter.
fn short_source(source: &str) -> String {
    let tail = source.split('/').next_back().unwrap_or(source);
    super::truncate(tail, 14)
}

/// Deterministic colour per stream so interleaved logs stay readable.
fn source_color(source: &str, theme: &crate::config::Theme) -> ratatui::style::Color {
    const PALETTE: [&str; 6] = [
        "cyan",
        "magenta",
        "green",
        "yellow",
        "blue",
        "lightcyan",
    ];
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
        assert_eq!(short_source("prod/api-0:app"), "api-0:app");
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
