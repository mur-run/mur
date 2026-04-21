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

Cargo workspace with two crates:

- **`mur-common`** — Shared types only. No logic, no I/O. `Pattern`, `KnowledgeBase`, `Workflow`, `Config`, `MurEvent`. Both crates depend on this.
- **`mur-core`** — All CLI logic and the `mur` binary. Structured as modules that map to the four-stage pipeline.

### Four-Stage Pipeline

```
capture/ → store/ → retrieve/ → inject/
                ↕
            evolve/
```

**Sources pipeline (P1.3 — Unified retrieve + Qdrant + tantivy BM25; Notion/Joplin arrive P1.4):** An alternate input to `store/` lives in `mur-core/src/sources/` — `KnowledgeSource` adapters pull documents from external note apps (Obsidian, Notion, Joplin) into the same retrieve pipeline as patterns. The vector store is abstracted behind `store::vector::VectorStore` (impls: `LanceDbStore` now; `QdrantStore` P1.3). See `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md`.

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
2. **文件網站 (Docs)** — `https://app.mur.run/docs/core`
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/docs-content/` (Markdown files)
   - Page component: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/docs/core/[[...slug]]/page.tsx`
   - Navigation: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/components/docs/coreNavigation.tsx`
3. **產品網站 (Product page)** — `https://app.mur.run/products/core`
   - Source: `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/src/app/products/core/page.tsx`
