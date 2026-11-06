//! §9 Phase 1 matrix rows for the locked conversation read (at-mention A4).
//! Every fixture is a real `ChannelService` in a `TempDir` with on-disk
//! identities under `<tmp>/agents/<name>/`.
use std::path::Path;

use chrono::Utc;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

use super::*;
use crate::ChannelService;
use crate::repair_record::REPAIR_EVENT_TYPE;
use crate::service::restart::RestartSigner;
use crate::verify::ROUTER_AGENT;

struct Fx {
    tmp: TempDir,
    svc: ChannelService,
    ch: String,
    agent: String,
    me: AgentIdentity,
    router: AgentIdentity,
}

fn plant(home: &Path, agent: &str) -> AgentIdentity {
    let id = AgentIdentity::generate();
    id.save(&home.join("agents").join(agent)).unwrap();
    id
}

impl Fx {
    /// A conversation between the local human and `agent`; the router
    /// (`mur`) has its own identity when `agent` is someone else.
    fn new(agent: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let me = plant(tmp.path(), agent);
        let router = if agent == ROUTER_AGENT {
            AgentIdentity::from_secret_bytes(&me.secret_bytes_for_handoff())
        } else {
            plant(tmp.path(), ROUTER_AGENT)
        };
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent(agent).unwrap().id;
        Self {
            tmp,
            svc,
            ch,
            agent: agent.into(),
            me,
            router,
        }
    }

    fn signed(&self, actor: ChannelActor, kind: EventKind, text: &str) -> u64 {
        self.svc
            .append_signed(
                &self.ch,
                &self.me,
                1,
                actor,
                kind,
                serde_json::json!({ "text": text }),
                None,
            )
            .unwrap()
            .seq
    }

    fn human(&self, text: &str) -> u64 {
        self.signed(ChannelActor::local_human(), EventKind::Message, text)
    }

    fn reply(&self, text: &str) -> u64 {
        let me = ChannelActor::Agent {
            id: self.agent.clone(),
        };
        self.signed(me, EventKind::Message, text)
    }

    fn unsigned(&self, actor: ChannelActor, text: &str) -> u64 {
        self.svc
            .append(
                &self.ch,
                actor,
                EventKind::Message,
                serde_json::json!({ "text": text }),
                None,
            )
            .unwrap()
            .seq
    }

    fn log(&self) -> std::path::PathBuf {
        self.svc.store().events_path(&self.ch).unwrap()
    }

    fn repair_as(&self, actor: ChannelActor, signer: &AgentIdentity) {
        self.svc
            .restart_chain(
                &self.ch,
                Utc::now(),
                actor,
                RestartSigner {
                    identity: signer,
                    kv: 1,
                    anchor_home: None,
                },
                || Ok(true),
                |_| Ok(serde_json::json!({ "type": REPAIR_EVENT_TYPE, "text": "repaired" })),
            )
            .unwrap()
            .expect("the repair ran");
    }

    fn assess_with(&self, writer: [u8; 32], cache: Option<u64>, seq: u64, input: &str) -> Verdict {
        let keys = Keys {
            mur_home: self.tmp.path(),
            agent: &self.agent,
            writer_pubkey: writer,
        };
        let cache = CacheState { watermark: cache };
        self.svc
            .store()
            .with_shared_lock(&self.ch, |s| {
                assess_locked(s, &self.ch, &keys, &cache, seq, input)
            })
            .unwrap()
    }

    fn assess(&self, cache: Option<u64>, seq: u64, input: &str) -> Verdict {
        self.assess_with(self.me.verifying_key_bytes(), cache, seq, input)
    }
}

fn ok(v: Verdict) -> Assessment {
    match v {
        Verdict::Ok(a) => a,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn damaged(v: Verdict) -> ConversationDamage {
    match v {
        Verdict::Damaged(d) => d,
        other => panic!("expected Damaged, got {other:?}"),
    }
}

fn msg(seq: u64, role: Role, text: &str) -> ProjectedMessage {
    ProjectedMessage {
        seq,
        role,
        text: text.into(),
    }
}

#[test]
fn fresh_channel_first_turn_passes_watermark_on_seq_0() {
    let fx = Fx::new("mur");
    assert_eq!(fx.human("hi"), 0);
    let a = ok(fx.assess(None, 0, "hi"));
    assert!(a.projected.is_empty());
    assert_eq!(a.log_last_seq, 0);
    assert_eq!(a.rebuilt_after_repair, None);
    assert!(!a.anchor_dirty);
}

#[test]
fn projects_only_human_and_self_messages_below_channel_seq() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.reply("b");
    fx.signed(
        ChannelActor::Agent { id: "other".into() },
        EventKind::Message,
        "c",
    );
    fx.human("d");
    let a = ok(fx.assess(None, 3, "d"));
    assert_eq!(
        a.projected,
        vec![msg(0, Role::User, "a"), msg(1, Role::Agent, "b")]
    );
}

#[test]
fn shell_note_at_channel_seq_is_accepted_when_signed_by_writer() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.reply("b");
    let note = fx.signed(ChannelActor::System, EventKind::Note, "$ ls\nfoo");
    let a = ok(fx.assess(None, note, "anything at all"));
    assert_eq!(
        a.projected,
        vec![msg(0, Role::User, "a"), msg(1, Role::Agent, "b")]
    );
}

