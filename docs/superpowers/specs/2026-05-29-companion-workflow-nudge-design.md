# Companion Workflow Nudge — Design

**Date:** 2026-05-29
**Status:** Design / spec (implementation-ready after plan)
**Umbrella:** `docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md` (§4.1 consumer wedge; §5 "authoring by recording"; §8.E automatic workflow mining)

The consumer wedge's remaining unbuilt piece. MUR already mines recurring behavior into suggested workflows, but only when the user runs a CLI command (`mur suggest`, `mur emerge`). This feature turns that **CLI-pull into a companion-push**: after a session, if MUR detects a procedure repeated across multiple sessions, the companion proactively asks — *"I noticed you repeated this. Save it as a replayable workflow?"* — with accept / dismiss / snooze. It is the zero-authoring counter to Archon's hand-written YAML workflows.

---

## 1. Goal & non-goals

**Goal.** Make MUR's existing emergence mining *visible at the right moment* through the companion, so a user discovers and saves replayable workflows without ever running a mining command — the "it watches how you work and offers to automate it" moment.

**Non-goals (v1).**
- No visual DAG editor and no workflow marketplace (umbrella §5/§7 — explicitly not built).
- No new mining algorithm — reuse `capture/emergence.rs` as-is.
- No co-occurrence signal in v1 (see §3 — its data feed is being removed by the Pattern→Skill migration; deferred and repointed later).
- No in-bubble step editor — accept creates a *draft* workflow; editing uses the existing workflow-edit surface.
- No new workflow storage — accept reuses the existing draft-creation path.

---

## 2. Background: existing mining (grounding)

