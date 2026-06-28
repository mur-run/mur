//! Ratatui rendering: transcript pane, input box, status bar, HITL modal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

use std::time::Instant;

use super::app::{App, ChatMsg, Role, SPINNER};
use super::complete;
use super::markdown;
use super::welcome::welcome_lines;

/// Draw the whole UI for one frame.
pub fn render(f: &mut Frame, app: &mut App) {
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
    render_completion(f, app, chunks[1]);
    render_status(f, app, chunks[2]);

    // Inline approval lives on the card (Task 4) when the runtime sent a
    // step_id. Fall back to the centered modal only for older runtimes.
    if let Some(hitl) = &app.hitl
        && hitl.step_id.is_none()
    {
        render_hitl(f, hitl);
    }
}

/// Draw completion menu list anchored just above the input box.
/// No-op when the menu is closed or empty.
fn render_completion(f: &mut Frame, app: &App, input_area: Rect) {
    let Some(state) = &app.completion else {
        return;
    };
    if state.items.is_empty() {
        return;
    }
    let theme = app.theme;

    let rows: Vec<ListItem> = state
        .items
        .iter()
        .map(|c| {
            let mut spans = vec![Span::styled(
                c.display.clone(),
                Style::default().fg(theme.border_title),
            )];
            if !c.desc.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    c.desc.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let visible = rows.len().min(complete::MAX_MENU_ROWS) as u16;
    let popup_height = visible + 2; // +2 for borders

    // Anchor above the input box, then clamp to the frame so a popup taller
    // than the space above the input (short / stacked-pane terminals) can never
    // render out of bounds — ratatui panics on an out-of-buffer index.
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: input_area.width,
        height: popup_height,
    }
    .intersection(f.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" ↑↓ move · Tab accept · Esc close ")
        .title_style(Style::default().fg(theme.border_title));

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    f.render_widget(Clear, popup_area);
    f.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        popup_area,
        &mut list_state,
    );
}

fn render_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(theme.inner_padding as u16))
        .title(format!(" chat · {} ", app.agent))
        .title_style(Style::default().fg(theme.border_title));
    let inner = block.inner(area);

    let mut lines: Vec<Line> = Vec::new();
    let msg_count = app.messages.len();
    for (i, m) in app.messages.iter().enumerate() {
        push_message(&mut lines, m, app.spinner, theme);
        if i + 1 < msg_count {
            if theme.show_separator {
                let sep_width = inner.width as usize;
                lines.push(Line::styled(
                    "─".repeat(sep_width),
                    Style::default().fg(theme.separator),
                ));
            } else {
                lines.push(Line::default());
            }
        }
    }

    if lines.is_empty() {
        // Empty transcript → progressive-disclosure welcome (mascot + identity +
        // one example + /help hint) instead of a bare prompt. The eye frame is a
        // pure function of wall-clock time; the event loop schedules redraws on
        // the blink deadline so an idle welcome animates without busy-looping.
        lines = welcome_lines(
            theme,
            app.mascot_mode,
            &app.agent,
            app.cwd.as_deref(),
            app.blink.eye_open(Instant::now()),
        );
    }

    let total = lines.len() as u16;
    let visible = inner.height;
    let max_off = total.saturating_sub(visible);
    let offset = max_off.saturating_sub(app.scroll_back);

    let output = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(output, area);

    // Clamp away over-scroll so PageDown never needs "dead" presses, and record
    // the page size for the key handler. Done after render: the immutable `lines`
    // borrow of `app` ends when `output` is consumed above.
    // ponytail: scroll_back counts logical (pre-wrap) lines while `.scroll()`
    // takes post-wrap rows, so a page can be a row or two off under heavy
    // wrapping. Upgrade to wrapped-row accounting if that ever feels wrong.
    app.scroll_back = app.scroll_back.min(max_off);
    app.scroll_page = visible;
}

