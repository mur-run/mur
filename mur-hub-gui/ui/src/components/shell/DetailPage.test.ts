import { describe, expect, it } from "vitest";
import { nextTab } from "./DetailPage";

const tabs = [{ id: "a", label: "A" }, { id: "b", label: "B" }, { id: "c", label: "C" }];

describe("nextTab", () => {
  it("wraps in both directions", () => {
    expect(nextTab(tabs, "a", 1)).toBe("b");
    expect(nextTab(tabs, "c", 1)).toBe("a");
    expect(nextTab(tabs, "a", -1)).toBe("c");
  });
});
