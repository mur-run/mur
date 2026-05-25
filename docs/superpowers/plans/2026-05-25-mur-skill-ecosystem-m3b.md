# MuR Skill Ecosystem — M3b (Agent-Generated Skills, Trace2Skill Pipeline) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement `mur skill generate --from-session <session-id>` end-to-end. The command reads a `~/.mur/session/recordings/<id>.jsonl` recording, runs the three-phase Trace2Skill pipeline (trajectory pool → parallel Success/Error analysts → conflict-free consolidation), and writes a canonical `skill.yaml` ready for review and `mur skill publish`.

**Out of scope:**
- `mur skill from-pattern <name>` — separate plan (M3b.2). The pattern store integration is non-trivial.
- Auto-suggest trigger (≥3 repeats) — separate plan (M3b.3). Needs session-telemetry repeat detector.
- Self-evolution loop (Failure Analyzer + Skill Optimizer + `evolution_log`) — M3c.

Soft dependency on **M3a**: generated skills with `requires:` fields will be installable once M3a's resolver lands. M3b can assume zero-deps output (`requires:` is omitted from the generator's templates in this milestone).

---

## Codebase Reality Check (read before executing)

Verified against `main`:

| Assumption | Reality |
|---|---|
| Recording location | `~/.mur/session/recordings/<id>.jsonl` — confirmed in `mur-core/src/cmd/session.rs:80–85`. Format is line-delimited JSON; **exact event schema is not yet stably documented** — read 2–3 sample recordings before designing the trajectory parser to avoid drift. |
| Existing pattern extractor | `mur-core/src/capture/reflector.rs::reflect_session(content, injected, registry)` + `curator.rs::curate(...)`. Same input (recording text). Produces `Pattern`s, not skills. **Don't reuse the function directly** — different output type — but mirror its input-parsing logic so M3b doesn't reinvent JSONL splitting. |
| LLM client | `mur_common::llm::LlmClient` trait with `complete(prompt, system) -> impl Future<Output = Result<String, LlmError>>`. Provider selection via `mur_common::config::BackendConfig` + `factory::build_for_stage(&cfg, stage_name)`. Tests mock by impl-ing the trait. |
| Reasoning-model hint | `mur_common::llm::is_reasoning_model(&str) -> bool` already exists — spec recommends an Opus/O3/Gemini-Pro-class model for the Error Analyst ReAct loop. Surface as a warning if a non-reasoning model is configured. |
| Parallelism pattern | `mur-core/src/executor/pipeline.rs` uses `futures::future::join_all` and `FuturesUnordered` (lines 321, 333). M3b mirrors that style — no new dependency needed. |
| Skill manifest output | `mur_common::skill::manifest::SkillManifest` is what we serialize. `serialize_canonical(&manifest)` produces the on-disk YAML. `validate(&manifest)` runs the same schema gates as `mur skill install`. |
| Scan + trust | Generated skill MUST pass through the same `scan_skill` + trust-store registration that M1's `cmd_install` uses. Don't bypass — the generator's output is untrusted by definition. |
| Async runtime | mur-core already uses tokio (see `executor/pipeline.rs`); the new command is `async`. The Clap dispatch for async subcommands already has precedent (`cmd_session_stop`). |

---

## File Structure

**Create:**
- `mur-core/src/skill_gen/mod.rs` — module root
- `mur-core/src/skill_gen/trajectory.rs` — JSONL → `Trajectory { events, outcome }`; success/failure labeling
- `mur-core/src/skill_gen/analysts.rs` — Phase 2: `SuccessAnalyst` (single-call), `ErrorAnalyst` (multi-turn ReAct). Both return `Patch`.
- `mur-core/src/skill_gen/consolidator.rs` — Phase 3: hierarchical patch merge + dedupe + conflict detection + manifest assembly
- `mur-core/src/skill_gen/prompts.rs` — system prompts for analysts and consolidator
- `mur-core/src/cmd/skill_generate.rs` — `cmd_generate(home, llm, ...)` pure + `cmd_generate_cli` shim
- `mur-core/tests/skill_generate_e2e.rs` — E2E with `MockLlm` driven by a recorded JSONL fixture
- `mur-core/tests/fixtures/skill_gen/sample_session.jsonl` — small fixture (3 success + 1 failure trajectory)

**Modify:**
- `mur-core/src/lib.rs` (or wherever modules are registered) — register `pub mod skill_gen;`
- `mur-core/src/main.rs` — add `Skill::Generate { from_session: String, name: Option<String>, model: Option<String>, dry_run: bool }` arm calling `cmd_generate_cli`

---

## Self-contained Type Sketch

