import { describe, expect, it } from "vitest";
import { bulkCounts, runBulk, startableIds, stoppableIds, type BulkItem } from "./bulkModel";

const items: BulkItem[] = [
  { id: "a", name: "A", status: "running" },
  { id: "b", name: "B", status: "idle" },
  { id: "c", name: "C", status: "restarting" },
  { id: "d", name: "D", status: "stopped" },
  { id: "e", name: "E", status: "failed" },
];

describe("bulkCounts / startableIds / stoppableIds", () => {
  it("running and restarting are stoppable; everything else is startable", () => {
    expect(bulkCounts(items)).toEqual({ startable: 3, stoppable: 2 });
    expect(startableIds(items)).toEqual(["b", "d", "e"]);
    expect(stoppableIds(items)).toEqual(["a", "c"]);
  });
});

describe("runBulk", () => {
  it("runs every call, keeps order, and turns a rejection into a failed result", async () => {
    const out = await runBulk(["x", "y", "z"], async (id) => {
      if (id === "y") throw new Error("boom");
    });
    expect(out).toEqual([
      { id: "x", ok: true },
      { id: "y", ok: false, error: "Error: boom" },
      { id: "z", ok: true },
    ]);
  });
});
