# Agent Wizard — MUR Hub Specialist Flow (Plan 4 of 5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fold the specialized-agent wizard into the Hub's existing "create Agent" flow via a Step 0 fork (Companion / Specialist / Both), where the Specialist path researches+drafts skills/prompt/entitlements, lets the human review-and-edit them, then creates + evals — all driven by the shared `mur-core::agent_wizard` engine.

**Architecture:** The Hub does NOT use the CLI's monolithic `run_wizard` (its `review_gate` is a synchronous in-process hook). Instead it composes the engine's public pieces across async Tauri commands: `catalog::load_catalog` → `build_wizard_draft` (with an event-emitting `WizardHooks` impl) → store the `WizardDraft` in a `WizardSpecState` → frontend Review screen edits it → `apply::apply_draft` + optional `eval::run_eval` on approve. Progress streams via a `wizard-spec-progress` Tauri event, mirroring the existing `wizard_start_render` → `wizard-render-progress` pattern. The editable Review screen IS the human draft-review gate.

**Tech Stack:** Rust (Tauri 2, `mur-core` dep — the Hub crate already depends on it), React + TypeScript + Vitest (`mur-hub-gui/ui`). The `mur-hub-gui` crate is workspace-EXCLUDED — build it via `cd mur-hub-gui/src-tauri && cargo build`; the UI via `cd mur-hub-gui/ui && npm run build` / `npm test`.

**Builds on Plans 1-3** (branch `feat/agent-wizard`): `mur-core::agent_wizard` exposes `catalog::{RoleManifest, load_catalog}`, `build_wizard_draft(manifest, workspace, model_ref, &llm, &notes, &mut hooks) -> WizardDraft`, `WizardDraft`/`SkillDraft`/`PromptDraft`/`EntitlementPlan`/`Progress`/`Stage` (all `Serialize`/`Deserialize`), `apply::apply_draft(&WizardDraft) -> Result<WizardOutcome>`, `eval::{run_eval, DialDriver, EvalReport}`, `WizardHooks` trait (`on_progress`, `review_gate`).

**Testing reality:** CI builds the Hub crate + UI but cannot run the WebKit UI headlessly. Unit-test the pure pieces (catalog→DTO mapping, `EmitHooks`, frontend reducers/components via Vitest). The full click-through is **manually verified** (documented per task) — same as the existing wizard.

---

## Reference (verified)

- Existing wizard backend: `mur-hub-gui/src-tauri/src/onboarding/mod.rs` — `WizardState(Mutex<Option<WizardSession>>)`, `#[tauri::command]` fns, async `wizard_start_render` emits `app.emit("wizard-render-progress", &snap)`.
- Command registration: `mur-hub-gui/src-tauri/src/lib.rs` `invoke_handler(tauri::generate_handler![ ... onboarding::wizard_* ... ])`.
- Frontend orchestrator: `mur-hub-gui/ui/src/components/wizard/WizardModal.tsx` (renders `Step1Persona`…`Step6Render` by `displayStep`; `invoke("wizard_*")`; `STEP_LABEL_KEYS` + i18n via `t(key)`).
- Async-progress UI pattern to mirror: `Step6Render` listens for `wizard-render-progress` (see `listen(...)` usage in the wizard UI).

---

## File Structure

**Backend (`mur-hub-gui/src-tauri/src/onboarding/`):**
- Create `spec.rs` — `WizardSpecState`, DTOs (`RoleChoice`, `SpecDraftDto`), `EmitHooks`, and the `wizard_spec_*` commands. One responsibility: the Specialist backend.
- Modify `mod.rs` — `pub mod spec;` (or `mod spec; pub use spec::*;`).
- Modify `lib.rs` — `.manage(onboarding::spec::WizardSpecState::default())` and register the new commands in `generate_handler!`.

