//! The live transcript band: what is still painted in the viewport, and the
//! flush that pushes its overflow into the terminal's own scrollback.

use super::INPUT_H_MIN;
use super::chooser::chooser_band_height;
use super::message::{
    agent_body_lines, gap_row, push_agent_header, push_message, wants_gap_before,
};
use super::rail::fleet_rail_height;
use ratatui::Frame;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Padding, Paragraph, Widget, Wrap};

use std::time::Instant;

use super::super::app::{App, ChatMsg, Role};
use super::super::welcome::welcome_lines;

#[cfg(test)]
mod tests;

/// Rows left for the live transcript band inside `viewport_h` once the
/// composer, status line, chooser band and fleet rail have taken theirs. The
/// band draws no border of its own (see `render_transcript`), so this is the
/// exact row count it paints into.
pub(super) fn band_capacity(viewport_h: u16, input_h: u16, chooser_h: u16, rail_h: u16) -> u16 {
    viewport_h.saturating_sub(input_h + 1 + chooser_h + rail_h)
}

/// Rows the live transcript band may KEEP inside a viewport of `viewport_h`.
///
/// A grown composer is not counted: it is temporary, the operator is typing
/// rather than reading, and rows flushed to fit it would leave a hole above
/// the composer once it shrinks back. The chooser band IS counted, although
/// it is temporary too: the operator must read the reply to choose, and a
/// band that keeps rows it cannot show hides exactly that reply behind a
/// "↑ 7 more" marker. Flushed rows land directly above the band, still on
/// screen, so pushing the reply up costs a few blank rows after the pick
/// and hiding it costs the reply. The rail stays for the session and takes
/// its rows outright.
pub(super) fn band_inner_rows(app: &App, viewport_h: u16) -> u16 {
    let chooser_h = chooser_band_height(app, viewport_h, INPUT_H_MIN);
    band_capacity(viewport_h, INPUT_H_MIN, chooser_h, fleet_rail_height(app))
}

