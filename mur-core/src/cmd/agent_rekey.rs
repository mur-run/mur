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
    let parsed_reason = if emergency {
        RekeyReason::Emergency
    } else {
        let r = RekeyReason::parse(reason)?;
        if matches!(r, RekeyReason::Emergency) {
            bail!("--reason emergency requires --emergency flag");
        }
        r
    };

    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);
    if !agent_dir.exists() {
        bail!("agent '{name}' not found at {}", agent_dir.display());
    }

    // Load current profile
    let profile_path = agent_dir.join("profile.yaml");
    let profile_yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: AgentProfile =
        serde_yaml_ng::from_str(&profile_yaml).context("parse profile.yaml")?;

    // Load OLD identity for normal rekey; emergency may have lost it.
    let old_identity_opt = if emergency {
        AgentIdentity::load(&agent_dir).ok()
    } else {
        Some(
            AgentIdentity::load(&agent_dir)
                .with_context(|| format!("load identity from {}", agent_dir.display()))?,
        )
    };

    let old_pubkey = old_identity_opt
        .as_ref()
        .map(|id| id.pubkey_text())
        .unwrap_or_else(|| profile.identity.pubkey.clone());
    let old_key_version = profile.identity.key_version;
    let new_key_version = old_key_version + 1;

    // Interactive confirm
    if emergency && !yes {
        eprintln!();
        eprintln!("\x1b[1;31m=== EMERGENCY REKEY — read carefully ===\x1b[0m");
        eprintln!();
        eprintln!("This rotation will be UNSIGNED. Other commanders / peers will");
        eprintln!("REFUSE to trust the new key until an admin runs:");
        eprintln!("    \x1b[1mmurc agent approve-rekey {}\x1b[0m", profile.id);
        eprintln!("on the commander host.");
        eprintln!();
        eprintln!("Active Noise sessions to this agent will fail until that");
        eprintln!("approval lands.");
        eprintln!();
        eprintln!("  agent name:        {name}");
        eprintln!("  agent UUID:        {}", profile.id);
        eprintln!("  current pubkey:    {old_pubkey}");
        eprintln!("  key_version:       {old_key_version} -> {new_key_version}");
        eprintln!();
        eprint!("Type \x1b[1mI UNDERSTAND\x1b[0m to proceed: ");
        std::io::stderr().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
        if buf.trim() != "I UNDERSTAND" {
            eprintln!("aborted (confirmation phrase not matched)");
            return Ok(());
        }
    } else if !yes {
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

    // Build attestation. Normal rotations sign with the OLD key; emergency
    // rotations leave signature empty and are gated by commander-side
    // out-of-band approval.
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
    if !emergency {
        let old_id = old_identity_opt
            .as_ref()
            .expect("normal rekey requires old key");
        attestation.sign(old_id.signing_key());
    }

    // Atomic on-disk rotation
    rotate_files_atomic(&agent_dir, &new_identity, &attestation)?;

    // Update profile
    let grace_expires = now + chrono::Duration::days(GRACE_DAYS_DEFAULT);
    profile.identity.previous_pubkey = Some(old_pubkey.clone());
    profile.identity.previous_key_version = Some(old_key_version);
    profile.identity.pubkey = new_pubkey.clone();
    profile.identity.key_version = new_key_version;
    profile.identity.created_at_key = Some(now_rfc.clone());
    profile.identity.rotated_at = Some(now_rfc.clone());
    profile.identity.grace_expires_at = Some(grace_expires.to_rfc3339());
    if emergency {
        profile.identity.emergency_rekey_at = Some(now_rfc);
    }
    profile.updated_at = chrono::Utc::now().to_rfc3339();

    let new_yaml = serde_yaml_ng::to_string(&profile).context("serialize updated profile.yaml")?;
    write_atomic(&profile_path, new_yaml.as_bytes())?;

    // SIGTERM the running runtime if any
    if let Some(pid) = read_running_pid(&agent_dir) {
        send_sigterm(pid);
        // brief wait — supervisor symlink will restart
        std::thread::sleep(Duration::from_millis(500));
    }

    if emergency {
        println!(
            "emergency rekey written: {name} key_version {old_key_version} -> {new_key_version}"
        );
        println!("  pubkey:              {new_pubkey}");
        println!("  previous (in grace): {old_pubkey}");
        println!();
        println!(
            "\x1b[1;33mPENDING APPROVAL\x1b[0m — peers will refuse trust until commander admin runs:"
        );
        println!("  murc agent approve-rekey {}", profile.id);
    } else {
        println!("rekey ok: {name} key_version {old_key_version} -> {new_key_version}");
        println!("  pubkey:              {new_pubkey}");
        println!("  previous (in grace): {old_pubkey}");
        println!("  grace expires:       {}", grace_expires.to_rfc3339());
    }
    Ok(())
}

