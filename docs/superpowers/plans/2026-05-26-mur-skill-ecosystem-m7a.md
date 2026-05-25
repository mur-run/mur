# M7a — Cross-Agent Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open the first cross-agent window. Read-only aggregation of skill stats, peer enumeration, per-agent fitness scoring (pure computation), and a cross-agent extension of M5b's Jaccard consolidate. No mutation of peer state. No genetic recombination — that is M7b.

**Spec mapping:** §M7 cross-agent evolution (observability half), §10.1 evolution tracking, §9.4 consolidation (cross-agent extension).

**Scoping doc:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7-scoping.md`. This plan resolves three of its seven open questions (Q1 peer scope, Q2 fitness decay, Q7 partial — canonicaliser shape) and defers the rest to M7b/M7c.

**Hard dependencies:**
- M5a (`SkillStats` sidecar at `<MUR_HOME>/skills/<name>/stats.json`, `merge_in_place`, lifecycle predicates) — shipped #278.
- M5b (`run_consolidate`, `SkillView`, `dedup::scan`, JSONL report writer) — shipped #279.
- `mur-common::skill::store::agent_skill_dir` and `local::list_installed_agent` (per-agent layout already in repo).
- C6 idle scheduler (no new wiring — M7a is observability only; idle wiring lands in M7c).

**Soft dependency (one task only):**
- M6c.1 vector dedup — required by Task 9 (cross-agent vector dedup). Task 9 is the only blocked task and is sequenced last; everything else ships without it.

**What M7a ships:**
1. `SkillStats::path_agent(home, agent, skill)` + `list_peer_agents(home)` helpers in `mur-common`.
2. `mur agent peers [--json]` CLI.
3. `mur skill stats <name> --all-agents [--json]` cross-agent aggregation.
4. `AgentFitness` pure module: `fitness(home, agent, now) -> AgentFitness` with 7-day half-life weight decay.
5. `mur skill consolidate --cross-agent [--dry-run] [--apply]` reusing M5b's Jaccard dedup with `(skill_name, agent_name)` view; cross-agent JSONL report.
6. `mur agent card <name>` extended with a `Fitness` section.
7. Per-task tests + an integration test against a synthetic multi-agent `MUR_HOME` fixture.

**What M7a does NOT ship:**
- Vector-similarity cross-agent dedup → Task 9, blocked on M6c.1.
- `SkillGene`, gene diff, recombination → M7b.
- Automatic propagation, credit ledger, intent canonicaliser → M7c.
- Remote (non-same-host) peer discovery → M7+ (per resolved Q1 below).
- Any write to peer agents' skill state. M7a only writes to the invoking agent's own `_consolidation/` reports.

**Tech stack:** Rust 2024. No new dependencies. Reuses existing `serde_json`, `chrono`, `globset`, `walkdir`. JSONL report format mirrors M5b's `_consolidation/<date>.jsonl` for tooling consistency.

---

## Resolved open questions (from M7 scoping doc §4)

These are decided in this plan. The remaining four (Q3 diff granularity, Q4 recombination conflict, Q5 credit model, Q6 trust inheritance) belong to M7b/M7c plans and are explicitly out of scope here.

**Q1 — Peer discovery scope:** Same-host only. Enumerate `<MUR_HOME>/agents/<name>/` directories that contain `skills/`. Remote peers (configured `peers.yaml` over A2A) deferred to M7+. Rationale: same-host is filesystem-only, no auth, no network — keeps M7a entirely offline and testable with `tempdir`. Remote needs trust + secret + transport story that doesn't pay off until M7c propagation lands.

**Q2 — Fitness weight decay:** 7-day half-life on `last_used_at`, floor 0.1× original weight. Config keys `cross_agent.fitness.half_life_days` (default 7) and `cross_agent.fitness.floor` (default 0.1) in `config.yaml`. Half-life chosen to match typical sprint cadence; floor prevents long-offline agents from being zeroed out entirely.

**Q7 (partial) — Intent canonicaliser interface for M6b:** M6b inject path does **not** need a hook for the future canonicaliser. The canonicaliser (M7c) operates as a post-process pass that rewrites `intent` strings in stored `ProcedureStep`s during the propagation sweep, not at inject time. Inject reads whatever the stored intent is. This means M6b can ship with no forward-compat work for M7c — the canonicaliser will be invisible to it.

---

## File Structure

**Create:**
- `mur-common/src/skill/peers.rs` — `list_peer_agents(home) -> Vec<PeerAgent>`, `PeerAgent { name, home_path, skills_count }`.
- `mur-core/src/cross_agent/mod.rs` — module root.
- `mur-core/src/cross_agent/stats_agg.rs` — `AgentSkillStats { agent, name, stats }`, `aggregate_skill_stats(home, skill_name) -> Vec<AgentSkillStats>`.
- `mur-core/src/cross_agent/fitness.rs` — `AgentFitness { weight, success_rate, sample_size, last_seen, decayed }`, `fitness(home, agent, now)`, half-life decay helpers.
- `mur-core/src/cross_agent/consolidate.rs` — `run_consolidate_cross_agent(home, opts)`, `CrossAgentSkillView { view: SkillView, agent: String }`, cross-agent JSONL writer (writes to `<MUR_HOME>/skills/_consolidation/cross-agent-<date>.jsonl`).
- `mur-core/src/cmd/agent/peers.rs` — CLI dispatcher for `mur agent peers`.
- `mur-core/src/cmd/skill_stats_cross.rs` — CLI dispatcher overload when `--all-agents` is passed.
- `mur-core/tests/cross_agent_peers.rs` — `list_peer_agents` against synthetic multi-agent home.
- `mur-core/tests/cross_agent_fitness.rs` — fitness decay math + boundary cases (fresh agent, 7-day-old, 30-day-old, never-used).
- `mur-core/tests/cross_agent_consolidate.rs` — three-agent fixture with one duplicate pair across two agents; verifies cross-agent JSONL contents.

**Modify:**
- `mur-common/src/skill/stats.rs` — add `pub fn path_agent(home, agent, skill) -> PathBuf` returning `<home>/agents/<agent>/skills/<skill>/stats.json`. Existing `path` unchanged.
- `mur-common/src/skill/mod.rs` — `pub mod peers;`.
- `mur-core/src/lib.rs` — `pub mod cross_agent;`.
- `mur-core/src/cli/skill.rs` — add `#[arg(long)] all_agents: bool` to the `stats` subcommand; add `#[arg(long)] cross_agent: bool` to `consolidate`.
- `mur-core/src/cli/agent.rs` (or wherever agent subcommands are declared) — add `peers` subcommand with `--json` flag.
- `mur-core/src/cmd/agent/mod.rs` — register `peers` dispatcher.
- `mur-core/src/cmd/skill_consolidate.rs` — when `--cross-agent` is set, route to `cross_agent::consolidate::run_consolidate_cross_agent` instead of M5b's `run_consolidate`.
- `mur-core/src/cmd/agent/card.rs` — add a `Fitness` section calling `cross_agent::fitness::fitness` for the inspected agent.
- `mur-common/src/config.rs` (or whichever config module hosts skill toggles) — add `CrossAgentConfig { fitness_half_life_days: u32, fitness_floor: f64 }` defaulting to `(7, 0.1)`.

