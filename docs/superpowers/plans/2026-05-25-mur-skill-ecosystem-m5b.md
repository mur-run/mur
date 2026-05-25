# M5b — Lifecycle Mutation + Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn M5a's read-only observability into a mutation pass. Ship the idempotent lifecycle sweep, the `--apply` repair engine for `mur skill doctor`, and the first consolidation pass (dedup + contradiction + orphan). Wire the sweep into the existing C6 idle-trigger plumbing so it runs without explicit user invocation.

**Spec mapping:** §9.1 lifecycle persistence, §9.2 auto-demote / auto-archive, §9.3 `mur skill doctor --fix`, §9.4 consolidation, §14 M5 entries that require mutation.

**Hard dependency on M5a:**
- `mur-common::skill::stats::SkillStats` + `merge_in_place`
- `mur-common::skill::lifecycle::{next_state, calculate_decay, transition_allowed, on_promotion}`
- `Event::SkillExecuted` + `StatsAggregator`
- `mur skill doctor` CLI (M5b adds behaviour, not surface)

**What M5b ships:**
1. `mur skill sweep [<filter>] [--dry-run] [--all]` — the persisting lifecycle pass.
2. `mur skill doctor --fix --apply` — best-effort per-check auto-repair.
3. `mur skill consolidate [--dry-run] [--apply]` — Jaccard-based dedup, rule-based contradiction, age-based orphan; reports written to `~/.mur/skills/_consolidation/<date>.jsonl`.
4. `mur skill archive <name>` — operator-driven archive.
5. C6 idle-trigger hook handler `skill-sweep` so users can `mur agent schedule idle-add --hook skill-sweep`.

**What M5b does NOT ship:**
- Cosine-similarity dedup via LanceDB skill vector index → M6.
- LLM-driven contradiction adjudication, coverage-gap detection, `api-drift` repair → M6.
- Cross-agent skill evolution / EvoMap → M7.
- Skill A/B testing → deferred per §15.

**Tech Stack:** Rust 2024. No new dependencies — Jaccard similarity is hand-rolled (<30 lines), idle-trigger handler reuses C6 plumbing. `globset` and `sysexits` are inherited from M5a.

---

## File Structure

**Create:**
- `mur-core/src/skill_lifecycle/sweep.rs` — `run_sweep`, drives `next_state` over all skills, persists transitions, emits events, writes summary.
- `mur-core/src/skill_lifecycle/mod.rs`.
- `mur-core/src/skill_repair/mod.rs` — repair engine: per-check `Repair` trait, dispatcher, dry-run vs apply, summary.
- `mur-core/src/skill_repair/{tool_availability,dep_freshness}.rs` — one repair impl per fixable check.
- `mur-core/src/skill_consolidate/{mod.rs,dedup.rs,contradiction.rs,orphan.rs,report.rs}` — passes + JSONL report writer.
- `mur-core/src/cmd/skill_sweep.rs` — CLI dispatcher.
- `mur-core/src/cmd/skill_consolidate.rs` — CLI dispatcher.
- `mur-core/src/cmd/skill_archive.rs` — CLI dispatcher.
- `mur-agent-runtime/src/hooks/skill_sweep_idle.rs` — idle-trigger handler that calls into `mur-core::skill_lifecycle::sweep::run_sweep`.
- `mur-core/tests/skill_sweep_idempotent.rs` — verify two consecutive sweeps with same now produce no second-pass changes.
- `mur-core/tests/skill_consolidate_jaccard.rs` — fixture-driven dedup/contradiction/orphan tests.
- `mur-core/tests/skill_doctor_fix_apply.rs` — end-to-end repair test (using a `file://` registry from M3 e2e tests).

**Modify:**
- `mur-core/src/cmd/skill_doctor.rs` — promote `--fix --apply` from no-op stub to active repair flow; reuse the Check trait's `Finding::fixable` flag to skip non-repairable findings.
- `mur-core/src/cmd/skill_cmd.rs` (or wherever `mur skill list` lives) — show new state column live (M5a already added it; M5b ensures the sweep-persisted value is read).
- `mur-core/src/lib.rs` — `pub mod {skill_lifecycle, skill_repair, skill_consolidate};`
- `mur-agent-runtime/src/hooks/mod.rs` — register `skill_sweep_idle` handler with the C6 idle scheduler.

