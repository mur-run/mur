import { describe, it, expect } from "vitest";
import { translate } from "./index";

describe("translate", () => {
  it("returns the English string for a key", () => {
    expect(translate("en", "app.newAgent")).toBe("New Agent");
  });
  it("returns the zh-TW string for a key", () => {
    expect(translate("zh-TW", "app.newAgent")).toBe("新增 Agent");
  });
  it("interpolates {vars}", () => {
    expect(translate("en", "dashboard.greeting", { name: "David" })).toBe("Good evening, David 👋");
  });
  it("falls back to en when a lang table is missing the key at runtime", () => {
    // "ja" is an unknown lang with no table; translate() falls back to en.
    expect(translate("ja", "app.newAgent")).toBe("New Agent");
  });
});
