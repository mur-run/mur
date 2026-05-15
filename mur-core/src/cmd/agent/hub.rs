//! `mur agent migrate-to-hub` — idempotent v0→v1 agent YAML migration.
//!
//! For each agent in ~/.mur/agents/:
//!   1. Writes profile.yaml.bak (skips if already present and identical).
//!   2. Deserialises profile.yaml; serde #[default] fills any missing
//!      `appearance` fields (style_preset, behavior_preset, render_status).
//!   3. Re-serialises and atomically overwrites profile.yaml.
//!   4. On macOS: removes the legacy mur-agent-gui LaunchAgent plist at
//!      ~/Library/LaunchAgents/run.mur.agent.<name>.plist, if present, and
//!      calls `launchctl unload` so the .app stops relaunching at login.
//!
//! The command is safe to run multiple times. It never touches agents that
//! already have a complete appearance block.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use mur_common::AgentProfile;

use super::resolve_mur_home;

pub fn cmd_migrate_to_hub() -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");

    if !agents_dir.exists() {
        println!(
            "No agents directory found at {}; nothing to migrate.",
            agents_dir.display()
        );
        return Ok(());
    }

    let mut migrated = 0usize;
    let mut skipped = 0usize;

    let entries =
        fs::read_dir(&agents_dir).with_context(|| format!("read {}", agents_dir.display()))?;

    for entry in entries.flatten() {
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }
        let Some(name) = agent_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
        else {
            continue;
        };

        let profile_path = agent_dir.join("profile.yaml");
        if !profile_path.exists() {
            tracing::debug!(%name, "skipping — no profile.yaml");
            skipped += 1;
            continue;
        }

        match migrate_agent(&name, &profile_path) {
            Ok(changed) => {
                if changed {
                    println!("  migrated: {name}");
                    migrated += 1;
                } else {
                    println!("  ok (no change): {name}");
                    skipped += 1;
                }
            }
            Err(e) => {
                eprintln!("  error migrating {name}: {e:#}");
            }
        }

        remove_legacy_autostart(&name);
    }

    println!("\nDone — {migrated} migrated, {skipped} already up to date.");
    Ok(())
}

/// Returns `true` if the profile was rewritten (had missing appearance fields).
fn migrate_agent(name: &str, profile_path: &PathBuf) -> Result<bool> {
    let raw = fs::read_to_string(profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;

    // Write .bak once (idempotent — skip if bak already exists).
    let bak_path = profile_path.with_extension("yaml.bak");
    if !bak_path.exists() {
        fs::write(&bak_path, raw.as_bytes())
            .with_context(|| format!("write {}", bak_path.display()))?;
    }

    // Deserialise; serde #[default] fills appearance if the key is absent.
    let profile: AgentProfile =
        serde_yaml_ng::from_str(&raw).with_context(|| format!("parse profile.yaml for {name}"))?;

    // Re-serialise and compare — only write if something changed.
    let new_yaml = serde_yaml_ng::to_string(&profile).context("serialize profile")?;

    if new_yaml.trim() == raw.trim() {
        return Ok(false);
    }

    // Atomic overwrite.
    let tmp = profile_path.with_extension("yaml.tmp");
    fs::write(&tmp, new_yaml.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, profile_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), profile_path.display()))?;

    Ok(true)
}

/// Remove the mur-agent-gui LaunchAgent plist for `name` if it exists.
/// No-op on non-macOS.
fn remove_legacy_autostart(name: &str) {
    #[cfg(target_os = "macos")]
    {
        let Some(home) = dirs::home_dir() else { return };
        // tauri_plugin_autostart + MacosLauncher::LaunchAgent uses the app
        // identifier as the plist label: run.mur.agent.<name>.plist
        let plist_path = home
            .join("Library/LaunchAgents")
            .join(format!("run.mur.agent.{name}.plist"));

        if !plist_path.exists() {
            return;
        }

        // Unload (best-effort — launchctl may not be in PATH in sandboxed env).
        let label = format!("run.mur.agent.{name}");
        let _ = std::process::Command::new("launchctl")
            .args(["unload", plist_path.to_str().unwrap_or("")])
            .output();

        match fs::remove_file(&plist_path) {
            Ok(()) => tracing::info!(%name, label, "removed legacy LaunchAgent plist"),
            Err(e) => tracing::warn!(%name, error = %e, "could not remove LaunchAgent plist"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name; // nothing to remove on other platforms for now
    }
}
