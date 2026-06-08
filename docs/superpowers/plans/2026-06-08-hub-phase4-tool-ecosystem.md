# Hub Phase 4 — Agentic Tool Ecosystem Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Phase 3 agentic loop call an agent's configured MCP tools, governed by a per-tool `allow`/`ask`/`deny` policy, with `ask` reusing the existing HITL card.

**Architecture:** Add a per-tool entitlement model (`ToolPolicy`/`ToolRule` on `Entitlements`) in `mur-common`; a supervisor-owned `McpPool` (lazy-spawn, lifetime-cache, per-client serialized) wrapping the existing `McpClient`; an `McpToolExecutor` implementing Phase 3's `ToolExecutor` trait; a `build_tools()` that assembles `bash` + non-`deny` MCP tools into `(Vec<ToolDef>, HashMap<name, Arc<dyn ToolExecutor>>)`; a policy check inside `handle_tool_call`; a `perm tool` CLI; and a small HitlCard rendering tweak.

**Tech Stack:** Rust 2024 / Tokio (`mur-common`, `mur-agent-runtime`, `mur-core`), TypeScript/React (`mur-hub-gui/ui`), `serde_json`, `async-trait`, `futures`.

**Spec:** `docs/superpowers/specs/2026-06-08-hub-phase4-tool-ecosystem-design.md`

**Disk note:** Firecuda4tb is near full (~80 MB free at design time). `cargo build` may ENOSPC; run `cargo clean -p <crate>` to free space before retrying.

**Test command:** `cargo test -p <crate> <filter>` (nextest may not be in PATH; plain `cargo test` is fine for these targeted unit tests).

**⚠️ Hard dependency:** This plan builds on Phase 3 symbols (`ToolExecutor`, `ToolError`, `run_agentic_loop`, `handle_tool_call`, `TaskRunner.tools`). **Task 0 must pass before any other task.** If Phase 3 is not yet merged to the working branch, stop and wait.

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `mur-common/src/agent.rs` | Modify | `ToolPolicy`, `ToolRule`, `Entitlements.tools`, `resolve_tool_policy()` |
| `mur-core/src/agent_admin/perm.rs` | Modify | `set_tool_policy` / `list_tool_rules` / `clear_tool_rule` |
| `mur-core/src/cmd/agent/perm.rs` (CLI dispatch — confirm path in Task 0) | Modify | `perm tool <allow\|ask\|deny\|list\|clear>` |
| `mur-agent-runtime/src/tools/naming.rs` | Create | wire-name encode + sanitize + uniqueness |
| `mur-agent-runtime/src/mcp/pool.rs` | Create | `McpPool` (lazy spawn, serialized client, stderr drain, shutdown) |
| `mur-agent-runtime/src/mcp/mod.rs` | Create | `pub mod pool;` |
| `mur-agent-runtime/src/tools/mcp.rs` | Create | `McpToolExecutor` + `render_mcp_result` |
| `mur-agent-runtime/src/tools/registry.rs` | Create | `build_tools()` |
| `mur-agent-runtime/src/tools/mod.rs` | Modify | `pub mod mcp; pub mod registry; pub mod naming;` |
| `mur-agent-runtime/src/lib.rs` | Modify | `pub mod mcp;` (if absent) |
| `mur-agent-runtime/src/task_runner.rs` | Modify | `tools_policy` field + `with_tools_policy()`; name-lookup map; policy gate in `handle_tool_call`; seed `McpInventory` |
| `mur-agent-runtime/src/supervisor_runner.rs` | Modify | build `McpPool` + `SandboxPolicy`; thread pool + `entitlements.tools` into runner; `build_tools` |
| `mur-agent-runtime/src/supervisor.rs` | Modify | `pool.shutdown()` on agent stop |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Modify | render MCP tool name + JSON args |

---

### Task 0: Reconcile against merged Phase 3 (BLOCKER — do first)

**Files:** none modified — verification only.

- [ ] **Step 1: Confirm Phase 3 has merged**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git log --oneline -20 | grep -iE "ToolExecutor|agentic|run_agentic_loop|BashTool" || echo "NOT FOUND — Phase 3 may not be merged"
```
Expected: commits for the Phase 3 tool loop. If "NOT FOUND", **stop** — wait for Phase 3 merge.

- [ ] **Step 2: Pin the exact `ToolExecutor` trait + `ToolError`**

```bash
sed -n '1,80p' mur-agent-runtime/src/tools/mod.rs
```
Confirm: trait method names (`name`, `def`, `execute`), `execute` signature `async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError>`, and the `ToolError` variants (this plan uses `ToolError::Execution(String)` and `ToolError::InvalidInput(String)`). If names differ, note the deltas and adjust every later task's code accordingly.

- [ ] **Step 3: Pin `handle_tool_call` shape + tool lookup**

```bash
grep -n "fn handle_tool_call\|fn run_agentic_loop\|self.tools\|ToolResultEntry" mur-agent-runtime/src/task_runner.rs | head -30
```
Confirm: `handle_tool_call(&self, task_id, call) -> Result<ToolResultEntry, TaskError>`; how it currently finds the executor for a call (linear scan of `self.tools` by `name()`?). Record the exact field name for the tool list (`tools`) and the `ToolResultEntry` field names (`call_id`, `content`, `is_error`).

- [ ] **Step 4: Pin the CLI dispatch path for `perm`**

```bash
grep -rn "perm\b" mur-core/src/cmd/ | grep -iE "allow_host|allow_read|Subcommand|enum Perm" | head
```
Record the file + enum that defines `perm` subcommands (the File Map guesses `mur-core/src/cmd/agent/perm.rs`; correct it here).

- [ ] **Step 5: Pin `agent_home` + entitlements availability at the runner build site**

```bash
grep -n "agent_home\|entitlements\|build_runner\|build_provider_runner" mur-agent-runtime/src/supervisor_runner.rs | head -30
```
Confirm `profile.inner.entitlements` and an `agent_home: &Path` are both in scope where `build_runner` is called (Task 9 depends on this).

- [ ] **Step 6: Record findings**

Write a short note at the top of this plan (a `> RECONCILE:` blockquote) listing any symbol-name deltas from the spec. No commit needed (doc-only scratch); later tasks consume it.

---

### Task 1: `ToolPolicy` + `ToolRule` + resolution in `mur-common`

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: same file, `#[cfg(test)] mod tool_policy_tests`

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `mur-common/src/agent.rs`:

