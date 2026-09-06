export type PaletteKind = "page" | "action" | "agent" | "fleet";

export interface PaletteItem {
  id: string;
  kind: PaletteKind;
  label: string;
  hint?: string;
  run: () => void;
}

const KIND_ORDER: Record<PaletteKind, number> = { page: 0, action: 1, agent: 2, fleet: 3 };
export const PALETTE_LIMIT = 12;

/** Prefix match beats substring; ties break by kind order, then label. With
 *  an empty query the caller's order is kept inside each kind (pages stay in
 *  sidebar order rather than sorting alphabetically). */
export function rankPalette(items: PaletteItem[], query: string, limit = PALETTE_LIMIT): PaletteItem[] {
  const q = query.trim().toLowerCase();
  const scored = items
    .map((it, index) => {
      const l = it.label.toLowerCase();
      const score = !q ? 1 : l.startsWith(q) ? 3 : l.includes(q) ? 2 : 0;
      return { it, score, index };
    })
    .filter((x) => x.score > 0);
  scored.sort(
    (a, b) =>
      b.score - a.score ||
      KIND_ORDER[a.it.kind] - KIND_ORDER[b.it.kind] ||
      (q ? a.it.label.localeCompare(b.it.label) : a.index - b.index),
  );
  return scored.slice(0, limit).map((x) => x.it);
}
