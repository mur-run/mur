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

- `ToolExecutor` trait (`tools/mod.rs`): `name()`, `def() -> ToolDef`, `async execute(input) -> Result<String, ToolError>`.
- `TaskRunner` already holds `tools: Vec<Arc<dyn ToolExecutor>>`, `pending_approvals`, `notifier`,
  `hitl_timeout_secs`, `max_iterations` (verified `task_runner.rs:62–72`). **Note:** `tools` is a flat
  `Vec`, *not* a name→executor map, and there is **no `entitlements` field yet** — Phase 4 adds both a
  name lookup and an entitlements thread-through.
- `handle_tool_call(call) -> Result<ToolResultEntry, TaskError>`: a returned `Err(TaskError)` fails
  the whole task, so recoverable tool/policy failures must be `Ok(ToolResultEntry { is_error: true })`,
  reserving `Err` for fatal cases only.
- `HitlDecision { allow, reason }` over `pending_approvals`; the HITL notification carries generic
  `tool_name` + `tool_input`. (The verified emission was the Phase-2 *test* handler — confirm the real
  Phase-3 `handle_tool_call` emits the same shape during reconcile.)

**Blocker:** Phase 3 must be **merged** before this plan executes — the `ToolExecutor`/loop symbols it
builds on are still on an in-flight branch. The plan's first task is a sync against merged Phase 3; if
anything was renamed, adjust the File Map rather than blindly applying.

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
| `mur-agent-runtime/src/tools/registry.rs` — `build_tools()` | New | Build `bash` + non-`deny` MCP tool set → `(Vec<ToolDef>, HashMap<name, Arc<dyn ToolExecutor>>)`; extends Phase 3's flat `tools: Vec`. |
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

`resolve(rules: &[ToolRule], tool_name: &str) -> ToolPolicy`.

**Pattern syntax (v1 — constrained so resolution is a total order):** a pattern is exactly one of
`bash`, `mcp__<server>__<tool>` (exact), `mcp__<server>__*` (server glob), or `mcp__*` (all MCP).
No mid-string or suffix globs — only an exact name or a single trailing `*`.

**Ordering (most-specific wins, deterministic):**
1. exact-name match, else
2. the trailing-`*` rule with the **longest literal prefix**, else
3. `ToolPolicy::default()` (`Ask`).

Ties can't occur — two trailing-`*` rules with the same prefix are the same pattern, and `perm tool`
upserts by pattern. Matching is a literal `==` (exact) or prefix test (trailing-`*`), so **no glob
crate / new dependency is needed.** `bash` with no rule resolves to `Ask`; no code path makes an
unconfigured tool resolve to `Allow`.

## Tool Naming

Provider tool-name constraint (Anthropic + OpenAI): `^[a-zA-Z0-9_-]{1,64}$` — **slashes are invalid
on the wire.** Therefore:

- Builtin Bash → `bash`.
- MCP tool → **`mcp__<server>__<tool>`** where `<server>` is the `McpServerEntry.name` sanitized to
  `[a-zA-Z0-9_-]` (other chars → `_`). The `mcp__` prefix guarantees no collision with builtins and
  matches the format users already know from Claude Code.
- **No decode at dispatch time.** Each `McpToolExecutor` stores its `server` and `tool` separately
  (set when the registry builds it); the loop dispatches by looking the wire name up in the executor
  map and never parses `mcp__…__…` back apart. (Parsing would be ambiguous — sanitization can
  introduce `__` into a server name.)
- The registry assigns each wire name at build time and guarantees uniqueness: on a sanitized
  collision it logs a warning and suffixes `_2`, `_3`… (deterministic by load order). Caveat: a
  suffixed server (`mcp__github_2__*`) won't match a user's `mcp__github__*` rule — rare, logged.

Entitlement patterns use the same wire form: `bash`, `mcp__github__*`, `mcp__github__merge_pr`.

