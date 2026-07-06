# murmur Panel P2 — Data Tabs + Schedule Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill the murmur Panel's data tabs (Information/Activities/Notifications) and add a fifth **Schedule** tab that unifies agent/workflow/fleet schedules.

**Architecture:** All aggregation logic lives in mur-core as library functions (the Hub's `src-tauri` already depends on `mur-core` directly — no shell-out needed). A hidden CLI `mur internals schedule-status` wraps the same function for debugging/spec parity. The Hub adds one Tauri-command module (`panel/data.rs`) and the React `PanelWindow` renders. murmur (TUI) side: only an additive `PanelTab::Schedule` variant + `/panel schedule` completion.

**Tech Stack:** Rust (mur-core, mur-hub-gui Tauri), React/TS (PanelWindow), serde JSON.

**Spec:** `docs/superpowers/specs/2026-07-06-murmur-panel-p2-data-tabs-design.md`

## Global Constraints

- Insert-only security model: no Panel action executes anything; clicks only send `insert {text}` (existing `panel_insert` command).
- Fail-soft aggregation: one unreadable schedule source → empty list for that source + entry in `warnings`; never a hard error.
- Single source file ≤ 800 lines (CLAUDE.md rule 4).
- No hardcoded values (CLAUDE.md rule 1) — reuse `mur_agent_runtime::scheduler::next_n_fires`, existing path helpers.
- `cargo fmt` + `cargo clippy --workspace -- -D warnings` green before every commit. Build env: `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` (mur-core lib needs it).
- Deviation noted from spec: Information cost shows **token counts** (input/output from telemetry); USD estimation deferred (fleet budget USD still shown on fleet Schedule rows, straight from `fleet.yaml`).
- Test with `cargo nextest run -p <crate>` if available, else `cargo test -p <crate>` (NOT `cargo test --workspace` — known flaky).

---

### Task 1: Detailed system-schedule listing (cron read-back)

`list_system_schedules()` returns only `(name, "launchd")` — the cron expression is lost. Add a detailed variant that recovers the cron expression: on macOS by reverse-parsing the plist's `StartCalendarInterval` (we wrote it, so the reverse mapping is deterministic), on Linux by splitting the tagged crontab line.

**Files:**
- Modify: `mur-core/src/cmd/system_schedule.rs`

**Interfaces:**
- Produces: `pub struct SystemSchedule { pub workflow: String, pub cron: Option<String> }`, `pub fn list_system_schedules_detailed() -> Vec<SystemSchedule>`, `pub(crate) fn calendar_interval_to_cron(plist: &str) -> Option<String>`, `pub(crate) fn crontab_line_to_cron(line: &str) -> Option<String>`

- [ ] **Step 1: Write failing tests** (append to the existing `#[cfg(test)] mod tests` in `system_schedule.rs`, or create the module if absent)

```rust
#[cfg(test)]
mod p2_tests {
    use super::*;

    #[test]
    fn calendar_interval_round_trips_cron() {
        // What install_launchd writes for "30 9 * * 1-5" — only Minute/Hour/Weekday present.
        let plist = r#"<dict>
  <key>StartCalendarInterval</key>
    <dict>
      <key>Minute</key>
      <integer>30</integer>
      <key>Hour</key>
      <integer>9</integer>
      <key>Weekday</key>
      <integer>1</integer>
    </dict>
</dict>"#;
        assert_eq!(calendar_interval_to_cron(plist).as_deref(), Some("30 9 * * 1"));
    }

    #[test]
    fn calendar_interval_missing_block_is_none() {
        assert_eq!(calendar_interval_to_cron("<dict></dict>"), None);
    }

    #[test]
    fn crontab_line_extracts_first_five_fields() {
        let line = "0 9 * * * /opt/homebrew/bin/mur run daily >> ~/.mur/logs/x.log 2>&1 # mur-schedule:daily";
        assert_eq!(crontab_line_to_cron(line).as_deref(), Some("0 9 * * *"));
    }

    #[test]
    fn crontab_short_line_is_none() {
        assert_eq!(crontab_line_to_cron("0 9 *"), None);
    }
}
```

- [ ] **Step 2: Run tests, verify FAIL** — `cargo test -p mur-core p2_tests` → compile error (functions undefined).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemSchedule {
    pub workflow: String,
    pub cron: Option<String>,
}

/// Recover a 5-field cron expression from the StartCalendarInterval block we
/// wrote in `cron_to_calendar_interval`. Keys map: Minute Hour Day Month
/// Weekday; a missing key means `*`. Only single-integer values were ever
/// written, so a simple key→integer scan is exact for our own plists.
pub(crate) fn calendar_interval_to_cron(plist: &str) -> Option<String> {
    if !plist.contains("StartCalendarInterval") {
        return None;
    }
    let field = |key: &str| -> String {
        plist
            .split(&format!("<key>{key}</key>"))
            .nth(1)
            .and_then(|rest| rest.split("<integer>").nth(1))
            .and_then(|rest| rest.split("</integer>").next())
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "*".to_string())
    };
    Some(format!(
        "{} {} {} {} {}",
        field("Minute"),
        field("Hour"),
        field("Day"),
        field("Month"),
        field("Weekday"),
    ))
}

