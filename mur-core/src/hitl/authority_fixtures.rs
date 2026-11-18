//! Test fixtures for C3: a router rekey with a chosen reason (D-10), a human
//! rekey (D-9), and human-key writes that go through the production writer.

use std::path::Path;

use mur_channel::ChannelService;
use mur_channel::service::{ChannelOwner, EnrollGate, HumanSign};
use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};

use crate::channel_owner::HumanOwner;
use crate::channel_writer::ROUTER_AGENT;

/// Rotate the router `old` → a fresh key the way `mur agent rekey` leaves the
/// files: a signed v0 → v1 attestation with `reason`, the old halves moved to
/// `.prev`, the new key in place, and the profile's `key_version` bumped.
pub(crate) fn rekey_router(
    home: &Path,
    old: &AgentIdentity,
    reason: RotationReason,
) -> AgentIdentity {
    let agent_home = home.join("agents").join(ROUTER_AGENT);
    let new = AgentIdentity::generate();
    let boot = RotationAttestation::new(
        "u",
        "",
        old.pubkey_text(),
        0,
        0,
        "2026-01-01T00:00:00Z",
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let mut step = RotationAttestation::new(
        "u",
        old.pubkey_text(),
        new.pubkey_text(),
        0,
        1,
        "2026-02-01T00:00:00Z",
        reason,
    );
    step.sign(old.signing_key());
    let lines: Vec<String> = [boot, step]
        .iter()
        .map(|a| serde_json::to_string(a).unwrap())
        .collect();
    std::fs::write(agent_home.join("rotations.jsonl"), lines.join("\n") + "\n").unwrap();
    let key_dir = mur_common::identity::private_key_dir(&agent_home);
    for (dir, f) in [(&key_dir, "identity.key"), (&agent_home, "identity.pub")] {
        std::fs::rename(dir.join(f), dir.join(format!("{f}.prev"))).unwrap();
    }
    new.save(&agent_home).unwrap();
    let yaml = crate::executor::delegation::grant::tests::profile_yaml(ROUTER_AGENT, &[]);
    let mut p: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    p.identity.key_version = 1;
    std::fs::write(
        agent_home.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
    new
}

/// `mur identity rekey --reason scheduled --yes`.
pub(crate) fn rekey_human(home: &Path) {
    crate::cmd::identity::run(
        home,
        crate::cmd::identity::IdentityAction::Rekey {
            reason: "scheduled".into(),
            yes: true,
        },
    )
    .unwrap();
}

/// A workflow channel the local human owns from seq 0 (B3).
pub(crate) fn owned_workflow(home: &Path) -> String {
    let owner = HumanOwner::load(home).unwrap();
    ChannelService::open(home)
        .unwrap()
        .create_for_workflow("g", owner.owner())
        .unwrap()
        .id
}

/// Enroll the local human on a legacy channel (C1) without writing an event
/// of its own: `human_cut` is the log's current last line.
pub(crate) fn enroll(home: &Path, ch: &str) {
    let owner = HumanOwner::load(home).unwrap();
    let ChannelOwner::Human { signer, router } = owner.owner() else {
        unreachable!()
    };
    ChannelService::open(home)
        .unwrap()
        .enroll_owner_key_if_absent(ch, signer, router, &open_gate())
        .unwrap()
        .expect("enrolled");
}

fn open_gate() -> EnrollGate<'static> {
    EnrollGate {
        identity_protected: true,
        log_intact: &|| Ok(true),
        legacy_sealer: None,
    }
}

/// Write `payload` as the human through `append_human`, signed by `signer`
/// (the loaded human key, or a stand-in that lies about its key or version).
pub(crate) fn human_write(
    home: &Path,
    ch: &str,
    signer: &dyn HumanSign,
    kind: EventKind,
    payload: serde_json::Value,
) -> ChannelEvent {
    let owner = HumanOwner::load(home).unwrap();
    let ChannelOwner::Human { router, .. } = owner.owner() else {
        unreachable!()
    };
    ChannelService::open(home)
        .unwrap()
        .append_human(ch, signer, router, &open_gate(), kind, payload, None)
        .unwrap()
}

/// The current human key, through the production loader. Enrolls first, as
/// every production caller does, so a test that never made an owned channel
/// still has a key to sign with.
pub(crate) fn human_signer(home: &Path) -> crate::identity_enroll::HumanSigner {
    crate::identity_enroll::ensure_enrolled(home).unwrap();
    crate::identity_enroll::HumanSigner::load(home).unwrap()
}

/// A human signer that signs with `key` but reports `key_version` — the
/// shape of an old key carrying the head's label (D-9 rev 4).
pub(crate) struct Labelled {
    pub key: AgentIdentity,
    pub key_version: u32,
}

impl HumanSign for Labelled {
    fn pubkey(&self) -> [u8; 32] {
        self.key.verifying_key_bytes()
    }
    fn key_version(&self) -> u32 {
        self.key_version
    }
    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.key.sign_bytes(msg)
    }
}

/// The human key a rekey set aside (`keys/.human/identity.key.prev`).
pub(crate) fn previous_human_key(home: &Path) -> AgentIdentity {
    let dir = mur_common::identity::human::human_key_dir(home);
    let bytes = std::fs::read(dir.join("identity.key.prev")).unwrap();
    AgentIdentity::from_secret_bytes(bytes.as_slice().try_into().unwrap())
}