/// Atomic rotation: write everything new with `.new` suffix first, then
/// rename in a crash-safe order so a partial state is always recoverable.
fn rotate_files_atomic(
    agent_dir: &Path,
    new_identity: &AgentIdentity,
    attestation: &RotationAttestation,
) -> Result<()> {
    // Private halves follow the key dir (#850 option (c)); public halves and
    // the attestation stay in the agent home, which is what peers read.
    // Writing these to `agent_dir` would silently un-migrate the agent on
    // every rekey.
    let key_dir = mur_common::identity::private_key_dir(agent_dir);
    std::fs::create_dir_all(&key_dir)
        .with_context(|| format!("create key dir {}", key_dir.display()))?;
    let key_path = key_dir.join("identity.key");
    let pub_path = agent_dir.join("identity.pub");
    let key_prev = key_dir.join("identity.key.prev");
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

/// `mur agent rekey-status <name>` — show current/previous keys, grace
/// remaining, and the chain history length.
pub fn cmd_rekey_status(name: &str, json_out: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);
    if !agent_dir.exists() {
        bail!("agent '{name}' not found at {}", agent_dir.display());
    }

    let profile_path = agent_dir.join("profile.yaml");
    let profile_yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: AgentProfile =
        serde_yaml_ng::from_str(&profile_yaml).context("parse profile.yaml")?;

    let rotations_path = agent_dir.join("rotations.jsonl");
    let rotation_count = if rotations_path.exists() {
        fs::read_to_string(&rotations_path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    } else {
        0
    };

    let now = chrono::Utc::now();
    let grace_remaining_days = profile
        .identity
        .grace_expires_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|exp| (exp.with_timezone(&chrono::Utc) - now).num_days());

    if json_out {
        let out = serde_json::json!({
            "name": name,
            "uuid": profile.id,
            "algorithm": profile.identity.algorithm,
            "key_version": profile.identity.key_version,
            "pubkey": profile.identity.pubkey,
            "previous_pubkey": profile.identity.previous_pubkey,
            "previous_key_version": profile.identity.previous_key_version,
            "grace_expires_at": profile.identity.grace_expires_at,
            "grace_remaining_days": grace_remaining_days,
            "created_at_key": profile.identity.created_at_key,
            "rotated_at": profile.identity.rotated_at,
            "emergency_rekey_at": profile.identity.emergency_rekey_at,
            "rotation_log_lines": rotation_count,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("agent: {name}");
        println!("  uuid:               {}", profile.id);
        println!("  algorithm:          {}", profile.identity.algorithm);
        println!("  key_version:        {}", profile.identity.key_version);
        println!("  pubkey:             {}", profile.identity.pubkey);
        if let Some(t) = &profile.identity.created_at_key {
            println!("  created_at_key:     {t}");
        }
        if let Some(prev) = &profile.identity.previous_pubkey {
            println!("  previous_pubkey:    {prev}");
            if let Some(v) = profile.identity.previous_key_version {
                println!("  previous_key_ver:   {v}");
            }
        } else {
            println!("  previous_pubkey:    <none in grace>");
        }
        if let Some(g) = &profile.identity.grace_expires_at {
            print!("  grace_expires_at:   {g}");
            if let Some(d) = grace_remaining_days {
                if d >= 0 {
                    println!(" ({d} days remaining)");
                } else {
                    println!(" (expired {} days ago)", -d);
                }
            } else {
                println!();
            }
        }
        if let Some(t) = &profile.identity.rotated_at {
            println!("  last_rotation_at:   {t}");
        }
        if let Some(t) = &profile.identity.emergency_rekey_at {
            println!("  EMERGENCY rekey at: {t}");
            println!("  → peers will refuse trust until commander admin runs:");
            println!("      murc agent approve-rekey {}", profile.id);
        }
        println!("  rotation log lines: {rotation_count}");
    }
    Ok(())
}