**Frontend (`mur-hub-gui/ui/src/components/wizard/`):**
- Create `steps/Step0Kind.tsx` — the fork (Companion / Specialist / Both).
- Create `steps/spec/SpecRole.tsx` — role picker (catalog + custom + risk).
- Create `steps/spec/SpecGenerating.tsx` — progress view (listens `wizard-spec-progress`).
- Create `steps/spec/SpecReview.tsx` — editable skills/prompt/entitlements + Approve.
- Create `steps/spec/SpecEval.tsx` — eval scores.
- Create `specFlow.ts` — pure state machine for the specialist branch (Vitest-tested).
- Modify `WizardModal.tsx` — render Step 0 first; branch into the specialist sub-flow.
- Modify `ui/src/i18n/en.ts` + `zh-TW.ts` — new step labels/strings.

---

## Task 1: Backend — role catalog command + DTOs

**Files:** Create `mur-hub-gui/src-tauri/src/onboarding/spec.rs`; modify `onboarding/mod.rs`, `lib.rs`
**Test:** inline `#[cfg(test)]` in `spec.rs`

- [ ] **Step 1: Write the failing test (catalog → DTO mapping)**

In `spec.rs`:

```rust
//! Specialist branch of the create-Agent wizard. Composes the mur-core agent_wizard engine.
use serde::{Deserialize, Serialize};

/// A role offered to the user (from the mur-core catalog).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoleChoice {
    pub id: String,
    pub display_name: String,
    pub charter: String,
    pub risk: String,     // "low" | "medium" | "high"
    pub category: String,
}

impl From<&mur_core::agent_wizard::catalog::RoleManifest> for RoleChoice {
    fn from(m: &mur_core::agent_wizard::catalog::RoleManifest) -> Self {
        RoleChoice {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
            charter: m.charter.clone(),
            risk: format!("{:?}", m.risk).to_lowercase(),
            category: m.category.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_manifest_maps_to_choice() {
        let m = mur_core::agent_wizard::catalog::RoleManifest {
            id: "pm".into(), display_name: "PM".into(), charter: "c".into(),
            risk: mur_core::agent_wizard::draft::RiskLevel::Low,
            skill_topics: vec![], category: "product".into(),
        };
        let c = RoleChoice::from(&m);
        assert_eq!(c.id, "pm");
        assert_eq!(c.risk, "low");
    }
}
```

> Verify the mur-core paths are public: `mur_core::agent_wizard::catalog::{RoleManifest, load_catalog}`, `mur_core::agent_wizard::draft::RiskLevel`. If `RoleManifest`'s fields aren't all `pub`, use `load_catalog` + getters instead — check `mur-core/src/agent_wizard/catalog.rs` and adapt; do not invent fields.

- [ ] **Step 2: Run, expect failure**

Run: `cd mur-hub-gui/src-tauri && cargo test spec::tests::role_manifest_maps 2>&1 | tail -15`
Expected: FAIL (module not declared / mapping missing).

- [ ] **Step 3: Wire module + the catalog command**

In `onboarding/mod.rs` add `pub mod spec;`. Append to `spec.rs`:

```rust
/// List role presets from the mur-core catalog (shipped + ~/.mur/wizard/roles).
#[tauri::command]
pub fn wizard_spec_catalog() -> Result<Vec<RoleChoice>, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| e.to_string())?;
    Ok(mur_core::agent_wizard::catalog::load_catalog(&home).iter().map(RoleChoice::from).collect())
}
```

