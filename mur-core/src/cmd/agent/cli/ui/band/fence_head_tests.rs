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

/// [DEBUG-2020] Resize mid-stream: `rebuild_after_resize` purges scrollback,
/// rewinds the flush cursors, and re-anchors. Model the purge as a fresh
/// backend at the new size and check the replay restores every line.
fn run_resize(chunk_chars: usize, at: usize, cols2: u16, rows2: u16) -> String {
    let reply = reply();
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = COLS;
    let mk = |c, r| {
        Terminal::with_options(
            TestBackend::new(c, r),
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT),
            },
        )
        .unwrap()
    };
    let mut term = mk(COLS, ROWS);
    app.begin_user_turn("q");
    let chars: Vec<char> = reply.chars().collect();
    for (i, chunk) in chars
        .chunks(chunk_chars)
        .map(|c| c.iter().collect::<String>())
        .enumerate()
    {
        if i == at {
            term = mk(cols2, rows2);
            app.flushed_upto = 0;
            app.flushed_bytes = 0;
            app.flushed_raw = false;
            app.width = cols2;
            app.rerender_markdown();
        }
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
fn debug_2020_resize_mid_stream() {
    let n = reply().chars().count();
    let mut fails = Vec::new();
    for chunk in [1usize, 7, 40] {
        let steps = n.div_ceil(chunk);
        for frac in [10, 30, 50, 70, 90] {
            let at = steps * frac / 100;
            for (c2, r2) in [(127u16, 58u16), (100, 40), (160, 70), (127, 30)] {
                let out = run_resize(chunk, at, c2, r2);
                for m in markers(&reply()) {
                    let k = out.matches(&m).count();
                    if k != 1 {
                        fails.push(format!("chunk={chunk} at={frac}% {c2}x{r2}: {m} x{k}"));
                    }
                }
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// [DEBUG-2020] Same replay, but the turn streams reasoning first — the
/// screenshot showed "↑ 618 more" over a settled reply, far taller than the
/// body alone.
fn run_thinking(chunk_chars: usize, think_lines: usize) -> (String, String) {
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
    for i in 0..think_lines {
        app.append_delta(&format!("thinking line {i}\n"), true);
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
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
    let screen = {
        let buf = term.backend().buffer();
        let w = usize::from(buf.area.width);
        buf.content
            .chunks(w)
            .map(|r| {
                r.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    (everything(&term), screen)
}

#[test]
fn debug_2020_thinking_turn() {
    let mut fails = Vec::new();
    for think in [0usize, 5, 600] {
        for chunk in [7usize, 40] {
            let (out, screen) = run_thinking(chunk, think);
            for m in markers(&reply()) {
                let k = out.matches(&m).count();
                if k != 1 {
                    fails.push(format!("think={think} chunk={chunk}: {m} x{k}"));
                }
            }
            if screen.contains("PgUp") {
                fails.push(format!(
                    "think={think} chunk={chunk}: PgUp marker on settled screen\n{screen}"
                ));
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// [DEBUG-2020] A `!cmd` card still live (`streaming = true`) at the top of
/// the band: does it pin `flushed_upto` so every later reply piles up behind
/// it under "↑ N more · PgUp"?
#[test]
fn debug_2020_live_shell_card_pins_the_band() {
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
    app.begin_shell("sleep 999");
    for _ in 0..3 {
        app.begin_user_turn("q");
        app.append_delta(&reply(), false);
        app.finish_agent_turn(reply(), None);
        for _ in 0..3 {
            flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
            term.draw(|f| render(f, &mut app)).unwrap();
        }
    }
    let screen = everything(&term);
    eprintln!(
        "flushed_upto={} of {} msgs; PgUp hint on screen: {}",
        app.flushed_upto,
        app.messages.len(),
        screen.contains("more · PgUp")
    );
    assert_eq!(app.flushed_upto, 0, "live shell card should pin the cursor");
    assert!(screen.contains("more · PgUp"));
}

/// [DEBUG-2020] A FINISHED (`[exit 0]`) but tall `!cmd` card at the top of
/// the band, followed by the turn its output starts. Does it still pin?
fn run_finished_shell(body: &str, with_turn: bool) -> (usize, usize, bool, Vec<String>) {
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
    let mut trace = Vec::new();
    app.begin_shell("cargo build");
    for chunk in body.as_bytes().chunks(8192) {
        app.append_shell_output(std::str::from_utf8(chunk).unwrap_or(""));
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    let _ = app.finish_shell(&crate::cmd::agent::cli::shell::ShellEnd::Exited(0));
    for _ in 0..2 {
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    trace.push(format!(
        "after exit: flushed_upto={} streaming={:?}",
        app.flushed_upto,
        app.messages
            .iter()
            .map(|m| (format!("{:?}", m.role), m.streaming))
            .collect::<Vec<_>>()
    ));
    if with_turn {
        app.begin_turn();
        app.append_delta(&reply(), false);
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        app.finish_agent_turn(reply(), None);
        for _ in 0..3 {
            flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
            term.draw(|f| render(f, &mut app)).unwrap();
        }
        trace.push(format!(
            "after turn: flushed_upto={} of {}",
            app.flushed_upto,
            app.messages.len()
        ));
    }
    let screen = everything(&term);
    (
        app.flushed_upto,
        app.messages.len(),
        screen.contains("more · PgUp"),
        trace,
    )
}

#[test]
fn debug_2020_finished_shell_card_variants() {
    let plain: String = (0..700).map(|i| format!("line {i}\n")).collect();
    let cr: String = (0..700)
        .map(|i| format!("Compiling {i}\r   Building [{i}/700]\n"))
        .collect();
    let tabs: String = (0..700)
        .map(|i| format!("\tcol\t{i}\twide 中文字元\n"))
        .collect();
    let longl: String = (0..80)
        .map(|i| format!("{}{i}\n", "x".repeat(400)))
        .collect();
    let ansi: String = (0..700)
        .map(|i| format!("\x1b[32m   Compiling\x1b[0m crate{i}\n"))
        .collect();
    let tiny: String = (0..66_000).map(|_| "a\n").collect();
    let tiny2: String = (0..66_500).map(|_| "a\n").collect();
    let prog: String = (0..4000)
        .map(|i| format!("\r   Building [=====>    ] {i}/4000: crate-name-here"))
        .collect();
    let mut out = Vec::new();
    for (name, body) in [
        ("cr-oneline", &prog),
        ("tiny66000", &tiny),
        ("tiny66500", &tiny2),
        ("plain", &plain),
        ("cr", &cr),
        ("tabs", &tabs),
        ("long", &longl),
        ("ansi", &ansi),
    ] {
        for with_turn in [false, true] {
            let (f, n, pg, tr) = run_finished_shell(body, with_turn);
            out.push(format!(
                "{name} turn={with_turn}: flushed {f}/{n} PgUp={pg} | {}",
                tr.join(" | ")
            ));
        }
    }
    panic!("\n{}", out.join("\n"));
}
