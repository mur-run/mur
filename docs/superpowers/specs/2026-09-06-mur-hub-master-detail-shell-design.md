# MUR Hub 2.0 — Master–Detail Shell and Design Language

**Date:** 2026-09-06
**Status:** Approved design (brainstormed with David, 2026-09-06); implementation plan pending
**Scope:** `mur-hub-gui/ui` (Tauri 2 + React 18 + plain CSS) and the `dashboard` window in `mur-hub-gui/src-tauri/tauri.conf.json`. Phase 1 covers the shell, the design tokens, and the Agents and Fleets pages. Library, Home, Chats and Settings pages are later phases.
**Mockup:** https://claude.ai/code/artifact/afaac847-5a8a-4ed5-9823-358d6731c256 (theme / page / window-width toggles)

## 1. Problem

Measured on the running Hub (v2.71.24, dark mode, 960 px window), not inferred from the code:

- **Fleets page shows four columns.** Shell sidebar 220 + fleet rail 220 + `FleetDetail` + inspector 320. `FleetDetail` is squeezed to roughly 180 px; the "Run as loop" button is clipped mid-word. `FleetDetail` and `FleetInspector` render the same goal / members / loop data side by side.
- **Agents page shows four columns.** Sidebar 220 + role rail 160 + card grid + inspector 320. The three-column card grid drops to ~60 px per card; agent names are cut. The inspector's tab bar shows 4 of the 11 tabs.
- **Root cause.** The inspector column was designed as a 320 px "supporting information" pane (spec 2026-07-05 §1), but `AgentInspector` is the agent's *primary* editor: eleven tabs of forms. `DashboardApp` compensates by growing the window 320 px whenever any selection exists (`desiredW = 960 + 320`), which is why the window changes size when a row is clicked.

The visual language has the same accretion problem: pastel light-blue tint on every surface, a solid brand-blue sidebar selection, a hero greeting occupying the top of the crushed grid, and status colours drawn differently by the list, the card, the inspector header and Home.

## 2. Decisions

| Decision | Outcome | Rejected |
|---|---|---|
| Brand direction | **Keep the identity** (blue starling mascot, coral CTA, blue brand) and redo the design language neutral-first. | New palette / new mascot treatment — would split the desktop Hub from app.mur.run, the iOS app and the desktop Pet, which all share the identity. |
| Detail navigation | **Master–detail**: fixed-width source list + full-width detail pane. Inspector column retired for Agents and Fleets. | Push navigation (full-page detail + back): loses list context, hides other agents' status while editing, back-stack state to manage, worse than today's fleet rail. Modal dialog: agent detail is a long-lived object with live state, not a task with an end; dims everything else; can't compare. Side drawer: option 3 with an overlay — the 11-tab width problem is unchanged. Resizable inspector (min 420): still squeezes the grid; wrong idiom for a primary editor. |
| Pop-out | **"Open in window" (⌘↩) as an additive action in Phase 2**, reusing the Phase 1 detail component. Never the only path. | Separate window as the primary path: window management for 26 agents, and two windows fetching the same state — this codebase's most common failure. |
| Scope of this spec | Sections 3–7 below are all Phase 1. They depend on each other: without breakpoints the tab IA doesn't fit; without the component family, Phase 2 rebuilds it. | Splitting tokens or components out into their own project. |
| Deferred | Open-in-window, multi-select bulk start/stop, Quick Look preview, side-peek from Home. | — |

## 3. Shell

### 3.1 Three panes with fixed roles

```
┌────────┬──────────────┬──────────────────────────────────┐
│ sidebar│ source list  │ detail                            │
│ 220/56 │ 300/280      │ everything that is left           │
│ (nav)  │ (this page's │ (header + tabs + body, or the     │
│        │  objects)    │  page overview when nothing is    │
│        │              │  selected)                        │
└────────┴──────────────┴──────────────────────────────────┘
```

| Window width | Sidebar | Source list | Detail |
|---|---|---|---|
| ≥ 1200 | 220 (labels) | 300 | remainder, ≥ 680 |
| 960 – 1199 | 56 (icon rail, badge kept) | 280 | remainder, ≥ 620 |
| < 960 | 56 | collapses to a toggleable overlay | full width |