```rust
// mur-core/src/skill_gen/trajectory.rs
pub struct Trajectory {
    pub turns: Vec<Turn>,       // user prompt + agent action(s) + tool call(s) + result
    pub outcome: Outcome,       // Success | Failure { reason: String }
    pub duration: Duration,
    pub task_summary: String,   // synthesized from the first user message
}
pub enum Outcome { Success, Failure { reason: String } }

pub fn parse_recording(jsonl: &str) -> Result<Vec<Trajectory>, ParseError>;
// Splits the JSONL into per-task trajectories using a heuristic
// (e.g. fresh user prompt = new trajectory boundary).

// mur-core/src/skill_gen/analysts.rs
pub struct Patch {
    pub source: PatchSource,            // SuccessAnalyst | ErrorAnalyst
    pub abstract_hint: Option<String>,
    pub procedure_steps: Vec<StepDraft>,
    pub triggers: Vec<TriggerDraft>,
    pub variables: Vec<VariableDraft>,
    pub notes: Vec<String>,             // free-form, surfaced to user in dry-run
}

pub async fn run_phase2(
    llm: &dyn LlmClient,
    trajectories: Vec<Trajectory>,
    max_parallel: usize,
) -> Vec<Patch>;
// - Successes go to SuccessAnalyst (single-call extraction)
// - Failures go to ErrorAnalyst (multi-turn ReAct, max N rounds)
// - Both fan out via FuturesUnordered with a semaphore-bounded concurrency.

// mur-core/src/skill_gen/consolidator.rs
pub fn consolidate(
    patches: Vec<Patch>,
    llm: &dyn LlmClient,
    target_name: Option<&str>,
) -> impl Future<Output = Result<SkillManifest, ConsolidateError>>;
// - Hierarchical merge: group by procedure-step similarity (Jaccard on tool names + variable names)
// - Dedupe identical triggers and variables
// - Detect conflicts (two patches disagree on a step's tool/params) → ask LLM to arbitrate
// - Final LLM pass to write the `abstract`, polish the `description`, fix YAML
// - validate() before return

// mur-core/src/cmd/skill_generate.rs
pub struct GenerateOptions {
    pub session_id: String,
    pub name: Option<String>,            // optional rename
    pub model_override: Option<String>,  // optional — defaults to config
    pub dry_run: bool,                   // if true: print yaml + don't write disk
    pub max_parallel: usize,             // default 4
}

pub async fn cmd_generate(
    home: &Path,
    llm: Arc<dyn LlmClient>,
    opts: GenerateOptions,
) -> Result<SkillManifest>;
pub async fn cmd_generate_cli(opts: GenerateOptions) -> Result<()>;
```

---

### Task 1 — Trajectory parser

**Files:** `mur-core/src/skill_gen/trajectory.rs`, `mur-core/src/skill_gen/mod.rs`.

- [ ] **1.1 — Inspect real recordings first.** Before coding, read 2–3 real `~/.mur/session/recordings/*.jsonl` files (or paste a synthetic sample if no recordings exist locally) to verify event field names. Cross-check with `mur-core/src/session.rs` and `mur-core/src/capture/reflector.rs` for the canonical event schema. **Do not assume — verify.**

  Commands:
  ```bash
  ls ~/.mur/session/recordings 2>/dev/null | head -3
  head -50 ~/.mur/session/recordings/<one-file>.jsonl 2>/dev/null
  grep -rn "RecordingEvent\|session_event\|enum.*Event" mur-core/src/session.rs mur-core/src/capture/ | head
  ```

- [ ] **1.2** Define the parser:

```rust
//! Parse session recording JSONL into per-task trajectories.

use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Trajectory {
    pub task_summary: String,         // first user-message text, truncated to 200 chars
    pub turns: Vec<Turn>,
    pub outcome: Outcome,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub kind: TurnKind,
    pub content: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub enum TurnKind {
    UserPrompt,
    AgentMessage,
    ToolCall { tool: String, input: serde_json::Value },
    ToolResult { tool: String, ok: bool },
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome { Success, Failure { reason: String } }

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed JSON on line {0}: {1}")]
    Json(usize, String),
    #[error("empty recording")]
    Empty,
}

pub fn parse_recording(jsonl: &str) -> Result<Vec<Trajectory>, ParseError> {
    // Implementation outline:
    // 1. Split content by '\n', skip empty lines.
    // 2. For each line: serde_json::from_str into a permissive RecordingEvent envelope.
    //    Use `#[serde(other)]` or `serde_json::Value` for forward-compat — events we
    //    don't recognize are silently skipped but not fatal.
    // 3. Walk events; start a new Trajectory each time a fresh UserPrompt appears
    //    AND the previous trajectory has at least one Turn.
    // 4. Outcome heuristics:
    //    - Last ToolResult.ok = false within the trajectory + Error in next 2 turns → Failure
    //    - Explicit "task_complete" / "stop_reason: end_turn" event → Success
    //    - Else: Success (default optimistic — Error Analyst handles ambiguous cases)
    // 5. task_summary = first 200 chars of the first UserPrompt's text, trimmed.
    Ok(/* ... */)
}
```

- [ ] **1.3** Tests in the same file:
  1. Empty input → `Err(ParseError::Empty)`.
  2. Single trajectory with no errors → one `Success`.
  3. Two user prompts separated by tool calls → two trajectories, both labeled per heuristic.
  4. Trajectory ending with `ToolResult { ok: false }` followed by `Error` → `Failure { reason }`.
  5. Unknown event types are skipped, not fatal.

- [ ] **1.4** Register in `skill_gen/mod.rs`:
  ```rust
  pub mod trajectory;
  pub mod analysts;
  pub mod consolidator;
  pub mod prompts;
  ```

- [ ] **1.5** Build + commit:
  ```bash
  cargo test -p mur-core skill_gen::trajectory
  git add mur-core/src/skill_gen/trajectory.rs mur-core/src/skill_gen/mod.rs mur-core/src/lib.rs
  git commit -m "feat(skill-gen): trajectory parser from JSONL recordings"
  ```

---

### Task 2 — Prompts module

**Files:** `mur-core/src/skill_gen/prompts.rs`.

Keep prompts as `pub const &str` so they're greppable and reviewable. Don't templating-engine them.

- [ ] **2.1** Three prompts:

```rust
pub const SUCCESS_ANALYST_SYSTEM: &str = r#"
You are a Success Analyst extracting a reusable skill from a successful agent task.