**Do not modify:**
- M5b's `run_consolidate(home, opts)` — leave it alone. The cross-agent variant is a sibling, not a refactor of the existing function.
- `SkillStats` schema — additive only (per Task 0 in M5b plan); M7a needs no new fields.
- `SkillView` — reused as-is. Cross-agent wraps it via composition.

---

### Task 1 — Per-agent path helpers + peer enumeration

**Files:** `mur-common/src/skill/stats.rs` (modify), `mur-common/src/skill/peers.rs` (new), `mur-common/src/skill/mod.rs` (modify).

- [ ] **Step 1: Add `SkillStats::path_agent`**

```rust
// mur-common/src/skill/stats.rs
impl SkillStats {
    pub fn path(mur_home: &Path, skill_name: &str) -> PathBuf {
        mur_home.join("skills").join(skill_name).join("stats.json")
    }

    /// Per-agent stats path: <MUR_HOME>/agents/<agent>/skills/<name>/stats.json
    pub fn path_agent(mur_home: &Path, agent: &str, skill_name: &str) -> PathBuf {
        mur_home
            .join("agents")
            .join(agent)
            .join("skills")
            .join(skill_name)
            .join("stats.json")
    }
}
```

No behavior change to existing `path`. Two unit tests verify both forms.

- [ ] **Step 2: `list_peer_agents`**

