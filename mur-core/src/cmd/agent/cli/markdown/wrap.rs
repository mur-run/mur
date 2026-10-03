//! Width-aware wrapping for prose rows (#1645).
//!
//! ratatui's `Wrap` only breaks at whitespace, so a run of CJK text — which
//! has no spaces — is one "word" to it: when the run does not fit, the whole
//! run moves down a row and the row above breaks far short of the edge. The
//! body indent was also prepended before that wrap, so only the first row of a
//! logical line sat under it. Pre-wrapping here, before the indent is added,
//! fixes both: every row fits its columns, so ratatui never wraps again and
//! the painted rows and the measured rows are the same rows.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// List markers a continuation row hangs under, so a wrapped bullet's text
/// lines up with its first word instead of with the bullet.
const BULLETS: [&str; 2] = ["• ", "▏ "];

/// CJK punctuation that must not open a row (line-start prohibition).
const NO_LINE_START: &str = "，。、；：！？）」』】》〉…,.;:!?)]}";

fn wide(c: char) -> bool {
    c.width().unwrap_or(0) >= 2
}

/// Wrap one styled line into rows of at most `w` display columns. A break may
/// fall at a space (the space is dropped) or between two characters when
/// either is wide. Continuation rows hang under the line's leading
/// indentation and list marker. Line-level style and alignment carry over.
pub(crate) fn wrap_line(line: &Line<'static>, w: usize) -> Vec<Line<'static>> {
    let w = w.max(1);
    if line.width() <= w {
        return vec![line.clone()];
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    let hang = hang_width(&chars).min(w / 2);
    let mut out = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < chars.len() {
        let lead = if first { 0 } else { hang };
        let room = w - lead;
        let mut end = start;
        let mut used = 0;
        let mut brk = None;
        while end < chars.len() {
            let cw = chars[end].0.width().unwrap_or(0);
            if used + cw > room {
                break;
            }
            if end > start && can_break_before(&chars, end) {
                brk = Some(end);
            }
            used += cw;
            end += 1;
        }
        if end < chars.len() {
            if can_break_before(&chars, end) {
                // The edge itself is a legal break.
            } else if let Some(b) = brk {
                end = b;
            }
        }
        if end == start {
            end = start + 1; // a glyph wider than the room goes alone
        }
        let mut row: Vec<(char, Style)> = Vec::with_capacity(end - start + lead);
        row.extend(std::iter::repeat_n((' ', Style::default()), lead));
        let mut piece = &chars[start..end];
        while let Some(((' ', _), rest)) = piece.split_last() {
            piece = rest;
        }
        row.extend_from_slice(piece);
        out.push(line_from_chars(&row, line));
        first = false;
        start = end;
        while start < chars.len() && chars[start].0 == ' ' {
            start += 1;
        }
    }
    if out.is_empty() {
        out.push(line.clone());
    }
    out
}

/// A row may start at `i` after a space, or where a wide glyph meets
/// anything — unless `chars[i]` is punctuation that must not open a row.
fn can_break_before(chars: &[(char, Style)], i: usize) -> bool {
    if i == 0 || i >= chars.len() {
        return false;
    }
    let (prev, cur) = (chars[i - 1].0, chars[i].0);
    if NO_LINE_START.contains(cur) {
        return false;
    }
    prev == ' ' || cur == ' ' || wide(prev) || wide(cur)
}

/// Columns of leading indentation plus a bullet / quote / `N. ` marker.
fn hang_width(chars: &[(char, Style)]) -> usize {
    let s: String = chars.iter().map(|(c, _)| *c).collect();
    let mut rest = s.as_str();
    let mut cols = 0;
    loop {
        let trimmed = rest.trim_start_matches(' ');
        cols += rest.len() - trimmed.len();
        rest = trimmed;
        if let Some(b) = BULLETS.iter().find(|b| rest.starts_with(**b)) {
            cols += unicode_width::UnicodeWidthStr::width(*b);
            rest = &rest[b.len()..];
            continue;
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(". ") {
            cols += digits + 2;
            rest = &rest[digits + 2..];
            continue;
        }
        return cols;
    }
}

/// Rebuild spans from styled characters, merging same-style neighbours.
fn line_from_chars(chars: &[(char, Style)], like: &Line<'static>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (c, style) in chars {
        match spans.last_mut() {
            Some(s) if s.style == *style => s.content.to_mut().push(*c),
            _ => spans.push(Span::styled(c.to_string(), *style)),
        }
    }
    let mut l = Line::from(spans).style(like.style);
    l.alignment = like.alignment;
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str, w: usize) -> Vec<String> {
        wrap_line(&Line::raw(s.to_string()), w)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn cjk_breaks_between_glyphs_at_the_edge() {
        assert_eq!(rows("那份 比較早的版本", 10), ["那份 比較", "早的版本"]);
    }

    #[test]
    fn latin_still_breaks_at_spaces_without_a_leading_space() {
        assert_eq!(rows("alpha beta gamma", 11), ["alpha beta", "gamma"]);
    }

    #[test]
    fn closing_punctuation_never_opens_a_row() {
        assert_eq!(rows("一二三四，五", 8), ["一二三", "四，五"]);
    }

    #[test]
    fn a_bullet_continuation_hangs_under_its_text() {
        assert_eq!(rows("• 一二三四五六", 8), ["• 一二三", "  四五六"]);
    }
}
