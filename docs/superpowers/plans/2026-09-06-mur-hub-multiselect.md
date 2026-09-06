# MUR Hub 2.0 — Phase 3(c) multi-select with bulk Start / Stop — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-06-mur-hub-multiselect-design.md` (§ references below point there). One PR (**PR 12**), five tasks, each commit builds.

## Goal

On the Agents and Fleets pages, ⌘-click / ⇧-click / ⇧↑↓ / ⌘A select several rows and the detail column becomes a bulk panel that starts or stops them all, reporting each result.

## Architecture

Pure selection helpers in `sourceListModel.ts` compute the next `{ anchor, ids }` from a click mode; `SourceList` derives the mode from the event and renders every id in `selectedIds` as selected. Each page keeps its anchor where it is today plus a local `multi` set, and swaps its detail for a generic `BulkPanel` whose Start / Stop run the page's commands with `Promise.allSettled`.

## Tech stack

React 18 + TypeScript 5.5 + Vite 5, plain CSS on the two-tier tokens, Vitest 4 without jsdom, the lightweight i18n (`en.ts` defines keys, `zh-TW.ts` is typed `Table`; `t()` substitutes every `{key}` it is given). No Rust.

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (`tsc` enforces the table).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX.
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive): the pure bulk helpers live in `bulkModel.ts`, the component in `BulkPanel.tsx`.
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`BulkPanel` uses `useT`, so only `bulkModel` is unit-tested).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. No new data path: the bulk actions call the four commands the single-item buttons already call (`start_agent`, `stop_agent`, `fleet_start`, `fleet_stop`).
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green and the manual acceptance list passes.
11. Pages that do not pass `selectedIds` (Chats, Library) keep today's markup and behaviour byte-for-byte.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/`.
- Line numbers cite `main` at `11464445` (2026-09-06); re-check with `grep -n` before cutting.
- Commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`. `npm run lint` reports 6 pre-existing warnings in files this plan does not touch; 0 errors is the bar.
- Browser acceptance: `npm run dev -- --port 5174 --strictPort`, the stored-in-`sessionStorage` Tauri stub from the Phase 3(b) plan, `Try again` clicked by text. Modifier clicks are dispatched from the console as `el.dispatchEvent(new MouseEvent("click", { bubbles: true, metaKey: true }))` (and `shiftKey`), keys as `listbox.dispatchEvent(new KeyboardEvent("keydown", { key, shiftKey, metaKey, bubbles: true }))`.
- Commit after every task with the message given.

## File structure

| File | Responsibility |
|---|---|
| `src/components/shell/sourceListModel.ts` (+ `.test.ts`) (modify) | `SelectMode`, `Selection`, `selectModeOf`, `applySelection`, `extendSelection`, `selectAll`, `collapseSelection` |
| `src/components/shell/SourceList.tsx` (+ `.test.tsx`) (modify) | `onSelect(id, mode)`, `selectedIds?`, `onExtend?`, `onSelectAll?`, `aria-multiselectable` |
| `src/components/shell/bulkModel.ts` (+ `.test.ts`) (new) | `BulkItem`, `BulkResult`, `bulkCounts`, `startableIds`, `stoppableIds`, `runBulk` |
| `src/components/shell/BulkPanel.tsx` (new) | the "N selected" panel |
| `src/styles/components/bulk.css` (new) + `src/styles/index.css` (modify) | `.bulk*` |
| `src/components/agents/AgentsPage.tsx` (modify) | `multi`, mode-aware select, extend / all, `BulkPanel` with `start_agent` / `stop_agent` |
| `src/components/fleet/FleetView.tsx` (modify) | the same with `fleet_start` / `fleet_stop` |
| `src/i18n/en.ts`, `src/i18n/zh-TW.ts` (modify) | `bulk.*` |

---

### Task 12.1 — selection helpers

**Interfaces.** Produces (all in `sourceListModel.ts`): `type SelectMode = "single" | "toggle" | "range"`, `interface Selection { anchor: string | null; ids: ReadonlySet<string> }`, `EMPTY_SELECTION`, `selectModeOf(e)`, `applySelection(visibleIds, current, clickedId, mode)`, `extendSelection(visibleIds, current, delta)`, `selectAll(visibleIds, current)`, `collapseSelection(current)`. 12.2, 12.4, 12.5 consume them.

- [x] Append to `src/components/shell/sourceListModel.test.ts`:

```ts
import {
  applySelection, collapseSelection, extendSelection, selectAll, selectModeOf, type Selection,
} from "./sourceListModel";

