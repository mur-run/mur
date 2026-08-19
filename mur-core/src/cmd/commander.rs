//! `mur commander` — pin the commander key + (v1) issue signed directives into a
//! fleet channel, and fold them into governance state. Engine is the closed crate.

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::commander::{COMMANDER_DIRECTIVE_KEY, GovernanceState};
use mur_common::identity::AgentIdentity;

pub const COMMANDER_DIR: &str = "commander";
pub const COMMANDER_PUB: &str = "identity.pub";
pub const COMMANDER_PREV_PUB: &str = "identity.prev.pub";

fn decode_pub(multibase: &str) -> Option<[u8; 32]> {
    let (_, bytes) = multibase::decode(multibase.trim()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    // Reject off-curve keys: a pinned-but-invalid key would silently render
    // governance inert (no directive would ever verify against it).
    mur_common::identity::valid_ed25519_pubkey(&arr).then_some(arr)
}

/// Accepted commander pubkeys: current `identity.pub` + optional previous. Empty
/// vec ⇒ no commander configured (governance inert).
pub fn accepted_pubkeys(mur_home: &Path) -> Vec<[u8; 32]> {
    let dir = mur_home.join(COMMANDER_DIR);
    let mut out = Vec::new();
    for name in [COMMANDER_PUB, COMMANDER_PREV_PUB] {
        if let Ok(s) = std::fs::read_to_string(dir.join(name))
            && let Some(pk) = decode_pub(&s)
        {
            out.push(pk);
        }
    }
    out
}

/// Fold governance for `fleet` from its channel. Err on channel read failure
/// (callers fail-closed). No pinned key ⇒ inert default.
#[allow(dead_code)] // consumed by Task 5 (daemon fleet_tick) + Task 4 (loop) integration
pub fn governance_state(mur_home: &Path, fleet: &str) -> Result<GovernanceState> {
    let keys = accepted_pubkeys(mur_home);
    if keys.is_empty() {
        return Ok(GovernanceState::default());
    }
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let channel_id = format!("fleet-{fleet}");
    let events = svc
        .load_events(&channel_id)
        .with_context(|| format!("load channel {channel_id}"))?;
    Ok(mur_channel::governance::fold_governance(
        &events,
        &channel_id,
        fleet,
        &keys,
    ))
}

pub fn cmd_commander_pin(mur_home: &Path, pubkey_multibase: &str, force: bool) -> Result<()> {
    let dir = mur_home.join(COMMANDER_DIR);
    std::fs::create_dir_all(&dir)?;
    if decode_pub(pubkey_multibase).is_none() {
        bail!("not a valid multibase Ed25519 pubkey (expected 32 on-curve bytes)");
    }
    let path = dir.join(COMMANDER_PUB);
    if path.exists() {
        if !force {
            bail!(
                "a commander key is already pinned at {} — re-pin is a governance change; pass --force",
                path.display()
            );
        }
        // Rotation: preserve the outgoing key as identity.prev.pub (which
        // accepted_pubkeys already loads) so directives it already signed — e.g.
        // an in-force kill — keep verifying until the operator re-issues them.
        if let Ok(old) = std::fs::read_to_string(&path) {
            std::fs::write(dir.join(COMMANDER_PREV_PUB), old)?;
            eprintln!(
                "warning: commander key rotated; previous key preserved at {} and still accepted. \
                 Re-issue any in-force directives with the new key, then remove the previous key.",
                dir.join(COMMANDER_PREV_PUB).display()
            );
        }
    }
    std::fs::write(&path, format!("{}\n", pubkey_multibase.trim()))?;
    println!("Pinned commander key → {}", path.display());
    Ok(())
}

/// Build the human-readable status lines (factored out for testing). Shows the
/// short fingerprint as the primary identifier, full key indented for OOB compare.
fn status_lines(mur_home: &Path) -> Result<Vec<String>> {
    let keys = accepted_pubkeys(mur_home);
    if keys.is_empty() {
        return Ok(vec!["No commander key pinned (governance inert).".into()]);
    }
    let dir = mur_home.join(COMMANDER_DIR);
    let cur = std::fs::read_to_string(dir.join(COMMANDER_PUB))
        .with_context(|| format!("read {}", dir.join(COMMANDER_PUB).display()))?;
    let cur = cur.trim();
    let mut out = vec![
        format!(
            "Commander key pinned: {}",
            mur_common::fleet_bundle::signer_fingerprint(cur)
        ),
        format!("  key: {cur}"),
    ];
    if dir.join(COMMANDER_PREV_PUB).exists() {
        let prev = std::fs::read_to_string(dir.join(COMMANDER_PREV_PUB)).unwrap_or_default();
        out.push(format!(
            "  previous (also accepted): {}",
            mur_common::fleet_bundle::signer_fingerprint(prev.trim())
        ));
    }
    Ok(out)
}

pub fn cmd_commander_status(mur_home: &Path) -> Result<()> {
    for line in status_lines(mur_home)? {
        println!("{line}");
    }
    Ok(())
}

/// v1 delivery: sign a directive with the local commander identity and append it
/// to the fleet channel. `now_ms` is injected for deterministic tests.
pub fn cmd_commander_directive(
    mur_home: &Path,
    fleet: &str,
    kind: &str,
    budget_usd: Option<f64>,
    now_ms: u64,
) -> Result<()> {
    if !matches!(kind, "kill" | "resume" | "budget_ceiling") {
        bail!("kind must be kill | resume | budget_ceiling");
    }
    let id = AgentIdentity::load(&mur_home.join(COMMANDER_DIR))
        .context("load commander identity (~/.mur/commander/identity.key)")?;
    let nonce = uuid::Uuid::now_v7().to_string();
    let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
        "kind": kind, "fleet": fleet, "budget_usd": budget_usd,
        "nonce": nonce, "issued_at_ms": now_ms,
    }});
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let ev = svc.append_signed(
        &format!("fleet-{fleet}"),
        &id,
        0,
        mur_common::channel::ChannelActor::System,
        mur_common::channel::EventKind::Note,
        payload,
        Some(nonce.clone()),
    )?;
    println!(
        "Issued commander '{kind}' for fleet '{fleet}' (seq {}, nonce {nonce})",
        ev.seq
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;

    fn seed_commander(home: &std::path::Path) -> AgentIdentity {
        let dir = home.join("commander");
        std::fs::create_dir_all(&dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&dir).unwrap(); // writes identity.key + identity.pub
        id
    }

    #[test]
    fn pin_refuses_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let pk1 = mur_common::identity::AgentIdentity::generate().public_key_multibase();
        let pk2 = mur_common::identity::AgentIdentity::generate().public_key_multibase();
        cmd_commander_pin(home, &pk1, false).unwrap();
        assert!(cmd_commander_pin(home, &pk2, false).is_err()); // refuse overwrite
        cmd_commander_pin(home, &pk2, true).unwrap(); // --force overwrites
        let pinned = std::fs::read_to_string(home.join("commander").join(COMMANDER_PUB)).unwrap();
        assert_eq!(pinned.trim(), pk2);
        // and an invalid pubkey is rejected even with --force
        assert!(cmd_commander_pin(home, "not-a-key", true).is_err());
    }

    #[test]
    fn force_repin_preserves_previous_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let a = AgentIdentity::generate().public_key_multibase();
        let b = AgentIdentity::generate().public_key_multibase();
        cmd_commander_pin(home, &a, false).unwrap();
        cmd_commander_pin(home, &b, true).unwrap();
        let dir = home.join("commander");
        assert_eq!(
            std::fs::read_to_string(dir.join(COMMANDER_PUB))
                .unwrap()
                .trim(),
            b
        );
        // outgoing key preserved so its in-force directives keep verifying
        assert_eq!(
            std::fs::read_to_string(dir.join(COMMANDER_PREV_PUB))
                .unwrap()
                .trim(),
            a
        );
        assert_eq!(accepted_pubkeys(home).len(), 2); // current + previous both accepted
    }

    #[test]
    fn status_shows_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let pk = AgentIdentity::generate().public_key_multibase();
        cmd_commander_pin(home, &pk, false).unwrap();
        let lines = status_lines(home).unwrap();
        let fp = mur_common::fleet_bundle::signer_fingerprint(&pk);
        assert!(lines.iter().any(|l| l.contains(&fp)));
        // the full key is also present for out-of-band comparison
        assert!(lines.iter().any(|l| l.contains(&pk)));
    }

    #[test]
    fn directive_then_governance_state_reflects_kill() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let id = seed_commander(home); // commander identity (key+pub) present
        // a fleet + its channel must exist to append into
        let fleet = mur_common::fleet::Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let svc = mur_channel::ChannelService::open(home).unwrap();
        svc.create_for_fleet("dev", "mur", &["pm".into()]).unwrap();

        // no directive yet → not killed
        assert!(!governance_state(home, "dev").unwrap().killed);
        // issue a kill via the CLI path
        cmd_commander_directive(home, "dev", "kill", None, 1000).unwrap();
        assert!(governance_state(home, "dev").unwrap().killed);
        // the pinned pubkey accepts the commander's own key
        assert_eq!(accepted_pubkeys(home), vec![id.verifying_key_bytes()]);
    }
}
