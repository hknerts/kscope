//! The left-hand contexts pane.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::app::{App, Pane};
use crate::config::Theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let focused = app.pane == Pane::Contexts;
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let items: Vec<ListItem> = app
        .contexts
        .iter()
        .map(|entry| {
            let active = entry.name == app.context_name;
            // The active context gets a filled marker so it stays obvious
            // which cluster you are actually looking at, even when the
            // cursor has wandered to another row.
            let marker = if active { "● " } else { "  " };
            let name_style = if active {
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::color(&theme.fg))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.accent())),
                Span::styled(super::truncate(&entry.name, 24), name_style),
            ]))
        })
        .collect();

    let items = if items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(" in-cluster", dim)))]
    } else {
        items
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if focused {
                    theme.border_focus()
                } else {
                    theme.border()
                }))
                .title(" contexts "),
        )
        .highlight_style(
            Style::default()
                .bg(theme.accent())
                .fg(Theme::color(&theme.match_fg))
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default();
    if !app.contexts.is_empty() {
        state.select(Some(app.context_selected.min(app.contexts.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}
