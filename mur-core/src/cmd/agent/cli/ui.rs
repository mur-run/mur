//! Ratatui rendering: transcript pane, input box, status bar, HITL modal.

use ratatui::Frame;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Widget, Wrap,
};

use std::time::Instant;

use super::app::{App, ChatMsg, Role, SPINNER, Severity};
use super::complete;
use super::markdown;
use super::welcome::welcome_lines;

/// Footer hint shown at the bottom of the full-screen transcript overlay
/// (Ctrl+O). Enter and Esc both return to chat; Ctrl+D quits outright, same
/// as the composer — the overlay never lets a keypress fall through
/// unhandled into the input box.
const OVERLAY_HINT: &str = " press Enter or Esc to return · Ctrl+D quit ";

/// Body indent under a role header ("you ›" / "● agent") so a message's content
/// reads as belonging to its speaker rather than sitting flush with the header.
const MSG_INDENT: &str = "  ";

/// Prepend the body indent to an already-styled line (e.g. cached markdown).
fn indent_line(mut line: Line<'static>) -> Line<'static> {
    line.spans.insert(0, Span::raw(MSG_INDENT));
    line
}

/// Draw the whole UI for one frame.
pub fn render(f: &mut Frame, app: &mut App) {
    // Full-screen transcript overlay (Ctrl+O) takes over the whole frame and
    // owns every keypress (see `overlay_key_action` in `mod.rs`'s
    // `handle_event`) — nothing else renders underneath it this frame.
    if app.overlay_open {
        render_overlay(f, app);
        return;
    }
    let input_lines = app.input.lines().len() as u16;
    let input_height = (input_lines + 2).clamp(3, 8);
    // The agent chooser (suggested replies) renders as its own layout band
    // between transcript and composer — never a Clear-overlay popup — so it
    // can't cover the reply the user must read to choose. The slash-command
    // menu keeps the compact popup (the user is typing, not reading).
    let chooser_h = chooser_band_height(app, f.area().height, input_height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(chooser_h),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_transcript(f, app, chunks[0]);
    if chooser_h > 0 {
        render_chooser_band(f, app, chunks[1]);
    } else {
        render_completion(f, app, chunks[2]);
    }
    f.render_widget(&app.input, chunks[2]);
    render_status(f, app, chunks[3]);

    // Inline approval lives on the card (Task 4) when the runtime sent a
    // step_id. Fall back to the centered modal only for older runtimes.
    if let Some(hitl) = &app.hitl
        && hitl.step_id.is_none()
    {
        render_hitl(f, hitl);
    }
}

/// Draw the full-screen transcript overlay (Ctrl+O): the plain-text
/// transcript (native select/copy/search works because we never leave raw
/// mode or the alt-screen — this is just another ratatui frame) plus a
/// footer hint. Scroll follows the normal `scroll_back`/PageUp/PageDown
/// state so the same keys work here as in the regular transcript pane.
fn render_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let text = app.overlay_text.as_deref().unwrap_or("");
    let total_lines = text.lines().count() as u16;
    let visible = chunks[0].height;
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.scroll_back.min(max_scroll);
    // scroll_back counts lines up from the bottom; ratatui's Paragraph scroll
    // counts down from the top, so invert it.
    let top_offset = max_scroll.saturating_sub(scroll);

    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" transcript ");
    f.render_widget(
        Paragraph::new(Text::raw(text))
            .block(block)
            .scroll((top_offset, 0)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            OVERLAY_HINT,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))),
        chunks[1],
    );
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

    // The suggested-reply chooser (`spaced`) is a Claude-Code-style option list:
    // each option gets a label line, an optional dimmed description line, and a
    // blank spacer row so the choices breathe. The slash-command menu stays
    // one-line-per-row and reverse-highlighted.
    let rows: Vec<ListItem> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if state.spaced {
                // Numbered option: "N  label" + a dimmed, aligned description +
                // a spacer. The number is a quiet affordance for digit-select.
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{}  ", i + 1), Style::default().fg(theme.system)),
                    Span::styled(c.display.clone(), Style::default().fg(theme.agent_text)),
                ])];
                if !c.desc.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("   {}", c.desc), // align under the label (past "N  ")
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::default()); // spacer between options
                ListItem::new(lines)
            } else {
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
            }
        })
        .collect();

    // Height = actual rendered lines of the shown items (+2 borders). Spaced
    // items span several lines each, so a flat item count would clip them.
    let shown = rows.len().min(complete::MAX_MENU_ROWS);
    let content_lines: usize = rows.iter().take(shown).map(ListItem::height).sum();
    let popup_height = (content_lines as u16).saturating_add(2);

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

    let title = if state.spaced {
        " 1-9 pick · ↑↓ move · Enter accept · Esc close "
    } else {
        " ↑↓ move · Tab accept · Esc close "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(state.spaced as u16))
        .title(title)
        .title_style(Style::default().fg(theme.border_title));

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    // Spaced items are multi-line; a full reverse bar would paint the spacer and
    // description too. Mark the selection with a caret + accent-bold label
    // instead. The slash menu keeps its compact reverse highlight.
    let (highlight_style, highlight_symbol) = if state.spaced {
        (
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
            "❯ ",
        )
    } else {
        (Style::default().add_modifier(Modifier::REVERSED), "")
    };

    f.render_widget(Clear, popup_area);
    f.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(highlight_style)
            .highlight_symbol(highlight_symbol),
        popup_area,
        &mut list_state,
    );
}

