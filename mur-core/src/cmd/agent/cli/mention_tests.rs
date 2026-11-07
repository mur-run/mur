use super::*;

fn ok(line: &str) -> Mention {
    match parse_mention(line) {
        Some(Ok(m)) => m,
        other => panic!("expected a mention for {line:?}, got {other:?}"),
    }
}

fn err(line: &str) -> MentionError {
    match parse_mention(line) {
        Some(Err(e)) => e,
        other => panic!("expected an error for {line:?}, got {other:?}"),
    }
}

#[test]
fn not_a_mention_without_leading_at() {
    assert_eq!(parse_mention("hello @bob"), None);
    assert_eq!(parse_mention(""), None);
}

#[test]
fn parses_name_only() {
    assert_eq!(
        ok("@bob   please look"),
        Mention {
            namespace: None,
            name: "bob".into(),
            ordinal: None,
            rest: "please look".into(),
        }
    );
    assert_eq!(ok("@bob").rest, "");
}

#[test]
fn parses_name_with_ordinal() {
    let m = ok("@bob#12 continue");
    assert_eq!((m.name.as_str(), m.ordinal), ("bob", Some(12)));
    assert_eq!(m.rest, "continue");
}

#[test]
fn ordinal_must_be_a_positive_integer() {
    for line in [
        "@bob# x",
        "@bob#0 x",
        "@bob#abc x",
        "@bob#-1 x",
        "@bob#1#2 x",
    ] {
        assert!(
            matches!(err(line), MentionError::Malformed(_)),
            "{line:?} should be malformed"
        );
    }
}

#[test]
fn parses_agent_prefix() {
    assert_eq!(
        ok("@agent:Bob#3 hi"),
        Mention {
            namespace: Some(Namespace::Agent),
            name: "Bob".into(),
            ordinal: Some(3),
            rest: "hi".into(),
        }
    );
}

#[test]
fn parses_fleet_prefix() {
    assert_eq!(
        ok("@fleet:ops deploy now"),
        Mention {
            namespace: Some(Namespace::Fleet),
            name: "ops".into(),
            ordinal: None,
            rest: "deploy now".into(),
        }
    );
}

#[test]
fn fleet_with_ordinal_is_malformed() {
    assert!(matches!(err("@fleet:x#3 hi"), MentionError::Malformed(_)));
}

#[test]
fn empty_or_unknown_prefix_is_malformed() {
    for line in [
        "@ hi",
        "@",
        "@agent: hi",
        "@#3 hi",
        "@laptop:bob hi",
        "@a:b:c hi",
    ] {
        assert!(
            matches!(err(line), MentionError::Malformed(_)),
            "{line:?} should be malformed"
        );
    }
}

#[test]
fn reserved_bare_name_is_error() {
    assert_eq!(err("@agent hi"), MentionError::ReservedName("agent".into()));
    assert_eq!(err("@Fleet hi"), MentionError::ReservedName("Fleet".into()));
    assert_eq!(
        err("@agent:fleet hi"),
        MentionError::ReservedName("fleet".into())
    );
}

#[test]
fn at_all_parses_as_name_all_reserved() {
    // Phase 5 turns `@all` into broadcast; Phase 1 refuses it by name.
    assert_eq!(err("@all hi"), MentionError::ReservedName("all".into()));
}
