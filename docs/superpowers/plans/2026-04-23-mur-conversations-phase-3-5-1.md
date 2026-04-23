# mur Conversations Phase 3.5.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--no-summarize` and `--summarize-model <model>` CLI flags to `mur ask`, layered on top of the Phase 3.5 config-only surface (`AskConfig.summarize_hits_enabled` + `AskConfig.summarize_model`).

**Architecture:** Pure CLI → config plumbing. Two new clap fields on `Commands::Ask` → two new fields on `AskArgs` → a small pure resolver helper in `conversations_cmd.rs` that collapses CLI + config into `(summarize_enabled, summarize_model)` → feeds the existing `AskRequest.summarize_enabled` / `AskRequest.summarize_model`. Zero changes to `prompt::render`, `ask_stream`, `abstractive.rs`, or `cache.rs`.

**Tech Stack:** Rust 2024 edition, clap derive macros, existing tokio/serde/anyhow. No new dependencies.

**Base directory:** `/Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-5-1`. Branch: `fix/conversations-phase-3-5-1`. Spec: `docs/superpowers/specs/2026-04-23-mur-conversations-phase-3-5-1-design.md`.

---

## Task 1: `resolve_summarize` helper + `AskArgs` fields

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs:979-997` (`AskArgs` struct)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (add helper `fn resolve_summarize` + its test module)

- [ ] **Step 1: Write the failing unit tests**

Append to `mur-core/src/cmd/conversations_cmd.rs`. If a `#[cfg(test)] mod tests` block doesn't already exist at the bottom of the file, add one. The tests go inside it:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_summarize_no_flag_uses_config_enabled_and_model() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ None,
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(enabled);
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }

    #[test]
    fn resolve_summarize_no_summarize_flag_forces_disabled_regardless_of_config() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ true,
            /* cli_model    */ None,
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(!enabled, "--no-summarize must override enabled config");
        // model still bubbles up (CLI didn't set one) — the disabled flag is what matters.
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }

    #[test]
    fn resolve_summarize_cli_model_overrides_config_model() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ Some("qwen3:4b"),
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(enabled);
        assert_eq!(model.as_deref(), Some("qwen3:4b"), "CLI model wins over config");
    }

    #[test]
    fn resolve_summarize_cli_model_falls_back_to_config_when_none() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ None,
            /* cfg_enabled  */ false,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(!enabled, "config-disabled stays disabled without CLI override");
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p mur-core cmd::conversations_cmd::tests::resolve_summarize`
Expected: FAIL — `resolve_summarize` is not defined.

- [ ] **Step 3: Add the two new fields to `AskArgs`**

In `mur-core/src/cmd/conversations_cmd.rs`, the `AskArgs` struct (around line 979). Add two fields after `strict_citations`:

Old:
```rust
pub struct AskArgs {
    pub question: Option<String>, // was String; now Option because --show-session has no question
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
    pub strict_citations: bool,
    pub continue_flag: bool,
    /// Explicit new-session flag. Default is to archive + start fresh, so this
    /// is only meaningful as a clap `conflicts_with = "continue_flag"` signal.
    #[allow(dead_code)]
    pub new_flag: bool,
    pub show_session: bool,
}
```

New:
```rust
pub struct AskArgs {
    pub question: Option<String>, // was String; now Option because --show-session has no question
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
    pub strict_citations: bool,
    pub continue_flag: bool,
    /// Explicit new-session flag. Default is to archive + start fresh, so this
    /// is only meaningful as a clap `conflicts_with = "continue_flag"` signal.
    #[allow(dead_code)]
    pub new_flag: bool,
    pub show_session: bool,
    /// Phase 3.5.1: disable Stage 1b for this invocation (overrides
    /// `conversations.ask.summarize_hits_enabled`).
    pub no_summarize: bool,
    /// Phase 3.5.1: override the Stage 1b model for this invocation (overrides
    /// `conversations.ask.summarize_model`). `None` means "use config value, or
    /// fall back to `ask.model` per the resolver below".
    pub summarize_model: Option<String>,
}
```

- [ ] **Step 4: Add the `resolve_summarize` helper**

In the same file, above `pub async fn cmd_ask(...)`. Place it right before the `cmd_ask` definition so it's easy to find:

```rust
/// Phase 3.5.1 — collapse CLI summarize flags + AskConfig defaults into the
/// effective `(summarize_enabled, summarize_model)` pair fed to `AskRequest`.
///
/// Precedence: CLI flag > config > hardcoded default (handled upstream by
/// `AskConfig::default`). clap rejects `--no-summarize` + `--summarize-model`
/// together at parse time (see `conflicts_with` in `main.rs`), so the
/// combination is unreachable here.
pub(crate) fn resolve_summarize(
    no_summarize: bool,
    cli_model: Option<&str>,
    cfg_enabled: bool,
    cfg_model: Option<&str>,
) -> (bool, Option<String>) {
    let enabled = !no_summarize && cfg_enabled;
    let model = cli_model
        .map(|s| s.to_string())
        .or_else(|| cfg_model.map(|s| s.to_string()));
    (enabled, model)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p mur-core cmd::conversations_cmd::tests::resolve_summarize`
Expected: PASS (all 4 tests).

Also run the full mur-core suite to confirm no regressions:
Run: `cargo test -p mur-core`
Expected: all green. (Note: `cmd_ask` doesn't yet use `resolve_summarize` or the new fields — that's Task 2. All existing behavior unchanged.)

- [ ] **Step 6: `cargo fmt` + clippy clean**

Run: `cargo fmt -p mur-core && cargo clippy -p mur-core -- -D warnings`
Expected: zero diffs, zero warnings.

The new `AskArgs` fields are unused (no caller yet constructs them) — verify clippy doesn't flag them. It shouldn't because `AskArgs` is `pub` and the fields are `pub`.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/conversations_cmd.rs
git commit -m "feat(conversations): Phase 3.5.1 Task 1 — AskArgs fields + resolve_summarize helper"
```

---

## Task 2: clap `Ask` flags + wire through `cmd_ask`

**Files:**
- Modify: `mur-core/src/main.rs:348-388` (`Commands::Ask` variant — add two flags)
- Modify: `mur-core/src/main.rs:1290-?` (`AskArgs { ... }` builder — pass the new flags through)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` inside `cmd_ask` (~line 1169 — use `resolve_summarize` in the `AskRequest` builder)
- Modify: `mur-core/src/main.rs` — add a unit test module for clap parsing

- [ ] **Step 1: Write the failing clap-parse tests**

Append to `mur-core/src/main.rs` — add a `#[cfg(test)] mod tests` block if none exists (main.rs typically doesn't have one; that's fine, add a fresh one at the very end of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Helper: parse a full argv and return the `Commands::Ask` variant fields
    /// we care about. Panics on any other variant.
    fn parse_ask(argv: &[&str]) -> (bool, Option<String>) {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Commands::Ask {
                no_summarize,
                summarize_model,
                ..
            } => (no_summarize, summarize_model),
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[test]
    fn ask_parses_no_summarize_flag() {
        let (no_summarize, model) = parse_ask(&["mur", "ask", "q?", "--no-summarize"]);
        assert!(no_summarize);
        assert!(model.is_none());
    }

    #[test]
    fn ask_parses_summarize_model_flag() {
        let (no_summarize, model) =
            parse_ask(&["mur", "ask", "q?", "--summarize-model", "qwen3:4b"]);
        assert!(!no_summarize);
        assert_eq!(model.as_deref(), Some("qwen3:4b"));
    }

    #[test]
    fn ask_rejects_no_summarize_with_summarize_model() {
        // clap `conflicts_with` on --no-summarize guards this.
        let r = Cli::try_parse_from([
            "mur",
            "ask",
            "q?",
            "--no-summarize",
            "--summarize-model",
            "qwen3:4b",
        ]);
        assert!(r.is_err(), "clap must reject the conflict, got: {r:?}");
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("--summarize-model") || msg.contains("summarize-model"),
            "error should mention the conflicting flag; got: {msg}"
        );
    }
}
```

The test helper assumes the top-level parser is called `Cli` and the subcommand enum is `Commands`. Verify these names in the existing `main.rs` — if different, adjust the helper's types. They should be standard clap-derive names.

If `Commands` doesn't currently `#[derive(Debug)]`, add it so the `panic!("expected Ask, got {other:?}")` works. Otherwise change the panic message to not use `Debug`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p mur-core --bin mur ask_parses_no_summarize_flag ask_parses_summarize_model_flag ask_rejects_no_summarize_with_summarize_model`
Expected: compile-fail with "no field `no_summarize` on variant `Ask`" (or similar). That's the correct red state.

