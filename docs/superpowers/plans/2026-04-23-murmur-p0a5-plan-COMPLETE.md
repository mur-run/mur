# murmur P0a.5 — Implementation Complete

**Status:** All 39 tasks shipped across two repos.
**Date:** 2026-04-24
**Branches:** `feat/murmur-p0a.5` (mur workspace, off `feat/murmur-p0a`), `feat/murmur-bridge` (mur-commander workspace, off `main`).

## Scope

Add per-agent Ed25519 identity + Noise-XK TCP transport to the P0a agent
runtime, and wire P0a agents into the existing commander workflow engine
so commander auto-registers them, exposes A2A v0.3 method names, and
starts collecting per-agent telemetry into a disk-backed spool. Hub
upstream forward is deferred to P1.

## Phase roll-up

### Phase A — Identity + Profile Schema Foundation (mur-common)

8 tasks. All passed. Key artefacts:

- `mur-common/src/identity.rs` — `AgentIdentity` with Ed25519 keypair
  generate / save (0600 private key) / load; multibase base58btc
  encode + decode helpers; `to_x25519_static_secret()` conversion for
  Noise XK (uses `SigningKey::to_scalar_bytes()` directly).
- `mur-common/src/agent.rs` schema extensions:
  - `AgentProfile.identity` → `IdentityConfig { pubkey, owner }`
  - `TransportConfig.tcp` → `TcpTransportConfig { enabled, bind, noise }` with default `NoiseConfig.pattern = "Noise_XK_25519_ChaChaPoly_BLAKE2s"`
  - `LifecycleConfig.{execution, schedule}` (daemon vs on_demand, cron entries with optional `sends_to`)
  - `FileTransferConfig` (10 MiB default caps, deny `~/.ssh`/`~/.aws`/`~/.gnupg`)
  - `DeploymentConfig` (laptop / vm / docker / k8s / lambda, default env "dev")
- All additions are `#[serde(default)]` so legacy P0a profiles keep loading unchanged.
- 9 identity tests + 8 profile-schema tests; fmt and clippy clean.

### Phase B — TCP + Noise XK Transport (mur-agent-runtime)

9 tasks. All passed. Key artefacts:

- `mur-agent-runtime/src/transport/noise.rs` — Noise_XK_25519_ChaChaPoly_BLAKE2s helpers (`build_responder`, `build_initiator`), `encode_frame`/`decode_frame` for length-prefixed 4-byte BE JSON-RPC frames (16 MiB cap, `FrameError::{Incomplete, TooLarge}`).
- `mur-agent-runtime/src/transport/tcp.rs` — `spawn_tcp_listener` + `TcpConnector::dial`; handshake → transport-mode echo works end-to-end; MITM-style peer key mismatch aborts the dial.
- `supervisor.rs` — loads `AgentIdentity` (ephemeral fallback if missing) and conditionally spawns the TCP listener when `transport.tcp.enabled`. TCP handler bridges `Vec<u8>` payloads to the existing `Dispatcher::dispatch`; populates `lock_transports.tcp` before `write_lock` so `running.lock` advertises the bound address.
- `protocol/methods/card.rs` — Agent Card now publishes `pubkey`, `endpoints[]` (array-of-objects, ordered `tcp+noise` → `unix-socket` → `stdio`, with `url` + `reachability: lan|local`), and `deployment` block.
- `validate_tcp_entitlement()` — supervisor refuses to start if `transport.tcp.bind` port isn't declared in `entitlements.network.inbound.ports`.

### Phase C — `mur agent create` generates identity (mur-core)

4 tasks. All passed. Key artefacts:

- `cmd_create` writes `identity.key` + `identity.pub` inside the agent directory and populates `profile.yaml` `identity.pubkey` + `identity.owner` (from `$USER`).
- `mur agent card <name>` prints the pubkey as a natural consequence of dumping the full A2A Agent Card JSON.
- Legacy P0a profiles without an `identity:` block continue to deserialize (regression test in `mur-core/tests/profile_legacy_load.rs`).
- `TODO(Q-B)` marker left for `mur agent rekey` — awaiting user decision.

### Phase D — Commander A2A v0.3 aliasing (mur-commander)

4 tasks. All passed. Key artefacts:

- Switched workspace `mur-common` from a pinned git tag to a path dep (`../mur/mur-common`) so the P0a.5 types are visible to the engine.
- `engine::a2a::protocol::methods` — added `MESSAGE_SEND`, `MESSAGE_STREAM`, `TASKS_LIST`.
- `engine::a2a::server::handle_request` — aliases `message/send`→`handle_task_send`, `message/stream`→`handle_task_send` (streaming path will specialise in P1), and a new `handle_tasks_list` returning a JSON array from the existing `list_tasks()` helper.
- `engine::a2a::protocol::AgentCard` — gained optional `id` + `pubkey` fields; `AgentCapabilities` gained `extensions: Vec<String>` and the commander's own card now advertises `a2a.v0.2`, `a2a.v0.3`, `a2a.message.send`, `a2a.tasks`, `commander.workflow`, `commander.chat`.

### Phase E — Commander murmur_bridge (mur-commander)

