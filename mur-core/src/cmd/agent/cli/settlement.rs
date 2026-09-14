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

/// Columns the rail and glyph column occupy together: the rail, a space, the
/// glyph, one space.
const GLYPH_COL: usize = 4;

/// The left rail every body row starts with. It is what makes the block read
/// as one unit on `ansi`, whose `surface` paints no background at all.
pub const RAIL: &str = "▎";

/// The title chip. Padded on both sides so the badge renders as a block.
const TITLE: &str = " SETTLEMENT ";
/// Deliberate inset on the left and right edge of every settlement card.
/// ponytail: no vertical counterpart — the title row already separates the
/// card from the prose above it, so blank surface rows would only add height.
const HORIZONTAL_PADDING: usize = 2;

/// Narrower than this and the hanging indent costs more than it buys, so the
/// card falls back to flush-left rows.
const MIN_INDENT_WIDTH: u16 = 24;

/// Colour for a row, chosen by its lead glyph.
fn row_styles(glyph: char, theme: &'static super::theme::Theme) -> (Style, Style) {
    let status = match glyph {
        '✔' => theme.settlement_ok,
        '✘' => theme.settlement_error,
        // The summary count already calls out non-fatal warnings. Keeping the
        // row glyph neutral stops a few warnings from turning into a banner.
        '⚠' => theme.settlement_muted,
        _ => theme.settlement_text,
    }
    .patch(theme.settlement_surface);
    // A status colour is a precise signal, not a flood fill: keep command and
    // diagnostic copy neutral so a card with warnings remains scannable.
    (
        status,
        theme.settlement_text.patch(theme.settlement_surface),
    )
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

/// The title row: the badge chip, then one count per outcome kind that
/// occurred, then `surface` out to the edge.
fn title_line(
    theme: &'static super::theme::Theme,
    w: usize,
    counts: [(usize, char, Style); 3],
) -> Line<'static> {
    let inset = HORIZONTAL_PADDING.min(w / 2);
    let mut spans = vec![
        Span::styled(" ".repeat(inset), theme.settlement_surface),
        // The label names the component without competing with the action
        // status below. `badge` remains reserved for the active agent chrome.
        Span::styled(
            TITLE,
            theme
                .muted
                .add_modifier(Modifier::BOLD)
                .patch(theme.settlement_surface),
        ),
    ];
    let mut used = inset + TITLE.width();
    for (n, glyph, style) in counts {
        if n == 0 {
            continue;
        }
        let s = format!("  {glyph} {n}");
        used += s.width();
        spans.push(Span::styled(s, style.patch(theme.settlement_surface)));
    }
    spans.push(Span::styled(
        " ".repeat(w.saturating_sub(used)),
        theme.settlement_surface,
    ));
    Line::from(spans)
}

