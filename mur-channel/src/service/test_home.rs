//! Shared fixture for the owner tests (C1 enroll, C5 restart): a `mur_home`
//! with a human identity + rotation chain and a router key.

use std::fs;
use std::path::Path;

use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::human::{human_identity_dir, human_key_dir, human_rotations_path};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use tempfile::TempDir;

use super::{ChannelOwner, ChannelService, EnrollGate, HumanSign, RouterSeal};
use crate::head_io::read_head;
use crate::verify::human::HumanTrust;

pub(super) struct TestHuman(pub(super) AgentIdentity, pub(super) u32);

impl HumanSign for TestHuman {
    fn pubkey(&self) -> [u8; 32] {
        self.0.verifying_key_bytes()
    }
    fn key_version(&self) -> u32 {
        self.1
    }
    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.0.sign_bytes(msg)
    }
}

pub(super) struct Home {
    pub(super) dir: TempDir,
    pub(super) human: TestHuman,
    pub(super) router: AgentIdentity,
}

pub(super) fn bootstrap(h: &AgentIdentity) -> RotationAttestation {
    RotationAttestation::new(
        "u",
        "",
        h.pubkey_text(),
        0,
        0,
        "2026-11-09T00:00:00Z",
        RotationReason::Scheduled,
    )
    .into_bootstrap()
}

impl Home {
    pub(super) fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let h = AgentIdentity::generate();
        h.save_split(&human_identity_dir(dir.path()), &human_key_dir(dir.path()))
            .unwrap();
        fs::write(
            human_rotations_path(dir.path()),
            format!("{}\n", serde_json::to_string(&bootstrap(&h)).unwrap()),
        )
        .unwrap();
        let router = AgentIdentity::from_secret_bytes(&[3u8; 32]);
        router
            .save(&dir.path().join("agents").join(crate::verify::ROUTER_AGENT))
            .unwrap();
        Self {
            dir,
            human: TestHuman(h, 0),
            router,
        }
    }

    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(super) fn router_home(&self) -> std::path::PathBuf {
        self.path().join("agents").join(crate::verify::ROUTER_AGENT)
    }

    pub(super) fn svc(&self) -> ChannelService {
        ChannelService::open(self.path()).unwrap()
    }

    pub(super) fn seal<'a>(&'a self, rh: &'a Path) -> RouterSeal<'a> {
        RouterSeal {
            identity: &self.router,
            kv: 0,
            key_home: rh,
        }
    }

    /// A legacy conversation with `n` agent-signed messages (no owner record,
    /// no head: what every pre-phase-2 channel is).
    pub(super) fn legacy_channel(&self, n: usize) -> String {
        let svc = self.svc();
        let ch = svc.create_for_agent("qa", ChannelOwner::Legacy).unwrap();
        for i in 0..n {
            svc.store()
                .append_event(
                    &ch.id,
                    ChannelActor::local_human(),
                    EventKind::Message,
                    serde_json::json!({ "text": format!("m{i}") }),
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        ch.id
    }

    pub(super) fn log(&self, id: &str) -> String {
        fs::read_to_string(self.svc().store().events_path(id).unwrap()).unwrap_or_default()
    }

    pub(super) fn trust(&self, id: &str) -> HumanTrust {
        let head = read_head(&crate::anchor::heads_dir(self.path()), id).unwrap();
        HumanTrust::from_log(self.path(), id, &self.log(id), head.as_ref())
    }
}

pub(super) fn manifest_path(h: &Home, id: &str) -> std::path::PathBuf {
    h.path().join("channels").join(id).join("channel.yaml")
}

pub(super) fn open_gate() -> EnrollGate<'static> {
    EnrollGate {
        identity_protected: true,
        log_intact: &|| Ok(true),
        legacy_sealer: None,
    }
}

pub(super) fn say(
    h: &Home,
    id: &str,
    gate: &EnrollGate<'_>,
    text: &str,
) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    let rh = h.router_home();
    h.svc().append_human(
        id,
        &h.human,
        h.seal(&rh),
        gate,
        EventKind::Message,
        serde_json::json!({ "text": text }),
        None,
    )
}