/// Height of the chooser band, or 0 when no agent chooser is open (slash
/// menu stays a popup). Full spaced rows (label + optional desc + spacer)
/// when they fit above the composer while leaving the transcript at least
/// `MIN_TRANSCRIPT_ROWS`; otherwise compact one-line rows; never taller
/// than the space available (the List scrolls the selection into view).
const MIN_TRANSCRIPT_ROWS: u16 = 3;

fn chooser_band_height(app: &App, total_h: u16, input_height: u16) -> u16 {
    let Some(state) = &app.completion else {
        return 0;
    };
    if !state.spaced || state.items.is_empty() {
        return 0;
    }
    let available = total_h
        .saturating_sub(input_height + 1) // composer + status line
        .saturating_sub(MIN_TRANSCRIPT_ROWS);
    let full: u16 = state
        .items
        .iter()
        .map(|c| 2 + u16::from(!c.desc.is_empty())) // label + spacer (+ desc)
        .sum::<u16>()
        .saturating_add(2); // borders
    let compact = (state.items.len() as u16).saturating_add(2);
    let auto = full.min(available).max(compact.min(available)).max(3);
    // Ctrl+↑/↓ while the chooser is open grows/shrinks the band on top of
    // the auto height, clamped so the transcript keeps its minimum rows.
    (i32::from(auto) + i32::from(app.chooser_grow)).clamp(3, i32::from(available.max(3))) as u16
}

/// Draw the agent chooser into its own layout band. Falls back to compact
/// one-line rows ("N label — desc") when the band is shorter than the full
/// spaced form.
fn render_chooser_band(f: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.completion else {
        return;
    };
    let theme = app.theme;
    let full: u16 = state
        .items
        .iter()
        .map(|c| 2 + u16::from(!c.desc.is_empty()))
        .sum::<u16>()
        .saturating_add(2);
    let compact = area.height < full;

    let rows: Vec<ListItem> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if compact {
                let mut spans = vec![
                    Span::styled(format!("{} ", i + 1), Style::default().fg(theme.system)),
                    Span::styled(c.display.clone(), Style::default().fg(theme.agent_text)),
                ];
                if !c.desc.is_empty() {
                    spans.push(Span::styled(
                        format!(" — {}", c.desc),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            } else {
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{} ", i + 1), Style::default().fg(theme.system)),
                    Span::styled(c.display.clone(), Style::default().fg(theme.agent_text)),
                ])];
                if !c.desc.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("   {}", c.desc),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::default());
                ListItem::new(lines)
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(1))
        .title(" 1-9 pick · ↑↓ move · Enter accept · Esc close · Ctrl+↑↓ resize ")
        .title_style(Style::default().fg(theme.border_title));

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    f.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("❯ "),
        area,
        &mut list_state,
    );
}

