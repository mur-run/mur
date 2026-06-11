# Workflow Engine W3a — Pattern Pipeline Removal & Repoint (v2 P1a+P1b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Execute workflow-engine v2 spec phases P1a+P1b — export the 160 dead patterns, delete the emergence/fingerprint pipeline, remove dead background spawns (`mur evolve`/`mur emerge`/`mur learn` do not exist as commands), and repoint every remaining Pattern consumer (nudge, skill-suggest, `mur context`, `mur sync`, server session routes) to skills/workflows/harvest-proposals.

**Architecture:** Deletion-led refactor. The hook injection path ALREADY serves skills+workflows only (`cmd/hook.rs` uses `load_skill_candidates` + `score_and_rank_generic`); the `Retrievable` trait ALREADY exists (`retrieve/scoring.rs:26`). What remains: (1) a one-shot `mur migrate --patterns` exporting `~/.mur/patterns/*.yaml` → markdown then deleting them + `fingerprints.jsonl`; (2) `capture/emergence.rs` deletion with its 6 call sites repointed to the W2 harvest-proposal inbox (the new `CandidateSource`); (3) `mur context` + `context_api` switched from Pattern scoring to the same skills+workflows path as hooks; (4) `mur sync` content generation switched from patterns to Stable+ skills; (5) Pattern-typed scorer wrappers (`ScoredPattern`, `score_and_rank_hybrid*`) deleted once callers are gone.

**Tech Stack:** Rust edition 2024. Tests via `cargo nextest run -p mur-core` (NOT plain cargo test). Known pre-existing env-flaky failures: `conversations::summarize::rollup::*` (host LanceDB dim mismatch) — not a gate.

**Spec:** `docs/superpowers/specs/2026-05-28-mur-workflow-engine-design-v2.md` (P1a, P1b, "Decision: remove Pattern", amendment A1) + `docs/superpowers/specs/2026-06-11-mur-ambient-capture-and-harvest-design.md` §3.4. **W3b (P2–P4: DAG schema, run-ledger, executor, lifecycle wire-up) is a separate follow-up plan.**

**Investigation facts this plan relies on (verified 2026-06-11):**
- 160 yamls in `~/.mur/patterns/`; `~/.mur/fingerprints.jsonl` exists.
- `mur evolve`, `mur emerge`, `mur learn` are NOT in the CLI (`./target/debug/mur evolve --help` → "unrecognized subcommand") yet are spawned by `spawn_background_pipeline` (`cmd/hook.rs:~330-350`) and `cmd_out_execute("analyze")` (`cmd/session.rs`, runs `learn extract --file … --llm`). All three spawns fail silently today.
- `emergence` callers: `cmd/session.rs` (cmd_session_stop M3b.3 block + legacy block in cmd_out), `cmd/skill_suggest.rs`, `capture/mod.rs`, `server/sessions.rs` (loads fingerprints into the session-detail response), `nudge/{candidate,mod}.rs` (`EmergenceSource`).
- Pattern-typed scoring callers: `cmd/context.rs`, `context_api/mod.rs` only (verify.rs hit is a test-fixture string).
- `evolve/decay.rs` no longer exists; evolve/ = {cooccurrence, feedback, skill_evolve, telemetry_reader}.
- `cmd_suggest` (`cmd/workflow.rs:617`) Part 1 mines patterns via `CooccurrenceMatrix` — degrade to workflows-only.
- W2 shipped `harvest::proposal::{Proposal, pending_in_dir, inbox_dir}` — the replacement candidate feed.

---

## File Structure

