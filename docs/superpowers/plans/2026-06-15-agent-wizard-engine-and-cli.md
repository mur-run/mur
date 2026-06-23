# Agent Wizard — Engine + Deterministic CLI (Plan 1 of 5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared `mur-core::agent_wizard` engine and wire a deterministic `mur agent wizard` CLI that takes a role (preset or custom), produces skill/prompt/entitlement drafts, pauses at a human review-and-approve gate, then creates + starts the agent — all without any LLM.

**Architecture:** A new `mur-core/src/agent_wizard/` module holds a stage state-machine that returns an in-memory `WizardDraft`, emitting progress through a callback so any driver (CLI now, Hub later) can render it. Stages 1/5/6/7 (define-role, entitlement-preset, review-gate, create+start) are deterministic; LLM stages 2–4 and eval (stage 8) are added in Plans 2–3 behind the same interface. The CLI command is a thin driver over the engine, reusing existing `agent_admin::{prompt,perm,skill}` helpers and the role catalog loaded from YAML manifests (no hardcoded role list).

**Tech Stack:** Rust (edition 2024), clap (subcommands), serde/serde_yaml_ng, `cargo nextest run`, existing `mur-common::AgentProfile` + `agent_admin` helpers.

**This is Plan 1 of a 5-plan decomposition** (see "Decomposition roadmap" at the end). It ships working software on its own: a deterministic, review-gated `mur agent wizard`.

---

## File Structure

New module `mur-core/src/agent_wizard/`:
- `mod.rs` — public API: `run_wizard(input, progress, gate) -> Result<WizardOutcome>`; re-exports.
- `draft.rs` — data types: `RoleSpec`, `RiskLevel`, `SkillDraft`, `PromptDraft`, `EntitlementPlan`, `WizardDraft`, `WizardOutcome`, `Stage`, `Progress`.
- `catalog.rs` — `RoleManifest` + `load_catalog()` merging shipped defaults + `~/.mur/wizard/roles/*.yaml`.
- `entitlements.rs` — `preset_for(role: &RoleSpec) -> EntitlementPlan` (risk → least-privilege plan).
- `stages.rs` — the stage runner: deterministic stages + progress emission; LLM/eval stages are trait hooks (no-op in Plan 1).
- `apply.rs` — `apply_draft(&WizardDraft) -> Result<String>`: writes profile/prompt/skills, sets entitlements, starts service — only after gate approval.

Modified:
- `mur-core/src/cli/agent.rs` — add a `Wizard { .. }` variant to the agent `Subcommand` enum (after `Create`, near line 10).
- `mur-core/src/cmd/agent/mod.rs` — dispatch `Wizard` to a new `cmd/agent/wizard.rs` handler.
- `mur-core/src/cmd/agent/wizard.rs` (Create) — CLI driver: prompts, terminal progress printer, terminal review gate.
- `mur-core/src/lib.rs` or the agent module root — `pub mod agent_wizard;`.

Shipped role manifests: `mur-core/resources/wizard-roles/{pm,qa,repomanager,rustsmith,custom}.yaml`.

---

## Task 1: Draft + RoleSpec data types

**Files:**
- Create: `mur-core/src/agent_wizard/mod.rs`
- Create: `mur-core/src/agent_wizard/draft.rs`
- Modify: `mur-core/src/lib.rs` (add `pub mod agent_wizard;`)
- Test: inline `#[cfg(test)]` in `draft.rs`

- [ ] **Step 1: Register the module**

In `mur-core/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod agent_wizard;
```

- [ ] **Step 2: Write the failing test for RiskLevel + RoleSpec defaults**

Create `mur-core/src/agent_wizard/draft.rs`:

```rust
//! In-memory draft types produced by the wizard before anything is written to disk.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Read-only / advisory agents (e.g. PM). No irreversible actions.
    #[default]
    Low,
    /// Writes code/tests, runs build tools (e.g. QA).
    Medium,
    /// Performs irreversible repo/release ops (e.g. Repo Manager). Triggers security eval suites.
    High,
}

/// What the user wants built. Either chosen from a catalog preset or fully custom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub name: String,
    pub display_name: String,
    pub charter: String,
    pub risk: RiskLevel,
    /// Catalog preset id this came from, or `None` for a fully custom role.
    pub preset_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_level_defaults_to_low() {
        assert_eq!(RiskLevel::default(), RiskLevel::Low);
    }

    #[test]
    fn role_spec_roundtrips_through_yaml() {
        let r = RoleSpec {
            name: "pm".into(),
            display_name: "PM".into(),
            charter: "Turns intent into buildable, testable work.".into(),
            risk: RiskLevel::Low,
            preset_id: Some("pm".into()),
        };
        let y = serde_yaml_ng::to_string(&r).unwrap();
        let back: RoleSpec = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(r, back);
    }
}
```

