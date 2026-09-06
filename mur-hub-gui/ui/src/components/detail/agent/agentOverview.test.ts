import { describe, expect, it } from "vitest";
import { activityFor } from "./agentOverview";
import type { ChannelSummary } from "../../../work/types";

const ch = (id: string, agents: string[], state: string, updated_at: string): ChannelSummary =>
  ({ id, title: id, state, goal: "", created_at: updated_at, updated_at, participants: [], agents, turns: 1, preview: "" });

describe("activityFor", () => {
  it("now = newest non-terminal channel; recent = newest first, capped", () => {
    const channels = [
      ch("old", ["aura"], "completed", "2026-09-01T00:00:00Z"),
      ch("live", ["aura", "mur"], "running", "2026-09-06T10:00:00Z"),
      ch("other", ["scout"], "running", "2026-09-06T11:00:00Z"),
      ch("done", ["aura"], "completed", "2026-09-05T00:00:00Z"),
    ];
    const a = activityFor(channels, "aura", 2);
    expect(a.now?.id).toBe("live");
    expect(a.recent.map((c) => c.id)).toEqual(["live", "done"]);
    expect(activityFor(channels, "nobody").now).toBeNull();
  });
});
