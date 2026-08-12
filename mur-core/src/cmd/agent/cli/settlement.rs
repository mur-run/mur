//! The per-turn settlement card: split off the reply, parsed, and rendered at
//! the pane's real width.
//!
//! The runtime emits it as fenced text so every non-TUI consumer (`mur agent
//! send`, `--plain`, logs, the Hub) can read it. The TUI upgrades that text to
//! a card, which is why the split happens here and not in the Markdown
//! renderer: the card needs a width, and `ChatMsg::rendered` is cached
//! width-free.

/// The marker the runtime writes as the fence's first line.
const MARKER: &str = "─ settlement ─";

/// Split a settlement card off the end of an agent reply.
///
/// Returns the reply with the card removed, and the card's body — every line
/// between the marker and the closing fence. Returns `(text, None)` unchanged
/// when there is no card, which is the common case: most turns do not earn
/// one.
pub fn split(text: &str) -> (String, Option<String>) {
    let Some(fence_at) = text.rfind("```\n") else {
        return (text.to_string(), None);
    };
    let after_fence = &text[fence_at + 4..];
    let Some(rest) = after_fence.strip_prefix(MARKER) else {
        return (text.to_string(), None);
    };
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(close_at) = rest.find("```") else {
        return (text.to_string(), None);
    };
    let body = rest[..close_at].trim_end_matches('\n').to_string();
    let head = text[..fence_at].trim_end_matches('\n').to_string();
    (head, Some(body))
}

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns the glyph column occupies: two spaces, the glyph, one space.
const GLYPH_COL: usize = 4;

/// Narrower than this and the hanging indent costs more than it buys, so the
/// card falls back to flush-left rows.
const MIN_INDENT_WIDTH: u16 = 24;

/// Colour for a row, chosen by its lead glyph.
fn row_style(glyph: char, theme: &'static super::theme::Theme) -> Style {
    let fg = match glyph {
        '✔' => theme.success,
        '✘' => theme.error,
        '⚠' => theme.warn,
        _ => theme.agent_text,
    };
    Style::default().fg(fg).bg(theme.card_bg)
}

/// Break `s` into chunks no wider than `width` display columns.
///
/// Greedy on word boundaries, falling back to a hard break for a single token
/// longer than the line — a 200-character error string with no spaces still
/// has to land somewhere, and cutting it is the one thing this card must not
/// do.
fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    for word in s.split(' ') {
        let word_w = word.width();
        if !line.is_empty() && line_w + 1 + word_w > width {
            out.push(std::mem::take(&mut line));
            line_w = 0;
        }
        if word_w > width {
            // Hard-break an unbreakable token.
            for c in word.chars() {
                let cw = c.width().unwrap_or(0);
                if line_w + cw > width {
                    out.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                line.push(c);
                line_w += cw;
            }
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
            line_w += 1;
        }
        line.push_str(word);
        line_w += word_w;
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Pad `s` to exactly `width` display columns.
fn pad(s: &str, width: usize) -> String {
    let w = s.width();
    if w >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - w))
}

/// Draw the settlement card for `body` at `width` columns.
///
/// Every row is padded to the full width and carries `theme.card_bg`, so the
/// block reads as one surface rather than ragged text. Nothing is elided: the
/// runtime already stopped guessing what fits, and this is the layer that
/// actually knows.
pub fn card_lines(
    body: &str,
    theme: &'static super::theme::Theme,
    width: u16,
) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let indent = if width >= MIN_INDENT_WIDTH {
        GLYPH_COL
    } else {
        0
    };
    let mut out = vec![Line::from(Span::styled(
        pad(" SETTLEMENT", w),
        Style::default()
            .fg(theme.border_title)
            .bg(theme.card_bg)
            .add_modifier(Modifier::BOLD),
    ))];
    for raw in body.lines() {
        let trimmed = raw.trim_start();
        let glyph = trimmed.chars().next().unwrap_or(' ');
        let style = row_style(glyph, theme);
        let is_row = matches!(glyph, '✔' | '✘' | '⚠' | '~');
        let (head, text) = if is_row {
            let rest = trimmed.chars().skip(1).collect::<String>();
            (format!("  {glyph} "), rest.trim_start().to_string())
        } else {
            (" ".repeat(indent.max(2)), trimmed.to_string())
        };
        let head_w = head.width();
        let avail = w.saturating_sub(head_w).max(1);
        for (i, chunk) in wrap(&text, avail).into_iter().enumerate() {
            let prefix = if i == 0 {
                head.clone()
            } else {
                " ".repeat(head_w)
            };
            out.push(Line::from(Span::styled(
                pad(&format!("{prefix}{chunk}"), w),
                style,
            )));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_the_card_off_the_prose() {
        let reply = "did the thing\n\n```\n─ settlement ─\n  ✔ bash · cargo test\n```";
        let (head, card) = split(reply);
        assert_eq!(head, "did the thing");
        assert_eq!(card.as_deref(), Some("  ✔ bash · cargo test"));
    }

    #[test]
    fn an_ordinary_code_fence_is_not_a_settlement() {
        let reply = "look:\n\n```\nfn main() {}\n```";
        let (head, card) = split(reply);
        assert_eq!(head, reply);
        assert!(card.is_none());
    }

    #[test]
    fn a_reply_with_no_fence_is_returned_whole() {
        let (head, card) = split("just prose");
        assert_eq!(head, "just prose");
        assert!(card.is_none());
    }

    use super::super::theme::DARK;
    use super::card_lines;
    use unicode_width::UnicodeWidthStr;

    fn plain(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn every_row_is_padded_to_the_pane_width() {
        let out = card_lines("  ✔ bash · cargo test", &DARK, 40);
        assert!(!out.is_empty());
        for row in plain(&out) {
            assert_eq!(row.width(), 40, "ragged row: {row:?}");
        }
    }

    #[test]
    fn long_detail_wraps_instead_of_being_cut() {
        let body = format!("  ✘ parallel_jobs · {}", "e".repeat(200));
        let narrow = card_lines(&body, &DARK, 40);
        let wide = card_lines(&body, &DARK, 100);
        assert!(
            narrow.len() > wide.len(),
            "narrow={} wide={} — text must reflow, not truncate",
            narrow.len(),
            wide.len()
        );
        for row in plain(&narrow) {
            assert!(!row.contains('…'), "nothing may be elided: {row:?}");
        }
    }

    #[test]
    fn the_card_names_itself() {
        let out = plain(&card_lines("  ✔ bash", &DARK, 40));
        assert!(out[0].contains("SETTLEMENT"), "{out:?}");
    }
}
