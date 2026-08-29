# CLAUDE.md

Operational guidance for Claude Code working in this repository. Detailed runtime / GUI / companion / P0a designs moved to `docs/architecture/runtime-overview.md`.

## Build & Install

```bash
# ── Quick install (build + install in one step) ──
./install.sh                    # Runs build.sh --release --install

# ── Build only ──
./build.sh                      # Release build with embedded web dashboard
./build.sh --install            # Build + install to ~/.local/bin (no sudo; MUR_INSTALL_DIR overrides)

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

`build.sh` requires `~/Projects/mur-web` (or `MUR_WEB_DIR`). The `mur-agent-gui` crate is workspace-EXCLUDED so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2.

## Architecture

Cargo workspace of small crates plus two workspace-excluded Tauri apps. The load-bearing ones:

- **`mur-common`** — Shared types, plus the small persisted files those types own (`Config`, `AgentProfile`, `ModelRegistry`, `LockFile`, ledger). It does do file I/O; what it does not hold is pipeline or CLI logic.
- **`mur-core`** — All CLI logic and the `mur` binary. Modules map to the four-stage pipeline. Hosts `mur agent ...` user-facing subcommands.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). Crate README has the walkthrough.
- **`mur-daemon`** — Long-running background daemon binary.
- **`mur-mcp-server`** — MCP server binary (stdio JSON-RPC). Exposes interactive lookup tools so AI tools can call MUR mid-conversation. Read-only; mutations go through hooks.
- **`mur-gui-core`** — Shared GUI library (sidecar supervisor, companion bridge, A2A client). Consumed by `mur-hub-gui` and during migration also by `mur-agent-gui`. See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.

**Shared state with its own file format gets its own crate** — `mur-channel`, `mur-compress`, `mur-open-items`. The rule exists because `mur-agent-runtime` must not depend on `mur-core` (that pulls LanceDB + Arrow into every agent process), so anything both of them read or write has to live below both. Reach for this before adding I/O to `mur-common`.

Workspace-excluded Tauri 2 GUI apps (built via their own manifests so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2):

- **`mur-agent-gui`** — Per-agent `.app` shell (legacy; deprecated in M-h8).
- **`mur-hub-gui`** — MUR Hub cross-agent desktop app (in development; replaces `mur-agent-gui` in v1).

### Agent Platform

MUR is a local-first Agent platform in native Rust. Architecture layers:

- **Agent Runtime** — `mur-agent-runtime` (P0a): per-agent A2A v0.3 supervisor with profile, prompt, MCP servers, skills, entitlements. One BusyBox-style binary, symlinked per agent.
- **Memory / Learning** — Four-stage pipeline below: patterns, workflows, notes, skills with maturity lifecycle and decay.
- **Agent Infrastructure** — MCP Server (interactive tools for AI tools mid-conversation), Skills (teach agents when/why to use MUR commands), Action Pipeline (file notification + deletion safety + task queue), Cost Router (governed spawn of claude/codex/agy).
- **Human Interface** — Companion (voice + proactive messaging), Hub GUI (cross-agent desktop app), Slack Bridge.
- **Governance** — Commander: cross-network orchestration with cryptographic governance and immutable audit.

Key specs: `docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md` (positioning), `2026-05-31-agent-action-pipeline-design.md` (action pipeline), `2026-06-01-mur-mcp-server-and-skills-design.md` (MCP + skills), `2026-06-01-cost-router-orchestrator-design.md` (cost router).

### Memory Pipeline (Four-Stage)

The learning subsystem that powers agent memory:

```
capture/ → store/ → retrieve/ → inject/
                ↕
            evolve/
