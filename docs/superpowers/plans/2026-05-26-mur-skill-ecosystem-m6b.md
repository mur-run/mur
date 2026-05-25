# M6b — Dynamic Tool Resolution + MCP Execution Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Skills can declare *intent* (what they want done) instead of hard-coding tool names. At skill-injection time the runtime resolves intent + tool_hint + agent's configured MCP server inventory to a concrete tool name, and the resolved name is what reaches the agent's prompt. Skills that already use literal tool names keep working unchanged.

**Spec mapping:** §11.2 MCP as the skill execution substrate, §11.3 Dynamic Tool Resolution. §M6 second + third bullets.

**Hard dependency on M6a:**
- `mur-common::skill::mcp::{SkillCapability, McpRequirement}` and `SkillManifest.mcp_requirements` exist.
- Manifest schema version `2.1` is supported (M6b bumps to `2.2` for the new step keys).
- Doctor checks `mcp-requirements-coverage` + `mcp-capability-available` exist (M6b adds a third: `intent-resolvable`).

**Architectural premise (read before writing code).** Today's skills are **prompt-injected**, not runtime-executed. `mur-agent-runtime/src/skills/trigger_matcher.rs::layer3_body` emits a `<skill-instruction>` block into the agent's prompt; the agent (an LLM) then decides what tools to call. M6b does **not** turn mur into an MCP dispatcher. Instead, M6b resolves `intent` → concrete tool name **at inject time**, so the agent sees a procedure with the *right* tool name for the currently configured MCP servers. This is "MCP as execution substrate" in the sense of "MCP shapes what the agent sees", not "mur calls MCP for the agent." Active dispatch is M7 territory.

**What M6b ships:**
1. Two optional fields on `ProcedureStep`: `intent: Option<String>` and `tool_hint: Option<String>`.
2. Manifest schema version `2.1` → `2.2` (additive).
3. `mur-core::skill_resolve::resolver` — pure function `resolve_step(step, available_tools, requirements) -> Resolution`.
4. Inject-time integration: `mur-agent-runtime/src/skills/trigger_matcher.rs::layer3_body` runs the resolver per step and emits resolved tool names.
5. `SkillStats` extension (additive — applies M5b Task 0 policy): `resolution_misses: u64` counter increments when a step's intent could not be resolved at inject time.
6. New doctor check `intent-resolvable` (Warning) — flags any step whose `intent` has no candidate match in the agent's MCP inventory.
7. Telemetry: `Event::SkillStepResolved { skill_name, step_index, intent, picked_tool, source }` where `source` is `Literal | Hint | IntentMatch | Fallback | Unresolved`.

