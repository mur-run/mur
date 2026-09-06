import { describe, expect, it } from "vitest";
import { isEditingTarget, isOpenInWindowShortcut } from "./openInWindow";

function key(over: Partial<KeyboardEvent>): KeyboardEvent {
  return { metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, key: "Enter", ...over } as KeyboardEvent;
}
function el(tagName: string, contenteditable: string | null = null): Element {
  return { tagName, getAttribute: () => contenteditable } as unknown as Element;
}

describe("isOpenInWindowShortcut", () => {
  it("accepts ⌘↩ and Ctrl+Enter", () => {
    expect(isOpenInWindowShortcut(key({ metaKey: true }))).toBe(true);
    expect(isOpenInWindowShortcut(key({ ctrlKey: true }))).toBe(true);
  });
  it("rejects plain Enter and extra modifiers", () => {
    expect(isOpenInWindowShortcut(key({}))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, shiftKey: true }))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, altKey: true }))).toBe(false);
    expect(isOpenInWindowShortcut(key({ metaKey: true, key: "k" }))).toBe(false);
  });
});

describe("isEditingTarget", () => {
  it("is true for fields and contenteditable, false otherwise", () => {
    expect(isEditingTarget(el("INPUT"))).toBe(true);
    expect(isEditingTarget(el("TEXTAREA"))).toBe(true);
    expect(isEditingTarget(el("SELECT"))).toBe(true);
    expect(isEditingTarget(el("DIV", "true"))).toBe(true);
    expect(isEditingTarget(el("DIV"))).toBe(false);
    expect(isEditingTarget(null)).toBe(false);
  });
});