```

- **`capture/`** — Noise filter, significance scoring, feedback extraction. Ambient session capture lives in `session/ambient.rs` (hooks record every event); the harvest gate (`harvest/`) turns idle sessions into workflow proposals.
- **`store/`** — `YamlStore` (transitional Pattern store), `LanceDbStore` (vector index, always rebuildable), `WorkflowYamlStore`. Vector store abstracted via `store::vector::VectorStore` (`LanceDbStore` now; `QdrantStore` P1.3).
- **`retrieve/`** — `score_and_rank_generic()` over the `Retrievable` trait (skills + workflows; Pattern transitional); applies recency / effectiveness / importance / decay / length normalization
- **`inject/`** — `hook.rs` formats skills + workflows for injection; `sync.rs` writes tool-specific configs (Claude Code hooks, Gemini CLI, etc.)
- **`evolve/`** — Skill lifecycle (Draft→Emerging→Stable→Canonical), feedback, co-occurrence

**Pattern removal (workflow-engine v2 P1a/P1b, 2026-06-11):** the legacy Pattern pipeline (emergence/fingerprint mining, pattern decay sweeps, pattern injection) is removed. `mur migrate --patterns` exports `~/.mur/patterns/` to markdown then deletes it. Skills (`category: Workflow` et al.) are the knowledge objects; `context_api::ingest`/`submit_feedback` still write transitional Patterns until the Notes migration (W3b+).

Sources pipeline: `mur-core/src/sources/` adapters (Obsidian / Notion / Joplin) feed the same retrieve pipeline. See `docs/architecture/runtime-overview.md`.

### Key Data Model

Skills are the primary knowledge object (`mur-common/src/skill/`): `SkillManifest` (name, description, category, content with abstract/context/procedure, triggers, tags) + `SkillStats` (lifecycle state, usage counts). `Workflow` wraps `KnowledgeBase` via `#[serde(flatten)]` and adds `steps`, `variables`, `trigger`, `schedule`.

Tier half-lives: session=14d, project=90d, core=365d. Scoring floor 0.42 (config `retrieval.min_score`). Max items/query 5. Max tokens ~2000.

### Data Storage (Runtime)

All data at `~/.mur/`:

- `skills/<name>/skill.yaml` — skills (source of truth). A running agent re-reads the tree when its fingerprint changes, so an edit, install, or removal lands on that agent's next turn with no restart and nothing to notify it — see `skills_fingerprint` in `mur-agent-runtime/src/skills/mod.rs` for why this is derived from disk rather than bumped by writers.
- `workflows/*.yaml` — multi-step workflow definitions
- `inbox/workflow-proposals/*.yaml` — harvest proposals pending review (`mur out`)
- `session/recordings/<id>.jsonl` — append-only event log (ambient capture)
- `exported-patterns/*.md` — legacy patterns exported by `mur migrate --patterns`
- `queue/events.jsonl` — CLI hook capture log. Redacted on write (shared
  `mur_common::redact`, the same chokepoint B0 rule 9 uses) and rotated
  newsyslog-style: `.0` uncompressed, `.1.gz` onward, oldest dropped. Tuned by
  `capture: {rotate_at_mb: 64, keep_generations: 5}` in `config.yaml`. Records
  written before 2026-08-19 carry no `recorded_at`, so `mur hook stats` reports
  their window as unknown rather than guessing.
- `config.yaml` — user config

LanceDB vector index is always rebuildable via `mur internals reindex`.

## CLI Surface (top level)

