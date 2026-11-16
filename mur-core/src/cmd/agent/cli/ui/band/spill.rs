//! Spill of the live band's overflow into native scrollback: whole settled
//! messages first, then complete blocks of a still-streaming reply.

use super::*;

/// How far past the overflow a whole-block spill may reach: the band must stay
/// at least half full afterwards. Below that, sending the block whole is
/// cheaper than tearing it (the block keeps its markdown in scrollback and the
/// band refills within a few deltas); above it, the empty rows are the reply's
/// head pushed off a short pane (#2020). Proportional to the band, not a row
/// count, so a 5-row pane and a 20-row one get the same guarantee.
fn spill_slack(cap: u32) -> u32 {
    cap / 2
}

/// Byte length of the fewest leading complete lines of `rest` whose raw rows
/// cover `need`, or 0 when `rest` has no complete line. Lines are never split:
/// the band paints raw text line by line, so a line is the smallest unit that
/// moves to scrollback without changing shape.
fn raw_lines_for(
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    rest: &str,
    need: u32,
    width: u16,
) -> usize {
    let cols = crate::cmd::agent::cli::markdown::body_cols(width, theme.inner_padding);
    let mut end = 0usize;
    let mut rows = 0u32;
    for line in rest.split_inclusive('\n') {
        if rows >= need || !line.ends_with('\n') {
            break;
        }
        end += line.len();
        rows += u32::from(band_rows(theme, raw_body_lines(line, cols), width));
    }
    end
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
        app.flushed_thinking = 0;
        app.flushed_raw = false;
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
        .map(|(n, m)| live_rows(app, start + n, m, if n == 0 { skip } else { 0 }, width))
        .collect();
    let mut total: u32 = rows.iter().map(|r| u32::from(*r)).sum();
    welcome_head::flush(terminal, app, total, cap, width)?;
    let settled = settle_end(app);
    let mut end = start;
    // Flush until what remains fits, even when that leaves the band short. The
    // alternative — keep the message whose departure would leave a blank slab,
    // and let the band hide its surplus rows above the fold — traded a few
    // empty rows for a reply the reader could not see ("↑ 7 more · PgUp" over
    // the one answer they were asked to act on). A flushed reply sits directly
    // above the band, on screen; a hidden one is gone until they page for it.
    //
    // Exception: the LAST settled message is the reply the chooser is asking
    // about right now — that is exactly the "one answer they were asked to
    // act on" from the comment above, and it must never be the thing that
    // gets pushed off into native scrollback with no on-screen trace. Stop
    // one short of it whenever there is another, older message to sacrifice
    // instead; only flush into it when it is the sole settled message left
    // (start == settled - 1), in which case there is nothing else to give up.
    let protect_last = !app.streaming && settled > start + 1;
    let stop_at = if protect_last {
        settled.saturating_sub(1).max(start)
    } else {
        settled
    };
    while end < stop_at && total > cap {
        total -= u32::from(rows[end - start]);
        end += 1;
    }
    // Reaching `stop_at` still overflowing means every older message has gone
    // and the protected reply is now the sole settled one left — the exception
    // above, arrived at within this same call. It used to be reached only on
    // the NEXT call (by then `start == settled - 1` made `protect_last` false),
    // and the frame drawn in between is the bug: one paint of the reply the
    // chooser is asking about, hidden behind "↑ N more · PgUp". A fixpoint
    // reached a frame late is a fixpoint the operator sees broken.
    if protect_last && end == stop_at && end < settled && total > cap {
        total -= u32::from(rows[end - start]);
        end += 1;
    }
    if end > start {
        // Replay ceiling. A resize resets `flushed_upto` to 0 (the purge wiped
        // scrollback), so this one call would otherwise re-emit the WHOLE
        // transcript — on a long session that is tens of thousands of rows of
        // re-wrapping and re-drawing per resize event, which is what turned a
        // window drag into a freeze. Past the ceiling the oldest messages are
        // dropped from the replay rather than re-printed: they are already
        // gone from the screen, the reader cannot scroll to rows that were
        // purged, and the conversation log on disk is untouched.
        let mut first = start;
        let mut replay: u32 = (start..end).map(|i| u32::from(rows[i - start])).sum();
        while first < end && replay > REPLAY_ROWS_MAX {
            replay -= u32::from(rows[first - start]);
            first += 1;
        }
        let mut lines: Vec<Line<'static>> = Vec::new();
        if first > start {
            lines.push(Line::from(Span::styled(
                format!("… {} earlier messages not replayed", first - start),
                theme.muted,
            )));
            lines.push(Line::default());
        }
        for i in first..end {
            let msg_skip = if i == start { skip } else { 0 };
            lines.extend(message_block(app, i, &app.messages[i], msg_skip, false));
        }
        emit(terminal, lines, pad, width)?;
        app.flushed_upto = end;
        app.flushed_bytes = 0;
        app.flushed_thinking = 0;
        app.flushed_raw = false;
        skip = 0;
    }

    // ── 2. still overflowing → spill complete blocks of the streaming turn ──
    // One block at a time, re-measuring, so the band keeps painting a full
    // screenful instead of emptying out mid-turn.
    // While the committed prefix ends mid-block, the rest of THAT block stays
    // raw: rendered as markdown from mid-table it would open with a torn
    // table. Once a spill reaches a real boundary again the latch clears, so
    // later headings and tables go up rendered (#1649).
    let mut raw = app.flushed_raw;
    while total > cap {
        let Some(m) = app.messages.get(app.flushed_upto) else {
            break;
        };
        // Reasoning turns spill too; refusing them hid the reply behind PgUp.
        if !m.streaming || m.role != Role::Agent || m.step.is_some() {
            break;
        }
        let rest = m.text.get(skip..).unwrap_or("");
        let need = total - cap;
        let closed = next_boundary_after(&m.text, skip).map_or(0, |b| b - skip);
        // A spill sends either one whole block, rendered, or just enough of a
        // block's complete lines, raw, to clear the overflow. Whole and
        // rendered is the default — it is what keeps scrollback styled. The
        // raw cut is for a block so tall that sending it whole would leave
        // the band mostly empty: a 14-row fence spilled to clear a 1-row
        // overflow left the band 13 rows short, and on a 30-row split pane
        // those rows were the reply's head, pushed off the top of the screen
        // (#2020). Raw costs nothing on screen — a streaming turn paints raw
        // text, so nothing changes shape, only where it lives — and only the
        // torn block stays raw once the turn settles.
        let (block_end, as_raw) = if closed == 0 {
            // One block taller than the band (a long table or fence has no
            // blank line inside it): waiting for it to close hides its head
            // behind "↑ N more" for as long as it streams. Commit complete
            // lines instead.
            let complete = rest.rfind('\n').map_or(0, |i| i + 1);
            (raw_lines_for(theme, &rest[..complete], need, width), true)
        } else if raw {
            // Still inside a torn block: the rest of it is raw either way.
            (raw_lines_for(theme, &rest[..closed], need, width), true)
        } else {
            let mut probe =
                agent_body_lines(&rest[..closed], false, app.spinner, theme, None, width);
            probe.push(Line::default());
            if u32::from(band_rows(theme, probe, width)) <= need + spill_slack(cap) {
                (closed, false)
            } else {
                (raw_lines_for(theme, &rest[..closed], need, width), true)
            }
        };
        if block_end == 0 {
            break;
        }
        raw = as_raw;
        let chunk = &rest[..block_end];
        let mut lines: Vec<Line<'static>> = Vec::new();
        if skip == 0 {
            if app.flushed_upto > 0 {
                // This path only ever emits an agent turn's own body, which
                // always opens a turn, but
                // the row itself comes from the one builder.
                lines.push(gap_row(theme, app.messages.get(app.flushed_upto - 1), m));
            }
            lines.push(Line::from(Span::styled(
                format!("● {}", app.agent_label),
                theme.accent.add_modifier(Modifier::BOLD),
            )));
        }
        // Reasoning can keep arriving after the body began spilling; commit
        // whatever of it has not gone up yet, ahead of this block.
        super::super::message::push_thinking(&mut lines, unflushed_thinking(app, m, skip), theme);
        if raw {
            lines.extend(raw_body_lines(
                chunk,
                crate::cmd::agent::cli::markdown::body_cols(width, theme.inner_padding),
            ));
        } else {
            lines.extend(agent_body_lines(
                chunk,
                false,
                app.spinner,
                theme,
                None,
                width,
            ));
            // The block ended at a blank line and the renderer trims its own
            // trailing blank; put the separator back, or every paragraph a
            // streaming reply spilled sat flush against the next.
            lines.push(Line::default());
        }
        emit(terminal, lines, pad, width)?;
        skip += block_end;
        app.flushed_bytes = skip;
        app.flushed_text = app.messages[app.flushed_upto].text[..skip].to_string();
        app.flushed_thinking = app.messages[app.flushed_upto].thinking.len();
        raw = !is_block_boundary(&app.messages[app.flushed_upto].text, skip);
        app.flushed_raw = raw;

        total = u32::from(live_rows(
            app,
            app.flushed_upto,
            &app.messages[app.flushed_upto],
            skip,
            width,
        ));
    }
    Ok(())
}
