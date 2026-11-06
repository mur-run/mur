//! B1: channel-keyed conversation store, provisional tail, per-conversation
//! locks. Pure — no runtime, no channel log; assessments are built by hand.
use crate::llm::RichMessage;
use crate::task_runner::channel_conversation::{
    Busy, ChannelConversationStore, ConversationLocks, Entry,
};
use mur_channel::conversation::{Assessment, ProjectedMessage, Role};
use std::sync::Arc;
use std::time::Duration;

const CH: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

fn pm(seq: u64, role: Role, text: &str) -> ProjectedMessage {
    ProjectedMessage {
        seq,
        role,
        text: text.into(),
    }
}

fn ok(projected: Vec<ProjectedMessage>, log_last_seq: u64) -> Assessment {
    Assessment {
        projected,
        log_last_seq,
        rebuilt_after_repair: None,
        anchor_dirty: false,
    }
}

fn ledger() -> RichMessage {
    RichMessage::TurnLedger {
        turn: 1,
        memory: crate::turn_memory::TurnMemory::empty(0),
    }
}

/// `(role, content)` per message; a ledger renders as `("ledger", "")`.
fn shape(h: &[RichMessage]) -> Vec<(String, String)> {
    h.iter()
        .map(|m| match m {
            RichMessage::Text { role, content } => (role.clone(), content.clone()),
            RichMessage::TurnLedger { .. } => ("ledger".into(), String::new()),
            other => panic!("unexpected message {other:?}"),
        })
        .collect()
}

fn s(role: &str, content: &str) -> (String, String) {
    (role.into(), content.into())
}

fn entries(store: &ChannelConversationStore) -> &[Entry] {
    store.map.get(CH).map_or(&[], |h| h.entries.as_slice())
}

/// Turn at seq 0 on a fresh channel: empty prior, then the turn is
/// remembered with a provisional reply `reply`.
fn first_turn(store: &mut ChannelConversationStore, user: &str, reply: &str) {
    let prior = store.reconcile(CH, &ok(vec![], 0), 0);
    assert!(prior.is_empty(), "fresh channel has no prior");
    store.remember_turn(CH, 0, user, reply, ledger());
}

#[test]
fn provisional_confirmed_takes_seq_and_keeps_ledger() {
    let mut store = ChannelConversationStore::default();
    first_turn(&mut store, "hi", "hello");

    // murmur persisted the reply at seq 1; the next user turn is seq 2.
    let a = ok(
        vec![pm(0, Role::User, "hi"), pm(1, Role::Agent, "hello")],
        2,
    );
    let h = store.reconcile(CH, &a, 2);
    assert_eq!(
        shape(&h),
        vec![s("user", "hi"), s("agent", "hello"), s("ledger", "")]
    );
    let seqs: Vec<_> = entries(&store).iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        vec![Some(0), Some(1), Some(1)],
        "reply and ledger take seq 1"
    );
}

#[test]
fn provisional_with_settlement_card_confirmed_by_pre_settlement_text() {
    let mut store = ChannelConversationStore::default();
    // The reply the user saw was "done\n\n─ settlement ─\n…"; murmur strips
    // the card before persisting, so only the pre-settlement text is stored.
    first_turn(&mut store, "do it", "done");

    let a = ok(
        vec![pm(0, Role::User, "do it"), pm(1, Role::Agent, "done")],
        2,
    );
    let h = store.reconcile(CH, &a, 2);
    assert_eq!(
        shape(&h),
        vec![s("user", "do it"), s("agent", "done"), s("ledger", "")]
    );
    let e = &entries(&store)[1];
    assert_eq!(e.pre_settlement.as_deref(), Some("done"));
    assert_eq!(e.seq, Some(1));
}

#[test]
fn provisional_absent_from_log_is_dropped_with_ledger() {
    let mut store = ChannelConversationStore::default();
    first_turn(&mut store, "hi", "hello");

    // murmur crashed before persisting the reply: seq 1 is the next user turn.
    let a = ok(vec![pm(0, Role::User, "hi")], 1);
    let h = store.reconcile(CH, &a, 1);
    assert_eq!(shape(&h), vec![s("user", "hi")]);
    assert!(entries(&store).iter().all(|e| e.seq.is_some()));
}

