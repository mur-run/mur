import { describe, expect, it } from "vitest";
import { NO_ROLE, roleFacets } from "./AgentsPage";
import type { AgentEntry } from "../../types";

const agent = (name: string, role: string | null): AgentEntry => ({
  name,
  display_name: name,
  category: "custom",
  status: "idle",
  model_id: "m",
  style_preset: "default-blob",
  role,
});

describe("roleFacets", () => {
  it("one chip per distinct role, sorted, plus a no-role bucket last", () => {
    const facets = roleFacets([agent("a", "Ops"), agent("b", "Engineer"), agent("c", "Engineer"), agent("d", null), agent("e", "  ")], "No role");
    expect(facets).toEqual([
      { id: "Engineer", label: "Engineer", count: 2 },
      { id: "Ops", label: "Ops", count: 1 },
      { id: NO_ROLE, label: "No role", count: 2 },
    ]);
  });
  it("empty input yields no chips", () => {
    expect(roleFacets([], "No role")).toEqual([]);
  });
});
