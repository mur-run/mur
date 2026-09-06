# MUR Hub 2.0 — Phase 2(b) open-in-window — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-06-mur-hub-open-in-window-design.md` (§ references below point there). Two PRs: **PR 8** (pure movement) and **PR 9** (the feature). Both from fresh `main`.

## Goal

⌘↩ (and the ⋯ menu, a row double-click, or a ⌘K action) opens the selected agent's or fleet's detail in its own document window, rendered by the same components on the same Tauri commands as the dashboard.

## Architecture

A Rust command `open_detail_window` mirrors `open_chat_window`: one `detail-<kind>-<safe>` window per target, `index.html#/detail/<kind>/<name>`, focus-if-exists. `App.tsx` routes that hash to a `DetailWindow` root that wraps `AgentProvider` + `DirtyProvider` and renders `AgentDetail` or the `FleetDetailPane` extracted from `FleetView` in PR 8. Cross-window freshness is refetch-on-focus; navigation from a window goes back through the existing `open_dashboard` command, extended with `fleet_name` / `page`.

## Tech stack

Tauri 2 (Rust 2024 edition, `WebviewWindowBuilder`, `tauri-plugin-window-state` already installed), React 18 + TypeScript 5.5 + Vite 5, plain CSS on the two-tier tokens, Vitest 4 without jsdom, the lightweight i18n (`en.ts` defines keys, `zh-TW.ts` is typed `Table`).

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (`tsc` enforces the table).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX.
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive; Vite and `tsc` resolve the wrong file).
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`useT` needs a provider, so markup tests cover only components without it).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. Rust fail-open conventions of the Hub crate: window-positioning and focus calls are `let _ =`; only `build()` and `show()` errors reach the UI.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green, the Hub crate compiles, and that PR's manual acceptance list passes.
11. No second data path: the window reads the same Tauri commands the dashboard reads; no window-only backend query.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/` unless they start with `mur-hub-gui/src-tauri/` or `docs/`.
- Line numbers cite `main` at `91f4d062` (2026-09-06); re-check with `grep -n` before cutting.
- UI commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`. `npm run lint` reports 6 pre-existing warnings in files this plan does not touch (`ModelSetupWizard`, `PetApp`, `ChatChannelRail`, `FleetView` [one existing warning], `PanelWindow`, `WizardModal`); 0 errors is the bar.
- Rust commands from `mur-hub-gui/src-tauri/`: `ORT_STRATEGY=download cargo test chat_window detail_window`. The Hub target is large (~24 GB); if the project drive cannot take it, push and rely on the **Hub GUI crate** CI job (it runs clippy with `-D warnings` and the unit tests on three OSes), and say so in the PR.
- Browser acceptance: `npm run dev -- --port 5174 --strictPort`, inject the Tauri stub the Phase 1 plan describes (`window.__TAURI_INTERNALS__` with `metadata.currentWindow`, an `invoke` stub, `plugin:event|listen → 1`, `plugin:event|unlisten → null`, `plugin:dialog|message → "Ok"`), inject twice (Vite's dep-optimize reload wipes the first), click the error boundary's **Try again**. A detail window is exercised by loading `http://localhost:5174/#/detail/agent/<name>` directly.
- Commit after every task with the message given.

## File structure

| PR | File | Responsibility |
|---|---|---|
| 8 | `src/components/detail/fleet/FleetDetailPane.tsx` (new) | fleet detail + jobs loading, `fleet:run_done` detail reload, tab state, the `DetailPage` render; exports `FLEET_GLYPH` |
| 8 | `src/components/fleet/FleetView.tsx` (modify) | list, selection, restore, label filter, create; renders `FleetDetailPane` |
| 9 | `mur-hub-gui/src-tauri/src/chat_window.rs` (modify) | `safe_label_part` and `urlenc` become `pub(crate)`; unit tests |
| 9 | `mur-hub-gui/src-tauri/src/detail_window.rs` (new) | `DetailKind`, `detail_label`, `open_detail_window`; unit tests |
| 9 | `mur-hub-gui/src-tauri/src/lib.rs` (modify) | `mod detail_window`; `open_dashboard` gains `fleet_name` / `page`; handler registration |
| 9 | `mur-hub-gui/src-tauri/capabilities/detail.json` (new) | permissions for `detail-*` windows |
| 9 | `src/components/detail/window/detailRoute.ts` (+ `.test.ts`) (new) | `parseDetailRoute(hash)` |
| 9 | `src/components/detail/window/openInWindow.ts` (+ `.test.ts`) (new) | `isOpenInWindowShortcut`, `isEditingTarget`, `openDetailWindow` |
| 9 | `src/components/detail/window/DetailWindow.tsx` (new) | the window root: providers, drag bar, Show in Hub, close guard, agent / fleet body, missing state |
| 9 | `src/styles/components/detail-window.css` (new) + `src/styles/index.css` (modify) | `.detail-window*` rules |
| 9 | `src/App.tsx` (modify) | `#/detail/` route |
| 9 | `src/components/detail/agent/AgentDetail.tsx` (modify) | `onOpenInWindow?` menu item; refetch on focus |
| 9 | `src/components/detail/fleet/FleetDetailPane.tsx` (modify) | `onOpenInWindow?` passed to the header; refetch on focus |
| 9 | `src/components/detail/fleet/FleetHeader.tsx` (modify) | `onOpenInWindow?` menu item |
| 9 | `src/components/shell/SourceList.tsx` (modify) | `onOpen?` on row double-click |
| 9 | `src/components/agents/AgentsPage.tsx` (modify) | passes `onOpen` and `onOpenInWindow` |
| 9 | `src/components/fleet/FleetView.tsx` (modify) | passes `onOpen` and `onOpenInWindow`; `onSelect` wired again |
| 9 | `src/components/DashboardApp.tsx` (modify) | `selectedFleet`, ⌘↩, palette items, `select-fleet` / `open-page` listeners |
| all | `src/i18n/en.ts`, `src/i18n/zh-TW.ts` | every new string |

---

## PR 8 — `FleetDetailPane` extraction

Branch `refactor/hub-2b-fleet-detail-pane`. No user-visible change, no i18n change.

### Task 8.1 — move the fleet detail out of `FleetView`

**Interfaces.** Produces `FleetDetailPane` and `FLEET_GLYPH` (below); PR 9 consumes both.

- [x] Create `src/components/detail/fleet/FleetDetailPane.tsx`:

```tsx
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../../i18n";
import type { AgentEntry } from "../../../types";
import type { FleetSummary, FleetDetail as Detail, JobRow, LabelView } from "../../fleet/types";
import { Ico } from "../../agents/GridCard";
import { DetailPage } from "../../shell/DetailPage";
import { fleetStatusOf } from "../../shell/Status";
import { FLEET_TABS, FLEET_TAB_LABEL_KEY, type FleetTabId } from "../../shell/detailTabs";
import { FleetHeader, fleetMeta } from "./FleetHeader";
import { FleetOverview } from "./FleetOverview";
import { FleetMembers } from "./FleetMembers";
import { FleetJobs } from "./FleetJobs";
import { FleetSettings } from "./FleetSettings";
import { showToast } from "./fleetActions";

/** The fleet glyph: list rows (28px) and the detail avatar (48px). */
export const FLEET_GLYPH = (
  <>
    <path d="M12 4l9 4.5-9 4.5-9-4.5z" />
    <path d="M3 13l9 4.5 9-4.5" />
  </>
);

export interface FleetDetailPaneProps {
  name: string;
  /** Status + labels, from `fleet_list`. */
  summary: FleetSummary;
  labels: LabelView[];
  agentMap: Map<string, AgentEntry>;
  /** The host reloads its list / labels; the pane reloads detail + jobs itself. */
  onRefresh: () => void;
  /** After a successful delete: the host clears its selection or closes the window. */
  onDeleted: () => void;
}

/** One fleet's detail page (spec 2(b) §5): owns `fleet_detail` + `fleet_jobs`,
 *  reloads on `fleet:run_done` for this fleet, and renders the four tabs.
 *  Hosts key it by `name`, so a selection change remounts it. */
export function FleetDetailPane({ name, summary, labels, agentMap, onRefresh, onDeleted }: FleetDetailPaneProps) {
  const { t } = useT();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [tab, setTab] = useState<FleetTabId>("overview");

  const load = useCallback(async () => {
    try {
      const [d, j] = await Promise.all([
        invoke<Detail>("fleet_detail", { name }),
        invoke<JobRow[]>("fleet_jobs", { name, all: false }),
      ]);
      setDetail(d);
      setJobs(j);
    } catch (err) {
      showToast(String(err), 4000);
    }
  }, [name]);

  useEffect(() => {
    void load();
  }, [load]);

  // A finished run for this fleet refreshes its jobs; the host toasts and
  // reloads the list (FleetView keeps that so it fires without a selection).
  useEffect(() => {
    const unlisten = listen<{ name: string; ok: boolean }>("fleet:run_done", (event) => {
      if (event.payload.name === name) void load();
    });
    return () => { void unlisten.then((fn) => fn()); };
  }, [name, load]);

  function refresh() {
    onRefresh();
    void load();
  }

  if (!detail) return null;

  return (
    <DetailPage
      avatar={<span className="fleet-avatar fleet-avatar--lg" aria-hidden="true"><Ico>{FLEET_GLYPH}</Ico></span>}
      title={detail.display_name}
      status={fleetStatusOf(summary)}
      meta={fleetMeta(detail, t)}
      actions={<FleetHeader detail={detail} onRefresh={refresh} onDelete={onDeleted} />}
      tabs={FLEET_TABS.map((id) => ({ id, label: t(FLEET_TAB_LABEL_KEY[id]) }))}
      activeTab={tab}
      onTab={setTab}
    >
      {tab === "overview" && <FleetOverview detail={detail} jobs={jobs} agentMap={agentMap} onGoTo={setTab} />}
      {tab === "members" && (
        <FleetMembers detail={detail} agentMap={agentMap} labels={labels} fleetLabels={summary.labels} onRefresh={refresh} />
      )}
      {tab === "jobs" && <FleetJobs detail={detail} jobs={jobs} onRefresh={refresh} />}
      {tab === "settings" && <FleetSettings detail={detail} onRefresh={refresh} onDelete={onDeleted} />}
    </DetailPage>
  );
}
```

- [x] Edit `src/components/fleet/FleetView.tsx` (288 lines). Apply exactly these changes:
  - Imports: delete the lines importing `DetailPage`, `FLEET_TABS, FLEET_TAB_LABEL_KEY, type FleetTabId`, `FleetHeader, fleetMeta`, `FleetOverview`, `FleetMembers`, `FleetJobs`, `FleetSettings`. Delete `FleetDetail as Detail, JobRow` from the `./types` import (keep `FleetSummary`, `LabelView`). Add `import { FleetDetailPane, FLEET_GLYPH } from "../detail/fleet/FleetDetailPane";`.
  - Delete the local `const FLEET_GLYPH = (…);` block (lines 45–50).
  - Delete the state lines `const [detail, setDetail] = useState<Detail | null>(null);`, `const [jobs, setJobs] = useState<JobRow[]>([]);`, `const [tab, setTab] = useState<FleetTabId>("overview");`.
  - Delete the effect `useEffect(() => { setTab("overview"); }, [selectedName]);`.
  - Delete the whole `async function loadDetail(name: string) { … }`.
  - Delete the effect commented `// Load detail whenever selection changes` (the one that calls `loadDetail(selectedName)` / `setDetail(null)`).
  - In the `fleet:run_done` effect delete the line `if (selectedRef.current === name) void loadDetail(name);` and change the destructuring to `const { ok } = event.payload;` (the toast and `loadList()` stay).
  - `handleRefresh` becomes:
    ```tsx
    function handleRefresh() {
      void loadList();
      void loadLabels();
    }
    ```
  - `handleDelete` becomes:
    ```tsx
    function handleDelete() {
      setSelectedName(null);
      void loadList();
    }
    ```
  - Replace `const summary = fleets.find((f) => f.name === detail?.name);` with `const summary = fleets.find((f) => f.name === selectedName);`.
  - Replace the whole `{detail && summary ? ( <DetailPage …> … </DetailPage> ) : ( <div className="fleet-view__empty"> … )}` block with:
    ```tsx
        {selectedName && summary ? (
          <FleetDetailPane
            key={selectedName}
            name={selectedName}
            summary={summary}
            labels={labels}
            agentMap={agentMap}
            onRefresh={handleRefresh}
            onDeleted={handleDelete}
          />
        ) : (
          <div className="fleet-view__empty">
            <p>{fleets.length === 0 ? t("fleet.empty") : t("fleet.selectHint")}</p>
          </div>
        )}
    ```
- [x] `set -o pipefail; npm test 2>&1 | grep -E 'Tests|FAIL'` → `Tests  261 passed (261)`; `npm run build` → `✓ built`; `npm run lint` → `0 errors`.
- [x] Browser acceptance (stub `fleet_list` with two fleets, `fleet_detail`, `fleet_jobs`, `fleet_labels_list`, `list_agents`): selecting a fleet loads its detail; switching fleets resets to Overview; a stubbed `fleet:run_done` event still toasts (dashboard) and the selected fleet's jobs reload; Delete clears the selection and reloads the list; a palette jump (`requestedName`) still selects that fleet. Identical to before.
- [x] Commit: `refactor(hub): extract FleetDetailPane from FleetView — pure movement`

**Done (2026-09-06):** as written; the browser acceptance ran with an array-keeping event stub (both `fleet:run_done` listeners fire), which the Phase 1 stub did not need.

---

## PR 9 — windows, route, triggers, bridge

Branch `feat/hub-2b-open-in-window`, from `main` after PR 8 merged.

### Task 9.1 — Rust: shared label helpers, `detail_window`, capability, `open_dashboard`

**Interfaces.** Produces the Tauri commands `open_detail_window { kind: "agent" | "fleet", name, title }` and `open_dashboard { agentName?, fleetName?, page? }` (emits `select-agent` / `select-fleet` / `open-page`), and the window labels `detail-agent-<safe>` / `detail-fleet-<safe>`. Tasks 9.2–9.5 consume the commands; nothing in the UI builds a label.

- [x] In `mur-hub-gui/src-tauri/src/chat_window.rs` replace the `label` and `pet_label` functions and `urlenc` (lines 12–44) with:

```rust
/// The label-safe form of an agent / fleet name: alphanumerics and `-` kept,
/// everything else becomes `-`. Shared by the chat, pet, and detail windows.
pub(crate) fn safe_label_part(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

fn label(agent_name: &str) -> String {
    format!("chat-{}", safe_label_part(agent_name))
}

fn pet_label(agent_name: &str) -> String {
    format!("pet-{}", safe_label_part(agent_name))
}

/// Spaces become `+` for the hash route; the UI reverses it (`agentNameFromHash`).
pub(crate) fn urlenc(s: &str) -> String {
    s.chars().map(|c| if c == ' ' { '+' } else { c }).collect()
}
```
  and append to the file:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_label_part_keeps_alphanumerics_and_dashes() {
        assert_eq!(safe_label_part("aura-2"), "aura-2");
        assert_eq!(safe_label_part("My Agent.v2"), "My-Agent-v2");
        assert_eq!(safe_label_part("研究員"), "研究員");
    }

    #[test]
    fn label_prefixes_kind() {
        assert_eq!(label("a b"), "chat-a-b");
        assert_eq!(pet_label("a b"), "pet-a-b");
    }

    #[test]
    fn urlenc_only_touches_spaces() {
        assert_eq!(urlenc("a b-c"), "a+b-c");
    }
}
```
- [x] Create `mur-hub-gui/src-tauri/src/detail_window.rs`:

```rust
//! Detail windows (Hub 2.0 Phase 2(b)): an agent's or a fleet's detail page in
//! its own document window, loaded from `index.html#/detail/<kind>/<name>`.
//! Mirrors `chat_window`: one window per target, focus-if-exists.

