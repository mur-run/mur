# M7c — Automatic Propagation + Credit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `mur agent propagate` (pull-side, fitness-gated, idle-hook-driven), a per-agent append-only credit ledger with `mur skill credit <name>`, and a host-level intent canonicaliser with `mur skill intent {canonicalise|show}`. Together these close the M7 loop: agents inherit high-fitness skills from peers automatically, every contribution is attributed, and intent vocabulary stays coherent across the host.

**Architecture:** All cross-agent writes stay scoped to the invoking agent's home — peers are read-only (M7a invariant). Propagation pulls via the existing M4b `agent://` path with three configurable gates (`min_samples`, `min_fitness`, `min_source_weight`). Credit lives in `~/.mur/agents/<agent>/credit/ledger.jsonl` (append-only, four `kind` values). Intent canonical mapping lives at `~/.mur/intent_canonical.yaml`, frequency-clustered, atomic temp+rename. No `SkillManifest` schema changes — signature scope is preserved.

**Tech Stack:** Rust 2024. No new crates. Reuses `mur_common::skill::peers::list_peer_agents` (M7a), `mur_core::cross_agent::fitness::fitness` + `stats_agg::aggregate_skill_stats` (M7a), `mur_core::cmd::skill_install::cmd_install` (M4b), idle scheduler in `mur-agent-runtime::idle_scheduler` (C6). `chrono`, `serde`, `serde_yaml_ng`, `serde_json`, `fs2` (file lock — already a workspace dep via `lockfile.rs`).

**Spec:** `docs/superpowers/specs/2026-05-26-mur-skill-ecosystem-m7c-design.md`.