```rust
#[cfg(test)]
mod tool_policy_tests {
    use super::*;

    fn rules() -> Vec<ToolRule> {
        vec![
            ToolRule { pattern: "mcp__github__merge_pr".into(), policy: ToolPolicy::Ask },
            ToolRule { pattern: "mcp__github__*".into(), policy: ToolPolicy::Allow },
            ToolRule { pattern: "mcp__*".into(), policy: ToolPolicy::Deny },
            ToolRule { pattern: "bash".into(), policy: ToolPolicy::Allow },
        ]
    }

    #[test]
    fn exact_beats_glob() {
        assert_eq!(resolve_tool_policy(&rules(), "mcp__github__merge_pr"), ToolPolicy::Ask);
    }

    #[test]
    fn longer_prefix_glob_beats_shorter() {
        // matches mcp__github__* (prefix "mcp__github__") over mcp__* (prefix "mcp__")
        assert_eq!(resolve_tool_policy(&rules(), "mcp__github__list_issues"), ToolPolicy::Allow);
    }

    #[test]
    fn shorter_glob_when_only_match() {
        assert_eq!(resolve_tool_policy(&rules(), "mcp__slack__post"), ToolPolicy::Deny);
    }

    #[test]
    fn exact_bash_rule() {
        assert_eq!(resolve_tool_policy(&rules(), "bash"), ToolPolicy::Allow);
    }

    #[test]
    fn unmatched_defaults_ask() {
        assert_eq!(resolve_tool_policy(&[], "bash"), ToolPolicy::Ask);
        assert_eq!(resolve_tool_policy(&[], "mcp__x__y"), ToolPolicy::Ask);
    }

    #[test]
    fn policy_default_is_ask() {
        assert_eq!(ToolPolicy::default(), ToolPolicy::Ask);
    }

    #[test]
    fn serde_roundtrip_lowercase() {
        let r = ToolRule { pattern: "bash".into(), policy: ToolPolicy::Deny };
        let y = serde_yaml::to_string(&r).unwrap();
        assert!(y.contains("policy: deny"), "got: {y}");
        let back: ToolRule = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, r);
    }
}
```

- [ ] **Step 2: Run tests, verify they fail to compile**

Run: `cargo test -p mur-common tool_policy_tests 2>&1 | tail -15`
Expected: compile errors — `ToolPolicy`, `ToolRule`, `resolve_tool_policy` not found.

- [ ] **Step 3: Add the types + resolution function**

Add near the `Entitlements` definition in `mur-common/src/agent.rs`:

```rust
/// Per-tool call policy for the agentic loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPolicy {
    /// Run silently, no approval prompt.
    Allow,
    /// Fire a HITL approval card on each call.
    Ask,
    /// Hidden from the LLM; hard-blocked if named anyway.
    Deny,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        ToolPolicy::Ask
    }
}

/// A single tool-policy rule. `pattern` is `bash`, an exact `mcp__<server>__<tool>`,
/// a server glob `mcp__<server>__*`, or `mcp__*` (trailing `*` only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRule {
    pub pattern: String,
    pub policy: ToolPolicy,
}

/// Resolve the policy for `tool_name`. Most-specific wins:
/// 1. exact-name match, else
/// 2. the trailing-`*` rule with the longest literal prefix, else
/// 3. `ToolPolicy::default()` (`Ask`).
pub fn resolve_tool_policy(rules: &[ToolRule], tool_name: &str) -> ToolPolicy {
    // 1. exact
    if let Some(r) = rules.iter().find(|r| !r.pattern.ends_with('*') && r.pattern == tool_name) {
        return r.policy;
    }
    // 2. longest-prefix trailing-* match
    let mut best: Option<(&ToolRule, usize)> = None;
    for r in rules.iter().filter(|r| r.pattern.ends_with('*')) {
        let prefix = &r.pattern[..r.pattern.len() - 1]; // strip trailing '*'
        if tool_name.starts_with(prefix) {
            let len = prefix.len();
            if best.map_or(true, |(_, b)| len > b) {
                best = Some((r, len));
            }
        }
    }
    if let Some((r, _)) = best {
        return r.policy;
    }
    // 3. default
    ToolPolicy::default()
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p mur-common tool_policy_tests 2>&1 | tail -15`
Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(common): ToolPolicy/ToolRule + resolve_tool_policy (exact > longest-prefix glob > Ask)"
```

---

### Task 2: `Entitlements.tools` field

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: same file, extend `tool_policy_tests`

- [ ] **Step 1: Write the failing test**

Add to `tool_policy_tests`:

```rust
#[test]
fn entitlements_tools_defaults_empty() {
    let e: Entitlements = serde_yaml::from_str("network: { outbound: { mode: off } }").unwrap();
    assert!(e.tools.is_empty());
}

#[test]
fn entitlements_tools_roundtrip() {
    let mut e = Entitlements::default();
    e.tools.push(ToolRule { pattern: "mcp__github__*".into(), policy: ToolPolicy::Allow });
    let y = serde_yaml::to_string(&e).unwrap();
    let back: Entitlements = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back.tools.len(), 1);
    assert_eq!(back.tools[0].policy, ToolPolicy::Allow);
}
```

> RECONCILE: if `Entitlements` has no `Default` impl or the `network` YAML shape differs, adjust the deserialization string in the first test to a minimal valid `Entitlements` (check the existing `Entitlements` tests in this file for the canonical minimal YAML).

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test -p mur-common tool_policy_tests::entitlements 2>&1 | tail -15`
Expected: compile error — no field `tools` on `Entitlements`.

