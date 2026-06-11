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

LanceDB vector index is always rebuildable via `mur reindex`.

## CLI Surface (top level)

- `mur verify [--file path] [--all]` — scan docs for stale claims (paths, commands, code refs)
- `mur agent <subcommand>` — manage murmur agents (create / list / status / send / card / cli / export / doctor / prompt / mcp / skill / perm / secret / companion / rekey / schedule). `cli <name>...` opens an interactive streaming TUI chat with a running agent (`--resume` to continue the last conversation); multiple names open one multiplexer pane per agent (tmux primary; zellij/WezTerm/kitty auto-detected). The `murmur` symlink is the quick form: `murmur a1 a2 a3` ≡ `mur agent cli a1 a2 a3`; bare `murmur` opens the concierge. Full surface in `docs/architecture/runtime-overview.md`.
- `mur model {add|list|show|remove|migrate}` — `~/.mur/models.yaml` provider/model registry. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`.

For detailed agent / companion / GUI export / runtime internals, read `docs/architecture/runtime-overview.md` only when the task requires it.

## Development Notes

- Rust edition 2024 — `let` chains stable (`if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans live in `plans/` and `docs/superpowers/plans/`. OpenSpec change specs in `openspec/changes/`.

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
   - Page component: `dashboard/src/app/docs/core/[[...slug]]/page.tsx`
   - Navigation: `dashboard/src/components/docs/coreNavigation.tsx`
3. **Product page** — https://app.mur.run/products/core
   - Source: `dashboard/src/app/products/core/page.tsx`

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