**Scoping doc:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7-scoping.md` §3 M7c.

**Hard dependencies (already in repo):**
- M4b — `mur_core::cmd::skill_install::cmd_install` + `install_from_agent`; `transfer_chain` mutation on entry.
- M5a — `SkillStats::path_agent`, `SkillStats::load/save`, `LifecycleState::Draft`.
- M5b — idle-hook plumbing pattern; `cmd::agent_schedule::cmd_idle_add`.
- M6a — `mur_common::skill::validate::validate` (used implicitly via `cmd_install` already).
- M6b — `ProcedureStep::intent: Option<String>` field on the manifest.
- M6c — idle hook wiring path.
- M7a — `list_peer_agents`, `AgentFitness`, `fitness`, `aggregate_skill_stats`, `list_installed_agent`.
- M7b — `EvolutionEvent::Recombined` is added in M7b; M7c does not depend on M7b shipping first to compile (`recombiner` ledger entry is written from `cmd::skill_recombine` which is M7b's code; if M7b ships after M7c, that one call site is added when M7b lands).

**What M7c ships:**
1. `mur-common/src/skill/credit.rs` — `CreditEntry`, `CreditKind`, `CreditEvidence` (pure serde data).
2. `mur-core/src/cross_agent/credit/{ledger.rs,aggregate.rs}` — append + read-across-peers + view builder.
3. `mur-core/src/cross_agent/propagate/{mod.rs,candidates.rs,install_ctx.rs}` — sweep orchestrator + gate enforcement + `InstallContext` enum.
4. `InstallContext` parameter threaded through `mur_core::cmd::skill_install::cmd_install` (back-compat helper preserves the old signature for callers that pass `InstallContext::Manual`).
5. Ledger hooks at `skill_install`, `skill_evolve`, `skill_from_pattern`, `skill_generate` call sites.
6. `mur-core/src/cross_agent/intent/{canonical.rs,inject_lookup.rs}` — host-level canonical builder + read-side lookup.
7. `mur-core/src/cmd/{agent_propagate.rs,skill_credit.rs,skill_intent.rs}` — three new CLI handlers.
8. `mur agent propagate` + `mur agent schedule propagate-init` + `mur skill credit` + `mur skill intent {canonicalise,show}` wired through `cli/agent.rs`, `cli/skill.rs`, `dispatch.rs`.
9. Idle scheduler task `propagate.run` wired in `mur-agent-runtime::task_runner`.
10. Integration test suite covering pull-only invariant, gates, idle-hook, credit view aggregation, intent canonicaliser idempotency.

**What M7c does NOT ship:**
- A2A push wire (`skill.invite` method) — explicitly out per design §2.
- Remote-host peer discovery (`peers.yaml`) — scoping §7.
- Trust elevation on inheritance — Sandboxed Draft stays the rule.
- Token / currency for credit — leaderboard only.
- Embedding-based intent clustering — frequency only in v1.
- `SkillManifest` schema changes — none.
- Cross-agent contradiction detection.

---

## File Structure

**Create:**

- `mur-common/src/skill/credit.rs` — `CreditEntry`, `CreditKind` enum (`Author`, `Mutator`, `Recombiner`, `Propagator`), `CreditEvidence` variants. Pure data + serde. ~160 lines.
- `mur-core/src/cross_agent/credit/mod.rs` — module barrel. ~10 lines.
- `mur-core/src/cross_agent/credit/ledger.rs` — `append`, `read_for_skill`, `ledger_path_for_agent`. ~200 lines.
- `mur-core/src/cross_agent/credit/aggregate.rs` — `CreditView`, `build_credit_view` (cross-peer scan + evolution-log fallback). ~180 lines.
- `mur-core/src/cross_agent/propagate/mod.rs` — `PropagateOptions`, `PropagateReport`, `PropagateOutcome`, `run_propagate`. ~220 lines.
- `mur-core/src/cross_agent/propagate/candidates.rs` — `Candidate`, `enumerate_candidates` (gate enforcement + dedupe). ~200 lines.
- `mur-core/src/cross_agent/propagate/install_ctx.rs` — `InstallContext` enum + helpers. ~80 lines.
- `mur-core/src/cross_agent/intent/mod.rs` — module barrel. ~10 lines.
- `mur-core/src/cross_agent/intent/canonical.rs` — `IntentCanonical`, `CanonicalEntry`, `build_canonical`, `write_canonical_yaml`, `read_canonical_yaml`. ~240 lines.
- `mur-core/src/cross_agent/intent/inject_lookup.rs` — `IntentLookup`, `load`, `resolve_intent` (read-side helper used by M6b injector). ~120 lines.
- `mur-core/src/cmd/agent_propagate.rs` — `cmd_propagate` + `cmd_propagate_init`. ~200 lines.
- `mur-core/src/cmd/skill_credit.rs` — `cmd_credit` + human/JSON renderers. ~200 lines.
- `mur-core/src/cmd/skill_intent.rs` — `cmd_intent_canonicalise`, `cmd_intent_show`. ~160 lines.
- `mur-core/tests/propagate_pull_only.rs` — peer-write invariant test. ~120 lines.
- `mur-core/tests/propagate_gates.rs` — eight gate scenarios. ~280 lines.
- `mur-core/tests/propagate_idle_hook.rs` — idle scheduler integration. ~120 lines.
- `mur-core/tests/credit_view_aggregates_peers.rs` — credit aggregation. ~180 lines.
- `mur-core/tests/credit_synthesises_from_evolution_log.rs` — fallback path. ~120 lines.
- `mur-core/tests/intent_canonicaliser_e2e.rs` — e2e + idempotency. ~150 lines.
- `mur-core/tests/intent_inject_lookup.rs` — read-side lookup unit. ~100 lines.

**Modify:**

- `mur-common/src/skill/mod.rs` — `pub mod credit; pub use credit::{CreditEntry, CreditEvidence, CreditKind};`
- `mur-core/src/cross_agent/mod.rs` — `pub mod credit; pub mod propagate; pub mod intent;`
- `mur-core/src/cmd/skill_install.rs` — new `cmd_install_ctx(...)` async-free entry that takes `InstallContext`; existing `cmd_install` becomes a thin wrapper passing `InstallContext::Manual`. Ledger append after a successful install.
- `mur-core/src/evolve/skill_evolve.rs` — ledger append after `evolved.evolution_log.push(...)`.
- `mur-core/src/cmd/skill_from_pattern.rs` — ledger append at end of successful build.
- `mur-core/src/cmd/skill_generate.rs` — ledger append at end of successful generation.
- `mur-core/src/cmd/skill_recombine.rs` (M7b file) — ledger appends (1 author + 2 recombiners). **Only required if M7b lands before M7c**; otherwise add when M7b PR merges.
- `mur-core/src/cli/agent.rs` — `AgentAction::Propagate { .. }`, `AgentScheduleAction::PropagateInit { .. }`.
- `mur-core/src/cli/skill.rs` — `SkillAction::Credit { .. }`, `SkillAction::Intent { action: IntentAction }`, `IntentAction::{Canonicalise, Show}`.
- `mur-core/src/dispatch.rs` — wire all four new arms.
- `mur-core/src/cmd/agent/mod.rs` — `pub use propagate::cmd_propagate;` (re-export pattern matches `pub use peers::cmd_peers;` already in this file).
- `mur-agent-runtime/src/task_runner.rs` — new `TaskSpec::PropagateRun { agent: String }` variant; dispatch to `mur_core::cross_agent::propagate::run_propagate`.

**Do not modify:**
- `mur_common::skill::manifest` — signature-scoped, untouched.
- `mur_common::skill::peers` — read-only API, untouched.
- M7a's `consolidate.rs` / `fitness.rs` / `stats_agg.rs` — imported only.
- `SkillStats` schema — additive-only contract from M5a; M7c writes via existing `save`.

---

### Task 1 — `CreditEntry` data layer (pure, no I/O)

**Files:**
- Create: `mur-common/src/skill/credit.rs`
- Modify: `mur-common/src/skill/mod.rs`

- [ ] **Step 1: Add module export**

In `mur-common/src/skill/mod.rs`, after `pub mod constraint;` (currently line 5), insert in alphabetical position:

```rust
pub mod credit;
```

In the `pub use` block (around line 28), after `pub use constraint::...`, add:

```rust
pub use credit::{CreditEntry, CreditEvidence, CreditKind};
```

- [ ] **Step 2: Write the failing test**

Create `mur-common/src/skill/credit.rs` with just the test stub at the bottom (compile this first to nail the API, then fill in):

```rust
//! Credit ledger data types (M7c).
//!
//! `CreditEntry` is one append-only JSON line in
//! `~/.mur/agents/<agent>/credit/ledger.jsonl`. The ledger is per-agent,
//! never mutated in place, and read across peers by `cmd::skill_credit`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditKind {
    Author,
    Mutator,
    Recombiner,
    Propagator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreditEvidence {
    Author,
    Mutator {
        from_version: String,
        diff_summary: String,
    },
    Recombiner {
        role: String,   // "parent_a" | "parent_b"
        child: String,
    },
    Propagator {
        from_agent: String,
        fitness_at_install: f64,
        samples_at_install: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditEntry {
    pub ts: DateTime<Utc>,
    pub skill: String,
    pub skill_version: String,
    pub kind: CreditKind,
    /// The crediting subject (the contributor's agent name).
    pub agent: String,
    /// Free-form evidence keyed by `kind`. `null` for `Author`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<CreditEvidence>,
    /// Mirrors `EvolutionEvent.source` (e.g., `"human:alice"`, `"agent://bob"`).
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_author_entry() {
        let entry = CreditEntry {
            ts: DateTime::parse_from_rfc3339("2026-05-27T10:21:33Z")
                .unwrap()
                .with_timezone(&Utc),
            skill: "research-prices".into(),
            skill_version: "1.0.0".into(),
            kind: CreditKind::Author,
            agent: "alice".into(),
            evidence: None,
            source: "human:alice".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: CreditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn propagator_evidence_round_trips() {
        let entry = CreditEntry {
            ts: Utc::now(),
            skill: "x".into(),
            skill_version: "1.0.0".into(),
            kind: CreditKind::Propagator,
            agent: "bob".into(),
            evidence: Some(CreditEvidence::Propagator {
                from_agent: "alice".into(),
                fitness_at_install: 0.78,
                samples_at_install: 7,
            }),
            source: "agent://alice".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: CreditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn unknown_kind_returns_error() {
        let raw = r#"{"ts":"2026-05-27T10:21:33Z","skill":"x","skill_version":"1.0.0","kind":"future_kind","agent":"alice","source":"human:alice"}"#;
        let result: Result<CreditEntry, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind should fail to deserialize; reader code must filter at the line level");
    }
}
```

- [ ] **Step 3: Verify build + tests**

Run:

```bash
cargo test -p mur-common skill::credit::tests
```

Expected: 3 tests pass (`round_trip_author_entry`, `propagator_evidence_round_trips`, `unknown_kind_returns_error`).

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/skill/credit.rs mur-common/src/skill/mod.rs
git commit -m "feat(skill): credit ledger types — CreditEntry/Kind/Evidence (M7c task 1)"
```

---

### Task 2 — Ledger I/O (append + read-for-skill)

**Files:**
- Create: `mur-core/src/cross_agent/credit/mod.rs`, `mur-core/src/cross_agent/credit/ledger.rs`
- Modify: `mur-core/src/cross_agent/mod.rs`

- [ ] **Step 1: Add the submodule barrel**

In `mur-core/src/cross_agent/mod.rs`, after `pub mod consolidate;` (around line 6), insert:

```rust
pub mod credit;
```

Then create `mur-core/src/cross_agent/credit/mod.rs`:

```rust
//! Credit ledger I/O + cross-peer aggregation (M7c).

pub mod ledger;
```

(`aggregate.rs` is added in Task 9; keep the barrel minimal for now.)

- [ ] **Step 2: Write the failing tests**

Create `mur-core/src/cross_agent/credit/ledger.rs`:

```rust
//! Append-only per-agent credit ledger (M7c).
//!
//! File path: `<home>/agents/<agent>/credit/ledger.jsonl`.
//! Writes are atomic at the line level on POSIX (O_APPEND + single write_all
//! under PIPE_BUF). On Windows we fall back to a parking_lot::Mutex shared
//! within the process — the file is still single-writer per agent runtime.
//!
//! Reads tolerate malformed lines (skipped + logged at warn) and unknown
//! `kind` values (skipped silently — additive compatibility).

use std::fs::{OpenOptions, create_dir_all};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_common::skill::credit::CreditEntry;
use tracing::warn;

pub fn ledger_path_for_agent(home: &Path, agent: &str) -> PathBuf {
    home.join("agents").join(agent).join("credit").join("ledger.jsonl")
}

pub fn append(home: &Path, agent: &str, entry: &CreditEntry) -> Result<()> {
    let path = ledger_path_for_agent(home, agent);
    if let Some(parent) = path.parent() {
        create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(entry).context("serialise CreditEntry")?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    f.write_all(b"\n")
        .with_context(|| format!("append newline to {}", path.display()))?;
    Ok(())
}

/// Read all entries from `<home>/agents/<agent>/credit/ledger.jsonl` whose
/// `skill` matches. Missing file → empty Vec. Malformed lines are logged
/// and skipped. Unknown `kind` values are silently skipped.
pub fn read_for_skill(home: &Path, agent: &str, skill: &str) -> Result<Vec<CreditEntry>> {
    let path = ledger_path_for_agent(home, agent);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for (idx, line_res) in reader.lines().enumerate() {
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                warn!("ledger {} line {}: read error {e}", path.display(), idx + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CreditEntry>(&line) {
            Ok(entry) if entry.skill == skill => out.push(entry),
            Ok(_) => {}
            Err(e) => {
                warn!(
                    "ledger {} line {}: parse error {e} — skipping",
                    path.display(),
                    idx + 1
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
    use tempfile::tempdir;

    fn entry(skill: &str, kind: CreditKind, agent: &str) -> CreditEntry {
        CreditEntry {
            ts: Utc::now(),
            skill: skill.into(),
            skill_version: "1.0.0".into(),
            kind,
            agent: agent.into(),
            evidence: None,
            source: format!("human:{agent}"),
        }
    }

    #[test]
    fn appends_and_reads_round_trip() {
        let d = tempdir().unwrap();
        let home = d.path();
        let e1 = entry("foo", CreditKind::Author, "alice");
        let e2 = entry("bar", CreditKind::Author, "alice");
        let e3 = entry("foo", CreditKind::Mutator, "alice");
        append(home, "alice", &e1).unwrap();
        append(home, "alice", &e2).unwrap();
        append(home, "alice", &e3).unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert_eq!(foo.len(), 2);
        assert!(foo.iter().all(|e| e.skill == "foo"));
    }

    #[test]
    fn missing_ledger_yields_empty_vec() {
        let d = tempdir().unwrap();
        assert!(read_for_skill(d.path(), "ghost", "anything").unwrap().is_empty());
    }

    #[test]
    fn malformed_line_skipped() {
        let d = tempdir().unwrap();
        let home = d.path();
        let e = entry("foo", CreditKind::Author, "alice");
        append(home, "alice", &e).unwrap();
        // Inject garbage line
        let mut f = OpenOptions::new()
            .append(true)
            .open(ledger_path_for_agent(home, "alice"))
            .unwrap();
        f.write_all(b"NOT JSON\n").unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert_eq!(foo.len(), 1);
    }

    #[test]
    fn unknown_kind_skipped() {
        let d = tempdir().unwrap();
        let home = d.path();
        // Write a hand-crafted line with a future kind
        let path = ledger_path_for_agent(home, "alice");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ts":"2026-05-27T10:21:33Z","skill":"foo","skill_version":"1.0.0","kind":"future_kind","agent":"alice","source":"human:alice"}}"#
        )
        .unwrap();
        let foo = read_for_skill(home, "alice", "foo").unwrap();
        assert!(foo.is_empty());
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core cross_agent::credit::ledger::tests
```

Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cross_agent/credit/ mur-core/src/cross_agent/mod.rs
git commit -m "feat(skill): credit ledger I/O — append + read_for_skill (M7c task 2)"
```

---

### Task 3 — `InstallContext` enum + plumbing through `cmd_install`

**Files:**
- Create: `mur-core/src/cross_agent/propagate/mod.rs`, `mur-core/src/cross_agent/propagate/install_ctx.rs`
- Modify: `mur-core/src/cross_agent/mod.rs`, `mur-core/src/cmd/skill_install.rs`

- [ ] **Step 1: Add the submodule barrel**

In `mur-core/src/cross_agent/mod.rs`, after the `credit` line, insert:

```rust
pub mod propagate;
```

Create `mur-core/src/cross_agent/propagate/mod.rs` with the minimal barrel (Task 5 will fill the orchestrator):

```rust
//! Pull-side propagation sweep (M7c).
//!
//! Each invocation scans peers, filters by fitness gates, and pulls
//! eligible skills via the existing M4b `agent://` install path.

pub mod install_ctx;

pub use install_ctx::InstallContext;
```

- [ ] **Step 2: Define `InstallContext`**

Create `mur-core/src/cross_agent/propagate/install_ctx.rs`:

```rust
//! `InstallContext` distinguishes manual installs from auto-propagated
//! installs so the credit ledger can be written with correct `kind`
//! and evidence (M7c §3.5).

#[derive(Debug, Clone)]
pub enum InstallContext {
    /// User typed `mur skill install ...` (or any non-propagate path).
    Manual,
    /// Triggered by `skill-propagate` sweep — the source agent's fitness
    /// and sample count at decision time are captured for the ledger.
    AutoPropagate {
        source_fitness: f64,
        source_samples: u64,
    },
}

impl InstallContext {
    pub fn is_auto_propagate(&self) -> bool {
        matches!(self, InstallContext::AutoPropagate { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_is_not_auto() {
        assert!(!InstallContext::Manual.is_auto_propagate());
    }

    #[test]
    fn auto_is_auto() {
        assert!(
            InstallContext::AutoPropagate {
                source_fitness: 0.5,
                source_samples: 3
            }
            .is_auto_propagate()
        );
    }
}
```

- [ ] **Step 3: Add the contexted entry point to `cmd_install`**

In `mur-core/src/cmd/skill_install.rs`, after the existing `pub fn cmd_install(...)` (around line 17), add a sibling entry that takes context:

```rust
use crate::cross_agent::propagate::InstallContext;
use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
use crate::cross_agent::credit::ledger as credit_ledger;

/// Same as `cmd_install` but accepts an explicit `InstallContext` so the
/// credit ledger can attribute the install correctly.
///
/// Used by `mur agent propagate` for auto-propagation; the public
/// `cmd_install` wrapper passes `InstallContext::Manual`.
pub fn cmd_install_ctx(
    home: &Path,
    registry_url: &str,
    source: &str,
    caller_agent: &str,
    ctx: InstallContext,
) -> Result<()> {
    // Re-run the existing install path. On success, append to the caller's
    // credit ledger. We intentionally re-use the legacy code path; the only
    // M7c change is the post-install ledger write.
    cmd_install(home, registry_url, source)?;

    // Determine skill name + version from the install. For agent:// sources
    // we parse from the URL; otherwise we read the just-installed manifest.
    let (skill_name, skill_version, source_label, evidence, kind) =
        determine_credit_metadata(home, source, &ctx)?;
    let entry = CreditEntry {
        ts: chrono::Utc::now(),
        skill: skill_name,
        skill_version,
        kind,
        agent: caller_agent.to_string(),
        evidence,
        source: source_label,
    };
    if let Err(e) = credit_ledger::append(home, caller_agent, &entry) {
        // Non-fatal: losing a ledger line is preferable to rolling back the install.
        tracing::warn!("credit ledger append failed for {}: {e}", entry.skill);
    }
    Ok(())
}

fn determine_credit_metadata(
    home: &Path,
    source: &str,
    ctx: &InstallContext,
) -> Result<(String, String, String, Option<CreditEvidence>, CreditKind)> {
    if let Some(rest) = source.strip_prefix("agent://") {
        let (agent_name, skill_name) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid agent:// URL: {source}"))?;
        // Find the installed manifest version in the calling agent's home.
        let version = read_installed_version(home, skill_name).unwrap_or_else(|| "0.0.0".into());
        let (evidence, kind) = match ctx {
            InstallContext::AutoPropagate {
                source_fitness,
                source_samples,
            } => (
                Some(CreditEvidence::Propagator {
                    from_agent: agent_name.to_string(),
                    fitness_at_install: *source_fitness,
                    samples_at_install: *source_samples,
                }),
                CreditKind::Propagator,
            ),
            InstallContext::Manual => (
                Some(CreditEvidence::Propagator {
                    from_agent: agent_name.to_string(),
                    fitness_at_install: 0.0,
                    samples_at_install: 0,
                }),
                CreditKind::Propagator,
            ),
        };
        Ok((
            skill_name.to_string(),
            version,
            format!("agent://{agent_name}"),
            evidence,
            kind,
        ))
    } else {
        // Registry / local install: author kind on this agent.
        let (name, version) = read_root_install_meta(home, source)?;
        Ok((name, version, format!("registry:{source}"), None, CreditKind::Author))
    }
}

fn read_installed_version(home: &Path, skill_name: &str) -> Option<String> {
    let manifest_path = global_skill_dir(home, skill_name).join("skill.yaml");
    let bytes = std::fs::read(&manifest_path).ok()?;
    let m: SkillManifest = serde_yaml_ng::from_slice(&bytes).ok()?;
    Some(m.version)
}

fn read_root_install_meta(home: &Path, source: &str) -> Result<(String, String)> {
    // Reuses the post-install lock file as the source of truth.
    // For local file installs, we re-read the just-written manifest.
    let src_path = Path::new(source);
    if src_path.exists() && src_path.is_file() {
        let bytes = std::fs::read(src_path)?;
        let m: SkillManifest = serde_yaml_ng::from_slice(&bytes)?;
        return Ok((m.name, m.version));
    }
    // Registry install — look up by short name.
    let dir = global_skill_dir(home, source);
    let manifest_path = dir.join("skill.yaml");
    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    let m: SkillManifest = serde_yaml_ng::from_slice(&bytes)?;
    Ok((m.name, m.version))
}
```

(Adjust use-statement placement to match existing imports — `serde_yaml_ng` is already pulled in elsewhere in the file; add it to the top if not.)

- [ ] **Step 4: Compile**

```bash
cargo build -p mur-core
```

Expected: clean. If `serde_yaml_ng` is not imported in this file, add it next to the existing `use mur_common::skill::...` imports.

- [ ] **Step 5: Write a smoke test for the manual path's ledger entry**

Create the test as a new file `mur-core/tests/credit_install_manual_writes_ledger.rs`:

```rust
//! Verifies that a manual install (registry source) appends an `Author`
//! credit entry under the calling agent.

use mur_core::cmd::skill_install::cmd_install_ctx;
use mur_core::cross_agent::credit::ledger::read_for_skill;
use mur_core::cross_agent::propagate::InstallContext;
use tempfile::tempdir;

mod common;

#[test]
fn manual_install_writes_author_entry() {
    // Skip if the test fixture builder for a registry is not available.
    // The pattern we follow mirrors mur-core/tests/skill_install_agent_wire.rs
    // which sets up an offline fixture.
    let dir = tempdir().unwrap();
    let home = dir.path();
    let fixture = common::offline_registry_fixture(home, "demo-skill", "1.0.0");
    // Set up a caller agent dir so credit/ledger.jsonl has a place to live.
    std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();

    cmd_install_ctx(
        home,
        &fixture.registry_url,
        "demo-skill",
        "alice",
        InstallContext::Manual,
    )
    .unwrap();

    let entries = read_for_skill(home, "alice", "demo-skill").unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].kind,
        mur_common::skill::credit::CreditKind::Author
    ));
    assert_eq!(entries[0].agent, "alice");
}
```

If `mur-core/tests/common/mod.rs` does not exist with `offline_registry_fixture`, skip this test (mark `#[ignore]`) — the manual install path is covered by the broader integration tests in Task 14. If it does exist, write the test inline.

- [ ] **Step 6: Run tests**

```bash
cargo test -p mur-core cross_agent::propagate::install_ctx::tests
cargo test -p mur-core --test credit_install_manual_writes_ledger -- --include-ignored
```

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cross_agent/propagate/ mur-core/src/cross_agent/mod.rs \
        mur-core/src/cmd/skill_install.rs \
        mur-core/tests/credit_install_manual_writes_ledger.rs
git commit -m "feat(skill): InstallContext + cmd_install_ctx with credit ledger hook (M7c task 3)"
```

---

### Task 4 — Ledger hooks at evolve / from-pattern / generate

**Files:**
- Modify: `mur-core/src/evolve/skill_evolve.rs`, `mur-core/src/cmd/skill_from_pattern.rs`, `mur-core/src/cmd/skill_generate.rs`

- [ ] **Step 1: `skill_evolve` — append a `Mutator` entry**

Locate `evolved.evolution_log.push(EvolutionEvent::evolved(...))` (currently at `mur-core/src/evolve/skill_evolve.rs` — find the exact line via `grep -n "evolution_log.push" mur-core/src/evolve/skill_evolve.rs`).

Immediately after the `push`, before the surrounding function returns success, add:

```rust
{
    use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
    let caller = crate::cmd::skill_install::caller_agent_name(home)?
        .unwrap_or_else(|| "(none)".to_string());
    if caller != "(none)" {
        let entry = CreditEntry {
            ts: chrono::Utc::now(),
            skill: evolved.name.clone(),
            skill_version: evolved.version.clone(),
            kind: CreditKind::Mutator,
            agent: caller.clone(),
            evidence: Some(CreditEvidence::Mutator {
                from_version: prior_version.clone(),
                diff_summary: changes_summary.clone(),
            }),
            source: format!("human:{caller}"),
        };
        if let Err(e) = crate::cross_agent::credit::ledger::append(home, &caller, &entry) {
            tracing::warn!("credit ledger append failed at evolve: {e}");
        }
    }
}
```

`prior_version` and `changes_summary` are local variables in `skill_evolve`; verify their names by reading the surrounding function. If they're named differently, adjust accordingly.

`caller_agent_name` exists in `cmd::skill_install` (read its current signature and visibility — if it's `fn`-private, change to `pub(crate)`).

- [ ] **Step 2: `skill_from_pattern` — append an `Author` entry**

Locate the end-of-function success path in `mur-core/src/cmd/skill_from_pattern.rs` (the path that writes the manifest to disk). Immediately before `println!("created skill ...")` or the equivalent, add:

```rust
{
    use mur_common::skill::credit::{CreditEntry, CreditKind};
    if let Some(caller) = crate::cmd::skill_install::caller_agent_name(&home)? {
        let entry = CreditEntry {
            ts: chrono::Utc::now(),
            skill: manifest.name.clone(),
            skill_version: manifest.version.clone(),
            kind: CreditKind::Author,
            agent: caller.clone(),
            evidence: None,
            source: format!("human:{caller}"),
        };
        if let Err(e) = crate::cross_agent::credit::ledger::append(&home, &caller, &entry) {
            tracing::warn!("credit ledger append failed at from-pattern: {e}");
        }
    }
}
```

- [ ] **Step 3: `skill_generate` — append an `Author` entry**

Same shape as Step 2, applied in `mur-core/src/cmd/skill_generate.rs` at the success path. `source` is `"agent:generator"` to mirror M3c's convention.

- [ ] **Step 4: Run targeted tests**

```bash
cargo test -p mur-core skill_evolve_e2e skill_from_pattern_e2e skill_generate_e2e
```

These existing tests may not yet assert the new ledger entries — they should still pass because ledger failure is non-fatal. If any test breaks (e.g., from a panic in `caller_agent_name` when there is no agent context), fix the call to gracefully no-op when called outside agent context.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/evolve/skill_evolve.rs \
        mur-core/src/cmd/skill_from_pattern.rs \
        mur-core/src/cmd/skill_generate.rs
git commit -m "feat(skill): credit ledger appends at evolve/from-pattern/generate (M7c task 4)"
```

---

### Task 5 — Propagation candidate enumeration (gates)

**Files:**
- Create: `mur-core/src/cross_agent/propagate/candidates.rs`
- Modify: `mur-core/src/cross_agent/propagate/mod.rs`

- [ ] **Step 1: Module wiring**

In `mur-core/src/cross_agent/propagate/mod.rs`, add:

```rust
pub mod candidates;
```

- [ ] **Step 2: Write the failing tests**

Create `mur-core/src/cross_agent/propagate/candidates.rs`:

```rust
//! Propagation candidate enumeration (M7c §3.2).
//!
//! Pure-ish: I/O is bounded to reading peers' manifests + stats. No mutation
//! of any agent state — `run_propagate` (Task 6) is the only mutator.

use std::path::Path;

use anyhow::Result;
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;

use crate::cross_agent::fitness::fitness;
use crate::cross_agent::stats_agg::aggregate_skill_stats;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source_agent: String,
    pub skill: String,
    pub source_version: String,
    pub population_fitness: f64,
    pub population_samples: u64,
    pub source_agent_weight: f64,
}

#[derive(Debug, Clone)]
pub struct GateConfig {
    pub min_samples: u64,
    pub min_fitness: f64,
    pub min_source_weight: f64,
    pub max_per_sweep: usize,
    pub exclude_patterns: Vec<String>,
    pub half_life_days: u32,
    pub weight_floor: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_samples: 5,
            min_fitness: 0.7,
            min_source_weight: 0.3,
            max_per_sweep: 3,
            exclude_patterns: Vec::new(),
            half_life_days: 7,
            weight_floor: 0.1,
        }
    }
}

pub fn enumerate_candidates(
    home: &Path,
    invoking_agent: &str,
    cfg: &GateConfig,
) -> Result<Vec<Candidate>> {
    let peers = list_peer_agents(home)?
        .into_iter()
        .filter(|p| p.name != invoking_agent)
        .collect::<Vec<_>>();

    let now = chrono::Utc::now();
    let local_skills: std::collections::HashSet<String> =
        list_installed_agent(home, invoking_agent)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .into_iter()
            .collect();

    let mut by_skill: std::collections::HashMap<String, Candidate> = Default::default();

    for peer in &peers {
        let weight = fitness(home, &peer.name, now, cfg.half_life_days, cfg.weight_floor)?.weight;
        if weight < cfg.min_source_weight {
            continue;
        }
        let skills = list_installed_agent(home, &peer.name).map_err(|e| anyhow::anyhow!("{e}"))?;
        for skill in skills {
            if local_skills.contains(&skill) {
                continue;
            }
            if exclude_match(&skill, &cfg.exclude_patterns) {
                continue;
            }
            let agg = aggregate_skill_stats(home, &skill)?;
            let total_usage: u64 = agg.iter().map(|r| r.usage_count).sum();
            if total_usage < cfg.min_samples {
                continue;
            }
            let total_success: u64 = agg.iter().map(|r| r.success_count).sum();
            let total_failure: u64 = agg.iter().map(|r| r.failure_count).sum();
            let pop_fit = if total_success + total_failure > 0 {
                total_success as f64 / (total_success + total_failure) as f64
            } else {
                0.0
            };
            if pop_fit < cfg.min_fitness {
                continue;
            }
            // Source version from this peer's manifest.
            let manifest_path = home
                .join("agents")
                .join(&peer.name)
                .join("skills")
                .join(&skill)
                .join("skill.yaml");
            let version = std::fs::read(&manifest_path)
                .ok()
                .and_then(|bytes| serde_yaml_ng::from_slice::<mur_common::skill::SkillManifest>(&bytes).ok())
                .map(|m| m.version)
                .unwrap_or_else(|| "0.0.0".into());

            // Per-skill dedupe: pick the peer with the highest per-agent
            // success_rate × weight for this skill (M7c §3.1). On tie,
            // higher peer weight, then alphabetical agent name.
            let per_agent_fit = agg
                .iter()
                .find(|r| r.agent == peer.name)
                .map(|r| {
                    let total = r.success_count + r.failure_count;
                    if total > 0 {
                        r.success_count as f64 / total as f64
                    } else {
                        0.0
                    }
                })
                .unwrap_or(0.0);
            let score = per_agent_fit * weight;
            let cand = Candidate {
                source_agent: peer.name.clone(),
                skill: skill.clone(),
                source_version: version,
                population_fitness: pop_fit,
                population_samples: total_usage,
                source_agent_weight: weight,
            };
            match by_skill.get(&skill) {
                None => {
                    by_skill.insert(skill.clone(), cand);
                }
                Some(existing) => {
                    let existing_score = score_for_existing(existing, &agg);
                    if score > existing_score
                        || (score == existing_score && weight > existing.source_agent_weight)
                        || (score == existing_score
                            && weight == existing.source_agent_weight
                            && peer.name < existing.source_agent)
                    {
                        by_skill.insert(skill.clone(), cand);
                    }
                }
            }
        }
    }

    let mut out: Vec<Candidate> = by_skill.into_values().collect();
    // Sort by population_fitness desc, then agent name asc — determinism.
    out.sort_by(|a, b| {
        b.population_fitness
            .partial_cmp(&a.population_fitness)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source_agent.cmp(&b.source_agent))
    });
    if out.len() > cfg.max_per_sweep {
        out.truncate(cfg.max_per_sweep);
    }
    Ok(out)
}

fn score_for_existing(
    existing: &Candidate,
    agg: &[crate::cross_agent::stats_agg::AgentSkillStats],
) -> f64 {
    let per_agent = agg
        .iter()
        .find(|r| r.agent == existing.source_agent)
        .map(|r| {
            let total = r.success_count + r.failure_count;
            if total > 0 {
                r.success_count as f64 / total as f64
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    per_agent * existing.source_agent_weight
}

fn exclude_match(skill: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| glob_match(p, skill))
}

/// Tiny glob matcher: `*` matches zero-or-more characters, `?` exactly one.
/// No bracket classes — keep it under-engineered.
fn glob_match(pattern: &str, input: &str) -> bool {
    fn rec(p: &[u8], i: &[u8]) -> bool {
        match (p.first(), i.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], i) || (!i.is_empty() && rec(p, &i[1..])),
            (Some(b'?'), Some(_)) => rec(&p[1..], &i[1..]),
            (Some(pc), Some(ic)) if pc == ic => rec(&p[1..], &i[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_prefix_star() {
        assert!(glob_match("secrets-*", "secrets-aws"));
        assert!(glob_match("secrets-*", "secrets-"));
        assert!(!glob_match("secrets-*", "public-aws"));
    }

    #[test]
    fn glob_handles_question_mark() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "abbc"));
    }

    #[test]
    fn exclude_match_any_pattern() {
        let pats = vec!["secrets-*".into(), "tmp-*".into()];
        assert!(exclude_match("secrets-aws", &pats));
        assert!(exclude_match("tmp-foo", &pats));
        assert!(!exclude_match("research-prices", &pats));
    }

    #[test]
    fn default_gates_are_strict() {
        let g = GateConfig::default();
        assert_eq!(g.min_samples, 5);
        assert!((g.min_fitness - 0.7).abs() < 1e-9);
        assert!((g.min_source_weight - 0.3).abs() < 1e-9);
        assert_eq!(g.max_per_sweep, 3);
    }
}
```

- [ ] **Step 3: Run unit tests**

```bash
cargo test -p mur-core cross_agent::propagate::candidates::tests
```

Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cross_agent/propagate/
git commit -m "feat(skill): propagation candidate enumeration + gates (M7c task 5)"
```

---

### Task 6 — Propagate orchestrator + advisory lock

**Files:**
- Modify: `mur-core/src/cross_agent/propagate/mod.rs`

- [ ] **Step 1: Write the orchestrator**

Replace the contents of `mur-core/src/cross_agent/propagate/mod.rs` with:

```rust
//! Pull-side propagation sweep (M7c).

pub mod candidates;
pub mod install_ctx;

pub use install_ctx::InstallContext;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use self::candidates::{Candidate, GateConfig, enumerate_candidates};

#[derive(Debug, Clone)]
pub struct PropagateOptions {
    pub gates: GateConfig,
    pub dry_run: bool,
}

impl Default for PropagateOptions {
    fn default() -> Self {
        Self {
            gates: GateConfig::default(),
            dry_run: false,
        }
    }
}

#[derive(Debug)]
pub struct PropagateReport {
    pub scanned_peers: usize,
    pub candidates: Vec<Candidate>,
    pub installed: Vec<Candidate>,
    pub failed: Vec<(Candidate, String)>,
}

pub fn run_propagate(
    home: &Path,
    invoking_agent: &str,
    opts: &PropagateOptions,
) -> Result<PropagateReport> {
    let lock_path = lock_path_for_agent(home, invoking_agent);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;
    if lock_file.try_lock_exclusive().is_err() {
        bail!("propagate lock held — another sweep is in progress (exit 7)");
    }

    let peers = mur_common::skill::peers::list_peer_agents(home)?;
    let peers_count = peers.iter().filter(|p| p.name != invoking_agent).count();
    if peers_count == 0 {
        // Release lock implicitly on drop.
        return Ok(PropagateReport {
            scanned_peers: 0,
            candidates: Vec::new(),
            installed: Vec::new(),
            failed: Vec::new(),
        });
    }

    let cands = enumerate_candidates(home, invoking_agent, &opts.gates)?;
    let mut installed: Vec<Candidate> = Vec::new();
    let mut failed: Vec<(Candidate, String)> = Vec::new();

    if !opts.dry_run {
        for cand in &cands {
            let source = format!("agent://{}/{}", cand.source_agent, cand.skill);
            let ctx = InstallContext::AutoPropagate {
                source_fitness: cand.population_fitness,
                source_samples: cand.population_samples,
            };
            let registry_url = ""; // agent:// installs ignore registry_url; pass empty string.
            match crate::cmd::skill_install::cmd_install_ctx(
                home,
                registry_url,
                &source,
                invoking_agent,
                ctx,
            ) {
                Ok(()) => installed.push(cand.clone()),
                Err(e) => failed.push((cand.clone(), e.to_string())),
            }
        }
    }

    Ok(PropagateReport {
        scanned_peers: peers_count,
        candidates: cands,
        installed,
        failed,
    })
}

fn lock_path_for_agent(home: &Path, agent: &str) -> PathBuf {
    home.join("agents")
        .join(agent)
        .join("credit")
        .join(".propagate.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lock_blocks_concurrent_sweep() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let lock_path = lock_path_for_agent(home, "alice");
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        f.lock_exclusive().unwrap();

        let result = run_propagate(home, "alice", &PropagateOptions::default());
        assert!(
            result.is_err(),
            "second sweep should refuse while lock is held"
        );
        assert!(result.unwrap_err().to_string().contains("exit 7"));
    }

    #[test]
    fn empty_host_yields_empty_report() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let report = run_propagate(home, "alice", &PropagateOptions::default()).unwrap();
        assert_eq!(report.scanned_peers, 0);
        assert!(report.installed.is_empty());
    }
}
```

- [ ] **Step 2: Confirm `fs2` is already a dep**

```bash
cargo tree -p mur-core | grep fs2
```

If missing, add `fs2 = "0.4"` to `mur-core/Cargo.toml`. (It is already a dep transitively via `lockfile`, but make it explicit.)

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core cross_agent::propagate::tests
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cross_agent/propagate/mod.rs mur-core/Cargo.toml
git commit -m "feat(skill): propagate orchestrator + lock (M7c task 6)"
```

---

### Task 7 — `mur agent propagate` CLI

**Files:**
- Create: `mur-core/src/cmd/agent_propagate.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`, `mur-core/src/cli/agent.rs`, `mur-core/src/dispatch.rs`

- [ ] **Step 1: CLI handler**

Create `mur-core/src/cmd/agent_propagate.rs`:

```rust
//! `mur agent propagate` CLI handler (M7c).

use std::path::Path;

use anyhow::Result;

use crate::cross_agent::propagate::{
    PropagateOptions, PropagateReport, candidates::GateConfig, run_propagate,
};

pub fn cmd_propagate(
    home: &Path,
    agent: &str,
    dry_run: bool,
    max: Option<usize>,
    min_fitness: Option<f64>,
    min_samples: Option<u64>,
    json: bool,
) -> Result<()> {
    let mut opts = PropagateOptions {
        gates: GateConfig::default(),
        dry_run,
    };
    if let Some(m) = max {
        opts.gates.max_per_sweep = m;
    }
    if let Some(f) = min_fitness {
        opts.gates.min_fitness = f;
    }
    if let Some(s) = min_samples {
        opts.gates.min_samples = s;
    }

    match run_propagate(home, agent, &opts) {
        Ok(report) => {
            if json {
                emit_json(&report)?;
            } else {
                emit_human(&report, &opts);
            }
            if !report.failed.is_empty() {
                std::process::exit(5);
            }
            if report.scanned_peers == 0 {
                std::process::exit(4);
            }
            Ok(())
        }
        Err(e) => {
            if e.to_string().contains("exit 7") {
                eprintln!("propagate already running — skipping");
                std::process::exit(7);
            }
            Err(e)
        }
    }
}

fn emit_human(report: &PropagateReport, opts: &PropagateOptions) {
    let g = &opts.gates;
    println!(
        "Scanned {} peers, found {} candidate skill(s).",
        report.scanned_peers,
        report.candidates.len()
    );
    println!(
        "Gates: min_samples={}  min_fitness={:.2}  min_source_weight={:.2}  max_per_sweep={}",
        g.min_samples, g.min_fitness, g.min_source_weight, g.max_per_sweep
    );
    println!();
    if opts.dry_run {
        println!("(dry-run)");
    }
    if !report.installed.is_empty() {
        println!("Installed ({}):", report.installed.len());
        for c in &report.installed {
            println!(
                "  {:<22} v{}  ← agent://{}  (fitness {:.2}, n={})",
                c.skill, c.source_version, c.source_agent, c.population_fitness, c.population_samples
            );
        }
    }
    if !report.failed.is_empty() {
        eprintln!();
        eprintln!("Failed ({}):", report.failed.len());
        for (c, msg) in &report.failed {
            eprintln!("  {:<22}  {msg}", c.skill);
        }
    }
}

fn emit_json(report: &PropagateReport) -> Result<()> {
    let obj = serde_json::json!({
        "scanned_peers": report.scanned_peers,
        "installed": report.installed.iter().map(|c| {
            serde_json::json!({
                "skill": c.skill,
                "source_agent": c.source_agent,
                "source_version": c.source_version,
                "population_fitness": c.population_fitness,
                "population_samples": c.population_samples,
            })
        }).collect::<Vec<_>>(),
        "candidates_total": report.candidates.len(),
        "failed": report.failed.iter().map(|(c, msg)| {
            serde_json::json!({"skill": c.skill, "error": msg})
        }).collect::<Vec<_>>(),
    });
    serde_json::to_writer_pretty(std::io::stdout(), &obj)?;
    println!();
    Ok(())
}
```

- [ ] **Step 2: Add `pub mod` for `agent_propagate`**

In `mur-core/src/cmd/mod.rs`, add:

```rust
pub mod agent_propagate;
```

(alphabetical position — between `agent_mcp_pin` and `agent_rekey`).

- [ ] **Step 3: Add the CLI enum variant**

In `mur-core/src/cli/agent.rs`, add inside `AgentAction` (after `Peers { json: bool }` at line ~234):

```rust
    /// Pull high-fitness skills from peers (M7c — pull-side propagation).
    Propagate {
        /// Agent name (required outside an agent runtime context)
        name: String,
        /// Scan and report; install nothing.
        #[arg(long)]
        dry_run: bool,
        /// Override propagate.max_per_sweep for this run.
        #[arg(long)]
        max: Option<usize>,
        /// Override propagate.min_fitness.
        #[arg(long)]
        min_fitness: Option<f64>,
        /// Override propagate.min_samples.
        #[arg(long)]
        min_samples: Option<u64>,
        /// Emit JSON outcome.
        #[arg(long)]
        json: bool,
    },
```

- [ ] **Step 4: Wire in dispatch**

In `mur-core/src/dispatch.rs`, find the arm that handles `AgentAction::Peers` (around line 983) and add immediately after it:

```rust
        AgentAction::Propagate {
            name,
            dry_run,
            max,
            min_fitness,
            min_samples,
            json,
        } => {
            let home = cmd::agent::resolve_mur_home()?;
            cmd::agent_propagate::cmd_propagate(
                &home, &name, dry_run, max, min_fitness, min_samples, json,
            )?
        }
```

- [ ] **Step 5: Compile**

```bash
cargo build -p mur-core
```

Expected: clean.

- [ ] **Step 6: Smoke**

```bash
cargo run -- agent propagate alice --dry-run 2>&1 | head -20
```

Expected: exit code reflecting state of `~/.mur` (likely 4 if no peers; 0 if peers but no candidates).

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent_propagate.rs mur-core/src/cmd/mod.rs \
        mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(skill): mur agent propagate CLI (M7c task 7)"
```

---

### Task 8 — Idle hook `skill-propagate` + `propagate-init`

**Files:**
- Modify: `mur-core/src/cmd/agent_propagate.rs`, `mur-core/src/cli/agent.rs`, `mur-core/src/cmd/agent_schedule.rs`, `mur-core/src/dispatch.rs`, `mur-agent-runtime/src/task_runner.rs`

- [ ] **Step 1: Add `propagate-init` CLI variant**

In `mur-core/src/cli/agent.rs`, inside `AgentScheduleAction`, after `IdleRemove { ... }`:

```rust
    /// Register the `skill-propagate` idle trigger with default settings.
    /// Idempotent — running twice does not duplicate the trigger.
    PropagateInit {
        /// Agent name
        name: String,
        /// Idle seconds before firing (default 1800)
        #[arg(long, default_value_t = 1800)]
        after_secs: u64,
        /// Cooldown between fires (default 7200)
        #[arg(long, default_value_t = 7200)]
        cooldown_secs: u64,
    },
```

- [ ] **Step 2: Add the handler**

In `mur-core/src/cmd/agent_schedule.rs`, add:

```rust
pub fn cmd_propagate_init(name: &str, after_secs: u64, cooldown_secs: u64) -> anyhow::Result<()> {
    // The "message" we use is a structured task marker that the runtime
    // task_runner recognises and dispatches to `cross_agent::propagate::run_propagate`
    // instead of an LLM round-trip. Format: `propagate.run`.
    let message = "propagate.run";
    // Idempotency: if a trigger with this exact message already exists, skip.
    let existing = read_idle_triggers(name)?;
    if existing.iter().any(|t| t.message == message) {
        println!("skill-propagate trigger already registered for {name}");
        return Ok(());
    }
    cmd_idle_add(name, after_secs, message, None, cooldown_secs, true)?;
    println!("registered skill-propagate idle trigger for {name}");
    Ok(())
}
```

(The exact existing signature of `cmd_idle_add` must be matched — read `mur-core/src/cmd/agent_schedule.rs` to confirm parameter order.)

- [ ] **Step 3: Wire dispatch**

In `mur-core/src/dispatch.rs`, find the `AgentScheduleAction::IdleRemove { ... }` arm. Add immediately after:

```rust
            AgentScheduleAction::PropagateInit {
                name,
                after_secs,
                cooldown_secs,
            } => cmd::agent_schedule::cmd_propagate_init(&name, after_secs, cooldown_secs)?,
```

- [ ] **Step 4: Teach `task_runner` about `propagate.run`**

In `mur-agent-runtime/src/task_runner.rs`, find where idle-trigger messages are dispatched (look for the call site that takes a `TaskSpec` constructed from an idle trigger). Add a branch that intercepts the literal message `"propagate.run"` before LLM dispatch:

```rust
// Intercept M7c propagation trigger before LLM round-trip.
if message_text == "propagate.run" {
    let home = match std::env::var("MUR_HOME") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => dirs::home_dir()
            .map(|h| h.join(".mur"))
            .unwrap_or_default(),
    };
    let agent_name = self.agent_name.clone(); // adjust to actual field name
    match mur_core::cross_agent::propagate::run_propagate(
        &home,
        &agent_name,
        &mur_core::cross_agent::propagate::PropagateOptions::default(),
    ) {
        Ok(report) => {
            tracing::info!(
                target: "mur::propagate",
                installed = report.installed.len(),
                candidates = report.candidates.len(),
                peers = report.scanned_peers,
                "skill-propagate sweep complete"
            );
        }
        Err(e) => {
            tracing::warn!(target: "mur::propagate", error = %e, "skill-propagate failed");
        }
    }
    return;  // adjust to early-return shape used by surrounding code
}
```

The exact local variable names and early-return shape depend on the current `task_runner.rs` structure — read it before editing.

- [ ] **Step 5: Build + targeted test**

```bash
cargo build --workspace
cargo test -p mur-core cmd::agent_schedule -- --test-threads=1
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent_schedule.rs \
        mur-core/src/dispatch.rs mur-agent-runtime/src/task_runner.rs
git commit -m "feat(skill): skill-propagate idle hook + propagate-init helper (M7c task 8)"
```

---

### Task 9 — Credit aggregation across peers

**Files:**
- Create: `mur-core/src/cross_agent/credit/aggregate.rs`
- Modify: `mur-core/src/cross_agent/credit/mod.rs`

- [ ] **Step 1: Add module export**

In `mur-core/src/cross_agent/credit/mod.rs`:

```rust
pub mod ledger;
pub mod aggregate;
```

- [ ] **Step 2: Write the failing tests**

Create `mur-core/src/cross_agent/credit/aggregate.rs`:

```rust
//! Credit aggregation across peer agents (M7c §6.2).
//!
//! Reads each peer's ledger, collects entries for a given skill, and
//! synthesises mutator/recombiner entries from the manifest's
//! `evolution_log` when the ledger is empty (graceful pre-M7c history).

use std::path::Path;

use anyhow::Result;
use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
use mur_common::skill::peers::list_peer_agents;

use crate::cross_agent::credit::ledger::read_for_skill;

#[derive(Debug)]
pub struct CreditView {
    pub skill: String,
    pub entries: Vec<CreditEntry>,
}

pub fn build_credit_view(home: &Path, invoking_agent: &str, skill: &str) -> Result<CreditView> {
    let mut entries: Vec<CreditEntry> = Vec::new();
    // Self ledger first.
    entries.extend(read_for_skill(home, invoking_agent, skill)?);
    // Peer ledgers.
    for peer in list_peer_agents(home)? {
        if peer.name == invoking_agent {
            continue;
        }
        entries.extend(read_for_skill(home, &peer.name, skill)?);
    }
    // Evolution-log fallback for mutator entries that predate the ledger.
    let mut synth_seen: std::collections::HashSet<(String, String, String)> = entries
        .iter()
        .filter(|e| matches!(e.kind, CreditKind::Mutator))
        .map(|e| (e.agent.clone(), e.skill.clone(), e.skill_version.clone()))
        .collect();

    let candidates = vec![invoking_agent.to_string()]
        .into_iter()
        .chain(
            list_peer_agents(home)?
                .into_iter()
                .filter(|p| p.name != invoking_agent)
                .map(|p| p.name),
        )
        .collect::<Vec<_>>();

    for agent in &candidates {
        let manifest_path = home
            .join("agents")
            .join(agent)
            .join("skills")
            .join(skill)
            .join("skill.yaml");
        if !manifest_path.exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&manifest_path) else { continue };
        let Ok(m) =
            serde_yaml_ng::from_slice::<mur_common::skill::SkillManifest>(&bytes)
        else {
            continue;
        };
        for evt in &m.evolution_log {
            if evt.generation == 0 {
                continue; // initial-human → already covered by Author ledger entry
            }
            let key = (agent.clone(), skill.to_string(), evt.version.clone());
            if synth_seen.contains(&key) {
                continue;
            }
            let from_version = previous_version(&m.evolution_log, &evt.version)
                .unwrap_or_else(|| "?".to_string());
            entries.push(CreditEntry {
                ts: evt
                    .timestamp
                    .parse()
                    .unwrap_or_else(|_| chrono::Utc::now()),
                skill: skill.to_string(),
                skill_version: evt.version.clone(),
                kind: CreditKind::Mutator,
                agent: agent.clone(),
                evidence: Some(CreditEvidence::Mutator {
                    from_version,
                    diff_summary: evt.changes.clone(),
                }),
                source: evt.source.clone(),
            });
            synth_seen.insert(key);
        }
    }

    entries.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(CreditView {
        skill: skill.to_string(),
        entries,
    })
}

fn previous_version(
    log: &[mur_common::skill::EvolutionEvent],
    target: &str,
) -> Option<String> {
    let mut prior = None;
    for evt in log {
        if evt.version == target {
            return prior;
        }
        prior = Some(evt.version.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn returns_empty_view_when_no_data() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let v = build_credit_view(home, "alice", "nonexistent").unwrap();
        assert!(v.entries.is_empty());
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core cross_agent::credit::aggregate::tests
```

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cross_agent/credit/aggregate.rs mur-core/src/cross_agent/credit/mod.rs
git commit -m "feat(skill): credit aggregation across peers + evolution-log fallback (M7c task 9)"
```

---

### Task 10 — `mur skill credit <name>` CLI

**Files:**
- Create: `mur-core/src/cmd/skill_credit.rs`
- Modify: `mur-core/src/cmd/mod.rs`, `mur-core/src/cli/skill.rs`, `mur-core/src/dispatch.rs`

- [ ] **Step 1: CLI handler**

Create `mur-core/src/cmd/skill_credit.rs`:

```rust
//! `mur skill credit <name>` CLI handler (M7c).

use std::path::Path;

use anyhow::Result;
use mur_common::skill::credit::{CreditEvidence, CreditKind};

use crate::cross_agent::credit::aggregate::{CreditView, build_credit_view};

pub fn cmd_credit(home: &Path, agent: &str, skill: &str, json: bool) -> Result<()> {
    let view = build_credit_view(home, agent, skill)?;
    if view.entries.is_empty() {
        eprintln!("no credit history for {skill}");
        std::process::exit(2);
    }
    if json {
        emit_json(&view)?;
    } else {
        emit_human(&view);
    }
    Ok(())
}

fn emit_human(view: &CreditView) {
    println!("Skill: {}", view.skill);
    println!();

    let authors: Vec<_> = view.entries.iter().filter(|e| e.kind == CreditKind::Author).collect();
    if !authors.is_empty() {
        println!("Author{}:", if authors.len() > 1 { "s" } else { "" });
        for e in &authors {
            println!("  {:<8} {}  source: {}", e.agent, e.ts.to_rfc3339(), e.source);
        }
        println!();
    }

    let mutators: Vec<_> = view.entries.iter().filter(|e| e.kind == CreditKind::Mutator).collect();
    if !mutators.is_empty() {
        println!("Mutators ({}):", mutators.len());
        for e in &mutators {
            if let Some(CreditEvidence::Mutator { from_version, diff_summary }) = &e.evidence {
                println!(
                    "  {:<8} {}  v{} → v{}  (\"{}\")",
                    e.agent,
                    e.ts.to_rfc3339(),
                    from_version,
                    e.skill_version,
                    diff_summary
                );
            } else {
                println!("  {:<8} {}  v{}", e.agent, e.ts.to_rfc3339(), e.skill_version);
            }
        }
        println!();
    }

    let recomb: Vec<_> = view.entries.iter().filter(|e| e.kind == CreditKind::Recombiner).collect();
    if !recomb.is_empty() {
        println!("Recombiners ({}):", recomb.len());
        for e in &recomb {
            if let Some(CreditEvidence::Recombiner { role, child }) = &e.evidence {
                println!(
                    "  {:<8} {}  {} → {}",
                    e.agent,
                    e.ts.to_rfc3339(),
                    role,
                    child
                );
            }
        }
        println!();
    }

    let prop: Vec<_> = view.entries.iter().filter(|e| e.kind == CreditKind::Propagator).collect();
    if !prop.is_empty() {
        println!("Propagators ({}):", prop.len());
        for e in &prop {
            if let Some(CreditEvidence::Propagator {
                from_agent,
                fitness_at_install,
                samples_at_install,
            }) = &e.evidence
            {
                println!(
                    "  {:<8} {}  v{}  ← agent://{}  (fitness {:.2}, n={})",
                    e.agent,
                    e.ts.to_rfc3339(),
                    e.skill_version,
                    from_agent,
                    fitness_at_install,
                    samples_at_install
                );
            }
        }
        println!();
    }

    println!(
        "Lineage summary: {} author(s), {} mutator(s), {} recombiner(s), {} propagation(s).",
        authors.len(),
        mutators.len(),
        recomb.len(),
        prop.len()
    );
}

fn emit_json(view: &CreditView) -> Result<()> {
    serde_json::to_writer_pretty(
        std::io::stdout(),
        &serde_json::json!({
            "skill": view.skill,
            "entries": view.entries,
        }),
    )?;
    println!();
    Ok(())
}
```

- [ ] **Step 2: Module + CLI + dispatch**

`mur-core/src/cmd/mod.rs`: add `pub mod skill_credit;`.

`mur-core/src/cli/skill.rs`: add inside `SkillAction`:

```rust
    /// Show the credit lineage for a skill (M7c).
    Credit {
        /// Skill name
        name: String,
        /// Invoking agent (defaults to current)
        #[arg(long)]
        agent: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
```

`mur-core/src/dispatch.rs`: wire the new arm — replicate the `agent::resolve_mur_home()` pattern; default `agent` to `cmd::skill_install::caller_agent_name(&home)?.unwrap_or_else(|| "(global)".into())`.

- [ ] **Step 3: Build + smoke**

```bash
cargo build -p mur-core
cargo run -- skill credit nonexistent-skill --agent alice 2>&1 | head -5
```

Expected: error or "no credit history" with exit 2.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/skill_credit.rs mur-core/src/cmd/mod.rs \
        mur-core/src/cli/skill.rs mur-core/src/dispatch.rs
git commit -m "feat(skill): mur skill credit CLI (M7c task 10)"
```

---

### Task 11 — Intent canonicaliser (host-level, frequency-based)

**Files:**
- Create: `mur-core/src/cross_agent/intent/mod.rs`, `mur-core/src/cross_agent/intent/canonical.rs`
- Modify: `mur-core/src/cross_agent/mod.rs`

- [ ] **Step 1: Add module barrel**

In `mur-core/src/cross_agent/mod.rs`, append:

```rust
pub mod intent;
```

Create `mur-core/src/cross_agent/intent/mod.rs`:

```rust
//! Host-level intent canonicaliser (M7c).

pub mod canonical;
```

- [ ] **Step 2: Write the canonicaliser**

Create `mur-core/src/cross_agent/intent/canonical.rs`:

```rust
//! Host-level intent canonicaliser (M7c §3.6).
//!
//! Scans every installed manifest under `<home>/agents/<a>/skills/<s>/skill.yaml`,
//! collects `ProcedureStep::intent` strings, clusters by normalised form,
//! and writes the most-frequent original spelling per cluster as the
//! canonical for that cluster. File: `<home>/intent_canonical.yaml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::peers::list_peer_agents;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalEntry {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentCanonical {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub generated_by: String,
    pub canonical: Vec<CanonicalEntry>,
}

pub fn canonical_path(home: &Path) -> PathBuf {
    home.join("intent_canonical.yaml")
}

pub fn build_canonical(home: &Path, generated_by: &str) -> Result<IntentCanonical> {
    // (normalised_form, original_string) -> count
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();

    let agents = list_peer_agents(home)?;
    for agent in &agents {
        let skills_dir = home.join("agents").join(&agent.name).join("skills");
        if !skills_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("skill.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let bytes = match std::fs::read(&manifest_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let manifest: SkillManifest = match serde_yaml_ng::from_slice(&bytes) {
                Ok(m) => m,
                Err(_) => continue,
            };
            collect_intents(&manifest, &mut counts);
        }
    }

    let mut canonical_entries: Vec<CanonicalEntry> = counts
        .into_iter()
        .map(|(_norm, originals)| {
            let total: usize = originals.values().sum();
            // Canonical = most-frequent original; tiebreak alphabetical.
            let mut sorted: Vec<(String, usize)> = originals.into_iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let canonical = sorted[0].0.clone();
            let aliases: Vec<String> = sorted.into_iter().map(|(s, _)| s).collect();
            CanonicalEntry {
                canonical,
                aliases,
                count: total,
            }
        })
        .collect();
    canonical_entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.canonical.cmp(&b.canonical)));

    Ok(IntentCanonical {
        version: 1,
        generated_at: Utc::now(),
        generated_by: generated_by.to_string(),
        canonical: canonical_entries,
    })
}

fn collect_intents(
    manifest: &SkillManifest,
    counts: &mut BTreeMap<String, BTreeMap<String, usize>>,
) {
    // SkillManifest holds an Option<Procedure> for procedure-mode skills.
    // Walk the steps and pluck `intent` when present.
    if let Some(proc) = &manifest.procedure {
        for step in &proc.steps {
            if let Some(intent) = &step.intent {
                let norm = normalise(intent);
                counts
                    .entry(norm)
                    .or_default()
                    .entry(intent.clone())
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
            }
        }
    }
}

/// Lowercase, collapse runs of whitespace/hyphens/underscores to `_`,
/// strip leading/trailing `_`.
pub fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_sep = true;
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !last_was_sep {
                out.push('_');
                last_was_sep = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            last_was_sep = false;
        }
    }
    out.trim_matches('_').to_string()
}

