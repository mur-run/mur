//! #1649: a long reply stayed raw markdown after it settled. One block taller
//! than the band spills as raw lines from mid-block; that latch used to hold
//! for the rest of the turn, so every later heading, table and bullet reached
//! scrollback (and the settled band) as raw `##` / `|---|` / `**` text.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const ROWS: u16 = 120;
const VIEWPORT: u16 = 20;

/// A first block with no blank line inside it, taller than the band, so the
/// spill must tear it; then ordinary blocks that each fit.
fn reply() -> String {
    let mut s = String::new();
    for i in 0..30 {
        s.push_str(&format!("plain line {i} of one tall paragraph\n"));
    }
    s.push_str("\n## Settled heading\n\n");
    s.push_str("| Name | Value |\n|---|---|\n| alpha | 1 |\n| beta | 2 |\n\n");
    s.push_str("- first **bold** bullet\n- second bullet\n\n");
    s.push_str("Closing sentence of the reply.\n");
    s
}

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

fn run(cols: u16, reply: &str) -> String {
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = cols;
    let mut term = Terminal::with_options(
        TestBackend::new(cols, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT),
        },
    )
    .unwrap();
    app.begin_user_turn("q");
    let chars: Vec<char> = reply.chars().collect();
    for chunk in chars.chunks(9).map(|c| c.iter().collect::<String>()) {
        app.append_delta(&chunk, false);
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(reply.to_string(), None);
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    everything(&term)
}

#[test]
fn blocks_after_a_torn_one_settle_as_rendered_markdown() {
    let reply = reply();
    for cols in [60u16, 80, 120] {
        let out = run(cols, &reply);
        for raw in ["## Settled", "|---", "**bold**", "- first"] {
            assert!(
                !out.contains(raw),
                "cols={cols}: raw markdown {raw:?} survived settling\n{out}"
            );
        }
        assert!(
            out.contains("Settled heading") && out.contains("alpha"),
            "cols={cols}: rendered blocks missing\n{out}"
        );
        assert!(
            out.contains("Closing sentence of the reply."),
            "cols={cols}: tail lost\n{out}"
        );
        // The torn block itself went up raw; it must still be all there.
        assert!(
            out.contains("plain line 0 of") && out.contains("plain line 29 of"),
            "cols={cols}: torn block incomplete\n{out}"
        );
    }
}