**Do not modify:**
- `mur-common::skill::stats::SkillStats` schema — M5b's writes go through the existing `merge_in_place`, no new fields.
- `mur-common::skill::lifecycle` — predicates stay pure; sweep is the driver, not the predicate.
- DSSE signing — repair flows that reinstall a dep go through the existing `cmd_install` path, which already enforces signature verification.

---

### Task 1 — Lifecycle sweep (`mur skill sweep`)

**Files:** `mur-core/src/skill_lifecycle/{mod.rs,sweep.rs}` (new), `mur-core/src/cmd/skill_sweep.rs` (new), CLI wiring.

- [ ] **Step 1: Reconcile loop**

```rust
// mur-core/src/skill_lifecycle/sweep.rs

use chrono::{DateTime, Utc};
use mur_common::skill::lifecycle::{calculate_decay, half_life_days, next_state, on_promotion, transition_allowed};
use mur_common::skill::stats::{LifecycleState, SkillStats};

pub struct SweepOptions {
    pub filter: Option<String>,   // exact name or glob
    pub dry_run: bool,
    pub now: DateTime<Utc>,        // injected for tests
}

pub struct SweepReport {
    pub examined: usize,
    pub transitions: Vec<Transition>,
    pub decayed: usize,
    pub archived: usize,
}

pub struct Transition {
    pub skill_name: String,
    pub from: LifecycleState,
    pub to: LifecycleState,
    pub reason: TransitionReason,
}

pub fn run_sweep(home: &Path, opts: SweepOptions) -> Result<SweepReport> {
    let installed = mur_common::skill::local::list_installed(home)?;
    let mut report = SweepReport::default();

    for name in installed {
        if !matches_filter(&name, opts.filter.as_deref()) { continue; }
        report.examined += 1;

        // Load (default if missing — newly installed skill, no events yet)
        let stats_path = SkillStats::path(home, &name);
        let current = SkillStats::load(&stats_path)?.unwrap_or_else(|| /* default */);

        let proposed = next_state(&current, opts.now);
        let decayed_value = calculate_decay(
            current.anchor_confidence,
            current.last_success_at,
            half_life_days(current.lifecycle_state),
            opts.now,
        );

        if proposed != current.lifecycle_state
            && transition_allowed(current.lifecycle_state, proposed, &current, opts.now)
        {
            report.transitions.push(Transition {
                skill_name: name.clone(),
                from: current.lifecycle_state,
                to: proposed,
                reason: classify_reason(&current, proposed, opts.now),
            });

            if !opts.dry_run {
                SkillStats::merge_in_place(&stats_path,
                    || current.clone(),
                    |s| {
                        let was = s.lifecycle_state;
                        s.lifecycle_state = proposed;
                        s.lifecycle_changed_at = opts.now;
                        if rank(proposed) > rank(was) {
                            // Anchor-reset gotcha: bake current decayed value as the new anchor
                            // BEFORE the new (longer) half-life would otherwise inherit it.
                            s.anchor_confidence = decayed_value;
                        }
                        if proposed == LifecycleState::Archived { /* report.archived += 1 */ }
                        Ok(())
                    })?;
            }
        }

        report.decayed += 1;  // (decay is recomputed on read; the report just counts what we saw)
    }

    Ok(report)
}
```

Key invariants documented inline:
- `next_state` returning a different value is necessary, `transition_allowed` returning true is also necessary. Both must hold.
- Anchor reset happens **only on promotion** (rank increases). Demotions / archives keep the previous anchor — otherwise a skill that gets demoted to Deprecated would lose its history.
- The reconcile loop is per-skill atomic: each `merge_in_place` is its own lock window. If the sweep is interrupted halfway, the remaining skills stay in their pre-sweep state and the next sweep picks up cleanly (idempotent).

- [ ] **Step 2: Emit transition events**

