//! B3: creators take a `ChannelOwner`.

use std::fs;
use std::path::Path;

use mur_common::channel::{ChannelActor, EventKind, OwnerKeyEnrolledPayload};
use mur_common::identity::human::{human_identity_dir, human_key_dir, human_rotations_path};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use tempfile::TempDir;

use super::*;
use crate::head_io::read_head;
use crate::verify::human::HumanTrust;

struct TestHuman(AgentIdentity);

impl HumanSign for TestHuman {
    fn pubkey(&self) -> [u8; 32] {
        self.0.verifying_key_bytes()
    }
    fn key_version(&self) -> u32 {
        0
    }
    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.0.sign_bytes(msg)
    }
}

/// A home with an enrolled human chain and a router identity.
struct Home {
    dir: TempDir,
    human: TestHuman,
    router: AgentIdentity,
}

impl Home {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let h = AgentIdentity::generate();
        h.save_split(&human_identity_dir(dir.path()), &human_key_dir(dir.path()))
            .unwrap();
        let boot = RotationAttestation::new(
            "u",
            "",
            h.pubkey_text(),
            0,
            0,
            "2026-11-09T00:00:00Z",
            RotationReason::Scheduled,
        )
        .into_bootstrap();
        fs::write(
            human_rotations_path(dir.path()),
            format!("{}\n", serde_json::to_string(&boot).unwrap()),
        )
        .unwrap();
        let router = AgentIdentity::from_secret_bytes(&[3u8; 32]);
        router
            .save(&dir.path().join("agents").join(crate::verify::ROUTER_AGENT))
            .unwrap();
        Self {
            dir,
            human: TestHuman(h),
            router,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn router_home(&self) -> std::path::PathBuf {
        self.path().join("agents").join(crate::verify::ROUTER_AGENT)
    }

    fn svc(&self) -> ChannelService {
        ChannelService::open(self.path()).unwrap()
    }
}

fn human<'a>(h: &'a Home, router_home: &'a Path) -> ChannelOwner<'a> {
    ChannelOwner::Human {
        signer: &h.human,
        router: RouterSeal {
            identity: &h.router,
            kv: 0,
            key_home: router_home,
        },
    }
}

#[test]
fn create_human_writes_v3_then_anchored_seq0_record() {
    let h = Home::new();
    let rh = h.router_home();
    let ch = h.svc().create_for_agent("qa", human(&h, &rh)).unwrap();
    assert_eq!(ch.v, mur_common::channel::CHANNEL_SCHEMA_VERSION);
    let manifest = h.svc().store().load_manifest(&ch.id).unwrap();
    assert_eq!(manifest.v, 3);

    let events = h.svc().load_events(&ch.id).unwrap();
    assert_eq!(events.len(), 1);
    let rec = &events[0];
    assert_eq!(rec.seq, 0);
    assert_eq!(rec.kind, EventKind::OwnerKeyEnrolled);
    let payload: OwnerKeyEnrolledPayload = serde_json::from_value(rec.payload.clone()).unwrap();
    assert_eq!(payload.human_cut, None);
    assert_eq!(payload.key_version, 0);
    assert!(
        matches!(&rec.actor, ChannelActor::Human { key: Some(k), .. } if *k == payload.owner_key)
    );

    // Anchored: the head covers seq 0 and is sealed by the router.
    let dir = crate::anchor::heads_dir(h.path());
    let head = read_head(&dir, &ch.id).unwrap().expect("head written");
    assert_eq!(head.seq, 0);
    assert!(head.verify(&h.router.verifying_key_bytes()));

    // And the record is live under B2's trust.
    let log = fs::read_to_string(h.svc().store().events_path(&ch.id).unwrap()).unwrap();
    let trust = HumanTrust::from_log(h.path(), &ch.id, &log, Some(&head));
    assert_eq!(trust.live_record().map(|r| r.seq), Some(0));
    assert!(trust.damage().is_empty());
}

#[test]
fn create_legacy_is_unchanged() {
    let h = Home::new();
    let ch = h
        .svc()
        .create_for_agent("qa", ChannelOwner::Legacy)
        .unwrap();
    assert!(h.svc().load_events(&ch.id).unwrap().is_empty());
    assert!(
        read_head(&crate::anchor::heads_dir(h.path()), &ch.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn every_creator_takes_an_owner() {
    let h = Home::new();
    let rh = h.router_home();
    let svc = h.svc();
    let fleet = svc
        .create_for_fleet("f", "mur", &["qa".into()], human(&h, &rh))
        .unwrap();
    let wf = svc.create_for_workflow("w", human(&h, &rh)).unwrap();
    for id in [fleet.id, wf.id] {
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 1, "{id}");
        assert_eq!(evs[0].kind, EventKind::OwnerKeyEnrolled);
    }
}

/// Crash between the manifest and the record: the channel is a v3 manifest
/// with an empty log — legacy-shaped, and never trusted.
#[test]
fn manifest_without_record_is_legacy_shaped_and_untrusted() {
    let h = Home::new();
    let svc = h.svc();
    let ch = svc.create_for_agent("qa", ChannelOwner::Legacy).unwrap();
    // The state a crash after `store.create` leaves behind.
    assert_eq!(svc.store().load_manifest(&ch.id).unwrap().v, 3);
    let log = fs::read_to_string(svc.store().events_path(&ch.id).unwrap()).unwrap_or_default();
    let trust = HumanTrust::from_log(h.path(), &ch.id, &log, None);
    assert!(trust.live_record().is_none());
    assert!(trust.damage().is_empty());
}
