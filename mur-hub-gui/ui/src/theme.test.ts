import { describe, it, expect } from "vitest";
import { themeAttr } from "./theme";

describe("themeAttr", () => {
  it("maps light/dark to the data-theme attribute value", () => {
    expect(themeAttr("light")).toBe("light");
    expect(themeAttr("dark")).toBe("dark");
  });
  it("maps system to null so prefers-color-scheme applies", () => {
    expect(themeAttr("system")).toBe(null);
  });
});