For each entry in `report.transitions`, emit:

```
tracing::info_span!("mur.skill.state_changed",
    skill = %t.skill_name,
    from = ?t.from,
    to = ?t.to,
    reason = ?t.reason,
).in_scope(|| tracing::info!("transition persisted"));
```

When the sweep runs inside the runtime (idle-trigger path), this becomes a real notification on the existing wire — the M4 health-dashboard tooling already consumes `mur.skill.*` spans.

- [ ] **Step 3: CLI dispatcher**

```rust
// mur-core/src/cmd/skill_sweep.rs

pub fn cmd_sweep(home: &Path, filter: Option<&str>, dry_run: bool) -> Result<()> {
    let report = run_sweep(home, SweepOptions { filter: filter.map(str::to_string), dry_run, now: Utc::now() })?;
    print_sweep_table(&report, dry_run);
    Ok(())
}
```

Output:
```
Examined: 12 skill(s)
Transitions (3):
  research-prices    Draft     -> Emerging    (3 successes; first success 14d ago)
  cite-source        Stable    -> Deprecated  (success_rate 0.21 over 14 uses)
  legacy-fetcher     Deprecated -> Archived   (confidence 0.07, age 192d)
Decayed (read): 12 (confidence recomputed on read; persisted only on promotion)
Archived: 1
```

`--dry-run` adds a `(dry-run; no changes written)` footer.

- [ ] **Step 4: Idempotency test**

`mur-core/tests/skill_sweep_idempotent.rs`:
- Construct a MUR_HOME with 5 skills at various synthetic states.
- Run sweep with fixed `now`.
- Run sweep again with the same `now`.
- Assert second run's `transitions` is empty.
- Assert the persisted state matches the first run's persisted state exactly.

- [ ] **Step 5: Anchor-reset test**

A skill at Draft with `anchor_confidence=1.0`, `last_success_at` 14 days ago. Decayed value is exactly 0.5 (one half-life). Sweep promotes to Emerging (3 successes met).

Assert: `anchor_confidence` is now ~0.5 (not 1.0). Subsequent decay under the 90-day half-life starts from 0.5, not 1.0 — preserves history.

- [ ] **Step 6: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_sweep
git commit -am "feat(skill): mur skill sweep — idempotent lifecycle reconcile (M5b)"
```

---

### Task 2 — `mur skill doctor --fix --apply`

**Files:** `mur-core/src/skill_repair/{mod.rs,tool_availability.rs,dep_freshness.rs}` (new), `mur-core/src/cmd/skill_doctor.rs` (modify).

- [ ] **Step 1: Repair trait**

```rust
// mur-core/src/skill_repair/mod.rs

pub enum RepairOutcome {
    Fixed,
    Skipped(String),    // not applicable (e.g., no newer version available)
    Failed(anyhow::Error),
    DryRun(String),     // what would have been done
}

pub trait Repair {
    fn check_id(&self) -> &'static str;
    fn applicable(&self, finding: &Finding) -> bool;
    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome;
}

pub struct RepairCtx<'a> {
    pub home: &'a Path,
    pub registry_url: &'a str,  // for reinstall paths
}