- The breakpoint decision is one pure function `sidebarModeFor(width, userPref)` in `components/shell/breakpoints.ts`. The user can pin either mode with **⌘\\**; the pin is persisted (`mur.shell.sidebar` = `auto` / `expanded` / `collapsed`) and wins over the width rule.
- The list ↔ detail divider is draggable, 240–400 px, persisted per page (`mur.<page>.listWidth`). Implemented as a pointer-drag on a 6 px hit target over the 1 px border; double-click resets to default.
- **`DashboardApp`'s window auto-resize effect is deleted.** `tauri.conf.json` `dashboard` window becomes 1200 × 760 default, 900 × 560 minimum (today: 960 × 620 / 720 × 480). Nothing in the UI calls `setSize` after launch.
- Column widths remain CSS custom properties on the shell grid (`--shell-sidebar-width`, `--shell-list-width`); the inspector track (`--shell-inspector-width`, `.shell--with-inspector`) stays only for the Chats and Library pages until Phase 2.

### 3.2 Title bar

macOS: `titleBarStyle: "Overlay"` + `hiddenTitle: true` on the `dashboard` window, so the traffic lights sit inside the sidebar and the sidebar vibrancy runs to the top edge. The sidebar gets a 28 px top inset (`padding-top`) reserved for the lights and is a drag region (`data-tauri-drag-region`). Windows/Linux keep the default decorations; the inset is 0 there (`navigator.platform` check in one place, `shell/platform.ts`).

### 3.3 Global toolbar retired

Today's `dashboard__bar` (brand mark, search, New Agent, grid/list toggle, refresh, settings) is removed. Its contents move:

| Item | New home |
|---|---|
| Search (⌘K) | Sidebar top: a search field that opens the command palette (§6.6) |
| New Agent | "+" in the Agents source-list header (New Fleet likewise) |
| Grid / list toggle | Removed. The grid is the Agents overview (§3.4); the list is the source list. |
| Refresh | Removed from chrome; becomes a palette command and gains a ⌘R binding to the existing refresh handler (there is no keyboard binding today) |
| Settings (⌘,) | Sidebar bottom, with the Hub version and CLI-skew nudge line under it |

Banners currently rendered above the bar (onboarding, upgrade nudge, app update, CLI skew) render at the top of the **detail pane**, not above the shell, so they never push the sidebar and list down.

### 3.4 Inspector retired for Agents and Fleets

`hasInspector()` returns `false` for `agents` and `fleets`; `AgentInspector` becomes `AgentDetail` in the detail pane; `FleetInspector` is deleted (its content already exists in `FleetDetail`). Chats and Library pages keep their inspectors unchanged in Phase 1 — they only pick up the token recolor.

