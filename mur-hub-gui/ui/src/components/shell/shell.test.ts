import { describe, expect, it } from "vitest";
import { isInspectorToggle } from "./Shell";

function key(overrides: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    key: "i",
    metaKey: true,
    altKey: true,
    ctrlKey: false,
    shiftKey: false,
    ...overrides,
  } as KeyboardEvent;
}

describe("isInspectorToggle", () => {
  it("matches meta+alt+i", () => {
    expect(isInspectorToggle(key({}))).toBe(true);
  });

  it("is case-insensitive", () => {
    expect(isInspectorToggle(key({ key: "I" }))).toBe(true);
  });

  it("rejects missing meta", () => {
    expect(isInspectorToggle(key({ metaKey: false }))).toBe(false);
  });

  it("rejects missing alt", () => {
    expect(isInspectorToggle(key({ altKey: false }))).toBe(false);
  });

  it("rejects extra ctrl modifier", () => {
    expect(isInspectorToggle(key({ ctrlKey: true }))).toBe(false);
  });

  it("rejects extra shift modifier", () => {
    expect(isInspectorToggle(key({ shiftKey: true }))).toBe(false);
  });

  it("rejects a different key", () => {
    expect(isInspectorToggle(key({ key: "j" }))).toBe(false);
  });
});
