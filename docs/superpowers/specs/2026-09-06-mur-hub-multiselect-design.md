# MUR Hub 2.0 — Phase 3(c): `SourceList` multi-select with bulk Start / Stop

**Date:** 2026-09-06 · **Status:** Draft — awaiting review
**Follows:** `2026-09-06-mur-hub-master-detail-shell-design.md` (§4.1 `SourceList`, §10 Phase 3), `2026-09-06-mur-hub-home-peek-design.md` (Phase 3(b), #1179–#1180).
**Scope:** `mur-hub-gui/ui` only. Agents and Fleets pages. No Rust change.

## 1. Problem

Starting or stopping several agents means clicking each one and pressing its button; a fleet of agents to bring up after a reboot, or to stop before a machine goes to sleep, is a dozen round trips. `SourceList` selects exactly one row and every page keeps a single `selectedId`; the shell spec deferred multi-select with bulk start / stop to Phase 3.

## 2. Decisions

| Question | Decision | Rejected |
|---|---|---|
| Where the bulk actions live | **The detail column becomes a `BulkPanel`** ("N selected", the list, Start k / Stop m, per-item results) once two or more rows are selected — Finder's "N items" preview. | A floating bar at the bottom of the list: 240–400px cannot hold two labelled buttons, and a single detail on the right would mislead about what the action applies to. |
| Actions | **Start / Stop only** — agents (`start_agent` / `stop_agent`) and fleets (`fleet_start` / `fleet_stop`), the commands that exist. Chats and Library pages do not opt in. | Export / Delete: asymmetric (agents have no delete command), and bulk delete is a high-risk batch. |
| Selection model | **Pages own the selection**, as today: the anchor stays where it is (Agents: `selectedAgent` in `AgentContext`; Fleets: `selectedName`), plus a local `multi: Set<string>`. `SourceList` computes the click mode and calls back; pure helpers derive the next set. Not persisted. | `SourceList` holding the set internally: the pages could not feed ⌘K / ⌘↩ / the peek, which all key off the anchor. Checkboxes per row: a touch / web-table idiom; the rows are already full. |
| Modifiers | macOS conventions: plain click = single, ⌘/Ctrl-click = toggle, ⇧-click = range from the anchor over the *visible* rows, ⇧↑ / ⇧↓ extend by one, ⌘A = all visible, Esc = collapse (first to the anchor, then to nothing). | — |
| Concurrency | The bulk action runs every command with `Promise.allSettled`; one failure never stops the rest; each row shows ✓ or ✗ + the error; a toast summarises. | Sequential: slower, and one hang blocks the tail. |

## 3. Selection helpers (`components/shell/sourceListModel.ts`, pure, tested)

```ts
export type SelectMode = "single" | "toggle" | "range";
export interface Selection { anchor: string | null; ids: ReadonlySet<string> }

/** ⌘/Ctrl → toggle, ⇧ → range, else single (a plain click never keeps a multi-selection). */
export function selectModeOf(e: { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean }): SelectMode

export function applySelection(visibleIds: readonly string[], current: Selection, clickedId: string, mode: SelectMode): Selection
//  single → { anchor: clickedId, ids: {clickedId} }
//  toggle → ids ± clickedId; anchor = clickedId if it is now in ids, else the anchor if still in ids, else any remaining id, else null
//  range  → ids = every visible id between anchor and clickedId inclusive (anchor null → single); anchor unchanged

/** ⇧↑ / ⇧↓: extend from the anchor's neighbour on the far edge of the current block, over visible rows. */
export function extendSelection(visibleIds: readonly string[], current: Selection, delta: 1 | -1): Selection
//  the block is the contiguous run of selected visible ids containing the anchor; the far edge is the end in `delta`'s direction; add its neighbour

export function selectAll(visibleIds: readonly string[], current: Selection): Selection
//  ids = all visible; anchor stays if visible, else the first visible id

export function collapseSelection(current: Selection): Selection
//  Esc step 1: ids = {anchor} (or empty when anchor is null)
```

Range and extend operate on the rows the list is showing (after filter + facet), the way Finder ranges over what is visible.

## 4. `SourceList`

- `onSelect: (id: string | null, mode: SelectMode) => void` — the row's `onClick` passes `selectModeOf(e)`; keyboard ↑/↓ and Esc pass `"single"` (Esc with `null`).
- New optional props: `selectedIds?: ReadonlySet<string>` (when given, every id in it gets `source-row--on` + `aria-selected="true"` and the listbox gets `aria-multiselectable="true"`; `selectedId` still drives `aria-activedescendant`), `onExtend?: (delta: 1 | -1) => void` (⇧↑ / ⇧↓ in the listbox), `onSelectAll?: () => void` (⌘A / Ctrl+A in the listbox; `preventDefault`).
- Rows in a multi-selection use the same `source-row--on` look; the anchor is not styled differently (the panel names it).
- Pages that do not pass `selectedIds` keep today's behaviour byte-for-byte (Chats, Library) — markup test.

## 5. Pages

**Agents** (`AgentsPage`): `const [multi, setMulti] = useState<ReadonlySet<string>>(new Set())`. `selection = { anchor: selectedAgent, ids: multi.size > 0 ? multi : (selectedAgent ? new Set([selectedAgent]) : new Set()) }`. `onSelect(id, mode)`:

```ts
const next = id === null ? { anchor: null, ids: new Set() } : applySelection(visibleIds, selection, id, mode);
// visibleIds = filterRows(rows, filter, facet).map(r => r.id) — the same rows SourceList shows
if (next.anchor !== selectedAgent && !(await confirmLeave(…))) return;   // leaving a dirty single detail
setSelected(next.anchor);
setMulti(next.ids.size > 1 ? next.ids : new Set());
if (next.ids.size <= 1) setListShown(false);                              // overlay mode: keep the list open while multi-selecting
```

Esc from the list (`onSelect(null, "single")`): when `multi.size > 1` → `setMulti(new Set())` (back to the anchor alone); otherwise clear the anchor as today. `onExtend` → `extendSelection`, `onSelectAll` → `selectAll`, both through the same `setSelected` / `setMulti` pair (no dirty prompt: the anchor does not change). A roster change removes vanished ids from `multi`.

Detail column: `multi.size > 1` → `<BulkPanel …/>`; else today's `AgentDetail` / `AgentsOverview`.

**Fleets** (`FleetView`): the same with `selectedName` as the anchor, `fleetStatusOf(summary)` as the status, and `fleet_start` / `fleet_stop`. `FleetView.onSelect` (for ⌘↩ / ⌘K) keeps reporting the anchor.

## 6. `BulkPanel` (`components/shell/BulkPanel.tsx`)

```ts
export interface BulkItem { id: string; name: string; status: StatusKind }
export interface BulkPanelProps {
  items: BulkItem[];                                 // the selection, in list order
  onStart: (ids: string[]) => Promise<BulkResult[]>; // page-specific commands
  onStop: (ids: string[]) => Promise<BulkResult[]>;
  onClear: () => void;                               // back to the anchor alone
}
export interface BulkResult { id: string; ok: boolean; error?: string }
export function bulkCounts(items: BulkItem[]): { startable: number; stoppable: number }
//  startable = status !== "running" && status !== "restarting"; stoppable = status === "running" || status === "restarting"
```

Markup: `section.bulk` → `h2.bulk__title` "N selected" · `ul.bulk__list` rows (`StatusDot` + name + a result slot: ✓ / ✗ + error text after a run) · `div.bulk__actions` with **Start k** (`btn btn--primary`, disabled when k = 0 or a run is in flight) and **Stop m** (`btn btn--secondary`, same rule) and **Clear selection** (`btn btn--ghost`). A run calls `onStart(startableIds)` / `onStop(stoppableIds)`; the page implements them with `Promise.allSettled(ids.map(id => invoke(cmd, args(id))))` mapped to `BulkResult[]`; the panel shows per-row results and toasts `bulk.summary` ("Started {ok}, failed {failed}" / "Stopped …"). Counts recompute from the live `status` after the runtime events arrive, so the buttons follow reality rather than the last click.

CSS (`styles/components/bulk.css`): the panel fills the detail column with `padding: var(--space-7) var(--space-8)`; list rows 32px with a 20px result column; the actions row sticks under the title.

## 7. Keyboard and interaction summary

| Gesture | Effect |
|---|---|
| click | single (multi collapses) |
| ⌘/Ctrl-click | toggle the row; anchor = the row (or stays if the row was removed) |
| ⇧-click | range anchor → row over visible rows |
| ⇧↑ / ⇧↓ | extend the block by one visible row |
| ⌘A in the list | all visible rows |
| ↑ / ↓ | single (collapses) |
| Esc | multi → anchor only; then anchor → none |
| ⌘↩, double-click, ⌘K Start/Stop | the anchor only (unchanged) |
| filter / facet change | the selection is untouched; rows filtered out stay selected but hidden (Finder does the same); the panel still lists them |

Overlay list mode (< 960px): a single selection hides the list as today; a multi-selection keeps it open so ⌘-click can continue; the panel is visible after "Show list" is dismissed.

## 8. Errors and edge cases

- A command fails → that row shows ✗ + the message; the others proceed; the toast counts both.
- The selection contains a row no longer in the roster (agent deleted elsewhere) → dropped from `multi` on the next roster change; the panel never shows a name it cannot find.
- Everything selected is already running → Start 0 is disabled (and vice versa).
- Dirty single detail → ⌘-click on another row prompts the discard dialog; Cancel keeps the single detail and the selection.

## 9. Testing

- `sourceListModel.test.ts`: `selectModeOf`; `applySelection` single / toggle add / toggle remove (anchor moves) / toggle to empty / range forward / range backward / range with null anchor; `extendSelection` down and up, at the edges; `selectAll`; `collapseSelection`.
- `bulkCounts` (`BulkPanel.test.ts`, pure part only — the component uses `useT`).
- `SourceList.test.tsx`: markup without `selectedIds` unchanged; with `selectedIds` two rows carry `source-row--on` and the listbox has `aria-multiselectable`.
- Browser acceptance (stubbed bridge): ⌘-click two agents → panel "2 selected"; ⇧-click a range; ⇧↓ extends; ⌘A selects all visible; filtering then ⌘A selects only the visible; Start k / Stop m call the right ids (stub records), a stubbed failure shows ✗ with text and the toast counts; Esc twice; plain click collapses; a dirty Persona edit prompts on ⌘-click; the Fleets page mirrors all of it with `fleet_start` / `fleet_stop`; Chats and Library pages ignore ⌘-click (single).

## 10. Implementation order

One PR (**PR 12**, branch `feat/hub-3c-multiselect`): (1) selection helpers + tests; (2) `SourceList` props + markup test; (3) `BulkPanel` + `bulkCounts` + CSS + strings; (4) Agents page wiring; (5) Fleets page wiring.

## 11. Later

- Bulk actions beyond Start / Stop once a delete command for agents exists.
- Multi-select on the Chats page (bulk pop-out) if anyone asks.
