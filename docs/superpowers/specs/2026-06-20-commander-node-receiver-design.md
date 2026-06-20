# Commander node-side receiver — design

**Goal:** Add the node-side HTTP endpoints that accept a cross-network, commander-signed
governance directive from the closed Commander engine and let the existing
`fold_governance` enforce it — plus the audit-readback endpoint the engine's
ComplianceChecker reads.

**Status:** open-source side of the commander/team-shared-fleets split. The hooks
(directive type, fold, loop+daemon honoring, local CLI, audit) shipped in PR #468. The
engine (closed `mur-commander`) is built on `feature/commander-engine-v1` and delivers
over HTTP. This spec is only the node receiver.

## Boundary / what already exists (do NOT rebuild)
- `mur_common::commander::{CommanderDirective, COMMANDER_DIRECTIVE_KEY, GovernanceState}`
- `mur_channel::sign::{sign_input, verify_event_sig}` and `governance::fold_governance`
- `mur_core::cmd::commander::accepted_pubkeys(mur_home) -> Vec<[u8;32]>`
- `mur_core::conversations::audit::{Audit, AuditAction::Governance, AuditEntry}`
- The node HTTP server `mur-core/src/server/mod.rs` (axum, `/api/*` bearer-auth via
  `require_auth`, token = `MUR_SERVER_TOKEN`, required on any non-loopback bind).

## Engine contract this must match (from `feature/commander-engine-v1`)
- **Transport:** plain HTTPS to the node's `mur-core` server. Push (engine → node).
- **Paths (versioned — engine PR updates its two constants to match):**
  - `POST /api/v1/governance/directive`
  - `GET  /api/v1/governance/audit/{fleet}`
- **Auth:** `Authorization: Bearer <token>` where the bearer equals the node's
  `MUR_SERVER_TOKEN`. (Operator sets the engine's `nodes.toml` `bearer_token_env` to an
  env holding that value.) The existing `require_auth` middleware already enforces this
  on `/api/*`; no new auth code.
- **POST request body:** `{ "event": <ChannelEvent JSON> }` — the engine sends the full
  pre-signed event: `actor` = `system`, `kind` = `note`, `payload` =
  `{ "commander_directive": {kind, fleet, budget_usd, nonce, issued_at_ms} }`,
  `idempotency_key` = nonce, `sig` = multibase Base58Btc Ed25519 over
  `sign_input{v:1, channel_id, actor, kind, payload, idempotency_key}` (seq/ts excluded;
  `seq` arrives as a placeholder `0`).
- **POST success:** `200 { "accepted": true, "seq": <u64> }`.
- **POST reject:** non-2xx + `{ "accepted": false, "reason": "<msg>" }`.
- **GET response:** `{ "entries": [AuditEntry...] }` — the engine matches an entry by
  `action.nonce` and recomputes `content_sha256 = hex(sha256(sign_input(...)))` to judge
  compliance. The existing `AuditEntry` serde shape (`action` tagged
  `{kind:"governance",fleet,directive,decision,nonce}` + `content_sha256`,`ts`,
  `prev_hash`,`entry_hash`) is exactly what it expects.

## Architecture
Two handlers in a new `mur-core/src/server/governance.rs`, two routes in
`server/mod.rs`. No daemon, no new transport, no new dependency. `fold_governance`
(read by loop + daemon) picks up the persisted event automatically — unchanged.

Approaches considered: (A) REST in `mur-core` server — chosen, it is exactly what the
engine delivers to. (B) daemon WebSocket A2A method — rejected: does not match the
engine's HTTP delivery.

## Data flow — POST /api/v1/governance/directive
1. Parse `{event}`. Missing/invalid JSON → 400.
2. Extract `event.payload["commander_directive"]["fleet"]` (string). Absent → 400.
3. `channel_id = format!("fleet-{fleet}")` — derived from the directive, NOT trusted from
   the body (the signature is bound to this exact id).
4. `keys = accepted_pubkeys(mur_home)`. Empty → 403 `"no commander key pinned"`.
5. Pull `actor`, `kind`, `payload`, `idempotency_key`, `sig`, `key_version` from `event`.
   Missing `sig` → 403. Verify: some `pk` in `keys` satisfies
   `verify_event_sig(channel_id, &actor, kind, &payload, idempotency_key.as_deref(),
   sig, pk)`. None → 403 `"invalid signature"`.
