# mur agent cli — Risk-Tiered Auto-Approve (`--auto-reads`) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An opt-in `--auto-reads` flag: when a tool-approval (HITL) fires for a `bash` tool whose command is unambiguously **read-only** (`cat`, `ls`, `grep`, `git status`, …), the cli auto-approves it (no prompt) and tags the card `[read · auto]`. Writes and anything uncertain still prompt. Kills approval fatigue for the common read-heavy case without weakening the gate on writes.

**Architecture:** Pure cli-side. A conservative classifier `is_readonly_bash(cmd) -> bool` (fail-safe: anything it can't prove read-only → `false` → normal prompt). In `handle_stream`'s `StreamMsg::Hitl` arm, extend the existing auto-decide condition (`--auto` / session-allow) with a read lane: `app.auto_reads && req.tool_name == "bash" && is_readonly_bash(<command>)`. The command is on `HitlRequest.tool_input["command"]` directly. Auto-approved reads get a `[read · auto]` note + a card tag so every decision stays visible in the transcript.

**Tech Stack:** Rust (edition 2024). No new dependency. No runtime change.

## Why opt-in (safety)

MUR's HITL is **post-hoc** — the runtime runs the tool, *then* asks. So auto-approving a read doesn't prevent execution; it skips the keystroke. A *misclassified* write that got auto-approved would execute silently without the user's review. Therefore: (1) default OFF — the user opts in with `--auto-reads`; (2) the classifier is conservative — it auto-approves only a fixed allowlist of read-only commands with **no shell metacharacters**, defaulting to "prompt" on anything else; (3) every auto-approved read is still shown in the transcript (note + card tag). This mirrors the project's "autonomous needs a safety lens: opt-in OFF + fail-closed" stance.

## Global Constraints

- **Independent cli feature** — branch from `main` (this plan was written on `feat/agent-cli-auto-reads`, cut off `main be34b843`). All edits in `mur-core/src/cmd/agent/cli/`.
- **Rust edition 2024**; **no hardcoded values** (the allowlists are named `const` arrays); brand "MUR" uppercase in user-facing copy (lowercase `bash`/command refs are fine).
- **Tests:** mur-core needs `ORT_STRATEGY=download`; toolchain cargo if rustup broken (`export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`, plain `cargo test`).
- **Lint gate:** `cargo clippy -p mur-core -- -D warnings` + `cargo fmt`.
- **Fail-safe classifier:** when in doubt, return `false` (prompt). A false negative (prompting for a read) is a minor annoyance; a false positive (auto-approving a write) is a safety hole. The test suite must assert the dangerous cases fall through to `false`.
- **E0027 guard:** adding `--auto-reads` to the `AgentAction::Cli` clap variant means every `Cli { … }` destructure (the dispatch arm + any `#[cfg(test)]` ones in `cli/agent.rs`) must add the field or `..`. A binary build won't catch a missed test destructure — run `cargo test -p mur-core --no-run` before claiming done (the bug that hit CI on the plain-mode branch).

---

### Task 1: The read-only bash classifier (pure + heavily tested)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/bash_class.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod bash_class;`)

**Interfaces:**
- Produces: `pub fn is_readonly_bash(cmd: &str) -> bool` — `true` only when `cmd` is a single simple command (no shell metacharacters) whose head is in a read-only allowlist (with per-command guards for `find`/`git`). `false` otherwise (fail-safe).

- [ ] **Step 1: Write the failing tests** (the classification matrix — this IS the deliverable)

Create `bash_class.rs` with the test module first (and a stub `is_readonly_bash` returning `false` so it compiles):

```rust
//! Conservative read-only classification of a `bash` tool command, for the
//! cli's `--auto-reads` lane. Fail-safe: anything not provably read-only
//! returns `false` (→ a normal HITL prompt). Never auto-approve a write.

/// Shell metacharacters that can chain, redirect, expand, substitute, or
/// background — any of them means "more than one simple command", so we refuse
/// to classify and fall through to a prompt.
const SHELL_META: &[char] = &[
    '>', '<', '|', ';', '&', '$', '`', '(', ')', '{', '}', '\n', '\\', '!',
];