When nothing is selected the detail pane shows the page **overview**: the greeting + mascot, the pet-card grid (existing `GridCard`, at the card's designed size, never squeezed), and quick actions. Clicking a card selects it; the list never changes width.

## 4. Component family

Both pages are built from the same two containers so Phase 2 (Skills, Workflows, MCP, Models, Plugins) is assembly, not design.

### 4.1 `SourceList`

```
components/shell/SourceList.tsx
  header:  title · count · "+" (onCreate)
  filter:  text field, ⌘F focuses it, filters the visible rows client-side
  chips:   facet chips with counts — replaces the Agents role rail and the
           Fleets label rail; single-select, "All" first
  rows:    <SourceRow avatar name subtitle status needsYou selected onSelect>
  keyboard: ↑↓ move selection, Enter selects, Esc clears selection
```

Row anatomy: 28 px avatar (existing `PetFace`/initials for agents; a neutral stacked-layers glyph for fleets), name (500 weight, ellipsis), subtitle (role · model for agents; member count · trigger for fleets), then on the right the **needs-you badge** (amber, count) and the **status dot**. Selected row = translucent brand tint (`--color-brand-soft`), not a solid fill.

### 4.2 `DetailPage`

```
components/shell/DetailPage.tsx
  header:  avatar(48) · name + StatusPill · meta line · actions (primary,
           secondary, "⋯" overflow menu)
  tabs:    horizontal, 2 px brand underline; ARIA tablist/tab/tabpanel
  body:    scrollable; banners slot at the top
  dirty:   `useDirtyGuard()` — a tab that has unsaved edits blocks selection
           change and tab change with a confirm (macOS-style sheet copy:
           "Discard changes to <tab>?")
```

### 4.3 Agent tabs: 11 → 6

| New tab | Contains today's tabs | Landing content |
|---|---|---|
| **Overview** | — (new) | needs-you banner (from `useInbox`), Now card (current task / last activity), stat row (cost today · turns · tool calls · since last activity), recent conversation (3 lines + Open chat), setup at a glance (model, skills, MCP, permissions, schedule, memory — each row links to its tab) |
| Identity | persona, style, behavior | sub-sections with sticky in-tab headings, in that order |
| Capabilities | skills, mcp, plugins, permissions | same |
| Memory | memory | unchanged body |
| Automation | schedule | unchanged body |
| Channels | inbox, mobile | inbox first |

The model / fallback-chain / smart-routing block that `AgentInspector` currently renders above the Persona form moves into **Identity** as its first section.

`DetailTab` (the 11 ids) is kept as the deep-link vocabulary: `desiredDetailTab` from Home still names the old id, and `detailGroupOf(tab)` (pure, tested) maps it to the new tab plus an in-tab anchor. Nothing that sets `desiredDetailTab` changes.

Header actions: **Chat** (primary — navigates to the Chats page with this agent active; `ChatsPage` gains an `initialAgent` prop for this, it has none today), **Stop/Start**, **⋯** (Export, Duplicate, Open in window [Phase 2], Delete). Delete stays behind a confirm.

### 4.4 Fleet tabs

| New tab | Contains today's `FleetDetail` sections |
|---|---|
| **Overview** | goal, stat row (last run · max iterations · budget · done-when, all from `loop_cfg`), members summary, jobs preview (3 rows), loop summary with links to Settings. Iterations used and budget spent have no Hub source yet (`FleetLoopView` carries only the limits); they render "—" until the `fleet status` run record (spec 2026-08-17) is exposed to the Hub — a follow-up, not Phase 1 |
| Members | member add/remove (`fleet-detail__mgmt` members part) |
| Jobs | job list with the active/all toggle and cancel |
| Settings | trigger / cron / loop guards / done-when / labels (`fleetSettingsForm`), Danger zone last |

Header actions: **Run ▾** split button (Run once / Run as loop / Send job…), **Stop/Start**, **⋯** (Export, Import, Delete). The worktree toggle moves next to the run options in the split menu. Fleet running state (from `fleet_detail` + the run events `FleetView` already listens to) drives the header pill; the `.stopped` sentinel shows as **Stopped** (red) with the Start action promoted to primary.

### 4.5 Status vocabulary — one component

`StatusDot` and `StatusPill` (`components/shell/Status.tsx`) replace the per-surface pill markup in `ListRow`, `GridCard`, `AgentInspector` header, `FleetDetail` header and Home's Now Running. Mapping:

| State | Colour token | Where |
|---|---|---|
| running | `--status-running` (green) | dot, pill |
| idle | `--status-idle` (slate) | dot, pill |
| stopped / failed | `--status-failed` (red) | dot, pill |
| restarting | `--status-restarting` (amber) | pill only |
| **needs you** | `--status-attention` (amber badge with count) | list row, Home badge, Overview banner |

Amber on a *badge* always means "waiting for you"; amber on a *pill* means restarting. They never share a shape.

### 4.6 `SplitButton`

Primary action + chevron; the chevron opens a menu. Used by Fleet Run in Phase 1; available to Library pages in Phase 2.

## 5. Design language (tokens)

Components already consume only `semantic.css`; this section changes that file and adds a few primitives. No component references a raw hex.

### 5.1 Colour usage rules

1. **Surfaces are neutral.** Light: window `#F4F5F8`, sidebar `#EEF0F4` (under vibrancy), list `#F9FAFC`, detail `#FFFFFF`, card `#FFFFFF`, secondary fill `#F4F5F8`; hairline `rgba(22,36,59,.10)`. Dark: unchanged ramp (`#0F1117` / `#13161E` / `#161922` / card `#1B2030` / secondary `#20263A`). Semantic names: `--surface-window`, `--surface-sidebar`, `--surface-list`, `--surface-detail`, `--surface-card`, `--surface-secondary`, `--border-line`, `--border-line-subtle`.
2. **Brand blue = selection and the primary action.** Selection is the translucent tint `--color-brand-soft`; solid `--color-brand` is reserved for the one primary button per header and the active-tab underline. The solid-blue sidebar selection goes away.
3. **Coral appears once per screen.** The mascot, or a single first-run / empty-state CTA. Never on a routine button.
4. **Status colours are not accents.** Only `StatusDot`/`StatusPill`/badge use them.
5. Text: `--text-primary` `#16243B` / `--text-secondary` `#5F6C80` / `--text-tertiary` `#98A3B3` (light); dark unchanged. Both themes are designed, not inverted: the tint alphas differ per theme (already the pattern in `semantic.css`).

### 5.2 Type, space, radius

- Type scale (px): 11 (eyebrow/labels, uppercase, +.06em), 12, 13 (lists, controls), 14 (body), 15 (list-pane title), 17 (stat values), 20 (detail title). Weights 400 / 500 / 600 only. Numbers that align use `font-variant-numeric: tabular-nums`. Ids, channel names and commands use the system mono stack (`ui-monospace, "SF Mono", Menlo`).
- Space: 4 / 8 / 12 / 16 / 24 / 32. Radius: 6 (controls), 8 (rows), 12 (cards, panes). `--radius-lg/xl/2xl` become aliases of 12 px in PR 1 so no component changes; the aliases are deleted once nothing references them.
- Cards: hairline border, no shadow (shadows are invisible in dark mode and noisy in light). `--shadow-pop` remains for menus and popovers only.

### 5.3 Motion and accessibility

- Detail pane content cross-fades 150 ms on selection change; sidebar collapse animates width 200 ms; list selection is instant. All motion is disabled under `prefers-reduced-motion`.
- Tabs use `role="tablist"`/`tab`/`tabpanel` with arrow-key navigation; the source list is a `listbox`; every interactive element has a visible `:focus-visible` ring (`--shadow-focus`). Text contrast ≥ 4.5:1 on both themes, checked for the tertiary text on each surface.

### 5.4 Side effect, stated up front

Tokens are global. Landing PR 1 (§8) recolours Home, Chats, Library and every modal before their layouts change. This is intended: one consistent palette everywhere, layouts follow per phase.

## 6. Data flow and state

1. **Selection** stays where it is: `AgentContext.selectedAgent` for agents, `fleetName` in `DashboardApp` for fleets. New: the last selection per page is persisted (`mur.agents.lastSelected`, `mur.fleets.lastSelected`, same `localStorage` pattern as `mur.fleet.labelFilter`) and restored when the page is reopened, if the object still exists.
2. **Deep links** from Home (HITL card → agent → Channels) keep using `desiredDetailTab` with the old tab id; `detailGroupOf` resolves the new tab and anchor.
3. **Needs-you badge** = `useInbox()` items grouped by agent. `InboxItem` has no agent field (`kind`, `id`, `ts`, `payload`), so `agentOf(item)` reads it per kind from the payload: HITL → the channel's agent, companion message → the agent it was polled for, install request → the target agent; blocked skill upgrades are global and count toward the Home badge only. No new store; dismissal is the existing session-local set in `DashboardApp`.
4. **Overview data** comes only from sources that exist: runtime state and uptime from `AgentRuntimeStatus`; the Now card and recent conversation from the agent's channel summary (`channel_list`, the same call Home's Recent Activity uses via `useChannels`); fleet last-run and limits from `fleet_detail`'s `loop_cfg`. Cost today and turns today have no per-agent source in the Hub yet — they render "—" and are listed as a follow-up, not a new Tauri command in Phase 1.
5. **Dirty guard**: each editable tab registers its dirty state with `DetailPage`; the source list and tab bar consult it before changing selection. Saves remain explicit per section (a running agent re-reads its profile on its next turn), with inline "Unsaved changes" text next to the Save button.
6. **Keyboard map** (Phase 1): ⌘K command palette — jump to any agent / fleet / page, plus four actions on the current selection (Start, Stop, New chat, Open Settings); ⌘F filter the current list; ↑↓ / Enter / Esc in the list; ⌘1–9 pages in sidebar order; ⌘\\ sidebar; ⌘, Settings. ⌘↩ is reserved for Phase 2 open-in-window. The palette is the existing ⌘K search field promoted: same trigger, same `query` state, results grouped by kind.

