# MuR Hub Companion — M-h0 · Workspace Scaffold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land empty but compilable `mur-gui-core` (workspace lib) and `mur-hub-gui` (Tauri 2 app, workspace-excluded) crates, plus CI matrix coverage for mac + win, with zero behavior changes to existing crates.

**Architecture:** Greenfield scaffolding only — no runtime logic. `mur-gui-core` becomes the shared lib that later milestones populate with sidecar supervisor, A2A client, and companion bridge code (extracted from `mur-agent-gui`). `mur-hub-gui` is a standalone Tauri 2 app (own `[workspace]` directive, mirrors mur-agent-gui's exclusion pattern) that ultimately replaces mur-agent-gui as the desktop entry point. M-h0 ends when `cargo build --workspace` ignores Hub, the Hub builds via its own manifest, and the Hub binary launches a blank "MuR Hub" window.

**Tech Stack:** Rust edition 2024, Tauri 2, React 18 + Vite (TypeScript), tokio runtime, GitHub Actions CI matrix (ubuntu / macOS / windows).

**Spec reference:** `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1, §10 (row M-h0).

---

## File Structure

```
mur/
├── Cargo.toml                              [modify] members += "mur-gui-core"; exclude += "mur-hub-gui"
├── CLAUDE.md                               [modify] mention new crates in Architecture
├── .github/workflows/ci.yml                [modify] new hub_gui job; fmt step for hub-gui
│
├── mur-gui-core/                           [create] shared lib crate
│   ├── Cargo.toml
│   ├── README.md
│   └── src/lib.rs                          single placeholder unit test, no real logic yet
│
└── mur-hub-gui/                            [create] standalone Tauri 2 app (workspace-excluded)
    ├── README.md
    ├── src-tauri/
    │   ├── Cargo.toml                      standalone [workspace] directive
    │   ├── build.rs
    │   ├── tauri.conf.json
    │   ├── icons/icon.png                  copied from mur-agent-gui (placeholder)
    │   ├── capabilities/default.json
    │   └── src/
    │       ├── main.rs                     boots Tauri, calls lib::run()
    │       └── lib.rs                      single empty window labelled "MuR Hub"
    └── ui/
        ├── package.json
        ├── tsconfig.json
        ├── vite.config.ts
        ├── index.html
        ├── .nvmrc                          "20" (same as mur-agent-gui)
        └── src/
            ├── main.tsx
            └── App.tsx                     "MuR Hub" placeholder render
```

Each file has one responsibility. No multi-purpose modules in M-h0.

---

## Task 1: Create `mur-gui-core` lib crate skeleton

**Files:**
- Create: `mur-gui-core/Cargo.toml`
- Create: `mur-gui-core/README.md`
- Create: `mur-gui-core/src/lib.rs`

- [ ] **Step 1: Write `mur-gui-core/Cargo.toml`**

```toml
[package]
name = "mur-gui-core"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Shared GUI core library — sidecar supervisor, companion bridge, A2A client. Consumed by mur-hub-gui and (during migration) mur-agent-gui."

[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write `mur-gui-core/src/lib.rs` with a single passing unit test**

```rust
//! Shared GUI core library for mur Hub and mur-agent-gui.
//!
//! M-h0: empty skeleton. Later milestones populate:
//!   - `sidecar` — spawn/supervise mur-agent-runtime children
//!   - `companion_bridge` — filesystem watcher on companion/inbox/
//!   - `a2a` — A2A v0.3 unix-socket client
//!
//! See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_version_is_non_empty() {
        assert!(!CRATE_VERSION.is_empty());
    }
}
```

- [ ] **Step 3: Write `mur-gui-core/README.md`**

```markdown
# mur-gui-core

Shared GUI core library consumed by `mur-hub-gui` (the new MuR Hub desktop
app) and during migration also by `mur-agent-gui` (legacy per-agent app).

This crate hosts code that must not fork between the two GUIs:

- `sidecar` — spawn / supervise `mur-agent-runtime` child processes
- `companion_bridge` — debounced filesystem watcher on
  `~/.mur/agents/<name>/companion/inbox/`
- `a2a` — A2A v0.3 unix-socket client

M-h0 is the empty scaffold. Later milestones populate the modules above.

See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.
```

- [ ] **Step 4: Verify the crate doesn't build yet (workspace not wired)**

Run: `cargo check -p mur-gui-core`
Expected: `error: package ID specification ... did not match any packages` (crate not in workspace yet)

- [ ] **Step 5: Commit scaffold files**

```bash
git add mur-gui-core/Cargo.toml mur-gui-core/README.md mur-gui-core/src/lib.rs
git commit -m "scaffold(gui-core): add empty mur-gui-core crate skeleton (M-h0)"
```

---

## Task 2: Wire `mur-gui-core` into root workspace

**Files:**
- Modify: `Cargo.toml` (root, line 3-9 `members` list)

- [ ] **Step 1: Edit root `Cargo.toml` to add `mur-gui-core` to members**

Change lines 3-9 from:

```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
    "mur-daemon",
    # "mur-commander",  # separate crate — v0.2.0 released
]
```

to:

```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
    "mur-daemon",
    "mur-gui-core",
    # "mur-commander",  # separate crate — v0.2.0 released
]
```

- [ ] **Step 2: Verify the crate now builds**

Run: `cargo check -p mur-gui-core`
Expected: `Finished ... [unoptimized + debuginfo] target(s)` with zero warnings.

- [ ] **Step 3: Verify the placeholder test runs**

Run: `cargo test -p mur-gui-core`
Expected: `test result: ok. 1 passed`

- [ ] **Step 4: Verify workspace-wide build still succeeds**

Run: `cargo check --workspace`
Expected: zero new errors; all 5 workspace members compile.

- [ ] **Step 5: Commit workspace wiring**

```bash
git add Cargo.toml
git commit -m "chore(workspace): add mur-gui-core to members (M-h0)"
```

---

## Task 3: Scaffold `mur-hub-gui/src-tauri` Rust side

**Files:**
- Create: `mur-hub-gui/src-tauri/Cargo.toml`
- Create: `mur-hub-gui/src-tauri/build.rs`
- Create: `mur-hub-gui/src-tauri/tauri.conf.json`
- Create: `mur-hub-gui/src-tauri/capabilities/default.json`
- Create: `mur-hub-gui/src-tauri/icons/icon.png` (copy from mur-agent-gui)
- Create: `mur-hub-gui/src-tauri/src/main.rs`
- Create: `mur-hub-gui/src-tauri/src/lib.rs`

- [ ] **Step 1: Write `mur-hub-gui/src-tauri/Cargo.toml`**

```toml
[package]
name = "mur-hub-gui"
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/mur-run/mur"
description = "MuR Hub — cross-agent desktop app (Tauri 2). Replaces mur-agent-gui in v1."

# Mark as a standalone workspace so cargo doesn't walk past the root
# Cargo.toml and accidentally attach to the main workspace, which excludes
# this crate. Mirrors mur-agent-gui/src-tauri's exclusion pattern.
[workspace]

[lib]
name = "mur_hub_gui_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[[bin]]
name = "mur-hub-gui"
path = "src/main.rs"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon", "image-png"] }
tauri-plugin-shell = "2"

# Workspace siblings — reused from the root repo via relative path.
mur-common = { path = "../../mur-common" }
mur-core = { path = "../../mur-core" }
mur-gui-core = { path = "../../mur-gui-core" }

serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "signal", "sync"] }

[features]
default = ["custom-protocol"]
custom-protocol = ["tauri/custom-protocol"]
```

- [ ] **Step 2: Write `mur-hub-gui/src-tauri/build.rs`**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 3: Write `mur-hub-gui/src-tauri/tauri.conf.json`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "MuR Hub",
  "version": "0.1.0",
  "identifier": "run.mur.hub",
  "build": {
    "devUrl": "http://localhost:5174",
    "frontendDist": "../ui/dist"
  },
  "app": {
    "windows": [
      {
        "label": "dashboard",
        "title": "MuR Hub",
        "width": 720,
        "height": 520,
        "minWidth": 560,
        "minHeight": 400,
        "resizable": true,
        "fullscreen": false,
        "visible": true
      }
    ],
    "security": {
      "csp": "default-src 'self'; img-src 'self' asset: data:; style-src 'self' 'unsafe-inline'; script-src 'self'"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["app"],
    "icon": [
      "icons/icon.png"
    ],
    "macOS": {
      "minimumSystemVersion": "12.0"
    },
    "windows": {
      "webviewInstallMode": { "type": "downloadBootstrapper" }
    }
  }
}
```

Dev port `5174` differs from mur-agent-gui's `5173` so both can run concurrently during the parallel migration phase.

- [ ] **Step 4: Write `mur-hub-gui/src-tauri/capabilities/default.json`**

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Default capability set for MuR Hub. M-h0 starts with shell/open only; later milestones add fs, dialog, autostart, etc.",
  "windows": ["dashboard"],
  "permissions": [
    "core:default",
    "shell:allow-open"
  ]
}
```

- [ ] **Step 5: Copy placeholder icon from mur-agent-gui**

```bash
mkdir -p mur-hub-gui/src-tauri/icons
cp mur-agent-gui/src-tauri/icons/icon.png mur-hub-gui/src-tauri/icons/icon.png
```

The icon is a placeholder; bespoke Hub branding lands in M-h8 polish.

- [ ] **Step 6: Write `mur-hub-gui/src-tauri/src/main.rs`**

```rust
// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mur_hub_gui_lib::run()
}
```

- [ ] **Step 7: Write `mur-hub-gui/src-tauri/src/lib.rs`**

```rust
//! MuR Hub — Tauri 2 desktop app.
//!
//! M-h0: boots a single empty dashboard window. Real popover, multi-agent
//! discovery, and pet windows arrive in later milestones (see
//! `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md`).

use tracing_subscriber::EnvFilter;

pub fn run() {
    init_tracing();
    tracing::info!(version = mur_gui_core::CRATE_VERSION, "starting mur-hub-gui");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn lib_links() {
        // Smoke test: the crate compiles and the lib symbol exists.
        // (We cannot actually run tauri::Builder in a unit test without a
        //  windowing context — that lives in a later integration test.)
        let _ = super::init_tracing;
    }
}
```

- [ ] **Step 8: Verify Rust side compiles**

Run: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: success (after Tauri downloads its build deps on first run, ~30-60s).

Note: this fails until Task 4 produces `mur-hub-gui/ui/dist/`. Tauri's build script reads `frontendDist` and warns but does not error if the directory is missing during `cargo check`. If it errors, mkdir an empty placeholder dist and re-run.

- [ ] **Step 9: Run the placeholder unit test**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib`
Expected: `test result: ok. 1 passed`

- [ ] **Step 10: Commit Rust scaffold**

```bash
git add mur-hub-gui/src-tauri/Cargo.toml mur-hub-gui/src-tauri/build.rs \
        mur-hub-gui/src-tauri/tauri.conf.json \
        mur-hub-gui/src-tauri/capabilities/default.json \
        mur-hub-gui/src-tauri/icons/icon.png \
        mur-hub-gui/src-tauri/src/main.rs \
        mur-hub-gui/src-tauri/src/lib.rs
git commit -m "scaffold(hub-gui): Tauri 2 Rust shell with empty dashboard window (M-h0)"
```

---

## Task 4: Scaffold `mur-hub-gui/ui` React + Vite frontend

**Files:**
- Create: `mur-hub-gui/ui/package.json`
- Create: `mur-hub-gui/ui/tsconfig.json`
- Create: `mur-hub-gui/ui/tsconfig.node.json`
- Create: `mur-hub-gui/ui/vite.config.ts`
- Create: `mur-hub-gui/ui/index.html`
- Create: `mur-hub-gui/ui/.nvmrc`
- Create: `mur-hub-gui/ui/src/main.tsx`
- Create: `mur-hub-gui/ui/src/App.tsx`
- Create: `mur-hub-gui/ui/src/styles.css`

- [ ] **Step 1: Write `mur-hub-gui/ui/.nvmrc`**

```
20
```

Same Node major as mur-agent-gui.

- [ ] **Step 2: Write `mur-hub-gui/ui/package.json`**

```json
{
  "name": "mur-hub-ui",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "preview": "vite preview"
  },
  "dependencies": {
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-shell": "^2",
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "@types/react": "^18.3.3",
    "@types/react-dom": "^18.3.0",
    "@vitejs/plugin-react": "^4.3.1",
    "typescript": "^5.5.3",
    "vite": "^5.3.1"
  }
}
```

- [ ] **Step 3: Write `mur-hub-gui/ui/tsconfig.json`**

```json
{
  "compilerOptions": {
    "target": "ES2020",
    "useDefineForClassFields": true,
    "lib": ["ES2020", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true
  },
  "include": ["src"],
  "references": [{ "path": "./tsconfig.node.json" }]
}
```

- [ ] **Step 4: Write `mur-hub-gui/ui/tsconfig.node.json`**

```json
{
  "compilerOptions": {
    "composite": true,
    "skipLibCheck": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "allowSyntheticDefaultImports": true,
    "strict": true
  },
  "include": ["vite.config.ts"]
}
```

- [ ] **Step 5: Write `mur-hub-gui/ui/vite.config.ts`**

```typescript
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Hub uses port 5174 so it can run alongside mur-agent-gui (5173) during
// the parallel migration phase (M-h0 → M-h7).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
  },
  build: {
    target: "es2020",
    minify: "esbuild",
    sourcemap: false,
  },
});
```

- [ ] **Step 6: Write `mur-hub-gui/ui/index.html`**

```html
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>MuR Hub</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 7: Write `mur-hub-gui/ui/src/main.tsx`**

