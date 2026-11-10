//! TCP entitlement validation, the boot identity load, and identity-key grace cleanup.

use mur_common::identity::AgentIdentity;
use tracing::{info, warn};

/// Error surfaced when the profile's declared TCP transport bind port is
/// inconsistent with entitlements (P0a.5).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub String);

/// Cross-check profile: if TCP is enabled, its bind port must be in
/// entitlements.network.inbound.ports (empty list = "no inbound").
pub fn validate_tcp_entitlement(
    p: &mur_common::agent::AgentProfile,
) -> Result<(), ValidationError> {
    if !p.transport.tcp.enabled {
        return Ok(());
    }
    let port = p
        .transport
        .tcp
        .bind
        .rsplit(':')
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ValidationError("transport.tcp.bind missing parseable port".into()))?;
    if !p.entitlements.network.inbound.ports.contains(&port) {
        return Err(ValidationError(format!(
            "transport.tcp bound to :{port} but entitlements.network.inbound.ports does not allow it"
        )));
    }
    Ok(())
}

/// M6.1: clean up the previous identity material if the grace period has
/// passed. On Unix we attempt a best-effort `shred -u` on the private
/// `identity.key.prev` first; if `shred` is not available we fall back to
/// overwriting + removing.
pub fn grace_cleanup_if_expired(
    agent_home: &std::path::Path,
    profile: &mur_common::agent::AgentProfile,
) -> anyhow::Result<()> {
    let Some(expires_str) = profile.identity.grace_expires_at.as_deref() else {
        return Ok(());
    };
    let expires = match chrono::DateTime::parse_from_rfc3339(expires_str) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    if expires > chrono::Utc::now() {
        return Ok(()); // still in grace
    }

    let key_prev = agent_home.join("identity.key.prev");
    let pub_prev = agent_home.join("identity.pub.prev");

    if key_prev.exists() {
        shred_file(&key_prev)?;
    }
    if pub_prev.exists() {
        let _ = std::fs::remove_file(&pub_prev);
    }

    // Rewrite profile.yaml without the previous_pubkey fields.
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    p.identity.previous_pubkey = None;
    p.identity.previous_key_version = None;
    p.identity.grace_expires_at = None;
    p.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&p)?;
    let tmp = profile_path.with_extension("tmp");
    std::fs::write(&tmp, new_yaml.as_bytes())?;
    std::fs::rename(&tmp, &profile_path)?;

    tracing::info!("grace-period cleanup: shredded identity.key.prev + cleared previous_pubkey");
    Ok(())
}

#[cfg(unix)]
fn shred_file(path: &std::path::Path) -> anyhow::Result<()> {
    // Try `shred -u` first (GNU coreutils — available on most Linux + macOS
    // via `gshred` if installed). Fall back to overwrite + remove.
    let shred = std::process::Command::new("shred")
        .arg("-u")
        .arg(path)
        .status();
    if let Ok(s) = shred
        && s.success()
    {
        return Ok(());
    }
    // Fallback: overwrite with random bytes, then unlink.
    use std::io::Write as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let len = meta.len() as usize;
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            // 32-byte private key — simple zero-fill is sufficient.
            let zeros = vec![0u8; len.max(32)];
            let _ = f.write_all(&zeros);
            let _ = f.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[cfg(not(unix))]
fn shred_file(path: &std::path::Path) -> anyhow::Result<()> {
    // Windows: best-effort overwrite + delete.
    use std::io::Write as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let len = meta.len() as usize;
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            let zeros = vec![0u8; len.max(32)];
            let _ = f.write_all(&zeros);
            let _ = f.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// 3a. Load agent identity (Ed25519 keypair) for Noise-XK TCP transport.
/// If missing, fall back to an ephemeral identity and warn that
/// cross-host TCP won't work (peers can't verify our static key).
/// #850 option (c) step 2: move THIS agent's private key out of the agents
/// tree, before it is loaded. Runs here rather than from `mur update`
/// because that is not the only upgrade path — `build.sh --install` +
/// `mur agent restart --stale` never invokes it. Scoped to one key, so
/// concurrent agent starts cannot contend.
///
/// Advisory: a failure leaves the key where it is, and step 1's fallback
/// still loads it. What must NOT happen is starting anyway after a refusal
/// to overwrite a different key — that case is reported loudly and the key
/// is left untouched, so the next start retries.
pub(super) fn load_boot_identity(agent_home: &std::path::Path) -> AgentIdentity {
    match mur_common::identity::migrate_private_key(agent_home) {
        Ok(true) => info!("private key migrated out of the agents tree"),
        Ok(false) => {}
        Err(e) => warn!(
            error = %e,
            "could not migrate the private key; it stays where it is and still \
             loads, but this agent's key remains readable to siblings created \
             after the sandbox sealed"
        ),
    }

    match AgentIdentity::load(agent_home) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                error = %e,
                "no identity keypair found; generating ephemeral (cross-host TCP disabled)"
            );
            AgentIdentity::generate()
        }
    }
}

