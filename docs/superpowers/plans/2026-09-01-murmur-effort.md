# Plan — reasoning effort: `effort_shape`, `/effort`, Hub control

> **Execute with `mur-executing-plans`. Phase 1 only — Tasks 1–4.** Tasks 1–3
> are pure functions in `mur-common` with no runtime dependencies; Task 4 is one
> client change that closes a live gap. Tasks 5–8 are marked *not yet
> executable* and say why. Do them in order — each Interfaces block names what
> the previous task produced.

Design: `docs/superpowers/specs/2026-09-01-murmur-effort-design.md`.

## Goal

Let a user change an agent's reasoning effort from inside a murmur conversation
and from the Hub, over a vendor table that covers more than two providers.

## Architecture

One function in `mur-common`, `effort_shape(model) -> EffortShape`, owns every
fact about which reasoning levels a model accepts and in what form. Each LLM
client asks it and renders the answer onto its own wire format — Anthropic
writes `output_config.effort`, OpenAI writes `reasoning_effort`, Ollama writes
`think`. Two surfaces set the value: murmur's `/effort` (this session by
default) and the Hub's Behavior tab (the profile), and both read one
`effective_effort()` so they never disagree about what is in force.

## Tech stack

Rust (edition 2024) for `mur-common`, `mur-core`, `mur-agent-runtime`.
TypeScript + React for `mur-hub-gui/ui`. Tauri 2 commands as the Hub transport.

## Global Constraints

Copied from the spec. Every task implicitly includes all of them.

- `provider:` in `models.yaml` must not be used as the key. It records the wire
  protocol, not the vendor: DeepSeek, Qwen, and every other OpenAI-compatible
  third party are all written `provider: openai`.
- Match against the bare model id with any `vendor/` prefix stripped —
  OpenRouter names models `openai/gpt-5`, and `google/gemini-3.6-flash` must
  not match a `gpt-5` prefix.
- Shape belongs to the model; wire format belongs to the client.
- `AlwaysOn` means send nothing. Sending the parameter to Magistral is HTTP
  422 — a hard failure, not a degradation.
- Capability is version-scoped, not family-scoped. Encode it as named `const`
  lists per capability tier, not model IDs buried in conditionals.
- Vendor levels below `Low` (`none`, `minimal`) are dropped. MUR's `Effort`
  starts at `Low` and this plan does not extend it.
- `Budget` level→token conversion is approximate and must be documented as
  such. Newer models treat effort as a ceiling, not a floor, so the mapping is
  not bidirectional.
