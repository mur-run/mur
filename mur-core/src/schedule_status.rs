//! Unified schedule-status aggregator for the murmur Panel P2 companion window.
//!
//! Merges three independent schedule sources into one flat list:
//! - agent-level cron / idle triggers (`~/.mur/agents/<name>/profile.yaml`)
//! - OS-level workflow schedules (launchd / crontab, via `cmd::system_schedule`)
//! - fleet loop triggers (`~/.mur/fleets/<name>/fleet.yaml`)
//!
//! Fail-soft: a single unreadable profile/fleet file is recorded as a warning
//! and skipped rather than aborting the whole aggregation.
//!
//! See `docs/superpowers/specs/2026-07-06-murmur-panel-p2-data-tabs-design.md`.

use std::path::Path;

use chrono::{DateTime, Local, TimeZone};
use mur_agent_runtime::scheduler::{next_n_fires, parse_bound};

use crate::cmd::fleet::loop_run::parse_duration;

use crate::cmd::system_schedule::list_system_schedules_detailed;

/// How many upcoming fire times to compute for cron-style previews.
const NEXT_FIRE_COUNT: usize = 3;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ScheduleItem {
    AgentCron {
        owner: String,
        expr: String,
        message: String,
        next_fires: Vec<String>,
        status: String,
        description: String,
        next_note: Option<String>,
    },
    AgentIdle {
        owner: String,
        after_secs: u64,
        cooldown_secs: u64,
        message: String,
        status: String,
        description: String,
        next_note: Option<String>,
    },
    Workflow {
        owner: String,
        expr: Option<String>,
        next_fires: Vec<String>,
        status: String,
        description: String,
        next_note: Option<String>,
    },
    Fleet {
        owner: String,
        trigger: String,
        next_fires: Vec<String>,
        status: String,
        budget_usd: f64,
        autorun_env: bool,
        description: String,
        next_note: Option<String>,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleStatus {
    pub schedules: Vec<ScheduleItem>,
    pub warnings: Vec<String>,
}

/// Aggregate agent / workflow / fleet schedules under `mur_home` into one
/// unified view. `agent_filter`, when set, restricts agent-owned schedule
/// items (cron + idle) to the named agent (case-insensitive); workflow and
/// fleet items are always included ("globals").
pub fn schedule_status(mur_home: &Path, agent_filter: Option<&str>) -> ScheduleStatus {
    let mut schedules = Vec::new();
    let mut warnings = Vec::new();

    collect_agents(mur_home, agent_filter, &mut schedules, &mut warnings);
    collect_workflows(&mut schedules);
    collect_fleets(mur_home, &mut schedules, &mut warnings);

    ScheduleStatus {
        schedules,
        warnings,
    }
}

/// Human phrasing for a cron expression.
///
/// Computed here, not in each client: cron → English is a real derivation, and
/// a second implementation of it eventually disagrees with this one. The
/// Dashboard already carries such a second implementation
/// (`mur-web/src/lib/schedule-parser.ts`), which is what this is working
/// toward retiring.
///
/// Only the shapes that actually occur are phrased; anything else falls back to
/// the raw expression. A wrong sentence is worse than a cron string a reader can
/// look up, so the fallback is deliberate rather than a gap to be filled in
/// later with guesses.
pub fn describe_cron(expr: &str) -> String {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return expr.to_string();
    }
    let (min, hour, dom, mon, dow) = (f[0], f[1], f[2], f[3], f[4]);
    let every_day = dom == "*" && mon == "*";
    let at = |h: &str, m: &str| -> Option<String> {
        let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
        (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
    };
    match (min, hour, dow) {
        (m, "*", "*") if every_day => {
            if let Some(n) = m.strip_prefix("*/").and_then(|n| n.parse::<u32>().ok()) {
                return format!("every {n} minutes");
            }
            if m == "*" {
                return "every minute".into();
            }
            if m.parse::<u32>().is_ok() {
                return "hourly".into();
            }
            expr.to_string()
        }
        (m, h, "*") if every_day => match at(h, m) {
            Some(t) => format!("daily at {t}"),
            None => expr.to_string(),
        },
        (m, h, "1-5") if every_day => match at(h, m) {
            Some(t) => format!("weekdays at {t}"),
            None => expr.to_string(),
        },
        _ => expr.to_string(),
    }
}

/// Human phrasing for a fleet loop trigger (`cron:…`, `interval:…`, `manual`).
fn describe_trigger(trigger: &str) -> String {
    if let Some(expr) = trigger.strip_prefix("cron:") {
        return describe_cron(expr);
    }
    if let Some(every) = trigger.strip_prefix("interval:") {
        return format!("every {every}");
    }
    if trigger == "manual" {
        return "manual — runs only when started".into();
    }
    trigger.to_string()
}

/// When a fleet next fires, or why that cannot be said. Exactly one of the two
/// is always present — a blank "Next" reads as "will not run again", and one of
/// the things it used to hide was a fleet running every thirty minutes.
fn fleet_next(dir: &Path, trigger: &str) -> (Vec<String>, Option<String>) {
    if let Some(expr) = trigger.strip_prefix("cron:") {
        let f = fires(expr);
        if f.is_empty() {
            return (f, Some(format!("could not read a schedule from `{expr}`")));
        }
        return (f, None);
    }
    if let Some(every) = trigger.strip_prefix("interval:") {
        return interval_next(dir, every);
    }
    if trigger == "manual" {
        return (
            Vec::new(),
            Some("no timetable — this runs only when something starts it".into()),
        );
    }
    (
        Vec::new(),
        Some(format!("could not read a schedule from `{trigger}`")),
    )
}

/// An interval is measured from the previous run, which `mur-daemon`'s fleet
/// tick stamps into `<fleet>/.last_run` — the same file the Hub already reads
/// to show "Last run". An earlier version of this module recorded that nothing
/// wrote that stamp and treated the gap as permanent; the daemon does write it,
/// so the fire times are computable and are computed here.
///
/// Two things it still will not claim: a fleet that has never run has nothing
/// to measure from, and a boundary that has already passed without the fleet
/// running means the daemon is not picking it up — naming the boundary *after*
/// that one would assert a wait the fleet is not doing.
fn interval_next(dir: &Path, every: &str) -> (Vec<String>, Option<String>) {
    let Some(step) = parse_duration(every)
        .map(|d| d.as_secs())
        .filter(|s| *s > 0)
    else {
        return (
            Vec::new(),
            Some(format!("could not read a duration from `{every}`")),
        );
    };
    let Some(last) = read_last_run(dir) else {
        return (
            Vec::new(),
            Some("not yet run — an interval is measured from the last run".into()),
        );
    };
    let now = Local::now().timestamp().max(0) as u64;
    let due = last + step;
    if due <= now {
        let when = local_fmt(due, "%Y-%m-%d %H:%M");
        return (
            Vec::new(),
            Some(match when {
                Some(t) => format!("due since {t} — waiting for the fleet daemon"),
                None => "overdue — waiting for the fleet daemon".into(),
            }),
        );
    }
    let out: Vec<String> = (0..NEXT_FIRE_COUNT as u64)
        .filter_map(|k| local_fmt(due + k * step, "%+"))
        .collect();
    if out.is_empty() {
        return (out, Some("could not represent the next fire time".into()));
    }
    (out, None)
}

fn read_last_run(dir: &Path) -> Option<u64> {
    std::fs::read_to_string(dir.join(".last_run"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn local_fmt(unix: u64, fmt: &str) -> Option<String> {
    Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map(|t| t.format(fmt).to_string())
}

fn fires(expr: &str) -> Vec<String> {
    bounded_fires(expr, None)
}

/// Firings that will actually occur — anything past the entry's bound dropped.
///
/// A bare cron reading of a one-shot lists next year and the year after, times
/// the scheduler retires the entry rather than reach (#1119). Showing them is
/// the same failure the bound exists to fix, relocated into the UI.
fn bounded_fires(expr: &str, not_after: Option<&str>) -> Vec<String> {
    let bound = parse_bound(not_after);
    next_n_fires(expr, NEXT_FIRE_COUNT)
        .map(|v| {
            v.iter()
                .filter(|t| bound.is_none_or(|b| **t <= b))
                .map(|t| t.to_rfc3339())
                .collect()
        })
        .unwrap_or_default()
}

/// Phrase what an entry will actually do, bound included.
///
/// One describer for every surface — `schedule list`, `schedule proposals`, the
/// Panel and the Hub — so none of them can claim an entry repeats when it does
/// not. Same discipline as #1095, applied to #1119's bound.
pub fn describe_bounded(cron: &str, not_after: Option<&str>) -> String {
    describe_bounded_at(cron, not_after, Local::now())
}

/// [`describe_bounded`] against an explicit clock, so the expired branch is
/// testable without waiting for a date to pass.
fn describe_bounded_at(cron: &str, not_after: Option<&str>, now: DateTime<Local>) -> String {
    match parse_bound(not_after) {
        // A spent one-off stays in the profile (the sandboxed scheduler cannot
        // edit it), so without the marker it reads exactly like a live one (#2437).
        Some(b) if b < now => format!("once, on {} (expired)", b.format("%Y-%m-%d %H:%M")),
        Some(b) => format!("once, on {}", b.format("%Y-%m-%d %H:%M")),
        None => describe_cron(cron),
    }
}

/// Whether a one-off's bound has passed. An absent or unreadable bound is no
/// bound — the same reading the scheduler uses — so it is never expired.
pub fn is_expired(not_after: Option<&str>, now: DateTime<Local>) -> bool {
    parse_bound(not_after).is_some_and(|b| b < now)
}

fn collect_agents(
    home: &Path,
    filter: Option<&str>,
    out: &mut Vec<ScheduleItem>,
    warnings: &mut Vec<String>,
) {
    let dir = home.join("agents");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(f) = filter
            && !f.eq_ignore_ascii_case(&name)
        {
            continue;
        }
        let path = entry.path().join("profile.yaml");
        if !path.exists() {
            continue;
        }
        let profile: mur_common::agent::AgentProfile = match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_yaml_ng::from_str(&s).map_err(anyhow::Error::from))
        {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("agent {name}: {e}"));
                continue;
            }
        };

        for s in &profile.lifecycle.schedule {
            let next_fires = bounded_fires(&s.cron, s.not_after.as_deref());
            out.push(ScheduleItem::AgentCron {
                owner: name.clone(),
                expr: s.cron.clone(),
                message: s.message.clone(),
                description: describe_bounded(&s.cron, s.not_after.as_deref()),
                // Two very different reasons produce no fire times, and a single
                // note for both is exactly the lie this batch keeps finding: a
                // spent one-off did its job, an unreadable cron never will.
                next_note: next_fires.is_empty().then(|| {
                    if parse_bound(s.not_after.as_deref()).is_some() {
                        "past its date — this was a one-off and does not repeat".to_string()
                    } else {
                        format!("could not read a schedule from `{}`", s.cron)
                    }
                }),
                next_fires,
                status: "enabled".into(),
            });
        }
        for t in &profile.lifecycle.idle_triggers {
            out.push(ScheduleItem::AgentIdle {
                owner: name.clone(),
                after_secs: t.after_secs,
                cooldown_secs: t.cooldown_secs,
                message: t.message.clone(),
                description: format!("after {}s idle", t.after_secs),
                // Not a failure to compute: an idle trigger has no clock to
                // read. Saying so beats a blank that reads as "never".
                next_note: Some("no fixed time — fires once the agent has been idle".into()),
                status: "enabled".into(),
            });
        }
    }
}

fn collect_workflows(out: &mut Vec<ScheduleItem>) {
    for s in list_system_schedules_detailed() {
        let next_fires = s.cron.as_deref().map(fires).unwrap_or_default();
        let description = s
            .cron
            .as_deref()
            .map(describe_cron)
            .unwrap_or_else(|| "manual — runs only when started".into());
        let next_note = next_fires.is_empty().then(|| match s.cron.as_deref() {
            Some(expr) => format!("could not read a schedule from `{expr}`"),
            None => "no timetable — this runs only when something starts it".into(),
        });
        out.push(ScheduleItem::Workflow {
            owner: s.workflow,
            expr: s.cron,
            next_fires,
            status: "enabled".into(),
            description,
            next_note,
        });
    }
}

fn collect_fleets(home: &Path, out: &mut Vec<ScheduleItem>, warnings: &mut Vec<String>) {
    let dir = home.join("fleets");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("fleet.yaml");
        if !path.exists() {
            continue;
        }
        let fleet: mur_common::fleet::Fleet = match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_yaml_ng::from_str(&s).map_err(anyhow::Error::from))
        {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!(
                    "fleet {}: {e}",
                    entry.file_name().to_string_lossy()
                ));
                continue;
            }
        };
        let Some(loop_cfg) = fleet.loop_cfg else {
            continue;
        };
        let stopped = entry.path().join(".stopped").exists();
        let (next_fires, next_note) = fleet_next(&entry.path(), &loop_cfg.trigger);
        let description = describe_trigger(&loop_cfg.trigger);
        out.push(ScheduleItem::Fleet {
            owner: fleet.name,
            trigger: loop_cfg.trigger,
            next_fires,
            status: if stopped { "stopped" } else { "enabled" }.into(),
            budget_usd: loop_cfg.budget_usd,
            autorun_env: std::env::var("MUR_FLEET_AUTORUN").is_ok_and(|v| v == "1"),
            description,
            next_note,
        });
    }
}

#[cfg(test)]
mod tests;
