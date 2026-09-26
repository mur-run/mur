# Channel Consult (`Ask` / `AskReply`) — Design

Date: 2026-09-26
Status: design approved (pending implementation). Every finding below comes
from reading the code: nothing was compiled, tested, or reproduced.
Plan: `docs/superpowers/plans/2026-09-26-channel-consult-plan.md`

## Problem

MUR's agent-to-agent traffic runs one way. The executor delegates to an agent,
the router plans, judges completion and summarises, and each agent runs its own
step. **Agents have no sanctioned way to ask one another a question.** Any
fleet where one specialist needs another's knowledge runs into this, for
example a reviewer asking a designer, a tester asking the product agent, or a
doc writer asking the implementer.

The only workaround today is plain `message/send`, and that path has no
access-control story. The answering agent reads with its own full filesystem
grant, so asking becomes a read-escalation path: an agent learns anything its
peer can read, whatever its own grant says.

This change makes asking a protocol capability with a defined trust model. It
does not rely on a naming convention inside `Message` payloads.

## What changes

- Two first-class channel event kinds, `EventKind::Ask` and
  `EventKind::AskReply` (`mur-common`). They sit next to `Delegation`,
  `Handoff`, `HitlRequest` and `HitlResponse`, which are also collaboration
  acts with their own kinds.
- An `ask` tool in the agent runtime. The **asker's own runtime** appends a
  self-signed `Ask` (actor `Agent{asker}`) and dials the answerer.
- An A2A method, `channel/consult`, in the answerer's runtime. It verifies the
  `Ask` itself, derives the read scope from the asker's own profile (never from
  a request parameter), runs one restricted turn, and appends a self-signed
  `AskReply`.
- Per-actor verification (`actor_pubkey`, `verify_event`) moves from
  `mur-core` down to `mur-channel`. Pure code movement, needed because
  `mur-agent-runtime` must not depend on `mur-core`.
- The store derives the next `seq` from raw log lines, so an event kind a
  binary cannot parse can never make it reuse a `seq`.
- Actor ids are validated before they are joined into a key path (P1).

**Crates touched:** `mur-common` (event kinds, payload types), `mur-channel`
(verification move, seq derivation), `mur-agent-runtime` (the `ask` tool, the
`channel/consult` handler, per-task tool allowlist, per-task read policy),
`mur-core` (consumers that match on `EventKind`), `mur-hub-gui` (rendering the
new kinds).

**Schema version:** not bumped. No reader compares `CHANNEL_SCHEMA_VERSION`
against a row, so a bump would not stop an older binary from misreading the
log. The mixed-version hazard is handled in §D7.

**Platforms:** macOS first. `channel/consult` refuses on Linux until P2 lands,
and on Windows until someone verifies it there.

## Goals / Non-goals

**Goals**
- An agent can ask a peer a question and get an answer in the same channel.
- The answer never draws on a file the asker could not read itself.
- Nothing in the dial request can widen that scope.
- If any check cannot be completed, the answerer refuses and states the reason.

**Non-goals**
- **Closing the direct-socket bypass.** A peer with `bash` can still reach
  another agent's socket without going through `consult`. That gap is tracked
  separately. This change still matters without that fix, because once the
  bypass is closed an unscoped `consult` would become the new hole.
- **Scoping the answerer's memory** (§D6).
- **Hiding the reply from other channel participants** (§D5).

## Context: how channel events are signed today

Channel events are Ed25519-signed over
`{v, channel_id, actor, kind, payload, idempotency_key}`, excluding `seq` and
`ts` (`mur-channel/src/sign.rs:2-3`). Verification is not automatic.
`load_events` does not verify (`mur-channel/src/sign.rs:8-9`), and a missing
signature passes unless the caller asks otherwise
(`mur-channel/src/sign.rs:100`: `None => !require_sig,`). The only enforcement
switch is the opt-in `MUR_CHANNEL_REQUIRE_SIG`
(`mur-core/src/channel_writer.rs:9`, parsed at
`mur-core/src/channel_verify.rs:56-60`).

