# Commander Governance Hooks — Design (rev. 2, post adversarial review)

**Status:** design approved + hardened after a 33-agent adversarial spec review (2026-06-20)
that found the rev-1 design dead (no delivery), replay-defeatable, and structurally mis-timed.
This rev fixes all three. Ready for implementation plan.

**Goal:** Let a network operator (the **commander**) impose two governance levers on a running
fleet — an emergency **kill** and a **budget ceiling** — via an Ed25519-signed directive in the
fleet channel that the fleet loop honors each iteration and records in the hash-chained audit.
The commander **engine** is a separate closed-source crate; this spec is the open-source **hooks**
+ a local `mur commander` CLI that signs/delivers directives (v1; cross-network A2A delivery is
Phase 2).

**Architecture (one sentence):** a directive is a channel event whose payload carries a typed,
**monotonically-numbered** `commander_directive`, signed by the commander key with the event's
`idempotency_key = nonce`; `fold_governance` verifies each against the pinned commander pubkey(s),
applies directives in `issued_at_ms` order **rejecting any not strictly newer than the last applied
of its kind** (replay-resistant), and yields `GovernanceState { killed, budget_ceiling }` that the
loop consults at the top of every iteration (kill → halt; ceiling → `min` with the local budget),
emitting compliance to the audit.

**Tech stack:** Rust edition 2024. Reuse v3d channel signing (`mur-channel::sign::{sign_event,
verify_event_sig}`), `ChannelService::{open, load_events, append_signed}`, the hash-chained audit
(`mur-core::conversations::audit`), `AgentIdentity` (Ed25519), and the fleet loop safety triad.

## Global Constraints

- Brand: user-facing **MUR**; CLI/paths lowercase. No hardcoded magic values (named consts).
- A directive is **untrusted until its signature verifies against a pinned commander pubkey** AND
  it is **strictly newer** than the last-applied directive of its kind (signature-valid ≠ fresh).
- **Fail-safe bias = less autonomy/spend.** No pinned key → hooks inert (back-compat). Bad/stale/
  unverifiable directive → ignored. Channel **read error** → **fail-closed** (treat the fleet as
  killed for this iteration; do not run). `budget_ceiling == 0` → **honored** (halts via budget);
  negative/NaN → ignored.
- **Honest-node model (be precise — do not overclaim):** these hooks make a *cooperating* node
  honor commander governance, raising the bar with cryptography + a channel-recorded, audited
  decision that the local `mur fleet start`/CLI cannot clear. They are **not** a sandbox against a
  local user with filesystem/root control of their own machine: such a user can delete the pinned
  key, re-pin their own, truncate the channel file, or tamper the local audit. **Real enforcement
  against a hostile node lives in the commander engine's network side** (it can refuse to issue
  work, detect a missing/rotated pin or a broken audit chain on sync, and revoke). This spec
  delivers the honest-node hook.
- `mur-common` stays types-only (no I/O); verify/fold logic in `mur-channel`; all I/O in
  `mur-core`/`mur-daemon`. Source files ≤ 800 lines.

---

## 1. Context & reuse (verified against the code)

- **No commander identity in the open repo today** — only PID-liveness
  (`mur-common/src/schedule_claim.rs`). This spec adds a commander identity dir `~/.mur/commander/`:
  `identity.pub` (pinned verify key, always) and — on an *issuing* host — `identity.key` (sign key),
  via the existing `AgentIdentity` layout. Optional `identity.prev.pub` holds the previous key
  across a rotation (fold accepts current OR previous).
- **Channel signing** (`mur-channel/src/sign.rs`): `sign_event(identity, channel_id, actor, kind,
  payload, idempotency_key) -> String` (multibase) and `verify_event_sig(channel_id, actor, kind,
  payload, idempotency_key, sig_multibase, pubkey: &[u8;32]) -> bool`. The sign-input is
  `{v, channel_id, actor, kind, payload, idempotency_key}` — it **includes `channel_id`** (so a
  directive can't be replayed into another fleet's channel) and **`idempotency_key`** (so binding
  `idempotency_key = nonce` makes the nonce signature-bound), and **excludes `seq`/`ts`** (so seq
  order is NOT a trust signal — see §4 replay handling).
- **Channel store** (`mur-channel` store/service): `append_signed` → `append_event` assigns `seq`
  monotonically and **dedups by `ChannelEvent.idempotency_key` when present** (first-write-wins).
  `load_events(channel_id) -> Result<Vec<ChannelEvent>>` returns the **raw** persisted log (no
  per-actor verification). **Any local writer can append any actor/kind** (no append-time authz);
  this is exactly why §4 makes freshness a fold responsibility, not a store/seq guarantee.
