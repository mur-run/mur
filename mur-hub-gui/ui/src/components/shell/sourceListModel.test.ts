import { describe, expect, it } from "vitest";
import { filterRows, moveSelection } from "./sourceListModel";

const rows = [
  { id: "aura", name: "AURA", subtitle: "Engineer · claude-sonnet", facets: ["Engineer"] },
  { id: "scout", name: "Scout", subtitle: "Research", facets: ["Research"] },
  { id: "muse", name: "Muse", subtitle: undefined, facets: ["__none__"] },
];

describe("filterRows", () => {
  it("text matches name or subtitle, case-insensitive", () => {
    expect(filterRows(rows, "sonnet", null).map((r) => r.id)).toEqual(["aura"]);
    expect(filterRows(rows, "SCOUT", null).map((r) => r.id)).toEqual(["scout"]);
  });
  it("facet and text compose", () => {
    expect(filterRows(rows, "", "Research").map((r) => r.id)).toEqual(["scout"]);
    expect(filterRows(rows, "a", "Engineer").map((r) => r.id)).toEqual(["aura"]);
  });
});

describe("moveSelection", () => {
  it("steps within bounds and enters from either end", () => {
    expect(moveSelection(rows, null, 1)).toBe("aura");
    expect(moveSelection(rows, null, -1)).toBe("muse");
    expect(moveSelection(rows, "aura", 1)).toBe("scout");
    expect(moveSelection(rows, "muse", 1)).toBe("muse");
    expect(moveSelection([], "x", 1)).toBeNull();
  });
});
