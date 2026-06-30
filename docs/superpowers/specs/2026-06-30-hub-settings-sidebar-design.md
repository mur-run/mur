# MUR Hub Settings Sidebar — Design

**Date:** 2026-06-30
**Status:** Approved (brainstorming) → ready for implementation plan
**Crate/app:** `mur-hub-gui` (Tauri 2 desktop app), UI under `mur-hub-gui/ui/`

## Problem

The Hub's Settings is a single 460px modal (`ui/src/components/SettingsModal.tsx`)
with three flat vertical sections — Appearance (language), Models (default-model
badge), Import (import agent/preset). As Hub-global settings grow, a flat modal
doesn't scale and buries functionality that today lives scattered in the tray
(CLI install, version-skew banner). We want a two-pane settings panel with a
left navigation sidebar, like macOS System Settings / VS Code.

## Scope

- **In scope:** restructure the existing modal into a left-nav + content-pane
  layout; group Hub-**global** settings into five sidebar sections; add two new
  capabilities (theme toggle, replay onboarding).
- **Out of scope:** per-agent settings (notifications, MCP servers, skills,
  companion quiet-hours, behavior, permissions) — these already live in
  `DetailPanel` and are NOT duplicated here. No new settings window; this stays
  a modal.

## Decisions

- **Layout:** two-pane modal (~720×480). Left nav reuses the existing
  `.sidebar` / `.sidebar-item` classes from `DashboardApp.tsx`
  (`ui/src/styles/components/dashboard.css`); active item highlighted with
  `--color-brand`. Right pane scrolls.
- **Routing:** `activeSection` React state in `SettingsModal.tsx`, default
  `"general"`. No router dependency — a `switch` over the section id.
- **File structure:** `SettingsModal.tsx` stays the shell (sidebar + section
  routing). Each section is a small self-contained component in a new
  `ui/src/components/settings/` folder, each owning its own `invoke` calls and
  local state. Matches the repo's sibling-split convention and keeps each
  section independently testable.
- **Global-only:** confirmed. Per-agent settings stay in `DetailPanel`.
- **i18n:** every new label gets a key in `ui/src/i18n/en.ts` and
  `ui/src/i18n/zh-TW.ts`. User-facing brand string is uppercase **MUR**.

## Sections (top → bottom)

| Order | Section | id | Content | Backing | Move/New |
|---|---|---|---|---|---|
| 1 | General | `general` | Language (English / 繁體中文) · Theme (System / Light / Dark) | `mur.hub.lang` localStorage; theme writes `data-theme` + `mur.hub.theme` | Language = move; **Theme = NEW** |
| 2 | Models | `models` | Default-model badge + "Open Model Library" button | `nudge_status`; opens existing `ModelLibraryPanels` | Move |
| 3 | Updates & CLI | `updates` | CLI install · version-skew banner · Hub update status | `install_cli_tools`, `cli_version_skew` | Relocate from tray/DashboardApp |
| 4 | Import / Export | `data` | Import Agent · Import Preset | existing import flows | Move |
| 5 | About | `about` | Hub version · docs + GitHub links · Replay onboarding | `@tauri-apps/api/app` `getVersion()`; replay = new command (below) | version+links = easy; **Replay = NEW** |

### Per-section detail

**General (`general`)**
- Language dropdown — unchanged behavior; relocated from the current Appearance
  section.
- Theme control (System / Light / Dark): on change, set
  `document.documentElement.setAttribute("data-theme", <light|dark>)` (or remove
  the attribute for System, falling back to the existing
  `prefers-color-scheme` media query in `styles/tokens/semantic.css`). Persist
  the choice in `localStorage["mur.hub.theme"]`; apply on app startup before
  first paint to avoid a flash.

**Models (`models`)**
- Show the default brain badge ("🧠 {model_name}" or "Not set") via
  `nudge_status`, as today.
- Button "Open Model Library" that opens the existing `ModelLibraryPanels`
  surface (no rebuild of the library inside settings).

**Updates & CLI (`updates`)**
- "Install CLI tools" button → `install_cli_tools` (symlinks bundled `mur`).
- Version-skew banner → `cli_version_skew`: if out of sync, show
  `{cli, hub, upgrade_hint}` and the hint command (`brew upgrade mur`, etc.).
- Hub update status / progress, consolidated from the existing tray logic in
  `DashboardApp.tsx`.

**Import / Export (`data`)**
- "Import Agent" → existing `.muragent` inspect/install flow.
- "Import Preset" → existing style-preset import flow.
- **Export stays per-agent** in `DetailPanel` (it requires a selected agent);
  not included here unless an agent-picker is later requested.

**About (`about`)**
- App version via `@tauri-apps/api/app` `getVersion()`.
- Links: docs (`https://app.mur.run/docs/core`) and the GitHub repo.
- "Replay onboarding" button → new Tauri command (below) to clear the
  first-launch marker, then re-trigger the onboarding flow.

## New work (everything else is relocation)

1. **Theme toggle** — UI only; the `data-theme` token infrastructure already
   exists in `styles/tokens/semantic.css`. Persist + apply at startup.
2. **Replay onboarding** — a small Tauri command in
   `mur-hub-gui/src/onboarding/first_launch.rs` (sibling of
   `check_first_launch` / `mark_first_launch_done`) that removes the
   first-launch marker file so onboarding shows again. The About button calls
   it then routes to the onboarding flow.

## Testing

Per the repo's one-check rule (non-trivial UI logic leaves one runnable check):

- A component test asserting the sidebar renders all five items and that
  changing `activeSection` swaps the rendered pane.
- A check that the theme control sets `document.documentElement`'s `data-theme`
  to `dark`/`light` for those choices and removes it for System.
- Rust: a unit test that the replay-onboarding command removes the marker file
  (temp `MUR_HOME`), so `check_first_launch` reports first-launch again.

## Risks / notes

- Theme applied after first paint would flash; apply from `localStorage` at app
  bootstrap (before React mount) to avoid it.
- Reusing `.sidebar` classes couples settings nav styling to the dashboard
  sidebar; acceptable (same visual language). Add a `settings-` scoped class
  only if the two need to diverge.
- Keep each section file small; if any approaches the file-size limit, split
  further following the same folder pattern.
