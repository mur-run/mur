//! B4: integrity sees owner state. Human-signed lines go in through the
//! production store (`append_presigned`), then a router-written note anchors
//! the head over them, the way C1's ordering does.

use super::*;
use crate::channel_writer::{append_as_writer, plant_writer_identity};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelEvent, HumanCut};
use mur_common::identity::human::{human_identity_dir, human_key_dir, human_rotations_path};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use tempfile::TempDir;

const ROUTER: &str = "mur";

/// Enroll a single-key human chain under `home`.
fn enroll_human(home: &Path) -> AgentIdentity {
    let h = AgentIdentity::generate();
    let att = RotationAttestation::new(
        "u",
        String::new(),
        h.pubkey_text(),
        0,
        0,
        "2026-11-09T00:00:00Z",
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    h.save_split(&human_identity_dir(home), &human_key_dir(home))
        .unwrap();
    std::fs::write(
        human_rotations_path(home),
        format!("{}\n", serde_json::to_string(&att).unwrap()),
    )
    .unwrap();
    h
}

fn keyed(h: &AgentIdentity) -> ChannelActor {
    ChannelActor::Human {
        name: "u".into(),
        key: Some(h.pubkey_text()),
    }
}

struct Owned {
    home: TempDir,
    svc: ChannelService,
    id: String,
}

impl Owned {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        plant_writer_identity(home.path());
        let svc = ChannelService::open(home.path()).unwrap();
        let id = svc.create_for_workflow("owner").unwrap().id;
        Self { home, svc, id }
    }

    fn note(&self, i: usize) -> ChannelEvent {
        append_as_writer(
            &self.svc,
            self.home.path(),
            &self.id,
            ROUTER,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": format!("event {i}") }),
            None,
        )
        .unwrap()
    }

    /// Append a line signed by `signer` claiming `actor`, unanchored.
    fn presigned(
        &self,
        signer: &AgentIdentity,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ChannelEvent {
        let sig = mur_channel::sign::sign_event(signer, &self.id, &actor, kind, &payload, None);
        self.svc
            .append_presigned(&self.id, actor, kind, payload, None, sig, Some(0))
            .unwrap()
    }

    fn record(&self, signer: &AgentIdentity, cut: Option<HumanCut>) -> ChannelEvent {
        let payload = serde_json::json!({
            "owner_key": signer.pubkey_text(), "key_version": 0, "human_cut": cut,
        });
        self.presigned(signer, keyed(signer), EventKind::OwnerKeyEnrolled, payload)
    }

    fn check(&self) -> IntegrityReport {
        check(self.home.path(), &self.id).unwrap()
    }
}

#[test]
fn human_signed_line_is_intact() {
    let f = Owned::new();
    let h = enroll_human(f.home.path());
    let rec = f.record(&h, None);
    f.presigned(
        &h,
        keyed(&h),
        EventKind::Message,
        serde_json::json!({ "text": "hi" }),
    );
    f.note(0); // anchors the head over both human lines
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.owner.live_record_seq, Some(rec.seq));
    assert!(r.unverified_seqs.is_empty());
}

#[test]
fn owner_unanchored_is_not_intact() {
    let f = Owned::new();
    f.note(0);
    let h = enroll_human(f.home.path());
    f.record(&h, None); // past the head: nothing sealed it
    let r = f.check();
    assert!(r.owner.unanchored, "{}", r.render());
    assert!(!r.is_intact());
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
    assert!(r.render().contains("not covered by the signed head"));
}

#[test]
fn owner_cut_mismatch_is_not_intact() {
    let f = Owned::new();
    let first = f.note(0);
    let h = enroll_human(f.home.path());
    let bogus = HumanCut {
        seq: first.seq,
        line_hash: "00".repeat(32),
    };
    f.record(&h, Some(bogus));
    f.note(1);
    let r = f.check();
    assert!(r.owner.cut_mismatch, "{}", r.render());
    assert!(!r.is_intact());
}

#[test]
fn foreign_record_reported_but_intact() {
    let f = Owned::new();
    enroll_human(f.home.path());
    let stranger = AgentIdentity::generate(); // another machine's human key
    let rec = f.record(&stranger, None);
    f.note(0);
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.owner.ignored_records, vec![rec.seq]);
    assert_eq!(r.owner.live_record_seq, None);
    assert!(r.render().contains("ignored, not damage"));
}

#[test]
fn repair_does_not_quarantine_healthy_v3_log() {
    use crate::cmd::channel_repair::{Confirm, Outcome, repair};
    let f = Owned::new();
    let h = enroll_human(f.home.path());
    f.record(&h, None);
    f.note(0);
    let path = mur_channel::ChannelStore::new(f.home.path())
        .events_path(&f.id)
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let out = repair(f.home.path(), &f.id, Confirm::Yes).unwrap();
    assert!(matches!(out, Outcome::Intact), "{out:?}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
