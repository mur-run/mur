# murmur slash-command completion — implementation plan

> **Execute with `mur-executing-plans`** (in-context, task by task). No MUR
> delegation is set up for this branch.

**Spec:** `docs/superpowers/specs/2026-09-08-murmur-slash-arg-completion-design.md`
**Branch:** `docs/murmur-slash-arg-completion-spec` (spec PR #1218); implement on a fresh branch off `main`.
**Status:** Tasks 1–4 done. One correction the plan did not anticipate is recorded below.

## Corrections found during execution

1. **A hand-written `profile.yaml` does not deserialize into `AgentProfile`** (Task 1). `current_model_ref` fails soft, so the test read `None` and asserted nothing about the resolution. It now uses `mur-common/tests/fixtures/profile_p0a_minimal.yaml` plus `write_model_ref`, the same fixture the neighbouring round-trip test uses.
2. **`sort_by` with a reversed comparator trips `clippy::unnecessary_sort_by`** (Task 2). Written as `sort_by_key(|s| std::cmp::Reverse(s.manifest.updated_at))`; CI runs clippy with `--all-targets -D warnings`.
3. **Clippy is red between Task 3 and Task 4 by construction** — `MenuContext` is dead code until `compute` takes it, and `-D warnings` implies `-D dead-code`. Task 3's steps do not lint for this reason; Task 4 is the first point where clippy can be clean.
4. **`offers` is test-only** (Task 4). Nothing in production asks it, and the `mur` binary target compiles these modules too, so an unconditional `pub fn` is dead code under `-D warnings`. It carries `#[cfg(test)]`.
5. **Task 5's two wiring lines moved into Task 4** — `MenuContext::load` stays dead code until it is called, so leaving it for Task 5 meant committing a clippy-red tree. Task 5 keeps its freshness test and the live verification.
6. **Mutation check run before committing Task 4** (not in the plan). Replacing the `Args::Effort` rows with a hardcoded `low medium high xhigh max` fails both effort tests, so they are testing the wiring rather than restating the table.

## Goal

Every slash command the parser accepts appears in murmur's completion menu, and
the commands whose arguments depend on the agent's state offer those arguments —
`/effort` first among them, with the levels the agent's own model accepts rather
than a hardcoded scale.

## Architecture

The completion table's third field changes from a static `&[&str]` to an `Args`
enum that either carries literal words or names a list on a new `MenuContext`.
`MenuContext` is the single place that reads disk (profile, model registry,
notes); it lives on `App`, is rebuilt after every slash command, and is passed
by reference into `complete::compute`, which stays a pure function on the
per-keystroke path. Effort levels come from `mur_common::llm::effort_shape`, so
the menu and the `/effort` handler cannot disagree.

## Tech stack

Rust 2024 · `ratatui` (menu rendering, untouched here) · `mur_common::llm`
(`effort_shape`, `Effort`) · `mur_common::model::ModelRegistry` ·
`mur_common::skill` (note loading) · `cargo nextest`.

## Global constraints (from the spec — every task includes these)

- Effort levels come from `effort_shape(model_id).levels()` and nowhere else. Never restate the scale, never key on `provider:` (that is the wire protocol, not the vendor).
- `complete::compute` and everything it calls stay pure: no file reads, no network, no clock. All I/O lives in `MenuContext::load`.
- Every list read is fail-soft: a read error leaves that list empty and the menu degrades, exactly as `load_agent_skills` already does.
- No hardcoded values: descriptions are UI copy, but lists of models, levels, keys and note names are always read.
- Single source file ≤ 800 lines. `complete.rs` is 377 today and finishes near 560; if a step pushes it past 800, split `MenuContext` into `menu_ctx.rs` and say so.
- Brand name in user-visible strings is uppercase `MUR`.
- Tests run under `cargo nextest`, never bare `cargo test`.

## Environment (every command in this plan assumes it)

```bash
cd /Volumes/Firecuda4tb/Projects/mur
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download RUST_MIN_STACK=33554432
```

`MUR_WEB_DIST` is required or `mur-core` will not compile (the dashboard is
embedded at build time). Baseline before any change:

```bash
cargo nextest run -p mur-core --lib cmd::agent::cli
# Summary [   0.7s] 347 tests run: 347 passed, 2369 skipped
```

## File structure

| File | Responsibility after this plan |
|---|---|
| `mur-core/src/cmd/agent/cli/model_cmd.rs` | Adds `current_model_id` — the one resolution of `model_ref` → registry → raw model id. |
| `mur-core/src/cmd/agent/cli/memory_cmds.rs` | Adds `live_note_names` — agent-local notes that are not Destroyed; `forget("last")` uses it. |
| `mur-core/src/cmd/agent/cli/complete.rs` | Adds `Args`, `MenuContext`, `offers`; rewrites the command table; `compute` takes `&MenuContext`. Still the only completion logic. |
| `mur-core/src/cmd/agent/cli/app.rs` | Adds the `menu_ctx` field. |
| `mur-core/src/cmd/agent/cli/mod.rs` | Loads and refreshes `menu_ctx`; `HELP` gains `/effort` and `/quit`; the guard test ties parser, `HELP` and menu together. |

---

## Task 1 — one resolution of the model id

`/effort` resolves the raw model id inline. `MenuContext` needs the same value,
and two copies of a rule that says "never key on `provider:`" is one copy too
many.

### Interfaces

**Produces:**
```rust
// mur-core/src/cmd/agent/cli/model_cmd.rs
pub(crate) fn current_model_id(home: &Path, agent: &str) -> Option<String>
```

### Steps

- [x] Append this test to the existing `mod tests` at the bottom of `mur-core/src/cmd/agent/cli/model_cmd.rs`:

```rust
    /// The id the effort table keys on is `ModelEntry.model` — the raw vendor
    /// id — never the alias and never `provider:`, which is the wire protocol.
    #[test]
    fn current_model_id_returns_the_raw_vendor_id() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("profile.yaml"),
            "name: a\nmodel_ref: fast\nupdated_at: '2026-09-08T00:00:00Z'\n",
        )
        .unwrap();
        // `reg` is the existing helper in this module: (alias, provider, model).
        // The provider is `openai` on purpose — DeepSeek speaks that protocol,
        // and a lookup keyed on it would resolve the wrong effort table.
        let reg = reg(&[("fast", "openai", "deepseek-v4")]);
        assert_eq!(
            resolve_model_id(&reg, current_model_ref(home.path(), "a").as_deref()),
            Some("deepseek-v4".to_string())
        );
    }
```

- [x] Run it and watch it fail to compile (`resolve_model_id` does not exist):

```bash
cargo nextest run -p mur-core --lib model_cmd 2>&1 | tail -5
# error[E0425]: cannot find function `resolve_model_id` in this scope
```

- [x] Add both functions to `model_cmd.rs`, directly below `current_model_ref`:

```rust
/// The raw vendor model id behind an alias, e.g. `fast` → `deepseek-v4`.
///
/// Split from `current_model_id` so the registry lookup is testable without a
/// registry file on disk.
pub(crate) fn resolve_model_id(reg: &ModelRegistry, model_ref: Option<&str>) -> Option<String> {
    reg.models.get(model_ref?).map(|e| e.model.clone())
}

/// The raw model id this agent is configured with.
///
/// Keyed on `ModelEntry.model`, never on `ModelEntry.provider` — that field
/// records the wire protocol, so DeepSeek, Qwen and every other
/// OpenAI-compatible third party all read `openai`. Anything asking "what can
/// this model do" must go through the raw id. Best-effort: `None` when the
/// profile, the registry, or the alias is missing.
pub(crate) fn current_model_id(home: &Path, agent: &str) -> Option<String> {
    let model_ref = current_model_ref(home, agent)?;
    let reg = ModelRegistry::default_path()
        .and_then(|p| ModelRegistry::load_from(&p))
        .ok()?;
    resolve_model_id(&reg, Some(&model_ref))
}
```

- [x] Watch it pass:

```bash
cargo nextest run -p mur-core --lib model_cmd 2>&1 | tail -3
# Summary [   0.1s] N tests run: N passed
```

- [x] In `mur-core/src/cmd/agent/cli/mod.rs`, replace the inline resolution in the `SlashCmd::Effort` arm. Delete these lines:

```rust
            let model_id = model_cmd::current_model_ref(&app.home, &app.agent)
                .and_then(|r| {
                    mur_common::model::ModelRegistry::default_path()
                        .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
                        .ok()
                        .and_then(|reg| reg.models.get(&r).map(|e| e.model.clone()))
                })
                .unwrap_or_default();
```

and put this in their place (keep the comment above them as it is):

```rust
            let model_id = model_cmd::current_model_id(&app.home, &app.agent).unwrap_or_default();
```

- [x] Full module check, then commit:

```bash
cargo nextest run -p mur-core --lib cmd::agent::cli 2>&1 | tail -2
# 348 tests run: 348 passed
git commit -am "refactor(murmur): the raw model id resolves in one place"
```

---

## Task 2 — one list of live agent-local notes

`/forget last` already filters agent-local notes that are not Destroyed. The
menu needs the same set, so the filter moves out of the `last` branch.

### Interfaces

**Consumes:** nothing from Task 1.

**Produces:**
```rust
// mur-core/src/cmd/agent/cli/memory_cmds.rs
pub fn live_note_names(home: &Path, agent: &str) -> Vec<String>
```
Ordered most-recently-updated first, so `[0]` is what `/forget last` resolves to.

### Steps

- [x] Add this test to `mod tests` in `mur-core/src/cmd/agent/cli/memory_cmds.rs`:

```rust
    /// The menu and `/forget last` must see the same set, in the same order:
    /// a forgotten note stays out of both, and the newest is first.
    #[test]
    fn live_note_names_drops_forgotten_and_leads_with_the_newest() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        remember(h, "a", &["first".to_string()]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        remember(h, "a", &["second".to_string()]).unwrap();

        let names = live_note_names(h, "a");
        assert_eq!(names.len(), 2, "{names:?}");

        forget(h, "a", Some("last")).unwrap();
        let after = live_note_names(h, "a");
        assert_eq!(after.len(), 1, "a forgotten note is still listed: {after:?}");
        assert_eq!(
            after[0], names[1],
            "`last` must forget the newest, leaving the older one"
        );
    }
```

The one-second sleep is load-bearing: note names are `note-%Y%m%d-%H%M%S`, and
two notes minted in the same second collide (`remember` bails on the second).

- [x] Watch it fail:

```bash
cargo nextest run -p mur-core --lib memory_cmds 2>&1 | tail -5
# error[E0425]: cannot find function `live_note_names` in this scope
```

- [x] Add the function to `memory_cmds.rs`, directly above `pub fn forget`:

```rust
/// Agent-local notes that are still injectable, newest first.
///
/// One definition for two callers: `/forget last` resolves to `[0]`, and the
/// completion menu offers the whole list. A Destroyed note is excluded from
/// both — offering a name that `forget` would then reject is worse than
/// offering nothing.
pub fn live_note_names(home: &Path, agent: &str) -> Vec<String> {
    let mut live: Vec<_> = load_all(home, agent)
        .into_iter()
        .filter(|s| s.scope == SkillScope::Agent && note_kind(&s.manifest).is_some())
        .filter(|s| {
            SkillStats::load(&SkillStats::path_agent(home, agent, &s.name))
                .ok()
                .flatten()
                .is_none_or(|st| st.lifecycle_state != LifecycleState::Destroyed)
        })
        .collect();
    live.sort_by(|a, b| b.manifest.updated_at.cmp(&a.manifest.updated_at));
    live.into_iter().map(|s| s.name).collect()
}
```

- [x] Rewrite the `last` branch of `forget` to use it. Replace:

```rust
    let name = if target == "last" {
        load_all(home, agent)
            .into_iter()
            .filter(|s| s.scope == SkillScope::Agent && note_kind(&s.manifest).is_some())
            .filter(|s| {
                SkillStats::load(&SkillStats::path_agent(home, agent, &s.name))
                    .ok()
                    .flatten()
                    .is_none_or(|st| st.lifecycle_state != LifecycleState::Destroyed)
            })
            .max_by_key(|s| s.manifest.updated_at)
            .map(|s| s.name)
            .ok_or_else(|| anyhow::anyhow!("no agent-local memories to forget"))?
    } else {
        target.to_string()
    };
```

with:

```rust
    let name = if target == "last" {
        live_note_names(home, agent)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no agent-local memories to forget"))?
    } else {
        target.to_string()
    };
```

- [x] Watch both the new test and the existing `remember_memories_forget_cycle` pass, then commit:

```bash
cargo nextest run -p mur-core --lib memory_cmds 2>&1 | tail -3
# Summary: N tests run: N passed
git commit -am "refactor(murmur): live agent-local notes list in one place"
```

---

## Task 3 — `MenuContext`: the only I/O the menu does

### Interfaces

**Consumes:**
```rust
model_cmd::current_model_id(home: &Path, agent: &str) -> Option<String>   // Task 1
memory_cmds::live_note_names(home: &Path, agent: &str) -> Vec<String>     // Task 2
```

**Produces:**
```rust
// mur-core/src/cmd/agent/cli/complete.rs
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuContext {
    pub effort: Vec<String>,
    pub models: Vec<(String, String)>,
    pub secrets: Vec<String>,
    pub notes: Vec<String>,
}
impl MenuContext { pub fn load(home: &Path, agent: &str) -> Self }
```

### Steps

- [x] Add the test first, in `mod tests` at the bottom of `complete.rs`:

```rust
    /// A missing agent reads nothing and must not panic: the menu degrades to
    /// its command layer rather than taking the session down.
    #[test]
    fn menu_context_is_fail_soft_on_a_missing_agent() {
        let home = tempfile::tempdir().unwrap();
        let ctx = MenuContext::load(home.path(), "nope");
        assert!(ctx.effort.is_empty());
        assert!(ctx.secrets.is_empty());
        assert!(ctx.notes.is_empty());
    }
```

`ctx.models` is deliberately not asserted: it reads the real
`~/.mur/models.yaml`, which is populated on a developer machine. `ctx.secrets`
is safe to assert only because no agent is named `nope`; the secret read goes
through the process-wide MUR home, not the tempdir.

- [x] Watch it fail:

```bash
cargo nextest run -p mur-core --lib complete 2>&1 | tail -5
# error[E0433]: failed to resolve: use of undeclared type `MenuContext`
```

- [x] Update the module doc comment at the top of `complete.rs`:

```rust
//! Pure autocomplete logic for the `mur agent cli` completion menu: build the
//! candidate set for the current input and filter it. No TUI and no I/O on the
//! per-keystroke path — the two functions that read disk (`load_agent_skills`
//! and `MenuContext::load`) are called at startup and after a slash command,
//! never from `compute`.
```

- [x] Add `MenuContext` below the `CompletionState` struct:

```rust
/// The argument lists a menu row can come from, read from disk.
///
/// `compute` is pure, so everything it needs that lives in a file is gathered
/// here first. Rebuilt after every slash command (see `mod.rs`), which is what
/// keeps `/effort` honest after a `/model` hot-switch: the levels are a
/// property of the model, and the model can change mid-session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuContext {
    /// Effort levels this agent's model accepts, in the model's own order.
    /// Empty when the model takes no reasoning parameter at all.
    pub effort: Vec<String>,
    /// Registry aliases, each with the raw model id behind it.
    pub models: Vec<(String, String)>,
    /// Secret KEYs the agent already holds.
    pub secrets: Vec<String>,
    /// Agent-local note names, newest first, led by the literal `last`.
    pub notes: Vec<String>,
}

impl MenuContext {
    /// Read all four lists. Fail-soft throughout: any list that cannot be read
    /// stays empty and its command simply opens no argument menu.
    pub fn load(home: &Path, agent: &str) -> Self {
        let effort = match super::model_cmd::current_model_id(home, agent) {
            // The levels are NOT a fixed scale — Opus 4.6 has no `xhigh`,
            // DeepSeek V4 has no `medium`, Qwen is a two-position switch. Ask
            // the table keyed on the raw model id; never restate it here.
            Some(id) => mur_common::llm::effort_shape(&id)
                .levels()
                .iter()
                .map(|e| e.as_str().to_string())
                .collect(),
            None => Vec::new(),
        };
        let models = mur_common::model::ModelRegistry::default_path()
            .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
            .map(|reg| {
                super::model_cmd::ordered_models(&reg)
                    .into_iter()
                    .map(|(alias, e)| (alias, e.model))
                    .collect()
            })
            .unwrap_or_default();
        // NOTE the asymmetry: `load_profile_for_edit` resolves the MUR home
        // itself and ignores `home`, exactly as `load_agent_skills` does. Do
        // not "fix" it by threading `home` through — that is a wider change
        // than this menu, and both callers here are the same process reading
        // its own agent.
        let secrets = crate::cmd::agent::load_profile_for_edit(agent)
            .map(|(_path, p)| p.secrets)
            .unwrap_or_default();
        let mut notes = super::memory_cmds::live_note_names(home, agent);
        if !notes.is_empty() {
            // `last` is what `/forget` resolves to, so it belongs in the menu
            // beside the names — and first, because it is the common case.
            notes.insert(0, "last".to_string());
        }
        Self {
            effort,
            models,
            secrets,
            notes,
        }
    }
}
```

- [x] Watch it pass, then commit:

```bash
cargo nextest run -p mur-core --lib complete 2>&1 | tail -3
# Summary: N tests run: N passed
git commit -am "feat(murmur): MenuContext gathers the menu's dynamic argument lists"
```

---

## Task 4 — argument sources, the six missing commands, and the guard

The largest task, and one unit: the guard test cannot go green until the table
carries every command, and the table cannot compile until `compute` takes the
context.

### Interfaces

**Consumes:**
```rust
complete::MenuContext                                   // Task 3
complete::MenuContext::load(home, agent) -> MenuContext // Task 3
```

**Produces:**
```rust
// complete.rs
pub enum Args { None, Fixed(&'static [(&'static str, &'static str)]), Effort, Model, Secret, Note }
pub fn compute(input: &str, skills: &[Candidate], ctx: &MenuContext) -> Option<CompletionState>
pub fn offers(word: &str) -> bool
// app.rs
pub menu_ctx: complete::MenuContext   // on App
```

### Steps

- [x] Write the guard first, in `mur-core/src/cmd/agent/cli/mod.rs`. Replace the whole `help_lists_every_command_the_parser_accepts` test with:

```rust
    /// Three lists describe the same set of commands — `parse_slash`, `HELP`,
    /// and the completion table — and nothing but this test ties them
    /// together. `/effort` shipped in the parser while missing from both of
    /// the others; that is the drift this exists to catch.
    #[test]
    fn every_command_is_parsed_documented_and_offered() {
        for cmd in one_of_each() {
            let Some(name) = help_name(&cmd) else {
                continue;
            };
            let parsed = parse_slash(&format!("/{name}"));
            assert!(
                !matches!(parsed, Some(SlashCmd::Unknown(_)) | None),
                "/{name} is in the documented list but the parser rejects it: {parsed:?}"
            );
            assert!(
                HELP.contains(&format!("/{name}")),
                "/{name} works but /help never mentions it"
            );
            assert!(
                super::complete::offers(name),
                "/{name} works but the completion menu never offers it"
            );
        }
    }
```

- [x] Add the missing variant to `one_of_each()`, immediately after the `SlashCmd::Model(None)` line, and replace its stale doc comment. The comment currently claims the list is structurally exhaustive; it is not, and that claim is why `Effort` was never noticed missing:

```rust
    /// One concrete instance per `SlashCmd` variant, to drive the round-trip
    /// check below.
    ///
    /// **This list is hand-maintained and nothing forces it to be complete.**
    /// `help_name`'s match is exhaustive over the enum, so a new variant must
    /// be *named* — but a variant absent from this list is silently never
    /// checked. `Effort` was missing here for its whole life, which is exactly
    /// why it reached users absent from `/help` and from the menu. Add the new
    /// variant here when you add one to `SlashCmd`.
```

```rust
            SlashCmd::Effort {
                level: None,
                save: false,
            },
```

- [x] Rename the `Quit` spelling so all three lists agree. In `help_name`, change `SlashCmd::Quit => Some("exit")` to:

```rust
            SlashCmd::Quit => Some("quit"),
```

- [x] In the `HELP` constant, replace `/panel [tab]  /exit ·` with:

```
/panel [tab]  /quit (or /exit)  /effort [level] (reasoning effort this model accepts) ·
```

- [x] Watch the guard fail, naming the six commands one at a time:

```bash
cargo nextest run -p mur-core --lib every_command_is_parsed 2>&1 | grep panicked
# panicked at ...: /effort works but the completion menu never offers it
```

- [x] In `complete.rs`, add the `Args` enum and the shared word lists above `COMMANDS`:

```rust
/// Where a command's second-layer rows come from.
///
/// The static-list version of this field is what made `/effort` unrepresentable:
/// its levels are a property of the agent's model, so there was no literal list
/// to write and the command was left out of the menu entirely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Args {
    /// No second layer.
    None,
    /// Literal words, each with a description for the right-hand column.
    Fixed(&'static [(&'static str, &'static str)]),
    /// `MenuContext::effort` — the levels THIS agent's model accepts.
    Effort,
    /// `MenuContext::models` — registry aliases.
    Model,
    /// `MenuContext::secrets` — KEYs already held, plus `--delete`.
    Secret,
    /// `MenuContext::notes` — agent-local note names, plus `last`.
    Note,
}

const ON_OFF: &[(&str, &str)] = &[("on", "enable"), ("off", "disable")];
const SKINS: &[(&str, &str)] = &[
    ("dark", "default"),
    ("light", "light terminals"),
    ("mur", "MUR brand"),
];
const MCP_SUBS: &[(&str, &str)] = &[
    ("list", "servers this agent has"),
    ("add", "add a stdio server"),
    ("remove", "remove a server"),
    ("add-remote", "add a Streamable HTTP server"),
    ("login", "authenticate a remote server"),
    ("registry-add", "add from the MCP registry"),
];
const SKILL_SUBS: &[(&str, &str)] = &[
    ("list", "skills this agent has"),
    ("add", "install a skill"),
    ("remove", "uninstall a skill"),
];
const PANEL_TABS: &[(&str, &str)] = &[
    ("information", ""),
    ("activities", ""),
    ("preview", ""),
    ("notifications", ""),
    ("schedule", ""),
    ("stream", ""),
];
const LOGIN_PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "Claude subscription"),
    ("chatgpt", "ChatGPT subscription"),
];
const CHANNELS_ARGS: &[(&str, &str)] = &[("--follow", "live-tail another channel")];
```

- [x] Replace the whole `COMMANDS` constant with:

```rust
/// Built-in commands: (word without slash, description, argument source).
/// `exit` is omitted as a duplicate of `quit`.
///
/// Every command the parser accepts must appear here — the guard test
/// `every_command_is_parsed_documented_and_offered` in `mod.rs` enforces it.
const COMMANDS: &[(&str, &str, Args)] = &[
    ("auto", "session-wide auto-approval", Args::Fixed(ON_OFF)),
    ("card", "show this agent's card", Args::None),
    (
        "channels",
        "list, switch, or follow channels",
        Args::Fixed(CHANNELS_ARGS),
    ),
    ("clear", "start a new conversation", Args::None),
    ("effort", "reasoning effort for this model", Args::Effort),
    ("forget", "drop an agent-local memory", Args::Note),
    ("help", "show the command cheatsheet", Args::None),
    (
        "login",
        "OAuth health / re-authenticate",
        Args::Fixed(LOGIN_PROVIDERS),
    ),
    ("mcp", "manage MCP servers", Args::Fixed(MCP_SUBS)),
    ("memories", "list this agent's memories", Args::None),
    ("model", "list or hot-switch the model", Args::Model),
    ("open", "what is still outstanding", Args::None),
    ("panel", "companion window (MUR Hub)", Args::Fixed(PANEL_TABS)),
    ("quit", "exit the chat", Args::None),
    ("remember", "save an agent-local memory", Args::None),
    (
        "secret",
        "hand the agent a credential (hidden input)",
        Args::Secret,
    ),
    ("sessions", "list past sessions", Args::None),
    ("skill", "manage agent skills", Args::Fixed(SKILL_SUBS)),
    ("skin", "switch theme", Args::Fixed(SKINS)),
    ("verbose", "expand tool cards", Args::Fixed(ON_OFF)),
];

/// Does the completion menu offer this command word? The guard test in
/// `mod.rs` uses it to tie the menu to the parser and to `HELP`.
pub fn offers(word: &str) -> bool {
    COMMANDS.iter().any(|(w, _, _)| *w == word)
}
```

- [x] Replace `subcommands_for`, `build_top_level` and `build_subcommands` with:

```rust
/// The argument source for `cmd` (without leading slash), or `None` if `cmd`
/// is unknown.
fn args_for(cmd: &str) -> Option<Args> {
    COMMANDS
        .iter()
        .find(|(w, _, _)| *w == cmd)
        .map(|(_, _, a)| *a)
}

/// Layer-2 candidates for a command word, resolved against `ctx`.
fn build_args(cmd: &str, args: Args, ctx: &MenuContext) -> Vec<Candidate> {
    let rows: Vec<(String, String)> = match args {
        Args::None => return Vec::new(),
        Args::Fixed(f) => f
            .iter()
            .map(|(w, d)| ((*w).to_string(), (*d).to_string()))
            .collect(),
        // No description column: any wording would be invented, and the one
        // useful label ("current") would lie under a session override, which
        // lives in `App` and not on disk.
        Args::Effort => ctx.effort.iter().map(|l| (l.clone(), String::new())).collect(),
        Args::Model => ctx.models.clone(),
        Args::Note => ctx.notes.iter().map(|n| (n.clone(), String::new())).collect(),
        Args::Secret => {
            let mut v: Vec<(String, String)> = ctx
                .secrets
                .iter()
                .map(|k| (k.clone(), "already set — replaces it".to_string()))
                .collect();
            v.push(("--delete".to_string(), "revoke a credential".to_string()));
            v
        }
    };
    rows.into_iter()
        .map(|(word, desc)| Candidate {
            display: word.clone(),
            insert: format!("/{cmd} {word} "),
            desc,
            has_children: false,
        })
        .collect()
}

/// Top-level candidates: every built-in command plus the agent's skills.
///
/// `has_children` is derived from whether the command actually has rows to
/// show right now, not merely from its declared source: `/effort` on a model
/// that takes no reasoning parameter has an `Effort` source and no rows, and
/// promising a layer that never opens is worse than promising nothing.
fn build_top_level(skills: &[Candidate], ctx: &MenuContext) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = COMMANDS
        .iter()
        .map(|(word, desc, args)| Candidate {
            display: format!("/{word}"),
            insert: format!("/{word} "),
            desc: (*desc).to_string(),
            has_children: !build_args(word, *args, ctx).is_empty(),
        })
        .collect();
    out.extend_from_slice(skills);
    out
}
```

- [x] Rewrite `compute` to thread the context:

```rust
pub fn compute(input: &str, skills: &[Candidate], ctx: &MenuContext) -> Option<CompletionState> {
    // ponytail: slash commands are single-line; a multiline composer has no menu.
    if input.contains('\n') {
        return None;
    }
    let after = input.trim_start().strip_prefix('/')?;
    let items = match after.split_once(char::is_whitespace) {
        // Still typing the command word.
        None => filter(build_top_level(skills, ctx), after),
        // Command word complete → maybe an argument layer.
        Some((cmd, rest)) => {
            // A second whitespace means we're typing an arg past layer 2.
            if rest.trim_start().contains(char::is_whitespace) {
                return None;
            }
            let args = args_for(cmd)?;
            filter(build_args(cmd, args, ctx), rest.trim_start())
        }
    };
    if items.is_empty() {
        return None;
    }
    Some(CompletionState {
        items,
        selected: 0,
        spaced: false,
    })
}
```

- [x] Add the `menu_ctx` field to `App` in `app.rs`, directly under the `skills` field:

```rust
    /// Argument lists for the completion menu — effort levels, registry
    /// models, secret KEYs, note names. Rebuilt after every slash command;
    /// `compute` is pure, so this is where that I/O lives.
    pub menu_ctx: complete::MenuContext,
```

and in the struct literal in `App::new`, beside `skills: Vec::new(),`:

```rust
            menu_ctx: complete::MenuContext::default(),
```

Check the `use` line at the top of `app.rs`: if `complete::MenuContext` does
not resolve, the file imports `CompletionState` directly — add
`use super::complete;` rather than widening the existing import.

- [x] Update every `compute` call site. In `mod.rs`, `refresh_completion`:

```rust
    app.completion = complete::compute(&app.input_text(), &app.skills, &app.menu_ctx);
```

and in `completion_accept`:

```rust
    app.completion = if descend {
        complete::compute(&app.input_text(), &app.skills, &app.menu_ctx)
    } else {
        None
    };
```

- [x] Update the existing `complete.rs` tests, which all call `compute(input, &skills)`. Add at the top of `mod tests`:

```rust
    fn ctx() -> MenuContext {
        MenuContext {
            effort: vec!["low".into(), "high".into(), "max".into()],
            models: vec![("fast".into(), "deepseek-v4".into())],
            secrets: vec!["GITHUB_TOKEN".into()],
            notes: vec!["last".into(), "note-20260908-101500".into()],
        }
    }
```

and pass `&ctx()` as the third argument in every existing `compute(...)` call
in that module. `panel_subcommands` and the `mcp` tests keep their assertions
unchanged — `Args::Fixed` produces the same words.

- [x] Add the level tests. The vendor differences are the point of this change, so they are asserted through `compute`, not through `effort_shape`:

```rust
    fn effort_ctx(model: &str) -> MenuContext {
        MenuContext {
            effort: mur_common::llm::effort_shape(model)
                .levels()
                .iter()
                .map(|e| e.as_str().to_string())
                .collect(),
            ..MenuContext::default()
        }
    }

    /// The levels are an arbitrary subset per model, never a prefix of one
    /// scale. A hardcoded low/medium/high/xhigh/max would be wrong for every
    /// row below except the first.
    #[test]
    fn effort_levels_follow_the_model_not_a_fixed_scale() {
        let levels = |model: &str| -> Vec<String> {
            compute("/effort ", &[], &effort_ctx(model))
                .map(|s| s.items.iter().map(|c| c.display.clone()).collect())
                .unwrap_or_default()
        };

        assert_eq!(levels("claude-opus-5").len(), 5);
        assert!(levels("claude-opus-5").contains(&"xhigh".to_string()));

        // 4.6 predates the xhigh step but keeps max.
        let opus46 = levels("claude-opus-4-6");
        assert!(!opus46.contains(&"xhigh".to_string()), "{opus46:?}");
        assert!(opus46.contains(&"max".to_string()), "{opus46:?}");

        // DeepSeek V4 publishes low/high/max — there is no medium.
        let ds = levels("deepseek-v4");
        assert!(!ds.contains(&"medium".to_string()), "{ds:?}");

        // A switch has two positions, not three that collapse to two.
        assert_eq!(levels("qwen3-32b").len(), 2);

        // gpt-5 and friends stop at high.
        assert_eq!(levels("gpt-5"), vec!["low", "medium", "high"]);
    }

    /// A model that rejects the parameter (Magistral, HTTP 422) or has no
    /// reasoning control (gpt-4o) opens no menu at all — and `/effort` still
    /// carries no ▸ marker promising one.
    #[test]
    fn a_model_without_effort_opens_no_menu_and_promises_none() {
        for model in ["magistral-medium-latest", "gpt-4o"] {
            let c = effort_ctx(model);
            assert!(c.effort.is_empty(), "{model}");
            assert!(compute("/effort ", &[], &c).is_none(), "{model}");
            let top = compute("/effort", &[], &c).unwrap();
            let row = top.items.iter().find(|i| i.display == "/effort").unwrap();
            assert!(!row.has_children, "{model} promised a layer it cannot open");
        }
    }

    /// `/secret` offers the KEYs already held plus the revoke flag; a new KEY
    /// is typed freely and simply matches nothing, which closes the menu.
    #[test]
    fn secret_offers_held_keys_and_delete() {
        let s = compute("/secret ", &[], &ctx()).unwrap();
        let d: Vec<String> = s.items.iter().map(|c| c.display.clone()).collect();
        assert!(d.contains(&"GITHUB_TOKEN".to_string()), "{d:?}");
        assert!(d.contains(&"--delete".to_string()), "{d:?}");
        assert!(compute("/secret NEW_KEY", &[], &ctx()).is_none());
    }

    /// `/model` completes registry aliases, described by the id behind them.
    #[test]
    fn model_offers_registry_aliases() {
        let s = compute("/model ", &[], &ctx()).unwrap();
        let row = s.items.iter().find(|c| c.display == "fast").unwrap();
        assert_eq!(row.insert, "/model fast ");
        assert_eq!(row.desc, "deepseek-v4");
    }

    /// `/forget` completes note names, `last` first.
    #[test]
    fn forget_offers_last_then_note_names() {
        let s = compute("/forget ", &[], &ctx()).unwrap();
        assert_eq!(s.items[0].display, "last");
        assert_eq!(s.items.len(), 2);
    }
```

- [x] Run the whole module. Every test in it must pass, including the guard that was red at the start of this task:

```bash
cargo nextest run -p mur-core --lib cmd::agent::cli 2>&1 | tail -2
# 356 tests run: 356 passed
```

- [x] Lint, format, commit:

```bash
cargo clippy -p mur-core --all-targets -- -D warnings 2>&1 | grep -c '^error'
# 0
cargo fmt -p mur-core && cargo fmt --check -p mur-core && echo fmt-clean
git commit -am "feat(murmur): every slash command completes, with model-aware arguments"
```

---

## Task 5 — keep the lists fresh

A `/model` hot-switch changes which effort levels exist. Stale levels after a
switch would offer a level the new model does not have — the exact failure the
spec exists to prevent.

### Interfaces

**Consumes:**
```rust
complete::MenuContext::load(home, agent) -> MenuContext   // Task 3
App::menu_ctx                                             // Task 4
```

**Produces:** nothing later tasks depend on.

### Steps

- [ ] Add the freshness test to `mod tests` in `complete.rs`:

```rust
    /// After a `/model` switch the menu must offer the NEW model's levels.
    /// Two shapes with different level counts, so a stale context cannot pass
    /// by coincidence.
    #[test]
    fn switching_models_changes_the_levels_on_offer() {
        let five = effort_ctx("claude-opus-5");
        let three = effort_ctx("gpt-5");
        assert_ne!(five.effort, three.effort);
        assert_eq!(compute("/effort ", &[], &five).unwrap().items.len(), 5);
        assert_eq!(compute("/effort ", &[], &three).unwrap().items.len(), 3);
    }
```

- [ ] Load the context at startup. In `mod.rs`, beside the existing skills load (`app.skills = complete::load_agent_skills(&agent);`):

```rust
    app.menu_ctx = complete::MenuContext::load(&home, &agent);
```

- [ ] Refresh after every slash command. At the single `handle_slash` call site, replace:

```rust
            handle_slash(app, cmd, tx).await;
```

with:

```rust
            handle_slash(app, cmd, tx).await;
            // One refresh site, not four. `/model`, `/secret`, `/remember` and
            // `/forget` each change one of these lists, and `handle_slash` has
            // early returns in most arms, so a per-arm refresh would rot the
            // first time an arm gains a return. Slash commands are typed by a
            // human; three small file reads per command is not a cost.
            app.menu_ctx = complete::MenuContext::load(&app.home, &app.agent);
```

- [ ] Verify the whole module, lint, format:

```bash
cargo nextest run -p mur-core --lib cmd::agent::cli 2>&1 | tail -2
# 357 tests run: 357 passed
cargo clippy -p mur-core --all-targets -- -D warnings 2>&1 | grep -c '^error'
# 0
cargo fmt -p mur-core && cargo fmt --check -p mur-core && echo fmt-clean
```

- [ ] Live-verify against a real agent, because a menu that only exists in tests has never been seen. Build the debug binary and drive it in tmux, polling for readiness before sending keys (sending early types into a terminal that is not listening yet):

```bash
cargo build -p mur-core --bin mur
tmux kill-session -t murmenu 2>/dev/null
tmux new-session -d -s murmenu -x 110 -y 36 "target/debug/mur agent cli mur"
for i in $(seq 1 40); do sleep 1; tmux capture-pane -pt murmenu | grep -q "Type a message" && break; done
tmux send-keys -t murmenu "/effort "; sleep 2; tmux capture-pane -pt murmenu | sed 's/ *$//'
```

Expected: a menu listing exactly the levels this agent's model accepts. Confirm
against the model shown in the status bar and the table in the spec. Then:

```bash
tmux send-keys -t murmenu C-c; tmux send-keys -t murmenu "/forget "; sleep 2
tmux capture-pane -pt murmenu | sed 's/ *$//'
tmux send-keys -t murmenu C-d; sleep 2; tmux kill-session -t murmenu
```

Expected: `last` first, then this agent's note names.

- [ ] Commit:

```bash
git commit -am "feat(murmur): the menu's argument lists refresh after every slash command"
```

---

## Self-review

**Spec coverage.** §1 `Args` enum → Task 4. §2 `MenuContext` + fail-soft +
`compute` stays pure → Task 3, wired in Task 4, refreshed in Task 5. §3 one
model-id resolution → Task 1. §4 models with no effort parameter → Task 4
(`a_model_without_effort_opens_no_menu_and_promises_none`). §5 the six missing
commands and `/effort` in `HELP` → Task 4. §6 the three-list guard, the
`one_of_each` hole, and the `/quit` spelling → Task 4. Spec's Testing section:
per-shape levels (Task 4), guard red first (Task 4, second step), `compute`
does no I/O (guaranteed by its signature; every test feeds a hand-built
context), freshness after a model switch (Task 5).

**Cross-task type consistency.** `current_model_id` (Task 1) is called only in
`MenuContext::load` (Task 3) and the `/effort` arm (Task 1). `live_note_names`
(Task 2) is called only in `MenuContext::load` and `forget` (Task 2).
`MenuContext` fields are written in Task 3 and read in Task 4's `build_args`;
the field names match in both (`effort`, `models`, `secrets`, `notes`).
`compute`'s three-argument form appears in Task 4's call-site step and in every
test from Task 4 onward. `Args::Fixed` rows are `(&str, &str)` everywhere.

**Task 2 note ordering.** `live_note_names` sorts newest-first and `forget
last` takes `[0]`; the old code took `max_by_key(updated_at)`. Same element, so
the existing `remember_memories_forget_cycle` test must still pass unchanged —
if it does not, the sort direction is inverted, not the test wrong.

**Counts in expected output** (347 → 348 → 356 → 357) are approximate: they
assume no other branch lands tests in `cmd::agent::cli` first. Read them as
"more than before, none failing", and treat any `FAIL` line as the real signal.
