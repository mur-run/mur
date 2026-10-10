//! P3b-§5.2 / §5.3 `LineMode` (AC-P3b-9, 12): the MURMUR send-confirmation
//! grammar next to the unchanged stdin one.

use std::collections::BTreeSet;

use super::constants::{
    RULE_NOT_OPEN_HINT, TARGET_NOT_FOUND_HINT, TARGET_NOTE_USAGE_HINT, UNKNOWN_COMMAND_HINT,
};
use super::driver::SendAnswer;
use super::note::{LineMode, NoteLine, parse_note_line, parse_note_line_mode, send_answer_for};
use super::ruling::RulingInput;
use super::schema::{HumanNote, Role};

fn members() -> [String; 2] {
    ["main-agent".into(), "rev-agent".into()]
}

fn identity(s: &str) -> String {
    s.to_string()
}

fn open_f1() -> BTreeSet<String> {
    ["F1".to_string()].into_iter().collect()
}

fn answer(line: &str, mode: LineMode) -> Result<SendAnswer, String> {
    send_answer_for(line, &members(), &open_f1(), mode, identity)
}

fn note(text: &str, target: Option<Role>) -> Result<SendAnswer, String> {
    Ok(SendAnswer::Note(HumanNote {
        text: text.into(),
        target,
    }))
}

/// The wrapper every existing caller uses is exactly `Stdin`.
#[test]
fn parse_note_line_is_the_stdin_mode() {
    for line in [
        "@ghost NOTE-A",
        "/note NOTE-A",
        "/bogus",
        "plain",
        "@main-agent X",
    ] {
        assert_eq!(
            parse_note_line(line, &members(), identity),
            parse_note_line_mode(line, &members(), LineMode::Stdin, identity),
            "{line:?}"
        );
    }
}

/// AC-P3b-12: stdin keeps the N3 hint; MURMUR broadcasts the whole typed
/// line, `@ghost` prefix included, so the agents see what was meant.
#[test]
fn unknown_agent_is_a_hint_on_stdin_and_a_broadcast_note_in_murmur() {
    assert_eq!(
        parse_note_line_mode("@ghost NOTE-A", &members(), LineMode::Stdin, identity),
        NoteLine::Hint(TARGET_NOT_FOUND_HINT.replace("{name}", "ghost"))
    );
    assert_eq!(
        parse_note_line_mode("  @ghost NOTE-A ", &members(), LineMode::Murmur, identity),
        NoteLine::Note(HumanNote {
            text: "@ghost NOTE-A".into(),
            target: None
        })
    );
}

#[test]
fn murmur_keeps_the_usage_hint_for_an_incomplete_target_note() {
    assert_eq!(
        parse_note_line_mode("@ghost", &members(), LineMode::Murmur, identity),
        NoteLine::Hint(TARGET_NOTE_USAGE_HINT.into())
    );
}

#[test]
fn murmur_send_answers() {
    for line in ["", "\n", "y", "YES", "  yes  "] {
        assert_eq!(
            answer(line, LineMode::Murmur),
            Ok(SendAnswer::Send),
            "{line:?}"
        );
    }
    assert_eq!(answer("/stop", LineMode::Murmur), Ok(SendAnswer::Stop));
    assert_eq!(answer("  /stop  ", LineMode::Murmur), Ok(SendAnswer::Stop));
}

/// AC-P3b-9: bare `q` is a note in MURMUR; `/stop` is the only stop.
#[test]
fn murmur_bare_q_is_a_note_not_a_stop() {
    assert_eq!(answer("q", LineMode::Murmur), note("q", None));
}

#[test]
fn murmur_notes() {
    assert_eq!(
        answer("plain NOTE-B", LineMode::Murmur),
        note("plain NOTE-B", None)
    );
    assert_eq!(
        answer("/note NOTE-C", LineMode::Murmur),
        note("NOTE-C", None)
    );
    assert_eq!(
        answer("@main-agent NOTE-D", LineMode::Murmur),
        note("NOTE-D", Some(Role::Main))
    );
    assert_eq!(
        answer("@ghost NOTE-E", LineMode::Murmur),
        note("@ghost NOTE-E", None)
    );
}

