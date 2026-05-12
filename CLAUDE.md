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

Cargo workspace with three crates:

- **`mur-common`** — Shared types only. No logic, no I/O. `Pattern`, `KnowledgeBase`, `Workflow`, `Config`, `MurEvent`, plus `AgentProfile`/`LockFile`/A2A envelopes/telemetry constants.
- **`mur-core`** — All CLI logic and the `mur` binary. Modules map to the four-stage pipeline. Hosts `mur agent ...` user-facing subcommands.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). Crate README has the walkthrough.

### Four-Stage Pipeline

```
capture/ → store/ → retrieve/ → inject/
                ↕
            evolve/
```

- **`capture/`** — Noise filter, significance scoring, emergence detection, feedback extraction
- **`store/`** — `YamlStore` (source of truth, atomic writes), `LanceDbStore` (vector index, always rebuildable), `WorkflowYamlStore`. Vector store abstracted via `store::vector::VectorStore` (`LanceDbStore` now; `QdrantStore` P1.3).
- **`retrieve/`** — `score_and_rank_hybrid()` combines vector similarity (0.7) + keyword BM25 (0.3); applies recency / effectiveness / importance / decay / length normalization
- **`inject/`** — `hook.rs` formats patterns; `sync.rs` writes tool-specific configs (Claude Code hooks, Gemini CLI, etc.)
- **`evolve/`** — Decay, maturity lifecycle (Draft→Emerging→Stable→Canonical), feedback, co-occurrence, pattern linking, emergence

Sources pipeline: `mur-core/src/sources/` adapters (Obsidian / Notion / Joplin) feed the same retrieve pipeline as patterns. See `docs/architecture/runtime-overview.md`.

### Key Data Model

`Pattern` wraps `KnowledgeBase` via `#[serde(flatten)]` — flat YAML, no nested `base:` key. `Pattern::deref()` forwards to `KnowledgeBase`, so `pattern.name` works directly.

`KnowledgeBase` fields: `name`, `description`, `content` (dual-layer: `technical` + `principle`), `tier` (session/project/core), `importance`, `confidence`, `tags`, `applies`, `evidence`, `links`, `lifecycle`, `maturity`, `decay`.

Tier half-lives: session=14d, project=90d, core=365d. Scoring floor 0.35. Max patterns/query 5. Max tokens ~2000.

### Data Storage (Runtime)

All data at `~/.mur/`:

- `patterns/*.yaml` — source of truth
- `workflows/*.yaml` — multi-step workflow definitions
- `session/active.json` — current session state
- `session/recordings/<id>.jsonl` — append-only event log
- `config.yaml` — user config

LanceDB vector index is always rebuildable via `mur reindex`.

## CLI Surface (top level)

- `mur verify [--file path] [--all]` — scan docs for stale claims (paths, commands, code refs)
- `mur agent <subcommand>` — manage murmur agents (create / list / status / send / card / export / doctor / prompt / mcp / skill / perm / secret / companion / rekey / schedule). Full surface in `docs/architecture/runtime-overview.md`.
- `mur model {add|list|show|remove|migrate}` — `~/.mur/models.yaml` provider/model registry. See `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`.

For detailed agent / companion / GUI export / runtime internals, read `docs/architecture/runtime-overview.md` only when the task requires it.

## Development Notes

- Rust edition 2024 — `let` chains stable (`if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans live in `plans/` and `docs/superpowers/plans/`. OpenSpec change specs in `openspec/changes/`.

## Release Process

After tagging a new release:

1. Bump workspace version FIRST in a release-prep PR so `workspace.version` matches the tag.
2. `git tag -a v<VERSION> -m "message" && git push origin main --tags`
3. Update Homebrew tap formula (`mur-run/homebrew-tap`):
   - `curl -sL https://github.com/mur-run/mur/archive/refs/tags/v<VERSION>.tar.gz | shasum -a 256`
   - Edit `Formula/mur.rb`: update `url` (new tag) and `sha256`. Commit, push.
4. Verify: `brew update && brew upgrade mur`

Pushing a git tag does NOT auto-update Homebrew.

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
