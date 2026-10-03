//! #1643: a long CJK reply lost its tail after it settled. Replays the exact
//! reply through stream → settle → flush at several real screen widths and
//! asserts the final sentence reaches scrollback or the screen intact.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const REPLY: &str = include_str!("cjk_tail_1643.md");
const ROWS: u16 = 50;
const VIEWPORT: u16 = 20;

fn everything(term: &Terminal<TestBackend>) -> String {
    let b = term.backend();
    let mut out = String::new();
    for buf in [b.scrollback(), b.buffer()] {
        let w = usize::from(buf.area.width).max(1);
        for row in buf.content.chunks(w) {
            // A real terminal paints a wide glyph over two columns; the
            // TestBackend keeps whatever stale symbol the second one held.
            let mut l = String::new();
            let mut i = 0;
            while i < row.len() {
                let sym = row[i].symbol();
                l.push_str(sym);
                i += if UnicodeWidthStr::width(sym) >= 2 {
                    2
                } else {
                    1
                };
            }
            out.push_str(l.trim_end());
            out.push('\n');
        }
    }
    out
}

fn run(cols: u16) -> String {
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
    let chars: Vec<char> = REPLY.chars().collect();
    for chunk in chars.chunks(7).map(|c| c.iter().collect::<String>()) {
        app.append_delta(&chunk, false);
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(REPLY.to_string(), None);
    app.push_system("110 open items (110 reported) · 14 stale — /open");
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    everything(&term)
}

#[test]
fn the_tail_of_a_long_cjk_reply_survives_settling() {
    for cols in [40u16, 61, 80, 99, 120, 157, 200, 241, 250, 263] {
        let out = run(cols);
        // Compare the wide glyphs only, in order: wrap points and the ASCII
        // between them vary with width, the CJK sequence must not.
        let wide = |s: &str| -> String {
            s.chars()
                .filter(|c| c.width().unwrap_or(0) == 2)
                .collect::<String>()
        };
        let got = wide(&out);
        let tail = REPLY.trim_end().rsplit("\n\n").next().unwrap();
        assert!(
            got.contains(&wide(tail)),
            "cols={cols}: last paragraph incomplete\n{out}"
        );
    }
}

/// The seam: `emit` sizes each `insert_before` buffer with `line_rows`. It
/// must equal the rows the padded paragraph actually paints, or the buffer is
/// too short and the last rows are cut off before they reach scrollback.
#[test]
fn line_rows_counts_the_rows_the_padded_paragraph_paints() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Text};
    use ratatui::widgets::{Block, Padding, Paragraph, Widget, Wrap};
    let line = Line::raw(REPLY.trim_end().rsplit("\n\n").next().unwrap().to_string());
    for (pad, width) in [(1u16, 40u16), (1, 61), (2, 80), (1, 99)] {
        let counted = super::line_rows(&line, pad, width);
        let area = Rect::new(0, 0, width, 64);
        let mut buf = Buffer::empty(area);
        Paragraph::new(Text::from(line.clone()))
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::horizontal(pad)))
            .render(area, &mut buf);
        let w = usize::from(width);
        let painted = buf
            .content
            .chunks(w)
            .rposition(|r| r.iter().any(|c| !c.symbol().trim().is_empty()))
            .map_or(0, |i| i + 1) as u16;
        assert_eq!(counted, painted, "pad={pad} width={width}");
    }
}
