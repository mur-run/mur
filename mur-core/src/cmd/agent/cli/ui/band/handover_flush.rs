//! Commit the whole settled transcript to scrollback before a terminal
//! handover (#2356).
//!
//! `flush_finished` only flushes the band's OVERFLOW, so the "handing over the
//! terminal…" notice normally stays painted inside the Inline viewport. A
//! handover then clears that viewport (`Suspended::begin` clears from its top
//! row down) and the child writes over the same rows. The notice survived
//! only in `app.messages`, and was painted again after the child exited, below
//! whatever the child had printed — so a failing command's own error appeared
//! above the line that announced it.
//!
//! Flushing everything settled first puts the notice (and the transcript above
//! it) into native scrollback, where the clear cannot reach it, ahead of the
//! child's output. This advances the flush cursor; it never rewinds it (see
//! `prepare_handover` for why a rewind would duplicate the transcript).

use ratatui::backend::Backend;
use ratatui::text::Line;

use super::super::super::app::{App, RenderMode};
use super::{effective_skip, emit, message_block, settle_end};

/// Emit `messages[flushed_upto..settle_end]` into scrollback. No-op outside
/// Inline mode and when nothing settled is left in the band.
pub fn flush_settled<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    app: &mut App,
) -> std::io::Result<()> {
    if app.render_mode != RenderMode::Inline {
        return Ok(());
    }
    let skip = effective_skip(app);
    if app.flushed_bytes > 0 && skip == 0 {
        // Same rule as `flush_finished`: the committed prefix no longer
        // belongs to that message, so it flushes whole.
        app.flushed_bytes = 0;
        app.flushed_raw = false;
    }
    let start = app.flushed_upto.min(app.messages.len());
    let end = settle_end(app);
    if end <= start {
        return Ok(());
    }
    let width = terminal.size()?.width.max(1);
    let pad = app.theme.inner_padding as u16;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for i in start..end {
        let msg_skip = if i == start { skip } else { 0 };
        lines.extend(message_block(app, i, &app.messages[i], msg_skip, false));
    }
    emit(terminal, lines, pad, width)?;
    app.flushed_upto = end;
    app.flushed_bytes = 0;
    app.flushed_raw = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::flush_finished;
    use super::flush_settled;
    use crate::cmd::agent::cli::app::{App, ChatMsg, RenderMode, Role};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    const VIEWPORT: u16 = 8;
    const NOTICE: &str = "!false: handing over the terminal…";

    fn inline_app_and_term() -> (App, Terminal<TestBackend>) {
        let mut app = App::test_fixture();
        app.messages.clear();
        app.render_mode = RenderMode::Inline;
        app.width = 80;
        let term = Terminal::with_options(
            TestBackend::new(80, 24),
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT),
            },
        )
        .unwrap();
        (app, term)
    }

    fn text_of(buf: &Buffer) -> String {
        buf.content.iter().map(|c| c.symbol()).collect()
    }

    /// Everything the suspend's `Clear(FromCursorDown)` cannot reach: native
    /// scrollback plus the screen rows above the Inline viewport. (The
    /// TestBackend anchors the viewport at row 0, so `insert_before` first
    /// pushes it down instead of scrolling; both places survive the clear.)
    fn outside_viewport(term: &mut Terminal<TestBackend>) -> String {
        let top = term.get_frame().area().y;
        let b = term.backend();
        let buf = b.buffer();
        let w = usize::from(buf.area.width);
        let above: String = buf.content[..usize::from(top) * w]
            .iter()
            .map(|c| c.symbol())
            .collect();
        text_of(b.scrollback()) + &above
    }

    /// The bug: the band only flushes overflow, so a short notice stays in
    /// the viewport the suspend clears. It must be in scrollback instead.
    #[test]
    fn the_handover_notice_reaches_scrollback_before_the_suspend() {
        let (mut app, mut term) = inline_app_and_term();
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.push_system(NOTICE);

        // Negative control: the ordinary flush leaves a band that fits alone.
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        assert!(!outside_viewport(&mut term).contains("handing over"));

        flush_settled(&mut term, &mut app).unwrap();
        assert!(
            outside_viewport(&mut term).contains("handing over"),
            "notice still only in the viewport the suspend clears"
        );
        assert_eq!(app.flushed_upto, app.messages.len());
    }

    /// Advancing only: a second call has nothing left and must not re-emit
    /// the transcript (the duplicate `prepare_handover` warns about).
    #[test]
    fn a_second_flush_emits_nothing() {
        let (mut app, mut term) = inline_app_and_term();
        app.push_system(NOTICE);
        flush_settled(&mut term, &mut app).unwrap();
        let before = outside_viewport(&mut term);
        flush_settled(&mut term, &mut app).unwrap();
        assert_eq!(outside_viewport(&mut term), before);
    }

    /// A still-streaming reply is not settled and stays in the band.
    #[test]
    fn a_streaming_turn_is_left_in_the_band() {
        let (mut app, mut term) = inline_app_and_term();
        app.push_system(NOTICE);
        app.messages.push(ChatMsg::for_test(Role::Agent, "partial"));
        app.streaming = true;
        flush_settled(&mut term, &mut app).unwrap();
        assert_eq!(app.flushed_upto, app.messages.len() - 1);
    }
}