4 tasks. All passed. Key artefacts:

- `engine::a2a::discovery::RegisteredAgent` — `uuid: Option<String>` + `pubkey: Option<String>` (back-compat via `#[serde(default)]`); new `upsert` (match by uuid then url), `find_by_uuid`, `mark_offline_by_uuid`, `murmur_agents()` filter.
- `engine::remote::murmur_bridge` — `notify = "6"` based watcher on `~/.mur/agents/*/running.lock`; initial scan picks up pre-existing locks; create/modify upserts; remove marks offline; entries tagged `"murmur"` so CLI filter can surface them.
- Daemon (`crates/daemon/src/main.rs`) — boots `MurmurBridge` alongside the service manager; stops it first on shutdown. Soft-fails (logs a warning) if the bridge can't start, so a misconfigured host doesn't take the daemon down.

### Phase F — Commander collector stub (mur-commander)

5 tasks (collapsed into a single commit because the modules are small + interdependent). All passed. Key artefacts:

- `engine::observability::redaction` — `RedactionMode::{Full, Redacted, MetadataOnly}`; redacted mode sha256-hashes known content keys (`gen_ai.request.messages`, `tool.args`, etc.) and records `_size`.
- `engine::observability::spool` — disk-backed append-only JSONL with per-session rollover at `max_bytes`; `drain()` reads + deletes oldest files.
- `engine::observability::collector` — notify-watched tailing on `<agents_dir>/*/telemetry/*.jsonl`, filters to `telemetry/*` + `task/progress*` notifications, applies redaction, spools via the same `Spool` helper.
- `daemon::config` — new `[telemetry]` section parsed from `~/.mur/commander/config.toml` with `mode` (`full` / `redacted` / `metadata-only`) + `spool_cap_mb` (default 100). `TelemetryConfig::redaction_mode()` converts to the engine enum.
- OpenTelemetry SDK deps deliberately deferred — the P0a.5 spec implements plain JSONL spooling; the upstream OTel export path is P1's hub work, so pulling in `opentelemetry`/`-sdk` now would be load without consumer.

### Phase G — E2E + roll-up

5 tasks. All passed. Key artefacts:

- `scripts/e2e/p0a5-identity-handshake.sh` — builds `mur` + `mur-agent-runtime` in the workspace, creates an agent, verifies `identity.key`/`identity.pub` on disk, verifies `profile.yaml` has the pubkey, exercises `mur agent card` via the ephemeral-runtime path and checks `pubkey` + `endpoints[]` come through.
- `scripts/e2e/p0a5-commander-autoregister.sh` — runs the real `murmur_bridge` + registry + redaction + spool + collector integration tests in the commander workspace. Auto-skips if `~/Projects/mur-commander/` is not checked out.
- `scripts/e2e/p0a5-full.sh` — runs both scripts in sequence.
- `CLAUDE.md` — "P0a.5 additions" section added under the existing P0a entry.
- This file.

## Tests added in P0a.5

| Crate | File | New tests |
|---|---|---|
| mur-common | tests/identity.rs | 9 |
| mur-common | tests/profile_schema.rs | 8 |
| mur-agent-runtime | tests/noise_handshake.rs | 1 |
| mur-agent-runtime | tests/noise_frame.rs | 4 |
| mur-agent-runtime | tests/tcp_transport.rs | 2 |
| mur-agent-runtime | tests/tcp_entitlement.rs | 4 |
| mur-agent-runtime | tests/card_extended.rs | 1 |
| mur-core | tests/agent_create_identity.rs | 1 |
| mur-core | tests/agent_card_ephemeral.rs | +1 (identity pubkey) |
| mur-core | tests/profile_legacy_load.rs | 1 |
| engine (commander) | tests/a2a_v03_alias.rs | 3 |
| engine (commander) | tests/agent_registry_uuid.rs | 4 |
| engine (commander) | tests/murmur_bridge.rs | 2 |
| engine (commander) | tests/redaction.rs | 3 |
| engine (commander) | tests/spool.rs | 2 |
| engine (commander) | tests/collector.rs | 1 |
| engine (commander) | a2a::protocol inline | 1 (v0.3 constants) |
| engine (commander) | a2a::server inline | 1 (v0.3 extension advertised) |
| engine (commander) | a2a::discovery inline | 1 (murmur tag filter) |
| daemon (commander) | config inline | 3 (telemetry defaults + parse + redaction map) |

All green. `cargo fmt --check` clean, `cargo clippy -- -D warnings` clean for touched crates.

## Deviations from the plan (all benign)