- Create: `mur-core/src/cmd/migrate_patterns.rs` — export+delete one-shot
- Modify: `mur-core/src/cli/mod.rs`, `mur-core/src/dispatch.rs` — hidden `Migrate` command
- Create: `mur-core/src/nudge/harvest_source.rs` — `HarvestProposalSource: CandidateSource`
- Modify: `mur-core/src/nudge/{mod.rs,candidate.rs}` — drop `EmergenceSource`
- Modify: `mur-core/src/cmd/session.rs` — M3b.3 block → harvest-proposal nudges; remove fingerprint blocks; fix dead `learn extract` spawn
- Modify: `mur-core/src/cmd/hook.rs` — remove dead `evolve`/`emerge` spawns
- Modify: `mur-core/src/cmd/skill_suggest.rs` — candidates from harvest proposals
- Modify: `mur-core/src/server/sessions.rs` — drop fingerprint loading (serve empty)
- Delete: `mur-core/src/capture/emergence.rs`; Modify `capture/mod.rs`
- Modify: `mur-core/src/cmd/context.rs`, `mur-core/src/context_api/mod.rs` — skills+workflows retrieval
- Modify: `mur-core/src/inject/sync.rs`, `mur-core/src/cmd/sync_cmd.rs` — sync content from skills; ignore cloud pattern payloads
- Modify: `mur-core/src/retrieve/scoring.rs` — delete `ScoredPattern` + `score_and_rank_hybrid*` wrappers
- Modify: `mur-common/src/pattern.rs` — remove decay helpers IF no callers remain (grep-gated)
- Modify: docs (runtime-overview, CLAUDE.md memory-pipeline section)

## Execution rules

- Each task: edit → `cargo nextest run -p mur-core <filter>` → `cargo clippy -p mur-core --bin mur` clean → commit.
- Deletion tasks MUST end with a `grep` proving zero remaining references (command given per task).
- Where this plan says "read the function first", the executor reads the current body before editing — line numbers drift.

---

### Task 1: `mur migrate --patterns` — export then delete

**Files:** Create `mur-core/src/cmd/migrate_patterns.rs`; modify `cli/mod.rs`, `dispatch.rs`, `cmd/mod.rs`

- [ ] Step 1: Write `migrate_patterns.rs` with `_in_dir` core + tests:

```rust
//! P1a one-shot: export every pattern to markdown under
//! `~/.mur/exported-patterns/`, then delete `patterns/` and
//! `fingerprints.jsonl`. Never hard-delete user data without a backup path.

use anyhow::{Context, Result};
use std::path::Path;

pub struct MigrateReport {
    pub exported: usize,
    pub deleted_fingerprints: bool,
}

pub fn migrate_patterns_in(mur_dir: &Path) -> Result<MigrateReport> {
    let patterns_dir = mur_dir.join("patterns");
    let export_dir = mur_dir.join("exported-patterns");
    let mut exported = 0usize;

    if patterns_dir.exists() {
        std::fs::create_dir_all(&export_dir)?;
        for entry in std::fs::read_dir(&patterns_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("pattern")
                .to_string();
            let yaml = std::fs::read_to_string(&path)?;
            let md = format!(
                "# {stem}\n\nExported from `patterns/{stem}.yaml` by `mur migrate --patterns` (workflow-engine v2 P1a).\n\n```yaml\n{yaml}\n```\n"
            );
            std::fs::write(export_dir.join(format!("{stem}.md")), md)
                .context("write exported pattern")?;
            exported += 1;
        }
        std::fs::remove_dir_all(&patterns_dir)?;
    }

    let fp = mur_dir.join("fingerprints.jsonl");
    let deleted_fingerprints = fp.exists();
    if deleted_fingerprints {
        std::fs::remove_file(&fp)?;
    }
    Ok(MigrateReport { exported, deleted_fingerprints })
}

pub fn cmd_migrate_patterns() -> Result<()> {
    let report = migrate_patterns_in(&crate::paths::mur_root(None))?;
    eprintln!(
        "✓ exported {} pattern(s) to ~/.mur/exported-patterns/; fingerprints.jsonl removed: {}",
        report.exported, report.deleted_fingerprints
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_then_deletes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pdir = tmp.path().join("patterns");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("a.yaml"), "name: a\n").unwrap();
        std::fs::write(tmp.path().join("fingerprints.jsonl"), "{}\n").unwrap();

        let r = migrate_patterns_in(tmp.path()).unwrap();
        assert_eq!(r.exported, 1);
        assert!(r.deleted_fingerprints);
        assert!(!pdir.exists());
        assert!(tmp.path().join("exported-patterns").join("a.md").exists());
    }

    #[test]
    fn idempotent_when_nothing_to_do() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = migrate_patterns_in(tmp.path()).unwrap();
        assert_eq!(r.exported, 0);
        assert!(!r.deleted_fingerprints);
    }
}
```

