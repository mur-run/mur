//! [DEBUG-2020] Replay a turn recorded with `MUR_BAND_REC=<file>` through the
//! band at the recorded width, then check that every wide glyph of the final
//! reply reached scrollback or the screen, in order. Ignored; run with:
//!   BAND_REPLAY=~/band-2020.rec cargo test -p mur-core --lib band_replay_2020 -- --ignored --nocapture
//! BAND_VH (viewport rows, default 17 — what the last log showed).

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn wide(s: &str) -> String {
    s.chars().filter(|c| c.width().unwrap_or(0) == 2).collect()
}

fn everything(term: &Terminal<TestBackend>) -> String {
    let b = term.backend();
    let mut out = String::new();
    for buf in [b.scrollback(), b.buffer()] {
        let w = usize::from(buf.area.width).max(1);
        for row in buf.content.chunks(w) {
            let mut i = 0;
            while i < row.len() {
                let sym = row[i].symbol();
                out.push_str(sym);
                i += if UnicodeWidthStr::width(sym) >= 2 {
                    2
                } else {
                    1
                };
            }
            out.push('\n');
        }
    }
    out
}

#[test]
#[ignore = "replay: needs BAND_REPLAY=<recording>"]
fn band_replay_2020() {
    let path = std::env::var("BAND_REPLAY").expect("set BAND_REPLAY");
    let vh: u16 = std::env::var("BAND_VH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(17);
    let events: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    // Replay only the last turn: everything after the second-to-last finish.
    let finishes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["kind"] == "finish")
        .map(|(i, _)| i)
        .collect();
    let end = *finishes.last().expect("no finished turn in recording");
    let begin = finishes.len().checked_sub(2).map_or(0, |k| finishes[k] + 1);
    let turn = &events[begin..=end];
    let cols = turn
        .iter()
        .filter_map(|e| e["width"].as_u64())
        .max()
        .unwrap_or(80) as u16;
    let reply = turn.last().unwrap()["text"].as_str().unwrap().to_string();

    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = cols;
    let mut term = Terminal::with_options(
        TestBackend::new(cols, vh + 40),
        TerminalOptions {
            viewport: Viewport::Inline(vh),
        },
    )
    .unwrap();
    app.begin_user_turn("replay");
    for e in &turn[..turn.len() - 1] {
        app.append_delta(e["text"].as_str().unwrap_or(""), e["thinking"] == true);
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(reply.clone(), None);
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    let got = wide(&everything(&term));
    let want = wide(&reply);
    if !got.contains(&want) {
        // Point at the first glyph run that went missing.
        let w: Vec<char> = want.chars().collect();
        let mut lo = 0;
        while lo < w.len() && got.contains(&w[..=lo].iter().collect::<String>()) {
            lo += 1;
        }
        let ctx: String = w[lo.saturating_sub(15)..(lo + 15).min(w.len())]
            .iter()
            .collect();
        panic!(
            "cols={cols} vh={vh}: output diverges at glyph {lo}/{}: …{ctx}…",
            w.len()
        );
    }
    eprintln!(
        "cols={cols} vh={vh}: all {} wide glyphs present",
        want.chars().count()
    );
}
