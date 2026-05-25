# M6a — Skill ↔ MCP Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the data-model and validator foundations for skill ↔ MCP integration. Skills can declare which MCP capabilities they need; `mur skill validate` and `mur skill doctor` understand the declaration. **Execution semantics — dynamically resolving a skill step to an actual MCP tool call — is M6b's job and is explicitly out of scope here.**

**Spec mapping:** §11.1 Skill → MCP Binding (the schema half only; §11.2/§11.3 are M6b). §M6 first bullet (`mcp_requirements` with tool patterns → trust capabilities).

**Hard dependency on M5a + M5b:**
- M5a: `mur skill doctor` Check trait + finding-output infrastructure.
- M5b: doctor `--fix --apply` repair engine (M6a adds a new check; non-fixable in M6a, so no Repair impl needed).
- M5b Task 0's schema-evolution doc comment is in place — M6a does NOT touch `SkillStats` but the same additive-only philosophy applies to `SkillManifest` here.

**What M6a ships:**
1. `mur-common::skill::mcp` — `SkillCapability` newtype mirroring commander's `McpCapability` via `From` impls, plus `McpRequirement` struct that becomes a new optional field on `SkillManifest`.
2. `SkillManifest.mcp_requirements: Vec<McpRequirement>` (optional, `#[serde(default, skip_serializing_if = Vec::is_empty)]`) — backwards-compatible additive change.
3. Manifest schema version bump (minor): `2.0` → `2.1`. M3-era skills (v2.0) keep working unchanged.
4. `mur skill validate` extension: type-check `mcp_requirements` entries, reject unknown capabilities at parse time (allow-list pattern from commander).
5. Two new doctor checks:
   - `mcp-requirements-coverage` (Severity::Info) — flags procedural skills that reference tool patterns in steps but omit `mcp_requirements`.
   - `mcp-capability-available` (Severity::Warning) — flags declared requirements with no matching MCP server in the agent's configured server list.
6. CLI surface: `mur skill show <name>` displays the `mcp_requirements` block (cosmetic; reuses existing pretty-printer).

**What M6a does NOT ship:**
- Dynamic tool resolution at execution time (`intent` + `tool_hint` step keys, runtime resolver) → **M6b**.
- MCP as the skill execution substrate (skill step → MCP tool invocation) → **M6b**.
- Auto-repair for `mcp-capability-available` (suggesting an MCP server to install) → future.
- DSSE signing of `mcp_requirements` — they ARE signed (part of `SkillManifest`); just no new signing logic, the existing path covers it automatically because `mcp_requirements` is inside `SkillManifest`.
- Cross-agent MCP capability propagation → M7.

**Tech Stack:** Rust 2024. No new dependencies. Re-use commander's `McpCapability` enum via path import (`mur-commander/crates/engine/src/mcp/trust.rs`), wrapped in a `mur-common::skill::mcp::SkillCapability` newtype.

---

## File Structure

**Create:**
- `mur-common/src/skill/mcp.rs` — `SkillCapability` newtype, `McpRequirement` struct, parse + display impls, `From<commander::McpCapability>` and reverse.
- `mur-core/src/skill_doctor/checks/mcp_requirements_coverage.rs` — Info-level check.
- `mur-core/src/skill_doctor/checks/mcp_capability_available.rs` — Warning-level check.
- `mur-common/tests/skill_mcp_parse.rs` — fixture: valid + invalid + missing capability strings round-trip.
- `mur-core/tests/skill_doctor_mcp.rs` — fixture-driven doctor check (procedural skill missing requirements; valid skill with requirements but no matching server; valid skill with matching server).
- `mur-core/tests/skill_manifest_compat.rs` — load an M3-era v2.0 manifest (no `mcp_requirements` block), assert default empty vec, round-trip through serialize → deserialize stays stable.

