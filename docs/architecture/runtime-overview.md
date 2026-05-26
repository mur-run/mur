# Runtime Overview — Detailed Reference

This document holds detail moved out of `CLAUDE.md` to keep the per-session context lean. CLAUDE.md retains the operational pointers; everything below is reference material.

> Last consolidated: 2026-05-08.

---

## Other CLI Modules

- **`verify.rs`** — Documentation verification engine: parses claims (file paths, CLI commands, code refs) from Markdown and checks them against the project. Known commands are auto-derived from the clap command tree at runtime.
- **`server.rs`** — Axum-based local API server (Phase 0 feature)
- **`community.rs`** — Community pattern browser
- **`dashboard.rs`** — Terminal overview
- **`interactive.rs`** — `dialoguer`-powered interactive pattern creation
- **`migrate/`** — legacy schema migration (rarely needed)
- **`auth.rs`** — Trust levels for community patterns

## Sources Pipeline (P1.4)

All adapters shipped: Obsidian + Notion + Joplin; `--watch` + `install-schedule`; `format_notes_section` helper ready.

An alternate input to `store/` lives in `mur-core/src/sources/` — `KnowledgeSource` adapters pull documents from external note apps into the same retrieve pipeline as patterns. The vector store is abstracted behind `store::vector::VectorStore` (impls: `LanceDbStore` now; `QdrantStore` P1.3).

- Spec: `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md`

## CLI Subcommand Detail

### `mur agent <subcommand>` — full surface

Subcommands: `create`, `list`, `status`, `stop`, `remove`, `rename`, `send`, `card`, `install-service`, `prompt {show|edit|set}`, `mcp {add|list|remove|rename}`, `skill {add|list|remove|show}`, `perm {show|set-mode|allow-host|deny-host|list-hosts|allow-read|allow-write|deny-path|allow-spawn|deny-spawn|set-limit}`, `secret {set|list|delete}`, `export {--format=pkg|bin|gui}`, `doctor [--format ...]`, `stats`, `logs`. The runtime binary that backs each agent lives in `mur-agent-runtime/`.

### `mur agent doctor [--format pkg|bin|gui|all] [--json]`

Pre-flight prereq check for export targets (no build, just diagnostics). Same logic is reused as the fail-fast step inside `--format gui` export pipeline.

### `mur model {add|list|show|remove|migrate}`

Manage `~/.mur/models.yaml` (provider/model/secret-ref registry). `migrate` walks existing agent profiles and synthesizes registry entries idempotently. Agents opt in via `profile.yaml`'s `model_ref:` field; supervisor resolves `model_ref → registry → SecretRef`, falling back to the legacy inline `model:` block. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`.

### `mur agent secret {set|list|delete}`

OS-keychain CRUD for an agent's secrets (account format `{agent}/{KEY}`, service `mur-agent`). `set` uses a hidden prompt via `rpassword` when no inline value is given.

### `mur agent companion <subcommand>`

Manage the companion subsystem for an agent (Phase 1.1):

- `companion init <name> [--answers <yaml>] [--re-init]` — run the onboarding wizard or re-initialise voice config
- `companion proactive enable|disable <name>` — opt in or out of proactive sends
- `companion quiet <name> --for <duration>|--until <rfc3339>|--off` — pause proactive sends
- `companion voice <name> eject|rebuild|diff` — manage the composed `voice.md` file on disk
- `companion templates eject [--scope agent|user] [<rel>.<locale>]` — eject embedded voice templates for editing
- `companion content add <name> <situation> [--from-stdin|--file]` — add content-pool entries
- `companion inbox <name> [--unread-only]` — list messages in the inbox
- `companion ack <name> <msg-id> --good|--bad|--dismiss` — rate or dismiss a message
- `companion preview <name> --situation <s> [--no-llm]` — preview a generated message
- `companion why-did-you-message <name> [<msg-id>]` — replay the event chain that triggered a message
- `companion rhythm wipe <name> [--yes]` — reset companion state (preserves voice config)

---

## GUI Export (`mur agent export --format gui`)

The third format. Produces a click-to-launch desktop app (`MyAgent.app` / `MyAgent.AppImage` / `MyAgent.exe`) bundling a single agent. Built on Tauri 2 + React 18 + Vite + Tailwind 4. The `mur-agent-gui` crate is **workspace-EXCLUDED** in the root `Cargo.toml` so default `cargo build --workspace` doesn't pull WebKitGTK / Cocoa / WebView2 toolchains.

