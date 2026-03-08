# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build (without embedded dashboard — fallback placeholder page)
cargo build --workspace
cargo build --release

# Build with embedded web dashboard (RECOMMENDED for releases)
# Must build mur-web first, then point MUR_WEB_DIST to its dist/
cd ~/Projects/mur-web && npm run build
MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build --release
# Or use the convenience script:
./build.sh

# Test (all crates)
cargo test --workspace

# Run a single test
cargo test --workspace <test_name>
cargo test -p mur-core <test_name>

# Lint
cargo clippy --workspace -- -D warnings
cargo fmt --check

# Run locally
cargo run -- <command>          # e.g. cargo run -- search "swift testing"
cargo run --release -- <command>
```

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

- **`server.rs`** — Axum-based local API server (Phase 0 feature)
- **`community.rs`** — Community pattern browser
- **`dashboard.rs`** — Terminal overview
- **`interactive.rs`** — `dialoguer`-powered interactive pattern creation
- **`migrate/`** — v1 (Go flat YAML) → v2 schema migration
- **`auth.rs`** — Trust levels for community patterns

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
