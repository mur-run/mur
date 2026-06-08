# Hub Phase 4 — Agentic Tool Ecosystem Design

## Goal

Let the Phase 3 agentic loop call the agent's **configured MCP tools**, governed by a per-tool
`allow` / `ask` / `deny` policy, with the `ask` path reusing the existing HITL approval card.
After this phase a MUR agent in the Hub can use the tools it was actually built with — not just Bash.

## Scope

- **In:**
  - MCP tool dispatch through the existing `McpClient` (`protocol/mcp_client.rs`).
  - Per-tool entitlement policy `allow` / `ask` / `deny` on `Entitlements`, plus a
    `mur agent perm tool <allow|ask|deny> <glob>` CLI.
  - `mcp__<server>__<tool>` tool naming on the wire (builtin Bash stays `bash`).
  - Lazy-spawned, lifetime-cached MCP connection pool.
  - `HitlCard.tsx` rendering of arbitrary MCP tool name + JSON args.
  - Wiring real tool names into `McpInventory` (closes the existing `// TODO: wire to MCP registry`).
- **Out (deferred):**
  - File read/write builtin tools.
  - Ollama tool support.
  - Argument-pattern entitlement rules (e.g. `bash(git push:*)`) — v1 is name → state only.
  - Idle-timeout eviction of pooled MCP servers.
  - Install-time tool-definition cache (a first-task latency optimization).
  - Hub **GUI** tool-policy editor — v1 is CLI-only (`perm tool`).

## Reconcile Checkpoint (read first)

This design assumes the **Phase 3** interfaces as specified in
`2026-06-08-hub-phase3-real-hitl-trigger-design.md`:

- `ToolExecutor` trait (`tools/mod.rs`) with `name()`, `def() -> ToolDef`, `async execute(input) -> Result<String, ToolError>`.
- `TaskRunner::run_agentic_loop` + `handle_tool_call`, and a `tools_for_loop()` accessor that yields the loop's tool set.
- `HitlDecision { allow: bool, reason: Option<String> }` sent over `pending_approvals`.
- The HITL notification already carries generic `tool_name` + `tool_input` (confirmed in `supervisor.rs`).

The implementation plan's **first task** is a 5-minute sync against the *merged* Phase 3 code to
confirm these symbol names/signatures before building on them. If Phase 3 renamed anything, adjust
the File Map, do not blindly apply.

## Architecture

```
                 ┌─────────────────────────── TaskRunner.run_agentic_loop ──────────────────────────┐
                 │                                                                                   │
   LLM turn ──►  │  tools: ToolDef[]  =  bash + (all non-`deny` MCP tools)   ◄── ToolRegistry        │
                 │                                                                                   │
   tool_calls ─► │  handle_tool_call(call):                                                          │
                 │     1. policy = entitlements.tools.resolve(call.tool_name)   // allow|ask|deny    │
                 │     2. ask   → HITL gate (existing pending_approvals + card) → HitlDecision        │
                 │        deny  → blocked → is_error tool_result                                      │
                 │        allow → continue                                                           │
                 │     3. pre_tool_use hook chain (existing)                                          │
                 │     4. executor = registry.get(call.tool_name)                                     │
                 │     5. executor.execute(call.input)  ──► (Bash | McpToolExecutor)                  │
                 │     6. ToolResultEntry { is_error } back into history                              │
                 └───────────────────────────────────────────────────────────────────────────────────┘
                                                   │
                          McpToolExecutor ─────────┤ calls McpPool.client(server)
                                                   ▼
                          McpPool (supervisor-owned): server_name → warm McpClient
                                                   ▼
                          McpClient::call_tool(tool, input)   (existing stdio JSON-RPC)
```

`run_agentic_loop`, `handle_tool_call`, the HITL gate, `McpClient`, and the HITL event shape are
**reused unchanged**. Phase 4 only (a) feeds the loop more tools, (b) inserts a policy check, and
(c) routes MCP calls through a pool.