- **`mur-agent-gui/`** — Tauri 2 main + React frontend. Sidecar manager (`src/sidecar.rs`), bootstrap (`src/bootstrap.rs`), theme loader (`src/theme.rs`), Tauri commands (`src/commands.rs`) that wrap `mur_core::agent_admin::*`. 5 built-in themes (light / dark / high-contrast / solarized / cyberpunk) with WCAG AA contrast validation enforced at build time.
- **`mur-core/src/agent_admin/`** — Public library façade over the existing CLI verbs. `perm`, `mcp`, `skill`, `prompt`, `lifecycle`, `observability` modules each expose mutators (delegating to `cmd::agent::cmd_*`) plus typed read views (StatusView / Entitlements / Vec<McpServerEntry> / etc.) for callers that need structured data instead of stdout.
- **`mur-core/src/cmd/agent_export_gui.rs`** — 13-phase pipeline: prereq_check → prepare_payload → prepare_theme → rewrite_tauri_conf → build_sidecar → build_frontend → tauri_build → codesign → notarize → staple → assess → package → move_to_out. RAII guard restores `tauri.conf.json` on any exit path.
- **CLI flags:** `--theme {light|dark|high-contrast|solarized|cyberpunk}`, `--icon /path/to.png`, `--clone-identity` (embed identity, recipient rekeys on launch; default = template mode mints fresh keys), `--skip-notarize` (testing only).
- **Spec:** `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan.md` + `-COMPLETE.md`
- **Cookbook:** `docs/cookbook/multi-platform-export.md`, `docs/cookbook/harness-office-12-gui-export.md`
- **E2E runner:** `scripts/e2e/p1-export-gui.sh` (quick or `FULL_E2E=1` mode)
- **CI matrix template:** `scripts/templates/agent-export-multi-platform.yml`

When iterating on the GUI shell:

```bash
cargo install tauri-cli --version '^2.0' --locked     # one-time
cd mur-agent-gui/ui && npm ci
cd mur-agent-gui/src-tauri && cargo tauri dev          # 6-tab settings window opens
```

`mur agent doctor --format gui` lists every prereq.

---

## Agent Runtime (murmur P0a)

The per-agent supervisor lives in `mur-agent-runtime/`. Each agent has a directory under `~/.mur/agents/<name>/` (`profile.yaml`, `sys_prompt.md`, `skills/`, `running.lock`, `telemetry/<date>.jsonl`) and a symlink in `MUR_AGENT_BIN_DIR` (default `~/.local/bin`) named `mur_agent_<name>`. The symlink is the runtime binary; argv[0] tells it which profile to load.

- **Spec:** `docs/superpowers/specs/2026-04-22-murmur-p0a-agent-runtime-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-22-murmur-p0a-agent-runtime-plan.md` (+ `-part2.md`, `-COMPLETE.md`, `-e2e-coverage.md`)
- **Crate README:** `mur-agent-runtime/README.md`
- **E2E runner:** `scripts/e2e/run-all.sh`

`entitlements.llm.mode` is `allowed` by default; bridges set it to `off` so the supervisor refuses to construct an LLM client. See `docs/cookbook/c1-a2a-bridge.md`.

### P0a.5 additions (identity + TCP Noise + commander integration)

