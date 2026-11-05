//! Consults (`Ask` / `AskReply`) are not member work: they neither reset
//! stuck detection nor satisfy a done marker (design §D8, R8).

use super::*;
use mur_common::channel::EventKind;

fn kind_event(seq: u64, id: &str, kind: EventKind, text: &str) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: if id.is_empty() {
            ChannelActor::System
        } else {
            ChannelActor::Agent { id: id.into() }
        },
        kind,
        payload: serde_json::json!({ "text": text }),
        idempotency_key: None,
        sig: None,
        key_version: None,
        prev_hash: None,
    }
}

/// R8 · Fleet stuck detection: when the only new agent-authored events since
/// the last iteration are consults, the stuck clock is not reset.
#[test]
fn consult_events_are_not_member_progress() {
    let evs = vec![
        kind_event(1, "", EventKind::Note, "goal"),
        kind_event(2, "pm", EventKind::Ask, "which file?"),
        kind_event(3, "tech-writer", EventKind::AskReply, "README.md"),
    ];
    assert!(!member_progressed(&evs, 1));
    let mut with_work = evs.clone();
    with_work.push(kind_event(4, "pm", EventKind::Message, "drafted"));
    assert!(member_progressed(&with_work, 1));
    assert!(!member_progressed(&with_work, 4));
}

/// A consult answer that quotes the done marker must not converge the loop.
#[test]
fn consult_reply_quoting_the_marker_does_not_converge() {
    let evs = vec![kind_event(
        1,
        "tech-writer",
        EventKind::AskReply,
        "the router stops on\nDONE_TOKEN",
    )];
    assert!(!channel_has_marker(&evs, "DONE_TOKEN", 0));
}