```typescript
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
```

- [ ] **Step 8: Write `mur-hub-gui/ui/src/App.tsx`**

```typescript
export default function App() {
  return (
    <main className="hub-shell">
      <h1>MuR Hub</h1>
      <p className="subtitle">Multi-agent dashboard — M-h0 scaffold.</p>
    </main>
  );
}
```

- [ ] **Step 9: Write `mur-hub-gui/ui/src/styles.css`**

```css
:root {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  color-scheme: light dark;
}

body {
  margin: 0;
  display: grid;
  place-items: center;
  min-height: 100vh;
  background: linear-gradient(135deg, #f7f5ef 0%, #ece5d6 100%);
}

.hub-shell {
  padding: 32px;
  text-align: center;
}

.hub-shell h1 {
  margin: 0 0 8px;
  font-size: 28px;
  font-weight: 600;
}

.hub-shell .subtitle {
  margin: 0;
  font-size: 13px;
  opacity: 0.6;
}
```

- [ ] **Step 10: Install npm deps and build the UI**

Run:
```bash
cd mur-hub-gui/ui && npm install && npm run build
```
Expected: `dist/` directory created with `index.html` + bundled JS.

- [ ] **Step 11: Re-run Rust check now that `frontendDist` exists**

Run: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: success without the previous `frontendDist` warning.