/// First five whitespace fields of a crontab line, if there are enough.
pub(crate) fn crontab_line_to_cron(line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split_whitespace().take(6).collect();
    if fields.len() < 6 {
        return None; // needs 5 schedule fields + at least a command
    }
    Some(fields[..5].join(" "))
}

/// Like `list_system_schedules` but recovers each entry's cron expression.
pub fn list_system_schedules_detailed() -> Vec<SystemSchedule> {
    if cfg!(target_os = "macos") {
        list_launchd()
            .into_iter()
            .map(|(workflow, _)| {
                let cron = std::fs::read_to_string(plist_path(&workflow))
                    .ok()
                    .and_then(|body| calendar_interval_to_cron(&body));
                SystemSchedule { workflow, cron }
            })
            .collect()
    } else {
        // Tagged crontab lines: "<5 cron fields> <cmd...> # mur-schedule:<name>"
        let output = std::process::Command::new("crontab").arg("-l").output();
        let Ok(out) = output else { return Vec::new() };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let name = line.split(CRON_TAG_PREFIX).nth(1)?.trim().to_string();
                Some(SystemSchedule { workflow: name, cron: crontab_line_to_cron(line) })
            })
            .collect()
    }
}
```

Note: `plist_path`, `list_launchd`, `CRON_TAG_PREFIX` already exist in this file. Add `serde` derive import if the file lacks it (`serde::Serialize` is path-qualified above, so no new `use` needed).

- [ ] **Step 4: Run tests, verify PASS** — `cargo test -p mur-core p2_tests`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/system_schedule.rs
git commit -m "feat(schedule): detailed system-schedule listing with cron read-back"
```

---

### Task 2: `schedule_status` aggregator in mur-core

Library function unifying the three schedule sources. New file, exported from lib.

**Files:**
- Create: `mur-core/src/schedule_status.rs`
- Modify: `mur-core/src/lib.rs` (add `pub mod schedule_status;`) — check `main.rs` too: modules used by both binaries are declared in BOTH `lib.rs` and `main.rs` in this crate (known gotcha); this one is lib-only (CLI reaches it via `crate::`), so declare in `lib.rs` and, if the binary fails to resolve it, also in `main.rs`.

