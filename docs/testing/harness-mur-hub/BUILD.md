# MUR Hub — Build & Launch on this mac

## Toolchain (verified 2026-06-04)

- node v22.22.0 / npm 10.9.4 (.nvmrc asks 20; v22 builds fine)
- rustc/cargo 1.95.0 (edition 2024)
- `cargo tauri` CLI: NOT preinstalled → `cargo install tauri-cli --version "^2" --locked`
- Host target triple: `aarch64-apple-darwin`

## externalBin sidecars

`mur-hub-gui/src-tauri/binaries/` ships **0-byte placeholders** for:
`mur`, `mur-agent-runtime`, `mlx-server` (× target triples). The release workflow
populates them. For local functional testing:
- Build real `mur` + `mur-agent-runtime` from this workspace (debug is fine) and copy
  over the aarch64 placeholders so the seeded agent can actually start.
- `mlx-server` stays 0B (release-only PyInstaller binary + model; can't build locally).
  Sidecar spawn fails gracefully (non-fatal); local inference unavailable in dev build.

## Build steps

```bash
WT=/Volumes/Firecuda4tb/Projects/mur/.claude/worktrees/hub-harness-test
cd "$WT"

# 1. real sidecars (debug)
cargo build --bin mur --bin mur-agent-runtime
cp target/debug/mur                 mur-hub-gui/src-tauri/binaries/mur-aarch64-apple-darwin
cp target/debug/mur-agent-runtime   mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin

# 2. frontend
cd mur-hub-gui/ui && npm install && npm run build && cd "$WT"

# 3. bundle the .app (debug profile = faster than release, still bundles resources)
cargo tauri build --debug --no-bundle=false --config mur-hub-gui/src-tauri/tauri.conf.json \
  || (cd mur-hub-gui/src-tauri && ~/.cargo/bin/cargo-tauri build --debug)
# Output: mur-hub-gui/src-tauri/target/debug/bundle/macos/MUR Hub.app
```

NOTE: resources (`mur-agent-template`, `models`) only resolve via `BaseDirectory::Resource`
inside a bundled `.app`. A plain `cargo build` binary will NOT find the template, so seed
testing REQUIRES the bundled .app (or `cargo tauri dev`, which stages resources).

## Launch for testing (SANDBOXED — never touch real ~/.mur)

```bash
export MUR_HOME=/tmp/hub-harness/mur            # isolated; protects real 7 agents
export RUST_LOG=debug
APP="mur-hub-gui/src-tauri/target/debug/bundle/macos/MUR Hub.app"
"$APP/Contents/MacOS/mur-hub-gui" >/tmp/hub-harness/hub.log 2>&1 &
```

- Empty MUR_HOME → seed_if_empty seeds Mur (happy path).
- Pre-populate MUR_HOME/agents/<dummy> → reproduces "no Mur" bug (current behavior).

## Driving the app

- Visual: AppleScript to open dashboard (tray app) + `screencapture` window shots.
- Backend: inspect `hub.log` tracing + resulting filesystem under sandbox MUR_HOME.