- [ ] **Step 3: Run the test, expect failure (module not wired / types missing)**

Run: `cargo nextest run -p mur-core agent_wizard::draft 2>&1 | tail -20`
Expected: FAIL — compile error until `mod.rs` declares the submodule.

- [ ] **Step 4: Add the remaining draft types + wire mod.rs**

Append to `draft.rs`:

```rust
/// One generated skill, not yet installed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillDraft {
    pub name: String,
    /// Full `skill.yaml` content (validated before it can reach the review gate).
    pub yaml: String,
}

/// The DoD system prompt draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptDraft {
    pub markdown: String,
}

/// A least-privilege entitlement plan, expressed as the `agent_admin::perm` calls to make.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntitlementPlan {
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub allow_spawn: Vec<String>,
    pub allow_host: Vec<String>,
    pub deny_path: Vec<String>,
    pub tool_allow: Vec<String>,
}

/// Everything the human reviews at the gate, before any disk write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WizardDraft {
    pub role: RoleSpec,
    pub skills: Vec<SkillDraft>,
    pub prompt: PromptDraft,
    pub entitlements: EntitlementPlan,
    pub model_ref: String,
}

/// Progress event emitted to the driver as stages run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    DefineRole,
    Research,
    AuthorSkills,
    DraftPrompt,
    Entitlements,
    ReviewGate,
    Create,
    Eval,
}

/// Result of a completed wizard run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WizardOutcome {
    pub agent_name: String,
    pub created: bool,
}
```

Create `mur-core/src/agent_wizard/mod.rs`:

```rust
//! Agent Wizard engine: builds a specialized agent from a role, with a human
//! review gate before anything is written. Drivers (CLI, Hub) share this engine.
pub mod apply;
pub mod catalog;
pub mod draft;
pub mod entitlements;
pub mod stages;

pub use draft::{
    EntitlementPlan, Progress, PromptDraft, RiskLevel, RoleSpec, SkillDraft, Stage, WizardDraft,
    WizardOutcome,
};
```

Create empty stubs so the module compiles: `catalog.rs`, `entitlements.rs`, `stages.rs`, `apply.rs` each containing only a `//!` doc comment for now.

- [ ] **Step 5: Run the tests, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::draft 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_wizard/ mur-core/src/lib.rs
git commit -m "feat(agent-wizard): draft data types (RoleSpec, WizardDraft, stages)"
```

---

## Task 2: Role catalog loader (extensible, no hardcoded list)

**Files:**
- Modify: `mur-core/src/agent_wizard/catalog.rs`
- Create: `mur-core/resources/wizard-roles/pm.yaml` (+ qa, repomanager, rustsmith, custom)
- Test: inline `#[cfg(test)]` in `catalog.rs`

- [ ] **Step 1: Write the failing test**

In `catalog.rs`:

```rust
//! Role catalog: shipped defaults + user manifests under ~/.mur/wizard/roles/.
use crate::agent_wizard::draft::RiskLevel;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleManifest {
    pub id: String,
    pub display_name: String,
    pub charter: String,
    #[serde(default)]
    pub risk: RiskLevel,
    /// Skill topic titles the research/author stages will produce (used as stubs in --no-llm).
    #[serde(default)]
    pub skill_topics: Vec<String>,
    #[serde(default)]
    pub category: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_manifest_from_yaml_str() {
        let y = r#"
id: pm
display_name: PM
charter: Turns intent into buildable work.
risk: low
category: product
skill_topics: ["product-spec-and-prd-writing", "issue-authoring-and-hygiene"]
"#;
        let m: RoleManifest = serde_yaml_ng::from_str(y).unwrap();
        assert_eq!(m.id, "pm");
        assert_eq!(m.skill_topics.len(), 2);
        assert_eq!(m.risk, RiskLevel::Low);
    }

    #[test]
    fn user_dir_overrides_shipped_by_id() {
        let shipped = vec![RoleManifest {
            id: "pm".into(), display_name: "PM".into(), charter: "a".into(),
            risk: RiskLevel::Low, skill_topics: vec![], category: "product".into(),
        }];
        let user = vec![RoleManifest {
            id: "pm".into(), display_name: "PM (mine)".into(), charter: "b".into(),
            risk: RiskLevel::Low, skill_topics: vec![], category: "product".into(),
        }];
        let merged = merge_catalog(shipped, user);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "PM (mine)");
    }
}
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo nextest run -p mur-core agent_wizard::catalog 2>&1 | tail -20`
Expected: FAIL — `merge_catalog` not defined.