**Interfaces:**
- Consumes: Task 1's `list_system_schedules_detailed()`; `mur_agent_runtime::scheduler::next_n_fires(expr, n) -> Result<Vec<DateTime<Local>>>`; `mur_common::agent::AgentProfile` (fields `lifecycle.schedule: Vec<ScheduleEntry {cron, message, sends_to}>`, `lifecycle.idle_triggers: Vec<IdleTrigger {after_secs, message, cooldown_secs, ..}>`); `mur_common::fleet::Fleet` (fields `name`, `loop_cfg: Option<FleetLoop {trigger, budget_usd, ..}>`).
- Produces:

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ScheduleItem {
    AgentCron { owner: String, expr: String, message: String, next_fires: Vec<String>, status: String },
    AgentIdle { owner: String, after_secs: u64, cooldown_secs: u64, message: String, status: String },
    Workflow { owner: String, expr: Option<String>, next_fires: Vec<String>, status: String },
    Fleet { owner: String, trigger: String, next_fires: Vec<String>, status: String, budget_usd: f64, autorun_env: bool },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleStatus { pub schedules: Vec<ScheduleItem>, pub warnings: Vec<String> }

pub fn schedule_status(mur_home: &std::path::Path, agent_filter: Option<&str>) -> ScheduleStatus
```

- [ ] **Step 1: Write failing tests** (in-file `#[cfg(test)]`, tempdir fixtures)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &std::path::Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn aggregates_agent_and_fleet_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(
            &home.join("agents/alice/profile.yaml"),
            "name: alice\nlifecycle:\n  schedule:\n    - cron: \"30 9 * * 1-5\"\n      message: standup\n  idle_triggers:\n    - after_secs: 3600\n      message: \"still there?\"\n",
        );
        write(
            &home.join("fleets/dev/fleet.yaml"),
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n  budget_usd: 5.0\n",
        );
        let st = schedule_status(home, None);
        assert!(st.warnings.is_empty(), "{:?}", st.warnings);
        let kinds: Vec<&str> = st.schedules.iter().map(|s| match s {
            ScheduleItem::AgentCron { .. } => "agent_cron",
            ScheduleItem::AgentIdle { .. } => "agent_idle",
            ScheduleItem::Workflow { .. } => "workflow",
            ScheduleItem::Fleet { .. } => "fleet",
        }).collect();
        assert!(kinds.contains(&"agent_cron"));
        assert!(kinds.contains(&"agent_idle"));
        assert!(kinds.contains(&"fleet"));
        // cron entries got next-fire previews
        let cron = st.schedules.iter().find_map(|s| match s {
            ScheduleItem::AgentCron { next_fires, .. } => Some(next_fires),
            _ => None,
        }).unwrap();
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
        let ScheduleItem::Fleet { status, .. } = &st.schedules[0] else { panic!() };
        assert_eq!(status, "stopped");
    }

    #[test]
    fn agent_filter_keeps_globals() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(&home.join("agents/alice/profile.yaml"),
            "name: alice\nlifecycle:\n  schedule:\n    - cron: \"0 9 * * *\"\n      message: hi\n");
        write(&home.join("agents/bob/profile.yaml"),
            "name: bob\nlifecycle:\n  schedule:\n    - cron: \"0 8 * * *\"\n      message: yo\n");
        write(&home.join("fleets/dev/fleet.yaml"),
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n");
        let st = schedule_status(home, Some("alice"));
        // bob's entry filtered out; fleet (global) kept
        assert!(st.schedules.iter().all(|s| !matches!(s, ScheduleItem::AgentCron { owner, .. } if owner == "bob")));
        assert!(st.schedules.iter().any(|s| matches!(s, ScheduleItem::Fleet { .. })));
    }

    #[test]
    fn bad_profile_is_fail_soft() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(&home.join("agents/broken/profile.yaml"), ": not yaml [");
        write(&home.join("agents/ok/profile.yaml"),
            "name: ok\nlifecycle:\n  schedule:\n    - cron: \"0 9 * * *\"\n      message: hi\n");
        let st = schedule_status(home, None);
        assert_eq!(st.warnings.len(), 1);
        assert!(st.schedules.iter().any(|s| matches!(s, ScheduleItem::AgentCron { owner, .. } if owner == "ok")));
    }
}
```

- [ ] **Step 2: Run tests, verify FAIL** — `cargo test -p mur-core schedule_status` → unresolved module.

- [ ] **Step 3: Implement `mur-core/src/schedule_status.rs`**

```rust
//! Unified schedule view over the three scheduler subsystems (Panel P2):
//! agent cron/idle triggers (profile.yaml), workflow OS schedules
//! (launchd/crontab), and fleet loop.trigger. Fail-soft per source.
//! Spec: docs/superpowers/specs/2026-07-06-murmur-panel-p2-data-tabs-design.md

use std::path::Path;

use mur_agent_runtime::scheduler::next_n_fires;

use crate::cmd::system_schedule::list_system_schedules_detailed;

const NEXT_FIRE_COUNT: usize = 3;

