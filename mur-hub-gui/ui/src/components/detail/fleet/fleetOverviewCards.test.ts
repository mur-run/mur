import { describe, it, expect } from "vitest";
import { statCards } from "./fleetOverviewCards";
import type { LimitsRowView, LimitsView } from "../../fleet/types";

const t = (k: string) => k;

const row = (p: Partial<LimitsRowView>): LimitsRowView => ({
  knob: "deadline",
  value: "2h",
  source: "fleet.yaml",
  note: null,
  applies: true,
  local: true,
  raw: "2h",
  ...p,
});

const view = (billable: boolean, cost: string): LimitsView => ({
  scope: "fleet",
  name: "acme",
  rows: [
    row({ knob: "deadline", value: "2h", source: "fleet.yaml", local: true, raw: "2h" }),
    row({ knob: "stuck", value: "10m", source: "built-in default", local: false, raw: null }),
    row({
      knob: "cost_usd",
      value: cost,
      source: cost === "—" ? "built-in default" : "fleet.yaml",
      local: cost !== "—",
      raw: cost === "—" ? null : "5",
    }),
  ],
  stale: [],
  billable,
  needs_restart: false,
  attended_note: "",
  bounded: "bounded",
  error: null,
});

describe("statCards", () => {
  it("cards are last-run / deadline / stuck / done-when on a non-billable fleet", () => {
    const cards = statCards(view(false, "—"), { trigger: "cron", deadline: "2h", done_when: "", last_run: null }, t as never);
    expect(cards.map((c) => c.label)).toEqual([
      "fleet.settings.lastRun",
      "limits.card.deadline",
      "limits.card.stuck",
      "fleet.settings.doneWhen",
    ]);
    expect(cards[2].value).toBe("10m");
  });

  it("cost cap replaces stuck on a capped billable fleet", () => {
    const cards = statCards(view(true, "$5.00"), { trigger: "cron", deadline: "2h", done_when: "", last_run: null }, t as never);
    expect(cards[2]).toEqual({ value: "$5.00", label: "limits.card.costCap" });
  });

  it("last-run falls back to never when the loop has not run, or there is no loop", () => {
    expect(statCards(view(false, "—"), null, t as never)[0].value).toBe("fleet.never");
    expect(statCards(view(false, "—"), { trigger: "cron", deadline: "2h", done_when: "", last_run: null }, t as never)[0].value).toBe(
      "fleet.never"
    );
  });
});
