import type { ReactNode } from "react";
import type { StatusKind } from "./Status";

export interface SourceRowData {
  id: string;
  name: string;
  subtitle?: string;
  /** Omitted for items that have no runtime (Library rows). */
  status?: StatusKind;
  /** Amber "needs you" count; 0 or undefined hides the badge. */
  needsYou?: number;
  /** Brand-coloured dot before the name (Chats: activity while not focused). */
  unread?: boolean;
  avatar: ReactNode;
  /** Facet ids this row belongs to (a role, label ids, …). */
  facets: string[];
}

export interface SourceFacet {
  id: string;
  label: string;
  count: number;
}

export function filterRows<T extends { name: string; subtitle?: string; facets: string[] }>(
  rows: T[],
  text: string,
  facet: string | null,
): T[] {
  const q = text.trim().toLowerCase();
  return rows.filter(
    (r) =>
      (facet === null || r.facets.includes(facet)) &&
      (!q || r.name.toLowerCase().includes(q) || (r.subtitle ?? "").toLowerCase().includes(q)),
  );
}

export function moveSelection<T extends { id: string }>(rows: T[], selectedId: string | null, delta: 1 | -1): string | null {
  if (rows.length === 0) return null;
  const i = rows.findIndex((r) => r.id === selectedId);
  if (i === -1) return delta === 1 ? rows[0].id : rows[rows.length - 1].id;
  return rows[Math.min(rows.length - 1, Math.max(0, i + delta))].id;
}

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
