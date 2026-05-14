# mur-agent-gui [legacy]

> **Deprecated as of M-h8.** Use [mur-hub-gui](../mur-hub-gui) instead.
> Run `mur agent migrate-to-hub` to move your agents to the new Hub.
> This crate is in maintenance mode and will be removed in v2.

Tauri 2 desktop shell for a single mur agent. Produced (one-shot, per-agent) by `mur agent export --format gui`.

> Spec: `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`
> Plan: `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan.md`

## Layout

```
src-tauri/
├── Cargo.toml              ← Tauri 2 main binary
├── tauri.conf.json         ← bundle template; rewritten per export
├── capabilities/main.json  ← Tauri 2 permission allowlist
├── entitlements.plist      ← macOS hardened-runtime entitlements
├── build.rs                ← embeds Tauri build context
├── src/
│   ├── main.rs             ← Tauri main + sidecar manager + Tauri commands
│   ├── commands.rs         ← #[tauri::command] handlers backed by mur_core::agent_admin
│   └── theme.rs            ← theme loader + appearance subscriber
└── themes/                 ← 5 built-in themes (light / dark / high-contrast / solarized / cyberpunk)
ui/                         ← React 18 + Vite + Tailwind 4 + shadcn/ui frontend
├── package.json
├── tsconfig.json
├── vite.config.ts
├── tailwind.config.ts
├── index.html
└── src/
    ├── main.tsx
    ├── App.tsx
    ├── tabs/{Status,Prompt,Skills,Mcp,Permissions,Identity}.tsx
    └── lib/api.ts          ← Tauri `invoke()` wrappers
```

## Workspace exclusion

This crate is **excluded** from the root `Cargo.toml` `[workspace]` members list. Reason: the Tauri toolchain pulls in WebKitGTK / Cocoa / WebView2 dependencies that would slow ordinary `cargo build --workspace` to a crawl. It is built by:

- `mur agent export --format gui` (per-agent export pipeline, P1.7)
- The GUI-relevant CI job in `scripts/templates/agent-export-multi-platform.yml` (P1.8)
- Local development: `cargo tauri dev` from `mur-agent-gui/src-tauri/`

## Status (2026-04-29)

P1.2 complete: scaffold checked in. The crate compiles with `cargo check` from inside `mur-agent-gui/src-tauri/`. Tauri commands stub out — they will be wired to `mur_core::agent_admin::*` in P1.3.

To run locally, install Tauri CLI first:

```bash
cargo install tauri-cli --version '^2.0' --locked
cd mur-agent-gui/ui && npm ci
cd ../src-tauri && cargo tauri dev
```

`mur agent doctor --format gui` enumerates the prereqs.