use serde::Deserialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::chat_window::{safe_label_part, urlenc};

const INNER_W: f64 = 960.0;
const INNER_H: f64 = 640.0;
const MIN_W: f64 = 720.0;
const MIN_H: f64 = 520.0;
/// Logical offset from the dashboard's top-left so windows cascade from the Hub.
/// `tauri-plugin-window-state` overrides this with the saved position once a
/// window of this label has been moved (it restores on webview-ready, after
/// this runs).
const CASCADE_OFFSET: f64 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetailKind {
    Agent,
    Fleet,
}

impl DetailKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Fleet => "fleet",
        }
    }
}

pub(crate) fn detail_label(kind: DetailKind, name: &str) -> String {
    format!("detail-{}-{}", kind.as_str(), safe_label_part(name))
}

#[tauri::command]
pub fn open_detail_window(
    kind: DetailKind,
    name: String,
    title: String,
    app: AppHandle,
) -> Result<(), String> {
    let lbl = detail_label(kind, &name);

    // Single-instance guard: the user explicitly re-opened it, so focus.
    if let Some(win) = app.get_webview_window(&lbl) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }

    let url = format!("index.html#/detail/{}/{}", kind.as_str(), urlenc(&name));
    let builder = WebviewWindowBuilder::new(&app, &lbl, WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(INNER_W, INNER_H)
        .min_inner_size(MIN_W, MIN_H)
        .resizable(true)
        .visible(false);
    // The dashboard's chrome: traffic lights inside the page, no title text.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let win = builder.build().map_err(|e| e.to_string())?;

    // Positions are physical; CASCADE_OFFSET is logical.
    if let Some(dash) = app.get_webview_window("dashboard")
        && let Ok(pos) = dash.outer_position()
    {
        let scale = dash.scale_factor().unwrap_or(1.0);
        let off = (CASCADE_OFFSET * scale) as i32;
        let _ = win.set_position(tauri::PhysicalPosition::new(pos.x + off, pos.y + off));
    }

    win.show().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_label_has_kind_and_safe_name() {
        assert_eq!(detail_label(DetailKind::Agent, "aura"), "detail-agent-aura");
        assert_eq!(detail_label(DetailKind::Fleet, "night ops"), "detail-fleet-night-ops");
    }

    #[test]
    fn detail_kind_deserializes_lowercase() {
        assert_eq!(serde_json::from_str::<DetailKind>("\"agent\"").unwrap(), DetailKind::Agent);
        assert_eq!(serde_json::from_str::<DetailKind>("\"fleet\"").unwrap(), DetailKind::Fleet);
        assert!(serde_json::from_str::<DetailKind>("\"skill\"").is_err());
    }
}
```
- [x] Create `mur-hub-gui/src-tauri/capabilities/detail.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "identifier": "detail",
  "description": "Capability set for agent / fleet detail windows (detail-* labels): the dashboard's plugin set (dialogs for export and discard prompts, shell open) plus the window controls a document window needs.",
  "windows": ["detail-*"],
  "permissions": [
    "core:default",
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-close",
    "core:window:allow-set-focus",
    "core:window:allow-start-dragging",
    "core:window:allow-set-size",
    "core:window:allow-set-min-size",
    "dialog:allow-confirm",
    "dialog:allow-open",
    "dialog:allow-save",
    "shell:allow-open"
  ]
}
```
- [x] In `mur-hub-gui/src-tauri/src/lib.rs`:
  - Add `pub mod detail_window;` after `pub mod detail;` (line 17).
  - Replace `open_dashboard` (lines 96–106) with:
    ```rust
    /// Show + focus the dashboard, then tell it where to go: `select-agent`
    /// (existing), `select-fleet`, and `open-page` are each emitted only when
    /// given. Detail windows use this as their "Show in Hub" / "Home" bridge.
    #[tauri::command]
    fn open_dashboard(
        app: AppHandle,
        agent_name: Option<String>,
        fleet_name: Option<String>,
        page: Option<String>,
    ) {
        let Some(win) = app.get_webview_window("dashboard") else {
            return;
        };
        let _ = win.show();
        let _ = win.set_focus();
        if let Some(name) = agent_name {
            let _ = app.emit("select-agent", name);
        }
        if let Some(name) = fleet_name {
            let _ = app.emit("select-fleet", name);
        }
        if let Some(id) = page {
            let _ = app.emit("open-page", id);
        }
    }
    ```
  - Change both internal callers: line 405 `open_dashboard(app.clone(), None);` → `open_dashboard(app.clone(), None, None, None);` and line 427 `"open" => open_dashboard(app.clone(), None),` → `"open" => open_dashboard(app.clone(), None, None, None),`.
  - In the `generate_handler!` list add `detail_window::open_detail_window,` right after `chat_window::open_chat_window,` (line 652).
- [x] `cd mur-hub-gui/src-tauri && ORT_STRATEGY=download cargo test window` → the 3 `chat_window::tests` and 2 `detail_window::tests` pass (`cargo test` takes one name filter; `window` matches both modules). If the target does not fit the drive, `cargo check -p mur-hub-gui` is also too large; push and read the **Hub GUI crate** CI job instead, and say so in the PR body.
- [x] Commit: `feat(hub): open_detail_window command, detail-* capability, open_dashboard fleet/page bridge`

**Done (2026-09-06):** as written, plus `rustfmt` reflowed three long lines. The drive had 78 GB free, so the crate's unit tests ran locally (see the PR body for the result).

### Task 9.2 — route parsing, shortcut predicates, i18n

**Interfaces.** Consumes the command names from 9.1. Produces `parseDetailRoute(hash): DetailRoute | null`, `type DetailKind`, `isOpenInWindowShortcut(e)`, `isEditingTarget(el)`, `openDetailWindow(kind, name, title): Promise<void>`, and the i18n keys listed below; 9.3–9.5 consume them.

- [x] Create `src/components/detail/window/detailRoute.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { parseDetailRoute } from "./detailRoute";