```rust
// mur-common/src/skill/peers.rs

use std::path::{Path, PathBuf};

pub struct PeerAgent {
    pub name: String,
    pub home_path: PathBuf,   // <MUR_HOME>/agents/<name>
    pub skills_count: usize,  // count of dirs under <home>/skills/
}

pub fn list_peer_agents(mur_home: &Path) -> std::io::Result<Vec<PeerAgent>> {
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(vec![]);
    }
    let mut peers = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let home_path = entry.path();
        let skills_dir = home_path.join("skills");
        let skills_count = if skills_dir.exists() {
            std::fs::read_dir(&skills_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .count()
        } else {
            0
        };
        peers.push(PeerAgent { name, home_path, skills_count });
    }
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(peers)
}
```

Skip hidden dirs (`.git`, `.cache`) by ignoring names starting with `.`.

- [ ] **Step 3: Module export**

In `mur-common/src/skill/mod.rs`: `pub mod peers;`. In `mur-common/src/lib.rs` re-export not needed — callers use `mur_common::skill::peers::list_peer_agents`.

- [ ] **Step 4: Unit tests in `peers.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lists_agent_directories() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("agents").join("alice").join("skills").join("s1")).unwrap();
        std::fs::create_dir_all(home.join("agents").join("bob").join("skills")).unwrap();
        std::fs::create_dir_all(home.join("agents").join(".hidden")).unwrap();

        let peers = list_peer_agents(home).unwrap();
        assert_eq!(peers.len(), 2);  // .hidden filtered
        assert_eq!(peers[0].name, "alice");
        assert_eq!(peers[0].skills_count, 1);
        assert_eq!(peers[1].name, "bob");
        assert_eq!(peers[1].skills_count, 0);
    }

    #[test]
    fn empty_when_agents_dir_missing() {
        let dir = tempdir().unwrap();
        assert!(list_peer_agents(dir.path()).unwrap().is_empty());
    }
}
```

---

### Task 2 — `mur agent peers` CLI

**Files:** `mur-core/src/cli/agent.rs` (modify), `mur-core/src/cmd/agent/peers.rs` (new), `mur-core/src/cmd/agent/mod.rs` (modify).

- [ ] **Step 1: CLI surface**

```rust
// in the agent subcommand enum
Peers {
    #[arg(long)]
    json: bool,
},
```

- [ ] **Step 2: Dispatcher**

```rust
// mur-core/src/cmd/agent/peers.rs
use mur_common::skill::peers::list_peer_agents;

pub fn cmd_peers(home: &Path, json: bool) -> anyhow::Result<()> {
    let peers = list_peer_agents(home)?;
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &peers.iter().map(|p| {
            serde_json::json!({
                "name": p.name,
                "home_path": p.home_path,
                "skills_count": p.skills_count,
            })
        }).collect::<Vec<_>>())?;
        println!();
        return Ok(());
    }
    if peers.is_empty() {
        println!("No peer agents found.");
        return Ok(());
    }
    println!("{:<24} {:>8}  {}", "AGENT", "SKILLS", "HOME");
    for p in &peers {
        println!("{:<24} {:>8}  {}", p.name, p.skills_count, p.home_path.display());
    }
    Ok(())
}
```

`PeerAgent` needs `#[derive(Serialize)]` for JSON path; add via `serde` (already a workspace dep).

- [ ] **Step 3: Wire dispatcher into `cmd/agent/mod.rs`**

Register alongside existing subcommands. Follow the same pattern as `cmd/agent/list.rs` (or whichever sibling has the most similar shape).

- [ ] **Step 4: Smoke test**

Manual test against `~/.mur` with the user's actual agents:

```bash
cargo run -- agent peers
cargo run -- agent peers --json
```

Expected: lists `carol`, `tg-bridge`, `tgX` with their skill counts.

---

### Task 3 — Cross-agent stats aggregation

