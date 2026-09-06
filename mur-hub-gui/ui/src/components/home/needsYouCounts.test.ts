import { describe, expect, it } from "vitest";
import { needsYouCounts } from "./needsYouCounts";
import type { InboxItem } from "./inbox";

const item = (kind: InboxItem["kind"], id: string, agent?: string): InboxItem =>
  ({ kind, id, ts: 1, title: "", subtitle: "", payload: null, agent });

describe("needsYouCounts", () => {
  it("counts per agent and ignores items without an agent", () => {
    const counts = needsYouCounts([
      item("hitl", "1", "aura"),
      item("companion", "2", "aura"),
      item("hitl", "3", "scout"),
      item("upgrade_blocked", "4"),
    ]);
    expect(counts).toEqual({ aura: 2, scout: 1 });
  });
});
