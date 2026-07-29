# murmur `--fleet`: a status rail for fleet work

**Date:** 2026-07-29
**Status:** design approved, not implemented

## Problem

`murmur` chats with one agent. A fleet is several agents working a shared goal,
and today nothing about that work is visible from the chat: you cannot see how
far the work has got, who is doing what, or — the one that costs real time —
which member is **blocked waiting for an approval you never saw**.

The request that started this: *"chat with mur and see all jobs of channels of
members of fleet."*

## What already exists

Three findings shaped the design; each removed work rather than adding it.

1. **Live-tailing another channel already ships.** `/channels <id> --follow`
   (`mur-core/src/cmd/agent/cli/follow.rs`) tails any channel into the
   transcript while you keep chatting — 700 ms poll, gated on the log actually
   growing.
2. **A fleet has exactly one channel.** `fleet-<name>`. Members write their own
   signed replies into it via `channel/delegate` (v3d-2), and `mur fleet jobs`
   already treats that channel as the truth for reconciling job status
   (`cmd/fleet/jobs.rs:132`). Seeing every member means following **one**
   channel, not N.
3. **The vocabulary exists.** `EventKind` (Message / ToolCall / StateChange /
   HitlRequest / HitlResponse / …), `ChannelActor::{Human, Agent, System}`,
   `ChannelState` (Working / InputRequired / Completed / Failed / …) in
   `mur-common/src/channel.rs`; `JobStatus` (Queued / Running / Done / Failed /
   Canceled) in `mur-common/src/fleet.rs`; `list_jobs()` in
   `cmd/fleet/jobs.rs:163`.

## Why not two modes

The obvious framing was "merged into the chat stream" vs "a separate pane", with
a flag to pick. Rejected, for two reasons.

**The merged mode already exists.** `/channels fleet-develop --follow` is
exactly that. Shipping `--fleet` as an alias for it would add a flag and no
capability.

