# Prefer `mur project search` over grep — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Steer Claude Code toward `mur project search` for concept/intent queries while keeping grep authoritative for exact/exhaustive/just-edited searches.

**Architecture:** Three independent pieces — (1) fix the background-index worker so it installs the git post-commit auto-reindex hook (parity with the foreground path); (2) ship a new builtin `mur-project-search` guiding skill that encodes the tool-choice rule; (3) connect the `mur-mcp-server` binary to Claude Code via a project `.mcp.json` so `mur_project_search` becomes a first-class tool.

**Tech Stack:** Rust (edition 2024, workspace crates `mur-core` / `mur-mcp-server`), embedded builtin skills (YAML via `include_str!`), MCP stdio JSON-RPC, git hooks.

**Spec:** `docs/superpowers/specs/2026-06-02-prefer-mur-project-search-over-grep-design.md`

**Commit author note:** this repo's local git config is `github-actions[bot]`. For hand commits, set the author explicitly: `--author="karajanchang <david@twdd.com.tw>"` (also pass `-c user.name=karajanchang -c user.email=david@twdd.com.tw`). Every commit body ends with the `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` trailer.

---

## File Structure

- `mur-core/src/codebase/mod.rs` — `ensure_git_hook` already implemented (line 707). **Add** a `#[cfg(test)]` module (none exists today) covering create/append/idempotency.
- `mur-core/src/cmd/project.rs` — `cmd_project_index_worker` (line ~489) is the background path that **omits** the `ensure_git_hook` call the foreground path has (line 333). **Modify** to call it on success.
- `mur-core/src/skills/mur_project_search.yaml` — **Create**. New builtin guiding skill (mirrors `mur_project_index.yaml`).
- `mur-core/src/cmd/sync_cmd.rs` — `ensure_mur_skill` (line 1127) embeds builtin skills via `include_str!`. **Modify** the `skills` array to register the new skill.
- `.mcp.json` (repo root) — **Create**. Project-scoped MCP server registration launching `mur-mcp-server`.
- `build.sh` — **Modify** to build + install the `mur-mcp-server` binary alongside `mur`.
- `mur-mcp-server/tests/integration.rs` — **Modify** to assert `mur_project_search` appears in `tools/list`.

---

## Task 1: Background-index worker installs the git hook

The foreground index path calls `ensure_git_hook` (`project.rs:333`); the background worker (`cmd_project_index_worker`) does not. A large project whose first index runs via `--background` silently skips hook install. `ensure_git_hook` itself has zero tests. Add tests for it, then add the missing call.

**Files:**
- Modify: `mur-core/src/codebase/mod.rs` (add `#[cfg(test)]` module at end of file; `ensure_git_hook` at line 707)
- Modify: `mur-core/src/cmd/project.rs` (`cmd_project_index_worker`, ~line 489)

- [ ] **Step 1: Write failing tests for `ensure_git_hook`**

Append to `mur-core/src/codebase/mod.rs` (end of file). The function signature is `pub fn ensure_git_hook(project_path: &Path, quiet: bool) -> Result<bool>` and the marker string is `# mur auto-index`.

