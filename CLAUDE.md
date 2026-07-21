# CLAUDE.md

Operational guidance for Claude Code working in this repository. Detailed runtime / GUI / companion / P0a designs moved to `docs/architecture/runtime-overview.md`.

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

`build.sh` requires `~/Projects/mur-web` (or `MUR_WEB_DIR`). The `mur-agent-gui` crate is workspace-EXCLUDED so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2.

## Architecture

Cargo workspace with five crates plus two workspace-excluded Tauri apps:

- **`mur-common`** — Shared types only. No logic, no I/O. `Pattern`, `KnowledgeBase`, `Workflow`, `Config`, `MurEvent`, plus `AgentProfile`/`LockFile`/A2A envelopes/telemetry constants.
- **`mur-core`** — All CLI logic and the `mur` binary. Modules map to the four-stage pipeline. Hosts `mur agent ...` user-facing subcommands.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). Crate README has the walkthrough.
- **`mur-daemon`** — Long-running background daemon binary.
- **`mur-mcp-server`** — MCP server binary (stdio JSON-RPC). Exposes interactive lookup tools so AI tools can call MUR mid-conversation. Read-only; mutations go through hooks.
- **`mur-gui-core`** — Shared GUI library (sidecar supervisor, companion bridge, A2A client). Consumed by `mur-hub-gui` and during migration also by `mur-agent-gui`. See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.

Workspace-excluded Tauri 2 GUI apps (built via their own manifests so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2):

- **`mur-agent-gui`** — Per-agent `.app` shell (legacy; deprecated in M-h8).
- **`mur-hub-gui`** — MuR Hub cross-agent desktop app (in development; replaces `mur-agent-gui` in v1).

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

- `skills/<name>/skill.yaml` — skills (source of truth)
- `workflows/*.yaml` — multi-step workflow definitions
- `inbox/workflow-proposals/*.yaml` — harvest proposals pending review (`mur out`)
- `session/recordings/<id>.jsonl` — append-only event log (ambient capture)
- `exported-patterns/*.md` — legacy patterns exported by `mur migrate --patterns`
- `config.yaml` — user config

LanceDB vector index is always rebuildable via `mur internals reindex`.

## CLI Surface (top level)