- [ ] Step 2: Register: `pub mod migrate_patterns;` in `cmd/mod.rs`; hidden CLI verb in `cli/mod.rs` `Commands`:

```rust
    /// One-shot data migrations (workflow-engine v2)
    #[command(hide = true)]
    Migrate {
        /// Export ~/.mur/patterns to markdown, then delete patterns + fingerprints
        #[arg(long)]
        patterns: bool,
    },
```

and in `dispatch.rs`:

```rust
        Commands::Migrate { patterns } => {
            if patterns {
                cmd::migrate_patterns::cmd_migrate_patterns()?;
            } else {
                eprintln!("Nothing to do. Try: mur migrate --patterns");
            }
        }
```

- [ ] Step 3: `cargo nextest run -p mur-core -E 'test(/migrate_patterns/)'` → PASS; clippy clean.
- [ ] Step 4: Commit `feat(migrate): export-then-delete one-shot for legacy patterns (v2 P1a)`.

---

### Task 2: `HarvestProposalSource` replaces `EmergenceSource`

**Files:** Create `nudge/harvest_source.rs`; modify `nudge/mod.rs`, `nudge/candidate.rs`

- [ ] Step 1: Read `nudge/candidate.rs` fully (it defines `WorkflowCandidate`, `CandidateSource`, `EmergenceSource`, `from_emergent`).
- [ ] Step 2: Create `nudge/harvest_source.rs`:

```rust
//! Candidate source backed by the W2 harvest-proposal inbox — replaces the
//! emergence/fingerprint miner (workflow-engine v2 P1a; ambient-capture spec §3.2).

use anyhow::Result;
use std::path::PathBuf;

use super::candidate::{CandidateSource, WorkflowCandidate};
use crate::harvest::proposal::{self, Proposal};

pub struct HarvestProposalSource {
    inbox_dir: PathBuf,
}

impl HarvestProposalSource {
    pub fn new(inbox_dir: PathBuf) -> Self {
        Self { inbox_dir }
    }

    pub fn default_source() -> Self {
        Self::new(proposal::inbox_dir())
    }
}

fn to_candidate(p: &Proposal) -> WorkflowCandidate {
    WorkflowCandidate {
        id: format!("harvest:{}", p.id),
        title: p.title.clone(),
        suggested_name: p.suggested_name.clone(),
        steps_preview: p.steps.iter().take(5).cloned().collect(),
        session_count: 1,
        evidence_session_ids: vec![p.id.clone()],
    }
}

impl CandidateSource for HarvestProposalSource {
    fn candidates(&self, _threshold: usize) -> Result<Vec<WorkflowCandidate>> {
        Ok(proposal::pending_in_dir(&self.inbox_dir)?
            .iter()
            .map(to_candidate)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::proposal::{Proposal, ProposalStatus, save_in_dir};

    #[test]
    fn maps_pending_proposals_to_candidates() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_in_dir(
            tmp.path(),
            &Proposal {
                id: "s1".into(),
                title: "Deploy api".into(),
                suggested_name: "deploy-api".into(),
                steps: vec!["cargo build".into()],
                event_count: 5,
                duration_secs: 60,
                created_at: "2026-06-11T00:00:00Z".into(),
                status: ProposalStatus::Pending,
                similar_to: None,
            },
        )
        .unwrap();
        let src = HarvestProposalSource::new(tmp.path().to_path_buf());
        let cands = src.candidates(3).unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].id, "harvest:s1");
        assert_eq!(cands[0].suggested_name, "deploy-api");
    }
}
```

> Field names of `WorkflowCandidate` must match what Step 1 found — adjust the constructor if the real struct differs (e.g. extra fields get sensible defaults).

