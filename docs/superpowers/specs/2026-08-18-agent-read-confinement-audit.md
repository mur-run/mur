# Agent read confinement under `~/.mur`: audit and options

**Date:** 2026-08-18
**Status:** audit complete; §8 implemented (read partition + Linux runtime reads)
**Issue:** #850

## 1. The gap, proven from the generated policy

`build_sbpl_profile` was run against a launch-chain policy for
`/Users/x/.mur/agents/alice`. Every line it emits touching the agents tree:

```
(deny  file-write* (subpath "/Users/x/.mur/agents"))
(allow file-write* (subpath "/Users/x/.mur/agents/alice"))
(deny  file-read*  (subpath "/Users/x/.mur/agents/alice/profile.yaml"))
(deny  file-write* (subpath "/Users/x/.mur/agents/alice/profile.yaml"))
(deny  file-read*  (subpath "/Users/x/.mur/agents/alice/identity.key"))
(deny  file-write* (subpath "/Users/x/.mur/agents/alice/identity.key"))
```

There is **no `deny file-read*` on the agents tree**. The launch chain
(spec 2026-08-11) denies writes there and re-allows the agent's own home; the
read denies are `SELF_PROTECTED_AGENT_FILES`, and they apply to
`agent_self_home()` only.

So: **alice cannot read her own `identity.key`, and can read bob's.**

That key is bob's Ed25519 signing key. With v3d signed channel events and
`channel/delegate`, holding it means forging events attributed to bob — the
confidentiality gap is also an integrity gap.

## 2. Why the obvious fix is wrong

#850's deliverable 2 proposes denying "everything not on the list … most
importantly other agents' homes".

Verifying a peer's signed events reads, from **that peer's** home:

| file | read by | why |
|---|---|---|
| `identity.pub` | `mur_channel::sign::resolve_writer_pubkey` (`sign.rs:135`) | the verification key |
| `rotations.jsonl` | same, `sign.rs:109` | the rotation chain, so a rotated key still verifies |

`mur-core::channel_verify::actor_pubkey` resolves `Agent{id}` →
`<mur_home>/agents/<id>` and calls into it. Verify-on-fold is **per-actor**
(v3d-2), so every multi-agent channel does this on every fold.

A blanket read-deny on `agents/` therefore breaks signature verification for
every fleet, delegation and shared channel — silently, fail-closed, which is
precisely the "a wrong deny bricks an agent" risk #850 names. Those two files
are public by construction; they are the published verification material.

**What actually needs denying is `identity.key` (and arguably `profile.yaml`)
in *every* agent home, not the whole tree.**

## 3. Why that is not a one-line change

Neither backend can express "a file with this name under any agent home":

- **SBPL** (macOS) — this codebase only emits `(subpath "literal")`. SBPL has
  `(regex …)` but nothing here uses it, and introducing it to the deny path
  means the deny surface is written in a second language.
- **Landlock** (Linux) — path-fd based. There is no pattern form; rules are
  installed against paths that exist when the ruleset is built.

Both would have to **enumerate** `<mur_home>/agents/*/identity.key` at seal
time. That leaves agents created *after* the seal readable — the same weakness
`LaunchChain::deny_paths`'s own comment calls out for the write side ("names not
created yet — the regression a path list cannot catch"). The write side escapes
it by denying the whole subtree. The read side cannot, per §2.

## 4. Options

**(a) Enumerate at seal time, accept the after-seal gap.**
Smallest change, closes the case that matters today (existing siblings). An
agent created while another is running stays exposed to it until that one
restarts. Portable, no new policy language, no migration.

**(b) SBPL regex on macOS, enumeration on Linux.**
Closes the gap on macOS only, and makes the two backends' deny surfaces
structurally different — which this repo has been bitten by before (one
mechanism, two backends, and the status text tells a different story per
platform).

**(c) Move private keys out of the agents tree.**
`~/.mur/keys/<agent>/identity.key`, with `agents/<name>/` keeping only public
material. Then a blanket `deny file-read* (subpath "<mur_home>/keys")` plus a
re-allow of the agent's own key expresses the whole rule as subtrees, on both
backends, with no enumeration and no after-seal gap.

Structurally right, and the only option that closes the gap completely. Costs a
data migration and touches every key read/write path
(`identity.rs`, rekey, export/import, `.muragent` bundles).

## 5. Recommendation

**(a) now, (c) as the real fix.**

(a) is small, portable, and removes the live exposure between agents that
already exist — which is every deployed fleet today. It should ship with the
after-seal gap written into the code, not just here, so it is not mistaken for
completeness.