const ids = ["a", "b", "c", "d", "e"];
const sel = (anchor: string | null, ...on: string[]): Selection => ({ anchor, ids: new Set(on) });
const on = (s: Selection) => [...s.ids].sort();

describe("selectModeOf", () => {
  it("shift → range, meta/ctrl → toggle, else single", () => {
    expect(selectModeOf({ metaKey: false, ctrlKey: false, shiftKey: true })).toBe("range");
    expect(selectModeOf({ metaKey: true, ctrlKey: false, shiftKey: false })).toBe("toggle");
    expect(selectModeOf({ metaKey: false, ctrlKey: true, shiftKey: false })).toBe("toggle");
    expect(selectModeOf({ metaKey: false, ctrlKey: false, shiftKey: false })).toBe("single");
  });
});

describe("applySelection", () => {
  it("single replaces everything and moves the anchor", () => {
    const s = applySelection(ids, sel("a", "a", "b"), "d", "single");
    expect(s.anchor).toBe("d");
    expect(on(s)).toEqual(["d"]);
  });
  it("toggle adds and moves the anchor to the added row", () => {
    const s = applySelection(ids, sel("a", "a"), "c", "toggle");
    expect(s.anchor).toBe("c");
    expect(on(s)).toEqual(["a", "c"]);
  });
  it("toggle removes; the anchor stays if still selected, else moves to a remaining row, else null", () => {
    expect(applySelection(ids, sel("a", "a", "c"), "c", "toggle")).toEqual(sel("a", "a"));
    const moved = applySelection(ids, sel("c", "a", "c"), "c", "toggle");
    expect(moved.anchor).toBe("a");
    expect(on(moved)).toEqual(["a"]);
    expect(applySelection(ids, sel("a", "a"), "a", "toggle")).toEqual(sel(null));
  });
  it("range selects the visible rows between anchor and click, either direction, anchor unchanged", () => {
    const down = applySelection(ids, sel("b", "b"), "d", "range");
    expect(down.anchor).toBe("b");
    expect(on(down)).toEqual(["b", "c", "d"]);
    const up = applySelection(ids, sel("d", "d"), "a", "range");
    expect(up.anchor).toBe("d");
    expect(on(up)).toEqual(["a", "b", "c", "d"]);
  });
  it("range without an anchor, or with one that is not visible, is a single", () => {
    expect(applySelection(ids, sel(null), "c", "range")).toEqual(sel("c", "c"));
    expect(applySelection(ids, sel("zzz", "zzz"), "c", "range")).toEqual(sel("c", "c"));
  });
});

describe("extendSelection", () => {
  it("grows the anchor's block by one visible row in the given direction", () => {
    expect(on(extendSelection(ids, sel("b", "b"), 1))).toEqual(["b", "c"]);
    expect(on(extendSelection(ids, sel("b", "b", "c"), 1))).toEqual(["b", "c", "d"]);
    expect(on(extendSelection(ids, sel("c", "b", "c"), -1))).toEqual(["a", "b", "c"]);
  });
  it("stops at the edges and without an anchor", () => {
    expect(extendSelection(ids, sel("e", "e"), 1)).toEqual(sel("e", "e"));
    expect(extendSelection(ids, sel(null), 1)).toEqual(sel(null));
  });
});

describe("selectAll / collapseSelection", () => {
  it("selectAll takes every visible row and keeps a visible anchor", () => {
    const s = selectAll(["b", "c"], sel("c", "c"));
    expect(s.anchor).toBe("c");
    expect(on(s)).toEqual(["b", "c"]);
    expect(selectAll(["b", "c"], sel("a", "a")).anchor).toBe("b");
    expect(selectAll([], sel("a", "a"))).toEqual(sel("a", "a"));
  });
  it("collapseSelection keeps only the anchor", () => {
    expect(collapseSelection(sel("b", "a", "b", "c"))).toEqual(sel("b", "b"));
    expect(collapseSelection(sel(null, "a"))).toEqual(sel(null));
  });
});
```
- [x] `npm test -- src/components/shell/sourceListModel.test.ts` → fails (missing exports).
- [x] Append to `src/components/shell/sourceListModel.ts`:

```ts
// ── Multi-select (spec 3(c) §3) ──────────────────────────────────────────

export type SelectMode = "single" | "toggle" | "range";

export interface Selection {
  /** The row the detail follows and ranges start from. */
  anchor: string | null;
  ids: ReadonlySet<string>;
}

