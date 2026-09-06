import { describe, expect, it } from "vitest";
import { clampWidth, parseStoredWidth } from "./useResizableColumn";

describe("clampWidth", () => {
  it("rounds and clamps", () => {
    expect(clampWidth(239.6, 240, 400)).toBe(240);
    expect(clampWidth(1000, 240, 400)).toBe(400);
    expect(clampWidth(300.4, 240, 400)).toBe(300);
  });
});

describe("parseStoredWidth", () => {
  it("falls back on junk and clamps stored values", () => {
    expect(parseStoredWidth(null, 300, 240, 400)).toBe(300);
    expect(parseStoredWidth("abc", 300, 240, 400)).toBe(300);
    expect(parseStoredWidth("9999", 300, 240, 400)).toBe(400);
    expect(parseStoredWidth("260", 300, 240, 400)).toBe(260);
  });
});