(c) is what makes the rule expressible rather than enumerable. It is a
migration and wants its own spec.

(b) should not be built: it buys one platform at the cost of a permanently
divergent deny surface.

## 6. Regression guard (deliverable 3)

Whatever lands, the guard is a test over `build_sbpl_profile`'s output
asserting a sibling's `identity.key` is read-denied. It fails today, which is
the point — it is written against the desired state, so it cannot be committed
before the fix. Landing it in the same PR as (a) is the natural sequencing.

A second guard is worth having regardless: assert the SBPL contains **no**
`deny file-read*` covering `identity.pub` or `rotations.jsonl` in any agent
home, so a future tightening cannot break verification without a red test.

## 7. Central-store reads: the runtime's actual read surface

The sandboxed process is `mur-agent-runtime`; spawned children (`mur`, `bash`,
MCP servers) inherit the same profile, so the *runtime's* reads are the whole
confinement surface worth enumerating. Surveyed from code, not by guessing:
every `~/.mur` path the runtime reads is a named `join(...)` site, so the list
below is the complete one for the process the profile actually seals.

### 7.1 What the runtime reads — and must be able to

| path | read by | why | gated by |
|---|---|---|---|
| `config.yaml` | `supervisor_runner.rs:417` (model-switch), `:694` (skills), `tools/fleet_run.rs:56,124`, `tools/remember.rs:45` | global config is load-bearing | always |
| `compress.yaml` + `compress/` | `hooks/builder.rs:38` | auto-compress hook (Surface 2) | `compress.yaml auto.enabled && auto.agent_runtime` |
| `fleets/<name>/fleet.yaml` | `protocol/methods/channel_delegate.rs:31,48`, `tools/fleet_run.rs:137` | fleet delegation | fleet-enabled agents only |
| `models.yaml` + `cache/` | `tools/fleet_run.rs:311-314` | spawned `mur fleet run` child | `fleet_run.agents` allowlist |

The runtime **writes** (not reads) three more: `inbox/` (snapshot outbox,
`supervisor.rs:271`), `channels/` (peer-writes-own self-reply,
`policy.rs:226`), `index/channels/` (channels.db refresh, `policy.rs:278`).
All three are already granted on the write side.

The runtime does **not** read, at any site: `skills/`, `index/` (the `*.lance`
stores and `capabilities.json`), `inbox/`, `models.yaml` (outside the fleet_run
carve-in), `secrets/`, `queue/`, `session/`, `telemetry/`, `traces/`,
`conversations/`, `commander/`. Skills injection reaches the runtime through
hooks/CLI, not a direct read.

### 7.2 The per-backend posture

- **macOS (SBPL, allow-default).** None of the central stores above is in the
  read-deny list — the `ordinary stores` gate test
  (`launch_chain.rs:468-493`) asserts `skills/`, `channels/`, `workflows/`,
  `commander/` pass through unrefused. So every agent can directly read
  `skills/`, `index/`, `inbox/`, `config.yaml`, `models.yaml` today. That is
  exactly the gap #850 deliverable 1 names; it is real and it is open.
- **Linux (Landlock, default-deny).** The read side is the *opposite* problem
  and it is live right now: `fs_read` is populated only from user-declared
  entitlements, the `fleet_run` carve-in, and `system_read_paths()`
  (`policy.rs:765`) — **`config.yaml`, `compress.yaml` and `fleets/` are not in
  it**. Landlock denies the runtime's own config reads, so
  `Config::load_or_default` silently falls back to defaults (wrong models),
  the compress hook never engages, and `channel/delegate` cannot read a fleet
  definition. Linux agents already run degraded; nobody noticed because the
  failure is silent.

### 7.3 The read-side partition gap

`partition_grants` (`launch_chain.rs`) is applied to **write grants only**
(`linux.rs partition_write_grants`); `fs_read` is never partitioned. So on
Linux a user-declared `fs_read` that contains a protected path — e.g.
`fs_read: [~/.mur]` or `~/.mur/secrets` — grants the read outright, because
Landlock has no deny-within-allow. The write side drops such a grant
fail-closed; the read side cannot, and does not. Same divergence #975 already
warned about ("one mechanism, two backends, status tells a different story").

## 8. Decision: partition `fs_read` like `fs_write`, and grant the runtime's own reads

§7 turns the open "should `fs_read` be partitioned like `fs_write`?" question
into two concrete changes that share one mechanism (the read allow-list on
Linux is the enforcement surface; the deny list is the macOS one):

