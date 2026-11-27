use super::*;

fn agent_event(seq: u64, id: &str, text: &str) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: ChannelActor::Agent { id: id.into() },
        kind: mur_common::channel::EventKind::Note,
        payload: serde_json::json!({ "text": text }),
        idempotency_key: None,
        sig: None,
        key_version: None,
        prev_hash: None,
    }
}

fn sys_event(seq: u64, text: &str) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: ChannelActor::System,
        kind: mur_common::channel::EventKind::Note,
        payload: serde_json::json!({ "text": text }),
        idempotency_key: None,
        sig: None,
        key_version: None,
        prev_hash: None,
    }
}

#[test]
fn final_synthesis_always_has_exactly_one_terminal_marker() {
    assert_eq!(
        finalize_synthesis("answer", "RESEARCH_COMPLETE"),
        "answer\n\nRESEARCH_COMPLETE"
    );
    assert_eq!(
        finalize_synthesis("answer\nRESEARCH_COMPLETE\n", "RESEARCH_COMPLETE"),
        "answer\nRESEARCH_COMPLETE"
    );
}

#[test]
fn synthesis_prompt_requires_report_and_own_line_marker() {
    let prompt = build_synthesis_prompt("What changed?", "RESEARCH_COMPLETE", "worker evidence");
    assert!(prompt.contains("What changed?"));
    assert!(prompt.contains("worker evidence"));
    assert!(prompt.contains("final cited answer"));
    assert!(prompt.contains("\nRESEARCH_COMPLETE\n"));
    assert!(prompt.contains("last non-blank line"));
}

#[test]
fn channel_has_marker_matches_member_events_after_baseline() {
    let evs = vec![
        // System goal event MENTIONS the token — must NOT self-trigger.
        sys_event(1, "goal: emit DONE_TOKEN when finished"),
        agent_event(2, "qa", "still working"),
        agent_event(3, "pm", "all green\nDONE_TOKEN"), // sentinel on its own line
    ];
    // a member emitted the marker as a sentinel after baseline 0 → converged
    assert!(channel_has_marker(&evs, "DONE_TOKEN", 0));
    // baseline at seq 3 excludes this-run events → not converged (stale-run guard)
    assert!(!channel_has_marker(&evs, "DONE_TOKEN", 3));
    // the System goal event alone never counts (Agent-authored only)
    assert!(!channel_has_marker(&evs[..1], "DONE_TOKEN", 0));
    // absent marker
    assert!(!channel_has_marker(&evs, "NOT_PRESENT", 0));
}

#[test]
fn channel_has_marker_rejects_prose_mentions_of_the_marker() {
    // The marker is fanned out to members in the goal, so prose that quotes
    // or negates it must NOT converge — only a deliberate own-line sentinel.
    let planning = vec![agent_event(
        2,
        "qa",
        "I will emit DONE_TOKEN when tests pass",
    )];
    assert!(!channel_has_marker(&planning, "DONE_TOKEN", 0));
    let negated = vec![agent_event(2, "qa", "DONE_TOKEN not yet emitted")];
    assert!(!channel_has_marker(&negated, "DONE_TOKEN", 0));
    let embedded = vec![agent_event(2, "qa", "see ABANDONED_TOKENS below")];
    assert!(!channel_has_marker(&embedded, "DONE_TOKEN", 0));
    // but a trailing-whitespace sentinel line still converges (trimmed)
    let sentinel = vec![agent_event(2, "qa", "done:\n  DONE_TOKEN  ")];
    assert!(channel_has_marker(&sentinel, "DONE_TOKEN", 0));
}
