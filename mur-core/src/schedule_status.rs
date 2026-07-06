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
    },
    AgentIdle {
        owner: String,
        after_secs: u64,
        cooldown_secs: u64,
        message: String,
        status: String,
    },
    Workflow {
        owner: String,
        expr: Option<String>,
        next_fires: Vec<String>,
        status: String,
    },
    Fleet {
        owner: String,
        trigger: String,
        next_fires: Vec<String>,
        status: String,
        budget_usd: f64,
        autorun_env: bool,
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
            out.push(ScheduleItem::AgentCron {
                owner: name.clone(),
                expr: s.cron.clone(),
                message: s.message.clone(),
                next_fires: fires(&s.cron),
                status: "enabled".into(),
            });
        }
        for t in &profile.lifecycle.idle_triggers {
            out.push(ScheduleItem::AgentIdle {
                owner: name.clone(),
                after_secs: t.after_secs,
                cooldown_secs: t.cooldown_secs,
                message: t.message.clone(),
                status: "enabled".into(),
            });
        }
    }
}

fn collect_workflows(out: &mut Vec<ScheduleItem>) {
    for s in list_system_schedules_detailed() {
        let next_fires = s.cron.as_deref().map(fires).unwrap_or_default();
        out.push(ScheduleItem::Workflow {
            owner: s.workflow,
            expr: s.cron,
            next_fires,
            status: "enabled".into(),
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
        out.push(ScheduleItem::Fleet {
            owner: fleet.name,
            trigger: loop_cfg.trigger,
            next_fires,
            status: if stopped { "stopped" } else { "enabled" }.into(),
            budget_usd: loop_cfg.budget_usd,
            autorun_env: std::env::var("MUR_FLEET_AUTORUN").is_ok_and(|v| v == "1"),
        });
    }
}

#[cfg(test)]
mod tests {
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
