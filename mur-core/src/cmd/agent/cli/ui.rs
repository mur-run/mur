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

/// Composer height when the input is empty (one text row plus its border).
/// Typing grows it up to `INPUT_H_MAX`.
const INPUT_H_MIN: u16 = 3;
const INPUT_H_MAX: u16 = 8;

/// Approval modal size, as a percentage of the viewport.
const HITL_PCT_X: u16 = 70;
const HITL_PCT_Y: u16 = 50;

/// Rows one PgUp/PgDn moves the approval modal's body. Fixed rather than
/// "one screenful" because the key handler decides the step and only the
/// renderer knows the box height; the renderer clamps whatever it is handed.
pub(super) const HITL_SCROLL_PAGE: u16 = 5;

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
    let input_height = (input_lines + 2).clamp(INPUT_H_MIN, INPUT_H_MAX);
    // The agent chooser (suggested replies) renders as its own layout band
    // between transcript and composer — never a Clear-overlay popup — so it
    // can't cover the reply the user must read to choose. The slash-command
    // menu keeps the compact popup (the user is typing, not reading).
    let chooser_h = chooser_band_height(app, f.area().height, input_height);
    let rail_h = fleet_rail_height(app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(rail_h),
            Constraint::Length(chooser_h),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_transcript(f, app, chunks[0]);
    if rail_h > 0 {
        render_fleet_rail(f, app, chunks[1]);
    }
    if chooser_h > 0 {
        render_chooser_band(f, app, chunks[2]);
    } else {
        render_completion(f, app, chunks[3]);
    }
    f.render_widget(&app.input, chunks[3]);
    render_status(f, app, chunks[4]);

    // The centered modal is the fallback whenever the approval's inline row on
    // a step card is not actually visible. Key it on VISIBILITY recomputed per
    // frame (`hitl_inline_visible`), never on whether the runtime sent a
    // step_id and never on a cached flag: the gate commonly fires before the
    // card exists, and a card that is live when the gate opens can be flushed
    // into frozen scrollback while it is still open. Both used to leave the
    // operator with neither surface. The invariant: an open gate always has at
    // least one place the operator can see it.
    if let Some(hitl) = app
        .hitl
        .clone()
        .filter(|h| !app.hitl_inline_visible(h.step_id.as_deref()))
    {
        app.hitl_scroll = render_hitl(
            f,
            &hitl,
            app.hitl_grant_confirm,
            app.input_text().is_empty(),
            app.hitl_scroll,
        );
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

/// Share of the viewport the transcript keeps when the chooser is open,
/// in percent.
const TRANSCRIPT_FLOOR_PCT: u16 = 40;

fn chooser_band_height(app: &App, total_h: u16, input_height: u16) -> u16 {
    let Some(state) = &app.completion else {
        return 0;
    };
    if !state.spaced || state.items.is_empty() {
        return 0;
    }
    let chrome = input_height + 1; // composer + status line
    let full: u16 = state
        .items
        .iter()
        .map(|c| 2 + u16::from(!c.desc.is_empty())) // label + spacer (+ desc)
        .sum::<u16>()
        .saturating_add(2); // borders
    let compact = (state.items.len() as u16).saturating_add(2);
    // Prefer the readable floor; fall back to the hard minimum only when the
    // floor would squeeze the chooser below its compact form. The chooser is
    // what the operator must act on, so it never loses this trade.
    let roomy = total_h
        .saturating_sub(chrome)
        .saturating_sub((total_h * TRANSCRIPT_FLOOR_PCT / 100).max(MIN_TRANSCRIPT_ROWS));
    let tight = total_h.saturating_sub(chrome + MIN_TRANSCRIPT_ROWS);
    let available = if roomy >= compact { roomy } else { tight };
    // Take `compact` exactly when the spaced form does not fit — padding the
    // band out to `available` spends rows on nothing.
    let auto = if full <= available {
        full
    } else {
        compact.min(available).max(3)
    };
    // Ctrl+↑/↓ while the chooser is open grows/shrinks the band on top of
    // the auto height, clamped so the transcript keeps its minimum rows.
    (i32::from(auto) + i32::from(app.chooser_grow))
        .clamp(3, i32::from(tight.max(MIN_TRANSCRIPT_ROWS))) as u16
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

/// Fixed separator width for flushed scrollback lines: the real frame width
/// isn't known at flush time (content prints above the inline viewport and
/// the terminal soft-wraps it), so a modest fixed rule stands in.
const SEPARATOR_WIDTH: usize = 60;

/// Rows the fleet rail paints: one collapsed line, plus a capped member list
/// when someone is blocked, plus one more row for the "… N more" truncation
/// notice when the member list doesn't fit. Must always equal
/// `rail_lines(view, _).len()` — see `the_rail_height_matches_painted_lines`.
/// A working fleet is not news; a stalled one is.
pub fn rail_height_for(view: &crate::cmd::agent::cli::fleet_rail::RailView) -> u16 {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let blocked = view
        .members
        .iter()
        .any(|m| matches!(m.state, MemberState::Blocked { .. }));
    if !blocked {
        return 1;
    }
    let shown = view.members.len().min(MAX_EXPANDED_ROWS) as u16;
    let truncated = view.members.len() > MAX_EXPANDED_ROWS;
    1 + shown + u16::from(truncated)
}

/// Height of the rail band for the current app state; 0 when `--fleet` is off.
pub fn fleet_rail_height(app: &App) -> u16 {
    app.fleet_view().map(rail_height_for).unwrap_or(0)
}

/// The fleet rail's content as plain lines: the head line, a capped member
/// list, and a truncation notice when the list doesn't fit. Pulled out of
/// `render_fleet_rail` so `rail_height_for` has something concrete to be
/// tested against instead of a number nothing checks — the truncation notice
/// is the thing most likely to silently fall off the bottom again.
fn rail_lines(
    view: &crate::cmd::agent::cli::fleet_rail::RailView,
    theme: &'static super::theme::Theme,
) -> Vec<Line<'static>> {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let mut lines: Vec<Line> = Vec::new();

    let head = match &view.notice {
        Some(n) => format!("{}  {n}", view.jobs_line),
        None => view.jobs_line.clone(),
    };
    lines.push(Line::styled(
        head,
        Style::default()
            .fg(theme.border_title)
            .add_modifier(Modifier::BOLD),
    ));

    if rail_height_for(view) > 1 {
        for m in view.members.iter().take(MAX_EXPANDED_ROWS) {
            let (body, color) = match &m.state {
                MemberState::Blocked { summary, .. } => (format!("blocked: {summary}"), theme.warn),
                MemberState::Working { tool, since } => (
                    match tool {
                        Some(t) => format!("working ({}) · {t}", elapsed(*since)),
                        None => format!("working ({})", elapsed(*since)),
                    },
                    theme.agent,
                ),
                MemberState::Done => ("done".to_string(), theme.success),
                MemberState::Failed => ("failed".to_string(), theme.error),
            };
            let glyph = m.state.glyph();
            lines.push(Line::styled(
                format!("  {:<10} {glyph} {body}", m.agent),
                Style::default().fg(color),
            ));
        }
        let extra = view.members.len().saturating_sub(MAX_EXPANDED_ROWS);
        if extra > 0 {
            lines.push(Line::styled(
                format!("  … {extra} more"),
                Style::default().fg(theme.system),
            ));
        }
    }

    lines
}

fn render_fleet_rail(f: &mut Frame, app: &App, area: Rect) {
    let Some(view) = app.fleet_view() else {
        return;
    };
    f.render_widget(
        Paragraph::new(Text::from(rail_lines(view, app.theme))),
        area,
    );
}

/// "2m" / "1h04m" — elapsed since a member last changed state. Shown instead
/// of a staleness verdict: a runtime that died mid-turn shows a growing
/// number rather than a state we guessed.
fn elapsed(since: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - since).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Rows left for the live transcript band inside `viewport_h` once the
/// composer, status line, chooser band, fleet rail and the band's own
/// TOP/BOTTOM borders have taken theirs.
fn band_capacity(viewport_h: u16, input_h: u16, chooser_h: u16, rail_h: u16) -> u16 {
    viewport_h.saturating_sub(input_h + 1 + chooser_h + rail_h + 2)
}

/// Rows the live transcript band may KEEP inside a viewport of `viewport_h`.
///
/// Deliberately the band's LARGEST height, not its height this frame: a
/// grown composer and an open chooser band are both temporary, but a flush
/// is not — rows pushed to scrollback never come back, so flushing to fit a
/// transient squeeze leaves a blank hole above the composer the moment that
/// squeeze ends. Over-keeping is free: the band tail-follows, so surplus
/// rows simply wait off-screen until the space is theirs again. The rail is
/// the exception — it stays for the session, so it really does take its rows.
fn band_inner_rows(app: &App, viewport_h: u16) -> u16 {
    band_capacity(viewport_h, INPUT_H_MIN, 0, fleet_rail_height(app))
}

/// Index one past the last message that is settled AND therefore flushable:
/// everything before the still-streaming turn.
fn settle_end(app: &App) -> usize {
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
    end
}

fn prefix_hash(s: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(s.as_bytes());
    h.finish()
}

/// Bytes of `messages[flushed_upto].text` already committed to scrollback —
/// or 0 when that bookkeeping no longer describes the message there.
///
/// It can stop describing it two ways: `finish_agent_turn` installs the
/// authoritative reply over the streamed text, and `fail_turn` drops the
/// streaming message outright. Both are caught by re-hashing the prefix, so a
/// remainder is never spliced onto text that never had that prefix.
fn effective_skip(app: &App) -> usize {
    if app.flushed_bytes == 0 {
        return 0;
    }
    let Some(m) = app.messages.get(app.flushed_upto) else {
        return 0;
    };
    if m.role != Role::Agent || m.step.is_some() {
        return 0;
    }
    match m.text.get(..app.flushed_bytes) {
        Some(p) if prefix_hash(p) == app.flushed_hash => app.flushed_bytes,
        _ => 0,
    }
}

/// Byte offset one past the FIRST complete markdown block in `rest` (0 when
/// there is none yet) — the next chunk of a streaming reply that can be
/// committed to scrollback.
///
/// A block ends at a blank line outside a fenced code block. Committing whole
/// blocks is what makes a chunk immutable: block-level markdown renders the
/// same alone as it does in context, so the renderer can never want to rewrite
/// a line that is already in scrollback and can no longer be redrawn.
fn next_block_end(rest: &str) -> usize {
    let mut in_fence = false;
    let mut off = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && trimmed.is_empty() {
            return off + line.len();
        }
        off += line.len();
    }
    0
}

/// Lines the live band paints for one message, honoring a committed prefix on
/// the band's head message (its committed part is already in scrollback).
fn push_live(lines: &mut Vec<Line<'static>>, app: &App, m: &ChatMsg, skip: usize) {
    push_live_inner(lines, app, m, skip, false)
}

/// Same, but rendered the way the message will look once it SETTLES — used for
/// every flush decision.
///
/// A streaming body paints raw (markdown is only parsed at finish time), and
/// raw is taller: markdown collapses the blank lines between list items and
/// paragraphs. Measuring raw over-commits — the band would be full while
/// streaming and then, the moment the turn settles and re-renders shorter,
/// short by exactly the lines markdown folded away, with no way to pull them
/// back out of scrollback. Measuring settled means the band is exactly full
/// after the turn; while streaming it simply tail-follows, as it always has.
fn push_live_measured(lines: &mut Vec<Line<'static>>, app: &App, m: &ChatMsg, skip: usize) {
    push_live_inner(lines, app, m, skip, true)
}

fn push_live_inner(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    m: &ChatMsg,
    skip: usize,
    as_settled: bool,
) {
    let streaming_agent = m.streaming && m.role == Role::Agent && m.step.is_none();
    if skip == 0 && !(as_settled && streaming_agent) {
        push_message(
            lines,
            m,
            app.spinner,
            app.theme,
            app.cards_expanded,
            app.width,
        );
        return;
    }
    if skip == 0 {
        // Measuring a streaming turn: header + reasoning, then the settled body.
        push_agent_header(lines, m, app.spinner, app.theme);
    }
    // Continuation of a partially-committed agent turn: body only, no header.
    lines.extend(agent_body_lines(
        m.text.get(skip..).unwrap_or(""),
        m.streaming && !as_settled,
        app.spinner,
        app.theme,
        None,
    ));
}

/// Wrapped (physical) row count of `lines` inside the band, measured exactly
/// the way `render_transcript` paints them (`Paragraph::line_count`, not
/// `lines.len()`), so flush decisions stay in lock-step with the band.
/// `outer_width` is the full pane width, before the border block trims it.
fn band_rows(
    theme: &'static super::theme::Theme,
    lines: Vec<Line<'static>>,
    outer_width: u16,
) -> u16 {
    if lines.is_empty() {
        return 0;
    }
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .padding(Padding::horizontal(theme.inner_padding as u16));
    let inner_width = block.inner(Rect::new(0, 0, outer_width.max(1), 1)).width;
    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .line_count(inner_width.max(1)) as u16
}

/// Print `lines` into the terminal's scrollback above the inline viewport.
fn emit<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    lines: Vec<Line<'static>>,
    pad: u16,
    width: u16,
) -> std::io::Result<()> {
    // Height must be the WRAPPED (physical) row count, not the logical line
    // count: `insert_before` renders into a buffer exactly `height` rows tall,
    // and `Wrap` soft-wraps any line wider than the pane into extra rows. Using
    // `lines.len()` clips every wrapped overflow row — a long message loses its
    // tail into the void (never reaches scrollback, so it can't be scrolled
    // back to). `Paragraph::line_count(width)` accounts for wrap + the padding
    // block. (Enabled by the `unstable-rendered-line-info` ratatui feature.)
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
    })
}

