# Official Free Orchestrator Implementation Plan

> **執行者必讀：** REQUIRED EXECUTION SKILL: use `mur-executing-plans` for sequential execution, or `mur-delegate-dev` only after freezing the interfaces below. This plan spans three repositories and must be executed in task order.

**Goal:** Publish `agents/orchestrator` as a free, portable official Agent whose installer asks for a model policy, deterministically selects an existing local registry chain, and whose runtime falls back only across the approved safe failure boundary.

**Architecture:** `mur-agent/3` adds signed `model_requirements`; new MUR builds accept v2 and v3 while old builds reject v3, so requirements can never be silently ignored. A pure planner ranks `~/.mur/models.yaml`; CLI and Hub own prompts/previews while the shared installer revalidates and atomically configures the installed profile. Runtime error typing is narrowed independently, then `official-sign` publishes a catalog-driven portable package only after the compatible MUR release exists.

**Tech stack:** Rust 2024, `serde`/`serde_yaml_ng`, Clap, Dialoguer, Tauri 2, React 18, TypeScript, Vitest, Go, Next.js, GitHub Actions, Ed25519/DSSE `.muragent` packages.

**Approved spec:** `docs/superpowers/specs/2026-09-17-official-orchestrator-model-selection-design.md`

## Global Constraints

These requirements are copied from the approved spec and apply to every task:

- The base official orchestrator is a **free** catalog item.
- A published version is immutable. Any later content or tier change requires a version bump.
- The package leaves both `model_ref` and `fallback_chain` unset; installation resolves and writes them for this machine.
- The package declares minimum requirements such as chat and tools. It does **not** encode a preferred vendor, a model registry key, or a global routing policy.
- Installers that do not understand the requirements metadata must fail clearly rather than installing an unusable profile.
- Unknown prices are unknown, never zero.
- Privacy-first uses the canonical billing inference and writes per-agent `routing.enabled: false` plus `smart.enabled: false` so global routing cannot escape the local chain.
- Requirements-bearing packages carry no `model_hint` and bypass the ordinary pull/API-key model wizard; v2/no-requirements packages preserve that wizard.
- Provider defaults and 4:1 projected cost have one authority in `mur-common`; planner, Smart auto-pick, router, and export classification may not diverge.
- The installer never writes `models.yaml`.
- The complete selected profile update is one atomic operation.
- A profile is never left with only some refs updated.
- Authentication failures, permission denial, billing or credit failure, safety-policy rejection, malformed requests, invalid responses, and unknown 4xx responses stop immediately; HTTP 404 and provider-tested model-not-found/context patterns still advance.
- Runtime taxonomy changes apply fleet-wide to every fallback client, not only orchestrator.
- Once any answer content has been emitted to the caller, the existing `committed > 0` guard stops the chain and must be preserved.
- Other official agents and all official fleets retain their current install behavior unless they opt into model requirements.
- One dispatch publishes one item.
- Recovery may regenerate and commit the index but never overwrite an existing release asset.
- Source files ≤ 800 lines. No hardcoded model/vendor preference in the orchestrator package. MUR is uppercase in user-visible prose; the executable remains `mur`.
- Use `cargo nextest`, never bare `cargo test`. Commands for `mur-core` set `MUR_WEB_DIST`.

## Release gates

1. Tasks 1–7 land and release in `mur` before the orchestrator artifact is published.
2. Task 5 lands in `mur-server` before Hub/catalog preview is announced.
3. Tasks 8–9 land in `official-sign` only after the minimum compatible MUR version is known.
4. The first real signing run remains manual and is not part of implementation automation.

## File structure

### Repository: `/Volumes/Firecuda4tb/Projects/mur`

