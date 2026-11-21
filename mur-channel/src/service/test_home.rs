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

pub(crate) struct TestHuman(pub(crate) AgentIdentity, pub(crate) u32);

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

pub(crate) struct Home {
    pub(crate) dir: TempDir,
    pub(crate) human: TestHuman,
    pub(crate) router: AgentIdentity,
}

pub(crate) fn bootstrap(h: &AgentIdentity) -> RotationAttestation {
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
    pub(crate) fn new() -> Self {
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
            .save(
                &dir.path()
                    .join("agents")
                    .join(crate::verify::CONTROL_PLANE_AGENT),
            )
            .unwrap();
        Self {
            dir,
            human: TestHuman(h, 0),
            router,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn router_home(&self) -> std::path::PathBuf {
        self.path()
            .join("agents")
            .join(crate::verify::CONTROL_PLANE_AGENT)
    }

    pub(crate) fn svc(&self) -> ChannelService {
        ChannelService::open(self.path()).unwrap()
    }

    pub(crate) fn seal<'a>(&'a self, rh: &'a Path) -> RouterSeal<'a> {
        RouterSeal {
            identity: &self.router,
            kv: 0,
            key_home: rh,
        }
    }

    /// A legacy conversation with `n` agent-signed messages (no owner record,
    /// no head: what every pre-phase-2 channel is).
    pub(crate) fn legacy_channel(&self, n: usize) -> String {
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

    pub(crate) fn log(&self, id: &str) -> String {
        fs::read_to_string(self.svc().store().events_path(id).unwrap()).unwrap_or_default()
    }

    pub(crate) fn trust(&self, id: &str) -> HumanTrust {
        let head = read_head(&crate::anchor::heads_dir(self.path()), id).unwrap();
        HumanTrust::from_log(self.path(), id, &self.log(id), head.as_ref())
    }
}

pub(crate) fn manifest_path(h: &Home, id: &str) -> std::path::PathBuf {
    h.path().join("channels").join(id).join("channel.yaml")
}

pub(crate) fn open_gate() -> EnrollGate<'static> {
    EnrollGate {
        identity_protected: true,
        log_intact: &|| Ok(true),
        legacy_sealer: None,
    }
}

pub(crate) fn say(
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
