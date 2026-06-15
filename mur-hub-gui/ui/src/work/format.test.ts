import { describe, it, expect } from "vitest";
import {
  eventVariant,
  eventKindLabel,
  actorName,
  relativeTime,
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
