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
