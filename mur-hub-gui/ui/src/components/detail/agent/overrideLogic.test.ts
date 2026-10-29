import { describe, expect, it } from "vitest";
import { NO_CHOICE, allChosen, anyChosen, canContinue, toggleAll } from "./overrideLogic";

describe("overrideLogic", () => {
  it("defaults to nothing chosen", () => {
    expect(NO_CHOICE).toEqual({ noHitl: false, netUnrestricted: false, fullPerms: false });
    expect(anyChosen(NO_CHOICE)).toBe(false);
    expect(allChosen(NO_CHOICE)).toBe(false);
  });

  it("toggleAll from a partial choice selects all", () => {
    const next = toggleAll({ ...NO_CHOICE, noHitl: true });
    expect(allChosen(next)).toBe(true);
  });

  it("toggleAll from all clears", () => {
    expect(toggleAll(toggleAll(NO_CHOICE))).toEqual(NO_CHOICE);
  });

  it("canContinue needs a flag", () => {
    expect(canContinue(NO_CHOICE, "alice", "alice")).toBe(false);
  });

  it("canContinue needs the exact name", () => {
    const c = { ...NO_CHOICE, fullPerms: true };
    expect(canContinue(c, "alice ", "alice")).toBe(false);
    expect(canContinue(c, "Alice", "alice")).toBe(false);
    expect(canContinue(c, "bob", "alice")).toBe(false);
    expect(canContinue(c, "alice", "alice")).toBe(true);
  });
});