#[test]
fn murmur_rule_and_unknown_command() {
    match answer("/rule drop F1 NOTE-R", LineMode::Murmur) {
        Ok(SendAnswer::SendWithRuling(RulingInput { .. })) => {}
        other => panic!("expected SendWithRuling, got {other:?}"),
    }
    assert_eq!(
        answer("/rule drop F9 NOTE-R", LineMode::Murmur),
        Err(RULE_NOT_OPEN_HINT.replace("{id}", "F9"))
    );
    assert_eq!(
        answer("/bogus", LineMode::Murmur),
        Err(UNKNOWN_COMMAND_HINT.replace("{command}", "/bogus"))
    );
}

/// `Stdin` decides exactly as `TerminalGate::confirm_send` does today.
#[test]
fn stdin_send_answers_match_the_terminal_gate() {
    assert_eq!(answer("\n", LineMode::Stdin), Ok(SendAnswer::Send));
    assert_eq!(answer("y\n", LineMode::Stdin), Ok(SendAnswer::Send));
    assert_eq!(answer("q\n", LineMode::Stdin), Ok(SendAnswer::Stop));
    assert_eq!(
        answer("anything else\n", LineMode::Stdin),
        Ok(SendAnswer::Stop)
    );
    assert_eq!(
        answer("", LineMode::Stdin),
        Ok(SendAnswer::Stop),
        "EOF never sends"
    );
    assert_eq!(
        answer("/note NOTE-C", LineMode::Stdin),
        note("NOTE-C", None)
    );
    assert_eq!(
        answer("@ghost NOTE-A", LineMode::Stdin),
        Err(TARGET_NOT_FOUND_HINT.replace("{name}", "ghost"))
    );
    assert_eq!(
        answer("/stop", LineMode::Stdin),
        Err(UNKNOWN_COMMAND_HINT.replace("{command}", "/stop")),
        "/stop is MURMUR-only"
    );
}

/// P3b-§5.3: the inline hint names exactly the lines `Murmur` broadcasts
/// because the target is unknown — never a member, an alias, or a usage error.
#[test]
fn unknown_target_matches_the_murmur_broadcast_rows() {
    use super::note::unknown_target;
    let members = members();
    for (line, want) in [
        ("@ghost fix it", Some("ghost")),
        ("  @Ghost   fix it ", Some("Ghost")),
        ("@main-agent fix it", None),
        ("@MAIN-AGENT fix it", None),
        ("@主 fix it", None),
        ("@審查 fix it", None),
        ("@ghost", None),
        ("@ fix it", None),
        ("/note hi", None),
        ("plain text", None),
        ("", None),
    ] {
        let got = unknown_target(line, &members, identity);
        assert_eq!(got.as_deref(), want, "line {line:?}");
        let broadcast = matches!(
            parse_note_line_mode(line, &members, LineMode::Murmur, identity),
            NoteLine::Note(HumanNote { target: None, .. })
        ) && line.trim_start().starts_with('@');
        assert_eq!(got.is_some(), broadcast, "hint iff broadcast for {line:?}");
    }
}

/// The resolver is the expensive part (it reads `agents/`); a line that is
/// not `@<name> <text>` never reaches it.
#[test]
fn unknown_target_does_not_resolve_non_target_lines() {
    use super::note::unknown_target;
    let calls = std::cell::Cell::new(0);
    let counting = |s: &str| {
        calls.set(calls.get() + 1);
        s.to_string()
    };
    for line in ["plain", "/note x", "", "@ghost", "@主 x"] {
        let _ = unknown_target(line, &members(), counting);
    }
    assert_eq!(calls.get(), 0);
}