**Files:** `mur-core/src/cross_agent/{mod.rs,stats_agg.rs}` (new), `mur-core/src/cli/skill.rs` (modify), `mur-core/src/cmd/skill_stats_cross.rs` (new).

- [ ] **Step 1: `AgentSkillStats` view + aggregator**

```rust
// mur-core/src/cross_agent/stats_agg.rs
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentSkillStats {
    pub agent: String,
    pub skill: String,
    pub usage_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub lifecycle: String,
}

/// Loads <skill_name> stats from every peer agent + the global skills dir.
/// Missing stats files are silently skipped (uninstalled or never used).
pub fn aggregate_skill_stats(home: &Path, skill_name: &str)
    -> anyhow::Result<Vec<AgentSkillStats>>
{
    let mut rows = Vec::new();

    // Global slot (mur skill stats already covers this — included for completeness)
    let global_path = SkillStats::path(home, skill_name);
    if global_path.exists() && let Some(stats) = SkillStats::load(&global_path)? {
        rows.push(row_from_stats("(global)", skill_name, &stats));
    }

    // Per-agent slots
    for peer in list_peer_agents(home)? {
        let path = SkillStats::path_agent(home, &peer.name, skill_name);
        if !path.exists() { continue; }
        if let Some(stats) = SkillStats::load(&path)? {
            rows.push(row_from_stats(&peer.name, skill_name, &stats));
        }
    }

    Ok(rows)
}

fn row_from_stats(agent: &str, skill: &str, s: &SkillStats) -> AgentSkillStats {
    AgentSkillStats {
        agent: agent.to_string(),
        skill: skill.to_string(),
        usage_count: s.usage_count,
        success_count: s.success_count,
        failure_count: s.failure_count,
        last_used_at: s.last_used_at,
        lifecycle: format!("{:?}", s.lifecycle_state),
    }
}
```

- [ ] **Step 2: CLI flag + dispatcher**

Add `--all-agents` to the existing `mur skill stats` subcommand. When set, branch to `cmd_skill_stats_all_agents`:

```rust
pub fn cmd_skill_stats_all_agents(home: &Path, name: &str, json: bool) -> anyhow::Result<()> {
    let rows = aggregate_skill_stats(home, name)?;
    if json {
        serde_json::to_writer_pretty(std::io::stdout(), &rows)?;
        println!();
        return Ok(());
    }
    if rows.is_empty() {
        println!("No stats found for '{}' on any agent.", name);
        return Ok(());
    }
    println!("{:<24} {:>8} {:>8} {:>8}  {:<10}  LAST USED",
        "AGENT", "USES", "OK", "FAIL", "LIFECYCLE");
    for r in &rows {
        println!("{:<24} {:>8} {:>8} {:>8}  {:<10}  {}",
            r.agent, r.usage_count, r.success_count, r.failure_count,
            r.lifecycle,
            r.last_used_at.map(|d| d.to_rfc3339()).unwrap_or_else(|| "-".into()));
    }
    // Aggregate footer
    let total_uses: u64 = rows.iter().map(|r| r.usage_count).sum();
    let total_ok: u64 = rows.iter().map(|r| r.success_count).sum();
    let success_rate = if total_uses > 0 {
        total_ok as f64 / total_uses as f64
    } else { 0.0 };
    println!("\nPopulation: {} agents, {} uses, {:.1}% success",
        rows.len(), total_uses, success_rate * 100.0);
    Ok(())
}
```

- [ ] **Step 3: Test**

Unit test in `stats_agg.rs` builds a fixture with three agents (one missing stats, one fresh, one mature) and asserts the row vector matches.

---

### Task 4 — `AgentFitness` pure computation

**Files:** `mur-core/src/cross_agent/fitness.rs` (new), `mur-common/src/config.rs` (modify), `mur-core/tests/cross_agent_fitness.rs` (new).

- [ ] **Step 1: Config additions**

```rust
// mur-common/src/config.rs (or skill section thereof)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossAgentConfig {
    #[serde(default = "default_half_life_days")]
    pub fitness_half_life_days: u32,
    #[serde(default = "default_fitness_floor")]
    pub fitness_floor: f64,
}

fn default_half_life_days() -> u32 { 7 }
fn default_fitness_floor() -> f64 { 0.1 }

impl Default for CrossAgentConfig {
    fn default() -> Self {
        Self {
            fitness_half_life_days: default_half_life_days(),
            fitness_floor: default_fitness_floor(),
        }
    }
}
```

