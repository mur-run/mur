//! Retract spec §4.3: the channel cache drops a withdrawn user turn only
//! when nothing ran; a turn that ran tools keeps its ledger. Pure — the
//! assessment is built by hand, as in `channel_conversation.rs`.
use crate::llm::RichMessage;
use crate::task_runner::channel_conversation::ChannelConversationStore;
use crate::turn_memory::{ToolMemory, ToolMemoryStatus, TurnMemory};
use mur_channel::conversation::{Assessment, ProjectedMessage, Role};

const CH: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

fn pm(seq: u64, role: Role, text: &str) -> ProjectedMessage {
    ProjectedMessage {
        seq,
        role,
        text: text.into(),
    }
}

fn assessment(projected: Vec<ProjectedMessage>, last: u64, retracted: Vec<u64>) -> Assessment {
    Assessment {
        projected,
        log_last_seq: last,
        rebuilt_after_repair: None,
        anchor_dirty: false,
        retracted,
        ignored_retracts: vec![],
    }
}

fn ledger(tools: &[&str]) -> RichMessage {
    let mut memory = TurnMemory::empty(0);
    memory.tools = tools
        .iter()
        .map(|t| ToolMemory {
            tool: (*t).into(),
            target: "x".into(),
            status: ToolMemoryStatus::Ok,
            error: None,
            excerpt: None,
        })
        .collect();
    memory.narrative_only = memory.tools.is_empty();
    RichMessage::TurnLedger { turn: 1, memory }
}

fn shape(h: &[RichMessage]) -> Vec<String> {
    h.iter()
        .map(|m| match m {
            RichMessage::Text { role, content } => format!("{role}:{content}"),
            RichMessage::TurnLedger { memory, .. } => format!("ledger:{}", memory.tools.len()),
            other => panic!("unexpected message {other:?}"),
        })
        .collect()
}

/// seq 0 "hi" / seq 1 "hello" answered; seq 2 "foo" cancelled with `tools`
/// remembered; seq 3 retracts it; the resend is seq 4.
fn cancelled_then_resent(tools: &[&str]) -> (ChannelConversationStore, Vec<RichMessage>) {
    let mut store = ChannelConversationStore::default();
    store.reconcile(CH, &assessment(vec![], 0, vec![]), 0);
    store.remember_turn(CH, 0, "hi", "hello", ledger(&[]));
    let answered = vec![pm(0, Role::User, "hi"), pm(1, Role::Agent, "hello")];
    store.reconcile(CH, &assessment(answered.clone(), 2, vec![]), 2);
    store.remember_turn(CH, 2, "foo", "turn ended: cancelled", ledger(tools));

    // The projection omits a validly retracted seq whether or not the
    // runtime later refuses it: refusal is the cache's call (§4.3).
    let h = store.reconcile(CH, &assessment(answered, 4, vec![2]), 4);
    (store, h)
}

#[test]
fn retract_of_a_toolless_cancelled_turn_drops_it() {
    let (store, h) = cancelled_then_resent(&[]);
    assert_eq!(shape(&h), vec!["user:hi", "agent:hello", "ledger:0"]);
    let seqs: Vec<_> = store.map[CH].entries.iter().map(|e| e.seq).collect();
    assert!(!seqs.contains(&Some(2)), "{seqs:?}");
    assert!(
        seqs.iter().all(Option::is_some),
        "no provisional left: {seqs:?}"
    );
}

#[test]
fn retract_of_a_turn_that_ran_tools_is_refused_and_keeps_the_ledger() {
    let (_, h) = cancelled_then_resent(&["bash"]);
    assert_eq!(
        shape(&h),
        vec!["user:hi", "agent:hello", "ledger:0", "user:foo", "ledger:1"]
    );
}

#[test]
fn a_retract_folded_on_an_earlier_turn_is_a_no_op() {
    let (mut store, first) = cancelled_then_resent(&[]);
    store.remember_turn(CH, 4, "foo bar", "ok", ledger(&[]));
    let projected = vec![
        pm(0, Role::User, "hi"),
        pm(1, Role::Agent, "hello"),
        pm(4, Role::User, "foo bar"),
        pm(5, Role::Agent, "ok"),
    ];
    let h = store.reconcile(CH, &assessment(projected, 6, vec![2]), 6);
    let mut want = shape(&first);
    want.extend([
        "user:foo bar".to_string(),
        "agent:ok".into(),
        "ledger:0".into(),
    ]);
    assert_eq!(shape(&h), want);
}

#[test]
fn a_refused_retract_stays_refused_on_later_turns() {
    let (mut store, first) = cancelled_then_resent(&["bash"]);
    store.remember_turn(CH, 4, "foo bar", "ok", ledger(&[]));
    let projected = vec![
        pm(0, Role::User, "hi"),
        pm(1, Role::Agent, "hello"),
        pm(4, Role::User, "foo bar"),
        pm(5, Role::Agent, "ok"),
    ];
    let h = store.reconcile(CH, &assessment(projected, 6, vec![2]), 6);
    let mut want = shape(&first);
    want.extend([
        "user:foo bar".to_string(),
        "agent:ok".into(),
        "ledger:0".into(),
    ]);
    assert_eq!(shape(&h), want);
}