- [ ] **Step 3: Implement loader + merge**

Append to `catalog.rs`:

```rust
/// Merge shipped defaults with user manifests; user id wins (override, no duplicates).
pub fn merge_catalog(shipped: Vec<RoleManifest>, user: Vec<RoleManifest>) -> Vec<RoleManifest> {
    let mut out: Vec<RoleManifest> = shipped;
    for u in user {
        if let Some(slot) = out.iter_mut().find(|m| m.id == u.id) {
            *slot = u;
        } else {
            out.push(u);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn read_dir_manifests(dir: &Path) -> Vec<RoleManifest> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "yaml" || x == "yml"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_yaml_ng::from_str::<RoleManifest>(&s).ok())
        .collect()
}

/// Load shipped manifests (embedded) + user manifests from `~/.mur/wizard/roles/`.
pub fn load_catalog(mur_home: &Path) -> Vec<RoleManifest> {
    let shipped = shipped_manifests();
    let user = read_dir_manifests(&mur_home.join("wizard").join("roles"));
    merge_catalog(shipped, user)
}

/// Shipped defaults are embedded at compile time so the binary is self-contained.
fn shipped_manifests() -> Vec<RoleManifest> {
    const FILES: &[&str] = &[
        include_str!("../../resources/wizard-roles/pm.yaml"),
        include_str!("../../resources/wizard-roles/qa.yaml"),
        include_str!("../../resources/wizard-roles/repomanager.yaml"),
        include_str!("../../resources/wizard-roles/rustsmith.yaml"),
    ];
    FILES.iter().filter_map(|s| serde_yaml_ng::from_str(s).ok()).collect()
}
```

- [ ] **Step 4: Create the shipped manifests**

Create `mur-core/resources/wizard-roles/pm.yaml`:

```yaml
id: pm
display_name: PM
charter: Turns fuzzy intent into problem-first specs, INVEST issues, and prioritized plans.
risk: low
category: product
skill_topics:
  - product-spec-and-prd-writing
  - issue-authoring-and-hygiene
  - prioritization-and-roadmapping
  - agent-team-orchestration
  - mur-repo-pm-context
```

Create `qa.yaml`, `repomanager.yaml`, `rustsmith.yaml` with the same shape (use the skill names already authored for those agents; risk: `medium` for qa, `high` for repo-manager, `medium` for rustsmith).

- [ ] **Step 5: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::catalog 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_wizard/catalog.rs mur-core/resources/wizard-roles/
git commit -m "feat(agent-wizard): extensible role catalog (shipped + ~/.mur/wizard/roles)"
```

---

## Task 3: Entitlement presets by risk level

**Files:**
- Modify: `mur-core/src/agent_wizard/entitlements.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

In `entitlements.rs`:

```rust
//! Map a role's risk level to a least-privilege entitlement plan.
use crate::agent_wizard::draft::{EntitlementPlan, RiskLevel, RoleSpec};

#[cfg(test)]
mod tests {
    use super::*;
    fn role(risk: RiskLevel) -> RoleSpec {
        RoleSpec { name: "x".into(), display_name: "X".into(), charter: "c".into(), risk, preset_id: None }
    }

    #[test]
    fn all_presets_deny_sensitive_paths() {
        for r in [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High] {
            let p = preset_for(&role(r), "/repo");
            assert!(p.deny_path.iter().any(|d| d.contains(".ssh")));
            assert!(p.tool_allow.contains(&"bash".to_string()));
        }
    }

    #[test]
    fn low_risk_has_no_write_by_default() {
        let p = preset_for(&role(RiskLevel::Low), "/repo");
        assert!(p.allow_write.is_empty(), "low-risk agents are read-only by default");
    }

    #[test]
    fn high_risk_allows_repo_write_and_git() {
        let p = preset_for(&role(RiskLevel::High), "/repo");
        assert!(p.allow_write.iter().any(|w| w == "/repo"));
        assert!(p.allow_spawn.contains(&"git".to_string()));
    }
}
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo nextest run -p mur-core agent_wizard::entitlements 2>&1 | tail -20`
Expected: FAIL — `preset_for` not defined.

