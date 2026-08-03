// Pure filter + group logic for the fleet rail's label taxonomy.
//
// Kept free of React so it can be unit-tested directly (this project's vitest
// setup has no jsdom — never test through the DOM here).
import type { FleetSummary, LabelView } from "./types";

/** Synthetic group id for fleets with no primary label. Always listed last. */
export const UNGROUPED = "__ungrouped__";

export interface FleetGroup {
  id: string;
  title: string;
  color: string | null;
  fleets: FleetSummary[];
}

/**
 * Keep fleets carrying ANY of the selected labels (OR). An empty selection
 * means "All". A fleet matches on any position, not only its primary, and is
 * returned at most once.
 */
export function filterByLabels(
  fleets: FleetSummary[],
  selected: string[],
): FleetSummary[] {
  if (selected.length === 0) return fleets;
  const wanted = new Set(selected);
  return fleets.filter((f) => f.labels.some((id) => wanted.has(id)));
}

/**
 * Bucket fleets under their primary label — `labels[0]` — so each fleet
 * appears in exactly one group. Groups follow registry order; fleets whose
 * primary is missing or unknown fall into Ungrouped, which sorts last. Empty
 * groups are omitted.
 */
export function groupFleets(
  fleets: FleetSummary[],
  labels: LabelView[],
): FleetGroup[] {
  const known = new Map(labels.map((l) => [l.id, l]));
  const buckets = new Map<string, FleetSummary[]>();

  for (const f of fleets) {
    const primary = f.labels[0];
    const id = primary && known.has(primary) ? primary : UNGROUPED;
    const bucket = buckets.get(id);
    if (bucket) bucket.push(f);
    else buckets.set(id, [f]);
  }

  const groups: FleetGroup[] = [];
  for (const l of labels) {
    const rows = buckets.get(l.id);
    if (!rows || rows.length === 0) continue;
    groups.push({
      id: l.id,
      title: l.display || l.id,
      color: l.color ?? null,
      fleets: rows,
    });
  }

  const loose = buckets.get(UNGROUPED);
  if (loose && loose.length > 0) {
    groups.push({ id: UNGROUPED, title: "", color: null, fleets: loose });
  }
  return groups;
}

/**
 * Toggle one label on a fleet's ordered assignment list. Adding appends, so the
 * primary (index 0) never changes by accident; removing the primary promotes
 * whatever was next.
 */
export function toggleAssignment(ids: string[], id: string): string[] {
  return ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id];
}

/**
 * Promote a label to primary by moving it to index 0, keeping the relative
 * order of the rest. A label not yet assigned is added as the new primary.
 */
export function makePrimary(ids: string[], id: string): string[] {
  return [id, ...ids.filter((x) => x !== id)];
}
