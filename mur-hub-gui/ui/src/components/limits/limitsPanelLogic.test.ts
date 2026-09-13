import { describe, it, expect } from "vitest";
import { rowAction, rowIsDimmed, chipLabel, patchForSave, patchForReset, badgeOf } from "./limitsPanelLogic";
import type { LimitsRowView, LimitsView } from "../fleet/types";

const t = (k: string) => k; // identity — the labels under test are keys

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

const view = (p: Partial<LimitsView>): LimitsView => ({
  scope: "fleet",
  name: "dev",
  rows: [],
  stale: [],
  billable: false,
  needs_restart: false,
  attended_note: "",
  bounded: "bounded",
  error: null,
  ...p,
});

describe("row presentation (§10.1)", () => {
  it("a local value is solid with Reset; an inherited one is dimmed with Override; a non-applying cost row has no action", () => {
    expect(rowAction(row({ local: true }))).toBe("reset");
    expect(rowIsDimmed(row({ local: true }))).toBe(false);
    expect(rowAction(row({ local: false, raw: null, source: "built-in default" }))).toBe("override");
    expect(rowIsDimmed(row({ local: false, raw: null }))).toBe(true);
    expect(
      rowAction(row({ knob: "cost_usd", applies: false, source: "", note: "runs on local models" }))
    ).toBe("none");
  });

  it("chips name the scope in the user's words", () => {
    expect(chipLabel(row({ source: "fleet.yaml" }), "fleet", t as never)).toBe("limits.chip.thisFleet");
    expect(chipLabel(row({ source: "profile.yaml" }), "agent", t as never)).toBe("limits.chip.thisAgent");
    expect(chipLabel(row({ source: "~/.mur/config.yaml" }), "fleet", t as never)).toBe("limits.chip.config");
    expect(chipLabel(row({ source: "built-in default" }), "global", t as never)).toBe("limits.chip.builtIn");
    expect(chipLabel(row({ source: "fleet.yaml (legacy loop.deadline)" }), "fleet", t as never)).toBe(
      "limits.chip.legacy"
    );
    expect(chipLabel(row({ source: "fleet.yaml" }), "agent", t as never)).toBe("limits.chip.fleet");
    expect(chipLabel(row({ source: "profile.yaml" }), "fleet", t as never)).toBe("limits.chip.agent");
    expect(chipLabel(row({ source: "command-line flag" }), "fleet", t as never)).toBe("limits.chip.flag");
  });
});

describe("patches", () => {
  it("save writes the one knob; reset unsets it (never writes the parent's value)", () => {
    expect(patchForSave("deadline", " 2h ")).toEqual({ deadline: "2h" });
    expect(patchForSave("stuck", "off")).toEqual({ stuck: "off" });
    expect(patchForSave("cost_usd", "5")).toEqual({ cost_usd: 5 });
    expect(patchForReset("stuck")).toEqual({ unset: ["stuck"] });
  });
});

describe("badge (§10.2)", () => {
  it("bounded / deadline only (amber) / unbounded", () => {
    expect(badgeOf(view({ bounded: "bounded" }), t as never)).toEqual({ text: "limits.badge.bounded", tone: "ok" });
    expect(badgeOf(view({ bounded: "deadline_only" }), t as never)).toEqual({
      text: "limits.badge.deadlineOnly",
      tone: "amber",
    });
    expect(badgeOf(view({ bounded: "unbounded" }), t as never)).toEqual({
      text: "limits.badge.unbounded",
      tone: "off",
    });
  });
});
