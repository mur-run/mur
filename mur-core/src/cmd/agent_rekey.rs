//! `mur agent rekey` — rotate an agent's Ed25519 identity keypair.
//!
//! See spec: docs/superpowers/specs/2026-04-24-murmur-agent-rekey-design.md

use anyhow::{Context, Result, anyhow, bail};
use mur_common::agent::AgentProfile;
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GRACE_DAYS_DEFAULT: i64 = 30;

#[derive(Debug, Clone, Copy)]
pub enum RekeyReason {
    Scheduled,
    SuspectCompromise,
    OwnerChange,
    Emergency,
}

impl RekeyReason {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "scheduled" => Ok(Self::Scheduled),
            "suspect-compromise" => Ok(Self::SuspectCompromise),
            "owner-change" => Ok(Self::OwnerChange),
            "emergency" => Ok(Self::Emergency),
            other => bail!(
                "unknown reason '{other}' (allowed: scheduled, suspect-compromise, owner-change, emergency)"
            ),
        }
    }
    fn into_attestation(self) -> RotationReason {
        match self {
            Self::Scheduled => RotationReason::Scheduled,
            Self::SuspectCompromise => RotationReason::SuspectCompromise,
            Self::OwnerChange => RotationReason::OwnerChange,
            Self::Emergency => RotationReason::Emergency,
        }
    }
}

pub fn cmd_rekey(name: &str, reason: &str, yes: bool, emergency: bool) -> Result<()> {
    if emergency {
        bail!("--emergency lands in M4 — not yet supported");
    }
    let parsed_reason = RekeyReason::parse(reason)?;
    if matches!(parsed_reason, RekeyReason::Emergency) {
        bail!("--reason emergency requires --emergency flag (M4)");
    }

    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);
    if !agent_dir.exists() {
        bail!("agent '{name}' not found at {}", agent_dir.display());
    }

    // Load current profile + identity
    let profile_path = agent_dir.join("profile.yaml");
    let profile_yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: AgentProfile =
        serde_yaml_ng::from_str(&profile_yaml).context("parse profile.yaml")?;
    let old_identity = AgentIdentity::load(&agent_dir)
        .with_context(|| format!("load identity from {}", agent_dir.display()))?;

    let old_pubkey = old_identity.pubkey_text();
    let old_key_version = profile.identity.key_version;
    let new_key_version = old_key_version + 1;

    // Interactive confirm
    if !yes {
        eprintln!("About to rotate identity for agent '{name}':");
        eprintln!("  current key_version: {old_key_version}");
        eprintln!("  current pubkey:      {old_pubkey}");
        eprintln!("  reason:              {reason}");
        eprintln!("  grace period:        {GRACE_DAYS_DEFAULT} days");
        eprintln!("  next key_version:    {new_key_version}");
        eprint!("\nProceed? [y/N] ");
        std::io::stderr().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
        if !buf.trim().eq_ignore_ascii_case("y") {
            eprintln!("aborted");
            return Ok(());
        }
    }

    // Generate new identity (in memory)
    let new_identity = AgentIdentity::generate();
    let new_pubkey = new_identity.pubkey_text();

    // Build + sign attestation
    let now = chrono::Utc::now();
    let now_rfc = now.to_rfc3339();
    let mut attestation = RotationAttestation::new(
        &profile.id,
        &old_pubkey,
        &new_pubkey,
        old_key_version,
        new_key_version,
        &now_rfc,
        parsed_reason.into_attestation(),
    );
    attestation.sign(old_identity.signing_key());

    // Atomic on-disk rotation
    rotate_files_atomic(&agent_dir, &new_identity, &attestation)?;

    // Update profile
    let grace_expires = now + chrono::Duration::days(GRACE_DAYS_DEFAULT);
    profile.identity.previous_pubkey = Some(old_pubkey.clone());
    profile.identity.previous_key_version = Some(old_key_version);
    profile.identity.pubkey = new_pubkey.clone();
    profile.identity.key_version = new_key_version;
    profile.identity.created_at_key = Some(now_rfc.clone());
    profile.identity.rotated_at = Some(now_rfc);
    profile.identity.grace_expires_at = Some(grace_expires.to_rfc3339());
    profile.updated_at = chrono::Utc::now().to_rfc3339();

    let new_yaml = serde_yaml_ng::to_string(&profile).context("serialize updated profile.yaml")?;
    write_atomic(&profile_path, new_yaml.as_bytes())?;

    // SIGTERM the running runtime if any
    if let Some(pid) = read_running_pid(&agent_dir) {
        send_sigterm(pid);
        // brief wait — supervisor symlink will restart
        std::thread::sleep(Duration::from_millis(500));
    }

    println!("rekey ok: {name} key_version {old_key_version} -> {new_key_version}");
    println!("  pubkey:              {new_pubkey}");
    println!("  previous (in grace): {old_pubkey}");
    println!("  grace expires:       {}", grace_expires.to_rfc3339());
    Ok(())
}