Events are signed in two ways:

- **Router-signed.** The executor writes `Delegation` as
  `ChannelActor::System`, signed by `ROUTER_AGENT`
  (`mur-core/src/executor/dag.rs:883-890`,
  `mur-core/src/channel_writer.rs:22`). Verifying such an event proves that
  the router wrote it. It does not prove which agent the event speaks for.
- **Self-signed.** A delegated specialist appends its own reply, signed with
  its own identity and attributed to `Agent{self}`
  (`mur-agent-runtime/src/protocol/methods/channel_delegate.rs:1-2`,
  `append_self_reply` at `:57`). The identity is loaded before the sandbox
  seals (`mur-agent-runtime/src/tools/fleet_run.rs:62`), and every agent can
  write the channel store (`mur-agent-runtime/src/sandbox/policy.rs:308-312`).

Verification looks up the pubkey from the actor. `Agent{id}` resolves to
`<mur_home>/agents/<id>`, and anything else resolves to the router
(`mur-core/src/channel_verify.rs:9-19`).

## Decisions

### D1. `Ask` / `AskReply` are new `EventKind` variants, not `Message` with a `type` field

`kind` is part of the signed input, so a signed `Message` can never be passed
off as an `Ask`. Existing collaboration acts already have their own kinds
(`mur-common/src/channel.rs:143-154`: `Delegation`, `Handoff`, `HitlRequest`,
`HitlResponse`). A payload convention would have to be re-learned by every
reader: fold, Hub, audit, replay, training data.

**Cost.** Older binaries fail to parse these lines and skip them
(`mur-channel/src/store.rs:143`), logging
`unparseable event line(s) skipped` (`:152`). Skipping is not the only
effect; see §D7.

### D2. The asker's own runtime signs the `Ask`

The actor is `Agent{asker}`, signed with the asker's identity, and the path
follows `append_self_reply`. A router-signed `Ask` would say nothing about who
asked.

Payload (all fields are signed):

| field | purpose |
|---|---|
| `to` | the answerer. `consult` refuses an `Ask` that does not name it |
| `question` | the text the answerer sees |
| `expires_at_ms` | `consult` refuses after this time. The limit comes from config, not a hard-coded constant |
| `nonce` | also written to `idempotency_key` (precedent: `mur-channel/src/governance.rs:128,135`) |

Signing `to` and `expires_at_ms` matters: with only a nonce, an `Ask`
addressed to A could be replayed to make B answer it.

### D3. `channel/consult(channel_id, nonce)`: what the answerer checks, in order

Every failed check refuses the call. Nothing falls back to a default.

1. **Locate** the event with `idempotency_key == nonce` and `kind == Ask` in
   `channel_id`. Refuse if there is none, or if there is more than one. The
   store dedupes on the key (`mur-channel/src/store.rs:200-212`), but a raw
   `bash` append does not go through the store.
2. **Actor.** It must be `Agent{id}`, and `id` must pass
   `mur_common::agent_name::validate_agent_name`
   (`mur-common/src/agent_name.rs:46`). This check has to come before any path
   join; see P1.
3. **Signature.** Verify against that actor's key with `require_sig = true`,
   regardless of the environment variable.
4. **Binding.** `to` must equal the answerer's own name, and `expires_at_ms`
   must not have passed.
5. **Single use.** Record `(asker, nonce)` in the answerer's own state and
   refuse if it is already there. Defence in depth only: the signature already
   covers `channel_id`, which blocks cross-channel copies, and §D5 explains why
   a replay reveals nothing new.
6. **Scope.** Read the asker's `profile.yaml` and take its filesystem
   entitlement. Refuse if the file is missing or does not parse. The dial
   parameters carry no scope.

The caller of the dial is **not** trusted and is not identified
(`mur-agent-runtime/src/communication_policy.rs:23-24` treats an unmatched pid
as the user). The `Ask` authenticates itself, so knowing the caller is
unnecessary.

