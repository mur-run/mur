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

use mur_agent_runtime::scheduler::next_n_fires;

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

/// Why an item has no next fire.
///
/// The invariant this exists for: **an empty `next_fires` always carries a
/// note.** A blank "Next" column is indistinguishable from "will not run
/// again", and one of the things it currently hides is a fleet that runs every
/// thirty minutes.
fn trigger_note(trigger: &str) -> Option<String> {
    if trigger == "manual" {
        return Some("no timetable — this runs only when something starts it".into());
    }
    if trigger.starts_with("interval:") {
        // `.last_run` is named by `schedule_status`'s own comment and by
        // `cmd/fleet/export.rs` as the state this would need — and nothing in
        // the tree writes it, so the gap is permanent until something does.
        return Some(
            "not tracked — an interval fires relative to its last run, which is not recorded"
                .into(),
        );
    }
    Some(format!("could not read a schedule from `{trigger}`"))
}

fn fires(expr: &str) -> Vec<String> {
    next_n_fires(expr, NEXT_FIRE_COUNT)
        .map(|v| v.iter().map(|t| t.to_rfc3339()).collect())
        .unwrap_or_default()
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
            let next_fires = fires(&s.cron);
            out.push(ScheduleItem::AgentCron {
                owner: name.clone(),
                expr: s.cron.clone(),
                message: s.message.clone(),
                description: describe_cron(&s.cron),
                next_note: next_fires
                    .is_empty()
                    .then(|| format!("could not read a schedule from `{}`", s.cron)),
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
        // Only cron-style triggers get next-fire previews; interval triggers
        // need `.last_run` state to compute the next fire and are shown with
        // an empty preview instead (fail-soft, not an error).
        let next_fires = loop_cfg
            .trigger
            .strip_prefix("cron:")
            .map(fires)
            .unwrap_or_default();
        let description = describe_trigger(&loop_cfg.trigger);
        let next_note = next_fires
            .is_empty()
            .then(|| trigger_note(&loop_cfg.trigger))
            .flatten();
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
mod tests {
    /// `(has a next fire, note explaining why not)` for any item.
    fn next_state(it: &ScheduleItem) -> (bool, Option<&str>) {
        match it {
            ScheduleItem::AgentCron {
                next_fires,
                next_note,
                ..
            }
            | ScheduleItem::Workflow {
                next_fires,
                next_note,
                ..
            }
            | ScheduleItem::Fleet {
                next_fires,
                next_note,
                ..
            } => (!next_fires.is_empty(), next_note.as_deref()),
            ScheduleItem::AgentIdle { next_note, .. } => (false, next_note.as_deref()),
        }
    }

    /// The invariant the whole change exists for: a blank "Next" is never an
    /// acceptable resting state. Either there is a fire time, or there is a
    /// sentence saying why there is not — never neither.
    #[test]
    fn every_item_either_has_a_next_fire_or_says_why_not() {
        let home = tempfile::tempdir().unwrap();
        let fleets = home.path().join("fleets");
        for (name, trigger) in [
            ("timed", "cron:*/15 * * * *"),
            ("paced", "interval:30m"),
            ("handstart", "manual"),
            ("garbled", "whenever-i-feel-like-it"),
        ] {
            let d = fleets.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("fleet.yaml"),
                format!(
                    "name: {name}\nchannel_id: fleet-{name}\nloop:\n  trigger: \"{trigger}\"\n"
                ),
            )
            .unwrap();
        }
        let st = schedule_status(home.path(), None);
        assert_eq!(st.schedules.len(), 4, "{:?}", st.schedules);
        for it in &st.schedules {
            let (has_fire, note) = next_state(it);
            assert!(
                has_fire != note.is_some(),
                "exactly one of a fire time and a note, got ({has_fire}, {note:?}) for {it:?}"
            );
        }
    }

    /// The row from the report: a fleet running every half hour rendered as
    /// "—", which reads as "will not run again".
    #[test]
    fn an_interval_fleet_says_why_it_has_no_next_time() {
        let home = tempfile::tempdir().unwrap();
        let d = home.path().join("fleets/smoke");
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("fleet.yaml"),
            "name: smoke\nchannel_id: fleet-smoke\nloop:\n  trigger: \"interval:30m\"\n",
        )
        .unwrap();
        let st = schedule_status(home.path(), None);
        let ScheduleItem::Fleet {
            description,
            next_note,
            ..
        } = &st.schedules[0]
        else {
            panic!("{:?}", st.schedules)
        };
        assert_eq!(description, "every 30m");
        assert!(
            next_note.as_deref().unwrap().contains("last run"),
            "{next_note:?}"
        );
    }

    #[test]
    fn cron_is_phrased_for_the_shapes_that_occur() {
        for (expr, want) in [
            ("*/15 * * * *", "every 15 minutes"),
            ("* * * * *", "every minute"),
            ("0 * * * *", "hourly"),
            ("30 9 * * *", "daily at 09:30"),
            ("0 3 * * *", "daily at 03:00"),
            ("30 9 * * 1-5", "weekdays at 09:30"),
        ] {
            assert_eq!(describe_cron(expr), want, "for {expr}");
        }
    }

    /// A cron string a reader can look up beats a sentence that is wrong, so
    /// anything unrecognised must come back verbatim rather than guessed at.
    #[test]
    fn an_unrecognised_cron_shape_is_returned_verbatim() {
        for expr in ["0 9 1 * *", "15 2 * 6 3", "not a cron", "0 9 * *"] {
            assert_eq!(describe_cron(expr), expr, "must not invent a phrasing");
        }
    }

    #[test]
    fn trigger_phrasing_covers_all_three_forms() {
        assert_eq!(describe_trigger("cron:0 * * * *"), "hourly");
        assert_eq!(describe_trigger("interval:2h"), "every 2h");
        assert_eq!(
            describe_trigger("manual"),
            "manual — runs only when started"
        );
    }

    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// A minimally-valid `AgentProfile` YAML fixture (all required top-level
    /// fields populated), parameterized by `name` and a `lifecycle:` block
    /// body so tests can plug in cron/idle-trigger schedules.
    fn minimal_profile_yaml(name: &str, lifecycle_body: &str) -> String {
        format!(
            r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: {name}
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: {{ tone: neutral, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "llama3.2:3b", params: {{ temperature: 0.2, max_tokens: 4096 }} }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: {{ mode: system }} }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {lifecycle_body}
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#
        )
    }

    #[test]
    fn aggregates_all_three_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(
            &home.join("agents/alice/profile.yaml"),
            &minimal_profile_yaml(
                "alice",
                r#"{ restart: on_failure, schedule: [{ cron: "30 9 * * 1-5", message: hi }], idle_triggers: [{ after_secs: 3600, message: yo }] }"#,
            ),
        );
        write(
            &home.join("fleets/dev/fleet.yaml"),
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n  budget_usd: 1.0\n",
        );

        let st = schedule_status(home, None);
        assert!(st.warnings.is_empty(), "{:?}", st.warnings);

        let kinds: Vec<&str> = st
            .schedules
            .iter()
            .map(|s| match s {
                ScheduleItem::AgentCron { .. } => "agent_cron",
                ScheduleItem::AgentIdle { .. } => "agent_idle",
                ScheduleItem::Workflow { .. } => "workflow",
                ScheduleItem::Fleet { .. } => "fleet",
            })
            .collect();
        assert!(kinds.contains(&"agent_cron"));
        assert!(kinds.contains(&"agent_idle"));
        assert!(kinds.contains(&"fleet"));

        // cron entries got next-fire previews
        let cron = st
            .schedules
            .iter()
            .find_map(|s| match s {
                ScheduleItem::AgentCron { next_fires, .. } => Some(next_fires),
                _ => None,
            })
            .unwrap();
        assert_eq!(cron.len(), 3);
    }

    #[test]
    fn stopped_fleet_reports_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(
            &home.join("fleets/dev/fleet.yaml"),
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"interval:1h\"\n",
        );
        write(&home.join("fleets/dev/.stopped"), "");

        let st = schedule_status(home, None);
        let ScheduleItem::Fleet { status, .. } = &st.schedules[0] else {
            panic!("expected a fleet schedule item");
        };
        assert_eq!(status, "stopped");
    }

    #[test]
    fn agent_filter_keeps_globals() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(
            &home.join("agents/alice/profile.yaml"),
            &minimal_profile_yaml(
                "alice",
                r#"{ restart: on_failure, schedule: [{ cron: "0 9 * * *", message: hi }] }"#,
            ),
        );
        write(
            &home.join("agents/bob/profile.yaml"),
            &minimal_profile_yaml(
                "bob",
                r#"{ restart: on_failure, schedule: [{ cron: "0 8 * * *", message: yo }] }"#,
            ),
        );
        write(
            &home.join("fleets/dev/fleet.yaml"),
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n",
        );

        let st = schedule_status(home, Some("alice"));

        assert!(st.schedules.iter().all(|s| match s {
            ScheduleItem::AgentCron { owner, .. } => owner == "alice",
            _ => true,
        }));
        assert!(
            st.schedules
                .iter()
                .any(|s| matches!(s, ScheduleItem::Fleet { .. }))
        );
    }

    #[test]
    fn broken_profile_is_a_warning_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(&home.join("agents/broken/profile.yaml"), "{ not: yaml: [");
        write(
            &home.join("agents/ok/profile.yaml"),
            &minimal_profile_yaml(
                "ok",
                r#"{ restart: on_failure, schedule: [{ cron: "0 9 * * *", message: hi }] }"#,
            ),
        );

        let st = schedule_status(home, None);

        assert_eq!(st.warnings.len(), 1);
        assert!(
            st.schedules
                .iter()
                .any(|s| matches!(s, ScheduleItem::AgentCron { owner, .. } if owner == "ok"))
        );
    }
}
