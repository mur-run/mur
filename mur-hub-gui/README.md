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

## Design reference

`docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md`