export const EMPTY_SELECTION: Selection = { anchor: null, ids: new Set() };

/** ⇧ → range, ⌘/Ctrl → toggle, else single (a plain click never keeps a multi-selection). */
export function selectModeOf(e: { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean }): SelectMode {
  if (e.shiftKey) return "range";
  if (e.metaKey || e.ctrlKey) return "toggle";
  return "single";
}

function single(id: string): Selection {
  return { anchor: id, ids: new Set([id]) };
}

/** Range and toggle work over the rows the list is showing (after filter + facet). */
export function applySelection(
  visibleIds: readonly string[],
  current: Selection,
  clickedId: string,
  mode: SelectMode,
): Selection {
  if (mode === "single") return single(clickedId);
  if (mode === "toggle") {
    const ids = new Set(current.ids);
    if (ids.has(clickedId)) {
      ids.delete(clickedId);
      const anchor = current.anchor !== null && ids.has(current.anchor) ? current.anchor : (ids.values().next().value ?? null);
      return { anchor, ids };
    }
    ids.add(clickedId);
    return { anchor: clickedId, ids };
  }
  const a = current.anchor === null ? -1 : visibleIds.indexOf(current.anchor);
  const b = visibleIds.indexOf(clickedId);
  if (a === -1 || b === -1) return single(clickedId);
  const [lo, hi] = a < b ? [a, b] : [b, a];
  return { anchor: current.anchor, ids: new Set(visibleIds.slice(lo, hi + 1)) };
}

/** ⇧↑ / ⇧↓: add the visible row past the far edge of the anchor's selected block. */
export function extendSelection(visibleIds: readonly string[], current: Selection, delta: 1 | -1): Selection {
  if (current.anchor === null) return current;
  const a = visibleIds.indexOf(current.anchor);
  if (a === -1) return current;
  let edge = a;
  while (edge + delta >= 0 && edge + delta < visibleIds.length && current.ids.has(visibleIds[edge + delta])) edge += delta;
  const next = edge + delta;
  if (next < 0 || next >= visibleIds.length) return current;
  const ids = new Set(current.ids);
  ids.add(visibleIds[next]);
  return { anchor: current.anchor, ids };
}

/** ⌘A: every visible row; the anchor stays if visible, else the first visible row. */
export function selectAll(visibleIds: readonly string[], current: Selection): Selection {
  if (visibleIds.length === 0) return current;
  const anchor = current.anchor !== null && visibleIds.includes(current.anchor) ? current.anchor : visibleIds[0];
  return { anchor, ids: new Set(visibleIds) };
}