6. Channel `channel_id` must exist — check the channel **manifest** (`channel.yaml` /
   `ChannelStore::load_manifest`), NOT `load_events` (which returns `Ok(empty)` for a
   missing channel and so cannot prove existence — and `append_event` must not be allowed
   to auto-create a channel for an attacker-named fleet). Absent → 404
   `"unknown fleet channel"`. Confirm during implementation whether `append_event`
   auto-creates; if it does, the manifest pre-check is the guard.
7. Persist verbatim (no re-sign):
   `ChannelStore::new(mur_home).append_event(channel_id, actor, kind, payload,
   idempotency_key, Some(sig), key_version)`. `append_event` dedups by
   `idempotency_key` (= nonce) under lock → a duplicate POST returns the existing event.
8. **On a fresh append only**, emit a receipt audit row:
   `Audit::open(mur_home).append(AuditAction::Governance{ fleet, directive: kind,
   decision: "received", nonce }, content_sha256)` where
   `content_sha256 = hex(Sha256(sign_input(channel_id, &actor, kind, &payload,
   idempotency_key.as_deref())))`. Dedup: skip if this nonce already has a `received`
   row (keeps the audit idempotent under retried POSTs). This makes compliance verifiable
   immediately and independent of whether/when a loop runs (a kill on an idle fleet would
   otherwise never produce an audit row — the loop is skipped precisely because it's
   killed). The loop still writes its own `halted`/`capped` rows on enforcement.
9. `200 { "accepted": true, "seq": <event.seq> }`.

## Data flow — GET /api/v1/governance/audit/{fleet}
1. `entries = audit::read_entries(mur_home)?` (new fn) filtered to
   `AuditAction::Governance` whose `fleet` matches the path param.
2. Optional `?since_nonce=<n>`: drop entries up to and including the one whose
   `action.nonce == n` (best-effort; absent param → all).
3. `200 { "entries": [AuditEntry...] }`.

## New/changed code
- `mur-core/src/server/governance.rs` — `post_directive`, `get_audit` handlers (~120 lines).
- `mur-core/src/server/mod.rs` — 2 `.route(...)`; add `AppError::Forbidden(String) -> 403`;
  add `AppState::mur_home()` mirroring `skills_dir()` (`patterns_dir.parent()`).
- `mur-core/src/conversations/audit.rs` — `pub fn read_entries(root_override: Option<&str>)
  -> Result<Vec<AuditEntry>>` (read `conversations/audit.jsonl`, parse each line); add
  `"received"` to the `AuditAction::Governance.decision` doc comment.
- Confirm `mur_channel::store::ChannelStore` is `pub` with `new(&Path)` + `append_event`;
  if `ChannelStore` is not re-exported, add a thin `ChannelService` pass-through that
  forwards `sig`/`key_version` (do NOT use `append`/`append_signed` — they drop or replace
  the commander's signature).

## Security / fail-closed
- Two layers: network bearer (`require_auth`) + commander Ed25519 signature (step 5,
  defense-in-depth — `fold_governance` re-verifies on read).
- `channel_id` is derived from the signed `fleet`, never taken from the request, so a
  caller cannot redirect a valid signature to another channel.
- Every reject path writes nothing before returning the error.
- No commander key pinned ⇒ 403 (closed by default; governance is opt-in via
  `mur commander pin`).

## Testing (one integration test file)
Pin a generated test commander key (`mur commander pin`), create a `fleet-dev` channel:
- valid signed kill → 200 `{accepted,seq}`; channel has the event; audit has a `received`
  row with matching nonce + `content_sha256`; `fold_governance` reports `killed`.
- wrong-key signature → 403, nothing persisted.
- no commander pinned → 403.
- unknown fleet → 404.
- duplicate POST (same nonce) → idempotent: same seq, `accepted:true`, no second audit row.
- `GET audit/{fleet}` returns the `received` row; recomputed `content_sha256` matches.
- (auth itself is covered by existing `require_auth` tests.)

## Dependency / coordination
The engine PR (`feature/commander-engine-v1`) updates `DIRECTIVE_PATH` and the audit URL
to the `/api/v1/governance/...` paths above. Path is not part of `sign_input`, so the
engine's golden-hash test is unaffected. Land both together.