/// Commands that only ever read (regardless of their flags).
const READONLY_HEADS: &[&str] = &[
    "cat", "ls", "ll", "pwd", "echo", "head", "tail", "wc", "grep", "egrep",
    "fgrep", "rg", "which", "type", "file", "stat", "du", "df", "tree",
    "realpath", "dirname", "basename", "env", "printenv", "date", "whoami",
    "hostname", "uname", "sort", "uniq", "cut", "diff", "cmp", "shasum",
    "md5sum", "xxd", "od", "nl", "tac", "column", "true", "false",
];

/// `git` subcommands that only read (regardless of flags). Excludes anything
/// with a write mode (`branch -D`, `tag X`, `remote add`, `config`, `stash`, …).
const GIT_READONLY_SUBCMDS: &[&str] = &[
    "status", "log", "diff", "show", "blame", "rev-parse", "ls-files",
    "ls-tree", "cat-file", "describe", "shortlog", "reflog", "whatchanged",
    "grep",
];

pub fn is_readonly_bash(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    if cmd.contains(SHELL_META) {
        return false; // not a single simple command — fail-safe
    }
    let mut toks = cmd.split_whitespace();
    let Some(head) = toks.next() else {
        return false;
    };
    match head {
        // `find` reads UNLESS it deletes or executes.
        "find" => {
            !cmd.contains("-delete") && !cmd.contains("-exec") && !cmd.contains("-ok")
        }
        // `git` only for a fixed read-only subcommand set.
        "git" => toks.next().is_some_and(|sub| GIT_READONLY_SUBCMDS.contains(&sub)),
        other => READONLY_HEADS.contains(&other),
    }
}

#[cfg(test)]
mod tests {
    use super::is_readonly_bash;

    #[test]
    fn auto_approves_plain_read_commands() {
        for c in [
            "cat Cargo.toml",
            "ls",
            "ls -la src/",
            "grep -rn foo src/",
            "rg TODO",
            "head -50 file.rs",
            "wc -l *.rs",
            "find . -name '*.rs'",
            "git status",
            "git log --oneline -10",
            "git diff HEAD~1",
        ] {
            assert!(is_readonly_bash(c), "should be read-only: {c}");
        }
    }

    #[test]
    fn prompts_on_writes_and_dangerous() {
        for c in [
            "rm -rf target/",
            "mv a b",
            "cp a b",
            "sed -i 's/a/b/' f",     // sed not in allowlist
            "awk '{print}' f",       // awk not in allowlist
            "chmod +x f",
            "curl http://x | sh",
            "cargo build",           // executes build scripts
            "git push",
            "git commit -m x",
            "git branch -D main",    // git write subcommand
            "git checkout main",
            "find . -delete",        // find mutate
            "find . -exec rm {} +",  // find execute
        ] {
            assert!(!is_readonly_bash(c), "should prompt (not auto): {c}");
        }
    }

    #[test]
    fn prompts_on_shell_metacharacters() {
        for c in [
            "cat a > b",             // redirect
            "cat a >> b",
            "echo x | tee f",        // pipe to writer
            "ls; rm -rf /",          // chain
            "ls && rm x",
            "cat $(echo f)",         // command substitution
            "cat `echo f`",
            "ls & ",                 // background
            "cat a < b",
        ] {
            assert!(!is_readonly_bash(c), "metachar must prompt: {c}");
        }
    }

    #[test]
    fn empty_or_unknown_prompts() {
        assert!(!is_readonly_bash(""));
        assert!(!is_readonly_bash("   "));
        assert!(!is_readonly_bash("somerandomtool --flag"));
        assert!(!is_readonly_bash("git")); // bare git, no subcommand
    }
}
```

Add `mod bash_class;` to `mod.rs` (near the other `mod step;`/`mod diff;`/`mod dump;` declarations).

- [ ] **Step 2: Run tests to verify they pass** (the impl above is complete — this task is classifier-first, so the impl ships with the tests)

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib bash_class 2>&1 | tail -15`
Expected: all 4 tests PASS. (If you wrote a `false`-stub first per strict TDD, see them fail, then paste the real impl and see them pass — either way show real RED→GREEN or the final GREEN.)