- [ ] Step 3: In `nudge/mod.rs` add `pub mod harvest_source;` (+ re-export if `EmergenceSource` was re-exported). Delete `EmergenceSource` and `WorkflowCandidate::from_emergent` from `candidate.rs` (their only consumers are updated in Tasks 3–4).
- [ ] Step 4: `cargo nextest run -p mur-core -E 'test(/nudge/)'`; clippy. Commit `feat(nudge): harvest-proposal candidate source replaces emergence`.

---

### Task 3: Repoint `cmd_session_stop` + clean `cmd_out` + fix dead spawns

**Files:** `cmd/session.rs`, `cmd/hook.rs`

- [ ] Step 1: In `cmd_session_stop` (read it first; the M3b.3 block mines fingerprints → `detect_emergent` → nudges), replace the whole fingerprint-mining + emergence block with:

```rust
            // Nudge hook: surface pending harvest proposals (replaces emergence mining).
            let source = crate::nudge::harvest_source::HarvestProposalSource::default_source();
            if let Ok(nudge_candidates) =
                crate::nudge::candidate::CandidateSource::candidates(&source, 0)
                && let Ok(surfaced) = record_nudges_for_candidates(&nudge_candidates)
                && !surfaced.is_empty()
            {
                // (keep the existing eprintln + companion delivery body unchanged)
            }
```

  Keep the existing companion-delivery inner body verbatim; only the candidate *source* changes. Also delete the `analyze && recording_path.exists()` fingerprint-extraction block above it.
