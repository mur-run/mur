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
  plugins: "🧩",
};

import { BUILTIN_PRESETS, type AgentEntry } from "./types";

// preset id → family, for theming the vector mascot avatar (cards + detail).
const PRESET_FAMILY: Record<string, string> = Object.fromEntries(
  BUILTIN_PRESETS.map((p) => [p.id, p.family]),
);
export const familyOf = (presetId: string): string =>
  PRESET_FAMILY[presetId] ?? "chibi";

// Agents created without a chosen style all fall back to the same green blob,
// making them hard to tell apart. When no preset is set, derive a stable
// distinct one from the agent name (identicon-style). Explicit user choices
// (e.g. the MUR concierge) are always respected.
export function avatarPreset(agent: AgentEntry): string {
  // "default-blob" is the value assigned at creation, not a deliberate pick —
  // treat it (and empty) as unset so the agent gets a distinct name-derived look.
  if (agent.style_preset && agent.style_preset !== "default-blob")
    return agent.style_preset;
  let h = 0;
  for (let i = 0; i < agent.name.length; i++)
    h = (h * 31 + agent.name.charCodeAt(i)) >>> 0;
  return BUILTIN_PRESETS[h % BUILTIN_PRESETS.length].id;
}

/**
 * Maps a supervisor runtime state to a status pill. Shared by the agent
 * list/cards AND the detail panel so they never disagree (was a bug: the
 * detail panel derived status from the lock-based AgentEntry.status instead).
 */
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
