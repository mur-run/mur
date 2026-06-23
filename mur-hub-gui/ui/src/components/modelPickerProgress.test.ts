import { describe, it, expect } from "vitest";
import { downloadProgress } from "./modelPickerProgress";

describe("downloadProgress", () => {
  it("normal case: computes percent correctly", () => {
    const result = downloadProgress(500, 1000);
    expect(result.indeterminate).toBe(false);
    expect(result.percent).toBe(50);
    expect(typeof result.label).toBe("string");
  });

  it("total=0 → indeterminate", () => {
    const result = downloadProgress(100, 0);
    expect(result.indeterminate).toBe(true);
    expect(result.percent).toBe(0);
  });

  it("total<0 → indeterminate", () => {
    const result = downloadProgress(0, -1);
    expect(result.indeterminate).toBe(true);
  });

  it("done > total clamps to 100", () => {
    const result = downloadProgress(1500, 1000);
    expect(result.indeterminate).toBe(false);
    expect(result.percent).toBe(100);
  });

  it("done=0, total=0 → indeterminate (no NaN)", () => {
    const result = downloadProgress(0, 0);
    expect(result.indeterminate).toBe(true);
    expect(Number.isNaN(result.percent)).toBe(false);
  });

  it("100% when done equals total", () => {
    const result = downloadProgress(1000, 1000);
    expect(result.indeterminate).toBe(false);
    expect(result.percent).toBe(100);
  });
});
