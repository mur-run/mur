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
    println!("removed idle trigger [{index}]: after_secs={}", removed.after_secs);
    Ok(())
}

/// Return the raw idle-trigger entries.
/// Public so integration tests can inspect state without going through stdout.
pub fn read_idle_triggers(name: &str) -> Result<Vec<IdleTrigger>> {
    let (_path, profile) = load_profile_for_edit(name)?;
    Ok(profile.lifecycle.idle_triggers)
}