## 7. Errors and empty states

- List fails to load: the list pane shows the error and a Retry button; the detail pane keeps its last content.
- Detail fails to load: header keeps name + status from the list's data; body shows the error and Retry.
- Empty list: mascot + one CTA (Create agent / New fleet). No selection: the overview (§3.4), never a blank pane.
- Save fails: stay on the tab, message beside the field group, no toast-and-forget. Toasts remain for successes and for actions with no owning surface (export path, run started).
- Fleet `.stopped`: header pill **Stopped**, Run disabled with a hint, Start promoted.

## 8. Implementation order and file touchpoints

Five PRs, each independently shippable; the app stays usable between them. Files over 800 lines are split on the sibling pattern (`components/detail/agent/{Header,Overview,Identity,Capabilities,Channels}.tsx`, `components/detail/fleet/{Header,Overview,Members,Jobs,Settings}.tsx`).

| PR | Contents | Touches |
|---|---|---|
| 1 Tokens | §5 semantic/primitive token changes; `StatusDot`/`StatusPill` extracted and adopted by existing surfaces. Recolor only, no layout change. | `styles/tokens/*.css`, `components/shell/Status.tsx`, `ListRow`, `GridCard`, `AgentInspector` header, `FleetDetail` header, `home/NowRunning` |
| 2 Shell | Overlay title bar, breakpoints + ⌘\\, sidebar search field and Settings footer, draggable list divider (unused until PR 4), delete the window auto-resize, banners moved to the detail slot. Global bar keeps New Agent until PR 4. | `tauri.conf.json`, `Shell.tsx`, `Sidebar.tsx`, `shell/breakpoints.ts`, `shell/platform.ts`, `shell.css`, `DashboardApp.tsx` |
| 3 Components | `SourceList`, `DetailPage` (+ `useDirtyGuard`), `SplitButton`, `detailGroupOf`, command palette. Unit tests. No page adopts them yet. | `components/shell/SourceList.tsx`, `DetailPage.tsx`, `SplitButton.tsx`, `CommandPalette.tsx`, `inspector/detailTabs.ts` |
| 4 Agents | Agents page rebuilt on `SourceList` + `AgentDetail` (from `AgentInspector`); role rail, grid/list toggle and the global bar removed; grid becomes the overview; `hasInspector` false for agents; last-selection persistence. | `agents/AgentsPage.tsx`, `components/detail/agent/*`, `shell/Inspector.tsx`, `DashboardApp.tsx`, `dashboard.css`, `detail-panel.css` |
| 5 Fleets | `FleetView` on `SourceList` (label chips replace `FleetRail`); `FleetDetail` split into the four tabs; Run split button; `FleetInspector` deleted; `hasInspector` false for fleets. | `fleet/FleetView.tsx`, `components/detail/fleet/*`, `fleet.css`, `shell/Inspector.tsx` |

