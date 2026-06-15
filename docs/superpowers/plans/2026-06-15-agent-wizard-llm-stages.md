# Agent Wizard — LLM Stages (Plan 2 of 5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `mur agent wizard` generate real, researched skills and a real DoD system prompt with the configured model — replacing Plan 1's deterministic stubs/templates when a model is available, and falling back to them gracefully when it isn't.

**Architecture:** Reuse the existing `mur_common::llm::LlmClient` abstraction (already used by `mur skill generate`). The engine's `run_wizard` becomes async and takes an `Option<Arc<dyn LlmClient>>`: when `Some`, async LLM stages author skills + draft the prompt (with optional research notes from a provider-agnostic `SearchProvider` seam); when `None` (no model or `--no-llm`), it falls back to Plan 1's `build_draft` stubs. The CLI builds the live `LlmClient` exactly like `cmd_generate_cli` does (Config → backend factory → `ChatBackendAdapter`). All LLM/search work is behind traits so unit tests use mocks and never hit the network.

**Tech Stack:** Rust 2024, `mur_common::llm::{LlmClient, LlmError}`, `crate::conversations::backend::{ChatBackend, ChatRequest, factory::build_for_stage}`, `mur_common::config::Config`, `mur_common::skill::{parse_canonical, validate}`, tokio (async), `cargo nextest run`.

**Builds on Plan 1** (branch `feat/agent-wizard`): `mur-core/src/agent_wizard/{draft,catalog,entitlements,stages,apply,mod}.rs` + `cmd/agent/wizard.rs`. **Ships working software:** wizard with LLM-authored output + graceful fallback. The concrete search-MCP provider is scoped as the final (optional) task; its absence is handled by `NoopSearch`.

---

## Reference: how to call the model from mur-core (verified)

`mur-core/src/cmd/skill_generate.rs` is the working analog. The live client is built as:

```rust
let home = crate::cmd::agent::resolve_mur_home()?;
let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
let backend_cfg = cfg.conversations.ask.synthesize_backend(); // has .model
let backend: std::sync::Arc<dyn crate::conversations::backend::ChatBackend> =
    crate::conversations::backend::factory::build_for_stage(&backend_cfg, "agent.wizard")?;
```

`mur_common::llm::LlmClient` (reuse, do NOT redefine) has:
`fn complete(&self, prompt: &str, system: Option<&str>) -> impl Future<Output = Result<String, LlmError>> + Send;`
and `async fn embed(&self, _: &str) -> Result<Vec<f32>, LlmError>`.

The `ChatBackendAdapter` in `skill_generate.rs` already adapts a `ChatBackend` to `LlmClient`. **Move it to a shared location** so the wizard reuses it (Task 5), rather than duplicating.

---

## File Structure

- Create `mur-core/src/agent_wizard/llm.rs` — async LLM stages: `author_skills_llm`, `draft_prompt_llm`, prompt builders, output parsing + validate-and-repair. One responsibility: turn a role (+research) into validated `SkillDraft`s and a `PromptDraft` via an `LlmClient`.
- Create `mur-core/src/agent_wizard/research.rs` — `SearchProvider` trait + `NoopSearch` + `ResearchNote`. One responsibility: optional grounding notes.
- Modify `mur-core/src/agent_wizard/stages.rs` — trim `WizardHooks` to `on_progress` + `review_gate` (remove the now-unused `author_skills`/`draft_prompt` hook methods; `build_draft` stays as the no-LLM fallback).
- Modify `mur-core/src/agent_wizard/mod.rs` — `run_wizard` becomes `async` and takes `llm: Option<Arc<dyn LlmClient>>` + `search: Option<Arc<dyn SearchProvider>>`; declare `pub mod llm; pub mod research;`.
- Modify `mur-core/src/cmd/agent/wizard.rs` — `run` becomes `async`; builds the live `LlmClient` (unless `--no-llm`) and a `SearchProvider`; passes them in.
- Modify `mur-core/src/cmd/skill_generate.rs` — relocate `ChatBackendAdapter` to a shared module (e.g. `crate::conversations::backend::adapter`) and re-use; keep `skill_generate` working.
- Modify `mur-core/src/dispatch.rs` — `AgentAction::Wizard { .. }` arm awaits `wizard::run(...).await?`.

