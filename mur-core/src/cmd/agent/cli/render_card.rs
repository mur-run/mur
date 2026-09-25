//! Ratatui lines for one in-transcript tool-call step card.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::call_summary::{arg_hint, hint_budget, intent_note, tool_description, width_of};
use super::step::{ARGS_MAX_LINES, StepCard, StepState};
use super::theme::Theme;

/// Maximum output lines shown inside a card before a "…+N more" truncation hint.
pub const OUTPUT_MAX_LINES: usize = 20;

/// Columns the `  → ` lead-in of a result gist occupies, charged against the
/// row before deciding whether a trailing intent still fits.
const GIST_CHROME_COLS: usize = 4;

/// Maximum changed/context rows shown on a collapsed edit card. The path header
/// is always retained; the full unbounded patch stays one Ctrl+O away.
const COLLAPSED_DIFF_PREVIEW_LINES: usize = 20;

/// Append a bounded edit preview and a clear route to the complete patch.
///
/// Tool cards live in a transcript, so allowing a large write to consume the
/// conversation makes both the agent's conclusion and the next approval hard
/// to find. The Ctrl+O overlay is already a full-screen, natively scrollable
/// representation and deliberately renders edit diffs without a cap.
fn push_collapsed_diff_preview(
    out: &mut Vec<Line<'static>>,
    diff_lines: Vec<Line<'static>>,
    theme: &'static Theme,
) {
    let path_rows = usize::from(diff_lines.first().is_some_and(|line| {
        line.spans
            .first()
            .is_some_and(|span| !span.content.starts_with("  "))
    }));
    let shown = path_rows + COLLAPSED_DIFF_PREVIEW_LINES;
    let hidden = diff_lines.len().saturating_sub(shown);
    out.extend(diff_lines.into_iter().take(shown));
    if hidden > 0 {
        out.push(Line::styled(
            format!(" … {hidden} more · Ctrl+O full diff"),
            theme.muted.add_modifier(Modifier::DIM),
        ));
    }
}

/// Turn a `StepCard` into renderable `Line`s for the transcript.
///
/// `expanded` controls verbosity. Collapsed (the default) shows a single
/// summary line — glyph, tool name, arg hint, a one-line result gist, and
/// duration — so a transcript of many tool calls stays scannable. Expanded
/// shows the full args + result (still bounded by `ARGS_MAX_LINES` /
/// `OUTPUT_MAX_LINES`). Errors and pending HITL rows always render in both
/// modes so nothing actionable is hidden. Full detail for a collapsed card is
/// always available in the Ctrl+O transcript overlay.
///
/// `width` is the terminal's column count: the header's arg hint is budgeted
/// from it rather than clipped at a fixed column, which used to cut a command
/// short at 40 characters and leave most of a wide row empty.
pub fn card_lines(
    card: &StepCard,
    theme: &'static Theme,
    expanded: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let budget = hint_budget(width);

    let accent = match card.state {
        StepState::Error => Style::default().fg(ratatui::style::Color::Red),
        _ => theme.accent,
    };

    // ── Header: glyph · name · arg-hint · duration ───────────────────────────
    //
    // The command always leads, in the accent weight, because the command is
    // the only thing that actually runs. A `description` is model-generated
    // prose that can drift from the call it describes, so it trails as a DIM
    // parenthetical — present as context, never mistakable for the subject.
    // It used to hold the header line with the command indented beneath it,
    // which trained the eye to read the paraphrase and skim the receipt.
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    let hint = arg_hint(card, budget);
    let header = format!("{} {} {}", card.glyph(), card.name, hint);
    let auto_tag = if card.auto_approved {
        Span::styled(" [auto]", theme.muted.add_modifier(Modifier::DIM))
    } else {
        Span::raw("")
    };
    let mut header_spans = vec![Span::styled(
        header.clone(),
        accent.add_modifier(Modifier::BOLD),
    )];
    let gist = (!expanded && card.error.is_none())
        .then(|| result_gist(card, budget))
        .flatten();
    if let Some(gist) = &gist {
        header_spans.push(Span::styled(format!("  → {gist}"), theme.muted));
    }
    header_spans.push(Span::styled(dur.clone(), theme.muted));
    header_spans.push(auto_tag);
    // Sized against what the row has already spent, so a long command pushes
    // the intent out rather than wrapping the line that carries the command.
    let spent = width_of(&header)
        + gist
            .as_deref()
            .map_or(0, |g| width_of(g) + GIST_CHROME_COLS)
        + width_of(&dur);
    if let Some(note) = intent_note(tool_description(card).as_deref(), spent, width) {
        header_spans.push(Span::styled(note, theme.muted.add_modifier(Modifier::DIM)));
    }
    out.push(Line::from(header_spans));

    // Collapsed cards keep the transcript compact, but edits need a visible
    // receipt: include their diff and let the transcript viewport scroll it.
    if !expanded {
        if card.error.is_none()
            && let Some(diff_lines) = super::diff::edit_diff_lines(&card.name, &card.args, theme)
        {
            push_collapsed_diff_preview(&mut out, diff_lines, theme);
        }
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
            out.push(Line::styled(format!(" {l}"), theme.muted));
        }
        if total_lines > ARGS_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total_lines - ARGS_MAX_LINES),
                theme.muted.add_modifier(Modifier::DIM),
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
            out.push(Line::styled(format!(" {l}"), theme.text));
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
                theme.muted.add_modifier(Modifier::DIM),
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
        Span::styled(" approve  ", theme.muted),
        Span::styled(
            "[a]",
            Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" always  ", theme.muted),
        Span::styled(
            "[n]",
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" deny / Esc", theme.muted),
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
fn result_gist(card: &StepCard, budget: usize) -> Option<String> {
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
        let end = out.floor_char_boundary(budget);
        return Some(if out.len() > budget {
            format!("{}…", &out[..end])
        } else {
            out.to_string()
        });
    }
    Some(format!("{lines} lines"))
}