**Modify:**
- `mur-common/src/skill/mod.rs` — `pub mod mcp;` and re-exports of `McpRequirement`, `SkillCapability`.
- `mur-common/src/skill/manifest.rs` — add `mcp_requirements: Vec<McpRequirement>` field with `#[serde(default, skip_serializing_if = "Vec::is_empty")]` AND bump `SKILL_MANIFEST_SCHEMA_VERSION` constant (or whatever the existing version anchor is — locate at Task 1 Step 1).
- `mur-common/src/skill/validate.rs` — call into `mcp::validate_requirements` from the schema validation entry point.
- `mur-core/src/skill_doctor/mod.rs` — register the two new checks in the check registry.
- `mur-core/src/cmd/skill_show.rs` (or wherever `mur skill show` lives) — render the `mcp_requirements` block.
- Workspace-level: depend on `mur-commander` is **already not** the relationship — `mur-common` does not depend on commander. Instead, redefine the six capabilities in `mur-common::skill::mcp` and provide `From` impls only behind a `#[cfg(feature = "commander-interop")]` flag if/when needed. The shared vocabulary is the **string form** (`"read_file"`, `"network_http"`, etc.), which is what gets serialized to YAML and what commander parses anyway.

**Do not modify:**
- `Skill` struct's security metadata (`trust_level`, `capabilities_declared`, `publisher_signature`) — those continue to be set by the trust store at install time. `mcp_requirements` is publisher-authored and inside `SkillManifest`, so the signature path covers it without changes.
- DSSE signing code — additive optional field is forward-compatible with the existing canonicalization (verify this in Task 1 Step 3).
- `SkillStats` — execution-time auditing of MCP resolution is M6b's concern.
- M5a/M5b doctor checks — only the registry is extended.

---

### Task 1 — `SkillCapability` newtype + `McpRequirement` schema

**Files:** `mur-common/src/skill/mcp.rs` (new), `mur-common/src/skill/manifest.rs` (modify), `mur-common/src/skill/mod.rs` (modify).

- [ ] **Step 1: Define the capability vocabulary**

```rust
// mur-common/src/skill/mcp.rs

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// MCP capabilities a skill may declare it requires. Mirrors the six
/// capabilities defined in mur-commander's `engine/src/mcp/trust.rs`.
///
/// **Shared vocabulary, not shared types.** The string form is the contract
/// between skill manifests and commander's MCP trust store. We keep our own
/// enum here so `mur-common` does not depend on commander, and so the YAML
/// schema is stable even if commander internally refactors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillCapability {
    ReadFile,
    ListTools,
    Search,
    WriteFile,
    ExecuteSafe,
    NetworkHttp,
}

impl SkillCapability {
    pub const ALL: &'static [SkillCapability] = &[
        SkillCapability::ReadFile,
        SkillCapability::ListTools,
        SkillCapability::Search,
        SkillCapability::WriteFile,
        SkillCapability::ExecuteSafe,
        SkillCapability::NetworkHttp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SkillCapability::ReadFile    => "read_file",
            SkillCapability::ListTools   => "list_tools",
            SkillCapability::Search      => "search",
            SkillCapability::WriteFile   => "write_file",
            SkillCapability::ExecuteSafe => "execute_safe",
            SkillCapability::NetworkHttp => "network_http",
        }
    }
}

impl std::fmt::Display for SkillCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillCapability {
    type Err = ParseCapabilityError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "read_file"    => SkillCapability::ReadFile,
            "list_tools"   => SkillCapability::ListTools,
            "search"       => SkillCapability::Search,
            "write_file"   => SkillCapability::WriteFile,
            "execute_safe" => SkillCapability::ExecuteSafe,
            "network_http" => SkillCapability::NetworkHttp,
            other => return Err(ParseCapabilityError(other.to_string())),
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown MCP capability '{0}' (expected one of: read_file, list_tools, search, write_file, execute_safe, network_http)")]
pub struct ParseCapabilityError(pub String);

// Serde uses the string form so YAML reads as `capability: read_file`, not `capability: ReadFile`.
impl Serialize for SkillCapability {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.as_str()) }
}
impl<'de> Deserialize<'de> for SkillCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
```