- [ ] **Step 12: Commit UI scaffold**

```bash
git add mur-hub-gui/ui/.nvmrc mur-hub-gui/ui/package.json \
        mur-hub-gui/ui/package-lock.json \
        mur-hub-gui/ui/tsconfig.json mur-hub-gui/ui/tsconfig.node.json \
        mur-hub-gui/ui/vite.config.ts mur-hub-gui/ui/index.html \
        mur-hub-gui/ui/src/main.tsx mur-hub-gui/ui/src/App.tsx \
        mur-hub-gui/ui/src/styles.css
git commit -m "scaffold(hub-gui/ui): React + Vite shell with 'MuR Hub' placeholder (M-h0)"
```

---

## Task 5: Add `mur-hub-gui` to workspace exclusion + write README + `.gitignore`

**Files:**
- Modify: `Cargo.toml` (root, lines 10-20 `exclude` list)
- Create: `mur-hub-gui/README.md`
- Create: `mur-hub-gui/.gitignore`

- [ ] **Step 1: Edit root `Cargo.toml` `exclude` list**

Change lines 10-20 from:

```toml
exclude = [
    # Tauri 2 GUI shell. Excluded from the workspace so default
    # `cargo build --workspace` does not pull WebKitGTK / Cocoa /
    # WebView2 toolchains. Built only by `mur agent export --format gui`
    # and the GUI-relevant CI job.
    "mur-agent-gui",
    # cargo-fuzz harness. Excluded because libfuzzer-sys requires a
    # nightly toolchain and the fuzz binaries are not part of normal
    # dev/CI builds. Run with: cargo +nightly fuzz run <target>
    "fuzz",
]
```

