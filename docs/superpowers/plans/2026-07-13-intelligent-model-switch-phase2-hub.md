# Intelligent Model Switching — Phase 2 (Hub GUI Management) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Manage the Phase-1 model-switching settings (global default + fallback chain + difficulty routing, and per-agent fallback chains) from the MUR Hub GUI — not only the CLI.

**Architecture:** Thin Tauri commands in `mur-hub-gui/src-tauri` wrap the Phase-1 `mur_core::store::config` load/save + ref-validation (headless-unit-testable via the `*_impl(home, …)` split that `notif.rs` already uses). The React `ModelsSettings` panel and the per-agent model picker call those commands. mur-hub-gui is a workspace-EXCLUDED Tauri app, so the Rust side is tested with `cargo test` against its own manifest, and the UI is verified by a Hub `.app` rebuild + visual QA (it cannot be unit-tested the way Phase 1 was).

**Tech Stack:** Rust (Tauri 2, `#[tauri::command]`, serde), React + TypeScript (Vite, `@tauri-apps/api`), vitest (pure-helper tests only).

## Global Constraints

- **Phase 1 is shipped and merged** (PR #692). This phase ADDS a GUI over it — it must NOT change Phase-1 behavior, config schema, or CLI. It reuses `mur_common::config::ModelSwitchConfig`/`RoutingConfig` and `mur_core::store::config::{load_config, save_config_at, config_path}` verbatim.
- **Priority per-agent → global** everywhere (already enforced by Phase-1 `resolve_model_refs`); the UI only surfaces it (per-agent settings shown as overriding global; empty = inherit).
- **Fail-closed ref validation:** every model_ref written (global default, global/per-agent fallback chain, routing cheap/frontier) must exist in `<home>/models.yaml` or the command returns `Err` and persists nothing — same rule as the Phase-1 CLI.
- **No hardcoded values:** routing threshold / retry values remain the Phase-1 config fields + `DEFAULT_*` consts.
- **Brand name "MUR" uppercase** in any user-facing GUI string.
- **Two sub-phases:** **2a** = Rust Tauri commands (Tasks 1–2, headless-unit-testable). **2b** = React UI (Tasks 3–4) + Hub build & visual QA (Task 5). 2b depends on 2a.
- Rust edition 2024; comments/strings English (except where the app is i18n'd — use the existing `useT()`/`TranslationKey` mechanism, do not hardcode display copy that the codebase localizes); files ≤ 800 lines.
- **Build/test env:** the Tauri crate builds against its own manifest. Rust tests: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml <name>`. UI helper tests: `cd mur-hub-gui/ui && npm test`. The full Hub `.app` build (Task 5) is `cd mur-hub-gui/ui && npm run build` then `cd mur-hub-gui && npx tauri build` (heavy; see the memory `gotcha_hub_local_app_build_recipe`).

## File Structure

- `mur-hub-gui/src-tauri/src/model_switch.rs` (new) — `model_switch_get`/`model_switch_set` + `agent_set_fallback_cmd` Tauri commands, each split into a testable `*_impl(home, …)` + a thin `#[tauri::command]` wrapper (mirrors `notif.rs`). (Tasks 1–2)
- `mur-hub-gui/src-tauri/src/lib.rs` — register the new commands in `invoke_handler![]`; add `mod model_switch;`. (Tasks 1–2)
- `mur-hub-gui/ui/src/components/settings/modelSwitch.ts` (new) — pure TS helpers (types + a `sanitizeChain`/validation helper) with vitest tests. (Task 3)
- `mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx` — add the global model-switch section. (Task 3)
- `mur-hub-gui/ui/src/components/…` per-agent picker (agent detail) — add the per-agent fallback editor. (Task 4)

---

## Phase 2a — Tauri commands (headless-testable)

### Task 1: `model_switch_get` / `model_switch_set` commands

**Files:**
- Create: `mur-hub-gui/src-tauri/src/model_switch.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub(crate) fn model_switch_get_impl(home: &Path) -> Result<ModelSwitchConfig, String>`
  - `pub(crate) fn model_switch_set_impl(home: &Path, next: ModelSwitchConfig) -> Result<ModelSwitchConfig, String>`
  - `#[tauri::command] pub fn model_switch_get() -> Result<ModelSwitchConfig, String>`
  - `#[tauri::command] pub fn model_switch_set(next: ModelSwitchConfig) -> Result<ModelSwitchConfig, String>`
- Consumes: `mur_common::config::{Config, ModelSwitchConfig}`, `mur_core::store::config::save_config_at`, `mur_common::model::ModelRegistry`, `crate::mur_home_path()`.

> **Design note (full-object set, not a patch):** the spec sketched `model_switch_set(patch)`, but a settings panel always holds the complete desired state and a patch over nested `retry`/`routing` needs awkward `Option<Option<String>>` to express "clear default". Sending the full `ModelSwitchConfig` and replacing `Config.models` wholesale is simpler and unambiguous. `ModelSwitchConfig` already derives `Serialize`/`Deserialize`, so it crosses the Tauri boundary as-is.

- [ ] **Step 1: Write the failing test** (in `model_switch.rs`, mirroring `notif.rs`'s `#[cfg(test)]` + `TempDir` harness)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::{ModelSwitchConfig, RoutingConfig};
    use mur_common::model::{ModelEntry, ModelRegistry};
    use tempfile::TempDir;

    fn seed_home() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let mut reg = ModelRegistry::default();
        for k in ["claude_sonnet", "deepseek_v4_pro"] {
            reg.models.insert(k.into(), ModelEntry {
                provider: "anthropic".into(), model: k.into(), ..Default::default()
            });
        }
        reg.save_to(&home.join("models.yaml")).unwrap();
        (dir, home)
    }

    #[test]
    fn get_returns_defaults_then_set_roundtrips() {
        let (_d, home) = seed_home();
        // Fresh home → default (empty) model-switch config.
        let got = model_switch_get_impl(&home).unwrap();
        assert_eq!(got.default, None);
        assert!(got.fallback_chain.is_empty());

        // Set a valid config → persisted + returned.
        let mut next = ModelSwitchConfig::default();
        next.default = Some("claude_sonnet".into());
        next.fallback_chain = vec!["claude_sonnet".into(), "deepseek_v4_pro".into()];
        let saved = model_switch_set_impl(&home, next.clone()).unwrap();
        assert_eq!(saved.default.as_deref(), Some("claude_sonnet"));
        // Reloading from disk reflects it.
        let reloaded = model_switch_get_impl(&home).unwrap();
        assert_eq!(reloaded.fallback_chain, vec!["claude_sonnet", "deepseek_v4_pro"]);
    }

    #[test]
    fn set_rejects_unknown_ref_and_persists_nothing() {
        let (_d, home) = seed_home();
        let mut bad = ModelSwitchConfig::default();
        bad.default = Some("does_not_exist".into());
        assert!(model_switch_set_impl(&home, bad).is_err());
        // Nothing persisted.
        assert_eq!(model_switch_get_impl(&home).unwrap().default, None);
    }

    #[test]
    fn set_validates_routing_and_chain_refs() {
        let (_d, home) = seed_home();
        let mut cfg = ModelSwitchConfig::default();
        cfg.routing = RoutingConfig {
            enabled: true,
            cheap: Some("claude_sonnet".into()),
            frontier: Some("nope".into()), // unknown → reject
            threshold_input_tokens: Some(1000),
        };
        assert!(model_switch_set_impl(&home, cfg).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml model_switch`
Expected: FAIL to compile (module/fns missing).

- [ ] **Step 3: Implement `model_switch.rs`**

```rust
//! Tauri commands to manage the global model-switching config (config.yaml
//! `models:` block) from the Hub. Wraps Phase-1 mur_core config load/save +
//! the same fail-closed ref validation the CLI uses.
use std::path::Path;

use mur_common::config::{Config, ModelSwitchConfig};
use mur_common::model::ModelRegistry;

/// Every model_ref referenced by the config must exist in `<home>/models.yaml`.
fn validate_refs(home: &Path, cfg: &ModelSwitchConfig) -> Result<(), String> {
    let reg = ModelRegistry::load_from(&home.join("models.yaml"))
        .map_err(|e| format!("load models.yaml: {e}"))?;
    let mut refs: Vec<&String> = Vec::new();
    if let Some(d) = &cfg.default { refs.push(d); }
    refs.extend(cfg.fallback_chain.iter());
    if let Some(c) = &cfg.routing.cheap { refs.push(c); }
    if let Some(f) = &cfg.routing.frontier { refs.push(f); }
    for r in refs {
        if !reg.models.contains_key(r) {
            return Err(format!("model_ref {r:?} not in models.yaml"));
        }
    }
    Ok(())
}

pub(crate) fn model_switch_get_impl(home: &Path) -> Result<ModelSwitchConfig, String> {
    Ok(Config::load_or_default(&home.join("config.yaml")).models)
}

pub(crate) fn model_switch_set_impl(
    home: &Path,
    next: ModelSwitchConfig,
) -> Result<ModelSwitchConfig, String> {
    validate_refs(home, &next)?;
    let mut cfg = Config::load_or_default(&home.join("config.yaml"));
    cfg.models = next;
    mur_core::store::config::save_config_at(&home.join("config.yaml"), &cfg)
        .map_err(|e| format!("save config: {e}"))?;
    Ok(cfg.models)
}

#[tauri::command]
pub fn model_switch_get() -> Result<ModelSwitchConfig, String> {
    model_switch_get_impl(&crate::mur_home_path())
}

#[tauri::command]
pub fn model_switch_set(next: ModelSwitchConfig) -> Result<ModelSwitchConfig, String> {
    model_switch_set_impl(&crate::mur_home_path(), next)
}
```

In `lib.rs`: add `mod model_switch;` near the other `mod` declarations, and add `model_switch::model_switch_get, model_switch::model_switch_set,` to the `tauri::generate_handler![…]` list (~line 578-690).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml model_switch` then `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (confirms the `generate_handler!` registration compiles — a bad command signature fails here).
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/model_switch.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): model_switch_get/set Tauri commands (global model-switch config, ref-validated)"
```

---

### Task 2: `agent_set_fallback` command (per-agent)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/model_switch.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `mur_core::cmd::agent::model_resolve::cmd_agent_set_fallback(home, name, refs)` (Phase-1, already validates refs fail-closed + writes `profile.fallback_chain`), and `mur_core::…` profile read for the getter.
- Produces:
  - `#[tauri::command] pub fn agent_get_fallback(name: String) -> Result<Vec<String>, String>`
  - `#[tauri::command] pub fn agent_set_fallback(name: String, refs: Vec<String>) -> Result<Vec<String>, String>`
  - testable `*_impl(home, …)` variants.

- [ ] **Step 1: Write the failing test** (append to `model_switch.rs` tests)

```rust
    #[test]
    fn agent_fallback_get_set_roundtrips_and_validates() {
        use std::fs;
        let (_d, home) = seed_home();
        // Seed a minimal agent profile at <home>/agents/coach/profile.yaml.
        let agent_dir = home.join("agents").join("coach");
        fs::create_dir_all(&agent_dir).unwrap();
        let p = mur_common::AgentProfile::default_for_tests();
        fs::write(agent_dir.join("profile.yaml"),
                  serde_yaml_ng::to_string(&p).unwrap()).unwrap();

        // Empty initially.
        assert!(agent_get_fallback_impl(&home, "coach").unwrap().is_empty());
        // Unknown ref rejected, nothing written.
        assert!(agent_set_fallback_impl(&home, "coach", &["nope".into()]).is_err());
        // Valid ref persists.
        agent_set_fallback_impl(&home, "coach", &["claude_sonnet".into()]).unwrap();
        assert_eq!(agent_get_fallback_impl(&home, "coach").unwrap(),
                   vec!["claude_sonnet".to_string()]);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml agent_fallback`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** (append to `model_switch.rs`)

```rust
pub(crate) fn agent_get_fallback_impl(home: &Path, name: &str) -> Result<Vec<String>, String> {
    let path = home.join("agents").join(name).join("profile.yaml");
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?,
    ).map_err(|e| format!("parse profile: {e}"))?;
    Ok(profile.fallback_chain)
}

pub(crate) fn agent_set_fallback_impl(home: &Path, name: &str, refs: &[String]) -> Result<Vec<String>, String> {
    mur_core::cmd::agent::model_resolve::cmd_agent_set_fallback(home, name, refs)
        .map_err(|e| format!("{e}"))?;
    agent_get_fallback_impl(home, name)
}

#[tauri::command]
pub fn agent_get_fallback(name: String) -> Result<Vec<String>, String> {
    agent_get_fallback_impl(&crate::mur_home_path(), &name)
}

#[tauri::command]
pub fn agent_set_fallback(name: String, refs: Vec<String>) -> Result<Vec<String>, String> {
    agent_set_fallback_impl(&crate::mur_home_path(), &name, &refs)
}
```

**Interface note:** confirm `cmd_agent_set_fallback` is `pub` and reachable at `mur_core::cmd::agent::model_resolve::cmd_agent_set_fallback` (Phase 1 made it `pub fn`). If the module path differs, grep `pub fn cmd_agent_set_fallback` and use the actual path. Register all four commands (`agent_get_fallback`, `agent_set_fallback` here + the two from Task 1) in `lib.rs` `generate_handler!`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml agent_fallback` then `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/model_switch.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): agent_get/set_fallback Tauri commands (per-agent fallback chain)"
```

---

## Phase 2b — React UI (verified via Hub build + visual QA)

> **Verification reality:** these tasks change React components. The repo has vitest for **pure helper** modules (e.g. `modelPicker.test.ts`), so extract logic into a testable `.ts` helper and unit-test THAT; the component wiring itself is verified by the Hub `.app` rebuild + visual QA in Task 5. Do not claim a component renders correctly from a unit test — say what was unit-tested (helpers) and defer render verification to Task 5.

### Task 3: Global model-switch section in Settings → Models

**Files:**
- Create: `mur-hub-gui/ui/src/components/settings/modelSwitch.ts`
- Create: `mur-hub-gui/ui/src/components/settings/modelSwitch.test.ts`
- Modify: `mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx`

**Interfaces:**
- Consumes (Tauri): `invoke<ModelSwitchConfig>("model_switch_get")`, `invoke<ModelSwitchConfig>("model_switch_set", { next })`; `invoke<ModelOption[]>("list_models")` (existing) for the combobox options.
- Produces: `modelSwitch.ts` types + helpers used by the panel.

- [ ] **Step 1: Write the failing helper test** (`modelSwitch.test.ts`)

```ts
import { describe, it, expect } from "vitest";
import { type ModelSwitchView, sanitizeChain, isChainValid } from "./modelSwitch";

describe("modelSwitch helpers", () => {
  it("sanitizeChain drops blanks and de-dupes preserving order", () => {
    expect(sanitizeChain(["a", "", "b", "a", "  "])).toEqual(["a", "b"]);
  });
  it("isChainValid requires every ref to be a known model id", () => {
    const known = new Set(["a", "b"]);
    expect(isChainValid(["a", "b"], known)).toBe(true);
    expect(isChainValid(["a", "x"], known)).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd mur-hub-gui/ui && npm test -- modelSwitch`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement `modelSwitch.ts`**

```ts
// Types + pure helpers for the global model-switch settings section.
export interface RoutingView {
  enabled: boolean;
  cheap: string | null;
  frontier: string | null;
  threshold_input_tokens: number | null;
}
export interface RetryView {
  max_retries: number;
  backoff_base_ms: number;
  cooldown_secs: number;
}
// Mirrors mur_common::config::ModelSwitchConfig over the Tauri boundary.
export interface ModelSwitchView {
  default: string | null;
  fallback_chain: string[];
  retry: RetryView;
  routing: RoutingView;
}

/** Drop blank/whitespace refs and de-duplicate, preserving first-seen order. */
export function sanitizeChain(chain: string[]): string[] {
  const out: string[] = [];
  for (const raw of chain) {
    const r = raw.trim();
    if (r && !out.includes(r)) out.push(r);
  }
  return out;
}

/** Every ref must be a known model id (fail-closed mirror of the Rust guard). */
export function isChainValid(chain: string[], known: Set<string>): boolean {
  return chain.every((r) => known.has(r));
}
```

- [ ] **Step 4: Run to verify the helper test passes**

Run: `cd mur-hub-gui/ui && npm test -- modelSwitch`
Expected: PASS.

- [ ] **Step 5: Wire the section into `ModelsSettings.tsx`**

Add a "Model switching" section to the existing `ModelsSettings` component. Load on mount and save on change through the new commands. Reuse the existing `ModelCombobox` (from `../ModelCombobox`) for the Default / cheap / frontier pickers, populated from the existing `list_models` invoke that the file already uses for slots. Concretely:

```tsx
// near the other useState hooks in ModelsSettings():
const [ms, setMs] = useState<ModelSwitchView | null>(null);
const [msErr, setMsErr] = useState<string | null>(null);

// in the refresh callback (alongside the existing invokes):
invoke<ModelSwitchView>("model_switch_get").then(setMs).catch((e) => setMsErr(String(e)));

// a save helper:
const saveMs = useCallback((next: ModelSwitchView) => {
  invoke<ModelSwitchView>("model_switch_set", { next })
    .then((saved) => { setMs(saved); setMsErr(null); })
    .catch((e) => setMsErr(String(e)));
}, []);
```

Render (inside the component's JSX, a new section — use the file's existing class/i18n conventions, `useT()` for labels):
- **Default model**: a `ModelCombobox` bound to `ms.default`; on change → `saveMs({ ...ms, default: value })`.
- **Fallback chain**: an ordered list of `ModelCombobox` rows with add/remove/reorder (↑/↓) buttons; on any change → `saveMs({ ...ms, fallback_chain: sanitizeChain(rows) })`.
- **Difficulty routing**: an enable checkbox bound to `ms.routing.enabled`; when enabled, cheap/frontier `ModelCombobox`es + a numeric threshold input; on change → `saveMs({ ...ms, routing: { ...ms.routing, … } })`.
- Show `msErr` inline (e.g. an unknown-ref rejection from the backend).

Keep the JSX under the 800-line file limit; if `ModelsSettings.tsx` would exceed it, extract the new section into `ModelSwitchSection.tsx` and render `<ModelSwitchSection />`.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/settings/modelSwitch.ts \
        mur-hub-gui/ui/src/components/settings/modelSwitch.test.ts \
        mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx
git commit -m "feat(hub-ui): global model-switch section in Settings (default/chain/routing)"
```

---

### Task 4: Per-agent fallback editor in agent detail

**Files:**
- Modify: the agent-detail model area (`mur-hub-gui/ui/src/components/ModelPickerModal.tsx` and/or the agent detail panel that renders the per-agent model picker — grep for where `apply_agent_model` / `set_concierge_model_ref` are invoked to find the exact component).

**Interfaces:**
- Consumes (Tauri): `invoke<string[]>("agent_get_fallback", { name })`, `invoke<string[]>("agent_set_fallback", { name, refs })`; `sanitizeChain` from `settings/modelSwitch.ts`.

- [ ] **Step 1: Locate the per-agent model picker**

Run: `grep -rn "apply_agent_model\|set_concierge_model_ref\|ModelPickerModal" mur-hub-gui/ui/src` to find the component that edits a single agent's model. That component (or its container) is where the fallback editor goes, directly below the primary-model picker.

- [ ] **Step 2: Add the per-agent fallback editor**

Below the existing primary-model picker for the selected agent, render a "Fallback chain (overrides global)" editor — the same ordered-list-of-`ModelCombobox` control as Task 3's global chain, but bound to the agent:

```tsx
const [agentChain, setAgentChain] = useState<string[]>([]);
useEffect(() => {
  invoke<string[]>("agent_get_fallback", { name: agentName })
    .then(setAgentChain).catch(() => setAgentChain([]));
}, [agentName]);

const saveAgentChain = (rows: string[]) => {
  const refs = sanitizeChain(rows);
  invoke<string[]>("agent_set_fallback", { name: agentName, refs })
    .then(setAgentChain).catch((e) => setErr(String(e)));
};
```

- When `agentChain` is empty, show a greyed inherited hint ("Inherits global fallback chain") so the per-agent → global priority is visible.
- Reuse the same add/remove/reorder row control as Task 3 (extract it into a shared `FallbackChainEditor.tsx` if it would duplicate meaningfully — DRY across Tasks 3 and 4).

- [ ] **Step 3: Build the UI bundle to confirm it compiles**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: the Vite build succeeds (TypeScript type-checks the new props/invokes). This is the compile gate for 2b; visual verification is Task 5.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/
git commit -m "feat(hub-ui): per-agent fallback chain editor in agent detail (inherits global)"
```

---

### Task 5: Hub `.app` rebuild + visual QA (controller/operator gate)

**Not a code task.** The UI tasks are verified by building the Hub and driving it. The controller (or operator) does:

- [ ] **Build the Hub app** per the memory `gotcha_hub_local_app_build_recipe`: `cd mur-hub-gui/ui && npm run build` then `cd mur-hub-gui && npx tauri build` (native build to dodge ggml; ad-hoc sign before install). This is heavy (~20 min) and is NOT part of CI for this crate.
- [ ] **Visual QA — global (Settings → Models):** the new section shows Default / Fallback chain / Difficulty routing. Setting a default persists (reopen Settings → it's still there; `~/.mur/config.yaml` `models.default` is set). Adding two fallback refs + reordering persists to `models.fallback_chain`. Enabling routing + picking cheap/frontier + threshold persists. Entering/selecting a ref not in the library surfaces the backend's rejection inline (fail-closed).
- [ ] **Visual QA — per-agent:** an agent's detail shows a fallback editor; empty shows the "inherits global" hint; setting a chain writes `profile.fallback_chain` (verify via `~/.mur/agents/<name>/profile.yaml`); it takes precedence over the global chain (per Phase-1 `resolve_model_refs`).
- [ ] **Regression:** the existing Model Library / slots / brain-badge in the same Settings panel still work unchanged.

If any persist/validate step fails, that's a defect → fix in the relevant 2a/2b task before considering Phase 2 done.

---

## Self-Review

**Spec coverage** (spec §"Hub GUI Management"):
- Global: Default combobox → Task 3; Fallback chain editor → Task 3; Difficulty routing toggle+cheap/frontier+threshold → Task 3; `model_switch_get/set` Tauri commands wrapping `store::config` → Task 1; ref validation fail-closed → Task 1. ✓
- Per-agent: fallback editor + routing override in agent detail → Task 4; `agent_set_fallback` command → Task 2; empty inherits global (greyed) → Task 4. ✓  *(Deviation: the spec listed a per-agent **routing override** too; this plan ships per-agent **fallback chain** in the GUI and leaves the per-agent routing-override control to a follow-up — the data model supports it (`AgentProfile.routing`), but a per-agent routing UI is lower value than the chain and would bloat Task 4. Flagged for the user.)*
- Phasing (2a testable / 2b build) → matches the spec's "Phase 2 is a separate implementation plan" note. ✓

**Deviations flagged (decide during execution):**
1. `model_switch_set` takes the full `ModelSwitchConfig`, not a `patch` (rationale in Task 1 design note).
2. Per-agent **routing** override UI is deferred (per-agent fallback chain ships; per-agent routing stays CLI/YAML-only for now).

**Placeholder scan:** Task 4 Step 1 is an explicit "locate the component via grep" instruction (the exact file isn't knowable without the grep, and the plan names the grep + what to look for) — not a hand-wave. No TBD/TODO. Task 5 is explicitly a controller/operator gate, not a code placeholder.

**Type consistency:** `ModelSwitchConfig` (Rust) ↔ `ModelSwitchView` (TS) fields match: `default: Option<String>`↔`string|null`, `fallback_chain: Vec<String>`↔`string[]`, `retry`/`routing` nested. `model_switch_get/set` and `agent_get/set_fallback` signatures identical across Task 1/2 (def) and Task 3/4 (invoke). `sanitizeChain` defined in Task 3, reused in Task 4.
