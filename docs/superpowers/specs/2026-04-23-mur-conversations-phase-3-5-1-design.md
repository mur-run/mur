# mur Conversations Phase 3.5.1 — CLI Flags for Summarize

**Status:** Design approved 2026-04-23. Ready for plan.
**Depends on:** Phase 3.5 shipped (merge `0ebab7d`) + code-review follow-ups (merge `f56232a`).
**Branch:** `fix/conversations-phase-3-5-1`, worktree at `/Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-5-1`.

---

## 1. Goal

Add two CLI flags to `mur ask` that override Phase 3.5's config-only surface for Stage 1b (LLM-abstractive hit summarization):

- `--no-summarize` — force-disable Stage 1b for this invocation.
- `--summarize-model <model>` — override the summarize model for this invocation.

Both were explicitly deferred from Phase 3.5 (Phase 3.5 spec §2 non-goals). This phase closes that commitment.

## 2. Non-goals

- No `--summarize` flag to force-enable when config sets `summarize_hits_enabled: false`. Editing config is the right path for persistent enablement.
- No symmetric `--no-compress` for Phase 3.4's heuristic Stage 1. Deferred until someone actually asks — heuristic compression has no latency/cost, so the debug-time use case is weak.
- No way to clear `AskConfig.summarize_model` back to the fallback (`ask.model`) via CLI. Corner case; edit config.
- No changes to the Stage 1b cascade, `prompt::render`, `ask_stream`, or `abstractive.rs` internals. Pure CLI → config plumbing.
- No new `AskResponse` / `AskEvent` fields. The existing `stage_1b` telemetry already tells you whether Stage 1b fired; CLI-vs-config provenance is not surfaced.
- No environment-variable overrides (`MUR_SUMMARIZE=0` etc.). Out of scope.

## 3. Architecture

Two new fields on the clap `Ask` subcommand → two new fields on `AskArgs` → applied in `cmd_ask` before building `AskRequest`:

```
CLI flag        > AskConfig key                    > Default
─────────────────────────────────────────────────────────────
--no-summarize  > conversations.ask.summarize_hits_enabled  > true
--summarize-model <m> > conversations.ask.summarize_model   > None (→ ask.model)
```

Applied in `cmd_ask`:

```rust
let effective_summarize_enabled =
    !args.no_summarize && ask_cfg.summarize_hits_enabled;
let effective_summarize_model =
    args.summarize_model.clone().or(ask_cfg.summarize_model.clone());
```

These feed `AskRequest.summarize_enabled` / `AskRequest.summarize_model` — the same fields Phase 3.5 wired in Task 8. Zero downstream changes.

## 4. Locked design choices

| # | Decision | Choice |
|---|---|---|
| D1 | Scope — only summarize flags, or compress too? | **Only summarize.** Heuristic Stage 1 has no compelling CLI-override use case; `--no-compress` stays deferred. |
| D2 | Conflict between `--no-summarize` and `--summarize-model` | **clap `conflicts_with`.** Passing both errors out at arg-parse time. Matches existing `continue_flag` / `new_flag` pattern. |
| D3 | Force-enable flag (`--summarize`) to override a disabled config | **No.** YAGNI — edit config for persistent enablement. |
| D4 | Precedence | **CLI > config > default.** Standard. |
| D5 | Telemetry for CLI-vs-config disablement | **None.** `stage_1b` field already reports whether Stage 1b fired; the "why" is noise. |
| D6 | Phase label | **3.5.1** (point release, following the 3.2.1 precedent for "deferred surface / gap fill" work). |

## 5. CLI surface (exact clap signatures)

In `mur-core/src/main.rs` under `Commands::Ask`, add two new arg fields alongside the existing ones:

```rust
/// Disable Stage 1b LLM-abstractive hit compression for this invocation.
/// Overrides `conversations.ask.summarize_hits_enabled` from config.
#[arg(long, conflicts_with = "summarize_model")]
no_summarize: bool,

/// Override the model used by Stage 1b for this invocation.
/// Overrides `conversations.ask.summarize_model`; `None` still falls back to `ask.model`.
#[arg(long)]
summarize_model: Option<String>,
```

`conflicts_with = "summarize_model"` on `no_summarize` is load-bearing — prevents the nonsense `--no-summarize --summarize-model X`.

## 6. `AskArgs` additions

In `mur-core/src/cmd/conversations_cmd.rs::AskArgs`, add fields directly after `strict_citations`:

```rust
pub no_summarize: bool,
pub summarize_model: Option<String>,
```

Populated from the clap match block in `main.rs` where the existing `AskArgs { ... }` literal is built.

## 7. `cmd_ask` plumbing

Before the existing `AskRequest { ... }` literal (around line 1156):

```rust
let effective_summarize_enabled =
    !args.no_summarize && ask_cfg.summarize_hits_enabled;
let effective_summarize_model =
    args.summarize_model.clone().or(ask_cfg.summarize_model.clone());
```

Then in the `AskRequest { ... }` literal, replace:

```rust
summarize_enabled: ask_cfg.summarize_hits_enabled,
summarize_model: ask_cfg.summarize_model.clone(),
```

with:

```rust
summarize_enabled: effective_summarize_enabled,
summarize_model: effective_summarize_model,
```