The reason for hand-rolling Serialize/Deserialize (instead of `#[derive] + serde(rename_all = "snake_case")`): we want the `FromStr` error message — a `serde` rename error is opaque; our `ParseCapabilityError` lists the valid options.

- [ ] **Step 2: `McpRequirement` struct**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRequirement {
    /// Glob pattern matching tool names, e.g. `"browser.*"` or `"filesystem.write.*"`.
    /// Match semantics use `globset` (same as M5a `mur skill doctor` filter).
    pub tool_pattern: String,

    /// Capability the matching tool will be invoked with. Used by commander's
    /// trust store at runtime (M6b) to decide whether to permit the call.
    pub capability: SkillCapability,

    /// Optional fallback. Empty string means "no fallback — fail if no match".
    /// Free-form tool name; not validated at the manifest layer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fallback: String,
}

/// Validate a list of requirements at parse time. Returns the index of the
/// first invalid entry plus a diagnostic — caller turns this into a validate-
/// or-doctor finding.
pub fn validate_requirements(reqs: &[McpRequirement]) -> Result<(), (usize, String)> {
    let mut seen_patterns = std::collections::HashSet::new();
    for (i, req) in reqs.iter().enumerate() {
        if req.tool_pattern.is_empty() {
            return Err((i, "tool_pattern must not be empty".into()));
        }
        if globset::Glob::new(&req.tool_pattern).is_err() {
            return Err((i, format!("invalid glob pattern: '{}'", req.tool_pattern)));
        }
        if !seen_patterns.insert((req.tool_pattern.clone(), req.capability)) {
            return Err((i, format!("duplicate (tool_pattern, capability) pair: '{}'/{}", req.tool_pattern, req.capability)));
        }
    }
    Ok(())
}
```

`globset` is already a `mur-core` dep from M5a — verify the import works (it might require a workspace move to `mur-common` since `mur-common` shouldn't depend on `mur-core`). If a move is needed, do it as part of this step.

- [ ] **Step 3: Wire into `SkillManifest`**

```rust
// mur-common/src/skill/manifest.rs

use super::mcp::McpRequirement;   // NEW

pub struct SkillManifest {
    // ... existing fields unchanged
    pub transfer_chain: Vec<String>,

    /// MCP tool capabilities this skill needs at runtime. Optional; absent
    /// in M3-era v2.0 manifests. Added in schema v2.1.
    ///
    /// **Signature scope:** signed as part of the manifest. Changing
    /// `mcp_requirements` invalidates an existing publisher signature.
    /// See `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m6-scoping.md` §4.3.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_requirements: Vec<McpRequirement>,
}
```

- [ ] **Step 4: Schema version bump**

Locate the existing schema-version anchor (probably `SKILL_MANIFEST_SCHEMA_VERSION` in `mur-common/src/skill/`). Bump from `"2.0"` to `"2.1"`. The validator continues to accept both — `2.0` skills with no `mcp_requirements` default to empty, behaviour unchanged.

If no schema-version anchor exists yet, this becomes the place to introduce one — `mur-common/src/skill/version.rs`:
```rust
pub const SKILL_MANIFEST_SCHEMA_VERSION: &str = "2.1";
pub fn is_supported(version: &str) -> bool { matches!(version, "2.0" | "2.1") }
```

- [ ] **Step 5: Round-trip + signature compatibility test**

```rust
// mur-common/tests/skill_manifest_compat.rs

#[test]
fn v20_manifest_loads_with_default_empty_requirements() {
    let yaml = include_str!("fixtures/skill_v20_no_mcp.yaml");
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(skill.manifest.mcp_requirements.len(), 0);
    // Reserialize — the absent field should stay absent (skip_serializing_if).
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    assert!(!out.contains("mcp_requirements"));
}

#[test]
fn v21_manifest_with_requirements_round_trips() { /* ... */ }