// (ScheduleItem / ScheduleStatus exactly as in the Interfaces block above.)

pub fn schedule_status(mur_home: &Path, agent_filter: Option<&str>) -> ScheduleStatus {
    let mut schedules = Vec::new();
    let mut warnings = Vec::new();

    collect_agents(mur_home, agent_filter, &mut schedules, &mut warnings);
    collect_workflows(&mut schedules);
    collect_fleets(mur_home, &mut schedules, &mut warnings);

    ScheduleStatus { schedules, warnings }
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
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
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
            .and_then(|s| serde_yaml::from_str(&s).map_err(anyhow::Error::from))
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
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path().join("fleet.yaml");
        if !path.exists() {
            continue;
        }
        let fleet: mur_common::fleet::Fleet = match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_yaml::from_str(&s).map_err(anyhow::Error::from))
        {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!("fleet {}: {e}", entry.file_name().to_string_lossy()));
                continue;
            }
        };
        let Some(loop_cfg) = fleet.loop_cfg else { continue };
        let stopped = entry.path().join(".stopped").exists();
        let next_fires = loop_cfg
            .trigger
            .strip_prefix("cron:")
            .map(fires)
            .unwrap_or_default(); // ponytail: interval triggers need .last_run state; show none
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
```

Verify field paths against `mur_common::agent::AgentProfile` — the schedule lives at `lifecycle.schedule` (see `mur-common/src/agent.rs:776`). If the intermediate struct name differs (e.g. `profile.lifecycle` is `Option`), adapt with `.as_ref()` defaults — the tests define the contract, not this snippet.

- [ ] **Step 4: Run tests, verify PASS** — `cargo test -p mur-core schedule_status` (4 tests)

- [ ] **Step 5: fmt/clippy + Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/schedule_status.rs mur-core/src/lib.rs
git commit -m "feat(schedule): unified schedule_status aggregator (agent/workflow/fleet)"
```

---

### Task 3: Hidden CLI `mur internals schedule-status`

**Files:**
- Modify: `mur-core/src/cli/actions.rs` (add variant to `InternalsAction`, around line 142–167)
- Modify: `mur-core/src/dispatch.rs` (dispatch arm, near `Commands::Internals` at ~line 1194)

**Interfaces:**
- Consumes: `crate::schedule_status::schedule_status(home, agent.as_deref())` from Task 2; `crate::paths::mur_root(None)` (same helper `MigrateChannels` uses).
- Produces: `mur internals schedule-status [--agent <name>]` printing the `ScheduleStatus` JSON to stdout (always JSON — machine-facing; no `--json` flag needed).

- [ ] **Step 1: Add the variant** (in `InternalsAction`)

```rust
    /// Unified schedule view (agent/workflow/fleet) as JSON — Panel data source
    #[command(hide = true)]
    ScheduleStatus {
        /// Filter to one agent's entries (globals always included)
        #[arg(long)]
        agent: Option<String>,
    },
```

- [ ] **Step 2: Add the dispatch arm** (in `dispatch.rs`, inside the `Commands::Internals` match)

```rust
    InternalsAction::ScheduleStatus { agent } => {
        let home = crate::paths::mur_root(None);
        let st = crate::schedule_status::schedule_status(&home, agent.as_deref());
        println!("{}", serde_json::to_string_pretty(&st)?);
    }
```

- [ ] **Step 3: Verify manually**

Run: `cargo run -p mur-core --bin mur -- internals schedule-status` (with build env vars). Expected: JSON with `"schedules"` and `"warnings"` keys (contents depend on your `~/.mur`).

- [ ] **Step 4: fmt/clippy + Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): hidden 'mur internals schedule-status' JSON command"
```

---

### Task 4: Cost fold + proposals listing (mur-core lib fns)

Extract the telemetry token fold out of `cmd_stats` (DRY) and add a pending-proposal lister; both consumed by the Hub in Task 5.

**Files:**
- Modify: `mur-core/src/cmd/agent/stats.rs` (extract fold; `cmd_stats` calls it)
- Modify: `mur-core/src/harvest/proposal.rs` (add lister)

**Interfaces:**
- Produces:

```rust
// stats.rs
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct TokenTotals { pub llm_calls: u64, pub input_tokens: u64, pub output_tokens: u64 }
/// Fold `gen_ai.usage.*` rows from every telemetry/*.jsonl under `agent_dir`.
pub fn agent_token_totals(agent_dir: &std::path::Path) -> TokenTotals