## 8. Tests

### 8.1 Unit tests (clap parse — in `main.rs` or a new `tests` module)

- `ask_parses_no_summarize_flag` — clap accepts `mur ask q --no-summarize`; `AskArgs.no_summarize == true`.
- `ask_parses_summarize_model_flag` — clap accepts `mur ask q --summarize-model qwen3:4b`; `AskArgs.summarize_model == Some("qwen3:4b".into())`.
- `ask_rejects_no_summarize_with_summarize_model` — clap errors with a conflict message when both are passed.

If `main.rs` has no existing test module, add these to a new `#[cfg(test)] mod tests` at the bottom of `main.rs` using clap's `try_parse_from`.

### 8.2 Unit tests (effective-value logic — in `conversations_cmd.rs`)

Pure-function tests for the override math. If `conversations_cmd.rs` doesn't already have a test module for `cmd_ask` helpers, extract the two-line override block into a small `fn resolve_summarize(args, cfg) -> (bool, Option<String>)` pub(crate) helper and test it directly:

- `resolve_summarize_no_flag_uses_config` — `--no-summarize=false, config.enabled=true` → returns `(true, None)`.
- `resolve_summarize_no_summarize_overrides_enabled_config` — `--no-summarize=true, config.enabled=true` → returns `(false, ...)`.
- `resolve_summarize_model_flag_overrides_config_model` — `--summarize-model=X, config.model=Y` → returns `(_, Some("X"))`.
- `resolve_summarize_flag_precedence_over_config` — combined case.

### 8.3 Integration tests (cli_conversations.rs — 2 new)

- `mur_ask_cli_no_summarize_flag_disables_stage_1b` — mirrors Phase 3.5's `mur_ask_stage_1b_disabled_via_config` but disables via `--no-summarize` instead of writing config. Seeds 500-char span + reindex; runs `mur ask --no-summarize --json`; asserts `stage_1b` absent or `compressed_count == 0`.
- `mur_ask_cli_summarize_model_changes_cache_key` — seeds + compacts once with the default model (warms cache under key A); runs `mur ask --summarize-model qwen3:4b --json` (key B); asserts JSON has `compressed_count > 0` AND `cache_hits == 0`. Proves the CLI flag actually reaches `cache::cache_key()`. Then a third run with the same `--summarize-model qwen3:4b` should see `cache_hits > 0`, confirming the new key is stable.

### 8.4 Golden path

No Step 19 needed. Golden path already proves Stage 1b works end-to-end; CLI override is a config-surface expansion, not new behavior.

## 9. Documentation

README: extend the existing "Ask Configuration" section (added by Phase 3.5 Task 14) with a short **"CLI overrides"** sub-bullet block — one line per flag, cross-referencing the existing config keys:

```
### Ask Configuration — CLI overrides

Override `summarize_*` per invocation without editing config:

- `--no-summarize` — disable Stage 1b for this invocation (overrides `ask.summarize_hits_enabled`).
- `--summarize-model <model>` — override the Stage 1b model (overrides `ask.summarize_model`; mutually exclusive with `--no-summarize`).
```

## 10. Error handling

- **Conflict at parse time:** clap handles. No runtime branch.
- **Invalid model string:** passed through to `AbstractiveCtx.model`; Ollama returns an error; Phase 3.5's `Skipped(OLLAMA_ERR)` soft-fail catches it. Same as editing config to a bad model — no new error path.

## 11. Success criteria

1. `mur ask --no-summarize "q"` produces JSON with `stage_1b` absent or all counts zero, regardless of `summarize_hits_enabled: true` in config.
2. `mur ask --summarize-model X "q"` under overflow → JSON has `stage_1b` with a non-empty `compressed_count` OR `cache_hits`, keyed on model `X`. Cache is consistent across repeated invocations with the same `--summarize-model`.
3. `mur ask --no-summarize --summarize-model X` errors at parse time with a clear conflict message.
4. No regression in any existing Phase 3.5 test (unit, integration, golden path).
5. `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all` green across macOS, Linux, Windows.
6. README CLI-overrides block renders cleanly and cross-references the config keys.

## 12. File-change summary

| File | Change |
|---|---|
| `mur-core/src/main.rs` | `Commands::Ask` gains `no_summarize: bool` + `summarize_model: Option<String>`. `AskArgs { ... }` builder picks them up. 3 unit tests for clap parsing. |
| `mur-core/src/cmd/conversations_cmd.rs` | `AskArgs` gains two fields. Extract `fn resolve_summarize(args, cfg) -> (bool, Option<String>)` helper. `cmd_ask` calls it. 4 unit tests for resolver. |
| `mur-core/tests/cli_conversations.rs` | 2 new integration tests (disabled-via-flag, summarize-model-changes-cache-key). |
| `README.md` | Extend "Ask Configuration" section with CLI-overrides sub-block. |

No new files. No new Cargo deps. No changes to `abstractive.rs`, `cache.rs`, `prompt.rs`, `format.rs`, `ask/mod.rs`, `AskConfig` / `AskRequest` / `AskResponse` / `AskEvent`.

---

_Spec approved for plan. Next: `docs/superpowers/plans/2026-04-23-mur-conversations-phase-3-5-1.md` via `superpowers:writing-plans`._
