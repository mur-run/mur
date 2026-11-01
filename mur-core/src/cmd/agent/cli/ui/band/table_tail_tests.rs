//! #2020: a reply shaped like the "explain ownership" one lost the rows
//! between a markdown table and the closing summary. Streams prose → a long
//! fenced block → a table → a summary, then settles, at several widths and
//! viewport heights, and asserts every marker reaches scrollback or screen.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_width::UnicodeWidthStr;

const ROWS: u16 = 60;
const MARKERS: &[&str] = &[
    "P01", "P02", "C01", "C20", "C40", "H01", "R01", "R02", "R03", "R04", "R05", "S01", "B01",
    "B02", "B03", "E01",
];

fn reply() -> String {
    let mut s = String::new();
    s.push_str("P01 Ownership 是 Rust 最核心的概念，下面先講規則，再看一段範例程式碼。\n\n");
    s.push_str("P02 每個值都有一個擁有者，擁有者離開作用域時值就會被釋放。\n\n");
    s.push_str("```rust\n");
    for n in 1..=40 {
        s.push_str(&format!(
            "    let v{n} = String::from(\"value\"); // C{n:02}\n"
        ));
    }
    s.push_str("```\n\n");
    s.push_str("## H01 常見卡關點\n\n");
    s.push_str("| 錯誤訊息 | 意思 |\n|---|---|\n");
    s.push_str("| `use of moved value` | R01 值已經被移走了 |\n");
    s.push_str("| `cannot borrow as mutable because it is also borrowed as immutable` | R02 不能同時借讀又借寫 |\n");
    s.push_str("| `does not live long enough` | R03 參照活得比值還久 |\n");
    s.push_str("| `cannot move out of borrowed content` | R04 不能從借來的東西裡搬走 |\n");
    s.push_str("| `borrowed value does not live long enough` | R05 生命週期不夠長 |\n\n");
    s.push_str(
        "S01 Ownership 把「誰負責釋放這塊記憶體」從執行期的問題，變成編譯期就能檢查的規則：\n\n",
    );
    s.push_str("- B01 擁有（T）：負責釋放，可以把所有權交出去。\n");
    s.push_str("- B02 借讀（&T）：可以有很多個。\n");
    s.push_str("- B03 借寫（&mut T）：同一時間只能有一個，而且是獨占的。\n\n");
    s.push_str("E01 剛開始常常會跟 borrow checker 吵架，但它擋下來的幾乎都是執行期 bug。\n");
    s
}

fn everything(term: &Terminal<TestBackend>) -> String {
    let b = term.backend();
    let mut out = String::new();
    for buf in [b.scrollback(), b.buffer()] {
        let w = usize::from(buf.area.width).max(1);
        for row in buf.content.chunks(w) {
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

fn run(cols: u16, vh: u16, chunk: usize, pause_at_table: bool) -> String {
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = cols;
    let mut term = Terminal::with_options(
        TestBackend::new(cols, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(vh),
        },
    )
    .unwrap();
    app.begin_user_turn("用繁體中文解釋 Rust 的 ownership");
    let reply = reply();
    let chars: Vec<char> = reply.chars().collect();
    for c in chars.chunks(chunk) {
        app.append_delta(&c.iter().collect::<String>(), false);
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        // The log showed an 11 s gap mid-table: redraw idle a few times.
        if pause_at_table && app_tail_is_mid_table(&c.iter().collect::<String>()) {
            for _ in 0..3 {
                flush_finished(&mut term, &mut app, vh).unwrap();
                term.draw(|f| render(f, &mut app)).unwrap();
            }
        }
    }
    app.finish_agent_turn(reply, None);
    app.push_system("100 open items (9 observed, 91 reported) · 134 stale — /open");
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    everything(&term)
}

fn app_tail_is_mid_table(chunk: &str) -> bool {
    chunk.contains('|')
}

#[test]
fn every_row_around_a_table_survives_settling() {
    let mut failures = Vec::new();
    for cols in [60u16, 79, 80, 100, 127] {
        for vh in [12u16, 16, 17, 20, 24] {
            for chunk in [3usize, 7, 40] {
                for pause in [false, true] {
                    let out = run(cols, vh, chunk, pause);
                    let missing: Vec<_> = MARKERS.iter().filter(|m| !out.contains(**m)).collect();
                    if !missing.is_empty() {
                        failures.push(format!(
                            "cols={cols} vh={vh} chunk={chunk} pause={pause}: missing {missing:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} cases lost rows:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