- `mur verify [--file path] [--all]` — scan docs for stale claims (paths, commands, code refs)
- `mur agent <subcommand>` — manage murmur agents (create / list / status / send / card / cli / export / doctor / prompt / mcp / skill / perm / secret / companion / rekey / schedule). `cli <name>...` opens an interactive streaming TUI chat with a running agent (`--resume` to continue the last conversation); in-session slash commands include `/channels [N]` (list/switch channels), `/sessions`, `/login [anthropic|chatgpt]` (OAuth health; repair escalates re-read → owner-CLI refresh → browser login with a terminal handover — unrelated to `mur auth login`, which signs in to mur.run), and `/model [N|name]` (list registry models; switch the running agent via the `model/set` A2A method — single-model agents hot-swap, chain/routing agents fall back to a profile write plus a restart hint); multiple names open one multiplexer pane per agent (tmux primary; zellij/WezTerm/kitty auto-detected). The `murmur` symlink is the quick form: `murmur a1 a2 a3` ≡ `mur agent cli a1 a2 a3`; bare `murmur` opens the concierge. `mcp` now also includes `add-remote`, `login`, and `registry-add` for remote (Streamable HTTP) servers with static bearer or OAuth 2.1 auth — see `docs/architecture/runtime-overview.md` for the full transport and auth details. Full surface in `docs/architecture/runtime-overview.md`.
- `mur fleet {create|list|show|status|run|stop|start|export|import|partition-plan|merge}` — squads of agents working a shared goal over one signed channel (id `fleet-<name>`), defined in `~/.mur/fleets/<name>/fleet.yaml`. `run` fans the goal to members via the DAG executor; `status` reports the fleet's most recent run through the same record and renderer as `mur job status` (one derivation, many surfaces — see `docs/superpowers/specs/2026-08-17-job-fleet-run-status-design.md`); `run --loop` adds guards (iteration cap, deadline, stuck-detection, `--budget-usd` from real per-token spend) and converges on `done_when: marker:<TEXT>` (own-line sentinel, not substring), `done_when: queue-empty` (stop once an iteration finds nothing queued), or router DONE/CONTINUE.
  - **Safety triad — do not weaken:** unattended auto-run is OFF unless `MUR_FLEET_AUTORUN=1`; auto-run also requires a positive `loop.budget_usd`; `mur fleet stop <name>` is the kill-switch (`.stopped` sentinel, honored by loop + daemon + manual run; cleared by `start`). Every path passes `yes:false` — never blanket-approve risk-tiered steps. Commander governance (kill + budget ceiling) is fail-closed on Err.
  - **Unattended approvals defer, they do not time out.** With no TTY the gate parks its `HitlRequest` and returns at once; the step is `blocked` (not failed), independent branches still run, the channel stays `input-required`, and the loop stops with `LoopStop::AwaitingApproval`. Approvals match on `action_hash` — never `hitl_id`, which is minted per call — so an approval given hours later releases the gate on the next run (7-day TTL), a denial is not re-asked, and changing the action invalidates the approval. `docs/superpowers/specs/2026-08-19-unattended-hitl-defer-design.md` has the P1–P3 plan and the list of rejected alternatives; read it before adding any auto-approval path.
  - Experimental, default OFF: `MUR_PARALLEL_EXEC=1` (per-track git worktrees), `MUR_PARALLEL_CONCURRENT=1` (`merge-concurrent` N-way line merge — converges bytes, NOT correctness).
  - Design, phase history, and the Spike-1 overlap decision: `docs/superpowers/specs/2026-06-19-mur-fleet-design.md`, `docs/superpowers/specs/2026-06-29-parallel-tracks-p3-concurrent-merge-design.md`, `docs/superpowers/validation/spike1-overlap-rate.md`.

- `mur model {connect|import|add|list|show|remove|migrate|prices|role|doctor}` — `~/.mur/models.yaml` provider/model registry. `connect [vendor]` is the key-driven bulk path (CLI counterpart of the Hub Model Library): key → Keychain, then models come from the models.dev catalog for known cloud vendors and from a live `/v1/models` probe for custom or local endpoints — the split #950 established, because a vendor's registry base URL is often a chat-only proxy that 401s on `/v1/models`. Non-native vendors are written as `provider: openai` (the wire protocol) with the vendor slug retained for catalog pricing; bare `connect` probes local runtimes. `import <file>` merges another machine's registry (never deletes, `--force` to overwrite) and reports which secret refs do not resolve locally. `add` accepts `--input-cost`/`--output-cost` (USD per 1k tokens) and auto-fills pricing + context window from the models.dev catalog unless `--no-fetch`; `prices {refresh|show}` manages the cached catalog (`~/.mur/cache/model-prices.json`); `doctor` is an offline read-only audit (dangling `model_ref`s, ids the catalog never carried, legacy `model:` blocks disagreeing with their ref, and `secret: file:` refs that are plaintext on disk — warn-only, exit code unchanged, because `file:` is the only backend on a headless Linux box and a gate for something the user cannot fix gets switched off) that never rewrites a model id — read its module doc before extending it, it records why it is NOT a deprecation check. The Hub GUI **Model Library** (mur-hub-gui) connects cloud providers (key → Keychain) and auto-detects local runtimes, discovers models via `/v1/models`, and adds them as registry aliases. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md` and `2026-06-17-mur-model-library-design.md`.
- `mur official {list|install <id>}` — browse and install official MUR agents/fleets from the app.mur.run catalog. Install requires `mur auth login`; pro-tier items require an active subscription. Downloads carry an account-bound `OfficialLicense` (stored in `~/.mur/licenses/`); the fleet/agent import paths refuse official-marked bundles without a matching license (anti-sharing gate; expiry gates downloads only, never installed content). See `docs/superpowers/specs/2026-07-20-official-catalog-design.md`.
- `mur deep-research {setup|""} [question]` — simplified web research UX: `setup` (one-time wizard for model, workers, budget, egress), `""` (status panel), or ask a question (preflight start + guarded run). `provision`/`run` remain as the advanced flag-based path. Egress consent is explicit: `setup`/`provision --grant-egress` only.

