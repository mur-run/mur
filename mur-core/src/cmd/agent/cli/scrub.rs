//! The last gate between text murmur did not write and the terminal.
//!
//! Model replies, tool output and upstream error strings are data. A control
//! character in them (ESC, BEL, C1 CSI U+009B, DEL, ...) is an instruction to
//! the terminal: clear the screen, move the cursor, repaint a fake approval
//! card. ratatui 0.29 only drops zero-width symbols and unicode-width 0.2
//! counts a control character as width 1, so it lands in a cell and crossterm
//! prints it verbatim. `char::is_control` is Unicode `Cc`: C0, DEL and C1.

use ratatui::buffer::Buffer;
use std::borrow::Cow;

/// What a scrubbed control character becomes: visible, so tampering shows
/// instead of silently vanishing, and width 1 like the cell it replaces.
const MARK: &str = "\u{FFFD}";

/// Rewrite every cell holding a control character. Run on the full frame
/// buffer after all widgets have drawn, so no widget can opt out of it.
pub(crate) fn scrub_buffer(buf: &mut Buffer) {
    for cell in &mut buf.content {
        let sym = cell.symbol();
        if sym.chars().any(char::is_control) {
            let repl = if sym == "\t" { " " } else { MARK };
            cell.set_symbol(repl);
        }
    }
}

/// Plain-mode (line) output: keep `\n` and `\t`, which a pipe expects, and
/// replace every other control character, `\r` included (it rewinds the line
/// so later text overwrites what was printed before it).
pub(crate) fn scrub_str(s: &str) -> Cow<'_, str> {
    let bad = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !s.chars().any(bad) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .map(|c| if bad(c) { '\u{FFFD}' } else { c })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn scrub_str_replaces_controls_but_keeps_newline_and_tab() {
        let s = scrub_str("a\u{1b}[2Jb\u{7}\u{9b}c\rd\u{7f}\n\te");
        assert_eq!(s, "a\u{FFFD}[2Jb\u{FFFD}\u{FFFD}c\u{FFFD}d\u{FFFD}\n\te");
    }

    #[test]
    fn scrub_str_borrows_clean_text() {
        // Negative control: clean text (CJK included) is untouched and not copied.
        assert!(matches!(scrub_str("你好 ok\n"), Cow::Borrowed("你好 ok\n")));
    }

    #[test]
    fn scrub_buffer_rewrites_only_control_cells() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(0, 0)].set_symbol("x");
        buf[(1, 0)].set_symbol("\u{1b}");
        buf[(2, 0)].set_symbol("\t");
        buf[(3, 0)].set_symbol("你");
        scrub_buffer(&mut buf);
        let got: Vec<&str> = buf.content.iter().map(|c| c.symbol()).collect();
        assert_eq!(got, ["x", MARK, " ", "你"]);
    }
}