> Confirm `mur_core::cmd::agent::resolve_mur_home` is `pub` (it's used by the CLI). If not exported from the lib crate, use the same home-resolution the existing `onboarding/mod.rs` uses (`mur_home_path()`).

- [ ] **Step 4: Register in lib.rs**

In `mur-hub-gui/src-tauri/src/lib.rs`, add `onboarding::spec::wizard_spec_catalog,` inside `generate_handler![ ... ]`.

- [ ] **Step 5: Run → PASS; build the crate**

Run: `cd mur-hub-gui/src-tauri && cargo test spec:: 2>&1 | tail -8` → PASS.
Run: `cd mur-hub-gui/src-tauri && cargo build 2>&1 | tail -5` → builds.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/onboarding/spec.rs mur-hub-gui/src-tauri/src/onboarding/mod.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub-wizard): wizard_spec_catalog command + RoleChoice DTO"
```

---

## Task 2: Backend — EmitHooks + generate command (async draft)

**Files:** `mur-hub-gui/src-tauri/src/onboarding/spec.rs`, `lib.rs`
**Test:** inline (`EmitHooks` capture without a real AppHandle — see note)

- [ ] **Step 1: Define `WizardSpecState` + `SpecDraftDto`**

In `spec.rs`:

```rust
use std::sync::Mutex;

/// Holds the in-progress draft between generate and approve (one session).
#[derive(Default)]
pub struct WizardSpecState(pub Mutex<Option<mur_core::agent_wizard::WizardDraft>>);

/// The draft as the Review screen needs it (editable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecDraftDto {
    pub name: String,
    pub display_name: String,
    pub skills: Vec<SkillDto>,
    pub prompt_markdown: String,
    pub entitlement_summary: Vec<String>, // human-readable lines
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDto { pub name: String, pub yaml: String }

impl SpecDraftDto {
    pub fn from_draft(d: &mur_core::agent_wizard::WizardDraft) -> Self {
        let e = &d.entitlements;
        SpecDraftDto {
            name: d.role.name.clone(),
            display_name: d.role.display_name.clone(),
            skills: d.skills.iter().map(|s| SkillDto { name: s.name.clone(), yaml: s.yaml.clone() }).collect(),
            prompt_markdown: d.prompt.markdown.clone(),
            entitlement_summary: vec![
                format!("write: {:?}", e.allow_write),
                format!("spawn: {:?}", e.allow_spawn),
                format!("hosts: {:?}", e.allow_host),
            ],
        }
    }
}
```

> `mur_core::agent_wizard::WizardDraft`/`SkillDraft`/`EntitlementPlan` field names are from Plan 1 — verify against `mur-core/src/agent_wizard/draft.rs` and adjust the field accesses to the real names.

- [ ] **Step 2: Write the failing test for `EmitHooks` (progress capture)**

`EmitHooks` implements `mur_core::agent_wizard::WizardHooks`. For testability, make it generic over a "sink" so the test can capture without a Tauri `AppHandle`:

```rust
use mur_core::agent_wizard::{Progress, WizardHooks};
use mur_core::agent_wizard::draft::WizardDraft as _Draft; // for review_gate signature

/// Emits each Progress through a sink closure. Real use: sink = app.emit.
pub struct EmitHooks<F: FnMut(&Progress)> { pub sink: F }
impl<F: FnMut(&Progress)> WizardHooks for EmitHooks<F> {
    fn on_progress(&mut self, p: &Progress) { (self.sink)(p); }
    // review_gate is unused in the Hub (the UI Review screen is the gate); accept the draft.
}

#[cfg(test)]
mod hook_tests {
    use super::*;
    #[test]
    fn emit_hooks_forwards_progress() {
        let mut seen = Vec::new();
        let mut h = EmitHooks { sink: |p: &Progress| seen.push(format!("{:?}", p.stage)) };
        h.on_progress(&Progress { stage: mur_core::agent_wizard::Stage::DefineRole, message: "x".into() });
        assert_eq!(seen.len(), 1);
    }
}
```

> Confirm `WizardHooks::review_gate` has a default impl (Plan 2/3 trimmed the trait to `on_progress` + `review_gate`, both defaulted). If `review_gate` is NOT defaulted, implement it here to return `Some(draft)`.

- [ ] **Step 3-4: Run (fail→pass)** `cd mur-hub-gui/src-tauri && cargo test spec::hook_tests 2>&1 | tail -8`.

- [ ] **Step 5: The async generate command**

```rust
use tauri::{AppHandle, Emitter, Manager, State};