Add `pub cross_agent: CrossAgentConfig` (with `#[serde(default)]`) to the top-level config struct. No migration — existing config files without the key get defaults.

- [ ] **Step 2: Fitness module**

```rust
// mur-core/src/cross_agent/fitness.rs
use chrono::{DateTime, Utc};
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::stats::SkillStats;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentFitness {
    pub agent: String,
    pub weight: f64,         // decayed_success_rate * recency_decay
    pub success_rate: f64,   // success / (success + failure), or 0 if no samples
    pub sample_size: u64,    // total usage_count across this agent's skills
    pub last_seen: Option<DateTime<Utc>>,
    pub recency_decay: f64,  // ∈ [floor, 1.0]
}

pub fn fitness(
    home: &Path,
    agent: &str,
    now: DateTime<Utc>,
    half_life_days: u32,
    floor: f64,
) -> anyhow::Result<AgentFitness> {
    let mut sample_size = 0u64;
    let mut success_total = 0u64;
    let mut failure_total = 0u64;
    let mut latest: Option<DateTime<Utc>> = None;

    for skill in list_installed_agent(home, agent).map_err(|e| anyhow::anyhow!("{e}"))? {
        let path = SkillStats::path_agent(home, agent, &skill);
        if !path.exists() { continue; }
        let Some(stats) = SkillStats::load(&path)? else { continue; };
        sample_size += stats.usage_count;
        success_total += stats.success_count;
        failure_total += stats.failure_count;
        if let Some(t) = stats.last_used_at {
            latest = Some(latest.map_or(t, |prev| prev.max(t)));
        }
    }

    let success_rate = if success_total + failure_total > 0 {
        success_total as f64 / (success_total + failure_total) as f64
    } else { 0.0 };

    let recency_decay = match latest {
        Some(t) => decay_factor(now - t, half_life_days, floor),
        None => floor,  // never used → floor
    };

    Ok(AgentFitness {
        agent: agent.to_string(),
        weight: success_rate * recency_decay,
        success_rate,
        sample_size,
        last_seen: latest,
        recency_decay,
    })
}

pub fn decay_factor(elapsed: chrono::Duration, half_life_days: u32, floor: f64) -> f64 {
    let days = elapsed.num_seconds() as f64 / 86_400.0;
    if days < 0.0 { return 1.0; }  // clock skew guard
    let raw = 0.5_f64.powf(days / half_life_days as f64);
    raw.max(floor)
}
```

- [ ] **Step 3: Tests**

```rust
// mur-core/tests/cross_agent_fitness.rs
use chrono::{Duration, Utc};
use mur_core::cross_agent::fitness::decay_factor;

#[test]
fn decay_at_one_half_life_is_half() {
    let v = decay_factor(Duration::days(7), 7, 0.1);
    assert!((v - 0.5).abs() < 1e-6);
}

#[test]
fn decay_at_zero_is_one() {
    assert_eq!(decay_factor(Duration::zero(), 7, 0.1), 1.0);
}

#[test]
fn decay_floors_at_long_offline() {
    let v = decay_factor(Duration::days(365), 7, 0.1);
    assert_eq!(v, 0.1);
}

#[test]
fn negative_elapsed_treated_as_present() {
    let v = decay_factor(Duration::seconds(-3600), 7, 0.1);
    assert_eq!(v, 1.0);
}
```

Add an integration test that builds a synthetic agent with two skills (one used today with 9/10 success, one used 14 days ago with 5/10) and asserts the aggregate `weight` matches the hand-computed value within `1e-6`.

---

### Task 5 — Cross-agent consolidate (Jaccard reuse)

**Files:** `mur-core/src/cross_agent/consolidate.rs` (new), `mur-core/src/cli/skill.rs` (modify), `mur-core/src/cmd/skill_consolidate.rs` (modify).

This is the highest-value M7a deliverable: it surfaces when two agents have evolved duplicate skills that should converge.