pub fn run_repairs(findings: &[Finding], apply: bool, ctx: &RepairCtx, repairs: &[Box<dyn Repair>]) -> RepairReport {
    /* dispatch each finding to the first matching Repair */
}
```

- [ ] **Step 2: Implement `tool_availability` repair**

When a finding says `MCP tool 'xyz' not available`, the only safe auto-fix is:
- If the tool is provided by a mur-managed skill that's already in the dependency graph but not installed → call `cmd_install`.
- Otherwise, emit `Skipped("manual MCP install required: <hint>")`. Print the appropriate manual command.

No silent provisioning of arbitrary MCP servers — that would be a privilege-escalation footgun.

- [ ] **Step 3: Implement `dep_freshness` repair**

When a finding says `dependency 'base' is at 1.0.0, constraint ^1.2.0`:
- Use M3a's `mur_core::cmd::skill_resolver` to compute the best version satisfying the constraint.
- If found → `apply ? cmd_install(home, registry_url, &format!("{name}@{v}")) : RepairOutcome::DryRun(...)`.
- If no version satisfies → `Skipped`.

`cmd_install` is the existing recursive installer (M3a) — it brings in the dep's transitive deps too, runs DSSE verification, registers in the profile. We deliberately do **not** add a "fast path" that skips verification.

- [ ] **Step 4: Doctor CLI wiring**

In `cmd_doctor`, replace the M5a no-op block:

```rust
if opts.fix {
    let repairs: Vec<Box<dyn Repair>> = vec![
        Box::new(ToolAvailabilityRepair),
        Box::new(DepFreshnessRepair),
    ];
    let ctx = RepairCtx { home: opts.home, registry_url: &opts.registry_url };
    let report = run_repairs(&findings, opts.apply, &ctx, &repairs);
    print_repair_summary(&report, opts.apply);
}
```

Output:
```
Repair summary:
  Fixed:      2  (dep_freshness: base@1.0.0 -> 1.3.1, dep_freshness: lib@0.5 -> 0.6)
  DryRun:     1  (would reinstall analytics@^2)
  Skipped:    1  (tool_availability: mcp:exotic-tool — manual install required)
  Unfixable:  0
```

When `--apply` is absent, all outcomes become `DryRun(...)` — no exception.

- [ ] **Step 5: Confirmation prompt**

For destructive `--apply` operations (currently only "reinstall to upgrade"), prompt:

```
About to reinstall 2 skill dependency(ies). Continue? [y/N]
```

Skipped when `--yes` is passed or when stdin is not a TTY (CI mode — assume already audited via `--dry-run`).

- [ ] **Step 6: Tests**

`mur-core/tests/skill_doctor_fix_apply.rs`:
- Reuse the file:// registry harness from M3 e2e tests.
- Install `child@1.0.0` requiring `base@^1.0.0` against a registry where `base@1.3.1` is the newest.
- Patch the lock to claim `base@1.0.0` (mimicking drift).
- Run `doctor --fix --apply --yes` → assert `base@1.3.1` is now installed.
- Run again → assert "Fixed: 0" (idempotent).

- [ ] **Step 7: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_doctor_fix_apply
git commit -am "feat(skill): mur skill doctor --fix --apply (M5b repair engine)"
```

---

### Task 3 — `mur skill consolidate`

**Files:** `mur-core/src/skill_consolidate/*.rs` (new), `mur-core/src/cmd/skill_consolidate.rs` (new).

- [ ] **Step 1: Pass orchestrator**

```rust
// mur-core/src/skill_consolidate/mod.rs

pub struct ConsolidateOptions { pub dry_run: bool, pub apply: bool }

pub struct ConsolidateReport {
    pub duplicates: Vec<DuplicatePair>,
    pub contradictions: Vec<ContradictionPair>,
    pub orphans: Vec<OrphanFinding>,
    pub apply_log: Vec<AppliedAction>,
}

pub fn run_consolidate(home: &Path, opts: &ConsolidateOptions) -> Result<ConsolidateReport> {
    let skills = load_all_with_stats(home)?;
    let mut report = ConsolidateReport::default();

    dedup::scan(&skills, &mut report)?;
    contradiction::scan(&skills, &mut report)?;
    orphan::scan(&skills, &mut report, Utc::now())?;

    if opts.apply {
        apply_findings(home, &mut report)?;
    }

    write_jsonl_report(home, &report)?;
    Ok(report)
}
```

- [ ] **Step 2: Dedup pass (Jaccard on token set)**

```rust
// mur-core/src/skill_consolidate/dedup.rs

const JACCARD_THRESHOLD: f64 = 0.85;

pub struct DuplicatePair {
    pub a: String,
    pub b: String,
    pub similarity: f64,
    pub keeper: String,   // higher score wins
    pub kept_reason: KeeperReason,
}

pub fn scan(skills: &[SkillView], report: &mut ConsolidateReport) -> Result<()> {
    // Token sets: lowercased words from {name, description, triggers, requires.name}
    // Symmetric pairwise. O(n²) — fine for hundreds of skills; document the limit.
    for i in 0..skills.len() {
        for j in (i+1)..skills.len() {
            let sim = jaccard(&tokens(&skills[i]), &tokens(&skills[j]));
            if sim >= JACCARD_THRESHOLD {
                report.duplicates.push(DuplicatePair { /* … */ });
            }
        }
    }
    Ok(())
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() { return 1.0; }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}
```