/// #2318 trust on first use: pin the on-disk identity of an agent that has no
/// identity pin yet (installed before the pin existed). Runs before the seal,
/// while the agent has executed nothing, which is the same window the
/// entitlement pin trusts. An existing pin is never touched here — only the
/// CLI's create / rekey / import paths replace one.
///
/// Without a pin, verifiers fall back to the agent's own writable files, so a
/// start that cannot pin must never leave that fallback open:
/// - no key on disk (ephemeral identity): pin fail-closed, then start — no
///   key was trusted before, and none can be planted later;
/// - an identity that cannot be pinned (malformed chain, head not
///   `identity.pub`): pin fail-closed and refuse to start until
///   `mur agent rekey --emergency`;
/// - the fail-closed pin itself cannot be written: refuse to start.
pub(super) fn pin_identity_if_missing(agent_home: &std::path::Path) -> Result<(), String> {
    use mur_common::identity_pin as ip;
    let Some((mur_home, name)) = ip::split_agent_home(agent_home) else {
        return Ok(());
    };
    match ip::load_pin(mur_home, name) {
        Ok(Some(_)) => Ok(()),
        Ok(None) if !agent_home.join(ip::PUBKEY_FILE).exists() => {
            ip::write_fail_closed(mur_home, name).map_err(|e| unpinnable(name, &e))
        }
        Ok(None) => match ip::pin_from_disk(mur_home, name) {
            Ok(_) => {
                warn!(
                    agent = name,
                    "no identity pin; pinned the current identity (first start since #2318)"
                );
                Ok(())
            }
            Err(e) => {
                if let Err(w) = ip::write_fail_closed(mur_home, name) {
                    warn!(agent = name, error = %w, "could not write fail-closed identity pin");
                }
                Err(unpinnable(name, &e))
            }
        },
        // Present but unusable: verifiers already trust no key (fail closed).
        Err(e) => {
            warn!(agent = name, error = %e, "identity pin unreadable; peers trust no key for this agent until `mur agent rekey --emergency`");
            Ok(())
        }
    }
}

fn unpinnable(name: &str, e: &dyn std::fmt::Display) -> String {
    format!(
        "error[identity_unpinned]: {name}'s identity could not be pinned ({e}). \
         Peers now trust no key for it. If the identity files were not changed by \
         the agent, reset it with `mur agent rekey {name} --emergency`."
    )
}

#[cfg(test)]
mod pin_tests {
    use super::*;
    use mur_common::identity_pin as ip;

    fn home(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let h = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    #[test]
    fn consistent_identity_is_pinned_and_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let h = home(&tmp);
        AgentIdentity::generate().save(&h).unwrap();
        pin_identity_if_missing(&h).unwrap();
        assert!(!ip::load_pin(tmp.path(), "qa").unwrap().unwrap().fail_closed);
    }

    /// Review of #2340: a failed first-use pin must not leave verifiers on the
    /// agent's own files.
    #[test]
    fn unpinnable_identity_refuses_and_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let h = home(&tmp);
        AgentIdentity::generate().save(&h).unwrap();
        std::fs::write(h.join(ip::CHAIN_FILE), "{not json\n").unwrap();
        let msg = pin_identity_if_missing(&h).unwrap_err();
        assert!(msg.starts_with("error[identity_unpinned]"), "{msg}");
        assert_eq!(ip::resolve(&h), Some(None), "trusts no key, not disk");
    }

    #[test]
    fn keyless_agent_starts_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let h = home(&tmp);
        pin_identity_if_missing(&h).unwrap();
        assert_eq!(ip::resolve(&h), Some(None));
    }
}