#[test]
fn unknown_capability_string_rejected() {
    let yaml = r#"
      name: bad
      version: "1.0"
      mcp_requirements:
        - tool_pattern: "x.*"
          capability: telepathy
    "#;
    let err = serde_yaml_ng::from_str::<SkillManifest>(yaml).unwrap_err();
    assert!(err.to_string().contains("unknown MCP capability"));
}
```

Critical: **the signature path is unchanged**. A v2.0 manifest signed under v2.0 → loaded as v2.1 with empty `mcp_requirements` → reserialized → re-signature against the new bytes is identical because `skip_serializing_if = "Vec::is_empty"` keeps the field out of the wire format. Add this as a separate test asserting byte-identical reserialization for a v2.0 fixture.

- [ ] **Step 6: Build + commit**

```
cargo build -p mur-common
cargo test -p mur-common --test skill_manifest_compat --test skill_mcp_parse
git add mur-common/src/skill/{mcp.rs,manifest.rs,mod.rs} mur-common/tests/
git commit -m "feat(skill): SkillCapability + McpRequirement schema (v2.1)"
```

---

### Task 2 — Validator integration

**Files:** `mur-common/src/skill/validate.rs` (modify).

- [ ] **Step 1: Call `validate_requirements` from the schema validator**

Locate the existing `validate_skill` (or equivalent) function. After existing checks, add:
```rust
if let Err((idx, msg)) = mcp::validate_requirements(&skill.manifest.mcp_requirements) {
    errors.push(ValidationError {
        path: format!("mcp_requirements[{idx}]"),
        message: msg,
    });
}
```

- [ ] **Step 2: Validate version compatibility**

If the manifest's schema version is `2.0` AND `mcp_requirements` is non-empty, that is a contradiction (v2.0 didn't define the field; producer should bump to `2.1`). Emit a validation error.

- [ ] **Step 3: Tests**

Reuse the existing validate-test pattern. Add three cases:
- valid v2.1 with one requirement → passes
- valid v2.1 with empty requirements → passes
- invalid v2.1 with duplicate (pattern, capability) → fails at that index

- [ ] **Step 4: Build + commit**

```
cargo build -p mur-common
git add mur-common/src/skill/validate.rs mur-common/tests/
git commit -m "feat(skill): validate mcp_requirements at parse time"
```

---

### Task 3 — Doctor check: `mcp-requirements-coverage` (Info)

**Files:** `mur-core/src/skill_doctor/checks/mcp_requirements_coverage.rs` (new), `mur-core/src/skill_doctor/mod.rs` (modify).

**Heuristic:** if the skill is procedural (mode = workflow) AND any step's `tool` field looks like a dotted tool name (e.g. `browser.navigate`, contains a `.`) AND `mcp_requirements` is empty, emit one Info-level finding.

- [ ] **Step 1: Implement the check**

```rust
// mur-core/src/skill_doctor/checks/mcp_requirements_coverage.rs

use super::{Check, CheckId, Finding, Severity};
use mur_common::skill::manifest::Skill;
use mur_common::skill::types::ContentMode;

pub struct McpRequirementsCoverage;

impl Check for McpRequirementsCoverage {
    fn id(&self) -> CheckId { CheckId("mcp-requirements-coverage") }

