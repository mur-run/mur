# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Install

```bash
# ── Quick install (build + install in one step) ──
./install.sh                    # Runs build.sh --release --install

# ── Build only ──
./build.sh                      # Release build with embedded web dashboard
./build.sh --install            # Build + install to /opt/homebrew/bin/mur

# ── Manual build (without embedded dashboard) ──
cargo build --workspace         # Debug build
cargo build --release           # Release build

# ── Build with embedded web dashboard (what build.sh does) ──
cd ~/Projects/mur-web && npm run build
MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build --release

# ── Test ──
cargo test --workspace
cargo test -p mur-core <test_name>

# ── Lint ──
cargo clippy --workspace -- -D warnings
cargo fmt --check

# ── Run locally ──
cargo run -- <command>          # e.g. cargo run -- search "swift testing"
```

### Build Scripts

| Script | What it does |
|--------|-------------|
| `build.sh` | Builds mur-web (Next.js) → embeds into mur binary → release build |
| `build.sh --install` | Same + copies binary to `/opt/homebrew/bin/mur` |
| `install.sh` | Shortcut for `./build.sh --release --install` |

**Requires:** `~/Projects/mur-web` (or set `MUR_WEB_DIR`). Without it, build fails.

## Architecture

Cargo workspace with three crates:

- **`mur-common`** — Shared types only. No logic, no I/O. `Pattern`, `KnowledgeBase`, `Workflow`, `Config`, `MurEvent`, plus `AgentProfile`/`LockFile`/A2A envelopes/telemetry constants. All other crates depend on this.
- **`mur-core`** — All CLI logic and the `mur` binary. Structured as modules that map to the four-stage pipeline. Hosts `mur agent ...` user-facing subcommands.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). See `mur-agent-runtime/README.md` for the walkthrough; spec at `docs/superpowers/specs/2026-04-22-murmur-p0a-agent-runtime-design.md`.

### Four-Stage Pipeline

```
capture/ → store/ → retrieve/ → inject/
                ↕
            evolve/
```

**Sources pipeline (P1.4 — All adapters shipped: Obsidian + Notion + Joplin; --watch + install-schedule; format_notes_section helper ready):** An alternate input to `store/` lives in `mur-core/src/sources/` — `KnowledgeSource` adapters pull documents from external note apps (Obsidian, Notion, Joplin) into the same retrieve pipeline as patterns. The vector store is abstracted behind `store::vector::VectorStore` (impls: `LanceDbStore` now; `QdrantStore` P1.3). See `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md`.

- **`capture/`** — Noise filter, significance scoring, emergence detection, feedback extraction from session transcripts
- **`store/`** — `YamlStore` (source of truth, atomic writes), `LanceDbStore` (vector index, always rebuildable), `WorkflowYamlStore`
- **`retrieve/`** — Multi-signal scoring: `score_and_rank_hybrid()` combines vector similarity (0.7) + keyword BM25 (0.3), then applies weights for recency, effectiveness, importance, time decay, and length normalization
- **`inject/`** — `hook.rs` formats patterns for injection into AI tools; `sync.rs` writes to tool-specific config files (Claude Code hooks, Gemini CLI, etc.)
- **`evolve/`** — Decay, maturity lifecycle (Draft→Emerging→Stable→Canonical), feedback processing, co-occurrence tracking, pattern linking (Zettelkasten-style), emergence detection, commander bridge

### Key Data Model

`Pattern` wraps `KnowledgeBase` via `#[serde(flatten)]` — so YAML stays flat with no nested `base:` key. `Pattern::deref()` forwards to `KnowledgeBase`, so `pattern.name` works directly.

`KnowledgeBase` fields: `name`, `description`, `content` (dual-layer: `technical` + `principle`), `tier` (session/project/core), `importance`, `confidence`, `tags`, `applies`, `evidence`, `links`, `lifecycle`, `maturity`, `decay`.

Pattern tiers have exponential half-lives: session=14d, project=90d, core=365d.

Scoring floor: 0.35. Max patterns injected per query: 5. Max tokens: ~2000.

### Data Storage (Runtime)

All data at `~/.mur/`:
- `patterns/*.yaml` — source of truth, human-readable
- `workflows/*.yaml` — multi-step workflow definitions
- `session/active.json` — current session state
- `session/recordings/<id>.jsonl` — append-only event log
- `config.yaml` — user config (embedding provider, tool enables)

LanceDB vector index is always rebuildable from YAML via `mur reindex`.

### Other Modules