| File | Responsibility |
|---|---|
| `mur-common/src/muragent/manifest.rs` | `mur-agent/3`, `ModelRequirements`, and schema compatibility rules. |
| `mur-common/src/muragent/writer.rs` | Build v2 manifests by default and v3 manifests when signed requirements exist. |
| `mur-common/src/muragent/validator.rs` | Accept v2/v3 in new MUR; reject malformed v3 requirements and requirements plus `model_hint`. |
| `mur-common/src/model.rs` | Shared provider billing/tier/locality and 4:1 projected-cost helpers used by planner and Smart. |
| `mur-common/src/muragent/model_class.rs` | Consume shared provider classification instead of a private locality table. |
| `mur-core/src/route/mod.rs` | Consume `ModelEntry::effective_route_tier()` instead of a private provider table. |
| `mur-core/src/official/model_selection.rs` (new) | Pure eligibility, warnings, total ordering, explicit-ref validation. |
| `mur-core/src/official/mod.rs` | Export planner API. |
| `mur-core/src/official/client.rs` | Deserialize catalog `model_requirements` and `min_mur_version`; perform early compatibility UX gate. |
| `mur-core/src/official/install.rs` | Options/outcome API, requirement revalidation, wizard suppression, privacy overrides, install rollback, atomic profile rewrite. |
| `mur-core/src/cmd/official.rs` | CLI policy prompt, preview, confirmation, and output. |
| `mur-core/src/cli/actions.rs` | `--model-policy`, `--model-ref`, repeated `--fallback`. |
| `mur-core/src/cli/mod.rs` | CLI parse regression tests. |
| `mur-core/src/dispatch.rs` | Pass install flags to the official command. |
| `mur-hub-gui/src-tauri/src/official_catalog.rs` | Plan/install DTOs and Tauri commands. |
| `mur-hub-gui/src-tauri/src/lib.rs` | Register `official_plan_models`. |
| `mur-hub-gui/ui/src/components/wizard/steps/spec/SpecOfficial.tsx` | Two-stage policy, preview, confirmation flow. |
| `mur-hub-gui/ui/src/components/wizard/steps/spec/SpecOfficial.test.tsx` (new) | Hub interaction and request-shape regression tests. |
| `mur-hub-gui/ui/src/i18n/en.ts` | English policy, warnings, and no-candidate copy. |
| `mur-hub-gui/ui/src/i18n/zh-TW.ts` | Traditional-Chinese policy, warnings, and no-candidate copy. |
| `mur-agent-runtime/src/llm/mod.rs` | Common typed failures and failover disposition. |
| `mur-agent-runtime/src/llm/openai/mod.rs` | Structured OpenAI-compatible error mapping. |
| `mur-agent-runtime/src/llm/anthropic.rs` | Structured Anthropic error mapping. |
| `mur-agent-runtime/src/llm/fallback/mod.rs` | Exhausted-chain reporting; preserve no-switch-after-output invariant. |

### Repository: `/Volumes/Firecuda4tb/Projects/mur-server`

| File | Responsibility |
|---|---|
| `internal/services/officialcatalog/catalog.go` | Preserve `model_requirements` from `index.json`. |
| `internal/services/officialcatalog/catalog_test.go` | Index parsing regression. |
| `internal/api/handlers/official_catalog.go` | Return requirements in list/detail responses. |
| `internal/api/handlers/official_catalog_test.go` | API pass-through regression. |
| `internal/api/handlers/registry.go` | Replace blanket Pro-plan error copy with per-item-safe wording. |
| `dashboard/src/lib/library.ts` | Remove misleading category tier; preserve item tier as authority. |
| `dashboard/src/lib/library.test.ts` (new) | Free/pro item mapping tests. |

### Repository: `/Volumes/Firecuda4tb/Projects/official-sign`

| File | Responsibility |
|---|---|
| `catalog.yaml` | Free orchestrator entry and signed model requirements. |
| `agents/orchestrator/profile.yaml` (new) | Portable least-privilege Agent profile. |
| `agents/orchestrator/prompt.md` (new) | Official delegation charter and safety boundaries. |
| `tools/official-sign/src/catalog.rs` | Full-catalog validation and optional requirements. |
| `tools/official-sign/src/portability.rs` (new) | Structural profile portability validator. |
| `tools/official-sign/src/sign.rs` | Stamp v3 signed requirements into the manifest. |
| `tools/official-sign/src/index.rs` | Publish requirements into `index.json`. |
| `tools/official-sign/src/lib.rs` | Export portability module. |
| `tools/official-sign/src/main.rs` | `--all` dry-run and catalog-derived source path. |
| `tools/official-sign/tests/end_to_end.rs` | Multi-item, requirements, and portability tests. |
| `.github/workflows/publish.yml` | Catalog-driven PR dry-run and selected-item manual publish. |
| `README.md` | Generic local dry-run/publish commands. |
| `docs/FIRST_PUBLISH.md` | `item_id` dispatch and recovery runbook. |

---

## Task 1: Add fail-loud signed model requirements (`mur`)

**Interfaces**

- Produces:
  - `mur_common::muragent::manifest::ModelRequirements { chat: bool, tools: bool, minimum_context_window: Option<u64> }`
  - `MuragentManifest::model_requirements: Option<ModelRequirements>`
  - requirements imply `model_hint.is_none()`
  - `MuragentManifest::requires_v3() -> bool`
  - `MuragentManifest::schema_supported() -> bool`, accepting exactly `mur-agent/2` and `mur-agent/3`.
  - `build_manifest_from_profile_with_requirements(profile, mur_version, Option<ModelRequirements>)`.
- Consumes: existing `MuragentManifest`, writer, validator, DSSE canonical signing path.

- [x] **Step 1: Write failing manifest/validator tests.** Add tests proving: v2 without requirements validates; v3 with `{chat:true, tools:true}` round-trips and validates; v2 carrying requirements is rejected; v3 with no requirements is rejected; v3 requirements plus non-empty `model_hint` is rejected; an unknown schema is rejected. Add a signing test that mutating `model_requirements.tools` after signing fails validation.
- [x] **Step 2: Prove red.**