/// Index one past the last message that is settled AND therefore flushable:
/// everything before the still-streaming turn.
pub(super) fn settle_end(app: &App) -> usize {
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

pub(super) fn prefix_hash(s: &str) -> u64 {
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
pub(super) fn effective_skip(app: &App) -> usize {
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
pub(super) fn next_block_end(rest: &str) -> usize {
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
pub(super) fn push_live(lines: &mut Vec<Line<'static>>, app: &App, m: &ChatMsg, skip: usize) {
    push_live_inner(lines, app, m, skip, false)
}

/// The welcome while it is still the head of the live band: painted above
/// the first message by `render_transcript`, and committed to scrollback
/// with it by `flush_finished`. `None` once that head has been flushed (see
/// [`App::welcome_header_live`]). Ends in a blank when there are messages
/// under it, so the hint line and the first message do not touch.
pub(super) fn welcome_header(app: &App, eye_open: bool) -> Option<Vec<Line<'static>>> {
    if !app.welcome_header_live() {
        return None;
    }
    let mut lines = welcome_lines(
        app.theme,
        app.mascot_mode,
        &app.agent,
        app.cwd.as_deref(),
        eye_open,
    );
    if !app.messages.is_empty() {
        lines.push(Line::default());
    }
    Some(lines)
}

/// Lines for one message, including the gap that precedes it.
///
/// One function for all three callers — the viewport, the scrollback emit, and
/// the row measurement that decides where to cut between them. They disagreed
/// in two ways that both showed:
///
/// * the viewport drew no gaps at all, so the same transcript was dense on
///   screen and spaced once it scrolled up — the reader's spatial map changed
///   with no action from the reader;
/// * the measurement summed message rows without the gap rows, so the band was
///   taller than the number the flush decision was made from.
///
/// Attributing each gap to the message it precedes is what lets one function
/// serve both: a measured block and a rendered block are the same rows.
pub(super) fn message_block(
    app: &App,
    idx: usize,
    m: &crate::cmd::agent::cli::app::ChatMsg,
    skip: usize,
    measured: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Never before a continuation: `skip > 0` resumes a message whose head is
    // already committed above.
    if idx > 0 && skip == 0 && wants_gap_before(m) {
        lines.push(gap_row(app.theme, app.messages.get(idx - 1), m));
    }
    if measured {
        push_live_measured(&mut lines, app, m, skip);
    } else {
        push_live(&mut lines, app, m, skip);
    }
    lines
}

pub(super) fn push_live_measured(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    m: &ChatMsg,
    skip: usize,
) {
    push_live_inner(lines, app, m, skip, true)
}

pub(super) fn push_live_inner(
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
        app.width,
    ));
}

/// Wrapped (physical) row count of `lines` inside the band, measured exactly
/// the way `render_transcript` paints them (`Paragraph::line_count`, not
/// `lines.len()`), so flush decisions stay in lock-step with the band.
/// `outer_width` is the full pane width, before the border block trims it.
pub(super) fn band_rows(
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    lines: Vec<Line<'static>>,
    outer_width: u16,
) -> u16 {
    if lines.is_empty() {
        return 0;
    }
    let block = Block::default().padding(Padding::horizontal(theme.inner_padding as u16));
    let inner_width = block.inner(Rect::new(0, 0, outer_width.max(1), 1)).width;
    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .line_count(inner_width.max(1)) as u16
}

/// Print `lines` into the terminal's scrollback above the inline viewport.
pub(super) fn emit<B: Backend>(
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
    use crate::cmd::agent::cli::app::RenderMode;
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
    let mut rows: Vec<u16> = app.messages[start..]
        .iter()
        .enumerate()
        .map(|(n, m)| {
            let lines = message_block(app, start + n, m, if n == 0 { skip } else { 0 }, true);
            band_rows(theme, lines, width)
        })
        .collect();
    // The welcome is a header over the first notice, never a message of its
    // own: it is measured with message 0 and leaves with it, so the band the
    // flush decision was made from is the band that was painted.
    let welcome = welcome_header(app, true);
    if let Some(r) = rows.first_mut()
        && let Some(w) = welcome.as_ref()
    {
        *r = r.saturating_add(band_rows(theme, w.clone(), width));
    }
    let mut total: u32 = rows.iter().map(|r| u32::from(*r)).sum();
    let settled = settle_end(app);
    let mut end = start;
    // Flush until what remains fits, even when that leaves the band short. The
    // alternative — keep the message whose departure would leave a blank slab,
    // and let the band hide its surplus rows above the fold — traded a few
    // empty rows for a reply the reader could not see ("↑ 7 more · PgUp" over
    // the one answer they were asked to act on). A flushed reply sits directly
    // above the band, on screen; a hidden one is gone until they page for it.
    while end < settled && total > cap {
        total -= u32::from(rows[end - start]);
        end += 1;
    }
    if end > start {
        let mut lines: Vec<Line<'static>> = welcome.unwrap_or_default();
        for i in start..end {
            let msg_skip = if i == start { skip } else { 0 };
            lines.extend(message_block(app, i, &app.messages[i], msg_skip, false));
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
                // This path only ever emits an agent turn's own body, which
                // always opens a turn — no `wants_gap_before` test needed, but
                // the row itself comes from the one builder.
                lines.push(gap_row(theme, app.messages.get(app.flushed_upto - 1), m));
            }
            lines.push(Line::from(Span::styled(
                "● agent".to_string(),
                theme.accent.add_modifier(Modifier::BOLD),
            )));
        }
        lines.extend(agent_body_lines(
            chunk,
            false,
            app.spinner,
            theme,
            None,
            width,
        ));
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
pub(super) fn blank_wide_char_continuations(buf: &mut ratatui::buffer::Buffer) {
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
pub(super) fn scroll_marker(max_scroll: u16, scroll_back: u16) -> Option<String> {
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
pub(super) fn render_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    // No border at all.
    //
    // The name was already in the status-bar badge, so a titled frame spent a
    // full-width rule restating it, and the bottom rule marked the same seam
    // as the composer's own titled top border beneath it. The top rule went
    // last: it only ever hosted the scroll marker, and on a screen already
    // ruled between speakers and around the composer it read as one line too
    // many. The marker now paints itself on the band's first row, right-
    // aligned, when — and only when — rows are hidden above.
    let block = Block::default().padding(Padding::horizontal(theme.inner_padding as u16));
    let inner = block.inner(area);
    let inner_width = inner.width;

    let start = app.flushed_upto.min(app.messages.len());
    // The head message may be partially committed to scrollback already (a
    // streaming reply spills complete blocks); paint only what follows.
    let skip = effective_skip(app);
    let mut lines: Vec<Line> = Vec::new();
    for (n, m) in app.messages[start..].iter().enumerate() {
        lines.extend(message_block(
            app,
            start + n,
            m,
            if n == 0 { skip } else { 0 },
            false,
        ));
    }

    // No conversation yet → progressive-disclosure welcome (mascot + identity
    // + one example + /help hint) above whatever notices slash commands have
    // produced, instead of a bare prompt. The eye frame is a pure function of
    // wall-clock time; the event loop schedules redraws on the blink deadline
    // so an idle welcome animates without busy-looping.
    if let Some(mut head) = welcome_header(app, app.blink.eye_open(Instant::now())) {
        head.append(&mut lines);
        lines = head;
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
    if let Some(marker) = scroll_marker(max_scroll, app.scroll_back) {
        let row = Rect { height: 1, ..inner };
        f.render_widget(Line::styled(marker, theme.muted).right_aligned(), row);
    }
}
