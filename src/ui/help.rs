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
        "the list",
        &[
            (
                "Enter  l",
                "logs; a workload merges all its pods, a fleet all clusters",
            ),
            ("m  e  d", "its metrics, events, or YAML (describe)"),
            (
                "a",
                "in a fleet: the object on every cluster, drift flagged",
            ),
            ("/", "filter by name, namespace or cluster"),
            (
                "S",
                "sort: name, newest, problems first, most restarts (pods)",
            ),
            (
                "!",
                "only problems: crash loops, pending, not ready, failed",
            ),
            (":", "pick a resource type (pods, deploy, svc, CRDs…)"),
            ("L", "label selector, e.g. app=api,tier!=batch"),
            ("Ctrl-n", "namespace (empty or * for all)"),
            ("Ctrl-r", "refresh now"),
        ],
    ),
    (
        "clusters (Tab to the left pane)",
        &[
            ("Enter", "switch to that cluster, or open that fleet"),
            ("/", "open a fleet by name or pattern: stg, prod-*, a,b"),
        ],
    ),
    (
        "logs",
        &[
            ("/  n  N", "search (regex, smart case), next, previous"),
            ("\\", "keep matching lines; prefix ! to drop them"),
            ("L", "minimum level: all, DEBUG, INFO, WARN, ERROR, FATAL"),
            ("F  w", "follow the tail, wrap long lines"),
            ("t  p", "timestamps, previous (crashed) container"),
            ("s  c", "save what is shown to a file, clear"),
            ("v", "on a node: kubelet, containerd or kernel log"),
            ("v", "in describe of a Secret: show values decoded, or hide"),
        ],
    ),
    (
        "metrics and events",
        &[("S", "sort by name, cpu or memory"), ("W", "warnings only")],
    ),
    (
        "everywhere",
        &[
            ("j k  ↑ ↓", "move;  Ctrl-d/u half page,  g/G top/bottom"),
            ("Tab", "between clusters and the list"),
            (
                ":fleet stg",
                "switch fleet (words and globs as -u); :fleet alone leaves",
            ),
            ("M", "mouse: off to select and copy text, on to scroll"),
            ("Esc", "back one step: from any tab to the list, then quit"),
            ("?", "this help"),
            ("q  Ctrl-c", "quit"),
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
                Span::styled(format!("   {k:<12}"), key),
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