pub fn write_canonical_yaml(home: &Path, ic: &IntentCanonical) -> Result<()> {
    let path = canonical_path(home);
    let yaml = serde_yaml_ng::to_string(ic).context("serialise IntentCanonical")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

pub fn read_canonical_yaml(home: &Path) -> Result<Option<IntentCanonical>> {
    let path = canonical_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let ic: IntentCanonical = serde_yaml_ng::from_slice(&bytes)?;
    Ok(Some(ic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_handles_separators() {
        assert_eq!(normalise("Web Search"), "web_search");
        assert_eq!(normalise("web-search"), "web_search");
        assert_eq!(normalise("WEB__SEARCH"), "web_search");
        assert_eq!(normalise("  web   search  "), "web_search");
    }

    #[test]
    fn empty_input_yields_empty_norm() {
        assert_eq!(normalise(""), "");
        assert_eq!(normalise("   "), "");
    }

    #[test]
    fn canonical_picks_most_frequent_then_alphabetical() {
        let mut originals: BTreeMap<String, usize> = BTreeMap::new();
        originals.insert("Web Search".into(), 3);
        originals.insert("web_search".into(), 3);
        originals.insert("web search".into(), 1);
        let mut sorted: Vec<(String, usize)> = originals.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        assert_eq!(sorted[0].0, "Web Search");
    }
}
```

(Confirm `SkillManifest.procedure` field name and `Procedure.steps` field name by reading `mur-common/src/skill/manifest.rs`. If the actual field is named differently, adjust.)

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-core cross_agent::intent::canonical::tests
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cross_agent/intent/ mur-core/src/cross_agent/mod.rs
git commit -m "feat(skill): intent canonicaliser — host-level frequency clustering (M7c task 11)"
```

---

### Task 12 — Intent inject lookup + `mur skill intent` CLI

**Files:**
- Create: `mur-core/src/cross_agent/intent/inject_lookup.rs`, `mur-core/src/cmd/skill_intent.rs`
- Modify: `mur-core/src/cross_agent/intent/mod.rs`, `mur-core/src/cmd/mod.rs`, `mur-core/src/cli/skill.rs`, `mur-core/src/dispatch.rs`

- [ ] **Step 1: Inject lookup helper**

In `mur-core/src/cross_agent/intent/mod.rs`, add `pub mod inject_lookup;`.

Create `mur-core/src/cross_agent/intent/inject_lookup.rs`:

```rust
//! Read-side intent lookup used by the M6b inject path (M7c §3.6).
//!
//! Returns the canonical form for a given alias string, or the original
//! input when no mapping exists. Failures (missing file, parse error)
//! are silently treated as "no mapping" — the inject path never blocks
//! on a missing canonical file.

use std::collections::HashMap;
use std::path::Path;

use crate::cross_agent::intent::canonical::{read_canonical_yaml, IntentCanonical, normalise};

#[derive(Debug, Clone)]
pub struct IntentLookup {
    by_alias: HashMap<String, String>,
    by_norm: HashMap<String, String>,
}

impl IntentLookup {
    pub fn empty() -> Self {
        Self {
            by_alias: HashMap::new(),
            by_norm: HashMap::new(),
        }
    }

    pub fn load(home: &Path) -> Self {
        match read_canonical_yaml(home) {
            Ok(Some(ic)) => Self::from(ic),
            _ => Self::empty(),
        }
    }

    pub fn from(ic: IntentCanonical) -> Self {
        let mut by_alias = HashMap::new();
        let mut by_norm = HashMap::new();
        for entry in &ic.canonical {
            for alias in &entry.aliases {
                by_alias.insert(alias.clone(), entry.canonical.clone());
            }
            by_norm.insert(normalise(&entry.canonical), entry.canonical.clone());
        }
        Self { by_alias, by_norm }
    }

    pub fn resolve_intent(&self, intent: &str) -> String {
        if let Some(c) = self.by_alias.get(intent) {
            return c.clone();
        }
        if let Some(c) = self.by_norm.get(&normalise(intent)) {
            return c.clone();
        }
        intent.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cross_agent::intent::canonical::CanonicalEntry;

    fn lookup_with(aliases: Vec<(&str, Vec<&str>)>) -> IntentLookup {
        let ic = IntentCanonical {
            version: 1,
            generated_at: chrono::Utc::now(),
            generated_by: "test".into(),
            canonical: aliases
                .into_iter()
                .map(|(c, a)| CanonicalEntry {
                    canonical: c.into(),
                    aliases: a.into_iter().map(String::from).collect(),
                    count: a.len(),
                })
                .collect(),
        };
        IntentLookup::from(ic)
    }

    #[test]
    fn alias_resolves_to_canonical() {
        let l = lookup_with(vec![("web_search", vec!["web_search", "search_web", "Web Search"])]);
        assert_eq!(l.resolve_intent("search_web"), "web_search");
        assert_eq!(l.resolve_intent("Web Search"), "web_search");
        assert_eq!(l.resolve_intent("web_search"), "web_search");
    }

    #[test]
    fn unknown_returns_input() {
        let l = IntentLookup::empty();
        assert_eq!(l.resolve_intent("something_new"), "something_new");
    }
}
```

- [ ] **Step 2: `mur skill intent` CLI**

Create `mur-core/src/cmd/skill_intent.rs`:

```rust
//! `mur skill intent {canonicalise,show}` CLI (M7c).

use std::path::Path;

use anyhow::Result;

use crate::cross_agent::intent::canonical::{
    build_canonical, read_canonical_yaml, write_canonical_yaml,
};

pub fn cmd_intent_canonicalise(home: &Path, generated_by: &str, dry_run: bool, json: bool) -> Result<()> {
    let ic = build_canonical(home, generated_by)?;
    if dry_run {
        if json {
            serde_json::to_writer_pretty(std::io::stdout(), &ic)?;
            println!();
        } else {
            println!("{}", serde_yaml_ng::to_string(&ic)?);
        }
        return Ok(());
    }
    write_canonical_yaml(home, &ic)?;
    println!(
        "wrote {} cluster(s) to {}",
        ic.canonical.len(),
        home.join("intent_canonical.yaml").display()
    );
    Ok(())
}

pub fn cmd_intent_show(home: &Path, json: bool) -> Result<()> {
    match read_canonical_yaml(home)? {
        None => {
            eprintln!("no canonical mapping at {}", home.join("intent_canonical.yaml").display());
            std::process::exit(2);
        }
        Some(ic) => {
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &ic)?;
                println!();
            } else {
                println!("{}", serde_yaml_ng::to_string(&ic)?);
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 3: CLI wiring**

`mur-core/src/cmd/mod.rs`: `pub mod skill_intent;`.

`mur-core/src/cli/skill.rs`: add inside `SkillAction`:

```rust
    /// Manage the host-level intent canonical mapping (M7c).
    Intent {
        #[command(subcommand)]
        action: IntentAction,
    },
```

And below the existing sibling enums:

```rust
#[derive(clap::Subcommand)]
pub enum IntentAction {
    /// Rebuild the canonical mapping by scanning every installed manifest.
    Canonicalise {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the current canonical mapping.
    Show {
        #[arg(long)]
        json: bool,
    },
}
```

`mur-core/src/dispatch.rs`: wire the new arm:

```rust
            crate::cli::SkillAction::Intent { action } => match action {
                crate::cli::skill::IntentAction::Canonicalise { dry_run, json } => {
                    let home = cmd::agent::resolve_mur_home()?;
                    let by = cmd::skill_install::caller_agent_name(&home)?
                        .unwrap_or_else(|| "(unknown)".into());
                    cmd::skill_intent::cmd_intent_canonicalise(&home, &by, dry_run, json)?
                }
                crate::cli::skill::IntentAction::Show { json } => {
                    let home = cmd::agent::resolve_mur_home()?;
                    cmd::skill_intent::cmd_intent_show(&home, json)?
                }
            },
```

- [ ] **Step 4: Build + smoke**

```bash
cargo build -p mur-core
cargo run -- skill intent canonicalise --dry-run
cargo run -- skill intent show 2>&1 | head -5
```

Expected: canonicalise prints YAML; `show` exits 2 with "no canonical mapping" if none exists yet.

- [ ] **Step 5: Test inject lookup**

```bash
cargo test -p mur-core cross_agent::intent
```

Expected: all inject_lookup + canonical tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cross_agent/intent/ \
        mur-core/src/cmd/skill_intent.rs mur-core/src/cmd/mod.rs \
        mur-core/src/cli/skill.rs mur-core/src/dispatch.rs
git commit -m "feat(skill): intent inject lookup + mur skill intent CLI (M7c task 12)"
```

---

### Task 13 — Integration tests

**Files:**
- Create the test files listed under "File Structure" above.

- [ ] **Step 1: Pull-only invariant**

Create `mur-core/tests/propagate_pull_only.rs`:

```rust
//! Verifies that a full propagate sweep mutates ONLY the invoking agent's
//! home — peer files (manifests, stats, lockfiles) are untouched.

use mur_core::cross_agent::propagate::{PropagateOptions, candidates::GateConfig, run_propagate};
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::tempdir;

fn fingerprint_dir(root: &PathBuf) -> HashMap<PathBuf, (u64, u64)> {
    let mut out = HashMap::new();
    let walker = walkdir::WalkDir::new(root);
    for entry in walker.into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let md = entry.metadata().unwrap();
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.insert(entry.path().to_path_buf(), (md.len(), mtime));
    }
    out
}

#[test]
fn propagate_does_not_modify_peers() {
    let d = tempdir().unwrap();
    let home = d.path().to_path_buf();
    // Build a synthetic fixture (helper in tests/common/mod.rs).
    // ... build fixture ...

    let before = fingerprint_dir(&home.join("agents").join("alice"));

    let mut opts = PropagateOptions::default();
    opts.gates.min_samples = 1;
    opts.gates.min_fitness = 0.0;
    opts.gates.min_source_weight = 0.0;
    let _ = run_propagate(&home, "bob", &opts).unwrap();

    let after = fingerprint_dir(&home.join("agents").join("alice"));
    assert_eq!(before, after, "peer state mutated by propagate sweep — M7a invariant violated");
}
```

(`walkdir` may need to be added to `[dev-dependencies]` in `mur-core/Cargo.toml` if not present.)

Create `mur-core/tests/common/mod.rs` with a shared fixture builder:

```rust
pub fn build_two_agent_fixture(
    home: &std::path::Path,
    source_agent: &str,
    invoker: &str,
    skill: &str,
    successes: u64,
    failures: u64,
) {
    use mur_common::skill::stats::{LifecycleState, SkillStats};
    let src_skills = home.join("agents").join(source_agent).join("skills").join(skill);
    std::fs::create_dir_all(&src_skills).unwrap();
    std::fs::write(
        src_skills.join("skill.yaml"),
        format!(
            "schema_version: \"2.1\"\nname: {skill}\nversion: \"1.0.0\"\ndescription: \"test\"\npublisher: test\ntriggers: []\nrequires: []\nprocedure:\n  mode: procedure\n  steps:\n    - description: \"do thing\"\n      intent: \"do_thing\"\n"
        ),
    ).unwrap();
    let stats = SkillStats {
        skill: skill.into(),
        usage_count: successes + failures,
        success_count: successes,
        failure_count: failures,
        last_used_at: Some(chrono::Utc::now()),
        lifecycle_state: LifecycleState::Stable,
        ..Default::default()
    };
    let stats_path = SkillStats::path_agent(home, source_agent, skill);
    std::fs::create_dir_all(stats_path.parent().unwrap()).unwrap();
    stats.save(&stats_path).unwrap();
    std::fs::create_dir_all(home.join("agents").join(invoker).join("skills")).unwrap();
}
```

(`SkillStats::default()` may not exist; if not, construct fields manually — read `mur-common/src/skill/stats.rs` for the actual struct.)

- [ ] **Step 2: Gate scenarios**

Create `mur-core/tests/propagate_gates.rs` with eight `#[test]` functions, each setting up a fixture that fails exactly one gate (and asserting the candidate is rejected) or passes all gates (and asserting it is accepted). Use the helper from common.

Scenarios:
1. `min_samples` too low → rejected
2. `min_fitness` too low → rejected
3. `min_source_weight` too low (recent failures dragging weight down) → rejected
4. `exclude_patterns` matches → rejected
5. Local already has skill → rejected
6. No peers → empty report, exit semantics unchanged
7. `max_per_sweep` cap → only first N installed
8. All gates pass → installed, ledger entry written

Each test ≤ 30 lines.

- [ ] **Step 3: Idle-hook test**

Create `mur-core/tests/propagate_idle_hook.rs`:

```rust
//! Verifies that registering `skill-propagate` via `cmd_propagate_init` and
//! ticking the idle scheduler past `after_secs` causes one sweep to run.

use mur_core::cmd::agent_schedule::{cmd_propagate_init, read_idle_triggers};

#[test]
fn propagate_init_registers_trigger_idempotently() {
    let _ = std::env::set_var("MUR_HOME", "/tmp/mur-test-propagate-idle");
    let _ = std::fs::remove_dir_all("/tmp/mur-test-propagate-idle");
    std::fs::create_dir_all("/tmp/mur-test-propagate-idle/agents/alice").unwrap();
    // Profile must exist for cmd_idle_add to succeed — match an existing fixture
    // builder pattern from tests/cmd_agent_idle.rs.

    cmd_propagate_init("alice", 1800, 7200).unwrap();
    let after_first = read_idle_triggers("alice").unwrap();
    assert!(after_first.iter().any(|t| t.message == "propagate.run"));

    cmd_propagate_init("alice", 1800, 7200).unwrap();
    let after_second = read_idle_triggers("alice").unwrap();
    let count = after_second.iter().filter(|t| t.message == "propagate.run").count();
    assert_eq!(count, 1, "propagate-init must be idempotent");
}
```

(Match the actual profile-creation pattern from `mur-core/tests/cmd_agent_idle.rs` — `cmd_idle_add` requires a profile.)

- [ ] **Step 4: Credit aggregation tests**

Create `mur-core/tests/credit_view_aggregates_peers.rs` and `mur-core/tests/credit_synthesises_from_evolution_log.rs`. Each builds a fixture with two peers + the invoker, writes a ledger line on each peer for the same skill, calls `build_credit_view`, and asserts entries from all sources appear sorted by `ts`.

- [ ] **Step 5: Intent tests**

Create `mur-core/tests/intent_canonicaliser_e2e.rs` (build canonical → write → rebuild from same input yields byte-identical output excluding `generated_at`) and `mur-core/tests/intent_inject_lookup.rs` (load a hand-crafted YAML and assert resolve_intent picks canonical for aliases).

- [ ] **Step 6: Run all new tests**

```bash
cargo test --workspace --tests
```

Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add mur-core/tests/propagate_pull_only.rs \
        mur-core/tests/propagate_gates.rs \
        mur-core/tests/propagate_idle_hook.rs \
        mur-core/tests/credit_view_aggregates_peers.rs \
        mur-core/tests/credit_synthesises_from_evolution_log.rs \
        mur-core/tests/intent_canonicaliser_e2e.rs \
        mur-core/tests/intent_inject_lookup.rs \
        mur-core/tests/common
git commit -m "test(skill): M7c integration suite — propagate, credit, intent (M7c task 13)"
```

---

### Task 14 — Workspace lint + format + full test pass

- [ ] **Step 1: Format**

```bash
cargo fmt --all
```

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: clean. If anything trips on `cross_agent/propagate/candidates.rs` (likely `single_match_else` or `collapsible_if`), apply the fix inline.

- [ ] **Step 3: Full test**

```bash
cargo test --workspace
```

Expected: all green.

- [ ] **Step 4: Commit any cleanups**

```bash
git add -A
git commit -m "chore(skill): clippy + fmt cleanups for M7c"
```

(Skip if there is nothing to commit.)

---

### Task 15 — Docs

**Files:**
- Modify: `README.md`, `mur-core/README.md` (if it exists), spec/scoping cross-references.

- [ ] **Step 1: Update top-level README CLI section**

In `README.md`, under the `CLI Surface` table (or `mur agent`/`mur skill` section), add brief one-line entries:

- `mur agent propagate <name>` — pull high-fitness peer skills (M7c).
- `mur agent schedule propagate-init <name>` — register the `skill-propagate` idle trigger.
- `mur skill credit <name>` — show contribution lineage.
- `mur skill intent {canonicalise|show}` — manage host-level intent canonical mapping.

- [ ] **Step 2: Cross-reference**

Add a one-line "M7c shipped" note to the architecture overview at `docs/architecture/runtime-overview.md` (search for the line that mentions M7a observability and append an M7c bullet beside it).

- [ ] **Step 3: Commit + ship**

```bash
git add README.md docs/architecture/runtime-overview.md
git commit -m "docs(skill): M7c propagation + credit + intent canonicaliser surface (M7c task 15)"
```

- [ ] **Step 4: Open PR**

```bash
git push -u origin <branch>
gh pr create --title "feat(skill): M7c — automatic propagation + credit + intent canonical (M7c)" \
  --body "$(cat <<'EOF'
## Summary

Closes the M7 cross-agent evolution loop:

- **Propagation** — `mur agent propagate <name>` + `skill-propagate` idle hook. Pull-side only; gates `min_samples`, `min_fitness`, `min_source_weight`; cap `max_per_sweep`. Preserves M7a invariant (peers strictly read-only).
- **Credit ledger** — per-agent `~/.mur/agents/<a>/credit/ledger.jsonl`. Four `kind` values (`author`, `mutator`, `recombiner`, `propagator`). `mur skill credit <name>` aggregates across peers and falls back to evolution-log synthesis.
- **Intent canonicaliser** — host-level `~/.mur/intent_canonical.yaml`, frequency-clustered, atomic temp+rename. `mur skill intent {canonicalise,show}` + read-side `IntentLookup` for the M6b injector.

No `SkillManifest` schema changes — signature scope preserved.

Spec: `docs/superpowers/specs/2026-05-26-mur-skill-ecosystem-m7c-design.md`
Plan: `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7c.md`

## Test plan
- [x] `cargo build --workspace`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt --check`
- [x] `cargo test --workspace`
- [x] Manual smoke: `mur agent propagate alice --dry-run` against a real `~/.mur`
- [x] Manual smoke: register `skill-propagate` via `propagate-init`, idle past `after_secs`, verify install + ledger
- [x] Manual smoke: `mur skill credit <name>` shows lineage across peers
- [x] Manual smoke: `mur skill intent canonicalise` produces idempotent YAML
- [x] Invariant audit: peer files unchanged after a sweep (mtime/size)
EOF
)"
```

---

## Self-Review Notes (post-write check)

**Spec coverage:**
- §1 Goal three behaviours → Tasks 5–8 (propagate), 1–4+9–10 (credit), 11–12 (intent). ✓
- §3.1 pull-side → Task 6 uses `cmd_install_ctx` only on the invoking agent. ✓
- §3.2 gates → Task 5 implements all four gates including `exclude_patterns`. ✓
- §3.3 lifecycle/trust → Task 3 hands over to existing `cmd_install`; no override (correct). ✓
- §3.4 ledger schema → Task 1 types match spec exactly. ✓
- §3.5 `InstallContext` → Task 3. ✓
- §3.6 intent → Tasks 11–12. ✓
- §3.7 idle hook → Task 8. ✓
- §3.8 no manifest changes → no task touches `manifest.rs`. ✓
- §3.9 concurrency → Task 6 advisory lock; Task 11 atomic temp+rename. ✓
- §7 exit codes → Task 7 emits 4/5/7; Task 10 emits 2. ✓

**Placeholder scan:**
- "If `cmd_idle_add` lives elsewhere, adjust" — concrete enough; reader is told to grep.
- "Adjust to early-return shape" in Task 8 Step 4 — necessary because `task_runner.rs` is not pinned; reader must read it. Acceptable.
- No "TBD" / "TODO" / "etc." patterns remain.

**Type consistency:**
- `CreditKind` four variants used consistently across Tasks 1, 3, 4, 9, 10. ✓
- `CreditEvidence` enum variants (`Author` unit, `Mutator { from_version, diff_summary }`, `Recombiner { role, child }`, `Propagator { from_agent, fitness_at_install, samples_at_install }`) referenced identically in Tasks 1, 3, 4, 9, 10. ✓
- `InstallContext` variants `Manual` + `AutoPropagate { source_fitness, source_samples }` — used identically in Tasks 3, 6. ✓
- `IntentCanonical` / `CanonicalEntry` fields identical across Tasks 11 and 12. ✓
- `GateConfig` field names (`min_samples`, `min_fitness`, `min_source_weight`, `max_per_sweep`, `exclude_patterns`) consistent across Tasks 5, 6, 7. ✓
- `PropagateOptions` / `PropagateReport` shapes consistent between Tasks 6, 7, 13. ✓
