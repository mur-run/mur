import type { InboxItem } from "./inbox";

/** Per-agent "needs you" counts for the list badge (spec §6.3). Items with no
 *  agent (blocked skill upgrades, install requests) count toward Home only. */
export function needsYouCounts(items: InboxItem[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const it of items) {
    if (!it.agent) continue;
    counts[it.agent] = (counts[it.agent] ?? 0) + 1;
  }
  return counts;
}