/// Build the draft (research→skills→prompt→entitlements) async, emitting progress,
/// store it in WizardSpecState, and emit the final draft DTO.
#[tauri::command]
pub async fn wizard_spec_generate(app: AppHandle, role_id: String, no_llm: bool) -> Result<(), String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| e.to_string())?;
    let manifest = mur_core::agent_wizard::catalog::load_catalog(&home).into_iter()
        .find(|m| m.id == role_id)
        .ok_or_else(|| format!("unknown role {role_id}"))?;
    let workspace = home.display().to_string();
    let model_ref = mur_core::agent_wizard::DEFAULT_MODEL_REF.to_string();

    // LLM client unless no_llm (mirrors the CLI's build_chat_adapter path).
    let llm: Option<std::sync::Arc<dyn mur_core::agent_wizard::llm::WizardLlm>> = if no_llm {
        None
    } else {
        mur_core::conversations::backend::adapter::build_chat_adapter(&home, None, "agent.wizard")
            .ok().map(|a| std::sync::Arc::new(a) as _)
    };

    let app_emit = app.clone();
    let mut hooks = EmitHooks { sink: move |p: &Progress| { let _ = app_emit.emit("wizard-spec-progress", p); } };
    let draft = mur_core::agent_wizard::build_wizard_draft(&manifest, &workspace, &model_ref, &llm, &[], &mut hooks).await;

    let dto = SpecDraftDto::from_draft(&draft);
    if let Ok(mut g) = app.state::<WizardSpecState>().0.lock() { *g = Some(draft); }
    app.emit("wizard-spec-draft", &dto).map_err(|e| e.to_string())?;
    Ok(())
}
```

> Verify the exact `build_wizard_draft` signature (Plan 1-3) and the `build_chat_adapter` path (`mur_core::conversations::backend::adapter::build_chat_adapter`, made pub in Plan 2). Adapt the `Arc<dyn WizardLlm>` coercion exactly as the CLI does in `cmd/agent/wizard.rs`.

- [ ] **Step 6: Register + build + commit**

Register `wizard_spec_generate` in `lib.rs` `generate_handler!` and `.manage(WizardSpecState::default())`. Build the crate.

```bash
git add mur-hub-gui/src-tauri/src/onboarding/spec.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub-wizard): EmitHooks + async wizard_spec_generate (draft + progress events)"
```

---

## Task 3: Backend — approve (apply + optional eval) + cancel

**Files:** `mur-hub-gui/src-tauri/src/onboarding/spec.rs`, `lib.rs`

- [ ] **Step 1: Approve command (takes the edited draft)**

```rust
/// Apply the (possibly edited) draft: create+attach+start, then optional eval. Emits progress.
#[tauri::command]
pub async fn wizard_spec_approve(app: AppHandle, edited: SpecDraftDto, run_eval: bool) -> Result<String, String> {
    // Reconstruct the WizardDraft: start from the stored draft, overlay the user's edits.
    let mut draft = app.state::<WizardSpecState>().0.lock().ok()
        .and_then(|mut g| g.take())
        .ok_or("no draft in progress")?;
    // Overlay edits (skills yaml + prompt) from the Review screen.
    draft.prompt.markdown = edited.prompt_markdown;
    for s in &mut draft.skills {
        if let Some(e) = edited.skills.iter().find(|x| x.name == s.name) { s.yaml = e.yaml.clone(); }
    }
    // Validate edited skills before creating anything (the gate already happened in the UI).
    let errs = mur_core::agent_wizard::apply::validate_drafts(&draft);
    if !errs.is_empty() { return Err(format!("invalid edited skills: {errs:?}")); }

    let _ = app.emit("wizard-spec-progress",
        &mur_core::agent_wizard::Progress { stage: mur_core::agent_wizard::Stage::Create, message: "creating agent".into() });
    let outcome = mur_core::agent_wizard::apply::apply_draft(&draft).map_err(|e| e.to_string())?;

    if run_eval {
        let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| e.to_string())?;
        // Eval requires an LLM judge + running agent; reuse the CLI path's client.
        if let Ok(adapter) = mur_core::conversations::backend::adapter::build_chat_adapter(&home, None, "agent.wizard") {
            let judge = std::sync::Arc::new(adapter);
            let skill_names: Vec<String> = draft.skills.iter().map(|s| s.name.clone()).collect();
            let tasks = mur_core::agent_wizard::eval_tasks::tasks_for(&draft.role, &skill_names);
            let driver = mur_core::agent_wizard::eval::DialDriver { home: home.clone(), agent: draft.role.name.clone() };
            let report = mur_core::agent_wizard::eval::run_eval(&driver, judge.as_ref(), &tasks).await;
            let _ = app.emit("wizard-spec-eval", &report);
        }
    }
    Ok(outcome.agent_name)
}

