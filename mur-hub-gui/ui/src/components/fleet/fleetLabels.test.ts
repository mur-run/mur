import { describe, it, expect } from "vitest";
import {
  filterByLabels,
  groupFleets,
  makePrimary,
  toggleAssignment,
  UNGROUPED,
} from "./fleetLabels";
import type { FleetSummary, LabelView } from "./types";

function fleet(name: string, labels: string[] = []): FleetSummary {
  return {
    name,
    display_name: name,
    goal: "",
    member_count: 0,
    active_jobs: 0,
    stopped: false,
    running: false,
    labels,
  };
}

const labels: LabelView[] = [
  { id: "dev", display: "Dev", color: null, fleet_count: 2 },
  { id: "ops", display: "Ops", color: null, fleet_count: 1 },
];

describe("filterByLabels", () => {
  it("empty selection means All", () => {
    const rows = [fleet("a", ["dev"]), fleet("b", [])];
    expect(filterByLabels(rows, []).map((f) => f.name)).toEqual(["a", "b"]);
  });

  it("matches a label in any position, not just the primary", () => {
    const rows = [fleet("a", ["dev", "ops"]), fleet("b", ["dev"])];
    expect(filterByLabels(rows, ["ops"]).map((f) => f.name)).toEqual(["a"]);
  });

  it("multi-select is OR, and never duplicates a fleet carrying both", () => {
    const rows = [fleet("a", ["dev", "ops"]), fleet("b", ["ops"]), fleet("c", [])];
    expect(filterByLabels(rows, ["dev", "ops"]).map((f) => f.name)).toEqual(["a", "b"]);
  });

  it("unlabelled fleets drop out once any chip is selected", () => {
    const rows = [fleet("a", ["dev"]), fleet("b", [])];
    expect(filterByLabels(rows, ["dev"]).map((f) => f.name)).toEqual(["a"]);
  });
});

describe("groupFleets", () => {
  it("groups by the primary label — the first entry — so a fleet appears once", () => {
    const rows = [fleet("a", ["dev", "ops"]), fleet("b", ["ops"])];
    const groups = groupFleets(rows, labels);
    expect(groups.map((g) => g.id)).toEqual(["dev", "ops"]);
    expect(groups[0].fleets.map((f) => f.name)).toEqual(["a"]);
    expect(groups[1].fleets.map((f) => f.name)).toEqual(["b"]);
    const appearances = groups.flatMap((g) => g.fleets.map((f) => f.name));
    expect(appearances).toEqual([...new Set(appearances)]);
  });

  it("uses registry order for groups, and puts Ungrouped last", () => {
    const rows = [fleet("z", []), fleet("a", ["ops"]), fleet("b", ["dev"])];
    const groups = groupFleets(rows, labels);
    expect(groups.map((g) => g.id)).toEqual(["dev", "ops", UNGROUPED]);
    expect(groups[2].fleets.map((f) => f.name)).toEqual(["z"]);
  });

  it("omits groups with no fleets", () => {
    const groups = groupFleets([fleet("a", ["dev"])], labels);
    expect(groups.map((g) => g.id)).toEqual(["dev"]);
  });

  it("a primary id missing from the registry falls back to Ungrouped", () => {
    const groups = groupFleets([fleet("a", ["ghost"])], labels);
    expect(groups.map((g) => g.id)).toEqual([UNGROUPED]);
  });

  it("group titles use display text, falling back to the id", () => {
    const bare: LabelView[] = [{ id: "dev", display: "", color: null, fleet_count: 1 }];
    expect(groupFleets([fleet("a", ["dev"])], bare)[0].title).toBe("dev");
    expect(groupFleets([fleet("a", ["dev"])], labels)[0].title).toBe("Dev");
  });
});

describe("toggleAssignment", () => {
  it("appends an unassigned label so the primary is untouched", () => {
    expect(toggleAssignment(["dev"], "ops")).toEqual(["dev", "ops"]);
  });

  it("removes an assigned label", () => {
    expect(toggleAssignment(["dev", "ops"], "ops")).toEqual(["dev"]);
  });

  it("removing the primary promotes the next one", () => {
    expect(toggleAssignment(["dev", "ops"], "dev")).toEqual(["ops"]);
  });
});

describe("makePrimary", () => {
  it("moves an assigned label to the front, keeping the rest in order", () => {
    expect(makePrimary(["dev", "ops", "lab"], "ops")).toEqual(["ops", "dev", "lab"]);
  });

  it("adds an unassigned label as the new primary", () => {
    expect(makePrimary(["dev"], "ops")).toEqual(["ops", "dev"]);
  });

  it("is a no-op on the current primary", () => {
    expect(makePrimary(["dev", "ops"], "dev")).toEqual(["dev", "ops"]);
  });
});