- One derivation for "the effort in force". No surface computes its own.
- Repo rules: no hardcoded values (use constants), single source file ≤ 800
  lines, `cargo fmt` clean, `cargo clippy --all-targets -- -D warnings` clean.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-common/src/llm.rs` | `EffortShape`, `effort_shape()`, the vendor table | 1 |
| `mur-common/src/llm.rs` | `supported_effort` / `openai_reasoning_effort` delegate | 2 |
| `mur-common/src/llm.rs` | `EffortSource`, `effective_effort()` | 3 |
| `mur-agent-runtime/src/llm/ollama.rs` | send `think` on both request paths | 4 |
| `mur-agent-runtime/src/protocol/methods/effort_set.rs` | `effort/set` A2A handler (new file) | 5 — Phase 2 |
| `mur-agent-runtime/src/supervisor.rs` | register `effort/set` | 5— Phase 2 |
| `mur-core/src/cmd/agent/cli/app.rs` | `SlashCmd::Effort` + parse | 6— Phase 2 |
| `mur-hub-gui/src-tauri/src/detail.rs` | `effort`, `effort_levels`, re-narrow on model change | 7 — Phase 2 |
| `mur-hub-gui/ui/src/types.ts` | `AgentDetail.effort`, `effort_levels`, `DetailPatch.effort` | 7 — Phase 2 |
| `mur-hub-gui/ui/src/components/inspector/tabs/BehaviorTab.tsx` | effort radio cards | 8 — Phase 2 |

---

## Task 1 — `EffortShape` and the vendor table

**Interfaces**

Consumes: `mur_common::llm::Effort` (existing: `Low | Medium | High | Xhigh |
Max`, with `Effort::ALL`, `as_str()`, `FromStr`).

Produces:
```rust
pub enum EffortShape {
    Graded(&'static [Effort]),
    Binary { on_at: Effort },
    Budget(&'static [Effort]),
    AlwaysOn,
    None,
}
impl EffortShape { pub fn levels(&self) -> &'static [Effort] }
pub fn effort_shape(model: &str) -> EffortShape
```

**Steps**

- [x] Add this test to the existing `mod tests` in `mur-common/src/llm.rs`:

```rust
#[test]
fn effort_shape_covers_each_vendor_tier() {
    use EffortShape::*;
    assert!(matches!(effort_shape("claude-opus-5"), Graded(l) if l.len() == 5));
    // Four, not three: 4.6 keeps `max` and lacks only the `xhigh` step that
    // 4.7 inserted between them. A level set is a subset, not a prefix.
    assert!(matches!(effort_shape("claude-opus-4-6"), Graded(l) if l.len() == 4));
    assert!(matches!(effort_shape("claude-opus-4-6"), Graded(l) if l.contains(&Effort::Max)));
    assert!(matches!(effort_shape("claude-opus-4-6"), Graded(l) if !l.contains(&Effort::Xhigh)));
    assert!(matches!(effort_shape("gpt-5"), Graded(_)));
    assert!(matches!(effort_shape("grok-4.6"), Graded(l) if l.contains(&Effort::Xhigh)));
    assert!(matches!(effort_shape("grok-4.5"), Graded(l) if !l.contains(&Effort::Xhigh)));
    assert!(matches!(effort_shape("gemini-3-pro"), Graded(_)));
    assert!(matches!(effort_shape("gemini-2.5-pro"), Budget(_)));
    assert!(matches!(effort_shape("qwen3-32b"), Binary { .. }));
    // A switch offers exactly two positions, never three that collapse to two.
    assert_eq!(effort_shape("qwen3-32b").levels().len(), 2);
    assert!(matches!(effort_shape("glm-4.6"), Binary { .. }));
    assert!(matches!(effort_shape("magistral-medium-latest"), AlwaysOn));
    assert!(matches!(effort_shape("gpt-4o"), None));
    assert!(matches!(effort_shape("llama3.2:3b"), None));
}

/// The three cases the design turns on. Each must fail if its guard is removed.
#[test]
fn effort_shape_negative_cases() {
    use EffortShape::*;
    // 1. DeepSeek V4 has low/high/max and NO medium. Offering medium is a 400.
    let EffortShape::Graded(levels) = effort_shape("deepseek-v4-pro") else {
        panic!("deepseek-v4-pro must be Graded");
    };
    assert!(!levels.contains(&Effort::Medium), "deepseek has no medium: {levels:?}");
    assert_eq!(levels, &[Effort::Low, Effort::High, Effort::Max]);

    // 2. A vendor prefix must be stripped before matching, and a Google model
    //    must never match an OpenAI family prefix.
    assert!(matches!(effort_shape("openai/gpt-5"), Graded(_)));
    assert!(matches!(effort_shape("google/gemini-3.6-flash"), Graded(_)));
    assert!(!matches!(effort_shape("google/gemini-2.5-flash"), Graded(_)));

    // 3. Magistral REJECTS the parameter (HTTP 422). AlwaysOn is not None:
    //    None means "send nothing and that is correct", AlwaysOn means
    //    "send nothing or every call fails".
    assert!(matches!(effort_shape("magistral-small-latest"), AlwaysOn));
    assert!(effort_shape("magistral-small-latest").levels().is_empty());
}
```

- [x] Run `cargo test -p mur-common --lib llm::tests::effort_shape` and watch
      both tests fail to compile (`cannot find function effort_shape`).

- [x] Add to `mur-common/src/llm.rs`, immediately above `supported_effort`:

```rust
/// What form of reasoning control a model accepts.
///
/// Provider controls do not share a shape, and three of these cannot be
/// expressed by a level→string table:
///
/// * [`EffortShape::AlwaysOn`] is not [`EffortShape::None`]. Mistral's
///   Magistral models always reason and reject `reasoning_effort` with HTTP
///   422 — a hard failure, not a degradation. `None` means "passing nothing is
///   correct"; `AlwaysOn` means "passing anything breaks every call".
/// * [`EffortShape::Binary`] is not a degenerate `Graded`. Qwen and GLM have a
///   switch, not a dial, and spell it differently (`chat_template_kwargs` vs
///   `thinking: {type}`). Two other agent products shipped that confusion.
/// * [`EffortShape::Budget`] takes an integer on the wire but still carries a
///   level list, because the user and both UIs deal in levels — only the
///   client converts, at the last possible moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortShape {
    /// Levels this model accepts, cheapest first.
    Graded(&'static [Effort]),
    /// Thinking is a switch; `on_at` is the lowest level that turns it on.
    Binary { on_at: Effort },
    /// Wants an integer token budget; these are the levels to offer.
    Budget(&'static [Effort]),
    /// Always reasons and rejects the parameter. Send nothing.
    AlwaysOn,
    /// No reasoning control.
    None,
}

/// Level sets, named so the table below reads as capability tiers rather than
/// as anonymous literals. Cheapest first, matching [`Effort::ALL`].
/// A level set is an arbitrary SUBSET of [`Effort::ALL`], not a prefix of it.
/// Two tiers below have holes: Anthropic's pre-4.7 lines have `max` but no
/// `xhigh` (the `xhigh` step was inserted between them in 4.7), and DeepSeek V4
/// publishes low/high/max with no medium. Naming these by a ceiling —
/// `LEVELS_TO_HIGH` and the like — is what produced a first draft that silently
/// downgraded `max` to `high` on Opus 4.6 and broke an existing test. Name the
/// membership, not a bound.
const LEVELS_ALL: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];
/// Anthropic before 4.7: the whole scale except the `xhigh` step.
const LEVELS_NO_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];
const LEVELS_LMHX: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh];
const LEVELS_LMH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];
/// DeepSeek V4 publishes low / high / max — there is no medium step.
const LEVELS_DEEPSEEK: &[Effort] = &[Effort::Low, Effort::High, Effort::Max];
/// A switch has two positions and must offer two, not three that collapse to
/// two. `Low` is off, `High` is on; the threshold lives in `Binary::on_at`.
const LEVELS_BINARY: &[Effort] = &[Effort::Low, Effort::High];

impl EffortShape {
    /// The levels a UI should offer. Empty for the two shapes that take no
    /// level from the user.
    pub fn levels(&self) -> &'static [Effort] {
        match self {
            EffortShape::Graded(l) | EffortShape::Budget(l) => l,
            EffortShape::Binary { .. } => LEVELS_BINARY,
            EffortShape::AlwaysOn | EffortShape::None => &[],
        }
    }
}

