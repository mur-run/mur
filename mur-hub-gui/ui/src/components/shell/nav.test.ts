import { describe, expect, it } from "vitest";
import { NAV_ITEMS, isLibrary, isPageId } from "./nav";

describe("NAV_ITEMS", () => {
  it("is ordered exactly per spec §1", () => {
    expect(NAV_ITEMS.map((i) => i.id)).toEqual([
      "home",
      "chats",
      "agents",
      "fleets",
      "skills",
      "workflows",
      "mcp",
      "models",
      "plugins",
    ]);
  });

  it("splits into workspace (4) and library (5) groups", () => {
    const workspace = NAV_ITEMS.filter((i) => i.group === "workspace");
    const library = NAV_ITEMS.filter((i) => i.group === "library");
    expect(workspace).toHaveLength(4);
    expect(library).toHaveLength(5);
  });
});

describe("isLibrary", () => {
  it("returns true for library-group ids", () => {
    expect(isLibrary("skills")).toBe(true);
  });
  it("returns false for workspace-group ids", () => {
    expect(isLibrary("home")).toBe(false);
  });
});

describe("isPageId", () => {
  it("accepts nav ids and rejects strangers", () => {
    expect(isPageId("agents")).toBe(true);
    expect(isPageId("home")).toBe(true);
    expect(isPageId("nope")).toBe(false);
  });
});
