import { describe, expect, it } from "vitest";
import { SETTINGS_SECTIONS, isSettingsSection } from "./settingsSections";

describe("SETTINGS_SECTIONS", () => {
  it("lists the five sections in order, ids unique", () => {
    const ids = SETTINGS_SECTIONS.map((s) => s.id);
    expect(ids).toEqual(["general", "models", "updates", "data", "about"]);
    expect(new Set(ids).size).toBe(ids.length);
  });
  it("isSettingsSection accepts each id and rejects strangers", () => {
    for (const s of SETTINGS_SECTIONS) expect(isSettingsSection(s.id)).toBe(true);
    expect(isSettingsSection("nope")).toBe(false);
  });
});