```rust
#[cfg(test)]
mod git_hook_tests {
    use super::ensure_git_hook;
    use std::fs;

    /// Build a temp dir that looks like a git repo (just needs `.git/hooks`).
    fn temp_repo() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mur-hook-test-{}-{}",
            std::process::id(),
            // monotonic-ish unique suffix without Instant/rand
            fs::read_dir(std::env::temp_dir()).map(|d| d.count()).unwrap_or(0)
        ));
        fs::create_dir_all(base.join(".git").join("hooks")).unwrap();
        base
    }

    #[test]
    fn creates_hook_with_shebang_when_absent() {
        let repo = temp_repo();
        let installed = ensure_git_hook(&repo, true).unwrap();
        assert!(installed, "should report it installed the hook");

        let hook = repo.join(".git/hooks/post-commit");
        let body = fs::read_to_string(&hook).unwrap();
        assert!(body.starts_with("#!/bin/sh"), "must start with shebang");
        assert!(body.contains("# mur auto-index"), "must contain marker");
        assert!(body.contains("project index"), "must run project index");

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn is_idempotent_on_second_call() {
        let repo = temp_repo();
        assert!(ensure_git_hook(&repo, true).unwrap());
        // Second call must be a no-op (marker already present).
        let installed_again = ensure_git_hook(&repo, true).unwrap();
        assert!(!installed_again, "second call should return false");

        let body = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
        assert_eq!(
            body.matches("# mur auto-index").count(),
            1,
            "marker must appear exactly once"
        );

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn appends_to_existing_hook_without_clobbering() {
        let repo = temp_repo();
        let hook = repo.join(".git/hooks/post-commit");
        fs::write(&hook, "#!/bin/sh\necho existing-user-hook\n").unwrap();

        let installed = ensure_git_hook(&repo, true).unwrap();
        assert!(installed);

        let body = fs::read_to_string(&hook).unwrap();
        assert!(
            body.contains("echo existing-user-hook"),
            "must preserve the pre-existing hook content"
        );
        assert!(body.contains("# mur auto-index"), "must append marker block");

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn returns_false_when_not_a_git_repo() {
        let base = std::env::temp_dir().join(format!("mur-hook-nogit-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        // No .git/hooks dir → ensure_git_hook returns Ok(false).
        assert!(!ensure_git_hook(&base, true).unwrap());
        fs::remove_dir_all(&base).ok();
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass against existing `ensure_git_hook`**

Run: `cargo test -p mur-core git_hook_tests`
Expected: all four PASS (they characterize the existing, correct `ensure_git_hook`). If `appends_to_existing_hook_without_clobbering` or `is_idempotent_on_second_call` fail, that is a real defect in `ensure_git_hook` — fix the function before continuing; otherwise proceed.

- [ ] **Step 3: Add the missing `ensure_git_hook` call to the background worker**

In `mur-core/src/cmd/project.rs`, inside `cmd_project_index_worker`, in the `Ok(stats) => { ... }` arm, after `index.release_lock();` and before the `send_notification(...)` call, add:

```rust
            // Parity with the foreground path: install the post-commit
            // auto-reindex hook on first successful index (idempotent).
            let _ = crate::codebase::ensure_git_hook(&project_path, true);
```

(Use `let _ =` — a hook-install failure must not fail or mask a successful index. `quiet = true` because the worker has no foreground console.)

- [ ] **Step 4: Verify it compiles and the suite still passes**

Run: `cargo build -p mur-core && cargo test -p mur-core git_hook_tests`
Expected: build succeeds; four tests PASS.

- [ ] **Step 5: Verify lint clean**

Run: `cargo clippy -p mur-core -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/codebase/mod.rs mur-core/src/cmd/project.rs
git -c user.name=karajanchang -c user.email=david@twdd.com.tw commit \
  --author="karajanchang <david@twdd.com.tw>" -m "fix(project): install git hook from background index worker

Background index path skipped ensure_git_hook, so a large project's
first --background index never installed the post-commit auto-reindex
hook. Add the call (idempotent) for parity with the foreground path,
plus characterization tests for ensure_git_hook.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Add the `mur-project-search` guiding skill

The mur skills loaded at session start cover indexing/removal only. There is no skill teaching the tool-choice rule. Ship one as a builtin so it auto-installs for all users (same mechanism as `mur-project-index`).

**Files:**
- Create: `mur-core/src/skills/mur_project_search.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (`ensure_mur_skill` array, ~line 1129)

- [ ] **Step 1: Create the skill YAML**

Create `mur-core/src/skills/mur_project_search.yaml` (mirrors the structure of `mur_project_index.yaml`):

```yaml
name: mur-project-search
version: 0.1.0
publisher: human:mur
description: "Search project code by meaning. Use for concept/intent queries; use grep for exact strings and exhaustive matches."
category: context
hosts: [all]
content:
  abstract: |
    For "where is the code that does X" style questions, use semantic search:
    `mur project search "<intent>"` (or the mur_project_search MCP tool).
    Keep grep for exact symbols, strings, and exhaustive matches.
  context: |
    # mur-project-search — Pick the right code search tool

    Semantic search (hybrid vector + BM25) answers *intent* questions and is
    usually cheaper than grepping then opening many files. But grep is exact,
    exhaustive, and always reflects the working tree right now. Choose per query:

    ## Use `mur project search "<query>"` (or mur_project_search MCP tool) when
    - You are looking for a concept or behavior, not a known token:
      "where is the logic that handles retries", "how does auth work",
      "which file is responsible for decay scoring".

    ## Use grep when
    - You know the exact symbol, string, import, or config key.
    - You need every occurrence (rename, find all callers, dead-code removal).
      Semantic search returns ranked top-k, not all matches — it will miss some.

    ## Hard rules (correctness)
    - Code you created or edited this session and have NOT committed/indexed is
      not in the index yet — use grep for it. Semantic results always lag
      un-committed edits.
    - Before trusting semantic results, check freshness with
      `mur project status`. If it reports no index or indexing in progress,
      fall back to grep.

    The index is kept fresh automatically by a post-commit hook (installed on
    first `mur project index`). See the mur-project-index skill.
