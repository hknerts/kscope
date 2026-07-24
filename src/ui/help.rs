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
        "general",
        &[
            ("q / Esc / Ctrl-c", "quit"),
            ("?", "toggle this help"),
            ("1 / 2", "logs view / metrics view"),
            ("Tab", "move focus between sidebar and content"),
            ("Shift-Tab", "switch view"),
            ("Ctrl-n", "change namespace scope"),
            ("Ctrl-p", "filter the pod list"),
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
            ("h / l / ← / →", "horizontal scroll (nowrap mode)"),
        ],
    ),
    (
        "streams",
        &[
            ("Enter", "expand a pod / attach a container"),
            ("a", "attach every container of the pod"),
            ("x", "detach all streams"),
            ("t", "toggle API-server timestamps"),
            ("p", "toggle previous (crashed) container logs"),
            ("c", "clear the buffer"),
            ("s", "save the visible buffer to a file"),
        ],
    ),
    (
        "search and filter",
        &[
            ("/", "search (regex, smart case)"),
            ("n / N", "next / previous match"),
            ("\\", "filter lines (prefix ! to exclude)"),
            ("L", "cycle the minimum level"),
            ("e", "errors only"),
            ("F", "toggle follow"),
            ("w", "toggle line wrapping"),
        ],
    ),
    (
        "metrics",
        &[
            ("m", "cycle the node, pod and volume tables"),
            ("S", "cycle sort: name / cpu / memory"),
        ],
    ),
];

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.config.theme;
    let popup = super::centered_rect(70, 80, area);
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
    lines.push(Line::from(Span::styled(
        " press any key to close",
        dim,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus()))
        .title(format!(" help · kscope {} ", env!("CARGO_PKG_VERSION")));

    f.render_widget(Paragraph::new(lines).block(block), popup);
}