---

## Task 1: LLM skill authoring (`author_skills_llm`) with mock

**Files:**
- Create: `mur-core/src/agent_wizard/llm.rs`
- Modify: `mur-core/src/agent_wizard/mod.rs` (add `pub mod llm;`)
- Test: inline `#[cfg(test)]` in `llm.rs`

- [ ] **Step 1: Declare the module**

In `mod.rs` add `pub mod llm;` next to the other submodule declarations.

- [ ] **Step 2: Write the failing test with a MockLlmClient**

In `llm.rs`:

```rust
//! LLM stages: author skills and draft the DoD prompt from a role, via an LlmClient.
use crate::agent_wizard::draft::{RoleSpec, SkillDraft};
use crate::agent_wizard::research::ResearchNote;
use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use std::sync::Arc;

/// Build the per-skill authoring prompt for one topic.
fn skill_prompt(role: &RoleSpec, topic: &str, notes: &[ResearchNote]) -> String {
    let mut p = format!(
        "Write a MUR agent skill manifest as a single YAML document for the skill topic \
\"{topic}\", for the agent role \"{dn}\" ({charter}).\n\
Output ONLY the YAML (no markdown fences). It MUST have these top-level keys: name (== \"{topic}\"), \
version: 1.0.0, publisher: human:{name}, a third-person trigger-rich `description`, \
category: context, hosts: [mur-agent], priority: normal, tags, triggers (session_start + a \
keyword regex + a /command), and content with `abstract` and `context`. The `context` body must be \
5-8 imperative rules, each with a one-line *why*.\n",
        topic = topic, dn = role.display_name, charter = role.charter, name = role.name,
    );
    if !notes.is_empty() {
        p.push_str("\nGround the rules in these researched notes (cite where relevant):\n");
        for n in notes {
            p.push_str(&format!("- {} ({})\n", n.summary, n.url));
        }
    }
    p
}

/// Author one skill per topic via the LLM, validating each; one repair retry on invalid YAML.
pub async fn author_skills_llm(
    llm: &Arc<dyn LlmClient>,
    role: &RoleSpec,
    topics: &[String],
    notes: &[ResearchNote],
) -> Result<Vec<SkillDraft>, LlmError> {
    let mut out = Vec::new();
    for topic in topics {
        let prompt = skill_prompt(role, topic, notes);
        let yaml = author_one(llm, &prompt).await?;
        out.push(SkillDraft { name: topic.clone(), yaml });
    }
    Ok(out)
}

/// Call the model, strip stray fences, and validate; on invalid, ask once to fix.
async fn author_one(llm: &Arc<dyn LlmClient>, prompt: &str) -> Result<String, LlmError> {
    let sys = "You are an expert author of MUR agent skills. Output only valid YAML.";
    let raw = strip_fences(&llm.complete(prompt, Some(sys)).await?);
    if mur_common::skill::parse_canonical(&raw)
        .and_then(|m| mur_common::skill::validate(&m).map_err(Into::into))
        .is_ok()
    {
        return Ok(raw);
    }
    let fix = format!(
        "The YAML you produced was not a valid MUR skill. Fix it and output ONLY corrected YAML.\n\
Previous output:\n{raw}"
    );
    Ok(strip_fences(&llm.complete(&fix, Some(sys)).await?))
}

/// Remove ```yaml / ``` fences a model may wrap output in.
fn strip_fences(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("```yaml").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.trim_end_matches("```").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::RiskLevel;

    /// A canned LlmClient that returns a fixed valid skill yaml.
    struct MockLlm(String);
    impl LlmClient for MockLlm {
        fn complete(
            &self, _p: &str, _s: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let v = self.0.clone();
            async move { Ok(v) }
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>, LlmError> { Ok(vec![]) }
    }

    fn role() -> RoleSpec {
        RoleSpec { name: "pm".into(), display_name: "PM".into(), charter: "c".into(),
            risk: RiskLevel::Low, preset_id: Some("pm".into()) }
    }

    fn valid_yaml(name: &str) -> String {
        format!("name: {name}\nversion: 1.0.0\npublisher: human:pm\n\
description: A test skill for {name} used when doing {name} work in the repo.\n\
category: context\nhosts: [mur-agent]\npriority: normal\ntags: [pm]\n\
triggers:\n  - type: session_start\n  - type: command\n    pattern: /{name}\n\
content:\n  abstract: A test abstract.\n  context: |\n    # {name}\n    - Do the thing. *Why: it helps.*\n")
    }

    #[tokio::test]
    async fn authors_one_skill_per_topic_and_strips_fences() {
        let fenced = format!("```yaml\n{}\n```", valid_yaml("product-spec"));
        let llm: Arc<dyn LlmClient> = Arc::new(MockLlm(fenced));
        let skills = author_skills_llm(&llm, &role(), &["product-spec".into()], &[]).await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "product-spec");
        assert!(skills[0].yaml.starts_with("name: product-spec"));
        // and it's valid:
        let m = mur_common::skill::parse_canonical(&skills[0].yaml).unwrap();
        assert!(mur_common::skill::validate(&m).is_ok());
    }
}
```