tags: [mur, project, search, grep, builtin]
triggers:
  - type: keyword
    pattern: "(where is the code|how does .{0,30} work|which file (handles|is responsible)|find the (logic|code) (that|responsible)|semantic.{0,8}search|search the (codebase|project) for)"
  - type: manual
priority: normal
```

- [ ] **Step 2: Register the skill in `ensure_mur_skill`**

In `mur-core/src/cmd/sync_cmd.rs`, in the `skills` array inside `ensure_mur_skill` (after the `mur-project-index` entry, ~line 1141), add:

```rust
        (
            "mur-project-search",
            include_str!("../skills/mur_project_search.yaml"),
        ),
```

- [ ] **Step 3: Write a test that the new skill is installed**

There is no existing dedicated test for `ensure_mur_skill`. Add one. Append to the existing `#[cfg(test)] mod tests` in `mur-core/src/cmd/sync_cmd.rs` (if none exists, create `#[cfg(test)] mod sync_skill_tests` at end of file):

```rust
    #[test]
    fn installs_project_search_skill() {
        let home = std::env::temp_dir().join(format!(
            "mur-skilltest-{}-{}",
            std::process::id(),
            std::fs::read_dir(std::env::temp_dir()).map(|d| d.count()).unwrap_or(0)
        ));
        std::fs::create_dir_all(&home).unwrap();

        super::ensure_mur_skill(&home).unwrap();

        let skill_yaml = home
            .join(".mur").join("skills").join("mur-project-search").join("skill.yaml");
        assert!(skill_yaml.exists(), "mur-project-search skill.yaml must be written");
        let body = std::fs::read_to_string(&skill_yaml).unwrap();
        assert!(body.contains("name: mur-project-search"));

        std::fs::remove_dir_all(&home).ok();
    }
```

(If the existing test module is named differently, use `ensure_mur_skill` directly via the module path that compiles — it is `pub(crate)`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p mur-core installs_project_search_skill`
Expected: PASS (the `include_str!` resolves at compile time, so a missing/renamed YAML would fail the build first).

- [ ] **Step 5: Verify it renders for AI tools and lint is clean**

Run: `cargo clippy -p mur-core -- -D warnings`
Expected: no warnings. (`ensure_mur_skill` also renders `SKILL.md` next to `skill.yaml`; the test above confirms the YAML, which is the source of truth.)

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/mur_project_search.yaml mur-core/src/cmd/sync_cmd.rs
git -c user.name=karajanchang -c user.email=david@twdd.com.tw commit \
  --author="karajanchang <david@twdd.com.tw>" -m "feat(skills): add mur-project-search guiding skill

Teaches the tool-choice rule: semantic search for concept/intent
queries, grep for exact/exhaustive/just-edited code, with a freshness
fallback. Shipped as a builtin so it auto-installs via ensure_mur_skill.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Connect the `mur-mcp-server` to Claude Code

`mur_project_search` exists as an MCP tool (`mur-mcp-server/src/tools.rs:77`) but the binary is not installed (`~/.mur/bin/` has no `mur-mcp-server`; `build.sh` installs only `mur`) and no `.mcp.json` registers it. Build + install the binary, register it, and lock the `tools/list` contract with a test.

**Files:**
- Modify: `build.sh` (install `mur-mcp-server` alongside `mur`)
- Create: `.mcp.json` (repo root)
- Modify: `mur-mcp-server/tests/integration.rs` (assert `mur_project_search` in `tools/list`)

- [ ] **Step 1: Add a `tools/list` assertion for `mur_project_search`**

In `mur-mcp-server/tests/integration.rs`, the existing `test_initialize_and_list_tools` already initializes and (per the file) checks the server name. Add a new test that sends `tools/list` and asserts the project-search tool is present. Append:

```rust
#[test]
fn lists_project_search_tool() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    );
    let _ = read_response(&mut stdout);

    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    let resp = read_response(&mut stdout);

    let names: Vec<String> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "mur_project_search"),
        "tools/list must include mur_project_search; got {names:?}"
    );

    let _ = child.kill();
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p mur-mcp-server lists_project_search_tool`
Expected: PASS (the tool is already registered in `tools.rs`). This locks the contract so the `.mcp.json` we add in Step 4 stays valid.

- [ ] **Step 3: Make `build.sh` install the `mur-mcp-server` binary**

`build.sh` currently builds the workspace and copies only `mur` to `/opt/homebrew/bin/mur` (line ~58). After that copy, add an install of the MCP server binary. Locate the block in `build.sh`:

```sh
  sudo cp "$BINARY" /opt/homebrew/bin/mur
