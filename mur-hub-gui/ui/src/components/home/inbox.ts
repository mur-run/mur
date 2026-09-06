export type InboxKind = "hitl" | "install" | "companion" | "upgrade_blocked";

export interface InboxItem {
  kind: InboxKind;
  id: string; // unique within kind
  ts: number; // unix seconds, sort key
  title: string;
  subtitle: string;
  payload: unknown; // kind-specific, cast card
  /** Owning agent, set by the hitl and companion adapters; absent for global items. */
  agent?: string;
}

/**
 * Merge inbox items from multiple sources into a single list, sorted
 * descending by ts. Sort is stable for equal ts. Items are deduplicated by
 * `kind+id`, keeping the newest (highest ts) occurrence.
 */
export function mergeInbox(sources: InboxItem[][]): InboxItem[] {
  const newestByKey = new Map<string, InboxItem>();

  for (const source of sources) {
    for (const item of source) {
      const key = `${item.kind}:${item.id}`;
      const existing = newestByKey.get(key);
      if (!existing || item.ts > existing.ts) {
        newestByKey.set(key, item);
      }
    }
  }

  return Array.from(newestByKey.values())
    .map((item, index) => ({ item, index }))
    .sort((a, b) => b.item.ts - a.item.ts || a.index - b.index)
    .map(({ item }) => item);
}

/** Count all items in the inbox. */
export function inboxBadge(items: InboxItem[]): number {
  return items.length;
}

/**
 * Filter out session-dismissed items (keyed by `kind:id`, matching the key
 * NeedsYou cards use). This is the single source of truth for "what's
 * actually actionable" — apply it BEFORE both `inboxBadge()` and rendering
 * so the sidebar/Dock badge and the visible card list never drift apart.
 */
export function visibleInboxItems(
  items: InboxItem[],
  dismissed: ReadonlySet<string>,
): InboxItem[] {
  return items.filter((it) => !dismissed.has(`${it.kind}:${it.id}`));
}
