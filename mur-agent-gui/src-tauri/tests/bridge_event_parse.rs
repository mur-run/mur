//! Bridge event front-matter parser.

use mur_agent_gui_lib::companion_bridge::event::{BridgeResponse, parse_inbox_md};

#[test]
fn parse_pending_message_returns_unset_response() {
    let path = std::path::Path::new("tests/fixtures/companion-inbox/pending-warm.md");
    let ev = parse_inbox_md(path).expect("must parse");
    assert_eq!(ev.id, "01HPENDING_WARM_001");
    assert_eq!(ev.situation, "morning_greeting");
    assert_eq!(ev.template_id, "greet_warm_zh_001");
    assert_eq!(ev.locale, "zh-TW");
    assert_eq!(ev.body, "早安 David。今天想從哪一件小事開始？");
    assert!(matches!(ev.response, BridgeResponse::Unset));
}

#[test]
fn parse_acked_message_carries_signal() {
    let path = std::path::Path::new("tests/fixtures/companion-inbox/acked-good.md");
    let ev = parse_inbox_md(path).expect("must parse");
    assert_eq!(ev.id, "01HACKED_GOOD_001");
    assert!(
        matches!(ev.response, BridgeResponse::Signal(s) if s == "good"),
        "expected response: good"
    );
}

#[test]
fn parse_malformed_returns_err() {
    let path = std::path::Path::new("tests/fixtures/companion-inbox/malformed.md");
    let err = parse_inbox_md(path).expect_err("malformed must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("front-matter") || msg.contains("frontmatter"),
        "error must mention front-matter, got: {msg}"
    );
}
