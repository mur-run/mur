# MUR Hub 2.0 Phase 2(a) — Library pages on the master–detail shell

**Date:** 2026-09-06
**Status:** Approved design (brainstormed with David, 2026-09-06); implementation plan `docs/superpowers/plans/2026-09-06-mur-hub-library-master-detail.md`
**Scope:** `mur-hub-gui/ui` Skills, Workflows, MCP and Plugins pages, plus one Tauri command (`skills_installed`) in `mur-hub-gui/src-tauri`. Builds on Phase 1 (`2026-09-06-mur-hub-master-detail-shell-design.md`, shipped as #1165–#1169). Models stays as it is. ⌘↩ open-in-window is Phase 2(b), a separate spec.

## 1. Problem

After Phase 1 the Agents and Fleets pages are master–detail, but the four Library pages still work the old way: a flat card list with an install toolbar on top, and a 320 px `LibraryInspector` that shows the same five fields the card already shows (name, version, origin, description, a few meta rows). Three consequences:

- Two layouts in one app. The Library pages are the only place the inspector column still appears for a primary object.
- The detail has nowhere to grow. "Which agents use this skill" and per-agent enable / remove exist as commands (`agent_skill_toggle`, `agent_mcp_remove`, …) but only inside each agent's own Capabilities tab.
- `skills_installed` knows nothing about agents, so a Skills detail could not show usage even if it had room.

## 2. Decisions

| Decision | Outcome | Rejected |
|---|---|---|
| Models page | **Unchanged.** `ModelLibrary` stays embedded; it is a provider-connection flow (#950–#957), not an item list. | Folding it into SourceList — a rewrite of a just-shipped feature for visual consistency only. |
| Skills usage data | **Small Rust change:** `skills_installed` items gain `agents: Vec<String>` (from each agent's `profile.installed_skills`) and `path: Option<String>` (the global skill dir). Same profile scan `mcp_installed` and `addons_installed` already do. | Frontend-only Skills detail with no "used by" — one page would stay thin and the change would come later anyway. |
| Install target | **`AgentPicker` lives in the list header** (a `toolbar` slot on `SourceList`); the list's "+" opens the page's install menu; the existing modals keep taking `agentName`. | Picker inside each modal — four modal edits, and the Plugins import needs the target before its folder dialog. Later polish. |
| Detail tabs | **One Overview tab**; `DetailPage` draws no tab bar for a single tab. | Inventing tabs the data cannot fill. |
| Install-to-agent from the detail | **Not in Phase 2(a).** `agent_skill_install` needs a `sourcePath`, `agent_mcp_add` needs command/args — the installed views carry neither. Installs go through the "+" menu. | — |

## 3. Design

### 3.1 Pages

Each of Skills, Workflows, MCP, Plugins renders the Phase 1 `master-detail` grid: `SourceList` | `ListDivider` | detail pane. The detail pane shows `LibraryDetail` for the selected item and a one-line hint otherwise (`library.selectHint`). `LibraryInspector`, `LibrarySelection`, DashboardApp's `libItem` / `onLibrarySelect` and the `isLibrary` branch of `hasInspector` are removed; after this the inspector column exists only for Chats.

| Page | Row | Subtitle | Facet chips | "+" menu | Detail meta rows | Per-agent actions |
|---|---|---|---|---|---|---|
| Skills | name | category · v{origin_version} · {status} | category | Install from URL… / Browse registry… | Category, Version, Status, Path | enable/disable (`agent_skill_toggle`), remove (`agent_skill_uninstall`) |
| MCP | name | transport · used by N | transport | Discover from other tools… / Add by URL… | Transport, Server id | enable/disable (`agent_mcp_toggle`), remove (`agent_mcp_remove`) |
| Plugins | id | N skills · N MCP · N commands | none | Import plugin folder… | Source, Skills, MCP, Commands | enable/disable (`agent_addon_toggle`), remove (`agent_addon_remove`) |
| Workflows | name | path (tail) | none | none ("+" hidden) | Path | none; header action **Open folder** (`reveal_in_finder`) |

Rows carry no status dot (Library items have no runtime); the upgrade status is text in the subtitle. Row avatar is a neutral glyph per kind (sparkle / plug / puzzle / flow, the sidebar's own icons at 28 px).

### 3.2 Component changes

- **`SourceList`** — `toolbar?: ReactNode` rendered between the header and the filter; `SourceRowData.status` becomes optional (no dot when absent); `onCreate` becomes optional and a new `createItems?: MenuItemDef[]` turns "+" into a menu (`MenuList` from `SplitButton.tsx`). "+" is hidden when neither is given.
- **`DetailPage`** — when `tabs.length <= 1` the tablist is not rendered; the body is still keyed by `activeTab`.
- **`LibraryDetail`** (`components/detail/library/LibraryDetail.tsx`) — generic over a `LibraryItem`:

  ```ts
  interface LibraryItem { id: string; kind: "skill" | "mcp" | "plugin" | "workflow"; name: string; description?: string; meta: { label: string; value: string }[]; path?: string | null }
  interface LibraryAgentUse { agent: string; enabled?: boolean }   // enabled undefined = no toggle
  props: { item; uses: LibraryAgentUse[]; busy: boolean; onToggle?(agent, enabled); onRemove?(agent); onOpenFolder?() }
  ```
  Renders `DetailPage` (avatar glyph, name, meta line = kind label, actions = Open folder when `path`) with one Overview tab: description card, meta `detail-kv` rows, and a "Used by" card listing `uses` with a checkbox (when `enabled` is defined) and a Remove button. Empty `uses` shows `library.notUsed`.

- **`libraryModel.ts`** — pure builders, one per page, tested: `skillRows`, `skillFacets`, `mcpRows`, `mcpFacets`, `pluginRows`, `workflowRows`, and `itemFor(kind, record)`.

### 3.3 Rust: `skills_installed`

```rust
pub struct InstalledSkillView { name, description, category, origin_version, status,
    pub agents: Vec<String>,      // agents whose profile.installed_skills lists this name, sorted
    pub path: Option<String> }    // the global skill dir, for "Open folder"
```
A pure `agents_by_skill(profiles: &[(String, Vec<SkillCardEntry>)]) -> BTreeMap<String, Vec<String>>` does the fold; the command reads `agents/*/profile.yaml` exactly the way `mcp_installed` does (unreadable profile → warn + skip). `list_skills` takes the map and sets `path = Some(dir)`.

### 3.4 Data flow and state

- Each page keeps its `refresh()` and its modals. Selection is page-local state; `mur.<page>.lastSelected` and `mur.<page>.listWidth` persist (`<page>` ∈ skills / workflows / mcp / plugins) with the Phase 1 one-shot restore and the write-after-restore guard. If a refresh drops the selected item, selection clears.
- The install target agent persists once for all four pages: `mur.library.installTarget`; the picker falls back to the first agent.
- Per-agent actions call the existing commands and then `refresh()`; the detail re-derives `uses` from the new list. `busy` disables the row controls while a command runs.
- Deep links into Library pages do not exist today and none are added.

### 3.5 Errors and empty states

- List load failure: error text + Retry in the list pane; detail keeps its content.
- Empty list: the page's existing empty copy, plus the "+" menu where installs exist.
- No selection: `library.selectHint`.
- Action failure: message inside the Used-by card, controls re-enabled; no toast-and-forget.

### 3.6 Testing

- Rust: `agents_by_skill` folds two agents sharing a skill and ignores agents without it; `list_skills` sets `path`.
- Vitest: `SourceList` markup with `toolbar`, with a row lacking `status` (no dot), with `createItems` (a menu trigger, no plain "+"); `DetailPage` single tab renders no tablist; `libraryModel` builders (rows, facets with counts, `itemFor` meta rows); `useInstallTarget` fallback.
- Manual acceptance per PR (light + dark, 1200 px): each page selects, restores, filters, opens its install modals, toggles/removes from an agent and refreshes.

### 3.7 PRs

- **PR 6** — Rust `skills_installed` + `SourceList` / `DetailPage` extensions + `LibraryDetail` + `libraryModel` + the Skills page.
- **PR 7** — MCP, Plugins, Workflows pages; `LibraryInspector` retired; DashboardApp / Inspector cleanup; `item-card*` CSS deleted where nothing references it.

## 4. Later

Phase 2(b): ⌘↩ opens the current detail (agent / fleet / library item) in its own Tauri window reusing `DetailPage`, reading the same commands (no second data path). Install-to-agent from a Library detail once the installed views carry a source. Picker-in-modal polish.
