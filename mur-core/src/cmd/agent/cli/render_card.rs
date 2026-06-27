//! Ratatui lines for one in-transcript tool-call step card.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::step::{ARGS_MAX_LINES, StepCard, StepState};
use super::theme::Theme;

/// Maximum output lines shown inside a card before a "…+N more" truncation hint.
pub const OUTPUT_MAX_LINES: usize = 20;

/// Turn a `StepCard` into renderable `Line`s for the transcript.
///
/// Each card is expanded by default (full args + result) but bounded by
/// `ARGS_MAX_LINES` and `OUTPUT_MAX_LINES` so a huge tool response can't
/// flood the transcript.
pub fn card_lines(card: &StepCard, theme: &'static Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    let accent = match card.state {
        StepState::Error => ratatui::style::Color::Red,
        _ => theme.agent,
    };

    // ── Header: glyph · name · arg-hint · duration ───────────────────────────
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    let header = format!("{} {} {}", card.glyph(), card.name, arg_hint(card));
    out.push(Line::from(vec![
        Span::styled(
            header,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(dur, Style::default().fg(theme.system)),
    ]));

    // ── Args (bounded) ────────────────────────────────────────────────────────
    if !card.args.is_null() {
        let pretty = serde_json::to_string_pretty(&card.args).unwrap_or_default();
        let total_lines = pretty.lines().count();
        for l in pretty.lines().take(ARGS_MAX_LINES) {
            out.push(Line::styled(
                format!(" {l}"),
                Style::default().fg(theme.system),
            ));
        }
        if total_lines > ARGS_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total_lines - ARGS_MAX_LINES),
                Style::default()
                    .fg(theme.system)
                    .add_modifier(Modifier::DIM),
            ));
        }
    }

    // ── Result / error (bounded) ──────────────────────────────────────────────
    if let Some(err) = &card.error {
        out.push(Line::styled(
            format!(" ✗ {err}"),
            Style::default().fg(ratatui::style::Color::Red),
        ));
    }

    if !card.output.is_empty() {
        let output_line_count = card.output.lines().count();
        for l in card.output.lines().take(OUTPUT_MAX_LINES) {
            out.push(Line::styled(
                format!(" {l}"),
                Style::default().fg(theme.agent_text),
            ));
        }
        let shown = output_line_count.min(OUTPUT_MAX_LINES);
        // Show "+N more" either when we clipped locally OR the runtime
        // already truncated the output (full_len > what we received).
        let total = if card.truncated {
            card.full_len
        } else {
            output_line_count
        };
        if card.truncated || output_line_count > OUTPUT_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total.saturating_sub(shown)),
                Style::default()
                    .fg(theme.system)
                    .add_modifier(Modifier::DIM),
            ));
        }
    }

    out
}

/// Compact first-scalar-arg hint for the header line (e.g. file path, query).
/// Clips at 40 chars so long paths don't wrap the header.
fn arg_hint(card: &StepCard) -> String {
    card.args
        .as_object()
        .and_then(|m| m.values().find_map(|v| v.as_str()))
        .map(|s| {
            if s.len() > 40 {
                format!("{}…", &s[..40])
            } else {
                s.to_string()
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::card_lines;
    use crate::cmd::agent::cli::step::StepCard;
    use crate::cmd::agent::cli::theme;

    #[test]
    fn running_card_shows_glyph_name_and_no_result() {
        let c = StepCard::new(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": "a.rs" }),
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"));
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("|");
        assert!(text.contains("read"), "expected 'read' in: {text}");
        assert!(text.contains('◐'), "expected '◐' in: {text}");
    }

    #[test]
    fn done_card_shows_output_and_duration() {
        let mut c = StepCard::new("s1".into(), "read".into(), serde_json::json!({}));
        c.complete(true, "412 lines".into(), false, 9, None, 8);
        let lines = card_lines(&c, theme::resolve_skin("dark"));
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            text.contains("412 lines"),
            "expected '412 lines' in: {text}"
        );
        assert!(text.contains("8ms"), "expected '8ms' in: {text}");
        assert!(text.contains('✔'), "expected '✔' in: {text}");
    }
}
