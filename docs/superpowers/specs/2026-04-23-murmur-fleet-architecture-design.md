# murmur Fleet Architecture — P0a.5 → P1 → P2 Design

**Status:** Draft — spec under review.
**Date:** 2026-04-23
**Authors:** David + Claude.
**Predecessor:** [`2026-04-22-murmur-p0-agent-runtime-design.md`](./2026-04-22-murmur-p0-agent-runtime-design.md) — P0a agent runtime foundation (shipping via PR #24 on `feat/murmur-p0a`).
**Successor stubs (not written yet):** separate P0a.5 implementation plan · P1 mur-hub detailed spec · P2 enterprise / K8s operator spec.

---

## 0. Executive Summary

Extend murmur from a **single-host** per-agent runtime (P0a) to a **fleet-wide** control plane suitable for cross-host orchestration, cross-NAT routing, UUID-based directory lookup, fleet observability, and cloud deployment targets. Introduce a new product `mur-hub` as the stateless-ish coordinator (directory + signaling + optional TLS relay + OTLP ingest + scheduler), keep existing `mur-commander` v0.7 untouched as the workflow engine + chat-platform gateway + per-host collector, formalize the integration contract between `mur-agent-runtime` (P0a) and `mur-commander`, and add agent identity (Ed25519) + Noise XK handshake as the cryptographic substrate for cross-host A2A. Data plane stays P2P whenever possible; hub is only in the hot path for discovery, signaling, and fallback relay of encrypted bytes.

Three staged phases:

| Phase | Scope | Est LOC | Duration |
|---|---|---|---|
| **P0a.5** | Agent identity keypair + TCP transport (Noise XK) + commander integration + A2A v0.3 method aliasing + collector role wiring | ~2 000 | 2-3 weeks after P0a merges |
| **P1** | `mur-hub` binary (solo + team modes) + directory/signaling/relay/OTLP/scheduler + `mur agent export --format={docker,k8s,cloudrun,lambda}` + `profile.lifecycle.{execution,schedule}` + `files/fetch` RPC + P0b entitlement enforcement + dashboard `/api/hub/*` | ~8 000-10 000 | 8-10 weeks |
| **P2+** | Hub cloud mode (multi-tenant + S3 blob storage) + K8s Operator + `MurmurAgent` CRD + external OTLP backend proxies + optional S3 file backend | ~5 000 | 3-4 weeks |

The design is directly aligned with the five user-stated anchors: (1) local execution, (2) exportable standalone agents, (3) commander-centric UUID routing with cross-host file / JSON transport, (4) fleet-wide observability across multiple murmur hosts, (5) future GCP/AWS cloud deployment.

---

## 1. Background & Motivation

### 1.1 What P0a delivered

P0a (PR #24, merging this week) ships a per-agent, OS-native executable model: `mur agent create <name>` produces a BusyBox-style symlink `mur_agent_<name>` to a shared `mur-agent-runtime` binary, plus a profile directory under `~/.mur/agents/<name>/`. Agents speak an A2A v0.3 subset over stdio or Unix socket, act as MCP clients for tools, declare fine-grained entitlements (enforcement deferred to P0b), and emit OpenTelemetry-GenAI-compliant JSONL telemetry. Discovery is filesystem-based via `running.lock` files; peer-to-peer A2A calls work without any central daemon.

P0a explicitly declared itself "completely independent of the existing mur CLI and MUR Commander."

### 1.2 What commander already has (v0.7.0)

The existing `mur-commander` product — shipped separately, 10 sub-crates, published on Homebrew tap and Docker Hub — is **not** merely a pattern-to-workflow bridge. It is a full local-first agent platform with:

- **Workflow engine** (YAML workflows, shadow mode, checkpoints, auto-fix, NL generation)
- **Chat platform gateways** (Slack, Telegram, Discord) in `crates/chat/` + `crates/gateway/`
- **Multi-machine support** — `crates/engine/src/remote/` with SSH direct execution and `mur.run relay` fallback, machine inventory with capability tags
- **A2A v0.3 server/client** at `crates/engine/src/a2a/` — uses **older method names** (`tasks/send`, `tasks/sendSubscribe`) vs. P0a's v0.3 names (`message/send`, `message/stream`)
- **Agent registry** at `~/.mur/commander/agents.json`, URL-keyed, no UUID field
- **Sub-agents** — in-process async task delegation (up to 5 concurrent) — distinct from P0a's separate-process agents
- **Scheduling** (cron + NL + file triggers + webhooks)
- **Jira integration** (12 commands + 6 agentic tools + `@mur implement PROJ-123`)
- **Safety** — Ed25519 constitution, hash-chained audit, policy engine, MCP sandbox
- **Plugin system** — MCP, CLI, REST providers
- **Docker Hub releases** for `linux/amd64` + `linux/arm64`

The commander's current workflow daemon is already the closest thing murmur has to a "fleet controller"; P0a's per-agent runtime was designed to be orthogonal to it. This spec formalizes how they compose.

### 1.3 The gap

User's five anchors map to gaps as follows:

| Anchor | P0a | Commander v0.7 | Gap |
|---|---|---|---|
| 1. Local execution | ✅ | ✅ | — |
| 2. Exportable standalone agents | ✅ `.murpkg` + self-contained binary | — | Cloud/container export format missing |
| 3. UUID routing + cross-host file / JSON transfer | ❌ (filesystem registry only) | Partial (URL-keyed registry, SSH transport, older A2A methods) | UUID as first-class key; P2P with NAT traversal; chunked file transfer |
| 4. Observe multiple murmurs + all agents | Per-agent JSONL only | Local dashboard only | Cross-host aggregation pipeline + fleet dashboard |
| 5. Cloud on GCP/AWS | Native binary only | Docker image but no cloud mode | Hub cloud deploy + container/K8s agent archetypes |

### 1.4 Research context (comparative architectures)

Parallel research into Anthropic's Claude Managed Agents + Claude Agent SDK and OpenAI's Agent Builder + Agents SDK + Responses API informed several decisions in this spec:

- **Neither vendor ships an inter-agent wire protocol** (A2A is murmur's differentiator)
- **Claude Managed Agents** has `multiagent_sessions` API (team-lead decomposition) + credential vault + file checkpointing
- **Claude Agent SDK** exposes 18 hook callbacks + `permissionMode` + `canUseTool` callback
- **OpenAI Agents SDK** has resumable `RunState` with tool-approval interruptions + three-outcome guardrails (skip / replace / tripwire) + provider-agnostic via LiteLLM
- **OpenAI Agent Builder** uses a DAG-as-JSON workflow with code-export to Python/TS + org-scoped Connector Registry
- **Both vendors have hosted serverless modes** (Anthropic $0.08/runtime-hour); `mur-hub` is architected to enable but never require this pricing shape

---

## 2. Goals / Non-Goals

### 2.1 P0a.5 Goals

- **G1.** Every agent has a long-lived Ed25519 identity keypair (`~/.mur/agents/<name>/identity.key` + `identity.pub`), generated at `mur agent create` time; UUID + pubkey is the global identity tuple.
- **G2.** `mur-agent-runtime` supports TCP transport in addition to Unix socket; TCP uses **Noise XK** handshake with known receiver pubkey + authenticated sender pubkey + E2E encryption. Bearer-token auth (originally P0b) is dropped in favor of Noise.
- **G3.** Agent Card (`agent/card` RPC response) includes `pubkey`, `endpoints[]` (ordered preference list), and `deployment` metadata (type: laptop / vm / docker / k8s / lambda; region; environment).
- **G4.** `mur-commander` A2A server/client supports v0.3 method names (`message/send`, `message/stream`, `tasks/list`) as aliases for its existing `tasks/send` etc., to talk to P0a agents without translation layer.
- **G5.** `mur-commander` auto-registers local P0a agents: a new collector subsystem tails `~/.mur/agents/*/running.lock` changes and populates commander's agent registry (UUID + URL + pubkey).
- **G6.** `mur-commander` starts the **collector role**: tails `~/.mur/agents/*/telemetry/*.jsonl` with inotify/FSEvents, normalizes to OTLP batches, buffers on disk at `~/.mur/commander/telemetry-spool/`, forwards to hub (stubbed until P1 ships hub).
- **G7.** Profile schema stable with new fields for cloud/scheduler (locked for P1 usage).

### 2.2 P1 Goals

- **G8.** `mur-hub` is a single Rust binary with three boot modes (`--solo`, `--team`, `--cloud`); SQLite backs solo, Postgres backs team/cloud.
- **G9.** Hub exposes Directory API (`POST /v1/agents/register`, `GET /v1/agents/{uuid}/endpoint`, `POST /v1/agents/{uuid}/heartbeat`).
- **G10.** Hub exposes Signaling API (WebSocket-based ICE-like endpoint exchange) between two agents that have resolved each other's UUIDs.
- **G11.** Hub exposes TLS Relay endpoint for fallback when P2P fails; relay is live-pipe only (no persistence beyond 10-minute in-memory buffer capped at 100 MB).
- **G12.** Hub exposes OTLP ingest endpoints (`/v1/otlp/traces`, `/v1/otlp/logs`, `/v1/otlp/metrics`); optionally proxies to external OTel backend (Tempo / Honeycomb / Datadog / Grafana Cloud).
- **G13.** Hub exposes Scheduler API (cron-triggered `message/send` invocations for agents with `lifecycle.execution: on_demand`).
- **G14.** `mur agent export` gains new formats: `--format=docker`, `--format=k8s`, `--format=cloudrun`, `--format=lambda`.
- **G15.** `files/fetch` JSON-RPC method added for chunked resumable file streaming between peers; sender outbox with TTL; receiver approval threshold.
- **G16.** P0b entitlement enforcement lands (container mapping + host sandbox). Entitlement declarations are automatically translated into Dockerfile + K8s SecurityContext + NetworkPolicy at export time.
- **G17.** `mur-server` dashboard gains `/api/hub/*` endpoints + multi-host observability UI.

### 2.3 Non-goals (explicit)

- **NG1.** Do not rename or rescope `mur-commander` v0.7; keep its name and full feature set. Slack/TG/Discord/Jira integrations remain in place and gain P0a agent reachability as a new capability rather than a replacement.
- **NG2.** No gossip mesh / Consul-style quorum / multi-DC federation. Per-host commander + central hub is sufficient for target scale.
- **NG3.** Hub is not a mailbox. It does not persist file content beyond the 10-minute live-relay window. If async file drop is needed, users configure an external S3-compatible store (P2+ feature).
- **NG4.** No "L3" commander-in-cloud layer. For regional/tenant isolation, deploy multiple hubs (partitioned by hostname or tenant ID); commander does not act as a regional gateway.
- **NG5.** No autoscaling of individual agents. Agents are identity-bearing long-lived entities, not stateless workload units. Scale-out is by creating additional agents, not replicating one.
- **NG6.** No vendor-specific cloud abstraction layer. Export targets are OCI image / K8s / Cloud Run / Lambda — open standards. No "mur cloud" SaaS in this spec (can layer on later).
- **NG7.** Agents never reach hub directly; all telemetry, registration, and scheduler interactions flow through the local commander. This keeps agent profiles hub-agnostic (relocatable) and preserves buffer-redact-batch discipline.
- **NG8.** No K8s Operator or `MurmurAgent` CRD in P1. Raw StatefulSet YAML (emitted by `mur agent export --format=k8s`) is sufficient; operator is P2.
- **NG9.** No hub-side persistent storage of LLM content (prompts/responses) by default. `redacted` mode is the team default; `metadata-only` is the public-hub default. `full` mode is solo-dev convenience only.
- **NG10.** No Bearer token auth on cross-host A2A. Noise XK with known pubkeys supersedes the P0a-spec's original P0b bearer-token plan.

---

## 3. Locked Design Decisions

| # | Axis | Decision | Rationale |
|---|---|---|---|
| **F1** | Commander role | Keep name, keep all v0.7 features, add integration with P0a runtime + add collector role. Do **not** rename / rescope / deprecate. | v0.7 is shipped with Homebrew tap + Docker Hub + 10-crate architecture (chat/gateway/jira/workflow etc.); rescoping would destroy established product value. |
| **F2** | New product vs. extension | Add **new product `mur-hub`** for fleet control plane; commander and hub are distinct products with clear scope. | Commander's job is workflow execution + human interfaces; hub's job is fleet directory + cross-machine coordination. Different lifecycles, different deployment shapes. |
| **F3** | Data-plane topology | **Tailscale-inspired hybrid**: hub mediates discovery + signaling + optional relay; data plane is P2P over Noise-encrypted TCP when direct reachability is possible. | All mature systems (Tailscale, Nebula, Zerotier, WebRTC) converged on this. Privacy-preserving: hub sees metadata + encrypted bytes only. |
| **F4** | Identity | Per-agent **Ed25519 keypair** generated on `mur agent create`; UUIDv7 + pubkey is global identity tuple; pubkey embedded in Agent Card and export bundles. | Foundation for Noise handshakes and hub signaling. Matches P0b's original "sign Agent Card" goal at identity layer. |
| **F5** | Cross-host transport | **Noise XK over TCP**, or HTTPS with mTLS for hub ingress. Agent Card advertises `endpoints[]` ordered by preference. | Noise XK matches the "I know receiver pubkey, I prove my pubkey" pattern; simpler than mutual certificate management. TLS for hub-facing endpoints for browser / cloud compatibility. |
| **F6** | A2A method compatibility | Commander adds v0.3 method aliases (`message/send`, `message/stream`, `tasks/list`) pointing to existing handlers; P0a's v0.3 names become canonical. | Commander's older `tasks/send` naming stays for backward compat; P0a agents interoperate immediately. |
| **F7** | Observability flow | **Two-tier push**: agent emits JSON-RPC notifications to its process's commander; commander normalizes to OTLP, buffers on disk, batches, pushes to hub. Agents never touch hub directly. | Per-host batching; crash-safe buffer; central place for redaction; hub-URL change doesn't require agent restart. |
| **F8** | Data format | Full adoption of **OpenTelemetry GenAI semantic conventions**; no murmur-specific telemetry format. | Future-proof; any OTel backend (Tempo, Honeycomb, Datadog, Grafana Cloud) works. |
| **F9** | Redaction model | Three commander-configurable modes: `full` / `redacted` / `metadata-only`. Defaults: solo=`full`, team=`redacted`, public-cloud=`metadata-only`. | Matches real tension between dev convenience and team/public privacy. |
| **F10** | File transfer | A2A `FilePart` three forms: `bytes` (<256 KB inline), `file_id` (chunked P2P stream via new `files/fetch` RPC), `uri` (external object store). Hub relays encrypted chunks live (10 min / 100 MB cap) when P2P fails; never persists. | Best practice from Tailscale Taildrop, Magic Wormhole, WebRTC data-channel. Hub as mailbox = scope creep + privacy + storage cost. |
| **F11** | Hub deployment modes | One binary with three modes: `--solo` (SQLite, 127.0.0.1), `--team` (Postgres, LAN/VPN), `--cloud` (Postgres + S3 + multi-tenant + public HTTPS). | One codebase; user scales up by reconfiguring, not by swapping tools. |
| **F12** | Cloud agent archetypes | Four: (1) hub-in-cloud + on-prem agents, (2) Docker container agent with runtime image + volume-mounted profile, (3) K8s StatefulSet (raw YAML; operator is P2), (4) Serverless scheduled agent (Cloud Run Job / Lambda, identity keys in Secret Manager). | Covers spectrum from personal laptop to enterprise K8s without vendor lock-in. |
| **F13** | Unified export | Extend `mur agent export --format=` family with `docker / k8s / cloudrun / lambda`. Same source profile; different deployment shells. | P0a already established `.murpkg` + `bin` formats; cloud formats follow the same philosophy. |
| **F14** | Entitlement enforcement in containers | Entitlement YAML compiles to Dockerfile ARGs + K8s `SecurityContext` + `NetworkPolicy` + `ResourceQuota`. Host-level P0b sandbox (sandbox-exec / bwrap) remains the enforcement path for native binaries. | Container isolation is naturally stronger than user-space sandbox; doubles as P0b's cloud story. |
| **F15** | Scheduler location | Hub owns the scheduler for `on_demand` agents (cron, event triggers). Per-host commander owns launchd/systemd installation for `daemon` agents. | Scheduler is inherently centralized (needs durable cron state); OS supervisor is inherently per-host. Split by nature. |
| **F16** | `profile.lifecycle` extension | Add `execution: daemon \| on_demand` and `schedule: [{cron, message, sends_to}]` fields. Cold-start agents reuse their identity key from Secret Manager / local file. | Enables Archetype 4 (serverless scheduled) without a second product. |
| **F17** | Commander-to-hub protocol | Commander → hub uses OTLP/HTTP for telemetry and a REST+WebSocket admin API for registry / scheduling / policy sync. | Standard-shape integration; no novel protocol. |
| **F18** | SSH backbone preservation | Commander's existing `remote/ssh.rs` + `remote/relay.rs` remain as LAN/fallback paths when hub is not configured; not the primary path once hub is deployed. | Zero-regression for current commander users; hub is opt-in. |

---

## 4. Architecture Overview

### 4.1 Topology

```
                    ┌─────────── mur-hub ─────────────┐
                    │  Directory (uuid → pubkey +     │
                    │    endpoints[] + owner_identity)│
                    │  Signaling (ICE-like over WS)   │
                    │  Relay fallback (10 min /       │
                    │    100 MB encrypted bytes)      │
                    │  OTLP ingest (HTTP/JSON)        │
                    │  Scheduler (cron → invoke)      │
                    │  modes: solo / team / cloud     │
                    └──┬─────────────────────┬────────┘
                       │register/OTLP         │register/OTLP
           ┌───────────┴───┐               ┌──┴──────────────┐
           │ Host A        │               │ Cloud target     │
           │ (laptop/      │               │ (Docker run /    │
           │  on-prem)     │               │  K8s StatefulSet/│
           │               │               │  Cloud Run Job / │
           │ mur-commander │               │  Lambda)         │
           │  v0.7+        │               │                  │
           │   + collector │               │ mur-agent-       │
           │   + registry  │               │  runtime         │
           │   + A2A v0.3  │               │ (pubkey stable   │
           │   + chat/jira │               │  across restarts │
           │               │               │  via Secret Mgr) │
           │ mur_agent_a   │               │                  │
           │ mur_agent_b   │               │                  │
           └──────┬────────┘               └────────┬─────────┘
                  ╲                                 ╱
                   ╲        P2P Noise XK           ╱
                    ╲      direct data plane      ╱
                     ╲══════════════════════════╱
                      (end-to-end encrypted; hub unaware of content)
```

### 4.2 Product boundaries

| Product | Scope | Distribution | Language | Binary count |
|---|---|---|---|---|
| `mur-core` | Pattern memory (capture/store/retrieve/inject/evolve) | Homebrew `mur`, Cargo workspace member | Rust | 1 CLI + embedded Axum server |
| `mur-commander` v0.7+ | Workflow engine + chat/jira gateways + SSH multi-machine + agent registry + **NEW: collector + v0.3 method aliasing** | Homebrew `mur-commander`, Docker Hub `murrun/mur-commander` | Rust | ~10 sub-crates, multiple binaries (`murc`, `mur-daemon`, `mur-gateway`) |
| `mur-agent-runtime` | Per-agent OS-native executable (A2A v0.3, MCP clients, telemetry) | Bundled with `mur-core`, symlinked per-agent | Rust | 1 multi-call binary (BusyBox-style) |
| `mur-hub` **(NEW)** | Fleet control plane: directory + signaling + relay + OTLP ingest + scheduler | Homebrew `mur-hub`, Docker Hub `murrun/mur-hub`, `kubectl apply` YAML | Rust | 1 binary, 3 modes |
| `mur-server` | Next.js dashboard UI + docs site at app.mur.run | Deployed as web app | TypeScript + Next.js | 1 web app, consumes hub REST API |

### 4.3 Trust boundaries

- **Within a host:** Agent ↔ commander uses Unix socket + SO_PEERCRED (P0a baseline). No auth token needed.
- **Across hosts:** Agent ↔ agent uses Noise XK over TCP. Sender knows receiver pubkey (from Agent Card lookup via hub); receiver verifies sender pubkey is in its `accepts_from` allowlist.
- **Agent ↔ hub (registration + heartbeat):** HTTPS + request body signed by agent's Ed25519 key (Ed25519-SHA512 detached signature over canonical JSON). Hub verifies signature matches the pubkey registered on first-use. See open question Q-K.
- **Commander ↔ hub (telemetry + admin):** mTLS or HTTPS + Bearer token; commander-held token managed out-of-band (operator input).
- **Hub ↔ storage (Postgres / S3):** Infrastructure-layer auth (connection strings, IAM roles); opaque to clients.

---

## 5. Product Details

### 5.1 `mur-agent-runtime` changes (P0a.5)

#### 5.1.1 Identity keypair

On `mur agent create <name>`:

```
~/.mur/agents/<name>/
  identity.key          ← Ed25519 private key (file mode 0600)
  identity.pub          ← Ed25519 public key (multibase encoded text)
  profile.yaml          ← existing P0a fields + new `identity` block
```

Profile addition:

```yaml
identity:
  uuid: 01JQX4TM8Y9K7VQH6B2N3R5DPE        # existing, renamed from top-level `id`
  pubkey: "z6Mk...base58btc"                # new — public half of identity.key
  owner: "david@twdd.com.tw"                 # new — who created this agent (free-form string)
```

#### 5.1.2 TCP transport

Extension of P0a's `transport.socket` block:

```yaml
transport:
  stdio: true
  unix_socket:                               # renamed from plain `socket` for clarity
    enabled: true
    bind: "unix:///Users/david/.mur/agents/agent_a/agent.sock"
  tcp:                                       # NEW in P0a.5
    enabled: false                           # opt-in; off by default
    bind: "0.0.0.0:39393"                    # port 393NN chosen to avoid common conflicts
    noise:
      pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s"   # standard Noise XK
      # Server static key = identity.key; client must already know it
```

When TCP is enabled:
- `mur_agent_<name> start` binds TCP listener (in addition to Unix socket)
- Each connection performs Noise XK handshake: peer proves pubkey via the pattern's `s` token exchange
- Post-handshake, JSON-RPC 2.0 frames are length-delimited (4-byte big-endian length prefix, then JSON body) — same as A2A spec allows

#### 5.1.3 Agent Card extension

```json
{
  "protocolVersion": "a2a/0.3",
  "name": "agent_a",
  "id": "01JQX4TM8Y9K7VQH6B2N3R5DPE",
  "pubkey": "z6Mk...base58btc",
  "displayName": "Price Hunter",
  "version": "0.1.0",
  "description": "...",
  "capabilities": ["a2a.message.send", "a2a.tasks"],
  "transports": ["stdio", "unix-socket", "tcp+noise"],
  "endpoints": [
    {"transport": "tcp+noise", "url": "tcp://lan-host:39393", "reachability": "lan"},
    {"transport": "unix-socket", "url": "unix:///Users/david/.mur/agents/agent_a/agent.sock", "reachability": "local"},
    {"transport": "stdio", "url": "pipe://self", "reachability": "local"}
  ],
  "deployment": {
    "type": "laptop",                        // laptop | vm | docker | k8s | lambda
    "region": null,
    "environment": "dev"
  },
  "persona": { ... },
  "skills": [ ... ],
  "entitlements": { ... }
}
```

### 5.2 `mur-commander` extensions (P0a.5 + P1)

No existing feature is removed or renamed. Extensions:

#### 5.2.1 A2A v0.3 method aliasing (P0a.5)

File: `crates/engine/src/a2a/protocol.rs` (existing), add:

```rust
pub mod methods {
    // Legacy (keep)
    pub const TASKS_SEND: &str = "tasks/send";
    pub const TASKS_SEND_SUBSCRIBE: &str = "tasks/sendSubscribe";

    // v0.3 aliases (new)
    pub const MESSAGE_SEND: &str = "message/send";
    pub const MESSAGE_STREAM: &str = "message/stream";
    pub const TASKS_LIST: &str = "tasks/list";
    // tasks/get and tasks/cancel unchanged — same names in both versions
}
```

Dispatcher maps `message/send` → same handler as `tasks/send`; `message/stream` → same as `tasks/sendSubscribe`; `tasks/list` → new handler returning `A2aServer::tasks` map filtered.

#### 5.2.2 P0a agent auto-registration (P0a.5)

New module `crates/engine/src/remote/murmur_bridge.rs`:

- Watches `~/.mur/agents/` via inotify / FSEvents
- On `running.lock` created: reads file, parses UUID + URL (Unix socket path or TCP endpoint), POSTs to local `AgentRegistry` (existing `crates/engine/src/a2a/discovery.rs`) with `RegisteredAgent { uuid, url, pubkey, ... }`
- `RegisteredAgent` gains new `uuid: Option<String>` field (Option for backward compat with non-murmur external agents)
- On `running.lock` removed: marks agent offline

#### 5.2.3 Collector role (P0a.5 stub, P1 full)

New module `crates/engine/src/observability/collector.rs`:

- Watches each registered agent's `telemetry/*.jsonl` file via inotify
- Tails newly-appended lines
- Normalizes JSON-RPC notification payload → OTel span/log/metric
- Applies redaction policy (full/redacted/metadata-only) from commander config
- Buffers in `~/.mur/commander/telemetry-spool/` (disk-backed ring, ~100 MB cap)
- P0a.5: stub — just writes to spool; no upstream forward yet
- P1: flushes to hub OTLP endpoint every 60s or 64 KB, whichever first; forces flush on `telemetry/error`

### 5.3 `mur-hub` — new product (P1)

Single binary, single Cargo crate. Dependencies: tokio, axum, sqlx (Postgres or SQLite), sled (optional for solo embedded), noise-protocol, prost (OTLP), serde, etc.

#### 5.3.1 Boot modes

```bash
mur-hub --solo [--listen 127.0.0.1:4939] [--data-dir ~/.mur/hub]
mur-hub --team [--listen 0.0.0.0:4939] [--db postgres://...] [--cert ...] [--key ...]
mur-hub --cloud [--db postgres://...] [--blob-bucket s3://...] [--oidc-issuer https://...]
```

- `--solo`: SQLite at `<data-dir>/hub.db`; HTTP only on loopback; no TLS; no auth (assumes same user). ~10 MB RSS footprint.
- `--team`: Postgres; mTLS or HTTPS; Bearer token auth per registered commander; no multi-tenant; suitable for small team self-host.
- `--cloud`: Postgres; HTTPS only; OIDC auth for operators; multi-tenant (tenant ID in JWT); S3 blob bucket for OTLP archive if configured.

#### 5.3.2 API surface

```
Registry
  POST   /v1/agents/register               — agent or commander publishes agent card
  PATCH  /v1/agents/{uuid}                  — update heartbeat / endpoints
  DELETE /v1/agents/{uuid}                  — unregister (on shutdown)
  GET    /v1/agents/{uuid}                  — resolve agent card
  GET    /v1/agents?tenant=...&tag=...      — list agents with filters
  GET    /v1/agents/{uuid}/endpoint         — lightweight endpoint resolution

Signaling
  WS     /v1/signal/{caller_uuid}/{target_uuid}   — bidirectional ICE-like exchange
                                                     (caller publishes candidates;
                                                      target subscribes + responds)

Relay fallback
  POST   /v1/relay/{session_id}/upload      — chunked upload of encrypted bytes
  GET    /v1/relay/{session_id}/download    — chunked download
  (session TTL 10 min, cap 100 MB, in-memory, never persisted to disk)

Observability
  POST   /v1/otlp/traces                    — OTLP/HTTP JSON trace ingest
  POST   /v1/otlp/logs                      — OTLP/HTTP JSON log ingest
  POST   /v1/otlp/metrics                   — OTLP/HTTP JSON metric ingest
  GET    /v1/telemetry/query?...            — query spans/logs (team/cloud mode)
  WS     /v1/telemetry/stream?...           — live tail (dashboard consumes)

Scheduler
  POST   /v1/schedules                      — register cron schedule for an agent
  DELETE /v1/schedules/{id}
  GET    /v1/schedules?agent_uuid=...
  (internally: cron evaluator spawns HTTP POST to commander/agent endpoint)

Admin
  GET    /v1/health                         — liveness + mode + version
  GET    /v1/metrics                        — Prometheus format (hub's own metrics)
```

#### 5.3.3 Directory schema

```sql
CREATE TABLE agents (
  uuid            TEXT PRIMARY KEY,              -- UUIDv7
  name            TEXT NOT NULL,
  pubkey          TEXT NOT NULL,                 -- multibase-encoded Ed25519
  owner_identity  TEXT,                          -- email or SSO subject
  tenant_id       TEXT,                          -- NULL in solo/team; UUID in cloud
  endpoints       JSONB NOT NULL,                -- ordered list of {transport, url, reachability}
  agent_card      JSONB NOT NULL,                -- full card cached
  deployment      JSONB,                         -- {type, region, environment}
  capabilities    TEXT[] NOT NULL DEFAULT '{}',
  tags            TEXT[] NOT NULL DEFAULT '{}',
  registered_at   TIMESTAMPTZ NOT NULL,
  last_heartbeat  TIMESTAMPTZ,
  health          TEXT NOT NULL DEFAULT 'unknown', -- unknown | healthy | stale | offline
  UNIQUE(tenant_id, name)
);
CREATE INDEX agents_pubkey_idx ON agents(pubkey);
CREATE INDEX agents_owner_idx ON agents(owner_identity);
CREATE INDEX agents_heartbeat_idx ON agents(last_heartbeat);
```

Health state machine:
- `unknown` on registration
- → `healthy` on first successful heartbeat
- → `stale` if no heartbeat for 3× heartbeat interval (default 30s × 3 = 90s)
- → `offline` if stale for another 5 minutes; row retained for 24 h then soft-deleted

### 5.4 `mur-server` — dashboard additions (P1)

Next.js app gains new routes consuming `mur-hub`:

```
/fleet                              — grid of all agents across hosts (filter by tenant/host/type)
/fleet/[uuid]                       — per-agent detail: card, tasks, telemetry timeline
/fleet/[uuid]/traces                — trace drill-down (task → LLM → tools → files → errors)
/fleet/[uuid]/files                 — file transfer history (sender/receiver/size/status)
/fleet/costs                        — token cost aggregation (group by agent/host/day)
/fleet/errors                       — error hotspot dashboard
/fleet/live                         — WebSocket-backed live telemetry tail
/fleet/schedules                    — scheduled tasks config + next-fire preview
```

All consume `mur-hub`'s REST API; no direct access to agents from dashboard.

---

## 6. Cryptography & Identity

### 6.1 Keypair generation

`mur agent create <name>` generates Ed25519 via `ed25519-dalek`:

```rust
let csprng = OsRng;
let signing_key = SigningKey::generate(&mut csprng);
let verifying_key = signing_key.verifying_key();

// Write private key, mode 0600
std::fs::write(agent_home.join("identity.key"), signing_key.to_bytes())?;
#[cfg(unix)]
set_permissions(&agent_home.join("identity.key"), Permissions::from_mode(0o600))?;

// Write public key, multibase-encoded (base58btc with `z` prefix) for text compat
let pubkey_text = multibase::encode(Base::Base58Btc, verifying_key.to_bytes());
std::fs::write(agent_home.join("identity.pub"), pubkey_text)?;
```

### 6.2 Noise XK handshake

Pattern: `Noise_XK_25519_ChaChaPoly_BLAKE2s`

- **K** in the second position = receiver's static key is known to initiator a priori. This is the hub-resolved pubkey.
- **X** in the first position = initiator sends their static key encrypted to the responder.
- Result: both sides learn each other's pubkey + derive shared symmetric keys.

Flow:

1. Caller resolves target UUID via hub → gets pubkey + endpoint list
2. Caller dials endpoint → opens TCP
3. Noise XK: `-> e, es` (initiator ephemeral + DH with responder static), `<- e, ee` (responder ephemeral + DH), `-> s, se` (initiator static encrypted; proves identity)
4. If responder static doesn't match hub-advertised pubkey → abort (MITM defense)
5. Caller signs opening payload with its identity key; responder verifies against its `accepts_from` allowlist (keyed by pubkey or UUID)
6. Post-handshake ChaCha20-Poly1305 AEAD for all subsequent frames

Conversion from Ed25519 signing keys to X25519 DH keys uses `ed25519-dalek`'s `to_montgomery()`.

### 6.3 Hub-relayed content is encrypted

When P2P fails and the sender falls back to hub relay:

1. Sender completes Noise handshake with receiver through the hub's relay session (opaque byte pipe)
2. All subsequent payloads are AEAD-encrypted end-to-end
3. Hub sees: session ID, sender/receiver UUIDs (from registration), timestamps, byte counts
4. Hub does NOT see: any plaintext JSON-RPC content, file content, LLM prompts/responses

This is consistent with Tailscale DERP behavior.

---

## 7. File Transfer Protocol

### 7.1 Size tiers

| Size | FilePart form | Transport |
|---|---|---|
| < 256 KB | `{ bytes: "<base64>" }` | Inline in A2A message |
| 256 KB – 1 GB | `{ file_id: "f-01JRZ...", size, sha256 }` | Chunked via `files/fetch` RPC over P2P Noise tunnel |
| > 1 GB | same as above | Resumable via `offset` + recommended chunk size 1 MB |
| Any (P2P fail) | same | Hub relay live pipe (10 min / 100 MB cap) |
| Any (async) | `{ uri: "https://s3..." }` | External object store (optional P2+ feature) |

### 7.2 `files/fetch` RPC (new in P1)

```json
// Request
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "files/fetch",
  "params": {
    "file_id": "f-01JRZ...",
    "offset": 0,
    "length": 1048576
  }
}

// Response (continuation)
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "bytes": "<base64>",
    "eof": false,
    "next_offset": 1048576,
    "sha256_partial": "..."  // optional, running hash for early-abort detection
  }
}

// Final response
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "bytes": "<base64>",
    "eof": true,
    "sha256_complete": "..."
  }
}
```

### 7.3 Sender outbox

On `message/send` with a `file_id` FilePart, sender stages the file at `~/.mur/agents/<name>/outbox/<file_id>` (symlink, not copy) with a metadata sidecar `<file_id>.meta.json`:

```json
{
  "file_id": "f-01JRZ...",
  "source_path": "/Users/david/Downloads/report.pdf",
  "size": 8388608,
  "sha256": "...",
  "created_at": "2026-04-23T10:00:00Z",
  "ttl_seconds": 3600,
  "expires_at": "2026-04-23T11:00:00Z",
  "recipient_uuid": "01JRZ..."
}
```

A background sweeper removes entries past `expires_at`. Fetches after expiry return `-32020: file expired`.

### 7.4 Receiver approval threshold

Profile gains:

```yaml
file_transfer:
  accept_incoming_file_max_bytes: 10_485_760   # 10 MB — auto-accept below this
  accept_incoming_total_per_hour: 104_857_600  # 100 MB/hour throttle
  require_approval_above_bytes: 10_485_760     # above this triggers RunState::AwaitingApproval
  reject_paths: ["~/.ssh", "~/.aws", "~/.gnupg"]
  allowed_mime_types: ["*"]                    # future filter
```

When a file exceeds threshold, task state transitions to `awaiting_approval`; RunState is serialized; resumable via `mur agent approve <task_id>` from CLI or via dashboard action. (This ties back to the OpenAI Agents SDK-inspired resumable approval pattern recommended in prior comparative analysis.)

### 7.5 Integrity

SHA-256 is mandatory in FilePart metadata. Receiver recomputes on completion; mismatch → discard + `telemetry/error { kind: FileHashMismatch }`.

### 7.6 Telemetry

Each transfer emits a `telemetry/file_transfer` notification:

```json
{
  "jsonrpc": "2.0",
  "method": "telemetry/file_transfer",
  "params": {
    "file_id": "f-01JRZ...",
    "sender_uuid": "01JQX4...",
    "receiver_uuid": "01JRZ...",
    "size_bytes": 8388608,
    "sha256": "...",
    "via": "p2p",                             // p2p | relay | s3
    "duration_ms": 1820,
    "status": "completed",                    // completed | rejected | expired | hash_mismatch
    "dest_path": "/Users/david/Desktop/report.pdf",
    "ts": "2026-04-23T10:00:05Z"
  }
}
```

---

## 8. Observability Pipeline

### 8.1 Two-tier flow

```
agent emits telemetry/* JSON-RPC notifications (P0a today)
    │
    ▼
~/.mur/agents/<name>/telemetry/YYYY-MM-DD.jsonl
    │
    ▼ (commander collector tails via inotify/FSEvents)
    │
commander normalizer: JSON-RPC payload → OTel spans/logs/metrics
    │
    ▼
commander redaction filter (full / redacted / metadata-only)
    │
    ▼
commander buffer: ~/.mur/commander/telemetry-spool/ (disk-backed ring)
    │
    ▼ (OTLP/HTTP POST every 60s or 64KB, whichever first;
    │   errors trigger immediate flush)
    │
    ▼
mur-hub /v1/otlp/* endpoints
    │
    ├── (solo/team mode) store in SQLite/Postgres for local query
    │
    └── (cloud mode) optional proxy to external OTel backend
        (Tempo / Loki / Honeycomb / Datadog / Grafana Cloud)
```

### 8.2 Mapping JSON-RPC → OTel

| murmur notification | OTel artifact |
|---|---|
| `telemetry/llm_call` | `gen_ai.chat_completion` span with `gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, latency_ms |
| `telemetry/tool_call` | `gen_ai.tool_call` span with tool name, mcp_server, duration_ms, ok |
| `telemetry/error` | OTel log record with severity=ERROR attached to active span |
| `telemetry/warning` | OTel log record with severity=WARN |
| `telemetry/heartbeat` | OTel metric `mur.agent.heartbeat` counter |
| `task/progress` (notification) | Span event on the `agent.task` root span |
| `telemetry/file_transfer` | Custom span `mur.file_transfer` with attributes |
| Task lifecycle (submitted → working → …) | Root span `agent.task` with status code |

Trace ID propagation: commander assigns root trace_id per task; agent inherits via `task/progress` correlation; cross-agent A2A calls propagate traceparent header in the JSON-RPC extension field.

### 8.3 Redaction

| Field | `full` | `redacted` | `metadata-only` |
|---|---|---|---|
| LLM prompt | Kept | SHA-256 + size | Stripped |
| LLM response | Kept | SHA-256 + size | Stripped |
| Tool args | Kept | Stripped if matches PII regex; else kept | Stripped |
| Tool result | Kept | Kept (structure); content → SHA-256 | Stripped |
| File content | Never shipped (only metadata) | Same | Same |
| Error messages | Kept | Kept | First 100 chars |
| Token counts / latency | Kept | Kept | Kept |
| Span names / timings | Kept | Kept | Kept |

### 8.4 Retention

| Hub mode | Default retention | Storage tier |
|---|---|---|
| `solo` | 7 days | SQLite; auto-vacuum at startup |
| `team` | 30 days | Postgres; `TIMESTAMPTZ` pruning via nightly job |
| `cloud` | 90 days hot, then S3 glacier | Postgres + S3 tiering |

Configurable via `hub.yaml`:

```yaml
telemetry:
  retention_days: 30
  archive_bucket: null           # null → delete after retention; path → move to archive
  archive_tier: glacier
```

### 8.5 Sampling

Commander collector supports head-based sampling to protect high-volume agents from saturating hub:

```yaml
telemetry:
  sample_rate: 1.0                              # 0.0 — 1.0
  # errors + warnings are NEVER sampled out (always 100%)
```

Trace-complete sampling: collector holds partial trace spans until root `agent.task` span ends; then decides (probabilistic) whether to ship whole trace or drop. Avoids orphan spans.

---

## 9. Cloud Deployment Archetypes

### 9.1 Archetype 1 — Hub-in-cloud + on-prem agents

Hub runs on public HTTPS (GCP Cloud Run, AWS ECS, EC2, or any Linux VM). Commanders on laptops/home servers register local agents with the hub. No cloud-hosted agents.

Suitable for: teams wanting centralized observability + cross-NAT reachability without moving agent workloads.

### 9.2 Archetype 2 — Docker container agent (primary cloud MVP)

```bash
docker run -d \
  --name mur_agent_researcher \
  -v agent_researcher_home:/data \
  -e MUR_HUB_URL=https://hub.example.com \
  -e MUR_AGENT_NAME=researcher \
  -e MUR_AGENT_IDENTITY_PRIV=/run/secrets/identity.key \
  --secret identity.key \
  mur/agent-runtime:2.3
```

Image contains:
- `/usr/local/bin/mur-agent-runtime` (single binary)
- No baked-in profile (mounted at runtime)
- Entry: runtime auto-detects `MUR_AGENT_NAME` and loads `/data/profile.yaml`

`mur agent export agent_a --format=docker` produces:
- `Dockerfile` (FROM scratch or alpine + the runtime binary from latest release)
- `docker-compose.yml`
- `agent_a.profile.yaml` + `agent_a.sys_prompt.md` + `agent_a.skills/*` + `agent_a.identity.pub` (NOT the private key)
- `README.md` explaining how to supply the private key via Docker secret or env var

Compatible with: Cloud Run (with `--always-on`), ECS Fargate, ECS on EC2, GCE COS, any Docker-host VM.

### 9.3 Archetype 3 — K8s StatefulSet (raw YAML in P1; Operator in P2)

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: mur-agent-researcher
spec:
  replicas: 1                                   # always 1 for identity stickiness
  serviceName: mur-agent-researcher
  selector:
    matchLabels:
      app: mur-agent
      name: researcher
  template:
    metadata:
      labels:
        app: mur-agent
        name: researcher
    spec:
      containers:
        - name: runtime
          image: mur/agent-runtime:2.3
          env:
            - name: MUR_HUB_URL
              value: "https://hub.example.com"
            - name: MUR_AGENT_NAME
              value: "researcher"
          envFrom:
            - secretRef:
                name: mur-agent-researcher-identity
          volumeMounts:
            - name: home
              mountPath: /data
          resources:
            limits:
              memory: 512Mi
              cpu: 500m
          securityContext:
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            capabilities:
              drop: ["ALL"]
  volumeClaimTemplates:
    - metadata: {name: home}
      spec:
        accessModes: ["ReadWriteOnce"]
        resources:
          requests:
            storage: 1Gi
---
apiVersion: v1
kind: Service
metadata:
  name: mur-agent-researcher
spec:
  selector:
    app: mur-agent
    name: researcher
  ports:
    - port: 39393
      targetPort: 39393
      protocol: TCP
      name: a2a-tcp
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: mur-agent-researcher
spec:
  podSelector:
    matchLabels:
      app: mur-agent
      name: researcher
  policyTypes: ["Egress"]
  egress:
    - to:
        - namespaceSelector: {matchLabels: {name: mur-hub}}
    - to:
        - ipBlock: {cidr: 0.0.0.0/0}
      ports:
        - port: 443                             # hub + allowed hosts
```

`mur agent export agent_a --format=k8s` emits this bundle.

### 9.4 Archetype 4 — Serverless scheduled agent

For `lifecycle.execution: on_demand` agents:

```yaml
# profile.yaml
lifecycle:
  execution: on_demand                          # agent exits after task completes
  schedule:
    - cron: "0 9 * * 1-5"                       # weekdays 9am
      message: "daily standup summary"
      sends_to: notify_a
    - cron: "0 18 * * *"
      message: "EOD report"
```

Cloud Run Job target:
- `mur agent export agent_a --format=cloudrun` produces a Cloud Run Job YAML + IAM binding + Cloud Scheduler config
- Hub's scheduler handles the cron evaluation; when cron fires, hub issues HTTP POST to Cloud Run Job trigger → Cloud Run pulls image + starts container with `MUR_ONESHOT_MESSAGE` env var → agent runs single task → exits
- Identity keypair stored in Secret Manager; same across cold starts → UUID stable

Lambda target: similar, with `--format=lambda` emitting an AWS SAM or CDK template.

### 9.5 Archetype cross-reference

| Archetype | Long-lived? | Cold-start? | Best for | P0a.5 ready? | P1 ready? |
|---|---|---|---|---|---|
| 1. Hub-in-cloud + on-prem | Agents yes | — | Personal + team sharing | Partial (TCP/identity needed) | Full |
| 2. Docker container | Yes | No | Solo/team cloud workloads | No | Full |
| 3. K8s StatefulSet | Yes | No | Enterprise platform teams | No | Raw YAML; operator P2 |
| 4. Serverless scheduled | No | Yes | Daily reports, event-driven | No | Full |

---

## 10. Commander Integration Contract (Option D detail)

### 10.1 No rename

`mur-commander` remains the product name and keeps all v0.7 features: Slack/TG/Discord/Jira/workflow engine/sub-agents/safety constitution/audit/plugins/Docker distribution.

### 10.2 Commander registers as an A2A peer itself

Commander's existing A2A server at `http://localhost:3939` continues to operate. Commander registers itself with hub as an agent with capabilities `["a2a.message.send", "a2a.tasks", "commander.workflow", "commander.chat"]`. This makes commander discoverable to P0a agents (e.g., an agent can POST a workflow request to commander via A2A, routing through its chat gateway).

### 10.3 Commander acts as per-host relay gateway

All host-local P0a agents register with commander first; commander aggregates and forwards to hub. From hub's perspective, each host has "one commander" that owns N agents; this is the unit of accounting and the unit of configuration (hub URL, telemetry mode, tenant ID).

### 10.4 Commander's SSH backbone preserved

`crates/engine/src/remote/{ssh,relay}.rs` continues to work as a fallback path when hub is not configured. The `mur.run` relay endpoint becomes a pre-built hub instance operators can point to (vs. self-host).

### 10.5 `commander_bridge.rs` in mur-core unchanged

`mur-core/src/evolve/commander_bridge.rs` (pattern → workflow promotion) is untouched. This integration point is orthogonal to the fleet architecture.

### 10.6 In-process sub-agent vs. P0a agent distinction

Commander sub-agents (async tasks, ≤5 concurrent, shared process space) remain for lightweight workflow step parallelism. P0a agents (separate OS processes, identity-bearing, crash-isolated) remain for long-lived autonomous behavior. **Both coexist.** Recommendations for user choice:

- Use sub-agents when: ephemeral, fan-out within one workflow step, no need for permanent identity or cross-session memory
- Use P0a agents when: long-lived, need sandbox, need cross-host discoverability, need chat/Jira addressability

---

## 11. Roadmap

### 11.1 P0a.5 — Transport + identity + integration (2-3 weeks after P0a merges)

**Tasks:**

1. Ed25519 keypair generation in `mur agent create` + profile schema update
2. Multibase pubkey encoding / decoding library wrap in `mur-common`
3. TCP listener with Noise XK handshake in `mur-agent-runtime`
4. Agent Card extension: `pubkey`, `endpoints[]`, `deployment`
5. Commander A2A method aliasing (`message/*` → existing `tasks/*` handlers)
6. Commander `crates/engine/src/remote/murmur_bridge.rs` — running.lock watcher + auto-register
7. Commander collector subsystem (stub: tails JSONL, normalizes, buffers; no upstream yet)
8. Profile schema stabilization (add `lifecycle.{execution,schedule}`, `identity.{uuid,pubkey,owner}`, `transport.tcp`, `file_transfer.*`)

**Deliverable:** P0a agents can register with commander, commander has registry + v0.3 aliasing, telemetry is being spooled (waiting for hub).

### 11.2 P1 — Hub + cloud export + P0b enforcement (8-10 weeks)

**Tasks:**

1. `mur-hub` crate scaffold + 3-mode boot
2. Directory API + Postgres/SQLite schema
3. Signaling WebSocket + ICE-like payload exchange
4. Relay fallback (in-memory ring buffer, 10 min / 100 MB)
5. OTLP ingest endpoints (traces/logs/metrics) + storage adapter
6. Scheduler: cron evaluator + on_demand HTTP invocation
7. `mur agent export --format=docker` + docker image build pipeline
8. `mur agent export --format=k8s` + manifest generator
9. `mur agent export --format=cloudrun` + GCP integration
10. `mur agent export --format=lambda` + AWS SAM template
11. `files/fetch` JSON-RPC method + sender outbox + receiver approval
12. `profile.lifecycle.schedule[]` + `execution: on_demand` runtime path
13. P0b entitlement enforcement: host sandbox (sandbox-exec / bwrap) + container manifest translation
14. Commander collector OTLP forwarder → hub (replaces stub from P0a.5)
15. `mur-server` dashboard `/fleet/*` routes + `/api/hub/*` endpoints
16. E2E test fleet: 3 hosts × 5 agents + 1 hub + integration harness

**Deliverable:** Self-hostable fleet control plane; cloud agent archetypes working end-to-end; full P0b enforcement.

### 11.3 P2+ — Enterprise + cloud multi-tenant (3-4 weeks each)

- Hub `--cloud` mode: multi-tenant, OIDC auth, S3 blob archive, public HTTPS with cert-manager
- K8s Operator (`murmur-operator`) + `MurmurAgent` CRD
- External OTLP backend proxies (Tempo, Honeycomb, Datadog, Grafana Cloud) — tested integrations
- S3-backed file transfer backend (asynchronous recipient scenario)
- Hub metrics dashboard (hub's own health: agent count, OTLP ingest rate, relay utilization)
- Audit logging (who created agent X, who modified entitlements, who triggered scheduler manually)

---

## 12. Rejected Alternatives

| # | Rejected | Reason |
|---|---|---|
| R1 | Rename `mur-commander` to reclaim "commander" for fleet control plane; move pattern→workflow to mur-core | v0.7 has 10 sub-crates + Slack/TG/Discord/Jira users + Docker Hub distribution; rescoping would destroy product value. Naming decision flipped once during brainstorming after discovering true commander scope. |
| R2 | Single new product replacing both commander and hub | Commander's workflow+chat+SSH backbone and hub's directory+signaling+relay are different jobs with different lifecycles; combining them creates a monolith. |
| R3 | Hub-centric: all traffic through hub (no P2P) | Violates privacy-by-design (hub sees content); scales poorly; latency penalty; industry precedent (Tailscale, Zerotier, WebRTC) universally uses P2P with relay fallback. |
| R4 | SSH-only backbone; no hub | No NAT traversal; fails on cloud agents behind load balancers; no UUID directory; fails anchors #3, #4, #5. |
| R5 | Gossip mesh (Consul / etcd-style) | Overkill for personal + small-team scale; consensus protocols add weeks of engineering for negligible benefit; user's scale does not need quorum. |
| R6 | Hub as mailbox (persist files until recipient comes online) | Scope creep into storage product; privacy risk; storage cost; not core to fleet control. Async scenarios handled by optional external S3 backend (P2+). |
| R7 | Agent direct-to-hub telemetry (no commander collector) | Loses per-host buffering, loses centralized redaction, loses crash-safety of in-flight data. |
| R8 | L3 commander-in-cloud as regional gateway | Multi-hub deployment (one per region) solves the same problem without a new product layer. |
| R9 | Agent autoscaling (replicate one agent across pods) | Agents are identity-bearing; scaling up = creating new agents with distinct identities, not replicating. StatefulSet replicas=1 is correct. |
| R10 | Bearer token auth for cross-host A2A (original P0b plan) | Noise XK with hub-validated pubkeys is stronger (mutual authentication + forward secrecy) and simpler (no token rotation management). |
| R11 | Per-agent Docker image (bake profile into each image) | Inflates storage; requires rebuild on profile change. Unified runtime image + mounted profile is more operable. |
| R12 | K8s Operator in P1 | YAGNI for P1 users; raw StatefulSet YAML works; operator is a P2 convenience. |

---

## 13. Open Questions (to resolve before P0a.5 implementation plan)

| # | Question | Proposed default | Needs user decision? |
|---|---|---|---|
| Q-A | Should `mur-hub --solo` bind to `127.0.0.1:4939` only, or offer auto-LAN exposure via mDNS? | 127.0.0.1 only; LAN via explicit `--team` | No; default fine |
| Q-B | Should identity keypair be re-generatable on demand (`mur agent rekey`)? | Yes, with re-registration to hub; old UUID retained but new pubkey. | Yes |
| Q-C | Do we ship `mur-hub` binary via Homebrew + Docker Hub simultaneously or stagger? | Both at P1 release | No |
| Q-D | Policy store (P0b's `accepts_from` / vault) — live in hub or per-commander? | Per-commander (receiver authoritative); hub holds advisory copy only | Yes |
| Q-E | Should `mur agent export --format=docker` produce a FROM-scratch image or `alpine:latest`? | FROM scratch for smaller image; no shell for debugging | Yes |
| Q-F | Scheduler cron syntax: standard 5-field cron or extended? | 5-field standard + optional second field; human-readable names like "every weekday at 9am" via NL parser (already in commander) | No |
| Q-G | Commander telemetry spool size cap | 100 MB default; warn at 80%, drop oldest past cap | No |
| Q-H | Hub relay fallback — can it be disabled entirely for paranoid users? | Yes, per-tenant config flag | No |
| Q-I | Dashboard WebSocket authentication | Reuse mur-server session cookie | No; existing pattern |
| Q-J | Should commander bridge watch `~/.mur/agents/*/` or rely on explicit `mur agent attach` call? | Watch automatically; user can disable via config flag | No |
| Q-K | Agent ↔ hub authentication: Ed25519-signed request body, mTLS, or both? | Ed25519-signed body only (simpler operator experience; mTLS optional via proxy) | Yes |
| Q-L | Signaling protocol shape: pure WebRTC ICE candidates, or a simpler murmur-specific exchange (endpoints[] + nonce + Noise handshake hints)? | Simpler murmur-specific: hub brokers the endpoint lists + handshake nonce; no STUN/TURN server role | No; default fine |

---

## 14. Appendix

### 14.1 LOC estimate breakdown

| Phase | Component | Rust LOC | TypeScript LOC |
|---|---|---|---|
| P0a.5 | Identity + keypair gen in core | 300 | — |
| P0a.5 | TCP transport + Noise handshake in runtime | 500 | — |
| P0a.5 | Agent Card schema + serialization | 200 | — |
| P0a.5 | Commander A2A aliasing | 150 | — |
| P0a.5 | Commander murmur_bridge auto-register | 400 | — |
| P0a.5 | Commander collector stub | 450 | — |
| **P0a.5 total** | | **~2 000** | **—** |
| P1 | mur-hub scaffold + 3 modes | 800 | — |
| P1 | Directory API + schema | 700 | — |
| P1 | Signaling WS | 500 | — |
| P1 | Relay fallback | 400 | — |
| P1 | OTLP ingest + storage | 900 | — |
| P1 | Scheduler | 600 | — |
| P1 | Export --format=docker | 400 | — |
| P1 | Export --format=k8s | 500 | — |
| P1 | Export --format=cloudrun | 400 | — |
| P1 | Export --format=lambda | 400 | — |
| P1 | files/fetch + outbox + approval | 800 | — |
| P1 | P0b entitlement enforcement | 1 500 | — |
| P1 | Commander collector→hub forwarder | 400 | — |
| P1 | mur-server dashboard /fleet + /api/hub | — | 2 500 |
| **P1 total** | | **~8 300** | **~2 500** |

### 14.2 Migration notes

P0a merge does not break anything; P0a agents keep running under filesystem registry.

P0a.5 adds new fields to profile.yaml with defaults that preserve old behavior:
- `identity.pubkey`: auto-generated if missing on next `start`
- `transport.tcp.enabled`: defaults `false` — no behavior change unless user opts in
- `lifecycle.execution`: defaults `daemon` — no change

P1 introduces `mur-hub` as a new binary; absence of `MUR_HUB_URL` env var means commander operates in legacy standalone mode (no cross-host routing; no fleet observability).

### 14.3 Relationship to prior comparative analysis

Several decisions in this spec are informed by comparative research with Claude Managed Agents, Claude Agent SDK, OpenAI Agent Builder, and OpenAI Agents SDK (documented in a prior conversation). Specific borrowings:

- **Tool approval resumable RunState** (OpenAI Agents SDK) → file transfer above-threshold pause-and-resume pattern (§7.4)
- **Three-outcome guardrails** (OpenAI Agents SDK) → three redaction modes (§8.3), though applied to telemetry rather than tool execution
- **Credential vault / Connector Registry** (Anthropic / OpenAI) → hub-held advisory policy + per-commander authoritative allowlists (Q-D open question)
- **Session state carryover + hub-assigned thread_id** (Anthropic Managed Agents) → potential P2 extension; not in P1 scope
- **Encrypted state for ZDR** (OpenAI Responses API `encrypted_content`) → `metadata-only` redaction mode is the lightweight version; full encrypted-state is potential P2+
- **DAG-as-JSON with code export** (OpenAI Agent Builder) → pattern to consider for future P3 agent workflow spec; not in this document
- **18-hook extensibility** (Claude Agent SDK) → potential P2 profile `hooks: {pre_tool_call, post_task, ...}` extension; not in P1

### 14.4 Glossary

- **Agent**: a P0a-style process running `mur-agent-runtime` with a profile; has UUID + pubkey identity.
- **Commander**: `mur-commander` v0.7+ daemon; workflow engine + chat gateway + collector.
- **Hub**: `mur-hub` — new fleet control plane product.
- **A2A**: Google Agent-to-Agent protocol v0.3; JSON-RPC 2.0 over HTTP (and stdio / Unix socket / TCP+Noise in murmur extensions).
- **Noise XK**: `Noise_XK_25519_ChaChaPoly_BLAKE2s` handshake pattern; initiator knows responder's static key a priori.
- **OTLP**: OpenTelemetry Protocol; used for trace/log/metric ingest.
- **Tenant**: isolation boundary in hub cloud mode; typically an organization.
- **Deployment archetype**: one of four cloud deploy shapes (hub-in-cloud + on-prem / Docker / K8s / serverless).