> Note: confirm `mur_common::skill::validate` returns an error type that converts into `LlmError` via `.map_err(Into::into)`. If not, map explicitly: `.map_err(|e| LlmError::Other(e.to_string()))`. Verify the real `validate` signature first (`grep -n "pub fn validate" mur-common/src/skill*`).

- [ ] **Step 3: Run, expect failure**

Run: `cargo nextest run -p mur-core agent_wizard::llm 2>&1 | tail -20`
Expected: FAIL — `research::ResearchNote` not defined yet (Task creates it) / module missing.

> If `research` doesn't exist yet, add a minimal `ResearchNote { pub summary: String, pub url: String }` in `research.rs` now (Task 4 fleshes the trait) so this compiles, OR reorder to do Task 4's `ResearchNote` first. Recommended: create `research.rs` with just `ResearchNote` here, full `SearchProvider` in Task 4.

- [ ] **Step 4: Implement to pass (the code above already does); create `research.rs` stub**

Create `mur-core/src/agent_wizard/research.rs`:

```rust
//! Optional research grounding for the LLM author stage.
/// A single researched note used to ground generated skills.
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchNote {
    pub summary: String,
    pub url: String,
}
```
Add `pub mod research;` to `mod.rs`.

- [ ] **Step 5: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::llm 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_wizard/llm.rs mur-core/src/agent_wizard/research.rs mur-core/src/agent_wizard/mod.rs
git commit -m "feat(agent-wizard): LLM skill authoring stage (author_skills_llm) + mock test"
```

---

## Task 2: LLM prompt drafting (`draft_prompt_llm`) with mock

**Files:**
- Modify: `mur-core/src/agent_wizard/llm.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

Add to `llm.rs` tests:

```rust
    #[tokio::test]
    async fn drafts_a_prompt_containing_role_and_honesty_rule() {
        let body = "# PM — c\n\n## Honesty\nyou must never fabricate command or file output.\n";
        let llm: Arc<dyn LlmClient> = Arc::new(MockLlm(body.to_string()));
        let md = draft_prompt_llm(&llm, &role()).await.unwrap();
        assert!(md.contains("PM"));
        assert!(md.to_lowercase().contains("never fabricate"));
    }
```

- [ ] **Step 2: Run, expect failure** — `draft_prompt_llm` not defined.

Run: `cargo nextest run -p mur-core agent_wizard::llm::tests::drafts_a_prompt 2>&1 | tail -10`

- [ ] **Step 3: Implement**

Add to `llm.rs`:

```rust
/// Draft a DoD system prompt for the role via the LLM. The result must carry the
/// honesty rule; if the model omits it, append it (defense-in-depth, not a silent trust).
pub async fn draft_prompt_llm(llm: &Arc<dyn LlmClient>, role: &RoleSpec) -> Result<String, LlmError> {
    let sys = "You write system prompts for specialized AI agents. Be concise and concrete.";
    let prompt = format!(
        "Write a system prompt (markdown) for an agent named \"{dn}\" whose charter is: {charter}.\n\
Include: a persona line; an operating-discipline / definition-of-done gate the agent must satisfy \
before claiming a task done; HITL rules (irreversible actions need explicit human confirmation); \
an honesty rule stating it must never fabricate command or file output; and a short 'narrate for a \
watching human' line. Risk level: {risk:?}.",
        dn = role.display_name, charter = role.charter, risk = role.risk,
    );
    let mut md = llm.complete(&prompt, Some(sys)).await?;
    if !md.to_lowercase().contains("never fabricate") {
        md.push_str("\n\n## Honesty\nYou must never fabricate command or file output. If you can't \
run a tool or read a file this turn, say so and reason from context.\n");
    }
    Ok(md)
}
```

- [ ] **Step 4: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard::llm 2>&1 | tail -10`
Expected: PASS (both llm tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/agent_wizard/llm.rs
git commit -m "feat(agent-wizard): LLM prompt drafting stage (draft_prompt_llm) + mock test"
```

---

## ⚠ Design correction (applied during execution): LlmClient is not dyn-compatible

`mur_common::llm::LlmClient` uses RPITIT, so `Arc<dyn LlmClient>` / `&dyn LlmClient` fail with **E0038**.
Tasks 1–2 therefore implemented `author_skills_llm`/`draft_prompt_llm` as **generic** `<L: LlmClient>`.
For the *optional* LLM in `run_wizard` (`Option<Arc<dyn …>>`), Task 3 introduces an **object-safe
wrapper** and switches the wizard to it:

```rust
// in llm.rs
use std::{pin::Pin, future::Future, sync::Arc};
use mur_common::error::LlmError;
pub trait WizardLlm: Send + Sync {
    fn complete<'a>(&'a self, prompt: &'a str, system: Option<&'a str>)
        -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + 'a>>;
}
impl<L: mur_common::llm::LlmClient + Send + Sync> WizardLlm for L {
    fn complete<'a>(&'a self, p: &'a str, s: Option<&'a str>)
        -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + 'a>> {
        Box::pin(mur_common::llm::LlmClient::complete(self, p, s))
    }
}
```

Then: `author_skills_llm`/`author_one`/`draft_prompt_llm` take `llm: &dyn WizardLlm` (call
`llm.complete(...)`), Task 1/2 tests call them with `&MockLlm(...)` (coerces via the blanket impl),
and `build_wizard_draft`/`run_wizard` take `Option<Arc<dyn WizardLlm>>`. Everywhere the plan below
says `dyn LlmClient`, read `dyn WizardLlm`. See memory `gotcha_llmclient_not_dyn_compatible`.

## Task 3: Async `run_wizard` with LLM-or-fallback

**Files:**
- Modify: `mur-core/src/agent_wizard/mod.rs`
- Modify: `mur-core/src/agent_wizard/stages.rs` (trim `WizardHooks`)
- Test: inline in `mod.rs`

- [ ] **Step 1: Trim `WizardHooks` in `stages.rs`**

