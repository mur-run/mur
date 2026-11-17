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

// ---- Cross-layer: the refusal reaches history readers (#2446 review) -----
//
// A cancelled turn that ran tools is refused by the runtime (§4.3). The gate
// records that refusal in the log, so the shared retract scan — what murmur
// replay and the projection use — keeps the message the model still sees.
mod refusal_reaches_the_log {
    use std::sync::Arc;

    use mur_channel::ChannelService;
    use mur_channel::conversation::retract;
    use mur_common::channel::{
        ChannelActor, EventKind, RetractPayload, RetractReason, RetractRefusal,
        retract_idempotency_key,
    };
    use mur_common::identity::AgentIdentity;

    use crate::protocol::methods::message_send::ChannelContext;
    use crate::task_runner::TaskRunner;
    use crate::task_runner::tests::end_turn_response;

    const AGENT: &str = "smith";

    struct Never;
    #[async_trait::async_trait]
    impl crate::llm::LlmClient for Never {
        async fn generate(
            &self,
            _: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
            Ok(end_turn_response("unused"))
        }
        fn model_name(&self) -> &str {
            "never"
        }
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        svc: ChannelService,
        ch: String,
        runner: TaskRunner,
    }

    fn fx(signed: bool) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let me = Arc::new(AgentIdentity::generate());
        me.save(&tmp.path().join("agents").join(AGENT)).unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent(AGENT).unwrap().id;
        let mut runner = TaskRunner::with_llm(Arc::new(Never))
            .with_agent_name(AGENT)
            .with_channel_conversations(tmp.path().to_path_buf(), None, 4);
        if signed {
            runner = runner.with_retract_signer(me, 1);
        }
        Fx {
            _tmp: tmp,
            svc,
            ch,
            runner,
        }
    }

    impl Fx {
        fn human(&self, text: &str) -> u64 {
            self.svc
                .append_message(
                    &self.ch,
                    ChannelActor::local_human(),
                    EventKind::Message,
                    text,
                    None,
                )
                .unwrap()
                .seq
        }

        fn retract(&self, target: u64) {
            let p = RetractPayload {
                target_seq: target,
                reason: RetractReason::Cancelled,
            };
            self.svc
                .append(
                    &self.ch,
                    ChannelActor::local_human(),
                    EventKind::Retract,
                    serde_json::to_value(p).unwrap(),
                    Some(retract_idempotency_key(target)),
                )
                .unwrap();
        }

        /// The cancelled turn `foo` at `seq`, remembered with `tools`.
        fn cancelled(&self, seq: u64, tools: &[&str]) {
            let cc = self.runner.channel_conversations.as_ref().unwrap();
            cc.store.lock().unwrap().remember_turn(
                &self.ch,
                seq,
                "foo",
                "turn ended: cancelled",
                super::ledger(tools),
            );
        }

        async fn gate(&self, seq: u64, text: &str) -> Vec<String> {
            let ctx = ChannelContext {
                channel_id: self.ch.clone(),
                channel_seq: seq,
            };
            let prior = self.runner.channel_prior(&ctx, text).await.unwrap();
            super::shape(&prior)
        }

        fn refusals(&self) -> Vec<u64> {
            self.svc
                .load_events(&self.ch)
                .unwrap()
                .iter()
                .filter(|e| matches!(&e.actor, ChannelActor::Agent { id } if id == AGENT))
                .filter_map(|e| RetractRefusal::from_payload(&e.payload))
                .map(|r| r.target_seq)
                .collect()
        }

        fn withdrawn(&self) -> Vec<u64> {
            let evs = self.svc.load_events(&self.ch).unwrap();
            retract::scan(&evs, AGENT, u64::MAX)
                .targets
                .into_iter()
                .collect()
        }
    }

    /// The race the review named: a tool ran before murmur saw any output,
    /// the user double-ESCs (retract), then resends.
    async fn race(signed: bool, tools: &[&str]) -> (Fx, Vec<String>) {
        let f = fx(signed);
        let foo = f.human("foo");
        f.gate(foo, "foo").await;
        f.cancelled(foo, tools);
        f.retract(foo);
        let resend = f.human("foo bar");
        let h = f.gate(resend, "foo bar").await;
        (f, h)
    }

    #[tokio::test]
    async fn a_refused_retract_is_recorded_and_replay_keeps_the_message() {
        let (f, h) = race(true, &["bash"]).await;
        assert_eq!(h, vec!["user:foo", "ledger:1"], "the model keeps foo");
        assert_eq!(f.refusals(), vec![0], "one refusal note, for seq 0");
        assert!(
            f.withdrawn().is_empty(),
            "history readers must keep what the model sees: {:?}",
            f.withdrawn()
        );

        // Re-observed on the next turn: the idempotency key appends nothing.
        let next = f.human("again");
        f.gate(next, "again").await;
        assert_eq!(f.refusals(), vec![0]);
    }

    #[tokio::test]
    async fn an_honoured_retract_records_nothing_and_replay_hides_it() {
        let (f, h) = race(true, &[]).await;
        assert!(h.is_empty(), "{h:?}");
        assert!(f.refusals().is_empty());
        assert_eq!(f.withdrawn(), vec![0]);
    }

    #[tokio::test]
    async fn a_refusal_note_does_not_damage_the_next_turn() {
        let (f, _) = race(true, &["bash"]).await;
        let next = f.human("again");
        let h = f.gate(next, "again").await;
        assert!(h.contains(&"user:foo bar".to_string()), "{h:?}");
    }
}