- `mur verify [--file path] [--all]` — scan docs for stale claims (paths, commands, code refs)
- `mur agent <subcommand>` — manage murmur agents (create / list / status / send / card / cli / export / doctor / prompt / mcp / skill / perm / secret / companion / rekey / schedule). `cli <name>...` opens an interactive streaming TUI chat with a running agent (`--resume` to continue the last conversation); in-session slash commands include `/channels [N]` (list/switch channels) and `/sessions`; multiple names open one multiplexer pane per agent (tmux primary; zellij/WezTerm/kitty auto-detected). The `murmur` symlink is the quick form: `murmur a1 a2 a3` ≡ `mur agent cli a1 a2 a3`; bare `murmur` opens the concierge. `mcp` now also includes `add-remote`, `login`, and `registry-add` for remote (Streamable HTTP) servers with static bearer or OAuth 2.1 auth — see `docs/architecture/runtime-overview.md` for the full transport and auth details. Full surface in `docs/architecture/runtime-overview.md`.
- `mur fleet {create|list|show|run|stop|start|export|import|partition-plan|merge}` — squads of agents working a shared goal over one signed channel (id `fleet-<name>`). `create` writes `~/.mur/fleets/<name>/fleet.yaml` + the shared channel (router→Router, members→Delegate; agent names canonicalized; fleet name validated as a lowercase slug); `run` executes one iteration by fanning the goal to each member via the existing DAG executor delegation; `run --loop [--max-iterations N] [--deadline 2h]` runs a **guarded loop** (Phase 2a) — iteration-cap / deadline / stuck-detection guards (in `cmd/fleet/loop_run.rs`, outside any agent) + convergence (structured `done_when: marker:<TEXT>` → deterministic, converge when a member emits the marker as a sentinel on its own line this run — own-line not substring, so prose quoting/negating it can't false-converge; else free-text → router DONE/CONTINUE; fail-safe to continue). Skills/rules gain a `scope: {User|Project|Fleet|Enterprise}` field (+ `scope_visible` predicate; live injection wiring is Phase 2b). `fleet` (agent squad) is orthogonal to `team` (the user's human org/seats) and to `fleet_sync` (device sync). See `docs/superpowers/specs/2026-06-19-mur-fleet-design.md`. **Phase 2b (shipped):** the daemon's `fleet_tick` (`mur-daemon/src/fleet_tick.rs`, on the 30s action-tick cycle) can auto-run any fleet whose `loop.trigger` is due — `interval:<dur>` or `cron:<5-field POSIX expr>` (local tz; cron fleets are baseline-stamped on first sight so they fire on the next boundary, never spuriously on enable) — tracked via `~/.mur/fleets/<name>/.last_run`, each loop on an isolated thread + fresh runtime so the blocking router dial never stalls the daemon; reuses the Phase-2a guards. **Auto-run is OFF by default** — the safety gate `MUR_FLEET_AUTORUN=1` must be set to opt in (best-practice: no unattended autonomy without an explicit switch + enforced budget + kill-switch). Both `run`/`--loop`/auto-run pass `yes:false` (fail-closed; never blanket-approve risk-tiered steps). **Budget guard (Phase 3, shipped):** `run --loop --budget-usd X` (or `loop.budget_usd`) stops before cumulative cost exceeds the budget. Spend is now **REAL** — each turn's actual input+output tokens flow back via `Task.usage` → `PipelineOutput.tokens_used` (summed across delegate turns + retries) → `loop_run`; a 0-token iteration falls back to the projection so spend can never silently under-count (fail-safe). Rate from `MUR_FLEET_COST_PER_1K` env → else dearest `models.yaml` output rate → else a documented default. **Kill-switch (Phase 3, shipped):** `mur fleet stop <name>` writes a `~/.mur/fleets/<name>/.stopped` sentinel — a running `--loop` bails next iteration (`LoopStop::Stopped`), the daemon won't auto-run it, and manual `run` refuses; `mur fleet start <name>` clears it. **Auto-run now requires a positive budget** (`due_fleets` skips interval fleets with `budget_usd<=0`) — so the auto-run safety triad is complete: `MUR_FLEET_AUTORUN` switch + per-fleet budget + kill-switch. **Router planning (Phase 3, shipped):** the router emits a structured DAG each iteration (`cmd/fleet/plan.rs`; member + dependency + cycle validated) that routes work to the right members, **falling back to broadcast-to-all** on any absent/invalid plan (`run` and `--loop` both). **Cron trigger (shipped):** `loop.trigger: cron:<5-field POSIX>` (local tz; `mur-daemon/src/fleet_tick.rs` `is_due` cron branch + `establish_cron_baselines`, reusing `mur-agent-runtime::scheduler::next_fire_after`). **Scope skills (shipped):** `scope_visible` is live in both the CLI hook and the runtime injector; harvest stamps Project scope from the repo root; `mur skill scope <name> [--fleet|--project|--user]` authors scope; fleet-scoped skills inject for members via a **membership-verified** `active_fleet` (derived from the `fleet-<name>` channel id, gated on local fleet membership — fail-closed). **Real per-token accounting (shipped)** + **structured `done_when` (shipped):** `done_when: marker:<TEXT>` converges deterministically (member emits the marker as an own-line sentinel this run; no router LLM call). **Commander governance (shipped):** commander governance hooks (kill + budget-ceiling) via `mur commander`, honored by loop + daemon (fail-closed on Err), audited via `GovernanceState`. Remaining Phase 3 (refinements): team-shared fleets. Live `run`/`run --loop`/auto-run need running member agents (operator-tested). **Bundle sharing (Phase A, shipped):** `export <name> [--with-members]` / `import <file> [--force] [--no-members] [--yes]` — share a fleet `.fleet` bundle (signed tar.gz: fleet.yaml + fleet-scoped skills + optional member profiles); import verifies signature, security-scans skills, installs at lowest trust (peer TOFU), regenerates member identities locally (never copies the private key), never overwrites existing agents, never auto-runs. **Concurrent merge (P3 Phase 0, experimental, default OFF):** `mur fleet merge-concurrent <name> [--stats] [--promote] [--target <path>]` (requires `MUR_PARALLEL_CONCURRENT=1`) runs zero-dependency Model-A post-hoc N-way line merge across a parallel run's worktrees: disjoint hunks auto-merge, any overlapping region is reported and escalated (never silently merged), `--promote` refuses on unresolved overlaps and reverts on `cargo check` failure, `--stats` writes `concurrent_stats.json` (Spike-1 overlap rate that gates whether the Loro CRDT engine in Phase 1 is ever built). Guarantees deterministic order-independent convergence of merged bytes, NOT correctness. See `docs/superpowers/specs/2026-06-29-parallel-tracks-p3-concurrent-merge-design.md` and `docs/superpowers/validation/spike1-overlap-rate.md`. **Spike-1 DECIDED (observational — `mur-core/examples/spike1_history.rs` mines real git-merge history through the production classifier): 0.1% overlap → StructuralMerger is the final answer, the Loro CRDT (Phase 1) is shelved.** **Agent-triggered runs (`fleet_run` built-in tool, shipped):** a sandboxed agent (e.g. the murmur concierge) can trigger a guarded fleet run / `mur deep-research` WITHOUT `~/.mur` fs grants — the runtime built-in tool (`mur-agent-runtime/src/tools/fleet_run.rs`) spawns the `mur` CLI; deny-by-default on both axes via `~/.mur/config.yaml` `fleet_run: {agents: [...], fleets: [...]}` (global config = out of any agent's write reach), requires a positive `loop.budget_usd`, and the sandbox seal adds the narrow carve-ins (`fleets`/`commander`/`conversations` write + `mur` spawn) only for allowlisted agents (`sandbox/policy.rs`). Config change → agent restart to apply. **Tier 1 worktree execution (experimental, default OFF):** `MUR_PARALLEL_EXEC=1 mur fleet run <name>` on a fleet with a `parallel:` block creates one git worktree per track (`.worktrees/`, via the previously-dormant `create_tracks`), prompt-routes each delegate to work+commit inside its worktree using the bash tool's existing per-call `cwd` param (no runtime change), caps fan-out (`max_concurrency`), and leaves worktrees + `tracks.json` for the existing reconcilers (`merge`/`compare`/`merge-concurrent`). A best-effort collision guard reports any agent that strays into the main checkout (the signal that justifies Tier 2 runtime-enforced cwd). Goal: dogfood parallel Claude Code on real MUR work and measure the live overlap rate (the serial-history proxy can't see the parallel regime). See `docs/superpowers/plans/2026-06-30-parallel-tracks-tier1-worktree-execution.md`.

- `mur model {add|list|show|remove|migrate|prices|role}` — `~/.mur/models.yaml` provider/model registry. `add` accepts `--input-cost`/`--output-cost` (USD per 1k tokens) and auto-fills pricing + context window from the models.dev catalog unless `--no-fetch`; `prices {refresh|show}` manages the cached catalog (`~/.mur/cache/model-prices.json`). The Hub GUI **Model Library** (mur-hub-gui) connects cloud providers (key → Keychain) and auto-detects local runtimes, discovers models via `/v1/models`, and adds them as registry aliases. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md` and `2026-06-17-mur-model-library-design.md`.
- `mur official {list|install <id>}` — browse and install official MUR agents/fleets from the app.mur.run catalog. Install requires `mur login`; pro-tier items require an active subscription. Downloads carry an account-bound `OfficialLicense` (stored in `~/.mur/licenses/`); the fleet/agent import paths refuse official-marked bundles without a matching license (anti-sharing gate; expiry gates downloads only, never installed content). See `docs/superpowers/specs/2026-07-20-official-catalog-design.md`.
- `mur deep-research {setup|""} [question]` — simplified web research UX: `setup` (one-time wizard for model, workers, budget, egress), `""` (status panel), or ask a question (preflight start + guarded run). `provision`/`run` remain as the advanced flag-based path. Egress consent is explicit: `setup`/`provision --grant-egress` only.

For detailed agent / companion / GUI export / runtime internals, read `docs/architecture/runtime-overview.md` only when the task requires it.

## Development Notes

- Rust edition 2024 — `let` chains stable (`if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans live in `plans/` and `docs/superpowers/plans/`. OpenSpec change specs in `openspec/changes/`.
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

1. **Bump `Cargo.toml` workspace version FIRST** so `mur --version` matches the tag.
   The CI now validates this: pushing a tag whose version doesn't match `Cargo.toml` fails
   immediately at the `validate-version` job. Example:
   ```bash
   # Bump version before tagging:
   sed -i 's/^version = ".*"/version = "X.Y.Z"/' Cargo.toml
   git add Cargo.toml && git commit -m "chore(release): bump workspace version to X.Y.Z"
   ```
2. `git tag -a v<VERSION> -m "message" && git push origin main --tags`
3. Release workflow (`release.yml`) handles the rest automatically: cross-platform build,
   GitHub Release, Homebrew formula update, installer deployment, crates.io publish.
4. Verify: `brew update && brew upgrade mur`

## Documentation Checklist

When making changes, check whether these need updating:

1. **`README.md`** — `/Volumes/Firecuda4tb/Projects/mur/README.md`
2. **Documents page** — https://app.mur.run/docs/core
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/docs-content/`
   - Page component: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/docs/core/[[...slug]]/page.tsx`
   - Navigation: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/components/docs/coreNavigation.tsx`
3. **Product page** — https://app.mur.run/products/mur
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/products/mur/page.tsx`

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
