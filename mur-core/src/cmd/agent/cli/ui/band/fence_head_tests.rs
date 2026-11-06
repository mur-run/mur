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

fn reply() -> String {
    // Same shape as the report: prose, a 14-line `sh` fence with blank lines
    // inside it, an intro line, a list, a closing line. The fence plus what
    // follows it is taller than the band (22 rows > 16) while the fence alone
    // is not — the geometry that made the whole fence spill at once.
    let mut s = String::new();
    for n in 1..=9 {
        s.push_str(&format!("PROSE-{n:02} 先說明第 {n} 段的背景。\n\n"));
    }
    s.push_str("```sh\n");
    s.push_str("FENCE-01 EMAIL='a@b.c'\nFENCE-02 NEWPW='x'\n\n");
    s.push_str("FENCE-03 curl -s -X POST -H 'content-type: application/json' \\\n");
    s.push_str("FENCE-04   -d '{\"email\":\"a\"}' http://x/forgot\n\n");
    s.push_str("FENCE-05 TOKEN=$(psql -tAc 'select 1')\n");
    s.push_str("FENCE-06   \"select reset_token from users where email='a'\")\n");
    s.push_str("FENCE-07 echo \"$TOKEN\"\n\n");
    s.push_str("FENCE-08 curl -s -X POST -H 'content-type: application/json' \\\n");
    s.push_str("FENCE-09   -d '{\"token\":\"t\"}' http://x/reset\n```\n\n");
    s.push_str("INTRO-01 跑完看這三件事：\n\n");
    s.push_str("- LIST-01 first\n- LIST-02 second\n- LIST-03 third\n\nTAIL-01 done.\n");
    s
}

fn markers(reply: &str) -> Vec<String> {
    reply
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| {
            ["PROSE-", "FENCE-", "INTRO-", "LIST-", "TAIL-"]
                .iter()
                .any(|p| w.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}

fn rows_of(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    let w = usize::from(buf.area.width).max(1);
    buf.content
        .chunks(w)
        .map(|row| {
            let l: String = row.iter().map(|c| c.symbol()).collect();
            l.trim_end().to_string()
        })
        .collect()
}

fn everything(term: &Terminal<TestBackend>) -> String {
    let b = term.backend();
    let mut out = String::new();
    for buf in [b.scrollback(), b.buffer()] {
        for l in rows_of(buf) {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out
}

fn run(chunk_chars: usize) -> String {
    everything(&run_at(ROWS, chunk_chars))
}

fn run_at(rows: u16, chunk_chars: usize) -> Terminal<TestBackend> {
    let reply = reply();
    let viewport = crate::cmd::agent::cli::term::viewport_h_for(rows);
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = COLS;
    let mut term = Terminal::with_options(
        TestBackend::new(COLS, rows),
        TerminalOptions {
            viewport: Viewport::Inline(viewport),
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
        flush_finished(&mut term, &mut app, viewport).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(reply, None);
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, viewport).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    term
}

/// Blank rows sitting between the last line of the band and the composer's
/// separator — the band's unused space.
fn band_slack(screen: &[String]) -> usize {
    let composer = screen
        .iter()
        .position(|l| l.contains("Type a message"))
        .expect("composer row");
    // The row just above the composer is its separator rule.
    screen[..composer.saturating_sub(1)]
        .iter()
        .rev()
        .take_while(|l| l.is_empty())
        .count()
}

/// #2020, the reporter's real geometry: a Ghostty split pane ~30 rows high.
/// The band overflowed by one row when the fence closed, and the spill sent
/// the whole 14-line fence up — leaving the band half empty while the reply's
/// head sat off the top of the pane. A spill must take only what the overflow
/// needs, so the band stays full and the screen shows as much of the reply as
/// the pane physically can.
#[test]
fn short_pane_spill_takes_only_the_overflow() {
    for chunk in [1usize, 7, 40] {
        let term = run_at(30, chunk);
        let screen = rows_of(term.backend().buffer());
        let scrollback = rows_of(term.backend().scrollback());
        let all = everything(&term);
        for m in markers(&reply()) {
            let n = all.matches(&m).count();
            assert_eq!(n, 1, "chunk={chunk}: {m} appeared {n}x\n{all}");
        }
        let slack = band_slack(&screen);
        assert!(
            slack <= 1,
            "chunk={chunk}: band left {slack} blank rows\n{}",
            screen.join("\n")
        );
        assert!(
            scrollback.iter().any(|l| l.contains("PROSE-01")),
            "chunk={chunk}: reply head missing from scrollback"
        );
        // Only the torn block goes raw; the list after the fence still renders.
        assert!(
            all.contains("• LIST-01"),
            "chunk={chunk}: list after the fence lost its rendering\n{all}"
        );
    }
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