to:

```toml
exclude = [
    # Tauri 2 GUI shells. Excluded from the workspace so default
    # `cargo build --workspace` does not pull WebKitGTK / Cocoa /
    # WebView2 toolchains. Built via their own manifests and CI jobs.
    "mur-agent-gui",
    "mur-hub-gui",
    # cargo-fuzz harness. Excluded because libfuzzer-sys requires a
    # nightly toolchain and the fuzz binaries are not part of normal
    # dev/CI builds. Run with: cargo +nightly fuzz run <target>
    "fuzz",
]
```

- [ ] **Step 2: Verify `cargo build --workspace` does NOT compile `mur-hub-gui`**

Run: `cargo build --workspace 2>&1 | grep -E "(Compiling|Finished)" | head -20`
Expected: `Compiling mur-hub-gui` is absent. `Compiling mur-gui-core` is present. Workspace finishes.

- [ ] **Step 3: Write `mur-hub-gui/README.md`**

```markdown
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
```

- [ ] **Step 4: Write `mur-hub-gui/.gitignore`**

```gitignore
# Rust
target/

# UI
ui/dist/
ui/node_modules/

# Tauri build artifacts
src-tauri/gen/
```

- [ ] **Step 5: Commit exclusion + docs**

```bash
git add Cargo.toml mur-hub-gui/README.md mur-hub-gui/.gitignore
git commit -m "chore(workspace): exclude mur-hub-gui from cargo workspace (M-h0)"
```