**A raw event stream does not answer the question.** Research into how 2026
agent tooling handles this converges on one point: an interleaved feed scales to
about one agent, and past that the operator's questions are *who is blocked*,
*what is running*, *how far along is the work* — which a chronological stream
structurally cannot answer. Claude Code has an open request for precisely this
([anthropics/claude-code#24537](https://github.com/anthropics/claude-code/issues/24537):
interleaved tool summaries hide which agent is stuck, and approvals get buried
in scroll). Agent-aware multiplexers
([Herdr](https://betterstack.com/community/guides/ai/herdr-ai-agent/),
[amux](https://amux.io/guides/best-ai-agent-multiplexers-2026/)) answer it with
a status rail that rolls each agent up to working / idle / blocked, with the
blocked ones floated to the top. The design problem is **attention routing**,
not display.

So `--fleet` builds the thing that does not exist: a fold of the channel into
per-member status, not another rendering of the stream.

## Design

### 1. Surface

```
murmur --fleet develop            # ≡ mur agent cli mur --fleet develop
murmur --fleet develop qa         # chat with a different agent; default is mur
```

`--fleet <name>` resolves to channel id `fleet-<name>`. An unknown fleet is a
hard error at startup — silently degrading to a plain murmur would leave the
user believing they are watching a fleet when they are not.

The rail is a layout band between the transcript and the composer (same slot and
mechanism as the existing chooser band), with a dynamic height.

**Collapsed — always present.** Job progress, the slow variable, from the job
store. `2/5` is jobs in a terminal state (done + failed + canceled) out of the
fleet's total:

```
 fleet · develop   job 2/5 ⏵ running · 1 ✖ failed
```

**Expanded — only when someone is blocked.** Every member that has events,
blocked first, then working, then finished; state folded from channel events:

```
 fleet · develop   ▲ 1 blocked
   backend    ▲ blocked: approve `cargo publish`
   rustsmith  ⏵ working (2m)
```

Expanding on *blocked* rather than on any activity is the point: a working fleet
is not news, a stalled one is. One line at rest costs almost nothing against the
fixed viewport; the band grows only when something needs the user. That split
matches the order the questions are actually asked: *how far along is the work*
first, *who is stuck* only when something stalls.

**Empty state:** fleet exists but has never run →
`fleet · develop   not run yet (mur fleet run develop)`.

### 2. Member state derivation

Existing enums only; no new vocabulary.

| channel event | member state |
|---|---|
| `StateChange → Working`, or `StateChange → "submitted"` | ⏵ working |
| `StateChange → InputRequired`, or a `HitlRequest` with no matching `HitlResponse` | ▲ **blocked** (with the request summary) |
| `StateChange → Completed` | ✔ done |
| `StateChange → Failed` / `Canceled` / `Rejected` | ✖ failed |
| latest `ToolCall` | the "what it is doing" detail on a working row |
| no events at all for that member | not shown — silence is not idleness, and inventing a state is a lie |

Members are `ChannelActor::Agent { id }`. `Human` and `System` events stay out of
the rail: those are the user's own turns and the executor's bookkeeping.

### 3. Components and data flow

**New type `FleetRail`** (`cmd/agent/cli/fleet_rail.rs`), living alongside
`Follow` rather than extending it. Different purposes deserve different types:
`Follow` turns events into transcript lines (history, reaches scrollback),
`FleetRail` folds events into current state (not history, repainted every frame,
never flushed). Keeping them separate also leaves `app.follow` free, so
`/channels <id> --follow` still works while a rail is up.

```rust
pub struct FleetRail {
    fleet: String,
    channel_id: String,      // fleet-<name>
    last_len: u64,           // channel log size gate (same trick as Follow)
    jobs_mtime: SystemTime,  // job store gate
    view: RailView,          // last computed result, reused when nothing moved
    next_poll: Instant,
}
```

**Poll** every 700 ms (`follow::POLL_INTERVAL`). Compare the channel log length
and the job file's mtime first; if neither moved, return. At rest a tick is two
`metadata()` calls and no parsing.

**Load once, use twice.** `list_jobs()` internally reloads the whole channel to
reconcile (`jobs.rs:140`), so calling it from the rail's tick would parse the
same log twice a second. The rail loads events once and feeds both the member
fold and the job reconciliation itself.

**Render.** `ui.rs` gains `fleet_rail_height(app)` and
`render_fleet_rail(...)`, mirroring the chooser band.

**The one place this touches existing logic:** `band_inner_rows()` — added in
#818 to decide when transcript content is flushed to scrollback — subtracts the
composer, status line, chooser band and borders. **It must subtract the rail
too.** If it does not, the flush decision drifts from what is actually painted,
and the symptom is the band losing a row or flushing one message too early.

### 4. Failure modes

The rail must never take the chat down with it.

| situation | behavior |
|---|---|
| channel unreadable or missing | collapsed line reads `fleet · develop  ⚠ channel unreadable`; chat continues. No rail error propagates to the event loop |
| signature verification | uses the existing verifying read path (v3d per-actor); never bypasses it. The rail vouches for *other* agents — rendering "qa ✔ done" from an unverified event would lend the UI's credibility to a forgery. `MUR_CHANNEL_REQUIRE_SIG` policy is inherited, not reimplemented |
| member stuck in working because its runtime died | show elapsed time (`⏵ working (2h14m)`) rather than inventing a timeout. Don't guess; let the clock say it |
| more members than fit | the expanded list is capped at K=6 rows plus `… N more`; the sort (blocked → working → finished) guarantees whatever is truncated is the least urgent |
| `jobs.yaml` corrupt | collapsed line falls back to the channel-derived summary; no crash |

### 5. Testing

All pure functions; no test needs a live fleet.

1. **Fold table test** — one case per row of §2: `StateChange→Working`;
   `HitlRequest` with no response → blocked; `HitlRequest` **followed by**
   `HitlResponse` → no longer blocked; `Failed`/`Canceled`/`Rejected` collapse to
   ✖; `Human`/`System` events ignored; empty channel → "not run yet".
2. **Height function** — 0 blocked → 1 row; N blocked → `1 + min(N, K)`.
3. **Consistency guard with #818** — assert
   `band_inner_rows() + rail height + composer + status + borders == viewport
   height`. This test exists for exactly one reason: to fail when someone
   changes the rail's height and forgets the flush capacity.

## Deliberately not in v1

- **Approving a blocked member from the rail.** The full attention-routing
  payoff, and the obvious step two — but it needs keyboard focus and the
  approval path wired in. Ship read-only first: once it is up, the rail itself
  reports how often blocking actually happens, and that number decides whether
  step two is worth building.
- **Multiple fleets at once.** One `--fleet` per session.
- **A pane per member.** The fleet has one channel; per-member panes would be
  N tails of the same log.