- **Audit** (`mur-core::conversations::audit`): `Audit::open(root) -> Result<Self>` +
  `append(action: AuditAction, content_sha256: String) -> Result<AuditEntry>`; entry_hash binds
  both the canonical action and `content_sha256`. We add a `Governance` action.
- **Loop** (`cmd/fleet/loop_run.rs`): currently `load_events` is called for planning (mid-iteration)
  and convergence (end-of-iteration), NOT at the top. This spec adds an explicit top-of-iteration
  governance read (§5). `control.rs` is the local `.stopped` kill-switch.

## 2. Directive model

A directive is an ordinary `ChannelEvent` in `fleet-<name>` (no new `EventKind`/`ChannelActor` —
avoids breaking cross-version readers). Its `payload` carries:

```jsonc
{ "commander_directive": {
    "kind": "kill" | "resume" | "budget_ceiling",
    "fleet": "devteam",        // must equal the channel's fleet name
    "budget_usd": 5.0,         // present only for budget_ceiling; >=0 honored, <0/NaN ignored
    "nonce": "<uuid>",         // unique per directive; ALSO set as ChannelEvent.idempotency_key
    "issued_at_ms": 1750000000000  // commander wall-clock millis; MONOTONIC ordering key (§4)
} }
```

Required wiring (load-bearing — the rev-1 nonce was inert): the issuing CLI/engine sets
`ChannelEvent.idempotency_key = nonce` so the nonce is signature-bound (sign-input includes it) and
store-dedup drops a verbatim local replay. `kind`/`actor` of the event are irrelevant to trust;
the gate is (signature ✓ against a pinned key) **and** (strictly newer than last-applied, §4).
Const `COMMANDER_DIRECTIVE_KEY = "commander_directive"`.

## 3. Trust model

- **Pinned key absent** → no commander configured → hooks inert (personal/offline: zero impact).
- **Signature verifies against a pinned pubkey (current or previous) AND directive is strictly
  newer than the last applied of its kind** → applied. Otherwise ignored.
- Fail-safe bias toward less autonomy (see Global Constraints): forged/stale `resume` ignored (a
  real kill stays in effect); read errors fail-closed.
- Pinning is provisioning (engine enrollment / TOFU), not the trust decision. `mur commander pin`
  writes the pinned pubkey and **refuses to overwrite an existing pin without `--force`** (a silent
  re-pin would be a governance-replacement; re-enroll/rotation is deliberate and audited).

## 4. Governance fold (pure, replay-resistant)

```rust
// mur-common/src/commander.rs  (pure types)
pub const COMMANDER_DIRECTIVE_KEY: &str = "commander_directive";
pub struct CommanderDirective { pub kind: String, pub fleet: String,
    pub budget_usd: Option<f64>, pub nonce: String, pub issued_at_ms: u64 }
#[derive(Default)]
pub struct GovernanceState { pub killed: bool, pub budget_ceiling: Option<f64> }

// mur-channel/src/governance.rs  (needs ChannelEvent + verify_event_sig; pure, no I/O)
pub fn fold_governance(
    events: &[ChannelEvent], channel_id: &str, fleet_name: &str, accepted_pubkeys: &[[u8; 32]],
) -> GovernanceState
```

Algorithm — **the replay defense is: order by the SIGNED `issued_at_ms`, NOT by store `seq`** (a
replayed old directive keeps its old timestamp and sorts to its old position, so it cannot supersede
a newer one), plus nonce-dedup:
1. **Candidate filter:** keep each event whose `payload[COMMANDER_DIRECTIVE_KEY]` parses as a
   `CommanderDirective` with `fleet == fleet_name`, and whose `sig` verifies via
   `verify_event_sig(channel_id, &e.actor, e.kind, &e.payload, e.idempotency_key.as_deref(), sig, pk)`
   for **some** `pk in accepted_pubkeys`.
2. **Nonce-dedup:** keep only the first candidate per `nonce` (drops verbatim replays; idempotent).
3. **Order by signature, not by store:** sort candidates by `(issued_at_ms, nonce)` ascending.
4. **Last-wins per kind in that order:** `killed` = the last `kill`/`resume` candidate is `kill`;
   `budget_ceiling` = the `budget_usd` of the last `budget_ceiling` candidate, applied iff `>= 0`
   (negative/NaN → that candidate contributes no ceiling). No `seq`/log-position is consulted, so a
   replayed older `resume` (sorted before a newer `kill` by its own `issued_at_ms`) cannot clear the
   kill; store-level dedup-by-`idempotency_key` drops verbatim local replays as defense-in-depth.