#[tauri::command]
pub fn wizard_spec_cancel(state: State<'_, WizardSpecState>) { if let Ok(mut g) = state.0.lock() { *g = None; } }
```

> Confirm `eval_tasks`, `eval::{run_eval, DialDriver}`, `apply::{validate_drafts, apply_draft}` are `pub` (Plan 1-3). `EvalReport` derives `Serialize` (Plan 3) so `emit` works. `judge.as_ref()` must be `&dyn WizardLlm` — `ChatBackendAdapter: LlmClient` → `WizardLlm` via the blanket impl; coerce if needed.

- [ ] **Step 2: Register + build + commit**

Register both commands in `lib.rs`. Build.

```bash
git add mur-hub-gui/src-tauri/src/onboarding/spec.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub-wizard): wizard_spec_approve (apply + optional eval) + cancel"
```

---

## Task 4: Frontend — Step 0 fork + spec flow state machine

**Files:** Create `steps/Step0Kind.tsx`, `specFlow.ts`; modify `WizardModal.tsx`
**Test:** `specFlow.test.ts` (Vitest)

- [ ] **Step 1: Failing test for the pure flow reducer**

Create `mur-hub-gui/ui/src/components/wizard/specFlow.ts` and `specFlow.test.ts`:

```ts
// specFlow.ts
export type SpecStep = "role" | "generating" | "review" | "creating" | "eval" | "done";
export type SpecEvent =
  | { t: "start" } | { t: "draftReady" } | { t: "approve" }
  | { t: "created" } | { t: "evalDone" } | { t: "cancel" };

