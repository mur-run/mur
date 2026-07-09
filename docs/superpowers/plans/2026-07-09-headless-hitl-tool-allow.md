# Headless HITL Unblock — Gateway Tool Pre-Approval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let deep-research workers execute their (read-only, SSRF-guarded, audited) research-gateway MCP tools during headless delegated turns, by stamping a per-tool `ToolPolicy::Allow` rule at provision time — removing the HITL auto-deny that blocks live native deep-research.

**Architecture:** No new subsystem. The runtime already skips the HITL gate for tools resolved to `ToolPolicy::Allow` (`mur-agent-runtime/src/task_runner.rs:1122` — "Execute without HITL gate below"), and `AgentProfile.entitlements.tools` already carries ordered `ToolRule`s with exact + prefix-glob matching (`mur_common::agent::resolve_tool_policy`). The fix is: (1) move the MCP wire-name contract (`mcp__<server>__<tool>`) into `mur-common` so both crates share one definition, (2) have `mur deep-research provision` push an Allow rule for `mcp__research-gateway__*` into each worker profile.

**Tech Stack:** Rust (edition 2024), existing crates only — `mur-common`, `mur-agent-runtime`, `mur-core`. No new dependencies.

## Root Cause (verified 2026-07-09)

A delegated/headless turn that calls a tool takes the `ToolPolicy::Ask` path
(the default — deep-research workers have no tool rules). The Ask path routes
a `tool/approval_needed` notification to the connected client
(`task_runner.rs:1220-1258`); the delegating peer (fleet `run` dial /
`mur agent send`) never answers, so after `hitl_timeout_secs` (default 300 s)
the decision falls to `allow: false` ("timed out") → `hitl_denied` → task ends
`state: failed` with zero replies. That is exactly the operator-E2E failure
mode recorded in `mem:gotcha_mur_agent_send_no_tools`.

Why pre-approval is safe here (and consent stays where it belongs): the two
gateway tools (`research_search`, `research_fetch`) are read-only and every
byte of egress they can cause is already governed by (a) the gateway's own
SSRF guard + deny-hosts + audit log, and (b) the **separate, explicit-consent**
`--grant-egress` step (`BroadAudited` + `[y/N]` prompt, PR #661/#663). Without
that egress grant the gateway inherits restricted (empty-allowlist) egress and
the tools can reach nothing — so the Allow rule on its own grants no new
capability. The risk boundary remains the egress consent.

## Global Constraints

- No hardcoded values: the `mcp__<server>__*` pattern must come from a shared helper in `mur-common`, not a string literal in `mur-core` (CLAUDE.md rule 1).
- Single source file ≤ 800 lines.
- `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` must stay clean.
- Provision tests that call `cmd_create` are Unix-only (`#[cfg(unix)]`) — runtime symlink creation fails on Windows CI ("os error 2"), per commit ba40150b.
- mur-core tests: use plain `cargo test -p mur-core deep_research` (nextest SIGABRT gotcha applies only to bin CLI-parse tests; these are lib tests).
- Env for local builds: `ORT_STRATEGY=download` (mur-core), toolchain cargo path per `mem:gotcha_env_rustup_nextest_missing`.

---

### Task 1: Move MCP wire-naming contract to `mur-common`

The `mcp__<server>__<tool>` format currently lives only in
`mur-agent-runtime/src/tools/naming.rs`. Task 2 (mur-core) needs to build the
pattern `mcp__research-gateway__*`; duplicating the format string would fork
the contract. Move the two pure functions to `mur-common` and add a
`tool_pattern` helper; the runtime module becomes a re-export shim so all
existing call sites (`registry.rs`, tests) keep compiling unchanged.

**Files:**
- Create: `mur-common/src/mcp_naming.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod mcp_naming;` in alphabetical order, between `pub mod media;` and `pub mod mobile;`)
- Modify: `mur-agent-runtime/src/tools/naming.rs` (replace bodies with re-exports; keep its tests)

**Interfaces:**
- Produces: `mur_common::mcp_naming::{sanitize_server(name: &str) -> String, wire_name(server_sanitized: &str, tool: &str) -> String, tool_pattern(server: &str) -> String}`. Task 2 consumes `tool_pattern`.

- [ ] **Step 1: Write the failing test** (in the new `mur-common/src/mcp_naming.rs`, included below with implementation — TDD here means: write the file with tests first referencing the not-yet-written `tool_pattern`, watch it fail, then fill in)

