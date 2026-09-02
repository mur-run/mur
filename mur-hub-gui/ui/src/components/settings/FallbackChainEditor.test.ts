import { describe, it, expect } from "vitest";
import { paidFallbackWarning } from "../chatgptSubscription";
import { pickNextFallback } from "./modelSwitch";

const sub = { ref_name: "chatgpt_sol", billing: "subscription" as const };
const paid = { ref_name: "openai_gpt", billing: "usage_billed" as const };
const local = { ref_name: "ollama_llama", billing: "local" as const };
const unknown = { ref_name: "legacy" };

describe("paidFallbackWarning", () => {
  it("subscription → usage-billed warns", () => {
    expect(paidFallbackWarning(sub, paid)).toBe("settings.modelSwitch.paidFallback");
  });
  it("subscription → local or subscription is silent", () => {
    expect(paidFallbackWarning(sub, local)).toBeNull();
    expect(paidFallbackWarning(sub, { ...sub, ref_name: "chatgpt_mini" })).toBeNull();
  });
  it("unknown billing is a neutral warning, never silence", () => {
    expect(paidFallbackWarning(sub, unknown)).toBe("settings.modelSwitch.unknownFallback");
  });
  it("a non-subscription primary never warns", () => {
    expect(paidFallbackWarning(paid, sub)).toBeNull();
    expect(paidFallbackWarning(unknown, paid)).toBeNull();
    expect(paidFallbackWarning(null, paid)).toBeNull();
    expect(paidFallbackWarning(sub, undefined)).toBeNull();
  });
});

describe("pickNextFallback", () => {
  it("never pre-selects a paid fallback for a subscription primary while a safe one exists", () => {
    expect(pickNextFallback([], [paid, local, sub], "chatgpt_sol")).toBe("ollama_llama");
    expect(pickNextFallback(["ollama_llama"], [paid, local, sub], "chatgpt_sol")).toBe("chatgpt_sol");
  });
  it("falls back to the first unused option when nothing safe is left", () => {
    expect(pickNextFallback(["ollama_llama", "chatgpt_sol"], [paid, local, sub], "chatgpt_sol")).toBe("openai_gpt");
  });
  it("is plain first-unused for a non-subscription primary", () => {
    expect(pickNextFallback([], [paid, local], "openai_gpt")).toBe("openai_gpt");
    expect(pickNextFallback(["openai_gpt"], [paid, local], null)).toBe("ollama_llama");
  });
  it("returns undefined with no options", () => {
    expect(pickNextFallback([], [], "x")).toBeUndefined();
  });
});
