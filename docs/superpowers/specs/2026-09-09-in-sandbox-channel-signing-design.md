# In-sandbox channel signing — handing the signing capability to a sealed child

Status: **Implemented for `fleet_run`** (mur#1239). Written 2026-09-09 from a
live failure; the record of what was rejected is the point of keeping it.

## Problem

A `mur` process running **inside an agent's kernel sandbox** cannot sign the
channel events it appends, so it writes them unsigned. Every event a fleet run
triggered by an agent produces is therefore outside the v3d signing guarantee —
and that is the most autonomous path MUR has, which inverts the threat model:
the runs with the least human attention have the weakest attribution.

Observed 2026-09-09, three times in one session:

```
WARN mur_core::channel_writer: writer signing key is present but unreadable —
writing this event UNSIGNED. Signature verification is not protecting this
channel while that holds. channel_id="01a08548-…" router_agent="mur"
error=identity key exists but is not readable:
/Users/david/.mur/keys/mur/identity.key: Operation not permitted (os error 1)
```

`~/.mur/channels/fleet-develop-rust/events.jsonl` currently holds a mix: 13
signed events (written by the operator's own unsandboxed `mur fleet run`) and 27
unsigned (written from inside the sandbox). Same code, same channel, same
writer identity — only the environment differs.

## Why it happens

- `mur_common::identity::AgentIdentity::load()` reads
  `<mur_home>/keys/<agent>/identity.key` **from disk on every call**.
- `<mur_home>/keys` is a kernel-denied subtree for every sandboxed process
  (`sandbox::launch_chain`, #850 option (c)). This is deliberate: a
  prompt-injected agent must not be able to exfiltrate its own signing key,
  which would let an attacker forge that agent's events off-box forever.
- The **runtime** escapes this only because it loads the identity into an
  `Arc<AgentIdentity>` at startup, *before* the sandbox seals
  (`supervisor.rs:195`) — the same pre-seal idiom used for provider secrets.
- `mur-core`'s writers (`executor/dag.rs`, `hitl/gate.rs`, `cmd/channel.rs`)
  have no such cached copy: they are libraries called from whatever process
  happens to host them, which in the agent-triggered case is a sealed child.

The write side already fails honestly (#975): with `MUR_CHANNEL_REQUIRE_SIG`
set, `append_as_writer` refuses rather than downgrading; without it, it warns.
So this is **not** a silent-failure bug any more. What is missing is the
capability itself.

## What must be true of any fix

1. A sealed child of an agent can produce signatures attributable to that agent.
2. A prompt-injected agent **cannot obtain the key material**, directly or by
   reading it out of another process it can observe.
3. Nothing that merely *reaches the agent's socket* gains the ability to sign.
4. No change to the on-disk key layout, and no new place a key is written.

## Options considered

### A. Pass the key in an environment variable — REJECTED

Simple, and it mirrors the secret handoff (`2026-09-07-murmur-secret-handoff-design.md`).
It is unsafe here: the bash tool can run `ps eww $(pgrep -f "mur fleet run")`,
which on macOS shows the environment of another process **of the same uid**.
A prompt-injected agent would read the key out of its own child. The secret
handoff gets away with env because those credentials are already the agent's to
use and are masked in tool results; a signing key is precisely the thing the
sandbox exists to keep away from the model.

### B. A `channel/sign` A2A method on the agent socket — REJECTED

Keeps the key in the parent and hands out one signature at a time, which is the
narrower capability in the abstract. But the agent's unix socket is a documented
passthrough (`mur agent dial <name> <method>`), so anything that can dial it —
including the agent's own bash tool — could sign arbitrary events as the agent.
That is a signing oracle reachable by the exact actor requirement 2 excludes.

### C. Route the child's appends back through the parent — REJECTED

Architecturally the cleanest (only the runtime ever signs), but the child would
still have to ask the parent over the socket, which is option B with more steps;
and it means reworking every `append_as_writer` call site in `mur-core` to know
whether it is sandboxed. Large, and it lands in the same oracle.

### D. Inherited pipe — CHOSEN

The runtime already holds the key from before the seal. At spawn it writes the
key into the child's **stdin** pipe and sets `MUR_CHANNEL_SIGNING_STDIN=1`.

Stdin rather than a `dup2`'d fd 3: `fleet_run` already passed `Stdio::null()`
there, nothing in a fleet run reads stdin (the HITL gate prompts only on a
TTY, and a pipe is not one), and it needs no `pre_exec` — which runs between
fork and exec, where the safe operations are few and the failure modes are
quiet. The security properties are identical; the fd variant remains the
answer if stdin ever gains a real use.

- Requirement 2 holds: an fd number is not the key. `/dev/fd/3` in the bash
  tool's shell is *that shell's* fd 3, not the child's. Reading another
  process's descriptors needs `task_for_pid`, which SIP denies.
- Requirement 3 holds: nothing is added to the socket surface.
- Requirement 4 holds: the key is never written to disk anywhere new.

## Design

**Runtime (`mur-agent-runtime`)**

- `FleetRunTool` carries `signing: Option<Arc<AgentIdentity>>` and the
  profile's `key_version` — both already in scope where the tool is built.
- On spawn: stdin is a pipe, `MUR_CHANNEL_SIGNING_STDIN=1` is set, and one
  JSON line (`SigningHandoff`) is written and the handle dropped, so the child
  sees EOF. The payload is ~200 bytes, far under a pipe buffer, so the write
  never blocks even against a child that never reads it.
- `None` — tests, and any runtime with no identity — spawns exactly as before.
- The MCP pool spawns long-lived children that also host the DAG
  (`parallel_jobs`). They are NOT covered; see Open questions.

**Core (`mur-core`)**

- `channel_writer::ingest_signing_handoff()` runs from `main` before anything
  else can touch stdin, so the pipe is drained deterministically rather than by
  whichever code path reads first. It fills a process-lifetime `OnceLock`.
- The cached identity is used only when the event names that same writer; a run
  writing on behalf of another agent falls through to the disk path.
- The disk path stays the default for every unsandboxed caller, unchanged.
- Precedence is fd → disk → `NotFound` (unsigned, the legitimate bootstrap
  case) → unreadable (bail or warn, per `MUR_CHANNEL_REQUIRE_SIG`, as today).

## Tests

1. A child given the fd signs; `verify_event` accepts against the agent's pubkey.
2. A child given **no** fd, with the key unreadable, still behaves as today:
   bail under `MUR_CHANNEL_REQUIRE_SIG`, warn otherwise. (Guards the fallback.)
3. A malformed or truncated fd payload does not panic and does not sign — it
   degrades to the disk path, then to the existing unreadable branch.
4. Negative control: the fd is closed after the first read, so a second read
   yields nothing rather than a stale key.
5. Live: run a fleet from `fleet_run` and assert that the resulting
   `events.jsonl` has **no** unsigned events — the mixed 13/27 file above is the
   before-picture and the acceptance criterion.

Shipped: test 1's core (the payload survives `read_line` as one line and
rebuilds a bit-identical key) as `a_signing_handoff_round_trips_to_the_same_key_on_one_line`,
and test 2 by construction — `signing: None` leaves the old path untouched.

**Test 5 has NOT been run.** It matters most: every other test can pass while
the wiring never reaches the process that actually writes fleet events. Run it
against a real `~/.mur` before believing this works.

## Open questions

- **Does the MCP server need it?** `parallel_jobs` runs the DAG *inside* the MCP
  server process, so its events are unsigned today. Giving that long-lived,
  model-facing process the key is a wider grant than giving it to a short-lived
  `mur fleet run`. Leaning: no — instead make `parallel_jobs` write through the
  runtime, or accept unsigned there and say so.
- **Key rotation mid-run.** A child holds the key for its lifetime. A rotation
  (`mur agent rekey`) during a long fleet run leaves the child signing with the
  previous version. `key_version` is already carried per event, so a verifier
  can resolve it via `rotations.jsonl` — confirm that path covers it.
- **Linux.** `dup2` + `pre_exec` is portable; Landlock denies `keys/` the same
  way. No known divergence, but it is untested there.
