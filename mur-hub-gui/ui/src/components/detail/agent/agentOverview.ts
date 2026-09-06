import type { ChannelSummary } from "../../../work/types";
import { isRunningChannel } from "../../home/useChannels";

export const RECENT_LIMIT = 3;

export interface AgentActivity {
  now: ChannelSummary | null;
  recent: ChannelSummary[];
}

/** What this agent is doing, from the same `channel_list` data Home reads. */
export function activityFor(channels: ChannelSummary[], agent: string, limit = RECENT_LIMIT): AgentActivity {
  const mine = channels
    .filter((c) => c.agents.includes(agent))
    .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  return { now: mine.find(isRunningChannel) ?? null, recent: mine.slice(0, limit) };
}