export function specReducer(s: SpecStep, e: SpecEvent): SpecStep {
  switch (e.t) {
    case "start": return "generating";
    case "draftReady": return "review";
    case "approve": return "creating";
    case "created": return "eval";
    case "evalDone": return "done";
    case "cancel": return "role";
    default: return s;
  }
}
```

```ts
// specFlow.test.ts
import { describe, it, expect } from "vitest";
import { specReducer } from "./specFlow";
describe("specReducer", () => {
  it("walks role→generating→review→creating→eval→done", () => {
    let s: any = "role";
    for (const e of [{t:"start"},{t:"draftReady"},{t:"approve"},{t:"created"},{t:"evalDone"}] as const)
      s = specReducer(s, e);
    expect(s).toBe("done");
  });
});
```

- [ ] **Step 2-4: Run (fail→pass)** `cd mur-hub-gui/ui && npx vitest run specFlow 2>&1 | tail -10`.

- [ ] **Step 5: Step 0 component**

Create `steps/Step0Kind.tsx`:

```tsx
import { t } from "../../../i18n/t";
export type AgentKind = "companion" | "specialist" | "both";
export function Step0Kind({ onPick }: { onPick: (k: AgentKind) => void }) {
  return (
    <div className="wizard-step0">
      <h2>{t("wizard.kind.title")}</h2>
      <div className="wizard-kind-grid">
        <button onClick={() => onPick("companion")}>🐾 {t("wizard.kind.companion")}</button>
        <button onClick={() => onPick("specialist")}>🛠️ {t("wizard.kind.specialist")}</button>
        <button onClick={() => onPick("both")}>✨ {t("wizard.kind.both")}</button>
      </div>
    </div>
  );
}
```

- [ ] **Step 6: Branch in WizardModal**

In `WizardModal.tsx`, add `const [kind, setKind] = useState<AgentKind | null>(null);`. Render `<Step0Kind onPick={...}>` when `kind === null`. When `kind === "companion"` render the existing flow unchanged. When `"specialist"`, render the specialist sub-flow (Task 5/6). `"both"` runs the specialist sub-flow then continues into the existing appearance steps (set `kind="companion"` after the specialist `done`).

- [ ] **Step 7: i18n + commit**

Add keys `wizard.kind.title/companion/specialist/both` to `ui/src/i18n/en.ts` + `zh-TW.ts`.

```bash
cd mur-hub-gui/ui && npx vitest run specFlow 2>&1 | tail -3
git add mur-hub-gui/ui/src/components/wizard/ mur-hub-gui/ui/src/i18n/
git commit -m "feat(hub-wizard): Step 0 fork (companion/specialist/both) + spec flow reducer"
```

---

## Task 5: Frontend — Role picker + Generating step

**Files:** Create `steps/spec/SpecRole.tsx`, `steps/spec/SpecGenerating.tsx`; modify `WizardModal.tsx`, i18n

- [ ] **Step 1: SpecRole (catalog picker)**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { t } from "../../../../i18n/t";

interface RoleChoice { id: string; display_name: string; charter: string; risk: string; category: string; }

export function SpecRole({ onStart }: { onStart: (roleId: string, noLlm: boolean) => void }) {
  const [roles, setRoles] = useState<RoleChoice[]>([]);
  const [sel, setSel] = useState<string>("");
  useEffect(() => { invoke<RoleChoice[]>("wizard_spec_catalog").then(setRoles).catch(() => setRoles([])); }, []);
  return (
    <div className="spec-role">
      <h2>{t("wizard.spec.role.title")}</h2>
      <ul>{roles.map((r) => (
        <li key={r.id}>
          <label><input type="radio" name="role" value={r.id} onChange={() => setSel(r.id)} />
            <b>{r.display_name}</b> <span className="risk">[{r.risk}]</span> — {r.charter}</label>
        </li>))}
      </ul>
      <button disabled={!sel} onClick={() => onStart(sel, false)}>{t("wizard.spec.role.generate")}</button>
    </div>
  );
}
```

- [ ] **Step 2: SpecGenerating (listen for progress)**

```tsx
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { t } from "../../../../i18n/t";

interface Progress { stage: string; message: string; }
export function SpecGenerating({ onDraft }: { onDraft: (dto: any) => void }) {
  const [lines, setLines] = useState<string[]>([]);
  useEffect(() => {
    const unp = listen<Progress>("wizard-spec-progress", (e) => setLines((l) => [...l, `${e.payload.stage}: ${e.payload.message}`]));
    const und = listen<any>("wizard-spec-draft", (e) => onDraft(e.payload));
    return () => { unp.then((f) => f()); und.then((f) => f()); };
  }, [onDraft]);
  return (<div className="spec-generating"><h2>{t("wizard.spec.generating.title")}</h2>
    <ul>{lines.map((l, i) => <li key={i}>{l}</li>)}</ul></div>);
}
```

- [ ] **Step 3: Wire into WizardModal specialist sub-flow + i18n keys**

Use `specReducer`: on SpecRole `onStart` → `invoke("wizard_spec_generate", { roleId, noLlm })` + dispatch `start`; on SpecGenerating `onDraft(dto)` → store dto + dispatch `draftReady`. Add the `wizard.spec.role.*` / `wizard.spec.generating.*` i18n keys (en + zh-TW).

- [ ] **Step 4: Build UI + commit**

