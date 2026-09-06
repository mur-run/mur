import { describe, expect, it } from "vitest";
import { isSidebarToggle } from "./Shell";

describe("isSidebarToggle", () => {
  const base = { key: "\\", metaKey: true, altKey: false, ctrlKey: false, shiftKey: false };
  it("matches meta+backslash", () => {
    expect(isSidebarToggle(base as KeyboardEvent)).toBe(true);
  });
  it("rejects extra modifiers and other keys", () => {
    expect(isSidebarToggle({ ...base, altKey: true } as KeyboardEvent)).toBe(false);
    expect(isSidebarToggle({ ...base, key: "/" } as KeyboardEvent)).toBe(false);
  });
});