/// Flush the OVERFLOW of the live band into the terminal's native scrollback
/// via `Terminal::insert_before`: the oldest settled messages, then complete
/// blocks of a still-streaming reply — and in both cases only as much as it
/// takes for what remains to fit the band.
///
/// Why overflow and not "everything settled": the Inline viewport has a fixed
/// height (see `viewport_h_for`), so flushing eagerly leaves the band blank and
/// the composer sitting that many rows above the screen bottom. Keeping the
/// band full of real content is what glues the composer to the bottom row with
/// no gap, and it means the viewport is never resized — so none of ratatui's
/// re-anchor paths (which can only leak blank rows into scrollback or float the
/// viewport up) ever run.
///
/// A whole message is flushable once it can no longer change: not itself
/// streaming, and not the trailing entry while a turn is in progress. A
/// streaming reply spills block by block instead, so a long answer scrolls into
/// native scrollback as it arrives rather than being trapped in the band.
/// No-op in Fullscreen mode (the overlay reads `app.messages` directly) and
/// while the band still fits.
pub fn flush_finished<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
    viewport_h: u16,
) -> std::io::Result<()> {
    use super::app::RenderMode;
    if app.render_mode != RenderMode::Inline {
        return Ok(());
    }
    let theme = app.theme;
    let pad = theme.inner_padding as u16;
    let width = terminal.size()?.width.max(1);
    let cap = u32::from(band_inner_rows(app, viewport_h));

    let mut skip = effective_skip(app);
    if app.flushed_bytes > 0 && skip == 0 {
        // The committed prefix no longer belongs to the message sitting there;
        // forget it and let that message flush whole. A visible duplicate of
        // the partial text beats splicing a remainder onto the wrong body.
        app.flushed_bytes = 0;
    }

    // ── 1. whole settled messages, oldest first ────────────────────────────
    // Per-message row counts: wrapping is per line and the band draws no
    // separators between messages, so the band total is their sum. Summing
    // once keeps this O(n) instead of re-measuring the whole tail per
    // candidate index (a resize resets `flushed_upto` to 0).
    let start = app.flushed_upto.min(app.messages.len());
    let rows: Vec<u16> = app.messages[start..]
        .iter()
        .enumerate()
        .map(|(n, m)| {
            let mut lines = Vec::new();
            push_live_measured(&mut lines, app, m, if n == 0 { skip } else { 0 });
            band_rows(theme, lines, width)
        })
        .collect();
    let mut total: u32 = rows.iter().map(|r| u32::from(*r)).sum();
    let settled = settle_end(app);
    let mut end = start;
    // Stop BEFORE the message whose departure would leave the band short. A
    // flush is one-way, and a message is flushed whole, so "flush while we
    // overflow" hands a 30-row reply to scrollback and leaves the band empty —
    // the transcript ends mid-screen with a blank slab down to the composer.
    // Keeping it costs nothing: the band tail-follows, so the surplus rows sit
    // off-screen (PageUp reaches them) until later messages push them out for
    // real.
    while end < settled && total > cap && total - u32::from(rows[end - start]) >= cap {
        total -= u32::from(rows[end - start]);
        end += 1;
    }
    if end > start {
        let mut lines: Vec<Line<'static>> = Vec::new();
        for i in start..end {
            let msg_skip = if i == start { skip } else { 0 };
            // Separator between messages — never before a continuation, which
            // resumes a message whose head is already in scrollback.
            if i > 0 && msg_skip == 0 {
                if theme.show_separator {
                    lines.push(Line::styled(
                        "─".repeat(SEPARATOR_WIDTH),
                        Style::default().fg(theme.separator),
                    ));
                } else {
                    lines.push(Line::default());
                }
            }
            push_live(&mut lines, app, &app.messages[i], msg_skip);
        }
        emit(terminal, lines, pad, width)?;
        app.flushed_upto = end;
        app.flushed_bytes = 0;
        skip = 0;
    }

    // ── 2. still overflowing → spill complete blocks of the streaming turn ──
    // One block at a time, re-measuring, so the band keeps painting a full
    // screenful instead of emptying out mid-turn.
    while total > cap {
        let Some(m) = app.messages.get(app.flushed_upto) else {
            break;
        };
        // ponytail: reasoning turns keep the whole-message flush. `thinking`
        // renders above the body, so committing a body block first would strand
        // any reasoning that arrives later after it in scrollback.
        if !m.streaming || m.role != Role::Agent || m.step.is_some() || !m.thinking.is_empty() {
            break;
        }
        let rest = m.text.get(skip..).unwrap_or("");
        let block_end = next_block_end(rest);
        if block_end == 0 {
            break;
        }
        let chunk = &rest[..block_end];
        let mut lines: Vec<Line<'static>> = Vec::new();
        if skip == 0 {
            if app.flushed_upto > 0 {
                if theme.show_separator {
                    lines.push(Line::styled(
                        "─".repeat(SEPARATOR_WIDTH),
                        Style::default().fg(theme.separator),
                    ));
                } else {
                    lines.push(Line::default());
                }
            }
            lines.push(Line::from(Span::styled(
                "● agent".to_string(),
                Style::default()
                    .fg(theme.agent)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        lines.extend(agent_body_lines(chunk, false, app.spinner, theme, None));
        emit(terminal, lines, pad, width)?;
        skip += block_end;
        app.flushed_bytes = skip;
        app.flushed_hash = prefix_hash(&app.messages[app.flushed_upto].text[..skip]);

        let mut live = Vec::new();
        push_live_measured(&mut live, app, &app.messages[app.flushed_upto], skip);
        total = u32::from(band_rows(theme, live, width));
    }
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

/// Right-hand title for the transcript's top border, or `None` when nothing
/// is hidden.
///
/// A band that silently drops the rows above it is indistinguishable from one
/// that never had them — which is exactly how a reply behind the suggested-
/// reply chooser reads as lost. `max_scroll` is the number of rows above the
/// band; `scroll_back` is how many the operator has already walked up.
fn scroll_marker(max_scroll: u16, scroll_back: u16) -> Option<String> {
    if max_scroll == 0 {
        return None;
    }
    Some(if scroll_back == 0 {
        format!(" ↑ {max_scroll} more · PgUp ")
    } else {
        format!(" ↑ {} · PgDn to follow ", max_scroll - scroll_back)
    })
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
    // The head message may be partially committed to scrollback already (a
    // streaming reply spills complete blocks); paint only what follows.
    let skip = effective_skip(app);
    let mut lines: Vec<Line> = Vec::new();
    for (n, m) in app.messages[start..].iter().enumerate() {
        push_live(&mut lines, app, m, if n == 0 { skip } else { 0 });
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

    let block = match scroll_marker(max_scroll, app.scroll_back) {
        Some(marker) => block.title(Line::from(marker).right_aligned()),
        None => block,
    };
    f.render_widget(output.block(block).scroll((offset, 0)), area);
}

/// Header line of an agent turn plus its reasoning block: an animated bullet
/// while streaming, a solid one once done — so an in-progress turn reads as
/// "live" at a glance vs. a finished one. Reasoning stays visible after the
/// turn finishes (D5).
fn push_agent_header(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static super::theme::Theme,
) {
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
}

/// Body lines of an agent turn — no header, no reasoning: raw text plus a
/// trailing spinner while streaming, markdown-rendered once settled.
///
/// `cached` is the markdown rendered once at finish time for the WHOLE message
/// (`ChatMsg::rendered`); pass `None` when rendering a slice of it, so the
/// slice gets its own render instead of the whole reply's.
fn agent_body_lines(
    text: &str,
    streaming: bool,
    spinner: usize,
    theme: &'static super::theme::Theme,
    cached: Option<&Vec<Line<'static>>>,
) -> Vec<Line<'static>> {
    if streaming {
        let mut body: Vec<Line<'static>> = text
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
        body
    } else if let Some(cached) = cached {
        // Finished reply: reuse the markdown rendered once at finish time.
        cached.iter().cloned().map(indent_line).collect()
    } else {
        markdown::render(text)
            .lines
            .into_iter()
            .map(indent_line)
            .collect()
    }
}

fn push_message(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static super::theme::Theme,
    cards_expanded: bool,
    width: u16,
) {
    // Step cards replace role-based rendering entirely for that message.
    if let Some(card) = &m.step {
        lines.extend(super::render_card::card_lines(
            card,
            theme,
            cards_expanded,
            width,
        ));
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
            push_agent_header(lines, m, spinner, theme);
            lines.extend(agent_body_lines(
                &m.text,
                m.streaming,
                spinner,
                theme,
                m.rendered.as_ref(),
            ));
            // Drawn here, not baked into `rendered`: this is the only place
            // that knows the pane width, and it runs every frame, so the card
            // reflows on resize for free.
            if let Some(body) = &m.settlement {
                let inner = width.saturating_sub(u16::from(theme.inner_padding) * 2);
                lines.extend(super::settlement::card_lines(body, theme, inner));
            }
        }
    }
}

/// Separator between status-bar segments.
const FOOTER_SEP: &str = " · ";

/// Below this many columns the status bar drops the steering hint and keeps
/// the numbers.
const STATUS_FULL_MIN_WIDTH: u16 = 100;

/// Longest joined tool-name list the `AUTO:` badge will spell out before it
/// falls back to a bare count. Sized so the badge cannot crowd out the rest of
/// the status bar on a narrow terminal — two typical tool names fit.
const AUTO_NAMES_MAX: usize = 24;

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let (msg, color) = if let Some(req) = &app.hitl {
        // Surface the auto-deny clock: the gate expires approvals after
        // DEFAULT_TIMEOUT (300s). Reuse `created_at` rather than tracking new
        // state; the status bar redraws on each blink deadline so it ticks.
        // The tool name goes here so the operator always knows WHAT they are
        // being asked to approve at any width; the decision keys live in the
        // framed modal / inline row, which is where the user is looking.
        let remaining = crate::hitl::gate::DEFAULT_TIMEOUT
            .as_secs()
            .saturating_sub(req.created_at.elapsed().as_secs());
        (
            format!("⏳ approve {} · auto-deny in {remaining}s", req.tool_name),
            Color::Yellow,
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        // The steering hint is the first thing to drop when the row is tight:
        // it is advice, and everything to its right is state.
        let msg = if area.width >= STATUS_FULL_MIN_WIDTH {
            format!("{spin} generating… · type to steer · Ctrl+C to cancel")
        } else {
            format!("{spin} generating…")
        };
        (msg, theme.agent)
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
    // Auto-approval visibility (#8 / proposal 2). All three auto-approval
    // paths now show a badge, not just the global `auto_approve`:
    //   - `auto_approve`               → ` AUTO `   (every tool, session)
    //   - `session_tool_allow` (N>0)   → ` AUTO:N ` (N tools muted via [a])
    //   - `auto_reads`                 → ` READS `  (read_file auto-approved)
    // Pure display; no behaviour change. Fixes the "AUTO badge vanished in a
    // new session" illusion where `[a]`-muted tools left no visible trace.
    if app.auto_approve {
        spans.push(Span::styled(
            " AUTO ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    } else if !app.session_tool_allow.is_empty() {
        // Name the muted tools when they fit. `AUTO:2` said something was
        // muted but never what, and nothing else would tell you either —
        // so the operator could not check whether a grant they did not mean
        // to make was still in force. `/auto off` revokes them.
        let mut names: Vec<&str> = app.session_tool_allow.iter().map(String::as_str).collect();
        names.sort_unstable();
        let joined = names.join(",");
        let label = if joined.chars().count() <= AUTO_NAMES_MAX {
            format!(" AUTO:{joined} ")
        } else {
            format!(" AUTO:{} ", names.len())
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    if app.auto_reads {
        spans.push(Span::styled(
            " READS ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
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
    //
    // Budgeted against the room actually left on the row. This used to be
    // assembled at full length and handed to the terminal, which clipped the
    // overflow — and what fell off the right edge was the context bar, the one
    // figure here that changes what you do next.
    let timer = app.turn_started.map(|t0| {
        let secs = t0.elapsed().as_secs();
        format!("{}m{:02}s · esc=stop", secs / 60, secs % 60)
    });
    let reserved: usize = spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>()
        + timer
            .as_deref()
            .map_or(0, |t| t.chars().count() + FOOTER_SEP.chars().count());
    let obs = footer_segments(
        app.turn_in,
        app.turn_out,
        app.session_in,
        app.session_out,
        app.ctx_tokens,
        &app.pricing,
        app.budget_usd,
        usize::from(area.width).saturating_sub(reserved + FOOTER_SEP.chars().count()),
    );
    if !obs.is_empty() {
        spans.push(Span::raw(FOOTER_SEP));
        spans.push(Span::styled(obs, Style::default().fg(theme.system)));
    }
    if let Some(t) = timer {
        spans.push(Span::styled(
            format!("{FOOTER_SEP}{t}"),
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

/// Hard-wrap `s` into rows at most `w` display columns wide.
///
/// Character-based rather than word-based: the payload is pretty-printed JSON
/// and shell, where breaking mid-token is honest and dropping the tail is not.
/// Widths come from `unicode_width` so a CJK argument does not overflow the
/// border.
fn wrap_row(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if w == 0 || s.is_empty() {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push(ch);
        used += cw;
    }
    rows.push(cur);
    rows
}

/// Draw the approval modal and return the scroll offset it actually used —
/// `scroll` clamped to the content, so the caller's stored offset cannot run
/// away past the end of a short input.
fn render_hitl(
    f: &mut Frame,
    hitl: &super::stream::HitlRequest,
    grant_confirm: Option<char>,
    composer_empty: bool,
    scroll: u16,
) -> u16 {
    let area = centered_rect(HITL_PCT_X, HITL_PCT_Y, f.area());
    let input = serde_json::to_string_pretty(&hitl.tool_input).unwrap_or_default();
    // Header rows stay pinned: scrolling the body must never carry the tool
    // name off-screen, since "which tool" is half of what is being approved.
    let head = vec![
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
    let row_w = area.width.saturating_sub(2) as usize;
    // Wrap every line and keep every line (#939). This modal exists so a human
    // reads the command before it runs; a destructive suffix past a horizontal
    // cut, or past a `.take(12)`, is exactly what must not be silently dropped.
    let mut body: Vec<Line> = Vec::new();
    for l in input.lines() {
        for row in wrap_row(l, row_w) {
            body.push(Line::styled(row, Style::default().fg(Color::DarkGray)));
        }
    }
    // When a session-wide grant is armed, the modal shows ONLY the confirm
    // instruction: the operator is answering "do you really mean the whole
    // session?", and re-printing the full key row there invites a reflex press.
    let keys = if let Some(c) = grant_confirm {
        let what = if c == 'a' {
            format!("`{}` for this session", hitl.tool_name)
        } else {
            "ALL tools for this session".to_string()
        };
        Line::from(vec![
            Span::styled(
                format!("press [{c}] again"),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" to allow {what} — any other key cancels")),
        ])
    } else {
        Line::from(vec![
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
                "[A]",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" allow all tools (session)    "),
            Span::styled(
                "[n]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" deny / Esc"),
        ])
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" approve tool call ");
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    // The key row is the only part of this modal that is never optional, so it
    // gets its own chunk. Previously it was the last entry in one clipped
    // Paragraph: a wrapped JSON input pushed it out of the box and left the
    // operator staring at a blocking gate with no visible way to answer it.
    //
    // While the composer holds text the `composer_empty` guard (#893) makes
    // y/a/A/n type instead of decide. Say so, and dim the row: advertising a
    // live key that is inert is how an operator ends up hitting the 5-minute
    // auto-deny wondering why nothing responds (#939).
    let keys_inert = !composer_empty && grant_confirm.is_none();
    let keys_text = if keys_inert {
        let dimmed = Line::from(
            keys.spans
                .iter()
                .map(|s| {
                    Span::styled(
                        s.content.clone(),
                        s.style.add_modifier(Modifier::DIM).fg(Color::DarkGray),
                    )
                })
                .collect::<Vec<_>>(),
        );
        Text::from(vec![
            dimmed,
            Line::styled(
                "these keys type while the composer has text — Ctrl+U clears it",
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else {
        Text::from(keys)
    };
    let keys_h = Paragraph::new(keys_text.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner.width.max(1)) as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(keys_h.max(1))])
        .split(inner);

    // Body = pinned header + a scrolled window over the wrapped input. When it
    // does not all fit, the notice reports what is HIDDEN — the old message
    // printed the number of rows kept, so widening the pane made the "hidden"
    // count go up (#939).
    let body_h = chunks[0].height as usize;
    let room = body_h.saturating_sub(head.len());
    let mut lines = head;
    let used_scroll = if body.len() > room && room > 1 {
        let visible = room - 1;
        let above = (scroll as usize).min(body.len() - visible);
        let below = body.len() - visible - above;
        lines.extend(body.into_iter().skip(above).take(visible));
        let note = if above == 0 {
            format!("… {below} more lines — PgDn to scroll")
        } else {
            format!("… {above} above · {below} below — PgUp/PgDn")
        };
        lines.push(Line::styled(note, Style::default().fg(Color::DarkGray)));
        above as u16
    } else {
        lines.extend(body);
        0
    };
    f.render_widget(Paragraph::new(Text::from(lines)), chunks[0]);
    f.render_widget(
        Paragraph::new(keys_text).wrap(Wrap { trim: false }),
        chunks[1],
    );
    used_scroll
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

/// Pure footer formatter: `"{turn}/{sess} tok · {cost} · ctx <bar> N%"`,
/// trimmed to `budget` columns.
///
/// Cost is the SESSION estimate, not the turn's: the turn's usage is zero for
/// the whole time a reply is streaming, so the bar read `$0.000 est` next to a
/// six-figure session token count — which parses as "this was free". When a cap
/// is set the same figure renders as `spent / cap` instead of printing two
/// different costs side by side. `—` when the model is unpriced; the ctx part
/// is omitted when no window is known.
///
/// Segments drop right-to-left as `budget` shrinks — token pair first, then
/// cost — because the context bar is the only number here that changes what
/// you do next.
#[allow(clippy::too_many_arguments)]
fn footer_segments(
    turn_in: u64,
    turn_out: u64,
    sess_in: u64,
    sess_out: u64,
    ctx_tokens: u64,
    pricing: &super::footer::Pricing,
    budget_usd: Option<f64>,
    budget: usize,
) -> String {
    use super::footer::{CTX_BAR_WIDTH, UsageCounts, context_pct, ctx_bar, turn_cost};

    let toks = format!(
        "{}/{} tok",
        fmt_tok(turn_in + turn_out),
        fmt_tok(sess_in + sess_out)
    );

    let spent = turn_cost(
        pricing,
        &UsageCounts {
            input: sess_in,
            output: sess_out,
        },
    );
    let cost = match (spent, budget_usd) {
        (Some(c), Some(cap)) => format!("${c:.2} / ${cap:.2}"),
        (Some(c), None) => format!("${c:.3} est"),
        (None, Some(cap)) => format!("/ ${cap:.2}"),
        (None, None) => "\u{2014}".to_string(), // em dash
    };

    let ctx = match pricing.window {
        Some(w) if w > 0 => {
            let pct = context_pct(ctx_tokens, w);
            format!("ctx {} {}%", ctx_bar(pct, CTX_BAR_WIDTH), pct)
        }
        _ => String::new(),
    };

    for parts in [
        [toks.as_str(), cost.as_str(), ctx.as_str()].as_slice(),
        [cost.as_str(), ctx.as_str()].as_slice(),
        [ctx.as_str()].as_slice(),
    ] {
        let s = parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(FOOTER_SEP);
        if s.chars().count() <= budget {
            return s;
        }
    }
    // Nothing fits: print nothing rather than a mangled half-number.
    String::new()
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
mod block_boundary_tests {
    use super::next_block_end;

    #[test]
    fn commits_one_complete_paragraph_at_a_time() {
        let s = "first para\n\nsecond para\n\nthird";
        let n = next_block_end(s);
        assert_eq!(&s[..n], "first para\n\n");
        // The next call, on the remainder, takes exactly the next block.
        let rest = &s[n..];
        let m = next_block_end(rest);
        assert_eq!(&rest[..m], "second para\n\n");
        // A trailing block with no blank line after it is never committable —
        // it may still grow.
        assert_eq!(next_block_end(&rest[m..]), 0);
    }

    #[test]
    fn a_blank_line_inside_a_fence_is_not_a_boundary() {
        // Committing here would strand an unterminated ``` in scrollback and
        // re-render the fence body once it closes.
        let s = "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\ntail";
        let n = next_block_end(s);
        assert_eq!(&s[..n], "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\n");
        assert_eq!(next_block_end("```\nunclosed\n\nstill inside\n"), 0);
    }

    #[test]
    fn nothing_to_commit_yet() {
        assert_eq!(next_block_end(""), 0);
        assert_eq!(next_block_end("one line, still streaming"), 0);
    }
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

    /// Enough room that these assertions test the formatting, not the trimming.
    const WIDE: usize = 200;

    #[test]
    fn shows_tokens_and_dash_cost_when_unpriced() {
        let s = footer_segments(1240, 0, 1240, 0, 0, &Pricing::default(), None, WIDE);
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
        let s = footer_segments(1000, 1000, 1000, 1000, 32_000, &p, None, WIDE);
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
        let s = footer_segments(1000, 1000, 1000, 1000, 0, &p, Some(20.0), WIDE);
        assert!(s.contains("$18.00 / $20.00"), "got: {s}");
        // unpriced model → spent omitted, cap still shown
        let s2 = footer_segments(
            1000,
            1000,
            1000,
            1000,
            0,
            &Pricing::default(),
            Some(20.0),
            WIDE,
        );
        assert!(s2.contains("/ $20.00"), "got: {s2}");
        assert!(!s2.contains("$18.00"));
    }

    #[test]
    fn cost_is_the_session_not_the_turn() {
        let p = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        // Mid-stream: the turn's usage hasn't been reported yet. The bar used
        // to price the turn, so it read "$0.000 est" beside a session total.
        let s = footer_segments(0, 0, 1000, 1000, 0, &p, None, WIDE);
        assert!(s.contains("$18.000 est"), "got: {s}");
    }

    #[test]
    fn narrow_bar_keeps_the_context_gauge_and_drops_the_rest() {
        let p = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(100_000),
        };
        let full = footer_segments(1000, 1000, 1000, 1000, 91_000, &p, None, WIDE);
        let tight = footer_segments(1000, 1000, 1000, 1000, 91_000, &p, None, 20);
        assert!(full.len() > tight.len());
        assert!(tight.contains("91%"), "got: {tight}");
        assert!(!tight.contains("tok"), "got: {tight}");
        assert!(tight.chars().count() <= 20, "got: {tight}");
    }
}

#[cfg(test)]
mod fleet_rail_layout_tests {
    use super::*;
    use crate::cmd::agent::cli::fleet_rail::{MemberRow, MemberState, RailView};

    fn view(blocked: usize) -> RailView {
        RailView {
            jobs_line: "fleet · dev   job 0/1".into(),
            members: (0..blocked)
                .map(|i| MemberRow {
                    agent: format!("m{i}"),
                    state: MemberState::Blocked {
                        summary: "approve".into(),
                        hitl_id: format!("h{i}"),
                    },
                })
                .collect(),
            notice: None,
        }
    }

    #[test]
    fn rail_is_one_row_until_someone_is_blocked() {
        assert_eq!(rail_height_for(&view(0)), 1);
        assert_eq!(rail_height_for(&view(1)), 2);
        assert_eq!(rail_height_for(&view(3)), 4);
    }

    #[test]
    fn the_expanded_rail_is_capped() {
        use crate::cmd::agent::cli::fleet_rail::MAX_EXPANDED_ROWS;
        assert_eq!(
            rail_height_for(&view(50)),
            // +1 for the member rows (capped), +1 for the "… N more" notice
            // that only appears once the list is actually truncated.
            1 + MAX_EXPANDED_ROWS as u16 + 1,
            "an unbounded rail would eat the transcript, and a truncated one \
             must still show its own truncation notice"
        );
    }

    #[test]
    fn the_live_band_gives_back_exactly_what_the_rail_takes() {
        // The guard for the one dangerous coupling: band_inner_rows decides
        // when transcript content is flushed to scrollback, so it must account
        // for every row the rail paints or the flush drifts from the picture.
        // Tested on the pure arithmetic so no App and no test-only seam in
        // production code are needed.
        let viewport_h = 20u16;
        let input_h = 3u16;
        let without = band_capacity(viewport_h, input_h, 0, 0);
        let with_rail = band_capacity(viewport_h, input_h, 0, rail_height_for(&view(3)));
        assert_eq!(without - with_rail, rail_height_for(&view(3)));
    }

    #[test]
    fn the_rail_height_matches_what_render_actually_paints() {
        // rail_height_for is a number computed independently of rail_lines;
        // nothing ties them together except this test. Without it, a line
        // added to (or removed from) rail_lines silently desyncs the height
        // from the paint — exactly how the truncation notice went missing.
        use crate::cmd::agent::cli::fleet_rail::MAX_EXPANDED_ROWS;
        let theme = crate::cmd::agent::cli::theme::resolve_skin("dark");
        for blocked in [0, 1, MAX_EXPANDED_ROWS, MAX_EXPANDED_ROWS + 3] {
            let v = view(blocked);
            assert_eq!(
                rail_lines(&v, theme).len() as u16,
                rail_height_for(&v),
                "mismatch at {blocked} blocked members"
            );
        }
    }
}

#[cfg(test)]
mod settlement_paint_tests {
    use super::super::app::{ChatMsg, Role};
    use super::super::theme::DARK;
    use super::push_message;

    #[test]
    fn a_carried_settlement_is_painted_after_the_body() {
        let mut m = ChatMsg::for_test(Role::Agent, "did it");
        m.settlement = Some("  ✔ bash · cargo test".into());
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &DARK, false, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text.iter().any(|l| l.contains("SETTLEMENT")),
            "card missing: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("cargo test")),
            "card body missing: {text:?}"
        );
    }

    #[test]
    fn a_message_without_one_paints_nothing_extra() {
        let m = ChatMsg::for_test(Role::Agent, "did it");
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &DARK, false, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(!text.iter().any(|l| l.contains("SETTLEMENT")), "{text:?}");
    }
}

#[cfg(test)]
mod hitl_modal_tests {
    use super::super::stream::HitlRequest;
    use super::render_hitl;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A tool input big enough that, wrapped, it fills the modal on its own.
    fn fat_request() -> HitlRequest {
        HitlRequest {
            hitl_id: "h1".into(),
            step_id: None,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({
                "command": "x".repeat(400),
                "cwd": "/Volumes/Firecuda4tb/Projects/mur",
            }),
            prompt: "Run `bash`?".into(),
            created_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn the_key_row_survives_an_oversized_input() {
        let mut term = Terminal::new(TestBackend::new(88, 24)).unwrap();
        term.draw(|f| {
            render_hitl(f, &fat_request(), None, true, 0);
        })
        .unwrap();
        let dump = term.backend().to_string();
        assert!(
            dump.contains("approve"),
            "the operator cannot answer a gate whose keys are off-screen:\n{dump}"
        );
        assert!(dump.contains("deny"), "{dump}");
    }

    /// #939 §1: the command body must never be cut horizontally. A marker at
    /// the very end of a long single-line command is the thing a destructive
    /// suffix would occupy, so it is what the test looks for.
    #[test]
    fn a_long_command_is_wrapped_not_truncated() {
        let req = HitlRequest {
            tool_input: serde_json::json!({
                "command": format!("git status {} && rm -rf /tmp/DANGER_MARKER", "-".repeat(120)),
            }),
            ..fat_request()
        };
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| {
            render_hitl(f, &req, None, true, 0);
        })
        .unwrap();
        let dump = term.backend().to_string().replace(['\n', ' '], "");
        assert!(
            dump.contains("DANGER_MARKER"),
            "the tail of the command was dropped — that is the whole defect:\n{}",
            term.backend()
        );
    }

    /// #939 §2: the notice counts HIDDEN rows. The old code printed the number
    /// kept, so a taller box reported MORE hidden. Growing the terminal must
    /// make the number go down, never up.
    #[test]
    fn hidden_line_count_falls_as_the_box_grows() {
        let req = HitlRequest {
            tool_input: serde_json::json!({ "command": "echo hi\n".repeat(400) }),
            ..fat_request()
        };
        let hidden_at = |h: u16| -> usize {
            let mut term = Terminal::new(TestBackend::new(100, h)).unwrap();
            term.draw(|f| {
                render_hitl(f, &req, None, true, 0);
            })
            .unwrap();
            let dump = term.backend().to_string();
            let tail = dump.split("… ").nth(1).expect("a residue notice");
            tail.split_whitespace()
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .expect("a numeric hidden count")
        };
        let small = hidden_at(20);
        let large = hidden_at(40);
        assert!(
            large < small,
            "a bigger box hid {large} lines vs {small} in a smaller one — \
             the count is tracking box height, not residual content"
        );
    }

    /// #939 §1+§3: scrolling reaches content that is off-screen at rest, and an
    /// over-large offset is clamped rather than scrolling into blank space.
    #[test]
    fn paging_reveals_the_tail_and_clamps_at_the_end() {
        let req = HitlRequest {
            tool_input: serde_json::json!({ "command": (0..40).map(|i| format!("step{i}")).collect::<Vec<_>>().join("\n") }),
            ..fat_request()
        };
        let draw = |scroll: u16| {
            let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
            term.draw(|f| {
                render_hitl(f, &req, None, true, scroll);
            })
            .unwrap();
            term.backend().to_string()
        };
        assert!(
            !draw(0).contains("step39"),
            "sanity: the tail starts hidden"
        );
        assert!(
            draw(200).contains("step39"),
            "an offset past the end must clamp to the last page, not blank the body"
        );
    }

    /// #939 §3: while the composer holds text the decision keys type instead of
    /// deciding, so the modal must say so rather than advertising live keys.
    #[test]
    fn a_nonempty_composer_is_announced_on_the_key_row() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| {
            render_hitl(f, &fat_request(), None, false, 0);
        })
        .unwrap();
        let dump = term.backend().to_string().replace('\n', " ");
        assert!(
            dump.contains("Ctrl+U"),
            "the operator gets no hint that y/a/A/n are inert:\n{dump}"
        );
    }
}

#[cfg(test)]
mod chooser_floor_tests {
    use super::super::app::App;
    use super::super::complete::{Candidate, CompletionState};
    use super::chooser_band_height;

    fn option(display: &str, desc: &str) -> Candidate {
        Candidate {
            display: display.into(),
            insert: display.into(),
            desc: desc.into(),
            has_children: false,
        }
    }

    /// Three suggested replies, each with a description — the shape from the
    /// report.
    fn app_with_three_options() -> App {
        let mut a = App::test_fixture();
        a.completion = Some(CompletionState {
            items: vec![
                option("open the PR", "wait for CI, then tag"),
                option("stronger model", "the 4B one fakes tool calls"),
                option("leave it", "change nothing"),
            ],
            selected: 0,
            spaced: true,
        });
        a
    }

    #[test]
    fn the_chooser_leaves_the_transcript_more_than_three_rows() {
        // Inline viewport is 20 rows; composer 3 + status 1 leaves 16.
        let h = chooser_band_height(&app_with_three_options(), 20, 3);
        assert!(
            h <= 8,
            "chooser took {h} rows, leaving the reply a peephole"
        );
    }

    #[test]
    fn a_short_terminal_still_gets_a_usable_chooser() {
        // The floor must yield rather than squeeze the chooser out: it is the
        // thing the operator has to act on.
        let h = chooser_band_height(&app_with_three_options(), 12, 3);
        assert!(h >= 5, "chooser unusable at {h} rows");
    }

    #[test]
    fn ctrl_up_still_reaches_the_spaced_form() {
        let mut a = app_with_three_options();
        a.chooser_grow = 6;
        assert_eq!(chooser_band_height(&a, 20, 3), 11);
    }
}

#[cfg(test)]
mod scroll_marker_tests {
    use super::scroll_marker;

    #[test]
    fn silence_when_everything_fits() {
        assert_eq!(scroll_marker(0, 0), None);
    }

    #[test]
    fn following_the_tail_points_up() {
        assert_eq!(scroll_marker(25, 0).as_deref(), Some(" ↑ 25 more · PgUp "));
    }

    #[test]
    fn scrolled_back_points_the_way_home() {
        assert_eq!(
            scroll_marker(25, 10).as_deref(),
            Some(" ↑ 15 · PgDn to follow ")
        );
    }
}