```bash
cd mur-hub-gui/ui && npm run build 2>&1 | tail -5
git add mur-hub-gui/ui/src/components/wizard/ mur-hub-gui/ui/src/i18n/
git commit -m "feat(hub-wizard): specialist Role picker + Generating progress steps"
```

---

## Task 6: Frontend — editable Review (the gate) + Eval steps

**Files:** Create `steps/spec/SpecReview.tsx`, `steps/spec/SpecEval.tsx`; modify `WizardModal.tsx`, i18n

- [ ] **Step 1: SpecReview (editable skills/prompt + Approve)**

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { t } from "../../../../i18n/t";

export function SpecReview({ draft, onCreated }: { draft: any; onCreated: (name: string) => void }) {
  const [edited, setEdited] = useState(draft);
  const setSkill = (i: number, yaml: string) =>
    setEdited((d: any) => ({ ...d, skills: d.skills.map((s: any, j: number) => j === i ? { ...s, yaml } : s) }));
  const approve = async () => {
    const name = await invoke<string>("wizard_spec_approve", { edited, runEval: true });
    onCreated(name);
  };
  return (<div className="spec-review">
    <h2>{t("wizard.spec.review.title")}</h2>
    <p className="muted">{t("wizard.spec.review.hint")}</p>
    <h3>{t("wizard.spec.review.prompt")}</h3>
    <textarea value={edited.prompt_markdown} onChange={(e) => setEdited((d: any) => ({ ...d, prompt_markdown: e.target.value }))} rows={8} />
    <h3>{t("wizard.spec.review.skills")}</h3>
    {edited.skills.map((s: any, i: number) => (
      <details key={s.name}><summary>{s.name}</summary>
        <textarea value={s.yaml} onChange={(e) => setSkill(i, e.target.value)} rows={10} /></details>))}
    <h3>{t("wizard.spec.review.entitlements")}</h3>
    <ul>{edited.entitlement_summary.map((l: string, i: number) => <li key={i}><code>{l}</code></li>)}</ul>
    <button onClick={approve}>{t("wizard.spec.review.approve")}</button>
  </div>);
}
```

- [ ] **Step 2: SpecEval (listen for scores)**

```tsx
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { t } from "../../../../i18n/t";

