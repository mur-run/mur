import { describe, it, expect } from "vitest";
import { channelLabel } from "./ChatChannelRail";
import type { ChannelSummary } from "../../work/types";

const UUID = "019ed0af-5e38-7912-b554-dc335a8fc2db";
const base: ChannelSummary = {
  id: UUID, title: "", state: "working", goal: "", created_at: "", updated_at: "",
  participants: [], agents: ["mur"], turns: 1, preview: "",
};

describe("channelLabel", () => {
  it("shows the channel title, never the uuid", () => {
    expect(channelLabel({ ...base, title: "install gitea for this mac" })).toBe("install gitea for this mac");
  });
  it("falls back to the preview line when the title is empty", () => {
    expect(channelLabel({ ...base, preview: "find the bug" })).toBe("find the bug");
  });
  it("keeps the short fleet name for fleet channels without a title", () => {
    expect(channelLabel({ ...base, id: "fleet-develop-rust" })).toBe("develop-rust");
  });
});
