//! C4 / C6 — `mur agent schedule add/list/remove/next` and `idle-{add,list,remove}`.
//!
//! Reads and writes `profile.lifecycle.schedule` / `profile.lifecycle.idle_triggers`
//! using the same `load_profile_for_edit` + `save_profile` helpers as `agent_webhook.rs`.

use anyhow::{Context, Result, bail};
use mur_common::agent::{IdleTrigger, ScheduleEntry};

use super::agent::{load_profile_for_edit, save_profile};

/// Append a new schedule entry to the agent's profile.
pub fn cmd_schedule_add(
    name: &str,
    cron: &str,
    message: &str,
    sends_to: Option<String>,
) -> Result<()> {
    validate_cron(cron)?;
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.lifecycle.schedule.push(ScheduleEntry {
        cron: cron.to_string(),
        message: message.to_string(),
        sends_to,
    });
    let idx = profile.lifecycle.schedule.len() - 1;
    save_profile(&path, &mut profile)?;
    println!("added schedule entry [{idx}]: {cron:?}  →  {message:?}");
    Ok(())
}

/// Where an agent leaves schedules it wants but cannot create.
fn proposal_dir(name: &str) -> std::path::PathBuf {
    crate::paths::mur_root(None)
        .join("agents")
        .join(name)
        .join(mur_common::agent::SCHEDULE_PROPOSAL_DIR)
}

fn read_proposals(name: &str) -> Vec<(String, mur_common::agent::ScheduleProposal)> {
    let Ok(entries) = std::fs::read_dir(proposal_dir(name)) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "yaml"))
        .filter_map(|e| {
            let id = e.path().file_stem()?.to_string_lossy().into_owned();
            let body = std::fs::read_to_string(e.path()).ok()?;
            Some((
                id,
                serde_yaml_ng::from_str::<mur_common::agent::ScheduleProposal>(&body).ok()?,
            ))
        })
        .collect();
    out.sort_by(|a, b| a.1.proposed_at.cmp(&b.1.proposed_at));
    out
}

/// List the schedules this agent has asked for and not been granted.
pub fn cmd_schedule_proposals(name: &str) -> Result<()> {
    let proposals = read_proposals(name);
    if proposals.is_empty() {
        println!("agent '{name}' has not asked for any schedules");
        return Ok(());
    }
    for (id, p) in &proposals {
        println!("{id}  {}  →  {}", p.cron, p.message);
        // The cron is not reviewable on its own — the question being asked is
        // whether it matches what was said, so what was said is printed.
        if let Some(asked) = &p.asked_for {
            println!("      asked for: {asked}");
        }
        match mur_agent_runtime::scheduler::next_n_fires(&p.cron, 1) {
            Ok(f) if !f.is_empty() => println!("      would first fire: {}", f[0]),
            _ => println!("      would first fire: never — this cron matches no time"),
        }
    }
    println!("\naccept one with: mur agent schedule accept {name} <id>");
    Ok(())
}

/// Grant a proposed schedule: the same write `add` performs, then the proposal
/// is gone.
///
/// Deliberately the same call rather than a second writer — a second path into
/// `lifecycle.schedule` is a second place for its validation to drift out of.
pub fn cmd_schedule_accept(name: &str, id: &str) -> Result<()> {
    let (_, p) = read_proposals(name)
        .into_iter()
        .find(|(pid, _)| pid == id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no proposal {id:?} for agent '{name}' — see `mur agent schedule proposals {name}`"
            )
        })?;
    cmd_schedule_add(name, &p.cron, &p.message, None)?;
    std::fs::remove_file(proposal_dir(name).join(format!("{id}.yaml")))
        .with_context(|| format!("remove proposal {id}"))?;
    println!("granted. Restart the agent for it to take effect: mur agent restart {name}");
    Ok(())
}

/// Refuse a proposed schedule. The agent is not told; it asked, and the answer
/// is that it does not happen.
pub fn cmd_schedule_decline(name: &str, id: &str) -> Result<()> {
    let path = proposal_dir(name).join(format!("{id}.yaml"));
    if !path.is_file() {
        anyhow::bail!("no proposal {id:?} for agent '{name}'");
    }
    std::fs::remove_file(&path).with_context(|| format!("remove proposal {id}"))?;
    println!("declined {id}");
    Ok(())
}

/// Print all schedule entries for the named agent.
pub fn cmd_schedule_list(name: &str) -> Result<()> {
    let entries = read_schedule(name)?;
    if entries.is_empty() {
        println!("no schedule entries for agent '{name}'");
        return Ok(());
    }
    println!("{:<4} {:<20} {:<30} SENDS_TO", "IDX", "CRON", "MESSAGE");
    for (i, e) in entries.iter().enumerate() {
        println!(
            "{:<4} {:<20} {:<30} {}",
            i,
            e.cron,
            e.message,
            e.sends_to.as_deref().unwrap_or("(self)")
        );
    }
    Ok(())
}

