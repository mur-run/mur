# MUR Hub 2.0 — Phase 2(b): ⌘↩ open-in-window

**Date:** 2026-09-06 · **Status:** Draft — awaiting review
**Follows:** `2026-09-06-mur-hub-master-detail-shell-design.md` (§2 "Pop-out", §6.6 keyboard map, §10) and `2026-09-06-mur-hub-library-master-detail-design.md` (Phase 2(a), shipped in #1171–#1173).
**Scope:** Hub GUI only (`mur-hub-gui/ui`, `mur-hub-gui/src-tauri`). No CLI, daemon, or runtime change.

## 1. Problem

The Phase 1 shell shows one agent or fleet at a time in the detail pane. Comparing two agents, or keeping a fleet's Jobs tab visible while working elsewhere in the Hub, means flipping the selection back and forth. The shell spec reserved ⌘↩ for an additive "Open in window" path and deferred it to Phase 2; this spec defines it.

The Hub already has one detached-window mechanism to copy: `open_chat_window` (`src-tauri/src/chat_window.rs`) builds a `chat-<agent>` webview on `index.html#/chat/<agent>`, focuses an existing one instead of rebuilding, and `App.tsx` routes the hash to a dedicated React root with its own capability file (`capabilities/chat.json`).

## 2. Decisions

| Question | Decision | Rejected |
|---|---|---|
| What opens in a window | **Agent and fleet detail.** Library items keep ⌘↩ inert (their detail is a few rows; nothing to keep open). | All four Library pages: a fourth root and label kind, plus per-window install-target and Used-by actions, for no real use. |
| Window chrome | **Ordinary document window**: resizable, not always-on-top, overlay title bar on macOS like the dashboard, title = the agent's / fleet's display name. | The chat window's HUD look (frameless, translucent, always-on-top): fine for a floating panel, wrong for six tabs of forms. |
| How the window gets its data | **The same components on the same Tauri commands**: `AgentDetail` already loads `get_agent_detail`; the fleet detail's loading moves out of `FleetView` into a `FleetDetailPane` both hosts render. No second data path. | Loading `DashboardApp` in a window pinned to one selection: every window becomes a second Hub (inbox polling, nudges, palette) — the duplicate-state failure the shell spec names. A child webview inside the dashboard: not a window. |
| Cross-window consistency | **Refetch on window focus.** Each detail reloads when its window becomes focused, unless it has unsaved edits. Runtime state keeps arriving on the existing `runtime-status-changed` event. | Backend push on every profile / fleet write: one missed `emit` = one stale window forever. |
| Actions that navigate | **Bridge back to the dashboard.** "Chat" opens the existing chat window; "Home" and "Show in Hub" go through the existing `open_dashboard` command, extended to carry a page or a fleet. | Hiding those actions in the window: the same detail would differ between hosts. |
| Triggers | ⌘↩ · the detail's ⋯ menu · double-click on a `SourceList` row · a ⌘K selection action. | — |

## 3. Windows (Rust)

`src-tauri/src/detail_window.rs` mirrors `chat_window.rs`.

- **Labels.** `detail-agent-<safe>` and `detail-fleet-<safe>`, where `safe` is `chat_window::safe_label_part` — the existing char-map (alphanumerics and `-` kept, everything else → `-`) extracted into a `pub(crate) fn` so the three window kinds share one rule. Labels are an implementation detail; the UI never builds them.
- **Command.** `open_detail_window(kind: DetailKind, name: String, title: String, app: AppHandle) -> Result<(), String>` with `enum DetailKind { Agent, Fleet }` (serde `rename_all = "lowercase"`). Single-instance guard first: if `app.get_webview_window(&label)` exists, `show()` + `set_focus()` and return. Otherwise build:
  - URL `index.html#/detail/<kind>/<urlenc(name)>` (`chat_window::urlenc`, spaces → `+`, also shared).
  - `.title(&title)` · `.inner_size(960.0, 640.0)` · `.min_inner_size(720.0, 520.0)` · `.resizable(true)` · `.visible(false)`.
  - macOS only: `.title_bar_style(tauri::TitleBarStyle::Overlay)` and `.hidden_title(true)` — the dashboard's configuration, so the traffic lights sit inside the page. Other platforms keep native decorations.
  - Position: the dashboard's `outer_position()` offset by `(40, 40)` logical points (scaled by the dashboard's `scale_factor()`), so windows cascade from the Hub. `tauri-plugin-window-state` is already installed with `StateFlags::all() - SIZE`, so a window that was moved reopens where it was; size always starts at the default, which is the plugin's existing behaviour for every Hub window.
  - `show()` last.
