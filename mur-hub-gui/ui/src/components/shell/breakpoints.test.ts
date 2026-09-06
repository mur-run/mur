import { describe, expect, it } from "vitest";
import {
  BP_COMPACT, BP_WIDE, SIDEBAR_PREF_KEY, listModeFor, readSidebarPref,
  sidebarModeFor, togglePref, writeSidebarPref,
} from "./breakpoints";

function mem(): Pick<Storage, "getItem" | "setItem"> & { data: Record<string, string> } {
  const data: Record<string, string> = {};
  return { data, getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = v; } };
}

describe("sidebarModeFor", () => {
  it("auto follows the width", () => {
    expect(sidebarModeFor(BP_WIDE, "auto")).toBe("expanded");
    expect(sidebarModeFor(BP_WIDE - 1, "auto")).toBe("collapsed");
    expect(sidebarModeFor(BP_COMPACT - 1, "auto")).toBe("collapsed");
  });
  it("a pin wins over the width", () => {
    expect(sidebarModeFor(800, "expanded")).toBe("expanded");
    expect(sidebarModeFor(2000, "collapsed")).toBe("collapsed");
  });
});

describe("listModeFor", () => {
  it("three bands", () => {
    expect(listModeFor(BP_WIDE)).toBe("wide");
    expect(listModeFor(BP_COMPACT)).toBe("compact");
    expect(listModeFor(BP_COMPACT - 1)).toBe("overlay");
  });
});

describe("togglePref", () => {
  it("toggles relative to what is shown, and returns to auto when the pin matches auto", () => {
    expect(togglePref("auto", 1400)).toBe("collapsed");
    expect(togglePref("collapsed", 1400)).toBe("auto");
    expect(togglePref("auto", 1000)).toBe("expanded");
    expect(togglePref("expanded", 1000)).toBe("auto");
  });
});

describe("pref persistence", () => {
  it("round-trips and defaults to auto on junk", () => {
    const s = mem();
    expect(readSidebarPref(s)).toBe("auto");
    writeSidebarPref(s, "collapsed");
    expect(s.data[SIDEBAR_PREF_KEY]).toBe("collapsed");
    expect(readSidebarPref(s)).toBe("collapsed");
    s.data[SIDEBAR_PREF_KEY] = "sideways";
    expect(readSidebarPref(s)).toBe("auto");
  });
});