Remove the `author_skills` and `draft_prompt` methods from the `WizardHooks` trait (they're superseded by the LLM stages). Keep `on_progress` and `review_gate`. Update the test `NoopHooks`/`CliHooks` accordingly. `build_draft` stays unchanged as the no-LLM fallback (it already uses stubs/templates directly).

- [ ] **Step 2: Write the failing test for async run_wizard**

In `mod.rs` tests (add `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::catalog::RoleManifest;
    use crate::agent_wizard::stages::WizardHooks;
    use mur_common::error::LlmError;
    use mur_common::llm::LlmClient;
    use std::sync::Arc;

    struct GateOnly; // approves without editing, no progress
    impl WizardHooks for GateOnly {}

    struct MockLlm(String);
    impl LlmClient for MockLlm {
        fn complete(&self, _p: &str, _s: Option<&str>)
            -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let v = self.0.clone();
            async move { Ok(v) }
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>, LlmError> { Ok(vec![]) }
    }

    fn manifest() -> RoleManifest {
        RoleManifest { id: "pm".into(), display_name: "PM".into(), charter: "c".into(),
            risk: crate::agent_wizard::draft::RiskLevel::Low,
            skill_topics: vec!["product-spec".into()], category: "product".into() }
    }

    // build_draft_only is the testable core that stops BEFORE apply (no disk writes).
    #[tokio::test]
    async fn llm_present_uses_llm_skills() {
        let yaml = "name: product-spec\nversion: 1.0.0\npublisher: human:pm\n\
description: Spec skill used when writing specs in the repo.\ncategory: context\n\
hosts: [mur-agent]\npriority: normal\ntags: [pm]\ntriggers:\n  - type: session_start\n\
  - type: command\n    pattern: /product-spec\ncontent:\n  abstract: A.\n  context: |\n    # x\n    - Do. *Why: y.*\n";
        let llm: Option<Arc<dyn LlmClient>> = Some(Arc::new(MockLlm(yaml.to_string())));
        let draft = build_wizard_draft(&manifest(), "/repo", "claude_sonnet", &llm, None, &mut GateOnly).await;
        // LLM skill yaml differs from the stub (no "Stub generated" marker):
        assert!(!draft.skills[0].yaml.contains("Stub generated"));
        assert!(draft.skills[0].yaml.starts_with("name: product-spec"));
    }

    #[tokio::test]
    async fn no_llm_falls_back_to_stub() {
        let none: Option<Arc<dyn LlmClient>> = None;
        let draft = build_wizard_draft(&manifest(), "/repo", "claude_sonnet", &none, None, &mut GateOnly).await;
        assert!(draft.skills[0].yaml.contains("Stub generated"));
    }
}
```

> Note: `build_wizard_draft` is the async assembler that runs research+LLM-or-stub stages and returns the `WizardDraft` WITHOUT applying (so it's unit-testable with no disk writes). `run_wizard` = `build_wizard_draft` → validate → gate → `apply_draft`.

- [ ] **Step 3: Run, expect failure** — `build_wizard_draft` not defined.

- [ ] **Step 4: Implement `build_wizard_draft` + async `run_wizard`**

In `mod.rs`:

```rust
use crate::agent_wizard::catalog::RoleManifest;
use crate::agent_wizard::research::{ResearchNote, SearchProvider};
use crate::agent_wizard::stages::{WizardHooks, build_draft};
use mur_common::llm::LlmClient;
use std::sync::Arc;

/// Assemble the draft (stages 1-5). Uses the LLM for skills+prompt when present,
/// else Plan-1 stubs/templates. Does NOT write anything to disk.
pub async fn build_wizard_draft(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    llm: &Option<Arc<dyn LlmClient>>,
    search: Option<&Arc<dyn SearchProvider>>,
    hooks: &mut impl WizardHooks,
) -> WizardDraft {
    // No LLM -> deterministic Plan-1 path.
    let Some(llm) = llm else {
        return build_draft(manifest, workspace, model_ref, hooks);
    };
    let role = RoleSpec {
        name: manifest.id.clone(), display_name: manifest.display_name.clone(),
        charter: manifest.charter.clone(), risk: manifest.risk, preset_id: Some(manifest.id.clone()),
    };
    hooks.on_progress(&Progress { stage: Stage::DefineRole, message: format!("role {}", role.name) });

    let notes: Vec<ResearchNote> = match search {
        Some(s) => {
            hooks.on_progress(&Progress { stage: Stage::Research, message: "researching".into() });
            s.research(&role, &manifest.skill_topics).await.unwrap_or_default()
        }
        None => Vec::new(),
    };

    let skills = match crate::agent_wizard::llm::author_skills_llm(llm, &role, &manifest.skill_topics, &notes).await {
        Ok(s) => s,
        Err(e) => { // graceful: fall back to stubs on LLM failure, flagged via progress
            hooks.on_progress(&Progress { stage: Stage::AuthorSkills, message: format!("LLM failed ({e}); using stubs") });
            return build_draft(manifest, workspace, model_ref, hooks);
        }
    };
    hooks.on_progress(&Progress { stage: Stage::AuthorSkills, message: format!("{} skills", skills.len()) });

    let prompt_md = crate::agent_wizard::llm::draft_prompt_llm(llm, &role).await
        .unwrap_or_else(|_| stages::template_prompt_public(&role));
    hooks.on_progress(&Progress { stage: Stage::DraftPrompt, message: "prompt drafted".into() });

    let entitlements = crate::agent_wizard::entitlements::preset_for(&role, workspace);
    hooks.on_progress(&Progress { stage: Stage::Entitlements, message: "entitlements scoped".into() });

    WizardDraft { role, skills, prompt: PromptDraft { markdown: prompt_md }, entitlements, model_ref: model_ref.to_string() }
}

/// Full entry: build draft -> validate -> gate -> apply.
pub async fn run_wizard(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    llm: Option<Arc<dyn LlmClient>>,
    search: Option<Arc<dyn SearchProvider>>,
    hooks: &mut impl WizardHooks,
) -> anyhow::Result<WizardOutcome> {
    let draft = build_wizard_draft(manifest, workspace, model_ref, &llm, search.as_ref(), hooks).await;
    let errs = apply::validate_drafts(&draft);
    if !errs.is_empty() { anyhow::bail!("skill validation failed: {errs:?}"); }
    hooks.on_progress(&Progress { stage: Stage::ReviewGate, message: "awaiting human approval".into() });
    let Some(approved) = hooks.review_gate(draft) else {
        return Ok(WizardOutcome { agent_name: manifest.id.clone(), created: false });
    };
    hooks.on_progress(&Progress { stage: Stage::Create, message: "creating agent".into() });
    apply::apply_draft(&approved)
}
```

In `stages.rs`, expose the template helper for fallback: rename `template_prompt` usage so there's a `pub fn template_prompt_public(role: &RoleSpec) -> String` (or make `template_prompt` `pub`). Keep `build_draft` using it.

- [ ] **Step 5: Run, expect pass**

Run: `cargo nextest run -p mur-core agent_wizard 2>&1 | tail -10`
Expected: PASS (all agent_wizard tests, including the two new run-path tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_wizard/mod.rs mur-core/src/agent_wizard/stages.rs
git commit -m "feat(agent-wizard): async run_wizard with LLM stages + graceful stub fallback"
```

---

## Task 4: `SearchProvider` seam + Noop (provider-agnostic research)

**Files:**
- Modify: `mur-core/src/agent_wizard/research.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

In `research.rs`:

```rust
use crate::agent_wizard::draft::RoleSpec;
use std::sync::Arc;

/// Optional research provider. Implementations may call a search MCP (Tavily/Exa/Brave/…).
/// Provider-agnostic: the engine only depends on this trait.
pub trait SearchProvider: Send + Sync {
    fn research(&self, role: &RoleSpec, topics: &[String])
        -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResearchNote>>> + Send + '_>>;
}

/// Default: no external research (pure model-knowledge drafting).
pub struct NoopSearch;
impl SearchProvider for NoopSearch {
    fn research(&self, _role: &RoleSpec, _topics: &[String])
        -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResearchNote>>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::RiskLevel;

    #[tokio::test]
    async fn noop_returns_no_notes() {
        let p: Arc<dyn SearchProvider> = Arc::new(NoopSearch);
        let role = RoleSpec { name: "x".into(), display_name: "X".into(), charter: "c".into(),
            risk: RiskLevel::Low, preset_id: None };
        let notes = p.research(&role, &["t".into()]).await.unwrap();
        assert!(notes.is_empty());
    }
}
```

> Note: the trait uses a boxed future (not `async fn` in trait) so it is object-safe as `dyn SearchProvider` (the engine holds `Arc<dyn SearchProvider>`). Confirm `LlmClient` is similarly used as `dyn` in Task 1/3 — it is, via `Arc<dyn LlmClient>` (its `complete` returns `impl Future` but is used through the existing object-safe path in skill_generate; if `dyn LlmClient` is NOT object-safe in this codebase, mirror skill_generate's exact usage — it already stores `Arc<L>` generically. If `dyn` fails to compile, make `run_wizard`/`build_wizard_draft` generic `<L: LlmClient>` instead of `dyn`, matching `cmd_generate<L>`).

- [ ] **Step 2: Run, expect failure** then **Step 3: it compiles/passes** (code above is the impl).

Run: `cargo nextest run -p mur-core agent_wizard::research 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/agent_wizard/research.rs
git commit -m "feat(agent-wizard): provider-agnostic SearchProvider seam + NoopSearch"
```

---

## Task 5: CLI wiring — build live LlmClient, async run

**Files:**
- Modify: `mur-core/src/cmd/skill_generate.rs` (relocate `ChatBackendAdapter` → shared)
- Create: `mur-core/src/conversations/backend/adapter.rs` (`ChatBackendAdapter` + a constructor `build_llm_for_stage(home, model_override, stage) -> Result<Arc<dyn LlmClient>>`)
- Modify: `mur-core/src/cmd/agent/wizard.rs` (async `run`, build llm unless `--no-llm`)
- Modify: `mur-core/src/dispatch.rs` (await the wizard arm)
- Test: manual smoke (LLM requires network — not a CI unit test)

- [ ] **Step 1: Extract a shared adapter + builder**

Move `ChatBackendAdapter` out of `skill_generate.rs` into `mur-core/src/conversations/backend/adapter.rs` (declare `pub mod adapter;` in `conversations/backend/mod.rs`), make it `pub`, and add:

```rust
use std::sync::Arc;
use mur_common::config::Config;
use mur_common::llm::LlmClient;
use anyhow::{Context, Result};

/// Build a live LlmClient for a given stage from the user's config (Config -> backend factory).
pub fn build_llm_for_stage(home: &std::path::Path, model_override: Option<&str>, stage: &str)
    -> Result<Arc<dyn LlmClient>> {
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let mut backend_cfg = cfg.conversations.ask.synthesize_backend();
    if let Some(m) = model_override { backend_cfg.model = m.to_string(); }
    let backend = super::factory::build_for_stage(&backend_cfg, stage).context("build llm")?;
    Ok(Arc::new(ChatBackendAdapter { backend, model: backend_cfg.model.clone() }))
}
```

Update `skill_generate.rs` to `use crate::conversations::backend::adapter::ChatBackendAdapter;` (delete its local copy). Run `cargo nextest run -p mur-core skill_generate` to confirm skill_generate still builds/passes.

- [ ] **Step 2: Make `wizard::run` async and build the client**

In `cmd/agent/wizard.rs`, change `pub fn run(...)` to `pub async fn run(role, workspace, headless, no_llm) -> anyhow::Result<()>` and, after resolving the manifest:

```rust
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let llm: Option<std::sync::Arc<dyn mur_common::llm::LlmClient>> = if no_llm {
        None
    } else {
        match crate::conversations::backend::adapter::build_llm_for_stage(&mur_home, None, "agent.wizard") {
            Ok(c) => Some(c),
            Err(e) => { eprintln!("warning: no usable model ({e}); generating deterministic stubs"); None }
        }
    };
    let search: Option<std::sync::Arc<dyn crate::agent_wizard::research::SearchProvider>> = None; // Task 6 / future
    let mut hooks = CliHooks { headless };
    let outcome = crate::agent_wizard::run_wizard(manifest, &ws, "claude_sonnet", llm, search, &mut hooks).await?;
```

- [ ] **Step 3: Await in dispatch**

In `mur-core/src/dispatch.rs`, change the `AgentAction::Wizard { role, workspace, headless, no_llm } =>` arm to `wizard::run(role, workspace, headless, no_llm).await?;`.

- [ ] **Step 4: Build + the no-LLM smoke still works**

Run: `cargo build -p mur-core 2>&1 | tail -5` (clean).
Run (no network needed): create `~/.mur/wizard/roles/wztest2.yaml` (id wztest2, low risk, 1 topic), then
`cargo run -p mur-core -- agent wizard --role wztest2 --workspace /tmp/wz2 --no-llm --headless 2>&1 | tail -20`
Expected: stub path runs, agent created. Then clean up: `mur agent stop wztest2; mur agent remove wztest2 --purge; rm ~/.mur/wizard/roles/wztest2.yaml`.

> The LLM path requires a live model (cc-proxy) and is verified manually by the operator, not in CI. Document this; do not add a network test.

- [ ] **Step 5: Commit (after cleanup)**

```bash
git add mur-core/src/conversations/backend/adapter.rs mur-core/src/conversations/backend/mod.rs mur-core/src/cmd/skill_generate.rs mur-core/src/cmd/agent/wizard.rs mur-core/src/dispatch.rs
git commit -m "feat(agent-wizard): wire live LlmClient into CLI; async run + dispatch"
```

---

## Task 6: Green gate + fmt + final review

- [ ] **Step 1: Gate**

Run: `cargo fmt --all && cargo build --workspace && cargo nextest run -p mur-core agent_wizard skill_generate 2>&1 | tail -10`
Expected: fmt clean, build clean, all tests pass.

- [ ] **Step 2: Clippy on touched files**

Run: `cargo clippy -p mur-core --all-targets 2>&1 | grep -E "agent_wizard/|wizard\.rs|backend/adapter\.rs" | sort -u`
Expected: empty (our code clippy-clean). Fix any that appear.

- [ ] **Step 3: Commit + push**

```bash
git add -A && git commit -m "chore(agent-wizard): fmt + clippy clean for plan 2" && git push
```

---

## Self-Review

- **Spec coverage:** LLM author skills ✓ (Task 1), LLM prompt ✓ (Task 2), hybrid graceful skip (`--no-llm`/no model/LLM error → stubs) ✓ (Task 3/5), provider-agnostic search seam ✓ (Task 4), live client via existing Config→backend path ✓ (Task 5), validate-and-repair on generated skills ✓ (Task 1). Concrete Tavily/MCP search impl is deferred (NoopSearch covers absence) — captured in roadmap.
- **Placeholder scan:** the two "confirm/Note" items (validate error-conversion; `dyn LlmClient` object-safety fallback to generics) are explicit verification steps with the exact grep/fallback, not vague placeholders. The stub marker string "Stub generated" referenced in tests must match Plan 1's `stub_skill_yaml` output — confirm the literal in `stages.rs` and align the assertion if Plan 1 used different words.
- **Type consistency:** `LlmClient`/`LlmError` from `mur_common`; `ResearchNote`/`SearchProvider`/`NoopSearch` in `research.rs`; `author_skills_llm`/`draft_prompt_llm` in `llm.rs`; `build_wizard_draft`/`run_wizard` async signatures consistent across Tasks 3 & 5; `ChatBackendAdapter`/`build_llm_for_stage` in the shared adapter module.

## Review-captured deferrals (Plan 2 final review, 2026-06-15)

Plan 2 reviewed (APPROVED). Fixed inline: `strip_fences` now handles any-case language tags +
no-fence input (regression test added). Deferred to Plan 3:

- **Re-validate-after-repair graceful fallback.** `author_one` returns the repaired YAML without
  re-validating; if still invalid, `run_wizard` `bail!`s (plan-explicit) with a raw `{errs:?}`
  dump. Plan 3: re-validate the repair and, on failure, fall back to the deterministic stub for
  that one skill (graceful) and/or surface a readable error — not a hard abort of the whole wizard.
- **Hardcoded `model_ref: "claude_sonnet"`** (`cmd/agent/wizard.rs`) — CLAUDE.md Rule 1. The agent
  is created pinned to `claude_sonnet` even when inference uses a different configured model. Plan 3:
  derive the registry `model_ref` from the resolved model / config (or expose `--model-ref`), with a
  named default constant. Proper derivation (config model name → matching `models.yaml` ref) is
  non-trivial, hence deferred.
- **Surface research failures via `on_progress`** (minor UX): `run_wizard` swallows `SearchProvider`
  errors with `unwrap_or_default()`; emit a progress warning when a real provider fails.

## Roadmap note

- **Plan 2b (optional) — concrete search MCP:** implement a `McpSearch` `SearchProvider` that calls a configured search MCP (Tavily default) using MUR's MCP client, two-layer discovery→extract, returning `ResearchNote`s with citations. Until then `NoopSearch` keeps research as pure model-knowledge drafting (spec-compliant graceful behavior).
- Plans 3–5 unchanged: eval (rubric + auto-fix + AgentDojo/HarmBench), Hub Specialist flow, catalog content.