- **`mur-common` schema** — `AgentProfile.identity` (Ed25519 pubkey + owner), `transport.tcp` (Noise XK bind + pattern, default off), `lifecycle.{execution,schedule}` (daemon vs on-demand + cron), `file_transfer` caps, `deployment` (laptop / vm / docker / k8s / lambda).
- **`mur-agent-runtime` TCP transport** — `transport/tcp.rs` + `transport/noise.rs`: Noise_XK_25519_ChaChaPoly_BLAKE2s handshake over length-prefixed 4-byte BE JSON-RPC frames (16 MiB cap). Supervisor spawns the listener only when `transport.tcp.enabled && entitlements.network.inbound.ports` contains the bind port.
- **Agent Card extensions** — now publishes `pubkey`, `endpoints[]` (ordered `tcp+noise` → `unix-socket` → `stdio`, each with `transport`/`url`/`reachability`), and `deployment`.
- **`mur agent create`** — generates `identity.key` (0600) and `identity.pub` (multibase base58btc, `z…` prefix) under the agent directory and writes the pubkey into `profile.yaml`.
- **mur-commander integration (cross-repo; see `~/Projects/mur-commander` branch `feat/murmur-bridge`):**
  - `engine::a2a::protocol` — adds `message/send`, `message/stream`, `tasks/list` constants + v0.3 capability extension tags (`a2a.v0.3`, `a2a.message.send`, …) on the commander's own Agent Card.
  - `engine::a2a::server` — aliases `message/send`→`tasks/send`, `message/stream`→`tasks/send`, plus a new `tasks/list` handler.
  - `engine::remote::murmur_bridge` — notify-based watcher on `~/.mur/agents/*/running.lock`; upserts/marks-offline `RegisteredAgent` entries tagged `"murmur"`. Daemon boots it alongside the existing service manager.
  - `engine::observability` — `redaction` (full / redacted / metadata-only), `spool` (disk-backed JSONL with rollover), `collector` (tails `telemetry/*.jsonl`, filters telemetry/task-progress notifications, redacts, spools). No hub upstream yet — that lands in P1.
- **Plan:** `docs/superpowers/plans/2026-04-23-murmur-p0a5-implementation-plan.md` (+ `-COMPLETE.md`)
- **E2E runner:** `scripts/e2e/p0a5-full.sh` (wraps `p0a5-identity-handshake.sh` + `p0a5-commander-autoregister.sh`).

### P0a.6 additions (`mur agent rekey` — identity rotation)

Per-agent Ed25519 identity keys can now be rotated via:

- **`mur agent rekey <name> [--reason scheduled|suspect-compromise|owner-change] [--yes]`** — normal rotation. Generates a new keypair, signs a `RotationAttestation` with the OLD private key, atomically rotates `identity.{key,pub}` to `.prev`, writes new keypair, appends to `rotations.jsonl`, updates `profile.yaml` (`key_version++`, `previous_pubkey`, `grace_expires_at = now + 30d`), SIGTERMs the running runtime so the symlink supervisor restarts it.
- **`mur agent rekey <name> --emergency`** — used when the old key is unrecoverable. Writes an UNSIGNED attestation; commander will quarantine the agent with `pending_emergency_approval` until an admin runs `murc agent approve-rekey <uuid>` on the commander host (option-a FS-gated).
- **`mur agent rekey-status <name> [--json]`** — show current/previous keys, grace remaining, rotation history count.

Identity schema (`mur-common::agent::IdentityConfig`) gained `algorithm`, `key_version`, `created_at_key`, `previous_pubkey`, `previous_key_version`, `grace_expires_at`, `rotated_at`, `emergency_rekey_at` — all `#[serde(default)]`. Bootstrap rotation entries are written into `rotations.jsonl` at agent create time.

`mur-common::identity` exports `RotationAttestation` (with sorted-key canonical JSON for signing) and `verify_chain` (M5.1) for end-to-end chain validation.

On every supervisor startup, an expired `grace_expires_at` triggers `shred -u identity.key.prev` + clears `previous_*` from `profile.yaml` (M6.1).

**mur-commander integration** (`feat/agent-rekey-commander`):

- `engine::a2a::discovery::AgentRegistry::apply_rotation` — verifies attestation signature against the OLD pubkey before advancing the registry; handles idempotent replay; quarantines on `OldKeyMismatch`; refuses emergency until approved.
- `approve_emergency_rotation` / `reject_emergency_rotation` — out-of-band gating for emergency rotations.
- `sweep_grace_expiries` — hourly daemon job clears expired `previous_pubkey` entries (M6.2).
- **Split-attestation detection** (M5.2) — when two diverging attestations claim the same `key_version` boundary with different `new_pubkey`, the agent is quarantined with a `split_attestation_vN_to_vN+1` marker.
- **`murc agent approve-rekey <uuid>` / `reject-rekey <uuid>`** (M4.2) — FS-gated CLI on the commander host.

**TcpConnector** (M3.2) — `dial_with_fallback(addr, identity, &[primary, prev])` lets peers retry a Noise handshake against either pubkey during the grace window. Agent Card (M3.1) publishes `previous_pubkey` + `grace_expires_at` while grace is active.

