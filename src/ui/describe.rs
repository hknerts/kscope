//! The describe pane: the open object rendered as YAML.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::App;
use crate::config::Theme;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let subject = app
        .selected_row()
        .map(|r| r.key())
        .unwrap_or_else(|| "—".into());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus()))
        .title(format!(" describe · {subject}   (d or Esc to close) "));

    // An overlay rather than a tab: describe is a different question about the
    // object you already have open, not a different view of it.
    let area = super::centered_rect(84, 88, area);
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.viewport_height = inner.height as usize;

    if let Some(err) = &app.describe_error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {err}"),
                Style::default().fg(theme.error()),
            ))),
            inner,
        );
        return;
    }
    if app.describe_loading || app.describe_text.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" loading…", dim))),
            inner,
        );
        return;
    }

    // Only the visible window is styled, as everywhere else in kscope: a
    // ConfigMap holding a megabyte of certificates should not cost more to
    // scroll than a five-line Service.
    let height = inner.height as usize;
    let lines: Vec<Line> = app
        .describe_text
        .lines()
        .skip(app.describe_scroll)
        .take(height)
        .map(|line| style_line(line, theme))
        .collect();

    f.render_widget(Paragraph::new(lines), inner);

    let total = app.describe_len();
    if total > height {
        let mut state =
            ScrollbarState::new(total.saturating_sub(height)).position(app.describe_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut state,
        );
    }
}

/// Colour keys apart from values, so nesting is readable at a glance.
fn style_line<'a>(line: &'a str, theme: &Theme) -> Line<'a> {
    let key_style = Style::default().fg(theme.accent());
    let value_style = Style::default().fg(Theme::color(&theme.fg));
    let punct = Style::default().fg(Theme::color(&theme.trace));

    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);

    // "- " list markers keep their dash dim, then fall through to key: value.
    let (marker, rest) = match rest.strip_prefix("- ") {
        Some(after) => ("- ", after),
        None => ("", rest),
    };

    match rest.split_once(':') {
        Some((key, value)) if !key.contains(' ') => Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(marker, punct),
            Span::styled(key.to_string(), key_style.add_modifier(Modifier::BOLD)),
            Span::styled(":", punct),
            Span::styled(value.to_string(), value_style),
        ]),
        _ => Line::from(vec![
            Span::raw(indent.to_string()),
            Span::styled(marker, punct),
            Span::styled(rest.to_string(), value_style),
        ]),
    }
}