#[cfg(test)]
mod tests {
    use super::card_lines;
    use crate::cmd::agent::cli::call_summary::{arg_hint, hint_budget};
    use crate::cmd::agent::cli::step::{CallOutcome, StepCard};
    use crate::cmd::agent::cli::theme;
    use ratatui::style::{Modifier, Style};

    /// A conventional 80-column terminal, so these assertions keep testing the
    /// card and not the width math.
    const TEST_WIDTH: u16 = 80;

    #[test]
    fn running_card_shows_glyph_name_and_no_result() {
        let c = StepCard::new(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": "a.rs" }),
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
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
        c.complete(CallOutcome::Ok, "412 lines".into(), false, 9, None, 8);
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
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

    fn done_card(args: serde_json::Value) -> StepCard {
        let mut c = StepCard::new("s".into(), "bash".into(), args);
        c.state = crate::cmd::agent::cli::step::StepState::Done;
        c.output = "version = \"2.71.7\"".into();
        c.duration_ms = Some(21);
        c
    }

    fn rows(card: &StepCard) -> Vec<String> {
        rows_at(card, TEST_WIDTH)
    }

    /// Same, at an explicit width — the intent note is width-dependent, so a
    /// test about it must say which terminal it is describing.
    fn rows_at(card: &StepCard, width: u16) -> Vec<String> {
        super::card_lines(card, &theme::ANSI, false, width)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// Spans, not flattened text: the point of the demotion is the STYLE
    /// difference between the command and the intent, which a joined string
    /// cannot show.
    fn spans(card: &StepCard) -> Vec<(String, Style)> {
        super::card_lines(card, &theme::ANSI, false, 200)
            .first()
            .expect("a header row")
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    /// The command leads and the intent trails it, dim. The old layout put the
    /// model's paraphrase on the header with the command indented beneath, so
    /// the reviewable thing was the subordinate one.
    #[test]
    fn a_description_trails_the_command_as_dim_metadata() {
        let card = done_card(serde_json::json!({
            "description": "Checking the workspace version",
            "command": "grep -m1 '^version' Cargo.toml"
        }));
        let r = rows_at(&card, 200);
        assert_eq!(r.len(), 1, "one row, not a split card: {r:?}");
        let cmd_at = r[0].find("grep").expect("command on the row");
        let intent_at = r[0].find("Checking").expect("intent on the row");
        assert!(cmd_at < intent_at, "command must lead: {r:?}");
        assert!(r[0].contains("21ms"), "{r:?}");

        let intent = spans(&card)
            .into_iter()
            .find(|(t, _)| t.contains("Checking"))
            .expect("intent span");
        assert!(
            intent.1.add_modifier.contains(Modifier::DIM),
            "intent must be dim: {intent:?}"
        );
        assert!(
            !intent.1.add_modifier.contains(Modifier::BOLD),
            "intent must not be bold: {intent:?}"
        );
    }

    /// The command keeps the bold accent it always had — demoting the intent
    /// must not also demote the thing the intent sits beside.
    #[test]
    fn the_command_stays_the_brightest_thing_on_the_row() {
        let card = done_card(serde_json::json!({
            "description": "Checking the workspace version",
            "command": "grep -m1 '^version' Cargo.toml"
        }));
        let cmd = spans(&card)
            .into_iter()
            .find(|(t, _)| t.contains("grep"))
            .expect("command span");
        assert!(
            cmd.1.add_modifier.contains(Modifier::BOLD),
            "command must stay bold: {cmd:?}"
        );
    }

    /// A narrow row spends its columns on the command, not the paraphrase.
    #[test]
    fn a_tight_row_drops_the_intent_rather_than_the_command() {
        let card = done_card(serde_json::json!({
            "description": "Checking the workspace version of the crate",
            "command": "grep -m1 '^version' Cargo.toml"
        }));
        let text: String = super::card_lines(&card, &theme::ANSI, false, 44)
            .first()
            .expect("a header row")
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("grep"), "command must survive: {text}");
        assert!(
            !text.contains("Checking"),
            "intent must be dropped when tight: {text}"
        );
    }

    /// Control — every tool that sends no description renders exactly as before.
    /// This is what makes the change additive rather than a rewrite.
    #[test]
    fn without_a_description_the_card_is_unchanged() {
        let r = rows(&done_card(serde_json::json!({
            "command": "grep -m1 '^version' Cargo.toml"
        })));
        assert_eq!(r.len(), 1, "still one line: {r:?}");
        assert!(r[0].contains("bash"), "{r:?}");
        assert!(r[0].contains("grep -m1"), "{r:?}");
        assert!(r[0].contains("21ms"), "{r:?}");
    }

    /// An empty or whitespace description adds no parenthetical at all.
    #[test]
    fn a_blank_description_adds_no_note() {
        for d in ["", "   "] {
            let r = rows(&done_card(serde_json::json!({
                "description": d,
                "command": "ls"
            })));
            assert_eq!(r.len(), 1, "description {d:?} should not split: {r:?}");
            assert!(
                !r[0].contains('('),
                "blank description {d:?} must add no note: {r:?}"
            );
        }
    }

    /// The real line from a real session, at the 80-column design baseline.
    /// Middle-elision produced `grep -m1 '^version'… head -20 Cargo.toml`,
    /// which reads as though the `||` fallback ran. It did not — grep
    /// succeeded.

    #[test]
    fn a_shell_command_keeps_the_branch_that_ran() {
        let cmd = "grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 Cargo.toml";
        let mut card = StepCard::new(
            "s".into(),
            "bash".into(),
            serde_json::json!({ "command": cmd }),
        );
        card.state = crate::cmd::agent::cli::step::StepState::Done;

        for width in [60u16, 80, 92] {
            let hint = arg_hint(&card, hint_budget(width));
            assert!(
                hint.starts_with("grep -m1"),
                "width {width}: the program must survive: {hint}"
            );
            assert!(
                !hint.contains("head -20"),
                "width {width}: reported a fallback branch that never ran: {hint}"
            );
        }
    }

    /// Negative control on the ROUTING: a path must still elide the middle, so
    /// two file calls differing only in the filename stay distinguishable.
    /// Flipping paths to head-keep would render them identically.
    #[test]
    fn two_paths_differing_only_in_filename_stay_distinct() {
        let hint_for = |path: &str| {
            let mut card = StepCard::new(
                "s".into(),
                "read_file".into(),
                serde_json::json!({ "path": path }),
            );
            card.state = crate::cmd::agent::cli::step::StepState::Done;
            arg_hint(&card, 24)
        };
        let a = hint_for("mur-core/src/cmd/agent/cli/alpha.rs");
        let b = hint_for("mur-core/src/cmd/agent/cli/omega.rs");
        assert!(a.contains('…'), "expected elision at this budget: {a}");
        assert_ne!(
            a, b,
            "two file calls must not render identically: {a} / {b}"
        );
    }

    /// The cost of head-keep, stated rather than hidden: two commands that
    /// differ only in their tail DO collapse to the same hint. That is accepted
    /// — the header is a hint and expanding shows the full command, whereas
    /// keeping the tail actively reports a branch that never ran.
    #[test]
    fn head_keep_trades_tail_detail_for_an_honest_front() {
        let hint_for = |cmd: &str| {
            let mut card = StepCard::new(
                "s".into(),
                "bash".into(),
                serde_json::json!({ "command": cmd }),
            );
            card.state = crate::cmd::agent::cli::step::StepState::Done;
            arg_hint(&card, 30)
        };
        let a = hint_for("grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 a.toml");
        let b = hint_for("grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 b.toml");
        assert_eq!(a, b, "documented trade-off");
        assert!(a.starts_with("grep -m1"), "{a}");
    }

    #[test]
    fn arg_hint_does_not_panic_on_long_multibyte_path() {
        let long_cjk = "檔".repeat(50); // 3 bytes/char → ~150 bytes, well past 40
        let c = StepCard::new(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": long_cjk }),
        );
        let _ = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH); // must NOT panic
    }

    #[test]
    fn error_card_shows_red_marker_and_message() {
        let mut c = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        c.complete(
            CallOutcome::Failed,
            "boom".into(),
            false,
            4,
            Some("exit 101".into()),
            3,
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
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
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
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
    fn collapsed_edit_card_shows_a_compact_diff_preview() {
        let c = StepCard::new(
            "s1".into(),
            "edit_file".into(),
            serde_json::json!({
                "path":"a.rs",
                "old_string":"old line",
                "new_string":"new line"
            }),
        );
        let text = joined(&card_lines(
            &c,
            theme::resolve_skin("dark"),
            false,
            TEST_WIDTH,
        ));
        assert!(text.contains("- old line"), "expected removal in: {text}");
        assert!(text.contains("+ new line"), "expected addition in: {text}");
        assert!(
            !text.contains("\"old_string\""),
            "raw JSON must not appear in: {text}"
        );
    }

    #[test]
    fn long_collapsed_preview_is_bounded_with_a_full_diff_affordance() {
        let content = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = StepCard::new(
            "s1".into(),
            "write_file".into(),
            serde_json::json!({ "path": "big.rs", "content": content }),
        );
        let text = joined(&card_lines(
            &c,
            theme::resolve_skin("dark"),
            false,
            TEST_WIDTH,
        ));
        assert!(text.contains("+ line 0"), "first diff row missing: {text}");
        assert!(
            text.contains("… 10 more · Ctrl+O full diff"),
            "missing affordance: {text}"
        );
        assert!(
            !text.contains("+ line 29"),
            "preview must be bounded: {text}"
        );
    }

    #[test]
    fn collapsed_card_folds_result_count_into_one_line() {
        let mut c = StepCard::new(
            "s1".into(),
            "mur_project_search".into(),
            serde_json::json!({ "query": "workflow" }),
        );
        c.complete(
            CallOutcome::Ok,
            r#"{"count":0,"results":[]}"#.into(),
            false,
            24,
            None,
            328,
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"), false, TEST_WIDTH);
        assert_eq!(lines.len(), 1, "collapsed card must be one line: {lines:?}");
        let text = joined(&lines);
        assert!(text.contains("0 results"), "expected gist in: {text}");
        assert!(text.contains("328ms"), "expected duration in: {text}");
        assert!(!text.contains("\"results\""), "raw JSON leaked: {text}");
    }

    #[test]
    fn collapsed_card_still_shows_errors_and_hitl() {
        let mut c = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        c.complete(
            CallOutcome::Failed,
            "boom".into(),
            false,
            4,
            Some("exit 101".into()),
            3,
        );
        c.awaiting_hitl = true;
        let text = joined(&card_lines(
            &c,
            theme::resolve_skin("dark"),
            false,
            TEST_WIDTH,
        ));
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
        c.complete(CallOutcome::Ok, "patched".into(), false, 1, None, 4);
        c.awaiting_hitl = true;
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("[y]"));
        assert!(text.contains("approve"));
        assert!(text.contains("[n]"));
    }
}
