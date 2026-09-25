import { describe, expect, it } from "vitest";
import {
  clampRailWidth,
  readRailWidth,
  writeRailWidth,
  MIN_CHAT_WIDTH,
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

  // The chat window opens at 380px wide and can be dragged narrower still,
  // while the rail's own ceiling is 420. Without a viewport-aware cap the rail
  // can eat the entire window and squeeze the conversation to nothing
  // (`.cw-main` is `min-width: 0`, so it collapses silently rather than
  // pushing back).
  it("never takes so much of a narrow window that the chat column vanishes", () => {
    const w = clampRailWidth(9999, 380);
    expect(w).toBeLessThanOrEqual(380 - MIN_CHAT_WIDTH);
    expect(w).toBeGreaterThanOrEqual(RAIL_MIN_WIDTH);
  });

  // A window narrower than rail-floor + chat-floor cannot satisfy both. The
  // rail floor wins (it is what CSS enforces anyway); the point is that the
  // clamp still returns a sane number instead of something below the floor.
  it("keeps the rail floor when the window is too narrow for both", () => {
    expect(clampRailWidth(9999, 200)).toBe(RAIL_MIN_WIDTH);
  });

  it("ignores an unusable viewport width and uses the plain ceiling", () => {
    expect(clampRailWidth(9999, 0)).toBe(RAIL_MAX_WIDTH);
    expect(clampRailWidth(9999, Number.NaN)).toBe(RAIL_MAX_WIDTH);
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