---

## Task 6: Add CI `hub_gui` job + fmt step

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add `hub_gui` paths filter**

In `.github/workflows/ci.yml` around line 60-65, after the `gui:` filter block, add:

```yaml
            hub_gui:
              - 'mur-hub-gui/**'
              - 'mur-gui-core/**'
              - 'mur-agent-runtime/**'
              - 'mur-core/**'
              - 'mur-common/**'
              - '.github/workflows/ci.yml'
```

And add `hub_gui` to the `outputs` block at line 29-32:

```yaml
    outputs:
      code: ${{ steps.filter.outputs.code }}
      gui:  ${{ steps.filter.outputs.gui }}
      hub_gui: ${{ steps.filter.outputs.hub_gui }}
      e2e:  ${{ steps.filter.outputs.e2e }}
```

- [ ] **Step 2: Add fmt step for `mur-hub-gui` in the `fmt` job**

In `.github/workflows/ci.yml` around line 154-165, after the existing `fmt mur-agent-gui` step, add:

```yaml
      - name: fmt mur-hub-gui (workspace-excluded)
        run: cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- --check
```

- [ ] **Step 3: Add the `hub_gui` job (multi-OS matrix)**

After the `gui:` job (around line 220) and before `e2e_quick:`, add a new job:

```yaml
  hub_gui:
    name: Hub GUI crate (mur-hub-gui) — ${{ matrix.os }}
    needs: changes
    if: github.event_name == 'push' || needs.changes.outputs.hub_gui == 'true'
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-22.04, macos-latest, windows-latest]
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: mur-hub-gui/src-tauri
      - uses: actions/setup-node@v4
        with:
          node-version-file: 'mur-hub-gui/ui/.nvmrc'
          cache: 'npm'
          cache-dependency-path: 'mur-hub-gui/ui/package-lock.json'
      - name: Install Linux system libraries (Tauri 2)
        if: runner.os == 'Linux'
        uses: awalsh128/cache-apt-pkgs-action@latest
        with:
          version: 1.0
          packages: >-
            libwebkit2gtk-4.1-dev
            libsoup-3.0-dev
            libayatana-appindicator3-dev
            librsvg2-dev
            pkg-config
            protobuf-compiler
            libprotobuf-dev
      - name: Install protobuf (macOS)
        if: runner.os == 'macOS'
        run: brew install protobuf
      - name: Install protobuf (Windows)
        if: runner.os == 'Windows'
        run: choco install protoc -y
      - name: Install npm deps
        run: cd mur-hub-gui/ui && npm ci
      - name: Frontend build
        run: cd mur-hub-gui/ui && npm run build
      - name: cargo check
        run: cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
      - name: cargo test --lib
        run: cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib
      - name: cargo clippy
        run: cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings
```

