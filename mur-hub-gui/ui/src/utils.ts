export const CATEGORY_COLORS: Record<string, string> = {
  research: "#4A90D9",
  automation: "#10B981",
  monitor: "#F59E0B",
  notify: "#EF4444",
  commerce: "#8B5CF6",
  custom: "#64748B",
};

export const CATEGORY_ICONS: Record<string, string> = {
  research: "🔍",
  automation: "⚡",
  monitor: "📊",
  notify: "🔔",
  commerce: "🛒",
  custom: "⚙️",
};

export const TAB_ICONS: Record<string, string> = {
  persona: "🎭",
  style: "🎨",
  behavior: "🦜",
  skills: "⚡",
  mcp: "🔌",
  permissions: "🔐",
  inbox: "📬",
  memory: "🧠",
};

import type { RuntimeState } from "./types";

/**
 * Maps a supervisor runtime state to a status pill. Shared by the agent
 * list/cards AND the detail panel so they never disagree (was a bug: the
 * detail panel derived status from the lock-based AgentEntry.status instead).
 */
export function runtimePill(rt: RuntimeState | undefined): {
  cls: string;
  key: "status.running" | "status.idle" | "status.error";
} {
  switch (rt?.state) {
    case "running":
    case "restarting":
      return { cls: "pill pill--run", key: "status.running" };
    case "failed":
      return { cls: "pill pill--fail", key: "status.error" };
    default:
      return { cls: "pill pill--idle", key: "status.idle" };
  }
}

export function avatarInitials(displayName: string): string {
  return displayName
    .split(" ")
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");
}

export function timeGreetingKey():
  | "dashboard.greeting.morning"
  | "dashboard.greeting.afternoon"
  | "dashboard.greeting.evening"
  | "dashboard.greeting.night" {
  const h = new Date().getHours();
  if (h < 5) return "dashboard.greeting.night";
  if (h < 12) return "dashboard.greeting.morning";
  if (h < 18) return "dashboard.greeting.afternoon";
  return "dashboard.greeting.evening";
}
