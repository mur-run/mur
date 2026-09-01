# Reasoning effort: one scale, five provider shapes, two surfaces

Status: designed (2026-09-01). **Not implemented** — no code in this repo
implements `effort_shape`, `/effort`, or the Hub control described below.
Everything under "What already exists" was read from the tree and is true
today; everything under "Design" is a proposal.

## Problem

`mur agent effort <name> <level>` sets an agent's reasoning effort, and the
runtime applies it per call. There is no way to change it from inside a
conversation, which is when you actually want to: you hit a hard sub-problem
three turns into a session and the only lever is to quit, run a CLI command,
and restart the agent, because the runtime reads `profile.yaml` once.

That is the surface gap. Underneath it is a second, larger one: the mapping
from MUR's `Effort` scale to a provider's wire parameter exists for exactly
two vendors. Every other model silently discards the setting.

## What already exists

Read from the tree, not recalled.

| Piece | Location |
|---|---|
| `Effort` enum (Low/Medium/High/Xhigh/Max), `ALL`, `as_str`, `FromStr` | `mur-common/src/llm.rs:85` |
| `supported_effort()` — Anthropic, narrows per model, `xhigh` → `high` on older lines | `mur-common/src/llm.rs` |
| `openai_reasoning_effort()` — gated on families `gpt-5\|o1\|o3\|o4` | `mur-common/src/llm.rs:194` |
| OpenAI client actually sends it | `mur-agent-runtime/src/llm/openai.rs:304,381` |
| Anthropic client actually sends it | `mur-agent-runtime/src/llm/anthropic.rs:573,650` |
| `AgentProfile.effort: Option<Effort>` | `mur-common/src/agent.rs:72` |
| `mur agent effort` CLI | `mur-core/src/cmd/agent/effort.rs`, `dispatch.rs:1591` |
| Runtime applies per call | `task_runner.rs:434` (`with_effort`) |

Two gaps found while reading:

- **`SlashCmd` has no `Effort` variant** (`mur-core/src/cmd/agent/cli/app.rs:158`).
  `/effort` in murmur is not broken; it was never built.
- **The Ollama client reads thinking but never sends it.** `ollama.rs` forwards
  the `message.thinking` field from responses (~line 171) and never sets the
  request's `think` parameter. Every local-model user has no effort control at
  all, on a runtime that supports one.

## What other tools do

Researched 2026-09-01 rather than recalled, because the vendor parameter shapes
are the part this design turns on.

**Pi** (`earendil-works/pi`) abstracts providers into one `ThinkingLevel`
(`off/minimal/low/medium/high/xhigh/max`), exposes `/effort`, cycles with a
keyboard shortcut, and persists the choice as `defaultThinkingLevel`. Its RPC
has `get_available_thinking_levels`, which returns `["off"]` for a model with
no reasoning support. Custom models declare their own `thinkingLevelMap`.

**Inspect AI** takes the same approach from the eval side: one option name over
a superset scale, mapped per provider, with each provider's own default when
unset.

**Cursor** went the other way and made effort a property of the *model entry*
in the picker (None/low/medium/high/very high). Users report effort variants
disappearing from the chat dropdown while still present in settings — one fact,
two surfaces, disagreeing. Not the model to copy.

The convergent lesson from the two good implementations: **the set of available
levels is a property of the resolved model, queried at runtime — not a global
constant.** MUR already half-does this in `supported_effort`.

## The five shapes

Provider controls do not share a shape. Compiled from vendor docs:

| Shape | Models | Wire |
|---|---|---|
| `Graded(&[Effort])` | Claude, gpt-5/o-series, DeepSeek V4, Grok, Gemini 3+, Mistral Small 3+/4 | a level name |
| `Binary { on_at }` | Qwen3/3.5, GLM (Z.ai) | a boolean |
| `Budget(&[Effort])` | Gemini 2.5, Claude on Bedrock | an integer token count |
| `AlwaysOn` | Magistral | **nothing — sending the parameter is an error** |
| `None` | gpt-4o, Llama, pre-4.5 Claude | — |

