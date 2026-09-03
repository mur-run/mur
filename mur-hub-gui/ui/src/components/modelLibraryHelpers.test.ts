import { describe, it, expect } from "vitest";
import { CLOUD_PRESETS, togglePick } from "./modelLibraryHelpers";
import { SUBSCRIPTION_PROVIDERS } from "./modelLibraryHelpers";
import { en } from "../i18n/en";
import { zhTW } from "../i18n/zh-TW";

describe("CLOUD_PRESETS", () => {
  it("ships the required provider keys", () => {
    const keys = CLOUD_PRESETS.map((p) => p.key);
    expect(keys).toEqual(
      expect.arrayContaining([
        "openai",
        "google",
        "openrouter",
        "xai",
        "mistral",
        "deepseek",
        "groq",
        "together",
        "fireworks",
        "cohere",
        "custom",
      ])
    );
  });

  it("has no duplicate provider keys", () => {
    const keys = CLOUD_PRESETS.map((p) => p.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("every preset has a non-empty name, baseUrl, logo, and color", () => {
    for (const p of CLOUD_PRESETS) {
      expect(p.name.length).toBeGreaterThan(0);
      expect(p.baseUrl.length).toBeGreaterThan(0);
      expect(p.logo.length).toBeGreaterThan(0);
      expect(p.color.length).toBeGreaterThan(0);
    }
  });
});

describe("togglePick", () => {
  it("adds an id that is not in the set", () => {
    const initial = new Set<string>(["a", "b"]);
    const result = togglePick(initial, "c");
    expect(result.has("c")).toBe(true);
    expect(result.size).toBe(3);
  });

  it("removes an id that is already in the set", () => {
    const initial = new Set<string>(["a", "b", "c"]);
    const result = togglePick(initial, "b");
    expect(result.has("b")).toBe(false);
    expect(result.size).toBe(2);
  });

  it("does not mutate the original set (immutable toggle)", () => {
    const initial = new Set<string>(["a"]);
    const frozen = new Set(initial);
    togglePick(initial, "b");
    expect(initial).toEqual(frozen);
  });

  it("works on an empty set (add)", () => {
    const result = togglePick(new Set<string>(), "x");
    expect(result.has("x")).toBe(true);
    expect(result.size).toBe(1);
  });

  it("removing the last element returns an empty set", () => {
    const result = togglePick(new Set<string>(["only"]), "only");
    expect(result.size).toBe(0);
  });
});

describe("subscription descriptors", () => {
  it("every copy key resolves in both languages and providers are distinct", () => {
    const keys = new Set<string>();
    for (const d of SUBSCRIPTION_PROVIDERS) {
      expect(keys.has(d.key)).toBe(false);
      keys.add(d.key);
      for (const k of Object.values(d.copy)) {
        expect(en[k], `${d.key}: ${k} missing in en`).toBeTruthy();
        expect(zhTW[k], `${d.key}: ${k} missing in zh-TW`).toBeTruthy();
      }
      expect(d.cliInstallCmd.length).toBeGreaterThan(0);
    }
    expect(new Set(SUBSCRIPTION_PROVIDERS.map((d) => d.provider)).size).toBe(
      SUBSCRIPTION_PROVIDERS.length,
    );
  });
});
