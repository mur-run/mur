//! #2020: the head of a final reply containing a fenced code block was cut
//! off. Replays a reply shaped like the report (prose, then a `sh` fence with
//! blank lines inside it, then a list) through stream → settle → flush at the
//! reporter's geometry, and asserts every source line reaches scrollback or
//! the screen exactly once.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const ROWS: u16 = 58;
const COLS: u16 = 127;
const VIEWPORT: u16 = 20;

fn reply() -> String {
    let mut s = String::new();
    for n in 1..=9 {
        s.push_str(&format!("PROSE-{n:02} 先說明第 {n} 段的背景。\n\n"));
    }
    s.push_str("```sh\n");
    s.push_str("FENCE-01 EMAIL='a@b.c'\nFENCE-02 NEWPW='x'\n\n");
    s.push_str("FENCE-03 curl -s -X POST -H 'content-type: application/json' \\\n");
    s.push_str("FENCE-04   -d '{\"email\":\"a\"}' http://x/forgot\n\n");
    s.push_str("FENCE-05 TOKEN=$(psql -tAc 'select 1')\nFENCE-06 echo \"$TOKEN\"\n\n");
    s.push_str("FENCE-07 curl -s -X POST http://x/reset\n```\n\n");
    s.push_str("- LIST-01 first\n- LIST-02 second\n\nTAIL-01 done.\n");
    s
}

fn markers(reply: &str) -> Vec<String> {
    reply
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| {
            ["PROSE-", "FENCE-", "LIST-", "TAIL-"]
                .iter()
                .any(|p| w.starts_with(p))
        })
        .map(str::to_string)
        .collect()
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

fn run(chunk_chars: usize) -> String {
    let reply = reply();
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = COLS;
    let mut term = Terminal::with_options(
        TestBackend::new(COLS, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT),
        },
    )
    .unwrap();
    app.begin_user_turn("q");
    let chars: Vec<char> = reply.chars().collect();
    for chunk in chars
        .chunks(chunk_chars)
        .map(|c| c.iter().collect::<String>())
    {
        app.append_delta(&chunk, false);
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(reply, None);
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    everything(&term)
}

#[test]
fn every_line_before_and_inside_a_fence_survives_a_streamed_reply() {
    for chunk in [1usize, 3, 7, 40, 10_000] {
        let out = run(chunk);
        for m in markers(&reply()) {
            let n = out.matches(&m).count();
            assert_eq!(n, 1, "chunk={chunk}: {m} appeared {n}x\n{out}");
        }
    }
}