- [ ] **Step 3: Gate + commit**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`

```bash
git add mur-core/src/cmd/agent/cli/bash_class.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): conservative read-only bash classifier for --auto-reads (fail-safe)"
```

---

### Task 2: `--auto-reads` flag + read lane in the HITL arm + card tag

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (the `Cli { … }` variant; any `#[cfg(test)]` destructure)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`cmd_cli`, `run_tui`, the `StreamMsg::Hitl` arm, `decide_hitl_with_note`)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`App.auto_reads` field + ctor; `mark_card_auto_approved`)
- Modify: `mur-core/src/cmd/agent/cli/step.rs` (`StepCard.auto_approved` field)
- Modify: `mur-core/src/cmd/agent/cli/render_card.rs` (render the `[read · auto]` tag)

**Interfaces:**
- Consumes: `bash_class::is_readonly_bash` (Task 1), `HitlRequest.tool_input` (`stream.rs`).
- Produces: `App.auto_reads: bool`; `StepCard.auto_approved: bool`; `App::mark_card_auto_approved(step_id)`. `cmd_cli(names, resume, auto, skin, plain, budget_usd, auto_reads)`.

- [ ] **Step 1: Add the flag + field + plumbing** (mirror `--budget-usd` from the last feature exactly)

`cli/agent.rs` — in the `Cli { … }` variant, after `budget_usd`:
```rust
        /// Auto-approve read-only bash commands (cat/ls/grep/git status/…) so
        /// only writes prompt. Opt-in; the classifier is conservative
        /// (fail-safe — anything uncertain still asks).
        #[arg(long = "auto-reads")]
        auto_reads: bool,
```
Update the dispatch arm (`dispatch.rs` — the `AgentAction::Cli { … } =>` destructure) + any `#[cfg(test)]` `Cli { … }` destructure in `cli/agent.rs` (add `auto_reads` / `auto_reads: _`).