/** Esc, first step: back to the anchor alone. */
export function collapseSelection(current: Selection): Selection {
  return { anchor: current.anchor, ids: current.anchor === null ? new Set() : new Set([current.anchor]) };
}
```
- [x] `npm test -- src/components/shell/sourceListModel.test.ts` → all pass (the existing `filterRows` / `moveSelection` tests plus the nine new ones). `npm run build`, `npm run lint`.
- [x] Commit: `feat(hub): SourceList selection helpers — mode, toggle, range, extend, all, collapse`

### Task 12.2 — `SourceList` multi-select props

**Interfaces.** Consumes `SelectMode`, `selectModeOf`. Produces `SourceListProps.onSelect: (id: string | null, mode: SelectMode) => void`, `selectedIds?: ReadonlySet<string>`, `onExtend?: (delta: 1 | -1) => void`, `onSelectAll?: () => void`. Existing callers that pass `(id) => …` still type-check (fewer parameters are assignable).

- [x] `src/components/shell/SourceList.tsx`:
  - Import: change `import { filterRows, moveSelection, type SourceFacet, type SourceRowData } from "./sourceListModel";` to `import { filterRows, moveSelection, selectModeOf, type SelectMode, type SourceFacet, type SourceRowData } from "./sourceListModel";`.
  - Props: change `onSelect: (id: string | null) => void;` to
    ```ts
    /** `mode` comes from the click's modifiers (spec 3(c) §4); keyboard ↑/↓ and Esc pass "single". */
    onSelect: (id: string | null, mode: SelectMode) => void;
    /** When given, every id in it renders selected (multi-select pages). */
    selectedIds?: ReadonlySet<string>;
    /** ⇧↑ / ⇧↓ in the listbox. */
    onExtend?: (delta: 1 | -1) => void;
    /** ⌘A / Ctrl+A in the listbox. */
    onSelectAll?: () => void;
    ```
  - `onListKey` becomes:
    ```tsx
    function onListKey(e: KeyboardEvent<HTMLDivElement>) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const delta = e.key === "ArrowDown" ? 1 : -1;
        if (e.shiftKey && p.onExtend) p.onExtend(delta);
        else p.onSelect(moveSelection(visible, p.selectedId, delta), "single");
      } else if (e.key === "Escape") {
        p.onSelect(null, "single");
      } else if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "a" && p.onSelectAll) {
        e.preventDefault();
        p.onSelectAll();
      }
    }
    ```
  - Listbox: add `aria-multiselectable={p.selectedIds ? true : undefined}` after `role="listbox"`.
  - Row: before the `visible.map`, nothing; inside the map replace the three selection-dependent attributes:
    ```tsx
            : visible.map((r) => {
                const isOn = p.selectedIds ? p.selectedIds.has(r.id) : r.id === p.selectedId;
                return (
                  <div
                    key={r.id}
                    id={`row-${r.id}`}
                    role="option"
                    aria-selected={isOn}
                    className={`source-row${isOn ? " source-row--on" : ""}`}
                    onClick={(e) => p.onSelect(r.id, selectModeOf(e))}
                    onDoubleClick={p.onOpen ? () => p.onOpen?.(r.id) : undefined}
                  >
                    <span className="source-row__avatar">{r.avatar}</span>
                    <span className="source-row__text">
                      <span className="source-row__name">{r.name}</span>
                      {r.subtitle && <span className="source-row__sub">{r.subtitle}</span>}
                    </span>
                    <span className="source-row__status">
                      {r.unread && <span className="source-row__unread" role="img" aria-label={p.unreadLabel} />}
                      <NeedsYouBadge count={r.needsYou ?? 0} />
                      {r.status && <StatusDot kind={r.status} />}
                    </span>
                  </div>
                );
              })}
    ```
    (The three inner spans are today's markup verbatim; only the wrapper and the map's braces change.)
- [x] `src/components/shell/SourceList.test.tsx`: add
  ```tsx
  it("renders every id in selectedIds as selected and marks the listbox multiselectable", () => {
    const props = { title: "Agents", count: 2, rows, facets: [], allLabel: "All", activeFacet: null, onFacet: noop,
      filter: "", onFilter: noop, filterPlaceholder: "Filter", selectedId: "aura", onSelect: noop, emptyState: <p>none</p> };
    const html = renderToStaticMarkup(<SourceList {...props} selectedIds={new Set(["aura", "scout"])} />);
    expect(html.match(/source-row--on/g)).toHaveLength(2);
    expect(html).toContain('aria-multiselectable="true"');
    const single = renderToStaticMarkup(<SourceList {...props} />);
    expect(single.match(/source-row--on/g)).toHaveLength(1);
    expect(single).not.toContain("aria-multiselectable");
  });
  ```
- [x] `npm test` (all pass), `npm run build`, `npm run lint`. Chats / Library still compile: their `onSelect={(id) => …}` ignore the second argument.
- [x] Commit: `feat(hub): SourceList — click modes, selectedIds, ⇧↑↓ extend, ⌘A`

### Task 12.3 — `bulkModel`, `BulkPanel`, CSS, strings

**Interfaces.** Consumes `StatusKind`, `StatusDot`, `showToast`. Produces `bulkModel.ts`: `BulkItem { id; name; status: StatusKind }`, `BulkResult { id; ok; error? }`, `bulkCounts(items)`, `startableIds(items)`, `stoppableIds(items)`, `runBulk(ids, call)`; `BulkPanel({ items, onStart, onStop, onClear })`; keys `bulk.selected`, `bulk.start`, `bulk.stop`, `bulk.clear`, `bulk.startedSummary`, `bulk.stoppedSummary`.

- [x] Create `src/components/shell/bulkModel.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { bulkCounts, runBulk, startableIds, stoppableIds, type BulkItem } from "./bulkModel";

const items: BulkItem[] = [
  { id: "a", name: "A", status: "running" },
  { id: "b", name: "B", status: "idle" },
  { id: "c", name: "C", status: "restarting" },
  { id: "d", name: "D", status: "stopped" },
  { id: "e", name: "E", status: "failed" },
];

describe("bulkCounts / startableIds / stoppableIds", () => {
  it("running and restarting are stoppable; everything else is startable", () => {
    expect(bulkCounts(items)).toEqual({ startable: 3, stoppable: 2 });
    expect(startableIds(items)).toEqual(["b", "d", "e"]);
    expect(stoppableIds(items)).toEqual(["a", "c"]);
  });
});