- [ ] **Step 3: Implement `preset_for`**

Append to `entitlements.rs`:

```rust
/// Build a least-privilege plan. `workspace` is the path the agent may read (and,
/// for higher risk, write). Sensitive paths are always denied; bash is always allowed
/// (the agent still has its own HITL gates for irreversible actions).
pub fn preset_for(role: &RoleSpec, workspace: &str) -> EntitlementPlan {
    let mut p = EntitlementPlan {
        allow_read: vec![workspace.to_string()],
        deny_path: vec!["~/.ssh".into(), "~/.aws".into(), "~/.gnupg".into()],
        tool_allow: vec!["bash".into()],
        allow_host: vec!["127.0.0.1".into(), "localhost".into()],
        ..Default::default()
    };
    match role.risk {
        RiskLevel::Low => {}
        RiskLevel::Medium => {
            p.allow_write.push(workspace.to_string());
            p.allow_spawn.extend(["git".into()]);
        }
        RiskLevel::High => {
            p.allow_write.push(workspace.to_string());
            p.allow_spawn.extend(["git".into()]);
        }
    }
    p
}
```

- [ ] **Step 4: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::entitlements 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/agent_wizard/entitlements.rs
git commit -m "feat(agent-wizard): risk-based least-privilege entitlement presets"
```

---

## Task 4: Stage runner + progress + gate trait (deterministic, --no-llm path)

**Files:**
- Modify: `mur-core/src/agent_wizard/stages.rs`
- Modify: `mur-core/src/agent_wizard/mod.rs` (add `run_wizard`)
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test (drives a full deterministic run to the gate)**

In `stages.rs`:

```rust
//! Stage runner. Deterministic stages run here; LLM/eval stages are trait hooks
//! (no-op default impl in Plan 1, real impls in Plans 2-3).
use crate::agent_wizard::catalog::RoleManifest;
use crate::agent_wizard::draft::*;
use crate::agent_wizard::entitlements::preset_for;

/// Driver-supplied hooks. Plan 1 uses the no-op defaults (deterministic, no LLM).
pub trait WizardHooks {
    fn on_progress(&mut self, _p: &Progress) {}
    /// Return Some(skills) to override stub skills (LLM author stage, Plan 2).
    fn author_skills(&mut self, _role: &RoleSpec, _topics: &[String]) -> Option<Vec<SkillDraft>> { None }
    /// Return Some(markdown) to override the template prompt (LLM stage, Plan 2).
    fn draft_prompt(&mut self, _role: &RoleSpec) -> Option<String> { None }
    /// The review gate: return the (possibly edited) draft to approve, or None to abort.
    fn review_gate(&mut self, draft: WizardDraft) -> Option<WizardDraft> { Some(draft) }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoopHooks;
    impl WizardHooks for NoopHooks {}

    fn manifest() -> RoleManifest {
        RoleManifest {
            id: "pm".into(), display_name: "PM".into(), charter: "c".into(),
            risk: RiskLevel::Low, category: "product".into(),
            skill_topics: vec!["product-spec-and-prd-writing".into()],
        }
    }

    #[test]
    fn builds_draft_with_stub_skills_and_template_prompt() {
        let draft = build_draft(&manifest(), "/repo", "claude_sonnet", &mut NoopHooks);
        assert_eq!(draft.role.name, "pm");
        assert_eq!(draft.skills.len(), 1);
        assert!(draft.skills[0].yaml.contains("name: product-spec-and-prd-writing"));
        assert!(draft.prompt.markdown.contains("never fabricate"));
        assert_eq!(draft.model_ref, "claude_sonnet");
    }
}
```

- [ ] **Step 2: Run, expect failure**

Run: `cargo nextest run -p mur-core agent_wizard::stages 2>&1 | tail -20`
Expected: FAIL — `build_draft` not defined.

- [ ] **Step 3: Implement `build_draft` (deterministic stages 1–5)**

Append to `stages.rs`:

```rust
fn stub_skill_yaml(topic: &str, role: &RoleSpec) -> String {
    format!(
        "name: {topic}\nversion: 1.0.0\npublisher: human:{name}\n\
description: >\n  Skill stub for {topic} ({dn}). Fill with imperative rules, each with a why.\n\
category: context\nhosts: [mur-agent]\npriority: normal\ntags: [{name}]\n\
triggers:\n  - type: session_start\n  - type: command\n    pattern: /{topic}\n\
content:\n  abstract: >\n    TODO (Plan 2 LLM author stage fills this): {topic}.\n\
  context: |\n    # {topic}\n    Stub generated by `mur agent wizard --no-llm`. Edit at the review gate.\n",
        topic = topic, name = role.name, dn = role.display_name,
    )
}