```

Immediately after it, add:

```sh
  MCP_BINARY="$SCRIPT_DIR/target/release/mur-mcp-server"
  if [ -f "$MCP_BINARY" ]; then
    sudo cp "$MCP_BINARY" /opt/homebrew/bin/mur-mcp-server
    echo "Installed mur-mcp-server -> /opt/homebrew/bin/mur-mcp-server"
  fi
```

(`cargo build --release` already builds the whole workspace, so `target/release/mur-mcp-server` exists after the build step; no extra build flag needed.)

- [ ] **Step 4: Create the project `.mcp.json`**

Create `.mcp.json` at the repo root. Use the installed binary by name (resolved from `PATH`), matching the stdio contract in the MCP design spec (§4.4):

```json
{
  "mcpServers": {
    "mur": {
      "command": "mur-mcp-server",
      "args": [],
      "env": {}
    }
  }
}
```

- [ ] **Step 5: Build, install, and verify the binary runs**

Run:
```bash
./build.sh --release --install
mur-mcp-server --help 2>/dev/null || echo "(no --help; server is stdio-only, that is expected)"
```
Expected: build + install succeed; `/opt/homebrew/bin/mur-mcp-server` exists (`ls -l /opt/homebrew/bin/mur-mcp-server`).

- [ ] **Step 6: Manually verify Claude Code picks up the server**

This is a manual verification step (no automated test — it depends on the Claude Code client).
1. In a NEW Claude Code session in this repo, run `/mcp` (or check the MCP server list).
2. Expected: a `mur` server listed as connected, exposing `mur_project_search` (and the other tools from `tools.rs`).
3. If `ENABLE_CLAUDEAI_MCP_SERVERS` in `~/.claude/settings.json` blocks project MCP servers, note that project-scoped `.mcp.json` servers are governed separately; confirm the server appears and is approved when prompted.

- [ ] **Step 7: Commit**

```bash
git add build.sh .mcp.json mur-mcp-server/tests/integration.rs
git -c user.name=karajanchang -c user.email=david@twdd.com.tw commit \
  --author="karajanchang <david@twdd.com.tw>" -m "feat(mcp): connect mur-mcp-server to Claude Code via .mcp.json

Install the mur-mcp-server binary in build.sh and register it as a
project-scoped MCP server so mur_project_search becomes a first-class
tool alongside Grep. Lock the tools/list contract with a test.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final verification

- [ ] **Run the full mur-core + mcp suites**

Run: `cargo test -p mur-core && cargo test -p mur-mcp-server`
Expected: all tests pass.

- [ ] **Workspace lint + format**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Docs checklist (per CLAUDE.md)**

Consider whether `README.md` and the docs site (`app.mur.run/docs/core`) should mention that the `mur` MCP server exposes `mur_project_search` and the new search-vs-grep guidance. If yes, update in a follow-up commit; if no, note why (internal behavior, no user-facing surface change beyond the skill).

---

## Notes for the implementer

- **Independence:** the three tasks are independent and can land in any order / separate PRs. Task 1 is the smallest and safest; Task 3 has a manual verification step. Do them in order for the cleanest history.
- **Existing hook on dev machines:** an older-format hook (`... --quiet &` instead of `--quiet --background`) may already be installed; the marker check makes `ensure_git_hook` skip it. Rewriting stale-format hooks is explicitly out of scope for this plan (spec "Out of scope").
- **Temp-dir uniqueness in tests:** `Instant`/`rand` are unavailable in the no-`Date::now` constraint; tests use `process::id()` + a dir count for a unique-enough suffix. If a collision ever occurs in CI, switch to a per-test fixed subdir name.