// harvest/proposal.rs
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProposalSummary { pub file: String, pub modified: String /* rfc3339 */ }
/// Pending proposals in `<home>/inbox/workflow-proposals`, newest first, capped at `limit`.
pub fn list_pending(mur_home: &std::path::Path, limit: usize) -> Vec<ProposalSummary>
```

- [ ] **Step 1: Write failing tests**

In `stats.rs` tests:

```rust
#[test]
fn token_totals_folds_gen_ai_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let tdir = tmp.path().join("telemetry");
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(
        tdir.join("a.jsonl"),
        concat!(
            r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":40}"#, "\n",
            r#"{"not":"an llm row"}"#, "\n",
            r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5}"#, "\n",
        ),
    ).unwrap();
    let t = agent_token_totals(tmp.path());
    assert_eq!((t.llm_calls, t.input_tokens, t.output_tokens), (2, 110, 45));
}
```

In `proposal.rs` tests:

```rust
#[test]
fn list_pending_newest_first_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("inbox/workflow-proposals");
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["a.yaml", "b.yaml", "c.yaml"] {
        std::fs::write(dir.join(n), "x: 1\n").unwrap();
    }
    let list = list_pending(tmp.path(), 2);
    assert_eq!(list.len(), 2);
}

#[test]
fn list_pending_empty_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(list_pending(tmp.path(), 5).is_empty());
}
```

- [ ] **Step 2: Run, verify FAIL** — `cargo test -p mur-core token_totals list_pending`

- [ ] **Step 3: Implement**

`stats.rs` — move the existing loop body (lines ~30–45, the `telemetry_dir` fold) into:

```rust
pub fn agent_token_totals(agent_dir: &std::path::Path) -> TokenTotals {
    let mut t = TokenTotals::default();
    let telemetry_dir = agent_dir.join("telemetry");
    let Ok(entries) = std::fs::read_dir(&telemetry_dir) else { return t };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else { continue };
        for line in body.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if v.get("gen_ai.request.model").is_some() {
                t.llm_calls += 1;
                t.input_tokens += v["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += v["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0);
            }
        }
    }
    t
}
```

then rewrite `cmd_stats`'s fold to call it (keep its separate latency/error counters where they are — only the token/call counting moves; if entangled, leave `cmd_stats` untouched and accept the small duplication with a `// ponytail:` note).

`proposal.rs`:

```rust
pub fn list_pending(mur_home: &std::path::Path, limit: usize) -> Vec<ProposalSummary> {
    let dir = mur_home.join("inbox").join("workflow-proposals");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut items: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("yaml"))
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            Some((m, e.file_name().to_string_lossy().to_string()))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items
        .into_iter()
        .take(limit)
        .map(|(m, file)| ProposalSummary {
            file,
            modified: chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339(),
        })
        .collect()
}
```

- [ ] **Step 4: Run, verify PASS**, then run the crate's existing stats tests too: `cargo test -p mur-core stats proposal`

- [ ] **Step 5: fmt/clippy + Commit**

```bash
git add mur-core/src/cmd/agent/stats.rs mur-core/src/harvest/proposal.rs
git commit -m "feat(panel): token-totals fold + pending-proposal lister lib fns"
```

---

### Task 5: `PanelTab::Schedule` wire variant + `/panel schedule`

Additive protocol change — P1's tolerant decode ignores it on old Hubs.

**Files:**
- Modify: `mur-common/src/panel.rs` (enum + test)
- Modify: `mur-core/src/cmd/agent/cli/panel.rs` (tab-name parse, if a match exists there — search for `"notifications"`)
- Modify: `mur-core/src/cmd/agent/cli/complete.rs` (add `schedule` to the `/panel` subcommand list; search for `"preview"`)

**Interfaces:**
- Produces: `PanelTab::Schedule` serializing as `"schedule"`.