### D4. The consult turn is restricted twice, and both restrictions are load-bearing

**Read scope.** A path P is readable in the consult turn only when the
answerer could read P in its own turn **and** the asker could read P in its
own turn. The check lives in `decide_read`
(`mur-agent-runtime/src/tools/fs_policy/mod.rs:326`), so `read_file` and the
project-instructions loader (`:343-346`) both go through it.

**Tool allowlist.** On its own, the read-scope check does not hold.
`decide_read` guards the file tools only: `mur-agent-runtime/src/tools/bash.rs`
calls no read or write gate, MCP servers run as their own processes, and the
kernel cage cannot be narrowed per child
(`mur-agent-runtime/src/sandbox/child.rs:39-41`; on macOS a second
`sandbox_apply` is refused, see the measurement at `:45-60`). A consult turn
that could run `bash` would read with the answerer's full grant. So the
consult turn gets `read_file` only: no `bash`, no MCP tools, no write tools,
and no `ask`, which also rules out recursion.

`TaskSpec` has no per-task tool field today
(`mur-agent-runtime/src/task_runner.rs:23`). The only per-turn list is
`disabled` (`:2193`), which holds tools that were refused at runtime. The
allowlist has to be added.

### D5. The reply is as public as the channel

`AskReply` goes to the same channel, and every agent can read and write the
channel store (`mur-agent-runtime/src/sandbox/policy.rs:308-312`). What the
scope rule bounds is **what the answer can draw on**: never more than the
asker could read. It does not limit **who reads the answer**. That matches the
existing channel model, where anything the asker posts is already visible to
every participant. A third agent that replays the `Ask` gets an answer that
was already posted to the channel.

### D6. Memory is out of scope

`recall` reads the loaded snapshot
(`mur-agent-runtime/src/tools/recall.rs:75`), and memories reach the system
prompt by injection (`mur-agent-runtime/src/skills/injector.rs:1`). An answer
can therefore reflect what the answerer remembers, regardless of file scope.
We accept this, and the user-facing docs have to say so.

### D7. Mixed versions: an older writer can reuse a `seq`

The next `seq` is computed from parsed events only:
`load_events(id)?.last()` (`mur-channel/src/store.rs:214`). Take a log whose
last line is an `Ask`. An older binary does not see that line, so its next
append gets the `Ask`'s `seq`. The index guard `AND ?2 > last_seq`
(`mur-channel/src/index.rs:324`) then drops the new event, and so does every
`seq > since` cursor. The older writer's event disappears silently.

The Hub builds `mur-channel` in (`mur-hub-gui/src-tauri/Cargo.toml:48`) and
appends events itself (`mur-hub-gui/src-tauri/src/chat.rs:318`), so a CLI and
Hub on different versions can hit this.

Changing `CHANNEL_SCHEMA_VERSION` would not help, because no reader compares
it.

**Decision: ship readers first, writers one release later.**
- Release N: the store derives the next `seq` from a minimal `{seq}` parse of
  every line, whatever its kind. Readers handle `Ask` / `AskReply`.
- Release N+1: runtimes start emitting `Ask`.

This protects anyone running two adjacent versions. Anyone who skips release N
is still exposed; the release notes say so.

### D8. Readers that filter only on actor need a kind filter

Several readers treat any `Agent{..}` event as the agent's output. An
`AskReply` would be read as:
- fleet progress, resetting stuck detection
  (`mur-core/src/cmd/fleet/loop_run.rs:1047-1049`);
- a fleet reply printed to the user (`mur-core/src/cmd/fleet/run.rs:597-606`);
- a deep-research final answer
  (`mur-core/src/cmd/deep_research/ask.rs:281-283`).

Each of these needs an explicit kind filter. Neither `Ask` nor `AskReply`
counts as progress.

## Prerequisites (separate PRs, before this change)

