//! Ratatui lines for one in-transcript tool-call step card.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::step::{ARGS_MAX_LINES, StepCard, StepState};
use super::theme::Theme;

/// Maximum output lines shown inside a card before a "…+N more" truncation hint.
pub const OUTPUT_MAX_LINES: usize = 20;

/// Turn a `StepCard` into renderable `Line`s for the transcript.
///
/// `expanded` controls verbosity. Collapsed (the default) shows a single
/// summary line — glyph, tool name, arg hint, a one-line result gist, and
/// duration — so a transcript of many tool calls stays scannable. Expanded
/// shows the full args + result (still bounded by `ARGS_MAX_LINES` /
/// `OUTPUT_MAX_LINES`). Errors and pending HITL rows always render in both
/// modes so nothing actionable is hidden. Full detail for a collapsed card is
/// always available in the Ctrl+O transcript overlay.
pub fn card_lines(card: &StepCard, theme: &'static Theme, expanded: bool) -> Vec<Line<'static>> {
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
    let auto_tag = if card.auto_approved {
        Span::styled(
            " [read · auto]",
            Style::default()
                .fg(theme.system)
                .add_modifier(Modifier::DIM),
        )
    } else {
        Span::raw("")
    };
    let mut header_spans = vec![Span::styled(
        header,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    // Collapsed: fold a one-line result gist into the header so the whole card
    // is a single scannable line (unless there's a detailed error to show).
    if !expanded
        && card.error.is_none()
        && let Some(gist) = result_gist(card)
    {
        header_spans.push(Span::styled(
            format!("  → {gist}"),
            Style::default().fg(theme.system),
        ));
    }
    header_spans.push(Span::styled(dur, Style::default().fg(theme.system)));
    header_spans.push(auto_tag);
    out.push(Line::from(header_spans));

    // Collapsed cards stop after the header (plus any error / HITL rows below).
    if !expanded {
        push_error_and_hitl(&mut out, card, theme);
        return out;
    }

    // ── Args: diff for edit tools, else bounded JSON ─────────────────────────
    if let Some(diff_lines) = super::diff::edit_diff_lines(&card.name, &card.args, theme) {
        out.extend(diff_lines);
    } else if !card.args.is_null() {
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
    if let Some(line) = error_line(card) {
        out.push(line);
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

    // ── Inline HITL approval (P2) ────────────────────────────────────────────
    if card.awaiting_hitl {
        out.push(hitl_row(theme));
    }

    out
}

/// Error line for a card, or `None` when the tool succeeded. Shown in both
/// collapsed and expanded modes.
fn error_line(card: &StepCard) -> Option<Line<'static>> {
    card.error.as_ref().map(|err| {
        Line::styled(
            format!(" ✗ {err}"),
            Style::default().fg(ratatui::style::Color::Red),
        )
    })
}

/// The `[y] approve [a] always [n] deny` inline-HITL prompt row.
fn hitl_row(theme: &'static Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "  [y]",
            Style::default()
                .fg(ratatui::style::Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" approve  ", Style::default().fg(theme.system)),
        Span::styled(
            "[a]",
            Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" always  ", Style::default().fg(theme.system)),
        Span::styled(
            "[n]",
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" deny / Esc", Style::default().fg(theme.system)),
    ])
}

/// Collapsed-card tail: error line (if any) + pending-HITL row (if any). The
/// success gist is folded into the header instead.
fn push_error_and_hitl(out: &mut Vec<Line<'static>>, card: &StepCard, theme: &'static Theme) {
    if let Some(line) = error_line(card) {
        out.push(line);
    }
    if card.awaiting_hitl {
        out.push(hitl_row(theme));
    }
}

/// One-line gist of a successful tool result for the collapsed header. Best
/// effort: for JSON results, count the obvious result set (`count`, or the
/// length of a `results`/`matches`/`items` array); otherwise fall back to a
/// short inline value or a line/char count. `None` when there's nothing useful
/// to say (empty output).
fn result_gist(card: &StepCard) -> Option<String> {
    let out = card.output.trim();
    if out.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(out) {
        if let Some(n) = v.get("count").and_then(serde_json::Value::as_u64) {
            return Some(format!("{n} results"));
        }
        for key in ["results", "matches", "items"] {
            if let Some(arr) = v.get(key).and_then(serde_json::Value::as_array) {
                return Some(format!("{} {key}", arr.len()));
            }
        }
    }
    // Non-JSON (or unrecognised shape): single short line inline, else counts.
    let lines = out.lines().count();
    if lines <= 1 {
        let end = out.floor_char_boundary(ARG_HINT_MAX);
        return Some(if out.len() > ARG_HINT_MAX {
            format!("{}…", &out[..end])
        } else {
            out.to_string()
        });
    }
    Some(format!("{lines} lines"))
}

/// Maximum byte length of the arg hint before truncation.
const ARG_HINT_MAX: usize = 40;

/// Compact first-scalar-arg hint for the header line (e.g. file path, query).
/// Clips at `ARG_HINT_MAX` chars so long paths don't wrap the header.
/// Uses `floor_char_boundary` to avoid panicking on multi-byte chars (CJK, emoji).
fn arg_hint(card: &StepCard) -> String {
    card.args
        .as_object()
        .and_then(|m| m.values().find_map(|v| v.as_str()))
        .map(|s| {
            if s.len() > ARG_HINT_MAX {
                let end = s.floor_char_boundary(ARG_HINT_MAX);
                format!("{}…", &s[..end])
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
        let lines = card_lines(&c, theme::resolve_skin("dark"), true);
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
        let lines = card_lines(&c, theme::resolve_skin("dark"), true);
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

    #[test]
    fn arg_hint_does_not_panic_on_long_multibyte_path() {
        let long_cjk = "檔".repeat(50); // 3 bytes/char → ~150 bytes, well past 40
        let c = StepCard::new(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": long_cjk }),
        );
        let _ = card_lines(&c, theme::resolve_skin("dark"), true); // must NOT panic
    }

    #[test]
    fn error_card_shows_red_marker_and_message() {
        let mut c = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        c.complete(false, "boom".into(), false, 4, Some("exit 101".into()), 3);
        let lines = card_lines(&c, theme::resolve_skin("dark"), true);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("exit 101"), "expected 'exit 101' in: {text}");
        assert!(text.contains('✗'), "expected '✗' in: {text}");
    }

    #[test]
    fn edit_card_renders_diff_not_raw_json() {
        let c = StepCard::new(
            "s1".into(),
            "edit".into(),
            serde_json::json!({"file_path":"a.rs","old_string":"old","new_string":"new"}),
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"), true);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("- old"), "expected '- old' in:\n{text}");
        assert!(text.contains("+ new"), "expected '+ new' in:\n{text}");
        // raw JSON key must NOT appear for an edit card
        assert!(
            !text.contains("\"old_string\""),
            "raw JSON key must not appear in:\n{text}"
        );
    }

    fn joined(lines: &[ratatui::text::Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn collapsed_card_folds_result_count_into_one_line() {
        let mut c = StepCard::new(
            "s1".into(),
            "mur_project_search".into(),
            serde_json::json!({ "query": "workflow" }),
        );
        c.complete(
            true,
            r#"{"count":0,"results":[]}"#.into(),
            false,
            24,
            None,
            328,
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"), false);
        assert_eq!(lines.len(), 1, "collapsed card must be one line: {lines:?}");
        let text = joined(&lines);
        assert!(text.contains("0 results"), "expected gist in: {text}");
        assert!(text.contains("328ms"), "expected duration in: {text}");
        assert!(!text.contains("\"results\""), "raw JSON leaked: {text}");
    }

    #[test]
    fn collapsed_card_still_shows_errors_and_hitl() {
        let mut c = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        c.complete(false, "boom".into(), false, 4, Some("exit 101".into()), 3);
        c.awaiting_hitl = true;
        let text = joined(&card_lines(&c, theme::resolve_skin("dark"), false));
        assert!(
            text.contains("exit 101"),
            "error must show collapsed: {text}"
        );
        assert!(text.contains("[y]"), "HITL must show collapsed: {text}");
    }

    #[test]
    fn awaiting_card_shows_inline_approval_row() {
        let mut c = StepCard::new(
            "s1".into(),
            "edit".into(),
            serde_json::json!({"file_path":"a.rs"}),
        );
        c.complete(true, "patched".into(), false, 1, None, 4);
        c.awaiting_hitl = true;
        let lines = card_lines(&c, theme::resolve_skin("dark"), true);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("[y]"));
        assert!(text.contains("approve"));
        assert!(text.contains("[n]"));
    }
}