**(1) Partition `fs_read` against the launch chain's protected paths.**
Apply the same `partition_grants` the write side uses to `fs_read`, on both
backends. On Linux this is the only thing that stops a broad read grant from
leaking `secrets/`, `queue/`, or a sibling's `identity.key` — Landlock cannot
carve. On macOS it is a no-op for enforcement (allow-default) but makes the
two backends agree about what a grant may name, and feeds the same
`dropped_grants` doctor report the write side already produces.

**(2) Grant the runtime's own central-store reads on Linux.**
Add `config.yaml`, `compress.yaml`, `fleets/`, and (when `fleet_run` is
allowlisted) `models.yaml` to `fs_read` for every agent. Without this, Linux
agents silently run with default config (§7.2). These are the runtime's own
load-bearing reads, so granting them is not a widening of the trust boundary —
it is making the allow-list match what the code already does.

Both are fail-closed in the same direction: miss a path in (2) and an agent
degrades silently; drop a grant in (1) and the agent loses a read it declared.
Either mistake needs the canary rollout #850 already requires.

**Why the two belong together:** (1) is only safe to populate *because* (2)
names the complete legitimate read set — once `config.yaml` etc. are on the
allow-list, the protected-path partition can drop everything else without
starving the runtime. Shipping (2) without (1) would grant reads of
`secrets/`-adjacent paths on the way to `config.yaml`; shipping (1) without (2)
breaks Linux. They are one PR, verified with a canary agent first, then
per-archetype.

**macOS read-deny extension** (`skills/`, `index/`, `inbox/`) stays **out** of
this PR. The runtime does not read them, but spawned `mur` children might
(`mur skill show`, `mur search`), and the canary is the place to learn which
are load-bearing before denying. The issue's "no new code paths that depend on
direct central-store reads" rule argues for denying them eventually; the
canary decides when.

## 9. What shipped, and what it was verified against

Both halves of §8, in one change:

- `LaunchChain::partition_read_grants` — the read counterpart of
  `partition_grants`, against the credential store plus sibling signing keys
  (**not** `deny_paths()`, per §2). Applied to the **user-declared** read
  entitlements only, at profile-build time.
- The runtime's own reads (`config.yaml`, `compress.yaml`, `compress/`,
  `fleets/`) added to `fs_read`, existence-checked.
- `SandboxPolicy::dropped_read_grants` + `mur agent doctor`'s `grant_scope`
  check extended to report them.

### Why only the user-declared grants are partitioned

The first cut partitioned the finished `fs_read` and silently dropped
`/private/tmp` — a `system_read_paths()` entry — because the test's `mur_home`
nested under it. A relocated MUR home would do the same to a real agent. Only a
human-declared grant may be dropped, because only a human can fix it; the
builder's own additions are not up for debate. Guarded by
`the_builders_own_read_paths_survive_an_overbroad_user_grant`.

That flaw was found by dumping a real profile, not by review.

### Verified with `sandbox-exec`, against a generated profile

Profile built from a policy carrying an overbroad `fs_read: [<mur_home>]`:

```
profile compiles:      sandbox-exec -f <profile> /usr/bin/true   -> exit 0
secrets/anthropic.key  DENIED      (Operation not permitted)
agents/pm/identity.key DENIED      (Operation not permitted)
agents/pm/identity.pub READABLE    <- §2 constraint holds
skills/x.yaml          READABLE
dropped_read_grants    ["<mur_home>"]   (and nothing else)
```

Negative control: delete the one `(deny file-read* … /secrets)` line from that
same profile and the key reads back as `SUPER-SECRET-KEY`.

**What this proves about macOS:** `skills/x.yaml` is readable even though the
partition removed its covering allow — because the SBPL baseline is
`(allow default)`. So dropping a read grant is a **no-op on macOS** and the
explicit denies are what confine reads there. The change is therefore
load-bearing on Linux and behaviour-preserving on macOS, which is why it needs
no macOS canary.

### Still open after this

1. **Linux cannot read peer public key material.** `<mur_home>/agents/` is in
   no Linux read grant, so a spawned `mur` verifying a peer's signed events
   would fail-close. macOS handles it (`macos.rs` re-allows `identity.pub` and
   `rotations.jsonl`); Landlock has no equivalent grant. Not fixed here: it
   needs enumeration (with the same after-seal gap as `sibling_signing_keys`),
   and in practice it requires `spawn(mur)`, which #850 withdrew. Worth a
   canary before building.
2. **macOS read-deny on the central stores** (`skills/`, `index/`, `inbox/`)
   — deliberately out of scope, per §8.
3. **Option (c)**, moving private keys out of the agents tree — still the only
   variant that closes the after-seal enumeration gap.
