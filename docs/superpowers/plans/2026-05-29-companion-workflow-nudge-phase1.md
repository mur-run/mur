# Companion Workflow Nudge — Phase 1 (Core Engine + CLI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the surface-agnostic core of the workflow nudge — emergence-mined candidates filtered through an anti-nag ledger, surfaced at session-end, accepted/dismissed via the CLI, with accept creating a draft workflow — so the full nudge loop works headlessly before any companion GUI work.

**Architecture:** A new `mur-core/src/nudge/` module turns `EmergentCandidate`s (from the existing `capture/emergence.rs`) into stable-id `WorkflowCandidate`s, persists their lifecycle in a JSON ledger (`~/.mur/nudges.json`) that enforces dedup / snooze / a daily cap, and on accept reuses the existing draft-workflow creation path. `mur session stop` computes and records actionable nudges; `mur suggest` lists them and `--accept`/`--dismiss` act on them.

**Tech Stack:** Rust 2024 (`mur-core`, `mur-common`), `serde`/`serde_json`, `sha2`, `chrono`, `clap`.

**Scope boundary:** Phase 1 = spec units 1 (CandidateSource), 2 (NudgeLedger), 3 (NudgeEmitter), 5 (CLI fallback). **Phase 2** (spec unit 4 — the companion speech-bubble surface: `Situation::WorkflowNudge`, i18n templates, outbox wiring, routing the `good`/`dismiss` `BridgeResponse` back to `apply_decision`) is a **separate plan**, because the `mur-agent-runtime` companion outbox is a mature orchestrator (generate/deliver/picker/passive-dismiss/rhythm) that warrants focused integration. Phase 1 delivers the entire loop via CLI; Phase 2 adds the proactive bubble on top of the same ledger.

**Spec:** `docs/superpowers/specs/2026-05-29-companion-workflow-nudge-design.md`

---

## File Structure

- Modify `mur-common/src/config.rs` — add `NudgeConfig` + `Config.nudge` field (follow the `SleepCycleConfig` pattern).
- Create `mur-core/src/nudge/mod.rs` — module root, re-exports.
- Create `mur-core/src/nudge/candidate.rs` — `WorkflowCandidate`, `CandidateSource` trait, `EmergenceSource`.
- Create `mur-core/src/nudge/ledger.rs` — `NudgeState`, `NudgeRecord`, `NudgeLedger` (load/save/filter/transition).
- Create `mur-core/src/nudge/emitter.rs` — `NudgeEmitter` (emit_pending, apply_decision, `NudgeDecision`).
- Modify `mur-core/src/lib.rs` (or `mur-core/src/cmd/mod.rs`) — register `pub mod nudge;`.
- Modify `mur-core/src/cmd/workflow.rs` — extract `create_draft_workflow(...)` from `cmd_suggest`; reuse it.
- Modify `mur-core/src/cli/mod.rs` — extend `Suggest` with `accept: Option<String>`, `dismiss: Option<String>`.
- Modify `mur-core/src/cmd/session.rs` — hook the emitter after `detect_emergent` in `cmd_session_stop`.

---

## Task 1: `NudgeConfig` (mur-common)

**Files:**
- Modify: `mur-common/src/config.rs`
- Test: `mur-common/src/config.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the test module in `mur-common/src/config.rs`:

```rust
#[test]
fn nudge_config_defaults() {
    let c = NudgeConfig::default();
    assert!(!c.enabled);          // opt-in until Phase 2 surface exists
    assert_eq!(c.daily_cap, 3);
    assert_eq!(c.snooze_days, 7);
    assert_eq!(c.threshold, 3);
}

#[test]
fn config_has_nudge_section_with_defaults() {
    // An empty config YAML still yields a usable nudge section via serde defaults.
    let c: Config = serde_yaml_ng::from_str("{}").unwrap();
    assert_eq!(c.nudge.daily_cap, 3);
}
```

(Confirm the crate's YAML import name — the loader uses `serde_yaml_ng`; match it.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common nudge_config`
Expected: FAIL — `cannot find type NudgeConfig`.

- [ ] **Step 3: Implement**

In `mur-common/src/config.rs`, add the field to `Config`:

