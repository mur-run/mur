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
    match mur_common::identity::AgentIdentity::load(&agent_home) {
        Ok(id) => {
            let kv = read_key_version(&agent_home);
            svc.append_signed(channel_id, &id, kv, actor, kind, payload, idem)
        }
        // No key at all: the legitimate bootstrap case (a fresh home, a test,
        // a workflow channel with no agent behind it). Unsigned is correct.
        Err(mur_common::identity::IdentityError::NotFound) => {
            svc.append(channel_id, actor, kind, payload, idem)
        }
        // The key is THERE and we may not read it — a sandbox deny (a spawned
        // `mur` cannot read a sibling's signing key since #975). Falling back
        // to unsigned here is a silent security downgrade: the event was meant
        // to be signed, and with `require_sig` off the reader accepts it, so
        // the whole v3d signing guarantee lapses with nothing to show for it.
        // The mirror of the read-side fix in `channel_verify::verify_event`.
        Err(e) => {
            if crate::channel_verify::require_sig_from_env() {
                // The reader would reject an unsigned event anyway; fail here,
                // where the cause is still legible, instead of at verification.
                anyhow::bail!(
                    "refusing to write an unsigned event to '{channel_id}': the \
                     writer key for '{router_agent}' is unreadable ({e}), and \
                     MUR_CHANNEL_REQUIRE_SIG is set"
                );
            }
            tracing::warn!(
                channel_id,
                router_agent,
                error = %e,
                "writer signing key is present but unreadable — writing this \
                 event UNSIGNED. Signature verification is not protecting this \
                 channel while that holds."
            );
            svc.append(channel_id, actor, kind, payload, idem)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;
    use tempfile::TempDir;

    /// An UNREADABLE writer key must not silently produce an unsigned event.
    ///
    /// This is the write-side mirror of `channel_verify::verify_event`: there,
    /// an unreadable key must not pass as an absent signature; here, it must
    /// not pass as "no key, so unsigned is fine". A sandboxed `mur` cannot read
    /// a sibling's signing key (#975), so before this every channel event such
    /// a process wrote was unsigned — and with `require_sig` off the reader
    /// accepted it, so the signing guarantee lapsed with no signal anywhere.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_writer_key_does_not_silently_write_unsigned() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let agent_home = home.join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        AgentIdentity::generate().save(&agent_home).unwrap();
        let key = agent_home.join("identity.key");
        let svc = ChannelService::open(home).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();

        // Precondition: with the key readable, the event IS signed.
        let signed = append_as_writer(
            &svc,
            home,
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({"text": "before"}),
            None,
        )
        .unwrap();
        assert!(signed.sig.is_some(), "precondition: signing works");

        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        // MUR_CHANNEL_REQUIRE_SIG is not set in the test env, so this takes the
        // warn-and-write path — the event is unsigned, but LOUDLY so. What is
        // asserted here is the discrimination itself: the loader must report
        // this as Denied, not NotFound, which is what makes the warning
        // possible at all.
        let err = mur_common::identity::AgentIdentity::load(&agent_home).unwrap_err();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(
            matches!(err, mur_common::identity::IdentityError::Denied(_)),
            "an unreadable writer key must be distinguishable from an absent \
             one, or the unsigned fallback stays silent: {err:?}"
        );
    }

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
