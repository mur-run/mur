# murmur slash-command completion: every command, and arguments that follow the model

**Status**: designed, not started.
**Field report**: typing `/effort` in `murmur` offers nothing — no command row, no
levels. Six commands the parser accepts are absent from the menu entirely, and
`/effort` is absent from `/help` as well.

## Problem

`mur agent cli`'s completion menu is driven by one table in
`mur-core/src/cmd/agent/cli/complete.rs`:

```rust
const COMMANDS: &[(&str, &str, &[&str])] = &[ … ];
//                 word   desc   subcommands
```

The third field is a static list of literal words. Three consequences, one of
which is the reported bug:

1. **A command whose arguments are not a fixed list cannot be expressed at
   all.** `/effort`'s levels are a property of the agent's model, so there was
   no row to write — and `/effort` was left out of the table entirely rather
   than added with an empty argument list. Same for `/model` (registry
   aliases), `/secret` (KEY names already held), `/forget` (note names).
2. **The table is a third list** beside `parse_slash` (`app.rs`) and the `HELP`
   string (`mod.rs`). Nothing ties it to either. Six commands the parser
   accepts — `/effort`, `/login`, `/remember`, `/memories`, `/forget`, `/open` —
   never reach the menu.
3. **Layer-2 rows carry an empty `desc`.** Even the commands that do have a
   fixed argument list explain none of them.

### Why the existing guard did not catch it

`mod.rs` already has `help_lists_every_command_the_parser_accepts`, which walks
`one_of_each()` and asserts each command appears in `HELP`. `one_of_each()` is a
hand-written list of `SlashCmd` variants, and it has no `Effort` entry — so the
one command missing from `HELP` is the one command the test skips. The list's
own doc comment claims `help_name`'s exhaustive match makes coverage
structural; it does not. The match forces a *name* to exist for every variant;
it does not force that variant into `one_of_each()`.

### The vendor trap

Effort levels are **not** a universal scale. `mur_common::llm::effort_shape`
already keys them on the raw model id (never on `provider:`, which records the
wire protocol, so DeepSeek, Qwen and every OpenAI-compatible third party all
read `openai`):

| Model | Levels offered |
|---|---|
| `claude-opus-5`, `claude-fable-5`, `claude-sonnet-5` | low · medium · high · xhigh · max |
| `claude-opus-4-6`, `claude-sonnet-4-6` | low · medium · high · max (no `xhigh`) |
| `gpt-5`, `o3`, `grok-4.5`, `gemini-3-pro` | low · medium · high |
| `grok-4.6` | low · medium · high · xhigh |
| `deepseek-v4` | low · high · max (no `medium`) |
| `qwen3-*`, `glm-*` | low · high (a switch, two positions) |
| `magistral-*` | none — rejects the parameter with HTTP 422 |
| `gpt-4o` | none — no reasoning control |

A hardcoded `low medium high xhigh max` in the menu would therefore offer a
level most models do not have. The level sets are arbitrary subsets, not
prefixes of one scale, which is why `effort_shape` names membership rather than
a ceiling. The menu must call it, not restate it.

## Design

### 1. The table's third field becomes a source, not a list

```rust
enum Args {
    None,
    /// Literal words, each with a one-line description.
    Fixed(&'static [(&'static str, &'static str)]),
    /// Resolved from `MenuContext` at menu-build time.
    Effort,
    Model,
    Secret,
    Note,
}
```

`Fixed` gains a description column, so `on`/`off`, the skins, and the `mcp`
subcommands can each say what they do. Existing entries migrate with empty
descriptions where none is warranted; that is a follow-on, not a blocker.

### 2. `MenuContext` — the only thing that does I/O

```rust
pub struct MenuContext {
    /// Effort levels this agent's model accepts, each with a short hint.
    pub effort: Vec<(String, String)>,
    /// Registry aliases, described by the model id behind them.
    pub models: Vec<(String, String)>,
    /// Secret KEYs the agent already holds.
    pub secrets: Vec<String>,
    /// Agent-local note names, plus the literal `last`.
    pub notes: Vec<String>,
}
```