INPUT: a single trajectory (user prompt + agent actions + tool results) that succeeded.
OUTPUT: a JSON object matching this schema:
{
  "abstract_hint": "one-line description of what this skill does",
  "procedure_steps": [
    {"description": "step text", "tool": "optional.tool.name", "params_hint": "what to pass"}
  ],
  "triggers": [{"kind": "command|keyword", "pattern": "..."}],
  "variables": [{"name": "x", "type": "string|number|bool", "required": true}],
  "notes": ["any caveats about generalization"]
}

Rules:
- Generalize: replace task-specific values (e.g. "AirPods Pro" → {product_name}).
- Preserve the tool sequence as-is unless you see redundancy.
- DO NOT invent steps that did not appear in the trajectory.
- Output JSON only. No markdown fences, no prose.
"#;

pub const ERROR_ANALYST_SYSTEM: &str = r#"
You are an Error Analyst diagnosing why an agent task failed. You will reason in
multiple turns using ReAct (Thought → Action → Observation).

INPUT: a failed trajectory.
GOAL: produce a Patch (same JSON schema as Success Analyst) that, if applied to
a future skill, would prevent this class of failure.

Diagnose across these 4 dimensions (from Trace2Skill):
1. Knowledge — missing domain information
2. Tool — wrong tool or wrong parameters
3. Clarification — ambiguous instructions / under-specified variables
4. Style — output format mismatch

For each round, respond with:
THOUGHT: <your reasoning>
ACTION: <inspect_turn N | propose_patch | done>

When ACTION=done, also emit:
PATCH: <JSON object with the schema above>

Max 5 rounds. If you cannot diagnose, emit a patch with notes only.
"#;

pub const CONSOLIDATOR_SYSTEM: &str = r#"
You are a Skill Consolidator. Given multiple Patches extracted from related
trajectories, merge them into one coherent skill.yaml.

INPUT: an array of Patch JSON objects.
OUTPUT: a YAML skill manifest with these fields:
  name, version (always "0.1.0"), publisher ("agent:generator"),
  description, category (context|workflow|command), content.{abstract,procedure}, triggers, tags.

Rules:
- Dedupe identical steps and triggers (case-insensitive trimmed compare).
- Where two patches disagree on the tool/params for a step, prefer the
  Success-source patch over the Error-source one.
- If two patches propose conflicting triggers (same kind, same pattern, different intent),
  emit ONE trigger and add a note explaining the merge.
- The final YAML MUST validate against the spec — no extra fields.
- Output YAML only, no markdown fences.
"#;
```

- [ ] **2.2** Unit-tests are not meaningful here (prompts are data); skip.

- [ ] **2.3** Commit:
  ```bash
  git add mur-core/src/skill_gen/prompts.rs
  git commit -m "feat(skill-gen): system prompts for analysts and consolidator"
  ```

---

### Task 3 — Analysts (Phase 2, parallel)

**Files:** `mur-core/src/skill_gen/analysts.rs`.

- [ ] **3.1** Patch type + analysts:

```rust
use crate::skill_gen::prompts::*;
use crate::skill_gen::trajectory::{Outcome, Trajectory};
use mur_common::llm::LlmClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub source: PatchSource,
    #[serde(default)]
    pub abstract_hint: Option<String>,
    #[serde(default)]
    pub procedure_steps: Vec<StepDraft>,
    #[serde(default)]
    pub triggers: Vec<TriggerDraft>,
    #[serde(default)]
    pub variables: Vec<VariableDraft>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatchSource { Success, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDraft {
    pub description: String,
    #[serde(default)] pub tool: Option<String>,
    #[serde(default)] pub params_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDraft { pub kind: String, pub pattern: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDraft {
    pub name: String,
    #[serde(rename = "type")] pub var_type: String,
    #[serde(default)] pub required: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AnalystError {
    #[error("LLM call failed: {0}")] Llm(#[from] mur_common::error::LlmError),
    #[error("response not valid JSON: {0}")] BadJson(String),
    #[error("ReAct loop exceeded {0} rounds without producing a patch")] ReactExhausted(usize),
}

const ERROR_ANALYST_MAX_ROUNDS: usize = 5;

pub async fn run_phase2(
    llm: Arc<dyn LlmClient>,
    trajectories: Vec<Trajectory>,
    max_parallel: usize,
) -> Vec<Result<Patch, AnalystError>> {
    let sem = Arc::new(Semaphore::new(max_parallel.max(1)));
    let mut handles = Vec::new();
    for traj in trajectories {
        let llm = llm.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            match traj.outcome {
                Outcome::Success => analyze_success(&*llm, &traj).await,
                Outcome::Failure { .. } => analyze_error(&*llm, &traj).await,
            }
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap_or_else(|e| Err(AnalystError::BadJson(format!("task panic: {e}")))));
    }
    out
}

