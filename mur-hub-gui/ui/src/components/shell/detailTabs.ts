import type { TranslationKey } from "../../i18n/types";
import { ALL_DETAIL_TABS, type DetailTab } from "../../types";

// Agent detail: 11 legacy tabs → 6 groups (spec §4.3).
export type AgentTabId = "overview" | "identity" | "capabilities" | "memory" | "automation" | "channels";
export const AGENT_TABS: AgentTabId[] = ["overview", "identity", "capabilities", "memory", "automation", "channels"];
export const AGENT_TAB_LABEL_KEY: Record<AgentTabId, TranslationKey> = {
  overview: "detail.tab.overview",
  identity: "detail.tab.identity",
  capabilities: "detail.tab.capabilities",
  memory: "detail.tab.memory",
  automation: "detail.tab.automation",
  channels: "detail.tab.channels",
};

const LEGACY_GROUP: Record<DetailTab, AgentTabId> = {
  persona: "identity",
  style: "identity",
  behavior: "identity",
  skills: "capabilities",
  mcp: "capabilities",
  plugins: "capabilities",
  permissions: "capabilities",
  memory: "memory",
  schedule: "automation",
  inbox: "channels",
  mobile: "channels",
};

/** `desiredDetailTab` still speaks the legacy id; this resolves the new tab
 *  and the in-tab anchor (`<section id="agent-<anchor>">`). */
export function detailGroupOf(legacy: string | null): { tab: AgentTabId; anchor: DetailTab | null } {
  if (legacy && (ALL_DETAIL_TABS as readonly string[]).includes(legacy)) {
    const id = legacy as DetailTab;
    return { tab: LEGACY_GROUP[id], anchor: id };
  }
  return { tab: "overview", anchor: null };
}

// Fleet detail (spec §4.4).
export type FleetTabId = "overview" | "members" | "jobs" | "settings";
export const FLEET_TABS: FleetTabId[] = ["overview", "members", "jobs", "settings"];
export const FLEET_TAB_LABEL_KEY: Record<FleetTabId, TranslationKey> = {
  overview: "fleet.tab.overview",
  members: "fleet.tab.members",
  jobs: "fleet.tab.jobs",
  settings: "fleet.tab.settings",
};