#[test]
fn provisional_vs_turn_ended_line_is_dropped() {
    let mut store = ChannelConversationStore::default();
    first_turn(&mut store, "hi", "hello");

    let a = ok(
        vec![
            pm(0, Role::User, "hi"),
            pm(1, Role::Agent, "turn ended: timeout"),
        ],
        2,
    );
    let h = store.reconcile(CH, &a, 2);
    assert_eq!(
        shape(&h),
        vec![s("user", "hi"), s("agent", "turn ended: timeout")],
        "the log wins; the provisional reply and its ledger are gone"
    );
}

#[test]
fn rebuild_discards_cache() {
    let mut store = ChannelConversationStore::default();
    first_turn(&mut store, "old", "old reply");
    let a = ok(
        vec![pm(0, Role::User, "old"), pm(1, Role::Agent, "old reply")],
        2,
    );
    store.reconcile(CH, &a, 2);
    store.remember_turn(CH, 2, "old two", "old two reply", ledger());

    // A verified repair restarted the chain: the log is shorter and different.
    let a = Assessment {
        rebuilt_after_repair: Some(2),
        ..ok(
            vec![pm(1, Role::User, "new"), pm(2, Role::Agent, "new reply")],
            3,
        )
    };
    let h = store.reconcile(CH, &a, 3);
    assert_eq!(shape(&h), vec![s("user", "new"), s("agent", "new reply")]);
}

#[test]
fn cache_behind_folds_missing_events() {
    let mut store = ChannelConversationStore::default();
    first_turn(&mut store, "hi", "hello");

    // Two more turns happened that this cache never saw.
    let a = ok(
        vec![
            pm(0, Role::User, "hi"),
            pm(1, Role::Agent, "hello"),
            pm(2, Role::User, "u2"),
            pm(3, Role::Agent, "a2"),
        ],
        4,
    );
    let h = store.reconcile(CH, &a, 4);
    assert_eq!(
        shape(&h),
        vec![
            s("user", "hi"),
            s("agent", "hello"),
            s("ledger", ""),
            s("user", "u2"),
            s("agent", "a2"),
        ]
    );
}

#[test]
fn reconcile_hides_entries_at_or_above_channel_seq() {
    let mut store = ChannelConversationStore::default();
    let all = vec![
        pm(0, Role::User, "u0"),
        pm(1, Role::Agent, "a1"),
        pm(2, Role::User, "u2"),
        pm(3, Role::Agent, "a3"),
    ];
    store.reconcile(CH, &ok(all, 4), 4);

    // A resend names an older event: nothing at or past seq 2 is replayed.
    let h = store.reconcile(
        CH,
        &ok(vec![pm(0, Role::User, "u0"), pm(1, Role::Agent, "a1")], 4),
        2,
    );
    assert_eq!(shape(&h), vec![s("user", "u0"), s("agent", "a1")]);
    assert_eq!(entries(&store).len(), 4, "hidden, not forgotten");
}

#[test]
fn events_after_channel_seq_are_folded_on_the_next_turn() {
    // Log: user@0, shell Note@1 (appended mid-turn), reply@2, user@3. The
    // Note is past turn 0's channel_seq, so it can only be folded next turn —
    // the watermark must not have skipped it.
    let mut store = ChannelConversationStore::default();
    let prior = store.reconcile(CH, &ok(vec![], 1), 0);
    assert!(prior.is_empty());
    store.remember_turn(CH, 0, "hi", "hello", ledger());

    let a = ok(
        vec![
            pm(0, Role::User, "hi"),
            pm(1, Role::User, "$ ls"),
            pm(2, Role::Agent, "hello"),
        ],
        3,
    );
    let h = store.reconcile(CH, &a, 3);
    assert_eq!(
        shape(&h),
        vec![
            s("user", "hi"),
            s("user", "$ ls"),
            s("agent", "hello"),
            s("ledger", "")
        ]
    );
}

#[test]
fn cache_state_reports_watermark() {
    let mut store = ChannelConversationStore::default();
    assert_eq!(store.cache_state(CH).watermark, None, "miss");
    first_turn(&mut store, "hi", "hello");
    assert_eq!(store.cache_state(CH).watermark, Some(0));
}