Keeper selection (high → low priority):
1. Higher lifecycle state (Canonical > Stable > Emerging > Draft).
2. Higher success_count.
3. Newer manifest version (semver compare).
4. Alphabetical name (deterministic tiebreaker).

`--apply` writes `duplicate_of: <keeper>` to the loser's `stats.json` (new field — needs schema bump? **No** — store it in the lifecycle layer as `LifecycleState::Deprecated` with reason "duplicate_of:<keeper>" in stats. Avoids changing the schema.)

Actually, **store in `stats.pinned_reason` only if we deprecate it**. Cleaner: dedup `--apply` flips the loser to Deprecated with `pinned_reason = format!("duplicate_of:{keeper}")` (overload is fine — Deprecated isn't pinned in the human sense, but pinned_reason is the existing free-form context field). Document this overload at the schema site so we don't grow new fields.

- [ ] **Step 3: Contradiction pass (rule-based)**

```rust
// mur-core/src/skill_consolidate/contradiction.rs

pub struct ContradictionPair { pub a: String, pub b: String, pub trigger: String, pub reason: String }

pub fn scan(skills: &[SkillView], report: &mut ConsolidateReport) -> Result<()> {
    // For each pair (i, j), find triggers that overlap by exact-string match
    // (skip glob/regex triggers — too noisy to compare without semantic analysis).
    // For each overlap, compare procedure first-step tool:
    //   if both define a procedure AND first step's tool differs -> flag.
    //   if one is procedure and the other is context -> skip (mode difference, not contradiction).
}
```

Rule-based only. No LLM. Doc deferred LLM mode at M6.

- [ ] **Step 4: Orphan pass**

```rust
// mur-core/src/skill_consolidate/orphan.rs

pub struct OrphanFinding { pub name: String, pub last_used: Option<DateTime<Utc>>, pub usage_count: u64 }

pub fn scan(skills: &[SkillView], report: &mut ConsolidateReport, now: DateTime<Utc>) -> Result<()> {
    for s in skills {
        if let Some(last) = s.stats.last_used_at {
            if (now - last).num_days() > 180 && s.stats.usage_count > 0 && !s.stats.pinned {
                report.orphans.push(/* … */);
            }
        }
    }
    Ok(())
}
```

`--apply` for orphans: invokes `cmd_archive(home, &name)` from Task 4. Pinned skills are excluded.

- [ ] **Step 5: JSONL report writer**

```
~/.mur/skills/_consolidation/2026-05-26.jsonl
```

One line per finding: `{"type":"duplicate","a":"...","b":"...","similarity":0.91,"keeper":"...","applied":true,"applied_at":"..."}`. Schema mirrors trace JSONL for tooling consistency.

- [ ] **Step 6: CLI dispatcher**

```rust
pub fn cmd_consolidate(home: &Path, dry_run: bool, apply: bool, yes: bool) -> Result<()> {
    if apply && !yes && atty::is(atty::Stream::Stdin) {
        // confirm before mutating
    }
    let report = run_consolidate(home, &ConsolidateOptions { dry_run, apply })?;
    print_consolidate_summary(&report, apply);
    Ok(())
}
```

Default behaviour with no flags: dry-run. `--apply` mutates.

- [ ] **Step 7: Tests**

`mur-core/tests/skill_consolidate_jaccard.rs`:
- Two near-identical skills (same triggers, similar descriptions) → exactly one DuplicatePair, keeper is the one with higher usage.
- Two contradictory skills (same trigger, different first-step tool) → one ContradictionPair.
- One orphan (usage 5, last used 200d ago) → one OrphanFinding; `--apply` flips it to Archived.
- Idempotency: re-running consolidate finds zero new duplicates (the deprecated loser is skipped because of its lifecycle state).

- [ ] **Step 8: Build + commit**

```
cargo build --workspace
cargo test -p mur-core skill_consolidate
git commit -am "feat(skill): mur skill consolidate — dedup + contradiction + orphan (M5b)"
```

---

### Task 4 — `mur skill archive <name>`

**Files:** `mur-core/src/cmd/skill_archive.rs` (new), CLI wiring.

Trivial driver: load stats, flip `lifecycle_state` to `Archived`, persist via `merge_in_place`. Useful both as an operator command and as the action that consolidate's `--apply` invokes for orphan findings.

- [ ] **Step 1: Implement**

```rust
pub fn cmd_archive(home: &Path, name: &str, reason: Option<&str>) -> Result<()> {
    let path = SkillStats::path(home, name);
    SkillStats::merge_in_place(&path,
        || /* default for never-used skill */,
        |s| {
            s.lifecycle_state = LifecycleState::Archived;
            s.lifecycle_changed_at = Utc::now();
            if let Some(r) = reason { s.pinned_reason = format!("archived: {r}"); }
            Ok(())
        })?;
    println!("Archived {}.", name);
    Ok(())
}
```

- [ ] **Step 2: Tests + commit**

Idempotent: archiving an already-archived skill is a no-op success.

```
git commit -am "feat(skill): mur skill archive <name>"
```

---

### Task 5 — Idle-trigger hook handler

**Files:** `mur-agent-runtime/src/hooks/skill_sweep_idle.rs` (new), `mur-agent-runtime/src/hooks/mod.rs` (modify).

The C6 idle scheduler (already shipped, PR #223) calls registered handlers every 30 s when the agent is idle. M5b adds a `skill-sweep` handler so users can:

```
mur agent schedule idle-add --hook skill-sweep --min-idle-mins 10
```

- [ ] **Step 1: Implement**

```rust
// mur-agent-runtime/src/hooks/skill_sweep_idle.rs

pub struct SkillSweepHandler { /* ... */ }

impl IdleHookHandler for SkillSweepHandler {
    fn id(&self) -> &'static str { "skill-sweep" }
    async fn run(&self, ctx: &IdleCtx) -> Result<()> {
        let home = ctx.mur_home();
        let report = tokio::task::spawn_blocking(move ||
            mur_core::skill_lifecycle::sweep::run_sweep(&home, SweepOptions { filter: None, dry_run: false, now: Utc::now() })
        ).await??;
        if !report.transitions.is_empty() {
            tracing::info!(transitions = report.transitions.len(), "skill sweep applied transitions");
        }
        Ok(())
    }
}
```

The sweep is blocking I/O (file locks + atomic renames). Wrapping in `spawn_blocking` keeps the tokio runtime healthy.

- [ ] **Step 2: Register**

```rust
// mur-agent-runtime/src/hooks/mod.rs

pub fn register_default_idle_handlers(scheduler: &IdleScheduler) {
    scheduler.register(Box::new(SkillSweepHandler::new()));
    // ... existing handlers ...
}
```

- [ ] **Step 3: Test**

Integration test that wires a fake `IdleScheduler` with the handler, ticks once, verifies the sweep ran (transitions written, span emitted).

- [ ] **Step 4: Build + commit**

```
cargo build --workspace
cargo test -p mur-agent-runtime hooks::skill_sweep
git commit -am "feat(skill): idle-trigger handler for skill-sweep (M5b)"
```

---

## Out of scope — deferred to M6 / M7

1. **LanceDB skill vector index** — would replace Jaccard with cosine similarity for dedup. M6.
2. **LLM-driven contradiction adjudication** — current rule-based pass only looks at first-step tool difference. M6 layer adds an LLM judge for nuanced cases.
3. **Coverage-gap detection** — needs trace failure clustering + LLM. M6.
4. **API drift repair** — the M5a stub stays a stub. M6.
5. **Cross-agent / federated consolidation** — M7.
6. **`mur skill sweep --schedule`** (cron-style explicit scheduling) — idle-trigger handler is enough for v1; cron-on-cli can be added if user demand surfaces.

---

## Self-Review

**Spec coverage:**

| Spec § | Requirement | M5b coverage |
|---|---|---|
| §9.1 | Lifecycle persistence | `run_sweep` writes `stats.json` via `merge_in_place` with anchor-reset on promotion |
| §9.2 | Auto-demote / auto-archive | `next_state` already returns the target; `run_sweep` persists when `transition_allowed` permits |
| §9.3 | `mur skill doctor --fix` | Repair trait + two repair impls; dry-run by default, `--apply` mutates |
| §9.4 | Consolidation | Jaccard dedup + rule-based contradiction + age-based orphan + JSONL report |
| §14 M5 | `mur skill evolve` (lifecycle sweep) | Shipped as `mur skill sweep` to avoid colliding with M3c's `mur skill evolve <name>` (LLM rewrite). Document the rename in CHANGELOG. |
| §14 M5 | `mur skill doctor --fix` | Shipped |

**Idempotency invariants:**
- `run_sweep` is a fixed-point operation: `run_sweep(home, now)` followed by `run_sweep(home, now)` with the same `now` produces zero new transitions. Tested.
- `cmd_archive` on an already-archived skill is a no-op success. Tested.
- `consolidate --apply` re-run is a no-op (loser is now Deprecated/Archived, contradiction-pair detection skips non-active skills, orphans are now archived). Tested.
- Doctor repair (`dep_freshness`) is naturally idempotent: once `base@1.3.1` is installed, the second run finds no drift.

**Concurrency:**
- All writes go through M5a's `SkillStats::merge_in_place` — fd-lock + temp-rename. Two agents racing on the same sweep:
  - Each acquires the lock per skill in turn; the second one reads the first one's already-persisted state and recomputes `next_state` (no-op).
  - Worst case: both sweeps execute the same transition; the lock ensures the second is a read-the-result-and-no-op rather than a double-apply.

**Anchor-reset correctness:**
- Tested explicitly: Draft → Emerging promotion at one-half-life of decay produces an anchor of 0.5 under the new (90d) half-life, not 1.0.
- Demotion (e.g., Stable → Deprecated) does **not** reset anchor. Intended — we want the skill's confidence history preserved as it ages out.

**Safety:**
- Dry-run is the default everywhere that mutates. `--apply` is explicit. Confirmation prompt for destructive ops unless `--yes` or non-TTY.
- All install/upgrade paths go through `cmd_install`, preserving DSSE verification + trust ladder.
- No new privilege escalation: `tool_availability` repair refuses to silently provision MCP servers.

**Test coverage:**
- Sweep idempotency: stress-tested with `propag-style` randomised stats (Task 1 Step 4 + 5).
- Repair end-to-end: file:// registry from M3 e2e (Task 2 Step 6).
- Consolidate fixtures: deterministic, no LLM needed (Task 3 Step 7).
- **Not covered**: Multi-host MUR_HOME (out of scope), interrupted sweep crash mid-skill (acceptable — per-skill atomicity means worst case is the unprocessed tail is picked up next sweep), millions of skills (Jaccard is O(n²); we document the practical limit).

**Backwards compatibility:**
- `mur skill doctor` CLI surface from M5a unchanged; `--fix --apply` now does something instead of warning.
- `SkillStats` schema unchanged — pinned_reason overloaded for "archived: …" / "duplicate_of: …" context. Documented at the schema.
- `mur skill list` shows persisted lifecycle state (M5a already added the column).

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m5b.md`.

Suggested branch: `feat/skill-ecosystem-m5b`, branched from the merged `feat/skill-ecosystem-m5a` tip.

Two execution options:
1. **Linear (`superpowers:executing-plans`)** — work the tasks in order. Roughly 5 commits. Recommended on first pass to stay close to the reconcile-loop discipline.
2. **Subagent-driven (`superpowers:subagent-driven-development`)** — Tasks 2, 3, and 4 are independent and can be parallelised after Task 1 lands (they all consume sweep+stats). Task 5 (idle handler) wraps Task 1 and must come last.