describe("runBulk", () => {
  it("runs every call, keeps order, and turns a rejection into a failed result", async () => {
    const out = await runBulk(["x", "y", "z"], async (id) => {
      if (id === "y") throw new Error("boom");
    });
    expect(out).toEqual([
      { id: "x", ok: true },
      { id: "y", ok: false, error: "Error: boom" },
      { id: "z", ok: true },
    ]);
  });
});
```
- [x] `npm test -- src/components/shell/bulkModel.test.ts` → fails (module missing).
- [x] Create `src/components/shell/bulkModel.ts`:

```ts
import type { StatusKind } from "./Status";

export interface BulkItem {
  id: string;
  name: string;
  status: StatusKind;
}

export interface BulkResult {
  id: string;
  ok: boolean;
  error?: string;
}

/** Running and restarting can be stopped; everything else can be started (spec 3(c) §6). */
const ACTIVE: ReadonlySet<StatusKind> = new Set(["running", "restarting"]);

export function startableIds(items: BulkItem[]): string[] {
  return items.filter((i) => !ACTIVE.has(i.status)).map((i) => i.id);
}

export function stoppableIds(items: BulkItem[]): string[] {
  return items.filter((i) => ACTIVE.has(i.status)).map((i) => i.id);
}

export function bulkCounts(items: BulkItem[]): { startable: number; stoppable: number } {
  return { startable: startableIds(items).length, stoppable: stoppableIds(items).length };
}

/** Every call runs; one rejection never stops the rest. Results keep `ids` order. */
export async function runBulk(ids: string[], call: (id: string) => Promise<unknown>): Promise<BulkResult[]> {
  const settled = await Promise.allSettled(ids.map((id) => call(id)));
  return settled.map((s, i) =>
    s.status === "fulfilled" ? { id: ids[i], ok: true } : { id: ids[i], ok: false, error: String(s.reason) },
  );
}
```
- [x] `npm test -- src/components/shell/bulkModel.test.ts` → 2 passed.
- [x] i18n. `en.ts` after `"peek.viewConversation"`:
  ```ts
  "bulk.selected": "{count} selected",
  "bulk.start": "Start {count}",
  "bulk.stop": "Stop {count}",
  "bulk.clear": "Clear selection",
  "bulk.startedSummary": "Started {ok}, failed {failed}",
  "bulk.stoppedSummary": "Stopped {ok}, failed {failed}",
  ```
  `zh-TW.ts` after `"peek.viewConversation"`:
  ```ts
  "bulk.selected": "已選取 {count} 個",
  "bulk.start": "啟動 {count}",
  "bulk.stop": "停止 {count}",
  "bulk.clear": "清除選取",
  "bulk.startedSummary": "已啟動 {ok}，失敗 {failed}",
  "bulk.stoppedSummary": "已停止 {ok}，失敗 {failed}",
  ```
- [x] Create `src/components/shell/BulkPanel.tsx`:

```tsx
import { useState } from "react";
import { useT } from "../../i18n";
import { showToast } from "../detail/fleet/fleetActions";
import { StatusDot } from "./Status";
import { bulkCounts, startableIds, stoppableIds, type BulkItem, type BulkResult } from "./bulkModel";

export interface BulkPanelProps {
  /** The selection, in list order. */
  items: BulkItem[];
  /** Page-specific commands over the given ids; resolve with one result per id. */
  onStart: (ids: string[]) => Promise<BulkResult[]>;
  onStop: (ids: string[]) => Promise<BulkResult[]>;
  /** Back to the anchor alone. */
  onClear: () => void;
}

type BulkKind = "start" | "stop";