**What M6b does NOT ship:**
- mur itself calling MCP tools (active dispatch) → M7+.
- An ontology / taxonomy of intents (we use opaque strings; intent matching is by `tool_hint` exact match against the agent's tool inventory, then by `mcp_requirements` tool_pattern glob match — no semantic similarity).
- LLM-driven intent inference for legacy steps with no `intent` set → M6c.
- Cross-agent intent vocabulary harmonisation → M7.
- A UI for browsing available intents → out of scope; `mur skill show <name>` already covers display.

**Tech Stack:** Rust 2024. Re-uses `globset` (already in `mur-core` via M5a, possibly already moved to `mur-common` by M6a Task 1 Step 2). No new dependencies.

---

## File Structure

**Create:**
- `mur-core/src/skill_resolve/mod.rs` — `resolve_step` pure function, `Resolution` enum, ranking logic.
- `mur-core/src/skill_resolve/inventory.rs` — `McpInventory` snapshot (the set of tool names visible to an agent right now), built once per inject pass.
- `mur-core/src/skill_doctor/checks/intent_resolvable.rs` — Warning-level check.
- `mur-core/tests/skill_resolve_unit.rs` — pure-function table-driven tests (no async, no I/O).
- `mur-core/tests/skill_resolve_inject.rs` — end-to-end: load skill with `intent`, build inventory from a mock MCP registry, invoke `layer3_body`, assert resolved tool name appears in the rendered prompt block.

**Modify:**
- `mur-common/src/skill/manifest.rs` — `ProcedureStep` gains `intent` and `tool_hint`, both `Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `mur-common/src/skill/version.rs` (created in M6a) — bump `SKILL_MANIFEST_SCHEMA_VERSION` to `"2.2"`, extend `is_supported` to `{2.0, 2.1, 2.2}`.
- `mur-common/src/skill/validate.rs` — validate the new fields (intent and tool_hint cannot both be empty when present; if present, the step's literal `tool` is treated as a hint not a hard binding — document this precedence).
- `mur-common/src/skill/stats.rs` — add `resolution_misses: u64` with `#[serde(default)]`.
- `mur-agent-runtime/src/skills/trigger_matcher.rs::layer3_body` — replace the existing step rendering with a resolver-driven render.
- `mur-agent-runtime/src/skills/injector.rs` — thread `McpInventory` through so `layer3_body` has the inventory at call time.
- `mur-agent-runtime/src/telemetry_writer.rs` — add `Event::SkillStepResolved` variant.
- `mur-core/src/skill_doctor/mod.rs` — register `IntentResolvable` check.
- `mur-core/src/cmd/skill_show.rs` — render `intent` / `tool_hint` on each step (cosmetic).

**Do not modify:**
- Trigger matching (`register_from`, `match_prompt`). Resolution is post-trigger, pre-inject.
- DSSE signing. New optional fields are skip-serializing-if-none, so byte-identical re-serialization of M6a manifests holds (Task 1 Step 6 re-runs the M6a round-trip test against M6b loader).
- `SkillStats` writers other than the new counter. Existing aggregator code is untouched; only the schema gains a field.

---

### Task 1 — Manifest schema: `intent` + `tool_hint`

**Files:** `mur-common/src/skill/manifest.rs` (modify), `mur-common/src/skill/version.rs` (modify), `mur-common/src/skill/validate.rs` (modify).

- [ ] **Step 1: Extend `ProcedureStep`**

```rust
// mur-common/src/skill/manifest.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureStep {
    pub description: String,

    /// Literal tool name. Pre-M6b behaviour: hard binding. Post-M6b: treated
    /// as a hint when `intent` is also set; otherwise still a hard binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// What the step is trying to accomplish. Free-form string, no central
    /// taxonomy. Resolved at inject time against the agent's MCP inventory.
    /// When set, the resolver prefers a tool whose name matches a glob in
    /// `mcp_requirements` over the literal `tool` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,

    /// Preferred tool name pattern (glob). Used as a tiebreaker among
    /// resolver candidates. Falls back to literal `tool`, then to any
    /// `mcp_requirements` match for the intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_hint: Option<String>,
}
```

Precedence is documented inline because it is the single most likely source of confusion.

- [ ] **Step 2: Schema version bump**

```rust
// mur-common/src/skill/version.rs

pub const SKILL_MANIFEST_SCHEMA_VERSION: &str = "2.2";
pub fn is_supported(version: &str) -> bool { matches!(version, "2.0" | "2.1" | "2.2") }
```

- [ ] **Step 3: Validation**

In `validate.rs`, after the M6a `mcp::validate_requirements` call, walk steps:
```rust
for (idx, step) in steps.iter().enumerate() {
    if let Some(hint) = &step.tool_hint {
        if hint.is_empty() {
            errors.push(ValidationError {
                path: format!("content.procedure.steps[{idx}].tool_hint"),
                message: "tool_hint must not be empty when present".into(),
            });
        }
    }
    if let Some(intent) = &step.intent {
        if intent.is_empty() {
            errors.push(ValidationError {
                path: format!("content.procedure.steps[{idx}].intent"),
                message: "intent must not be empty when present".into(),
            });
        }
    }
    // intent without any way to pick a tool is suspicious but not an error;
    // doctor's `intent-resolvable` check (Task 5) will flag it.
}
```

- [ ] **Step 4: Backwards-compat tests**

Three fixtures:
- v2.0 (no `mcp_requirements`, no `intent`) — loads, round-trips, no fields added.
- v2.1 (has `mcp_requirements`, no `intent`) — loads, round-trips, signature scope unchanged.
- v2.2 (has `intent` + `tool_hint`) — loads, validate passes.

Critical: byte-identical re-serialization for v2.0 and v2.1 fixtures (no surprise added fields). Add this assertion in the test.

- [ ] **Step 5: Build + commit**

```
cargo build -p mur-common
cargo test -p mur-common --test skill_manifest_compat
git add mur-common/src/skill/{manifest.rs,version.rs,validate.rs}
git commit -m "feat(skill): intent + tool_hint on ProcedureStep (schema v2.2)"
```

---

### Task 2 — `McpInventory` snapshot

**Files:** `mur-core/src/skill_resolve/inventory.rs` (new), `mur-core/src/skill_resolve/mod.rs` (new).

The inventory is a read-only snapshot of every MCP tool name visible to an agent at the moment of injection. Cheap to build, cheap to clone (it's just `Vec<String>` behind an `Arc`). Built once per inject pass and threaded into the resolver.

- [ ] **Step 1: Define `McpInventory`**

```rust
// mur-core/src/skill_resolve/inventory.rs

use std::sync::Arc;

/// Read-only snapshot of MCP tool names visible to one agent.
#[derive(Debug, Clone, Default)]
pub struct McpInventory {
    tools: Arc<Vec<String>>,
}

impl McpInventory {
    pub fn from_tool_names(tools: Vec<String>) -> Self {
        Self { tools: Arc::new(tools) }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool { self.tools.is_empty() }
}
```

- [ ] **Step 2: Builder from the agent runtime side**

In `mur-agent-runtime/src/skills/injector.rs`, before iterating skills, call into the existing MCP registry (the read-side wrapper added in M6a Task 4 Step 1) to build the inventory once. If that wrapper is async, build the inventory upstream in the same async context that already collects skills.

```rust
// mur-agent-runtime/src/skills/injector.rs (modify)
async fn inject_layer3(...) -> ... {
    let inventory = build_mcp_inventory(ctx).await.unwrap_or_default();
    // existing skill-iteration loop, but `layer3_body(manifest, &inventory)` instead of (manifest)
}
```

If `build_mcp_inventory` fails (no MCP servers configured, registry unreadable, etc.), `unwrap_or_default()` yields an empty inventory — the resolver then falls back to literal tool names everywhere, which is exactly today's behaviour.

- [ ] **Step 3: Build + commit**

```
cargo build -p mur-core -p mur-agent-runtime
git add mur-core/src/skill_resolve/{mod.rs,inventory.rs} mur-agent-runtime/src/skills/injector.rs
git commit -m "feat(skill): McpInventory snapshot for resolver"
```

---

### Task 3 — `resolve_step` pure function

**Files:** `mur-core/src/skill_resolve/mod.rs` (extend).

The resolver is **pure** — no I/O, no async, easy to table-test. Decision tree:

1. If `step.tool` is set AND `intent` is unset → return `Literal(step.tool)`. (Pre-M6b behaviour preserved exactly.)
2. If `tool_hint` is set AND inventory contains a tool matching `tool_hint` (treated as glob if it contains `*`, else literal) → return `Hint(tool)`.
3. If `intent` is set AND any `mcp_requirements` entry has a `tool_pattern` matching at least one inventory tool → return `IntentMatch(picked_tool, capability)`. Among multiple matches, pick the **shortest** tool name (deterministic tiebreaker; shortest tends to be the canonical form).
4. If `intent` is set AND `mcp_requirements` has a fallback for the matching capability and the fallback name is in the inventory → return `Fallback(fallback)`.
5. Otherwise → return `Unresolved { reason }`.

- [ ] **Step 1: Type definitions**

```rust
// mur-core/src/skill_resolve/mod.rs

pub mod inventory;
pub use inventory::McpInventory;

use mur_common::skill::manifest::ProcedureStep;
use mur_common::skill::mcp::{McpRequirement, SkillCapability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Step had a literal `tool` and no `intent` — pre-M6b path.
    Literal { tool: String },
    /// `tool_hint` matched an inventory entry.
    Hint { tool: String },
    /// An `mcp_requirements` glob matched at least one inventory tool.
    IntentMatch { tool: String, capability: SkillCapability },
    /// No glob match, but the requirement's `fallback` exists in the inventory.
    Fallback { tool: String, capability: SkillCapability },
    /// No usable tool. Emitted as text in the prompt so the agent sees the intent
    /// description but knows no MCP server is wired up.
    Unresolved { reason: String },
}

impl Resolution {
    pub fn picked_tool(&self) -> Option<&str> {
        match self {
            Resolution::Literal { tool } | Resolution::Hint { tool }
            | Resolution::IntentMatch { tool, .. } | Resolution::Fallback { tool, .. } => Some(tool),
            Resolution::Unresolved { .. } => None,
        }
    }
    pub fn source_tag(&self) -> &'static str {
        match self {
            Resolution::Literal { .. }     => "literal",
            Resolution::Hint { .. }        => "hint",
            Resolution::IntentMatch { .. } => "intent_match",
            Resolution::Fallback { .. }    => "fallback",
            Resolution::Unresolved { .. }  => "unresolved",
        }
    }
}
```

- [ ] **Step 2: The resolver**

```rust
pub fn resolve_step(
    step: &ProcedureStep,
    requirements: &[McpRequirement],
    inventory: &McpInventory,
) -> Resolution {
    // Rule 1
    if step.intent.is_none() {
        if let Some(t) = step.tool.as_deref() {
            return Resolution::Literal { tool: t.to_string() };
        }
        return Resolution::Unresolved { reason: "step has neither tool nor intent".into() };
    }

    // Rule 2 — tool_hint
    if let Some(hint) = step.tool_hint.as_deref() {
        if let Some(t) = match_in_inventory(hint, inventory) {
            return Resolution::Hint { tool: t };
        }
    }

    // Rule 3 — intent_match via mcp_requirements
    let mut best: Option<(String, SkillCapability)> = None;
    for req in requirements {
        let Ok(glob) = globset::Glob::new(&req.tool_pattern) else { continue; };
        let m = glob.compile_matcher();
        let mut candidates: Vec<&str> = inventory.iter().filter(|t| m.is_match(t)).collect();
        if candidates.is_empty() { continue; }
        // Shortest name wins; lexicographically smallest on ties.
        candidates.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        let pick = candidates[0].to_string();
        // First requirement that finds a match wins — declaration order is meaningful.
        best = Some((pick, req.capability));
        break;
    }
    if let Some((tool, cap)) = best {
        return Resolution::IntentMatch { tool, capability: cap };
    }

    // Rule 4 — fallback
    for req in requirements {
        if req.fallback.is_empty() { continue; }
        if inventory.contains(&req.fallback) {
            return Resolution::Fallback { tool: req.fallback.clone(), capability: req.capability };
        }
    }

    // Rule 5
    Resolution::Unresolved {
        reason: format!(
            "intent '{}' has no matching tool in inventory ({} tools, {} requirements)",
            step.intent.as_deref().unwrap_or(""),
            inventory.iter().count(),
            requirements.len(),
        ),
    }
}

fn match_in_inventory(pattern: &str, inv: &McpInventory) -> Option<String> {
    if pattern.contains('*') {
        let Ok(g) = globset::Glob::new(pattern) else { return None; };
        let m = g.compile_matcher();
        let mut hits: Vec<&str> = inv.iter().filter(|t| m.is_match(t)).collect();
        hits.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        hits.first().map(|s| s.to_string())
    } else {
        inv.contains(pattern).then(|| pattern.to_string())
    }
}
```

- [ ] **Step 3: Table-driven tests**

```rust
// mur-core/tests/skill_resolve_unit.rs

use mur_common::skill::manifest::ProcedureStep;
use mur_common::skill::mcp::{McpRequirement, SkillCapability};
use mur_core::skill_resolve::{McpInventory, Resolution, resolve_step};

fn step(tool: Option<&str>, intent: Option<&str>, hint: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        description: "x".into(),
        tool: tool.map(String::from),
        intent: intent.map(String::from),
        tool_hint: hint.map(String::from),
    }
}

#[test]
fn literal_only_unchanged() {
    let r = resolve_step(&step(Some("browser.navigate"), None, None), &[], &McpInventory::default());
    assert_eq!(r, Resolution::Literal { tool: "browser.navigate".into() });
}

#[test]
fn intent_match_picks_shortest_glob_hit() {
    let inv = McpInventory::from_tool_names(vec![
        "browser.navigate".into(),
        "browser.navigate.full_page".into(),
        "browser.click".into(),
    ]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(), capability: SkillCapability::NetworkHttp, fallback: "".into(),
    }];
    let r = resolve_step(&step(None, Some("navigate"), None), &reqs, &inv);
    // Shortest among matches is "browser.click" (13) tied with "browser.navigate" (16) — 13 wins.
    assert_eq!(r, Resolution::IntentMatch { tool: "browser.click".into(), capability: SkillCapability::NetworkHttp });
}
```

(More cases: tool_hint glob, fallback path, unresolved, ordering of requirements, etc. Cover each rule.)

- [ ] **Step 4: Build + commit**

```
cargo test -p mur-core --test skill_resolve_unit
git add mur-core/src/skill_resolve/ mur-core/tests/skill_resolve_unit.rs
git commit -m "feat(skill): resolve_step — intent + tool_hint resolver"
```

---

### Task 4 — Inject-time integration

**Files:** `mur-agent-runtime/src/skills/trigger_matcher.rs` (modify), `mur-agent-runtime/src/skills/injector.rs` (modify).

- [ ] **Step 1: Pass inventory + requirements through `layer3_body`**

```rust
// mur-agent-runtime/src/skills/trigger_matcher.rs

pub fn layer3_body(
    manifest: &mur_common::skill::SkillManifest,
    inventory: &mur_core::skill_resolve::McpInventory,
) -> Option<String> {
    let c = &manifest.content;
    if let Some(ctx) = &c.context { return Some(ctx.clone()); }

    if let Some(p) = &c.procedure {
        let lines: Vec<String> = p.steps.iter().enumerate().map(|(i, step)| {
            let res = mur_core::skill_resolve::resolve_step(step, &manifest.mcp_requirements, inventory);
            render_step(i + 1, step, &res)
        }).collect();
        return Some(lines.join("\n"));
    }
    c.command.clone()
}

fn render_step(idx: usize, step: &ProcedureStep, res: &Resolution) -> String {
    match res.picked_tool() {
        Some(tool) => format!("{idx}. {} — tool: {tool}", step.description),
        None => format!("{idx}. {} — (no tool available: {})", step.description, match res {
            Resolution::Unresolved { reason } => reason.as_str(),
            _ => "unknown",
        }),
    }
}
```

- [ ] **Step 2: Inventory plumbing in `injector.rs`**

Build the inventory once per inject pass (Task 2 Step 2). Thread it into every `layer3_body` call. Skills with `mode == Context` skip the resolver entirely — confirm by reading the `if let Some(ctx)` early-return above.

- [ ] **Step 3: Telemetry emission**

For every step where `res.source_tag() != "literal"`, emit `Event::SkillStepResolved`. Literal resolution is the default path and would flood telemetry; skip it.

```rust
if res.source_tag() != "literal" {
    let _ = ctx.telemetry.try_send(Event::SkillStepResolved {
        skill_name: manifest.name.clone(),
        step_index: i,
        intent: step.intent.clone(),
        picked_tool: res.picked_tool().map(String::from),
        source: res.source_tag().into(),
    });
    if matches!(res, Resolution::Unresolved { .. }) {
        // Increment SkillStats.resolution_misses via the aggregator (M5a path).
        let _ = ctx.stats_tx.try_send(StatsDelta::ResolutionMiss(manifest.name.clone()));
    }
}
```

- [ ] **Step 4: Tests**

`mur-core/tests/skill_resolve_inject.rs`:
- Load a v2.2 skill fixture with one literal step + one intent step.
- Build inventory `["browser.navigate"]`.
- Call `layer3_body`. Assert the rendered string contains both `browser.navigate` (for the literal step) and the resolver's pick for the intent step.
- Confirm v2.0 / v2.1 fixtures still produce identical output to pre-M6b (regression test).

- [ ] **Step 5: Build + commit**

```
cargo build -p mur-core -p mur-agent-runtime
cargo test -p mur-core --test skill_resolve_inject
git add mur-agent-runtime/src/skills/{trigger_matcher.rs,injector.rs} mur-core/tests/skill_resolve_inject.rs
git commit -m "feat(skill): inject-time intent resolution"
```

---

### Task 5 — Doctor check: `intent-resolvable` (Warning)

**Files:** `mur-core/src/skill_doctor/checks/intent_resolvable.rs` (new), `mur-core/src/skill_doctor/mod.rs` (modify).

- [ ] **Step 1: Implement**

For each step with `intent.is_some()`, call `resolve_step` against the doctor's MCP inventory snapshot. If the result is `Resolution::Unresolved`, emit one Warning per step.

```rust
pub struct IntentResolvable;

impl Check for IntentResolvable {
    fn id(&self) -> CheckId { CheckId("intent-resolvable") }

    fn run(&self, skill: &Skill, ctx: &super::Ctx) -> Vec<Finding> {
        let Some(proc) = &skill.manifest.content.procedure else { return vec![]; };
        let inventory = ctx.mcp_inventory();
        let reqs = &skill.manifest.mcp_requirements;

        proc.steps.iter().enumerate().filter_map(|(idx, step)| {
            if step.intent.is_none() { return None; }
            match resolve_step(step, reqs, &inventory) {
                Resolution::Unresolved { reason } => Some(Finding {
                    check: self.id(),
                    severity: Severity::Warning,
                    skill: skill.manifest.name.clone(),
                    message: format!("step[{idx}] intent '{}' unresolvable: {reason}",
                        step.intent.as_deref().unwrap_or("")),
                    fixable: false,
                }),
                _ => None,
            }
        }).collect()
    }
}
```

- [ ] **Step 2: Register + test**

Mirror M6a Task 3/4 pattern. Three fixtures:
- v2.2 skill with intent matched by inventory → 0 findings.
- v2.2 skill with intent + requirement but no inventory match → 1 Warning per unresolved step.
- v2.2 skill with intent + fallback that IS in inventory → 0 findings.

- [ ] **Step 3: Build + commit**

```
cargo test -p mur-core --test skill_doctor_mcp -- intent_resolvable
git add mur-core/src/skill_doctor/checks/intent_resolvable.rs mur-core/src/skill_doctor/mod.rs mur-core/tests/skill_doctor_mcp.rs
git commit -m "feat(skill-doctor): intent-resolvable check"
```

---

### Task 6 — `SkillStats.resolution_misses` counter

**Files:** `mur-common/src/skill/stats.rs` (modify), `mur-core/src/skill_stats/aggregator.rs` (modify).

- [ ] **Step 1: Add field (additive, follows M5b Task 0 policy)**

```rust
// mur-common/src/skill/stats.rs (modify, near other counters)

pub struct SkillStats {
    // ... existing fields
    /// Count of inject-time `Resolution::Unresolved` outcomes for this skill.
    /// Spike here means the skill declares intents that no longer match the
    /// agent's MCP inventory — doctor's `intent-resolvable` check surfaces this.
    #[serde(default)]
    pub resolution_misses: u64,
}
```

- [ ] **Step 2: Wire the delta**

`StatsDelta::ResolutionMiss(skill_name)` already implied at Task 4 Step 3. Add the enum variant and the merge arm in the aggregator. Counter is commutative — increments by 1, no special locking beyond the existing `merge_in_place` window.

- [ ] **Step 3: Test**

End-to-end: emit 3 unresolved injections for skill X, flush, assert `stats.resolution_misses == 3`.

- [ ] **Step 4: Build + commit**

```
cargo build -p mur-common -p mur-core
git add mur-common/src/skill/stats.rs mur-core/src/skill_stats/aggregator.rs
git commit -m "feat(skill-stats): resolution_misses counter"
```

---

### Task 7 — `mur skill show` rendering + docs

**Files:** `mur-core/src/cmd/skill_show.rs` (modify), `docs/architecture/runtime-overview.md` (modify).

- [ ] **Step 1: Render intent + hint in step listing**

```
Procedure:
  1. Navigate to search page
       intent: web_navigate
       tool_hint: browser.navigate
       tool (literal): —
  2. Click first result
       intent: web_click
       (no hint)
```

- [ ] **Step 2: Docs**

Add to the `Skill ↔ MCP` section (created in M6a Task 6):
1. The new `intent` + `tool_hint` fields and the resolution precedence (literal-only → hint → intent_match → fallback → unresolved).
2. What `mur skill doctor` reports for unresolved intents.
3. Backwards-compat note: skills written before M6b keep working unchanged because they have no `intent`.

- [ ] **Step 3: Commit**

```
git add mur-core/src/cmd/skill_show.rs docs/architecture/runtime-overview.md
git commit -m "feat(skill): show intent/tool_hint + docs"
```

---

## Operator Walkthrough

After M6b ships, a skill author can write:

```yaml
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http

content:
  procedure:
    steps:
      - description: Open the search page
        intent: web_navigate
        tool_hint: browser.navigate
      - description: Type the query
        intent: text_input
```

At inject time, if the agent has `browser.navigate` configured, both steps resolve and the prompt sees:
```
1. Open the search page — tool: browser.navigate
2. Type the query — (no tool available: intent 'text_input' has no matching tool…)
```

The agent now sees the right tool name without the skill author hard-coding it, and the unresolved step is honest about its gap.

---

## Out of scope — deferred to M6c / M7

1. **Active dispatch of MCP calls by mur** — current model is prompt-injection; M6b respects that. M7 may explore mur-driven dispatch.
2. **LLM inference of `intent` from `description`** — would let legacy skills benefit. M6c (uses the LLM helper from §4.5 of M6 scoping).
3. **Per-step capability override** — a step's `intent` currently inherits the capability from whichever `mcp_requirements` entry matched. Allowing per-step capability override is plausible but adds schema surface; skip for now.
4. **Tiebreaker beyond shortest-name** — the resolver picks shortest name. If field data shows this is wrong, M7 can add per-skill rank hints. The current rule is deterministic and good enough.
5. **Cross-agent intent vocabulary** — agents will accumulate diverging intent strings. M7 introduces an optional canonicaliser.

## Risks

| Risk | Mitigation |
|---|---|
| `layer3_body` signature change ripples through call sites | The function is called in two places (injector + a test). Update both at Task 4 Step 1. |
| Inventory build is async; doctor checks are sync today | M6a Task 4 already made doctor async-capable. M6b doctor check piggybacks. |
| Inject latency grows if inventory build is slow | Cache the inventory at the agent-runtime injector layer; rebuild on MCP server config change events. Out of scope for v1; address only if observed. |
| Telemetry flood from `SkillStepResolved` on hot inject paths | Literal source tag is skipped (Task 4 Step 3); non-literal events are rare in practice. |
| `resolution_misses` increments race the file lock | Same mechanism as M5a `usage_count` increments; the aggregator handles batching + locked merge. |
| Skill authors confused by precedence between `tool`, `tool_hint`, `intent` | Documented inline in `manifest.rs` doc comment AND in `mur skill show` rendering (Task 7 Step 1 explicitly labels which fields contributed). |
| v2.1 → v2.2 schema bump invalidates publisher signatures | No — same mechanism as M6a, `#[serde(skip_serializing_if = "Option::is_none")]` keeps absent fields out of canonicalization. Re-run M6a's byte-identity test (Task 1 Step 5 above) against the v2.1 fixture. |