- **`verify.rs`** — Documentation verification engine: parses claims (file paths, CLI commands, code refs) from Markdown and checks them against the project. Known commands are auto-derived from the clap command tree at runtime.
- **`server.rs`** — Axum-based local API server (Phase 0 feature)
- **`community.rs`** — Community pattern browser
- **`dashboard.rs`** — Terminal overview
- **`interactive.rs`** — `dialoguer`-powered interactive pattern creation
- **`migrate/`** — legacy schema migration (rarely needed)
- **`auth.rs`** — Trust levels for community patterns

### CLI Commands (New)

- **`mur verify [--file path] [--all]`** — Scan docs for stale claims (paths, commands, code refs)
- **`mur agent <subcommand>`** — Manage murmur agents (P0a). Subcommands: `create`, `list`, `status`, `stop`, `remove`, `rename`, `send`, `card`, `install-service`, `prompt {show|edit|set}`, `mcp {add|list|remove|rename}`, `skill {add|list|remove|show}`, `perm {show|set-mode|allow-host|deny-host|list-hosts|allow-read|allow-write|deny-path|allow-spawn|deny-spawn|set-limit}`, `secret {set|list|delete}`, `export {--format=pkg|bin|gui}`, `doctor [--format ...]`, `stats`, `logs`. The runtime binary that backs each agent lives in `mur-agent-runtime/`.
- **`mur agent doctor [--format pkg|bin|gui|all] [--json]`** — Pre-flight prereq check for export targets (no build, just diagnostics). Same logic is reused as the fail-fast step inside `--format gui` export pipeline.
- **`mur model {add|list|show|remove|migrate}`** — Manage `~/.mur/models.yaml` (provider/model/secret-ref registry). `migrate` walks existing agent profiles and synthesizes registry entries idempotently. Agents opt in via `profile.yaml`'s `model_ref:` field; supervisor resolves `model_ref → registry → SecretRef`, falling back to the legacy inline `model:` block. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`.
- **`mur agent secret {set|list|delete}`** — OS-keychain CRUD for an agent's secrets (account format `{agent}/{KEY}`, service `mur-agent`). `set` uses a hidden prompt via `rpassword` when no inline value is given.
- **`mur agent companion <subcommand>`** — Manage the companion subsystem for an agent (Phase 1.1):
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

### GUI Export (`mur agent export --format gui`)

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

### Agent Runtime (murmur P0a)

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

### Companion subsystem (Phase 1.1)

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

## Development Notes

- Rust edition 2024 — `let` chains are stable and used throughout (e.g., `if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly on the pattern
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans and architecture docs live in `plans/`. OpenSpec change specs in `openspec/changes/`.

## Release Process

After tagging a new release:

1. **Tag and push:** `git tag -a v2.0.0-alpha.X -m "message" && git push origin main --tags`
2. **Update Homebrew tap:** The formula in `mur-run/homebrew-tap` must be manually updated.
   - Get sha256: `curl -sL https://github.com/mur-run/mur/archive/refs/tags/v<VERSION>.tar.gz | shasum -a 256`
   - Edit `Formula/mur.rb` in `/opt/homebrew/Library/Taps/mur-run/homebrew-tap/` (or clone from `https://github.com/mur-run/homebrew-tap`)
   - Update `url` (new tag) and `sha256`, commit, push
3. **Verify:** `brew update && brew upgrade mur`

> ⚠️ Pushing a git tag does NOT auto-update Homebrew. The tap formula must be updated separately.

## Documentation Checklist

When making changes to this repo, check whether the following need to be updated:

1. **`README.md`** — `/Volumes/Firecuda4tb/Projects/mur/README.md`
2. **Documents page** — `https://app.mur.run/docs/core`
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/docs-content/` (Markdown files)
   - Page component: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/docs/core/[[...slug]]/page.tsx`
   - Navigation: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/components/docs/coreNavigation.tsx`
3. **Product page** — `https://app.mur.run/products/core`
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/products/core/page.tsx`

## Mandatory Rules

1. **No hardcoded values**: Never hardcode strings, paths, magic numbers, or
   config values in design or implementation. Use constants, config files, or
   environment variables. When unsure of best practice, research before
   implementing.
2. **Ask, don't guess**: If requirements, file paths, API contracts, or
   intended behavior are ambiguous, ask for clarification before coding.
   Exception: in auto mode, make reasonable assumptions for low-risk decisions
   and flag them in the response.
3. **SSH Connection**: User Desktop Commander to ssh to remote server instead of BASH/SSH.