- [ ] **Step 1: Failing test** (extend `frames_round_trip` in `mur-common/src/panel.rs`)

```rust
        let line = serde_json::to_string(&PanelFrame::Panel { focus: PanelTab::Schedule }).unwrap();
        assert!(line.contains("\"focus\":\"schedule\""));
```

- [ ] **Step 2: Run, FAIL** — `cargo test -p mur-common panel`

- [ ] **Step 3: Add `Schedule` to `PanelTab`**, then grep `mur-core/src/cmd/agent/cli/` for the string `"notifications"` and add `"schedule" => PanelTab::Schedule` beside it in the tab parser, plus `schedule` in complete.rs's `/panel` candidates.

- [ ] **Step 4: Run, PASS** — `cargo test -p mur-common panel && cargo test -p mur-core panel`

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/panel.rs mur-core/src/cmd/agent/cli/panel.rs mur-core/src/cmd/agent/cli/complete.rs
git commit -m "feat(panel): Schedule tab wire variant + /panel schedule completion"
```

---

### Task 6: Hub Tauri data commands (`panel/data.rs`)

One new module exposing the P2 data to the frontend; Hub calls mur-core directly.

**Files:**
- Create: `mur-hub-gui/src-tauri/src/panel/data.rs`
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs` (`pub mod data;`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register the four commands in `invoke_handler` next to `panel_sessions`/`panel_insert`/`open_panel_window`)
- Modify: `mur-hub-gui/src-tauri/capabilities/panel.json` (allow the new commands, same shape as existing entries)

**Interfaces:**
- Consumes: `mur_core::schedule_status::{schedule_status, ScheduleStatus}` (Task 2), `mur_core::cmd::agent::stats::{agent_token_totals, TokenTotals}` (Task 4 — if `stats` is `pub(crate)` in mur-core, make the module `pub` as part of this task), `mur_core::harvest::proposal::{list_pending, ProposalSummary}` (Task 4), existing `work::list_channels` + `hitl.rs`'s pending list.
- Produces (Tauri commands, camelCase args on the TS side):
  - `panel_schedule_status(agent: Option<String>) -> ScheduleStatus`
  - `panel_cost(agent: String) -> TokenTotals`
  - `panel_proposals() -> Vec<ProposalSummary>`
  - `panel_git_info(cwd: String) -> GitInfo`

- [ ] **Step 1: Write `data.rs`**

```rust
//! Panel P2 data commands — thin adapters over mur-core lib fns.
//! Spec: docs/superpowers/specs/2026-07-06-murmur-panel-p2-data-tabs-design.md

use serde::Serialize;

fn mur_home() -> std::path::PathBuf {
    mur_core::paths::mur_root(None)
}

#[tauri::command]
pub fn panel_schedule_status(agent: Option<String>) -> mur_core::schedule_status::ScheduleStatus {
    mur_core::schedule_status::schedule_status(&mur_home(), agent.as_deref())
}

#[tauri::command]
pub fn panel_cost(agent: String) -> mur_core::cmd::agent::stats::TokenTotals {
    mur_core::cmd::agent::stats::agent_token_totals(&mur_home().join("agents").join(agent))
}

#[tauri::command]
pub fn panel_proposals() -> Vec<mur_core::harvest::proposal::ProposalSummary> {
    mur_core::harvest::proposal::list_pending(&mur_home(), 5)
}

#[derive(Debug, Default, Serialize)]
pub struct GitInfo {
    pub root: Option<String>,
    pub branch: Option<String>,
    pub branches: Vec<String>,
    pub worktrees: Vec<String>,
    pub dirty: bool,
}

/// Best-effort `git` shell-out against the session cwd; empty on non-repos.
#[tauri::command]
pub fn panel_git_info(cwd: String) -> GitInfo {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&cwd)
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let Some(root) = git(&["rev-parse", "--show-toplevel"]) else {
        return GitInfo::default();
    };
    GitInfo {
        branch: git(&["branch", "--show-current"]),
        branches: git(&["branch", "--format=%(refname:short)"])
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default(),
        worktrees: git(&["worktree", "list", "--porcelain"])
            .map(|s| {
                s.lines()
                    .filter_map(|l| l.strip_prefix("worktree "))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        dirty: git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
        root: Some(root),
    }
}
```