**P0. Move verification into `mur-channel`.** Move `actor_pubkey` and
`verify_event` there, with no behaviour change (`mur-agent-runtime` cannot
depend on `mur-core`). The stale Linux comment at
`mur-core/src/channel_verify.rs:29-30` gets corrected in the same PR; see P2.

**P1. Validate the actor id before joining it into a path.** `actor_pubkey`
joins `Agent{id}` into `agents/<id>` unchecked. Every agent can write
`channels/` (`mur-agent-runtime/src/sandbox/policy.rs:308-312`). An id such as
`../channels/<dir>` could therefore point verification at a key the attacker
planted, and would point `consult` at a `profile.yaml` the attacker wrote. The
id has to be validated inside the moved `actor_pubkey`, so every verifier
benefits and not only `consult`. *Found by reading the code; no exploit has
been run.*

**P2. Linux: the asker's `profile.yaml` is readable but cannot be trusted.**
Readability is fine: `agents/` is granted for reading as a whole
(`mur-agent-runtime/src/sandbox/policy.rs:462-472`, commit `feb7107c`), so the
comment at `mur-core/src/channel_verify.rs:29-30` saying peers created after
the seal are unreadable is out of date. Trust is the problem. Landlock has no
deny-within-allow (`mur-agent-runtime/src/sandbox/policy.rs:289-293`).
`agent_home` is always writable (`policy.rs:301-306`), and the self-home is
exempt from grant partitioning
(`mur-agent-runtime/src/sandbox/launch_chain.rs:315-317`). That leaves only
the file-tool gate protecting `profile.yaml` on Linux, and `bash` bypasses it,
so on Linux an agent can apparently rewrite its own entitlements. That is #712
on Linux, a problem that exists independently of this change. `consult`
refuses on Linux until it is fixed. *Found by reading the code; not
reproduced on Linux.*

macOS enforces the write-deny in the kernel
(`mur-agent-runtime/src/sandbox/macos.rs:224-231`). Windows has not been
examined, so `consult` refuses there until someone checks.

## Related finding (out of scope, tracked separately)

The HITL gate verifies a `HitlResponse` but never checks **who** sent it.
`mur-core/src/hitl/gate.rs:410-428` matches on `hitl_id` and verifies the
signature, then takes `allow` from the payload (`:443-449`). There is no actor
check anywhere in the file. The `action_hash` it compares against is written
in the `HitlRequest` payload (`mur-common/src/hitl/mod.rs:195`), in a channel
every agent can write. With `MUR_CHANNEL_REQUIRE_SIG` unset (the default), an
unsigned response passes. Reading the code, an agent can approve its own gated
action. This is the same principle §D3 enforces: a verified signature is not
an authorization.

## Requirements

Each requirement lists the scenarios the implementation must test (plan §F).

### R1. `Ask` is a self-signed, first-class event

A question from one agent to another is an `EventKind::Ask` event, with actor
`Agent{asker}`, signed by the asker's own identity. The payload carries `to`,
`question`, `expires_at_ms` and `nonce`, and the event's `idempotency_key`
equals `nonce`. The router never writes `Ask` events on an agent's behalf.

- **Asker's runtime writes the Ask.** WHEN agent `pm` calls the `ask` tool
  naming `tech-writer`, THEN `pm`'s runtime appends an `Ask` whose actor is
  `Agent{pm}`, signed with `pm`'s key, with `to = "tech-writer"` and
  `idempotency_key` set to the payload's `nonce`.
- **The kind cannot be forged from a Message.** WHEN a signed `Message`
  event's `kind` field is rewritten to `ask`, THEN signature verification
  fails, because `kind` is part of the signed input.

### R2. consult authenticates the `Ask`, not the caller

`channel/consult` accepts only `channel_id` and `nonce`. It locates exactly one
`Ask` with that `idempotency_key`, validates the actor id with
`validate_agent_name`, and verifies the signature with `require_sig = true`
regardless of `MUR_CHANNEL_REQUIRE_SIG`. It refuses when any of these fails.
It never uses the dialer's identity or pid for any decision.