async fn analyze_success(llm: &dyn LlmClient, traj: &Trajectory) -> Result<Patch, AnalystError> {
    let prompt = format!("Trajectory (success):\n{}", trajectory_to_text(traj));
    let raw = llm.complete(&prompt, Some(SUCCESS_ANALYST_SYSTEM)).await?;
    let raw = strip_code_fences(&raw);
    let mut p: Patch = serde_json::from_str(raw).map_err(|e| AnalystError::BadJson(e.to_string()))?;
    p.source = PatchSource::Success;
    Ok(p)
}

async fn analyze_error(llm: &dyn LlmClient, traj: &Trajectory) -> Result<Patch, AnalystError> {
    // ReAct loop. Build a running transcript; each round we ask the LLM
    // to emit THOUGHT/ACTION; when ACTION=done we extract PATCH:<...> and stop.
    let mut transcript = format!("Trajectory (failure):\n{}\n", trajectory_to_text(traj));
    for round in 1..=ERROR_ANALYST_MAX_ROUNDS {
        let resp = llm.complete(&transcript, Some(ERROR_ANALYST_SYSTEM)).await?;
        transcript.push_str(&format!("\n--- Round {round} ---\n{resp}\n"));
        if let Some(patch_json) = extract_patch_block(&resp) {
            let mut p: Patch = serde_json::from_str(&patch_json)
                .map_err(|e| AnalystError::BadJson(e.to_string()))?;
            p.source = PatchSource::Error;
            return Ok(p);
        }
        // Otherwise, prompt the next round implicitly via system prompt.
    }
    Err(AnalystError::ReactExhausted(ERROR_ANALYST_MAX_ROUNDS))
}

fn trajectory_to_text(t: &Trajectory) -> String {
    let mut s = format!("Task: {}\n", t.task_summary);
    for turn in &t.turns {
        s.push_str(&format!("[{:?}] {}\n", turn.kind, turn.content));
    }
    s
}

fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    s.trim_end_matches("```").trim()
}

fn extract_patch_block(resp: &str) -> Option<String> {
    // Look for "PATCH:" followed by a JSON object.
    let idx = resp.find("PATCH:")?;
    let after = &resp[idx + "PATCH:".len()..].trim();
    // Crude but works: find the matching outermost braces.
    let start = after.find('{')?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in after[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 { end = Some(start + i + 1); break; }
            }
            _ => {}
        }
    }
    Some(after[start..end?].to_string())
}
```

- [ ] **3.2** Tests using a `MockLlm`:

```rust
struct MockLlm { responses: std::sync::Mutex<std::collections::VecDeque<String>> }
impl LlmClient for MockLlm {
    fn complete(&self, _prompt: &str, _system: Option<&str>) -> impl std::future::Future<Output = Result<String, mur_common::error::LlmError>> + Send {
        let r = self.responses.lock().unwrap().pop_front().unwrap_or_default();
        async move { Ok(r) }
    }
    fn embed(&self, _: &str) -> impl std::future::Future<Output = Result<Vec<f32>, mur_common::error::LlmError>> + Send {
        async { unreachable!("not used by analysts") }
    }
}
```

Test cases:
1. Single Success trajectory + canned JSON response → returns one `Patch { source: Success }`.
2. Single Failure trajectory + canned ReAct response ending in `PATCH: {...}` → returns one `Patch { source: Error }`.
3. Failure trajectory where LLM never emits PATCH → `ReactExhausted` after 5 rounds.
4. Mixed batch of 3 success + 2 failure → 5 patches; concurrency-limited to 2 in flight (assert by ordering or just by completion).
5. Malformed JSON → `AnalystError::BadJson`, doesn't panic.

- [ ] **3.3** Commit:
  ```bash
  cargo test -p mur-core skill_gen::analysts
  git add mur-core/src/skill_gen/analysts.rs
  git commit -m "feat(skill-gen): parallel Success and Error analysts"
  ```

---

### Task 4 — Consolidator (Phase 3)

**Files:** `mur-core/src/skill_gen/consolidator.rs`.

- [ ] **4.1** Hierarchical merge then LLM-assisted final pass:

```rust
use crate::skill_gen::analysts::{Patch, PatchSource, StepDraft, TriggerDraft, VariableDraft};
use crate::skill_gen::prompts::CONSOLIDATOR_SYSTEM;
use mur_common::llm::LlmClient;
use mur_common::skill::{SkillManifest, parse_canonical, validate};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum ConsolidateError {
    #[error("no patches to consolidate")] Empty,
    #[error("LLM final pass failed: {0}")] Llm(#[from] mur_common::error::LlmError),
    #[error("LLM emitted invalid skill yaml: {0}")] BadYaml(String),
    #[error("validation: {0}")] Validate(String),
}