describe("parseDetailRoute", () => {
  it("parses agent and fleet routes", () => {
    expect(parseDetailRoute("#/detail/agent/aura")).toEqual({ kind: "agent", name: "aura" });
    expect(parseDetailRoute("#/detail/fleet/night-ops")).toEqual({ kind: "fleet", name: "night-ops" });
  });
  it("decodes + and percent escapes like the chat window", () => {
    expect(parseDetailRoute("#/detail/agent/my+agent")).toEqual({ kind: "agent", name: "my agent" });
    expect(parseDetailRoute("#/detail/agent/%E7%A0%94%E7%A9%B6")).toEqual({ kind: "agent", name: "研究" });
  });
  it("rejects other hashes, unknown kinds, empty names, and bad escapes", () => {
    expect(parseDetailRoute("#/chat/aura")).toBeNull();
    expect(parseDetailRoute("#/detail/skill/x")).toBeNull();
    expect(parseDetailRoute("#/detail/agent/")).toBeNull();
    expect(parseDetailRoute("#/detail/agent")).toBeNull();
    expect(parseDetailRoute("#/detail/agent/%E0%A4%A")).toBeNull();
  });
});
```
- [x] `npm test -- src/components/detail/window/detailRoute.test.ts` → fails (module missing).
- [x] Create `src/components/detail/window/detailRoute.ts`:

```ts
/** `#/detail/<kind>/<name>` — the hash `open_detail_window` loads (spec 2(b) §4). */
export type DetailKind = "agent" | "fleet";