- [ ] **Step 4: Add `hub_gui` to the `ci-pass` aggregator job**

Modify the `needs:` list around line 253:

```yaml
    needs: [changes, test, clippy, fmt, gui, hub_gui, e2e_quick, required-reason-apis]
```

And add a corresponding check block before the final success echo (around line 280):

```yaml
          if [[ "${{ needs.hub_gui.result }}" != "success" && "${{ needs.hub_gui.result }}" != "skipped" ]]; then
            echo "::error::hub_gui job failed: ${{ needs.hub_gui.result }}"; exit 1
          fi
```

And update the `for job in` echo line around line 261 to include `hub_gui`:

```yaml
          for job in test clippy fmt gui hub_gui e2e_quick required-reason-apis; do
```

- [ ] **Step 5: Validate workflow YAML syntax locally**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo OK`
Expected: `OK`

(If python yaml isn't available, use `yq '.' .github/workflows/ci.yml > /dev/null` or any local linter.)

- [ ] **Step 6: Verify fmt step passes on the new crate**

Run: `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- --check`
Expected: exits 0 with no diff.

- [ ] **Step 7: Commit CI changes**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add hub_gui job (mac/win/linux matrix) + fmt step for mur-hub-gui (M-h0)"
```

---

## Task 7: Update `CLAUDE.md` Architecture section

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Locate the Architecture section listing the three crates**

Find the block (around line 38 in current `CLAUDE.md`) that reads:

```markdown
Cargo workspace with three crates:

- **`mur-common`** — Shared types only. No logic, no I/O. ...
- **`mur-core`** — All CLI logic and the `mur` binary. ...
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). ...
```

- [ ] **Step 2: Replace with the updated crate list including the two new ones**

```markdown
Cargo workspace with five crates plus two workspace-excluded Tauri apps:

- **`mur-common`** — Shared types only. No logic, no I/O. `Pattern`, `KnowledgeBase`, `Workflow`, `Config`, `MurEvent`, plus `AgentProfile`/`LockFile`/A2A envelopes/telemetry constants.
- **`mur-core`** — All CLI logic and the `mur` binary. Modules map to the four-stage pipeline. Hosts `mur agent ...` user-facing subcommands.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). Crate README has the walkthrough.
- **`mur-daemon`** — Long-running background daemon binary.
- **`mur-gui-core`** — Shared GUI library (sidecar supervisor, companion bridge, A2A client). Consumed by `mur-hub-gui` and during migration also by `mur-agent-gui`. See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.

Workspace-excluded Tauri 2 GUI apps (built via their own manifests so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2):

- **`mur-agent-gui`** — Per-agent `.app` shell (legacy; deprecated in M-h8).
- **`mur-hub-gui`** — MuR Hub cross-agent desktop app (in development; replaces `mur-agent-gui` in v1).
```

