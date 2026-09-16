# MUR as an MCP server — serving its own tools to a spawned CLI

Status: **draft, awaiting approval**
Date: 2026-09-16

## Problem

The CLI-spawn design (`2026-09-16-cli-spawn-backends-design.md`) chose "the
CLI owns the loop, MUR owns the tools", and described the MUR side in one
sentence: *MUR's tools are mounted into it as an MCP server that MUR itself
serves.* No such server exists. `mur-agent-runtime/src/mcp/` is a client
pool (`McpClient`), pointing the other way.

That one sentence is carrying the whole safety argument of the CLI track,
because it is what puts entitlements, secret masking and the HITL gate in
front of a model running inside somebody else's process.

## The hazard, already written down in this repo

The obvious implementation — an MCP server that resolves a tool name and
calls `ToolExecutor::execute()` — is specifically what the runtime warns
against. Two comments in `task_runner.rs` say so, unprompted:

> D8: every tool executes inside the owner task's scope, from BOTH execute
> sites, so `bash` can stamp its job. One helper — **a third site must call
> it too or its jobs belong to nobody.**

> The one place tool output is scrubbed before it can reach the model. BOTH
> tool-execution sites call this; **a third site must too, or that tool's
> output reaches the model unmasked.**

An MCP server calling `execute()` is exactly that third site.

## What the guarded path actually does

Measured by reading `task_runner.rs`, not assumed. Around every tool call,
in order:

1. **Resolve the step notifier**, routed by task id, so `step/started` and
   `step/completed` reach the right channel.
2. **Policy gate** — `effective_tool_policy(&self.tools_policy, name)`:
   - `Deny` → refuse without executing, `DenialScope::Tool`, and tell the
     model the tool is gone *for the rest of this turn*.
   - `Ask` → the HITL batch gate first, bounded by `hitl_timeout_secs`.
   - `Allow` → straight through.
3. **`execute_scoped(tool, task_id, input)`** — runs inside
   `CURRENT_TASK_ID`, which is how a spawned `bash` job gets an owner and
   can be killed with its task.
4. **`self.masked(out.text)`** — the single scrubbing chokepoint for
   secrets, applied to output before the model can read it.
5. **`ToolError::NotAuthorized` → withdrawal**, so a refused tool is not
   retried for the remainder of the turn.

Five obligations. A second implementation of them is a second thing to keep
correct, and the failure mode is silent: a missed step does not error, it
just stops protecting.

## Goal

Let a spawned CLI call MUR's tools over MCP, with every call passing the
same five obligations as an in-process turn — by construction, not by
reimplementation.

### Hard constraint

**No second execution path.** If a future reader can find two places where
a MUR tool runs, the design has failed regardless of whether both are
currently correct.

## Chosen: extract the chokepoint, serve through it

Lift the guarded sequence out of `task_runner.rs` into one callable unit —
working name `GuardedToolCall` — holding the tools, the policy, the HITL
gate, the secret vault and the notifier. `task_runner` calls it. The MCP
server calls it. Nothing calls `ToolExecutor::execute()` except that unit.

```
in-process turn ─┐
                 ├─► GuardedToolCall ─► policy · HITL · scope · mask ─► ToolExecutor
spawned CLI ─────┘        (the only caller of execute())
   via MCP
```

The extraction is the deliverable, not a refactor done along the way: it is
what makes the hard constraint checkable by `grep`.

## Rejected: an MCP server that calls tools directly

Resolve the name, call `execute()`, return the output. It is perhaps thirty
lines and it is wrong in five separate ways at once — no policy gate, no
HITL, no task scope (so `bash` jobs outlive their task with no owner to kill
them), no secret masking, no withdrawal on refusal.