export interface DetailRoute {
  kind: DetailKind;
  name: string;
}

export const DETAIL_HASH_PREFIX = "#/detail/";

/** Null for anything that is not a well-formed detail route; the window
 *  root then shows its "nothing to show" state instead of guessing. */
export function parseDetailRoute(hash: string): DetailRoute | null {
  if (!hash.startsWith(DETAIL_HASH_PREFIX)) return null;
  const rest = hash.slice(DETAIL_HASH_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  const kind = rest.slice(0, slash);
  if (kind !== "agent" && kind !== "fleet") return null;
  let name: string;
  try {
    // Same encoding as `AgentChatWindow.agentNameFromHash`: `+` is a space.
    name = decodeURIComponent(rest.slice(slash + 1).replace(/\+/g, " "));
  } catch {
    return null;
  }
  return name ? { kind, name } : null;
}
```
- [x] `npm test -- src/components/detail/window/detailRoute.test.ts` → 3 passed.
- [x] Create `src/components/detail/window/openInWindow.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { isEditingTarget, isOpenInWindowShortcut } from "./openInWindow";

function key(over: Partial<KeyboardEvent>): KeyboardEvent {
  return { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, key: "Enter", ...over } as KeyboardEvent;
}
function el(tagName: string, contenteditable: string | null = null): Element {
  return { tagName, getAttribute: () => contenteditable } as unknown as Element;
}

describe("isOpenInWindowShortcut", () => {
  it("accepts ⌘↩ and Ctrl+Enter", () => {
    expect(isOpenInWindowShortcut(key({ metaKey: true }))).toBe(true);
    expect(isOpenInWindowShortcut(key({ ctrlKey: true }))).toBe(true);
  });
  it("rejects plain Enter and extra modifiers", () => {
    expect(isOpenInWindowShortcut(key({}))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, shiftKey: true }))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, altKey: true }))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, key: "k" }))).toBe(false);
  });
});

