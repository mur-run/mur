# Hub: Agent Permissions — See, Grant, and Stop Being Interrupted

**Status**: Designed, not started. Four phases, one user story.
**Scope**: sub-project A of two. B (browsing the agent's own on-disk files under
`~/.mur/agents/<name>/`) is deliberately not designed here.

## Problem

Two complaints, one story:

1. An agent's entitlements are invisible in the Hub. `CapabilitiesTab` renders
   a `PermissionsTab` that is 41 lines of stub: `capabilities` strings, then the
   *counts* of MCP servers and installed skills. `profile.entitlements` never
   reaches `AgentDetail`. The network mode, allow-hosts, filesystem grants,
   spawn policy, per-tool allow/ask/deny, LLM entitlement and
   `fail_closed_on_sandbox_error` exist only in `mur agent perm`.
2. A task that needs several permissions asks for them one at a time, each
   prompt interrupting the run. The user wants to see what is being asked at
   once and grant item by item — "不要跑一個跳一個".

## Two gates, not one

The design changed when it turned out the interruptions come from two
mechanisms with different physics. Everything below follows from this table.

| | A. Workflow / fleet gate | B. In-chat tool gate |
|---|---|---|
| Where | `mur-core/src/hitl/gate.rs` (`gate()`) | `mur-agent-runtime/src/task_runner.rs` (`ToolPolicy::Ask` arm) |
| Request | Durable `HitlRequest` channel event, pinned by `action_hash` | In-memory oneshot; `tool/approval_needed` sent to the issuing connection |
| Unanswered | `Wait` (stdin is a TTY) / `Defer` (no TTY) / `Deny` (policy floor) | Waits `hitl.timeout_secs`, then **denies** |
| Memory | A settled decision for the same `action_hash` releases the gate without asking (7-day TTL) | None — every call asks again |
| Hub today | `channel_hitl_respond` → `mur_core::cmd::channel::approve`, same path as the CLI | `chat.rs` relays `hitl-approval-needed`, one card per call |

Consequences:

- **Hub-initiated runs already park and continue.** The Hub process has no TTY,
  so `default_unanswered()` picks `Defer`: the step is `blocked`, independent
  branches keep running, the loop stops with `LoopStop::AwaitingApproval`. Gate
  A needs no new mechanism — it needs a Hub surface that shows the accumulated
  batch, resumes the run, and notifies once.
- **Gate B cannot "continue the turn".** The LLM is mid-turn waiting for that
  tool's result. Synthesising a "deferred" result invites the model to route
  around the gate. (Claude Code blocks here too.) The honest goal for chat is
  *fewer* prompts, not zero: batch the calls one response makes, remember
  decisions, and let a decision become a rule.

## Constraints the design must respect

Two facts already encoded in `mur-core/src/cmd/agent/perm.rs` must not be
re-derived, because a second derivation is a second thing to keep true:

1. **Runtime traffic is not MCP-server traffic.** `allow_hosts` guards the
   runtime's own HTTP client and the B0 gate on `network.*` tools. A spawned
   MCP server runs neither; in `inherit` mode it is bounded only by the OS
   sandbox, which restricts ports, not hosts. `outbound_picture()`'s doc
   comment records that showing one subject without the other is exactly how a
   user comes to believe `perm allow-host` scopes their MCP servers.
2. **A configured grant is not an enforced grant.** `paths_picture()` reads the
   running agent's `LockFile` sandbox seal and leads with it: not running means
   nothing is enforced; running with no seal means what took effect is unknown;
   `enforcing: false` means the agent can reach *more* than the list shows.

And two layers must not be conflated when granting:

3. **`ToolPolicy` and entitlements are independent layers.** `ToolRule` is
   name / trailing-glob only (`resolve_tool_policy`: exact > longest prefix >
   default `Ask`). It answers "may this tool run without asking". Whether the
   process can *reach* a path or host is `entitlements.filesystem` /
   `network.outbound`, enforced by the sandbox and the B0 gate. A tool set to
   `Allow` still fails on an ungranted path; a granted path still asks under
   `Ask`.