- [ ] **Step 3: Add the two new flags to the `Ask` subcommand**

In `mur-core/src/main.rs`, find `Commands::Ask` (~line 348). Insert the new flags immediately after `show_session`:

Old tail of the `Ask` variant:
```rust
        /// Print current session path, turn count, last turn time.
        /// Ignores question if given; no LLM calls.
        #[arg(long, conflicts_with_all = ["continue_flag", "new_flag"])]
        show_session: bool,
    },
```

New:
```rust
        /// Print current session path, turn count, last turn time.
        /// Ignores question if given; no LLM calls.
        #[arg(long, conflicts_with_all = ["continue_flag", "new_flag"])]
        show_session: bool,
        /// Phase 3.5.1: Disable Stage 1b (LLM-abstractive hit compression)
        /// for this invocation. Overrides `conversations.ask.summarize_hits_enabled`.
        #[arg(long, conflicts_with = "summarize_model")]
        no_summarize: bool,
        /// Phase 3.5.1: Override the Stage 1b model for this invocation.
        /// Overrides `conversations.ask.summarize_model`; `None` (flag omitted)
        /// falls back to the config value, and ultimately to `ask.model`.
        #[arg(long)]
        summarize_model: Option<String>,
    },
```

- [ ] **Step 4: Pass the new flags through the `AskArgs { ... }` builder**

