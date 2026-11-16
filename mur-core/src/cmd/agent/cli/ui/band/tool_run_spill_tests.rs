//! A tool-heavy turn must keep the band scrolling. The reported screen
//! (Ghostty, 30-row pane) showed "↑ 815 more · PgUp" while a run of `bash`
//! cards streamed: the previous reply and every card stayed trapped in the
//! band instead of going up into native scrollback.
//!
//! Shape of the turn reproduced here, as the runtime streams it: reasoning
//! deltas, then a tool card, reasoning again, another card, … then the final
//! reply — followed by a second user turn of the same shape.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const ROWS: u16 = 30;
const COLS: u16 = 100;
const STEPS: usize = 8;

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

struct Harness {
    app: App,
    term: Terminal<TestBackend>,
    viewport: u16,
    /// Frames that hid rows behind the marker, with a label of what ran.
    hidden: Vec<String>,
}

impl Harness {
    fn new() -> Self {
        let viewport = crate::cmd::agent::cli::term::viewport_h_for(ROWS);
        let mut app = App::test_fixture();
        app.render_mode = RenderMode::Inline;
        app.width = COLS;
        let term = Terminal::with_options(
            TestBackend::new(COLS, ROWS),
            TerminalOptions {
                viewport: Viewport::Inline(viewport),
            },
        )
        .unwrap();
        Self {
            app,
            term,
            viewport,
            hidden: Vec::new(),
        }
    }

    fn frame(&mut self, label: &str) {
        flush_finished(&mut self.term, &mut self.app, self.viewport).unwrap();
        self.term.draw(|f| render(f, &mut self.app)).unwrap();
        let screen = rows_of(self.term.backend().buffer());
        if screen.iter().any(|l| l.contains("more · PgUp")) {
            self.hidden.push(format!("{label}\n{}", screen.join("\n")));
        }
    }

    fn turn(&mut self, t: usize, with_text_before_tools: bool) {
        self.app.begin_user_turn(&format!("Q{t}"));
        self.frame(&format!("t{t} begin"));
        for s in 0..STEPS {
            self.app
                .append_delta(&format!("T{t}-THINK-{s} checking\n"), true);
            self.frame(&format!("t{t} s{s} thinking"));
            if with_text_before_tools {
                self.app
                    .append_delta(&format!("T{t}-NOTE-{s} 讀一下。\n"), false);
                self.frame(&format!("t{t} s{s} note"));
            }
            let id = format!("t{t}-s{s}");
            self.app.push_step_started(
                id.clone(),
                "bash".into(),
                serde_json::json!({"command": format!("sed -n {s},99p band.rs")}),
            );
            self.frame(&format!("t{t} s{s} started"));
            let output: String = (0..20).map(|n| format!("T{t}-S{s}-OUT-{n:02}\n")).collect();
            self.app
                .update_step_completed(&id, true, output, false, 0, None, 12, false, false);
            self.frame(&format!("t{t} s{s} completed"));
        }
        self.app.append_delta(&format!("T{t}-FINAL-THINK\n"), true);
        self.frame(&format!("t{t} final thinking"));
        let reply: String = (0..12)
            .map(|n| format!("T{t}-PARA-{n:02} 說明。\n\n"))
            .collect();
        self.app.finish_agent_turn(reply, None);
        for i in 0..3 {
            self.frame(&format!("t{t} settled {i}"));
        }
    }
}

fn assert_never_hidden(h: &Harness) {
    assert!(
        h.hidden.is_empty(),
        "{} frames hid rows behind PgUp; first:\n{}",
        h.hidden.len(),
        h.hidden[0]
    );
}

#[test]
fn reasoning_only_segments_between_tools_never_hide_the_band() {
    let mut h = Harness::new();
    h.turn(1, false);
    h.turn(2, false);
    assert_never_hidden(&h);
}

#[test]
fn text_segments_between_tools_never_hide_the_band() {
    let mut h = Harness::new();
    h.turn(1, true);
    h.turn(2, true);
    assert_never_hidden(&h);
}

#[test]
fn no_message_is_left_streaming_after_the_turn() {
    let mut h = Harness::new();
    h.turn(1, false);
    let stuck: Vec<usize> = h
        .app
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.streaming)
        .map(|(i, _)| i)
        .collect();
    assert!(stuck.is_empty(), "messages left streaming: {stuck:?}");
}