/** The detail column while two or more rows are selected (spec 3(c) §6). */
export function BulkPanel({ items, onStart, onStop, onClear }: BulkPanelProps) {
  const { t } = useT();
  const [busy, setBusy] = useState<BulkKind | null>(null);
  const [results, setResults] = useState<ReadonlyMap<string, BulkResult>>(new Map());
  const counts = bulkCounts(items);

  async function run(kind: BulkKind) {
    const ids = kind === "start" ? startableIds(items) : stoppableIds(items);
    setBusy(kind);
    setResults(new Map());
    try {
      const out = await (kind === "start" ? onStart(ids) : onStop(ids));
      setResults(new Map(out.map((r) => [r.id, r])));
      const ok = out.filter((r) => r.ok).length;
      showToast(t(kind === "start" ? "bulk.startedSummary" : "bulk.stoppedSummary", { ok, failed: out.length - ok }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="bulk">
      <h2 className="bulk__title">{t("bulk.selected", { count: items.length })}</h2>
      <div className="bulk__actions">
        <button type="button" className="btn btn--primary" disabled={busy !== null || counts.startable === 0} onClick={() => void run("start")}>
          {t("bulk.start", { count: counts.startable })}
        </button>
        <button type="button" className="btn btn--secondary" disabled={busy !== null || counts.stoppable === 0} onClick={() => void run("stop")}>
          {t("bulk.stop", { count: counts.stoppable })}
        </button>
        <button type="button" className="btn btn--link" onClick={onClear}>
          {t("bulk.clear")}
        </button>
      </div>
      <ul className="bulk__list">
        {items.map((it) => {
          const r = results.get(it.id);
          return (
            <li key={it.id} className="bulk__row">
              <StatusDot kind={it.status} />
              <span className="bulk__name">{it.name}</span>
              {r && (
                <span className={`bulk__result${r.ok ? "" : " bulk__result--failed"}`} aria-label={r.ok ? "ok" : r.error}>
                  {r.ok ? "✓" : "✗"}
                </span>
              )}
              {r && !r.ok && r.error && <span className="bulk__error">{r.error}</span>}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
```
- [x] Create `src/styles/components/bulk.css` and add `@import "./components/bulk.css";` after the `peek.css` line in `src/styles/index.css`:

```css
/* Bulk panel (Phase 3(c) §6): the detail column while several rows are selected. */
.bulk { padding: var(--space-7) var(--space-8); display: flex; flex-direction: column; gap: var(--space-6); min-height: 0; overflow: auto; }
.bulk__title { margin: 0; font-size: var(--text-xl); font-weight: var(--fw-semi); letter-spacing: -.015em; }
.bulk__actions { display: flex; align-items: center; gap: var(--space-4); }
.bulk__list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
.bulk__row { display: flex; align-items: center; gap: var(--space-4); height: 32px; font-size: var(--text-sm); border-bottom: 1px solid var(--border-line); }
.bulk__name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bulk__result { width: 20px; text-align: center; color: var(--status-running); }
.bulk__result--failed { color: var(--status-failed); }
.bulk__error { color: var(--text-secondary); font-size: var(--text-xs); max-width: 40%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
```
  (`--status-running` and `--status-failed` are the Phase 1 status tokens `StatusDot` uses; `grep -n 'status-running\|status-failed' src/styles/tokens/semantic.css` confirms.)
- [x] `npm test`, `npm run build`, `npm run lint`.
- [x] Commit: `feat(hub): BulkPanel — N selected, Start k / Stop m with per-row results`

### Task 12.4 — Agents page

**Interfaces.** Consumes 12.1–12.3. Produces the Agents page behaviour of spec §5 / §7.

- [x] `src/components/agents/AgentsPage.tsx`:
  - Imports: add `import { invoke } from "@tauri-apps/api/core";`, change the model import to `import { applySelection, collapseSelection, extendSelection, filterRows, selectAll, type SelectMode, type Selection, type SourceFacet, type SourceRowData } from "../shell/sourceListModel";`, add `import { BulkPanel } from "../shell/BulkPanel";` and `import { runBulk, type BulkItem } from "../shell/bulkModel";`.
  - State, after `const [facet, setFacet] = …`:
    ```tsx
    // Multi-select (spec 3(c) §5): empty, or two or more ids including the anchor.
    const [multi, setMulti] = useState<ReadonlySet<string>>(new Set());
    ```
  - After the `rows` memo add:
    ```tsx
    const visibleIds = useMemo(() => filterRows(rows, filter, facet).map((r) => r.id), [rows, filter, facet]);
    const selection: Selection = {
      anchor: selectedAgent,
      ids: multi.size > 0 ? multi : new Set(selectedAgent ? [selectedAgent] : []),
    };

    // A roster change drops vanished ids; a multi that shrinks below two collapses.
    useEffect(() => {
      if (multi.size === 0) return;
      const kept = new Set([...multi].filter((id) => agents.some((a) => a.name === id)));
      if (kept.size !== multi.size) setMulti(kept.size > 1 ? kept : new Set());
    }, [agents, multi]);

    function commit(next: Selection) {
      setSelected(next.anchor);
      setMulti(next.ids.size > 1 ? next.ids : new Set());
      // Overlay list mode: keep the list open while multi-selecting.
      if (next.ids.size <= 1) setListShown(false);
    }
    ```
  - Replace `select` with:
    ```tsx
    async function select(name: string | null, mode: SelectMode) {
      if (name === null) {
        // Esc: multi → the anchor alone; then the anchor → nothing.
        if (multi.size > 1) {
          setMulti(new Set());
          return;
        }
        if (selectedAgent !== null) commit({ anchor: null, ids: new Set() });
        return;
      }
      const next = applySelection(visibleIds, selection, name, mode);
      if (next.anchor === selectedAgent && next.ids.size === selection.ids.size && [...next.ids].every((id) => selection.ids.has(id))) return;
      // Leaving a single detail with unsaved edits asks first (the anchor changes or the detail is replaced by the panel).
      if (multi.size <= 1 && (next.anchor !== selectedAgent || next.ids.size > 1)
        && !(await confirmLeave(t("detail.discardBody"), t("detail.discardTitle")))) return;
      commit(next);
    }
    ```
  - `<SourceList …>`: change `onSelect={(id) => { void select(id); }}` to `onSelect={(id, mode) => { void select(id, mode); }}`, and add:
    ```tsx
        selectedIds={multi.size > 1 ? multi : undefined}
        onExtend={(delta) => commit(extendSelection(visibleIds, selection, delta))}
        onSelectAll={() => commit(selectAll(visibleIds, selection))}
    ```
  - Detail column: replace `{selectedAgent && entry ? (` with
    ```tsx
        {multi.size > 1 ? (
          <BulkPanel
            items={rows.filter((r) => multi.has(r.id)).map((r): BulkItem => ({ id: r.id, name: r.name, status: r.status ?? "idle" }))}
            onStart={(ids) => runBulk(ids, (name) => invoke("start_agent", { name }))}
            onStop={(ids) => runBulk(ids, (name) => invoke("stop_agent", { name }))}
            onClear={() => commit(collapseSelection(selection))}
          />
        ) : selectedAgent && entry ? (
    ```
    (the rest of the ternary is unchanged.)
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [x] Browser acceptance (stub: three agents `aura` running, `scout` idle, `muse` idle; `start_agent` resolves for `scout` and rejects for `muse` with `"runtime busy"`; `stop_agent` resolves; `list_runtime_statuses` accordingly): on Agents, click `aura`, ⌘-click `scout` → the panel says "2 selected", Start 1 / Stop 1; ⇧-click `muse` from anchor `aura` → 3 selected; ⇧↓ / ⇧↑ on the listbox grow / (no shrink) the block; Esc → back to `aura` alone (detail), Esc → none; ⌘A → 3 selected; type `sc` in the filter then ⌘A → only `scout` (a single, so the detail shows); clear the filter, ⌘-click all three, **Start 2** → `start_agent` called for `scout` and `muse`, `scout` ✓, `muse` ✗ "runtime busy", toast "Started 1, failed 1"; **Stop 1** → `stop_agent` for `aura`; a plain click on `scout` collapses to it; with `aura` selected and a Persona edit made dirty (type in the Identity tab's persona field), ⌘-click `scout` → the discard prompt (the stub's `plugin:dialog|message` answers Ok, so it proceeds; `window.__calls` shows the dialog call); Chats page: ⌘-click a row selects only it.
- [x] Commit: `feat(hub): Agents page multi-select with bulk Start / Stop`

### Task 12.5 — Fleets page

**Interfaces.** Consumes 12.1–12.3. Produces the Fleets page behaviour of spec §5 / §7 with `fleet_start` / `fleet_stop`.

- [x] `src/components/fleet/FleetView.tsx`:
  - Imports: extend the model import to `import { applySelection, collapseSelection, extendSelection, filterRows, selectAll, type SelectMode, type Selection, type SourceFacet, type SourceRowData } from "../shell/sourceListModel";`, add `import { BulkPanel } from "../shell/BulkPanel";` and `import { runBulk, type BulkItem } from "../shell/bulkModel";` (`invoke` is already imported).
  - State, after `const [filter, setFilter] = useState("");`: `const [multi, setMulti] = useState<ReadonlySet<string>>(new Set());`.
  - After the `facets` array add:
    ```tsx
    const visibleIds = filterRows(rows, filter, activeLabel).map((r) => r.id);
    const selection: Selection = {
      anchor: selectedName,
      ids: multi.size > 0 ? multi : new Set(selectedName ? [selectedName] : []),
    };
    function commit(next: Selection) {
      setSelectedName(next.anchor);
      setMulti(next.ids.size > 1 ? next.ids : new Set());
      if (next.ids.size <= 1) setListShown(false);
    }
    function select(id: string | null, mode: SelectMode) {
      if (id === null) {
        if (multi.size > 1) setMulti(new Set());
        else commit({ anchor: null, ids: new Set() });
        return;
      }
      commit(applySelection(visibleIds, selection, id, mode));
    }
    ```
    and a prune effect next to the other effects:
    ```tsx
    useEffect(() => {
      if (multi.size === 0) return;
      const kept = new Set([...multi].filter((id) => fleets.some((f) => f.name === id)));
      if (kept.size !== multi.size) setMulti(kept.size > 1 ? kept : new Set());
    }, [fleets, multi]);
    ```
    (`rows` is declared before `facets`; move the `visibleIds` / `selection` / `commit` / `select` block after `facets` so `rows` is in scope. Fleets have no dirty guard, so no prompt.)
  - `<SourceList …>`: replace `onSelect={(id) => { setSelectedName(id); setListShown(false); }}` with `onSelect={select}` and add `selectedIds={multi.size > 1 ? multi : undefined}`, `onExtend={(delta) => commit(extendSelection(visibleIds, selection, delta))}`, `onSelectAll={() => commit(selectAll(visibleIds, selection))}`.
  - Detail column: before `{selectedName && summary ? (` insert the bulk branch:
    ```tsx
        {multi.size > 1 ? (
          <BulkPanel
            items={rows.filter((r) => multi.has(r.id)).map((r): BulkItem => ({ id: r.id, name: r.name, status: r.status ?? "idle" }))}
            onStart={async (ids) => { const out = await runBulk(ids, (name) => invoke("fleet_start", { name })); void loadList(); return out; }}
            onStop={async (ids) => { const out = await runBulk(ids, (name) => invoke("fleet_stop", { name })); void loadList(); return out; }}
            onClear={() => commit(collapseSelection(selection))}
          />
        ) : selectedName && summary ? (
    ```
    (`fleet_list` carries the `stopped` / `running` flags the rows' status comes from, so the list reload refreshes the counts.)
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [x] Browser acceptance (stub: three fleets, one `stopped: true`; `fleet_start` / `fleet_stop` resolve and flip the stub's flags): ⌘-click two fleets → "2 selected" with the right counts; Start / Stop call `fleet_start` / `fleet_stop` with the right names and the counts update after the list reloads; ⇧-click, ⇧↓, ⌘A, Esc twice, plain click collapse — as on Agents; the palette's ⌘↩ still opens the anchor's window.
- [x] Commit: `feat(hub): Fleets page multi-select with bulk Start / Stop`

**Done (2026-09-06), notes:**
- All five tasks as written. Browser acceptance ran on both pages with synthetic modifier events.
- Known theoretical race, left as is: `AgentsPage.select` awaits the dirty guard, so two ⌘-clicks dispatched in the same tick compute from the same stale `selection` and the second wins — a hand cannot click within one render; a `selectionRef` would close it if it ever shows up.
- Acceptance-script note: the listbox must be focused before dispatching Esc (as it is under a real keypress); with focus on `body`, `DashboardApp`'s global Esc handler also fires and clears the anchor.
- The plan's Agents acceptance text assumed the anchor stays on `aura` after a ⌘-click; per spec §3 the toggle moves the anchor to the added row, so ⇧-click ranges from `scout`. The spec is right; the acceptance prose was loose.

**Manual acceptance PR 12 (real build):** ⌘-click and ⇧-click with real modifier keys on macOS (the browser checks dispatch synthetic events); ⇧↑ / ⇧↓ from a focused list; ⌘A does not select the page text; a real bulk start of three idle agents shows three ✓ and the runtime dots turn green as `runtime-status-changed` events arrive.

## Spec coverage

| Spec § | Task |
|---|---|
| 3 helpers | 12.1 |
| 4 `SourceList` | 12.2 |
| 5 pages | 12.4 (Agents), 12.5 (Fleets) |
| 6 `BulkPanel`, counts, CSS | 12.3 |
| 7 keyboard / interaction | 12.2 (keys), 12.4 / 12.5 (Esc steps, overlay rule) |
| 8 errors / edge cases | 12.3 (`runBulk`), 12.4 / 12.5 (prune effect, dirty prompt) |
| 9 tests | 12.1, 12.2, 12.3; browser lists in 12.4 / 12.5 |
