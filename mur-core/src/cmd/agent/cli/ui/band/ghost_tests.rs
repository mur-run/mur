//! A reply must reach scrollback exactly once, and a long one must not hide
//! its head while it streams.
//!
//! The terminal here is a real screen size, so `insert_before` actually
//! scrolls rows into the backend's scrollback and a doubled flush shows up as
//! a doubled row — the shape of the "new table drawn over the old reply"
//! report. A 600-row test screen never scrolls and cannot see it.

use super::super::render;
use super::{effective_skip, flush_finished, relocate_prefix};
use crate::cmd::agent::cli::app::{App, ChatMsg, RenderMode, Role};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const COLS: u16 = 80;
const ROWS: u16 = 40;
const VIEWPORT: u16 = 20;

fn table(rows: usize) -> String {
    let mut s = String::from("| id | item |\n|---|---|\n");
    for i in 0..rows {
        s.push_str(&format!("| r{i:02} | row item {i:02} |\n"));
    }
    s
}

fn setup() -> (App, Terminal<TestBackend>) {
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = COLS;
    let term = Terminal::with_options(
        TestBackend::new(COLS, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT),
        },
    )
    .unwrap();
    (app, term)
}

fn paint(app: &mut App, term: &mut Terminal<TestBackend>) {
    flush_finished(term, app, VIEWPORT).unwrap();
    term.draw(|f| render(f, app)).unwrap();
}

/// Scrollback, then screen, as text.
fn everything(term: &Terminal<TestBackend>) -> String {
    let b = term.backend();
    let mut out = String::new();
    for buf in [b.scrollback(), b.buffer()] {
        let w = usize::from(buf.area.width).max(1);
        for row in buf.content.chunks(w) {
            let l: String = row.iter().map(|c| c.symbol()).collect();
            out.push_str(l.trim_end());
            out.push('\n');
        }
    }
    out
}

fn stream(app: &mut App, term: &mut Terminal<TestBackend>, text: &str) -> usize {
    let mut hidden = 0;
    for chunk in text.split_inclusive('\n') {
        app.append_delta(chunk, false);
        paint(app, term);
        if term.backend().to_string().contains("PgUp") {
            hidden += 1;
        }
    }
    hidden
}

/// The report itself: two model calls streamed into one bubble with no blank
/// line between them, and the reply joined them with one. The committed head
/// no longer matched byte-for-byte, so the whole reply was flushed again
/// beneath the copy already in scrollback.
#[test]
fn a_reply_that_only_respaces_the_stream_is_not_printed_twice() {
    let (mut app, mut term) = setup();
    app.begin_user_turn("q");
    // Model output rarely ends on a newline; the next call's first token then
    // lands on the same line as the table's last row.
    let first = format!("first draft\n\n{}", table(30).trim_end());
    // Long enough that the line the two calls were glued on is itself
    // pushed up to scrollback before the reply arrives.
    let second = format!(
        "second draft\n\n{}\nclosing line\n",
        table(30).replace("row item", "later item")
    );
    stream(&mut app, &mut term, &first);
    stream(&mut app, &mut term, &second);
    assert!(
        app.flushed_bytes > 0,
        "the table must have spilled mid-stream"
    );

    let reply = format!("{}\n\n{}", first.trim_end(), second.trim_end());
    app.finish_agent_turn(reply, None);
    paint(&mut app, &mut term);
    paint(&mut app, &mut term);

    let all = everything(&term);
    for needle in [
        "first draft",
        "row item 00",
        "row item 29",
        "second draft",
        "later item 29",
        "closing line",
    ] {
        assert_eq!(all.matches(needle).count(), 1, "{needle:?} once:\n{all}");
    }
}

/// A table taller than the band has no blank line inside it, so block-wise
/// spilling had nothing to commit until it closed: its head sat behind
/// "↑ N more · PgUp" for the whole stream.
#[test]
fn a_table_taller_than_the_band_does_not_hide_its_head_while_streaming() {
    let (mut app, mut term) = setup();
    app.begin_user_turn("q");
    let text = format!(
        "intro\n\n{}\n```\nmur open\n```\n\nclosing line\n",
        table(40)
    );
    let hidden = stream(&mut app, &mut term, &text);
    assert_eq!(hidden, 0, "frames with rows hidden above the band");

    app.finish_agent_turn(text.clone(), None);
    paint(&mut app, &mut term);
    let all = everything(&term);
    for needle in ["intro", "r00", "r39", "mur open", "closing line"] {
        assert_eq!(all.matches(needle).count(), 1, "{needle:?} once:\n{all}");
    }
    assert!(!term.backend().to_string().contains("PgUp"));
}

/// A reply that genuinely replaced what streamed still flushes whole — the
/// relocation must not claim a head it does not share.
#[test]
fn a_rewritten_reply_is_not_spliced_onto_the_old_head() {
    let (mut app, _term) = setup();
    app.messages
        .push(ChatMsg::for_test(Role::Agent, "new text entirely"));
    app.flushed_upto = 0;
    app.flushed_text = "old head\n\n".into();
    app.flushed_bytes = app.flushed_text.len();
    assert_eq!(effective_skip(&app), 0);
}

#[test]
fn relocation_skips_whitespace_and_lands_on_the_next_block() {
    assert_eq!(relocate_prefix("a b\n\n\nc", "ab\n"), Some(6));
    assert_eq!(relocate_prefix("ab", "ab"), Some(2));
    assert_eq!(relocate_prefix("ax", "ab"), None);
    assert_eq!(relocate_prefix("a", "ab"), None);
}
