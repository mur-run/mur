//! A1 — `mur agent hooks show [--json]`

use anyhow::{Context, Result};
use serde::Serialize;

use mur_common::agent::AgentProfile;

use super::agent::resolve_mur_home;

#[derive(Serialize)]
pub struct HookEntry {
    pub name: &'static str,
    pub tier: &'static str,
    pub enabled: bool,
    pub source: String,
}

fn chain_entries(profile: &AgentProfile) -> Vec<HookEntry> {
    let cfg = &profile.hooks;
    let mut out = vec![
        HookEntry {
            name: "TelemetryHook",
            tier: "mandatory",
            enabled: true,
            source: "hardcoded".into(),
        },
        HookEntry {
            name: "B0SafetyHook",
            tier: "mandatory",
            enabled: true,
            source: "hardcoded".into(),
        },
        HookEntry {
            name: "LedgerHook",
            tier: "optional",
            enabled: cfg.ledger,
            source: "hooks.ledger".into(),
        },
    ];

    let companion_voice_on = cfg.companion_voice.unwrap_or(profile.companion.enabled);
    out.push(HookEntry {
        name: "CompanionVoiceHook",
        tier: "optional",
        enabled: companion_voice_on,
        source: match cfg.companion_voice {
            Some(v) => format!("hooks.companion_voice = {v}"),
            None => format!("auto ← companion.enabled = {}", profile.companion.enabled),
        },
    });

    let voice_input_on = cfg.voice_input.unwrap_or(profile.voice.enabled);
    out.push(HookEntry {
        name: "VoiceInputHook",
        tier: "optional",
        enabled: voice_input_on,
        source: match cfg.voice_input {
            Some(v) => format!("hooks.voice_input = {v}"),
            None => format!("auto ← voice.enabled = {}", profile.voice.enabled),
        },
    });

    out
}

pub fn cmd_hooks_show(name: &str, json: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let profile_path = mur_home.join("agents").join(name).join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: AgentProfile =
        serde_yaml::from_str(&yaml).with_context(|| format!("parse {}", profile_path.display()))?;

    let entries = chain_entries(&profile);

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("── Hook chain for agent \"{name}\" ────────────────────────────────");
    for e in &entries {
        let state = if e.enabled { "on " } else { "off" };
        println!("  [{:9}]  {:20}  {}  ({})", e.tier, e.name, state, e.source);
    }
    println!("──────────────────────────────────────────────────────────────────");
    Ok(())
}
