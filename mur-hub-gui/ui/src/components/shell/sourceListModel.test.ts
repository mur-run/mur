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

import {
  applySelection, collapseSelection, extendSelection, selectAll, selectModeOf, type Selection,
} from "./sourceListModel";

const ids = ["a", "b", "c", "d", "e"];
const sel = (anchor: string | null, ...on: string[]): Selection => ({ anchor, ids: new Set(on) });
const on = (s: Selection) => [...s.ids].sort();

describe("selectModeOf", () => {
  it("shift → range, meta/ctrl → toggle, else single", () => {
    expect(selectModeOf({ metaKey: false, ctrlKey: false, shiftKey: true })).toBe("range");
    expect(selectModeOf({ metaKey: true, ctrlKey: false, shiftKey: false })).toBe("toggle");
    expect(selectModeOf({ metaKey: false, ctrlKey: true, shiftKey: false })).toBe("toggle");
    expect(selectModeOf({ metaKey: false, ctrlKey: false, shiftKey: false })).toBe("single");
  });
});

describe("applySelection", () => {
  it("single replaces everything and moves the anchor", () => {
    const s = applySelection(ids, sel("a", "a", "b"), "d", "single");
    expect(s.anchor).toBe("d");
    expect(on(s)).toEqual(["d"]);
  });
  it("toggle adds and moves the anchor to the added row", () => {
    const s = applySelection(ids, sel("a", "a"), "c", "toggle");
    expect(s.anchor).toBe("c");
    expect(on(s)).toEqual(["a", "c"]);
  });
  it("toggle removes; the anchor stays if still selected, else moves to a remaining row, else null", () => {
    expect(applySelection(ids, sel("a", "a", "c"), "c", "toggle")).toEqual(sel("a", "a"));
    const moved = applySelection(ids, sel("c", "a", "c"), "c", "toggle");
    expect(moved.anchor).toBe("a");
    expect(on(moved)).toEqual(["a"]);
    expect(applySelection(ids, sel("a", "a"), "a", "toggle")).toEqual(sel(null));
  });
  it("range selects the visible rows between anchor and click, either direction, anchor unchanged", () => {
    const down = applySelection(ids, sel("b", "b"), "d", "range");
    expect(down.anchor).toBe("b");
    expect(on(down)).toEqual(["b", "c", "d"]);
    const up = applySelection(ids, sel("d", "d"), "a", "range");
    expect(up.anchor).toBe("d");
    expect(on(up)).toEqual(["a", "b", "c", "d"]);
  });
  it("range without an anchor, or with one that is not visible, is a single", () => {
    expect(applySelection(ids, sel(null), "c", "range")).toEqual(sel("c", "c"));
    expect(applySelection(ids, sel("zzz", "zzz"), "c", "range")).toEqual(sel("c", "c"));
  });
});

describe("extendSelection", () => {
  it("grows the anchor's block by one visible row in the given direction", () => {
    expect(on(extendSelection(ids, sel("b", "b"), 1))).toEqual(["b", "c"]);
    expect(on(extendSelection(ids, sel("b", "b", "c"), 1))).toEqual(["b", "c", "d"]);
    expect(on(extendSelection(ids, sel("c", "b", "c"), -1))).toEqual(["a", "b", "c"]);
  });
  it("stops at the edges and without an anchor", () => {
    expect(extendSelection(ids, sel("e", "e"), 1)).toEqual(sel("e", "e"));
    expect(extendSelection(ids, sel(null), 1)).toEqual(sel(null));
  });
});

describe("selectAll / collapseSelection", () => {
  it("selectAll takes every visible row and keeps a visible anchor", () => {
    const s = selectAll(["b", "c"], sel("c", "c"));
    expect(s.anchor).toBe("c");
    expect(on(s)).toEqual(["b", "c"]);
    expect(selectAll(["b", "c"], sel("a", "a")).anchor).toBe("b");
    expect(selectAll([], sel("a", "a"))).toEqual(sel("a", "a"));
  });
  it("collapseSelection keeps only the anchor", () => {
    expect(collapseSelection(sel("b", "a", "b", "c"))).toEqual(sel("b", "b"));
    expect(collapseSelection(sel(null, "a"))).toEqual(sel(null));
  });
});