`mod.rs` — add `auto_reads: bool` to `cmd_cli` and `run_tui` signatures (thread it through; the plain/multiplex paths don't need it — just compile). In `run_tui`, after `build_app`, next to the `--auto`/`--budget` blocks:
```rust
    app.auto_reads = auto_reads;
    if auto_reads {
        app.push_system(
            "--auto-reads is ON — read-only bash (cat/ls/grep/git status/…) is auto-approved; writes still ask",
        );
    }
```

`app.rs` — field after `auto_approve`:
```rust
    /// Auto-approve read-only bash commands (`--auto-reads`). Conservative +
    /// opt-in; writes still prompt. Never persisted.
    pub auto_reads: bool,
```
Ctor init: `auto_reads: false,`.

- [ ] **Step 2: Wire the read lane into the `StreamMsg::Hitl` arm** (`mod.rs`)

Current arm computes `let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name);`. Extend it with the read lane, and remember whether THIS approval came from the read lane (for the card tag + note):
```rust
        StreamMsg::Hitl { req, .. } => {
            app.saw_hitl_this_turn = true;
            if let Some(sid) = req.step_id.clone() {
                app.mark_card_awaiting(&sid);
            }
            // Read lane: --auto-reads auto-approves a read-only bash command.
            let read_auto = app.auto_reads
                && req.tool_name == "bash"
                && req
                    .tool_input
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(bash_class::is_readonly_bash);
            let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name) || read_auto;
            if !app.focused && !auto {
                notify_unfocused(&app.agent, &format!("Tool approval needed: {}", req.tool_name));
            }
            if read_auto && let Some(sid) = req.step_id.clone() {
                app.mark_card_auto_approved(&sid);
            }
            app.hitl = Some(req);
            if auto {
                decide_hitl_with_note(app, tx, true, true);
            }
        }
```
> Keep `app.mark_card_awaiting` first (so a card that's about to be auto-approved still correlates), then mark auto-approved. The `decide_hitl_with_note(…, true)` already clears `awaiting`.

- [ ] **Step 3: Card tag** (`step.rs` + `app.rs` + `render_card.rs`)

`step.rs` — `StepCard` field after `awaiting_hitl`:
```rust
    /// True when this card's tool call was auto-approved by the `--auto-reads`
    /// read lane (rendered as a `[read · auto]` tag).
    pub auto_approved: bool,
```
Initialize it `false` wherever `StepCard` is constructed (grep `StepCard {` — likely `push_step_started`/a `StepCard::new`; add `auto_approved: false`).

`app.rs` — mirror `mark_card_awaiting`:
```rust
    /// Tag the card with this `step_id` as auto-approved (read lane).
    pub fn mark_card_auto_approved(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.auto_approved = true;
        }
    }
```

`render_card.rs` — in the header line (where the glyph/name/duration render), append a tag when `card.auto_approved`:
```rust
    if card.auto_approved {
        spans.push(Span::styled(
            "  [read · auto]",
            Style::default().fg(theme.system),
        ));
    }
```
> Match the real render structure (the gather shows a header `Line::from(vec![...])` with glyph+name+dur spans); add the tag span to that line. Use the theme's dim/system color so it reads as a subtle annotation.

- [ ] **Step 4: Run tests + the E0027 guard + gate**

Run:
```
ORT_STRATEGY=download cargo test -p mur-core --lib "bash_class" 2>&1 | grep "test result"
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli 2>&1 | grep "test result"
ORT_STRATEGY=download cargo test -p mur-core --no-run 2>&1 | grep -E "error|Finished" | tail   # E0027 guard
ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings && cargo fmt
```
Expected: tests green; test build `Finished` with no `error[E0027]`. Verify the flag: `ORT_STRATEGY=download cargo run -p mur-core -- agent cli --help 2>&1 | grep auto-reads`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/cli/
git commit -m "feat(cli): --auto-reads auto-approves read-only bash + tags the card [read · auto]"
```

---

## Manual verification (after both tasks)

1. Build: `cargo build --release -p mur-core`.
2. On a bash agent with an `ask` policy (so HITL fires), run `./target/release/mur agent cli <agent> --auto-reads`.
3. Confirm the startup note. Ask it to run a read (`cat Cargo.toml`, `git status`): the card shows `[read · auto]`, a `auto-approved \`bash\`` note appears, and **no `[y/a/n]` prompt**.
4. Ask it to run a write (`echo hi > /tmp/x`, `git commit`): the `[y/a/n]` prompt still appears (not auto-approved).
5. Sanity: WITHOUT `--auto-reads`, every bash prompts as before (read lane off).

## Out of scope (ponytail)

- A `/auto-reads` live slash toggle (the flag is enough for v1; mirrors `/auto` if wanted later).
- Classifying non-`bash` tools (MUR agents are bash-centric; other tools always prompt).
- A user-configurable allowlist (the built-in conservative set covers the common reads; config later if asked).
- Auto-approving `cargo check`/`sed`/`awk` (they execute build scripts / can write — deliberately excluded; fail-safe).

## Self-Review (completed)

- **Spec coverage:** classifier (T1), flag + read-lane wiring + card tag (T2). ✔
- **Placeholder scan:** none — full classifier + tests + wiring code given; the allowlist contents are explicit, not "TODO add commands". ✔
- **Type consistency:** `is_readonly_bash(&str)->bool` (T1) consumed in the Hitl arm (T2); `App.auto_reads` + `StepCard.auto_approved` + `mark_card_auto_approved` defined and consumed in T2; `HitlRequest.tool_input["command"]` is the confirmed source. ✔
- **Safety:** opt-in (default off), conservative classifier (fail-safe false), every auto-approval visible (note + card tag); dangerous/metachar/write cases explicitly asserted `false` in T1. ✔
- **E0027 guard:** T2 Step 4 runs `cargo test --no-run`. ✔