- [ ] Step 2: In `cmd_out` (W2 version), delete the legacy fingerprint-extraction block (`use crate::capture::emergence::…` inside the `stop()` branch).
- [ ] Step 3: In `cmd_out_execute("analyze")`: the spawn of `mur learn extract --file … --llm` targets a nonexistent command. Replace with the working path — spawn `mur session export <id> --format markdown --analyze`? **No** — read `cmd_session_export`'s analyze flag first; if it runs `extract_llm` synchronously, call that same function directly here instead of spawning. The acceptance criterion: `mur out --action analyze` must do real work (exit 0 and produce output), no silent dead spawn.
- [ ] Step 4: In `cmd/hook.rs::spawn_background_pipeline`, delete the `mur evolve` and `mur emerge` spawns (commands don't exist). Keep `mur sync --quiet`.
- [ ] Step 5: `cargo nextest run -p mur-core -E 'test(/session::tests/)'`; clippy; smoke: `echo '{}' | ./target/debug/mur hook stop --tool claude` exits 0. Commit `fix(session): repoint nudges to harvest proposals; remove dead evolve/emerge/learn spawns`.

---

### Task 4: Repoint `mur skill suggest` + server sessions route

**Files:** `cmd/skill_suggest.rs`, `server/sessions.rs`

- [ ] Step 1: Read `cmd/skill_suggest.rs` (uses `extract_fingerprints` over recordings + `detect_emergent`). Replace the mining stage: candidates now come from `HarvestProposalSource::default_source().candidates(opts.threshold)`; keep everything downstream (dedup vs existing skills, draft creation, output format) unchanged. Delete the emergence imports.
- [ ] Step 2: In `server/sessions.rs`, the session-detail response loads `crate::capture::emergence::load_fingerprints()` into a `fingerprints: Vec<BehaviorFingerprint>` field. Keep the response field (Hub compatibility) but always serve `vec![]`, and drop the emergence import.
- [ ] Step 3: `cargo nextest run -p mur-core -E 'test(/skill_suggest|server::sessions/)'`; clippy. Commit `feat(suggest): skill suggestions sourced from harvest proposals`.

---

### Task 5: Delete `capture/emergence.rs`

**Files:** Delete `capture/emergence.rs`; modify `capture/mod.rs`

- [ ] Step 1: `grep -rn "emergence" mur-core/src --include="*.rs" | grep -v "capture/emergence.rs"` — must show ONLY `capture/mod.rs` (the `pub mod emergence;`). If anything else remains, fix it first (Tasks 2–4 missed a site).
- [ ] Step 2: Delete the file; remove `pub mod emergence;` (and any re-exports) from `capture/mod.rs`.
- [ ] Step 3: Full check: `cargo nextest run -p mur-core -E 'test(/capture|nudge|session::tests/)'` PASS; `grep -rn "extract_fingerprints\|detect_emergent\|EmergenceSource" mur-core/src` → empty. Commit `feat(capture)!: remove emergence/fingerprint pipeline (v2 P1a)`.

---

### Task 6: `mur context` + `context_api` → skills+workflows

**Files:** `cmd/context.rs`, `context_api/mod.rs`

- [ ] Step 1: Read both files. They are the last real users of `score_and_rank_hybrid*`/`ScoredPattern`. Re-implement retrieval the same way `cmd_hook_prompt` does (this is the single-code-path goal of P1b):

```rust
    let mur_dir = mur_common::trust::mur_home();
    let candidates = crate::retrieve::skill_candidates::load_skill_candidates(
        &mur_dir.join("skills"),
        &mur_dir,
    )
    .unwrap_or_default();
    let workflows = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?
        .list_all()?;
    let scored: Vec<_> = crate::retrieve::scoring::score_and_rank_generic(&query, candidates)
        .into_iter()
        .filter(|s| {
            s.item.stats.lifecycle_state != mur_common::skill::stats::LifecycleState::Archived
        })
        .collect();
    let output = crate::inject::hook::format_skills_for_injection(&scored, &workflows, max_tokens);
```

  Preserve each command's existing flags/output framing (quiet mode, JSON mode if present) — only the source of retrieved items changes. Where the old code attached Pattern-specific details (diagram attachments etc.), drop them; skills carry their own content.
- [ ] Step 2: `cargo nextest run -p mur-core -E 'test(/context/)'`; clippy; smoke: `MUR_HOME=$(mktemp -d) ./target/debug/mur context "deploy"` exits 0 with empty-state message. Commit `feat(context): retrieve skills+workflows — one retrieval path with hooks (v2 P1b)`.

---

### Task 7: `mur sync` content from skills; ignore cloud pattern payloads

**Files:** `inject/sync.rs`, `cmd/sync_cmd.rs`

- [ ] Step 1: Read `inject/sync.rs::generate_sync_content` (writes top patterns into tool-config files, `max_patterns: 20`). Change the content source to Stable-or-better skills (reuse `load_skill_candidates`, filter `lifecycle_state` ∈ {Stable, Canonical}, cap by the same `max_patterns` field — rename field to `max_items` ONLY if the config key stays serde-aliased; otherwise keep the name). Workflows section stays as-is if present.
- [ ] Step 2: In `cmd/sync_cmd.rs` device-sync pull path (`for p in &response.patterns { … }` writing into `patterns_dir`): stop writing pattern payloads to disk (log `tracing::debug!("ignoring {} legacy cloud patterns", …)`); keep the rest of the protocol handling intact so old servers still sync skills/workflows.
- [ ] Step 3: Also update the `mur sync` git-auto-commit line that stages `patterns/` (sync_cmd.rs ~:595) to stop referencing the deleted dir (keep `workflows/`, `config.yaml`, add `skills/`).
- [ ] Step 4: `cargo nextest run -p mur-core -E 'test(/sync/)'`; clippy. Commit `feat(sync): tool-config sync sources skills; ignore legacy cloud pattern payloads`.

---

### Task 8: Delete Pattern-typed scorer wrappers

**Files:** `retrieve/scoring.rs` (+ any straggler imports)

- [ ] Step 1: `grep -rn "score_and_rank_hybrid\|ScoredPattern\|score_and_rank_with_scope\b" mur-core/src --include="*.rs" | grep -v retrieve/scoring.rs` — after Task 6 this must be empty (the verify.rs hit is inside a test string literal; update that fixture string to reference `score_and_rank_generic` instead so the grep gate is clean).
- [ ] Step 2: In `retrieve/scoring.rs` delete `ScoredPattern`, `score_and_rank`, `score_and_rank_hybrid_with_config`, `score_and_rank_hybrid_with_scope`, `score_and_rank_hybrid_with_scope_and_config`, and any Pattern-only helpers they used. KEEP: `Retrievable`, `Scored<T>`, `score_and_rank_generic`, scope/config machinery used by the generic path. Pattern keeps its `impl Retrievable` (transitional, per spec).
- [ ] Step 3: `cargo nextest run -p mur-core -E 'test(/retrieve/)'`; `cargo clippy --workspace` clean. Commit `refactor(retrieve)!: drop Pattern-typed scorer wrappers; Retrievable generic is the only path`.

---

### Task 9: `cmd_suggest` co-occurrence degrades to workflows-only + decay-helper sweep

**Files:** `cmd/workflow.rs` (cmd_suggest Part 1), `mur-common/src/pattern.rs`

- [ ] Step 1: Read `cmd_suggest` Part 1 (`CooccurrenceMatrix` over `pattern_store.list_all()`). With `patterns/` deleted the store returns empty — verify the code path tolerates empty input (no panic, no noisy warning). If it requires patterns, feed it workflows only or skip Part 1 when patterns are absent, printing nothing. The nudge part (Part 2) keeps working via Task 2's source.
- [ ] Step 2: `grep -rn "decay_half_life\|decay_half_life_days" mur-core/src mur-common/src --include="*.rs"` — if the only hits are the definitions in `mur-common/src/pattern.rs` (+ their tests), delete those helpers and fields' *logic* uses; keep serde fields on `KnowledgeBase` (Workflow embeds it — YAML compat). If real callers remain, leave them and note in the PR body (transitional Pattern).
- [ ] Step 3: `cargo nextest run -p mur-core -p mur-common`; clippy workspace. Commit `refactor(evolve): suggest degrades gracefully without patterns; prune dead decay helpers`.

---

### Task 10: Docs + final verification + run the migration

- [ ] Step 1: Update `docs/architecture/runtime-overview.md` memory-pipeline wording and `CLAUDE.md` §Memory Pipeline: patterns are exported/removed; retrieval = skills + workflows; capture = ambient (W1/W2); note `mur migrate --patterns`.
- [ ] Step 2: Full gates: `cargo fmt --check`, `cargo clippy --workspace`, `cargo nextest run -p mur-core -p mur-common` (rollup env-flaky pair excepted).
- [ ] Step 3: E2E smoke in a temp MUR_HOME: seed 2 fake pattern yamls + fingerprints.jsonl → `mur migrate --patterns` → exported-patterns/*.md exist, patterns/ gone → `mur context "anything"` exits 0 → `echo '{}' | mur hook stop --tool claude` exits 0.
- [ ] Step 4: Commit docs; push; PR titled `feat!: remove pattern pipeline, repoint to skills/harvest (workflow-engine v2 P1a+P1b / W3a)`. PR body MUST tell users to run `mur migrate --patterns` once after upgrading (or note that empty patterns dir is harmless).

## Out of scope (W3b plan)
- P2: ProcedureStep DAG fields + JSON Schema emit + run-ledger + stats reducer
- P3: unified DAG executor (`needs_approval`, `--yes`, rank-parallel)
- P4: lifecycle wire-up (Broken fast-path, thresholds to config, Provenance + curation gate per amendment A1)
- P7 migration `workflows/ → skills/`; server-side anything

## Self-review notes
- P1a coverage: export ✓(T1), emergence/fingerprints delete ✓(T2-T5), noise_filter KEPT (untouched — used by W2 gate) ✓.
- P1b coverage: Retrievable already exists; single retrieval path ✓(T6), sync repoint ✓(T7), wrapper deletion ✓(T8). hook.rs needed no repoint (already skills-only — verified).
- Amendment A1 untouched here (extraction stays advisory; no auto-promotion added).
- Every deletion task carries a grep gate; `mur out --action analyze` regains a working implementation (T3.3).