- [ ] **Step 3: Add the field**

In the `Entitlements` struct, add (after the existing fields):

```rust
    /// Per-tool call policy for the agentic loop. Empty → every tool defaults to `Ask`.
    #[serde(default)]
    pub tools: Vec<ToolRule>,
```

- [ ] **Step 4: Fix any literal initializers**

```bash
grep -rn "Entitlements {" mur-common/src mur-core/src mur-agent-runtime/src | grep -v "test" | head
```
For each struct-literal construction of `Entitlements` (not using `..Default::default()`), add `tools: vec![],`.

- [ ] **Step 5: Run tests + workspace check**

Run: `cargo test -p mur-common tool_policy_tests 2>&1 | tail -15`
Run: `cargo check -p mur-common 2>&1 | grep "^error" | head`
Expected: tests pass; no errors.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(common): Entitlements.tools (Vec<ToolRule>, serde default empty)"
```

---

### Task 3: CLI `mur agent perm tool`

**Files:**
- Modify: `mur-core/src/agent_admin/perm.rs`
- Modify: CLI dispatch file (path confirmed in Task 0 Step 4)
- Test: `mur-core/src/agent_admin/perm.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

In `mur-core/src/agent_admin/perm.rs`, add to the test module (mirror the style of the existing `allow_host`/`allow_read` tests — find one and copy its agent-fixture setup):

```rust
#[test]
fn set_tool_policy_upserts() {
    let (home, name) = test_agent(); // reuse the existing fixture helper in this module
    set_tool_policy(&home, &name, mur_common::agent::ToolPolicy::Allow, "mcp__github__*").unwrap();
    set_tool_policy(&home, &name, mur_common::agent::ToolPolicy::Deny, "mcp__github__*").unwrap(); // upsert
    let profile = load_profile(&home, &name).unwrap(); // reuse existing loader used by other perm tests
    let rules = &profile.entitlements.tools;
    assert_eq!(rules.len(), 1, "upsert must replace, not append");
    assert_eq!(rules[0].policy, mur_common::agent::ToolPolicy::Deny);
}

#[test]
fn clear_tool_rule_removes() {
    let (home, name) = test_agent();
    set_tool_policy(&home, &name, mur_common::agent::ToolPolicy::Ask, "bash").unwrap();
    clear_tool_rule(&home, &name, "bash").unwrap();
    let profile = load_profile(&home, &name).unwrap();
    assert!(profile.entitlements.tools.is_empty());
}
```

> RECONCILE: replace `test_agent()` / `load_profile()` with the exact helper names used by the existing `perm.rs` tests (Task 0). The signatures of `set_tool_policy` below assume those helpers take `&home, &name`; match the existing `allow_host` signature exactly.

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test -p mur-core agent_admin::perm 2>&1 | tail -15`
Expected: compile error — `set_tool_policy` / `clear_tool_rule` not found.

- [ ] **Step 3: Implement the functions**

Match the load → mutate → atomic-write pattern of the sibling `allow_host` in the same file. Add:

```rust
use mur_common::agent::{ToolPolicy, ToolRule};

pub fn set_tool_policy(home: &Path, name: &str, policy: ToolPolicy, pattern: &str) -> Result<()> {
    with_profile_mut(home, name, |p| {
        let rules = &mut p.entitlements.tools;
        if let Some(r) = rules.iter_mut().find(|r| r.pattern == pattern) {
            r.policy = policy;
        } else {
            rules.push(ToolRule { pattern: pattern.to_string(), policy });
        }
        Ok(())
    })
}

pub fn clear_tool_rule(home: &Path, name: &str, pattern: &str) -> Result<()> {
    with_profile_mut(home, name, |p| {
        p.entitlements.tools.retain(|r| r.pattern != pattern);
        Ok(())
    })
}

pub fn list_tool_rules(home: &Path, name: &str) -> Result<Vec<ToolRule>> {
    let p = load_profile(home, name)?;
    Ok(p.entitlements.tools.clone())
}
```

> RECONCILE: `with_profile_mut` / `load_profile` are placeholders for whatever load-mutate-save helper `allow_host` uses. If `allow_host` inlines the load/save instead, inline it here the same way. Do not invent a helper that doesn't exist.

- [ ] **Step 4: Wire the CLI subcommand**

In the CLI dispatch file (Task 0 Step 4), add a `Tool` variant to the `perm` subcommand enum and dispatch:

```rust
// in the Perm subcommand enum:
/// Set or inspect per-tool call policy (allow|ask|deny|list|clear)
Tool {
    #[command(subcommand)]
    action: PermToolAction,
},

#[derive(clap::Subcommand)]
pub enum PermToolAction {
    Allow { pattern: String },
    Ask   { pattern: String },
    Deny  { pattern: String },
    List,
    Clear { pattern: String },
}
```

Dispatch:

```rust
PermToolAction::Allow { pattern } =>
    set_tool_policy(&home, &name, ToolPolicy::Allow, &pattern)?,
PermToolAction::Ask { pattern } =>
    set_tool_policy(&home, &name, ToolPolicy::Ask, &pattern)?,
PermToolAction::Deny { pattern } =>
    set_tool_policy(&home, &name, ToolPolicy::Deny, &pattern)?,
PermToolAction::Clear { pattern } =>
    clear_tool_rule(&home, &name, &pattern)?,
PermToolAction::List => {
    for r in list_tool_rules(&home, &name)? {
        println!("{:<28} {:?}", r.pattern, r.policy);
    }
}
```

> RECONCILE: match the surrounding clap derive style and how `home`/`name` are resolved in sibling `perm` arms.

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p mur-core agent_admin::perm 2>&1 | tail -15`
Run: `cargo build -p mur-core 2>&1 | grep "^error" | head`
Expected: 2 tests pass; builds.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/agent_admin/perm.rs mur-core/src/cmd/
git commit -m "feat(cli): mur agent perm tool <allow|ask|deny|list|clear>"
```

---

### Task 4: Wire-name encoding (`tools/naming.rs`)

**Files:**
- Create: `mur-agent-runtime/src/tools/naming.rs`
- Modify: `mur-agent-runtime/src/tools/mod.rs` (`pub mod naming;`)

- [ ] **Step 1: Create the module with failing tests**

Create `mur-agent-runtime/src/tools/naming.rs`:

```rust
//! MCP tool wire-name encoding. Provider tool names must match `^[a-zA-Z0-9_-]{1,64}$`,
//! so slashes are invalid; we encode as `mcp__<server>__<tool>`. The (server, tool) pair
//! is always carried alongside the wire name by the executor — wire names are NEVER decoded.