#[test]
fn shell_note_before_channel_seq_is_projected_as_a_user_turn() {
    let fx = Fx::new("mur");
    fx.signed(ChannelActor::System, EventKind::Note, "$ ls\nfoo");
    fx.reply("you listed foo");
    fx.human("thanks");
    let a = ok(fx.assess(None, 2, "thanks"));
    assert_eq!(
        a.projected,
        vec![
            msg(0, Role::User, "$ ls\nfoo"),
            msg(1, Role::Agent, "you listed foo")
        ]
    );
}

#[test]
fn unsigned_shell_note_at_channel_seq_is_watermark() {
    let fx = Fx::new("mur");
    let seq = fx
        .svc
        .append(
            &fx.ch,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "$ ls" }),
            None,
        )
        .unwrap()
        .seq;
    let d = damaged(fx.assess(None, seq, "$ ls"));
    assert_eq!(d.reason(), "watermark", "{d}");
}

#[test]
fn watermark_text_mismatch_is_refused() {
    let fx = Fx::new("mur");
    fx.human("hi");
    let d = damaged(fx.assess(None, 0, "bye"));
    assert!(matches!(d, ConversationDamage::Watermark { .. }), "{d}");
}

#[test]
fn blank_input_normalizes_to_marker() {
    let fx = Fx::new("mur");
    fx.human(BLANK_USER_TURN);
    ok(fx.assess(None, 0, "   "));
}

#[test]
fn watermark_on_an_agent_event_is_refused() {
    let fx = Fx::new("mur");
    fx.human("hi");
    fx.reply("hi");
    let d = damaged(fx.assess(None, 1, "hi"));
    assert!(matches!(d, ConversationDamage::Watermark { .. }), "{d}");
}

#[test]
fn watermark_missing_event_is_refused() {
    let fx = Fx::new("mur");
    fx.human("hi");
    let d = damaged(fx.assess(None, 5, "hi"));
    assert!(matches!(d, ConversationDamage::Watermark { .. }), "{d}");
}

#[test]
fn events_file_missing_with_manifest_is_watermark() {
    let fx = Fx::new("mur");
    assert!(!fx.log().exists(), "precondition: no events yet");
    let d = damaged(fx.assess(None, 0, "hi"));
    assert!(matches!(d, ConversationDamage::Watermark { .. }), "{d}");
}

#[test]
fn log_past_channel_seq_is_folded_but_not_projected() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.reply("b");
    fx.human("c");
    fx.signed(ChannelActor::System, EventKind::Note, "later");
    let a = ok(fx.assess(None, 2, "c"));
    assert_eq!(
        a.projected,
        vec![msg(0, Role::User, "a"), msg(1, Role::Agent, "b")]
    );
    assert_eq!(a.log_last_seq, 3);
}

#[test]
fn repair_pending_is_repairing() {
    let fx = Fx::new("mur");
    fx.human("hi");
    crate::repair_marker::mark(fx.tmp.path(), &fx.ch, &Utc::now().to_rfc3339()).unwrap();
    assert!(matches!(fx.assess(None, 0, "hi"), Verdict::Repairing));
}

#[test]
fn cache_ahead_after_router_repair_rebuilds() {
    let fx = Fx::new("bob");
    fx.human("old");
    fx.repair_as(ChannelActor::System, &fx.router);
    let seq = fx.human("hi");
    assert_eq!(seq, 1);
    let a = ok(fx.assess(Some(10), 1, "hi"));
    assert_eq!(a.rebuilt_after_repair, Some(10));
    assert!(a.projected.is_empty(), "the repair record is not a turn");
    assert_eq!(a.log_last_seq, 1);
}