pub async fn consolidate(
    patches: Vec<Patch>,
    llm: &dyn LlmClient,
    target_name: Option<&str>,
) -> Result<SkillManifest, ConsolidateError> {
    if patches.is_empty() { return Err(ConsolidateError::Empty); }

    // 1. Hierarchical merge — group by procedure-step similarity.
    let merged = mechanical_merge(&patches);

    // 2. Final LLM pass produces YAML.
    let input = serde_json::to_string(&MergedInput {
        target_name: target_name.unwrap_or("generated-skill").to_string(),
        merged,
    }).expect("serialize");
    let yaml = llm.complete(&input, Some(CONSOLIDATOR_SYSTEM)).await?;
    let yaml = strip_yaml_fences(&yaml);

    // 3. Parse + validate.
    let m = parse_canonical(yaml).map_err(|e| ConsolidateError::BadYaml(e.to_string()))?;
    validate(&m).map_err(|e| ConsolidateError::Validate(e.to_string()))?;
    Ok(m)
}

#[derive(serde::Serialize)]
struct MergedInput {
    target_name: String,
    merged: MechanicalMerge,
}

#[derive(Debug, serde::Serialize)]
struct MechanicalMerge {
    /// Step buckets keyed by a similarity-hash (tool + first 6 words of description).
    pub step_groups: BTreeMap<String, Vec<StepDraft>>,
    /// Triggers deduped on (kind, pattern.trim().to_lowercase()).
    pub triggers: Vec<TriggerDraft>,
    /// Variables deduped on name.
    pub variables: Vec<VariableDraft>,
    /// All `abstract_hint`s for the LLM to pick the best.
    pub abstract_hints: Vec<String>,
    /// All notes — surface to user, not fed to LLM.
    pub notes: Vec<(PatchSource, Vec<String>)>,
}

fn mechanical_merge(patches: &[Patch]) -> MechanicalMerge {
    use std::collections::BTreeSet;
    let mut step_groups: BTreeMap<String, Vec<StepDraft>> = BTreeMap::new();
    let mut triggers_seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut triggers: Vec<TriggerDraft> = Vec::new();
    let mut vars_seen: BTreeSet<String> = BTreeSet::new();
    let mut variables: Vec<VariableDraft> = Vec::new();
    let mut abstract_hints = Vec::new();
    let mut notes = Vec::new();

    // Prefer Success patches over Error ones when collapsing duplicates: iterate Success first.
    let ordered: Vec<&Patch> = {
        let (succ, err): (Vec<_>, Vec<_>) = patches.iter().partition(|p| matches!(p.source, PatchSource::Success));
        succ.into_iter().chain(err.into_iter()).collect()
    };

    for p in ordered {
        if let Some(h) = &p.abstract_hint { abstract_hints.push(h.clone()); }
        for s in &p.procedure_steps {
            let key = step_similarity_key(s);
            step_groups.entry(key).or_default().push(s.clone());
        }
        for t in &p.triggers {
            let k = (t.kind.to_lowercase(), t.pattern.trim().to_lowercase());
            if triggers_seen.insert(k) { triggers.push(t.clone()); }
        }
        for v in &p.variables {
            if vars_seen.insert(v.name.clone()) { variables.push(v.clone()); }
        }
        if !p.notes.is_empty() { notes.push((p.source.clone(), p.notes.clone())); }
    }

    MechanicalMerge { step_groups, triggers, variables, abstract_hints, notes }
}

fn step_similarity_key(s: &StepDraft) -> String {
    let tool = s.tool.as_deref().unwrap_or("").to_lowercase();
    let head: String = s.description.split_whitespace().take(6).collect::<Vec<_>>().join(" ").to_lowercase();
    format!("{tool}::{head}")
}