- [ ] **Step 1: `CrossAgentSkillView`**

```rust
// mur-core/src/cross_agent/consolidate.rs
use crate::skill_consolidate::{SkillView, dedup, ConsolidateReport};
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;
use std::path::Path;

pub struct CrossAgentSkillView {
    pub view: SkillView,
    pub agent: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CrossAgentDuplicatePair {
    pub a_agent: String,
    pub a_skill: String,
    pub b_agent: String,
    pub b_skill: String,
    pub similarity: f64,
    pub keeper_agent: String,
    pub keeper_skill: String,
}

#[derive(Debug, Default)]
pub struct CrossAgentReport {
    pub duplicates: Vec<CrossAgentDuplicatePair>,
}

pub fn run_consolidate_cross_agent(
    home: &Path,
    opts: &crate::skill_consolidate::ConsolidateOptions,
) -> anyhow::Result<CrossAgentReport> {
    let views = load_all_peer_views(home)?;
    let mut report = CrossAgentReport::default();

    scan_cross_agent_duplicates(&views, &mut report);

    write_cross_agent_jsonl(home, &report, opts.apply)?;

    // M7a is read-only on peer state. `--apply` is accepted but only writes
    // the report — no skill mutations across agents in M7a.
    Ok(report)
}
```

- [ ] **Step 2: Load views from every peer**

```rust
fn load_all_peer_views(home: &Path) -> anyhow::Result<Vec<CrossAgentSkillView>> {
    let mut out = Vec::new();
    for peer in list_peer_agents(home)? {
        let agent_home = &peer.home_path;  // <home>/agents/<name>
        for skill_name in list_installed_agent(home, &peer.name)
            .map_err(|e| anyhow::anyhow!("{e}"))? {
            // Load manifest + stats (same shape as M5b's load_all_with_stats,
            // but rooted at the per-agent skills dir).
            let manifest_path = agent_home.join("skills").join(&skill_name).join("skill.yaml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else { continue };
            let Ok(m) = mur_common::skill::parser::parse_canonical(&text) else { continue };

            let stats_path = SkillStats::path_agent(home, &peer.name, &skill_name);
            let stats = SkillStats::load(&stats_path)?
                .unwrap_or_else(|| SkillStats::new(&skill_name, "unknown", "", chrono::Utc::now()));

            let view = SkillView {
                name: skill_name.clone(),
                description: m.description,
                triggers: m.triggers.into_iter().filter_map(|t| t.pattern).collect(),
                requires: m.requires.into_iter().map(|r| r.name).collect(),
                stats,
            };
            out.push(CrossAgentSkillView { view, agent: peer.name.clone() });
        }
    }
    Ok(out)
}
```

- [ ] **Step 3: Pairwise scan reusing M5b's Jaccard**

Reuse `dedup::tokens` and `dedup::jaccard` from M5b (export if needed — add `pub(crate)` or `pub` visibility at the `dedup` module level; that is the only M5b touchpoint).

```rust
fn scan_cross_agent_duplicates(views: &[CrossAgentSkillView], report: &mut CrossAgentReport) {
    const THRESHOLD: f64 = 0.85;  // same as M5b
    for i in 0..views.len() {
        for j in (i+1)..views.len() {
            let a = &views[i];
            let b = &views[j];

            // Same agent — already handled by M5b's intra-agent consolidate. Skip.
            if a.agent == b.agent { continue; }

            let sim = dedup::jaccard(&dedup::tokens(&a.view), &dedup::tokens(&b.view));
            if sim >= THRESHOLD {
                let (keeper_agent, keeper_skill) = select_keeper(a, b);
                report.duplicates.push(CrossAgentDuplicatePair {
                    a_agent: a.agent.clone(),
                    a_skill: a.view.name.clone(),
                    b_agent: b.agent.clone(),
                    b_skill: b.view.name.clone(),
                    similarity: sim,
                    keeper_agent,
                    keeper_skill,
                });
            }
        }
    }
}

fn select_keeper(a: &CrossAgentSkillView, b: &CrossAgentSkillView) -> (String, String) {
    // Same priority order as M5b dedup: higher lifecycle > higher success > alphabetical.
    // The result is informational only in M7a (we don't delete the loser).
    let prefer_a = match (a.view.stats.lifecycle_state, b.view.stats.lifecycle_state) {
        (la, lb) if la as i32 > lb as i32 => true,
        (la, lb) if la as i32 < lb as i32 => false,
        _ => match (a.view.stats.success_count, b.view.stats.success_count) {
            (sa, sb) if sa > sb => true,
            (sa, sb) if sa < sb => false,
            _ => a.agent.cmp(&b.agent).is_lt(),
        }
    };
    if prefer_a {
        (a.agent.clone(), a.view.name.clone())
    } else {
        (b.agent.clone(), b.view.name.clone())
    }
}
```

