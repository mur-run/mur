# Agent read confinement under `~/.mur`: audit and options

**Date:** 2026-08-18
**Status:** audit complete, decision requested — nothing implemented
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

## 7. Not audited

Reads of the central stores (`skills/`, `channels/`, `index/`, `inbox/`,
`compress/`, `fleets/`) — #850 deliverable 1 asks for these too. The write side
is already granted selectively in `sandbox/policy.rs`; the read side is open and
was not surveyed here, because the private-key exposure is the part with an
integrity consequence and it changes what the deny plan can look like. That
survey remains open.