For detailed agent / companion / GUI export / runtime internals, read `docs/architecture/runtime-overview.md` only when the task requires it.

Before changing anything that pins, verifies, or launches an MCP server, read `docs/architecture/mcp-supply-chain.md` — it records what each check covers, what it structurally cannot, and the two things deliberately not built (with reasons, so they don't get re-proposed).

## Development Notes

- Rust edition 2024 — `let` chains stable (`if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans live in `docs/superpowers/plans/`. OpenSpec change specs in `openspec/changes/`.
- **Unified Channel v3a–v3d** implemented on branch `feat/unified-channel-v3b` (v3d on `feat/unified-channel-v3d`):
  - v3a: DAG executor emits attributed `StateChange`/`ToolCall`/`ToolResult` events as `ChannelActor::System`.
  - v3b: Deterministic `idem_key`, `run_id` per workflow run.
  - v3c: Risk-tiered, SHA-256-pinned HITL gate (`mur-common/src/hitl.rs`; `mur-core/src/hitl/`). Steps with `risk: write` or higher pause before execution, write a `HitlRequest` channel event, and wait for `mur channel approve <channel_id> <hitl_id>` (or `--deny`). The executor re-verifies the hash at the execute boundary (fail-closed on drift). `append_event` is dedup-aware (idempotency key under exclusive lock). Crashed runs resume via a `ToolResult` cursor check.
  - `CHANNEL_SCHEMA_VERSION = 2` (v2: `HitlResponse` events carry approval authority).
  - Use `mur workflow run --channel-new <skill>` or `--channel <id>` to attach execution; `mur channel approve <channel_id> <hitl_id> [--deny] [--reason <msg>]` to act on HITL gates.
  - v3d: Channel events are Ed25519-signed by the channel's writer (`mur_channel::sign`; `ChannelEvent.sig`/`key_version`), verified on fold. The canonical sign-input EXCLUDES `seq`/`ts`. `MUR_CHANNEL_REQUIRE_SIG` enforces verification (default off = migration-safe: legacy unsigned events tolerated). A2 peer-writes-own (specialist runtimes signing their own events) is the v3d-2 follow-on. See `mem:project_unified_channel_pr433`.
- **Unified Channel v4a** (mobile sync foundation) on branch `feat/unified-channel-v4a`:
  - Every mobile turn (LAN + relay) is persisted into the agent's channel via `mur-core::mobile::persist_mobile_exchange`; the old `mobile-events.jsonl` mirror is retained for Hub live-tail.
  - `ClientFrame::ChannelQuery { op, channel_id, since_seq }` / `ServerFrame::ChannelData { op, payload }` in `mur-common::mobile`: `op` = "list" returns channel summaries; `op` = "events" returns all events for a channel (from `since_seq` if given).
  - Both daemon paths (`mobile_server.rs`, `relay_client.rs`) handle `ChannelQuery` by calling `mur-core::mobile::channel_query`.
  - `mur-mobile-sdk`: `ChannelListItem`, `ChannelEventItem` UniFFI records; `MobileEvent::ChannelList/ChannelEvents/ChannelUpdate`; `MobileClient::list_channels()` and `fetch_channel_events(id, since_seq)`.
  - Live-push: daemon spawns a `watch_channels` watcher; broadcasts `channel.updated` events to all connected phones via `tokio::sync::broadcast`; SDK translates to `MobileEvent::ChannelUpdate`.
  - v3d-2: Adds the `channel/delegate` A2A method — a delegated specialist runs its turn and **signs+writes its own** reply (`Agent{self}`) into the shared channel; the concierge dials `channel/delegate` instead of `message/send` and no longer mediates/signs the specialist's reply. Verify-on-fold is now **per-actor** (each event verified against its actor's `<mur_home>/agents/<id>` key) via `mur-core::channel_verify`.

## Release Process

`main` is protected, so the whole release is one PR: **merging the version bump to `main` IS the release.** `tag.yml` tags the merge commit and dispatches `release.yml` automatically — there is no manual tag step, so review the bump PR as the release approval. Full step-by-step (both Cargo.lock files, the macOS `sed` trap, why a hand-pushed `git push origin main --tags` is still the one thing never to do): the **`mur-release`** skill (`.claude/skills/mur-release/`).

## Documentation Checklist

After a user-facing change, update all three: **`README.md`**, the **docs site** (https://app.mur.run/docs/core), and the **product page** (https://app.mur.run/products/mur).
Exact source paths, wiring, and publish gotchas live in the **`update-docs`** skill (`.claude/skills/update-docs/`) — use it, don't reconstruct the paths.

## Mandatory Rules

1. **No hardcoded values.** Use constants, config, or env vars. Research best practice if unsure.
2. **Ask, don't guess.** If requirements / paths / API contracts / behavior are ambiguous, ask. In auto mode, make low-risk assumptions and flag them.
3. **SSH connection.** Use Desktop Commander to ssh, not Bash/SSH.
4. **Single source file ≤ 800 lines.** When approaching the limit, split into submodules following the same structural pattern as siblings (e.g., `cmd/agent/{create,list,export}.rs`, `server/routes/{patterns,agents}.rs`, `companion/outbox/{step}.rs`). Pure code movement first; behavior changes in a separate PR.
5. **Read narrowly.** Prefer LSP queries (goToDefinition, findReferences) and `grep`/`Grep` over reading whole large files. When you must read a file, target the relevant range with `offset`/`limit` if you already know the section.
6. **CLAUDE.md is operational, not a changelog.** Historical milestone descriptions, completed phase notes, and detailed design walkthroughs belong in `docs/architecture/` or `docs/superpowers/specs/`. Keep this file lean so every session starts cheap.

7. **Brand name is uppercase "MUR".** Everywhere a user can see it — GUI strings, `display_name`, docs, marketing copy, companion/voice text, notifications — the brand is the three uppercase letters **MUR** (never "Mur" or "MuR"). The ONLY exceptions are the CLI binary/command (`mur`), code identifiers, file paths, internal `name`/directory slugs (e.g. the concierge agent's dir + `name: mur`), and the `~/.mur` home. Use `display_name` for the uppercase user-facing label; keep internal `name` lowercase so it matches the on-disk directory (the runtime spoof check is exact-match).

8. **Agent name lookup is case-insensitive (CLI).** `mur agent send mur` and `... Mur` must both resolve. Resolution maps the typed name to the canonical on-disk name via `a2a_dial::canonicalize_agent_name`; downstream still uses the exact canonical name so the runtime spoof check passes.


5. **Token saving rules**
- Skip brainstorming unless explicitly requested
- Skip multi-step planning for small tasks
- Do not create plan files automatically
- Keep responses concise
- Avoid restating requirements
- Do not use subagents unless necessary
- Never use subagents for simple tasks
- Prefer direct implementation
- Use one-pass execution
