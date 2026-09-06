//! Pure list logic for the unified Chats page. Kept free of React so it can be
//! unit-tested: turns the raw agent roster + conversation attention into an
//! ordered, searchable list of chat rows.

import type { ReactNode } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ConversationAttention } from "../../conversation/reducer";
import type { ChannelSummary } from "../../work/types";
import type { SourceFacet, SourceRowData } from "../shell/sourceListModel";
import { statusOf } from "../shell/Status";
import { relativeTime } from "../../work/format";

export interface ChatListItem {
  name: string;
  displayName: string;
  agent: AgentEntry;
  /** Unread deltas arrived while this chat was not focused. */
  unread: boolean;
  /** A HITL approval is pending on this chat. */
  hitl: boolean;
  /** Epoch ms of the primary channel's updated_at; undefined without a channel. */
  lastActivityMs?: number;
  /** The primary channel's updated_at (ISO), for relativeTime. */
  updatedAt?: string;
  /** The primary channel's preview line; undefined without a channel or when empty. */
  preview?: string;
  /** The primary channel's id and turn count, for the header meta. */
  channelId?: string;
  turns?: number;
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
 * Build the ordered, filtered chat list from the agent roster, the live
 * conversation attention map, and the channel summaries. An agent's primary
 * channel has the agent's name as its id (the rule ChatChannelRail uses);
 * fleet and other channels are ignored here. `query` filters by name or
 * display name.
 */
export function buildChatList(
  agents: AgentEntry[],
  attention: Record<string, ConversationAttention>,
  channels: ChannelSummary[],
  query?: string,
): ChatListItem[] {
  const q = query?.trim().toLowerCase() ?? "";
  const byId = new Map(channels.map((c) => [c.id, c]));
  const items = agents
    .filter((a) => !q || a.name.toLowerCase().includes(q) || a.display_name.toLowerCase().includes(q))
    .map((a): ChatListItem => {
      const attn = attention[a.name];
      const ch = byId.get(a.name);
      const updated = ch ? Date.parse(ch.updated_at) : Number.NaN;
      return {
        name: a.name,
        displayName: a.display_name,
        agent: a,
        unread: attn?.unread ?? false,
        hitl: attn?.hitl ?? false,
        lastActivityMs: Number.isFinite(updated) ? updated : undefined,
        updatedAt: ch?.updated_at,
        preview: ch?.preview || undefined,
        channelId: ch?.id,
        turns: ch?.turns,
      };
    });
  return sortConversations(items);
}

export const FACET_NEEDS_YOU = "needsYou";
export const FACET_UNREAD = "unread";

const SUBTITLE_SEP = " · ";

/** SourceList rows for the Chats page (spec 3(a) §4). `avatar` is injected so
 *  this module stays free of JSX. */
export function chatRows(
  items: ChatListItem[],
  runtimeMap: Map<string, AgentRuntimeStatus>,
  nowMs: number,
  labels: { noChannel: string },
  avatar: (item: ChatListItem) => ReactNode,
): SourceRowData[] {
  return items.map((i) => ({
    id: i.name,
    name: i.displayName,
    subtitle: i.preview && i.updatedAt ? `${i.preview}${SUBTITLE_SEP}${relativeTime(i.updatedAt, nowMs)}` : labels.noChannel,
    status: statusOf(runtimeMap.get(i.name)?.state),
    needsYou: i.hitl ? 1 : 0,
    unread: i.unread,
    avatar: avatar(i),
    facets: [...(i.hitl ? [FACET_NEEDS_YOU] : []), ...(i.unread ? [FACET_UNREAD] : [])],
  }));
}

/** Chips: Needs you / Unread, each only while it has members. */
export function chatFacets(items: ChatListItem[], labels: { needsYou: string; unread: string }): SourceFacet[] {
  const needsYou = items.filter((i) => i.hitl).length;
  const unread = items.filter((i) => i.unread).length;
  return [
    ...(needsYou > 0 ? [{ id: FACET_NEEDS_YOU, label: labels.needsYou, count: needsYou }] : []),
    ...(unread > 0 ? [{ id: FACET_UNREAD, label: labels.unread, count: unread }] : []),
  ];
}