- **Capability.** `src-tauri/capabilities/detail.json`, `"windows": ["detail-*"]`: `core:default`, `core:event:allow-listen`, `core:event:allow-unlisten`, `core:window:allow-close`, `core:window:allow-set-focus`, `core:window:allow-start-dragging`, `core:window:allow-set-size`, `core:window:allow-set-min-size`, `dialog:allow-confirm`, `dialog:allow-open`, `dialog:allow-save`, `shell:allow-open`. This is `default.json`'s plugin set (the agent detail exports through the save dialog and confirms discards) plus the window controls the chat window has.
- **Bridge.** `open_dashboard` (`lib.rs`) gains two optional parameters: `open_dashboard(app, agent_name: Option<String>, fleet_name: Option<String>, page: Option<String>)`. After show + focus it emits, in order and only when given: `select-agent` (existing), `select-fleet` (new, payload = fleet name), `open-page` (new, payload = page id). Existing callers pass only `agent_name` and are unaffected.
- **Registration.** Both commands in the `generate_handler!` list; `detail_window` module declared next to `chat_window`.

## 4. UI root

- **Route.** `App.tsx`'s `getRoute()` gains `"detail"` for hashes starting `#/detail/`. Parsing lives in `src/components/detail/window/detailRoute.ts`: `parseDetailRoute(hash): { kind: "agent" | "fleet"; name: string } | null` (decodes `+` → space and `decodeURIComponent`, like `agentNameFromHash` in `AgentChatWindow.tsx`). Unknown kind or empty name → `null` → the root renders the error state below.
- **`DetailWindow`** (`src/components/detail/window/DetailWindow.tsx`) is the route's component:
  ```
  <AgentProvider>            // agents + runtime statuses, as the dashboard
    <DirtyProvider>          // per window: closing checks only this window's edits
      <div className="detail-window">
        <div className="detail-window__bar" data-tauri-drag-region>   // 38px, macOS inset
          <button className="btn btn--secondary">Show in Hub</button>
        </div>
        <AgentDetail … /> | <FleetDetailPane … /> | error state
  ```
  `AgentProvider` also listens to `select-agent`; in a detail window that only changes a context field nobody reads. The window ignores `selectedAgent` and takes its target from the hash.