/// A cache miss after a router repair (the runtime restarted) reads the same
/// log: the router-signed seq 0 record is excluded from the signature row.
#[test]
fn cache_miss_after_router_repair_is_not_signature_damage() {
    let fx = Fx::new("bob");
    fx.repair_as(ChannelActor::System, &fx.router);
    fx.human("hi");
    let a = ok(fx.assess(None, 1, "hi"));
    assert_eq!(a.rebuilt_after_repair, None);
}

#[test]
fn cache_ahead_after_gate_b_repair_rebuilds() {
    let fx = Fx::new("bob");
    fx.human("old");
    fx.repair_as(ChannelActor::Agent { id: "bob".into() }, &fx.me);
    fx.human("hi");
    let a = ok(fx.assess(Some(10), 1, "hi"));
    assert_eq!(a.rebuilt_after_repair, Some(10));
}

#[test]
fn cache_ahead_seq0_repair_by_other_agent_is_log_regressed() {
    let fx = Fx::new("bob");
    let carol = plant(fx.tmp.path(), "carol");
    fx.repair_as(ChannelActor::Agent { id: "carol".into() }, &carol);
    fx.human("hi");
    let d = damaged(fx.assess(Some(10), 1, "hi"));
    assert_eq!(d, ConversationDamage::LogRegressed);
}

#[test]
fn cache_ahead_seq0_repair_record_with_a_forged_signature_is_log_regressed() {
    let fx = Fx::new("bob");
    let forger = AgentIdentity::generate();
    fx.repair_as(ChannelActor::System, &forger);
    fx.human("hi");
    let d = damaged(fx.assess(Some(10), 1, "hi"));
    assert_eq!(d, ConversationDamage::LogRegressed);
}

#[test]
fn cache_ahead_without_repair_record_is_log_regressed() {
    let fx = Fx::new("mur");
    fx.human("hi");
    let d = damaged(fx.assess(Some(3), 0, "hi"));
    assert_eq!(d, ConversationDamage::LogRegressed);
}

#[test]
fn cache_ahead_of_empty_log_is_log_regressed() {
    let fx = Fx::new("mur");
    let d = damaged(fx.assess(Some(1), 0, "hi"));
    assert_eq!(d, ConversationDamage::LogRegressed);
}

#[test]
fn cache_equal_to_log_is_not_a_regression() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.reply("b");
    fx.human("c");
    let a = ok(fx.assess(Some(2), 2, "c"));
    assert_eq!(a.rebuilt_after_repair, None);
}

#[test]
fn anchor_dirty_alone_proceeds() {
    let fx = Fx::new("mur");
    fx.human("hi");
    crate::anchor::mark_dirty(fx.tmp.path(), &fx.ch).unwrap();
    let a = ok(fx.assess(None, 0, "hi"));
    assert!(a.anchor_dirty);
}

#[test]
fn chain_break_is_refused() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.reply("b");
    fx.human("c");
    // Delete the first line: every remaining signature still verifies, but
    // the next line's `prev_hash` no longer chains.
    let raw = std::fs::read_to_string(fx.log()).unwrap();
    let rest: Vec<&str> = raw.lines().skip(1).collect();
    std::fs::write(fx.log(), rest.join("\n") + "\n").unwrap();
    let d = damaged(fx.assess(None, 2, "c"));
    assert_eq!(d, ConversationDamage::Chain);
}

/// Editing a signed line breaks its own signature and the next link; the
/// table checks `unverified` before `chain_breaks`, so it reads as signature.
#[test]
fn edited_line_is_signature_before_chain() {
    let fx = Fx::new("mur");
    fx.human("a");
    fx.human("b");
    let raw = std::fs::read_to_string(fx.log()).unwrap();
    std::fs::write(fx.log(), raw.replacen("\"a\"", "\"A\"", 1)).unwrap();
    let d = damaged(fx.assess(None, 1, "b"));
    assert_eq!(d, ConversationDamage::Signature);
}

fn anchored_human(fx: &Fx, signer: &AgentIdentity, kv: u32, text: &str) -> u64 {
    fx.svc
        .append_and_anchor(
            &fx.ch,
            signer,
            kv,
            &fx.tmp.path().join("agents").join(&fx.agent),
            ChannelActor::local_human(),
            EventKind::Message,
            serde_json::json!({ "text": text }),
            None,
        )
        .unwrap()
        .seq
}

#[test]
fn anchor_mismatch_is_refused() {
    let fx = Fx::new("mur");
    anchored_human(&fx, &fx.me, 1, "a");
    anchored_human(&fx, &fx.me, 1, "b");
    // Truncate below the signed head: the chain that is left still holds.
    let raw = std::fs::read_to_string(fx.log()).unwrap();
    std::fs::write(fx.log(), format!("{}\n", raw.lines().next().unwrap())).unwrap();
    let d = damaged(fx.assess(None, 0, "a"));
    assert_eq!(d, ConversationDamage::Anchor);
}

