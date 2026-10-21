//! The compare tab: the open object as every fleet member has it.
//!
//! One row per member. A value that differs from what most members have is
//! drawn in the warning colour, so drift between clusters is the first thing
//! you see.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::compare::{agreement, drift, State};
use crate::config::Theme;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.config.theme;
    let dim = Style::default().fg(Theme::color(&theme.trace));
    let plain = Style::default().fg(Theme::color(&theme.fg));
    let odd = Style::default()
        .fg(theme.warn())
        .add_modifier(Modifier::BOLD);
    let fleet = app.fleet.as_ref().map(|f| f.name.as_str()).unwrap_or("—");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus()))
        .title(format!(" all clusters · fleet {fleet} "));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.compare.is_empty() {
        let text = if app.compare_loading {
            " asking every cluster…"
        } else {
            " nothing to compare"
        };
        f.render_widget(Paragraph::new(Span::styled(text, dim)), inner);
        return;
    }

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    let drift = drift(&app.compare);
    let summary = if drift.is_empty() {
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(Theme::color(&theme.info))),
            Span::styled(agreement(&app.compare), plain),
        ])
    } else {
        let mut spans = vec![Span::styled(" ≠ ", odd)];
        for (what, members) in [
            ("images differ", &drift.images),
            ("replicas differ", &drift.replicas),
            ("spec differs", &drift.spec),
        ] {
            if !members.is_empty() {
                spans.push(Span::styled(format!("{what} on "), plain));
                spans.push(Span::styled(members.join(", "), odd));
                if what == "spec differs" && !drift.spec_fields.is_empty() {
                    spans.push(Span::styled(
                        format!(" ({})", drift.spec_fields.join(", ")),
                        plain,
                    ));
                }
                spans.push(Span::styled("   ", plain));
            }
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(summary), parts[0]);

    // Status and age come from the fleet listing, which already has them.
    let listed = |context: &str| {
        let row = app.selected_row()?;
        app.rows
            .iter()
            .find(|r| r.context == context && r.namespace == row.namespace && r.name == row.name)
    };
    let mark = |differs: bool, text: String| {
        Cell::from(Span::styled(text, if differs { odd } else { plain }))
    };
    let rows: Vec<Row> = app
        .compare
        .iter()
        .map(|member| {
            let context = Cell::from(Span::styled(member.context.clone(), plain));
            match &member.state {
                State::Found(facts) => {
                    let status = listed(&member.context)
                        .map(|r| r.status.clone())
                        .unwrap_or_default();
                    let age = listed(&member.context)
                        .map(|r| crate::k8s::fmt_age(r.age_seconds))
                        .unwrap_or_default();
                    let is = |set: &Vec<String>| set.contains(&member.context);
                    Row::new(vec![
                        context,
                        Cell::from(Span::styled(status, plain)),
                        mark(
                            is(&drift.replicas),
                            facts.replicas.map(|r| r.to_string()).unwrap_or("—".into()),
                        ),
                        mark(
                            is(&drift.spec),
                            if is(&drift.spec) { "differs" } else { "same" }.into(),
                        ),
                        mark(is(&drift.images), facts.images.join("  ")),
                        Cell::from(Span::styled(age, dim)),
                    ])
                }
                // The explanation goes in the wide images column.
                State::Absent => Row::new(vec![
                    context,
                    Cell::from(Span::styled("—", dim)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled("not on this cluster", dim)),
                ]),
                State::Unreachable(err) => Row::new(vec![
                    context,
                    Cell::from(Span::styled(
                        "unreachable",
                        Style::default().fg(theme.error()),
                    )),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled(
                        err.clone(),
                        Style::default().fg(theme.error()),
                    )),
                ]),
            }
        })
        .collect();

    let header = Row::new(["CLUSTER", "STATUS", "REPLICAS", "SPEC", "IMAGES", "AGE"])
        .style(dim.add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(20),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .column_spacing(2);
    f.render_widget(table, parts[1]);
}