/// Draw the settlement card for `body` at `width` columns.
///
/// Every row is padded to the full width and carries `theme.settlement_surface`,
/// so the block reads as one surface rather than ragged text. Nothing is elided: the
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
    let (mut ok, mut bad, mut warn) = (0usize, 0usize, 0usize);
    for raw in body.lines() {
        match raw.trim_start().chars().next() {
            Some('✔') => ok += 1,
            Some('✘') => bad += 1,
            Some('⚠') => warn += 1,
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(body.lines().count() + 1);
    out.push(title_line(
        theme,
        w,
        [
            (ok, '✔', theme.settlement_ok),
            (bad, '✘', theme.settlement_error),
            (warn, '⚠', theme.settlement_warn),
        ],
    ));
    let inset = HORIZONTAL_PADDING.min(w / 2);
    let side_padding = Span::styled(" ".repeat(inset), theme.settlement_surface);
    let rail = Span::styled(
        RAIL,
        theme.settlement_accent.patch(theme.settlement_surface),
    );
    let rail_w = RAIL.width();
    let body_w = w.saturating_sub(2 * inset + rail_w).max(1);
    for raw in body.lines() {
        let trimmed = raw.trim_start();
        let glyph = trimmed.chars().next().unwrap_or(' ');
        let (status_style, copy_style) = row_styles(glyph, theme);
        let is_row = matches!(glyph, '✔' | '✘' | '⚠' | '~');
        let (head, text) = if is_row {
            let rest = trimmed.chars().skip(1).collect::<String>();
            (format!(" {glyph} "), rest.trim_start().to_string())
        } else {
            (
                " ".repeat(indent.saturating_sub(rail_w).max(1)),
                trimmed.to_string(),
            )
        };
        let head_w = head.width();
        let avail = body_w.saturating_sub(head_w).max(1);
        for (i, chunk) in wrap(&text, avail).into_iter().enumerate() {
            let prefix = if i == 0 {
                head.clone()
            } else {
                " ".repeat(head_w)
            };
            let line = pad(&format!("{prefix}{chunk}"), body_w);
            let body_spans = if is_row && i == 0 {
                // `glyph_width` is a display-column width, not a UTF-8 byte
                // offset: slicing `line[..glyph_width]` panics for ✔/✘/⚠.
                if glyph == '⚠' {
                    vec![Span::styled(line, copy_style)]
                } else {
                    let glyph_head = format!(" {glyph} ");
                    let remainder = pad(
                        &format!("{}{}", " ".repeat(head_w - glyph_head.width()), chunk),
                        body_w.saturating_sub(glyph_head.width()),
                    );
                    vec![
                        Span::styled(glyph_head, status_style),
                        Span::styled(remainder, copy_style),
                    ]
                }
            } else {
                vec![Span::styled(line, copy_style)]
            };
            let mut spans = vec![side_padding.clone(), rail.clone()];
            spans.extend(body_spans);
            spans.push(side_padding.clone());
            out.push(Line::from(spans));
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

    use super::super::theme::ANSI;
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
        let out = card_lines("  ✔ bash · cargo test", &ANSI, 40);
        assert!(!out.is_empty());
        for row in plain(&out) {
            assert_eq!(row.width(), 40, "ragged row: {row:?}");
        }
    }

    #[test]
    fn long_detail_wraps_instead_of_being_cut() {
        let body = format!("  ✘ parallel_jobs · {}", "e".repeat(200));
        let narrow = card_lines(&body, &ANSI, 40);
        let wide = card_lines(&body, &ANSI, 100);
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

    use super::super::theme::{CLAY, LIGHT, MUR};
    use super::{HORIZONTAL_PADDING, RAIL};
    use ratatui::style::Modifier;

    /// Settlement uses a quiet label, not the strong filled badge reserved for
    /// the active agent identity. Only outcome glyphs and counts carry status
    /// colour; the body copy remains readable, neutral text.
    #[test]
    fn title_and_rows_prioritize_content_over_alert_chrome() {
        for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
            let out = card_lines(
                "  ✔ bash · cargo test\n  ✘ edit · denied\n  ⚠ note · inspect output",
                theme,
                60,
            );
            let title = &out[0];
            let chip = title
                .spans
                .iter()
                .find(|s| s.content.contains("SETTLEMENT"))
                .expect("settlement label");
            assert_eq!(
                chip.style,
                theme
                    .muted
                    .add_modifier(Modifier::BOLD)
                    .patch(theme.settlement_surface)
            );
            assert!(
                !chip.style.add_modifier.contains(Modifier::REVERSED),
                "title must not use reverse-video banner treatment"
            );
            assert_ne!(
                chip.style.fg, theme.badge.fg,
                "title must not borrow the identity badge foreground"
            );

            let first_row = &out[1];
            assert_eq!(
                first_row.spans[1].style,
                theme.settlement_accent.patch(theme.settlement_surface)
            );
            assert_eq!(
                first_row.spans[2].style,
                theme.settlement_ok.patch(theme.settlement_surface)
            );
            assert_eq!(
                first_row.spans[3].style,
                theme.settlement_text.patch(theme.settlement_surface)
            );
        }
    }

    /// A left rail on every body row makes the block read as one unit even on
    /// `ansi`, whose `surface` paints no background at all.
    #[test]
    fn every_body_row_carries_the_accent_rail() {
        let out = card_lines("  ✔ bash\n  a note line", &ANSI, 40);
        assert_eq!(out.len(), 3);
        for line in &out[1..] {
            let rail = &line.spans[1];
            assert_eq!(rail.content.as_ref(), RAIL);
            assert_eq!(rail.style, ANSI.accent.patch(ANSI.settlement_surface));
        }
    }

    #[test]
    fn settlement_has_compact_horizontal_padding() {
        let out = card_lines("  ✔ bash", &ANSI, 40);
        assert_eq!(out.len(), 2);
        let title = &out[0];
        assert_eq!(
            title.spans[0].content.as_ref(),
            " ".repeat(HORIZONTAL_PADDING)
        );
        let body = &out[1];
        assert_eq!(
            body.spans[0].content.as_ref(),
            " ".repeat(HORIZONTAL_PADDING)
        );
        assert_eq!(
            body.spans.last().unwrap().content.as_ref(),
            " ".repeat(HORIZONTAL_PADDING)
        );
    }

    #[test]
    fn ansi_settlement_uses_a_quiet_surface_not_reverse_video() {
        assert!(
            !ANSI
                .settlement_surface
                .add_modifier
                .contains(Modifier::REVERSED),
            "a full reverse-video settlement overwhelms the terminal; the rail and title chip provide its boundary"
        );
    }

    #[test]
    fn every_skin_paints_a_distinct_settlement_surface() {
        for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
            let out = card_lines("  ✔ bash", theme, 40);
            assert_eq!(
                out.last()
                    .expect("card row")
                    .spans
                    .last()
                    .expect("right surface")
                    .style,
                theme.settlement_surface,
                "{} must paint its settlement boundary",
                super::super::theme::skin_name(theme),
            );
        }
    }

    #[test]
    fn the_card_names_itself() {
        let out = plain(&card_lines("  ✔ bash", &ANSI, 40));
        assert!(out[0].contains("SETTLEMENT"), "{out:?}");
    }
}
