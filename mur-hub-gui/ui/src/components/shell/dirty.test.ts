import { describe, expect, it } from "vitest";
import { shouldConfirmLeave } from "./dirty";

describe("shouldConfirmLeave", () => {
  it("only when something is dirty", () => {
    expect(shouldConfirmLeave(new Set())).toBe(false);
    expect(shouldConfirmLeave(new Set(["persona"]))).toBe(true);
  });
});