fn template_prompt(role: &RoleSpec) -> String {
    format!(
        "# {dn} — {charter}\n\nYou are {dn}. Your attached skills are your standard of \"good\".\n\n\
## Operating discipline\nComplete your role's definition of done before claiming a task done.\n\n\
## Honesty\n**Never fabricate command or file output.** If you can't run a tool or read a file this \
turn, say so and reason from context.\n\n## Watching\nNarrate at a high level for a live human.\n",
        dn = role.display_name, charter = role.charter,
    )
}

/// Run deterministic stages 1-5 and assemble the draft. LLM hooks may override
/// skills/prompt; otherwise stubs/templates are used.
pub fn build_draft(
    m: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    hooks: &mut impl WizardHooks,
) -> WizardDraft {
    let role = RoleSpec {
        name: m.id.clone(),
        display_name: m.display_name.clone(),
        charter: m.charter.clone(),
        risk: m.risk,
        preset_id: Some(m.id.clone()),
    };
    hooks.on_progress(&Progress { stage: Stage::DefineRole, message: format!("role {}", role.name) });

    let skills = hooks.author_skills(&role, &m.skill_topics).unwrap_or_else(|| {
        m.skill_topics.iter().map(|t| SkillDraft { name: t.clone(), yaml: stub_skill_yaml(t, &role) }).collect()
    });
    hooks.on_progress(&Progress { stage: Stage::AuthorSkills, message: format!("{} skills", skills.len()) });

    let prompt = PromptDraft { markdown: hooks.draft_prompt(&role).unwrap_or_else(|| template_prompt(&role)) };
    hooks.on_progress(&Progress { stage: Stage::DraftPrompt, message: "prompt drafted".into() });

    let entitlements = preset_for(&role, workspace);
    hooks.on_progress(&Progress { stage: Stage::Entitlements, message: "entitlements scoped".into() });

    WizardDraft { role, skills, prompt, entitlements, model_ref: model_ref.to_string() }
}
```

- [ ] **Step 4: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::stages 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/agent_wizard/stages.rs
git commit -m "feat(agent-wizard): deterministic stage runner with progress + hook trait"
```

---

## Task 5: Validate drafts + apply (create, attach, start) — gated

**Files:**
- Modify: `mur-core/src/agent_wizard/apply.rs`
- Modify: `mur-core/src/agent_wizard/mod.rs` (`run_wizard`)
- Test: inline `#[cfg(test)]` for validation; manual smoke for apply

- [ ] **Step 1: Write the failing test for draft validation**

In `apply.rs`:

```rust
//! Apply an approved draft to disk: create agent, write prompt, attach skills, set perms, start.
use crate::agent_wizard::draft::{WizardDraft, WizardOutcome};

/// Validate every skill draft via the existing skill validator before the gate.
/// Returns the list of (skill name, error) for any that fail.
pub fn validate_drafts(draft: &WizardDraft) -> Vec<(String, String)> {
    draft.skills.iter().filter_map(|s| {
        match crate::skill_validate::validate_yaml_str(&s.yaml) {
            Ok(()) => None,
            Err(e) => Some((s.name.clone(), e.to_string())),
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::*;

    #[test]
    fn invalid_skill_yaml_is_reported() {
        let draft = WizardDraft {
            role: RoleSpec { name: "x".into(), display_name: "X".into(), charter: "c".into(), risk: RiskLevel::Low, preset_id: None },
            skills: vec![SkillDraft { name: "bad".into(), yaml: "not: a valid skill".into() }],
            prompt: PromptDraft { markdown: "p".into() },
            entitlements: EntitlementPlan::default(),
            model_ref: "claude_sonnet".into(),
        };
        let errs = validate_drafts(&draft);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, "bad");
    }
}
```

> Note: confirm the existing validator entry point. Search: `grep -rn "fn validate" mur-core/src/skill* mur-core/src/cmd/skill_cmd.rs`. If the public fn is named differently (e.g. `skills::validate::run`), adjust the call in `validate_drafts` to match — do NOT invent a name.

- [ ] **Step 2: Run, expect failure**

Run: `cargo nextest run -p mur-core agent_wizard::apply 2>&1 | tail -20`
Expected: FAIL — validator path wrong/undefined.

- [ ] **Step 3: Fix the validator call to the real entry point, then implement `apply_draft`**

