import { describe, it, expect } from "vitest";
import {
  eventVariant,
  eventKindLabel,
  actorName,
  relativeTime,
  ordinalLabel,
  shortId,
  SHORT_ID_LEN,
  channelTitle,
  displayNameMap,
  TITLE_FALLBACK_SEP,
} from "./format";
import type { ChannelActor } from "./types";

describe("eventVariant", () => {
  it("maps known kinds to render variant", () => {
    expect(eventVariant("message")).toBe("message");
    expect(eventVariant("note")).toBe("note");
    expect(eventVariant("state-change")).toBe("state");
  });
  it("maps unknown kinds to card", () => {
    expect(eventVariant("delegation")).toBe("card");
    expect(eventVariant("tool-call")).toBe("card");
    expect(eventVariant("hitl-request")).toBe("card");
    expect(eventVariant("totally-unknown")).toBe("card");
  });
});

describe("eventKindLabel", () => {
  it("title-cases kebab strings", () => {
    expect(eventKindLabel("tool-call")).toBe("Tool Call");
    expect(eventKindLabel("hitl-request")).toBe("Hitl Request");
    expect(eventKindLabel("message")).toBe("Message");
  });
});

describe("actorName", () => {
  it("resolves agent display name", () => {
    const a: ChannelActor = { kind: "agent", id: "qa" };
    expect(actorName(a, { qa: "QA Bot" })).toBe("QA Bot");
  });
  it("falls back to id when no display name", () => {
    const a: ChannelActor = { kind: "agent", id: "mur" };
    expect(actorName(a, {})).toBe("mur");
  });
  it("labels human by name", () => {
    expect(actorName({ kind: "human", name: "alan" }, {})).toBe("alan");
  });
  it("labels nameless human as 'you'", () => {
    expect(actorName({ kind: "human" }, {})).toBe("you");
  });
  it("labels system", () => {
    expect(actorName({ kind: "system" }, {})).toBe("system");
  });
});

describe("relativeTime", () => {
  const now = Date.parse("2026-06-15T12:00:00Z");

  it("shows 'just now' for sub-minute", () => {
    expect(relativeTime("2026-06-15T11:59:50Z", now)).toBe("just now");
  });
  it("shows minutes", () => {
    expect(relativeTime("2026-06-15T11:55:00Z", now)).toBe("5m ago");
  });
  it("shows hours", () => {
    expect(relativeTime("2026-06-15T09:00:00Z", now)).toBe("3h ago");
  });
  it("shows days", () => {
    expect(relativeTime("2026-06-13T12:00:00Z", now)).toBe("2d ago");
  });
});

describe("channel handles", () => {
  const UUID = "019ed0af-5e38-7912-b554-dc335a8fc2db";

  it("shortens an id to the same prefix the CLI accepts", () => {
    expect(shortId(UUID)).toBe("019ed0af");
    expect(shortId(UUID)).toHaveLength(SHORT_ID_LEN);
  });

  it("labels an ordinal with a hash", () => {
    expect(ordinalLabel(2)).toBe("#2");
  });

  it("shows nothing for an unassigned ordinal rather than a number that would not work", () => {
    expect(ordinalLabel(0)).toBe("");
    expect(ordinalLabel(-1)).toBe("");
  });
});

describe("channelTitle", () => {
  const UUID = "019ed0af-5e38-7912-b554-dc335a8fc2db";
  const CREATED = "2026-11-05T06:43:08Z";
  const base = { id: UUID, title: "", agents: ["mur"], created_at: CREATED };
  const date = new Date(CREATED).toLocaleDateString("en-US");

  it("uses the stored title when there is one", () => {
    expect(channelTitle({ ...base, title: "explain this repo" })).toBe("explain this repo");
  });
  it("renders {agent display name} · {date} for an agent-opened channel", () => {
    expect(channelTitle(base, { mur: "MUR" }, "en-US")).toBe(`MUR${TITLE_FALLBACK_SEP}${date}`);
  });
  it("falls back to the raw agent name when no display name is known", () => {
    expect(channelTitle(base, {}, "en-US")).toBe(`mur${TITLE_FALLBACK_SEP}${date}`);
  });
  it("treats a whitespace-only title as untitled", () => {
    expect(channelTitle({ ...base, title: "  " }, {}, "en-US")).toBe(`mur${TITLE_FALLBACK_SEP}${date}`);
  });
  it("keeps the short id only when there is no agent or no date", () => {
    expect(channelTitle({ ...base, agents: [] })).toBe(shortId(UUID));
    expect(channelTitle({ ...base, created_at: "" })).toBe(shortId(UUID));
  });
});

describe("displayNameMap", () => {
  it("maps directory names to display names", () => {
    expect(displayNameMap([{ name: "mur", display_name: "MUR" }])).toEqual({ mur: "MUR" });
  });
});