- **Unsigned Ask with enforcement off.** WHEN `MUR_CHANNEL_REQUIRE_SIG` is
  unset and the `Ask` has no `sig`, THEN consult refuses.
- **Traversal in the actor id.** WHEN the `Ask` actor is
  `Agent{"../channels/x"}`, THEN consult refuses before reading any file
  derived from that id.
- **Two Asks share a nonce.** WHEN two `Ask` lines in the channel carry the
  same `idempotency_key`, THEN consult refuses.

### R3. The `Ask` is bound to one answerer and one window

consult refuses an `Ask` whose `to` is not the answering agent's own name, or
whose `expires_at_ms` has passed. It refuses an `(asker, nonce)` pair it has
already answered.

- **Ask addressed to someone else.** WHEN `tech-writer` receives a consult for
  an `Ask` with `to = "reviewer"`, THEN `tech-writer` refuses.
- **Second consult for the same Ask.** WHEN `tech-writer` has already answered
  `(pm, n1)` and is asked to consult `(pm, n1)` again, THEN `tech-writer`
  refuses.

### R4. Read scope is the intersection, computed by the answerer

During a consult turn, a path is readable only when both the answering agent
and the asking agent could read it in their own turns. The asker's side comes
from the asker's `profile.yaml`; nothing in the request contributes to it.
When the asker's profile cannot be read or parsed, consult refuses. The check
lives in the shared read decision, so it covers the file tools and the
project-instructions loader alike.

- **Asker lacks a path the answerer has.** WHEN `tech-writer` can read
  `~/docs/internal` and `pm` cannot, THEN during `pm`'s consult, `read_file` on
  `~/docs/internal/x.md` is refused.
- **Asker has a path the answerer lacks.** WHEN `pm` can read `~/secret` and
  `tech-writer` cannot, THEN during `pm`'s consult, `read_file` on
  `~/secret/x` is still refused.

### R5. The consult turn runs with `read_file` only

A consult turn offers and dispatches only `read_file`. `bash`, MCP tools, write
tools and `ask` are never available in a consult turn.

- **Model calls bash during consult.** WHEN the model emits a `bash` call
  during a consult turn, THEN the call is refused without running, and `bash`
  was not among the offered tools.

### R6. consult refuses where the asker's profile is not trustworthy

consult refuses with an explanatory error on any platform where an agent's own
`profile.yaml` is not write-protected by the kernel.

- **Linux before P2.** WHEN consult runs on Linux and P2 has not landed, THEN
  it refuses, and the error names the platform limitation.

### R7. `Ask` events never cause a reused `seq` in mixed-version logs

The store computes the next `seq` from every non-empty line's `seq`, whether
or not that line's kind can be parsed.

- **Log ends in a kind this binary cannot parse.** WHEN the last line of
  `events.jsonl` has a `kind` unknown to this binary and `seq = 7`, THEN this
  binary's next append gets `seq = 8`.

### R8. `AskReply` is not agent progress

Readers that treat an agent-authored event as progress, as a reply, or as a
final answer exclude `Ask` and `AskReply`.

- **Fleet stuck detection.** WHEN the only new agent-authored events since the
  last iteration are `AskReply`, THEN the stuck clock is not reset.

## Risks / Trade-offs

- **The bypass stays open until the direct-socket gap is fixed.** `consult`
  controls a door that the direct-socket bypass lets an agent walk around. It
  is still needed so that, once the bypass is closed, the controlled path is
  not itself a new hole.
- **A consult turn with only `read_file` is a weaker answerer.** Intended: the
  answerer cannot run code on the asker's behalf.
- **Linux and Windows users cannot use `ask`** until P2 lands and Windows has
  been checked.
- **Older binaries log `unparseable event line(s) skipped`** for `Ask` lines,
  which looks like corruption. It disappears once versions converge.