After confirming the real validator fn from the grep in Step 1, implement:

```rust
/// Write the approved draft to disk and start the agent. Assumes the gate already approved.
/// Reuses existing CLI-equivalent library calls so behavior matches `mur agent create` etc.
pub fn apply_draft(draft: &WizardDraft) -> anyhow::Result<WizardOutcome> {
    let name = &draft.role.name;

    // 1. Create the agent profile (provider anthropic / model from registry).
    crate::cmd::agent::create_agent_noninteractive(
        name, &draft.role.display_name, "anthropic", "claude-sonnet-4-6",
    )?; // confirm the real create helper name via grep; adjust if different.

    // 2. Ensure model_ref resolves under launchd.
    set_model_ref(name, &draft.model_ref)?; // small helper editing profile.yaml; see Step 4.

    // 3. System prompt.
    crate::agent_admin::prompt::set(name, Some(&draft.prompt.markdown), None)?;

    // 4. Entitlements.
    let e = &draft.entitlements;
    for p in &e.allow_read { crate::agent_admin::perm::allow_read(name, p)?; }
    for p in &e.allow_write { crate::agent_admin::perm::allow_write(name, p)?; }
    for b in &e.allow_spawn { crate::agent_admin::perm::allow_spawn(name, b)?; }
    for h in &e.allow_host { crate::agent_admin::perm::allow_host(name, h)?; }
    for d in &e.deny_path { crate::agent_admin::perm::deny_path(name, d)?; }
    for t in &e.tool_allow {
        crate::agent_admin::perm::set_tool_policy(name, crate::agent_admin::perm::ToolPolicy::Allow, t)?;
    }

    // 5. Attach skills (writes each to a temp file then `agent_admin::skill::add`).
    for s in &draft.skills {
        let tmp = std::env::temp_dir().join(format!("{}-{}.yaml", name, s.name));
        std::fs::write(&tmp, &s.yaml)?;
        crate::agent_admin::skill::add(name, tmp.to_str().unwrap())?;
        let _ = std::fs::remove_file(&tmp);
    }

    // 6. Install + start the service.
    crate::agent_admin::lifecycle::install_service(name, false)?;

    Ok(WizardOutcome { agent_name: name.clone(), created: true })
}
```