fn push_message(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static super::theme::Theme,
) {
    // Step cards replace role-based rendering entirely for that message.
    if let Some(card) = &m.step {
        lines.extend(super::render_card::card_lines(card, theme));
        return;
    }
    match m.role {
        Role::User => {
            lines.push(Line::from(Span::styled(
                "you ›",
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
            )));
            for l in m.text.lines() {
                lines.push(Line::styled(
                    l.to_string(),
                    Style::default().fg(theme.user_text),
                ));
            }
        }
        Role::System => {
            for (i, l) in m.text.lines().enumerate() {
                let prefix = if i == 0 { "· " } else { "  " };
                lines.push(Line::styled(
                    format!("{prefix}{l}"),
                    Style::default()
                        .fg(theme.system)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
        Role::Shell => {
            // `$ cmd` highlighted, output dim — visually a local terminal block.
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    Style::default()
                        .fg(theme.shell)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(
                    l.to_string(),
                    Style::default().fg(theme.system),
                ));
            }
        }
        Role::Agent => {
            lines.push(Line::from(Span::styled(
                "● agent",
                Style::default()
                    .fg(theme.agent)
                    .add_modifier(Modifier::BOLD),
            )));
            // Reasoning stays visible after the turn finishes (D5).
            if !m.thinking.is_empty() {
                for l in m.thinking.lines() {
                    lines.push(Line::styled(
                        l.to_string(),
                        Style::default()
                            .fg(theme.thinking)
                            .add_modifier(Modifier::ITALIC | Modifier::DIM),
                    ));
                }
            }
            if m.streaming {
                let mut body: Vec<Line> =
                    m.text.lines().map(|l| Line::raw(l.to_string())).collect();
                // Trailing spinner so the user sees liveness.
                let spin = SPINNER[spinner % SPINNER.len()];
                match body.last_mut() {
                    Some(last) => last.spans.push(Span::styled(
                        format!(" {spin}"),
                        Style::default().fg(theme.agent),
                    )),
                    None => body.push(Line::styled(
                        spin.to_string(),
                        Style::default().fg(theme.agent),
                    )),
                }
                lines.extend(body);
            } else if let Some(cached) = &m.rendered {
                // Finished reply: reuse the markdown rendered once at finish time.
                lines.extend(cached.iter().cloned());
            } else {
                // Fallback (should not happen): render on the fly.
                lines.extend(markdown::render(&m.text).lines);
            }
        }
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let (msg, color) = if app.hitl.is_some() {
        (
            "tool approval needed — [y] approve · [a] always (session) · [n] deny".to_string(),
            Color::Yellow,
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        (
            format!("{spin} generating… · type to steer · Ctrl+C to cancel"),
            theme.agent,
        )
    } else {
        let ctx = if app.context_task_id.is_some() {
            " · context kept"
        } else {
            ""
        };
        (format!("ready{ctx}"), theme.system)
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.agent),
            Style::default().fg(theme.badge_fg).bg(theme.badge_bg),
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
            Style::default().fg(theme.agent),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(msg, Style::default().fg(color)));

    // Glass Box observability: tokens · cost · ctx · timer.
    let obs = footer_segments(
        app.turn_in,
        app.turn_out,
        app.session_in,
        app.session_out,
        app.ctx_tokens,
        &app.pricing,
        app.budget_usd,
    );
    spans.push(Span::raw(" · "));
    spans.push(Span::styled(obs, Style::default().fg(theme.system)));
    if let Some(t0) = app.turn_started {
        let secs = t0.elapsed().as_secs();
        spans.push(Span::styled(
            format!(" · {}m{:02}s · esc=stop", secs / 60, secs % 60),
            Style::default().fg(theme.system),
        ));
    }

    let right_hint: Option<(String, Color)> = if app.scroll_back > 0 {
        Some((
            format!("↑ {} lines · ⬇ to bottom", app.scroll_back),
            theme.system,
        ))
    } else if app.esc_hint {
        let hint = if app.streaming {
            "ESC again to cancel"
        } else {
            "ESC again to clear"
        };
        Some((hint.to_string(), theme.system))
    } else {
        None
    };

    if let Some((hint_text, hint_color)) = right_hint {
        let hint_display = format!(" {} ", hint_text);
        let hint_width = hint_display.chars().count() as u16;
        let left_width: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
        let bar_width = area.width;
        // Only right-align if there's room; otherwise skip the hint.
        if bar_width > left_width + hint_width {
            let pad = bar_width - left_width - hint_width;
            spans.push(Span::raw(" ".repeat(pad as usize)));
            spans.push(Span::styled(
                hint_display,
                Style::default().fg(hint_color).add_modifier(Modifier::DIM),
            ));
        }
    }

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
            Span::styled("tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(hitl.tool_name.clone(), Style::default().fg(Color::Yellow)),
        ]),
    ];
    for l in input.lines().take(12) {
        lines.push(Line::styled(
            l.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
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

/// Format a token count with thousands separator (e.g. 1240 → "1,240").
fn fmt_tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!(
            "{},{:03},{:03}",
            n / 1_000_000,
            (n / 1_000) % 1_000,
            n % 1_000
        )
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}

/// Pure footer formatter: `"{turn}/{sess} tok · {cost} · ctx <bar> N%"`.
/// Cost shows `—` when unpriced; ctx part is omitted when no window is known.
fn footer_segments(
    turn_in: u64,
    turn_out: u64,
    sess_in: u64,
    sess_out: u64,
    ctx_tokens: u64,
    pricing: &super::footer::Pricing,
    budget_usd: Option<f64>,
) -> String {
    use super::footer::{CTX_BAR_WIDTH, UsageCounts, context_pct, ctx_bar, turn_cost};

    let turn_tok = turn_in + turn_out;
    let sess_tok = sess_in + sess_out;

    let u = UsageCounts {
        input: turn_in,
        output: turn_out,
    };
    let cost = match turn_cost(pricing, &u) {
        Some(c) => format!("${:.3} est", c),
        None => "\u{2014}".to_string(), // em dash
    };

    let ctx_part = match pricing.window {
        Some(w) if w > 0 => {
            let pct = context_pct(ctx_tokens, w);
            format!(" · ctx {} {}%", ctx_bar(pct, CTX_BAR_WIDTH), pct)
        }
        _ => String::new(),
    };

    // Budget suffix: estimated session spend against the cap. The spent figure
    // is omitted (just `/ $cap`) when the model has no pricing.
    let budget_part = match budget_usd {
        Some(cap) => {
            let sess = UsageCounts {
                input: sess_in,
                output: sess_out,
            };
            match turn_cost(pricing, &sess) {
                Some(spent) => format!(" · ${spent:.2} / ${cap:.2}"),
                None => format!(" · / ${cap:.2}"),
            }
        }
        None => String::new(),
    };

    format!(
        "{}/{} tok · {}{}{}",
        fmt_tok(turn_tok),
        fmt_tok(sess_tok),
        cost,
        ctx_part,
        budget_part
    )
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

#[cfg(test)]
mod footer_fmt_tests {
    use super::footer_segments;
    use crate::cmd::agent::cli::footer::Pricing;

    #[test]
    fn shows_tokens_and_dash_cost_when_unpriced() {
        let s = footer_segments(1240, 0, 1240, 0, 0, &Pricing::default(), None);
        assert!(s.contains("1,240 tok") || s.contains("1240 tok"));
        assert!(s.contains('\u{2014}')); // em dash — no price
    }

    #[test]
    fn shows_cost_and_ctx_when_priced() {
        let p = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(100_000),
        };
        let s = footer_segments(1000, 1000, 1000, 1000, 32_000, &p, None);
        assert!(s.contains("$0.018"));
        assert!(s.contains("32%"));
        assert!(!s.contains(" / $")); // no budget suffix when budget is None
    }

    #[test]
    fn shows_budget_suffix_when_cap_set() {
        let p = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        // session 1000/1000 → $18.00 spent, cap $20.00
        let s = footer_segments(1000, 1000, 1000, 1000, 0, &p, Some(20.0));
        assert!(s.contains("$18.00 / $20.00"), "got: {s}");
        // unpriced model → spent omitted, cap still shown
        let s2 = footer_segments(1000, 1000, 1000, 1000, 0, &Pricing::default(), Some(20.0));
        assert!(s2.contains("/ $20.00"), "got: {s2}");
        assert!(!s2.contains("$18.00"));
    }
}
