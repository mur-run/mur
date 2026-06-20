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
    bytes.try_into().ok()
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
#[allow(dead_code)]
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
    let path = dir.join(COMMANDER_PUB);
    if path.exists() && !force {
        bail!(
            "a commander key is already pinned at {} — re-pin is a governance change; pass --force",
            path.display()
        );
    }
    std::fs::write(&path, format!("{}\n", pubkey_multibase.trim()))?;
    println!("Pinned commander key → {}", path.display());
    Ok(())
}

pub fn cmd_commander_status(mur_home: &Path) -> Result<()> {
    let keys = accepted_pubkeys(mur_home);
    if keys.is_empty() {
        println!("No commander key pinned (governance inert).");
        return Ok(());
    }
    let dir = mur_home.join(COMMANDER_DIR);
    let cur = std::fs::read_to_string(dir.join(COMMANDER_PUB)).unwrap_or_default();
    println!("Commander key pinned: {}", cur.trim());
    if dir.join(COMMANDER_PREV_PUB).exists() {
        println!("  (previous key also accepted for rotation)");
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
        cmd_commander_pin(home, "z11111", false).unwrap();
        assert!(cmd_commander_pin(home, "z22222", false).is_err());
        cmd_commander_pin(home, "z22222", true).unwrap(); // force overwrites
        let pinned = std::fs::read_to_string(home.join("commander").join(COMMANDER_PUB)).unwrap();
        assert_eq!(pinned.trim(), "z22222");
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
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
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