/// Which reasoning control `model` accepts.
///
/// Keyed on the model id with any `vendor/` prefix stripped, never on the
/// registry's `provider:` field — that records the wire protocol, so DeepSeek,
/// Qwen and every other OpenAI-compatible third party all read `openai`.
///
/// Capability is version-scoped, not family-scoped: grok-4.3, 4.5 and 4.6 each
/// accept a different set, exactly as the Claude 4-5 / 4-6 / 4-7 / 5 lines do.
/// Named const lists per tier, so a new model is one edit in one place.
///
/// Vendor levels below `Low` (`none`, `minimal`) are deliberately dropped:
/// MUR's scale starts at `Low`, and a level MUR cannot name is a level MUR
/// does not offer. Nothing is mis-sent.
pub fn effort_shape(model: &str) -> EffortShape {
    /// Anthropic lines with the `xhigh` step (Opus 4.7 and later).
    const ANTHROPIC_FULL: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    /// Anthropic lines that take effort but have no `xhigh` step.
    const ANTHROPIC_NO_XHIGH: &[&str] =
        &["claude-opus-4-6", "claude-sonnet-4-6", "claude-opus-4-5"];
    /// OpenAI reasoning families. `xhigh`/`max` clamp to `high` at the wire.
    const OPENAI_REASONING: &[&str] = &["gpt-5", "o1", "o3", "o4"];
    /// DeepSeek V4 thinking effort.
    const DEEPSEEK_GRADED: &[&str] = &["deepseek-v4"];
    /// Grok lines that added the `xhigh` step.
    const GROK_XHIGH: &[&str] = &["grok-4.6", "grok-4.7"];
    /// Grok lines with low/medium/high only.
    const GROK_GRADED: &[&str] = &["grok-4.3", "grok-4.5"];
    /// Gemini 3+ takes `thinkingConfig.thinkingLevel` — a level name.
    const GEMINI_LEVEL: &[&str] = &["gemini-3"];
    /// Gemini 2.5 takes `thinkingConfig.thinkingBudget` — an integer.
    const GEMINI_BUDGET: &[&str] = &["gemini-2.5"];
    /// Mistral hybrids where `reasoning_effort` enables reasoning.
    const MISTRAL_GRADED: &[&str] = &["mistral-small-3", "mistral-small-4"];
    /// Dedicated reasoning models that REJECT the parameter with HTTP 422.
    const ALWAYS_ON: &[&str] = &["magistral"];
    /// Models whose only control is an on/off switch.
    const BINARY: &[&str] = &["qwen3", "glm-"];

    let m = model.to_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m);
    let has = |prefixes: &[&str]| prefixes.iter().any(|p| bare.starts_with(p));

    // AlwaysOn is checked first: sending anything to these is a hard error, so
    // no later arm may claim them.
    if has(ALWAYS_ON) {
        return EffortShape::AlwaysOn;
    }
    if has(ANTHROPIC_FULL) {
        return EffortShape::Graded(LEVELS_ALL);
    }
    // Anthropic pre-4.7 keeps `max`; it is only the `xhigh` step it lacks.
    if has(ANTHROPIC_NO_XHIGH) {
        return EffortShape::Graded(LEVELS_NO_XHIGH);
    }
    if has(OPENAI_REASONING) || has(GROK_GRADED) {
        return EffortShape::Graded(LEVELS_LMH);
    }
    if has(GROK_XHIGH) {
        return EffortShape::Graded(LEVELS_LMHX);
    }
    if has(DEEPSEEK_GRADED) {
        return EffortShape::Graded(LEVELS_DEEPSEEK);
    }
    if has(GEMINI_LEVEL) || has(MISTRAL_GRADED) {
        return EffortShape::Graded(LEVELS_LMH);
    }
    if has(GEMINI_BUDGET) {
        return EffortShape::Budget(LEVELS_LMH);
    }
    if has(BINARY) {
        return EffortShape::Binary {
            on_at: Effort::Medium,
        };
    }
    EffortShape::None
}
```

- [x] Run `cargo test -p mur-common --lib llm::tests::effort_shape` and watch
      both tests pass.
- [x] Run `cargo fmt -p mur-common` then
      `cargo clippy -p mur-common --all-targets -- -D warnings`. Expect no
      output beyond `Finished`.
- [x] Commit: `feat(llm): effort_shape names what reasoning control a model takes`

---

## Task 2 — Existing mappers delegate to the table

**Interfaces**

Consumes: `effort_shape(model) -> EffortShape`, `EffortShape::levels()` from
Task 1.

Produces: no signature change. `supported_effort(model: &str, want: Effort) ->
Option<Effort>` and `openai_reasoning_effort(model: &str, want: Effort) ->
Option<&'static str>` keep their behavior and become table callers.