In `mur-core/src/main.rs`, find the command dispatch that builds `AskArgs` (~line 1290). Add the two new fields to the struct literal.

The existing pattern will look like:
```rust
Commands::Ask { question, src, since, until, k, model, min_score, json, no_escalate, debug_prompt, strict_citations, continue_flag, new_flag, show_session } => {
    cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
        question, src, since, until, k, model, min_score, json, no_escalate,
        debug_prompt, strict_citations, continue_flag, new_flag, show_session,
    }).await
}
```

(Exact layout varies.) After your edit it should destructure `no_summarize` + `summarize_model` and pass them through:

```rust
Commands::Ask {
    question, src, since, until, k, model, min_score, json, no_escalate,
    debug_prompt, strict_citations, continue_flag, new_flag, show_session,
    no_summarize, summarize_model,
} => {
    cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
        question, src, since, until, k, model, min_score, json, no_escalate,
        debug_prompt, strict_citations, continue_flag, new_flag, show_session,
        no_summarize, summarize_model,
    }).await
}
```

Follow the existing formatting — if the match arm uses single-line destructuring, add the fields on the existing line; if multi-line, append. Exact-field-name shorthand works because the clap field and the `AskArgs` field have identical names and types.

- [ ] **Step 5: Run the clap parse tests to verify they now pass**

Run: `cargo test -p mur-core --bin mur ask_parses_no_summarize_flag ask_parses_summarize_model_flag ask_rejects_no_summarize_with_summarize_model`
Expected: all 3 PASS.

- [ ] **Step 6: Wire `resolve_summarize` into `cmd_ask`**

In `mur-core/src/cmd/conversations_cmd.rs`, inside `cmd_ask` around line 1169 (the `AskRequest { ... }` builder). Immediately before the `let req = ask::AskRequest {` line, add:

```rust
    let (effective_summarize_enabled, effective_summarize_model) = resolve_summarize(
        args.no_summarize,
        args.summarize_model.as_deref(),
        ask_cfg.summarize_hits_enabled,
        ask_cfg.summarize_model.as_deref(),
    );
```

Then in the `AskRequest { ... }` literal replace:

```rust
        compress_enabled: ask_cfg.compress_hits_enabled,
        summarize_enabled: ask_cfg.summarize_hits_enabled,
        summarize_model: ask_cfg.summarize_model.clone(),
```

with:

```rust
        compress_enabled: ask_cfg.compress_hits_enabled,
        summarize_enabled: effective_summarize_enabled,
        summarize_model: effective_summarize_model,
```

- [ ] **Step 7: Full test suite green**

Run: `cargo test -p mur-core`
Expected: all PASS, including the 4 resolver tests from Task 1 and the 3 clap tests from this task.

- [ ] **Step 8: `cargo fmt` + clippy clean**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero diffs, zero warnings.

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/main.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "feat(conversations): Phase 3.5.1 Task 2 — clap --no-summarize + --summarize-model"
```

---

## Task 3: CLI integration tests

**Files:**
- Modify: `mur-core/tests/cli_conversations.rs` — append 2 new tests at end

- [ ] **Step 1: Open the file, find the appropriate insertion point**

Read `mur-core/tests/cli_conversations.rs`. Look for the existing Phase 3.5 `mur_ask_stage_1b_*` tests (ends with `mur_ask_stage_1b_soft_fails_gracefully`). Insert the two new tests after it, before the end of the file. They follow the same seeding/reindex pattern as Phase 3.5 Task 12's tests (seed summary markdown, `reindex --spans-only`, then ask with flags).

- [ ] **Step 2: Write both failing tests**

Append to `mur-core/tests/cli_conversations.rs`:

```rust
/// Phase 3.5.1: `--no-summarize` flag must disable Stage 1b for the
/// invocation regardless of the config. JSON must omit `stage_1b` or
/// report `compressed_count == 0`. Mirrors `mur_ask_stage_1b_disabled_via_config`
/// but disables via CLI rather than writing config.
#[test]
fn mur_ask_cli_no_summarize_flag_disables_stage_1b() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // Config has Stage 1b ENABLED — the CLI flag must override.
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    // Seed a 500-char eligible span + reindex (same pattern as the Phase 3.5
    // integration tests).
    seed_rich_span(&mur_home, "2026-04-21", "cc/c1", "sha-no-summarize");

    let reindex = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(
        reindex.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex.stderr)
    );

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "--no-summarize", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --no-summarize");
    assert!(
        out.status.success(),
        "ask failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("parse JSON failed: {e}; stdout: {stdout}"));
    let compressed = v
        .pointer("/stage_1b/compressed_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let cache_hits = v
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert_eq!(
        compressed + cache_hits,
        0,
        "--no-summarize must prevent Stage 1b from firing; got: {stdout}"
    );
}

/// Phase 3.5.1: `--summarize-model <X>` must produce a different cache key
/// than the default model, so a query that warmed the cache under the
/// default model must see `cache_hits == 0` when re-run with a different
/// `--summarize-model`. The third run with the SAME --summarize-model value
/// should see `cache_hits > 0`, confirming the new key is stable.
#[test]
fn mur_ask_cli_summarize_model_changes_cache_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    seed_rich_span(&mur_home, "2026-04-21", "cc/c1", "sha-model-cachekey");

    let reindex = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(reindex.status.success());

    // Run 1 — default model (config has none, so falls back to ask.model =
    // "qwen3:14b"). Warms the cache under that key.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 1");
    assert!(out1.status.success());

    // Run 2 — override model to qwen3:4b. Cache key is different → must
    // fresh-compress, cache_hits == 0.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "ask",
            "--json",
            "--summarize-model",
            "qwen3:4b",
            "what was discussed?",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 2");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let v2: serde_json::Value = serde_json::from_str(&stdout2).expect("parse JSON run 2");
    let cache_hits_2 = v2
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let compressed_2 = v2
        .pointer("/stage_1b/compressed_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert_eq!(
        cache_hits_2, 0,
        "run 2 (different model) must NOT hit the run-1 cache key; got: {stdout2}"
    );
    assert!(
        compressed_2 > 0,
        "run 2 must fresh-compress under the new key; got: {stdout2}"
    );

    // Run 3 — same --summarize-model qwen3:4b. Must now hit run 2's cache.
    let out3 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "ask",
            "--json",
            "--summarize-model",
            "qwen3:4b",
            "what was discussed?",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 3");
    assert!(out3.status.success());
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    let v3: serde_json::Value = serde_json::from_str(&stdout3).expect("parse JSON run 3");
    let cache_hits_3 = v3
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert!(
        cache_hits_3 > 0,
        "run 3 (same model as run 2) must hit run 2's cache; got: {stdout3}"
    );
}