describe("isEditingTarget", () => {
  it("is true for fields and contenteditable, false otherwise", () => {
    expect(isEditingTarget(el("INPUT"))).toBe(true);
    expect(isEditingTarget(el("TEXTAREA"))).toBe(true);
    expect(isEditingTarget(el("SELECT"))).toBe(true);
    expect(isEditingTarget(el("DIV", "true"))).toBe(true);
    expect(isEditingTarget(el("DIV"))).toBe(false);
    expect(isEditingTarget(null)).toBe(false);
  });
});
```
- [x] `npm test -- src/components/detail/window/openInWindow.test.ts` → fails.
- [x] Create `src/components/detail/window/openInWindow.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../fleet/fleetActions";
import type { DetailKind } from "./detailRoute";

/** ⌘↩ on macOS, Ctrl+Enter elsewhere (spec 2(b) §7). */
export function isOpenInWindowShortcut(e: KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key === "Enter";
}

/** True while a text field owns the keyboard, so page shortcuts stay out. */
export function isEditingTarget(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.getAttribute("contenteditable") === "true";
}

const OPEN_ERROR_TOAST_MS = 4000;

/** Every trigger (⌘↩, ⋯, double-click, palette) goes through here. `title`
 *  is the display name; it becomes the window title. */
export async function openDetailWindow(kind: DetailKind, name: string, title: string): Promise<void> {
  try {
    await invoke("open_detail_window", { kind, name, title });
  } catch (err) {
    showToast(String(err), OPEN_ERROR_TOAST_MS);
  }
}
```
- [x] `npm test -- src/components/detail/window/openInWindow.test.ts` → 3 passed.
- [x] i18n, both tables. `en.ts`: after `"action.openChatWindow"` add `"action.openInWindow": "Open in window",`; after `"palette.action.stop"` add `"palette.action.openInWindow": "Open {name} in window",`; after `"detail.discardBody"` add:
  ```ts
  "detailWindow.showInHub": "Show in Hub",
  "detailWindow.missingAgent": "This agent no longer exists.",
  "detailWindow.missingFleet": "This fleet no longer exists.",
  "detailWindow.badRoute": "Nothing to show here.",
  "detailWindow.close": "Close window",
  ```
  `zh-TW.ts`, same anchors: `"action.openInWindow": "在視窗開啟",` · `"palette.action.openInWindow": "在視窗開啟 {name}",` · `"detailWindow.showInHub": "在 Hub 中顯示",` · `"detailWindow.missingAgent": "這個 agent 已不存在。",` · `"detailWindow.missingFleet": "這個機群已不存在。",` · `"detailWindow.badRoute": "這裡沒有可顯示的內容。",` · `"detailWindow.close": "關閉視窗",`.
- [x] `npm test`, `npm run build` (tsc checks the zh table). Commit: `feat(hub): detail route parser, open-in-window predicates, strings`

### Task 9.3 — `DetailWindow` root, route, CSS, close guard

**Interfaces.** Consumes `parseDetailRoute`, the i18n keys, `FleetDetailPane`, `AgentDetail`, the commands from 9.1. Produces the `#/detail/` route.

- [x] Create `src/components/detail/window/DetailWindow.tsx`:

```tsx
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AgentEntry } from "../../../types";
import { AgentProvider, useAgents } from "../../../context/AgentContext";
import { useT } from "../../../i18n";
import type { FleetSummary, LabelView } from "../../fleet/types";
import { visibleInboxItems } from "../../home/inbox";
import { needsYouCounts } from "../../home/needsYouCounts";
import { useChannels } from "../../home/useChannels";
import { useInbox } from "../../home/useInbox";
import { DirtyProvider, useDirtyGuard } from "../../shell/dirty";
import { isMac } from "../../shell/platform";
import { AgentDetail } from "../agent/AgentDetail";
import { FleetDetailPane } from "../fleet/FleetDetailPane";
import { parseDetailRoute, type DetailRoute } from "./detailRoute";

/** Dismissals are session state in DashboardApp; a fresh window has none. */
const NOTHING_DISMISSED: ReadonlySet<string> = new Set();

/** The `#/detail/<kind>/<name>` root (spec 2(b) §4): the dashboard's
 *  providers, a drag bar with "Show in Hub", and one detail filling the window. */
export function DetailWindow() {
  const route = parseDetailRoute(window.location.hash);
  return (
    <AgentProvider>
      <DirtyProvider>
        <DetailWindowInner route={route} />
      </DirtyProvider>
    </AgentProvider>
  );
}

