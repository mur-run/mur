//! P3a-§3 note line parsing (AC-P3a-4, 5, 15, 16, 17).

use super::constants::{NOTE_USAGE_HINT, TARGET_NOTE_USAGE_HINT};
use super::note::{NoteLine, parse_note_line};
use super::schema::{HumanNote, Role};

fn members() -> [String; 2] {
    ["main-agent".into(), "rev-agent".into()]
}

/// A resolver that never matches anything: returns its input unchanged,
/// like `canonicalize_agent_name` does for an unknown name.
fn identity(s: &str) -> String {
    s.to_string()
}

fn parse(line: &str) -> NoteLine {
    parse_note_line(line, &members(), identity)
}

fn note(text: &str, target: Option<Role>) -> NoteLine {
    NoteLine::Note(HumanNote {
        text: text.into(),
        target,
    })
}

#[test]
fn slash_note_is_a_broadcast_note() {
    assert_eq!(
        parse("/note keep the API stable"),
        note("keep the API stable", None)
    );
    assert_eq!(parse("  /note   spaced  out  "), note("spaced  out", None));
}

/// AC-P3a-15.
#[test]
fn empty_slash_note_is_the_usage_hint() {
    for line in ["/note", "/note   "] {
        assert_eq!(
            parse(line),
            NoteLine::Hint(NOTE_USAGE_HINT.into()),
            "{line:?}"
        );
    }
}

/// AC-P3a-4, alias path: the fixed map, no name lookup.
#[test]
fn alias_targets_resolve_without_lookup() {
    let never = |_: &str| -> String { panic!("alias must not call the resolver") };
    assert_eq!(
        parse_note_line("@主 Z", &members(), never),
        note("Z", Some(Role::Main))
    );
    assert_eq!(
        parse_note_line("@審查 Z", &members(), never),
        note("Z", Some(Role::Reviewer))
    );
}

/// AC-P3a-4, name path: a different case resolves through the resolver.
#[test]
fn member_name_in_another_case_resolves_through_the_resolver() {
    let canon = |s: &str| s.to_lowercase();
    assert_eq!(
        parse_note_line("@REV-Agent look at F2", &members(), canon),
        note("look at F2", Some(Role::Reviewer))
    );
    assert_eq!(
        parse_note_line("@Main-Agent hi", &members(), canon),
        note("hi", Some(Role::Main))
    );
}

/// AC-P3a-5.
#[test]
fn unknown_agent_is_the_n3_hint() {
    assert_eq!(
        parse("@nobody W"),
        NoteLine::Hint("agent nobody not found; use /note <text> to send it to both sides".into())
    );
}

/// AC-P3a-17: the resolver finds a real agent that is not in the session.
#[test]
fn real_non_member_agent_is_the_n3_hint() {
    let canon = |_: &str| "other-agent".to_string();
    assert_eq!(
        parse_note_line("@Other-Agent W", &members(), canon),
        NoteLine::Hint(
            "agent Other-Agent not found; use /note <text> to send it to both sides".into()
        )
    );
}

#[test]
fn at_without_name_or_text_is_the_usage_hint() {
    for line in ["@", "@ text", "@rev-agent", "@主", "@rev-agent   "] {
        assert_eq!(
            parse(line),
            NoteLine::Hint(TARGET_NOTE_USAGE_HINT.into()),
            "{line:?}"
        );
    }
}

/// AC-P3a-16: unknown slash commands, typos included, are hints — never
/// a stop.
#[test]
fn unknown_slash_command_is_a_hint() {
    assert_eq!(
        parse("/foo"),
        NoteLine::Hint("unknown command: /foo".into())
    );
    assert_eq!(
        parse("/riule drop F1 x"),
        NoteLine::Hint("unknown command: /riule".into())
    );
    assert_eq!(
        parse("/notes x"),
        NoteLine::Hint("unknown command: /notes".into())
    );
}

/// `/rule` belongs to the ruling parser; P1 answers are untouched.
#[test]
fn rule_and_plain_answers_are_not_notes() {
    for line in ["/rule drop F1 x", "/rule", "q", "nope", "", "y", "  "] {
        assert_eq!(parse(line), NoteLine::NotNote, "{line:?}");
    }
}
