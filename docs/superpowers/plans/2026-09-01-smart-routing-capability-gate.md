# Implementation Plan — Smart Routing Capability Gate + Inheritable Policy

> **Execute with**: `mur-executing-plans` (in-context, task by task). Each task
> ends green and committed; stop and report on any blocker rather than
> improvising.
> **Spec**: `docs/superpowers/specs/2026-09-01-smart-routing-capability-gate-design.md`

**Goal**: automatic model substitution may never hand a request to a model that
cannot serve it, and Smart background routing becomes opt-in with real
three-state per-agent inheritance.

**Architecture**: a pure capability predicate in `mur-common`
(`Requirement`/`satisfies`) filters every *automatic* candidate inside
`FallbackLlmClient::candidates_for`, leaving explicit user choices untouched.
Separate `SmartOverride`/`RoutingOverride` partial types replace the
whole-struct per-agent override, merged field-by-field onto the global config,
with the effective values read through two `AgentProfile` helpers so every
call site resolves inheritance identically.

**Tech stack**: Rust 2024 (workspace crates `mur-common`, `mur-agent-runtime`,
`mur-core`), Tauri 2 + React/TypeScript (`mur-hub-gui`, workspace-excluded),
`cargo nextest` for tests, `vitest` for the Hub UI.

## Global Constraints

Copied verbatim; every task implicitly includes all of them.

- **No hardcoded values.** Use constants, config, or env vars (CLAUDE.md rule 1).
- **Single source file ≤ 800 lines** (CLAUDE.md rule 4). `mur-agent-runtime/src/supervisor_runner.rs` (954) and `mur-core/src/cmd/model.rs` (807) are ALREADY over budget: do not grow them beyond a few lines — new code goes into new sibling modules, and no code-movement refactor is in scope here.
- **Explicit beats smart.** A user- or profile-pinned model is never overridden (spec §3.2).
- **Fail-expensive, never fail-broken.** When unsure, keep the better model (spec §2).
- **Above the chat baseline, capability is fail-closed: unstated is not permission** (spec §2).
- **Do not touch `classify()` or the Phase-1 retryable/fatal boundary** (spec §3.2).
- Hub i18n keys land in **both** `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts`.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass (a `--lib`-only run hides broken test targets).
- `cargo fmt` before every commit.

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-common/src/model.rs` | modify | `Requirement`, `CAP_*` consts, `satisfies`, capability-aware `pick_cheap_model` |
| `mur-common/src/config.rs` | modify | `SmartOverride`, `RoutingOverride`, `merged()` on both configs, `DEFAULT_SMART_ENABLED = false`, drop `RoutingConfig.smart` |
| `mur-common/src/agent.rs` | modify | `AgentProfile.smart`, retype `routing`, `effective_smart()` / `effective_routing()` |
| `mur-agent-runtime/src/llm/fallback/mod.rs` | modify | `requirements_of`, registry helpers, candidate filtering, read effective config via the helpers |
| `mur-agent-runtime/src/llm/fallback/tests.rs` | modify | candidate-assembly tests incl. the incident regression |
| `mur-agent-runtime/src/supervisor_runner.rs` | modify | `needs_routing_client` predicate + boot gate (few lines only) |
| `mur-core/src/cmd/model_smart.rs` | **new** | `cmd_model_smart` — global Smart toggle |
| `mur-core/src/cmd/model.rs` | modify | `ModelCmd::Smart` variant + dispatch arm; `ensure_ref_exists` → `pub(crate)` |
| `mur-core/src/cmd/mod.rs` | modify | `pub mod model_smart;` |
| `mur-core/src/cmd/agent/model_resolve.rs` | modify | `cmd_agent_set_smart` |
| `mur-core/src/cli/agent.rs` | modify | `AgentAction::Smart` variant |
| `mur-core/src/dispatch.rs` | modify | `AgentAction::Smart` arm |
| `mur-core/src/cmd/agent/lifecycle.rs`, `mur-core/src/cmd/agent_companion/connector.rs` | modify | add `smart: None` to the two `AgentProfile` struct literals |
| `mur-hub-gui/src-tauri/src/model_switch.rs` | modify | `agent_get_smart` / `agent_set_smart` commands |
| `mur-hub-gui/src-tauri/src/lib.rs` | modify | register the two commands |
| `mur-hub-gui/ui/src/components/settings/modelSwitch.ts` | modify | `normalizeMs` default flips to off |
| `mur-hub-gui/ui/src/components/inspector/AgentInspector.tsx` | modify | three-state per-agent Smart control |
| `mur-hub-gui/ui/src/i18n/{en,zh-TW}.ts` | modify | 5 new keys each |

---

## Task 1 — Capability vocabulary and capability-aware auto-pick

**Interfaces**

*Consumes*: nothing.

*Produces*:
- `mur_common::model::Requirement` (`enum { Vision, Tools }`, `Copy`), `Requirement::capability(self) -> &'static str`
- `mur_common::model::{CAP_CHAT, CAP_TOOLS, CAP_VISION}: &str`
- `mur_common::model::satisfies(e: &ModelEntry, reqs: &[Requirement]) -> bool`
- `mur_common::model::pick_cheap_model(reg: &ModelRegistry, exclude: Option<&str>, reqs: &[Requirement]) -> Option<String>` (**arity changed**)
- `mur_agent_runtime::llm::fallback::requirements_of(req: &LlmRequest) -> Vec<Requirement>` (private to the module)

**Steps**

- [ ] Add both failing tests to the `mod tests` block in `mur-common/src/model.rs` (append after `pick_cheap_model_lowest_cost_chat_excluding_primary`):

