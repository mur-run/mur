//! A resize that lands between our own size check and ratatui's `draw` lets
//! `autoresize` issue a cursor-position query while the `EventStream` still
//! owns stdin. The reply is swallowed by the event reader, crossterm times out
//! after ~2s, and the `?` on `draw` / `flush_finished` used to take the whole
//! session down. Those two calls must be fail-open — rebuild, not exit — the
//! same way `rebuild_after_resize` and `purge_and_reanchor` already are.
use super::super::term::{DrawFault, classify_paint_error};
use std::io;

/// crossterm 0.28 `cursor::position()` timeout, verbatim.
fn cursor_timeout() -> anyhow::Error {
    anyhow::Error::new(io::Error::other(
        "The cursor position could not be read within a normal duration",
    ))
}

#[test]
fn cursor_timeout_is_recoverable() {
    assert_eq!(classify_paint_error(&cursor_timeout()), DrawFault::Recover);
}

#[test]
fn cursor_timeout_survives_context_wrapping() {
    // `draw` errors reach the loop wrapped by anyhow context in places.
    let wrapped = cursor_timeout().context("draw frame");
    assert_eq!(classify_paint_error(&wrapped), DrawFault::Recover);
}

#[test]
fn a_broken_pipe_still_kills_the_session() {
    let gone = anyhow::Error::new(io::Error::from(io::ErrorKind::BrokenPipe));
    assert_eq!(classify_paint_error(&gone), DrawFault::Fatal);
}

/// A failed viewport rebuild must not fall through to a paint: `paint` opens
/// with `flush_finished`, whose `insert_before` measures against the
/// pre-resize terminal and height (#2020).
mod stale_geometry_gate {
    use super::super::super::app::{App, RenderMode};
    use super::super::super::events::{paint_gated, rebuild_pending};
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    const COLS: u16 = 80;
    const ROWS: u16 = 40;
    const VIEWPORT: u16 = 10;

    /// An app whose streaming reply is long enough that `flush_finished` has
    /// something to spill into scrollback.
    fn setup() -> (App, Terminal<TestBackend>) {
        let mut app = App::test_fixture();
        app.render_mode = RenderMode::Inline;
        app.width = COLS;
        app.begin_user_turn("q");
        for i in 0..60 {
            app.append_delta(&format!("line {i:02}\n\n"), false);
        }
        let term = Terminal::with_options(
            TestBackend::new(COLS, ROWS),
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT),
            },
        )
        .unwrap();
        (app, term)
    }

    #[test]
    fn a_pending_rebuild_cannot_reach_paint() {
        let (mut app, mut term) = setup();
        let mut force_rebuild = true; // what `Err(_)` from the rebuild sets
        paint_gated(&mut term, &mut app, VIEWPORT, &mut force_rebuild).unwrap();
        assert_eq!(app.flushed_bytes, 0, "flush_finished ran on stale geometry");
        assert!(
            term.backend().scrollback().content.is_empty(),
            "insert_before ran on stale geometry"
        );
        assert!(force_rebuild, "the owed rebuild must stay owed");
    }

    #[test]
    fn once_the_rebuild_succeeds_the_same_state_paints() {
        let (mut app, mut term) = setup();
        let mut force_rebuild = false;
        paint_gated(&mut term, &mut app, VIEWPORT, &mut force_rebuild).unwrap();
        assert!(app.flushed_bytes > 0, "control: the fixture must spill");
        assert!(!force_rebuild);
    }

    #[test]
    fn fullscreen_never_waits_on_a_rebuild() {
        let (mut app, _term) = setup();
        app.render_mode = RenderMode::Fullscreen;
        assert!(!rebuild_pending(&app, true));
    }
}