#[test]
fn unsigned_legacy_events_are_projected() {
    let fx = Fx::new("mur");
    fx.unsigned(ChannelActor::local_human(), "old q");
    fx.unsigned(ChannelActor::Agent { id: "mur".into() }, "old a");
    fx.unsigned(ChannelActor::local_human(), "new q");
    let a = ok(fx.assess(None, 2, "new q"));
    assert_eq!(
        a.projected,
        vec![msg(0, Role::User, "old q"), msg(1, Role::Agent, "old a")]
    );
}

#[test]
fn bad_signature_under_writer_key_is_refused() {
    let fx = Fx::new("mur");
    let stranger = AgentIdentity::generate();
    fx.svc
        .append_signed(
            &fx.ch,
            &stranger,
            1,
            ChannelActor::local_human(),
            EventKind::Message,
            serde_json::json!({ "text": "hi" }),
            None,
        )
        .unwrap();
    let d = damaged(fx.assess(None, 0, "hi"));
    assert_eq!(d, ConversationDamage::Signature);
}

#[test]
fn one_unparseable_line_names_its_number() {
    let fx = Fx::new("mur");
    fx.human("a");
    let mut raw = std::fs::read_to_string(fx.log()).unwrap();
    raw.push_str("{not json\n");
    std::fs::write(fx.log(), raw).unwrap();
    let d = damaged(fx.assess(None, 0, "a"));
    assert_eq!(d, ConversationDamage::Unparseable { lines: vec![2] });
}

/// `<agent_home>/rotations.jsonl` for `old` (v0) → `new` (v1), signed by
/// `old`, then the key files swapped the way `mur agent rekey` leaves them.
fn rekey(agent_home: &Path, old: &AgentIdentity) -> AgentIdentity {
    use mur_common::identity::{RotationAttestation, RotationReason, encode_pubkey};
    let new = AgentIdentity::generate();
    let enc = |i: &AgentIdentity| encode_pubkey(&i.signing_key().verifying_key());
    let boot = RotationAttestation::new(
        "u",
        "",
        enc(old),
        0,
        0,
        "2026-01-01T00:00:00Z",
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let mut step = RotationAttestation::new(
        "u",
        enc(old),
        enc(&new),
        0,
        1,
        "2026-02-01T00:00:00Z",
        RotationReason::Scheduled,
    );
    step.sign(old.signing_key());
    let lines: Vec<String> = [boot, step]
        .iter()
        .map(|a| serde_json::to_string(a).unwrap())
        .collect();
    std::fs::write(agent_home.join("rotations.jsonl"), lines.join("\n") + "\n").unwrap();
    let key_dir = mur_common::identity::private_key_dir(agent_home);
    for (dir, f) in [
        (key_dir.as_path(), "identity.key"),
        (agent_home, "identity.pub"),
    ] {
        std::fs::rename(dir.join(f), dir.join(format!("{f}.prev"))).unwrap();
    }
    new.save(agent_home).unwrap();
    new
}

/// §9 "Head sealed before a rekey": the head and the lines written under the
/// old key are re-checked under the key the verified rotation history gives
/// their `key_version`; no refusal.
#[test]
fn rekeyed_head_is_rechecked_under_old_key() {
    let fx = Fx::new("mur");
    anchored_human(&fx, &fx.me, 0, "hi");
    let new = rekey(&fx.tmp.path().join("agents").join("mur"), &fx.me);
    let a = ok(fx.assess_with(new.verifying_key_bytes(), None, 0, "hi"));
    assert!(a.projected.is_empty());
}

/// The same swap without a rotation history is still damage: an old version
/// is trusted only through the verified chain.
#[test]
fn key_swap_without_rotation_history_is_damage() {
    let fx = Fx::new("mur");
    fx.human("hi");
    let stranger = AgentIdentity::generate();
    let d = damaged(fx.assess_with(stranger.verifying_key_bytes(), None, 0, "hi"));
    assert_eq!(d, ConversationDamage::Signature);
}

#[test]
fn damage_serializes_as_the_kebab_reason() {
    let v = serde_json::to_value(ConversationDamage::Unparseable { lines: vec![2] }).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "reason": "unparseable", "lines": [2] })
    );
    let v = serde_json::to_value(ConversationDamage::LogRegressed).unwrap();
    assert_eq!(v, serde_json::json!({ "reason": "log-regressed" }));
    assert_eq!(ConversationDamage::LogRegressed.reason(), "log-regressed");
}
