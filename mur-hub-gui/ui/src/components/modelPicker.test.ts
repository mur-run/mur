import { describe, it, expect } from "vitest";
import {
  filterModels,
  groupByProvider,
  formatCost,
  type ModelOption,
} from "./modelPicker";

const M: ModelOption[] = [
  {
    ref_name: "claude_opus",
    provider: "anthropic",
    model: "claude-opus-4-8",
    capabilities: ["vision", "tool_use"],
  },
  {
    ref_name: "claude_sonnet",
    provider: "anthropic",
    model: "claude-sonnet-5",
    capabilities: ["tool_use"],
  },
  {
    ref_name: "ds_flash",
    provider: "deepseek",
    model: "deepseek-v4-flash",
    capabilities: [],
  },
];

describe("filterModels", () => {
  it("returns all models for empty term", () => {
    expect(filterModels(M, "")).toEqual(M);
  });

  it("filters by ref_name (case-insensitive)", () => {
    expect(filterModels(M, "claude_opus")).toHaveLength(1);
    expect(filterModels(M, "CLAUDE_OPUS")).toHaveLength(1);
    expect(filterModels(M, "opus")).toHaveLength(1);
  });

  it("filters by provider (case-insensitive)", () => {
    expect(filterModels(M, "anthropic")).toHaveLength(2);
    expect(filterModels(M, "deepseek")).toHaveLength(1);
    expect(filterModels(M, "DEEP")).toHaveLength(1);
  });

  it("filters by model (case-insensitive)", () => {
    expect(filterModels(M, "claude-opus")).toHaveLength(1);
    expect(filterModels(M, "4-8")).toHaveLength(1);
    expect(filterModels(M, "v4-flash")).toHaveLength(1);
  });

  it("trims whitespace from term", () => {
    expect(filterModels(M, "  deepseek  ")).toHaveLength(1);
  });

  it("returns empty array for no matches", () => {
    expect(filterModels(M, "gpt")).toHaveLength(0);
  });
});

describe("groupByProvider", () => {
  it("groups models by provider", () => {
    const grouped = groupByProvider(M);
    expect(grouped).toHaveLength(2);

    const [provider1, models1] = grouped[0];
    const [provider2, models2] = grouped[1];

    expect(provider1).toBe("anthropic");
    expect(models1).toHaveLength(2);
    expect(provider2).toBe("deepseek");
    expect(models2).toHaveLength(1);
  });

  it("returns entries sorted by provider name insertion order", () => {
    const grouped = groupByProvider(M);
    expect(grouped.map(([p]) => p)).toEqual(["anthropic", "deepseek"]);
  });

  it("handles empty array", () => {
    expect(groupByProvider([])).toEqual([]);
  });
});

describe("formatCost", () => {
  it("returns null for undefined", () => {
    expect(formatCost()).toBeNull();
    expect(formatCost(undefined)).toBeNull();
  });

  it("returns null for null", () => {
    expect(formatCost(null)).toBeNull();
  });

  it("formats cost in millions with 1 decimal place", () => {
    expect(formatCost(0.003)).toBe("$3/M");
    expect(formatCost(0.005)).toBe("$5/M");
    expect(formatCost(0.01)).toBe("$10/M");
  });

  it("handles zero cost", () => {
    expect(formatCost(0)).toBe("$0/M");
  });

  it("handles large costs", () => {
    expect(formatCost(0.1)).toBe("$100/M");
    expect(formatCost(1)).toBe("$1,000/M");
  });

  it("rounds to 1 decimal place in millions", () => {
    expect(formatCost(0.0001)).toBe("$0.1/M");
  });
});