i18n: every new string lands in both `en.ts` and `zh-TW.ts` in the same PR (existing rule). Brand name stays uppercase **MUR** in all user-visible strings.

## 9. Testing

Vitest (existing runner, `npm test` in `mur-hub-gui/ui`):

- `shell/breakpoints.test.ts` — `sidebarModeFor(width, pref)` across the three bands and the three pins.
- `inspector/detailTabs.test.ts` — `detailGroupOf` maps all 11 legacy ids; unknown ids fall back to Overview (mirrors today's `ALL_DETAIL_TABS.includes` guard).
- `home/needsYou.test.ts` — inbox items → per-agent counts; dismissed items excluded.
- `shell/SourceList.test.tsx` — filter text + chip facet compose; keyboard selection.
- Existing `nav.test.ts` / `shell.test.ts` keep passing; the inspector-toggle shortcut test stays valid for Chats/Library.

Manual acceptance per PR 4 and PR 5, against the mockup: 960 and 1200 widths × light and dark × selection / no selection, plus Esc, ⌘F, ⌘K, the dirty guard, and a Home HITL deep link landing on Channels.

## 10. Later phases

- **Phase 2 — Library pages + open-in-window.** Skills / Workflows / MCP / Models / Plugins adopt `SourceList` + `DetailPage`; `LibraryInspector` retired; ⌘↩ opens the current detail in its own Tauri window reusing `DetailPage` (state read from the same Tauri commands, no second data path).
- **Phase 3 — Home / Chats / Settings**, multi-select bulk actions in `SourceList`, Quick Look preview (space bar), side-peek from Home.