- **Agent.** `AgentDetail` as-is, with: `entry` / `runtime` from `useAgents()`; `channels` from `useChannels()` and `needsYou` from `needsYouCounts(visibleInboxItems(useInbox().items, new Set()))` (the dismissed set is per-session state in `DashboardApp`; a fresh window has dismissed nothing) — the exact hooks and helpers `DashboardApp` uses (`src/components/home/useChannels.ts`, `useInbox.ts`, `inbox.ts`, `needsYouCounts.ts`), so the badge and the Channels tab show what the dashboard shows. `onOpenChat` → `invoke("open_chat_window", { agentName })`. `onOpenHome` → `invoke("open_dashboard", { page: "home" })`.
- **Fleet.** `FleetDetailPane` (§5) fed by a `useFleetWindowData(name)` hook that loads `fleet_list` (for the summary: status + labels), `fleet_labels_list`, and `list_agents` → `agentMap`, and reloads them on `onRefresh`. `onDeleted` → `getCurrentWindow().close()`.
- **Show in Hub.** Agent: `open_dashboard { agentName }`. Fleet: `open_dashboard { fleetName }`. `DashboardApp` adds two listeners beside the existing `select-agent` one: `select-fleet` → `setFleetRequest(name); setPage("fleets")` (the palette's jump path); `open-page` → `setPage(id)` when `id` is a `PageId`.
- **Error state.** Route unparsable, or the agent / fleet no longer listed after load: a centred `detail-window__missing` message ("This agent no longer exists." / fleet variant) with a Close button (`getCurrentWindow().close()`). `AgentDetail`'s own error banner still covers a failing `get_agent_detail` for an agent that is listed.
- **Title.** The window title is set at creation from `title`; the root does not rename it (a rename inside the detail lands on the next open).
- **CSS.** `src/styles/components/detail-window.css`: `.detail-window` (grid: bar + 1fr, full height), `.detail-window__bar` (38px, flex-end, `-webkit-app-region` is handled by `data-tauri-drag-region`), `.detail-window__missing`. Non-macOS: the bar keeps its height so the layout does not shift; only the traffic-light inset is a macOS matter.

## 5. `FleetDetailPane` (pure movement)

`src/components/detail/fleet/FleetDetailPane.tsx` takes over from `FleetView` exactly these responsibilities: `detail` + `jobs` state, `loadDetail(name)` (`fleet_detail` + `fleet_jobs`, stale-guarded on the name), the `fleet:run_done` listener (toast + reload of this fleet), the `tab` state reset per fleet, and the `DetailPage` render with `FleetHeader` / `FleetOverview` / `FleetMembers` / `FleetJobs` / `FleetSettings`.

```ts
export interface FleetDetailPaneProps {
  name: string;
  summary: FleetSummary;              // status + labels, from fleet_list
  labels: LabelView[];
  agentMap: Map<string, AgentEntry>;
  onRefresh: () => void;              // host reloads its list / labels; the pane reloads detail + jobs itself
  onDeleted: () => void;              // host clears selection or closes the window
  onOpenInWindow?: () => void;        // dashboard only: the ⋯ item; undefined inside a window
}
```

`FleetView` keeps the list, selection, restore, label filter, and `handleCreated`, and renders `<FleetDetailPane key={name} …/>` for the selection. Its `fleet:run_done` handling shrinks to the list reload; the pane's copy reloads the detail. Behaviour is unchanged by this PR (§10, PR 8); the seam exists so the window and the page render one component.

## 6. Freshness

- **Refetch on focus.** `AgentDetail` and `FleetDetailPane` subscribe to `getCurrentWindow().onFocusChanged`; on `focused === true` they call their existing loader (`get_agent_detail` / `loadDetail`) unless `useDirtyGuard().isDirty` — an in-progress form is never overwritten. This runs in the dashboard too, so editing in a window and switching back to the Hub shows the new values without a manual refresh. The subscription is removed on unmount.
- **Runtime state** keeps flowing on `runtime-status-changed` to every `AgentProvider`; nothing changes.
- **Lists** (agents, fleets, labels) are not refetched on focus in this phase: the dashboard's `AgentProvider` already reloads on `agents-updated`, and fleet lists reload on the existing paths (`handleRefresh`, `fleet:run_done`). A fleet renamed in a window shows its old row title in the Hub until one of those fires; acceptable for 2(b), noted in §11.

## 7. Triggers

- **⌘↩** — `isOpenInWindowShortcut(e)` in `src/components/shell/openInWindow.ts` (`(meta || ctrl) && !alt && !shift && key === "Enter"`). `DashboardApp`'s keydown handler calls it and, when `page` is `agents` with `selectedAgent`, or `fleets` with a selected fleet, invokes `open_detail_window`. Ignored when the active element is an input, textarea, select, or a `[contenteditable]`; the `SourceList` listbox does not own Enter with a modifier, so no conflict. Inside a detail window ⌘↩ is not bound; ⌘W is the native close.
- **⋯ menu** — `AgentDetail` gets an `onOpenInWindow?: () => void` prop rendered as the item "Open in window" after "Open chat in a window"; `FleetHeader` the same item after Import. Undefined inside a window (the item is not rendered).
- **Double-click** — `SourceList` gains `onOpen?: (id: string) => void`; the row's `onDoubleClick` calls it. `AgentsPage` and `FleetView` pass it; Library pages do not, so their rows keep ignoring double-click.
- **⌘K** — `DashboardApp`'s `paletteItems` adds `action:openInWindow` ("Open <name> in window") next to Start/Stop when an agent is selected on the Agents page, and the fleet equivalent when a fleet is selected. `FleetView` reports its selection up through its existing `onSelect?: (name: string | null) => void` prop (PR 5 stopped passing it from `DashboardApp`; it is wired again because the palette and ⌘↩ need it), stored in `DashboardApp` as `selectedFleet`.
- **Opening** — one helper `openDetailWindow(kind, name, title)` in `openInWindow.ts` wraps the invoke and toasts the error; every trigger calls it. `title` is the display name.

## 8. Closing

- `DetailWindow` registers `getCurrentWindow().onCloseRequested(async (e) => { if (!(await confirmLeave(body, title))) e.preventDefault(); })` with the same `confirmLeave` / strings `AgentsPage` uses for switching selection. A window with no unsaved edits closes immediately.
- Deleting the fleet from inside its window closes the window on success (§4). Nothing in the agent detail deletes the agent, so an agent window only closes by the user or by the error state's button.
- Quitting the Hub closes detail windows with it; nothing is persisted about which windows were open (reopening is one ⌘↩).

## 9. Non-macOS

Native decorations; `title_bar_style` / `hidden_title` are behind `#[cfg(target_os = "macos")]` like the dashboard's own config. `DetailWindow` keeps the 38px bar on every platform (it holds "Show in Hub"); only the drag region matters on macOS. Ctrl replaces ⌘ in the shortcut through the existing `metaKey || ctrlKey` convention.

## 10. Implementation order

- **PR 8 — `FleetDetailPane` extraction.** §5 only. Pure movement; `FleetView` behaviour identical (manual check: select / restore / run_done toast / delete / palette jump). No user-visible change, no i18n change.
- **PR 9 — windows, route, triggers, bridge.** §3, §4, §6, §7, §8, §9. Rust: `detail_window.rs`, `capabilities/detail.json`, `open_dashboard` parameters, shared `safe_label_part` / `urlenc`. UI: `detailRoute.ts`, `DetailWindow.tsx`, `openInWindow.ts`, `detail-window.css`, `SourceList.onOpen`, `AgentDetail.onOpenInWindow`, `FleetHeader` item, `FleetView.onSelect`, `DashboardApp` (⌘↩, palette items, `select-fleet` / `open-page` listeners, `selectedFleet`), focus refetch in `AgentDetail` / `FleetDetailPane`, i18n in both tables.

## 11. Testing

- **Pure functions (Vitest):** `parseDetailRoute` (agent / fleet / `+` and percent decoding / unknown kind / empty name); `isOpenInWindowShortcut` (meta, ctrl, rejects alt/shift/plain Enter); `isPageId` (nav ids in, strangers out); `SourceList` markup identical with and without `onOpen` (`renderToStaticMarkup`, as its existing test). The palette items are built inline in `DashboardApp` and are covered by the browser acceptance, not a unit test.
- **Rust unit tests:** `safe_label_part` (spaces, unicode, dots → `-`; alphanumerics and `-` kept) and `detail_label(kind, name)`.
- **Browser acceptance (dev server + the Tauri stub the Phase 1 plan describes):** `#/detail/agent/aura` and `#/detail/fleet/nightly` render the same detail as the dashboard; unparsable hash and unknown name show the missing state; `open_detail_window` / `open_dashboard` invocations are observed with the expected args from ⌘↩, the ⋯ item, double-click, and the palette; PR 8 leaves `FleetView` behaviour unchanged.
- **Real window (needs a Hub build):** overlay title bar and traffic lights; cascade position; second ⌘↩ focuses instead of duplicating; close with unsaved edits prompts; "Show in Hub" raises the dashboard on the right page; editing in a window and refocusing the Hub shows the new value. Not verifiable on the current dev drive (Hub target does not fit); the PR says so.

## 12. Later

- Window title following a rename; list refetch on focus (§6).
- Restoring open windows across launches.
- Library detail windows if a use case appears (the route and command are kind-agnostic; adding a kind is a `DetailKind` variant, a root branch, and a label prefix).
- Quick Look (space bar) and side-peek stay Phase 3 per the shell spec.