Create `mur-common/src/mcp_naming.rs` with module doc + tests only (functions stubbed out as `todo!()` is NOT allowed in committed code — instead, write the complete file in one pass and rely on the test run to prove behavior; the "failing" checkpoint is the pre-move state where `mur_common::mcp_naming` doesn't exist):

```rust
//! MCP wire-name encoding: `mcp__<server>__<tool>`.
//!
//! Single source of truth for the wire-name contract shared by the agent
//! runtime (tool registry) and mur-core (per-tool policy patterns).
//!
//! The LLM tool-name field must match `^[a-zA-Z0-9_-]{1,64}$`.
//! Server names are sanitised by collapsing non-alphanumeric/dash chars into `_`.

/// Sanitise an MCP server name so it's safe to embed in a wire name.
///
/// Collapses any run of non-`[a-zA-Z0-9-]` chars into a single `_`.
pub fn sanitize_server(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    // Trim trailing underscore that would make the boundary look odd.
    out.trim_end_matches('_').to_string()
}

/// Encode a server + tool name into the `mcp__<server>__<tool>` wire format.
pub fn wire_name(server_sanitized: &str, tool: &str) -> String {
    format!("mcp__{server_sanitized}__{tool}")
}

/// Prefix-glob `ToolRule` pattern matching every tool of `server`
/// (sanitises first): `mcp__<server>__*`. Feed to
/// `mur_common::agent::resolve_tool_policy` / `ToolRule.pattern`.
pub fn tool_pattern(server: &str) -> String {
    format!("mcp__{}__*", sanitize_server(server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal() {
        assert_eq!(sanitize_server("github"), "github");
    }

    #[test]
    fn sanitize_slash_to_underscore() {
        assert_eq!(sanitize_server("my/server"), "my_server");
    }

    #[test]
    fn sanitize_collapse_runs() {
        assert_eq!(sanitize_server("my//server"), "my_server");
    }

    #[test]
    fn sanitize_dash_preserved() {
        assert_eq!(sanitize_server("my-server"), "my-server");
    }

    #[test]
    fn wire_name_format() {
        assert_eq!(wire_name("github", "merge_pr"), "mcp__github__merge_pr");
    }

    #[test]
    fn tool_pattern_matches_wire_names_of_that_server() {
        use crate::agent::{ToolPolicy, ToolRule, resolve_tool_policy};
        let rules = vec![ToolRule {
            pattern: tool_pattern("research-gateway"),
            policy: ToolPolicy::Allow,
            risk: None,
        }];
        // Every tool of the server resolves to Allow…
        let wn = wire_name(&sanitize_server("research-gateway"), "research_search");
        assert_eq!(resolve_tool_policy(&rules, &wn), ToolPolicy::Allow);
        // …other servers' tools stay at the default (Ask).
        let other = wire_name("github", "merge_pr");
        assert_eq!(resolve_tool_policy(&rules, &other), ToolPolicy::Ask);
    }
}
```

Add to `mur-common/src/lib.rs` after `pub mod media;`:

```rust
pub mod mcp_naming;
```

- [ ] **Step 2: Run the new tests, verify they pass**

Run: `cargo test -p mur-common mcp_naming`
Expected: 6 tests PASS.

- [ ] **Step 3: Convert the runtime module to a re-export shim**

Replace the two function bodies in `mur-agent-runtime/src/tools/naming.rs` (keep the `#[cfg(test)] mod tests` block there untouched — it now exercises the re-exported functions, proving the move is behavior-identical):

```rust
//! MCP wire-name encoding — re-exported from `mur_common::mcp_naming`
//! (single source of truth; mur-core builds `ToolRule` patterns from the
//! same contract).

pub use mur_common::mcp_naming::{sanitize_server, wire_name};
```

Delete the original `sanitize_server` / `wire_name` bodies and the module-doc lines they replace. Do NOT touch `registry.rs` — its `use super::naming::wire_name` (or equivalent) path still resolves via the re-export.

- [ ] **Step 4: Run runtime tests to verify nothing broke**

Run: `cargo test -p mur-agent-runtime naming`
Expected: all pre-existing naming tests PASS (they now run against the re-export).

Run: `cargo clippy -p mur-common -p mur-agent-runtime -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/mcp_naming.rs mur-common/src/lib.rs mur-agent-runtime/src/tools/naming.rs
git commit -m "refactor(mcp): move wire-name contract to mur-common::mcp_naming"
```

---

### Task 2: Provision stamps `Allow` for gateway tools on each worker

**Files:**
- Modify: `mur-core/src/cmd/deep_research/provision.rs` (the per-worker loop in `provision()`, currently lines ~95-127; plus one printed line in `cmd_provision()`; plus a new test)

**Interfaces:**
- Consumes: `mur_common::mcp_naming::tool_pattern` (Task 1), `mur_common::agent::{ToolPolicy, ToolRule}` (existing).
- Produces: worker profiles whose `entitlements.tools` contains `ToolRule { pattern: "mcp__research-gateway__*", policy: Allow, risk: None }`. The runtime consumes this via the existing `resolve_tool_policy` path — no runtime change.

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/cmd/deep_research/provision.rs` tests module (same idiom as `provision_creates_restricted_workers_with_gateway`: `MUR_HOME_LOCK`, `MUR_AGENT_BIN_DIR` redirect, `seed_models_yaml`; Unix-only per Global Constraints):

```rust
    // Unix-only: same cmd_create runtime-symlink constraint as the sibling
    // provision tests (see the comment on
    // provision_creates_restricted_workers_with_gateway).
    #[cfg(unix)]
    #[test]
    fn provision_stamps_gateway_tool_allow_rule() {
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};

        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let names = provision(tmp.path(), "dr_tool", 1, DEFAULT_WORKER_MODEL).unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();

        // The gateway tools resolve to Allow (headless delegated turns skip
        // the HITL gate for them)…
        let search = mur_common::mcp_naming::wire_name(
            &mur_common::mcp_naming::sanitize_server(GATEWAY_MCP_NAME),
            "research_search",
        );
        assert_eq!(
            resolve_tool_policy(&p.entitlements.tools, &search),
            ToolPolicy::Allow
        );

        // …while every other tool keeps the fail-closed default (Ask): the
        // rule must be gateway-scoped, never a blanket allow.
        assert_eq!(
            resolve_tool_policy(&p.entitlements.tools, "bash"),
            ToolPolicy::Ask
        );
        assert_eq!(
            resolve_tool_policy(&p.entitlements.tools, "mcp__github__merge_pr"),
            ToolPolicy::Ask
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core provision_stamps_gateway_tool_allow_rule`
Expected: FAIL — `resolve_tool_policy` returns `Ask` for the gateway tool (no rule stamped yet).

- [ ] **Step 3: Stamp the rule in `provision()`**

In `provision()`'s per-worker loop, the "Fix 2" block already loads and saves the profile. Extend that same edit (one load, one atomic save):

```rust
        // Fix 2: seed the agent-level outbound allow-list with loopback so
        // the worker can reach its own LLM endpoint (local cc-proxy) to
        // reason. Stays `restricted` — this only widens the allow-list off
        // empty, never touches the gateway MCP entry's own `network` block
        // (that stays `None`/Inherit until the separate `--grant-egress`
        // step in `grant_egress` below).
        let (path, mut profile) = load_profile_for_edit(&name)?;
        profile.entitlements.network.outbound.allow_hosts = WORKER_LLM_ALLOW_HOSTS
            .iter()
            .map(|h| h.to_string())
            .collect();
        // Pre-approve the gateway's OWN tools (read-only search/fetch) so
        // headless delegated turns don't dead-end on the HITL gate
        // (`tool/approval_needed` has no answerer under fleet delegation →
        // 300 s timeout → deny → task failed). Scoped to
        // `mcp__research-gateway__*` only — every other tool keeps the
        // fail-closed `Ask` default. This grants no egress by itself: the
        // gateway's outbound stays Inherit/restricted until the separate
        // explicit-consent `--grant-egress` step.
        profile.entitlements.tools.push(mur_common::agent::ToolRule {
            pattern: mur_common::mcp_naming::tool_pattern(GATEWAY_MCP_NAME),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        });
        save_profile(&path, &mut profile)?;
```

(`cmd_create` writes a fresh profile, so a plain `push` cannot duplicate; if a
future caller re-provisions over an existing agent, `cmd_create` fails first.)

- [ ] **Step 4: Surface it in `cmd_provision()` output**

After the per-name `println!` loop in `cmd_provision()` (line ~185), add (using the existing `GATEWAY_MCP_NAME` const — do not introduce a new one):

```rust
    println!(
        "  tool policy: {} → allow (gateway search/fetch pre-approved for headless turns)",
        mur_common::mcp_naming::tool_pattern(GATEWAY_MCP_NAME)
    );
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core deep_research`
Expected: all deep_research tests PASS, including the new `provision_stamps_gateway_tool_allow_rule` and the pre-existing `provision_creates_restricted_workers_with_gateway` / `provision_threads_explicit_model_alias` / `grant_sets_broad_audited_with_authorization` / `provision_rejects_zero_and_over_max_count`.

Run: `cargo clippy -p mur-core -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/deep_research/provision.rs
git commit -m "feat(deep-research): pre-approve gateway tools at provision (unblock headless HITL)"
```

---

### Task 3: Documentation

**Files:**
- Modify: `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`: add a short subsection to the provisioning section describing the stamped tool rule and the safety argument (tool Allow ≠ egress grant; consent stays on `--grant-egress`).
- Modify: `docs/architecture/runtime-overview.md` — in the deep-research provisioning description, one sentence: "provision also stamps `mcp__research-gateway__* → allow` so delegated worker turns execute gateway tools without a HITL answerer; all other tools keep the `ask` default."

**Interfaces:** none (docs only).

- [ ] **Step 1: Write the spec subsection**

Add under the provisioning section of the deep-research spec:

```markdown
#### Headless HITL: gateway tool pre-approval

`provision` stamps one `ToolRule` per worker:
`{ pattern: mcp__research-gateway__*, policy: allow }`.

Rationale: fleet-delegated turns are headless — the `tool/approval_needed`
prompt the runtime emits on the default `ask` policy has no answerer, so
risk-tiered tool calls dead-end in a 300 s timeout → deny → `state: failed`
(the operator-E2E blocker). The two gateway tools are read-only and fully
governed downstream (SSRF guard, deny-hosts, audit log), and the rule grants
no egress by itself: the gateway's outbound stays Inherit/restricted until
the separate explicit-consent `--grant-egress` step. The consent boundary is
unchanged; only the redundant per-call prompt (which nothing can answer) is
removed, and only for this one server's tools. Everything else keeps the
fail-closed `ask` default.
```

- [ ] **Step 2: Update runtime-overview.md** (one sentence as above, in the deep-research paragraph)

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/*deep-research*.md docs/architecture/runtime-overview.md
git commit -m "docs(deep-research): document gateway tool pre-approval for headless turns"
```

---

## Operator Verification (manual, after merge — the T11 re-run)

Not a code task; the live proof that the blocker is gone. Requires a running
agent + LLM credentials, so it stays operator-driven:

1. `mur deep-research provision --prefix dr_worker --count 1 --model <alias>` (new build).
2. Point the worker's gateway entry at the stub for a hermetic first check: edit the worker profile's `research-gateway` MCP entry command to the `stub_gateway` binary (same hand-edit as the previous E2E), or grant real egress via `--grant-egress` for the full path.
3. Start the worker, then headless: `mur agent send dr_worker_1 '{"role":"user","parts":[{"kind":"text","text":"Use the research_search tool to find documents about RUST_MEMORY and report the top title."}]}'`
4. **Expected (fixed):** task completes with a real reply quoting stub-corpus content; the runtime log shows the tool executing with NO `tool/approval_needed` emission.
   **Old behavior (bug):** 300 s stall → `state: failed`, zero replies.
5. Full-fleet check: recreate the deep-research fleet, `mur deep-research run <fleet>` — router delegates, workers now return researched answers; convergence via the router's own-line `RESEARCH_COMPLETE` marker.

---

## Phase 2 (separate plan, only if/when needed): channel-routed async HITL

Phase 1 unblocks deep-research because its workers' only risk-tiered tools are
the pre-approvable gateway pair. The **general** capability — arbitrary
risk-tiered tools in headless turns with a real human approver — is a bigger,
separate effort. Design sketch to seed that future plan:

- **Route:** when the Ask path has no live answerer (or unconditionally, config-gated), the runtime appends a `HitlRequest` event to the agent's channel (machinery exists: `mur-common/src/hitl.rs` risk tiers, channel `HitlRequest`/`HitlResponse` events with approval authority at `CHANNEL_SCHEMA_VERSION = 2`, `mur channel approve <channel_id> <hitl_id>`, Ed25519 verify-on-fold).
- **Resolve:** a channel watcher (the v4a `watch_channels` pattern) resolves the pending `HitlApprovals` oneshot when a verified `HitlResponse` folds in; timeout becomes a per-agent config (`hitl_timeout_secs` already exists as a builder knob) with fail-closed deny preserved.
- **Surface:** pending approvals in the murmur TUI (`[y] approve` already exists for socket-routed prompts), Hub approvals inbox, and mobile via the v4a `channel.updated` push.
- **Non-goals:** no blanket fleet-level auto-approve (`yes:false` stays; CLAUDE.md fleet rule "never blanket-approve risk-tiered steps").

---

## Self-Review Notes

- Spec coverage: root cause → Task 2 (the Allow rule); no-hardcoded-values → Task 1 (shared `tool_pattern`); docs → Task 3; live proof → operator verification.
- Type consistency: `ToolRule { pattern: String, policy: ToolPolicy, risk: Option<RiskTier> }` matches `mur-common/src/agent.rs:691`; `tool_pattern` consumed in Task 2 exactly as produced in Task 1.
- Windows CI: the new provision test is `#[cfg(unix)]` like its siblings.
