import { describe, it, expect } from "vitest";
import { channelLabel, idChip, rowTooltip } from "./ChatChannelRail";
import type { ChannelSummary } from "../../work/types";

const UUID = "019ed0af-5e38-7912-b554-dc335a8fc2db";
const base: ChannelSummary = {
  id: UUID, title: "", state: "working", goal: "", created_at: "", updated_at: "",
  participants: [], agents: ["mur"], turns: 1, preview: "", ordinal: 2, handle: "",
};

describe("channelLabel", () => {
  it("shows the channel title, never the uuid", () => {
    expect(channelLabel({ ...base, title: "install gitea for this mac" })).toBe("install gitea for this mac");
  });
  it("falls back to the preview line when the title is empty", () => {
    expect(channelLabel({ ...base, preview: "find the bug" })).toBe("find the bug");
  });
  it("labels an agent-opened channel {agent} · {date}, not its uuid", () => {
    const label = channelLabel({ ...base, created_at: "2026-11-05T06:43:08Z" }, { mur: "MUR" });
    expect(label.startsWith("MUR · ")).toBe(true);
    expect(label).not.toContain(UUID.slice(0, 8));
  });
  it("keeps the legacy main label for an agent-named channel", () => {
    expect(channelLabel({ ...base, id: "rustsmith" })).toBe("main");
  });
  it("keeps the short fleet name for fleet channels without a title", () => {
    expect(channelLabel({ ...base, id: "fleet-develop-rust" })).toBe("develop-rust");
  });
});

describe("the id handle on a rail row", () => {
  const run: ChannelSummary = {
    ...base, id: "fleet-develop-rust", title: "fleet: develop-rust", handle: "#fleet-develop-rust",
  };
  it("prints a conversation's handle as the backend computed it", () => {
    expect(idChip({ ...base, handle: "019ed0af5" })).toBe("019ed0af5");
  });
  it("never prints eight characters of a run's id, which name no run", () => {
    expect(idChip(run)).toBe("");
    expect(idChip(run)).not.toBe("fleet-de");
  });
  it("keeps a run's whole handle in the tooltip, where it can be read back", () => {
    expect(rowTooltip(run)).toBe("#2 · #fleet-develop-rust — fleet: develop-rust");
    expect(rowTooltip({ ...base, handle: "019ed0af5", ordinal: 0 })).toBe(`019ed0af5 — ${UUID}`);
  });
});