    fn run(&self, skill: &Skill, _ctx: &super::Ctx) -> Vec<Finding> {
        if skill.manifest.content.mode() != Some(ContentMode::Workflow) {
            return vec![];
        }
        if !skill.manifest.mcp_requirements.is_empty() {
            return vec![];
        }
        let Some(proc) = &skill.manifest.content.procedure else { return vec![]; };

        let referenced: Vec<&str> = proc.steps.iter()
            .filter_map(|s| s.tool.as_deref())
            .filter(|t| t.contains('.'))
            .collect();

        if referenced.is_empty() { return vec![]; }

        vec![Finding {
            check: self.id(),
            severity: Severity::Info,
            skill: skill.manifest.name.clone(),
            message: format!(
                "procedural skill references {} dotted tool name(s) ({}…) but declares no mcp_requirements",
                referenced.len(),
                referenced.iter().take(2).copied().collect::<Vec<_>>().join(", ")
            ),
            fixable: false,
        }]
    }
}
```

Not fixable in M6a — auto-fix would require inferring the right `capability` for each tool pattern (e.g., is `filesystem.write.*` `WriteFile` or `ExecuteSafe`?), which is a guess. Future M6c can layer an LLM-driven `--fix --apply` here.

- [ ] **Step 2: Register**

```rust
// mur-core/src/skill_doctor/mod.rs
checks.push(Box::new(checks::mcp_requirements_coverage::McpRequirementsCoverage));
```

- [ ] **Step 3: Test**

Three fixtures: (a) workflow skill with `browser.navigate` in a step and empty requirements → 1 finding; (b) same skill with requirements → 0 findings; (c) context-mode skill → 0 findings even with dotted text.

- [ ] **Step 4: Build + commit**

```
cargo test -p mur-core --test skill_doctor_mcp -- coverage
git add mur-core/src/skill_doctor/checks/mcp_requirements_coverage.rs mur-core/src/skill_doctor/mod.rs mur-core/tests/skill_doctor_mcp.rs
git commit -m "feat(skill-doctor): mcp-requirements-coverage check"
```

---

### Task 4 — Doctor check: `mcp-capability-available` (Warning)

**Files:** `mur-core/src/skill_doctor/checks/mcp_capability_available.rs` (new), `mur-core/src/skill_doctor/mod.rs` (modify).

**Heuristic:** for each `McpRequirement`, look up the agent's MCP server registry (existing surface: `mur agent mcp list`). If no server provides a tool matching `req.tool_pattern`, emit a Warning-level finding.

- [ ] **Step 1: MCP server inventory lookup**

The agent runtime already knows what MCP servers are configured (the binding store). Locate the read-side API — likely `mur-agent-runtime::mcp::registry::list_tools()` or similar. If not exposed as a library function, add a thin `mur-core::mcp_registry::list_available_tool_names()` wrapper that reads the same on-disk config. Decide at Task 4 Step 1 — adding a wrapper is fine if the runtime API is async-only.

- [ ] **Step 2: Implement the check**

```rust
// mur-core/src/skill_doctor/checks/mcp_capability_available.rs

use globset::Glob;

pub struct McpCapabilityAvailable;

impl Check for McpCapabilityAvailable {
    fn id(&self) -> CheckId { CheckId("mcp-capability-available") }

    fn run(&self, skill: &Skill, ctx: &super::Ctx) -> Vec<Finding> {
        if skill.manifest.mcp_requirements.is_empty() { return vec![]; }

        let available: Vec<String> = ctx.mcp_tools().unwrap_or_default();
        // ctx.mcp_tools() returns the union of tool names across all MCP servers
        // bound to the agent (or globally configured at the user level).

        let mut findings = vec![];
        for (i, req) in skill.manifest.mcp_requirements.iter().enumerate() {
            let Ok(glob) = Glob::new(&req.tool_pattern) else { continue; };
            let matcher = glob.compile_matcher();
            let has_match = available.iter().any(|t| matcher.is_match(t));
            if !has_match && req.fallback.is_empty() {
                findings.push(Finding {
                    check: self.id(),
                    severity: Severity::Warning,
                    skill: skill.manifest.name.clone(),
                    message: format!(
                        "mcp_requirements[{i}]: no MCP server provides tool matching '{}' (capability {}, no fallback)",
                        req.tool_pattern, req.capability
                    ),
                    fixable: false,
                });
            }
        }
        findings
    }
}
```

A requirement with a `fallback` is downgraded to Info or skipped — exact decision: **skip**. The fallback exists precisely for this case; warning here would be noise.

- [ ] **Step 3: Register + test**

Same pattern as Task 3. Fixtures:
- Skill with `browser.*` requirement, no browser MCP server configured → 1 Warning.
- Skill with `browser.*` requirement, `mock-browser.navigate` available → 0 findings (glob matches).
- Skill with `browser.*` requirement + `fallback: builtin-http`, no browser server → 0 findings.

- [ ] **Step 4: Build + commit**

```
cargo test -p mur-core --test skill_doctor_mcp -- capability
git add mur-core/src/skill_doctor/checks/mcp_capability_available.rs mur-core/src/skill_doctor/mod.rs mur-core/tests/skill_doctor_mcp.rs
git commit -m "feat(skill-doctor): mcp-capability-available check"
```

---

### Task 5 — `mur skill show` cosmetic rendering

**Files:** `mur-core/src/cmd/skill_show.rs` (modify).

- [ ] **Step 1: Render the `mcp_requirements` block when non-empty**

Place after the existing `tags` / `triggers` section:
```
MCP Requirements:
  - browser.*          (capability: network_http, fallback: builtin-http)
  - filesystem.write.* (capability: write_file)