```bash
cargo nextest run -p mur-common -E 'test(/model_requirements/) or test(/schema_v3/)'
```

Expected: compile failures for the missing field/type, then test failures until schema validation exists.

- [x] **Step 3: Implement the exact schema contract.** In `manifest.rs` add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRequirements {
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_context_window: Option<u64>,
}
```

Add `model_requirements` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. `schema_supported()` accepts v2/v3; validation enforces the biconditional `schema == "mur-agent/3"` iff `model_requirements.is_some()` and rejects v3 requirements with `model_hint`. Keep `build_manifest_from_profile()` producing v2 for every existing caller; the new builder emits v3 only when requirements are supplied. Do not put requirements in `deployment`, because old v2 readers deliberately ignore it.

- [x] **Step 4: Run focused and package tests.**

```bash
cargo nextest run -p mur-common -E 'test(/muragent/)'
```

Expected: all tests pass; existing v2 fixture tests remain green.

- [x] **Step 5: Commit.**

```bash
git add mur-common/src/muragent/{manifest.rs,writer.rs,validator.rs}
git commit -m "feat(muragent): sign install-time model requirements"
```

---

## Task 2: Implement the deterministic model planner (`mur`)

**Interfaces**

- Consumes: Task 1 `ModelRequirements`, `ModelRegistry`, `RouteTier`.
- Produces shared `mur-common` authorities before the planner:
  - `inferred_billing_for_provider(provider: &str) -> BillingMode`
  - `ModelEntry::effective_route_tier() -> RouteTier`
  - `ModelEntry::projected_cost(input_tokens: u64, output_tokens: u64) -> Option<f64>`
  - migrated `pick_cheap_model`, `mur-core::route`, and export model classification callers.
- Produces:

```rust
pub const MAX_FALLBACKS: usize = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSelectionPolicy { CapabilityFirst, CostFirst, PrivacyFirst }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelSelectionWarning {
    UnverifiedToolCapability { model_ref: String },
    UnknownContextWindow { model_ref: String },
    UnknownPrice { model_ref: String },
    StalePrice { model_ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSelectionPlan {
    pub primary: String,
    pub fallbacks: Vec<String>,
    pub warnings: Vec<ModelSelectionWarning>,
}

pub fn plan_model_selection(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    policy: ModelSelectionPolicy,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<ModelSelectionPlan>;

pub fn validate_explicit_selection(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    primary: &str,
    fallbacks: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<ModelSelectionPlan>;
```

- [x] **Step 1: Pin current provider inference and cost behavior with failing/characterization tests.** Cover every alias currently present in `mur-core/src/route/mod.rs` and `mur-common/src/muragent/model_class.rs`, billing inference for ollama/claude/codex/unknown, legacy one-sided costs, unknown costs, and prove `pick_cheap_model` ranks by the 4,000-input/1,000-output projection rather than output-only price.
- [x] **Step 2: Extract shared helpers in `mur-common/src/model.rs`.** Make `billing_or_inferred`, `pick_cheap_model`, `mur-core::route`, and `muragent::model_class` consume them; delete private `LOCAL_PROVIDERS` tables only after characterization tests pass.
- [x] **Step 3: Create `model_selection.rs` with table-driven failing tests** for every planner bullet in the spec: tier, context, local tie-break, billing classes, known/unknown cost, privacy exclusion, explicit missing tools, legacy empty capabilities warning, minimum context exclusion, catalog verification, stable key order, deduplication, and `MAX_FALLBACKS`.
- [x] **Step 4: Prove red.**

```bash
cargo nextest run -p mur-common -E 'test(/(provider_inference|projected_cost|pick_cheap_model)/)'
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-core -E 'test(model_selection)'
```

Expected: planner tests fail because ranking is not implemented.

- [x] **Step 5: Implement one eligibility pass and one total comparator per policy.** Provider/model must be non-empty. `capabilities.is_empty()` is legacy unknown; otherwise required names must be present. A hard context minimum excludes `None` and too-small values. Cost-first orders `Local < Subscription < UsageBilled`; inside usage-billed compare `entry.projected_cost(4_000, 1_000)`, with `None` after every known value. Capability-first orders `Frontier` before `Local`, then known/larger context, then local billing. Every comparator ends with catalog verification, explicit tools, and registry key ascending. Sort once, choose the first as primary, and take at most three distinct remaining refs.
- [x] **Step 6: Mutation-check unknown price and shared cost drift.** Temporarily map `None` cost to `0.0`; confirm `unknown_cost_is_not_free` fails; temporarily restore output-only ranking in `pick_cheap_model` and confirm its 4:1 parity test fails; restore and rerun.
- [x] **Step 7: Run all focused tests and Clippy.**

```bash
cargo nextest run -p mur-common -E 'test(/(model|model_class)/)'
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-core -E 'test(model_selection) or test(route)'
MUR_WEB_DIST=/tmp/mur-web-dist cargo clippy -p mur-common -p mur-core --all-targets -- -D warnings
```

- [x] **Step 8: Commit.**

```bash
git add mur-common/src/{model.rs,muragent/model_class.rs} mur-core/src/route/mod.rs mur-core/src/official/{mod.rs,model_selection.rs}
git commit -m "feat(official): deterministically plan agent model chains"
```

---

## Task 3: Make official installation option-aware and transactional (`mur`)

**Interfaces**

- Consumes: Tasks 1–2, `cmd::agent::save_profile`, existing official license/signature gates.
- Produces:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialInstallOptions { pub model_selection: ModelSelection }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelSelection {
    Automatic(ModelSelectionPolicy),
    Explicit { primary: String, fallbacks: Vec<String> },
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOutcome {
    pub item_id: String,
    pub agent_name: Option<String>,
    pub model_selection: Option<ModelSelectionPlan>,
}

pub async fn install_item(id: &str, options: &OfficialInstallOptions) -> Result<InstallOutcome>;
```

Keep a compatibility wrapper `install_item_unchanged(id)` only until all in-tree callers migrate in Task 6; delete it before Task 7 finishes.

- [ ] **Step 1: Add failing tests** using a temporary `MUR_HOME` and signed fixture packages. Prove: v2/no-requirements + `Unchanged` preserves behavior; selection flags on no-requirement packages fail; requirement packages reject `Unchanged`; no candidate creates no agent; explicit refs are checked; global `models.yaml` bytes are unchanged; primary is written to `model_ref` and legacy inline `model`; fallback refs are written atomically; privacy-first writes `RoutingOverride { enabled: Some(false), .. }` and `SmartOverride { enabled: Some(false), .. }`; global routing/smart enabled cannot alter its runtime candidates; requirements-bearing imports never call `maybe_resolve_model`, while v2 imports still do; a registry ref removed immediately before commit rolls back a fresh install and restores an update directory byte-for-byte.
- [ ] **Step 2: Prove red.**

```bash
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-core -E 'test(official_install)'
```

- [ ] **Step 3: Add an explicit ordinary-import model-resolution mode.** Thread `ResolveModelAfterInstall::{ExistingWizard, Suppress}` (or repository-equivalent enum) through `cmd_install`; preserve `ExistingWizard` for every current caller. Official v3 requirements installs use `Suppress`; v2/no-requirements official installs keep `ExistingWizard`. Do not infer suppression from item name.
- [ ] **Step 4: Split download/verification from mutation inside `install.rs`.** After license verification, read `MuragentArchive`, validate its signed manifest, derive/revalidate the plan from `<mur_home>/models.yaml`, and only then call the ordinary importer. Do not trust catalog requirements as installation authority.
- [ ] **Step 5: Add rollback guard.** Before import, snapshot an existing same-name Agent directory into a temp directory; record whether it was absent. On any post-import error, remove a newly-created directory or restore the snapshot. Disarm the guard only after the profile save succeeds. License persistence may remain; the approved spec allows that. Existing package collision checks remain authoritative.
- [ ] **Step 6: Re-read `models.yaml` immediately before profile commit**, validate every selected ref again, set `profile.model_ref`, `profile.fallback_chain`, and inline `profile.model` from the primary registry entry. For `PrivacyFirst`, also set per-agent routing and Smart `enabled: Some(false)` overrides; other policies preserve package overrides. Serialize once and call `cmd::agent::save_profile`, which performs temp-file + rename. Never call the printing `cmd_agent_set_fallback` path.
- [ ] **Step 7: Run focused tests and mutation-check rollback.** Force the post-import revalidation test hook to return an unknown ref; verify both fresh and update rollback tests fail if the guard is disabled, then restore it.
- [x] **Step 8: Commit.**

```bash
git add mur-core/src/official/install.rs mur-core/src/cmd/agent/{mod.rs,install.rs}
git commit -m "feat(official): configure model chains transactionally"
```

---

## Task 4: Add CLI flags, policy prompt, preview, and non-TTY default (`mur`)

**Interfaces**

- Consumes: Task 3 options/outcome and Task 2 planner.
- Produces CLI syntax:
  - `mur official install ID --model-policy capability-first|cost-first|privacy-first`
  - `mur official install ID --model-ref REF --fallback REF...`

- [ ] **Step 1: Add Clap parse tests.** Prove all three policies parse; repeated fallback preserves order; policy conflicts with model-ref; fallback without model-ref fails. Keep the existing fleet install parse test.
- [ ] **Step 2: Add pure command-decision tests.** Given `has_requirements` and `is_tty`, prove: interactive/no flags asks; non-TTY/no flags chooses capability-first; explicit flags bypass prompting; flags against fleets or no-requirement Agents fail rather than being ignored; catalog `min_mur_version` above `env!("CARGO_PKG_VERSION")` fails before download with an upgrade-MUR message.
- [ ] **Step 3: Prove red.**

```bash
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-core -E 'test(/official.*(cli|install|policy)/)'
```

- [ ] **Step 4: Implement.** Use `std::io::IsTerminal` on both stdin and stdout and `dialoguer::Select` with labels `Capability first`, `Cost first`, `Privacy first`. Render primary, numbered fallbacks, and warnings; ask `Confirm installation?` before calling `install_item`. In non-TTY mode print that capability-first was selected by default. Do not put prompting or printing in `official::install`.
- [ ] **Step 5: Run CLI tests and help snapshot/manual assertion.**

```bash
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-core -E 'test(/official/)'
MUR_WEB_DIST=/tmp/mur-web-dist cargo run -p mur-core -- official install --help | grep -E -- '--model-policy|--model-ref|--fallback'
```

Expected: all three flags appear.

- [ ] **Step 6: Commit.**

```bash
git add mur-core/src/{cli/actions.rs,cli/mod.rs,dispatch.rs,cmd/official.rs,official/client.rs}
git commit -m "feat(cli): choose official agent model policy at install"
```

---

## Task 5: Carry requirements through the server and make item tier authoritative (`mur-server`)

**Interfaces**

- Consumes JSON from official-sign Task 8:

```json
{"model_requirements":{"chat":true,"tools":true,"minimum_context_window":null}}
```

- Produces the same optional field plus optional `min_mur_version` from catalog list/detail endpoints. Existing entries without either remain valid.

- [ ] **Step 1: Add Go tests** to parse requirements from an index fixture and assert list/detail preserve it and `min_mur_version`. Add handler tests that blanket registry Pro copy is gone. Add dashboard tests proving one free Agent and one pro Agent retain their own tiers and no category tier overrides them.
- [ ] **Step 2: Prove red.**

```bash
go test ./internal/services/officialcatalog ./internal/api/handlers
cd dashboard && npm test -- --run src/lib/library.test.ts
```

- [ ] **Step 3: Add typed optional `ModelRequirements` and `MinMurVersion` fields** in `catalog.go` and the API response; do not use a second stringly-typed parser. In `library.ts`, remove `tier` from `LIBRARY_TYPES` metadata because no caller uses it and it falsely states all official Agents are pro. Preserve `loadLibrary()` item mapping as the only badge authority. Replace `registry.go`'s blanket "Official agents, fleets and workflows require a Pro plan" with item-safe authentication/entitlement wording.
- [ ] **Step 4: Run tests and dashboard build.**

```bash
go test ./internal/services/officialcatalog ./internal/api/handlers
cd dashboard && npm test -- --run src/lib/library.test.ts && npm run build
```

- [ ] **Step 5: Commit in `mur-server`.**

```bash
git add internal/services/officialcatalog/{catalog.go,catalog_test.go} \
  internal/api/handlers/{official_catalog.go,official_catalog_test.go} \
  dashboard/src/lib/{library.ts,library.test.ts} internal/api/handlers/registry.go
git commit -m "feat(catalog): expose model requirements per official item"
```

---

## Task 6: Add the Hub two-stage policy and preview flow (`mur`)

**Interfaces**

- Consumes: Task 2 planner, Task 3 installer, Task 5 catalog field.
- Produces Tauri commands:

```rust
#[tauri::command]
pub async fn official_plan_models(id: String, policy: ModelSelectionPolicy)
    -> Result<ModelSelectionPlan, String>;

#[tauri::command]
pub async fn official_install(
    id: String,
    model_selection: ModelSelection,
) -> Result<InstallOutcomeView, String>;
```

The backend replans/revalidates on install; the preview is never an authority.

- [x] **Step 1: Add Rust DTO/command tests** proving plan and install deserialize kebab-case policy values and return identical plans for the same registry. Add React tests with mocked `invoke`: an item requiring a newer MUR shows an upgrade error and cannot preview/install; selecting a compatible item with requirements opens policy UI; choosing privacy calls `official_plan_models`; preview renders primary/fallback/warnings; confirm calls `official_install`; no-requirement items retain one-click install; no-candidate errors remain on the policy stage.
- [x] **Step 2: Prove red.**

```bash
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-hub-gui -E 'test(official)'
cd mur-hub-gui/ui && npm test -- SpecOfficial.test.tsx
```

- [x] **Step 3: Implement the state machine** `select-item -> select-policy/preview -> confirm/install`. The item DTO includes optional requirements. Add localized labels/descriptions for all policies and warnings in both locale files. Never sort models in TypeScript.
- [x] **Step 4: Register the command and remove the temporary compatibility installer wrapper** after all callers use options.
- [x] **Step 5: Run tests, lint, and build.**

```bash
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-hub-gui -E 'test(official)'
cd mur-hub-gui/ui && npm test -- SpecOfficial.test.tsx && npm run lint && npm run build
```

- [x] **Step 6: Commit.**

```bash
git add mur-hub-gui/src-tauri/src/{official_catalog.rs,lib.rs} \
  mur-hub-gui/ui/src/components/wizard/steps/spec/{SpecOfficial.tsx,SpecOfficial.test.tsx} \
  mur-hub-gui/ui/src/i18n/{en.ts,zh-TW.ts} mur-core/src/official/install.rs
git commit -m "feat(hub): preview official agent model selection"
```

---

## Task 7: Narrow fleet-wide runtime fallback without regressing #947 (`mur`)

**Scope:** This changes every Agent using `FallbackLlmClient`, not only the
orchestrator. Preserve candidate-specific fallback for HTTP 404 and tested
provider model-not-found/context conditions; unknown 4xx become Stop.

**Interfaces**

- Produces common errors:

```rust
ContextExceeded(String),
PermissionDenied(u16, String),
SafetyPolicyRejected(String),
Rejected(u16, String),
```

- Produces provider seams:
  - `map_openai_error(status: u16, body: &str) -> LlmError`
  - `map_anthropic_error(status: u16, body: &str) -> LlmError`
- Dispositions:
  - `AdvanceNow`: HTTP 404/`ModelNotFound`, provider-tested unavailable codes,
    `ContextExceeded`.
  - `RetryThenAdvance`: `RateLimit`, `Timeout`, `Connect`, `ServerError`.
  - `Stop`: `Auth`, `InsufficientCredit`, `PermissionDenied`,
    `SafetyPolicyRejected`, unknown `Rejected`, `Http`, `InvalidResponse`.

- [x] **Step 1: Freeze the #947 and streaming invariants before changing defaults.** Keep tests proving HTTP 404/model rename advances, factory failure remains in the failure list, and `committed > 0` prevents switching. Add a scope comment that the classifier is fleet-wide.
- [x] **Step 2: Add the complete failing matrix.** OpenAI-compatible fixtures: structured `context_length_exceeded`, model-not-found/unavailable, `insufficient_quota`, and `content_policy_violation`. Anthropic fixtures: an exact current 400 `invalid_request_error` prompt-too-long message shape, a near-miss prompt message, spend-limit `invalid_request_error`, `permission_error`, and safety refusal. Assert unknown 400/409/413/422 stop; 413 alone is not context overflow. Add `claude` and `codex` tests proving setup/loopback `Http` failures stop while delegated HTTP gateway fixtures retain Anthropic/OpenAI mapping.
- [x] **Step 3: Prove red.**

```bash
cargo nextest run -p mur-agent-runtime -E 'test(/(from_status|fallback|context|permission|safety|anthropic_error|openai_error)/)'
```

- [x] **Step 4: Implement provider mapping before generic status mapping.** Parse provider JSON `type`/`code` first. OpenAI-compatible mapping uses enumerated codes. Anthropic mapping may use only anchored, versioned message patterns local to `anthropic.rs` for prompt overflow because `invalid_request_error` is ambiguous; every allowed pattern has a positive and near-miss fixture. Never use a generic cross-provider substring classifier. Change generic unknown 4xx to `Rejected` + `Stop`; classify 402 credit as `Stop`; retain HTTP 404 as `ModelNotFound` + `AdvanceNow`.
- [x] **Step 5: Preserve existing streaming and diagnostics.** Do not redesign `committed > 0`; keep it returning immediately. `all_candidates_failed` continues listing each ref/reason and chooses the highest-actionability source using `Stop > AdvanceNow > RetryThenAdvance`.
- [x] **Step 6: Mutation checks.** Temporarily classify `InsufficientCredit` and unknown `Rejected` as `AdvanceNow`; confirm tests fail. Remove one Anthropic anchor; confirm its positive fixture fails. Broaden it to `contains("too long")`; confirm a near-miss/spend-limit fixture fails. Temporarily remove `committed > 0`; confirm the existing streaming test fails. Restore all.
- [x] **Step 7: Run runtime suite and Clippy.**

```bash
cargo nextest run -p mur-agent-runtime -E 'test(/llm/) or test(/fallback/)'
cargo clippy -p mur-agent-runtime --all-targets -- -D warnings
```

- [x] **Step 8: Commit.**

```bash
git add mur-agent-runtime/src/llm/{mod.rs,anthropic.rs,openai/mod.rs,fallback/mod.rs,claude.rs,codex.rs}
git commit -m "fix(runtime): bound fleet-wide fallback by typed failures"
```

---

## Task 8: Make official-sign catalog-driven and validate portability (`official-sign`)

**Interfaces**

- Consumes Task 1 `ModelRequirements` and v3 writer.
- Produces:

```rust
pub fn load_catalog(path: &Path) -> Result<Vec<CatalogEntry>>;
pub fn validate_catalog(entries: &[CatalogEntry], repo_root: &Path) -> Result<()>;
pub fn validate_profile_portability(profile_path: &Path) -> Result<()>;
```

`CatalogEntry` gets `model_requirements: Option<ModelRequirements>` and `min_mur_version: Option<String>`; `IndexItem` carries both fields.

- [x] **Step 1: Add failing tests** for duplicate id and duplicate `(kind,name,version)`, missing source dir, profile/catalog version mismatch, missing/invalid `min_mur_version` on requirement items, absolute path, owner/user identity, `model_ref`, non-empty fallback chain, credential/secret, machine socket, and a valid portable profile. Extend end-to-end tests to build researcher v2 and orchestrator v3 from one catalog.
- [x] **Step 2: Prove red.**

```bash
cargo nextest run --manifest-path tools/official-sign/Cargo.toml
```

- [x] **Step 3: Implement structural portability validation for v3 requirement items.** Deserialize `AgentProfile`; for catalog entries carrying `model_requirements`, recursively inspect YAML values for credential/secret keys and absolute paths, and explicitly reject `model_ref`, non-empty fallback chain, owner/user fields, and enabled Unix socket binds. Preserve the existing v2 build and validation path unchanged for catalog entries without requirements, including already-published packages such as researcher 1.0.1. Separately inspect the generated v3 `MuragentManifest` and reject requirements with non-empty `model_hint`. Error messages include `file:field`, never secret values.
- [x] **Step 4: Make the signer derive source path** as `<kind>s/<name>` from the selected catalog entry. Add `--all` mutually exclusive with `--id`; `--all` validates and builds every entry. Pass catalog requirements into `build_manifest_from_profile_with_requirements`; verify manifest requirements equal catalog requirements, `model_hint` is absent, profile version equals catalog version, and requirement entries declare a semver `min_mur_version`. Carry both requirements and minimum version into `index.json`.
- [x] **Step 5: Rewrite workflow inputs and scripts.** `workflow_dispatch.inputs.item_id` is required. PR runs `official-sign --all` with the throwaway key. Publish resolves name/version/kind/source/bundle/tag from `catalog.yaml` through an `official-sign --describe ITEM --format github-output` command, not `awk`. Every later step consumes those outputs. Release creation refuses an existing tag; recover skips release upload; commit message uses selected name/version.
- [x] **Step 6: Run tests and locally inspect generated artifacts.**

```bash
cargo nextest run --manifest-path tools/official-sign/Cargo.toml
head -c 32 /dev/urandom > /tmp/official-test.key
FP="$(cargo run --manifest-path tools/official-sign/Cargo.toml -- --print-fp /tmp/official-test.key)"
ORT_STRATEGY=download cargo run --manifest-path tools/official-sign/Cargo.toml -- \
  --all --catalog catalog.yaml --out-dir /tmp/official-out \
  --key /tmp/official-test.key --expect-fp "$FP"
```

Expected after Task 9 content exists: both `researcher-1.0.1.muragent` and `orchestrator-1.0.0.muragent`, plus both index entries.

- [x] **Step 7: Commit infrastructure separately.**

```bash
git add tools/official-sign .github/workflows/publish.yml README.md docs/FIRST_PUBLISH.md
git commit -m "feat(publish): build official items from the catalog"
```

---

## Task 9: Add the portable free orchestrator package (`official-sign`)

**Interfaces**

- Catalog identity: `agents/orchestrator`, `agent`, `orchestrator`, `1.0.0`, `free`.
- Signed requirements: `chat: true`, `tools: true`, no hard minimum context window.

- [x] **Step 1: Add the catalog entry exactly as approved.** Include:

```yaml
  - id: agents/orchestrator
    kind: agent
    name: orchestrator
    version: 1.0.0
    tier: free
    description: "Official multi-agent task orchestrator with safe model fallback."
    min_mur_version: "2.85.0"
    model_requirements:
      chat: true
      tools: true
```


- [x] **Step 2: Create `profile.yaml`.** Use a stable UUIDv7, version `1.0.0`, no `model_ref`, empty fallback chain, no routing/Smart preference, and a syntactically valid neutral inline model placeholder only where `AgentProfile` requires it; the Task 3 installer must overwrite it before commit. Disable Unix socket transport. Grant only A2A task discovery/status/message capabilities required by the actual orchestrator tools; network/filesystem/process entitlements remain deny-by-default unless a test demonstrates a direct orchestrator need.
- [x] **Step 3: Create `prompt.md`.** State that it decomposes work, delegates to eligible Agents/Fleets, observes progress, reports evidence, never claims delegated work succeeded without results, never expands its own permissions through a worker, and stops for auth/permission/safety/billing failures rather than routing around them. Do not mention a model vendor or local path.
- [x] **Step 4: Run portability and build tests.**

```bash
cargo nextest run --manifest-path tools/official-sign/Cargo.toml
head -c 32 /dev/urandom > /tmp/official-test.key
FP="$(cargo run --manifest-path tools/official-sign/Cargo.toml -- --print-fp /tmp/official-test.key)"
ORT_STRATEGY=download cargo run --manifest-path tools/official-sign/Cargo.toml -- \
  --id agents/orchestrator --catalog catalog.yaml --out-dir /tmp/orchestrator-out \
  --key /tmp/official-test.key --expect-fp "$FP"
```

Expected: signed v3 package and index entry with `tier: free`, requirements, and `min_mur_version`; no portability findings.

- [x] **Step 5: Inspect package contents with the real reader test**, not `tar` assumptions. Add an end-to-end assertion that profile/prompt contain no `/Users/`, `/Volumes/`, `/tmp/`, `model_ref`, `fallback_chain`, credential, Claude, OpenAI, or Ollama preference; separately assert the generated manifest has no `model_hint`.
- [x] **Step 6: Commit content.**

```bash
git add catalog.yaml agents/orchestrator tools/official-sign/tests/end_to_end.rs
git commit -m "feat(catalog): add the free official orchestrator"
```

---

## Task 10: Cross-repository verification and release handoff

**Interfaces**

- Consumes every earlier task.
- Produces evidence only; no real official signing key is used and no release is published.

- [ ] **Step 1: Run the `mur` gates.**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
MUR_WEB_DIST=/tmp/mur-web-dist cargo nextest run -p mur-common -p mur-core -p mur-agent-runtime
MUR_WEB_DIST=/tmp/mur-web-dist cargo clippy -p mur-common -p mur-core -p mur-agent-runtime --all-targets -- -D warnings
cd mur-hub-gui/ui && npm test && npm run lint && npm run build
```

Expected: zero failures and warnings promoted to errors remain zero.

- [ ] **Step 2: Run `mur-server` gates.**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server
go test ./internal/services/officialcatalog ./internal/api/handlers
cd dashboard && npm test && npm run build
```

- [x] **Step 3: Run official dry-run.** Use Task 8's throwaway-key `--all` command and verify `index.json` contains researcher and orchestrator with distinct item tiers.
- [ ] **Step 4: Exercise four temporary MUR homes** with global Smart and difficulty-routing enabled, using fixture model registries: mixed local/cloud, cloud-only, local-only, and no eligible model. Assert capability/cost/privacy produce the documented chains; privacy candidates remain local at runtime despite globals; no-candidate leaves no `agents/orchestrator`; each successful profile references only existing registry keys; original `models.yaml` SHA-256 is unchanged. Also assert a v3 install emits no ordinary pull/API-key wizard prompt while researcher v2 preserves it.
- [x] **Step 5: Verify backward failure.** Run the pre-Task-1 released `mur` binary against the v3 orchestrator fixture and capture `schema version mismatch`; it must not install. With the new client and an artificially high catalog `min_mur_version`, capture the actionable upgrade-MUR error before download. Run the new binary against researcher v2 and confirm unchanged installation.
- [ ] **Step 6: Write the release handoff** to `~/.mur/artifacts/mur/official-orchestrator-20260917/verification.md`, containing command, exit code, and concise output tail for every gate. This is a run artifact, not a project file.
- [ ] **Step 7: Stop before real publication.** The human release sequence is: release compatible MUR → deploy `mur-server` → merge `official-sign` → manually dispatch `publish` with `item_id=agents/orchestrator` → inspect release asset and index diff → announce. Do not invoke the protected signing environment from this plan.

## Final self-review checklist

- [ ] Spec coverage: every acceptance criterion maps to Tasks 1–10.
- [ ] Placeholder scan returns empty:

```bash
python3 - <<'PY'
from pathlib import Path
p = Path('docs/superpowers/plans/2026-09-17-official-orchestrator-implementation.md')
terms = ['T'+'BD', 'TO'+'DO', 'FIX'+'ME', 'similar to '+'Task', 'appropriate error '+'handling']
found = [term for term in terms if term in p.read_text()]
assert not found, found
PY
```

- [ ] Interface consistency: `ModelRequirements`, `ModelSelectionPolicy`, `ModelSelectionPlan`, `OfficialInstallOptions`, and `InstallOutcome` have one canonical definition each.
- [ ] Old-reader behavior is fail-loud because v3 is a schema bump, not an ignored optional v2 field.
- [ ] Front ends preview; core replans and revalidates.
- [ ] Installer never writes global model settings; privacy-first disables per-agent Smart/routing inheritance.
- [ ] Requirements-bearing installs bypass the ordinary model wizard; v2 installs preserve it.
- [ ] Shared provider and projected-cost helpers are the only authorities used by planner, Smart, router, and export classification.
- [ ] Runtime never routes around auth, permission, safety, or billing failures; 404/tested unavailable/context failures still advance fleet-wide.
- [ ] Official package carries no host-specific values, vendor preference, or `model_hint`; index carries `min_mur_version`.
- [ ] Dashboard badges come from item tier only.
- [ ] Real signing remains a deliberate manual action.