The tempting half-measure — copy the five steps into the server — is worse
than it looks: it type-checks, it passes a demo, and it drifts. The repo's
own comments predict exactly this ("a fix that lands on one of them is not
a fix").

## Where the code lives

`mur-mcp-server` cannot be reused as-is: it depends on `mur-core`, and
`mur-agent-runtime` must not, because that pulls LanceDB and Arrow into
every agent process. But the dependency is not in the protocol — measured:

| file | lines | `mur_core` references |
|---|---|---|
| `jsonrpc.rs` | 126 | **0** |
| `server.rs` | 124 | **0** |
| `tools.rs` | 1198 | 50 |

So the protocol and the dispatch loop are already free of it; only the tool
set is bound. Extract those 250 lines into a crate below both — working name
`mur-mcp-proto` — exactly as the repo rule prescribes for anything two
crates on either side of that line must share. `mur-mcp-server` keeps its
`mur-core` tools; `mur-agent-runtime` serves the agent's tools; neither
grows a dependency on the other.

## What the CLI is offered

Only MUR's tools, and this is measurable rather than assumed. The
`--tools "" --strict-mcp-config --mcp-config <file>` recipe was probed on
`claude` 2.1.273 and its `system init` event reported `tools: []` — an empty
list before MUR mounts anything.

That is the acceptance condition for a spawn: the CLI's own `init` output
must list MUR's tools **and nothing else**. Read from the CLI's report, not
from the model's answer — during the probe the model said `NO_TOOLS` in a
run where 44 of the user's own MCP tools were in fact mounted.

## Transport and lifetime

One stdio server per spawned turn, not a long-lived daemon on a port:

- The CLI already speaks stdio MCP, and `--mcp-config` names the command to
  run, so there is nothing to discover and no port to collide.
- A server that dies with its turn cannot be reached by anything outside
  that turn. A listening socket is an authorization surface; not opening one
  is cheaper than defending one.
- `task_id` is fixed for the life of the process, which is what makes
  obligation 3 (task scope) hold without the server having to be told which
  task a call belongs to.

## HITL has a transport: `elicitation/create`

An earlier draft of this document claimed MCP has no "ask the human and come
back" state. That was wrong, and the correction changes the design rather
than just a sentence.

`elicitation/create` is a **server → client** request: the server pauses
inside `tools/call`, asks the client to obtain something from the user, and
resumes when the reply arrives. That is precisely the shape obligation 2
needs.

It is also not merely in the specification. Probed against `claude` 2.1.273
by standing up a minimal MCP server and reading the `initialize` params it
received:

```json
{
  "protocolVersion": "2025-11-25",
  "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
  "clientInfo": { "name": "claude-code", "version": "2.1.273" }
}
```

The client declares `elicitation`, and the round trip completes. The same
probe server issued an `elicitation/create` from inside a `tools/call`, and
the call returned normally carrying the outcome.

### Unattended is already fail-closed

In `-p` (headless) mode, with no human to ask, the client answered:

```json
{ "action": "cancel" }
```

Immediately — it did not hang, and it did not approve. That matters more
than the happy path, because it is the behaviour MUR's unattended-HITL
design already specifies: an approval that cannot be obtained parks rather
than blocking, and is never assumed. `cancel` maps onto parking the
`HitlRequest` and returning at once; the step is blocked, not failed, and
the existing `action_hash` matching lets an approval given later release the
gate on a subsequent run. See
`docs/superpowers/specs/2026-08-19-unattended-hitl-defer-design.md`.

So the gate does not need a new concept for the spawned case. It needs a
second transport for one it already has.

### What is not established

The probe ran headless, so it shows the unattended path and the protocol
round trip. It does **not** show an interactive `claude` session actually
prompting a human and returning `accept` — that needs a session a script
cannot drive. Nor does it say anything about `codex` or `agy`: neither was
probed for elicitation support, and a client that does not declare the
capability leaves this backend without a HITL transport, which under the
activation gate keeps it disabled.

## Open questions

1. Whether `GuardedToolCall` needs to be re-entrant. An in-process turn
   holds the loop; a spawned CLI may issue concurrent `tools/call`
   requests. The HITL batch gate is written around a turn's worth of calls
   arriving together, and it is not yet established that it behaves
   correctly when calls arrive independently.
2. Whether `codex` and `agy` can even be offered this. Both mount MCP
   *persistently*, into a config file, so a per-turn stdio command implies
   writing that file per turn into their private home. Verified for neither.
3. What `step/started` and `step/completed` mean when the step was initiated
   by a model MUR is not running. The events are how a channel renders a
   turn; a spawned turn's shape is the CLI's, not MUR's.

## Verification plan

- **One execution path, enforced**: a test that greps the runtime for
  `.execute(` and fails on any call site outside `GuardedToolCall`. The
  hard constraint is only real if it is mechanical — the existing comments
  show that a rule kept by attention alone gets a third site eventually.
- **Every obligation, from the server side**: for each of the five, a test
  that drives a `tools/call` through the MCP server and asserts the
  obligation held — a denied tool refuses without executing; an `Ask` tool
  waits for the gate; a `bash` job started via MCP is killed with its task;
  a secret in tool output is masked; a `NotAuthorized` refusal withdraws the
  tool.
- **Parity, not similarity**: the same tool call driven through
  `task_runner` and through the MCP server must produce the same
  `ToolResultEntry` — asserted as one test over both paths, so they cannot
  drift apart quietly.
- **Empty before mount**: spawn `claude` with the three flags and assert its
  `system init` lists MUR's tools and nothing else. Already demonstrated with
  a stub server: the list came back as exactly `["mcp__probe__probe_noop"]`.
- **Elicitation capability, per backend, before that backend ships**: assert
  the client declared `elicitation` in `initialize`. A backend without it has
  no HITL transport, and the activation gate keeps it disabled rather than
  spawning it with obligation 2 unenforceable.
- **Unattended never approves**: drive a gated tool headless and assert the
  outcome is `cancel` or `decline` — never `accept`, and never a hang.