## New / Changed Components

| Unit | New/Change | Responsibility |
|---|---|---|
| `mur-common/src/agent.rs` — `ToolPolicy`, `ToolRule`, `Entitlements.tools` | New | Per-tool policy data model + resolution. |
| `mur-core/src/agent_admin/perm.rs` — `set_tool_policy()` + `mur agent perm tool` | New | CLI to mutate `Entitlements.tools`. |
| `mur-agent-runtime/src/mcp/pool.rs` — `McpPool` | New | Lazy-spawn + lifetime-cache MCP `McpClient`s; teardown on stop. |
| `mur-agent-runtime/src/tools/mcp.rs` — `McpToolExecutor` | New | `ToolExecutor` impl wrapping one MCP tool; `execute()` → `McpClient::call_tool`. |
| `mur-agent-runtime/src/tools/registry.rs` (or extend Phase 3's `tools_for_loop`) | New/Extend | Build `bash` + non-`deny` MCP tool set: `Vec<ToolDef>` + `name → Arc<dyn ToolExecutor>`. |
| `mur-agent-runtime/src/task_runner.rs` — policy check in `handle_tool_call` | Change | Resolve policy → allow/ask/deny before the existing HITL/hook/execute path. |
| `mur-agent-runtime/src/task_runner.rs` — `McpInventory` population | Change | Replace `McpInventory::default()` (line ~180) with names from the registry. |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Minor | Render generic `tool_name` + pretty JSON `tool_input` (MCP-aware labels). |

## New Types

### `mur-common/src/agent.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPolicy {
    Allow, // run silently, no prompt
    Ask,   // fire HITL approval card each call
    Deny,  // hidden from the LLM; hard-blocked if named
}