- **`capture/emergence.rs`** — `extract_fingerprints(transcript, session_id)` derives behavior fingerprints from a session transcript; `detect_emergent(fingerprints, threshold)` clusters them by `jaccard_similarity` and returns `EmergentCandidate { description, session_count, session_ids, … }` for clusters seen in ≥ `threshold` (default 3) distinct sessions. `generate_suggested_name(keywords)` and `build_candidate(...)` already exist. Fingerprints persist via `save_fingerprints` / `load_fingerprints` / `prune_fingerprints`. **Transcript-based — independent of the pattern model.**
- **`evolve/cooccurrence.rs` + `compose.rs`** — `record_cooccurrence(&injected_patterns)` builds a `CooccurrenceMatrix`; `suggest_workflows(matrix, threshold)` yields `WorkflowSuggestion`. **Fed by the pattern-injection path**, which the Pattern→Skill migration removes (notes-design Resolved Decision #3: "the `inject/` push modules are removed"). Out of scope for v1 (see §3).
- **`mur suggest [--create]`** (`cli/mod.rs:206`, `cmd/evolve_cmd.rs`) — prints suggestions; `--create` writes draft workflows via `workflow_store`. This is the existing CLI-pull surface and the accept path we reuse.
- **Companion surface (`mur-gui-core`)** — `companion_bridge/{scanner,watcher}.rs` (filesystem bridge), `expression.rs` (speech bubbles/expressions), idle triggers (C6), `voice/dnd.rs` (DND/Focus detection). The nudge surfaces here.
- **Recording lifecycle** — `mur-in` starts recording; `mur-out` stops and extracts. Session-end (`mur-out`) is where mining naturally runs.

---

## 3. Signal choice: Emergence now, Co-occurrence later

Emergence and co-occurrence capture **different** things:

| | Emergence | Co-occurrence |
|---|---|---|
| Semantics | **Ordered, procedural** recurrence (a repeated tool/command sequence across ≥3 sessions) | **Unordered, associative** ("these items tend to appear together") |
| Natural product | A **replayable workflow** (has step order) | A **cluster / link** of related items (a relatedness graph) |
| Data feed | Session transcript — **durable** | `record_cooccurrence(&injected_patterns)` — **feed removed by the migration** |

**Decision: v1 is emergence-only.** Reasons:
1. **Co-occurrence's data feed is being deleted.** Building the nudge on it would repeat the "built on a soon-removed model" mistake; reviving it post-migration requires repointing it to a new signal (skill co-retrieval / workflow co-run) and is partially blocked on the migration.
2. **Higher fidelity for workflows.** "Save what you *repeated*" = an ordered procedure = emergence. An unordered cluster must invent step order/trigger, weakening the accept→run experience.
3. **Co-occurrence's honest long-term role is *note/skill linking*** (a "these notes are related — link them?" nudge), not workflow synthesis. It returns post-migration in that role.
4. **Zero architectural cost to defer.** The nudge layer consumes a generic `WorkflowCandidate` via a pluggable `CandidateSource`; adding a second source later is purely additive.

---

## 4. Architecture (unit boundaries)

Five units, each with one responsibility and a clear interface. The mining→ledger→emit logic lives in **`mur-core`** (surface-agnostic and testable without a GUI); only rendering lives in **`mur-gui-core`**.

```
mur-out (session end)
   │
   ▼
[1] CandidateSource (core)         emergence → Vec<WorkflowCandidate>
   │
   ▼
[2] NudgeLedger (core)             dedup / frequency-cap / snooze  → keep only actionable
   │
   ▼
[3] NudgeEmitter (core)            write pending nudge(s) for surfaces to consume
   │
   ├───────────────► [5] CLI fallback (core)  `mur suggest` lists pending; accept = existing --create
   ▼
[4] Companion surface (gui-core)   idle pickup → DND gate → speech bubble (accept/dismiss/snooze)
   │
   ▼  decision written back
[3] NudgeEmitter applies decision  accept → create draft workflow (existing path); else update ledger
```

### Unit 1 — `CandidateSource` (core)

```rust
// mur-core/src/nudge/candidate.rs
pub struct WorkflowCandidate {
    pub id: String,            // stable hash of the behavior fingerprint cluster
    pub title: String,         // human one-liner ("Run tests then commit then push")
    pub suggested_name: String,// kebab name for the draft workflow
    pub steps_preview: Vec<String>,
    pub session_count: usize,
    pub evidence_session_ids: Vec<String>,
}

pub trait CandidateSource {
    fn candidates(&self, threshold: usize) -> anyhow::Result<Vec<WorkflowCandidate>>;
}
```

v1 ships one impl, `EmergenceSource`, mapping `EmergentCandidate` → `WorkflowCandidate`. `id` = a stable hash over the cluster's normalized fingerprint so the same recurring behavior maps to the same id across runs (essential for dedup).

### Unit 2 — `NudgeLedger` (core)

Persistent anti-nag brain at `~/.mur/nudges.json`:

```rust
// mur-core/src/nudge/ledger.rs
#[derive(Serialize, Deserialize)]
pub enum NudgeState { Surfaced, Accepted, Dismissed, Snoozed { until: String /* RFC3339 */ } }

#[derive(Serialize, Deserialize)]
pub struct NudgeRecord { pub state: NudgeState, pub last_ts: String, pub surface_count: u32 }

pub struct NudgeLedger { /* candidate_id -> NudgeRecord */ }
```

`filter_actionable(candidates, now, daily_cap)` returns only candidates that are: not `Accepted`, not `Dismissed`, not currently `Snoozed`, and within the per-day surface cap (default config `nudge.daily_cap = 2`). Thresholds/caps live in config (Mandatory Rule #1 — no hardcoding).

### Unit 3 — `NudgeEmitter` (core)

- `emit_pending(actionable: &[WorkflowCandidate])` — writes a pending-nudge record (one file per candidate or a single `pending.json`) under a bridge-watched path (e.g. `~/.mur/companion/nudges/`), and marks each `Surfaced` in the ledger with `surface_count += 1`.
- `apply_decision(candidate_id, decision)` — `Accept` → create a draft workflow via the existing path (see §5) and mark `Accepted`; `Dismiss` → `Dismissed`; `Snooze` → `Snoozed { until = now + config.nudge.snooze_days }`. Removes the pending record.

### Unit 4 — Companion surface (`mur-gui-core`)

- The `companion_bridge` watcher gains awareness of the nudges path. On an **idle** tick (reuse C6 idle), it reads pending nudges, **checks DND/Focus** via `voice/dnd.rs`, and if clear, renders a speech bubble through `expression.rs` with three actions: **Save it** / **Not now** (snooze) / **No thanks** (dismiss).
- The chosen action is written back to the bridge; the agent/daemon side calls `NudgeEmitter::apply_decision`. If DND is active, the nudge stays pending (re-checked next idle).

### Unit 5 — CLI fallback (core)

- `mur suggest` (existing) gains a section listing pending nudges (id, title, session_count) so headless / no-GUI users still see them; accepting reuses `mur suggest --create`. No new top-level command in v1 (keep surface small); a thin `--accept <id>` / `--dismiss <id>` on `suggest` is the minimal wiring.

---

## 5. Accept action (migration-independent)

Accept reuses the **existing** draft-workflow creation path used by `mur suggest --create` / `cmd/evolve_cmd.rs` (`workflow_store`), producing a draft the user runs with `mur run <name>`. This is independent of the Pattern→Skill migration (workflows are not removed by it; only patterns are). **Forward-compat note:** once the migration lands and workflows become `category: workflow` skills, the accept path repoints to skill creation — a one-line change behind the same `apply_decision` interface; not a v1 concern.

---

## 6. Data flow

1. `mur-out` finishes extraction; mining (`detect_emergent`) runs as it does today.
2. `EmergenceSource::candidates(threshold)` → `Vec<WorkflowCandidate>`.
3. `NudgeLedger::filter_actionable` drops accepted/dismissed/snoozed/over-cap.
4. `NudgeEmitter::emit_pending` writes pending nudges + marks `Surfaced`.
5. Companion idle tick reads pending → DND gate → speech bubble.
6. User picks Save / Not now / No thanks → decision written back.
7. `NudgeEmitter::apply_decision` creates the draft (accept) or updates the ledger, and clears the pending record.

---

## 7. Anti-nag guarantees

- **Dismissed never re-surfaces** (ledger `Dismissed` is terminal for that candidate id).
- **Snooze** hides for `config.nudge.snooze_days` (default 7), then becomes actionable again.
- **Daily cap** (`config.nudge.daily_cap`, default 2) bounds interruptions.
- **DND/Focus respected** — never bubbles during DND; stays pending.
- **Stable ids** — the same recurring behavior is one ledger entry, so re-detection does not multiply nudges.

---

## 8. Testing

- **Candidate mapping:** an `EmergentCandidate` with N sessions maps to a `WorkflowCandidate` with a stable `id` (same input → same id; reordered fingerprints that normalize equal → same id).
- **Ledger dedup:** a `Dismissed` candidate is excluded by `filter_actionable` forever; an `Accepted` one too.
- **Snooze:** snoozed candidate is excluded until `until`, then included.
- **Daily cap:** with `daily_cap = 2`, the 3rd actionable candidate the same day is withheld.
- **Accept creates a draft:** `apply_decision(id, Accept)` results in a draft workflow via the existing path (assert `workflow_store` contains it), idempotent on repeat.
- **DND suppression:** with DND active, the companion leaves the nudge pending (unit-test the gate function in `mur-gui-core`).
- **CLI fallback:** `mur suggest` lists pending nudges; `--accept <id>` creates the draft and marks `Accepted`.

---

## 9. Sequencing & honesty caveats

- **Migration-independent.** Emergence (transcript-based) and the workflow-draft path are decoupled from the Pattern→Skill migration; v1 is **not blocked**.
- **Co-occurrence deferred** to post-migration, repointed to a durable signal (skill co-retrieval / workflow co-run) and likely surfaced as a separate "link related notes?" nudge rather than workflow synthesis (§3).
- **No marketplace / no DAG editor** (umbrella §5/§7).
- A **read-only** visual viewer of a mined workflow is explicitly out of scope (optional future, per umbrella §5).

---

## 10. Open questions (for the plan)

- Pending-nudge wire format and exact bridge path under `~/.mur/companion/` — align with how `companion_bridge/scanner.rs` already discovers files.
- Whether the daemon or the `mur-out` process is the one that writes pending nudges (depends on which runs at session end in the current recording lifecycle) — confirm during planning.
- Bubble copy / action labels (microcopy) — pick during planning; default to "Save it / Not now / No thanks".
