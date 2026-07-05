//! Pure list logic for the unified Chats page. Kept free of React so it can be
//! unit-tested: turns the raw agent roster + conversation attention into an
//! ordered, searchable list of chat rows.

import type { AgentEntry } from "../../types";
import type { ConversationAttention } from "../../conversation/reducer";

export interface ChatListItem {
  name: string;
  displayName: string;
  agent: AgentEntry;
  /** Unread deltas arrived while this chat was not focused. */
  unread: boolean;
  /** A HITL approval is pending on this chat. */
  hitl: boolean;
  /** Epoch ms of latest activity, if known; undefined when never active. */
  lastActivityMs?: number;
}

/**
 * Order chats for the list pane. HITL-pending rows pin to the top, then unread
 * rows, then by latest activity (desc), then alphabetically by display name so
 * the order is stable when everything else ties.
 */
export function sortConversations(items: ChatListItem[]): ChatListItem[] {
  return [...items].sort((a, b) => {
    if (a.hitl !== b.hitl) return a.hitl ? -1 : 1;
    if (a.unread !== b.unread) return a.unread ? -1 : 1;
    const at = a.lastActivityMs ?? 0;
    const bt = b.lastActivityMs ?? 0;
    if (at !== bt) return bt - at;
    return a.displayName.localeCompare(b.displayName);
  });
}

/** Group chat rows by agent name (one bucket per agent). */
export function groupByAgent(
  items: ChatListItem[],
): Record<string, ChatListItem[]> {
  const out: Record<string, ChatListItem[]> = {};
  for (const item of items) {
    (out[item.name] ??= []).push(item);
  }
  return out;
}

/**
 * Build the ordered, filtered chat list from the agent roster and the live
 * conversation attention map. `query` filters by name or display name.
 */
export function buildChatList(
  agents: AgentEntry[],
  attention: Record<string, ConversationAttention>,
  query?: string,
): ChatListItem[] {
  const q = query?.trim().toLowerCase() ?? "";
  const items = agents
    .filter(
      (a) =>
        !q ||
        a.name.toLowerCase().includes(q) ||
        a.display_name.toLowerCase().includes(q),
    )
    .map((a): ChatListItem => {
      const attn = attention[a.name];
      return {
        name: a.name,
        displayName: a.display_name,
        agent: a,
        unread: attn?.unread ?? false,
        hitl: attn?.hitl ?? false,
      };
    });
  return sortConversations(items);
}
