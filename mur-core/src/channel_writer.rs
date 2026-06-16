//! Append channel events signed by the channel's router/owner agent (v3d).
//!
//! The channel's WRITER (the router/owner agent, e.g. the `"mur"` concierge)
//! signs the events it appends so a downstream reader can verify authority
//! before trusting an event (notably the HITL gate verifying a `HitlResponse`).
//!
//! Migration-safe: if the router's identity is unavailable we fall back to an
//! unsigned `append`, so existing channels (and tests that never created an
//! identity) keep working. Enforcement is opt-in via `MUR_CHANNEL_REQUIRE_SIG`
//! on the verification side.
use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// The channel's router/owner agent — the trusted WRITER that signs the events
/// appended to workflow/HITL channels (v3d). The concierge `"mur"` owns these
/// channels; its on-disk identity (`<home>/agents/mur/`) signs `HitlRequest`/
/// `HitlResponse`/workflow events, and the HITL gate verifies an incoming
/// `HitlResponse` against this agent's pubkey before releasing. Internal
/// directory slug, so lowercase (matches the on-disk `name`).
pub const ROUTER_AGENT: &str = "mur";

/// Read the router agent's current key version from its `profile.yaml`
/// (`identity.key_version`). Returns 0 on any failure (missing profile,
/// malformed YAML), matching the bootstrap key version.
fn read_key_version(agent_home: &Path) -> u32 {
    let profile_path = agent_home.join("profile.yaml");
    let Ok(yaml) = std::fs::read_to_string(&profile_path) else {
        return 0;
    };
    match serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml) {
        Ok(profile) => profile.identity.key_version,
        Err(_) => 0,
    }
}

/// Append `actor`/`kind`/`payload` to `channel_id`, SIGNED by `router_agent`'s
/// identity when it is available, else unsigned (migration-safe).
///
/// `home` is the `~/.mur` root; the router identity is loaded from
/// `<home>/agents/<router_agent>/identity.{key,pub}` and its key version from
/// that agent's `profile.yaml`.
#[allow(clippy::too_many_arguments)]
pub fn append_as_writer(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
    idem: Option<String>,
) -> anyhow::Result<ChannelEvent> {
    let agent_home = home.join("agents").join(router_agent);
    if let Ok(id) = mur_common::identity::AgentIdentity::load(&agent_home) {
        let kv = read_key_version(&agent_home);
        return svc.append_signed(channel_id, &id, kv, actor, kind, payload, idem);
    }
    svc.append(channel_id, actor, kind, payload, idem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;
    use tempfile::TempDir;

    #[test]
    fn unsigned_when_identity_absent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        let ev = append_as_writer(
            &svc,
            tmp.path(),
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "hi" }),
            None,
        )
        .unwrap();
        assert!(ev.sig.is_none(), "no identity → unsigned (migration-safe)");
    }

    #[test]
    fn signed_when_identity_present_and_verifies() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        // Plant the router identity under <home>/agents/mur/.
        let agent_home = tmp.path().join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let id = AgentIdentity::generate();
        id.save(&agent_home).unwrap();

        let ev = append_as_writer(
            &svc,
            tmp.path(),
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::HitlResponse,
            serde_json::json!({ "allow": true }),
            None,
        )
        .unwrap();
        assert!(ev.sig.is_some(), "identity present → signed");
        let pubkey = id.verifying_key_bytes();
        assert!(
            mur_channel::sign::verify_one(&ch.id, &ev, &pubkey, true),
            "signed event must verify against the router pubkey even when require_sig"
        );
    }
}