- [ ] **Step 4: JSONL report writer**

```rust
fn write_cross_agent_jsonl(home: &Path, report: &CrossAgentReport, applied: bool)
    -> anyhow::Result<()>
{
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let dir = home.join("skills").join("_consolidation");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("cross-agent-{date}.jsonl"));
    let mut file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&path)?;
    use std::io::Write;
    for d in &report.duplicates {
        let row = serde_json::json!({
            "type": "cross_agent_duplicate",
            "a_agent": d.a_agent,
            "a_skill": d.a_skill,
            "b_agent": d.b_agent,
            "b_skill": d.b_skill,
            "similarity": d.similarity,
            "keeper_agent": d.keeper_agent,
            "keeper_skill": d.keeper_skill,
            "applied": applied,
            "applied_at": applied.then(|| chrono::Utc::now().to_rfc3339()),
        });
        writeln!(file, "{row}")?;
    }
    Ok(())
}
```

- [ ] **Step 5: CLI flag wiring**

Add `--cross-agent` to `mur skill consolidate`. The existing `--apply` and `--dry-run` semantics carry over. Route in `cmd/skill_consolidate.rs`:

```rust
if cross_agent {
    let report = cross_agent::consolidate::run_consolidate_cross_agent(home, &opts)?;
    print_cross_agent_summary(&report);
    return Ok(());
}
// else fall through to existing M5b path
```

`print_cross_agent_summary` prints one line per duplicate plus a footer with the report path.

- [ ] **Step 6: Integration test**

`mur-core/tests/cross_agent_consolidate.rs`: build a tempdir with `agents/alice/skills/research/skill.yaml` + `agents/bob/skills/lookup/skill.yaml` whose descriptions paraphrase each other (Jaccard ≥ 0.85), plus an unrelated skill on agent `carol`. Run the function and assert exactly one `CrossAgentDuplicatePair` is emitted between alice and bob, none involving carol, and the JSONL file is written.

---

### Task 6 — `mur agent card` fitness section

**Files:** `mur-core/src/cmd/agent/card.rs` (modify).

- [ ] **Step 1: Append a Fitness section**

After the existing card sections, call into `cross_agent::fitness::fitness` for the agent being inspected and render:

```
Fitness
  weight:        0.612
  success_rate:  0.875 (35 ok / 5 fail / 40 total)
  recency:       0.700 (last seen 3.0 days ago)
  half_life:     7 days  floor: 0.10
```

When the agent has zero usage, print `Fitness: (no usage data)` instead.

- [ ] **Step 2: Manual verification**

```bash
cargo run -- agent card carol
```

Expected: existing sections render unchanged plus the new Fitness block at the end.

---

### Task 7 — End-to-end integration test

**File:** `mur-core/tests/cross_agent_e2e.rs` (new).

- [ ] **Step 1: Multi-agent fixture builder**

Single test that:
1. Creates a tempdir with three agents (alice, bob, carol) each with 2–3 skills, stats files at varied `last_used_at` and lifecycle states.
2. Calls `list_peer_agents` — asserts the three agents are returned.
3. Calls `aggregate_skill_stats(home, "<shared-skill>")` — asserts only the agents that have the skill appear.
4. Calls `fitness` on each agent — asserts the decay math.
5. Calls `run_consolidate_cross_agent` — asserts the expected duplicate pairs and that the JSONL report file exists with the expected line count.

This is the single test that proves M7a hangs together across modules. Keep it tight (~80 lines, one `#[test]` fn) so it documents the cross-agent flow for future readers.