Check `mur_core::paths::mur_root` visibility — if not `pub`, use whatever home-resolution helper the Hub already uses (grep `src-tauri/src` for `.mur`/`mur_root`); mirror it.

- [ ] **Step 2: Register + capabilities.** In `lib.rs`, extend the `tauri::generate_handler![...]` list with `panel::data::panel_schedule_status, panel::data::panel_cost, panel::data::panel_proposals, panel::data::panel_git_info`. In `capabilities/panel.json`, add the four command names to the allow list, matching the syntax of the existing `panel_sessions` entry.

- [ ] **Step 3: Build check.** `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (needs a `ui/dist/index.html` stub if absent — known gotcha; don't commit the stub). Expected: green.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/panel/data.rs mur-hub-gui/src-tauri/src/panel/mod.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/panel.json
git commit -m "feat(hub): Panel P2 data commands (schedule/cost/proposals/git)"
```

---

### Task 7: Activities data command

Reuse `work.rs` channel summaries + pending HITL, filtered to the bound agent.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/panel/data.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` + `capabilities/panel.json` (register `panel_activities`)

**Interfaces:**
- Consumes: `crate::work::{list_channels, ChannelSummary}` (`list_channels(home: &Path) -> anyhow::Result<Vec<ChannelSummary>>`); `crate::hitl`'s `hitl_pending_list() -> Result<Vec<HitlRequestView>, String>` — reuse its inner logic; if it's only exposed as a `#[tauri::command]`, extract the body into a plain `pub fn pending_views() -> Vec<HitlRequestView>` and have both call it.
- Produces: `panel_activities(agent: String) -> Activities` where

```rust
#[derive(Serialize)]
pub struct Activities {
    pub channels: Vec<crate::work::ChannelSummary>,
    pub hitl: Vec<crate::hitl::HitlRequestView>,
}
```

- [ ] **Step 1: Implement**

```rust
#[tauri::command]
pub fn panel_activities(agent: String) -> Activities {
    let home = mur_home();
    let channels = crate::work::list_channels(&home)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            c.participants
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&agent))
        })
        .collect();
    let hitl = crate::hitl::pending_views()
        .into_iter()
        .filter(|h| h.agent.eq_ignore_ascii_case(&agent))
        .collect();
    Activities { channels, hitl }
}
```

Field names (`participants`, `p.name`, `h.agent`) must be checked against the real `ChannelSummary`/`HitlRequestView` structs (`work.rs:18-46`, `hitl.rs:76`) and adjusted — the filter intent (participant/agent name match, case-insensitive) is the contract.

- [ ] **Step 2: Build check + register + commit**

```bash
cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/panel/data.rs mur-hub-gui/src-tauri/src/hitl.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/panel.json
git commit -m "feat(hub): panel_activities command (channels + pending HITL per agent)"
```

---

### Task 8: PanelWindow frontend — five tabs with data

**Files:**
- Modify: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx`
- Modify: `mur-hub-gui/ui/src/components/panel/panel.css` (row/table styles as needed)

**Interfaces:**
- Consumes: Tauri commands from Tasks 6–7 via `invoke` (already imported in PanelWindow for `panel_sessions`/`panel_insert`); `panel_insert(pid, text)` for all click-to-insert actions.

- [ ] **Step 1: Extend types + fetching**

```tsx
type Tab = "information" | "activities" | "preview" | "notifications" | "schedule";
const TABS: Tab[] = ["information", "activities", "preview", "notifications", "schedule"];
// add "schedule": "Schedule" to TAB_LABEL