impl Default for ToolPolicy {
    fn default() -> Self { ToolPolicy::Ask } // safe default for unconfigured tools
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRule {
    /// Tool name or glob: `bash`, `mcp__github__merge_pr`, `mcp__github__*`.
    pub pattern: String,
    pub policy: ToolPolicy,
}
```

Add to `Entitlements`:

```rust
/// Per-tool call policy for the agentic loop. Empty → every tool defaults to `Ask`.
#[serde(default)]
pub tools: Vec<ToolRule>,
```

### Resolution

A free function / method `resolve(rules: &[ToolRule], tool_name: &str) -> ToolPolicy`:

- **Most-specific wins:** an exact-name rule beats a glob; a longer/more-specific glob beats a
  shorter one. Specificity = (exact before glob, then longer literal-prefix length).
- **No match → `ToolPolicy::default()` (`Ask`).**
- Glob matching is shell-style `*` over the tool name (reuse the same glob crate `perm allow_host`
  already uses; if none, a minimal prefix/`*` matcher — no new dependency without checking).

`bash` is just another tool name; with no rule it resolves to `Ask`. There is no code path that
makes an unconfigured tool resolve to `Allow`.

## Tool Naming

Provider tool-name constraint (Anthropic + OpenAI): `^[a-zA-Z0-9_-]{1,64}$` — **slashes are invalid
on the wire.** Therefore:

- Builtin Bash → `bash`.
- MCP tool → **`mcp__<server>__<tool>`** where `<server>` is the `McpServerEntry.name` sanitized to
  `[a-zA-Z0-9_-]` (other chars → `_`). The `mcp__` prefix guarantees no collision with builtins and
  matches the format users already know from Claude Code.
- The executor splits a wire name back to `(server, tool)` for dispatch: strip `mcp__`, split on the
  **first** `__` into server, remainder is the tool name. (Tool names themselves may contain `__`;
  server names are sanitized but the split is server-first so tool `__` is preserved.)
- If sanitization makes two servers collide, the registry logs a warning and suffixes `_2`, `_3`…
  to keep wire names unique (deterministic by load order).

Entitlement patterns use the same wire form: `bash`, `mcp__github__*`, `mcp__github__merge_pr`.

## MCP Connection Lifecycle — `McpPool`

- Supervisor owns one `McpPool` per agent: `Mutex<HashMap<String /*server*/, Arc<McpClient>>>`.
- **Lazy at first use, cached for agent lifetime:** the first task that needs the agent's tools
  spawns + `initialize` + `tools/list` for every server that has **at least one non-`deny` tool**;
  the warm `McpClient` and its tool defs are cached. Every later turn/task reuses the warm client and
  cached defs → zero spawn cost after the first task.
- Servers whose tools are **all `deny`** are never spawned.
- On (re)list, verify `McpServerEntry.description_hash`; on mismatch, re-list, surface a warning, and
  proceed with the fresh defs (reuses the existing supply-chain verification intent).
- `McpPool::shutdown()` drains and `McpClient::shutdown()`s every client on agent stop.
- **Deferred:** idle-timeout eviction; reading install-time cached defs to skip the first-task spawn.

## Tool Discovery (before the first LLM turn)

`ToolRegistry::build(profile, entitlements, pool)`:

1. Start with the builtin `BashTool` (Phase 3) unless `bash` policy is `deny`.
2. For each `McpServerEntry`: compute each tool's wire name; drop tools whose policy is `deny`. If the
   server has ≥1 surviving tool, obtain its `tools/list` from the pool (spawning if needed) and build
   an `McpToolExecutor` + `ToolDef` per surviving tool. A server contributing zero non-`deny` tools is
   skipped (not spawned).
3. Produce `(Vec<ToolDef>, HashMap<String, Arc<dyn ToolExecutor>>)`.
4. The tool-name list also seeds `McpInventory::from_tool_names(...)`, replacing the
   `McpInventory::default()` TODO at `task_runner.rs:~180` so skill Layer 3 sees real tools.

A `tools/list` failure for one server contributes no tools + a logged warning; it never fails the task.

## `McpToolExecutor`

```rust
pub struct McpToolExecutor {
    wire_name: String,     // mcp__github__merge_pr
    server: String,        // github
    tool: String,          // merge_pr
    def: ToolDef,          // from tools/list
    pool: Arc<McpPool>,
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn name(&self) -> &str { &self.wire_name }
    fn def(&self) -> ToolDef { self.def.clone() }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let client = self.pool.client(&self.server).await
            .map_err(|e| ToolError::Execution(format!("mcp spawn/connect failed: {e}")))?;
        match client.call_tool(&self.tool, input).await {
            Ok(v)  => Ok(render_mcp_result(&v)),       // MCP content blocks → String
            Err(e) => Err(ToolError::Execution(format!("{e}"))),
        }
    }
}
```

`handle_tool_call` already converts an executor `Err` into an `is_error` `ToolResultEntry`
(Phase 3 behavior), so MCP failures flow back to the model as recoverable tool results.

## Policy Check in `handle_tool_call`

Insert before the existing HITL/hook/execute steps:

```rust
let policy = resolve(&self.entitlements.tools, &call.tool_name);
match policy {
    ToolPolicy::Deny => {
        return ToolResultEntry { call_id, is_error: true,
            content: format!("Tool `{}` is denied by policy.", call.tool_name) };
    }
    ToolPolicy::Ask   => { /* fall through to existing HITL gate */ }
    ToolPolicy::Allow => { /* skip HITL gate, go straight to hook chain + execute */ }
}
```

The global `HitlConfig` master override ("require approval for ALL tools") forces every resolved
`Allow` to behave as `Ask`. `Deny` should be unreachable from the model (denied tools aren't in the
`ToolDef` list) but is hard-blocked here as defense-in-depth.

## CLI — `mur agent perm tool`

```
mur agent perm tool <allow|ask|deny> <pattern>   # upsert a ToolRule
mur agent perm tool list                         # show current rules
mur agent perm tool clear <pattern>              # remove a rule
```

`set_tool_policy(name, policy, pattern)` in `agent_admin/perm.rs`: load profile → upsert/replace the
`ToolRule` with matching `pattern` in `Entitlements.tools` → atomic YAML write (same pattern as
`allow_host` / `allow_read`).

## Hub UI — `HitlCard.tsx`

The card already receives `{ tool_name, tool_input, prompt, timeout_ms }`. Changes:

- Show the tool name as a labeled chip; for `mcp__server__tool`, display `server · tool`.
- Pretty-print `tool_input` JSON in a `<pre>` block (collapsed if large).
- Keep the existing allow / deny(-with-reason, from Phase 3) buttons unchanged.

No new Tauri commands or event shapes — the Phase 3 `agent_hitl_respond(name, hitl_id, allow, reason)`
covers MCP tools as-is.

## Error Handling (all recoverable → `is_error` tool_result, never task-fatal)

| Failure | Result |
|---|---|
| MCP spawn / `initialize` failure | `is_error` tool_result for that call; server contributes no tools |
| `tools/list` failure | server skipped during discovery + logged warning |
| `call_tool` error / timeout | `is_error` tool_result; loop continues |
| `description_hash` drift | re-list, warn, proceed |
| Denied tool named by model | hard-blocked `is_error` tool_result |
| `max_iterations` reached | **only** terminal case → `TaskOutcome::Failed` (Phase 3) |

## Testing

- **`ToolPolicy` resolution** (`mur-common`): exact > glob > default; longer glob > shorter glob;
  unmatched → `Ask`; `bash` unmatched → `Ask`; `deny` resolves to `Deny`.
- **Wire-name encode/decode**: `mcp__github__merge_pr` ⇄ (`github`, `merge_pr`); server sanitization;
  collision suffixing; tool names containing `__` survive the server-first split.
- **`McpToolExecutor::execute`** against a stub `McpClient`: success → rendered string; error → `ToolError`.
- **`McpPool`**: lazy spawn exactly once per server; warm reuse on second call; deny-only server never
  spawned; `shutdown` drains all.
- **`ToolRegistry::build`**: deny tool absent from `ToolDef`s; deny-only server skipped;
  one server's `tools/list` failure doesn't drop others; `McpInventory` seeded.
- **Loop integration** (stub LLM emitting an MCP tool_call): `allow` executes; `ask` denied →
  `is_error` result with reason; denied tool not present in request `tools`.
- **CLI**: `perm tool allow|ask|deny|list|clear` round-trips through `Entitlements.tools` YAML.

## Files Changed

| File | Action |
|---|---|
| `mur-common/src/agent.rs` | Add `ToolPolicy`, `ToolRule`, `Entitlements.tools`, `resolve()` + tests |
| `mur-core/src/agent_admin/perm.rs` | Add `set_tool_policy` / list / clear |
| `mur-core/src/cmd/agent/perm.rs` (or equivalent CLI dispatch) | Add `perm tool` subcommand |
| `mur-agent-runtime/src/mcp/pool.rs` | New — `McpPool` |
| `mur-agent-runtime/src/mcp/mod.rs` | New — `pub mod pool;` (if `mcp` module doesn't exist) |
| `mur-agent-runtime/src/tools/mcp.rs` | New — `McpToolExecutor` + `render_mcp_result` |
| `mur-agent-runtime/src/tools/registry.rs` | New/Extend — `ToolRegistry::build` |
| `mur-agent-runtime/src/tools/mod.rs` | `pub mod mcp; pub mod registry;` |
| `mur-agent-runtime/src/task_runner.rs` | Policy check in `handle_tool_call`; seed `McpInventory` |
| `mur-agent-runtime/src/supervisor.rs` / `supervisor_runner.rs` | Own `McpPool`; thread into `TaskRunner`/registry; `shutdown` on stop |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Render MCP tool name + JSON args |

## Disk Note

Firecuda4tb is near full (~80 MB free at design time). `cargo build` may ENOSPC; run
`cargo clean -p <crate>` to free space before retrying (same caveat as Phase 3).