---

### Task 8 — Docs

**Files:** `README.md` (modify), `docs/architecture/runtime-overview.md` (modify if it has a CLI surface section).

- [ ] **Step 1: README CLI table updates**

Add three rows to whatever CLI surface table the README has:
- `mur agent peers [--json]`
- `mur skill stats <name> --all-agents`
- `mur skill consolidate --cross-agent`

- [ ] **Step 2: `runtime-overview.md` cross-agent paragraph**

Add a 5–10 line subsection under whatever section catalogs the agent runtime, titled `### Cross-agent observability (M7a)`, summarising what's read, what's not written, and pointing back to this plan.

Skip if no such section exists — `runtime-overview.md` is the architecture doc, not the changelog. Don't manufacture a section just to document M7a.

---

### Task 9 — [BLOCKED on M6c.1] Cross-agent vector dedup

**Status:** Not started. Do not start until M6c.1 ships and exposes a `VectorDedup` API.

**Files (placeholder, to be confirmed once M6c.1 lands):** `mur-core/src/cross_agent/consolidate_vector.rs` (new).

**Intended shape:**
- Reuse M6c.1's `VectorDedup::find_pairs(corpus, threshold)` against a corpus assembled from per-agent skill embeddings.
- Same `CrossAgentDuplicatePair` output, with an extra field `similarity_source: "vector"` to distinguish from Jaccard rows.
- Same JSONL file (additional lines with `"type": "cross_agent_duplicate_vector"`).
- Gated by `--cross-agent --vector` (additive flag on `mur skill consolidate`).

**Hand-off checklist (do this at M6c.1 merge):**
1. Confirm M6c.1's `VectorDedup` API accepts a corpus selector (not hardcoded to global skills only). If it does not, file a tiny follow-up to add one before starting this task.
2. Confirm embedding model availability for per-agent skills — if per-agent skills aren't auto-indexed, decide whether M7a indexes them on demand or whether per-agent indexing is its own task.
3. Re-open this plan and unblock Task 9.

Do not implement speculatively. The M6c.1 API shape is the gate.

---

## Verification checklist

Before declaring M7a complete:

1. `cargo build --workspace` — clean.
2. `cargo clippy --workspace -- -D warnings` — clean.
3. `cargo fmt --check` — clean.
4. `cargo test --workspace` — all green; includes the four new test files.
5. Manual smoke on the user's actual `~/.mur` (which has carol, tg-bridge, tgX):
   - `mur agent peers` lists three agents.
   - `mur agent peers --json` produces parseable JSON.
   - `mur skill stats <some-installed-skill> --all-agents` runs without panic (rows may be empty if nothing has used the skill).
   - `mur agent card carol` shows the Fitness block.
   - `mur skill consolidate --cross-agent --dry-run` runs and (if duplicates exist) writes `~/.mur/skills/_consolidation/cross-agent-<date>.jsonl`.
6. Task 9 stays unticked. M7a ships with Tasks 1–8 complete; Task 9 is a known-blocked successor, not a regression.

---

## Out of scope (carried to M7b/M7c)

- **`SkillGene` + recombination** → M7b. M7a builds the visibility layer; M7b builds the genetic layer.
- **Automatic propagation, idle hook, credit ledger, intent canonicaliser** → M7c.
- **Remote peer discovery** → M7+. Same-host filesystem scan only in M7a.
- **Vector dedup** → Task 9, blocked on M6c.1.
- **Cross-agent contradiction / orphan detection** → deferred. Contradictions across agents are often legitimate divergence, not bugs; orphans on one agent are normal usage variation. Only cross-agent duplicates (Jaccard ≥ 0.85) are clearly actionable and ship in M7a.

---

## Open questions deferred to later plans

| # | Question | Decided in |
|---|---|---|
| Q3 | Gene diff granularity (field vs token level) | M7b plan |
| Q4 | Recombination conflict resolution | M7b plan |
| Q5 | Credit model (leaderboard vs token vs fitness-only) | M7c plan |
| Q6 | Trust inheritance across agents | M7c plan |
| Q7 | Intent canonicaliser ownership + storage location | M7c plan (M6b needs no forward-compat — resolved above) |
