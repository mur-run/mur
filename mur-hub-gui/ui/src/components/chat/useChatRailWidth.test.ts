import { describe, expect, it } from "vitest";
import {
  clampRailWidth,
  readRailWidth,
  writeRailWidth,
  RAIL_DEFAULT_WIDTH,
  RAIL_MAX_WIDTH,
  RAIL_MIN_WIDTH,
  RAIL_WIDTH_KEY,
  type WidthStore,
} from "./useChatRailWidth";

function fakeStore(initial: Record<string, string> = {}): WidthStore & { map: Record<string, string> } {
  const map = { ...initial };
  return {
    map,
    getItem: (k) => (k in map ? map[k] : null),
    setItem: (k, v) => {
      map[k] = v;
    },
  };
}

describe("clampRailWidth", () => {
  it("keeps a width inside the bounds untouched", () => {
    expect(clampRailWidth(240)).toBe(240);
  });
  it("clamps a drag past either edge instead of collapsing the rail", () => {
    expect(clampRailWidth(10)).toBe(RAIL_MIN_WIDTH);
    expect(clampRailWidth(9999)).toBe(RAIL_MAX_WIDTH);
  });
  it("falls back to the default for a non-number", () => {
    expect(clampRailWidth(Number.NaN)).toBe(RAIL_DEFAULT_WIDTH);
  });
});

describe("rail width persistence", () => {
  it("round-trips a width through the store", () => {
    const store = fakeStore();
    writeRailWidth(320, store);
    expect(store.map[RAIL_WIDTH_KEY]).toBe("320");
    expect(readRailWidth(store)).toBe(320);
  });

  it("uses the default when nothing was ever stored", () => {
    expect(readRailWidth(fakeStore())).toBe(RAIL_DEFAULT_WIDTH);
  });

  it("clamps a stored value that is out of range or junk", () => {
    expect(readRailWidth(fakeStore({ [RAIL_WIDTH_KEY]: "20" }))).toBe(RAIL_MIN_WIDTH);
    expect(readRailWidth(fakeStore({ [RAIL_WIDTH_KEY]: "banana" }))).toBe(RAIL_DEFAULT_WIDTH);
  });

  it("survives storage being unavailable", () => {
    expect(readRailWidth(null)).toBe(RAIL_DEFAULT_WIDTH);
    expect(() => writeRailWidth(300, null)).not.toThrow();
  });
});
