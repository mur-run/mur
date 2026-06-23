# mur-hub-gui

MuR Hub — the cross-agent desktop app. Tauri 2 + React + Vite.

In v1 this replaces `mur-agent-gui` as the single desktop entry point;
during the migration window both apps coexist (the design spec calls this
Phase 1 / Phase 2; see §9 of the design doc).

## Layout

- `src-tauri/` — Rust shell (standalone workspace, excluded from root
  `cargo build --workspace`). Mirrors mur-agent-gui's exclusion pattern.
- `ui/` — React 18 + Vite 5 frontend. Dev server runs on port `5174`
  so it can run alongside mur-agent-gui (5173) during migration.

## Build

```bash
cd mur-hub-gui/ui && npm ci && npm run build
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```

## Run (dev)

```bash
# Terminal 1
cd mur-hub-gui/ui && npm run dev
# Terminal 2
cargo tauri dev --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```

## Bundling (self-contained .app)

`tauri.conf.json` embeds three sidecars from `src-tauri/binaries/` —
`mur`, `mur-agent-runtime`, `mlx-server` — each suffixed with the target triple
(e.g. `mur-agent-runtime-aarch64-apple-darwin`); the release workflow builds
these via `scripts/build-mlx-server.sh`.

The default model is **not** bundled (slim build): the Hub downloads it on first
run into `~/.mur/models/`, or the user connects a cloud/local LLM. For a dev or
offline build that bakes the model into the `.app`, run
`scripts/fetch-bundle-model.sh mlx-community/Qwen3.5-2B-MLX-4bit` (populates
`src-tauri/resources/models/default/`) before bundling and re-add the
`resources/models/**/*` glob to `tauri.conf.json`.

## Design reference

`docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md`