## MCP Connection Lifecycle — `McpPool`

- Supervisor owns one `McpPool` per agent. Each entry serializes its client (see **C2**):
  `Mutex<HashMap<String /*server*/, Arc<tokio::sync::Mutex<McpClient>>>>`.
- **SandboxPolicy.** The pool is built with `SandboxPolicy::from_entitlements(&entitlements,
  &agent_home)` (`sandbox/policy.rs:27`) — both inputs are in scope at the runner build site
  (`supervisor_runner.rs:~403`) — and passes it to `McpClient::spawn(entry, &policy)`. Exact
  spawn→`list_tools` precedent: `mur-core/src/cmd/agent_mcp_pin.rs:158`.
- **Spawn sequence:** `spawn` → `initialize(&mut self)` (needs `&mut`, so initialize **before** the
  client is shared) → wrap in `Arc<Mutex<…>>` → cache.
- **C2 — one in-flight request per client.** `McpClient::request` releases the stdin lock, then reads
  stdout and discards any line whose JSON-RPC `id` doesn't match as a "notification"
  (`mcp_client.rs:90–102`). Two concurrent `call_tool`s on a shared client would let one reader
  swallow the other's response. The per-client `Mutex` enforces a single outstanding request — correct
  (Phase 3 runs a turn's calls sequentially) and future-proofs concurrent A2A tasks. Calls to the
  *same* server serialize; different servers run concurrently.
- **Lazy at first use, cached for agent lifetime:** the first task that needs tools spawns servers
  with ≥1 non-`deny` tool; warm clients + tool defs are cached → zero spawn cost on later turns.
  Servers whose tools are **all `deny`** are never spawned.
- **Drain stderr (M4).** `spawn` pipes stderr (`mcp_client.rs:59`) but nothing reads it; for a
  lifetime-cached server a chatty stderr can fill the ~64KB pipe and **block the child**. The pool
  spawns a detached per-client task draining stderr to the agent log. (Per-task teardown hid this;
  pooling exposes it.)
- On (re)list, verify `McpServerEntry.description_hash`; on mismatch, re-list, warn, proceed with the
  fresh defs (reuses the existing supply-chain verification intent).
- `McpPool::shutdown()` `McpClient::shutdown()`s every client + aborts drain tasks on agent stop.
- **Deferred:** idle-timeout eviction; install-time cached defs to skip the first-task spawn; evicting
  a server whose tools all became `deny` mid-session (stays warm until stop).

## Tool Discovery (before the first LLM turn)

`build_tools(profile, entitlements, pool) -> (Vec<ToolDef>, HashMap<String, Arc<dyn ToolExecutor>>)`
— extends Phase 3's tool set (it already seeds `bash`); the map is the name lookup `handle_tool_call`
needs, since Phase 3's `tools` is a flat `Vec`.

1. Include the builtin `BashTool` unless `bash` policy is `deny`.
2. **Discover MCP servers concurrently (M3)** via `futures::future::join_all`: for each
   `McpServerEntry`, if *every* tool would resolve to `deny`, skip without spawning; otherwise get its
   `tools/list` from the pool (spawning if needed). Serial discovery would add ~N×(spawn+init) to the
   first turn.
3. For each surviving (non-`deny`) tool, build a `ToolDef`. **C3 — default the schema:** MCP
   `inputSchema` may be absent → `Value::Null` (`mcp_client.rs:140`), which providers reject as an
   invalid tool schema (400s the *whole* request). Coerce null/missing → `{"type":"object","properties":{}}`.
4. Build an `McpToolExecutor` per surviving tool, keyed by wire name in the map.
5. Seed `McpInventory::from_tool_names(...)` from the surviving names, replacing the
   `McpInventory::default()` TODO at `task_runner.rs:~180` so skill Layer 3 sees real tools.

**Ordering note:** the inventory is seeded for skill Layer 3 during *system-prompt assembly*, so
building the prompt triggers pool discovery — intended; the first task pays it once, every later task
is warm. A `tools/list` failure for one server contributes no tools + a logged warning; it never fails
the task and never drops other servers.

## `McpToolExecutor`

```rust
pub struct McpToolExecutor {
    wire_name: String,     // mcp__github__merge_pr
    server: String,        // github
    tool: String,          // merge_pr  (stored — never decoded from wire_name)
    def: ToolDef,          // from tools/list, schema-defaulted
    pool: Arc<McpPool>,
    timeout: Duration,     // H1 — per-call execution cap
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn name(&self) -> &str { &self.wire_name }
    fn def(&self) -> ToolDef { self.def.clone() }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let client = self.pool.client(&self.server).await          // Arc<Mutex<McpClient>>
            .map_err(|e| ToolError::Execution(format!("mcp spawn/connect failed: {e}")))?;
        // H1 — McpClient::request has no internal timeout (mcp_client.rs:90); bound it here.
        let call = async { client.lock().await.call_tool(&self.tool, input).await };
        let raw = match tokio::time::timeout(self.timeout, call).await {
            Err(_)     => return Err(ToolError::Execution("mcp tool timed out".into())),
            Ok(Err(e)) => return Err(ToolError::Execution(format!("{e}"))),
            Ok(Ok(v))  => v,
        };
        // C1 — call_tool returns the raw `result` { content:[…], isError:bool }; respect both.
        let text = render_mcp_result(&raw);
        if raw.get("isError").and_then(|b| b.as_bool()).unwrap_or(false) {
            return Err(ToolError::Execution(text));   // → is_error tool_result; model can recover
        }
        Ok(text)
    }
}
```

**`render_mcp_result(&Value) -> String`** concatenates the `text` of each `content[]` block with
`type == "text"`; non-text blocks (`image`, `resource`) become a short placeholder
(`[image omitted]` / `[resource: <uri>]`) — v1 does not forward binary content to the model. Empty/
missing `content` falls back to the compact JSON of `result`.

`handle_tool_call` converts an executor `Err` into an `is_error` `ToolResultEntry` (Phase 3), so both
timeouts and MCP `isError` flow back to the model as recoverable results.

## Policy Check in `handle_tool_call`

`TaskRunner` gains a `tools_policy: Vec<ToolRule>` field (cloned from `entitlements.tools` at build —
the runner has **no** `entitlements` field today, `task_runner.rs:44–72`). Insert before the existing
HITL/hook/execute steps. `handle_tool_call` returns `Result<ToolResultEntry, TaskError>`, so policy
outcomes are `Ok(...)` results — **not** `Err`, which would fail the whole task:

```rust
match resolve(&self.tools_policy, &call.tool_name) {
    ToolPolicy::Deny => {
        // unreachable from the model (denied tools omitted from ToolDefs) — defense in depth
        return Ok(ToolResultEntry {
            call_id: call.call_id.clone(),
            is_error: true,
            content: format!("Tool `{}` is denied by policy.", call.tool_name),
        });
    }
    ToolPolicy::Ask   => { /* fall through to the existing HITL gate */ }
    ToolPolicy::Allow => { /* skip HITL gate → hook chain → execute */ }
}
```

The global `HitlConfig` master override ("require approval for ALL tools") forces every resolved
`Allow` to behave as `Ask`.

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

## Error Handling (recoverable → `Ok(is_error tool_result)`; only `max_iterations` fails the task)

| Failure | Result |
|---|---|
| MCP spawn / `initialize` failure | `is_error` tool_result for the call; server contributes no tools at discovery |
| `tools/list` failure (one server) | server skipped at discovery + logged warning; others unaffected |
| `call_tool` transport error | `is_error` tool_result; loop continues |
| MCP result `isError: true` (C1) | `is_error` tool_result carrying the rendered content |
| Execution timeout (H1) | `is_error` tool_result ("mcp tool timed out"); loop continues |
| Invalid / null `inputSchema` (C3) | coerced to `{"type":"object"}` at discovery — never reaches the provider as null |
| `description_hash` drift | re-list, warn, proceed with fresh defs |
| stderr backpressure (M4) | drained to agent log by a per-client task; never blocks the child |
| Denied tool named by model | hard-blocked `Ok(is_error)` tool_result |
| `max_iterations` reached | **only** terminal case → `TaskOutcome::Failed` (Phase 3) |

## Testing

- **`ToolPolicy` resolution** (`mur-common`): exact > longer-prefix `*` > shorter-prefix `*` >
  default; unmatched → `Ask`; `bash` unmatched → `Ask`; `deny` → `Deny`; trailing-`*`-only (no
  mid/suffix globs).
- **Wire-name encoding**: `(github, merge_pr)` → `mcp__github__merge_pr`; server sanitization;
  collision suffix `_2`. (No decode test — dispatch is by map lookup; the executor carries server/tool.)
- **`McpToolExecutor::execute`** (stub `McpClient`): text joined from `content[].text`; **`isError:true`
  → `Err`** (C1); **timeout → `Err`** (H1); non-text block → placeholder.
- **Schema defaulting (C3)**: tool with missing/null `inputSchema` → `def.input_schema ==
  {"type":"object","properties":{}}`.
- **`McpPool`**: lazy spawn exactly once per server; warm reuse; **serialized — two concurrent
  `call_tool`s don't interleave** (C2); deny-only server never spawned; `initialize` before share;
  `shutdown` drains clients + stderr tasks.
- **`build_tools`**: deny tool absent from `ToolDef`s + map; deny-only server skipped; one server's
  `tools/list` failure doesn't drop others; concurrent discovery; `McpInventory` seeded.
- **Loop integration** (stub LLM emitting an MCP tool_call): `allow` executes; `ask`-denied →
  `Ok(is_error)` with reason; `deny` tool absent from the request `tools` array.
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
| `mur-agent-runtime/src/tools/registry.rs` | New — `build_tools()` → `(Vec<ToolDef>, HashMap<name, Arc<dyn ToolExecutor>>)` |
| `mur-agent-runtime/src/tools/mod.rs` | `pub mod mcp; pub mod registry;` |
| `mur-agent-runtime/src/task_runner.rs` | New `tools_policy: Vec<ToolRule>` + `with_tools_policy()`; name-lookup map; policy check in `handle_tool_call`; seed `McpInventory` |
| `mur-agent-runtime/src/supervisor.rs` / `supervisor_runner.rs` | Build `McpPool` (+ `SandboxPolicy::from_entitlements`); thread pool + `entitlements.tools` into the runner; `pool.shutdown()` on stop |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Render MCP tool name + JSON args |

## Security & Known Limitations

- **Tool-description injection (L5).** MCP `tools/list` descriptions are sent to the model as
  `ToolDef.description` — a compromised/malicious server can attempt prompt injection or tool
  poisoning. In-scope mitigations: `ask`/HITL gating of consequential calls + `description_hash` drift
  detection on relist. First-party *install-time* trust is out of scope (handled by the existing B0
  signature/pin checks at `mur agent mcp` install).
- **`ask` re-prompts per call (L2).** No "allow for this session" — every call of an `ask` tool fires
  a card. Intentional for consequential actions; a session-scoped grant is a future enhancement.
- **Suffixed server names (L3).** A collision-suffixed `mcp__github_2__*` won't match a
  `mcp__github__*` rule; logged when it occurs.
- **Mid-session deny doesn't evict (L1).** Policy changes are picked up at the next task's discovery
  (denied tools vanish from `ToolDef`s), but an already-warm server stays warm until agent stop.

## Disk Note

Firecuda4tb is near full (~80 MB free at design time). `cargo build` may ENOSPC; run
`cargo clean -p <crate>` to free space before retrying (same caveat as Phase 3).