Three of these are load-bearing in ways a flat "level → string" table cannot
express:

**`AlwaysOn` is not `None`.** Mistral's dedicated Magistral models always
reason and reject `reasoning_effort` with **HTTP 422**. `None` means "no
control, and passing nothing is correct". `AlwaysOn` means "it reasons, and
passing anything is a hard failure". Treating Magistral as `Graded` breaks
every call. A table mapping levels to strings has no cell that says *do not
send this field*.

**`Binary` is not a degenerate `Graded`.** Qwen and GLM have an on/off switch,
not a dial, and they disagree on its spelling — Qwen uses
`chat_template_kwargs: {enable_thinking: bool}`, Z.ai uses
`thinking: {type: "enabled"|"disabled"}`. Pi shipped this exact confusion
(issue #2025: Qwen's parameter sent to Z.ai, so thinking stayed on when the
user turned it off) and OpenClaw shipped a stale-parameter variant of it
(#97772). Two mature products got it wrong the same way, which is the argument
for one typed table rather than per-client knowledge.

**`Budget` takes no level ON THE WIRE.** Gemini 2.5 accepts only
`thinkingConfig.thinkingBudget` as an integer (0 disables, -1 dynamic), while
Gemini 3+ accepts `thinkingConfig.thinkingLevel` as a name — same vendor,
different parameter across generations. Level→token conversion follows the
LiteLLM convention (low ≈ 1024, medium ≈ 8–16K, high ≈ 32K+) and is documented
as approximate: newer OpenAI models treat effort as a **ceiling, not a floor**,
so the mapping is not bidirectional and must not be presented as exact.
The variant still carries a level list, because the user and both UIs deal in
levels — only the client converts, at the last possible moment.

## Capability is version-scoped, not family-scoped

Grok demonstrates why a family prefix is not enough:

- `grok-4.3` — none / low / medium / high
- `grok-4.5` — low / medium / high, **cannot disable reasoning**
- `grok-4.6` — adds `xhigh`; older models silently treat `xhigh` as `high`

This is the same shape as Claude's `4-5` / `4-6` / `4-7` / `5` tiers that
`supported_effort` already encodes, and it is encoded the same way: **named
`const` lists per capability tier**, not model IDs buried in conditionals. The
existing function's own doc comment gives the reason — one place to edit when a
model ships.

Gemini needs the same treatment for a stronger reason: its Pro models reject
`minimal`, and thinking cannot be disabled at all on some 3.x lines.

## Design

### 1. `effort_shape()` — one owner for the knowledge

```rust
// mur-common/src/llm.rs
pub enum EffortShape {
    /// Levels this model actually accepts, cheapest first.
    Graded(&'static [Effort]),
    /// Thinking is a switch; `on_at` is the lowest level that turns it on.
    Binary { on_at: Effort },
    /// Takes an integer token budget rather than a level name. Still carries
    /// the levels to OFFER: the user picks a level, the client converts. A
    /// bare `Budget` would leave `/effort` and the Hub with nothing to list.
    Budget(&'static [Effort]),
    /// Always reasons and REJECTS the parameter. Send nothing.
    AlwaysOn,
    /// No reasoning control.
    None,
}

pub fn effort_shape(model: &str) -> EffortShape
```

`supported_effort()` and `openai_reasoning_effort()` become thin callers.
Their observable behavior does not change, so the two paths that work today
carry no regression risk.

Initial table (breadth-sampled from vendor docs, not from one user's registry):

```
Graded([Low,Medium,High,Xhigh,Max])  claude-opus-5/4-8/4-7, claude-sonnet-5,
                                     claude-fable-5, claude-mythos-5
Graded([Low,Medium,High])            claude-opus-4-6/4-5, claude-sonnet-4-6
Graded([Low,Medium,High])            gpt-5, o1, o3, o4
Graded([Low,High,Max])               deepseek-v4-*        ← no Medium
Graded([Low,Medium,High])            grok-4.3, grok-4.5
Graded([Low,Medium,High,Xhigh])      grok-4.6+
Graded([Low,Medium,High])            gemini-3*            ← thinkingLevel
Budget([Low,Medium,High])            gemini-2.5*          ← thinkingBudget int
Graded([Low,Medium,High])            mistral-small-3*/4*
AlwaysOn                             magistral-*          ← 422 if sent
Binary { on_at: Medium }             qwen3*, glm-*
None                                 everything else
```

Two narrowings the table performs silently, recorded here so they are not
mistaken for omissions:

- **Vendor levels below `Low` are dropped.** `grok-4.3` accepts `none`,
  Gemini accepts `minimal`, and OpenAI accepts both. MUR's `Effort` starts at
  `Low` and this design does not extend it (see *Deliberately not built*), so
  those levels are simply not offered. Nothing is mis-sent; a level MUR cannot
  name is a level MUR does not expose.
- **Vendor levels above `Max` do not exist**, so no clamping is needed at the
  top beyond the per-tier lists above.

Matched against the bare model id with any `vendor/` prefix stripped, as
`openai_reasoning_effort` already does — OpenRouter names models
`openai/gpt-5`, and `google/gemini-3.6-flash` must not match a `gpt-5` prefix.

**`provider:` in `models.yaml` must not be used as the key.** It records the
wire protocol, not the vendor: DeepSeek, Qwen, and every other
OpenAI-compatible third party are all written `provider: openai`.

### 2. Shape belongs to the model; wire format belongs to the client

The same `qwen3` is `think: "high"` through Ollama and
`chat_template_kwargs.enable_thinking` through a bare OpenAI-compatible server.
Shape is identical, transport differs. So:

- `effort_shape(model)` answers *which levels this model understands*
- each client renders that onto its own wire — `anthropic.rs` writes
  `output_config.effort`, `openai.rs` writes `reasoning_effort`, `ollama.rs`
  writes `think`, a Gemini client writes `thinkingConfig`

This split is what lets the Ollama gap close without the knowledge being
duplicated.

### 3. murmur `/effort`

```
/effort                    list the levels this model accepts, the current
                           value, and where that value came from
/effort <level>            this session only (default)
/effort <level> --save     this session and profile.yaml
```

Session-scoped by default because the two surfaces answer different questions:
`mur agent effort` describes the agent's **job** (a build specialist earns
`xhigh`, a fan-out research worker `medium`) and is rightly persistent, while
`/effort` means *think harder about this one thing*.

Hot-swap needs a new A2A method `effort/set`, mirroring `model/set`
(`supervisor.rs:1065`). Effort is a per-call parameter
(`task_runner.rs:434`), so setting the field takes effect on the next turn —
cheaper than a model swap and requiring no restart.

On a `None` or `AlwaysOn` model, `/effort <level>` must say so rather than
accept silently. On `Binary`, it shows the switch, not five levels.

### 4. Hub — `BehaviorTab`

The Hub already has the per-agent surface. `AgentInspector.tsx` has tabs
`persona · style · behavior · skills · mcp · plugins · permissions · schedule`,
and `BehaviorTab.tsx` already renders **radio cards** (label plus description
per option) for `behavior_preset`. Effort is a second group in that tab, reusing
the component. Writes go through the existing
`invoke<AgentDetail>("update_agent_detail", …)` with a `DetailPatch` — the same
command that already carries `model_ref`.

Hub writes the profile by default, because a settings pane is a statement about
the agent, not about one conversation.

Two constraints that the existing code does *not* already satisfy:

- **The options list must come from the backend.** `BehaviorTab`'s current
  radio cards are a frontend `const`. Effort's are model-dependent, so
  `AgentDetail` carries `effort` and `effort_levels`, computed from
  `effort_shape()`. Copying the `const` pattern would offer `medium` on
  `deepseek-v4-pro` (which has none) and `xhigh` on older Claude (a 400).
- **Changing the model can strand the effort value.** `model_ref` and `effort`
  travel in the same `DetailPatch`. An agent on `claude-opus-5` at `xhigh`
  switched to `deepseek-v4-pro` has an effort the new model does not accept.
  `update_agent_detail` must re-narrow on model change and report what it did.
  The same holds for murmur's `/model` hot-swap.

### 5. One derivation for "the effort in force"

`effective_effort(agent) -> (Effort, Source)` where `Source` is
`SessionOverride | Profile | Unset`. murmur and the Hub both read it; neither
computes its own. Without this, murmur shows the session value and the Hub
shows the profile value, and the user has two answers to one question — the
failure this repo has already shipped as "Hub reports every agent idle" and
"Hub wrote the vendor name as `provider`".

## Deliberately not built

**OpenRouter's unified `reasoning: {effort|max_tokens}`.** It is an
*endpoint*-level concern: the same `anthropic/claude-opus-5` routed through
OpenRouter should use OpenRouter's normalization, which is keyed on base URL,
not model id. Folding it into a model-id table makes both dirty. Separate
issue.

**The Qwen/GLM wire path.** Their shapes are in the table, so `/effort`
reports honestly that these models have a switch rather than a dial — but
`chat_template_kwargs` and `thinking: {type}` are not sent yet. There is no
endpoint here to test against, and this is precisely where Pi and OpenClaw
shipped bugs. Writing an unverified vendor parameter is the failure mode, not
the fix for it.

**Adding `Off`/`Minimal` to `Effort`.** Several vendors have them. Extending
the enum changes a persisted profile field, which is a migration, and nothing
in this design needs them. `Binary { on_at }` covers the only case where "off"
is currently reachable.

## Testing

`effort_shape` is a pure function over a string; a table test per vendor tier
is the whole suite. Three negative cases carry the design and must fail if the
guards are removed:

1. `deepseek-v4-pro` must not offer `Medium` — it has `low/high/max`.
2. `google/gemini-3.6-flash` must not match the `gpt-5` family — the
   `vendor/` prefix is stripped before matching, and a substring match here
   would silently mis-map an entire vendor.
3. `magistral-*` must return `AlwaysOn`, and no client may send a parameter for
   it — the failure is a 422, not a degradation.

`/effort` parsing follows the existing `SlashCmd` test shape
(`cli/app.rs:1830`). The Hub's model-change re-narrowing needs a test that
switches `model_ref` while an incompatible effort is set and asserts the
returned `AgentDetail` reports the narrowed value.

## Where this lands in code

| Concern | Location |
|---|---|
| Shape table, the only place vendor knowledge lives | `mur-common/src/llm.rs` |
| Effective value + its source | `mur-common/src/llm.rs` |
| Wire rendering, per transport | `mur-agent-runtime/src/llm/{anthropic,openai,ollama}.rs` |
| `effort/set` A2A method | `mur-agent-runtime/src/supervisor.rs` |
| `/effort` slash command | `mur-core/src/cmd/agent/cli/app.rs` |
| Persistent CLI form (exists) | `mur-core/src/cmd/agent/effort.rs` |
| Hub control | `mur-hub-gui/ui/src/components/inspector/tabs/BehaviorTab.tsx` |
| Hub transport | `update_agent_detail`, `DetailPatch`, `AgentDetail` |

## Sources

Vendor documentation and implementations consulted 2026-09-01:
x.ai reasoning guide; Google Gemini thinking docs; OpenRouter reasoning-tokens
guide; Ollama thinking capability docs; Mistral Magistral announcement;
DeepSeek API changelog; Qwen quickstart; Z.ai GLM-4.6 migration guide; Pi
model-resolution and thinking-levels reference and issue #2025; OpenClaw issue
#97772; Inspect AI reasoning reference; Cursor community reports on missing
effort variants.