`mur-core::cmd::commander::governance_state(mur_home, fleet) -> Result<GovernanceState>` (I/O
wrapper, for the daemon): load accepted pubkeys (current + optional previous); if none → `Ok(default)`
(inert); else `ChannelService::open(mur_home)?.load_events("fleet-<fleet>")` and `fold_governance`.
**A load error is propagated as `Err`** (caller decides fail-closed; see §5/§8).

## 5. Loop integration & un-overridable semantics

Governance state lives in commander-signed channel events, not a local sentinel, so the local
`mur fleet start` (which deletes `.stopped`) cannot affect it. A kill is cleared only by a strictly-
newer valid commander `resume`.

`cmd_fleet_run_loop` loads the accepted commander pubkeys **once** before the loop (`None` → governance
disabled, skip entirely — zero overhead/behavior change for ungoverned fleets). Then, at the **very
top of each iteration** (a NEW explicit read, before the existing checks):

1. `let events = svc.load_events(channel_id)` — on `Err` → **fail-closed**: `break
   LoopStop::CommanderKilled` (do not run an iteration we can't govern). On Ok, `let gov =
   fold_governance(&events, channel_id, fleet, &pubkeys)`.
2. If `gov.killed` → emit the audit entry (§7) → `break LoopStop::CommanderKilled` (highest priority).
3. `control::is_stopped` (local kill-switch) → `LoopStop::Stopped`.
4. `check_guards` (cap / deadline / stuck).
5. budget check with `effective_budget = min(local, gov.budget_ceiling)` (§6).
6. plan → execute → convergence.

New `LoopStop::CommanderKilled`. Latency: a kill is honored at the **next iteration boundary**
(cooperative, like the existing kill-switch) — documented, not mid-step.

## 6. Budget ceiling

`effective_budget`:
```
match (local, gov.budget_ceiling) {
    (Some(l), Some(c)) => Some(l.min(c)),   // tighten only
    (None,    Some(c)) => Some(c),          // commander imposes a budget even if user set none
    (l,       None)    => l,
}
```
**Important implementation note:** the existing `effective_budget`/`budget_exceeded` helpers guard
`b > 0.0` (so a local `budget_usd: 0` means "no budget" — unchanged). A commander ceiling of `0.0`
must therefore be **special-cased** so it is not silently discarded: in the loop, if
`gov.budget_ceiling == Some(0.0)` → `break LoopStop::Budget` immediately (before any spend) =
"spend nothing"; for `Some(c)` with `c > 0.0`, use `effective_budget = min(local, c)` through the
existing `budget_exceeded` against **real** cumulative spend (already shipped); `None` → unchanged.
The fold (§4) already drops negative/NaN ceilings, so `gov.budget_ceiling` is only ever `None` or
`Some(c >= 0.0)`. This special case keeps the local-budget semantics (`0 == unset`) untouched while
making a commander `0` ceiling a hard governance halt.

## 7. Audit emission

Add `AuditAction::Governance { fleet: String, directive: String, decision: String, nonce: String }`
(serde-tagged like its siblings). When the loop honors a directive, it emits **before** breaking:
`Audit::open(None)?.append(AuditAction::Governance { fleet, directive: "kill"|"budget_ceiling",
decision: "halted"|"capped", nonce }, content_sha256)` where `content_sha256` is the SHA-256 of the
directive's **reproducible sign-input** (`sign::sign_input(channel_id, actor, kind, payload, idem)`)
— NOT the stored row (which carries store-assigned seq/ts and isn't reproducible by the engine).
Append failure is logged, never blocks the halt (halting is the safety-critical act).

## 8. Daemon integration

`mur-daemon/src/fleet_tick.rs::due_fleets` must skip a fleet where `governance_state(mur_home,
fleet)?.killed` (don't auto-LAUNCH a killed fleet). A `governance_state` **`Err` → fail-closed**:
skip the fleet (do not launch). This is a launch gate; an already-running daemon loop honors a kill
at its next iteration boundary via §5 (the two together bound exposure to one iteration). Composes
with `MUR_FLEET_AUTORUN` + positive budget + local kill-switch (defense-in-depth).

## 9. CLI (`mur commander`)

```
mur commander pin <pubkey-multibase> [--force]    # write ~/.mur/commander/identity.pub (refuse overwrite w/o --force)
mur commander status                               # pinned? fingerprint? prev-key present?
mur commander directive <fleet> kill|resume|budget-ceiling [--budget-usd <X>]
                                                   # v1 delivery: sign with ~/.mur/commander/identity.key + append to fleet-<fleet>
```

`directive` is the v1 delivery path (local operator / same-host engine) **and** the headless test
entry point: it loads the commander `AgentIdentity` from `~/.mur/commander/`, builds the payload
(fresh `nonce` uuid, `issued_at_ms` = now), sets `ChannelEvent.idempotency_key = nonce`, signs via
`sign_event`, and `append_signed`s to the fleet channel. (Cross-network A2A delivery — a new
`channel/commander_directive` method that verifies an inbound envelope against the pinned key and
appends the commander-signed event verbatim — is Phase 2, §13.)

## 10. Components / files

| File | New/Mod | Responsibility |
|---|---|---|
| `mur-common/src/commander.rs` | New | `CommanderDirective`, `GovernanceState`, `COMMANDER_DIRECTIVE_KEY` — pure types. |
| `mur-common/src/lib.rs` | Mod | `pub mod commander;` |
| `mur-channel/src/governance.rs` + `lib.rs` | New/Mod | `fold_governance` (verify multi-key + monotonic + nonce-dedup; pure). |
| `mur-core/src/cmd/commander.rs` | New | accepted-pubkey load, `governance_state` wrapper, `pin`/`status`/`directive` commands. |
| `mur-core/src/cmd/fleet/loop_run.rs` | Mod | top-of-iteration governance read (fail-closed) + kill → `LoopStop::CommanderKilled` + `min` budget + audit emit. |
| `mur-daemon/src/fleet_tick.rs` | Mod | `due_fleets` skips killed (Err → fail-closed skip). |
| `mur-core/src/conversations/audit.rs` | Mod | `AuditAction::Governance`. |
| `cli/actions.rs`, `dispatch.rs`, `cmd/mod.rs` | Mod | wire `mur commander`. |

## 11. Error handling / fail-safe (summary)

No pinned key → inert. Bad/unverifiable/stale (not strictly newer) directive → ignored. Verbatim
replay → dropped by nonce dedup (+ store dedup). Channel **read error** → fail-closed (loop:
`CommanderKilled`; daemon: skip-launch). `budget_ceiling`: `>=0` honored (0 halts), `<0`/NaN ignored.
Directive whose `fleet` ≠ channel fleet → ignored. Audit append failure → logged, never blocks halt.
Key rotation: fold accepts current + previous pubkey; a deeper rotation requires the engine to
re-issue (re-sign, with a newer `issued_at_ms`) the active directives under the new key.

## 12. Testing (headless — no commander engine)

- **Pure fold (`mur-channel`):** build events signed by a test commander `AgentIdentity`. Assert:
  kill → `killed`; later (newer `issued_at_ms`) resume → `!killed`; **replay of the old resume
  (original issued_at_ms, same nonce) after a kill → kill STANDS** (monotonic + nonce-dedup); a
  directive signed by a DIFFERENT key → ignored; `budget_ceiling` (>=0) applied, `0` applied,
  `<0` ignored; wrong-`fleet` ignored; verify against `previous` pubkey works.
- **CLI:** `mur commander pin` refuses overwrite without `--force`; `directive` writes a verifiable
  signed event with `idempotency_key == nonce`; `status` reports the fingerprint.
- **Loop integration:** a commander kill in the channel halts one iteration with `CommanderKilled`
  + a `Governance` audit entry; a subsequent local `mur fleet start` does NOT clear it (still killed
  until a newer commander resume); commander ceiling < local → `effective_budget` is the ceiling;
  channel-read error → fail-closed halt.
- **Daemon:** `due_fleets` excludes a commander-killed fleet; `governance_state` Err → skip.
- **Negative:** unpinned → ignored; forged-sig → ignored; replayed older directive → ignored.

## 13. Out of scope / roadmap

- **Phase 2 — cross-network delivery:** A2A `channel/commander_directive` (node runtime verifies an
  inbound envelope against the pinned key and appends the commander-signed event verbatim, preserving
  the foreign signature — `ChannelStore::append_event` already accepts `sig: Option<String>`).
- Closed-crate engine: issuing directives at scale, the signed constitution + per-step policy
  evaluation, cross-network orchestration, the separate commander audit chain (bridged via the
  existing `AuditAction::Migrate`), network-side enforcement against hostile nodes.
- Hub/GUI surface; richer key-rotation (version chain beyond current+previous); `pause` (kill/resume
  already covers it).