4. **The runtime re-reads the profile only at start.** Any entitlement write
   takes effect on restart (`warn_if_running`). Nothing may rely on a profile
   write to release a gate that is open now.

## Phases

### P1 — See: the complete permissions table, read-only

**§1.1 One derivation — `mur-core/src/cmd/agent/perm_view.rs` (new)**

```rust
pub struct PermissionsView {
    pub enforcement: Enforcement,       // NotRunning | SealUnknown | Advisory | Enforcing
    pub runtime_outbound: OutboundView, // mode, allow_hosts, model-host note
    pub mcp_servers: Vec<McpNetView>,   // name, mode, detail, bounded_by_allow_hosts
    pub filesystem: PathsView,          // read, write, deny
    pub processes: ProcessesView,       // spawn mode + allowlist
    pub tools: Vec<ToolRuleView>,       // pattern -> allow/ask/deny (+ risk)
    pub llm: LlmView,
    pub limits: LimitsView,
    pub fail_closed_on_sandbox_error: bool,
}
pub fn permissions_view(profile: &AgentProfile, lock: Option<&LockFile>) -> PermissionsView;
```

`outbound_picture()` and `paths_picture()` are rewritten to render *from* the
view. Their text does not change; their existing unit tests are the no-drift
guard. Both stay in `perm.rs` — only the derivation moves, which also brings
`perm.rs` (859 lines) back under the 800-line rule.

**§1.2 DTO** — `AgentDetail.permissions: PermissionsView`, populated in
`mur-hub-gui/src-tauri/src/detail.rs` from the loaded profile plus
`running.lock` when present.

**§1.3 UI** — replaces the stub in Capabilities → Permissions; no new tab.

1. The **enforcement banner comes first**. "Not running — these are the grants
   it would ask for; nothing is enforced until it starts" is more honest than a
   well-formatted table.
2. **Runtime traffic and MCP servers are two blocks.** `inherit` servers are
   marked as not bounded by the allow-hosts above. Constraint 1 is layout, not
   a footnote.
3. Filesystem, processes, tools use the existing badge-row / list vocabulary.
4. Each block carries its `mur agent perm ...` command, copyable. (Superseded
   by P2's controls, but P1 ships alone and a user who can see a grant should
   not have to hunt for how to change it.)

**§1.4 Tests** — derivation tests for all four enforcement states and an
`inherit` server whose `bounded_by_allow_hosts` is false while `allow_hosts` is
non-empty; existing picture tests unchanged; one `detail.rs` projection test.

### P2 — Grant: item-by-item editing from the table

Every row P1 shows can be changed from the Hub: outbound mode and hosts,
filesystem read / write / deny paths, spawn mode and allowlist, tool rules.

