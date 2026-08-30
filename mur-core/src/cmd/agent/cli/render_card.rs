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
        StepState::Error => ratatui::style::Color::Red,
        _ => theme.agent,
    };

    // ── Header: glyph · name · arg-hint · duration ───────────────────────────
    //
    // With a `description` the card splits in two: the intent on the header
    // line, the command indented under a `⎿`. A command says what ran and
    // cannot say what for, and the model is the only thing that knows —
    // deriving a subject from the command text would just restate the line
    // below it in worse words.
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    let subject = tool_description(card);
    let header = match &subject {
        Some(d) => format!("{} {}", card.glyph(), d),
        None => format!("{} {} {}", card.glyph(), card.name, arg_hint(card, budget)),
    };
    let auto_tag = if card.auto_approved {
        Span::styled(
            " [auto]",
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
    // Without a subject the gist folds into the header, keeping the whole card
    // one scannable line. With one it belongs on the `⎿` row beside the command
    // it came from.
    if subject.is_none()
        && !expanded
        && card.error.is_none()
        && let Some(gist) = result_gist(card, budget)
    {
        header_spans.push(Span::styled(
            format!("  → {gist}"),
            Style::default().fg(theme.system),
        ));
    }
    if subject.is_none() {
        header_spans.push(Span::styled(dur.clone(), Style::default().fg(theme.system)));
    }
    header_spans.push(auto_tag);
    out.push(Line::from(header_spans));

    // The command row. Dim throughout: the subject above is what the eye should
    // land on, and this is the receipt underneath it.
    if subject.is_some() {
        let mut row = vec![
            Span::styled(
                "  ⎿ ",
                Style::default()
                    .fg(theme.system)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                arg_hint(card, budget),
                Style::default()
                    .fg(theme.system)
                    .add_modifier(Modifier::DIM),
            ),
        ];
        if !expanded
            && card.error.is_none()
            && let Some(gist) = result_gist(card, budget)
        {
            row.push(Span::styled(
                format!("  → {gist}"),
                Style::default()
                    .fg(theme.system)
                    .add_modifier(Modifier::DIM),
            ));
        }
        row.push(Span::styled(
            dur,
            Style::default()
                .fg(theme.system)
                .add_modifier(Modifier::DIM),
        ));
        out.push(Line::from(row));
    }

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

/// Columns the header spends on everything that is not the arg hint: the state
/// glyph, the tool name, the separators and the right-hand result gist.
const HEADER_OVERHEAD: usize = 40;

/// Floor for the arg hint on a narrow terminal — below this the header would be
/// an ellipsis with barely any command in front of it, which says nothing.
const ARG_HINT_MIN: usize = 24;

/// Bytes of arg hint that fit on one header row at `width` columns.
///
/// Was a flat 40 regardless of terminal size: on a 120-column terminal the
/// command was cut two thirds of the way short with the rest of the row left
/// empty, and on a narrow one the header still wrapped. An 80-column terminal
/// lands back on the old 40 — this widens the wide case, it does not re-tune
/// the common one.
fn hint_budget(width: u16) -> usize {
    usize::from(width)
        .saturating_sub(HEADER_OVERHEAD)
        .max(ARG_HINT_MIN)
}

/// One display column carved out of the hint budget for the `…`
/// middle-elision marker itself, so `elide_middle` never returns something
/// longer than `budget` columns. The budget arithmetic here is byte-based,
/// not column-based, so this (like the rest of the budget) is only an
/// approximation of display width once the string contains wide characters.
const ELLIPSIS_OVERHEAD: usize = 1;

/// Session-constant prefix every bash command shares when the agent re-`cd`s
/// into the working directory before each call. Stripping it before hinting
/// is what stops eighteen rows in a row from sharing one useless 55-byte
/// prefix and differing only in the part that gets truncated away.
fn strip_cd_prefix(cmd: &str) -> &str {
    let Some(rest) = cmd.strip_prefix("cd ") else {
        return cmd;
    };
    match rest.find(" && ") {
        Some(i) => &rest[i + 4..],
        None => cmd,
    }
}

/// Pick the field that actually describes what a tool call did: the command
/// for bash, the path for file tools, falling back to the first string value
/// in `args` (the old behaviour, which picked an arbitrary key because
/// `serde_json::Map` sorts alphabetically) only when neither is present.
fn hint_field(card: &StepCard) -> Option<&str> {
    let obj = card.args.as_object()?;
    if card.name.eq_ignore_ascii_case("bash")
        && let Some(cmd) = obj.get("command").and_then(|v| v.as_str())
    {
        return Some(cmd);
    }
    if let Some(path) = obj
        .get("file_path")
        .or_else(|| obj.get("path"))
        .and_then(|v| v.as_str())
    {
        return Some(path);
    }
    obj.values().find_map(|v| v.as_str())
}

/// Truncate `s` to `budget` bytes by cutting out of the middle and keeping
/// head and tail, so the distinguishing suffix (a filename, a flag, the part
/// that differs between two otherwise-identical commands) survives instead
/// of being the first thing cut. Uses `floor_char_boundary`/
/// `ceil_char_boundary` so multi-byte chars (CJK, emoji) can't be split.
/// Keep the HEAD, cut at a word boundary.
///
/// For a shell command the identifying part is the front — the program and its
/// first arguments — and the tail is routinely a branch that did not run.
/// Middle-elision at 80 columns turned
/// `grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 Cargo.toml`
/// into `grep -m1 '^version'… head -20 Cargo.toml`, which reads as though the
/// fallback executed. At 60 it produced `grep -m1 '^…0 Cargo.toml`.
///
/// Cutting at the last space keeps the hint from ending mid-token, unless that
/// would throw away most of the budget on a single long word.
fn elide_tail_at_word(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let keep = budget.saturating_sub(ELLIPSIS_OVERHEAD);
    let end = s.floor_char_boundary(keep);
    let cut = match s[..end].rfind(' ') {
        // Backing up past half the budget costs more than the ragged edge.
        Some(i) if i * 2 >= end => i,
        _ => end,
    };
    format!("{}…", s[..cut].trim_end())
}

fn elide_middle(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let keep = budget.saturating_sub(ELLIPSIS_OVERHEAD);
    let head_len = keep / 2;
    let tail_len = keep - head_len;
    let head_end = s.floor_char_boundary(head_len);
    let tail_start = s.ceil_char_boundary(s.len().saturating_sub(tail_len));
    if tail_start <= head_end {
        // Budget too small to fit both a head and a tail — fall back to a
        // plain head clip rather than emit an empty or malformed hint.
        let end = s.floor_char_boundary(budget);
        return format!("{}…", &s[..end]);
    }
    format!("{}…{}", &s[..head_end], &s[tail_start..])
}

/// Compact arg hint for the header line (e.g. bash command, file path, query).
///
/// Which end survives truncation depends on where the identity lives. Two calls
/// to a file tool differ in the FILENAME, so paths elide the middle and keep
/// both ends. Two `bash` calls differ in the PROGRAM and its first arguments,
/// and a command's tail is routinely `|| fallback` or `2>/dev/null` — keeping
/// it while dropping the front reports a branch that never ran.
///
/// Bash also loses a leading `cd <path> && `: session state repeated on every
/// row, carrying nothing that distinguishes one call from the next.
/// The model's own one-line account of why it is running this, when it gave one.
///
/// Blank or whitespace reads as absent: a card that renders an empty subject
/// line is worse than one that never split.
fn tool_description(card: &StepCard) -> Option<String> {
    let d = card.args.as_object()?.get("description")?.as_str()?.trim();
    (!d.is_empty()).then(|| d.to_string())
}

fn arg_hint(card: &StepCard, budget: usize) -> String {
    let Some(raw) = hint_field(card) else {
        return String::new();
    };
    if card.name.eq_ignore_ascii_case("bash") {
        elide_tail_at_word(strip_cd_prefix(raw), budget)
    } else {
        elide_middle(raw, budget)
    }
}

#[cfg(test)]
mod tests {
    use super::{ARG_HINT_MIN, card_lines, elide_middle, hint_budget, hint_field, strip_cd_prefix};
    use crate::cmd::agent::cli::step::{CallOutcome, StepCard};
    use crate::cmd::agent::cli::theme;

    /// A conventional 80-column terminal, so these assertions keep testing the
    /// card and not the width math.
    const TEST_WIDTH: u16 = 80;

    #[test]
    fn hint_budget_grows_with_the_terminal_and_has_a_floor() {
        // The bug: a flat 40 columns of command on every terminal, so a wide
        // one showed `… ` with two thirds of the row empty.
        assert!(hint_budget(200) > hint_budget(TEST_WIDTH));
        assert_eq!(hint_budget(40), ARG_HINT_MIN);
        assert_eq!(hint_budget(0), ARG_HINT_MIN);
    }

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
        super::card_lines(card, &theme::DARK, false, TEST_WIDTH)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// With a description the card splits: intent on top, command underneath.
    #[test]
    fn a_description_puts_the_intent_first_and_the_command_under_it() {
        let r = rows(&done_card(serde_json::json!({
            "description": "Checking the workspace version",
            "command": "grep -m1 '^version' Cargo.toml"
        })));
        assert!(
            r[0].contains("Checking the workspace version"),
            "intent must lead: {r:?}"
        );
        assert!(!r[0].contains("grep"), "the command belongs below: {r:?}");
        assert!(r[1].contains('⎿'), "expected a command row: {r:?}");
        assert!(r[1].contains("grep -m1"), "{r:?}");
        assert!(
            r[1].contains("21ms"),
            "timing rides with the command: {r:?}"
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

    /// An empty or whitespace description is absent, not a blank subject line.
    #[test]
    fn a_blank_description_does_not_split_the_card() {
        for d in ["", "   "] {
            let r = rows(&done_card(serde_json::json!({
                "description": d,
                "command": "ls"
            })));
            assert_eq!(r.len(), 1, "description {d:?} should not split: {r:?}");
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
            let hint = super::arg_hint(&card, super::hint_budget(width));
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

    /// No half-tokens. Middle-elision produced `Cargo…ull` at 92 columns and
    /// `'^…0` at 60 — fragments that mean nothing.
    #[test]
    fn a_truncated_command_never_ends_mid_token() {
        let cmd = "grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 Cargo.toml";
        for budget in [24usize, 30, 40, 52] {
            let hint = super::elide_tail_at_word(cmd, budget);
            let body = hint.trim_end_matches('…');
            assert!(
                cmd.starts_with(body),
                "budget {budget}: not a prefix of the command: {hint}"
            );
            assert!(
                cmd[body.len()..].starts_with(' ') || body.len() == cmd.len(),
                "budget {budget}: cut mid-token: {hint}"
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
            super::arg_hint(&card, 24)
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
            super::arg_hint(&card, 30)
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
    fn strip_cd_prefix_removes_session_constant_cd() {
        assert_eq!(
            strip_cd_prefix(r#"cd /some/path && grep -n "x" f.rs"#),
            r#"grep -n "x" f.rs"#
        );
        // No cd prefix: left untouched.
        assert_eq!(
            strip_cd_prefix(r#"grep -n "x" f.rs"#),
            r#"grep -n "x" f.rs"#
        );
    }

    #[test]
    fn hint_field_picks_bash_command_over_an_earlier_sorting_key() {
        // `args` is a serde_json::Map (BTreeMap), so "aaa_unrelated" sorts
        // before "command" alphabetically. The old code took the first
        // string value in the map and would have picked this one instead.
        let c = StepCard::new(
            "s1".into(),
            "bash".into(),
            serde_json::json!({ "aaa_unrelated": "not the command", "command": "cargo test" }),
        );
        assert_eq!(hint_field(&c), Some("cargo test"));
    }

    #[test]
    fn elide_middle_keeps_head_and_tail_so_differing_tails_differ() {
        // Old tail-truncation cut everything after `budget` bytes, so two
        // commands sharing a long common prefix and differing only at the
        // end produced identical, useless hints.
        let a = "cargo test --workspace --package mur-core --lib render_card::tests::alpha";
        let b = "cargo test --workspace --package mur-core --lib render_card::tests::zzzzz";
        let hint_a = elide_middle(a, 40);
        let hint_b = elide_middle(b, 40);
        assert_ne!(hint_a, hint_b);
        assert!(hint_a.contains('…'));
        assert!(hint_b.contains('…'));
    }

    #[test]
    fn elide_middle_on_long_multibyte_path_does_not_panic_and_respects_budget() {
        let long_cjk = "檔".repeat(50); // 3 bytes/char → ~150 bytes, well past 40
        let budget = 40;
        let hint = elide_middle(&long_cjk, budget);
        // `…` itself is 3 UTF-8 bytes but ELLIPSIS_OVERHEAD reserves 1 (a
        // display column, not a byte — see its doc comment), so the byte
        // length can run a couple of bytes over `budget`, never far over it.
        assert!(hint.len() <= budget + "…".len());
        assert!(hint.contains('…'));
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
        let lines = card_lines(&c, theme::resolve_skin("dark"), true, TEST_WIDTH);
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
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("[y]"));
        assert!(text.contains("approve"));
        assert!(text.contains("[n]"));
    }
}