/// Atomic rotation: write everything new with `.new` suffix first, then
/// rename in a crash-safe order so a partial state is always recoverable.
fn rotate_files_atomic(
    agent_dir: &Path,
    new_identity: &AgentIdentity,
    attestation: &RotationAttestation,
) -> Result<()> {
    let key_path = agent_dir.join("identity.key");
    let pub_path = agent_dir.join("identity.pub");
    let key_prev = agent_dir.join("identity.key.prev");
    let pub_prev = agent_dir.join("identity.pub.prev");
    let att_path = agent_dir.join("identity.attestation.json");
    let att_new = agent_dir.join("identity.attestation.new.json");
    let rotations = agent_dir.join("rotations.jsonl");

    // 1. Write new keypair into a SCRATCH dir so the canonical paths stay
    //    untouched until rename time. We use a sibling subdir so renames
    //    cross no filesystem boundaries.
    let scratch = agent_dir.join(".rekey-scratch");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).ok();
    }
    new_identity
        .save(&scratch)
        .context("save new identity to scratch")?;

    // 2. Write attestation alongside as `.new` for crash safety.
    let att_json = serde_json::to_string(attestation).context("serialize attestation")?;
    write_atomic(&att_new, att_json.as_bytes())?;

    // 3. Move OLD key files to .prev (overwriting any stale .prev from a
    //    previous interrupted rotation).
    if key_prev.exists() {
        fs::remove_file(&key_prev).ok();
    }
    if pub_prev.exists() {
        fs::remove_file(&pub_prev).ok();
    }
    if key_path.exists() {
        fs::rename(&key_path, &key_prev)?;
    }
    if pub_path.exists() {
        fs::rename(&pub_path, &pub_prev)?;
    }

    // 4. Move new key files from scratch into canonical paths.
    fs::rename(scratch.join("identity.key"), &key_path)?;
    fs::rename(scratch.join("identity.pub"), &pub_path)?;
    fs::remove_dir_all(&scratch).ok();

    // 5. Promote .new attestation to canonical path.
    fs::rename(&att_new, &att_path)?;

    // 6. Append attestation to rotations.jsonl (append-only; no rename
    //    needed since we never rewrite the file).
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rotations)
        .with_context(|| format!("open {}", rotations.display()))?;
    writeln!(f, "{att_json}")?;

    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn resolve_mur_home() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_HOME") {
        return Ok(PathBuf::from(v));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home dir"))?
        .join(".mur"))
}

fn read_running_pid(agent_dir: &Path) -> Option<u32> {
    let lock = agent_dir.join("running.lock");
    let bytes = fs::read(&lock).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32)
}

#[cfg(unix)]
fn send_sigterm(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {
    // Windows: SIGTERM not available — runtime doesn't auto-restart on
    // Windows in P0a anyway. The user must restart manually.
}