- **Write path = load whole `AgentProfile`, mutate one field, save whole.**
  Reuse `load_profile_for_edit` / `save_profile` from `cmd/agent`. Never a
  narrow DTO written back under the same key — `insert(key, T { ..Default })`
  silently drops every field the DTO does not model (#957).
- **Validation is the CLI's.** `validate_host_pattern` and the path rules in
  `perm.rs` are called, not copied. Paths are absolute; the UI shows the
  expanded form it will write.
- **Restart is said, not implied.** After any write to a running agent the row
  shows "takes effect on restart" with a Restart action (Constraint 4).
- `processes`, `limits`, `llm`, `fail_closed_on_sandbox_error` stay read-only
  in P2. Wrong values there break the agent outright; they wait for a request.

### P3 — Chat gate (B): fewer prompts, remembered decisions, decisions that become rules

**§3.1 One card per LLM response.** When a response carries several tool calls
whose effective policy is `Ask`, the runtime emits one `tool/approval_needed`
carrying all of them and waits for one answer set. Each call still gets its own
decision; none executes before its decision. `hitl_id` stays per call so the
existing `tool/hitl_respond` shape is unchanged; the batch is a new optional
`calls: [...]` array on the notification, with the single-call form preserved
for old clients.

**§3.2 Remember settled decisions.** Before asking, the runtime looks up a
settled decision for this `action_hash` (same canonicalisation as
`mur-core::hitl::pin`, which must move to `mur-common::hitl` for the runtime to
share it — the runtime cannot depend on `mur-core`). The store is the agent's
channel (`mur-channel`, already a runtime dependency): a `HitlResponse` for the
hash inside the TTL releases or denies without asking. A human's explicit "no"
outranks any standing grant, matching gate A's ordering.

**§3.3 "This time" or "always".** The Hub's card offers both per call.
*Always* = the narrowest `ToolRule { pattern: <exact tool name>, policy: Allow }`
plus the one-time answer for this `action_hash`. Because of Constraint 3 the
card also shows, when the call names a path or host outside current
entitlements, the P2 grant that would let it *succeed* — as a second, separate
control, never bundled into "always". Because of Constraint 4 the rule takes
effect on restart and the card says so.

**§3.4 Not built.** No "approve all". The user asked for 一一開放; the batch is
a presentation batch, each decision is pinned to its own hash. Bulk *deny* is
allowed (fail-safe direction). No synthetic "deferred" tool result. No
auto-approval path — `yes: false` stays unreachable from unattended paths.

### P4 — Run gate (A): the batch view, one notification, resume

**§4.1 Batch view.** Pending `HitlRequest`s for a run are grouped
run → kind (network / filesystem / process / tool / spend), showing the step
name, the concrete resource from `tool_input`, the tier, and the summary — never
the hash. Per-item approve / deny; multi-select deny only.

**§4.2 One notification at quiescence.** The Hub notifies once when a run has
stopped with `AwaitingApproval` (no runnable step left), with the count —
not once per request. An approval arriving hours later still releases the gate
(7-day TTL); a changed action changes the hash and is asked again. This is the
existing defer contract, surfaced.

**§4.3 Resume.** After answering, "Resume run" re-invokes the run; the executor
skips completed steps via the existing `ToolResult` cursor and passes the gates
that now have answers. The plan must verify this path on a real run before the
button ships — the cursor resume exists for crash recovery and has not been
exercised as a user action.

**§4.4 Audit honesty.** `HitlResponse.surface` is hard-coded `"cli"` in
`cmd::channel::approve`; the Hub calling it today signs as the CLI. Add a
`surface` parameter; the Hub passes `"hub"`. Approvals never write channel
events from the Hub directly — always through the mur-core function the CLI
uses.

**§4.5 "Always" from the batch view** reuses P3.3's projection, in mur-core, so
both gates offer the same rule for the same action.

## Testing

- **P1**: as §1.4.
- **P2**: round-trip test — a profile with every entitlement field set, one
  field changed through the Hub command, all other fields byte-identical.
- **P3**: runtime test that N `Ask` calls in one response produce one
  notification and N decisions; settled-decision lookup test (allow, deny,
  expired); projection tests — exact-name rule only, never a glob; no
  projection for spend/dispatch tools.
- **P4**: gate test that a Hub-surfaced approval carries `surface: "hub"`;
  resume test on a two-step DAG where step 1 was blocked and is later approved.

## Not in scope

- The agent's own files (`profile.yaml`, prompt, `skills/`, keys, logs) —
  sub-project B.
- Editing `processes` / `limits` / `llm` / `fail_closed` (P2 leaves read-only).
- **Learned manifest** — showing "this agent has historically asked for X"
  before a run starts. The data already exists (past `HitlRequest`s in the
  channel); this is the natural P5 and the closest thing to "show everything
  up front" that does not require skills to declare needs. Not designed here.
