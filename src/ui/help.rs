//! The `?` help overlay.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::config::Theme;

const BINDINGS: &[(&str, &[(&str, &str)])] = &[
    (
        "resources",
        &[
            (":", "pick a resource type (pods, deploy, svc, crds…)"),
            ("Tab", "accept the completion / cycle matches"),
            ("↑ / ↓", "move through the completions"),
            ("Enter / l", "open the object's logs"),
            ("d", "describe it (overlay; d or Esc closes)"),
            ("m / E", "monitoring / events for it"),
            ("/", "filter the list by name or namespace"),
            ("!", "show only the objects in trouble"),
            ("L", "set a label selector"),
            ("Ctrl-n", "change the namespace scope"),
            ("Ctrl-r", "re-list now"),
        ],
    ),
    (
        "general",
        &[
            ("q / Ctrl-c", "quit"),
            ("Esc", "leave the detail view, then quit"),
            ("?", "toggle this help"),
            ("Tab", "move focus between contexts and resources"),
            ("Enter", "on a context: switch cluster"),
        ],
    ),
    (
        "navigation",
        &[
            ("j / k / ↑ / ↓", "move one row or line"),
            ("Ctrl-d / Ctrl-u", "half page down / up"),
            ("Ctrl-f / PgDn", "page down"),
            ("Ctrl-b / PgUp", "page up"),
            ("g / Home", "jump to the top"),
            ("G / End", "jump to the bottom and follow"),
            ("[ / ]", "horizontal scroll (nowrap mode)"),
        ],
    ),
    (
        "detail: l logs",
        &[
            ("/", "search (regex, smart case)"),
            ("n / N", "next / previous match"),
            ("\\", "filter lines (prefix ! to exclude)"),
            ("L", "cycle the minimum level"),
            ("e", "errors only"),
            ("F / w", "follow / line wrapping"),
            ("t / p", "timestamps / previous container"),
            ("c / s", "clear buffer / save to file"),
            ("x", "detach all streams"),
        ],
    ),
    (
        "detail: m monitoring, E events, d describe",
        &[
            ("m", "monitoring, scoped to the open object"),
            ("S", "cycle sort: name / cpu / memory"),
            ("E", "events for the open object"),
            ("W", "events: warnings only"),
            ("d", "describe as YAML; j/k/g/G scroll it"),
        ],
    ),
];

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let popup = super::centered_rect(74, 88, area);
    f.render_widget(Clear, popup);

    let heading = Style::default()
        .fg(theme.accent())
        .add_modifier(Modifier::BOLD);
    let key = Style::default().fg(Theme::color(&theme.info));
    let dim = Style::default().fg(Theme::color(&theme.trace));

    let mut lines: Vec<Line> = Vec::new();
    for (section, entries) in BINDINGS {
        lines.push(Line::from(Span::styled(format!(" {section}"), heading)));
        for (k, description) in *entries {
            lines.push(Line::from(vec![
                Span::styled(format!("   {k:<18}"), key),
                Span::raw(*description),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        " kscope is read-only: it never mutates cluster state.",
        dim,
    )));
    lines.push(Line::from(Span::styled(" press any key to close", dim)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus()))
        .title(format!(" help · kscope {} ", env!("CARGO_PKG_VERSION")));

    f.render_widget(Paragraph::new(lines).block(block), popup);
}
