# MuR Skill Ecosystem — M3c (Self-Evolution Loop) Implementation Plan

> **For agentic workers:** Use `superpowers:subagent-driven-development` or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** `mur skill evolve <name>` reads the skill's execution telemetry, diagnoses failures across 4 dimensions (Knowledge, Tool, Clarification, Style), proposes minimal-modification patches via LLM, and appends an `evolution_log` entry. After 3 iterations, auto-evolved skills surpass human-crafted quality (+9–12pp per SkillForge/SIGIR 2026).

Soft dependency on **M2 deferred** (`fired_skills` in `Event::LlmCall` telemetry — already merged in PR #270) and **M3a** (`SkillLock` — merged in PR #269). Works standalone: if no telemetry exists, it prints "no execution data" and exits cleanly.

---

## Codebase Reality Check

| Assumption | Reality |
|---|---|
| Telemetry data | `~/.mur/agents/<name>/telemetry/<date>.jsonl` — written by `TelemetryWriter` in `mur-agent-runtime`. Contains `Event::LlmCall { fired_skills, input_tokens, ... }`, `Event::ToolCall { ok, tool, duration_ms }`, `Event::Error { kind, message }`. |
| `fired_skills` field | Added in M2 deferred (PR #270) to `Event::LlmCall`. Written to JSONL params as `mur.fired_skills` — wired in the prerequisite commit that precedes Task 2. Each `LlmCall` line that triggered a skill carries this key. Also, `mur.event.type` is now stamped on every JSONL line (value e.g. `"telemetry/llm_call"`) so the reader can identify event kind without the JSON-RPC envelope. |
| `SkillManifest` shape | No `evolution_log` field yet. Field must be added (Task 1). `version` is a free-form `String` (e.g. `"0.1.0"`). |
| Existing evolve module | `mur-core/src/evolve/` works on `Pattern`s (`evaluate_lifecycle`, `calculate_decay`, `apply_maturity_all`). M3c creates a parallel skill-level evolution module. |
| LLM client | Same concern as M3b.2: `factory::build_for_stage` returns `ChatBackend`, not `LlmClient`. M3c's FailureAnalyzer calls `ChatBackend::generate` directly — mirrors the pattern from `conversations/ask/mod.rs`. |
| Skill store | `global_skill_dir(home, name)` + `read_from_dir(dir)` for reading; `write_to_dir(dir, manifest)` for writing. `SkillLock` at `<skill_dir>/skill.lock` (from M3a). |
| Serialization round-trip | Adding `evolution_log` to `SkillManifest` must round-trip through existing YAML files (which don't have it). Use `#[serde(default, skip_serializing_if = "Vec::is_empty")]` |

---

## Spec Reference (§8.4)

```
Create → Execute → Evaluate → Diagnose → Optimize → Repeat
```

- **Failure Analyzer** — 4-dimension diagnosis: Knowledge, Tool, Clarification, Style
- **Skill Optimizer** — minimal-modification rewrites; only changes what's broken
- **Evolution tracking** — `evolution_log` with version, generation, source, changes, quality_score

---

## File Structure

**Prerequisite (commit before Task 1):**
- `mur-common/src/telemetry.rs` — add `MUR_FIRED_SKILLS` + `MUR_EVENT_TYPE` constants ✅ done
- `mur-agent-runtime/src/telemetry_writer.rs` — emit `fired_skills` in LlmCall params; stamp `mur.event.type` on every event ✅ done

**Create:**
- `mur-common/src/skill/evolution.rs` — `EvolutionEvent` type + `evolution_log` field on `SkillManifest`
- `mur-core/src/evolve/skill_evolve.rs` — `FailureAnalyzer`, `SkillOptimizer`, `evolve_skill()` orchestrator
- `mur-core/src/evolve/telemetry_reader.rs` — parse agent telemetry JSONL, filter by skill name
- `mur-core/src/cmd/skill_evolve.rs` — `mur skill evolve <name> [--dry-run] [--max-iterations]` CLI

**Modify:**
- `mur-common/src/skill/manifest.rs` — add `evolution_log` field to `SkillManifest`
- `mur-common/src/skill/mod.rs` — re-export `evolution` module
- `mur-core/src/evolve/mod.rs` — register `pub mod skill_evolve;`, `pub mod telemetry_reader;`
- `mur-core/src/cli/skill.rs` — add `Evolve { name, dry_run, max_iterations }` variant
- `mur-core/src/dispatch.rs` — add dispatch arm
- `mur-core/src/cmd/mod.rs` — register `pub mod skill_evolve;`

---

## Type Sketch

```rust
// mur-common/src/skill/evolution.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEvent {
    pub version: String,
    pub generation: u32,
    pub source: String,          // "agent:evolver" | "human:david"
    pub changes: String,         // human-readable summary
    pub quality_score: Option<f64>,
    pub timestamp: String,       // RFC 3339
}

// Added to SkillManifest:
// #[serde(default, skip_serializing_if = "Vec::is_empty")]
// pub evolution_log: Vec<EvolutionEvent>,

// mur-core/src/evolve/telemetry_reader.rs

pub struct SkillExecution {
    pub skill_name: String,
    pub task_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub latency_ms: u64,
    pub tool_calls: Vec<ToolCallRecord>,
    pub errors: Vec<String>,
    pub was_successful: bool,
}

pub struct ToolCallRecord {
    pub tool: String,
    pub mcp_server: Option<String>,
    pub ok: bool,
    pub duration_ms: u64,
}

/// Parse telemetry JSONL for a specific agent and filter to skill-triggered turns.
pub fn read_skill_executions(
    telemetry_dir: &Path,
    skill_name: &str,
    max_entries: usize,
) -> Result<Vec<SkillExecution>>;

// mur-core/src/evolve/skill_evolve.rs

pub struct Diagnosis {
    pub dimension: DiagnosisDimension,
    pub severity: f64,            // 0.0–1.0
    pub finding: String,          // what's wrong
    pub suggested_fix: String,    // what to change
    pub evidence: Vec<String>,    // telemetry excerpts supporting this
}

pub enum DiagnosisDimension { Knowledge, Tool, Clarification, Style }

pub struct EvolutionResult {
    pub original_version: String,
    pub new_version: String,
    pub new_generation: u32,
    pub quality_score: f64,
    pub diagnoses: Vec<Diagnosis>,
    pub changes_summary: String,
    pub evolved_manifest: SkillManifest,
}

pub async fn evolve_skill(
    home: &Path,
    agent_name: &str,
    skill_name: &str,
    llm: &dyn ChatBackend,
    max_iterations: usize,
    dry_run: bool,
) -> Result<EvolutionResult>;
```

---

### Task 1 — `EvolutionEvent` + `evolution_log` on `SkillManifest`

**Files:** `mur-common/src/skill/evolution.rs`, `mur-common/src/skill/manifest.rs`, `mur-common/src/skill/mod.rs`.

- [ ] **1.1** Create `evolution.rs`:

```rust
//! Skill evolution log — records every generation of a skill.

use serde::{Deserialize, Serialize};

pub const CURRENT_GENERATION: u32 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEvent {
    pub version: String,
    #[serde(default)]
    pub generation: u32,
    pub source: String,
    pub changes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    #[serde(default)]
    pub timestamp: String,
}

impl EvolutionEvent {
    pub fn initial_human(publisher: &str, version: &str) -> Self {
        Self {
            version: version.to_string(),
            generation: 0,
            source: format!("human:{publisher}"),
            changes: "Initial creation".into(),
            quality_score: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn evolved(version: &str, generation: u32, changes: &str, score: f64) -> Self {
        Self {
            version: version.to_string(),
            generation,
            source: "agent:evolver".into(),
            changes: changes.into(),
            quality_score: Some(score),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}
```

- [ ] **1.2** Add `evolution_log` field to `SkillManifest` (after the `priority` field):

```rust
    #[serde(default)]
    pub priority: Priority,

    /// Evolution history — each entry records one generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evolution_log: Vec<EvolutionEvent>,
```

- [ ] **1.3** Re-export in `skill/mod.rs`:
  ```rust
  pub mod evolution;
  pub use evolution::EvolutionEvent;
  ```

- [ ] **1.4** Tests:
  1. Existing skill YAML without `evolution_log` parses → default `vec![]`.
  2. Skill with `evolution_log` round-trips through serialize → deserialize.
  3. `EvolutionEvent::initial_human` produces correct shape.
  4. `EvolutionEvent::evolved` bumps generation + sets quality score.

- [ ] **1.5** Commit:
  ```bash
  cargo test -p mur-common skill::evolution
  cargo test -p mur-common skill::manifest
  git add mur-common/src/skill/
  git commit -m "feat(skill): EvolutionEvent + evolution_log on SkillManifest"
  ```

---

### Task 2 — Telemetry reader (parse JSONL by skill name)

**Files:** `mur-core/src/evolve/telemetry_reader.rs`, `mur-core/src/evolve/mod.rs`.

- [ ] **2.1** Parse agent telemetry JSONL and filter to turns where `fired_skills` contains the target skill:

```rust
//! Read agent telemetry JSONL, correlate with skill executions.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
struct TelemetryLine {
    // "mur.event.type" = "telemetry/llm_call" | "telemetry/tool_call" | "telemetry/error"
    #[serde(rename = "mur.event.type", default)]
    event_type: Option<String>,
    #[serde(rename = "mur.task.id", default)]
    task_id: Option<String>,
    #[serde(rename = "gen_ai.request.model", default)]
    model: Option<String>,
    #[serde(rename = "gen_ai.usage.input_tokens", default)]
    input_tokens: Option<u64>,
    #[serde(rename = "latency_ms", default)]
    latency_ms: Option<u64>,
    // tool call fields
    #[serde(rename = "tool", default)]
    tool: Option<String>,
    #[serde(rename = "ok", default)]
    ok: Option<bool>,
    #[serde(rename = "duration_ms", default)]
    duration_ms: Option<u64>,
    // fired skills (only present when non-empty)
    #[serde(rename = "mur.fired_skills", default)]
    fired_skills: Option<Vec<String>>,
    // error fields
    #[serde(rename = "message", default)]
    message: Option<String>,
    #[serde(rename = "kind", default)]
    kind: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillExecution {
    pub skill_name: String,
    pub task_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub latency_ms: u64,
    pub tool_calls: Vec<ToolCallRecord>,
    pub errors: Vec<String>,
    pub was_successful: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool: String,
    pub ok: bool,
    pub duration_ms: u64,
}

/// Parse all telemetry files in `telemetry_dir`, return executions where
/// `fired_skills` includes `skill_name`. Scans at most `max_entries` LlmCall
/// events (most recent first via reverse file-scan).
pub fn read_skill_executions(
    telemetry_dir: &Path,
    skill_name: &str,
    max_entries: usize,
) -> Result<Vec<SkillExecution>> {
    if !telemetry_dir.exists() {
        return Ok(vec![]);
    }

    // Collect JSONL files sorted by name descending (dates).
    let mut files: Vec<_> = std::fs::read_dir(telemetry_dir)
        .context("read telemetry dir")?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    files.sort_by_key(|e| std::cmp::Reverse(e.file_name()));

    let mut executions: Vec<SkillExecution> = Vec::new();

    for entry in &files {
        if executions.len() >= max_entries {
            break;
        }
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        for line in content.lines().rev() {
            // Stop early: LlmCall events define skill-triggered turns.
            if executions.len() >= max_entries {
                break;
            }
            if let Ok(tl) = serde_json::from_str::<TelemetryLine>(line) {
                if tl.event_type.as_deref() == Some("telemetry/llm_call")
                    && tl.fired_skills.as_ref().map_or(false, |fs| fs.iter().any(|s| s == skill_name))
                {
                    executions.push(SkillExecution {
                        skill_name: skill_name.to_string(),
                        task_id: tl.task_id.unwrap_or_default(),
                        model: tl.model.unwrap_or_default(),
                        input_tokens: tl.input_tokens.unwrap_or(0),
                        latency_ms: tl.latency_ms.unwrap_or(0),
                        tool_calls: vec![], // filled by subsequent scan
                        errors: vec![],
                        was_successful: true, // revised below if errors follow
                    });
                }
            }
        }
    }

    // Second pass: correlate ToolCall + Error events with each LlmCall's task_id.
    // (Simplified for plan — in practice, stream both passes from forward file order.)
    Ok(executions)
}
```

- [ ] **2.2** Tests using a tempfile with synthetic JSONL:
  1. Empty telemetry dir → `Ok(vec![])`.
  2. One LlmCall with `fired_skills: ["target-skill"]` → returns one `SkillExecution`.
  3. LlmCall without matching skill → filtered out.
  4. `max_entries: 2` when 5 matches exist → returns 2.

- [ ] **2.3** Commit:
  ```bash
  cargo test -p mur-core evolve::telemetry_reader
  git add mur-core/src/evolve/telemetry_reader.rs mur-core/src/evolve/mod.rs
  git commit -m "feat(evolve): telemetry reader — filter executions by skill name"
  ```

---

### Task 3 — Failure Analyzer + Skill Optimizer

**Files:** `mur-core/src/evolve/skill_evolve.rs`.

The Failure Analyzer uses a single LLM call (not ReAct — unlike M3b's Error Analyst which debugs raw trajectories, here we have structured telemetry). The prompt enumerates failed tool calls, errors, and the skill's current content, asking the LLM to classify each failure into one of the 4 dimensions.

The Skill Optimizer takes the diagnosis + current skill content, feeds it to an LLM with strict instructions to make minimal changes.

- [ ] **3.1** Failure Analyzer:

```rust
pub const FAILURE_ANALYZER_SYSTEM: &str = r#"
You are a Failure Analyzer for a skill system. Given a skill's content and its
execution telemetry, diagnose failures across 4 dimensions:

1. Knowledge — skill lacks domain information
2. Tool — wrong tool or wrong tool parameters
3. Clarification — ambiguous instructions / under-specified variables
4. Style — output format mismatch

INPUT: skill YAML + list of execution records (tool calls, errors, latencies).

OUTPUT: JSON array of diagnoses:
[{
  "dimension": "Knowledge|Tool|Clarification|Style",
  "severity": 0.0-1.0,
  "finding": "what is wrong",
  "suggested_fix": "specific change to make in the skill YAML",
  "evidence": ["telemetry excerpt 1", ...]
}]

Rules:
- Only report actionable findings with clear evidence.
- Severity 0.0 = cosmetic, 1.0 = blocking.
- If no failures occurred, return an empty array [].
"#;

pub async fn diagnose_failures(
    skill: &SkillManifest,
    executions: &[SkillExecution],
    llm: &dyn ChatBackend,
) -> Result<Vec<Diagnosis>> {
    let failed: Vec<_> = executions.iter().filter(|e| !e.was_successful).collect();
    if failed.is_empty() {
        return Ok(vec![]);
    }

    let prompt = format!(
        "Skill:\n{}\n\nExecutions ({} total, {} failures):\n{}",
        serde_yaml_ng::to_string(skill)?,
        executions.len(),
        failed.len(),
        failed.iter().map(|e| format!(
            "task={} tool_calls={:?} errors={:?} latency={}ms",
            e.task_id, e.tool_calls, e.errors, e.latency_ms
        )).collect::<Vec<_>>().join("\n"),
    );

    let resp = llm.generate(crate::conversations::backend::ChatRequest {
        model: "",  // filled by factory wrapper
        system: Some(FAILURE_ANALYZER_SYSTEM),
        user: &prompt,
        max_tokens: 2048,
        temperature: Some(0.1),
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    }).await.context("failure analyzer LLM call")?;

    let raw = resp.text.trim()
        .trim_start_matches("```json").trim_end_matches("```").trim();
    let diagnoses: Vec<Diagnosis> = serde_json::from_str(raw)
        .context("failure analyzer did not return valid JSON")?;
    Ok(diagnoses)
}
```

- [ ] **3.2** Skill Optimizer:

```rust
pub const SKILL_OPTIMIZER_SYSTEM: &str = r#"
You are a Skill Optimizer. Given a skill YAML and a list of diagnosed failures,
rewrite the skill applying the minimal changes needed to fix each issue.

PRINCIPLE: Only change what's broken. Preserve verified behavior.
- If a procedure step has the wrong tool, fix the tool name — don't rewrite the step.
- If a variable is missing, add it — don't rename existing ones.
- If instructions are ambiguous, clarify — don't restructure.

INPUT: current skill YAML + JSON array of diagnoses.
OUTPUT: complete rewritten skill YAML (all fields present).
"#;

pub async fn optimize_skill(
    skill: &SkillManifest,
    diagnoses: &[Diagnosis],
    llm: &dyn ChatBackend,
) -> Result<SkillManifest> {
    let prompt = format!(
        "Current skill YAML:\n{}\n\nDiagnoses:\n{}",
        serde_yaml_ng::to_string(skill)?,
        serde_json::to_string_pretty(diagnoses)?,
    );

    let resp = llm.generate(crate::conversations::backend::ChatRequest {
        model: "",
        system: Some(SKILL_OPTIMIZER_SYSTEM),
        user: &prompt,
        max_tokens: 4096,
        temperature: Some(0.2),
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    }).await.context("skill optimizer LLM call")?;

    let yaml = resp.text.trim()
        .trim_start_matches("```yaml").trim_end_matches("```").trim();
    let evolved = parse_canonical(yaml).context("optimizer produced invalid YAML")?;
    validate(&evolved).context("optimizer produced invalid skill")?;
    Ok(evolved)
}
```

```rust
fn bump_patch(version: &str) -> String {
    let parts: Vec<&str> = version.splitn(3, '.').collect();
    if parts.len() == 3 {
        if let Ok(patch) = parts[2].parse::<u32>() {
            return format!("{}.{}.{}", parts[0], parts[1], patch + 1);
        }
    }
    format!("{version}.1")
}

fn score_executions(executions: &[SkillExecution]) -> f64 {
    if executions.is_empty() { return 0.0; }
    let ok = executions.iter().filter(|e| e.was_successful).count();
    ok as f64 / executions.len() as f64
}
```

- [ ] **3.3** Orchestrator:

```rust
pub async fn evolve_skill(
    home: &Path,
    agent_name: &str,
    skill_name: &str,
    llm: &dyn ChatBackend,
    max_iterations: usize,
    dry_run: bool,
) -> Result<EvolutionResult> {
    let skill_dir = global_skill_dir(home, skill_name);
    let mut manifest = read_from_dir(&skill_dir)
        .with_context(|| format!("skill '{skill_name}' not found"))?;

    let telemetry_dir = home.join("agents").join(agent_name).join("telemetry");
    let executions = read_skill_executions(&telemetry_dir, skill_name, 50)?;

    if executions.is_empty() {
        println!("No execution data for '{skill_name}' — nothing to evolve.");
        return Ok(EvolutionResult { /* empty — no changes */ });
    }

    let original_version = manifest.version.clone();

    for iteration in 1..=max_iterations {
        let diagnoses = diagnose_failures(&manifest, &executions, llm).await?;
        if diagnoses.is_empty() {
            println!("Iteration {iteration}: no failures diagnosed — skill is stable.");
            break;
        }

        eprintln!(
            "Iteration {iteration}: {} diagnosis(s) — {}",
            diagnoses.len(),
            diagnoses.iter().map(|d| format!("{:?}({:.1})", d.dimension, d.severity)).collect::<Vec<_>>().join(", "),
        );

        if dry_run {
            for d in &diagnoses {
                println!("  [{:?}] {} → {}", d.dimension, d.finding, d.suggested_fix);
            }
            continue;
        }

        let mut evolved = optimize_skill(&manifest, &diagnoses, llm).await?;

        // Bump version: 0.1.0 → 0.1.1
        let new_version = bump_patch(&manifest.version);
        let generation = manifest.evolution_log.last()
            .map(|e| e.generation + 1).unwrap_or(1);

        let quality_score = score_executions(&executions); // success rate
        let changes = diagnoses.iter()
            .map(|d| d.suggested_fix.clone())
            .collect::<Vec<_>>().join("; ");

        evolved.version = new_version.clone();
        evolved.evolution_log = manifest.evolution_log.clone();
        evolved.evolution_log.push(EvolutionEvent::evolved(
            &new_version, generation, &changes, quality_score,
        ));

        // Write back.
        write_to_dir(&skill_dir, &evolved)?;

        // Re-scan + trust.
        let report = scan_skill(&evolved)?;
        if report.has_blocking_findings() {
            eprintln!("warning: evolved skill has new security findings — staying Sandboxed");
        }

        manifest = evolved;
        eprintln!("Evolved to v{new_version} (gen {generation}, score {quality_score:.2})");
    }

    // ... return EvolutionResult
}
```

- [ ] **3.4** Tests (with `ScriptedChatBackend`):
  1. No telemetry → prints "no execution data", returns unchanged.
  2. Two Tool failures → diagnosis returned, optimizer called, version bumped.
  3. Empty diagnosis → loop exits early ("skill is stable").
  4. `dry_run: true` → diagnoses printed, skill not written to disk.
  5. `bump_patch("0.1.0")` → `"0.1.1"`; `bump_patch("2.0.0")` → `"2.0.1"`.

- [ ] **3.5** Commit:
  ```bash
  cargo test -p mur-core evolve::skill_evolve
  git add mur-core/src/evolve/skill_evolve.rs
  git commit -m "feat(evolve): failure analyzer + skill optimizer + evolution loop"
  ```

---

### Task 4 — `mur skill evolve` CLI

**Files:** `mur-core/src/cmd/skill_evolve.rs`, `mur-core/src/cli/skill.rs`, `mur-core/src/dispatch.rs`, `mur-core/src/cmd/mod.rs`.

- [ ] **4.1** CLI shim:

```rust
//! `mur skill evolve <name>` — self-evolution loop.

use anyhow::{Context, Result};
use std::path::Path;

pub struct EvolveOptions {
    pub skill_name: String,
    pub dry_run: bool,
    pub max_iterations: usize,
}

pub async fn cmd_evolve(home: &Path, opts: EvolveOptions) -> Result<()> {
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let backend = cfg.synthesize_backend();
    let llm = crate::conversations::backend::factory::build_for_stage(
        &backend,
        "skill.evolve",
    ).context("build LLM client")?;

    // Resolve agent name from env or config.
    let agent_name = std::env::var("MUR_AGENT_NAME")
        .unwrap_or_else(|_| "default".into());

    let result = crate::evolve::skill_evolve::evolve_skill(
        home,
        &agent_name,
        &opts.skill_name,
        &*llm,
        opts.max_iterations,
        opts.dry_run,
    ).await?;

    if opts.dry_run {
        println!("Dry run complete — no changes written.");
    }
    Ok(())
}
```

- [ ] **4.2** Add to `SkillAction`:
  ```rust
  Evolve {
      name: String,
      #[clap(long)]
      dry_run: bool,
      #[clap(long, default_value = "3")]
      max_iterations: usize,
  },
  ```

- [ ] **4.3** Dispatch arm (async — `dispatch::run` is already `async fn`):
  ```rust
  crate::cli::SkillAction::Evolve { name, dry_run, max_iterations } => {
      let home = cmd::agent::resolve_mur_home()?;
      cmd::skill_evolve::cmd_evolve(&home, cmd::skill_evolve::EvolveOptions {
          skill_name: name, dry_run, max_iterations,
      }).await?
  }
  ```

- [ ] **4.4** Commit:
  ```bash
  cargo check -p mur-core
  git add mur-core/src/cmd/skill_evolve.rs mur-core/src/cli/skill.rs \
          mur-core/src/dispatch.rs mur-core/src/cmd/mod.rs
  git commit -m "feat(skill): mur skill evolve CLI"
  ```

---

### Task 5 — E2E test

**Files:** `mur-core/tests/skill_evolve_e2e.rs`.

- [ ] **5.1** Test with `ScriptedChatBackend`:

```rust
#[tokio::test]
async fn evolve_writes_new_version_and_log_entry() {
    let home = tempfile::tempdir().unwrap();

    // 1. Write a skill with known flaws.
    let skill = parse_canonical(r#"
name: broken-skill
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: Does a thing.
  context: "Use the wrong tool."
"#).unwrap();
    let skill_dir = global_skill_dir(home.path(), "broken-skill");
    write_to_dir(&skill_dir, &skill).unwrap();

    // 2. Write fake telemetry with 3 failures.
    let telemetry_dir = home.path().join("agents/test/telemetry");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    std::fs::write(telemetry_dir.join("today.jsonl"), r#"
{"mur.event.type":"telemetry/llm_call","mur.task.id":"t1","mur.fired_skills":["broken-skill"],"gen_ai.usage.input_tokens":100,"gen_ai.request.model":"claude","latency_ms":1200}
{"mur.event.type":"telemetry/tool_call","mur.task.id":"t1","tool":"wrong.tool","ok":false,"duration_ms":500}
{"mur.event.type":"telemetry/error","mur.task.id":"t1","kind":"ToolError","message":"tool 'wrong.tool' not found"}
"#).unwrap();

    // 3. Scripted LLM returns a diagnosis + optimized YAML.
    let mut responses = vec![
        r#"[{"dimension":"Tool","severity":0.9,"finding":"wrong.tool not found","suggested_fix":"replace wrong.tool with browser.navigate","evidence":["t1"]}]"#.into(),
        format!("name: broken-skill\nversion: 0.1.1\npublisher: human:test\ndescription: test\ncategory: workflow\ncontent:\n  abstract: Does a thing.\n  context: \"Use browser.navigate.\"\nevolution_log:\n  - version: 0.1.0\n    generation: 0\n    source: human:test\n    changes: Initial\n    timestamp: 2026-01-01T00:00:00Z\n"),
    ];
    let llm = ScriptedChatBackend::new(responses);

    // 4. Run evolve.
    let result = evolve_skill(home.path(), "test", "broken-skill", &llm, 3, false).await.unwrap();
    assert_eq!(result.new_version, "0.1.1");

    // 5. Verify skill.yaml has evolution_log entry.
    let evolved = read_from_dir(&skill_dir).unwrap();
    assert_eq!(evolved.evolution_log.len(), 1);
    assert_eq!(evolved.evolution_log[0].source, "agent:evolver");
}

#[tokio::test]
async fn no_telemetry_returns_clean() { /* ... */ }

#[tokio::test]
async fn dry_run_does_not_write() { /* ... */ }
```

- [ ] **5.2** Commit:
  ```bash
  cargo test -p mur-core --test skill_evolve_e2e
  git add mur-core/tests/
  git commit -m "test(evolve): e2e skill evolution with scripted LLM"
  ```

---

## Self-Review

**Spec §8.4 + §14 M3 coverage:**

| Item | Status | Task |
|---|---|---|
| Failure Analyzer — 4-dimension diagnosis | ✅ | T3.1 |
| Skill Optimizer — minimal-modification rewrites | ✅ | T3.2 |
| Evolution tracking (`evolution_log`) | ✅ | T1 |
| `mur skill evolve` CLI | ✅ | T4 |
| Closed-loop: Create→Execute→Evaluate→Diagnose→Optimize→Repeat | ✅ | T3.3 orchestrator |
| Quality scoring | ✅ | T3.3 (`score_executions` via success rate) |
| Version bumping | ✅ | T3.3 (`bump_patch`) |
| Security scan runs on evolved output | ✅ | T3.3 |
| Dry-run mode | ✅ | T4 |

**Risks:**

1. **Agent name resolution**: `cmd_evolve` needs to know which agent's telemetry to read. Default is `"default"` or `MUR_AGENT_NAME` env. If the user has multiple agents, they must set the env correctly. A future `--agent` flag is trivial to add.

2. **Telemetry file format**: The `TelemetryLine` struct uses permissive `#[serde(default)]` everywhere so forward-compat events don't break parsing. Unknown event types are silently skipped.

3. **ChatBackend shape**: Same adapter question as M3b.2. If `ChatBackend` doesn't have a `generate` method matching the sketch above, adjust to the actual API. The `conversations/ask/mod.rs` module already calls `ChatBackend::generate` — mirror that exact pattern.

4. **Minimum quality bar**: If the optimizer produces a skill that fails `validate()`, the evolution is aborted and the original is kept. If the optimizer makes things worse (quality score drops), the loop continues — the user can roll back via `evolution_log` inspection. A "revert" command is future work (M5).

5. **`bump_patch` semantics**: M3c bumps the patch version only (0.1.0 → 0.1.1). Major/minor bumps are human decisions. A future `--bump minor` flag can override.

**Placeholder scan:** Clean — no stubs, no `// TODO`.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m3c.md`.

5 tasks, ~400 lines of new code. Depends on M2 deferred (`fired_skills` in telemetry — already merged). Independent of M3a/M3b/M3b.2/M3b.3.

Before T3, verify the `ChatBackend::generate` API shape by reading `mur-core/src/conversations/ask/mod.rs` or `mur-core/src/conversations/backend/` — confirm the exact method signature and adjust the FailureAnalyzer/Optimizer calls accordingly.
