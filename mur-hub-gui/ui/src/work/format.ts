import type { ChannelActor } from "./types";

export type EventVariant = "message" | "note" | "state" | "card";

export function eventVariant(kind: string): EventVariant {
  switch (kind) {
    case "message":
      return "message";
    case "note":
      return "note";
    case "state-change":
      return "state";
    default:
      return "card";
  }
}

export function eventKindLabel(kind: string): string {
  return kind
    .split("-")
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

export function actorName(
  actor: ChannelActor,
  displayNames: Record<string, string> = {},
): string {
  switch (actor.kind) {
    case "agent": {
      const id = actor.id ?? "";
      return displayNames[id] ?? id;
    }
    case "human":
      return actor.name ?? "you";
    case "system":
      return "system";
  }
}

const MINUTE = 60_000;
const HOUR = 3_600_000;
const DAY = 86_400_000;

export function relativeTime(isoTs: string, nowMs: number): string {
  const diffMs = nowMs - new Date(isoTs).getTime();
  if (diffMs < MINUTE) return "just now";
  if (diffMs < HOUR) return `${Math.floor(diffMs / MINUTE)}m ago`;
  if (diffMs < DAY) return `${Math.floor(diffMs / HOUR)}h ago`;
  return `${Math.floor(diffMs / DAY)}d ago`;
}

const STATE_BADGE_MAP: Record<string, string> = {
  submitted: "submitted",
  working: "working",
  "input-required": "inputRequired",
  completed: "completed",
  failed: "failed",
  canceled: "canceled",
  rejected: "rejected",
};

/** Returns a CSS modifier suffix for `.work-badge--<suffix>`. */
export function stateBadge(state: string): string {
  return STATE_BADGE_MAP[state] ?? "unknown";
}