/// Sanitize a server name to `[a-zA-Z0-9_-]`, collapsing any run of other chars to a single `_`
/// and trimming leading/trailing `_` so it can't introduce a `__` boundary.
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
    out.trim_matches('_').to_string()
}

/// Build the wire name for an MCP tool.
pub fn wire_name(server_sanitized: &str, tool: &str) -> String {
    format!("mcp__{server_sanitized}__{tool}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_server("github"), "github");
        assert_eq!(sanitize_server("my server"), "my_server");
        assert_eq!(sanitize_server("a..b"), "a_b");      // run collapses to single _
        assert_eq!(sanitize_server("_lead_"), "lead");   // trimmed, no __ boundary
        assert_eq!(sanitize_server("a  b"), "a_b");
    }

    #[test]
    fn wire_name_format() {
        assert_eq!(wire_name("github", "merge_pr"), "mcp__github__merge_pr");
        // tool names may contain __; that's fine — we never split it back apart
        assert_eq!(wire_name("gh", "a__b"), "mcp__gh__a__b");
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-agent-runtime/src/tools/mod.rs` add:
```rust
pub mod naming;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-agent-runtime tools::naming 2>&1 | tail -15`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/tools/naming.rs mur-agent-runtime/src/tools/mod.rs
git commit -m "feat(tools): MCP wire-name encoding + server sanitization (collision-safe, no __ boundary)"
```

---

### Task 5: `McpPool`

**Files:**
- Create: `mur-agent-runtime/src/mcp/pool.rs`
- Create: `mur-agent-runtime/src/mcp/mod.rs`
- Modify: `mur-agent-runtime/src/lib.rs` (`pub mod mcp;` if not present)

- [ ] **Step 1: Create `mcp/mod.rs`**

```rust
//! MCP host-side integration for the agent runtime.
pub mod pool;
```

Add `pub mod mcp;` to `mur-agent-runtime/src/lib.rs` if it isn't already declared (check first: `grep -n "pub mod mcp" mur-agent-runtime/src/lib.rs`).

- [ ] **Step 2: Write `pool.rs` with the pool + a test**

Create `mur-agent-runtime/src/mcp/pool.rs`:

```rust
//! Per-agent MCP connection pool.
//!
//! - Lazy: a server is spawned the first time `client()` is called for it.
//! - Lifetime-cached: warm clients are reused for the agent's lifetime.
//! - Serialized: each client is wrapped in a `tokio::sync::Mutex` so only one
//!   JSON-RPC request is in flight at a time. `McpClient::request` discards lines
//!   whose id doesn't match as "notifications", so concurrent calls on one client
//!   would lose responses (see spec C2).
//! - stderr drained: a detached task reads each child's stderr to the log so a
//!   chatty long-lived server can't fill the pipe and block (spec M4).

use crate::protocol::mcp_client::{McpClient, McpError};
use crate::sandbox::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct McpPool {
    policy: SandboxPolicy,
    entries: HashMap<String, McpServerEntry>,
    clients: Mutex<HashMap<String, Arc<Mutex<McpClient>>>>,
}

impl McpPool {
    /// Build a pool for an agent's configured MCP servers.
    pub fn new(servers: &[McpServerEntry], policy: SandboxPolicy) -> Self {
        let entries = servers.iter().map(|e| (e.name.clone(), e.clone())).collect();
        Self { policy, entries, clients: Mutex::new(HashMap::new()) }
    }

    /// Get (spawning + initializing on first use) a warm client for `server`.
    pub async fn client(&self, server: &str) -> Result<Arc<Mutex<McpClient>>, McpError> {
        let mut guard = self.clients.lock().await;
        if let Some(c) = guard.get(server) {
            return Ok(c.clone());
        }
        let entry = self.entries.get(server).ok_or_else(|| {
            McpError::Server(format!("no MCP server named `{server}` on this agent"))
        })?;
        // spawn → initialize (needs &mut) → share.
        let mut client = McpClient::spawn(entry, &self.policy).await?;
        client.initialize().await?;
        let shared = Arc::new(Mutex::new(client));
        guard.insert(server.to_string(), shared.clone());
        Ok(shared)
    }

    /// List tools for `server` (spawns on first use). Convenience for discovery.
    pub async fn list_tools(
        &self,
        server: &str,
    ) -> Result<Vec<crate::protocol::mcp_client::ToolInfo>, McpError> {
        let c = self.client(server).await?;
        let tools = c.lock().await.list_tools().await?;
        Ok(tools)
    }

    /// Shut down every warm client. Call on agent stop.
    pub async fn shutdown(&self) {
        let mut guard = self.clients.lock().await;
        for (_, c) in guard.drain() {
            // Best-effort: if other Arcs are held, skip; otherwise own + shutdown.
            if let Ok(m) = Arc::try_unwrap(c) {
                m.into_inner().shutdown().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

    fn echo_server(name: &str) -> McpServerEntry {
        // A trivially-spawnable "server": `cat` echoes nothing useful but spawns/initializes
        // will fail gracefully. We only assert lazy/caching behavior reachable without a real
        // MCP server: an unknown server name returns an error without spawning.
        McpServerEntry {
            name: name.into(),
            command: "true".into(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
        }
    }

    fn minimal_policy() -> SandboxPolicy {
        SandboxPolicy::from_entitlements(
            &mur_common::agent::Entitlements::default(),
            std::path::Path::new("/tmp"),
        )
    }

    #[tokio::test]
    async fn unknown_server_errors_without_panic() {
        let pool = McpPool::new(&[echo_server("known")], minimal_policy());
        let err = pool.client("nope").await.err().expect("unknown server must error");
        assert!(format!("{err}").contains("no MCP server"));
    }
}
```

> RECONCILE: `crate::sandbox::SandboxPolicy` import path and `SandboxPolicy::from_entitlements(&Entitlements, &Path)` signature were verified at `sandbox/policy.rs:27`; confirm the module path (`crate::sandbox::SandboxPolicy` vs `crate::sandbox::policy::SandboxPolicy`). If `Entitlements::default()` doesn't exist, build a minimal one like the existing `policy.rs` tests do (`minimal_entitlements()`).

- [ ] **Step 3: Run the test**

Run: `cargo test -p mur-agent-runtime mcp::pool 2>&1 | tail -20`
Expected: 1 test passes (unknown-server path needs no real spawn).

- [ ] **Step 4: Add the stderr-drain task**

In `McpClient::spawn` the child's stderr is piped but unread. Rather than change `McpClient`, drain it from the pool: after a successful `spawn`, the pool cannot reach the child's stderr handle (it's owned inside `McpClient`). **Decision:** add a small accessor to `McpClient` to take its stderr once, then spawn a drain task.

Add to `mur-agent-runtime/src/protocol/mcp_client.rs`:

```rust
// in McpClient: store the stderr handle so the pool can drain it.
// Add field: stderr: Mutex<Option<tokio::process::ChildStderr>>,
// In spawn(): let raw_stderr = child.stderr.take();
//             let stderr = raw_stderr.map(tokio::process::ChildStderr::from_std).transpose()?;
//             ... stderr: Mutex::new(stderr),
pub async fn take_stderr(&self) -> Option<tokio::process::ChildStderr> {
    self.stderr.lock().await.take()
}
```

Then in `McpPool::client`, after `initialize`, before sharing:

```rust
if let Some(mut err) = client.take_stderr().await {
    let server_name = server.to_string();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(&mut err).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(server = %server_name, "mcp stderr: {line}");
        }
    });
}
```

> RECONCILE: confirm `child.stderr` is `Some` (spawn sets `.stderr(Stdio::piped())` — verified `mcp_client.rs:59`). Add the `stderr` field + `from_std` conversion next to the existing `stdin`/`stdout` conversions.

- [ ] **Step 5: Run tests again + build**

Run: `cargo test -p mur-agent-runtime mcp::pool 2>&1 | tail -20`
Run: `cargo build -p mur-agent-runtime 2>&1 | grep "^error" | head`
Expected: passes; builds.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/mcp/ mur-agent-runtime/src/lib.rs mur-agent-runtime/src/protocol/mcp_client.rs
git commit -m "feat(mcp): McpPool (lazy spawn, per-client serialized, stderr drain, shutdown)"
```

---

### Task 6: `McpToolExecutor` + `render_mcp_result`

**Files:**
- Create: `mur-agent-runtime/src/tools/mcp.rs`
- Modify: `mur-agent-runtime/src/tools/mod.rs` (`pub mod mcp;`)

- [ ] **Step 1: Write `render_mcp_result` + tests first**

Create `mur-agent-runtime/src/tools/mcp.rs`:

```rust
//! MCP-backed ToolExecutor: wraps one MCP tool, dispatches via the pool.

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Render an MCP `tools/call` result (`{content:[…], isError}`) to a string for the model.
/// Joins text blocks; non-text blocks become short placeholders.
pub fn render_mcp_result(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return serde_json::to_string(result).unwrap_or_default();
    };
    if content.is_empty() {
        return serde_json::to_string(result).unwrap_or_default();
    }
    let mut out = String::new();
    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => out.push_str(block.get("text").and_then(|t| t.as_str()).unwrap_or("")),
            Some("image") => out.push_str("[image omitted]"),
            Some("resource") => {
                let uri = block
                    .get("resource")
                    .and_then(|r| r.get("uri"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("?");
                out.push_str(&format!("[resource: {uri}]"));
            }
            _ => {}
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

pub struct McpToolExecutor {
    pub wire_name: String,
    pub server: String,
    pub tool: String,
    pub def: ToolDef,
    pub pool: Arc<McpPool>,
    pub timeout: Duration,
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn name(&self) -> &str {
        &self.wire_name
    }
    fn def(&self) -> ToolDef {
        self.def.clone()
    }
    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let client = self
            .pool
            .client(&self.server)
            .await
            .map_err(|e| ToolError::Execution(format!("mcp spawn/connect failed: {e}")))?;
        let tool = self.tool.clone();
        let call = async move { client.lock().await.call_tool(&tool, input).await };
        let raw = match tokio::time::timeout(self.timeout, call).await {
            Err(_) => return Err(ToolError::Execution("mcp tool timed out".into())),
            Ok(Err(e)) => return Err(ToolError::Execution(format!("{e}"))),
            Ok(Ok(v)) => v,
        };
        let text = render_mcp_result(&raw);
        if raw.get("isError").and_then(|b| b.as_bool()).unwrap_or(false) {
            return Err(ToolError::Execution(text));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_text_blocks() {
        let r = json!({"content": [{"type":"text","text":"hello"},{"type":"text","text":"world"}]});
        assert_eq!(render_mcp_result(&r), "hello\nworld");
    }

    #[test]
    fn renders_nontext_placeholder() {
        let r = json!({"content": [{"type":"image","data":"…"}]});
        assert_eq!(render_mcp_result(&r), "[image omitted]");
    }

    #[test]
    fn empty_content_falls_back_to_json() {
        let r = json!({"content": [], "isError": false});
        assert!(render_mcp_result(&r).contains("isError"));
    }
}
```

- [ ] **Step 2: Register module**

In `mur-agent-runtime/src/tools/mod.rs`:
```rust
pub mod mcp;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-agent-runtime tools::mcp 2>&1 | tail -20`
Expected: 3 `render_mcp_result` tests pass.

> RECONCILE: confirm `ToolExecutor`/`ToolError` import path (`super::{ToolError, ToolExecutor}`) and `ToolError::Execution(String)` variant from Task 0. Confirm `crate::llm::ToolDef`.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/tools/mcp.rs mur-agent-runtime/src/tools/mod.rs
git commit -m "feat(tools): McpToolExecutor (timeout + MCP isError handling) + render_mcp_result"
```

---

### Task 7: `build_tools` registry (schema-default C3, concurrent discovery M3)

**Files:**
- Create: `mur-agent-runtime/src/tools/registry.rs`
- Modify: `mur-agent-runtime/src/tools/mod.rs` (`pub mod registry;`)

- [ ] **Step 1: Write `registry.rs` with the schema-default helper + tests**

Create `mur-agent-runtime/src/tools/registry.rs`:

```rust
//! Assemble the agentic loop's tool set: builtin `bash` + non-`deny` MCP tools.

use super::mcp::McpToolExecutor;
use super::naming::{sanitize_server, wire_name};
use super::ToolExecutor;
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;
use mur_common::agent::{resolve_tool_policy, McpServerEntry, ToolPolicy, ToolRule};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// Coerce a possibly-null/missing MCP inputSchema into a valid JSON-Schema object.
/// Providers reject `null` as a tool schema (spec C3).
pub fn schema_or_default(raw: serde_json::Value) -> serde_json::Value {
    if raw.is_object() {
        raw
    } else {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

/// Build the loop's tools. Returns the provider-facing defs + a name→executor map.
/// `bash_def`/`bash_exec` are the Phase 3 builtin (passed in so this module doesn't own Bash).
pub async fn build_tools(
    servers: &[McpServerEntry],
    rules: &[ToolRule],
    pool: Arc<McpPool>,
    bash: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
) -> (Vec<ToolDef>, HashMap<String, Arc<dyn ToolExecutor>>) {
    let mut defs = Vec::new();
    let mut map: HashMap<String, Arc<dyn ToolExecutor>> = HashMap::new();

    if let Some((def, exec)) = bash {
        if resolve_tool_policy(rules, "bash") != ToolPolicy::Deny {
            defs.push(def);
            map.insert("bash".to_string(), exec);
        }
    }

    // Concurrent discovery (spec M3).
    let futs = servers.iter().map(|entry| {
        let pool = pool.clone();
        async move {
            let sanitized = sanitize_server(&entry.name);
            match pool.list_tools(&entry.name).await {
                Ok(tools) => Some((entry.name.clone(), sanitized, tools)),
                Err(e) => {
                    tracing::warn!(server = %entry.name, "mcp tools/list failed: {e}");
                    None
                }
            }
        }
    });
    let discovered = futures::future::join_all(futs).await;

    for (server, sanitized, tools) in discovered.into_iter().flatten() {
        for t in tools {
            let wname = wire_name(&sanitized, &t.name);
            if resolve_tool_policy(rules, &wname) == ToolPolicy::Deny {
                continue;
            }
            let def = ToolDef {
                name: wname.clone(),
                description: t.description.clone(),
                input_schema: schema_or_default(t.input_schema.clone()),
            };
            defs.push(def.clone());
            let exec: Arc<dyn ToolExecutor> = Arc::new(McpToolExecutor {
                wire_name: wname.clone(),
                server: server.clone(),
                tool: t.name.clone(),
                def,
                pool: pool.clone(),
                timeout: MCP_TOOL_TIMEOUT,
            });
            // Uniqueness: suffix on collision (spec — suffix _2, _3…).
            let key = unique_key(&map, wname);
            map.insert(key, exec);
        }
    }
    (defs, map)
}

fn unique_key(map: &HashMap<String, Arc<dyn ToolExecutor>>, base: String) -> String {
    if !map.contains_key(&base) {
        return base;
    }
    for n in 2.. {
        let cand = format!("{base}_{n}");
        if !map.contains_key(&cand) {
            tracing::warn!("wire-name collision on `{base}`; using `{cand}`");
            return cand;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn null_schema_defaults_to_object() {
        assert_eq!(schema_or_default(json!(null)), json!({"type":"object","properties":{}}));
        assert_eq!(schema_or_default(json!("x")), json!({"type":"object","properties":{}}));
    }

    #[test]
    fn object_schema_preserved() {
        let s = json!({"type":"object","properties":{"a":{"type":"string"}}});
        assert_eq!(schema_or_default(s.clone()), s);
    }
}
```

- [ ] **Step 2: Register module + add `futures` dep if needed**

In `mur-agent-runtime/src/tools/mod.rs`: `pub mod registry;`

```bash
grep -n "^futures" mur-agent-runtime/Cargo.toml || echo "ADD futures"
```
If absent, add `futures = "0.3"` to `mur-agent-runtime/Cargo.toml` `[dependencies]` (check the workspace root for a pinned version to match).

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-agent-runtime tools::registry 2>&1 | tail -20`
Expected: 2 schema tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/tools/registry.rs mur-agent-runtime/src/tools/mod.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(tools): build_tools registry (concurrent discovery, schema default, collision suffix)"
```

---

### Task 8: `TaskRunner` policy gate + name lookup + `McpInventory` seeding

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs`

- [ ] **Step 1: Write a failing policy-gate test**

Add to the existing `agentic_tests` (or a new `policy_tests`) module in `task_runner.rs`. Reuse the Phase 3 `SequenceLlm` stub if present; otherwise mirror `run_sync_llm_error_yields_failed`'s `StubLlm`.

```rust
#[tokio::test]
async fn denied_tool_named_by_model_returns_error_result() {
    use mur_common::agent::{ToolPolicy, ToolRule};
    // LLM: first turn calls a denied tool, second turn ends.
    let responses = vec![
        tool_call_response("id-1", "echo hi"), // tool_name = "bash" in the helper
        end_turn_response("done"),
    ];
    let llm = SequenceLlm::new(responses);
    let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
    let pa = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let runner = std::sync::Arc::new(
        TaskRunner::with_llm(llm)
            .with_pending_approvals(pa)
            .with_notifier(notif_tx)
            .with_hitl_timeout_secs(1)
            .with_tools_policy(vec![ToolRule { pattern: "bash".into(), policy: ToolPolicy::Deny }]),
    );
    let spec = TaskSpec {
        input: mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![mur_common::a2a::MessagePart::Text { text: "go".into() }],
        },
        context_task_id: None,
    };
    // Denied tool → Ok(is_error) result fed back → second turn ends → Completed.
    let outcome = runner.run_sync(spec).await;
    assert!(matches!(outcome, TaskOutcome::Completed(_)), "got {outcome:?}");
}
```

> RECONCILE: `tool_call_response`/`end_turn_response`/`SequenceLlm`/`with_pending_approvals`/`with_notifier`/`with_hitl_timeout_secs` come from the Phase 3 plan's `agentic_tests`. If their names differ, adapt. The point of this test: a `Deny` rule makes the loop return a recoverable error result, not crash or execute.

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test -p mur-agent-runtime denied_tool_named 2>&1 | tail -20`
Expected: compile error — `with_tools_policy` not found.

- [ ] **Step 3: Add the field + builder**

In the `TaskRunner` struct (after `max_iterations`):
```rust
    tools_policy: Vec<mur_common::agent::ToolRule>,
```
In every `TaskRunner` constructor/`with_backend` default init, add:
```rust
    tools_policy: Vec::new(),
```
Add the builder (next to `with_max_iterations`):
```rust
pub fn with_tools_policy(mut self, rules: Vec<mur_common::agent::ToolRule>) -> Self {
    self.tools_policy = rules;
    self
}
```

- [ ] **Step 4: Insert the policy gate in `handle_tool_call`**

At the very top of `handle_tool_call`, before the existing HITL/hook/execute logic:

```rust
use mur_common::agent::{resolve_tool_policy, ToolPolicy};
match resolve_tool_policy(&self.tools_policy, &call.tool_name) {
    ToolPolicy::Deny => {
        return Ok(crate::llm::ToolResultEntry {
            call_id: call.call_id.clone(),
            content: format!("Tool `{}` is denied by policy.", call.tool_name),
            is_error: true,
        });
    }
    ToolPolicy::Ask => { /* fall through to the existing HITL gate */ }
    ToolPolicy::Allow => {
        // Skip the HITL gate. Set a local flag the existing code checks, or
        // restructure so the HITL block is conditional on `policy == Ask`.
    }
}
```

> RECONCILE: Phase 3's `handle_tool_call` currently always runs the HITL gate. Restructure so the HITL wait is guarded by `policy == ToolPolicy::Ask` (compute `policy` once at the top; wrap the existing HITL block in `if policy == ToolPolicy::Ask { … }`). Keep the hook chain + execute path unconditional. Honor the `HitlConfig` master override if Phase 3 exposes one (force `Allow`→`Ask`); if not present yet, leave a `// TODO: master override` and note it.

- [ ] **Step 5: Run the test, verify it passes**

Run: `cargo test -p mur-agent-runtime denied_tool_named 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Seed `McpInventory` from the tool map**

Find `let inventory = McpInventory::default(); // TODO: wire to MCP registry` (≈ `task_runner.rs:180`). The runner needs the live tool-name list. Add a field populated at build time:
```rust
    tool_names: Vec<String>,   // wire names of all loop tools (bash + mcp__*)
```
default `Vec::new()`, builder:
```rust
pub fn with_tool_names(mut self, names: Vec<String>) -> Self {
    self.tool_names = names;
    self
}
```
Replace the TODO line with:
```rust
let inventory = mur_common::skill::inventory::McpInventory::from_tool_names(self.tool_names.clone());
```

> RECONCILE: confirm `McpInventory::from_tool_names` path (`mur_common::skill::inventory`). The actual executor map (Task 7) lives in the runner too if Phase 3 dispatches by map; if Phase 3 keeps `tools: Vec<Arc<dyn ToolExecutor>>` and scans by `name()`, you can derive `tool_names` from that Vec instead of a separate field — prefer that to avoid duplication.

- [ ] **Step 7: Run tests + build**

Run: `cargo test -p mur-agent-runtime task_runner 2>&1 | tail -20`
Run: `cargo build -p mur-agent-runtime 2>&1 | grep "^error" | head`
Expected: passes; builds.

- [ ] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): tool policy gate in handle_tool_call + McpInventory seeding"
```

---

### Task 9: Supervisor wiring (build pool + thread into runner)

**Files:**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs`
- Modify: `mur-agent-runtime/src/supervisor.rs`

- [ ] **Step 1: Build the pool + tools at the runner build site**

In `supervisor_runner.rs`, where `build_runner` / `build_provider_runner` assembles the runner (the site that has `profile.inner.entitlements`, `profile.inner.mcp_servers`, and `agent_home` in scope — confirmed Task 0 Step 5):

```rust
use crate::mcp::pool::McpPool;
use crate::sandbox::SandboxPolicy;

let sandbox_policy = SandboxPolicy::from_entitlements(&profile.inner.entitlements, agent_home);
let pool = std::sync::Arc::new(McpPool::new(&profile.inner.mcp_servers, sandbox_policy));

let (tool_defs, tool_map) = crate::tools::registry::build_tools(
    &profile.inner.mcp_servers,
    &profile.inner.entitlements.tools,
    pool.clone(),
    bash_builtin(agent_home), // Phase 3's BashTool as (ToolDef, Arc<dyn ToolExecutor>)
).await;
```

Thread into the runner builder:
```rust
runner = runner
    .with_tools_policy(profile.inner.entitlements.tools.clone())
    .with_tools(tool_defs, tool_map);   // or the Phase 3 equivalent setter
```

> RECONCILE: Phase 3 owns how `tools` get onto the runner (it has a `tools: Vec<Arc<dyn ToolExecutor>>` field). Use Phase 3's setter; if it only accepts a `Vec`, pass `tool_map.values().cloned().collect()` and keep `tool_defs` wherever the loop reads provider defs. `bash_builtin()` is Phase 3's Bash constructor — call whatever Phase 3 exposes; if Phase 3 already injects Bash into the runner, pass `None` for `bash` to `build_tools` and let Phase 3 keep owning Bash (then policy-`deny` of bash is enforced only at the gate — acceptable for v1; note it).

- [ ] **Step 2: Store the pool for shutdown**

Return/stash the `Arc<McpPool>` alongside the runner so the supervisor can shut it down. If `build_provider_runner` returns a tuple, add `Arc<McpPool>` to it; thread it to wherever the agent's stop path lives in `supervisor.rs`.

- [ ] **Step 3: Shut down the pool on agent stop**

In `supervisor.rs`, at the agent shutdown path (where the A2A server / runner is torn down), add:
```rust
pool.shutdown().await;
```

> RECONCILE: locate the existing stop/teardown site (search for where the supervisor exits its serve loop or handles SIGTERM). If there's a `Drop` or explicit shutdown sequence, hook in there.

- [ ] **Step 4: Build the whole runtime**

Run: `cargo build -p mur-agent-runtime 2>&1 | grep "^error" | head -20`
Expected: no errors. Fix signature mismatches against Phase 3's real setters.

- [ ] **Step 5: Run the crate test suite**

Run: `cargo test -p mur-agent-runtime 2>&1 | tail -25`
Expected: all green (existing + new).

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/supervisor_runner.rs mur-agent-runtime/src/supervisor.rs
git commit -m "feat(runtime): wire McpPool + tool policy into the agent runner; shutdown on stop"
```

---

### Task 10: HitlCard renders MCP tool name + args

**Files:**
- Modify: `mur-hub-gui/ui/src/components/HitlCard.tsx`

- [ ] **Step 1: Inspect the current card**

```bash
sed -n '1,120p' mur-hub-gui/ui/src/components/HitlCard.tsx
```
Confirm it receives `tool_name: string` and `tool_input: Record<string, unknown>` (per `types.ts:196`).

- [ ] **Step 2: Add a label helper + args block**

Add near the top of the component file:

```tsx
function toolLabel(toolName: string): string {
  // mcp__server__tool → "server · tool"; bash → "bash"
  if (toolName.startsWith("mcp__")) {
    const rest = toolName.slice("mcp__".length);
    const idx = rest.indexOf("__");
    if (idx > 0) return `${rest.slice(0, idx)} · ${rest.slice(idx + 2)}`;
  }
  return toolName;
}
```

In the render, replace the raw tool-name display with `{toolLabel(req.tool_name)}` and add an args block (collapsed if large):

```tsx
<pre className="hitl-args">
  {JSON.stringify(req.tool_input, null, 2)}
</pre>
```

> RECONCILE: match the existing prop/variable name (`req` vs destructured `tool_name`) and the component's class-name / styling conventions. Keep the Phase 3 allow / deny-with-reason buttons unchanged.

- [ ] **Step 3: Typecheck + lint the UI**

```bash
cd mur-hub-gui/ui && npm run typecheck 2>&1 | tail -15 && npm run lint 2>&1 | tail -15; cd -
```
Expected: no new errors. (If `typecheck`/`lint` scripts differ, use the ones in `mur-hub-gui/ui/package.json`.)

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/HitlCard.tsx
git commit -m "feat(hub-ui): HitlCard renders MCP tool label + JSON args"
```

---

### Task 11: Full-workspace verification

**Files:** none.

- [ ] **Step 1: Workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: green. (If the known flaky mur-core tests fail, re-run with `cargo nextest run --workspace` per project memory.)

- [ ] **Step 2: Lint + format**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -30`
Run: `cargo fmt --check 2>&1 | tail -5`
Expected: no warnings; formatted. Fix anything that fails, then `cargo fmt`.

- [ ] **Step 3: Manual smoke (optional, needs a real MCP server)**

```bash
# Configure an agent with one MCP server, set a policy, run a task that uses a tool.
mur agent perm tool allow "mcp__<server>__<readonly_tool>"
mur agent perm tool ask  "mcp__<server>__*"
# Start the agent (or via Hub) and send a prompt that triggers the tool;
# verify: allow tool runs silently, ask tool raises a HitlCard, deny tool is absent.
```

- [ ] **Step 4: Final commit (if fmt/clippy fixes were made)**

```bash
git add -A
git commit -m "style: clippy/fmt for Hub Phase 4 tool ecosystem"
```

---

## Self-Review Notes (coverage map)

| Spec item | Task |
|---|---|
| `ToolPolicy`/`ToolRule`/resolution (M2 total order) | 1 |
| `Entitlements.tools` | 2 |
| `perm tool` CLI | 3 |
| Wire naming, no decode, collision suffix (M1) | 4, 7 |
| `McpPool` lazy/serialized (C2)/SandboxPolicy (H4)/stderr drain (M4)/initialize-before-share (L4) | 5 |
| `McpToolExecutor`: timeout (H1) + `isError` (C1) + `render_mcp_result` | 6 |
| `build_tools`: concurrent discovery (M3) + schema default (C3) | 7 |
| Policy gate in `handle_tool_call` (H2/H3) + `Ok(is_error)` returns + `McpInventory` seeding | 8 |
| Supervisor wiring + pool shutdown | 9 |
| HitlCard MCP rendering | 10 |
| Tests / clippy / fmt | 1–11 |

**Known v1 limitations (carried from spec, not bugs):** `ask` re-prompts per call (L2); suffixed server names don't match server globs (L3); mid-session deny doesn't evict warm clients (L1); tool-description injection mitigated by `ask` + `description_hash` only (L5). File r/w tools, Ollama tools, argument-pattern rules, idle eviction, install-time def cache, and the GUI policy editor are **out of scope**.