export function SpecEval({ agentName }: { agentName: string }) {
  const [report, setReport] = useState<any | null>(null);
  useEffect(() => {
    const un = listen<any>("wizard-spec-eval", (e) => setReport(e.payload));
    return () => { un.then((f) => f()); };
  }, []);
  return (<div className="spec-eval"><h2>{t("wizard.spec.eval.title", { name: agentName })}</h2>
    {!report ? <p>{t("wizard.spec.eval.running")}</p> :
      <div><p>{report.passed ? "✅" : "⚠️"} {report.passed ? t("wizard.spec.eval.passed") : t("wizard.spec.eval.failed")}</p>
        <ul>{report.results.map((r: any) => <li key={r.task_id}>{r.task_id}: c{r.scores.correctness} h{r.scores.honesty} s{r.scores.uses_skills} {r.scores.safety_ok ? "🔒" : "❌"}</li>)}</ul></div>}
  </div>);
}
```

- [ ] **Step 3: Wire + i18n + build + commit**

Wire SpecReview `onCreated` → dispatch `created` + render SpecEval; SpecEval after `evalDone`/close → finish. Add `wizard.spec.review.*` and `wizard.spec.eval.*` i18n keys (en + zh-TW).

```bash
cd mur-hub-gui/ui && npm run build && npx vitest run 2>&1 | tail -5
git add mur-hub-gui/ui/src/components/wizard/ mur-hub-gui/ui/src/i18n/
git commit -m "feat(hub-wizard): editable Review (gate) + live Eval steps"
```

---

## Task 7: Build gate + manual E2E + final review

- [ ] **Step 1: Build both halves**

Run: `cd mur-hub-gui/src-tauri && cargo build 2>&1 | tail -5` (clean).
Run: `cd mur-hub-gui/ui && npm run build && npx vitest run 2>&1 | tail -8` (build + tests pass).

- [ ] **Step 2: Manual E2E (documented; WebKit UI can't run in CI)**

Launch the Hub (`./build.sh` per CLAUDE.md, or the Hub's dev command), open create-Agent:
1. Step 0 shows Companion / Specialist / Both.
2. Specialist → Role picker lists pm/qa/repomanager/rustsmith (+risk). Pick one → Generating shows progress lines.
3. Review screen shows editable prompt + skills + entitlement summary; edit a skill, click Approve.
4. Agent is created (appears in the Hub agent list); Eval shows scores (or a graceful message if no model).
5. Companion path is unchanged; Both runs specialist then the appearance steps.
Record the result in the PR description.

- [ ] **Step 3: Commit any fixes + push**

```bash
git add -A && git commit -m "chore(hub-wizard): build gate green for plan 4" && git push
```

---

## Self-Review

- **Spec coverage:** Step 0 fork (companion/specialist/both) ✓ (Task 4); Role (catalog+risk) ✓ (Task 5); Generating async + `wizard-spec-progress` ✓ (Tasks 2,5); editable Review = the gate ✓ (Task 6); Create + Eval live scores ✓ (Tasks 3,6); `wizard_spec_*` commands over the shared engine ✓ (Tasks 1-3); mirrors `wizard_start_render` event pattern ✓. Companion path untouched ✓ (Task 4).
- **Placeholder scan:** the several "verify the mur-core path/field names" notes are explicit verification steps (with the file to check + "don't invent") — required because the Hub consumes Plan 1-3 types whose exact field visibility must be confirmed at the boundary; not vague TODOs. UI styling/CSS is intentionally minimal (functional, not placeholder).
- **Type consistency:** `RoleChoice`/`SpecDraftDto`/`SkillDto`/`WizardSpecState`/`EmitHooks` (backend) and `SpecStep`/`specReducer`/`Step0Kind`/`SpecRole`/`SpecGenerating`/`SpecReview`/`SpecEval` (frontend) used consistently; event names `wizard-spec-progress`/`wizard-spec-draft`/`wizard-spec-eval` match between emit (Rust) and listen (TS).

## Review-captured follow-ups (Plan 4 final review, 2026-06-15)

Plan 4 reviewed (CHANGES REQUIRED → fixed). Fixed inline: `handleClose` now calls
`wizard_spec_cancel` on the specialist/both path (no stale backend draft); BACK from `eval` is a
no-op (terminal — agent already created, draft consumed); the "Both" hint no longer over-promises
companion-appearance steps. Tracked for follow-up:

- **"Both" continuation:** today "Both" runs the specialist flow only; wire it to continue into the
  existing companion appearance steps after eval. Hint relabeled honestly until then.
- **Hub-side skill validation timing:** the Hub validates edited skills inside `wizard_spec_approve`
  (non-fatal warn; the human can fix YAML in the Review textarea) rather than before the gate as the
  CLI does. Acceptable (stub/LLM skills are pre-validated); revisit if invalid hand-edits get common.
- **`apply.rs` cosmetic:** `cmd_create` gets a hardcoded `"anthropic/claude-sonnet-4-6"` then is
  immediately `set_model_ref`'d — harmless but could pass `draft.model_ref` directly.

## Roadmap note

- Plan 5: seed the full role catalog content (DevOps/SRE, Security reviewer, Tech writer, Data/ML, Frontend, Support-triage) — the Hub Role picker surfaces them automatically via `load_catalog`.
- Deferred (from Plans 2-3): real auto-fix mutation loop; concrete search-MCP provider (Plan 2b); `grade_skill_usage` short-name threshold.
- Hub polish (follow-up): a custom-role text-entry path in SpecRole (needs the LLM author stage; today the picker lists manifest-backed roles only).