```

Skip the section entirely when `mcp_requirements.is_empty()`.

- [ ] **Step 2: Build + commit**

```
cargo build -p mur-core
git add mur-core/src/cmd/skill_show.rs
git commit -m "feat(skill): show mcp_requirements in mur skill show"
```

---

### Task 6 — Documentation

**Files:** `docs/architecture/runtime-overview.md` (modify), `README.md` (skim — only update if MCP integration is mentioned).

- [ ] **Step 1: Add a `Skill ↔ MCP` subsection under the skills section**

Cover:
1. The six capabilities (with one-line descriptions).
2. The YAML shape:
```yaml
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http
  - tool_pattern: "filesystem.write.*"
    capability: write_file
```
3. What doctor reports (`mcp-requirements-coverage` Info, `mcp-capability-available` Warning).
4. A note that M6a only ships **declaration + validation**; runtime resolution lands in M6b.

- [ ] **Step 2: Commit**

```
git add docs/architecture/runtime-overview.md
git commit -m "docs(skill): mcp_requirements declaration and doctor checks"
```

---

## Out of scope — deferred to M6b / M6c / M7

1. **`intent` + `tool_hint` step keys + runtime resolver** — M6b. Adding these in M6a would leave them un-consumed, which violates the "no schema fields without producers" rule from M6 scoping doc §4.1.
2. **`mcp-capability-available` auto-fix** (suggest install of an MCP server) — needs a registry of "which server provides which tool", which is a separate dataset. Future.
3. **LLM-driven inference of `capability` from `tool_pattern`** — M6c (overlaps with M6c's LLM substrate work).
4. **Cross-agent capability propagation** — M7.
5. **Capability mapping table sync with commander** — string vocabulary is the contract; if commander adds a 7th capability, M6a is a pure additive bump here (add the variant, add `as_str`/`from_str` arms, bump schema version to 2.2). No coupling tightening needed.

## Risks

| Risk | Mitigation |
|---|---|
| Signature breakage for re-serialized v2.0 manifests | Round-trip test in Task 1 Step 5 asserts byte-identical re-serialization with `skip_serializing_if`. |
| Commander adds a 7th capability before this lands | Hand-rolled list — adding a variant is a 4-line patch. Document the add-variant procedure in `mcp.rs`. |
| `globset` move from `mur-core` to `mur-common` triggers other ripple changes | Locate at Task 1 Step 2 first. If ripples are large, keep `globset` in `mur-core` and inline a minimal glob check in `mur-common` (`*` as wildcard suffix only, since `tool_pattern` rarely needs full glob). |
| Procedural-step `tool` field doesn't have an unambiguous "dotted name" semantic | Heuristic: contains `.` AND is not a relative path (no leading `./`). Document the heuristic in Task 3 Step 1 code; emit Info, not Warning, precisely because the heuristic is imperfect. |
| `mcp_tools()` read-side requires async access to MCP registry | Add the wrapper in Task 4 Step 1; doctor is already async (M5b doctor `--fix --apply` is async). |
| Users on M3-era registries see no behavioural change | That's correct and expected; the v2.1 schema is backwards-compatible. Document in M6a release notes. |
