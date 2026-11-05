//! Card rendering tests, split out of `render_card.rs` to keep it under 800 lines.

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

/// The intent leads, the command trails it as a dim parenthetical, and
/// duration and tokens close the row: `✔ bash intent (cmd) · 77ms / 100 tokens`.
#[test]
fn a_description_leads_and_the_command_trails_dim() {
    let mut card = done_card(serde_json::json!({
        "description": "Checking the workspace version",
        "command": "grep -m1 '^version' Cargo.toml"
    }));
    card.tokens = Some(100);
    let r = rows_at(&card, 200);
    assert_eq!(r.len(), 1, "one row when it fits: {r:?}");
    let intent_at = r[0].find("Checking").expect("intent on the row");
    let cmd_at = r[0].find("(grep").expect("command in parens on the row");
    assert!(intent_at < cmd_at, "intent must lead: {r:?}");
    assert!(
        r[0].contains(" · 21ms / 100 tokens"),
        "duration then tokens: {r:?}"
    );

    let sp = spans(&card);
    let intent = sp.iter().find(|(t, _)| t.contains("Checking")).unwrap();
    assert!(
        !intent.1.add_modifier.contains(Modifier::BOLD),
        "a successful row carries no bold (spec §4): {intent:?}"
    );
    let cmd = sp.iter().find(|(t, _)| t.contains("grep")).unwrap();
    assert!(
        cmd.1.add_modifier.contains(Modifier::DIM),
        "command trails dim: {cmd:?}"
    );
}

/// No token count yet (older runtime, or the follow-up has not arrived):
/// the row ends at the duration, with no dangling `/`.
#[test]
fn without_a_token_count_the_row_ends_at_the_duration() {
    let card = done_card(serde_json::json!({
        "description": "Checking the workspace version",
        "command": "grep -m1 '^version' Cargo.toml"
    }));
    let r = rows_at(&card, 200);
    assert!(r[0].contains(" · 21ms"), "{r:?}");
    assert!(!r[0].contains('/') && !r[0].contains("tokens"), "{r:?}");
}

/// A tight row never loses the command: it moves to its own dim line
/// under the intent instead of being elided away.
#[test]
fn a_tight_row_moves_the_command_to_a_second_line() {
    let card = done_card(serde_json::json!({
        "description": "Checking the workspace version of the crate",
        "command": "grep -m1 '^version' Cargo.toml"
    }));
    let r = rows_at(&card, 44);
    assert!(r[0].starts_with("✔ bash Check"), "intent leads: {r:?}");
    assert!(!r[0].contains("grep"), "command left the tight row: {r:?}");
    let second = r.get(1).expect("command row");
    assert!(
        second.trim_start().starts_with("(grep"),
        "command on its own row: {r:?}"
    );
    assert!(
        second.contains("Cargo.toml"),
        "its own row has room for the whole command: {r:?}"
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
    assert!(
        text.contains("awaiting approval"),
        "the awaiting marker must show collapsed: {text}"
    );
}

/// The card marks a pending decision but carries no keys of its own: the
/// docked panel is the one place a decision is made (spec §3, one surface).
#[test]
fn awaiting_card_shows_a_marker_without_keys() {
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
    assert!(text.contains("⏳ awaiting approval"), "{text}");
    assert!(!text.contains("[y]") && !text.contains("[n]"), "{text}");
}

fn edit_failure() -> StepCard {
    let mut c = StepCard::new(
        "s1".into(),
        "edit_file".into(),
        serde_json::json!({"file_path": "~/murui-demo/hello.md"}),
    );
    c.complete(
        CallOutcome::Failed,
        String::new(),
        false,
        0,
        Some(
            "tool error: tool execution failed: path not write-entitled: \
             ~/murui-demo/hello.md (grant it via `mur agent perm allow-write mur \
             ~/murui-demo/hello.md && mur agent restart mur`)"
                .into(),
        ),
        19,
    );
    c
}

/// Spec §7 `failed_tool_is_one_summary_line`: one `✗` row plus one `fix:`
/// row, the wrapper chain gone, the cause said once.
#[test]
fn failed_tool_is_one_summary_line() {
    let lines = card_lines(&edit_failure(), theme::resolve_skin("dark"), false, 160);
    let text = joined(&lines);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(text.starts_with("✗ edit_file"), "{text}");
    assert!(text.contains("— path not write-entitled"), "{text}");
    assert!(
        !text.contains("tool error") && !text.contains("tool execution failed"),
        "{text}"
    );
    assert_eq!(text.matches("not write-entitled").count(), 1, "{text}");
    let fix = text.lines().nth(1).unwrap();
    assert!(
        fix.trim_start()
            .starts_with("fix: mur agent perm allow-write mur"),
        "{fix}"
    );
}

/// Only the glyph and the tool name take `error`; the cause takes `text` and
/// the `fix:` row `muted`.
#[test]
fn failure_ink() {
    for theme in [&theme::ANSI, &theme::LIGHT, &theme::MUR, &theme::CLAY] {
        let lines = card_lines(&edit_failure(), theme, false, 160);
        let span = |needle: &str| {
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .find(|s| s.content.contains(needle))
                .unwrap()
                .style
        };
        assert_eq!(span("✗").fg, theme.error.fg);
        assert_eq!(span("edit_file").fg, theme.error.fg);
        assert_eq!(span("not write-entitled"), theme.text);
        assert_eq!(span("fix:"), theme.muted);
    }
}

/// Spec §4 ink: on a done row the `✔` takes `ok`, and nothing else on the
/// header carries `accent`, `ok` or BOLD — under every skin.
#[test]
fn success_row_colours_only_the_glyph() {
    for name in ["ansi", "light", "mur", "clay"] {
        let t = theme::resolve_skin(name);
        let card = done_card(serde_json::json!({
            "description": "Listing the demo dir",
            "command": "ls ~/murui-demo"
        }));
        let header = &card_lines(&card, t, false, TEST_WIDTH)[0];
        for s in &header.spans {
            if s.content.trim().is_empty() {
                continue;
            }
            if s.content.as_ref() == "✔" {
                assert_eq!(s.style, t.ok, "{name}: glyph takes ok");
                continue;
            }
            assert!(
                !s.style.add_modifier.contains(Modifier::BOLD),
                "{name}: bold {s:?}"
            );
            for tok in [t.accent, t.ok] {
                if tok.fg.is_some() {
                    assert_ne!(s.style.fg, tok.fg, "{name}: {s:?} carries a highlight ink");
                }
            }
        }
    }
}

#[test]
fn duration_formats() {
    use crate::cmd::agent::cli::call_summary::fmt_duration;
    assert_eq!(fmt_duration(640), "640ms");
    assert_eq!(fmt_duration(12_400), "12.4s");
    assert_eq!(fmt_duration(126_076), "2m06s");
    assert_eq!(fmt_duration(59_999), "59.9s", "never rounds up to 60.0s");
}

/// The intent keeps its first words and ends in `…`; it is never
/// middle-elided into `head…tail`.
#[test]
fn intent_is_tail_elided() {
    let card = done_card(serde_json::json!({
        "description": "Running the whole runtime test suite to see which supervisor test hangs",
        "command": "cargo test -q -p mur-agent-runtime"
    }));
    let r = rows_at(&card, 60);
    assert!(r[0].contains("✔ bash Running the"), "{r:?}");
    let after = r[0].split('…').nth(1).expect("elided");
    assert!(
        !after.chars().next().is_some_and(char::is_alphanumeric),
        "text after the ellipsis means middle-elision: {r:?}"
    );
}