Built by `MenuContext::load(home, agent)`, stored on `App`, and rebuilt at
startup and after `/model`, `/secret`, `/remember` and `/forget` — the four
commands that change what these lists contain. `/model` is the load-bearing
one: it hot-swaps the model mid-session, and stale effort levels after a swap
would be exactly the wrong-by-vendor failure this spec exists to prevent.

`compute()` takes `&MenuContext` and stays a pure function. The module's
contract — "no TUI, no I/O here" — is what keeps file reads out of the
per-keystroke path, and it is preserved by construction rather than by
discipline.

Fail-soft, like `load_agent_skills`: any read error yields empty lists, and the
menu degrades to the command layer.

### 3. One resolution of the model id

The `/effort` handler (`mod.rs`) already resolves `model_ref` → registry →
`ModelEntry.model`. That block moves to `model_cmd::current_model_id(home,
agent) -> Option<String>` and both the handler and `MenuContext::load` call it.
Net deletion: the resolution exists once instead of twice, so the
"never key on `provider:`" rule is enforced in one place.

Levels then come from `effort_shape(&model_id).levels()` — the same call the
handler makes, so the menu and the command can never disagree about what a
model accepts.

### 4. Models that take no effort parameter

`levels()` returns an empty slice for `AlwaysOn` and `None` shapes. Then
`ctx.effort` is empty, `compute` finds no candidates and returns `None`, and no
layer-2 menu opens. Pressing Enter on a bare `/effort` still prints the
existing explanation ("takes no reasoning effort parameter — nothing to set").
No new branch, no menu row that leads nowhere.

### 5. The six missing commands

`/effort`, `/login`, `/remember`, `/memories`, `/forget`, `/open` join the
table. `/effort` also joins `HELP`, which never mentioned it.

Argument sources: `/effort` → `Effort`; `/login` → `Fixed` (`anthropic`,
`chatgpt`); `/forget` → `Note`; `/remember`, `/memories`, `/open` → `None`.
`/model` → `Model` and `/secret` → `Secret` are upgrades to rows that already
exist.

### 6. A guard that ties all three lists

The existing test grows a third assertion: for every `SlashCmd` variant name,
the parser accepts it, `HELP` mentions it, **and** the completion table offers
it. `Effort` is added to `one_of_each()`, which is what let this slip.

**Rejected**: generating `HELP` from the command table, so two lists become
one and drift is structurally impossible. `HELP` carries hand-written nuance a
table would flatten — the note that `/login` is unrelated to `mur auth login`,
the `!cmd` shell escape, the key bindings — and rewriting it to fit a table
makes the cheatsheet worse to read. Three lists plus one test that goes red on
divergence is the cheaper trade. Revisit if a fourth list appears.

## Testing

- **Per-shape level sets**: Opus 5 offers five levels; Opus 4.6 offers four and
  not `xhigh`; DeepSeek V4 offers no `medium`; Qwen offers two; Magistral and
  `gpt-4o` offer none and open no menu. Driven through `compute`, not through
  `effort_shape` directly, so the test covers the wiring rather than restating
  the table.
- **The three-list guard**, written first and confirmed red against today's
  tree — it must name `/effort` as both missing from `HELP` and missing from the
  menu, or it is not the guard this spec asked for.
- **`compute` does no I/O**: guaranteed by the signature; tests feed a
  hand-built `MenuContext`.
- **Freshness**: after a `/model` switch, `MenuContext` reload yields the new
  model's levels. Unit-level, against two registry entries with different
  shapes.

## Out of scope

- `/channels` completing channel numbers and titles. It needs the channel index
  on the menu path, which is a different cost class from reading one profile.
- Descriptions for the argument rows of commands that already work
  (`/skin`, `/auto`, `/verbose`, `/mcp`). The `Fixed` variant makes them
  possible; filling them in is a follow-on.
- Refreshing the skill list mid-session. `load_agent_skills` is still cached at
  startup, so `/skill add` remains invisible to the menu until restart. Same
  class of staleness, different owner.