```rust
    #[serde(default)]
    pub nudge: NudgeConfig,
```

and define (near `SleepCycleConfig`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeConfig {
    /// Master switch. Default off; Phase 2 (companion surface) flips the default.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_nudge_daily_cap")]
    pub daily_cap: u32,
    #[serde(default = "default_nudge_snooze_days")]
    pub snooze_days: u32,
    #[serde(default = "default_nudge_threshold")]
    pub threshold: usize,
}

fn default_nudge_daily_cap() -> u32 { 3 }
fn default_nudge_snooze_days() -> u32 { 7 }
fn default_nudge_threshold() -> usize { 3 }

impl Default for NudgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_cap: default_nudge_daily_cap(),
            snooze_days: default_nudge_snooze_days(),
            threshold: default_nudge_threshold(),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common nudge_config config_has_nudge_section`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(nudge): NudgeConfig (daily_cap/snooze_days/threshold)"
```

---

## Task 2: `WorkflowCandidate` + `CandidateSource` + `EmergenceSource` (mur-core)

**Files:**
- Create: `mur-core/src/nudge/mod.rs`
- Create: `mur-core/src/nudge/candidate.rs`
- Modify: `mur-core/src/lib.rs` (add `pub mod nudge;`)
- Test: `mur-core/src/nudge/candidate.rs` (inline tests)

> Grounding: `EmergentCandidate { behavior, keywords, session_count, session_ids, evidence, suggested_name, suggested_content }` and `detect_emergent(&[BehaviorFingerprint], threshold) -> Vec<EmergentCandidate>` / `load_fingerprints() -> Result<Vec<BehaviorFingerprint>>` are in `mur-core/src/capture/emergence.rs`.

- [ ] **Step 1: Create the module root**

`mur-core/src/nudge/mod.rs`:

```rust
//! Workflow nudge engine: turn emergence-mined recurring behavior into
//! actionable "save this as a workflow?" prompts (surface-agnostic).
pub mod candidate;
pub mod ledger;
pub mod emitter;

pub use candidate::{CandidateSource, EmergenceSource, WorkflowCandidate};
pub use emitter::{NudgeDecision, NudgeEmitter};
pub use ledger::{NudgeLedger, NudgeRecord, NudgeState};
```

Add `pub mod nudge;` to `mur-core/src/lib.rs` (alongside the other `pub mod` declarations).

- [ ] **Step 2: Write the failing test**

`mur-core/src/nudge/candidate.rs` (start with the test):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::emergence::EmergentCandidate;

    fn ec(behavior: &str, kw: &[&str]) -> EmergentCandidate {
        EmergentCandidate {
            behavior: behavior.into(),
            keywords: kw.iter().map(|s| s.to_string()).collect(),
            session_count: 3,
            session_ids: vec!["s1".into(), "s2".into(), "s3".into()],
            evidence: vec!["ran tests".into(), "committed".into()],
            suggested_name: "test-then-commit".into(),
            suggested_content: "…".into(),
        }
    }

    #[test]
    fn id_is_stable_and_order_independent() {
        let a = WorkflowCandidate::from_emergent(&ec("b", &["test", "commit"]));
        let b = WorkflowCandidate::from_emergent(&ec("b", &["commit", "test"]));
        assert_eq!(a.id, b.id); // keyword order must not change the id
        assert_eq!(a.session_count, 3);
        assert_eq!(a.suggested_name, "test-then-commit");
    }

    #[test]
    fn emergence_source_maps_candidates() {
        let src = EmergenceSource::from_fingerprints(vec![]); // empty → no candidates
        assert!(src.candidates(3).unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::candidate`
Expected: FAIL — `WorkflowCandidate` undefined.

- [ ] **Step 4: Implement**

Prepend to `mur-core/src/nudge/candidate.rs`:

```rust
use crate::capture::emergence::{detect_emergent, EmergentCandidate};
use mur_common::event::BehaviorFingerprint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowCandidate {
    pub id: String,
    pub title: String,
    pub suggested_name: String,
    pub steps_preview: Vec<String>,
    pub session_count: usize,
    pub evidence_session_ids: Vec<String>,
}

impl WorkflowCandidate {
    pub fn from_emergent(e: &EmergentCandidate) -> Self {
        let mut kw = e.keywords.clone();
        kw.sort();
        let mut h = Sha256::new();
        h.update(e.behavior.as_bytes());
        h.update([0]);
        h.update(kw.join(",").as_bytes());
        let id = format!("{:x}", h.finalize());
        Self {
            id,
            title: e.behavior.clone(),
            suggested_name: e.suggested_name.clone(),
            steps_preview: e.evidence.clone(),
            session_count: e.session_count,
            evidence_session_ids: e.session_ids.clone(),
        }
    }
}

/// A source of workflow candidates. v1 has one impl (emergence); co-occurrence
/// is added post-migration without changing consumers.
pub trait CandidateSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>>;
}

pub struct EmergenceSource {
    fingerprints: Vec<BehaviorFingerprint>,
}

impl EmergenceSource {
    pub fn from_fingerprints(fingerprints: Vec<BehaviorFingerprint>) -> Self {
        Self { fingerprints }
    }
    /// Load all persisted fingerprints (~/.mur/fingerprints.jsonl).
    pub fn from_disk() -> anyhow::Result<Self> {
        Ok(Self { fingerprints: crate::capture::emergence::load_fingerprints()? })
    }
}

impl CandidateSource for EmergenceSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>> {
        Ok(detect_emergent(&self.fingerprints, threshold)
            .iter()
            .map(WorkflowCandidate::from_emergent)
            .collect())
    }
}
```

Confirm `sha2` is a `mur-core` dependency (it is used elsewhere; if not, add to `mur-core/Cargo.toml`).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::candidate`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/nudge/mod.rs mur-core/src/nudge/candidate.rs mur-core/src/lib.rs mur-core/Cargo.toml
git commit -m "feat(nudge): WorkflowCandidate + emergence CandidateSource"
```

---

## Task 3: `NudgeLedger` (mur-core)

**Files:**
- Create: `mur-core/src/nudge/ledger.rs`
- Test: `mur-core/src/nudge/ledger.rs` (inline tests)

> The ledger is the single source of truth for nudge state AND the candidate snapshot (so accept can rebuild the draft later). File: `~/.mur/nudges.json`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;
    use chrono::{Duration, Utc};

    fn cand(id: &str) -> WorkflowCandidate {
        WorkflowCandidate {
            id: id.into(), title: "t".into(), suggested_name: "n".into(),
            steps_preview: vec![], session_count: 3, evidence_session_ids: vec![],
        }
    }

    #[test]
    fn dismissed_never_resurfaces() {
        let mut l = NudgeLedger::default();
        l.set_state("a", NudgeState::Dismissed, Utc::now());
        let actionable = l.filter_actionable(&[cand("a"), cand("b")], Utc::now(), 10);
        let ids: Vec<_> = actionable.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]); // "a" dismissed → excluded
    }

    #[test]
    fn snooze_hides_until_expiry() {
        let mut l = NudgeLedger::default();
        let now = Utc::now();
        l.set_state("a", NudgeState::Snoozed { until: (now + Duration::days(3)).to_rfc3339() }, now);
        assert!(l.filter_actionable(&[cand("a")], now, 10).is_empty());
        let later = now + Duration::days(4);
        assert_eq!(l.filter_actionable(&[cand("a")], later, 10).len(), 1);
    }

    #[test]
    fn daily_cap_limits_new_surfaces() {
        let l = NudgeLedger::default();
        let now = Utc::now();
        let out = l.filter_actionable(&[cand("a"), cand("b"), cand("c")], now, 2);
        assert_eq!(out.len(), 2); // cap = 2
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::ledger`
Expected: FAIL — `NudgeLedger` undefined.

- [ ] **Step 3: Implement**

```rust
use crate::nudge::candidate::WorkflowCandidate;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NudgeState {
    Surfaced,
    Accepted,
    Dismissed,
    Snoozed { until: String }, // RFC3339
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeRecord {
    pub state: NudgeState,
    pub last_ts: String,
    pub surface_count: u32,
    /// Snapshot kept so accept can rebuild the draft without re-mining.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<WorkflowCandidate>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NudgeLedger {
    #[serde(default)]
    records: BTreeMap<String, NudgeRecord>,
}

impl NudgeLedger {
    pub fn default_path() -> PathBuf {
        crate::default_mur_dir().join("nudges.json")
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
            Err(_) => Ok(Self::default()),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<&NudgeRecord> {
        self.records.get(id)
    }
    pub fn set_state(&mut self, id: &str, state: NudgeState, now: DateTime<Utc>) {
        let rec = self.records.entry(id.to_string()).or_insert(NudgeRecord {
            state: NudgeState::Surfaced, last_ts: now.to_rfc3339(), surface_count: 0, candidate: None,
        });
        rec.state = state;
        rec.last_ts = now.to_rfc3339();
    }
    /// Mark a candidate Surfaced (storing its snapshot) and bump surface_count.
    pub fn mark_surfaced(&mut self, c: &WorkflowCandidate, now: DateTime<Utc>) {
        let rec = self.records.entry(c.id.clone()).or_insert(NudgeRecord {
            state: NudgeState::Surfaced, last_ts: now.to_rfc3339(), surface_count: 0, candidate: None,
        });
        rec.state = NudgeState::Surfaced;
        rec.last_ts = now.to_rfc3339();
        rec.surface_count += 1;
        rec.candidate = Some(c.clone());
    }

    /// Candidates eligible to surface: not accepted/dismissed, not currently
    /// snoozed, capped at `daily_cap` newly-actionable items.
    pub fn filter_actionable(
        &self,
        candidates: &[WorkflowCandidate],
        now: DateTime<Utc>,
        daily_cap: u32,
    ) -> Vec<WorkflowCandidate> {
        let mut out = Vec::new();
        for c in candidates {
            match self.records.get(&c.id).map(|r| &r.state) {
                Some(NudgeState::Accepted) | Some(NudgeState::Dismissed) => continue,
                Some(NudgeState::Snoozed { until }) => {
                    let expired = DateTime::parse_from_rfc3339(until)
                        .map(|u| now >= u.with_timezone(&Utc))
                        .unwrap_or(true);
                    if !expired { continue; }
                }
                _ => {}
            }
            out.push(c.clone());
            if out.len() as u32 >= daily_cap { break; }
        }
        out
    }
}
```

(Confirm `crate::default_mur_dir()` is the canonical `~/.mur` accessor — `agent_history.rs` uses `default_mur_dir()`. Match the real path of that helper.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::ledger`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/ledger.rs
git commit -m "feat(nudge): ledger with dedup/snooze/daily-cap"
```

---

## Task 4: Extract `create_draft_workflow` helper (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/workflow.rs` (refactor `cmd_suggest`, ~lines 625-762)
- Test: `mur-core/src/cmd/workflow.rs` (inline test) or `mur-core/tests/cli_nudge.rs`

> Goal: one reusable function both `mur suggest --create` and nudge-accept call, so draft creation is DRY. Pure refactor of existing field construction — no behavior change.

- [ ] **Step 1: Write the failing test**

Add an inline test in `workflow.rs` (or a new `mur-core/tests/cli_nudge.rs` using a temp `MUR_HOME`):

```rust
#[test]
fn create_draft_workflow_persists_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::store::workflow_yaml::WorkflowYamlStore::new(tmp.path().join("workflows")).unwrap();
    create_draft_workflow_in(&store, "test-then-commit", "Run tests then commit",
        "after editing code", &["s1".into(), "s2".into()]).unwrap();
    assert!(store.exists("test-then-commit"));
    let wf = store.get("test-then-commit").unwrap();
    assert!(wf.base.is_draft);
    assert_eq!(wf.trigger, "after editing code");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core create_draft_workflow_persists`
Expected: FAIL — `create_draft_workflow_in` undefined.

- [ ] **Step 3: Implement the helper + reuse it in `cmd_suggest`**

In `workflow.rs`, extract the `Workflow` construction currently inside `cmd_suggest` into:

```rust
/// Build and save a draft Workflow. Shared by `mur suggest --create` and nudge-accept.
pub(crate) fn create_draft_workflow_in(
    store: &crate::store::workflow_yaml::WorkflowYamlStore,
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
) -> anyhow::Result<()> {
    if store.exists(name) {
        return Ok(()); // idempotent: don't clobber an existing workflow
    }
    let mut base = mur_common::KnowledgeBase::new(name, description); // use the real constructor
    base.is_draft = true;
    let wf = mur_common::workflow::Workflow {
        base,
        steps: vec![],
        variables: vec![],
        source_sessions: source_sessions.to_vec(),
        trigger: trigger.to_string(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };
    store.save(&wf)
}

/// Convenience over the default store.
pub(crate) fn create_draft_workflow(
    name: &str, description: &str, trigger: &str, source_sessions: &[String],
) -> anyhow::Result<()> {
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    create_draft_workflow_in(&store, name, description, trigger, source_sessions)
}
```

Then replace the inline construction in `cmd_suggest` with a call to `create_draft_workflow_in(&workflow_store, &s.suggested_name, &description, &s.suggested_trigger, &[])`. Match the exact `KnowledgeBase` constructor the file already uses (grep `KnowledgeBase::new` / how `cmd_suggest` builds `base` today and mirror it — do not invent field names).

- [ ] **Step 4: Run test + existing suggest tests**

Run: `cargo test -p mur-core create_draft_workflow_persists` and `cargo test -p mur-core suggest`
Expected: PASS; existing suggest behavior unchanged.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/workflow.rs
git commit -m "refactor(workflow): extract create_draft_workflow helper (DRY)"
```

---

## Task 5: `NudgeEmitter` (mur-core)

**Files:**
- Create: `mur-core/src/nudge/emitter.rs`
- Test: `mur-core/src/nudge/emitter.rs` (inline tests)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;
    use crate::nudge::ledger::{NudgeLedger, NudgeState};
    use chrono::Utc;

    fn cand(id: &str) -> WorkflowCandidate {
        WorkflowCandidate {
            id: id.into(), title: "Run tests then commit".into(),
            suggested_name: "test-then-commit".into(), steps_preview: vec![],
            session_count: 3, evidence_session_ids: vec!["s1".into()],
        }
    }

    #[test]
    fn emit_marks_surfaced() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Surfaced));
        assert_eq!(l.get("a").unwrap().surface_count, 1);
    }

    #[test]
    fn dismiss_decision_updates_ledger() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        NudgeEmitter::apply_decision(&mut l, "a", NudgeDecision::Dismiss, 7, Utc::now(), &|_c| Ok(())).unwrap();
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Dismissed));
    }

    #[test]
    fn accept_decision_calls_creator_and_marks_accepted() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        let created = std::cell::Cell::new(false);
        NudgeEmitter::apply_decision(&mut l, "a", NudgeDecision::Accept, 7, Utc::now(),
            &|c| { assert_eq!(c.suggested_name, "test-then-commit"); created.set(true); Ok(()) }).unwrap();
        assert!(created.get());
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Accepted));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::emitter`
Expected: FAIL — `NudgeEmitter` undefined.

- [ ] **Step 3: Implement**

```rust
use crate::nudge::candidate::WorkflowCandidate;
use crate::nudge::ledger::{NudgeLedger, NudgeState};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NudgeDecision { Accept, Dismiss, Snooze }

pub struct NudgeEmitter;

impl NudgeEmitter {
    /// Mark each actionable candidate Surfaced (records its snapshot).
    pub fn emit_pending(ledger: &mut NudgeLedger, actionable: &[WorkflowCandidate], now: DateTime<Utc>) {
        for c in actionable {
            ledger.mark_surfaced(c, now);
        }
    }

    /// Apply a user decision. `create` is called with the candidate on Accept
    /// (injected so the emitter stays free of workflow-store deps for testing).
    pub fn apply_decision(
        ledger: &mut NudgeLedger,
        id: &str,
        decision: NudgeDecision,
        snooze_days: u32,
        now: DateTime<Utc>,
        create: &dyn Fn(&WorkflowCandidate) -> Result<()>,
    ) -> Result<()> {
        match decision {
            NudgeDecision::Accept => {
                let cand = ledger.get(id).and_then(|r| r.candidate.clone())
                    .ok_or_else(|| anyhow!("no pending nudge with id {id}"))?;
                create(&cand)?;
                ledger.set_state(id, NudgeState::Accepted, now);
            }
            NudgeDecision::Dismiss => ledger.set_state(id, NudgeState::Dismissed, now),
            NudgeDecision::Snooze => {
                let until = (now + Duration::days(snooze_days as i64)).to_rfc3339();
                ledger.set_state(id, NudgeState::Snoozed { until }, now);
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::emitter`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/emitter.rs
git commit -m "feat(nudge): emitter (emit_pending + apply_decision)"
```

---

## Task 6: CLI `--accept` / `--dismiss` + list pending (mur-core)

**Files:**
- Modify: `mur-core/src/cli/mod.rs` (the `Suggest` variant, ~line 207)
- Modify: `mur-core/src/cmd/workflow.rs` (the `cmd_suggest` entry/dispatch)
- Test: `mur-core/tests/cli_nudge.rs`

- [ ] **Step 1: Write the failing test**

`mur-core/tests/cli_nudge.rs` (drive the dispatch fn with a temp `MUR_HOME`; mirror how other `tests/cli_*.rs` set the home env):

```rust
#[test]
fn accept_creates_draft_and_marks_ledger() {
    let home = tempfile::tempdir().unwrap();
    // seed a ledger with one surfaced candidate id "abc"
    // (write ~/.mur/nudges.json via NudgeLedger directly)
    // ... set MUR_HOME=home.path() ...
    mur_core::cmd::workflow::cmd_suggest_accept("abc").unwrap();
    let l = mur_core::nudge::NudgeLedger::load(&mur_core::nudge::NudgeLedger::default_path()).unwrap();
    assert!(matches!(l.get("abc").unwrap().state, mur_core::nudge::NudgeState::Accepted));
    // and the workflow draft exists
    assert!(mur_core::store::workflow_yaml::WorkflowYamlStore::default_store().unwrap().exists("…"));
}
```

(Use the actual `MUR_HOME`/home override the test harness in `tests/cli_drafts.rs` uses.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cli_nudge accept_creates_draft`
Expected: FAIL — `cmd_suggest_accept` undefined.

- [ ] **Step 3: Extend the CLI variant**

In `mur-core/src/cli/mod.rs`:

```rust
    /// Show workflow composition suggestions and pending nudges.
    Suggest {
        /// Auto-create suggested workflows/patterns as drafts
        #[arg(long)]
        create: bool,
        /// Accept a pending nudge by id → create its draft workflow
        #[arg(long, value_name = "ID")]
        accept: Option<String>,
        /// Dismiss a pending nudge by id (never re-surfaces)
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,
    },
```

- [ ] **Step 4: Implement the handlers + listing**

In `workflow.rs`:

```rust
pub fn cmd_suggest_accept(id: &str) -> anyhow::Result<()> {
    let cfg = mur_common::config::Config::load_or_default(&mur_common::config::Config::default_path());
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger, id, crate::nudge::NudgeDecision::Accept,
        cfg.nudge.snooze_days, chrono::Utc::now(),
        &|c| create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids),
    )?;
    ledger.save(&path)?;
    println!("✓ Saved workflow draft from nudge {id}. Run it with `mur run <name>`.");
    Ok(())
}

pub fn cmd_suggest_dismiss(id: &str) -> anyhow::Result<()> {
    let cfg = mur_common::config::Config::load_or_default(&mur_common::config::Config::default_path());
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger, id, crate::nudge::NudgeDecision::Dismiss,
        cfg.nudge.snooze_days, chrono::Utc::now(), &|_| Ok(()))?;
    ledger.save(&path)?;
    println!("✓ Dismissed nudge {id}.");
    Ok(())
}
```

Add a pending-nudges section to the existing `cmd_suggest` output: load the ledger, print each `Surfaced` (or expired-`Snoozed`) record as `  [<id-short>] <title> (seen in <n> sessions) — mur suggest --accept <id>`. Wire the `accept`/`dismiss` args in the dispatch (where `Suggest { .. }` is matched — grep for the match arm and route to the two new fns; `create` keeps its current behavior).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p mur-core --test cli_nudge accept_creates_draft`
Then: `cargo run -p mur-core -- suggest --help` (confirm `--accept/--dismiss` show).
Expected: PASS; help renders.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/mod.rs mur-core/src/cmd/workflow.rs mur-core/tests/cli_nudge.rs
git commit -m "feat(nudge): mur suggest --accept/--dismiss + pending listing"
```

---

## Task 7: Session-end hook (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/session.rs` (`cmd_session_stop`, after `detect_emergent` ~line 138-144)
- Test: `mur-core/tests/cli_nudge.rs`

> Replace the current "run `mur skill suggest`" eprintln with: map candidates → filter through the ledger → `emit_pending` → persist → print a concise hint pointing at `mur suggest`. Respect `config.nudge.enabled` (when false, keep old behavior / skip emission — gate so Phase 1 is opt-in until Phase 2 surface ships, per Task 1 default).

- [ ] **Step 1: Write the failing test**

In `cli_nudge.rs`:

```rust
#[test]
fn session_stop_records_pending_nudges() {
    let home = tempfile::tempdir().unwrap();
    // set MUR_HOME, enable nudges in config, seed ~/.mur/fingerprints.jsonl with
    // >=3 sessions of the same behavior so detect_emergent yields a candidate.
    // ... then:
    mur_core::cmd::session::record_nudges_for_candidates(&candidates).unwrap();
    let l = mur_core::nudge::NudgeLedger::load(&mur_core::nudge::NudgeLedger::default_path()).unwrap();
    assert_eq!(l.get(&candidates[0].id).unwrap().surface_count, 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cli_nudge session_stop_records`
Expected: FAIL — `record_nudges_for_candidates` undefined.

- [ ] **Step 3: Implement the hook**

Add to `session.rs`:

```rust
/// Filter candidates through the nudge ledger and mark the actionable ones
/// Surfaced. Returns the ids that were surfaced (for the CLI hint).
pub(crate) fn record_nudges_for_candidates(
    candidates: &[crate::nudge::WorkflowCandidate],
) -> anyhow::Result<Vec<String>> {
    let cfg = mur_common::config::Config::load_or_default(&mur_common::config::Config::default_path());
    if !cfg.nudge.enabled || candidates.is_empty() {
        return Ok(vec![]);
    }
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    let now = chrono::Utc::now();
    let actionable = ledger.filter_actionable(candidates, now, cfg.nudge.daily_cap);
    crate::nudge::NudgeEmitter::emit_pending(&mut ledger, &actionable, now);
    ledger.save(&path)?;
    Ok(actionable.into_iter().map(|c| c.id).collect())
}
```

Then in `cmd_session_stop`, where `detect_emergent(&fps, 3)` runs (~line 138), map to candidates and call the hook:

```rust
let candidates: Vec<_> = crate::capture::emergence::detect_emergent(&fps, cfg.nudge.threshold)
    .iter()
    .map(crate::nudge::WorkflowCandidate::from_emergent)
    .collect();
let surfaced = record_nudges_for_candidates(&candidates)?;
if !surfaced.is_empty() {
    eprintln!("💡 Noticed {} repeated workflow(s). Review with `mur suggest`.", surfaced.len());
}
```

(Load `cfg` once near the top of `cmd_session_stop` if not already present.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --test cli_nudge session_stop_records`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/session.rs mur-core/tests/cli_nudge.rs
git commit -m "feat(nudge): surface pending nudges at session end (gated by config)"
```

---

## Task 8: End-to-end integration test (mur-core)

**Files:**
- Test: `mur-core/tests/cli_nudge.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn end_to_end_detect_accept_no_resurface() {
    let home = tempfile::tempdir().unwrap();
    // 1. enable nudges; seed fingerprints for one recurring behavior across 3 sessions.
    // 2. compute candidates + record pending (record_nudges_for_candidates).
    // 3. accept the candidate by id → draft created, ledger Accepted.
    // 4. recompute + filter_actionable → the accepted candidate is NOT actionable again.
    let cands = /* detect via EmergenceSource::from_disk()?.candidates(3) */;
    let id = cands[0].id.clone();
    mur_core::cmd::session::record_nudges_for_candidates(&cands).unwrap();
    mur_core::cmd::workflow::cmd_suggest_accept(&id).unwrap();

    let l = mur_core::nudge::NudgeLedger::load(&mur_core::nudge::NudgeLedger::default_path()).unwrap();
    assert!(matches!(l.get(&id).unwrap().state, mur_core::nudge::NudgeState::Accepted));
    assert!(l.filter_actionable(&cands, chrono::Utc::now(), 10).is_empty());
    assert!(mur_core::store::workflow_yaml::WorkflowYamlStore::default_store().unwrap()
        .exists(&cands[0].suggested_name));
}
```

- [ ] **Step 2: Run + full suite + clippy/fmt**

Run:
```bash
cargo test -p mur-core --test cli_nudge
cargo test -p mur-core nudge::
cargo test -p mur-common nudge_config
cargo clippy -p mur-core -p mur-common -- -D warnings
cargo fmt --check
```
Expected: all PASS / clean.

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/cli_nudge.rs
git commit -m "test(nudge): end-to-end detect→accept→no-resurface"
```

---

## Phase 2 (companion surface — separate plan)

**Do not implement here.** Spec unit 4 (the proactive speech bubble) integrates the nudge into the `mur-agent-runtime` companion outbox subsystem. Write `docs/superpowers/plans/<date>-companion-workflow-nudge-phase2.md` covering:
- A `Situation::WorkflowNudge` variant (`mur_common::companion`) + i18n templates (`companion/outbox/i18n.rs`, `companion/i18n.rs`).
- Emitting the nudge as a `CompanionMessage` to the agent companion inbox (`companion/inbox.rs::write_inbox_md`) at session-end / idle, gated by `voice/dnd.rs::is_focus_active()`.
- Routing the user's `BridgeResponse` (`good` → Accept, `dismiss` → Dismiss; add a `snooze` signal value in `companion_bridge/event.rs`) back to `NudgeEmitter::apply_decision`, reusing the existing outbox passive-dismiss/picker plumbing (`companion/outbox/mod.rs`).
- Flipping `NudgeConfig::enabled` default to `true` once the surface exists.
- Tests: DND suppression leaves the nudge pending; `good`/`dismiss`/`snooze` responses map to the right ledger transitions.

---

## Self-Review

**Spec coverage (Phase 1 scope):**
- §3 emergence-only signal, pluggable source → Task 2 (`CandidateSource`/`EmergenceSource`). Co-occurrence correctly deferred.
- §4 unit 1 (CandidateSource) → Task 2; unit 2 (NudgeLedger) → Task 3; unit 3 (NudgeEmitter) → Task 5; unit 5 (CLI fallback) → Task 6. Unit 4 (companion) → Phase 2 (deferred).
- §5 accept reuses existing draft path → Task 4 (extracted helper) + Task 6 (accept wires it).
- §6 data flow (session-end → candidates → ledger filter → emit; accept → draft) → Tasks 7, 6, 8.
- §7 anti-nag (dismissed terminal, snooze window, daily cap, stable ids) → Task 3 (filter) + Task 2 (stable id).
- §8 testing (ledger dedup, snooze, daily cap, candidate mapping, accept creates draft) → Tasks 2,3,5,6,8. DND suppression → Phase 2 (companion-only).

**Placeholder scan:** Code steps carry real code. The few "match the existing constructor/harness in <file>" notes point at concrete files (`tests/cli_drafts.rs` for the home-env harness; the existing `cmd_suggest` `KnowledgeBase` construction) rather than leaving logic undefined — necessary because the exact `KnowledgeBase::new` shape must be read from source, not invented.

**Type consistency:** `WorkflowCandidate` (Task 2) fields are used unchanged in Tasks 3,5,6,7,8. `NudgeState`/`NudgeLedger` (Task 3) used in Tasks 5,6,7,8. `NudgeDecision`/`NudgeEmitter` (Task 5) used in Tasks 6,7. `create_draft_workflow`/`create_draft_workflow_in` (Task 4) used in Task 6. `record_nudges_for_candidates` (Task 7) used in Task 8.