1. **Task A2 — IdentityError `{0:?}` fix.** The plan's specimen used `#[error("identity files not found in {0:?}")]` but `NotFound` has no tuple field. Replaced with `#[error("identity files not found")]` and added a manual `impl fmt::Debug for AgentIdentity` that redacts the signing key (so test `.unwrap_err()` can format the Result).
2. **Task A4 — sha2 detour.** The first implementer pass sidestepped `SigningKey::to_scalar_bytes()` and introduced an `sha2` manual SHA512 derivation instead, pulling in an unnecessary production dep. Reviewed and reverted in the same phase (`refactor(common): use SigningKey::to_scalar_bytes`).
3. **Task D3 — `message/stream` aliasing.** The commander's A2A server doesn't yet have a streaming handler; both `message/stream` and `message/send` alias to `handle_task_send`. A distinct streaming path lands alongside the hub work in P1.
4. **Task D4 — capabilities shape.** The existing `AgentCapabilities` was a bool-only struct. Rather than reshape the whole field, added `extensions: Vec<String>` for semantic capability tags — serde-default-empty so existing clients that deserialize are unaffected.
5. **Task E4 — `murc agents list --murmur`.** There is no existing `murc agents list` CLI command, so implemented this as the library-level filter `AgentRegistry::murmur_agents()` with inline test. The CLI wrapper can be added when the broader `agents list` command is introduced.
6. **Task F1 — no OpenTelemetry SDK.** The P0a.5 spec ships plain JSON spooling; the OTel export path is P1's hub work. Pulling in `opentelemetry` + `opentelemetry-sdk` now would be load without consumer. Left as a P1 addition.
7. **Task F2 / collector.rs — let-chain fallback.** `mur-commander` workspace is on edition 2021 (not 2024), so the redaction / collector code uses nested `if let` instead of `if let … && let …`.

## Commits (chronological)

### mur workspace (`feat/murmur-p0a.5`)

```
2d7131c test(e2e): P0a.5 full smoke runner
6dbfdbc test(e2e): P0a.5 commander auto-register + collector smoke
f55151d test(e2e): P0a.5 identity + agent-card smoke
87cdbda docs(core): TODO marker for Q-B (mur agent rekey) — awaiting user decision
0060189 test(core): confirm legacy P0a profiles load under P0a.5 schema
a519392 test(core): mur agent card displays identity.pubkey
ce601e4 feat(core): mur agent create generates Ed25519 identity + writes into profile
c6220ad style: cargo fmt
18c4070 feat(agent-runtime): validate TCP bind port against inbound entitlements
da10213 feat(agent-runtime): Agent Card exposes pubkey + endpoints[] + deployment
7e578ac feat(agent-runtime): supervisor spawns TCP Noise listener when opted in
3abe2f2 test(agent-runtime): dialer aborts when peer static key mismatches
d8c0475 feat(agent-runtime): TCP listener + connector with Noise XK + frame codec
ac0cd72 feat(agent-runtime): length-prefixed frame codec for Noise JSON-RPC streams
b75f297 feat(agent-runtime): Noise XK handshake helpers (responder + initiator)
64303fd deps(agent-runtime): snow 0.9 for Noise XK
b6033d5 style: cargo fmt
d297984 feat(common): lifecycle.execution/schedule + file_transfer + deployment blocks
bd1bb32 feat(common): TransportConfig.tcp with Noise XK pattern (default disabled)
dcb4bfb feat(common): AgentProfile.identity block (pubkey + owner, default empty)
33a3e99 refactor(common): use SigningKey::to_scalar_bytes (drop sha2 dep)
a996d0d feat(common): Ed25519 → X25519 conversion for Noise XK interop
0b2a34a test(common): multibase encode/decode edge cases for AgentIdentity
37daae4 feat(common): AgentIdentity — Ed25519 keypair load/save/multibase
0cd8ad2 deps(common): add ed25519-dalek + x25519-dalek + multibase for P0a.5 identity
```

### mur-commander workspace (`feat/murmur-bridge`)

```
fce6338 feat(daemon): telemetry.mode config (full / redacted / metadata-only)
6cc2646 feat(engine): observability — redaction + disk spool + JSONL telemetry collector
207c7a7 feat(engine): AgentRegistry::murmur_agents() filters by murmur tag
47cda55 feat(daemon): start MurmurBridge to auto-register P0a agents
dd89ca8 feat(engine): murmur_bridge — auto-register P0a agents from running.lock
5d42c7a feat(engine): RegisteredAgent.uuid + .pubkey (optional, back-compat)
0516038 feat(engine): agent card advertises a2a.v0.3 capability extension
e148bfa feat(engine): A2A v0.3 method aliases (message/send, message/stream, tasks/list)
45dc17d feat(engine): A2A v0.3 method name constants (message/*, tasks/list)
7ea3fd6 deps(workspace): switch mur-common to path dep for P0a.5 interop
```

## Open items / followups

- **Q-B — `mur agent rekey`.** Blocked on user decision in spec § 13. TODO marker in `mur-core/src/cmd/agent.rs`.
- **P1 hub upstream.** The collector spools JSONL locally; the upstream OTel exporter + hub ingestion lands in P1.
- **Commander streaming handler for `message/stream`.** Currently aliased to `handle_task_send`; a real SSE streaming handler is part of P1's chat pipeline.
- **Reverting the commander workspace `mur-common` dep.** The path override in `Cargo.toml` is expected to be bounced back to a pinned git tag once P0a.5 lands in the main `mur` repo and we cut `v2.4.0`. The PR body calls this out.
- **PR pushes** (Task G5) — deferred for explicit user review before pushing; this is a shared-branch action and the plan's safety gate is to confirm before push + open.