/// Draws the transcript pane and returns its *ideal* height (content rows +
/// borders, unclamped by `area`) — the caller uses this to shrink the inline
/// viewport itself via `Terminal::resize` before the next frame. Clamped
/// drawing still happens against `area` here so a still-oversized viewport
/// (the frame right after content shrinks, before the resize takes effect)
/// never panics or scrolls oddly.
/// Fixed separator width for flushed scrollback lines: the real frame width
/// isn't known at flush time (content prints above the inline viewport and
/// the terminal soft-wraps it), so a modest fixed rule stands in.
const SEPARATOR_WIDTH: usize = 60;

/// Flush every settled message into the terminal's native scrollback via
/// `Terminal::insert_before`, so the live viewport only ever paints the
/// currently-streaming tail. A message is "settled" once it can no longer
/// change: not itself streaming, and not the trailing entry while a turn is
/// in progress (the tail may still gain appended text). No-op in Fullscreen
/// mode (the overlay reads `app.messages` directly) and when nothing new has
/// settled since the last call.
pub fn flush_finished<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
) -> std::io::Result<()> {
    use super::app::RenderMode;
    if app.render_mode != RenderMode::Inline {
        return Ok(());
    }
    let theme = app.theme;
    let total = app.messages.len();
    let ceiling = if app.streaming {
        total.saturating_sub(1)
    } else {
        total
    };
    let mut end = app.flushed_upto;
    while end < ceiling && !app.messages[end].streaming {
        end += 1;
    }
    if end <= app.flushed_upto {
        return Ok(());
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for i in app.flushed_upto..end {
        if i > 0 {
            if theme.show_separator {
                lines.push(Line::styled(
                    "─".repeat(SEPARATOR_WIDTH),
                    Style::default().fg(theme.separator),
                ));
            } else {
                lines.push(Line::default());
            }
        }
        push_message(
            &mut lines,
            &app.messages[i],
            app.spinner,
            theme,
            app.cards_expanded,
        );
    }

    // Height must be the WRAPPED (physical) row count, not the logical line
    // count: `insert_before` renders into a buffer exactly `height` rows tall,
    // and `Wrap` soft-wraps any line wider than the pane into extra rows. Using
    // `lines.len()` clips every wrapped overflow row — a long message loses its
    // tail into the void (never reaches scrollback, so it can't be scrolled
    // back to). `Paragraph::line_count(width)` accounts for wrap + the padding
    // block. (Enabled by the `unstable-rendered-line-info` ratatui feature.)
    let pad = theme.inner_padding as u16;
    let width = terminal.size()?.width.max(1);
    let text = Text::from(lines);
    let block = || Block::default().padding(Padding::horizontal(pad));
    let height = (Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .block(block())
        .line_count(width) as u16)
        .max(1);
    terminal.insert_before(height, |buf| {
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(block())
            .render(buf.area, buf);
        blank_wide_char_continuations(buf);
    })?;
    app.flushed_upto = end;
    Ok(())
}

/// Work around a ratatui 0.29 bug: `Terminal::insert_before` flushes the whole
/// buffer through `draw_lines`, which (unlike the normal diff-based flush) does
/// NOT skip the trailing continuation cell of a wide (CJK) grapheme. That cell
/// holds a space, so every wide char prints as "char " — spacing out CJK text
/// in scrollback. Blank the continuation cell's symbol so the backend prints
/// nothing there; the cursor has already advanced two columns for the wide
/// char, so the next glyph lands correctly. (Live-viewport draws use the diff
/// path and are unaffected.)
fn blank_wide_char_continuations(buf: &mut ratatui::buffer::Buffer) {
    use unicode_width::UnicodeWidthStr;
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x + 1 < area.right() {
            if buf[(x, y)].symbol().width() >= 2 {
                buf[(x + 1, y)].set_symbol("");
                x += 2;
            } else {
                x += 1;
            }
        }
    }
}

