import { describe, it, expect } from "vitest";
import { statusLabel, knobFor } from "./fleetJobsLogic";
import type { JobRow } from "../../fleet/types";

const t = (k: string) => k;

const job = (p: Partial<JobRow>): JobRow => ({
  id: "j1",
  text: "do the thing",
  status: "failed",
  created_at: "2026-09-12T00:00:00Z",
  ...p,
});

describe("statusLabel", () => {
  it("shows the stop reason when the guard stopped the loop", () => {
    expect(statusLabel(job({ stop_reason: "deadline" }), t as never)).toBe("limits.stopped");
  });

  it("shows finished for a converged stop, never the reason wording", () => {
    expect(statusLabel(job({ stop_reason: "converged" }), t as never)).toBe("limits.finished");
  });

  it("falls back to the plain job status when there is no stop reason", () => {
    expect(statusLabel(job({ status: "done" }), t as never)).toBe("fleet.status.done");
  });
});

describe("knobFor", () => {
  it("maps a stop reason to the limits knob it names", () => {
    expect(knobFor("deadline")).toBe("deadline");
    expect(knobFor("stuck")).toBe("stuck");
    expect(knobFor("budget")).toBe("cost_usd");
    expect(knobFor("converged")).toBeNull();
    expect(knobFor("max-iterations")).toBeNull();
  });
});