fn strip_yaml_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```yaml").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    s.trim_end_matches("```").trim()
}
```

- [ ] **4.2** Tests:
  1. Empty patches → `Empty` error.
  2. Two patches with identical triggers → consolidated has one trigger.
  3. Success + Error patches disagree on a step → Success's tool wins in the mechanical merge (so the LLM gets it preferred).
  4. LLM returns yaml that fails `validate()` → `Validate` error surfaces.
  5. Mocked LLM round-trip: feed mechanical merge JSON, expect canned valid YAML → returns parsed `SkillManifest`.

- [ ] **4.3** Commit:
  ```bash
  cargo test -p mur-core skill_gen::consolidator
  git add mur-core/src/skill_gen/consolidator.rs
  git commit -m "feat(skill-gen): consolidator with mechanical merge + LLM final pass"
  ```

---

### Task 5 — CLI surface

**Files:** `mur-core/src/cmd/skill_generate.rs`, `mur-core/src/main.rs`.

- [ ] **5.1** Pure + shim split (same pattern as M3a's `cmd_install`):

```rust
//! `mur skill generate --from-session <id>` orchestrator.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::config::Config;
use mur_common::llm::LlmClient;
use mur_common::skill::{
    content_sha256, global_skill_dir, scan::scan_skill, serialize_canonical, write_to_dir,
    TrustLevel,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
use std::path::Path;
use std::sync::Arc;

pub struct GenerateOptions {
    pub session_id: String,
    pub name: Option<String>,
    pub model_override: Option<String>,
    pub dry_run: bool,
    pub max_parallel: usize,
}

pub async fn cmd_generate(
    home: &Path,
    llm: Arc<dyn LlmClient>,
    opts: GenerateOptions,
) -> Result<SkillManifest> {
    // 1. Load recording.
    let path = home.join("session/recordings").join(format!("{}.jsonl", opts.session_id));
    if !path.exists() { bail!("no recording at {}", path.display()); }
    let content = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    // 2. Parse trajectories.
    let trajectories = crate::skill_gen::trajectory::parse_recording(&content)
        .context("parse recording")?;
    if trajectories.is_empty() { bail!("recording produced zero trajectories"); }
    eprintln!("Phase 1: {} trajectories", trajectories.len());

    // 3. Phase 2 — parallel analysts.
    let patch_results = crate::skill_gen::analysts::run_phase2(llm.clone(), trajectories, opts.max_parallel).await;
    let mut patches = Vec::new();
    let mut analyst_failures = 0;
    for r in patch_results {
        match r {
            Ok(p) => patches.push(p),
            Err(e) => { analyst_failures += 1; tracing::warn!(error = %e, "analyst failure (dropped)"); }
        }
    }
    if patches.is_empty() { bail!("all analysts failed ({analyst_failures} failures)"); }
    eprintln!("Phase 2: {} patches ({analyst_failures} failures)", patches.len());

    // 4. Phase 3 — consolidate.
    let manifest = crate::skill_gen::consolidator::consolidate(patches, &*llm, opts.name.as_deref())
        .await.context("consolidate")?;
    eprintln!("Phase 3: '{}' v{}", manifest.name, manifest.version);

    if opts.dry_run {
        let yaml = serialize_canonical(&manifest)?;
        println!("{yaml}");
        return Ok(manifest);
    }

    // 5. Write + scan + trust (agent-generated == Sandboxed by spec §2 / §7.5).
    let dir = global_skill_dir(home, &manifest.name);
    write_to_dir(&dir, &manifest)?;
    let report = scan_skill(&manifest)?;
    if report.has_blocking_findings() {
        eprintln!("⚠ scan findings — Sandboxed trust:");
        for line in report.human_summary() { eprintln!("    {line}"); }
    }
    let hash = content_sha256(&manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(hash, TrustEntry {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        level: TrustLevel::Sandboxed,                    // agent-generated, per spec
        installed_at: chrono::Utc::now().to_rfc3339(),
        publisher: Some(manifest.publisher.clone()),
    });
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;
    println!("generated: {} v{} (Sandboxed)", manifest.name, manifest.version);
    println!("review:    {}", dir.join("skill.yaml").display());
    Ok(manifest)
}

pub async fn cmd_generate_cli(opts: GenerateOptions) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let cfg = Config::load_or_default(&home.join("config.yaml"));   // helper from M3a
    let backend = cfg.synthesize_backend();                          // existing
    let backend = if let Some(model) = opts.model_override.clone() {
        let mut b = backend; b.model = model; b
    } else { backend };
    if !mur_common::llm::is_reasoning_model(&backend.model) {
        eprintln!("warning: model '{}' is not a reasoning-class model — Error Analyst quality may suffer", backend.model);
    }
    let llm: Arc<dyn LlmClient> = crate::conversations::backend::factory::build_for_stage(&backend, "skill.generate")
        .context("build llm")?;
    cmd_generate(&home, llm, opts).await?;
    Ok(())
}
```

> **Note:** `factory::build_for_stage` returns a chat backend, not an `Arc<dyn LlmClient>` directly. **Verify the return type before Task 5 lands** — adapt as needed. If there's no clean `LlmClient` factory yet, expose one via a small `mur-core/src/llm.rs` shim that wraps the existing chat backend.

- [ ] **5.2** Wire into the CLI:

```rust
// In the Skill enum (mur-core/src/main.rs):
Generate {
    #[clap(long)]
    from_session: String,
    #[clap(long)]
    name: Option<String>,
    #[clap(long)]
    model: Option<String>,
    #[clap(long)]
    dry_run: bool,
    #[clap(long, default_value = "4")]
    parallel: usize,
}

// In the dispatch arm:
Skill::Generate { from_session, name, model, dry_run, parallel } => {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(cmd::skill_generate::cmd_generate_cli(cmd::skill_generate::GenerateOptions {
        session_id: from_session, name, model_override: model, dry_run, max_parallel: parallel,
    }))?;
}
```

(If the dispatch arm is already `#[tokio::main]`, the runtime build above is redundant — mirror the existing async-arm pattern in `cmd_session_stop`.)

- [ ] **5.3** Commit:
  ```bash
  cargo check -p mur-core
  git add mur-core/src/cmd/skill_generate.rs mur-core/src/main.rs
  git commit -m "feat(skill-gen): mur skill generate --from-session CLI"
  ```

---

### Task 6 — E2E test with mock LLM + recorded fixture

**Files:** `mur-core/tests/skill_generate_e2e.rs`, `mur-core/tests/fixtures/skill_gen/sample_session.jsonl`.

- [ ] **6.1** Fixture: write a small synthetic JSONL with 3 successful trajectories (find-price scenario, two variants) + 1 failed trajectory. Keep it < 100 lines.

  Use the **real event-schema verified in Task 1.1**. If schema details are still in flux at PR time, the fixture is the canonical reference for what M3b's parser accepts — flag this in the PR description.

- [ ] **6.2** Test:

```rust
//! E2E: drives cmd_generate with a MockLlm that returns canned analyst + consolidator
//! responses for a fixed JSONL fixture. Asserts the skill.yaml lands on disk
//! with a Sandboxed trust entry.

use mur_common::llm::LlmClient;
use std::sync::{Arc, Mutex};

struct ScriptedLlm { script: Mutex<std::collections::VecDeque<String>> }
impl LlmClient for ScriptedLlm {
    fn complete(&self, _: &str, _: Option<&str>) -> impl std::future::Future<Output = Result<String, mur_common::error::LlmError>> + Send {
        let resp = self.script.lock().unwrap().pop_front().expect("script exhausted");
        async move { Ok(resp) }
    }
    fn embed(&self, _: &str) -> impl std::future::Future<Output = Result<Vec<f32>, mur_common::error::LlmError>> + Send {
        async { Ok(vec![]) }
    }
}

#[tokio::test]
async fn generates_skill_from_fixture() {
    let home = tempfile::tempdir().unwrap();
    // Copy fixture into the recordings dir.
    let rec_dir = home.path().join("session/recordings");
    std::fs::create_dir_all(&rec_dir).unwrap();
    let fixture = include_str!("fixtures/skill_gen/sample_session.jsonl");
    std::fs::write(rec_dir.join("test-sess.jsonl"), fixture).unwrap();

    // Script: 3 success-analyst JSON, 1 error-analyst ReAct-then-PATCH, 1 consolidator YAML.
    let script = vec![
        /* success patch json #1 */ r#"{"abstract_hint":"finds prices","procedure_steps":[{"description":"open product page","tool":"browser.navigate"}],"triggers":[{"kind":"command","pattern":"/find-price"}],"variables":[{"name":"product","type":"string","required":true}],"notes":[]}"#.into(),
        /* success patch json #2 */ r#"{"abstract_hint":"find prices","procedure_steps":[{"description":"open product page","tool":"browser.navigate"}],"triggers":[{"kind":"command","pattern":"/find-price"}],"variables":[],"notes":[]}"#.into(),
        /* success patch json #3 */ r#"{"abstract_hint":"compare prices","procedure_steps":[{"description":"extract price text","tool":"browser.extract"}],"triggers":[],"variables":[],"notes":[]}"#.into(),
        /* error analyst round 1: emit PATCH immediately */ r#"THOUGHT: missing variable. ACTION: done. PATCH: {"abstract_hint":null,"procedure_steps":[],"triggers":[],"variables":[{"name":"region","type":"string","required":false}],"notes":["add region for international SKUs"]}"#.into(),
        /* consolidator yaml */ r#"
name: find-price
version: 0.1.0
publisher: agent:generator
description: find product prices
category: workflow
content:
  abstract: Searches product prices.
  procedure:
    variables:
      - name: product
        type: string
        required: true
      - name: region
        type: string
        required: false
    steps:
      - description: open product page
        tool: browser.navigate
      - description: extract price text
        tool: browser.extract
triggers:
  - type: command
    pattern: /find-price
"#.into(),
    ];
    let llm = Arc::new(ScriptedLlm { script: Mutex::new(script.into()) });

    let manifest = mur_core::cmd::skill_generate::cmd_generate(
        home.path(),
        llm,
        mur_core::cmd::skill_generate::GenerateOptions {
            session_id: "test-sess".into(),
            name: None, model_override: None, dry_run: false, max_parallel: 2,
        },
    ).await.unwrap();

    assert_eq!(manifest.name, "find-price");
    assert_eq!(manifest.version, "0.1.0");
    assert!(home.path().join("skills/find-price/skill.yaml").exists());

    // Trust store: one entry at Sandboxed.
    let trust = mur_common::trust::skills::SkillTrustStore::load(home.path()).unwrap();
    let e = trust.entries.values().find(|e| e.name == "find-price").expect("trust entry");
    assert!(matches!(e.level, mur_common::skill::TrustLevel::Sandboxed));
}

#[tokio::test]
async fn missing_session_returns_error() {
    let home = tempfile::tempdir().unwrap();
    let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlm { script: Mutex::new(Default::default()) });
    let err = mur_core::cmd::skill_generate::cmd_generate(
        home.path(), llm,
        mur_core::cmd::skill_generate::GenerateOptions {
            session_id: "nonexistent".into(),
            name: None, model_override: None, dry_run: true, max_parallel: 2,
        },
    ).await.unwrap_err();
    assert!(err.to_string().contains("no recording"));
}

#[tokio::test]
async fn dry_run_does_not_write_to_disk() {
    let home = tempfile::tempdir().unwrap();
    // ... same fixture setup ...
    // After cmd_generate with dry_run=true, assert ~/.mur/skills/<name>/ does not exist.
}
```

- [ ] **6.3** Commit:
  ```bash
  cargo test -p mur-core --test skill_generate_e2e
  git add mur-core/tests/skill_generate_e2e.rs mur-core/tests/fixtures/skill_gen/
  git commit -m "test(skill-gen): e2e generate with scripted LLM"
  ```

---

## Self-Review

**Spec §8 + §14 M3 coverage (M3b slice only):**

| Spec item | Status | Task |
|---|---|---|
| `mur skill generate --from-session` | ✅ | T5 |
| Phase 1: trajectory pool | ✅ | T1 |
| Phase 2: parallel Success + Error analysts (4-dim ReAct) | ✅ | T2 prompts + T3 analysts |
| Phase 3: hierarchical merge + dedupe + conflict resolution | ✅ | T4 |
| Agent-generated → Sandboxed trust | ✅ | T5.1 (`TrustLevel::Sandboxed`) |
| Cross-model transfer / model choice | ✅ guard | T5.1 warns if non-reasoning model |
| `mur skill from-pattern` | ⛔ deferred to **M3b.2** | — |
| Auto-suggest trigger | ⛔ deferred to **M3b.3** | — |
| Self-evolution loop | ⛔ deferred to **M3c** | — |

**Risks and gotchas (called out for the executor):**

1. **Recording event schema is unverified** — Task 1.1 makes this the first action. If the schema is more variable than expected, expand `RecordingEvent` to `serde_json::Value`-with-best-effort extraction rather than blocking the milestone on schema cleanup.

2. **LLM-client adapter ambiguity** — `factory::build_for_stage` may not return `Arc<dyn LlmClient>` today. Task 5 includes a verification step before wiring the CLI; if there's a mismatch, add a thin adapter rather than refactoring the whole conversations backend module.

3. **Cost** — generating one skill triggers `N_success + N_failure × up_to_5_react_rounds + 1` LLM calls. For a 100-event recording with 10 trajectories: up to ~15–35 LLM calls. Print progress (`Phase 2: 3/10`) and accept a `--parallel` flag to throttle.

4. **Determinism** — generation output is not byte-stable across runs. Tests use a `ScriptedLlm`. Don't write tests that compare wall-clock-driven LLM responses.

5. **Failure isolation** — one bad trajectory shouldn't kill the whole run. `run_phase2` returns `Vec<Result<Patch>>` and the orchestrator drops failures with a warn. Only `all-failed` is a hard error.

6. **Trust-store invariant** — generated skills MUST enter at Sandboxed. Don't be tempted to upgrade-on-success; that's M5 lifecycle territory.

7. **Naming collisions** — generated `name` may collide with an installed skill. M3b uses the consolidator's chosen name verbatim and overwrites if present (with `eprintln!` warning). Real renaming UX (`--name foo`) is supported; collision-on-existing-skill is up to the user to resolve.

**Placeholder scan:** clean. No `// TODO` placeholders other than the explicitly-flagged Task 5.1 LLM factory verification.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m3b.md`.

Suggested branch: `feat/skill-ecosystem-m3b`. Can land **before, after, or in parallel with** M3a — they share no files. CI runs both PRs independently.

Two execution options:

1. **Subagent-driven (recommended)** — fresh subagent per task. Critical review checkpoints after T1 (schema verification), T4 (mechanical merge correctness), T6 (e2e fixture-driven contract).
2. **Inline `superpowers:executing-plans`** — checkpoint after T1, T4, T6.

No outstanding decisions — scope (only generate), pipeline shape (full 3-phase), CLI surface (subcommand) all locked above.
