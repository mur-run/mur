# Commander Governance Hooks — Design

**Status:** design approved (brainstorming 2026-06-20); ready for implementation plan.

**Goal:** Let a network operator (the **commander**) impose two un-overridable governance
levers on a running fleet — an emergency **kill** and a **budget ceiling** — via a
cryptographically-signed directive posted to the fleet channel, which the fleet loop honors
each iteration and records in the tamper-evident audit chain. The commander **engine** is a
separate closed-source crate; this spec is ONLY the open-source **hooks** the loop/daemon
must honor, plus a minimal pin/test path.

**Architecture (one sentence):** a commander directive is a channel event carrying a typed
`commander_directive` payload, Ed25519-signed by a pinned commander key
(`~/.mur/commander/identity.pub`); a pure `fold_governance` folds the channel's directives
(verifying each against the pinned key) into a `GovernanceState { killed, budget_ceiling }`
that the fleet loop and the daemon's auto-run both consult — kill halts (un-overridable by the
local kill-switch), and the ceiling tightens the budget via `min`.

**Tech stack:** Rust edition 2024. Reuse: v3d channel signing/verify (`mur-channel::sign`),
the hash-chained audit (`mur-core::conversations::audit`), the fleet loop safety triad
(`cmd/fleet/{loop_run,control}.rs`, `mur-daemon/fleet_tick.rs`), real per-token budget
accounting (already shipped), and `AgentIdentity` (Ed25519) for the pinned key + test signing.

## Global Constraints

- Brand: user-facing text uppercase **MUR**; CLI/`name`/paths lowercase.
- No hardcoded magic values: named constants (payload marker key, pinned-key path).
- A commander directive is **untrusted until verified** against the pinned key — never act on
  an unsigned/unverifiable directive.
- **Fail-safe bias = stay halted / stay bounded:** no pinned key → hooks inert (back-compat);
  bad/absent signature → ignore the directive; can't read the channel → honor last-known state.
- **Un-overridable:** a valid commander kill cannot be cleared by the local `mur fleet start`;
  only a later valid commander `resume` clears it. The budget ceiling can only tighten, never
  loosen, the local budget.
- The commander **engine** (issuing directives, the constitution, cross-network orchestration)
  is out of scope — closed crate. This spec implements only the in-repo hooks.
- `mur-common` stays types-only (no I/O); signing/verify logic lives in `mur-channel`; all
  filesystem I/O in `mur-core`/`mur-daemon`.
- Source files ≤ 800 lines.

---

## 1. Context & reuse

Confirmed by exploration of the current repo:

- **No commander identity exists in the open repo** — only PID-liveness
  (`mur-common/src/schedule_claim.rs`: `commander_pid_path()` → `~/.mur/commander/commander.pid`,
  `is_commander_running()`). This spec adds a **pinned commander public key** at
  `~/.mur/commander/identity.pub` (multibase, same format as an agent's `identity.pub`).
- **Channel events are per-actor Ed25519 signed** — `mur-channel::sign::verify_event_sig(channel_id,
  actor, kind, payload, idempotency_key, sig_multibase, pubkey: &[u8;32]) -> bool`. The governance
  fold reuses this to verify a directive against the pinned commander key directly, over the **raw**
  event log (`ChannelService::load_events`), independent of the channel's per-actor verify pass — so
  `MUR_CHANNEL_REQUIRE_SIG` cannot drop a commander directive before the fold sees it.
- **Audit chain is wired** — `mur-core::conversations::audit::Audit::open(root_override)?` +
  `.append(action: AuditAction, content_sha256: String) -> Result<AuditEntry>`; hash =
  `SHA256(prev_hash \n canonical_json(action) \n content_sha256)`. We add a `Governance` action.
- **Loop hook point** — `cmd/fleet/loop_run.rs` checks `control::is_stopped` at the top of each
  iteration (the local kill-switch `.stopped` sentinel). The commander-kill check slots in at a
  HIGHER priority, just above it.

## 2. Directive model

A commander directive is an ordinary `ChannelEvent` (no new `EventKind` or `ChannelActor` — avoids
breaking cross-version readers like the mobile SDK) whose `payload` carries a typed marker:

```jsonc
{ "commander_directive": {
    "kind": "kill" | "resume" | "budget_ceiling",
    "fleet": "devteam",            // must equal the channel's fleet name
    "budget_usd": 5.0,             // present only for budget_ceiling
    "nonce": "<uuid>",             // engine-side idempotency; fold orders by event seq
    "issued_at": "2026-..Z"        // informational
} }
```

`CommanderDirective` is a pure serde type in `mur-common`. The marker key
`COMMANDER_DIRECTIVE_KEY = "commander_directive"` is a named const. The directive is signed (over
the canonical sign-input `{v, channel_id, actor, kind, payload, idempotency_key}`, excluding seq/ts)
by the commander key when the engine posts it; in tests we sign with a test `AgentIdentity` whose
pubkey is the pinned key. The authoring `ChannelActor`/`EventKind` are irrelevant to trust — the
**only** gate is the signature verifying against the pinned commander pubkey.

## 3. Trust model

- **Pinned key absent** (`~/.mur/commander/identity.pub` missing) → no commander configured → the
  hooks are inert; fleets behave exactly as today. (Personal/offline users: zero impact.)
- **Directive present, signature verifies against the pinned key** → honored.
- **Directive present, signature invalid / unverifiable / wrong key** → ignored. Fail-safe bias:
  a forged `resume` is ignored (a real kill stays in effect → un-overridable holds); a forged
  `kill` cannot exist (no valid signature), and the local kill-switch still serves the local user.
- Pinning is provisioning, not part of the trust decision: the operator/engine writes the pinned
  key (org enrollment / TOFU). v1 ships a minimal `mur commander pin <pubkey>` to write it (manual
  enroll + headless tests); the hooks only ever **read** it.

## 4. Governance fold (pure)

```rust
// mur-common/src/commander.rs  (pure types only)
pub const COMMANDER_DIRECTIVE_KEY: &str = "commander_directive";
pub struct CommanderDirective { pub kind: String, pub fleet: String,
    pub budget_usd: Option<f64>, pub nonce: String, pub issued_at: String }
#[derive(Default)]
pub struct GovernanceState { pub killed: bool, pub budget_ceiling: Option<f64> }

// mur-channel/src/governance.rs  (needs ChannelEvent + verify_event_sig; pure, no I/O)
pub fn fold_governance(
    events: &[ChannelEvent], channel_id: &str, fleet_name: &str, pinned_pubkey: &[u8; 32],
) -> GovernanceState
```

`fold_governance` iterates events in `seq` order; for each event whose `payload[COMMANDER_DIRECTIVE_KEY]`
parses as a `CommanderDirective` with `fleet == fleet_name` AND whose `sig` verifies via
`verify_event_sig(channel_id, &e.actor, e.kind, &e.payload, e.idempotency_key.as_deref(), sig, pinned_pubkey)`,
it applies: `kill` → `killed = true`; `resume` → `killed = false`; `budget_ceiling` → `budget_ceiling = budget_usd`.
Latest-wins falls out of seq order (last valid kill/resume wins; last valid `budget_ceiling` wins). Events
that don't parse, don't match the fleet, or don't verify are skipped (fail-safe).

`mur-core::cmd::commander::governance_state(mur_home, fleet) -> GovernanceState` is the I/O wrapper:
load the pinned pubkey (`~/.mur/commander/identity.pub`); if absent return `GovernanceState::default()`
(inert); else `ChannelService::open(mur_home).load_events("fleet-<fleet>")` and call `fold_governance`.

## 5. Un-overridable semantics

Governance state lives in commander-signed channel events, NOT a local sentinel — so the local
`mur fleet start` (which deletes `.stopped`) cannot affect it.

- **commander-kill active** ⟺ the latest valid commander `kill`/`resume` directive is `kill`.
  Cleared ONLY by a later valid commander `resume`.
- The fleet loop, at the **top of each iteration**, orders checks:
  1. governance fold → if `killed` → `break LoopStop::CommanderKilled` (highest).
  2. `control::is_stopped` (local kill-switch) → `LoopStop::Stopped`.
  3. `check_guards` (cap / deadline / stuck).
  4. budget check with the effective ceiling (§6).
  5. plan → execute.

  The loop already reloads the channel events each iteration (for convergence + stuck
  detection), so it folds over **those** events — `fold_governance(&events, channel_id, fleet,
  &pinned)` — with the pinned pubkey loaded **once** before the loop (None → inert, skip the
  check entirely). It does NOT re-read the channel. The daemon (which holds no loaded events)
  uses the `governance_state(mur_home, fleet)` wrapper instead (§8).

## 6. Budget ceiling

`effective_budget` becomes the **min** of the locally-resolved budget (CLI flag > `fleet.yaml`
`loop.budget_usd` > none) and the commander ceiling (when present):

```
effective = match (local, commander_ceiling) {
    (Some(l), Some(c)) => Some(l.min(c)),
    (None,    Some(c)) => Some(c),     // commander imposes a budget even if the user set none
    (l,       None)    => l,
}
```

Enforced by the existing `budget_exceeded` guard against **real** cumulative spend (already shipped).
A commander ceiling thus stops the loop early via `LoopStop::Budget` once real spend approaches the
ceiling, regardless of the local budget.

## 7. Audit emission

Add to `mur-core::conversations::audit::AuditAction`:

```rust
Governance { fleet: String, directive: String, decision: String, nonce: String },
```

When the loop honors a commander directive (halts on `CommanderKilled`, or stops on a commander
ceiling), it emits `Audit::open(None)?.append(AuditAction::Governance { fleet, directive: "kill"|"budget_ceiling",
decision: "halted"|"capped", nonce }, content_sha256)` where `content_sha256` is the SHA-256 of the
directive event (binding the audit entry to the exact signed directive). This records compliance in
the tamper-evident chain. Audit append failures are logged but never block the halt (halting is the
safety-critical action; the audit is the record of it).

## 8. Daemon integration

`mur-daemon/src/fleet_tick.rs::due_fleets` must skip a fleet whose `governance_state(mur_home, fleet).killed`
is true — the daemon must not auto-run a commander-killed fleet. This composes with the existing
auto-run gates (`MUR_FLEET_AUTORUN` switch + positive budget + local kill-switch) as defense-in-depth.

## 9. Provisioning CLI (minimal)

```
mur commander pin <pubkey-multibase>   # write ~/.mur/commander/identity.pub
mur commander status                    # show whether a key is pinned + its fingerprint
```

`mur-core::cmd::commander` hosts `cmd_commander_pin`/`cmd_commander_status` +
`load_pinned_commander_pubkey(mur_home) -> Option<[u8; 32]>` + `governance_state(...)`. Wired in
`cli/actions.rs` (a `Commander` subcommand) + `dispatch.rs`.

## 10. Components / files

| File | New/Mod | Responsibility |
|---|---|---|
| `mur-common/src/commander.rs` | New | `CommanderDirective`, `GovernanceState`, `COMMANDER_DIRECTIVE_KEY` — pure types. |
| `mur-common/src/lib.rs` | Mod | `pub mod commander;` |
| `mur-channel/src/governance.rs` | New | `fold_governance(events, channel_id, fleet, pinned_pubkey)` (verify + fold; pure). |
| `mur-channel/src/lib.rs` | Mod | `pub mod governance;` |
| `mur-core/src/cmd/commander.rs` | New | pinned-key load + `governance_state` wrapper + `pin`/`status` commands. |
| `mur-core/src/cmd/fleet/loop_run.rs` | Mod | governance check (kill + min-budget) + `LoopStop::CommanderKilled` + audit emit. |
| `mur-daemon/src/fleet_tick.rs` | Mod | `due_fleets` skips commander-killed fleets. |
| `mur-core/src/conversations/audit.rs` | Mod | `AuditAction::Governance` variant. |
| `mur-core/src/cli/actions.rs`, `dispatch.rs`, `cmd/mod.rs` | Mod | wire `mur commander`. |

## 11. Error handling / fail-safe (summary)

- No pinned key → inert. Bad/unverifiable directive → ignored. Unreadable channel → treat as no
  new directive (honor last-known; default = not killed, no ceiling). Audit append failure → log,
  never block a halt. A `budget_ceiling` with a non-positive value → ignored (a 0 ceiling would
  halt everything; treat as unset, fail-safe toward running-but-bounded-by-local). A directive whose
  `fleet` field ≠ the channel's fleet → ignored.

## 12. Testing (headless — no commander engine)

- **Pure fold (`mur-channel`):** generate a test `AgentIdentity` as the commander; sign a `kill`
  directive event into a synthetic event list → `fold_governance` returns `killed=true`; a later
  `resume` → `killed=false`; a directive signed by a DIFFERENT key → ignored (`killed=false`); a
  `budget_ceiling` → `budget_ceiling=Some(x)`; latest-wins ordering; wrong-`fleet` directive ignored.
- **Pinned-key load + wrapper (`mur-core`):** no pinned key → `GovernanceState::default()`; pinned
  key + a signed kill in the channel → `killed=true`.
- **Loop integration:** with a commander-kill in the fleet channel, one loop iteration halts with
  `CommanderKilled`, appends a `Governance` audit entry, and a subsequent local `mur fleet start`
  does NOT clear it (still killed). Commander ceiling < local budget → `effective_budget` is the
  ceiling.
- **Daemon:** `due_fleets` excludes a commander-killed fleet.
- **Negative:** unpinned → directive ignored; forged-signature directive ignored.

## 13. Out of scope / roadmap

The commander **engine** (issuing directives, evaluating a signed constitution, cross-network
orchestration, the separate commander audit chain bridged via `AuditAction::Migrate`), synchronous
per-step policy/allow-deny gating, `pause`/`resume` beyond what kill/resume already provides, and any
Hub/GUI surface. These build on these hooks but live in the closed crate / future specs.
