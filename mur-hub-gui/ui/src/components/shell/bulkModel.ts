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
