//! The left-hand pane: a `fleets` box above the `contexts` box.
//!
//! Both boxes share one cursor (`App::context_selected` runs over fleets
//! first, then contexts), so `j`/`k` and the arrows cross from one box to the
//! other without a key of their own.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::app::{App, Pane};
use crate::config::Theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let fleets = app.fleets.len();
    if fleets == 0 {
        draw_contexts(f, app, area);
        return;
    }
    // The fleets box is as tall as its list, up to half the pane; it
    // scrolls beyond that.
    let wanted = fleets as u16 + 2;
    let height = wanted.min((area.height / 2).max(3));
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(height), Constraint::Min(3)])
        .split(area);
    draw_fleets(f, app, parts[0]);
    draw_contexts(f, app, parts[1]);
}

fn draw_fleets(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let items: Vec<ListItem> = app
        .fleets
        .iter()
        .map(|fleet| {
            let active = app.fleet.as_ref().is_some_and(|f| f.name == fleet.name);
            ListItem::new(Line::from(vec![
                Span::styled(
                    if active { "◆ " } else { "◇ " },
                    Style::default().fg(theme.accent()),
                ),
                Span::styled(super::truncate(&fleet.name, 18), name_style(app, active)),
                Span::styled(format!(" ×{}", fleet.members.len()), dim),
            ]))
        })
        .collect();
    let cursor = app.context_selected;
    let selected = (cursor < app.fleets.len()).then_some(cursor);
    render_box(f, app, area, " fleets ", items, selected);
}

fn draw_contexts(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let in_fleet = |name: &str| {
        app.fleet
            .as_ref()
            .is_some_and(|f| f.members.iter().any(|m| m == name))
    };
    let items: Vec<ListItem> = app
        .contexts
        .iter()
        .map(|entry| {
            let active = entry.name == app.context_name;
            // The active context gets a filled marker so it stays obvious
            // which cluster you are actually looking at, even when the
            // cursor has wandered to another row.
            // Members of the fleet being browsed are marked too.
            let marker = if active {
                "● "
            } else if in_fleet(&entry.name) {
                "· "
            } else {
                "  "
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.accent())),
                Span::styled(super::truncate(&entry.name, 24), name_style(app, active)),
            ]))
        })
        .collect();
    let items = if items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(" in-cluster", dim)))]
    } else {
        items
    };
    let selected = app
        .context_selected
        .checked_sub(app.fleets.len())
        .filter(|_| !app.contexts.is_empty())
        .map(|i| i.min(app.contexts.len() - 1));
    render_box(f, app, area, " contexts ", items, selected);
}

fn name_style(app: &App, active: bool) -> Style {
    let theme = &app.config.theme;
    if active {
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::color(&theme.fg))
    }
}

/// One bordered list. The box holding the cursor gets the focus colour
/// while the pane is focused.
fn render_box(
    f: &mut Frame,
    app: &App,
    area: Rect,
    title: &str,
    items: Vec<ListItem>,
    selected: Option<usize>,
) {
    let theme = &app.config.theme;
    let focused = app.pane == Pane::Contexts && selected.is_some();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if focused {
                    theme.border_focus()
                } else {
                    theme.border()
                }))
                .title(title.to_string()),
        )
        .highlight_style(
            Style::default()
                .bg(theme.accent())
                .fg(Theme::color(&theme.match_fg))
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default();
    state.select(selected);
    f.render_stateful_widget(list, area, &mut state);
}
