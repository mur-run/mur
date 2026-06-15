//! Ratatui rendering: transcript pane, input box, status bar, HITL modal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::app::{App, ChatMsg, Role, SPINNER};
use super::markdown;

const USER: Color = Color::Green;
const AGENT: Color = Color::Cyan;
const SYSTEM: Color = Color::DarkGray;

/// Draw the whole UI for one frame.
pub fn render(f: &mut Frame, app: &App) {
    let input_lines = app.input.lines().len() as u16;
    let input_height = (input_lines + 2).clamp(3, 8);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_transcript(f, app, chunks[0]);
    f.render_widget(&app.input, chunks[1]);
    render_status(f, app, chunks[2]);

    if let Some(hitl) = &app.hitl {
        render_hitl(f, hitl);
    }
}

fn render_transcript(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" chat · {} ", app.agent));
    let inner = block.inner(area);

    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages {
        push_message(&mut lines, m, app.spinner);
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "Say hello — type below and press Enter.",
            Style::default().fg(SYSTEM),
        ));
    }

    let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let total = para.line_count(inner.width) as u16;
    let max_off = total.saturating_sub(inner.height);
    let offset = max_off.saturating_sub(app.scroll_back);

    f.render_widget(block, area);
    f.render_widget(para.scroll((offset, 0)), inner);
}

fn push_message(lines: &mut Vec<Line<'static>>, m: &ChatMsg, spinner: usize) {
    match m.role {
        Role::User => {
            lines.push(Line::from(Span::styled(
                "you ›",
                Style::default().fg(USER).add_modifier(Modifier::BOLD),
            )));
            for l in m.text.lines() {
                lines.push(Line::raw(l.to_string()));
            }
            lines.push(Line::default());
        }
        Role::System => {
            for (i, l) in m.text.lines().enumerate() {
                let prefix = if i == 0 { "· " } else { "  " };
                lines.push(Line::styled(
                    format!("{prefix}{l}"),
                    Style::default().fg(SYSTEM).add_modifier(Modifier::ITALIC),
                ));
            }
            lines.push(Line::default());
        }
        Role::Shell => {
            // `$ cmd` highlighted, output dim — visually a local terminal block.
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    Style::default().fg(USER).add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(l.to_string(), Style::default().fg(SYSTEM)));
            }
            lines.push(Line::default());
        }
        Role::Agent => {
            lines.push(Line::from(Span::styled(
                "● agent",
                Style::default().fg(AGENT).add_modifier(Modifier::BOLD),
            )));
            if m.streaming {
                if !m.thinking.is_empty() {
                    for l in m.thinking.lines() {
                        lines.push(Line::styled(
                            l.to_string(),
                            Style::default()
                                .fg(SYSTEM)
                                .add_modifier(Modifier::ITALIC | Modifier::DIM),
                        ));
                    }
                }
                let mut body: Vec<Line> =
                    m.text.lines().map(|l| Line::raw(l.to_string())).collect();
                // Trailing spinner so the user sees liveness.
                let spin = SPINNER[spinner % SPINNER.len()];
                match body.last_mut() {
                    Some(last) => last
                        .spans
                        .push(Span::styled(format!(" {spin}"), Style::default().fg(AGENT))),
                    None => body.push(Line::styled(spin.to_string(), Style::default().fg(AGENT))),
                }
                lines.extend(body);
            } else if let Some(cached) = &m.rendered {
                // Finished reply: reuse the markdown rendered once at finish time.
                lines.extend(cached.iter().cloned());
            } else {
                // Fallback (should not happen): render on the fly.
                lines.extend(markdown::render(&m.text).lines);
            }
            lines.push(Line::default());
        }
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let (msg, color) = if app.hitl.is_some() {
        (
            "tool approval needed — [y] approve · [a] always (session) · [n] deny".to_string(),
            Color::Yellow,
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        (format!("{spin} generating… Ctrl+C to cancel"), AGENT)
    } else {
        let ctx = if app.context_task_id.is_some() {
            " · context kept"
        } else {
            ""
        };
        (format!("ready{ctx}"), SYSTEM)
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.agent),
            Style::default().fg(Color::Black).bg(AGENT),
        ),
        Span::raw("  "),
    ];
    if app.auto_approve {
        spans.push(Span::styled(
            " AUTO ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    if let Some(meta) = &app.channel {
        let short: String = meta.id.chars().take(8).collect();
        spans.push(Span::styled(
            format!(" ⏵ {}:{} ", short, meta.state),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(msg, Style::default().fg(color)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_hitl(f: &mut Frame, hitl: &super::stream::HitlRequest) {
    let area = centered_rect(70, 50, f.area());
    let input = serde_json::to_string_pretty(&hitl.tool_input).unwrap_or_default();
    let mut lines = vec![
        Line::from(Span::styled(
            hitl.prompt.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("tool: ", Style::default().fg(SYSTEM)),
            Span::styled(hitl.tool_name.clone(), Style::default().fg(Color::Yellow)),
        ]),
    ];
    for l in input.lines().take(12) {
        lines.push(Line::styled(l.to_string(), Style::default().fg(SYSTEM)));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            "[y]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" approve    "),
        Span::styled(
            "[a]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" always allow this tool (session)    "),
        Span::styled(
            "[n]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" deny / Esc"),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" approve tool call ");
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}