**Correction found during execution.** The plan ordered the mutation check
before the refactor. At that point the guard *cannot* fail: `supported_effort`
does not yet read `LEVELS_NO_XHIGH`, so breaking that constant is invisible to
it (Task 1's `l.len() == 4` is what catches it there). The mutation must run
AFTER the delegation, which is where it was actually done — and where it
produced `left: Some(High), right: Some(Max)`, the silent downgrade this guard
exists to catch.

**Steps**

- [x] Add this test, which pins delegation rather than duplication:

```rust
/// Delegation must not change what the mappers return. This pins the exact
/// case a first draft of this plan got wrong: Anthropic's pre-4.7 lines keep
/// `max` and lack only `xhigh`, so a level set expressed as a ceiling silently
/// downgrades `max` to `high` here. Fails loudly if the table regresses.
#[test]
fn delegation_preserves_the_hole_in_the_pre_4_7_scale() {
    // The hole: xhigh absent, max present.
    assert_eq!(supported_effort("claude-opus-4-6", Effort::Xhigh), Some(Effort::High));
    assert_eq!(supported_effort("claude-opus-4-6", Effort::Max), Some(Effort::Max));
    // The full scale is unaffected.
    assert_eq!(supported_effort("claude-opus-5", Effort::Xhigh), Some(Effort::Xhigh));
    // A model the table calls AlwaysOn must get nothing from either mapper,
    // because sending it anything is a 422.
    assert_eq!(supported_effort("magistral-small-latest", Effort::High), None);
    assert_eq!(openai_reasoning_effort("magistral-small-latest", Effort::High), None);
}
```

- [x] Run `cargo test -p mur-common --lib llm::tests::delegation_preserves` and
      watch it PASS against the current implementation — it is written to
      describe today's behavior, which is exactly what a refactor guard must do.
      Then break it deliberately: change `LEVELS_NO_XHIGH` to drop `Effort::Max`,
      re-run, confirm it fails, and put `Max` back. A guard you have not seen
      fail is not a guard.

- [x] Replace the body of `supported_effort` with:

```rust
pub fn supported_effort(model: &str, want: Effort) -> Option<Effort> {
    let levels = match effort_shape(model) {
        EffortShape::Graded(l) => l,
        // Anthropic is Graded or nothing. Budget/Binary/AlwaysOn models never
        // reach this client, and AlwaysOn must send nothing anywhere.
        _ => return None,
    };
    if levels.contains(&want) {
        return Some(want);
    }
    // Degrade to the most expensive level this model does accept rather than
    // send one it will 400 on.
    levels.iter().rev().find(|l| **l < want).copied()
}
```

- [x] Replace the body of `openai_reasoning_effort` with:

```rust
pub fn openai_reasoning_effort(model: &str, want: Effort) -> Option<&'static str> {
    const OPENAI_REASONING: &[&str] = &["gpt-5", "o1", "o3", "o4"];
    let m = model.to_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m);
    if !OPENAI_REASONING.iter().any(|f| bare.starts_with(f)) {
        return None;
    }
    if !matches!(effort_shape(model), EffortShape::Graded(_)) {
        return None;
    }
    Some(match want {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    })
}
```

- [x] Run the whole module: `cargo test -p mur-common --lib llm`. **Every
      pre-existing test in this module must still pass** — they are the
      regression guard for this refactor. If any fails, the delegation changed
      behavior and must be fixed, not the test.
- [x] Run `cargo fmt -p mur-common` and
      `cargo clippy -p mur-common --all-targets -- -D warnings`.
- [x] Commit: `refactor(llm): the vendor mappers read the shape table`

---

## Task 3 — `effective_effort()`, one derivation

**Interfaces**

Consumes: `Effort`, `effort_shape`, `EffortShape` from Tasks 1–2.

Produces:
```rust
pub enum EffortSource { SessionOverride, Profile, Unset }
pub fn effective_effort(
    session: Option<Effort>,
    profile: Option<Effort>,
    model: &str,
) -> (Option<Effort>, EffortSource)
```

**Steps**

- [x] Add this test:

```rust
#[test]
fn effective_effort_reports_value_and_where_it_came_from() {
    use EffortSource::*;
    // Session beats profile.
    assert_eq!(
        effective_effort(Some(Effort::Low), Some(Effort::Max), "claude-opus-5"),
        (Some(Effort::Low), SessionOverride)
    );
    // Profile when there is no session override.
    assert_eq!(
        effective_effort(None, Some(Effort::Max), "claude-opus-5"),
        (Some(Effort::Max), Profile)
    );
    // Neither set.
    assert_eq!(effective_effort(None, None, "claude-opus-5"), (None, Unset));
    // A value the model cannot take is narrowed, and the SOURCE is preserved:
    // the user still set it, they just get the nearest level that works.
    assert_eq!(
        effective_effort(None, Some(Effort::Medium), "deepseek-v4-pro"),
        (Some(Effort::Low), Profile)
    );
    // A model with no control reports nothing regardless of what is stored.
    assert_eq!(
        effective_effort(Some(Effort::Max), Some(Effort::Max), "gpt-4o"),
        (None, Unset)
    );
}
```

- [x] Run `cargo test -p mur-common --lib llm::tests::effective_effort` and
      watch it fail to compile.

- [x] Add to `mur-common/src/llm.rs`:

```rust
/// Where the effort in force came from, so a surface can say so instead of
/// showing a bare number the user cannot account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSource {
    /// Set for this conversation only (murmur `/effort`).
    SessionOverride,
    /// Stored on the agent profile.
    Profile,
    /// Nothing set — the provider's own default applies. Note that is not
    /// "no effort": the API default is high.
    Unset,
}

/// The effort actually in force for `model`, and where it came from.
///
/// The ONLY derivation of this. murmur and the Hub both call it; neither
/// computes its own, because two surfaces answering one question differently
/// is a failure this codebase has already shipped twice.
///
/// A stored level the model cannot accept is narrowed to the nearest level it
/// can, and the source is preserved — the user did set it; they are simply
/// getting the closest thing that will not 400.
pub fn effective_effort(
    session: Option<Effort>,
    profile: Option<Effort>,
    model: &str,
) -> (Option<Effort>, EffortSource) {
    let (want, source) = match (session, profile) {
        (Some(e), _) => (e, EffortSource::SessionOverride),
        (None, Some(e)) => (e, EffortSource::Profile),
        (None, None) => return (None, EffortSource::Unset),
    };
    let levels = effort_shape(model).levels();
    if levels.is_empty() {
        return (None, EffortSource::Unset);
    }
    if levels.contains(&want) {
        return (Some(want), source);
    }
    match levels.iter().rev().find(|l| **l < want).copied() {
        Some(narrowed) => (Some(narrowed), source),
        // `want` is below every level this model offers; take its cheapest.
        None => (levels.first().copied(), source),
    }
}
```

- [x] Run `cargo test -p mur-common --lib llm` and watch everything pass.
- [x] Run `cargo fmt -p mur-common` and
      `cargo clippy -p mur-common --all-targets -- -D warnings`.
- [x] Commit: `feat(llm): effective_effort is the one derivation of what is in force`

---

## Task 4 — Ollama sends `think`

`ollama.rs` currently reads `message.thinking` off responses and never sets the
request's `think`. Local-model users have no effort control at all.

**Interfaces**

Consumes: `effort_shape`, `EffortShape` (Task 1). The request type already
carries `req.effort: Option<Effort>` — the OpenAI client reads it the same way
at `openai.rs:304`.

Produces: no new public names.

**Steps**

- [x] Add this test to `mod tests` in `mur-agent-runtime/src/llm/ollama.rs`:

```rust
#[test]
fn think_is_sent_only_for_models_that_take_it() {
    use mur_common::llm::{Effort, EffortShape, effort_shape};
    // A graded local model gets the level name Ollama documents.
    assert_eq!(ollama_think("qwen3-32b", Some(Effort::High)), Some("high"));
    // Binary models still resolve through the same helper — Ollama's own
    // `think` accepts levels, so the shape's level list is what matters.
    assert!(ollama_think("llama3.2:3b", Some(Effort::High)).is_none());
    // No effort requested: send nothing.
    assert_eq!(ollama_think("qwen3-32b", None), None);
    // AlwaysOn must never be sent a value.
    assert!(matches!(effort_shape("magistral"), EffortShape::AlwaysOn));
    assert!(ollama_think("magistral", Some(Effort::Max)).is_none());
}
```

- [x] Run `ORT_STRATEGY=download cargo test -p mur-agent-runtime --lib
      llm::ollama::tests::think_is_sent` and watch it fail to compile.

- [x] Add above `impl OllamaClient` in `mur-agent-runtime/src/llm/ollama.rs`:

```rust
/// The value for Ollama's top-level `think` field, or `None` to omit it.
///
/// Ollama accepts `low | medium | high | max` as well as a boolean, and
/// detects reasoning support from the model's GGUF metadata — so a level sent
/// to a model without it is ignored rather than rejected. We still gate on
/// [`effort_shape`] so behavior matches every other client: a model MUR knows
/// takes no reasoning control is not sent one, and an `AlwaysOn` model is
/// never sent a value anywhere.
fn ollama_think(model: &str, want: Option<mur_common::llm::Effort>) -> Option<&'static str> {
    let want = want?;
    let levels = mur_common::llm::effort_shape(model).levels();
    if levels.is_empty() {
        return None;
    }
    let level = if levels.contains(&want) {
        want
    } else {
        levels.iter().rev().find(|l| **l < want).copied()?
    };
    Some(match level {
        mur_common::llm::Effort::Low => "low",
        mur_common::llm::Effort::Medium => "medium",
        mur_common::llm::Effort::High => "high",
        // Ollama's top scale ends at `max`; xhigh has no separate step.
        mur_common::llm::Effort::Xhigh | mur_common::llm::Effort::Max => "max",
    })
}
```

- [x] In the non-streaming path, immediately after
      `let mut body = json!({"model": self.model, "messages": messages, "stream": false});`
      (line ~71), add:

```rust
        if let Some(think) = ollama_think(&self.model, req.effort) {
            body["think"] = json!(think);
        }
```

- [x] In the streaming path, immediately after
      `let mut body = json!({"model": self.model, "messages": messages, "stream": true});`
      (line ~126), add the identical three lines. Both request builders must
      send it, or effort silently applies only to non-streaming turns.

- [x] Run `ORT_STRATEGY=download cargo test -p mur-agent-runtime --lib llm::ollama`
      and watch it pass.
- [x] Run `cargo fmt -p mur-agent-runtime` and
      `ORT_STRATEGY=download cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`.
- [x] Commit: `fix(ollama): send the think parameter, not just read it back`

---

---

# Phase 2 — not yet executable

**Stop after Task 4.** Everything below describes behavior rather than showing
code, which `mur-writing-plans` names as a plan failure, and this section is
marked rather than pretended to be ready.

Three specific things are missing, and each one is a place where writing the
step blind produces a wrong step:

1. **Task 5 widens `TaskRunner`'s `effort` field to a shared cell.** The exact
   edit depends on how the supervisor constructs the runner and on every other
   caller of `with_effort` (there is at least one in `task_runner.rs`'s own
   tests, around line 1962). That is a cross-file change in a 2800-line file;
   it needs the real signatures in front of it.
2. **Task 5 assumes `model_name` is in scope** at the `model/set` registration
   in `supervisor.rs`. Never verified. The first draft of this plan invented a
   filename the same way (`agent_detail.rs` for `detail.rs`), which is what
   this note exists to prevent repeating.
3. **Tasks 6 and 7** describe the murmur slash handling and the Hub Rust edits
   in prose. Both need their insertion points read first.

**Phase 1 stands alone and is worth shipping alone.** After Task 4,
`mur agent effort` maps correctly for Grok, Gemini, DeepSeek and Mistral users
instead of silently discarding the setting, and Ollama users get reasoning
control for the first time. The surfaces in Phase 2 are UX on top of a
foundation that will then exist — and their signatures will be real rather than
predicted.

Specify Phase 2 against the merged Phase 1, not against this draft.

## Task 5 — `effort/set` A2A method

**Interfaces**

Consumes: `Effort`, `effort_shape`, `EffortShape` (Task 1). Mirrors the
existing `model/set` registration at `supervisor.rs:1065`.

Produces: A2A method `effort/set`, params `{"level": "<low|medium|high|xhigh|max>"}`,
result `{"applied": "<level>", "narrowed": <bool>}`. Errors with
`-32602` when the level does not parse and when the model takes no effort.

**Steps**

- [ ] Create `mur-agent-runtime/src/protocol/methods/effort_set.rs` with:

```rust
//! `effort/set` — change the running agent's reasoning effort for this
//! session, without a restart.
//!
//! Effort is a per-call parameter (`task_runner.rs`, `with_effort`), so unlike
//! a model swap this needs no reconstruction: set the field and the next turn
//! carries it. The value is deliberately NOT written to `profile.yaml` — the
//! persistent form is `mur agent effort`, and murmur's `--save` calls that.

use std::sync::Arc;

use mur_common::llm::{Effort, EffortShape, effort_shape};

/// Session effort, shared with the task runner.
pub type SessionEffort = Arc<std::sync::RwLock<Option<Effort>>>;

pub struct EffortSetHandler {
    session: SessionEffort,
    model: String,
}

impl EffortSetHandler {
    pub fn new(session: SessionEffort, model: String) -> Self {
        Self { session, model }
    }

    /// Split out from the handler so it is testable without a dispatcher.
    pub fn apply(&self, raw: &str) -> Result<serde_json::Value, String> {
        let want: Effort = raw.parse()?;
        let levels = effort_shape(&self.model).levels();
        if levels.is_empty() {
            return Err(format!(
                "model '{}' takes no reasoning effort parameter",
                self.model
            ));
        }
        let applied = if levels.contains(&want) {
            want
        } else {
            levels
                .iter()
                .rev()
                .find(|l| **l < want)
                .copied()
                .or_else(|| levels.first().copied())
                .ok_or_else(|| "no usable level".to_string())?
        };
        *self.session.write().map_err(|e| e.to_string())? = Some(applied);
        Ok(serde_json::json!({
            "applied": applied.as_str(),
            "narrowed": applied != want,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(model: &str) -> EffortSetHandler {
        EffortSetHandler::new(Arc::new(std::sync::RwLock::new(None)), model.to_string())
    }

    #[test]
    fn a_supported_level_applies_verbatim() {
        let h = handler("claude-opus-5");
        let out = h.apply("xhigh").unwrap();
        assert_eq!(out["applied"], "xhigh");
        assert_eq!(out["narrowed"], false);
        assert_eq!(*h.session.read().unwrap(), Some(Effort::Xhigh));
    }

    #[test]
    fn an_unsupported_level_narrows_and_says_so() {
        // DeepSeek V4 has no medium step.
        let out = handler("deepseek-v4-pro").apply("medium").unwrap();
        assert_eq!(out["applied"], "low");
        assert_eq!(out["narrowed"], true);
    }

    #[test]
    fn a_model_without_effort_is_an_error_not_a_silent_success() {
        let err = handler("gpt-4o").apply("high").unwrap_err();
        assert!(err.contains("takes no reasoning effort"), "{err}");
    }

    #[test]
    fn an_always_on_model_is_refused_rather_than_sent_a_value() {
        assert!(matches!(effort_shape("magistral"), EffortShape::AlwaysOn));
        assert!(handler("magistral").apply("high").is_err());
    }

    #[test]
    fn a_typo_is_reported_with_the_valid_set() {
        let err = handler("claude-opus-5").apply("hgih").unwrap_err();
        assert!(err.contains("valid:"), "{err}");
    }
}
```

- [ ] Add `pub mod effort_set;` to `mur-agent-runtime/src/protocol/methods/mod.rs`,
      in the existing alphabetical run of `pub mod` lines.
- [ ] Run `ORT_STRATEGY=download cargo test -p mur-agent-runtime --lib
      protocol::methods::effort_set` and watch all five tests pass.
- [ ] Wire the handler into the dispatcher in `mur-agent-runtime/src/supervisor.rs`,
      directly after the `model/set` registration block:

```rust
    // murmur /effort. Unlike model/set this needs no rebuild — effort is a
    // per-call parameter, so the next turn picks it up.
    d.register(
        "effort/set",
        Box::new(crate::protocol::methods::effort_set::EffortSetHandler::new(
            session_effort.clone(),
            model_name.clone(),
        )),
    );
```

- [ ] Thread `session_effort: SessionEffort` from the supervisor into
      `TaskRunner::with_effort`'s call site so the runner reads the shared cell
      instead of a fixed `Option<Effort>`: change `task_runner.rs`'s `effort`
      field to `SessionEffort`, seeded from the profile value at construction.
- [ ] Run `ORT_STRATEGY=download cargo test -p mur-agent-runtime --lib` — the
      full crate suite, because this task changed a field type the runner uses.
- [ ] Run `cargo fmt -p mur-agent-runtime` and
      `ORT_STRATEGY=download cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`.
- [ ] Commit: `feat(a2a): effort/set changes reasoning effort without a restart`

---

## Task 6 — murmur `/effort`

**Interfaces**

Consumes: the `effort/set` A2A method (Task 5), `effective_effort` and
`EffortSource` (Task 3), `effort_shape` (Task 1).

Produces: `SlashCmd::Effort { level: Option<String>, save: bool }`.

**Steps**

- [ ] Add this test beside the existing `parse_slash` tests in
      `mur-core/src/cmd/agent/cli/app.rs` (the block containing
      `assert_eq!(parse_slash("/model"), Some(SlashCmd::Model(None)));`):

```rust
#[test]
fn effort_parses_bare_level_and_save() {
    assert_eq!(
        parse_slash("/effort"),
        Some(SlashCmd::Effort { level: None, save: false })
    );
    assert_eq!(
        parse_slash("/effort high"),
        Some(SlashCmd::Effort { level: Some("high".into()), save: false })
    );
    assert_eq!(
        parse_slash("/effort high --save"),
        Some(SlashCmd::Effort { level: Some("high".into()), save: true })
    );
    // Order must not matter, and --save alone is a listing, not a write.
    assert_eq!(
        parse_slash("/effort --save xhigh"),
        Some(SlashCmd::Effort { level: Some("xhigh".into()), save: true })
    );
    assert_eq!(
        parse_slash("/effort --save"),
        Some(SlashCmd::Effort { level: None, save: true })
    );
}
```

- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
      cargo test -p mur-core --lib effort_parses_bare_level` and watch it fail
      to compile (`no variant named Effort`).

- [ ] Add to the `SlashCmd` enum in `app.rs`, directly after the `Model` variant:

```rust
    /// `/effort [level] [--save]` — list the levels this model accepts, or set
    /// one. Session-scoped unless `--save` also writes the profile.
    Effort {
        level: Option<String>,
        save: bool,
    },
```

- [ ] Add to `parse_slash`'s match, directly after the `"model" =>` arm:

```rust
        "effort" => {
            let args: Vec<&str> = words.collect();
            SlashCmd::Effort {
                level: args
                    .iter()
                    .find(|s| !s.starts_with("--"))
                    .map(|s| (*s).to_string()),
                save: args.iter().any(|s| *s == "--save"),
            }
        }
```

- [ ] Run the parse test and watch it pass.
- [ ] Handle the variant where `SlashCmd::Model` is handled, using this
      behavior: with `level: None`, print the levels from
      `effort_shape(model).levels()`, the current value and source from
      `effective_effort`, and — when the list is empty — the single line
      `this model takes no reasoning effort parameter`. With `level: Some(l)`,
      dial `effort/set`; on `narrowed: true`, print both the requested and the
      applied level. With `save: true`, additionally call
      `mur_core::cmd::agent::cmd_effort(name, Some(l), false)`.
- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
      cargo test -p mur-core --lib cmd::agent::cli` and watch the CLI suite pass.
- [ ] Run `cargo fmt -p mur-core` and
      `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo clippy -p mur-core --all-targets -- -D warnings`.
- [ ] Commit: `feat(murmur): /effort sets reasoning effort for this session`

---

## Task 7 — Hub backend: effort on `AgentDetail`, re-narrow on model change

**Interfaces**

Consumes: `effort_shape`, `effective_effort`, `EffortSource` (Tasks 1, 3).

Produces: `AgentDetail.effort: Option<String>`,
`AgentDetail.effort_levels: Vec<String>`, `DetailPatch.effort: Option<String>`.

**Steps**

- [ ] Add this test to `mod tests` in `mur-hub-gui/src-tauri/src/detail.rs` (477 lines today; the 800-line rule leaves room):

```rust
/// `model_ref` and `effort` travel in the same patch, so a model change can
/// strand an effort the new model does not accept. Switching an agent from
/// claude-opus-5 at xhigh to deepseek-v4-pro (no xhigh, no medium) must return
/// a narrowed value, not the stale one.
#[test]
fn changing_the_model_renarrows_a_stranded_effort() {
    use mur_common::llm::{Effort, effective_effort};
    let (before, _) = effective_effort(None, Some(Effort::Xhigh), "claude-opus-5");
    assert_eq!(before, Some(Effort::Xhigh));
    let (after, _) = effective_effort(None, Some(Effort::Xhigh), "deepseek-v4-pro");
    assert_eq!(after, Some(Effort::High), "xhigh must narrow, not persist");
    assert!(!effort_shape("deepseek-v4-pro").levels().contains(&Effort::Medium));
}
```

- [ ] Run `cd mur-hub-gui/src-tauri && cargo test --lib detail::tests::changing_the_model`
      and watch it fail to compile.
- [ ] Add `effort: Option<String>` and `effort_levels: Vec<String>` to the
      `AgentDetail` struct, populated as
      `effective_effort(None, profile.effort, &model_name)` mapped through
      `Effort::as_str`, and `effort_shape(&model_name).levels()` mapped the
      same way.
- [ ] Add `effort: Option<String>` to `DetailPatch`, parsed with
      `str::parse::<Effort>()` and written to `profile.effort`.
- [ ] In `update_agent_detail`, after applying the patch, recompute `effort`
      through `effective_effort` against the **post-patch** model so a
      `model_ref` change narrows a stranded value before the response is built.
- [ ] Run `cd mur-hub-gui/src-tauri && cargo test --lib detail` and watch it
      pass, then `cargo fmt` and `cargo clippy --all-targets -- -D warnings` in
      that directory. (`update_agent_detail` is also re-exported from
      `src-tauri/src/lib.rs`; the command registration there needs no change.)
- [ ] Add the same two fields to `AgentDetail` and `effort` to `DetailPatch` in
      `mur-hub-gui/ui/src/types.ts`:

```ts
  effort: string | null;
  effort_levels: string[];
```
```ts
  effort?: string;
```

- [ ] Commit: `feat(hub): agent detail carries effort and the levels its model takes`

---

## Task 8 — Hub: effort radio cards in the Behavior tab

**Interfaces**

Consumes: `AgentDetail.effort`, `AgentDetail.effort_levels`,
`DetailPatch.effort` (Task 7). Reuses the existing `radio-card` CSS classes
already used by `BEHAVIOR_OPTIONS` in this file.

Produces: no new exports.

**Steps**

- [ ] In `mur-hub-gui/ui/src/components/inspector/tabs/BehaviorTab.tsx`, add
      state and a setter beside the existing `pick`:

```tsx
  const [effort, setEffort] = useState(detail.effort);

  async function pickEffort(level: string) {
    setEffort(level);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { effort: level } as DetailPatch,
      });
      setEffort(updated.effort);
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }
```

- [ ] Render the group after the behavior-preset cards, inside the same
      `tab-form` div:

```tsx
      {detail.effort_levels.length > 0 && (
        <>
          <div className="notif-section__title">{t("detail.effort")}</div>
          <p className="field-muted" style={{ marginBottom: 12, fontSize: 12 }}>
            {t("detail.effortHint")}
          </p>
          {detail.effort_levels.map((level) => (
            <label
              key={level}
              className={`radio-card${effort === level ? " radio-card--active" : ""}${saving ? " radio-card--disabled" : ""}`}
            >
              <input
                type="radio"
                name="effort"
                value={level}
                checked={effort === level}
                disabled={saving}
                onChange={() => pickEffort(level)}
              />
              <div className="radio-card-label">{level}</div>
            </label>
          ))}
        </>
      )}
```

- [ ] **Do not add a `const EFFORT_OPTIONS` array.** The levels are a property
      of the agent's model and must come from `detail.effort_levels`. A frontend
      constant would offer `medium` on `deepseek-v4-pro` (which has no medium)
      and `xhigh` on older Claude (a 400). The empty-list guard above is what
      hides the group for a model with no reasoning control.
- [ ] Add `detail.effort` and `detail.effortHint` to **both** locale files —
      `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts` — and to the key union in
      `types.ts`, matching how `detail.behaviorHint` is declared. A key present
      in one locale and not the other fails the `TranslationKey` type check;
      a key missing from both renders as the raw key string.
- [ ] Run `cd mur-hub-gui/ui && npm run build` and confirm it completes with no
      TypeScript errors.
- [ ] Commit: `feat(hub): reasoning effort control in the Behavior tab`

---

## Self-review

**Spec-coverage walk.** Every requirement in the design points at a task:
`EffortShape` five variants → Task 1; mappers as callers → Task 2;
`effective_effort` single derivation → Task 3; Ollama gap → Task 4; `effort/set`
hot-swap → Task 5; `/effort` with session default and `--save` → Task 6; Hub
options from the backend and re-narrowing on model change → Task 7; Hub UI →
Task 8. The three "deliberately not built" items (OpenRouter unified parameter,
Qwen/GLM wire path, `Off`/`Minimal` levels) correctly have no task.

**Placeholder scan.** No `TBD`, no "add appropriate error handling", no "similar
to Task N" — Task 4's second insertion point repeats its three lines rather than
referring back, and Task 8 spells out the JSX rather than describing it.

**Cross-task type consistency.** `EffortShape` and its five variants are spelled
identically in Tasks 1, 4, 5. `effective_effort(session, profile, model)` keeps
the same three-argument order in Tasks 3, 6, 7. `EffortSource` variants
(`SessionOverride | Profile | Unset`) appear only in Task 3 and are consumed by
name in 6 and 7. `SessionEffort` is defined in Task 5 and used only there.
`effort_levels` is `Vec<String>` in Rust and `string[]` in TypeScript, and both
are produced by Task 7 and consumed by Task 8 under the same name.

**Path claims verified against the tree.** `detail.rs` (not `agent_detail.rs`
— the first draft invented that name), `protocol/methods/mod.rs` with its eight
existing `pub mod` lines, and exactly two locale files, `en.ts` and `zh-TW.ts`.

**One known soft spot, stated rather than hidden.** Task 5's last step changes
`TaskRunner`'s `effort` field from `Option<Effort>` to a shared cell. That is
the only task whose blast radius exceeds its own file, which is why its
verification step runs the whole `mur-agent-runtime` lib suite rather than one
module.