function DetailWindowInner({ route }: { route: DetailRoute | null }) {
  const { t } = useT();
  const { confirmLeave } = useDirtyGuard();

  // Closing with unsaved edits asks first (spec §8); the same prompt the
  // dashboard shows when switching selection.
  useEffect(() => {
    const un = getCurrentWindow().onCloseRequested(async (e) => {
      if (!(await confirmLeave(t("detail.discardBody"), t("detail.discardTitle")))) e.preventDefault();
    });
    return () => { void un.then((f) => f()); };
  }, [confirmLeave, t]);

  function showInHub() {
    const args = !route ? {} : route.kind === "agent" ? { agentName: route.name } : { fleetName: route.name };
    invoke("open_dashboard", args).catch(console.error);
  }

  return (
    <div className={`detail-window${isMac() ? " detail-window--inset" : ""}`}>
      <div className="detail-window__bar" data-tauri-drag-region>
        <button type="button" className="btn btn--secondary" onClick={showInHub}>
          {t("detailWindow.showInHub")}
        </button>
      </div>
      {!route ? (
        <Missing text={t("detailWindow.badRoute")} />
      ) : route.kind === "agent" ? (
        <AgentBody name={route.name} />
      ) : (
        <FleetBody name={route.name} />
      )}
    </div>
  );
}

function Missing({ text }: { text: string }) {
  const { t } = useT();
  return (
    <div className="detail-window__missing">
      <p>{text}</p>
      <button type="button" className="btn btn--secondary" onClick={() => void getCurrentWindow().close()}>
        {t("detailWindow.close")}
      </button>
    </div>
  );
}

function AgentBody({ name }: { name: string }) {
  const { t } = useT();
  const { agents, runtimeStatuses } = useAgents();
  const { channels } = useChannels();
  const { items } = useInbox();
  const needsYou = needsYouCounts(visibleInboxItems(items, NOTHING_DISMISSED));
  const entry = agents.find((a) => a.name === name);
  // Before the list arrives `entry` is undefined and the header shows the
  // raw name; once it has arrived, an absent entry means the agent is gone.
  if (agents.length > 0 && !entry) return <Missing text={t("detailWindow.missingAgent")} />;
  return (
    <AgentDetail
      agentName={name}
      entry={entry}
      runtime={runtimeStatuses.find((s) => s.name === name)}
      channels={channels}
      needsYou={needsYou[name] ?? 0}
      onOpenChat={(agentName) => {
        invoke("open_chat_window", { agentName }).catch(console.error);
      }}
      onOpenHome={() => {
        invoke("open_dashboard", { page: "home" }).catch(console.error);
      }}
    />
  );
}

function FleetBody({ name }: { name: string }) {
  const { t } = useT();
  const { agents } = useAgents();
  const [fleets, setFleets] = useState<FleetSummary[] | null>(null);
  const [labels, setLabels] = useState<LabelView[]>([]);
  const [agentMap, setAgentMap] = useState<Map<string, AgentEntry>>(new Map());

  useEffect(() => {
    setAgentMap(new Map(agents.map((a) => [a.name, a])));
  }, [agents]);

  // The pane's host data: the summary (status + labels) and the label registry.
  const load = useCallback(() => {
    invoke<FleetSummary[]>("fleet_list").then(setFleets).catch(() => setFleets([]));
    invoke<LabelView[]>("fleet_labels_list").then(setLabels).catch(() => setLabels([]));
  }, []);
  useEffect(load, [load]);

  if (fleets === null) return null;
  const summary = fleets.find((f) => f.name === name);
  if (!summary) return <Missing text={t("detailWindow.missingFleet")} />;
  return (
    <FleetDetailPane
      name={name}
      summary={summary}
      labels={labels}
      agentMap={agentMap}
      onRefresh={load}
      onDeleted={() => void getCurrentWindow().close()}
    />
  );
}
```
- [x] Create `src/styles/components/detail-window.css` and add `@import "./components/detail-window.css";` after the `detail-page.css` line in `src/styles/index.css`:

```css
/* Detail windows (Phase 2(b) §4): a drag bar and one DetailPage filling the window. */
.detail-window { display: grid; grid-template-rows: auto 1fr; height: 100%; min-height: 0; background: var(--surface-detail); }
.detail-window__bar {
  display: flex; align-items: center; justify-content: flex-end;
  height: calc(var(--shell-titlebar-inset) + 10px); padding: 0 var(--space-6);
}
/* macOS overlay title bar: keep the button clear of the traffic lights. */
.detail-window--inset .detail-window__bar { padding-left: 80px; }
.detail-window > .detail-page { min-height: 0; }
.detail-window__missing {
  display: grid; place-items: center; align-content: center; gap: var(--space-5);
  height: 100%; color: var(--text-secondary); font-size: var(--text-sm);
}
.detail-window__missing p { margin: 0; }
```
- [x] In `src/App.tsx`: add `import { DetailWindow } from "./components/detail/window/DetailWindow";`; extend `getRoute()`'s return type with `| "detail"` and add `if (hash.startsWith("#/detail/")) return "detail";` after the `#/chat/` line; add `if (route === "detail") return <DetailWindow />;` after the `chat` line (it brings its own `AgentProvider`).
- [x] `npm test`, `npm run build`, `npm run lint`.
- [x] Browser acceptance: extend the stub with `fleet_list` (two fleets), `fleet_detail`, `fleet_jobs`, `fleet_labels_list`, `get_agent_detail` (with `effort_levels: []`, `addons: []`), `agent_get_fallback → []`, `agent_get_smart → null`, `list_models → []`, `channel_list → []`, `hitl_pending_list → []`, `install_inbox_list → []`; record `open_dashboard` / `open_chat_window` / `plugin:window|close` calls. Then:
  - `#/detail/agent/aura` renders the agent's six-tab detail under a bar with **Show in Hub**; the bar's button calls `open_dashboard {agentName:"aura"}`; the Overview's Home link calls `open_dashboard {page:"home"}`; the header's Chat calls `open_chat_window`.
  - `#/detail/fleet/<name>` renders the fleet tabs; **Show in Hub** calls `open_dashboard {fleetName}`.
  - `#/detail/agent/nope` (not in `list_agents`) and `#/detail/fleet/nope` show the missing state; `#/detail/skill/x` shows "Nothing to show here."; the Close button calls the window close.
  - `plugin:window|close`-style acceptance of the guard cannot run in a browser (no close event); it is on the real-window list in Task 9.5.
- [x] Commit: `feat(hub): DetailWindow root on #/detail/<kind>/<name> with Show in Hub and a close guard`

