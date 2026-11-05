import type { ChannelActor, ChannelSummary } from "./types";

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

/** Characters of a channel uuid shown as its typeable handle. Must match the
 *  CLI's footer chip and `/channels <prefix>` resolver (8). */
export const SHORT_ID_LEN = 8;

/** The id prefix a human can type into `/channels` to reach this channel. */
export function shortId(id: string): string {
  return id.slice(0, SHORT_ID_LEN);
}

/** `name → display_name` for {@link channelTitle}; keeps the uppercase label
 *  (rule 8) without each surface rebuilding the map. */
export function displayNameMap(
  agents: ReadonlyArray<{ name: string; display_name: string }>,
): Record<string, string> {
  return Object.fromEntries(agents.map((a) => [a.name, a.display_name]));
}

/** Separator between the agent and the date in an untitled channel's label. */
export const TITLE_FALLBACK_SEP = " · ";

/**
 * What a surface prints as a channel's name. The stored title is derived from
 * the first human message, so a channel the agent opened (a proactive
 * companion message, an agent-authored opener) has none until the human
 * replies. Spec §7.5: render `{agent display name} · {local date}` then — a
 * display-only fallback, never written back. A channel with neither an agent
 * nor a parseable date keeps the short id, the last resort.
 */
export function channelTitle(
  ch: Pick<ChannelSummary, "id" | "title" | "agents" | "created_at">,
  displayNames: Record<string, string> = {},
  locale?: string,
): string {
  const title = ch.title.trim();
  if (title) return title;
  const who = ch.agents.map((a) => displayNames[a] || a).join(", ");
  const created = new Date(ch.created_at);
  if (!who || Number.isNaN(created.getTime())) return shortId(ch.id);
  return `${who}${TITLE_FALLBACK_SEP}${created.toLocaleDateString(locale)}`;
}

/** The display number for a channel, `#2`. Ordinals start at 1; anything
 *  lower means the row predates the index backfill, so show nothing rather
 *  than a number that would not work when typed. */
export function ordinalLabel(ordinal: number): string {
  return ordinal >= 1 ? `#${ordinal}` : "";
}