```rust
    #[test]
    fn satisfies_is_permissive_at_baseline_and_fail_closed_above_it() {
        let mk = |caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        // Baseline: an entry written before the field existed is still chat.
        assert!(satisfies(&mk(&[]), &[]));
        assert!(satisfies(&mk(&["chat"]), &[]));
        assert!(!satisfies(&mk(&["embedding"]), &[]));
        // Above baseline: unstated is not permission.
        assert!(!satisfies(&mk(&[]), &[Requirement::Vision]));
        assert!(!satisfies(&mk(&["chat"]), &[Requirement::Vision]));
        assert!(satisfies(&mk(&["chat", "vision"]), &[Requirement::Vision]));
        assert!(!satisfies(&mk(&["chat", "vision"]), &[Requirement::Tools]));
        assert!(satisfies(
            &mk(&["chat", "vision", "tools"]),
            &[Requirement::Vision, Requirement::Tools]
        ));
    }

    /// The incident, as a regression test: an image request against a registry
    /// where nothing declares vision must find no cheap candidate at all.
    #[test]
    fn pick_cheap_model_declines_when_no_entry_declares_the_requirement() {
        let mk = |cost: f64, caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            cost_per_1k_tokens: Some(cost),
            ..Default::default()
        };
        let mut reg = ModelRegistry::default();
        reg.models.insert("cheap_text".into(), mk(0.0001, &["chat"]));
        reg.models.insert("legacy".into(), mk(0.0002, &[]));
        reg.models
            .insert("frontier".into(), mk(0.01, &["chat", "vision"]));
        // No requirement → cheapest wins (today's behaviour, unchanged).
        assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap_text".into()));
        // Vision required → only the declaring entry qualifies, cost be damned.
        assert_eq!(
            pick_cheap_model(&reg, None, &[Requirement::Vision]),
            Some("frontier".into())
        );
        // Nothing declares vision → None, so Smart goes inert.
        let mut blind = ModelRegistry::default();
        blind.models.insert("cheap_text".into(), mk(0.0001, &["chat"]));
        blind.models.insert("legacy".into(), mk(0.0002, &[]));
        assert_eq!(pick_cheap_model(&blind, None, &[Requirement::Vision]), None);
    }
```

- [ ] Run `cargo nextest run -p mur-common model::tests` → both new tests fail to compile (`cannot find value Requirement`, `this function takes 2 arguments but 3 were supplied`). This is the expected red.
- [ ] In `mur-common/src/model.rs`, insert immediately **above** `pub fn pick_cheap_model`:

```rust
/// Registry capability strings. The baseline (`chat`) is legacy-permissive —
/// an entry with no `capabilities` at all predates the field and is assumed
/// chat-capable. Everything above the baseline is fail-closed.
pub const CAP_CHAT: &str = "chat";
pub const CAP_TOOLS: &str = "tools";
pub const CAP_VISION: &str = "vision";

/// A capability the request needs from whatever model serves it. Derived from
/// the request itself (an image in the messages, a tool list) and never from
/// config: a router may only substitute a model that can do the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// The request carries an image; the model has to be able to see it.
    Vision,
    /// The request declares tools; the model has to be able to call them.
    Tools,
}

impl Requirement {
    /// The registry capability an entry must declare to satisfy this.
    pub fn capability(self) -> &'static str {
        match self {
            Requirement::Vision => CAP_VISION,
            Requirement::Tools => CAP_TOOLS,
        }
    }
}

/// Can this entry serve a request needing `reqs`?
///
/// No registry write path emits `vision` today, so a `Vision` requirement
/// disqualifies every current entry — auto-substitution goes inert for image
/// requests rather than answering them blind. The same code makes a finer
/// distinction the day entries start declaring it; there is no second version
/// of this function to write later.
pub fn satisfies(e: &ModelEntry, reqs: &[Requirement]) -> bool {
    let chat_capable = e.capabilities.is_empty() || e.capabilities.iter().any(|c| c == CAP_CHAT);
    if !chat_capable {
        return false;
    }
    reqs.iter()
        .all(|r| e.capabilities.iter().any(|c| c == r.capability()))
}
```

- [ ] Replace the body and signature of `pick_cheap_model` with:

```rust
/// Pick the cheapest registry entry that can serve a request needing `reqs`,
/// excluding `exclude` (the agent's own primary). None when no qualifying
/// entry exists → caller keeps normal candidates (fail-expensive).
pub fn pick_cheap_model(
    reg: &ModelRegistry,
    exclude: Option<&str>,
    reqs: &[Requirement],
) -> Option<String> {
    reg.models
        .iter()
        .filter(|(k, _)| exclude != Some(k.as_str()))
        .filter(|(_, e)| satisfies(e, reqs))
        .filter_map(|(k, e)| {
            // Not the deprecated field directly: `mur model add --output-cost`
            // deliberately leaves it unset, so reading it drops every entry
            // added with the current flags instead of ranking it.
            let (input, output) = e.effective_costs();
            output.or(input).map(|c| (c, k.clone()))
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, k)| k)
}
```

- [ ] Update the three call sites inside the existing test `pick_cheap_model_lowest_cost_chat_excluding_primary` to pass `&[]` as the third argument: `pick_cheap_model(&reg, Some("cheap"), &[])`, `pick_cheap_model(&reg, None, &[])`, `pick_cheap_model(&empty, None, &[])`.
- [ ] Run `cargo nextest run -p mur-common model::tests` → all green, including the two new tests.
- [ ] In `mur-agent-runtime/src/llm/fallback/mod.rs`, change the import line `use mur_common::model::{choose_by_difficulty, resolve_model_refs};` to `use mur_common::model::{Requirement, choose_by_difficulty, resolve_model_refs};`
- [ ] Replace `fn autopick_cheap` (currently at line 427) with:

```rust
/// The capabilities this request needs from whatever model serves it. Derived
/// from the request, never from config: an image in the messages means the
/// model has to be able to see it, a tool list means it has to be able to call.
fn requirements_of(req: &LlmRequest) -> Vec<Requirement> {
    let mut out = Vec::new();
    if req
        .messages
        .iter()
        .any(|m| matches!(m, RichMessage::ImageText { .. }))
    {
        out.push(Requirement::Vision);
    }
    if !req.tools.is_empty() {
        out.push(Requirement::Tools);
    }
    out
}

fn autopick_cheap(primary: Option<&str>, reqs: &[Requirement]) -> Option<String> {
    let path = mur_common::model::ModelRegistry::default_path().ok()?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).ok()?;
    mur_common::model::pick_cheap_model(&reg, primary, reqs)
}
```

- [ ] In `candidates_for`, inside the `CandidateSource::PerRequest` arm, add `let reqs = requirements_of(req);` as the first line of the arm, and change the auto-pick call from `.or_else(|| autopick_cheap(primary.as_deref()))` to `.or_else(|| autopick_cheap(primary.as_deref(), &reqs))`.
- [ ] Run `cargo nextest run -p mur-agent-runtime llm::fallback` → green (existing tests unaffected; they pin `smart.cheap` and never reach auto-pick).
- [ ] `cargo fmt && git commit -am "feat(model): capability requirements gate cheap-model auto-pick"`

---

## Task 2 — Filter every automatic substitution, not just auto-pick

**Interfaces**

*Consumes*: `Requirement`, `satisfies`, `pick_cheap_model`, `requirements_of` (Task 1).

*Produces*: private to `mur-agent-runtime/src/llm/fallback/mod.rs`:
- `load_registry() -> Option<ModelRegistry>`
- `ref_ok(reg: Option<&ModelRegistry>, model_ref: &str, reqs: &[Requirement]) -> bool`
- `filter_eligible(refs: Vec<String>, keep: Option<&str>, reqs: &[Requirement], reg: Option<&ModelRegistry>) -> Vec<String>`

**Steps**

- [ ] Add the failing test to `mur-agent-runtime/src/llm/fallback/tests.rs` (append at the end of the file):

```rust
/// The incident at the candidate-assembly layer: a background image turn must
/// not be handed a cheap model that never declared it can see. Text work on the
/// same config is untouched — this gate costs nothing when nothing is required.
#[test]
fn background_image_turn_drops_a_cheap_model_that_cannot_see() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    use mur_common::model::{ModelEntry, ModelRegistry};

    let mk = |cost: f64, caps: &[&str]| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        cost_per_1k_tokens: Some(cost),
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut reg = ModelRegistry::default();
    reg.models.insert("cheap".into(), mk(0.0001, &["chat"]));
    reg.models.insert("primary".into(), mk(0.01, &["chat"]));
    reg.save_to(&tmp.path().join("models.yaml")).unwrap();
    // nextest runs one process per test, so this env write is not shared.
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

    let cfg = ModelSwitchConfig {
        default: Some("primary".into()),
        smart: SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        },
        ..Default::default()
    };
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(Default::default()),
        retry0(),
    );

    let text_turn = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        ..Default::default()
    };
    assert_eq!(
        fb.candidates_for(&text_turn),
        vec!["cheap".to_string(), "primary".into()],
        "text background work still downgrades"
    );

    let image_turn = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        messages: vec![RichMessage::ImageText {
            role: "user".into(),
            media_type: "image/png".into(),
            data: "aGk=".into(),
            text: "what is in this photo".into(),
        }],
        ..Default::default()
    };
    assert_eq!(
        fb.candidates_for(&image_turn),
        vec!["primary".to_string()],
        "nothing declares vision, so the explicit primary is all that is left"
    );
}
```

- [ ] Run `cargo nextest run -p mur-agent-runtime background_image_turn` → **fails**, asserting `["cheap", "primary"]` for the image turn. That failure is the bug, reproduced.
- [ ] Add the three helpers to `mur-agent-runtime/src/llm/fallback/mod.rs`, immediately after `requirements_of`:

```rust
/// The registry, or `None` when it cannot be read. Eligibility treats `None`
/// as "no opinion" and keeps every candidate: a routing heuristic must not turn
/// registry I/O into a hard dependency on the request path.
fn load_registry() -> Option<mur_common::model::ModelRegistry> {
    let path = mur_common::model::ModelRegistry::default_path().ok()?;
    mur_common::model::ModelRegistry::load_from(&path).ok()
}

/// Can `model_ref` serve a request needing `reqs`? A ref the registry does not
/// know is kept, so the existing per-candidate factory-error reporting (#947)
/// still names it instead of it vanishing from the failure list.
fn ref_ok(
    reg: Option<&mur_common::model::ModelRegistry>,
    model_ref: &str,
    reqs: &[Requirement],
) -> bool {
    match reg.and_then(|r| r.models.get(model_ref)) {
        Some(e) => mur_common::model::satisfies(e, reqs),
        None => true,
    }
}

/// Drop automatic candidates that cannot serve the request. `keep` — the user's
/// explicit primary — always survives: it is their choice and must fail loudly
/// rather than be second-guessed. Empty `reqs` is the common case and is free.
fn filter_eligible(
    refs: Vec<String>,
    keep: Option<&str>,
    reqs: &[Requirement],
    reg: Option<&mur_common::model::ModelRegistry>,
) -> Vec<String> {
    if reqs.is_empty() {
        return refs;
    }
    refs.into_iter()
        .filter(|r| Some(r.as_str()) == keep || ref_ok(reg, r, reqs))
        .collect()
}
```

- [ ] Replace the whole `CandidateSource::PerRequest { profile, cfg } => { … }` arm of `candidates_for` with:

```rust
            CandidateSource::PerRequest { profile, cfg } => {
                let reqs = requirements_of(req);
                let reg = if reqs.is_empty() {
                    None
                } else {
                    load_registry()
                };

                // Per-agent routing overrides global; disabled → None → normal
                // model_ref/default primary. A routed pick is an automatic
                // substitution, so it has to clear the capability bar too;
                // when it cannot, resolution falls through to the primary.
                let routing = profile
                    .routing
                    .clone()
                    .unwrap_or_else(|| cfg.routing.clone());
                let routed = if routing.enabled {
                    choose_by_difficulty(estimate_input_tokens(req), &routing)
                        .filter(|r| ref_ok(reg.as_ref(), r, &reqs))
                } else {
                    None
                };
                let base = resolve_model_refs(profile, cfg, routed);
                let keep = base.first().cloned();

                // Smart background: cheap model first, base (primary + chain)
                // behind it (cascade). Per-agent Smart config overrides global.
                let smart = profile
                    .routing
                    .as_ref()
                    .and_then(|r| r.smart.clone())
                    .unwrap_or_else(|| cfg.smart.clone());
                if matches!(req.intent, RequestIntent::Background(_)) && smart.enabled {
                    let cheap = smart
                        .cheap
                        .clone()
                        .filter(|c| ref_ok(reg.as_ref(), c, &reqs))
                        .or_else(|| autopick_cheap(keep.as_deref(), &reqs));
                    if let Some(c) = cheap {
                        let mut out = vec![c];
                        for r in base {
                            if !out.contains(&r) {
                                out.push(r);
                            }
                        }
                        return filter_eligible(out, keep.as_deref(), &reqs, reg.as_ref());
                    }
                }
                filter_eligible(base, keep.as_deref(), &reqs, reg.as_ref())
            }
```

- [ ] Run `cargo nextest run -p mur-agent-runtime llm::fallback` → all green, including the new test.
- [ ] `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings` → clean.
- [ ] `cargo fmt && git commit -am "fix(routing): never substitute a model that cannot serve the request"`

**Layer 1 is complete and shippable here.** Tasks 3-7 are the policy layer.

---

## Task 3 — Partial override types, per-field merge, and the opt-in default

**Interfaces**

*Consumes*: nothing from Tasks 1-2.

*Produces*:
- `mur_common::config::SmartOverride { enabled: Option<bool>, cheap: Option<String>, max_escalations: Option<u32> }` (`Default`, `PartialEq`)
- `mur_common::config::RoutingOverride { enabled: Option<bool>, cheap: Option<String>, frontier: Option<String>, threshold_input_tokens: Option<u32>, smart: Option<SmartOverride> }` (`Default`, `PartialEq`)
- `SmartConfig::merged(&self, ov: Option<&SmartOverride>) -> SmartConfig`
- `RoutingConfig::merged(&self, ov: Option<&RoutingOverride>) -> RoutingConfig`
- `mur_common::config::DEFAULT_SMART_ENABLED: bool` (= `false`)
- `RoutingConfig.smart` is left in place here and removed in Task 4, where `AgentProfile.routing` is retyped — until then `fallback/mod.rs` still reads the legacy `profile.routing.smart` through it.

**Steps**

- [ ] Add the failing tests to the `mod tests` block at the bottom of `mur-common/src/config.rs`:

```rust
    #[test]
    fn smart_override_inherits_field_by_field() {
        let global = SmartConfig {
            enabled: true,
            cheap: Some("g".into()),
            max_escalations: 3,
        };
        assert_eq!(global.merged(None), global);
        assert_eq!(global.merged(Some(&SmartOverride::default())), global);
        // Overriding one field must not reset the others — the whole point.
        let only_cheap = SmartOverride {
            cheap: Some("a".into()),
            ..Default::default()
        };
        let m = global.merged(Some(&only_cheap));
        assert!(m.enabled, "overriding cheap must not disable Smart");
        assert_eq!(m.cheap.as_deref(), Some("a"));
        assert_eq!(m.max_escalations, 3);
        // An explicit false still beats a global true.
        let off = SmartOverride {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!global.merged(Some(&off)).enabled);
    }

    #[test]
    fn routing_override_inherits_field_by_field() {
        let global = RoutingConfig {
            enabled: true,
            cheap: Some("c".into()),
            frontier: Some("f".into()),
            threshold_input_tokens: Some(9),
            smart: None, // removed in Task 4
        };
        let only_cheap = RoutingOverride {
            cheap: Some("a".into()),
            ..Default::default()
        };
        let m = global.merged(Some(&only_cheap));
        assert!(m.enabled, "overriding cheap must not disable routing");
        assert_eq!(m.cheap.as_deref(), Some("a"));
        assert_eq!(m.frontier.as_deref(), Some("f"));
        assert_eq!(m.threshold_input_tokens, Some(9));
    }
```

- [ ] Run `cargo nextest run -p mur-common config::tests` → red (`SmartOverride` not found).
- [ ] In `mur-common/src/config.rs`, add next to the other default constants (after `DEFAULT_SMART_MAX_ESCALATIONS`, line 16):

```rust
/// Smart background routing is opt-in. Its failure mode is silent and
/// irreversible for the turn it degrades, which is the kind of automation that
/// has to be asked for. See the capability-gate spec §2.
pub const DEFAULT_SMART_ENABLED: bool = false;
```

and the serde helper next to `default_smart_max_escalations`:

```rust
fn default_smart_enabled() -> bool {
    DEFAULT_SMART_ENABLED
}
```

- [ ] In `SmartConfig`, change `#[serde(default = "default_true")]` on `enabled` to `#[serde(default = "default_smart_enabled")]`, and in `impl Default for SmartConfig` change `enabled: true` to `enabled: DEFAULT_SMART_ENABLED`. Update the doc comment above `SmartConfig` — replace "Defaults ON with `cheap: None`" with "Defaults OFF; enable per-agent or globally (`mur model smart on`). `cheap: None` auto-picks".
- [ ] Add both override types and both `merged` impls immediately after `RoutingConfig`:

```rust
/// Partial view of [`SmartConfig`] for per-agent overrides: `None` on a field
/// means "inherit the global value". A distinct type rather than reusing
/// `SmartConfig`, because a full config standing in for a partial is exactly
/// what made an omitted field silently mean `false` instead of "unset".
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SmartOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_escalations: Option<u32>,
}

/// Partial view of [`RoutingConfig`]. Same inheritance rule as
/// [`SmartOverride`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RoutingOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_input_tokens: Option<u32>,
    /// Legacy nesting. Smart used to be overridden at `routing.smart`;
    /// profiles written before the promotion to `AgentProfile.smart` — and
    /// every exported `.muragent` bundle — still carry it, so it stays
    /// readable forever. MUR never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<SmartOverride>,
}

impl SmartConfig {
    /// Global values with an agent's override layered on, field by field.
    pub fn merged(&self, ov: Option<&SmartOverride>) -> SmartConfig {
        let Some(o) = ov else { return self.clone() };
        SmartConfig {
            enabled: o.enabled.unwrap_or(self.enabled),
            cheap: o.cheap.clone().or_else(|| self.cheap.clone()),
            max_escalations: o.max_escalations.unwrap_or(self.max_escalations),
        }
    }
}

impl RoutingConfig {
    /// Global values with an agent's override layered on, field by field.
    pub fn merged(&self, ov: Option<&RoutingOverride>) -> RoutingConfig {
        let Some(o) = ov else { return self.clone() };
        RoutingConfig {
            enabled: o.enabled.unwrap_or(self.enabled),
            cheap: o.cheap.clone().or_else(|| self.cheap.clone()),
            frontier: o.frontier.clone().or_else(|| self.frontier.clone()),
            threshold_input_tokens: o.threshold_input_tokens.or(self.threshold_input_tokens),
            // Carried through untouched; the field itself goes away in Task 4.
            smart: self.smart.clone(),
        }
    }
}
```

- [ ] Confirm `SmartConfig` still derives `PartialEq` (it does today) — the new tests compare whole structs.
- [ ] Fix the existing default-value test around `mur-common/src/config.rs:2700`: change `assert!(cfg.models.smart.enabled); // default ON` to:

```rust
        assert!(
            !cfg.models.smart.enabled,
            "Smart background routing is opt-in (capability-gate spec §2)"
        );
```

- [ ] Run `cargo nextest run -p mur-common config` → green.
- [ ] `cargo fmt && git commit -am "feat(config): partial override types for smart/routing, Smart defaults off"`

---

## Task 4 — Promote the per-agent override onto the profile

**Interfaces**

*Consumes*: `SmartOverride`, `RoutingOverride`, `merged` (Task 3).

*Produces*:
- `AgentProfile.smart: Option<SmartOverride>` (new field, serde-default, skipped when `None`)
- `AgentProfile.routing: Option<RoutingOverride>` (**retyped** from `Option<RoutingConfig>`)
- `AgentProfile::effective_smart(&self, cfg: &ModelSwitchConfig) -> SmartConfig`
- `AgentProfile::effective_routing(&self, cfg: &ModelSwitchConfig) -> RoutingConfig`
- **Removed**: `RoutingConfig.smart` — the global type stops carrying a per-agent override; only `RoutingOverride` keeps one, for the legacy read

**Steps**

- [ ] Add the failing test to the `mod tests` block in `mur-common/src/agent.rs`:

```rust
    #[test]
    fn effective_smart_prefers_the_promoted_field_then_the_legacy_nesting() {
        use crate::config::{ModelSwitchConfig, SmartConfig, SmartOverride};
        let cfg = ModelSwitchConfig {
            smart: SmartConfig {
                enabled: false,
                cheap: Some("g".into()),
                max_escalations: 2,
            },
            ..Default::default()
        };
        // No override at all → the global values, untouched.
        let p = AgentProfile::default_for_tests();
        assert_eq!(p.effective_smart(&cfg), cfg.smart);

        // Legacy profiles carry the override nested under `routing`.
        let mut legacy = AgentProfile::default_for_tests();
        legacy.routing = Some(crate::config::RoutingOverride {
            smart: Some(SmartOverride {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(legacy.effective_smart(&cfg).enabled, "legacy nesting is read");
        assert_eq!(
            legacy.effective_smart(&cfg).cheap.as_deref(),
            Some("g"),
            "unset fields still inherit"
        );

        // The promoted field wins when both are present.
        let mut both = legacy.clone();
        both.smart = Some(SmartOverride {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(!both.effective_smart(&cfg).enabled);
    }
```

- [ ] Run `cargo nextest run -p mur-common agent::tests` → red.
- [ ] In `mur-common/src/agent.rs`, retype the `routing` field and add `smart` directly after it:

```rust
    /// Per-agent difficulty-routing override. Absent fields inherit the global
    /// `models.routing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<crate::config::RoutingOverride>,
    /// Per-agent Smart background-routing override. Absent fields inherit the
    /// global `models.smart`; `None` means "follow the global setting".
    /// Promoted out of `routing` — nesting it there meant overriding Smart
    /// silently rewrote this agent's difficulty routing as a side effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<crate::config::SmartOverride>,
```

- [ ] Add the two helpers inside `impl AgentProfile` (next to `default_for_tests`):

```rust
    /// This agent's effective Smart config: the global values with the agent's
    /// override layered on. Reads the promoted `smart` field first and falls
    /// back to the legacy `routing.smart` nesting that older profiles and
    /// exported `.muragent` bundles still carry.
    pub fn effective_smart(
        &self,
        cfg: &crate::config::ModelSwitchConfig,
    ) -> crate::config::SmartConfig {
        let ov = self
            .smart
            .as_ref()
            .or_else(|| self.routing.as_ref().and_then(|r| r.smart.as_ref()));
        cfg.smart.merged(ov)
    }

    /// This agent's effective difficulty-routing config.
    pub fn effective_routing(
        &self,
        cfg: &crate::config::ModelSwitchConfig,
    ) -> crate::config::RoutingConfig {
        cfg.routing.merged(self.routing.as_ref())
    }