/// Remove the schedule entry at `index` (0-based).
pub fn cmd_schedule_remove(name: &str, index: usize) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let len = profile.lifecycle.schedule.len();
    if index >= len {
        bail!("index {index} out of range (agent '{name}' has {len} entries)");
    }
    let removed = profile.lifecycle.schedule.remove(index);
    save_profile(&path, &mut profile)?;
    println!("removed entry [{index}]: {:?}", removed.cron);
    Ok(())
}

/// Print the next `count` fire times for each schedule entry.
pub fn cmd_schedule_next(name: &str, count: usize) -> Result<()> {
    let entries = read_schedule(name)?;
    if entries.is_empty() {
        println!("no schedule entries for agent '{name}'");
        return Ok(());
    }
    for (i, e) in entries.iter().enumerate() {
        println!("[{i}] {}", e.cron);
        match mur_agent_runtime::scheduler::next_n_fires(&e.cron, count) {
            Ok(times) => {
                for t in times {
                    println!("  {}", t.format("%Y-%m-%d %H:%M:%S %Z"));
                }
            }
            Err(err) => println!("  (invalid expression: {err})"),
        }
    }
    Ok(())
}

/// Return the raw schedule entries from the agent's profile.
/// Public so integration tests can inspect state without going through stdout.
pub fn read_schedule(name: &str) -> Result<Vec<ScheduleEntry>> {
    let (_path, profile) = load_profile_for_edit(name)?;
    Ok(profile.lifecycle.schedule)
}

/// Validate a 5-field POSIX cron expression by attempting a dry parse.
fn validate_cron(expr: &str) -> Result<()> {
    mur_agent_runtime::scheduler::next_n_fires(expr, 1)
        .with_context(|| format!("invalid cron expression: {expr:?}"))?;
    Ok(())
}

// ── C6: idle triggers ─────────────────────────────────────────────────────────

/// Append a new idle trigger to the agent's profile.
pub fn cmd_idle_add(
    name: &str,
    after_secs: u64,
    message: &str,
    sends_to: Option<String>,
    cooldown_secs: u64,
    respect_quiet_hours: bool,
) -> Result<()> {
    if after_secs == 0 {
        bail!("after_secs must be > 0");
    }
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.lifecycle.idle_triggers.push(IdleTrigger {
        after_secs,
        message: message.to_string(),
        sends_to,
        cooldown_secs,
        respect_quiet_hours,
    });
    let idx = profile.lifecycle.idle_triggers.len() - 1;
    save_profile(&path, &mut profile)?;
    println!("added idle trigger [{idx}]: after_secs={after_secs} → {message:?}");
    Ok(())
}

/// Print all idle triggers for the named agent.
pub fn cmd_idle_list(name: &str) -> Result<()> {
    let entries = read_idle_triggers(name)?;
    if entries.is_empty() {
        println!("no idle triggers for agent '{name}'");
        return Ok(());
    }
    println!(
        "{:<4} {:<10} {:<10} {:<5} {:<30} SENDS_TO",
        "IDX", "AFTER", "COOLDOWN", "QH", "MESSAGE"
    );
    for (i, e) in entries.iter().enumerate() {
        println!(
            "{:<4} {:<10} {:<10} {:<5} {:<30} {}",
            i,
            e.after_secs,
            e.cooldown_secs,
            if e.respect_quiet_hours { "yes" } else { "no" },
            e.message,
            e.sends_to.as_deref().unwrap_or("(self)")
        );
    }
    Ok(())
}

/// Remove the idle trigger at `index` (0-based).
pub fn cmd_idle_remove(name: &str, index: usize) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let len = profile.lifecycle.idle_triggers.len();
    if index >= len {
        bail!("index {index} out of range (agent '{name}' has {len} idle triggers)");
    }
    let removed = profile.lifecycle.idle_triggers.remove(index);
    save_profile(&path, &mut profile)?;
    println!(
        "removed idle trigger [{index}]: after_secs={}",
        removed.after_secs
    );
    Ok(())
}

/// Return the raw idle-trigger entries.
/// Public so integration tests can inspect state without going through stdout.
pub fn read_idle_triggers(name: &str) -> Result<Vec<IdleTrigger>> {
    let (_path, profile) = load_profile_for_edit(name)?;
    Ok(profile.lifecycle.idle_triggers)
}

/// Register the `skill-propagate` idle trigger with idempotency (M7c).
pub fn cmd_propagate_init(name: &str, after_secs: u64, cooldown_secs: u64) -> Result<()> {
    let message = "propagate.run";
    let existing = read_idle_triggers(name)?;
    if existing.iter().any(|t| t.message == message) {
        println!("skill-propagate trigger already registered for {name}");
        return Ok(());
    }
    cmd_idle_add(name, after_secs, message, None, cooldown_secs, true)?;
    println!("registered skill-propagate idle trigger for {name}");
    Ok(())
}