**Done (2026-09-06):** as written. Browser note: a hash change alone does not re-route (`getRoute` runs once), so each route was exercised with `location.reload()` and a stub kept in `sessionStorage`; `#/detail/fleet/nope` was not exercised separately (same `Missing` path as the agent case, one lookup).

### Task 9.4 — refetch on focus

**Interfaces.** Consumes nothing new. Produces no API; `AgentDetail` and `FleetDetailPane` reload when their window is focused (spec §6).

- [x] In `src/components/detail/agent/AgentDetail.tsx`: add `import { getCurrentWindow } from "@tauri-apps/api/window";`; change `const { confirmLeave } = useDirtyGuard();` (line 50) to `const { confirmLeave, isDirty } = useDirtyGuard();`; after the first `useEffect` (the `get_agent_detail` load, lines 60–66) add:

```tsx
  // Another window may have saved this agent: reload when ours is focused,
  // unless a form here is mid-edit (spec 2(b) §6). Runs in the dashboard too.
  useEffect(() => {
    const un = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (!focused || isDirty) return;
      invoke<AgentDetailData>("get_agent_detail", { name: agentName })
        .then(setDetail)
        .catch((e) => setError(String(e)));
    });
    return () => { void un.then((f) => f()); };
  }, [agentName, isDirty]);
```
- [x] In `src/components/detail/fleet/FleetDetailPane.tsx`: add `import { getCurrentWindow } from "@tauri-apps/api/window";` and after the `fleet:run_done` effect add:

```tsx
  // Refetch when this window is focused (spec 2(b) §6). Fleet forms never
  // mark dirty, so no guard is needed here.
  useEffect(() => {
    const un = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused) void load();
    });
    return () => { void un.then((f) => f()); };
  }, [load]);
```
- [x] `npm test`, `npm run build`, `npm run lint`. Browser check: the stub's `plugin:event|listen` is called with `tauri://focus` / `tauri://blur` on both the dashboard's agent detail and a `#/detail/...` window (read `window.__calls`).
- [x] Commit: `feat(hub): agent and fleet detail refetch when their window regains focus`

### Task 9.5 — triggers in the dashboard: ⌘↩, ⋯, double-click, ⌘K, and the two bridge listeners

**Interfaces.** Consumes `isOpenInWindowShortcut`, `isEditingTarget`, `openDetailWindow`. Produces `SourceList.onOpen?`, `AgentDetail.onOpenInWindow?`, `FleetDetailPane.onOpenInWindow?` → `FleetHeader.onOpenInWindow?`, and `DashboardApp.selectedFleet`.

- [x] `src/components/shell/SourceList.tsx`: add to `SourceListProps` after `onSelect`:
  ```ts
  /** Row double-click (spec 2(b) §7): open in a window. Absent on Library pages. */
  onOpen?: (id: string) => void;
  ```
  and on the row `<div … onClick={() => p.onSelect(r.id)}>` (line 130) add `onDoubleClick={p.onOpen ? () => p.onOpen?.(r.id) : undefined}`.
- [x] `src/components/shell/SourceList.test.tsx`: add a third `it` in the `SourceList markup` describe:
  ```tsx
  it("renders the same markup with or without onOpen (handlers are not markup)", () => {
    const props = { title: "Agents", count: 2, rows, facets: [], allLabel: "All", activeFacet: null, onFacet: noop,
      filter: "", onFilter: noop, filterPlaceholder: "Filter", selectedId: "aura", onSelect: noop, onCreate: noop,
      createLabel: "New", emptyState: <p>none</p> };
    expect(renderToStaticMarkup(<SourceList {...props} onOpen={noop} />)).toBe(renderToStaticMarkup(<SourceList {...props} />));
  });
  ```
  `npm test -- src/components/shell/SourceList.test.tsx` → 3 passed.
- [x] `src/components/detail/agent/AgentDetail.tsx`: add `onOpenInWindow?: () => void;` to `AgentDetailProps` (after `onOpenHome`) with the comment `/** Dashboard only: the ⋯ "Open in window" item. Undefined inside a window. */`; destructure it in the component signature; in the `OverflowMenu` `items` array (line 172) add after the `chatWindow` item:
  ```tsx
          ...(onOpenInWindow
            ? [{ id: "openInWindow", label: t("action.openInWindow"), onSelect: onOpenInWindow }]
            : []),
  ```
- [x] `src/components/detail/fleet/FleetHeader.tsx`: add `onOpenInWindow?: () => void;` to `FleetHeaderProps`; destructure; in its `OverflowMenu` `items` (line 183) insert after the `import` item:
  ```tsx
          ...(onOpenInWindow
            ? [{ id: "openInWindow", label: t("action.openInWindow"), onSelect: onOpenInWindow }]
            : []),
  ```
