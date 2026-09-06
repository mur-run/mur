import { describe, expect, it } from "vitest";
import { ALL_DETAIL_TABS } from "../../types";
import { AGENT_TABS, FLEET_TABS, detailGroupOf } from "./detailTabs";

describe("detailGroupOf", () => {
  it("maps all 11 legacy ids into the 6 groups", () => {
    for (const legacy of ALL_DETAIL_TABS) {
      const g = detailGroupOf(legacy);
      expect(AGENT_TABS).toContain(g.tab);
      expect(g.anchor).toBe(legacy);
    }
    expect(detailGroupOf("persona").tab).toBe("identity");
    expect(detailGroupOf("permissions").tab).toBe("capabilities");
    expect(detailGroupOf("mobile").tab).toBe("channels");
    expect(detailGroupOf("schedule").tab).toBe("automation");
  });
  it("unknown or empty falls back to Overview", () => {
    expect(detailGroupOf(null)).toEqual({ tab: "overview", anchor: null });
    expect(detailGroupOf("nope")).toEqual({ tab: "overview", anchor: null });
  });
  it("tab orders match the spec", () => {
    expect(AGENT_TABS).toEqual(["overview", "identity", "capabilities", "memory", "automation", "channels"]);
    expect(FLEET_TABS).toEqual(["overview", "members", "jobs", "settings"]);
  });
});