#[test]
fn remembered_turn_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = ChannelConversationStore::new(Some(dir.path()), 8_000);
    first_turn(&mut store, "hi", "hello");
    assert!(
        dir.path()
            .join("channels")
            .join(format!("{CH}.json"))
            .is_file()
    );

    let mut after = ChannelConversationStore::new(Some(dir.path()), 8_000);
    assert_eq!(after.cache_state(CH).watermark, Some(0));
    let a = ok(
        vec![pm(0, Role::User, "hi"), pm(1, Role::Agent, "hello")],
        2,
    );
    let h = after.reconcile(CH, &a, 2);
    assert_eq!(
        shape(&h),
        vec![s("user", "hi"), s("agent", "hello"), s("ledger", "")],
        "provisional tail survived on disk and was confirmed"
    );
}

#[test]
fn remember_turn_trims_whole_turns_from_the_front() {
    // Budget of 10 tokens ≈ 40 chars: three 30-char turns cannot all fit.
    let mut store = ChannelConversationStore::new(None, 10);
    let long = "x".repeat(30);
    store.remember_turn(CH, 0, "first", &long, ledger());
    store.remember_turn(CH, 2, "second", &long, ledger());
    store.remember_turn(CH, 4, "third", &long, ledger());
    let e = entries(&store);
    assert!(
        matches!(&e[0].message, RichMessage::Text { role, content } if role == "user" && content == "third"),
        "history still starts on a user turn, the newest one kept: {:?}",
        e.iter().map(|x| x.seq).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn locks_serialize_same_conversation() {
    let locks = ConversationLocks::new(4);
    let held = locks.acquire(CH).await.expect("first acquire");
    let waited = tokio::time::timeout(Duration::from_millis(50), locks.acquire(CH)).await;
    assert!(
        waited.is_err(),
        "second turn on the same conversation waits"
    );
    drop(held);
    let again = tokio::time::timeout(Duration::from_millis(50), locks.acquire(CH)).await;
    assert!(matches!(again, Ok(Ok(_))), "released lock is acquirable");
}

#[tokio::test]
async fn locks_refuse_past_max_waiting() {
    let max_waiting = 2;
    let locks = Arc::new(ConversationLocks::new(max_waiting));
    // max_waiting + 2 acquires in total: one holds, max_waiting wait, one is refused.
    let held = locks.acquire(CH).await.expect("holder");
    let handles: Vec<_> = (0..max_waiting + 1)
        .map(|_| {
            let l = Arc::clone(&locks);
            tokio::spawn(async move { l.acquire(CH).await.map(drop) })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(held);
    let mut busy = 0;
    for h in handles {
        if let Err(Busy) = h.await.expect("join") {
            busy += 1;
        }
    }
    assert_eq!(busy, 1, "exactly one acquire past max_waiting is refused");
}

#[tokio::test]
async fn abandoned_waiter_frees_its_slot() {
    let locks = ConversationLocks::new(1);
    let held = locks.acquire(CH).await.expect("holder");
    // A waiter that gives up (client disconnected) must not count forever.
    let _ = tokio::time::timeout(Duration::from_millis(20), locks.acquire(CH)).await;
    let w = tokio::time::timeout(Duration::from_millis(20), locks.acquire(CH)).await;
    assert!(
        w.is_err(),
        "waits (not Busy): the abandoned slot was released"
    );
    drop(held);
}

#[tokio::test]
async fn different_conversations_do_not_wait() {
    let locks = ConversationLocks::new(0);
    let _held = locks.acquire("a").await.expect("a");
    let b = tokio::time::timeout(Duration::from_millis(50), locks.acquire("b")).await;
    assert!(
        matches!(b, Ok(Ok(_))),
        "another conversation is independent"
    );
}

// ---- B2: wire fields, error codes, pre-lock gate -------------------------

mod gate {
    use crate::protocol::a2a_server::{Dispatcher, HandlerError, MethodHandler, RequestContext};
    use crate::protocol::methods::message_send::{
        ChannelContext, MessageSendHandler, parse_channel_context,
    };
    use crate::task_runner::TaskRunner;
    use crate::task_runner::channel_gate::{GateInput, assess, pre_lock_checks};
    use mur_channel::ChannelService;
    use mur_channel::conversation::{CacheState, ConversationDamage};
    use mur_common::channel::{CHANNEL_SCHEMA_VERSION, ChannelActor, EventKind, ParticipantRole};
    use serde_json::{Value, json};
    use std::sync::Arc;

    const AGENT: &str = "rustsmith";

    struct Home {
        tmp: tempfile::TempDir,
        svc: ChannelService,
    }

    fn home() -> Home {
        let tmp = tempfile::tempdir().expect("tmp");
        let svc = ChannelService::open(tmp.path()).expect("open");
        Home { tmp, svc }
    }

    fn ctx(id: &str, seq: u64) -> ChannelContext {
        ChannelContext {
            channel_id: id.to_string(),
            channel_seq: seq,
        }
    }

    fn check(h: &Home, c: &ChannelContext) -> Result<(), HandlerError> {
        pre_lock_checks(
            &h.svc,
            &GateInput {
                mur_home: h.tmp.path(),
                agent: AGENT,
                writer_pubkey: [7; 32],
                ctx: c,
                turn_input: "hi",
            },
        )
    }

    fn events_file(h: &Home, id: &str) -> std::path::PathBuf {
        h.svc.store().events_path(id).expect("path")
    }

    #[test]
    fn channel_context_parses_both_fields() {
        let p = json!({ "context": { "channel_id": "c1", "channel_seq": 3 } });
        assert_eq!(parse_channel_context(&p).expect("ok"), Some(ctx("c1", 3)));
    }

    #[test]
    fn channel_seq_without_channel_id_is_ignored() {
        let p = json!({ "context": { "channel_seq": 3 } });
        assert_eq!(parse_channel_context(&p).expect("ok"), None);
    }

    #[tokio::test]
    async fn channel_id_without_seq_is_invalid_params() {
        let handler = MessageSendHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let p = json!({
            "message": { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] },
            "context": { "channel_id": "c1" }
        });
        let err = handler
            .handle(Some(p), &RequestContext::none())
            .await
            .expect_err("refused");
        assert!(
            matches!(&err, HandlerError::InvalidParams(m) if m.contains("channel_seq")),
            "{err}"
        );
        assert_eq!(err.code(), -32602);
    }

    #[test]
    fn conversation_error_codes_are_stable() {
        let d = ConversationDamage::Signature;
        let codes = [
            (HandlerError::UnknownConversation(String::new()), -32020),
            (HandlerError::ChannelTooNew(String::new()), -32021),
            (HandlerError::ChannelNotConversation(String::new()), -32022),
            (HandlerError::NotAParticipant(String::new()), -32023),
            (HandlerError::ConversationRepairing(String::new()), -32024),
            (HandlerError::ConversationDamaged(d), -32025),
            (HandlerError::ConversationBusy(String::new()), -32026),
        ];
        for (e, code) in codes {
            assert_eq!(e.code(), code, "{e}");
        }
    }

    struct Damaged;
    #[async_trait::async_trait]
    impl MethodHandler for Damaged {
        async fn handle(
            &self,
            _: Option<Value>,
            _: &RequestContext,
        ) -> Result<Value, HandlerError> {
            Err(HandlerError::ConversationDamaged(
                ConversationDamage::Unparseable { lines: vec![2, 5] },
            ))
        }
    }

    #[tokio::test]
    async fn damaged_error_carries_reason_and_lines_in_data() {
        let mut d = Dispatcher::new();
        d.register("x", Box::new(Damaged));
        let req = serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 1, "method": "x" }))
            .expect("req");
        let resp = d
            .dispatch(req, &RequestContext::none())
            .await
            .expect("resp");
        let err = resp.error.expect("error");
        assert_eq!(err.code, -32025);
        assert_eq!(
            err.message,
            "conversation damaged: unparseable: lines [2, 5]"
        );
        assert_eq!(
            err.data,
            Some(json!({ "reason": "unparseable", "lines": [2, 5] }))
        );
    }

    #[test]
    fn unknown_channel_dir_is_unknown_conversation() {
        let h = home();
        let missing = "0190a1b2-c3d4-7e5f-8a9b-000000000000";
        assert!(matches!(
            check(&h, &ctx(missing, 0)),
            Err(HandlerError::UnknownConversation(_))
        ));
        assert!(matches!(
            check(&h, &ctx("../escape", 0)),
            Err(HandlerError::UnknownConversation(_))
        ));
    }

    #[test]
    fn fleet_channel_is_not_conversation() {
        let h = home();
        let ch = h.svc.create_for_fleet("f", AGENT, &[]).expect("fleet");
        assert!(matches!(
            check(&h, &ctx(&ch.id, 0)),
            Err(HandlerError::ChannelNotConversation(_))
        ));
    }

    #[test]
    fn non_member_damaged_channel_is_not_a_participant_and_log_untouched() {
        let h = home();
        let mut ch = h.svc.create_for_agent("someone-else").expect("ch");
        h.svc
            .append_message(
                &ch.id,
                ChannelActor::local_human(),
                EventKind::Message,
                "hi",
                None,
            )
            .expect("append");
        let ev = events_file(&h, &ch.id);
        std::fs::write(&ev, "not json\n").expect("damage");
        let before = std::fs::metadata(&ev)
            .and_then(|m| m.modified())
            .expect("mtime");
        ch.participants
            .retain(|p| !matches!(&p.actor, ChannelActor::Agent { id } if id == AGENT));
        h.svc.store().save_manifest(&ch).expect("save");
        let err = check(&h, &ctx(&ch.id, 0)).expect_err("refused");
        assert!(
            matches!(&err, HandlerError::NotAParticipant(m) if m.contains(AGENT)),
            "{err}"
        );
        let after = std::fs::metadata(&ev)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(before, after, "the gate never touches the log");
        assert_eq!(std::fs::read_to_string(&ev).expect("read"), "not json\n");
    }

    #[test]
    fn too_new_manifest_is_channel_too_new() {
        let h = home();
        let mut ch = h.svc.create_for_agent(AGENT).expect("ch");
        ch.v = CHANNEL_SCHEMA_VERSION + 1;
        h.svc.store().save_manifest(&ch).expect("save");
        assert!(matches!(
            check(&h, &ctx(&ch.id, 0)),
            Err(HandlerError::ChannelTooNew(_))
        ));
    }

    #[test]
    fn missing_manifest_with_log_is_damaged_manifest() {
        let h = home();
        let ch = h.svc.create_for_agent(AGENT).expect("ch");
        h.svc
            .append_message(
                &ch.id,
                ChannelActor::local_human(),
                EventKind::Message,
                "hi",
                None,
            )
            .expect("append");
        let manifest = events_file(&h, &ch.id).with_file_name("channel.yaml");
        std::fs::remove_file(manifest).expect("rm manifest");
        assert!(matches!(
            check(&h, &ctx(&ch.id, 0)),
            Err(HandlerError::ConversationDamaged(
                ConversationDamage::Manifest { .. }
            ))
        ));
    }

    #[test]
    fn legacy_purpose_none_resolves_as_conversation() {
        let h = home();
        let mut ch = h.svc.create_for_agent(AGENT).expect("ch");
        ch.purpose = None;
        h.svc.store().save_manifest(&ch).expect("save");
        check(&h, &ctx(&ch.id, 0)).expect("legacy chat is a conversation");
    }

    #[test]
    fn member_added_later_passes_the_gate() {
        let h = home();
        let ch = h.svc.create_for_agent("someone-else").expect("ch");
        h.svc
            .add_participant(&ch.id, AGENT, ParticipantRole::Delegate)
            .expect("add");
        check(&h, &ctx(&ch.id, 0)).expect("member");
    }

    fn assess_at(h: &Home, c: &ChannelContext, input: &str) -> Result<Vec<String>, HandlerError> {
        let g = GateInput {
            mur_home: h.tmp.path(),
            agent: AGENT,
            writer_pubkey: [7; 32],
            ctx: c,
            turn_input: input,
        };
        assess(&h.svc, &g, &CacheState::default())
            .map(|a| a.projected.into_iter().map(|m| m.text).collect())
    }

    #[test]
    fn assess_returns_history_below_channel_seq() {
        let h = home();
        let ch = h.svc.create_for_agent(AGENT).expect("ch");
        for t in ["one", "two"] {
            h.svc
                .append_message(
                    &ch.id,
                    ChannelActor::local_human(),
                    EventKind::Message,
                    t,
                    None,
                )
                .expect("append");
        }
        let last = h
            .svc
            .store()
            .load_events(&ch.id)
            .expect("events")
            .last()
            .expect("ev")
            .seq;
        let prior = assess_at(&h, &ctx(&ch.id, last), "two").expect("ok");
        assert_eq!(prior, vec!["one".to_string()]);
    }

    #[test]
    fn assess_maps_watermark_damage() {
        let h = home();
        let ch = h.svc.create_for_agent(AGENT).expect("ch");
        let err = assess_at(&h, &ctx(&ch.id, 9), "hi").expect_err("no event at 9");
        assert!(
            matches!(
                &err,
                HandlerError::ConversationDamaged(ConversationDamage::Watermark { .. })
            ),
            "{err}"
        );
    }
}