/// Wrapped (physical) row count of the *live* region — the still-unflushed
/// transcript tail (`app.messages[flushed_upto..]`), soft-wrapped at the
/// transcript pane's inner width. Same accounting as `render_transcript` and
/// `flush_finished` (`Paragraph::line_count`, not `lines.len()`), so the
/// dynamic viewport height in the event loop stays in lock-step with what the
/// live region actually paints. `outer_width` is the full transcript pane
/// width (before the TOP/BOTTOM border block trims it to inner width).
///
/// Returns 0 when there is nothing live to paint (all settled + flushed).
/// The empty-transcript welcome screen is intentionally NOT measured here:
/// `desired_viewport_h` keeps the full viewport in that case.
pub fn live_tail_rows(app: &App, outer_width: u16) -> u16 {
    let theme = app.theme;
    let start = app.flushed_upto.min(app.messages.len());
    if start >= app.messages.len() {
        return 0;
    }
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages[start..] {
        push_message(&mut lines, m, app.spinner, theme, app.cards_expanded);
    }
    if lines.is_empty() {
        return 0;
    }
    // Reproduce render_transcript's block exactly so the wrapped row count
    // matches the painted region: horizontal padding trims the same columns,
    // and `line_count` folds in wrap + padding at the inner width.
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .padding(Padding::horizontal(theme.inner_padding as u16));
    let inner_width = block.inner(Rect::new(0, 0, outer_width.max(1), 1)).width;
    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .line_count(inner_width.max(1)) as u16
}

/// Draws the *live* region only: everything at index `< app.flushed_upto` has
/// already been flushed into the terminal's own scrollback (see
/// `flush_finished`), so this is at most the one currently-streaming message
/// — auto-following its tail as it grows, the same way `tail -f` does, rather
/// than user-controlled paging (there's nothing left here to page through;
/// full history lives in native scrollback, or the Ctrl+O overlay's own
/// `scroll_back`-driven view of the complete `app.messages`).
fn render_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(theme.border_type)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(theme.inner_padding as u16))
        .title(format!(" chat · {} ", app.agent))
        .title_style(Style::default().fg(theme.border_title));
    let inner = block.inner(area);
    let inner_width = inner.width;

    let start = app.flushed_upto.min(app.messages.len());
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages[start..] {
        push_message(&mut lines, m, app.spinner, theme, app.cards_expanded);
    }

    if lines.is_empty() && start == 0 {
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

    // Same wrapped-row accounting as before (`line_count`, not `lines.len()`).
    // `scroll_back` counts lines up from the bottom (0 = follow the tail);
    // PageUp/PageDown and the chooser both rely on this so a long unflushed
    // reply stays reachable even while an option list is open.
    let output = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let total = output.line_count(inner_width) as u16;
    let visible = inner.height;
    app.scroll_page = visible.max(1);
    let max_scroll = total.saturating_sub(visible);
    app.scroll_back = app.scroll_back.min(max_scroll);
    let offset = max_scroll - app.scroll_back;

    f.render_widget(output.block(block).scroll((offset, 0)), area);
}