- **Spec:** `docs/superpowers/specs/2026-04-24-murmur-agent-rekey-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-24-murmur-agent-rekey-plan.md` (+ `-COMPLETE.md`)
- **PRs:** `mur-run/mur#30` (mur side: M1, M3, M4.1, M5.1, M6.1, M6.3) + `mur-run/mur-commander#12` (commander side: M2, M4.2/4.3, M5.2, M6.2)

---

## Companion subsystem (Phase 1.1)

The companion subsystem lives in `mur-agent-runtime/src/companion/`. It injects a relationship-keyed warm voice into the agent's `sys_prompt` and drives an opt-in proactive outbox. All state is stored under `~/.mur/agents/<name>/companion/` (`state.yaml`, `inbox/`, `content/`, `telemetry/`). The voice template chain is: embedded templates → `~/.mur/companion/templates/` (user-level overrides) → `~/.mur/agents/<name>/companion/voice.md` (ejected disk override).

Submodules:

- **`clock.rs`** — `Clock` trait + `SystemClock` + `MockClock` (deterministic test harness)
- **`voice.rs`** — voice.md composition: in-memory rendering from onboarding answers, disk override chain, eject/rebuild/diff operations
- **`i18n.rs`** — locale heuristic + `ensure_locale` (translate fallback via LLM or no-op)
- **`linter.rs`** — voice quality linter: sentence count, banned phrase detection, emoji check, exclamation density, zh/English ratio
- **`picker.rs`** — bandit-state `WeightedIndex` situation/template picker with cooldown tracking and `record(Signal)`
- **`schedule.rs`** — deterministic interval `should_send_now` + active window enforcement (Spec §4.7)
- **`situations.rs`** — hour-of-day situation weight table (Spec §4.6)
- **`earned_permission.rs`** — proactive gate: enabled / paused / learning / quiet states
- **`notifier.rs`** — `Notifier` trait + `StdoutNotifier` (writes inbox markdown files)
- **`inbox.rs`** — front-matter + body markdown writer (`O_CREAT | O_EXCL`, R16 atomicity)
- **`outbox.rs`** — 12-step tick loop: gate → resume → dismiss → schedule → pick → generate → lint → i18n → deliver → finalize
- **`telemetry.rs`** — frozen `OutboxEvent` ledger schema (13 variants)

- **Spec:** `docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-29-companion-phase-1-1-plan.md`

---

## Skills System

Skills are installable agent capabilities packaged as YAML or Markdown manifests. Each skill lives under `~/.mur/skills/<name>/skill.yaml` (or `skill.md`). The CLI surface is `mur skill {validate,fmt,list,show,remove,search,info,audit,trust,from-pattern,doctor,consolidate,reindex-vec,reindex-stats}`.

Key modules:

- **`mur-common/src/skill/`** — Shared types and validation: `SkillManifest` (schema v2.1), `Skill` wrapper, `Content`, `Procedure`, `Trigger`, `Category`, `TrustLevel`, plus `scan_skill()` security scanning.
- **`mur-core/src/cmd/skill_cmd.rs`** — `mur skill` CLI handlers (validate, fmt, list, show, remove, search, info, audit, trust).
- **`mur-core/src/cmd/skill_doctor.rs`** — `mur skill doctor` checks: deprecated fields, missing abstract, untrusted skills, trigger coverage, MCP requirements coverage, MCP capability availability.
- **`mur-core/src/cmd/skill_from_pattern.rs`** — `mur skill from-pattern`: promote a Stable/Canonical pattern to a skill, with optional LLM polish.
- **`mur-core/src/skill_index/`** — Vector embedding (LanceDB) and BM25 index for `mur skill search`.
- **`mur-core/src/cmd/skill_registry.rs`** — Remote registry fetch + search.
- **`mur-core/src/skill_llm/`** — LLM-augmented skill maintenance (M6c). Provides `maintenance_call()` with content-hash caching (30d TTL), per-day budget tracking ($0.50 default), and role-resolution from the model registry. Three LLM-backed checks: api-drift, coverage-gap, and contradiction adjudication.
- **`mur-core/src/skill_traces/`** — Trace clustering helpers shared by api-drift and coverage-gap doctor checks (M6c).

### Skill LLM Maintenance (M6c)