> Note: `create_agent_noninteractive`, `set_model_ref`, and `lifecycle::install_service` names must be verified against the codebase (grep in Step 1's spirit). The exact `ToolPolicy` path was seen at `agent_admin/perm.rs`. Where a helper does not exist as a callable lib fn, add a thin wrapper in `cmd/agent` rather than shelling out.

- [ ] **Step 4: Add `run_wizard` orchestration to `mod.rs`**

```rust
use crate::agent_wizard::catalog::RoleManifest;
use crate::agent_wizard::stages::{build_draft, WizardHooks};

/// Full engine entry point: build draft (stages 1-5) -> validate -> gate -> apply.
pub fn run_wizard(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    hooks: &mut impl WizardHooks,
) -> anyhow::Result<WizardOutcome> {
    let draft = build_draft(manifest, workspace, model_ref, hooks);
    let errs = apply::validate_drafts(&draft);
    if !errs.is_empty() {
        anyhow::bail!("skill validation failed: {errs:?}");
    }
    hooks.on_progress(&Progress { stage: Stage::ReviewGate, message: "awaiting human approval".into() });
    let Some(approved) = hooks.review_gate(draft) else {
        return Ok(WizardOutcome { agent_name: manifest.id.clone(), created: false });
    };
    hooks.on_progress(&Progress { stage: Stage::Create, message: "creating agent".into() });
    apply::apply_draft(&approved)
}
```

- [ ] **Step 5: Run, expect pass (validation test)**

Run: `cargo nextest run -p mur-core agent_wizard 2>&1 | tail -20`
Expected: PASS for all `agent_wizard` tests.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_wizard/
git commit -m "feat(agent-wizard): draft validation + gated apply (create/attach/start)"
```

---

## Task 6: CLI command `mur agent wizard` (interactive + flags)

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (add `Wizard` variant, ~line 27)
- Create: `mur-core/src/cmd/agent/wizard.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` (dispatch + `mod wizard;`)
- Test: `--headless` smoke (manual run documented)

- [ ] **Step 1: Add the clap variant**

In `mur-core/src/cli/agent.rs`, after the `Create { .. }` variant (around line 28), add:

```rust
    /// Build a specialized agent: role -> drafts -> human review -> create + start.
    Wizard {
        /// Role preset id from the catalog, or omit for interactive selection / custom.
        #[arg(long)]
        role: Option<String>,
        /// Path the agent may read/write (defaults to current dir).
        #[arg(long)]
        workspace: Option<String>,
        /// Non-interactive: accept generated drafts without the editor gate (still prints them).
        #[arg(long)]
        headless: bool,
        /// Skip all LLM stages; use catalog stubs/templates only.
        #[arg(long = "no-llm")]
        no_llm: bool,
    },
```

- [ ] **Step 2: Implement the CLI driver (terminal hooks)**

Create `mur-core/src/cmd/agent/wizard.rs`:

```rust
//! CLI driver for `mur agent wizard`: terminal prompts, progress printing, review gate.
use crate::agent_wizard::{self, catalog, stages::WizardHooks, Progress, WizardDraft};

struct CliHooks { headless: bool }

impl WizardHooks for CliHooks {
    fn on_progress(&mut self, p: &Progress) {
        eprintln!("  [{:?}] {}", p.stage, p.message);
    }
    fn review_gate(&mut self, draft: WizardDraft) -> Option<WizardDraft> {
        println!("\n=== Review drafts for '{}' ===", draft.role.name);
        for s in &draft.skills { println!("- skill: {}", s.name); }
        println!("- prompt: {} chars", draft.prompt.markdown.len());
        println!("- entitlements: write={:?} spawn={:?} hosts={:?}",
            draft.entitlements.allow_write, draft.entitlements.allow_spawn, draft.entitlements.allow_host);
        if self.headless {
            println!("(--headless: auto-approved)");
            return Some(draft);
        }
        print!("\nApprove and create this agent? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if line.trim().eq_ignore_ascii_case("y") { Some(draft) } else { println!("Aborted."); None }
    }
}

pub fn run(role: Option<String>, workspace: Option<String>, headless: bool, _no_llm: bool) -> anyhow::Result<()> {
    let mur_home = crate::paths::mur_home(); // confirm helper name via grep `fn mur_home`
    let catalog = catalog::load_catalog(&mur_home);

    let role_id = match role {
        Some(r) => r,
        None => prompt_role_choice(&catalog)?, // interactive picker; see Step 3
    };
    let manifest = catalog.iter().find(|m| m.id == role_id)
        .ok_or_else(|| anyhow::anyhow!("unknown role '{role_id}'. Known: {:?}",
            catalog.iter().map(|m| &m.id).collect::<Vec<_>>()))?;

    let ws = workspace.unwrap_or_else(|| std::env::current_dir().unwrap().display().to_string());
    let mut hooks = CliHooks { headless };
    let outcome = agent_wizard::run_wizard(manifest, &ws, "claude_sonnet", &mut hooks)?;
    if outcome.created {
        println!("\n✅ Created and started agent '{}'.", outcome.agent_name);
    } else {
        println!("\nNo agent created.");
    }
    Ok(())
}
```

- [ ] **Step 3: Add the interactive role picker**

Append to `wizard.rs`:

```rust
fn prompt_role_choice(catalog: &[catalog::RoleManifest]) -> anyhow::Result<String> {
    println!("Choose a role preset:");
    for (i, m) in catalog.iter().enumerate() {
        println!("  {}) {} — {}", i + 1, m.id, m.charter);
    }
    println!("  (or type a new id for a custom role)");
    print!("> ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim();
    if let Ok(n) = s.parse::<usize>() {
        if (1..=catalog.len()).contains(&n) { return Ok(catalog[n - 1].id.clone()); }
    }
    // Custom role: in Plan 1 (--no-llm), require it to already exist as a manifest.
    anyhow::bail!("custom roles need an LLM (Plan 2) or a manifest in ~/.mur/wizard/roles/; pick a listed preset for now");
}
```

- [ ] **Step 4: Wire dispatch in `cmd/agent/mod.rs`**

Add `pub mod wizard;` and in the match that dispatches `AgentCmd` variants, add:

```rust
        AgentCmd::Wizard { role, workspace, headless, no_llm } => {
            wizard::run(role, workspace, headless, no_llm)?;
        }
```

- [ ] **Step 5: Build + run the headless smoke**

Run: `cargo build -p mur-core 2>&1 | tail -5`
Expected: builds clean.

Run: `cargo run -p mur-core -- agent wizard --role pm --workspace /tmp/wztest --headless 2>&1 | tail -20`
Expected: prints stage progress, the review summary, "auto-approved", then "✅ Created and started agent 'pm'". (Use a throwaway name by first copying `pm.yaml` to a `wztest` id if `pm` already exists, to avoid clobbering.)

- [ ] **Step 6: Clean up the smoke agent + commit**

```bash
mur agent stop wztest 2>/dev/null; mur agent remove wztest --purge 2>/dev/null
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/wizard.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(agent-wizard): mur agent wizard CLI (interactive + --headless/--no-llm)"
```

---

## Task 7: Gate the whole plan green

- [ ] **Step 1: Full gate**

Run: `cargo build --workspace && cargo nextest run -p mur-core agent_wizard && cargo clippy -p mur-core --all-targets -- -D warnings 2>&1 | tail -15`
Expected: build clean, all `agent_wizard` tests pass, zero clippy warnings.

- [ ] **Step 2: Commit any clippy fixes**

```bash
git add -A && git commit -m "chore(agent-wizard): clippy clean for plan 1"
```

---

## Self-Review

- **Spec coverage (Plan 1 slice):** engine module ✓ (Tasks 1–5), deterministic stages 1/5/6/7 ✓, role catalog custom-first + extensible ✓ (Task 2), risk→entitlement preset ✓ (Task 3), human review gate ✓ (Task 5/6 `review_gate`), skill validation before gate ✓ (Task 5), CLI driver ✓ (Task 6). LLM stages (2–4), eval (8), and Hub are explicitly deferred to Plans 2–5.
- **Placeholder scan:** the two "confirm the real entry point" notes (validator fn, create helper, `mur_home`) are deliberate verification steps, not vague placeholders — each says exactly what to grep and forbids inventing names. The skill stub YAML contains a literal `TODO` *as generated output content* for the Plan-2 author stage to replace, not as a plan placeholder.
- **Type consistency:** `WizardDraft`/`RoleSpec`/`EntitlementPlan`/`Stage`/`Progress`/`WizardHooks`/`build_draft`/`run_wizard`/`apply_draft`/`validate_drafts` names are used identically across Tasks 1–6.

---

## Decomposition roadmap (remaining plans)

Each is a separate plan file, written when its turn comes; each ships testable software.

- **Plan 2 — LLM stages:** implement `WizardHooks::author_skills` / `draft_prompt` and the research stage by calling the model registry (→ cc-proxy), with a `MockModelProvider` for offline tests and graceful skip (`--no-llm` / no model). Provider-agnostic search-MCP augmentation (default reference Tavily), two-layer discovery→extract, citations.
- **Plan 3 — Eval stage:** rubric (per-dimension graders; deterministic safety + skill-usage checks, LLM judge for subjective dims), pass bar (each ≥4/5 AND overall ≥0.90 AND zero safety violations), auto-fix capped at N=2, AgentDojo/HarmBench for high-risk; records to `eval-runs/`; passing set becomes regression set.
- **Plan 4 — Hub Specialist flow:** Step 0 fork (Companion/Specialist/Both); `wizard_spec_*` Tauri commands over the same engine; `wizard-progress` events; editable draft-review screen; live eval scores.
- **Plan 5 — Catalog content:** seed the full starter role catalog (DevOps/SRE, Security reviewer, Tech writer, Data/ML, Frontend, Support-triage) and document authoring a custom role manifest.

## Review-captured deferrals (from Plan 1 final review, 2026-06-15)

Plan 1 was implemented and reviewed (APPROVED). Three items were deliberately deferred and are
tracked here so later plans pick them up:

- **`$EDITOR` inline draft editing (→ Plan 2/Hub).** Plan 1's review gate is approve-only
  (`[y/N]`), which satisfies the "blocking human approval before creation" safety requirement.
  Inline editing of the generated skills/prompt at the gate (CLI `$EDITOR` round-trip; Hub
  editable fields) lands with the LLM/Hub work.
- **Medium vs High entitlement differentiation (→ Plan 3).** In Plan 1 both tiers get
  workspace-write + git. Per the design, the High distinction is expressed in eval (AgentDojo/
  HarmBench security suites run for high-risk agents), not in the base entitlement preset. Revisit
  if a per-tier entitlement difference is wanted.
- **Guided `custom.yaml` template (→ Plan 2).** Custom roles already work in Plan 1 via a manifest
  in `~/.mur/wizard/roles/` (the shipped pm/qa/repo-manager/rustsmith manifests are the examples).
  A first-class custom path (describe-a-role → LLM-generate) is the Plan 2 feature; ship a copyable
  `custom.yaml` template alongside it.
