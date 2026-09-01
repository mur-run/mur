# Smart Routing — Capability Gate + Inheritable Policy Design Spec

> **Date**: 2026-09-01
> **Status**: Ready for review
> **Scope**: Make automatic model substitution capability-preserving (Layer 1), and make the Smart-routing policy opt-in with three-state per-agent inheritance (Layer 2). Builds on model-switching Phase 1 (#692), Phase 2 (#693) and Phase A+B (#697).
> **Out of scope**: quality-based re-routing (still out of scope, see Phase A+B spec); learned/kNN routing (cost-router Phase 3); merging the Smart and difficulty-routing switches into one user-visible toggle (rejected, §9); routing-decision visibility for background turns (Layer 3 — deferred, but see §7).

## 1. The incident this comes from

An agent running scheduled image-recognition turns had its model silently
substituted by Smart background routing. Recognition quality dropped hard and
the user had no way to know. Three independent facts make that outcome
inevitable rather than unlucky:

1. **Auto-pick does not consider capability.** `pick_cheap_model`
   (`mur-common/src/model.rs:293`) filters on exactly one predicate:
   `capabilities` contains `chat` **or is empty**. No vision vocabulary exists
   anywhere: no registry write path (`mur model add`, `mur model connect`)
   emits one, and observed registries carry only `chat` and `tools`, with most
   entries carrying no `capabilities` key at all. Every cheap text-tier model
   is therefore an eligible candidate for an image request.
2. **Cascade cannot catch a quality failure.** Escalation fires on
   `LlmError::InvalidResponse` (malformed/empty output). A confidently wrong
   recognition is well-formed, so the cascade never runs. Quality-based
   re-routing is explicitly out of scope — meaning quality-sensitive work
   currently has *no* backstop under Smart.
3. **The transparency promise does not cover background turns.** The
   Phase A+B caption (`⚡ model · Smart (background)`) renders in Hub chat.
   Smart only acts on **background** turns — scheduled tasks and companion
   outbox generation — which have no chat bubble to carry the caption.

A fourth fact would reproduce the same failure through the other mechanism:
the token estimator counts an `ImageText` message as `text.len()`
(`mur-agent-runtime/src/llm/fallback/mod.rs:482`), so "image + short caption"
estimates as trivially easy and difficulty routing also sends it to `cheap`.

## 2. First principle

> **A router may only substitute a model that can satisfy the request. Price
> orders the eligible set; it does not define membership.**

This is the load-balancer invariant. Today price *is* the only criterion —
"can this model do this job" is absent from the decision. Every symptom above
is that one omission.

Two consequences shape the whole design:

- **Fail-closed above the baseline, per requirement.** An entry with no declared
  capabilities is assumed chat-capable (the existing legacy clause, which must
  stay or every pre-capability entry drops out). Above the baseline the rule
  follows the *failure mode*, not a blanket policy:
  - **Vision — silence disqualifies.** A model that cannot see answers with
    confident nonsense: silent, and unrecoverable for that turn. Unstated is
    not permission.
  - **Tools — silence is permitted.** A tool-incapable model is rejected by the
    provider, loudly, and the existing retry/advance path already handles it.
    Treating silence as incapacity would drop every pre-`capabilities` entry —
    most of a real registry — out of the fallback chain of every tool-carrying
    turn, a large regression bought for very little. (Confirmed against a live
    registry: the entries in an actual global chain carry no `capabilities` key
    at all.)

  An entry that *does* declare capabilities is taken at its word either way:
  enumerating what you can do and leaving `tools` out is a statement, not
  silence.
- **Automation appetite tracks failure mode, not preference.** The Phase A+B
  spec turned Smart on by default because background failures were judged
  loud (structural) and correctable (one-click re-run). The incident shows the
  real failure is silent and irreversible for that turn. Silent + irreversible
  ⇒ opt-in.

## 3. Layer 1 — Capability gate (mechanism)

### 3.1 Requirements derived from the request

```rust
// mur-common/src/model.rs — pure, testable, no LlmRequest dependency
pub enum Requirement { Vision, Tools }

/// Baseline (chat) stays legacy-permissive: empty capabilities = chat-capable.
/// Everything above baseline is fail-closed: the entry must declare it.
pub fn satisfies(e: &ModelEntry, reqs: &[Requirement]) -> bool;

/// Cheapest chat-capable entry that ALSO satisfies `reqs`.
pub fn pick_cheap_model(reg: &ModelRegistry, exclude: Option<&str>,
                        reqs: &[Requirement]) -> Option<String>;
```

```rust
// mur-agent-runtime — LlmRequest lives here
fn requirements_of(req: &LlmRequest) -> Vec<Requirement> {
    // messages contains RichMessage::ImageText  -> Vision
    // !req.tools.is_empty()                     -> Tools
}
```

`Requirement::permitted_when_undeclared` carries the per-requirement rule from
§2, so `satisfies` enforces a declared capability list exactly and treats an
absent one according to which failure the requirement guards against.

The crate split follows the existing rule: the pure predicate lives in
`mur-common` (below both `mur-core` and `mur-agent-runtime`); only the
derivation from `LlmRequest` lives in the runtime.

**Why this is simultaneously the minimal and the complete fix:** no registry
entry declares `vision` today, so an image request finds no eligible cheap
candidate and Smart goes inert — the exact behaviour a hardcoded "images are
never downgraded" check would produce, with no migration. When vision
declarations later arrive (models.dev carries modality data; `mur model
connect` already reads that catalog), the same code starts making the finer
distinction with no second change. There is no minimal-version/full-version
fork to schedule.

### 3.2 Where the filter applies

Applied in `FallbackLlmClient::candidates_for`
(`mur-agent-runtime/src/llm/fallback/mod.rs:143`), to **automatic
substitutions only**:

| Candidate source | Filtered | Why |
|---|---|---|
| `req.pin_model_ref` | No | User re-run. Explicit beats smart. |
| Primary (`model_ref` / `models.default`) | No | The user configured it; it must fail loudly, not be second-guessed. |
| Smart auto-pick `cheap` | **Yes** | Chosen by MUR for this request. |
| Difficulty-routed `cheap`/`frontier` | **Yes** | Config declares a policy, not this request's model. Also closes the `text.len()` estimator hole (§1.4). |
| `fallback_chain` members | **Yes** | Consumed automatically on failure. An ineligible member is skipped like any other unusable candidate. |

If filtering empties the candidate list, the existing exhaustion path returns
the last error. A loud failure is the correct outcome — never answer an image
request with a blind model.

`satisfies` never changes `classify()` or the Phase-1 retryable/fatal
boundary; it only shortens the candidate list before the loop starts.

## 4. Layer 2 — Policy (opt-in + three-state inheritance)

### 4.1 The defect being fixed

`profile.routing` is `Option<RoutingConfig>`
(`mur-common/src/agent.rs:88`) and is consumed with
`profile.routing.clone().unwrap_or_else(|| cfg.routing.clone())`
(`fallback/mod.rs:146`). The full config struct is being used as a partial
override, so it has only two states — all or nothing — and any field the agent
omits silently takes the serde default instead of the global value.

The compound trap: disabling Smart for one agent requires writing
`routing: { smart: { enabled: false } }`, because the per-agent Smart override
is nested inside `routing` (`config.rs:110`) — and that write simultaneously
sets that agent's difficulty-routing `enabled` to `false`. Two mechanisms, one
keystroke, no diagnostic.

### 4.2 Types

```rust
// mur-common/src/config.rs — partials are their own type; None = inherit
pub struct SmartOverride {
    pub enabled: Option<bool>,
    pub cheap: Option<String>,
    pub max_escalations: Option<u32>,
}
pub struct RoutingOverride {
    pub enabled: Option<bool>,
    pub cheap: Option<String>,
    pub frontier: Option<String>,
    pub threshold_input_tokens: Option<u32>,
}

impl SmartConfig   { pub fn merged(&self, ov: Option<&SmartOverride>)   -> SmartConfig; }
impl RoutingConfig { pub fn merged(&self, ov: Option<&RoutingOverride>) -> RoutingConfig; }
```

A distinct override type is load-bearing, not ceremony: the bug in §4.1 is
caused by a full struct standing in for a partial one. The type must say
"I am a partial" so a missing field cannot mean `Default::default()`.

### 4.3 Profile shape

`AgentProfile` gains `smart: Option<SmartOverride>` **promoted out of
`routing`** — the nesting is what creates the compound trap. `routing` becomes
`Option<RoutingOverride>`.

Resolution order for the effective Smart config:

1. `profile.smart` (new location)
2. `profile.routing.smart` (legacy nested location — read-only compatibility)
3. global `config.models.smart`

### 4.4 Default and boot gate

- `SmartConfig::default().enabled` flips **`true` → `false`** (`config.rs:51`).
- `supervisor_runner.rs:474` currently short-circuits to a plain single client
  on `refs.len() <= 1 && !routing.enabled`, **without consulting
  `smart.enabled`**. Consequence today: an agent with no fallback chain and no
  difficulty routing never runs Smart at all, regardless of the global toggle —
  the switch says "on" and nothing happens. The gate gains the effective Smart
  check so the toggle stops lying in both directions.

### 4.5 Surfaces

- CLI. Nothing today writes `models.smart` or `models.routing` from the command
  line — `mur model default` / `fallback` and `mur agent fallback` cover only
  the chain, and `mur model route` is the unrelated cost-router namespace
  (role-based spawn routing, `route estimate`). This is the one gap in the
  model/switch surface:
  - `mur model smart <on|off> [--cheap <ref>]` — global
  - `mur agent smart <name> <on|off|follow>` — per-agent, `follow` clears the
    override
  - Both validate refs against `models.yaml` fail-closed, matching
    `mur model fallback` / `mur agent fallback`.
- Hub: the Smart row in `ModelsSettings.tsx` stays a two-state toggle (global);
  the per-agent control is **three-state** (`follow` / `on` / `off`) — inherit
  is a first-class displayed state, not something inferred from a blank field.
  `validate_refs` in `mur-hub-gui/src-tauri/src/model_switch.rs` extends to the
  override's `cheap`. i18n keys land in **both** `en.ts` and `zh-TW.ts`.
  Beware the known narrow-DTO hazard: `model_switch_set` is a full-object write,
  so any new field must round-trip through the Hub DTO or it is erased on save.

### 4.6 Migration

None, deliberately.

`RoutingOverride.enabled` is `Option<bool>`, so a legacy profile that wrote
`enabled: false` explicitly still deserializes to `Some(false)` and keeps its
behaviour exactly. Only a profile that *omitted* `enabled` changes meaning —
from "disabled" (the serde default) to "inherit global".

That difference is observable in exactly one configuration: global difficulty
routing on **and** a hand-written per-agent `routing:` block that omits
`enabled`. There, the old behaviour was itself the bug — omission was the only
way to express a partial override, and it silently disabled the mechanism the
user had just enabled globally. The new reading is what the author meant.

A file-rewriting migration was considered and rejected. `config_migrate.rs`
operates on the global `config.yaml` **text** and is wired into
`Config::load_or_default` / `save_config_at`; it never sees
`agents/*/profile.yaml`. Honouring the old reading would mean a new
profile-rewriting pass over every agent on disk to defend a configuration that
needs two uncommon conditions to coincide.

Exported `.muragent` bundles carry old profiles, so the legacy nested read
(§4.3 step 2) is permanent, not transitional.

## 5. Data flow

```
LlmRequest ──▶ requirements_of()  ──┐
                                    ├─▶ candidates_for()
profile.smart ─┐                    │     • pin?      -> [pinned]           (unfiltered)
profile.routing├─ merged(global) ───┘     • primary   -> kept               (unfiltered)
global models  ┘                          • cheap/chain -> satisfies(reqs)? (filtered)
                                          └─▶ [eligible candidates]
                                                └─▶ existing retry/cascade loop (unchanged)
```

## 6. Failure modes

| Situation | Behaviour |
|---|---|
| Image request, no entry declares vision | Smart inert; primary used. Fail-expensive. |
| Image request, primary itself cannot see | Primary is unfiltered → provider errors loudly. Not the router's call to silently fix. |
| Tools requested, cheap entry declares `capabilities` without `tools` | Cheap skipped; next eligible candidate. |
| Tools requested, entry declares no `capabilities` at all | Kept. Silence is permitted for tools (§2); a genuinely tool-incapable model fails loudly and the chain advances. |
| All candidates filtered out | Exhaustion path returns the last error (loud). |
| `smart.cheap` pinned by the user to an ineligible ref | Filtered like any substitution; a pinned *cheap* is a policy, not a per-request choice. Hub/CLI validate the ref exists but cannot validate per-request eligibility. |
| Legacy profile with nested `routing.smart` | Read at §4.3 step 2; unchanged behaviour. |

## 7. Layer 3 — Visibility (shipped as `mur agent routing`)

Background routing decisions are recorded (`mur.routing` telemetry, Phase B)
but had no read surface outside Hub chat captions, which background turns never
reach — so the least visible decisions were the ones nothing rendered.

`mur agent routing <name> [--limit N] [--downgrades-only]` is that surface. It
reads `<agent_home>/telemetry/*.jsonl`, prints decisions newest first, and marks
with `↓` the turns where `reason == smart-background` — the ones MUR chose for
you rather than ones you configured. The summary line counts every decision on
disk rather than the rows printed, because a "no downgrades" line speaking only
for the last 20 turns would be the same half-truth the command exists to remove.

The dependency this section originally recorded is therefore satisfied:
**restoring `smart.enabled` to a default of `true` required a
background-visible routing surface first.** That is not itself an argument for
flipping it back — the default remains off on the reasoning in §2 — only a
statement that the blocker is gone.

Surfaces deliberately not built: a Hub notification on substitution, and a
provenance line on companion/scheduled output. Both need UI work; the CLI reader
answers the question with the telemetry that already exists.

## 8. Testing

Unit (`mur-common`):
- `satisfies`: empty capabilities → chat yes, vision no, tools no; declared
  vision → yes; declared `tools` only → tools yes, vision no.
- `pick_cheap_model` with `[Vision]` against a registry where nothing declares
  vision → `None` (the incident, as a regression test).
- `merged`: `None` field inherits global; `Some` overrides; per-field
  independence (overriding `cheap` does not disable `enabled`).

Unit (`mur-agent-runtime`):
- `requirements_of`: `ImageText` → `Vision`; non-empty `tools` → `Tools`;
  plain text → empty.
- `candidates_for`: pinned and primary survive filtering; Smart cheap and chain
  members are filtered; empty result → exhaustion error, not a blind call.
- Boot gate: agent with one ref and Smart effective-on builds the routed client
  (regression for §4.4).

Config/back-compat:
- Legacy `models:` without `smart:` → `enabled: false` (new default).
- Legacy profile with nested `routing.smart` still resolves.
- Legacy profile with an explicit `routing.enabled: false` still resolves to
  `Some(false)` — no silent re-enable (§4.6).

## 9. Rejected alternatives

- **Only flip the default to off.** Leaves the mechanism defect intact: anyone
  who later turns Smart on — including a future reader of this spec — hits the
  identical failure, now with "you enabled it yourself" as the explanation. It
  also converts a mechanism gap into a preference setting, which is how such
  gaps stop getting fixed.
- **Hardcode "never downgrade image requests".** Same immediate effect as §3.1
  but does not generalise, and has to be torn out when capability data arrives.
  The declared-capability rule costs the same and upgrades itself.
- **Quality-based re-routing** (judge the output, re-run if poor). Cannot
  determine quality without a second expensive call, doubles cost on exactly
  the turns Smart exists to make cheaper, and remains out of scope per the
  Phase A+B spec.
- **A per-task "do not downgrade" flag.** A third knob for what per-agent
  `off` already covers. YAGNI.
- **One user-visible "smart routing" switch gating both mechanisms.** Their
  blast radii differ by an order of magnitude — Smart touches background turns
  only, difficulty routing swaps the model on *interactive* turns by token
  count. One click producing two classes of behaviour change is the opposite of
  the transparency principle. What they should share is the inheritance
  semantics (§4.2), not the switch.

## 10. Phasing

| Phase | Content | Gate |
|---|---|---|
| L1 | `Requirement`/`satisfies`/`pick_cheap_model` + `requirements_of` + `candidates_for` filter + tests | Ships alone; fixes the incident without any config change |
| L2 | Override types, profile promotion, default flip, boot gate, CLI, Hub three-state | Depends on L1 (turning the switch on must already be safe) |
| L3 | `mur agent routing` — background-visible routing surface | **Shipped.** Was the precondition for ever defaulting Smart back on (§7) |

L1 is deliberately shippable on its own: it makes the current default-on
configuration safe for users who never read this spec.