Three LLM-augmented checks, all opt-in via `--llm`/`--llm-adjudicate`:

| Check | CLI flag | Description |
|-------|----------|-------------|
| api-drift | `mur skill doctor --llm` | Compares skill procedure against recent trace tool usage; warns if the procedure has drifted from observed behavior |
| coverage-gap | `mur skill doctor --llm` | Clusters repeated failures and asks the LLM what skill or step would unblock them |
| contradiction adjudication | `mur skill consolidate --llm-adjudicate` | Takes rule-based contradiction pairs and asks the LLM: contradict, coexist, or duplicate? |

**Configuration** (`~/.mur/config.yaml`):
```yaml
skill_llm:
  per_call_token_cap: 1500    # max output tokens per call
  per_day_usd_cap: 0.50       # daily budget cap
  cache_ttl_days: 30           # content-hash cache TTL
  model_ref: null              # explicit model key override (null = role resolution)
```

**Model role**: Use `mur model role set maintenance <model_key>` to dedicate a cheap model (e.g., Haiku) to maintenance. Falls back to `roles.chat` then the first chat-capable model.

**Graceful degradation**: Every check degrades to its pre-M6c stub when no model is available. LLMs are an upgrade, never a hard dependency. Run `mur skill doctor --llm-status` to see the current state.

### Skill↔MCP Integration (v2.1)

Skills can declare MCP tool requirements via the `mcp_requirements` field, introduced in schema v2.1. This allows a skill manifest to specify which MCP tools it needs and what capability each tool provides, enabling the runtime to verify tool availability before execution (M6b).

**Data model** (`mur-common/src/skill/mcp.rs`):

- `SkillCapability` enum — `ReadFile | ListTools | Search | WriteFile | ExecuteSafe | NetworkHttp` (string-form serde, e.g. `"read_file"`).
- `McpRequirement` struct — `tool_pattern` (glob), `capability` (SkillCapability), `fallback` (optional description for graceful degradation).

Example manifest fragment:

```yaml
mcp_requirements:
  - tool_pattern: "filesystem.read_*"
    capability: read_file
  - tool_pattern: "web.search"
    capability: search
    fallback: "Use built-in web search if MCP web tool unavailable"
```

**Validation** — `validate_requirements()` in `mur-common/src/skill/mcp.rs` checks glob syntax and rejects duplicate tool patterns. The main `validate()` function calls this for every manifest parse, returning `ValidationError::McpRequirements(idx, message)` on failure.

**Doctor checks** (`mur-core/src/cmd/skill_doctor.rs`):

| Check | Severity | What it does |
|-------|----------|--------------|
| `mcp-requirements-coverage` | Warn | Flags procedural skills whose steps reference dotted tool names (e.g. `filesystem.read_file`) but lack a matching `mcp_requirements` entry. |
| `mcp-capability-available` | Warn/Unknown | Glob-matches each requirement's `tool_pattern` against the agent's configured MCP tools. Emits `Unknown` when run outside an agent context (no tool list available). Skills with a `fallback` are skipped. |

**`mur skill show`** — When a skill has `mcp_requirements`, the command prints a formatted "MCP Requirements:" block after the YAML, listing each tool pattern with its capability and optional fallback.

### Cross-Agent Observability (M7a)

Read-only aggregation of peer agent skill stats and per-agent fitness scoring. New CLI surface:

| Command | Description |
|---------|-------------|
| `mur agent peers [--json]` | List peer agents on this host |
| `mur skill stats <name> --all-agents [--json]` | Aggregate skill stats across all peer agents |
| `mur skill consolidate --cross-agent [--dry-run] [--apply]` | Cross-agent Jaccard duplicate scan; writes `_consolidation/cross-agent-<date>.jsonl` |

Agent fitness uses a 7-day half-life decay on `last_used_at` (floor 0.1), configurable via `cross_agent.fitness.half_life_days` and `cross_agent.fitness.floor` in `config.yaml`. `mur agent card <name>` now includes a `Fitness` section.

M7a is **read-only** on peer state — it never mutates another agent's skills. Mutation (gene model + propagation) lands in M7b/M7c. See `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7a.md` for the full plan.

Execution-time enforcement of these requirements (resolving globs, checking tool availability, applying fallbacks) is deferred to M6b.