fn push_message(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static super::theme::Theme,
    cards_expanded: bool,
) {
    // Step cards replace role-based rendering entirely for that message.
    if let Some(card) = &m.step {
        lines.extend(super::render_card::card_lines(card, theme, cards_expanded));
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
                    format!("{MSG_INDENT}{l}"),
                    Style::default().fg(theme.user_text),
                ));
            }
        }
        Role::System => {
            // Severity paints the whole note and picks a lead glyph, so a
            // warning reads amber and a success reads green at a glance.
            let (color, glyph) = match m.severity {
                Severity::Info => (theme.system, "·"),
                Severity::Warn => (theme.warn, "▲"),
                Severity::Error => (theme.error, "✖"),
                Severity::Success => (theme.success, "✔"),
            };
            let bold = !matches!(m.severity, Severity::Info);
            for (i, l) in m.text.lines().enumerate() {
                let prefix = if i == 0 {
                    format!("{glyph} ")
                } else {
                    "  ".to_string()
                };
                let mut style = Style::default().fg(color);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                lines.push(Line::styled(format!("{prefix}{l}"), style));
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
            // Animated bullet while streaming, solid bullet once done — so an
            // in-progress turn reads as "live" at a glance vs. a finished one.
            let (bullet, header_style) = if m.streaming {
                let spin = SPINNER[spinner % SPINNER.len()];
                (format!("{spin} agent"), Style::default().fg(theme.agent))
            } else {
                (
                    "● agent".to_string(),
                    Style::default()
                        .fg(theme.agent)
                        .add_modifier(Modifier::BOLD),
                )
            };
            lines.push(Line::from(Span::styled(bullet, header_style)));
            // Reasoning stays visible after the turn finishes (D5).
            if !m.thinking.is_empty() {
                for l in m.thinking.lines() {
                    lines.push(Line::styled(
                        format!("{MSG_INDENT}{l}"),
                        Style::default()
                            .fg(theme.thinking)
                            .add_modifier(Modifier::ITALIC | Modifier::DIM),
                    ));
                }
            }
            if m.streaming {
                let mut body: Vec<Line> = m
                    .text
                    .lines()
                    .map(|l| Line::raw(format!("{MSG_INDENT}{l}")))
                    .collect();
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
                lines.extend(cached.iter().cloned().map(indent_line));
            } else {
                // Fallback (should not happen): render on the fly.
                lines.extend(markdown::render(&m.text).lines.into_iter().map(indent_line));
            }
        }
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let (msg, color) = if let Some(req) = &app.hitl {
        // Surface the auto-deny clock: the gate expires approvals after
        // DEFAULT_TIMEOUT (300s). Reuse `created_at` rather than tracking new
        // state; the status bar redraws on each blink deadline so it ticks.
        let remaining = crate::hitl::gate::DEFAULT_TIMEOUT
            .as_secs()
            .saturating_sub(req.created_at.elapsed().as_secs());
        (
            format!(
                "tool approval needed (auto-deny in {remaining}s) — [y] approve · [a] always (session) · [n] deny"
            ),
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
    } else if app.ctrl_c_hint {
        Some(("Ctrl+C again to quit".to_string(), theme.system))
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
mod wide_char_tests {
    use super::blank_wide_char_continuations;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    #[test]
    fn cjk_continuation_cells_are_blanked() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        Paragraph::new("有既有workflow")
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        // ratatui fills wide-char continuation cells with a space.
        assert_eq!(buf[(1, 0)].symbol(), " ");
        blank_wide_char_continuations(&mut buf);
        // After the fix: each CJK glyph is followed by an empty (skipped) cell,
        // so the backend prints "有既有" with no interleaved spaces.
        assert_eq!(buf[(0, 0)].symbol(), "有");
        assert_eq!(buf[(1, 0)].symbol(), "");
        assert_eq!(buf[(2, 0)].symbol(), "既");
        assert_eq!(buf[(3, 0)].symbol(), "");
        assert_eq!(buf[(4, 0)].symbol(), "有");
        assert_eq!(buf[(5, 0)].symbol(), "");
        // ASCII run is untouched.
        assert_eq!(buf[(6, 0)].symbol(), "w");
        assert_eq!(buf[(7, 0)].symbol(), "o");
    }
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