- [x] `src/components/detail/fleet/FleetDetailPane.tsx`: add `onOpenInWindow?: () => void;` to `FleetDetailPaneProps` (same comment as AgentDetail's), destructure, and pass `onOpenInWindow={onOpenInWindow}` to `<FleetHeader …/>`.
- [x] `src/components/agents/AgentsPage.tsx`: add `import { openDetailWindow } from "../detail/window/openInWindow";`. On `<SourceList …>` add `onOpen={(id) => { const a = agents.find((x) => x.name === id); if (a) void openDetailWindow("agent", a.name, a.display_name); }}`. On `<AgentDetail …>` add `onOpenInWindow={() => { void openDetailWindow("agent", entry.name, entry.display_name); }}` (`entry` is in scope and non-null on that branch).
- [x] `src/components/fleet/FleetView.tsx`: add `import { openDetailWindow } from "../detail/window/openInWindow";`. On `<SourceList …>` add `onOpen={(id) => { const f = fleets.find((x) => x.name === id); if (f) void openDetailWindow("fleet", f.name, f.display_name); }}`. On `<FleetDetailPane …>` add `onOpenInWindow={() => { void openDetailWindow("fleet", summary.name, summary.display_name); }}`. The `onSelect` prop and its report-up effect already exist (lines 52–53, 96–100); nothing to add here.
- [x] `src/components/DashboardApp.tsx`:
  - Imports: `import { isEditingTarget, isOpenInWindowShortcut, openDetailWindow } from "./detail/window/openInWindow";` and `import { isPageId } from "./shell/nav";` (added below).
  - State, next to `fleetRequest` (line 109): `const [selectedFleet, setSelectedFleet] = useState<string | null>(null);` and `const onFleetSelect = useCallback((name: string | null) => setSelectedFleet(name), []);`.
  - `<FleetView requestedName={fleetRequest} onRequestHandled={clearFleetRequest} />` → add `onSelect={onFleetSelect}`.
  - Extend the ⌘K/⌘R keydown effect (lines 288–302): add a branch before the `⌘R` one:
    ```tsx
      } else if (isOpenInWindowShortcut(e)) {
        if (isEditingTarget(document.activeElement)) return;
        if (page === "agents" && selectedAgent) {
          e.preventDefault();
          const a = agents.find((x) => x.name === selectedAgent);
          void openDetailWindow("agent", selectedAgent, a?.display_name ?? selectedAgent);
        } else if (page === "fleets" && selectedFleet) {
          e.preventDefault();
          const f = paletteFleets.find((x) => x.name === selectedFleet);
          void openDetailWindow("fleet", selectedFleet, f?.display_name ?? selectedFleet);
        }
    ```
    and add `page, selectedAgent, selectedFleet, agents, paletteFleets` to that effect's dependency array. (`paletteFleets` is only loaded when the palette opens; the fallback title is the fleet name, which the window title tolerates — it is the same value the palette shows before load.)
  - Palette items: inside the `...(selectedAgent ? [ … ] : [])` spread (line 360) add, after the start/stop object, a second element:
    ```tsx
            {
              id: "action:openAgentInWindow",
              kind: "action" as const,
              label: t("palette.action.openInWindow", { name: selectedAgent }),
              run: () => {
                const a = agents.find((x) => x.name === selectedAgent);
                void openDetailWindow("agent", selectedAgent, a?.display_name ?? selectedAgent);
              },
            },
    ```
    and add a new spread after it:
    ```tsx
        ...(selectedFleet
          ? [{
              id: "action:openFleetInWindow",
              kind: "action" as const,
              label: t("palette.action.openInWindow", { name: selectedFleet }),
              run: () => {
                const f = paletteFleets.find((x) => x.name === selectedFleet);
                void openDetailWindow("fleet", selectedFleet, f?.display_name ?? selectedFleet);
              },
            }]
          : []),
    ```
  - Bridge listeners: after the `select-agent` effect (lines 196–210) add:
    ```tsx
      // "Show in Hub" from a fleet detail window, and page jumps from any window.
      useEffect(() => {
        const unFleet = listen<string>("select-fleet", (e) => {
          setFleetRequest(e.payload);
          setPage("fleets");
        });
        const unPage = listen<string>("open-page", (e) => {
          if (isPageId(e.payload)) setPage(e.payload);
        });
        return () => {
          void unFleet.then((fn) => fn());
          void unPage.then((fn) => fn());
        };
      }, []);
    ```
- [x] `src/components/shell/nav.ts`: add after `isLibrary`:
  ```ts
  /** Type guard for page ids arriving over events (`open-page`). */
  export function isPageId(id: string): id is PageId {
    return NAV_ITEMS.some((n) => n.id === id);
  }
  ```
  (`NAV_ITEMS` lists every `PageId`, `home` included.) Add to `src/components/shell/nav.test.ts`:
  ```ts
  it("isPageId accepts nav ids and rejects strangers", () => {
    expect(isPageId("agents")).toBe(true);
    expect(isPageId("home")).toBe(true);
    expect(isPageId("nope")).toBe(false);
  });
  ```
  and change that file's import to `import { NAV_ITEMS, isLibrary, isPageId } from "./nav";`.
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [x] Browser acceptance (dashboard stub, record `open_detail_window` calls): on Agents with a selection, ⌘↩ → `open_detail_window {kind:"agent", name, title:<display name>}`; ⌘↩ while the filter field is focused → no call; the ⋯ menu shows "Open in window" and calls it; double-click on a row calls it with that row; ⌘K lists "Open <name> in window"; the same four on Fleets with `kind:"fleet"`; Library pages: ⌘↩ and double-click do nothing. Emit stubbed `select-fleet` / `open-page` events (call the stored listener callbacks) → the page switches to Fleets with that fleet selected / to the named page.
- [x] Commit: `feat(hub): ⌘↩, ⋯, double-click, and ⌘K open the selected agent or fleet in a window`

**Done (2026-09-06):** 9.4 and 9.5 as written. Browser acceptance drove the triggers with dispatched `keydown` / `dblclick` events and the stub's stored listener callbacks; the ⌘K label shows the agent's `name` (like the existing Start/Stop items), the window title gets the display name.

**Manual acceptance PR 9 (real Hub build — needed once the drive allows it, otherwise the PR says it is unverified):** overlay title bar with traffic lights and the display name in Mission Control; second ⌘↩ focuses the existing window; the window cascades from the Hub and reopens where it was moved; Close with an unsaved Persona edit prompts, Cancel keeps the window; **Show in Hub** raises the dashboard on Agents (agent selected) / Fleets (fleet selected); editing the agent in the window then clicking the Hub shows the new value in the dashboard detail; deleting a fleet from its window closes the window and the Hub list drops it.

## Spec coverage

| Spec § | Task |
|---|---|
| 3 windows, capability, bridge command | 9.1 |
| 4 route, root, agent / fleet bodies, missing state, CSS | 9.2 (parser), 9.3 |
| 5 `FleetDetailPane` | 8.1 (+ `onOpenInWindow` in 9.5) |
| 6 refetch on focus | 9.4 |
| 7 triggers | 9.5 |
| 8 closing | 9.3 (guard), 9.3 (`onDeleted` closes) |
| 9 non-macOS | 9.1 (`cfg`), 9.3 (`detail-window--inset` only on macOS) |
| 11 tests | 9.1 (Rust), 9.2, 9.5 (`SourceList`, `isPageId`); browser + real-window lists per task |