/// Phase 3.5.1 — shared seeding helper. Writes a single 2026-04-21 summary
/// markdown under `<mur_home>/conversations/summary/` with an extractive
/// span long enough (~500 chars) to qualify for Stage 1b
/// (`MIN_CONTENT_CHARS = 400`) and with no `. ` terminators so Stage 1's
/// heuristic compression skips it (`COMPRESS_MIN_SENTENCES = 4`).
fn seed_rich_span(mur_home: &std::path::Path, date: &str, conv_ref: &str, sha: &str) {
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    let span_text = "fact ".repeat(100); // 500 chars, zero ". " terminators
    let (src_prefix, conv_id) = conv_ref.split_once('/').unwrap();
    std::fs::write(
        summary_dir.join(format!("{date}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {date}\n\
             generated_at: {date}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [{src_prefix}]\n\
             pattern_refs: []\n\
             keywords: []\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: {sha}\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{{conv_ref} @L1}}_:\n> {span_text}\n\n\
             ## Abstractive narrative\n\n\
             Narrative.\n"
        ),
    )
    .unwrap();
}
```

**Notes on the helper:** `seed_rich_span` is a minor duplication of what the Phase 3.5 tests inline. If the file already has a very similar helper from a previous pass, reuse it instead of adding a new one; otherwise this helper stays scoped to the two new tests and doesn't need cross-file visibility. If you notice the Phase 3.5 tests also inline the same 25-line template, consider promoting the helper for them too — but only if the diff stays small and focused.

- [ ] **Step 3: Run the tests single-threaded**

Run: `cargo test -p mur-core --test cli_conversations mur_ask_cli_ -- --test-threads=1`
Expected: both PASS.

Also run the full cli_conversations suite to confirm no regression:
Run: `cargo test -p mur-core --test cli_conversations -- --test-threads=1`
Expected: all tests PASS (including the 4 Phase 3.5 `mur_ask_stage_1b_*` and the 2 new ones).

- [ ] **Step 4: `cargo fmt` + clippy clean**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`

- [ ] **Step 5: Commit**

```bash
git add mur-core/tests/cli_conversations.rs
git commit -m "test(conversations): Phase 3.5.1 Task 3 — CLI integration tests for --no-summarize + --summarize-model"
```

---

## Task 4: README documentation

**Files:**
- Modify: `README.md` — extend the "Ask Configuration" section added by Phase 3.5

- [ ] **Step 1: Locate the existing section**

Run: `grep -n "Ask Configuration\|summarize_hits_enabled\|summarize_model" README.md | head -20`

You should find a section introduced by Phase 3.5 Task 14 (commit in the `main..Phase-3.5-merge` range) that documents the two config keys. That's where the CLI-overrides sub-block goes.

- [ ] **Step 2: Add the CLI overrides sub-block**

Inside the existing "Ask Configuration" section, after the `summarize_model` config key documentation, add a new subsection. The exact placement depends on the current structure — insert it as a peer or child of the existing bullets. Here's the content:

```markdown
#### CLI overrides (per-invocation)

Override the `summarize_*` config keys for a single `mur ask` invocation without editing `~/.mur/config.yaml`:

- `--no-summarize` — disable Stage 1b for this invocation. Overrides `ask.summarize_hits_enabled`. Useful for demos, benchmarks, or scripted comparisons.
- `--summarize-model <model>` — override the Stage 1b model for this invocation. Overrides `ask.summarize_model`. Pair with a faster model like `qwen3:4b` to trade quality for speed on the summarize hop. The cache key includes the model name, so changing this flag produces a fresh cache entry.

The two flags are mutually exclusive: passing both errors at argument-parse time.
```

Match the exact markdown style of the surrounding section (heading level, bullet style, code-span formatting) rather than copying this template verbatim — if the existing section uses `###` headings, bump `####` to `###`; if it uses tables instead of bullets, convert.

- [ ] **Step 3: Spot-check for broken claims**

Run: `grep -n "summarize-model\|no-summarize" README.md`
Confirm: the flag names match exactly `--no-summarize` and `--summarize-model`.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: Phase 3.5.1 Task 4 — README CLI-overrides subsection for summarize flags"
```

---

## Final Verification

- [ ] **Step 1: Full test pass**

Run:
```bash
cargo fmt --check --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: zero format diffs, zero clippy warnings, all tests green.

- [ ] **Step 2: Manual smoke test (optional, requires real Ollama)**

Only if you have Ollama running locally:
```bash
cargo run --release -- ask "what did I ship yesterday?" --json --no-summarize
cargo run --release -- ask "what did I ship yesterday?" --json --summarize-model qwen3:4b
cargo run --release -- ask "q" --no-summarize --summarize-model qwen3:4b 2>&1 | head -5
```
Expected:
- Run 1: `stage_1b` absent or counts all zero.
- Run 2: `stage_1b.compressed_count > 0` (fresh cache under new model).
- Run 3: clap error mentioning `--summarize-model`.

- [ ] **Step 3: Spec cross-check**

Re-read `docs/superpowers/specs/2026-04-23-mur-conversations-phase-3-5-1-design.md` §11 "Success criteria". Verify each of items 1–6 is met:

1. `mur ask --no-summarize` → Stage 1b never fires — ✅ covered by Task 3 test 1.
2. `mur ask --summarize-model X` → Stage 1b fires with key X — ✅ covered by Task 3 test 2.
3. `--no-summarize --summarize-model X` → parse error — ✅ covered by Task 2 test 3.
4. No regression in Phase 3.5 tests — ✅ covered by final cargo test --workspace.
5. cargo test / clippy / fmt green across platforms — ✅ relies on CI (macOS / Linux / Windows).
6. README CLI-overrides block exists — ✅ covered by Task 4.

---

## Notes for the implementing agent

1. **Spec is source of truth.** When in doubt between plan and spec, spec wins. Flag the discrepancy back to the controller so the plan gets updated.
2. **Field ordering in `AskArgs` and `Ask { ... }` must match** (by name — Rust's struct-literal shorthand relies on identical names and types). The plan adds fields in the same order to both sites.
3. **clap `conflicts_with` takes the Rust field name** (in snake_case), not the CLI flag name. `conflicts_with = "summarize_model"` refers to the field, which clap turns into `--summarize-model` for the user-facing message. This is a common foot-gun — don't write `conflicts_with = "summarize-model"`.
4. **Windows CI parity**: Phase 3.5 surfaced two Windows-only bugs (`MUR_HOME` honoring in `config_path`, `--force` wall-clock-second race in `write_rollup`). Both are fixed on `main` at `0ebab7d` and later. Task 3's integration tests run the same `MUR_HOME` + `HOME` + `USERPROFILE` env triad — they should be Windows-clean already.
5. **No `AskResponse` / `AskEvent` changes.** Task 3's tests read JSON fields (`stage_1b.compressed_count`, `stage_1b.cache_hits`) that already exist from Phase 3.5. Don't add new telemetry.
6. **Resolver test sugar (`/* named-param */ ...` comments)** — the `resolve_summarize` signature has 4 bools/options, so the test-site comments are load-bearing for readability. Keep them.
