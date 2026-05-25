# MuR Skill Ecosystem — M2 (Runtime Injection + Sandboxing) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Wire the agent runtime to (1) load installed skills (global + per-agent) once at boot, (2) inject Layer 2 abstracts of `session_start`-triggered skills into the system prompt with adaptive budget, (3) match `command`/`keyword` triggers per user prompt and swap in Layer 3 on the fly, and (4) gate skill-triggered tool calls with a `TrustLevel`→`Entitlements` derived sandbox.

---

## Codebase Reality Check (read before executing)

Verified against the current tree on branch `feat/skill-ecosystem-m1`:

| Assumption | Reality |
|---|---|
| Skill APIs | `mur_common::skill::local::{list_installed, load_installed, get_trust_level}` only know `mur_home`. **No per-agent listing exists** — Task 2 adds `list_installed_agent`. |
| Trust store lookup | `SkillTrustStore::lookup(&hash) -> Option<&TrustEntry>` (NOT `.entries.get`). Direct field access on `entries: BTreeMap` also works but `lookup` is the API. |
| Drift | `mur_common::skill::drift_status(&manifest, expected: Option<&str>)` — pass `lookup(hash).map(|e| &*e.name)` is wrong; the second arg is the **expected hash**, so pass `Some(&pinned_hash)`. |
| Regex | Workspace dep is `regex = "1"`, not `regex-lite`. |
| `Config` loader | `mur_common::config::Config` is a struct; **no `load_config()` function exists**. Other crates instantiate via `Config::default()` or read provider-specific subconfigs directly. M2 reads `skills:` out of an explicit on-disk file (`~/.mur/config.yaml`) with `serde_yaml_ng::from_str` and falls back to `SkillsConfig::default()`. |
| `TaskRunner` system prompt | `with_system_prompt(Option<String>)` is fixed at boot. `run_llm` reads `self.system_prompt` and there is no per-turn rebuild hook. M2 adds a per-turn rebuild path. |
| `Message` shape | `Message { role, parts: Vec<MessagePart> }`; extract text with `text_of(&msg)` (already in scope inside `task_runner.rs`). The original plan's `input.content.last().text` does not compile. |
| Hook trait method | Tool gate is `Hook::pre_tool_use(&self, ctx, &ToolCall, tok)` returning `Decision`. There is no `on_tool_call` / `ToolCallView`. |
| **Hook chain dispatch** | **`HookChain::on_prompt_submit` / `pre_tool_use` / `post_tool_use` / `on_message_send` are defined but never invoked from production code** — only `on_startup` and `on_shutdown` fire today. Wiring those into `TaskRunner` is itself a multi-day project and out of M2 scope. M2 therefore takes the pragmatic path: trigger matching lives in `TaskRunner.run_llm` directly; sandbox gating lives behind a small `SkillCallGate` that wraps the LLM client's tool-use path. The hook-chain re-architecture is filed for M3. |
| `TriggerKind` collision | `mur_common::skill::types::TriggerKind { Command, Keyword, SessionStart, Manual }` is **a different enum** from `mur_common::TriggerKind { Webhook, Cron, Message, Manual, Companion }` (the runtime's). Keep these straight via fully qualified paths. |
| `supervisor.rs` | Currently 969 lines (over the 800-line cap in `CLAUDE.md` §4). M2 must NOT add lines net-positive; it extracts a helper for the 3× repeated `TaskRunner::with_llm(...).with_system_prompt(...)` block (Ollama/Anthropic/OpenAI) in the same PR. |

**Architectural decision flagged for user approval before Task 5 lands:** Trigger matching as inline TaskRunner code (M2-pragmatic) vs. proper SkillTriggerHook on a freshly wired hook-chain (M3-clean). Default: pragmatic; revisit after M2 ships.

---

## File Structure

**Create:**
- `mur-common/src/skill/loader.rs` — single-pass loader returning `Vec<LoadedSkill>` with drift + trust resolved
- `mur-agent-runtime/src/skills/mod.rs`, `injector.rs`, `trigger_matcher.rs`, `sandbox_map.rs` — runtime side
- `mur-agent-runtime/src/supervisor_runner.rs` — extracted `build_runner_for_provider` helper to keep `supervisor.rs` under 800 lines
- `mur-agent-runtime/tests/skill_runtime_e2e.rs` — single end-to-end test covering boot → inject → trigger → Layer-3 swap → sandbox gate

**Modify:**
- `mur-common/src/config.rs` — add `SkillsConfig` + `Config::skills`
- `mur-common/src/skill/local.rs` — add `list_installed_agent(mur_home, agent_name)`
- `mur-common/src/skill/mod.rs` — re-export `loader::*`
- `mur-agent-runtime/src/lib.rs` — register `skills` module + new helper module
- `mur-agent-runtime/src/supervisor.rs` — call loader once, pass `LoadedSkills` into `build_runner_for_provider` (net line change ≤ 0 after extracting the 3× block)
- `mur-agent-runtime/src/task_runner.rs` — hold optional `Arc<RuntimeSkills>`; per-turn rebuild of system message with Layer-2/Layer-3 swap

---

## Self-contained Type Sketch

```rust
// mur-common/src/skill/loader.rs
pub enum SkillScope { Global, Agent }

pub struct LoadedSkill {
    pub name: String,
    pub manifest: SkillManifest,
    pub trust: TrustLevel,
    pub scope: SkillScope,
    pub content_hash: String,
}

pub fn load_all(mur_home: &Path, agent_name: &str) -> Vec<LoadedSkill>;
// - lists global + agent, dedupes (agent overrides global by name)
// - drift-checks each manifest against SkillTrustStore; drops drifted skills with a tracing::warn
// - never returns an Err: a malformed skill is skipped and logged, agent must still boot

// mur-agent-runtime/src/skills/mod.rs
pub struct RuntimeSkills {
    pub all: Vec<LoadedSkill>,
    pub triggers: Vec<RegisteredTrigger>,  // command+keyword only; session_start goes to injector
}
impl RuntimeSkills {
    pub fn build(loaded: Vec<LoadedSkill>) -> Self;
}

// mur-agent-runtime/src/skills/injector.rs
pub struct InjectionResult {
    pub system_addendum: String,        // formatted Layer-2 block; empty if budget skipped
    pub injected_names: Vec<String>,    // for telemetry
    pub budget_skipped: bool,
}
pub fn inject_layer2(
    skills: &[LoadedSkill],
    cfg: &SkillsConfig,
    context_fill_ratio: f64,
    recently_fired: &HashSet<String>,
) -> InjectionResult;
// - includes ONLY skills whose triggers contain SessionStart (per spec §4.1)
// - applies trust-then-priority sort, max_skills cap, char-based budget
// - returns empty when remaining-context ratio < adaptive.min_remaining_context_ratio
// - emits `skill_skip_context_full` via tracing for now (real telemetry hook lands in M3)

// mur-agent-runtime/src/skills/trigger_matcher.rs
pub struct RegisteredTrigger {
    pub skill_name: String,
    pub pattern: TriggerPattern,
    pub trust: TrustLevel,
}
pub enum TriggerPattern { Command(String), Keyword(Regex) }
impl TriggerMatcher {
    pub fn match_prompt<'a>(&'a self, prompt: &str) -> Vec<&'a RegisteredTrigger>;
}

// mur-agent-runtime/src/skills/sandbox_map.rs
pub fn restrict_for_trust(base: &Entitlements, trust: TrustLevel) -> Entitlements;
// returns a copy of `base` with limits tightened per trust level (see Task 6)
```

---

### Task 1 — `SkillsConfig` in `mur-common::config`

**Files:** `mur-common/src/config.rs`

- [ ] **1.1** Add the structs after the existing config block:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub max_skills_in_prompt: usize,
    pub max_total_tokens: usize,
    /// "agent" before "global" = per-agent skills win ties.
    pub priority_order: Vec<String>,
    pub adaptive: Option<AdaptiveSkillsConfig>,
}
impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            max_skills_in_prompt: 5,
            max_total_tokens: 2000,
            priority_order: vec!["agent".into(), "global".into()],
            adaptive: Some(AdaptiveSkillsConfig::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptiveSkillsConfig {
    pub context_fill_decay: f64,
    pub min_remaining_context_ratio: f64,
    pub recent_fire_boost_turns: usize,
}
impl Default for AdaptiveSkillsConfig {
    fn default() -> Self {
        Self {
            context_fill_decay: 1.5,
            min_remaining_context_ratio: 0.20,
            recent_fire_boost_turns: 5,
        }
    }
}
```

- [ ] **1.2** Add `pub skills: SkillsConfig` to `Config` (with `#[serde(default)]` so existing files still parse) and add a unit test that an empty YAML hydrates `Config { skills: default, .. }`.

- [ ] **1.3** Build + commit:
  ```bash
  cargo check -p mur-common && cargo test -p mur-common config::
  git add mur-common/src/config.rs
  git commit -m "feat(config): SkillsConfig with adaptive budget"
  ```

---

### Task 2 — Single-pass skill loader (`mur-common::skill::loader`)

**Files:** Create `mur-common/src/skill/loader.rs`; modify `mur-common/src/skill/local.rs` + `mod.rs`.

- [ ] **2.1** In `local.rs` add per-agent listing (mirror of `list_installed` but rooted in `agent_skill_dir`):

```rust
pub fn list_installed_agent(mur_home: &Path, agent_name: &str) -> Result<Vec<String>, StoreError> {
    let dir = crate::skill::store::agent_skill_dir(mur_home, agent_name);
    if !dir.exists() { return Ok(vec![]); }
    let mut names: Vec<_> = fs::read_dir(&dir).map_err(StoreError::Io)?
        .filter_map(|e| {
            let e = e.ok()?;
            if e.file_type().ok()?.is_dir() { e.file_name().to_str().map(str::to_string) } else { None }
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn load_installed_agent(mur_home: &Path, agent_name: &str, skill: &str)
    -> Result<SkillManifest, StoreError>
{
    read_from_dir(&crate::skill::store::agent_skill_dir(mur_home, agent_name).join(skill))
}
```

Add a unit test that creates `~/.mur/agents/test/skills/foo/skill.yaml` and asserts `list_installed_agent` returns `["foo"]`.

- [ ] **2.2** Create `loader.rs`:

```rust
//! Single-pass skill loader: lists global + per-agent skills,
//! resolves trust level, checks drift, returns one flat Vec.

use crate::skill::types::TrustLevel;
use crate::skill::{
    SkillManifest, content_sha256, drift_status, local, DriftStatus,
};
use crate::trust::skills::SkillTrustStore;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope { Global, Agent }

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub manifest: SkillManifest,
    pub trust: TrustLevel,
    pub scope: SkillScope,
    pub content_hash: String,
}

pub fn load_all(mur_home: &Path, agent_name: &str) -> Vec<LoadedSkill> {
    let trust = SkillTrustStore::load(mur_home).unwrap_or_default();
    let mut out: Vec<LoadedSkill> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = Default::default();

    // Per-agent first (wins on name collision)
    if let Ok(names) = local::list_installed_agent(mur_home, agent_name) {
        for name in names {
            if let Some(loaded) = load_one(mur_home, &name, SkillScope::Agent, &trust,
                                          |m, n| local::load_installed_agent(m, agent_name, n)) {
                seen_names.insert(loaded.name.clone());
                out.push(loaded);
            }
        }
    }
    if let Ok(names) = local::list_installed(mur_home) {
        for name in names {
            if seen_names.contains(&name) { continue; }
            if let Some(loaded) = load_one(mur_home, &name, SkillScope::Global, &trust,
                                          |m, n| local::load_installed(m, n)) {
                out.push(loaded);
            }
        }
    }
    out
}

fn load_one<F>(
    mur_home: &Path, name: &str, scope: SkillScope, trust: &SkillTrustStore, loader: F,
) -> Option<LoadedSkill>
where
    F: FnOnce(&Path, &str) -> Result<SkillManifest, crate::skill::StoreError>,
{
    let manifest = match loader(mur_home, name) {
        Ok(m) => m,
        Err(e) => { tracing::warn!(skill = %name, error = %e, "skill load failed; skipping"); return None; }
    };
    let hash = match content_sha256(&manifest) {
        Ok(h) => h,
        Err(e) => { tracing::warn!(skill = %name, error = %e, "skill hash failed; skipping"); return None; }
    };
    // Drift check: if there's a pinned hash for this skill in the trust store
    // and it disagrees, refuse to load.
    let entry = trust.entries.get(&hash);
    if let Some(pinned) = entry {
        if let Ok(DriftStatus::Drift { expected, actual }) = drift_status(&manifest, Some(&hash)) {
            tracing::warn!(skill = %name, expected, actual, "skill drift detected; skipping");
            return None;
        }
        if trust.is_revoked(&hash) {
            tracing::warn!(skill = %name, "skill hash revoked; skipping");
            return None;
        }
        Some(LoadedSkill { name: name.into(), manifest, trust: pinned.level, scope, content_hash: hash })
    } else {
        // Unpinned = first-load Sandboxed, per spec §2 Layer 2.
        Some(LoadedSkill { name: name.into(), manifest, trust: TrustLevel::Sandboxed, scope, content_hash: hash })
    }
}
```

Re-export from `mur-common/src/skill/mod.rs`:
```rust
pub mod loader;
pub use loader::{LoadedSkill, SkillScope, load_all};
```

- [ ] **2.3** Tests in `loader.rs`:
  1. Empty `~/.mur` → returns `vec![]`.
  2. One global skill, no trust entry → returned at `Sandboxed`.
  3. Same skill name in global + agent → only agent's `LoadedSkill` returned, with `scope = Agent`.
  4. Trust entry with mismatched hash (write a manifest, take its hash, mutate the manifest, install a `TrustEntry` keyed on the OLD hash) → loader returns nothing for that skill.

- [ ] **2.4** Build + commit:
  ```bash
  cargo test -p mur-common skill::loader
  git add mur-common/src/skill/ mur-common/src/skill/mod.rs
  git commit -m "feat(skill): single-pass loader with drift and per-agent scope"
  ```

---

### Task 3 — `SkillInjector`: session_start only, adaptive budget

**Files:** Create `mur-agent-runtime/src/skills/mod.rs` + `injector.rs`; modify `lib.rs`.

- [ ] **3.1** `skills/mod.rs`:

```rust
pub mod injector;
pub mod trigger_matcher;
pub mod sandbox_map;

use mur_common::skill::loader::LoadedSkill;
use trigger_matcher::RegisteredTrigger;

pub struct RuntimeSkills {
    pub loaded: Vec<LoadedSkill>,
    pub triggers: Vec<RegisteredTrigger>,
}
impl RuntimeSkills {
    pub fn build(loaded: Vec<LoadedSkill>) -> Self {
        let triggers = trigger_matcher::register_from(&loaded);
        Self { loaded, triggers }
    }
}
```

- [ ] **3.2** `skills/injector.rs`:

```rust
use mur_common::config::SkillsConfig;
use mur_common::skill::loader::LoadedSkill;
use mur_common::skill::types::{HostId, TrustLevel};
use mur_common::skill::TriggerKind;
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub struct InjectionResult {
    pub system_addendum: String,
    pub injected_names: Vec<String>,
    pub budget_skipped: bool,
}

pub fn inject_layer2(
    skills: &[LoadedSkill],
    cfg: &SkillsConfig,
    context_fill_ratio: f64,
    recently_fired: &HashSet<String>,
) -> InjectionResult {
    // Adaptive cutoff: skip entirely when remaining context is too small.
    if let Some(ad) = &cfg.adaptive {
        let remaining = 1.0 - context_fill_ratio;
        if remaining < ad.min_remaining_context_ratio {
            return InjectionResult { budget_skipped: true, ..Default::default() };
        }
    }

    // Filter: must have at least one `SessionStart` trigger.
    let mut candidates: Vec<&LoadedSkill> = skills.iter()
        .filter(|s| s.manifest.hosts.is_empty() || s.manifest.hosts.iter().any(|h| matches!(h, HostId::All | HostId::MurAgent)))
        .filter(|s| s.manifest.triggers.iter().any(|t| matches!(t.kind, TriggerKind::SessionStart)))
        .collect();

    // Sort: trust desc, recent-fired boost, then priority asc, then name for determinism.
    candidates.sort_by(|a, b| {
        let trust_cmp = b.trust.cmp(&a.trust);
        if trust_cmp != std::cmp::Ordering::Equal { return trust_cmp; }
        let a_recent = recently_fired.contains(&a.name);
        let b_recent = recently_fired.contains(&b.name);
        if a_recent != b_recent { return b_recent.cmp(&a_recent); }
        a.manifest.priority.cmp(&b.manifest.priority).then(a.name.cmp(&b.name))
    });

    candidates.truncate(cfg.max_skills_in_prompt);

    // Adaptive token budget (char-based proxy; honest token counting lands later).
    let budget = cfg.adaptive.as_ref().map(|ad| {
        let remaining = 1.0 - context_fill_ratio;
        ((cfg.max_total_tokens as f64) * remaining.powf(ad.context_fill_decay)) as usize
    }).unwrap_or(cfg.max_total_tokens).max(100);

    let mut spent = 0usize;
    let mut lines = Vec::new();
    let mut names = Vec::new();
    for s in candidates {
        let line = format!("[Skill: {} ({:?})] {}",
                           s.name, s.trust, s.manifest.content.r#abstract.trim());
        if spent + line.len() + 1 > budget { continue; }
        spent += line.len() + 1;
        lines.push(line);
        names.push(s.name.clone());
    }
    if lines.is_empty() {
        return InjectionResult::default();
    }
    InjectionResult {
        system_addendum: format!("\n--- Bound Skills ---\n{}\n---\n", lines.join("\n")),
        injected_names: names,
        budget_skipped: false,
    }
}
```

- [ ] **3.3** Tests in `injector.rs`:
  1. No `session_start` trigger → not injected even if other triggers present.
  2. Two skills, one Sandboxed one Trusted → Trusted appears first.
  3. `recently_fired` contains a Sandboxed skill's name; trust still wins (recent-fired only breaks ties within same trust).
  4. `context_fill_ratio = 0.9` with default config (`min_remaining = 0.2`) → not skipped, but `spent` is materially smaller than baseline.
  5. `context_fill_ratio = 0.85` → `budget_skipped == true`.
  6. `max_skills_in_prompt = 2` with 5 candidates → exactly 2 selected.

- [ ] **3.4** Register `pub mod skills;` in `mur-agent-runtime/src/lib.rs`. Build + commit:
  ```bash
  cargo test -p mur-agent-runtime --lib skills::injector
  git add mur-agent-runtime/src/skills/ mur-agent-runtime/src/lib.rs
  git commit -m "feat(runtime): skill injector (session_start, adaptive budget)"
  ```

---

### Task 4 — `TriggerMatcher` (command + keyword)

**Files:** `mur-agent-runtime/src/skills/trigger_matcher.rs`

- [ ] **4.1** Implementation:

```rust
use mur_common::skill::loader::LoadedSkill;
use mur_common::skill::types::TrustLevel;
use mur_common::skill::TriggerKind;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct RegisteredTrigger {
    pub skill_name: String,
    pub pattern: TriggerPattern,
    pub trust: TrustLevel,
}

#[derive(Debug, Clone)]
pub enum TriggerPattern {
    Command(String),
    Keyword(Regex),
}

pub fn register_from(skills: &[LoadedSkill]) -> Vec<RegisteredTrigger> {
    let mut out = Vec::new();
    for s in skills {
        for t in &s.manifest.triggers {
            let p_opt = match (&t.kind, &t.pattern) {
                (TriggerKind::Command, Some(p)) => Some(TriggerPattern::Command(p.clone())),
                (TriggerKind::Keyword, Some(p)) => match Regex::new(p) {
                    Ok(rx) => Some(TriggerPattern::Keyword(rx)),
                    Err(e) => { tracing::warn!(skill = %s.name, pattern = %p, error = %e, "bad keyword regex"); None }
                },
                _ => None,
            };
            if let Some(pattern) = p_opt {
                out.push(RegisteredTrigger { skill_name: s.name.clone(), pattern, trust: s.trust });
            }
        }
    }
    out
}

pub fn match_prompt<'a>(triggers: &'a [RegisteredTrigger], prompt: &str) -> Vec<&'a RegisteredTrigger> {
    triggers.iter().filter(|t| match &t.pattern {
        TriggerPattern::Command(cmd) => prompt.trim_start().starts_with(cmd),
        TriggerPattern::Keyword(rx) => rx.is_match(prompt),
    }).collect()
}
```

- [ ] **4.2** Helper to load Layer 3 body from a manifest:

```rust
pub fn layer3_body(manifest: &mur_common::skill::SkillManifest) -> Option<String> {
    let c = &manifest.content;
    if let Some(ctx) = &c.context { return Some(ctx.clone()); }
    if let Some(p) = &c.procedure {
        return Some(p.steps.iter().map(|s| s.description.clone()).collect::<Vec<_>>().join("\n"));
    }
    c.command.clone()
}

pub fn format_layer3(skill_name: &str, trust: TrustLevel, body: &str) -> String {
    format!("<skill-instruction source=\"{skill_name}\" trust=\"{trust:?}\">\n{body}\n</skill-instruction>")
}
```

- [ ] **4.3** Tests:
  1. `/research foo` matches command trigger `/research`.
  2. `find prices` matches keyword regex `(find|search) prices`.
  3. Invalid keyword regex is dropped with warn, no panic.
  4. `format_layer3` produces the exact opening tag `<skill-instruction source="x" trust="Sandboxed">`.

- [ ] **4.4** Commit:
  ```bash
  cargo test -p mur-agent-runtime --lib skills::trigger_matcher
  git add mur-agent-runtime/src/skills/trigger_matcher.rs
  git commit -m "feat(runtime): trigger matcher (command + keyword)"
  ```

---

### Task 5 — Wire into supervisor + TaskRunner (with extracted helper)

**Files:** Create `mur-agent-runtime/src/supervisor_runner.rs`; modify `supervisor.rs` and `task_runner.rs`.

- [ ] **5.1** **Extract `build_runner_for_provider` helper** out of `supervisor.rs` (lines ~294–390). The three branches (Ollama / Anthropic / OpenAI) currently each call `TaskRunner::with_llm(client).with_system_prompt(profile.system_prompt.clone())`; consolidate into:

```rust
// supervisor_runner.rs
use std::sync::Arc;
use crate::llm::LlmClient;
use crate::task_runner::TaskRunner;
use crate::skills::RuntimeSkills;

pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
) -> Arc<TaskRunner> {
    Arc::new(
        TaskRunner::with_llm(client)
            .with_system_prompt(base_system_prompt)
            .with_skills(skills),
    )
}
```

This **reduces** supervisor.rs net lines (3 sites collapse to 3 one-line calls).

- [ ] **5.2** In `supervisor.rs` after `profile` is loaded (before the provider match block), load skills:

```rust
let skills_cfg = load_skills_config(&mur_home);  // helper below
let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
let runtime_skills = Arc::new(crate::skills::RuntimeSkills::build(loaded));
```

`load_skills_config(mur_home)` reads `<mur_home>/config.yaml` into `Config` and extracts `skills`, falling back to `SkillsConfig::default()` if the file is absent. Tuck this in `mur-common/src/config.rs` as `Config::load_or_default(path: &Path) -> Config`.

Then each provider branch becomes:
```rust
let runner = crate::supervisor_runner::build_runner(client, profile.system_prompt.clone(), runtime_skills.clone());
```

- [ ] **5.3** In `task_runner.rs` extend `TaskRunner`:

```rust
use crate::skills::{RuntimeSkills, injector::inject_layer2, trigger_matcher::{match_prompt, layer3_body, format_layer3}};
use mur_common::config::SkillsConfig;
use std::collections::HashSet;
use std::sync::Arc;

pub struct TaskRunner {
    // ... existing fields ...
    skills: Option<Arc<RuntimeSkills>>,
    skills_cfg: SkillsConfig,
    recently_fired: std::sync::Mutex<std::collections::VecDeque<(u64, String)>>,
    turn_counter: std::sync::atomic::AtomicU64,
}

impl TaskRunner {
    pub fn with_skills(mut self, skills: Arc<RuntimeSkills>) -> Self {
        self.skills = Some(skills);
        self
    }
    pub fn with_skills_cfg(mut self, cfg: SkillsConfig) -> Self {
        self.skills_cfg = cfg;
        self
    }

    fn assemble_system_prompt(&self, user_prompt: &str) -> (String, Vec<String>) {
        let base = self.system_prompt.clone().unwrap_or_default();
        let Some(skills) = &self.skills else { return (base, vec![]); };

        let turn = self.turn_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let recently: HashSet<String> = {
            let q = self.recently_fired.lock().unwrap();
            let horizon = turn.saturating_sub(self.skills_cfg.adaptive
                .as_ref().map(|a| a.recent_fire_boost_turns as u64).unwrap_or(0));
            q.iter().filter(|(t, _)| *t >= horizon).map(|(_, n)| n.clone()).collect()
        };

        // ctx_fill is a stub for M2 (we don't have a token counter in run_llm yet).
        // M3 will wire honest context_fill via LlmResponse.input_tokens / model max_tokens.
        let ctx_fill: f64 = 0.0;

        let injection = inject_layer2(&skills.loaded, &self.skills_cfg, ctx_fill, &recently);
        let triggered = match_prompt(&skills.triggers, user_prompt);

        // Layer-3 blocks; record fires for recent-fired boosting.
        let mut layer3 = String::new();
        let mut suppress_names: HashSet<&str> = HashSet::new();
        for t in &triggered {
            let Some(loaded) = skills.loaded.iter().find(|s| s.name == t.skill_name) else { continue; };
            let Some(body) = layer3_body(&loaded.manifest) else { continue; };
            layer3.push('\n');
            layer3.push_str(&format_layer3(&loaded.name, loaded.trust, &body));
            suppress_names.insert(loaded.name.as_str());
            self.recently_fired.lock().unwrap().push_back((turn, loaded.name.clone()));
        }

        // Suppress Layer 2 lines for skills whose Layer 3 just loaded (spec §4.2 "replacing").
        let addendum = strip_lines_for(&injection.system_addendum, &suppress_names);

        let fired: Vec<String> = triggered.iter().map(|t| t.skill_name.clone()).collect();
        let mut combined = base;
        if !addendum.is_empty() { combined.push('\n'); combined.push_str(&addendum); }
        if !layer3.is_empty()   { combined.push('\n'); combined.push_str(&layer3); }
        (combined, fired)
    }
}

fn strip_lines_for(addendum: &str, names: &HashSet<&str>) -> String {
    if names.is_empty() { return addendum.to_string(); }
    addendum.lines()
        .filter(|line| !names.iter().any(|n| line.contains(&format!("[Skill: {n} "))))
        .collect::<Vec<_>>()
        .join("\n")
}
```

In `run_llm`, replace the `if let Some(sp) = &self.system_prompt { ... }` block with:
```rust
let user_text = text_of(input);
let (system, fired_skills) = self.assemble_system_prompt(&user_text);
if !system.is_empty() {
    messages.push(LlmMessage { role: "system".into(), content: system });
}
// fired_skills available for future telemetry emission
```

- [ ] **5.4** Tests in `task_runner.rs`:
  - `assemble_system_prompt` with no skills → returns base prompt unchanged.
  - With one `session_start` + one `command` skill, prompting `/cmd foo`: addendum suppresses the command skill's Layer-2 line and Layer-3 block appears.
  - Recently-fired Sandboxed skill outranks a non-recently-fired Sandboxed skill (same trust tier).

- [ ] **5.5** Build + commit:
  ```bash
  cargo test -p mur-agent-runtime --lib
  cargo check --workspace
  git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/supervisor_runner.rs \
          mur-agent-runtime/src/task_runner.rs mur-agent-runtime/src/lib.rs \
          mur-common/src/config.rs
  git commit -m "feat(runtime): wire skill injector + trigger matcher into TaskRunner"
  # Verify supervisor.rs is under 800 lines after extraction:
  wc -l mur-agent-runtime/src/supervisor.rs
  ```

---

### Task 6 — `TrustLevel` → `Entitlements` sandbox gate

**Files:** Create `mur-agent-runtime/src/skills/sandbox_map.rs`.

**Scope note for M2:** Because the hook chain's `pre_tool_use` isn't wired into the LLM call path (see Reality Check), tool-call gating during a skill-triggered turn is enforced via a **`SkillCallGate`** that we will call from `TaskRunner` once it gains tool-use support. For M2 we ship the **policy function and the integration point**, marked behind a `// TODO(M3): invoke from tool-use site` until tool-use lands. Document this explicitly in the PR description.

- [ ] **6.1** Implementation:

```rust
use mur_common::agent::{
    Entitlements, NetworkEntitlement, OutboundNetwork, NetworkOutboundMode,
    ProcessesEntitlement, SpawnEntitlement, SpawnMode,
};
use mur_common::skill::types::TrustLevel;

/// Tighten `base` according to `trust`. Never widens.
pub fn restrict_for_trust(base: &Entitlements, trust: TrustLevel) -> Entitlements {
    let mut e = base.clone();
    match trust {
        TrustLevel::Sandboxed => {
            // No network, no spawn, leave filesystem to the agent's existing FS entitlement.
            e.network.outbound = OutboundNetwork {
                mode: NetworkOutboundMode::Off,
                allow_hosts: vec![],
                protocols: vec![],
                resolve_dns: Default::default(),
            };
            e.processes = ProcessesEntitlement {
                spawn: SpawnEntitlement { mode: SpawnMode::Allowlist, allowed: vec![] },
            };
        }
        TrustLevel::Verified => {
            // Keep outbound mode but force Restricted (allowlist required).
            if matches!(e.network.outbound.mode, NetworkOutboundMode::Unrestricted) {
                e.network.outbound.mode = NetworkOutboundMode::Restricted;
            }
            // Spawn stays Allowlist; do not widen.
        }
        TrustLevel::Trusted => { /* pass-through */ }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{FilesystemEntitlement, InboundNetwork, LimitsEntitlement, ResolveDnsConfig, SyscallsEntitlement};

    fn base_open() -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Unrestricted,
                    allow_hosts: vec![],
                    protocols: vec!["tcp".into()],
                    resolve_dns: ResolveDnsConfig::default(),
                },
            },
            filesystem: FilesystemEntitlement::default(),
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement { mode: SpawnMode::Unrestricted, allowed: vec!["sh".into()] },
            },
            syscalls: SyscallsEntitlement::default(),
            limits: LimitsEntitlement::default(),
            llm: Default::default(),
        }
    }

    #[test]
    fn sandboxed_kills_network_and_spawn() {
        let e = restrict_for_trust(&base_open(), TrustLevel::Sandboxed);
        assert!(matches!(e.network.outbound.mode, NetworkOutboundMode::Off));
        assert!(matches!(e.processes.spawn.mode, SpawnMode::Allowlist));
        assert!(e.processes.spawn.allowed.is_empty());
    }

    #[test]
    fn verified_narrows_network_to_restricted() {
        let e = restrict_for_trust(&base_open(), TrustLevel::Verified);
        assert!(matches!(e.network.outbound.mode, NetworkOutboundMode::Restricted));
    }

    #[test]
    fn trusted_is_identity() {
        let before = base_open();
        let after = restrict_for_trust(&before, TrustLevel::Trusted);
        assert!(matches!(after.network.outbound.mode, NetworkOutboundMode::Unrestricted));
    }
}
```

- [ ] **6.2** **Integration sketch (no behavior change yet):** Add a `// TODO(M3)` comment at the top of `sandbox_map.rs` describing where it will be invoked from once `pre_tool_use` is wired. In `TaskRunner::run_llm`, after `assemble_system_prompt`, if `fired_skills` is non-empty, log a `tracing::info!(target = "skill.sandbox", fired = ?fired_skills, "would restrict via sandbox_map");` placeholder. This makes the seam observable.

- [ ] **6.3** Commit:
  ```bash
  cargo test -p mur-agent-runtime --lib skills::sandbox_map
  git add mur-agent-runtime/src/skills/sandbox_map.rs mur-agent-runtime/src/task_runner.rs
  git commit -m "feat(runtime): TrustLevel->Entitlements sandbox mapping (gate site stub for M3)"
  ```

---

### Task 7 — End-to-end integration test

**Files:** Create `mur-agent-runtime/tests/skill_runtime_e2e.rs`.

- [ ] **7.1** One test, three assertions:

```rust
use mur_common::config::SkillsConfig;
use mur_common::skill::{parse_canonical, write_to_dir};
use mur_common::skill::loader::load_all;
use mur_agent_runtime::skills::{RuntimeSkills, injector::inject_layer2, trigger_matcher::{match_prompt, layer3_body, format_layer3}};
use std::collections::HashSet;
use tempfile::tempdir;

#[test]
fn boot_inject_trigger_swap_e2e() {
    let dir = tempdir().unwrap();
    let mur_home = dir.path();

    // Skill A: session_start only -> always injected as Layer 2
    let a = parse_canonical(r#"
name: house-rules
version: 1.0.0
publisher: human:t
description: always-on rules
category: context
content:
  abstract: Always reply concisely.
  context: "Long body for house-rules."
triggers:
  - type: session_start
"#).unwrap();

    // Skill B: command -> Layer 3 swap path
    let b = parse_canonical(r#"
name: find-price
version: 1.0.0
publisher: human:t
description: find prices
category: context
content:
  abstract: Searches product prices.
  context: "Full procedure: navigate, search, extract."
triggers:
  - type: command
    pattern: /find-price
"#).unwrap();

    write_to_dir(&mur_home.join("skills").join("house-rules"), &a).unwrap();
    write_to_dir(&mur_home.join("skills").join("find-price"), &b).unwrap();

    let loaded = load_all(mur_home, "alice");
    assert_eq!(loaded.len(), 2);

    let runtime = RuntimeSkills::build(loaded);

    // (1) Layer 2 contains house-rules, not find-price (no session_start).
    let inj = inject_layer2(&runtime.loaded, &SkillsConfig::default(), 0.0, &HashSet::new());
    assert!(inj.system_addendum.contains("house-rules"));
    assert!(!inj.system_addendum.contains("find-price"));

    // (2) Trigger matcher finds find-price for "/find-price airpods".
    let matched = match_prompt(&runtime.triggers, "/find-price airpods");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].skill_name, "find-price");

    // (3) Layer 3 body + formatting end-to-end.
    let body = layer3_body(&runtime.loaded.iter().find(|s| s.name == "find-price").unwrap().manifest).unwrap();
    let wrapped = format_layer3("find-price", matched[0].trust, &body);
    assert!(wrapped.starts_with("<skill-instruction source=\"find-price\""));
    assert!(wrapped.contains("Full procedure"));
}
```

- [ ] **7.2** Run + commit:
  ```bash
  cargo test -p mur-agent-runtime --test skill_runtime_e2e
  git add mur-agent-runtime/tests/skill_runtime_e2e.rs
  git commit -m "test(runtime): e2e boot->inject->trigger->layer3 swap"
  ```

---

## Self-Review

**Spec coverage (`docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md` §14 M2 checklist):**

| M2 item | Task |
|---|---|
| Agent runtime reads skills | T2 (loader), T5.2 (supervisor wiring) |
| Inject Layer 2 into system prompt | T3 (session_start only) + T5.3 (`assemble_system_prompt`) |
| Token budget + trust+priority ordering | T3 |
| Adaptive budget (Context Rot) | T3 (`min_remaining_context_ratio` + decay) |
| Trigger matching engine | T4 |
| Layer 3 on-demand loading + Layer 2 replacement | T5.3 (`strip_lines_for`) |
| Per-skill sandbox derived from trust | T6 (policy function; integration point staged for M3 hook-chain wiring) |
| Skill instruction boundary `<skill-instruction>` | T4.2 `format_layer3` |
| B0 hook chain applies to skill-triggered tool calls | **Deferred to M3** — flagged in Reality Check; needs prior wiring of `pre_tool_use` into TaskRunner |
| SHA-256 drift detection at load time | T2.2 `load_one` |

**What this plan does NOT do (and why it's OK to defer):**
- `pre_tool_use` enforcement: blocked on a prior architecture change (hook chain isn't called from `run_llm`). T6 ships the policy function so M3's wiring step is a one-liner.
- Honest token-count-based `ctx_fill`: needs a token counter that knows the active model; lands when `LlmResponse.input_tokens` is plumbed back to TaskRunner.
- Skill execution telemetry events (`skill_injected`, `skill_skip_context_full`): M2 ships `tracing::warn` / `tracing::info` only; structured telemetry hooks land in M4 per spec.
- A `mur skill config` inspector command: not required by M2 checklist; defer to M2.1 if needed.

**Line-count compliance:** Task 5.1 extracts a helper that nets ~30 lines OUT of `supervisor.rs` (3 sites × 5 lines collapsed to 3 × 1 line, plus the new helper file). After M2 ships `supervisor.rs` must measure < 800 (currently 969; budget 169 line removal from that file alone). T5.5 includes the `wc -l` verification.

**Placeholder scan:** Only intentional `// TODO(M3)` in `sandbox_map.rs` invocation site, called out explicitly in T6.2.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m2.md`. Before kicking off:

1. Confirm the **pragmatic-vs-hook-chain** architectural decision (Reality Check, last row) — the plan as written takes the pragmatic path and stages the hook-chain rewire for M3.
2. Confirm the **supervisor.rs line-budget extraction** (T5.1) is acceptable in the same PR as the skill wiring.

Two execution options:
1. **Subagent-driven (recommended)** — fresh subagent per task with review between.
2. **Inline executing-plans** — same session with checkpoints after T3, T5, T7.