```

- [ ] Fix the existing round-trip test at `mur-common/src/agent.rs:2055`: replace

```rust
        p.routing = Some(crate::config::RoutingConfig {
            enabled: true,
            ..Default::default()
        });
```

with

```rust
        p.routing = Some(crate::config::RoutingOverride {
            enabled: Some(true),
            ..Default::default()
        });
```

- [ ] Add `smart: None,` to the two `AgentProfile` struct literals — `mur-core/src/cmd/agent/lifecycle.rs:134` (next to `routing: None,`) and `mur-core/src/cmd/agent_companion/connector.rs:548`. `AgentProfile` has no `derive(Default)`, so a literal that omits the new field will not compile.
- [ ] Replace the four inheritance read sites in `mur-agent-runtime/src/llm/fallback/mod.rs` with the helpers:
  - in `selection_reason`: `let smart = profile.effective_smart(cfg);`
  - in `candidates_for`: `let routing = profile.effective_routing(cfg);` and `let smart = profile.effective_smart(cfg);`
  - in `generate_with_meta`'s `max_esc` match arm: `let smart = profile.effective_smart(cfg);`

  Each replaces the corresponding `profile.routing…unwrap_or_else(|| cfg.…clone())` expression; nothing else in those blocks changes.
- [ ] Now that the legacy read goes through `RoutingOverride.smart`, delete the `smart: Option<SmartConfig>` field (and its two serde attribute lines) from `RoutingConfig` in `mur-common/src/config.rs`, drop the `smart: self.smart.clone(),` line from `RoutingConfig::merged`, and drop the now-dangling `smart:` line from three literals: the `routing_override_inherits_field_by_field` test (Task 3), `mur-agent-runtime/src/llm/fallback/tests.rs` in `routed_generate_picks_frontier_for_large_request`, and `mur-hub-gui/src-tauri/src/model_switch.rs` around line 155.
- [ ] Run `cargo nextest run -p mur-common && cargo nextest run -p mur-agent-runtime llm::fallback` → green.
- [ ] `cargo check --workspace --all-targets` → clean (catches any remaining `AgentProfile` literal in a test target).
- [ ] `cargo fmt && git commit -am "feat(agent): three-state smart override on the profile"`

---

## Task 5 — Stop the boot gate from silently disabling Smart

**Interfaces**

*Consumes*: `effective_smart`, `effective_routing` (Task 4).

*Produces*: `mur_agent_runtime::supervisor_runner::needs_routing_client(refs: usize, routing_on: bool, smart_on: bool) -> bool` (crate-private).

**Steps**

- [ ] Add the failing test to the existing `#[cfg(test)] mod` at `mur-agent-runtime/src/supervisor_runner.rs:808`:

```rust
    /// An agent with one model ref and no chain still needs the routing-aware
    /// client when Smart is on for it — the boot gate used to consult only the
    /// chain length and difficulty routing, so Smart was dead for exactly the
    /// agents that never configured anything else.
    #[test]
    fn single_ref_agent_with_smart_on_still_needs_the_routing_client() {
        assert!(!super::needs_routing_client(1, false, false));
        assert!(super::needs_routing_client(1, false, true));
        assert!(super::needs_routing_client(1, true, false));
        assert!(super::needs_routing_client(2, false, false));
    }
```

- [ ] Run `cargo nextest run -p mur-agent-runtime needs_routing_client` → red (function does not exist).
- [ ] Add the predicate just above the function containing the gate (search for `if refs.len() <= 1 && !routing.enabled {`):

```rust
/// Does this agent need the routing-aware client, or is a single plain client
/// enough? Extracted so the decision is testable without building providers:
/// when this is wrong, Smart is silently inert for every agent with no
/// fallback chain and the toggle reports a state it does not have.
pub(crate) fn needs_routing_client(refs: usize, routing_on: bool, smart_on: bool) -> bool {
    refs > 1 || routing_on || smart_on
}
```

- [ ] Replace the gate block:

```rust
    let routing = profile.inner.effective_routing(&switch_cfg);
    let smart = profile.inner.effective_smart(&switch_cfg);
    let refs = mur_common::model::resolve_model_refs(&profile.inner, &switch_cfg, None);

    if !needs_routing_client(refs.len(), routing.enabled, smart.enabled) {
```

  (the old `let routing = profile.inner.routing.clone().unwrap_or_else(…);` goes away; the body of the `if` is unchanged.)
- [ ] Update the comment block above it: the sentence "With no `models:` config and no per-agent chain/routing, `refs.len() <= 1 && !routing.enabled`" becomes "With no `models:` config, no per-agent chain/routing and Smart off (the default), `needs_routing_client` is false".
- [ ] Run `cargo nextest run -p mur-agent-runtime` → green.
- [ ] `cargo fmt && git commit -am "fix(runtime): boot gate consults the effective Smart setting"`

---

## Task 6 — CLI: `mur model smart` and `mur agent smart`

**Interfaces**

*Consumes*: `SmartOverride` (Task 3), `AgentProfile.smart` (Task 4).

*Produces*:
- `mur_core::cmd::model_smart::cmd_model_smart(home: &Path, on: bool, cheap: Option<&str>) -> anyhow::Result<()>`
- `mur_core::cmd::agent::model_resolve::cmd_agent_set_smart(home: &Path, name: &str, state: &str) -> anyhow::Result<()>`
- `ModelCmd::Smart { state: String, cheap: Option<String> }`, `AgentAction::Smart { name: String, state: String }`
- `mur_core::cmd::model::ensure_ref_exists` becomes `pub(crate)`

**Steps**

- [ ] Create `mur-core/src/cmd/model_smart.rs` with its test first:

```rust
//! `mur model smart <on|off>` — the global Smart background-routing toggle.
//!
//! Its own module rather than another arm in `cmd/model.rs`, which is already
//! at the 800-line ceiling (CLAUDE.md rule 4), matching the `model_doctor` /
//! `model_connect` siblings.
use std::path::Path;

use crate::cmd::model::ensure_ref_exists;

/// Set `models.smart.enabled`, optionally pinning the model Smart downgrades
/// to. The ref is validated against `models.yaml` fail-closed, like every other
/// ref-taking setter.
pub fn cmd_model_smart(home: &Path, on: bool, cheap: Option<&str>) -> anyhow::Result<()> {
    if let Some(c) = cheap {
        ensure_ref_exists(home, c)?;
    }
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.smart.enabled = on;
    if let Some(c) = cheap {
        cfg.models.smart.cheap = Some(c.to_string());
    }
    crate::store::config::save_config_at(&home.join("config.yaml"), &cfg)?;
    println!(
        "smart background routing = {} (cheap = {})",
        if on { "on" } else { "off" },
        cfg.models.smart.cheap.as_deref().unwrap_or("auto")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::{ModelEntry, ModelRegistry};

    #[test]
    fn toggles_and_validates_the_cheap_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                ..Default::default()
            },
        );
        reg.save_to(&home.join("models.yaml")).unwrap();

        cmd_model_smart(home, true, Some("haiku")).unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert!(cfg.models.smart.enabled);
        assert_eq!(cfg.models.smart.cheap.as_deref(), Some("haiku"));

        // Turning it off keeps the pinned ref — the user's choice survives.
        cmd_model_smart(home, false, None).unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert!(!cfg.models.smart.enabled);
        assert_eq!(cfg.models.smart.cheap.as_deref(), Some("haiku"));

        // Unknown ref is refused before anything is written.
        assert!(cmd_model_smart(home, true, Some("nope")).is_err());
    }
}
```

- [ ] Add `pub mod model_smart;` to `mur-core/src/cmd/mod.rs` after `pub mod model_doctor;` (line 53).
- [ ] Change `fn ensure_ref_exists` in `mur-core/src/cmd/model.rs:704` to `pub(crate) fn ensure_ref_exists`.
- [ ] Add the subcommand variant to `ModelCmd` in `mur-core/src/cmd/model.rs`, after `Fallback`:

```rust
    /// Turn Smart background routing on or off globally (config.yaml
    /// `models.smart`). Off by default: background turns then run on the
    /// agent's own model. `--cheap` pins the model Smart downgrades to;
    /// omit it to auto-pick the cheapest capable chat model.
    Smart {
        /// `on` or `off`.
        #[arg(value_parser = ["on", "off"])]
        state: String,
        /// Registry key Smart downgrades to. Omit for auto-pick.
        #[arg(long)]
        cheap: Option<String>,
    },
```

- [ ] Add the dispatch arm next to `ModelCmd::Fallback` (around line 394):

```rust
        ModelCmd::Smart { state, cheap } => crate::cmd::model_smart::cmd_model_smart(
            &crate::cmd::agent::resolve_mur_home()?,
            state == "on",
            cheap.as_deref(),
        )?,
```

- [ ] Add `cmd_agent_set_smart` to `mur-core/src/cmd/agent/model_resolve.rs`, directly after `cmd_agent_set_fallback`:

```rust
/// Set (or clear) this agent's Smart background-routing override.
/// `follow` removes the override so the agent inherits `models.smart`.
pub fn cmd_agent_set_smart(home: &Path, name: &str, state: &str) -> Result<()> {
    let profile_path = home.join("agents").join(name).join("profile.yaml");
    if !profile_path.exists() {
        bail!("agent '{name}' not installed at {}", profile_path.display());
    }
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    // Preserve any other overridden field (e.g. a pinned cheap model): this
    // sets one field, it does not replace the block.
    let existing = profile.smart.take().unwrap_or_default();
    profile.smart = match state {
        "follow" => None,
        "on" => Some(mur_common::config::SmartOverride {
            enabled: Some(true),
            ..existing
        }),
        "off" => Some(mur_common::config::SmartOverride {
            enabled: Some(false),
            ..existing
        }),
        other => bail!("unknown state '{other}' (expected on, off, or follow)"),
    };
    std::fs::write(&profile_path, serde_yaml_ng::to_string(&profile)?)
        .with_context(|| format!("write {}", profile_path.display()))?;
    println!("agent '{name}' smart routing = {state}");
    Ok(())
}
```

- [ ] Add the CLI variant to `AgentAction` in `mur-core/src/cli/agent.rs`, after `Fallback`:

```rust
    /// Turn Smart background routing on or off for this agent, or `follow` to
    /// clear the override and inherit the global setting (`mur model smart`).
    Smart {
        /// Agent name.
        name: String,
        /// `on`, `off`, or `follow`.
        #[arg(value_parser = ["on", "off", "follow"])]
        state: String,
    },
```

- [ ] Add the dispatch arm in `mur-core/src/dispatch.rs`, after the `AgentAction::Fallback` arm:

```rust
        AgentAction::Smart { name, state } => cmd::agent::model_resolve::cmd_agent_set_smart(
            &cmd::agent::resolve_mur_home()?,
            &name,
            &state,
        )?,
```

- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core model_smart` → green. (`mur-core` needs both env vars to build; without `MUR_WEB_DIST` the dashboard embed fails.)
- [ ] Verify the surface end to end:

```bash
export MUR_HOME=$(mktemp -d)
cargo run -q -- model smart on --cheap nope   # expect: error, model_ref "nope" not in models.yaml
cargo run -q -- model smart off               # expect: smart background routing = off (cheap = auto)
```

- [ ] `cargo fmt && git commit -am "feat(cli): mur model smart and mur agent smart"`

---

## Task 7 — Hub: three-state per-agent control and the flipped default

**Interfaces**

*Consumes*: `AgentProfile.smart`, `SmartOverride` (Tasks 3-4), `cmd_agent_set_smart` (Task 6).

*Produces*:
- Tauri commands `agent_get_smart(name) -> Option<bool>` and `agent_set_smart(name, state: String) -> Option<bool>`
- i18n keys `detail.smartRouting`, `detail.smartFollow`, `detail.smartOn`, `detail.smartOff`, `detail.smartHint` in both locales

**Steps**

- [ ] Add the two command impls to `mur-hub-gui/src-tauri/src/model_switch.rs`, after `agent_set_fallback`:

```rust
/// Tri-state read: `None` = the agent follows the global setting.
pub(crate) fn agent_get_smart_impl(home: &Path, name: &str) -> Result<Option<bool>, String> {
    let path = home.join("agents").join(name).join("profile.yaml");
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("parse profile: {e}"))?;
    Ok(profile.smart.and_then(|s| s.enabled))
}

pub(crate) fn agent_set_smart_impl(
    home: &Path,
    name: &str,
    state: &str,
) -> Result<Option<bool>, String> {
    mur_core::cmd::agent::model_resolve::cmd_agent_set_smart(home, name, state)
        .map_err(|e| format!("{e}"))?;
    agent_get_smart_impl(home, name)
}

#[tauri::command]
pub fn agent_get_smart(name: String) -> Result<Option<bool>, String> {
    agent_get_smart_impl(&crate::mur_home_path(), &name)
}

#[tauri::command]
pub fn agent_set_smart(name: String, state: String) -> Result<Option<bool>, String> {
    agent_set_smart_impl(&crate::mur_home_path(), &name, &state)
}
```

- [ ] Register both in the `tauri::generate_handler![...]` list in `mur-hub-gui/src-tauri/src/lib.rs`, next to `agent_get_fallback` / `agent_set_fallback`.
- [ ] Flip the TS default in `mur-hub-gui/ui/src/components/settings/modelSwitch.ts` — inside `normalizeMs`, change `enabled: raw.smart?.enabled ?? true` to `enabled: raw.smart?.enabled ?? false`, and update the doc comment sentence about `smart` to say the fallback mirrors the Rust default, which is now off.
- [ ] Add the vitest case to `mur-hub-gui/ui/src/components/settings/modelSwitch.test.ts`:

```ts
  it("defaults smart to off when the config predates the field", () => {
    const raw = { retry: {}, routing: {} } as unknown as ModelSwitchView;
    expect(normalizeMs(raw).smart.enabled).toBe(false);
  });
```

- [ ] Add the five keys to `mur-hub-gui/ui/src/i18n/en.ts`:

```ts
  "detail.smartRouting": "Smart routing",
  "detail.smartFollow": "Follow global setting",
  "detail.smartOn": "On for this agent",
  "detail.smartOff": "Off for this agent",
  "detail.smartHint":
    "Background tasks may run on a cost-saving model. Never applies to requests this agent's cheaper models cannot serve.",
```

and to `mur-hub-gui/ui/src/i18n/zh-TW.ts`:

```ts
  "detail.smartRouting": "智慧路由",
  "detail.smartFollow": "跟隨全域設定",
  "detail.smartOn": "此 agent 開啟",
  "detail.smartOff": "此 agent 關閉",
  "detail.smartHint":
    "背景任務可能改用較省錢的模型。若該模型無法勝任這個請求，一律不會降級。",
```

- [ ] Wire the control in `mur-hub-gui/ui/src/components/inspector/AgentInspector.tsx`. Add state and a loader next to the existing `agentChain` pair (around line 91):

```tsx
  // Per-agent Smart override. null = follow the global setting.
  const [agentSmart, setAgentSmart] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<boolean | null>("agent_get_smart", { name: agentName })
      .then(setAgentSmart)
      .catch(() => setAgentSmart(null));
  }, [agentName]);

  function saveAgentSmart(state: string) {
    invoke<boolean | null>("agent_set_smart", { name: agentName, state })
      .then(setAgentSmart)
      .catch((e) => setChainErr(String(e)));
  }
```

  and render it directly below the `FallbackChainEditor` block (inside the same `div.tab-form`, after the `chainErr` paragraph):

```tsx
              <label className="field-label" htmlFor="agent-smart">
                {t("detail.smartRouting")}
              </label>
              <select
                id="agent-smart"
                value={agentSmart === null ? "follow" : agentSmart ? "on" : "off"}
                onChange={(e) => saveAgentSmart(e.target.value)}
              >
                <option value="follow">{t("detail.smartFollow")}</option>
                <option value="on">{t("detail.smartOn")}</option>
                <option value="off">{t("detail.smartOff")}</option>
              </select>
              <p className="settings-hint">{t("detail.smartHint")}</p>
```

- [ ] Run the UI gate: `cd mur-hub-gui/ui && npm run test && npm run build` → tests green, build succeeds (this also produces `ui/dist`, which the Rust side needs).
- [ ] Run the Tauri gate: `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings` → clean. (Requires `ui/dist` from the previous step.)
- [ ] `cargo fmt && git commit -am "feat(hub): three-state per-agent smart routing control"`

---

## Final verification

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo nextest run -p mur-common -p mur-agent-runtime`
- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core`
- [ ] `cd mur-hub-gui/ui && npm run test && npm run build`
- [ ] Spec coverage walk: §3.1 → T1; §3.2 → T2; §4.2 → T3; §4.3 → T4; §4.4 → T3 (default) + T5 (gate); §4.5 → T6 (CLI) + T7 (Hub); §4.6 → no task, by design; §7 (Layer 3) → deliberately out of scope.
- [ ] Docs: this change adds two CLI commands and flips a user-visible default, so it triggers the documentation checklist (README, docs site, product page) via the `update-docs` skill. Not part of this plan — raise it as the follow-up when the branch is ready.