- [ ] **Step 3: Commit CLAUDE.md update**

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): note mur-gui-core and mur-hub-gui crates (M-h0)"
```

---

## Task 8: End-to-end verification + Hub launch smoke test

**Files:** (none modified — verification only)

- [ ] **Step 1: Re-run full workspace build to confirm no regression**

Run: `cargo build --workspace --release`
Expected: success; binaries for mur-common, mur-core, mur-agent-runtime, mur-daemon, mur-gui-core present under `target/release/`. Hub is NOT built.

- [ ] **Step 2: Re-run full workspace test suite**

Run: `cargo nextest run --workspace`
Expected: all existing tests pass; new `mur-gui-core::tests::crate_version_is_non_empty` passes.

- [ ] **Step 3: Build Hub end-to-end**

Run:
```bash
cd mur-hub-gui/ui && npm run build && cd ..
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --release
```
Expected: produces `mur-hub-gui/src-tauri/target/release/mur-hub-gui` (mac/linux) or `mur-hub-gui.exe` (Windows).

- [ ] **Step 4: Launch the Hub binary and verify the empty window appears**

Run: `mur-hub-gui/src-tauri/target/release/mur-hub-gui`
Expected: a single 720×520 window titled "MuR Hub" opens showing "MuR Hub" heading and "Multi-agent dashboard — M-h0 scaffold." subtitle. Close the window with ⌘W / Alt-F4.

This is a manual step — record success/failure in the commit message body of the final commit. CI does not run a windowed binary at this stage.

- [ ] **Step 5: Run clippy on the new Hub crate**

Run: `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings`
Expected: zero warnings.

- [ ] **Step 6: Run fmt check on both new crates**

Run:
```bash
cargo fmt -p mur-gui-core -- --check
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- --check
```
Expected: both exit 0.

- [ ] **Step 7: Final smoke commit (empty — marks M-h0 completion)**

```bash
git commit --allow-empty -m "$(cat <<'EOF'
chore(m-h0): MuR Hub workspace scaffold — milestone complete

Verified locally:
- cargo build --workspace succeeds, does NOT pull mur-hub-gui
- mur-gui-core unit test passes (1/1)
- mur-hub-gui builds via own manifest, launches empty 720x520 window
- clippy/fmt clean on both new crates

Next: M-h1 — Hub UI shell with multi-agent discovery from ~/.mur/agents/*.
EOF
)"
```

- [ ] **Step 8: Push and verify CI green**

```bash
git push
```
Watch the GitHub Actions run: `test`, `clippy`, `fmt`, `gui`, `hub_gui` (× 3 OSes), `e2e_quick`, `required-reason-apis` all green or skipped. The `ci-pass` aggregator must report success.

---

## Risks specific to M-h0

| Risk | Mitigation |
|------|-----------|
| Tauri 2 toolchain on Windows runner missing WebView2 SDK | `webviewInstallMode: downloadBootstrapper` in tauri.conf.json defers WebView2 acquisition to runtime; CI `cargo check` does not need it. |
| `protoc` missing on a CI runner | Job installs protobuf-compiler / brew protobuf / choco protoc; mirrors existing `gui` job exactly. |
| Workspace exclusion ordering matters in some cargo versions | Verified locally in Step 5.2 — both apps appear before fuzz; cargo 1.78+ handles either order. |
| Vite 5 + React 18 minor drift from mur-agent-gui's stack | Hub deliberately starts with minimal deps; future tailwind/etc. can be added in M-h1 without breaking M-h0. |
| Tauri config requires `frontendDist` to exist for build | Task 4 builds `ui/dist/` before Task 5's final cargo check; Step 3.8 calls this out explicitly. |

## What M-h0 deliberately does NOT include

- Tray icon — arrives in M-h1.
- Popover window — arrives in M-h1.
- Any A2A wiring — `mur-gui-core` has no logic yet beyond a version constant; supervisor / bridge / a2a modules land in M-h2.
- Voice (cpal/whisper/ort), notifications, multimodal decoder — these stay in `mur-agent-gui` for now and only port over when their consuming feature ships in a later Hub milestone.
- Removing `mur-agent-gui` — it stays untouched until M-h8.

## Definition of Done

- [ ] All 8 tasks above show every checkbox ticked in the executed plan.
- [ ] `cargo build --workspace` and `cargo nextest run --workspace` succeed locally and on CI.
- [ ] `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --release` succeeds.
- [ ] Launching the Hub binary produces a single 720×520 empty window titled "MuR Hub" on the developer's local machine.
- [ ] CI's `hub_gui` job is green on ubuntu-22.04, macos-latest, and windows-latest.
- [ ] `CLAUDE.md` Architecture section lists 5 workspace members + 2 excluded Tauri apps.

After M-h0 lands, the next plan to write is **M-h1 · Hub UI shell** (popover + dashboard window + multi-agent discovery from `~/.mur/agents/*/agent.yaml`).