type ScheduleItem = {
  kind: "agent_cron" | "agent_idle" | "workflow" | "fleet";
  owner: string;
  expr?: string; trigger?: string; message?: string;
  next_fires?: string[]; status: string;
  after_secs?: number; cooldown_secs?: number;
  budget_usd?: number; autorun_env?: boolean;
};
type ScheduleStatus = { schedules: ScheduleItem[]; warnings: string[] };
type TokenTotals = { llm_calls: number; input_tokens: number; output_tokens: number };
type GitInfo = { root?: string; branch?: string; branches: string[]; worktrees: string[]; dirty: boolean };
type ProposalSummary = { file: string; modified: string };
type Activities = { channels: any[]; hitl: any[] }; // narrow to the real fields used below
```

State + effect: on `sess` change, tab switch, and window focus, fetch the active tab's data; additionally `setInterval` 30 000 ms polling for `activities` and `schedule` only while `document.visibilityState === "visible"` (clear on unmount/tab-change). Schedule fetch: `invoke<ScheduleStatus>("panel_schedule_status", { agent: showAll ? null : sess.agent })` with a `showAll` boolean toggle in the tab header.

- [ ] **Step 2: Render**

- **Information**: keep P1's agent/cwd/terminal `<dl>`; append git rows (root, branch + dirty dot, branches count, worktrees count) from `panel_git_info({ cwd: sess.cwd })` and cost rows (`llm_calls`, `input_tokens`, `output_tokens`) from `panel_cost({ agent: sess.agent })`. Remove the P1 `testInsert` demo button.
- **Activities**: list channels (name/state/preview) and pending HITL items; clicking a HITL item calls `panel_insert` with `` `mur channel approve ${h.channel_id} ${h.hitl_id}` `` (use the real field names from `HitlRequestView`).
- **Notifications**: proposal count headline ("N workflow proposals pending") + newest 5 rows from `panel_proposals()`; clicking inserts `mur session out`.
- **Schedule**: table with columns Kind / Owner / When / Next / Status. `When` = `expr` (cron kinds), `trigger` (fleet), or `every ${after_secs}s idle` (idle). `Next` = first entry of `next_fires` rendered as local time, tooltip with all three. `Status` shows `stopped` in red; fleet rows append `budget $X` and, when `autorun_env` is false, an "autorun off" hint. Render `warnings` as a muted footer line. `showAll` toggle switches the agent filter.

- [ ] **Step 3: Build + typecheck** — `cd mur-hub-gui/ui && npm run build`. Expected: success.

- [ ] **Step 4: Manual verify (P1 convention)** — build the Hub `.app` per the verified local recipe (`gotcha_hub_local_app_build_recipe`), run `murmur`, `/panel schedule` → Schedule tab shows real entries; `/panel` tab clicks; HITL/ proposal click inserts text into murmur's input box.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/
git commit -m "feat(hub-ui): Panel P2 — data-filled tabs + Schedule tab"
```

---

### Task 9: Workspace green + docs

- [ ] **Step 1:** `cargo fmt --all` (plus excluded Tauri crates via `--manifest-path` — CI gotcha), `cargo clippy --workspace -- -D warnings`, `cargo nextest run -p mur-core -p mur-common` (or `cargo test`).
- [ ] **Step 2:** Update `docs/architecture/runtime-overview.md` panel section (P2 shipped: five tabs, `internals schedule-status`); one-line mention in CLAUDE.md is NOT needed (operational file — panel detail lives in the spec).
- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs(panel): P2 data tabs shipped — runtime overview update"
```

---

## Self-Review

**Spec coverage:** Information git+cost → Tasks 6, 8. Activities → Tasks 7, 8. Notifications proposals → Tasks 4, 6, 8. Schedule tab + 3 sources → Tasks 1, 2, 6, 8. `internals schedule-status` → Task 3. Fail-soft → Task 2 (test included). Refresh triggers/30 s poll → Task 8. Zero murmur wire changes except additive tab variant → Task 5. Testing section of spec → Tasks 1, 2, 4 tests; manual UI → Task 8 Step 4.

**Deviations from spec (approved direction, noted in Global Constraints):** cost shown as token counts, USD deferred; Hub calls mur-core directly instead of shelling out (Hub already depends on mur-core — strictly simpler and the CLI command still exists for parity); no `--json` flag (command always emits JSON).

**Known verify-at-implementation points (flagged inline):** `AgentProfile.lifecycle` exact shape (Task 2), `hitl_pending_list` extraction (Task 7), `ChannelSummary` participant field names (Task 7), `mur_core::paths::mur_root` visibility (Task 6). Each task's tests define the contract where the snippet may need adaptation.
