//! A streaming reply that carries reasoning must spill into native scrollback
//! as it grows, like one without. The spill loop used to refuse any turn with
//! non-empty `thinking`, so on a 30-row Ghostty pane the whole reply stayed in
//! the band and its head hid behind "↑ N more · PgUp" until the turn ended —
//! the transcript above never moved.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const ROWS: u16 = 30;
const COLS: u16 = 100;

fn reasoning() -> String {
    (1..=4)
        .map(|n| format!("THINK-{n:02} weighing option {n}\n"))
        .collect()
}

fn reply() -> String {
    (1..=24)
        .map(|n| format!("PARA-{n:02} 第 {n} 段說明。\n\n"))
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

struct Run {
    term: Terminal<TestBackend>,
    /// Frames, while the reply streamed, that hid rows behind the marker.
    hidden_frames: usize,
}

fn run(chunk_chars: usize) -> Run {
    let viewport = crate::cmd::agent::cli::term::viewport_h_for(ROWS);
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = COLS;
    let mut term = Terminal::with_options(
        TestBackend::new(COLS, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(viewport),
        },
    )
    .unwrap();
    app.begin_user_turn("q");
    let step = |app: &mut App, term: &mut Terminal<TestBackend>| -> bool {
        flush_finished(term, app, viewport).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        rows_of(term.backend().buffer())
            .iter()
            .any(|l| l.contains("more · PgUp"))
    };
    app.append_delta(&reasoning(), true);
    step(&mut app, &mut term);
    let mut hidden_frames = 0;
    let chars: Vec<char> = reply().chars().collect();
    for chunk in chars.chunks(chunk_chars) {
        app.append_delta(&chunk.iter().collect::<String>(), false);
        if step(&mut app, &mut term) {
            hidden_frames += 1;
        }
    }
    app.finish_agent_turn(reply(), None);
    for _ in 0..3 {
        step(&mut app, &mut term);
    }
    Run {
        term,
        hidden_frames,
    }
}

#[test]
fn reasoning_reply_spills_while_streaming() {
    for chunk in [1usize, 7, 40] {
        let r = run(chunk);
        let screen = rows_of(r.term.backend().buffer()).join("\n");
        assert_eq!(
            r.hidden_frames, 0,
            "chunk={chunk}: {} frames hid the reply behind PgUp\n{screen}",
            r.hidden_frames
        );
    }
}

#[test]
fn reasoning_and_every_paragraph_land_exactly_once() {
    for chunk in [1usize, 7, 40, 10_000] {
        let r = run(chunk);
        let all = everything(&r.term);
        let markers = reasoning()
            .lines()
            .chain(reply().lines())
            .filter_map(|l| l.split_whitespace().next())
            .filter(|w| w.starts_with("THINK-") || w.starts_with("PARA-"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        for m in markers {
            let n = all.matches(&m).count();
            assert_eq!(n, 1, "chunk={chunk}: {m} appeared {n}x\n{all}");
        }
        // Reasoning sits above the body it belongs to.
        let think = all.find("THINK-01").unwrap();
        let body = all.find("PARA-01").unwrap();
        assert!(
            think < body,
            "chunk={chunk}: reasoning landed below the body\n{all}"
        );
    }
}
