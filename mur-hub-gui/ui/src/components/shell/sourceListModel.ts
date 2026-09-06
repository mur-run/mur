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
